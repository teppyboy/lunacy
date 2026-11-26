//! Daemon: one worker thread per project, poll loop, state, trigger channel.

use crate::config::{self, Config, ProjectCfg, TriggerMode};
use crate::git;
use crate::model::{
    read_last_sha, read_pr_shas, read_seen_tags, reconcile_orphaned_runs, seen_tags_path,
    write_last_sha, write_pr_shas, write_seen_tags, Run, RunStatus, Trigger,
};
use crate::pipeline::{self, Ctx, LogChunkCallback, PrContext, RunSpec};
use crate::report::Github;
use crate::ws::WsHub;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Live diagnostics for verified `POST /api/github/webhook` deliveries.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WebhookStats {
    pub deliveries: u64,
    pub last_event: Option<String>,
    pub last_repo: Option<String>,
    pub last_delivery_id: Option<String>,
    pub last_received_at: Option<String>,
    pub last_outcome: Option<String>,
}

/// Live state the API reads: everything is behind one mutex for simplicity.
#[derive(Default)]
pub struct Shared {
    pub config: Config,
    pub state_dir: PathBuf,
    pub projects: Vec<ProjectState>,
    /// Set by the API to stop the daemon.
    pub shutdown: bool,
    /// Diagnostics for recent verified GitHub webhook deliveries.
    pub webhook_stats: WebhookStats,
    /// Real-time WebSocket broadcast hub.
    pub ws: WsHub,
}

/// Result of requesting cancellation of a project's active run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CancelProjectOutcome {
    /// Cancel flag set; payload is the active run id (empty if not yet emitted).
    Cancelling(String),
    /// Project exists, but no run is currently active.
    NotRunning,
    /// No project with that name is configured.
    NotFound,
}

impl Shared {
    /// Most settings hot-reload; the listen address does not.
    pub fn server_restart_needed(&self) -> bool {
        true
    }

    /// Signal cancellation for the active run matching `run_id`.
    /// Returns `Some((project_name, run_id))` when found.
    pub fn cancel_run(&self, run_id: &str) -> Option<(String, String)> {
        for p in &self.projects {
            if p.running && p.current_run.as_ref().is_some_and(|r| r.id == run_id) {
                if let Some(flag) = &p.cancel {
                    flag.store(true, Ordering::SeqCst);
                }
                return Some((p.name.clone(), run_id.to_string()));
            }
        }
        None
    }

    /// Signal cancellation for a project's currently active run.
    pub fn cancel_project(&self, name: &str) -> CancelProjectOutcome {
        let Some(p) = self.projects.iter().find(|p| p.name == name) else {
            return CancelProjectOutcome::NotFound;
        };
        if !p.running {
            return CancelProjectOutcome::NotRunning;
        }
        if let Some(flag) = &p.cancel {
            flag.store(true, Ordering::SeqCst);
        }
        let run_id = p
            .current_run
            .as_ref()
            .map(|r| r.id.clone())
            .unwrap_or_default();
        CancelProjectOutcome::Cancelling(run_id)
    }
}

/// Lightweight summary of an open pull request tracked on `ProjectState`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PrSummary {
    pub number: u64,
    pub title: String,
    pub head_ref: String,
    pub base_ref: String,
    pub head_sha: String,
    pub last_tested_sha: Option<String>,
    pub status: Option<RunStatus>,
}

pub struct ProjectState {
    pub name: String,
    pub branch: String,
    pub github_repo: Option<String>,
    pub upstream_sha: Option<String>,
    pub last_sha: Option<String>,
    pub running: bool,
    pub current_run: Option<Run>,
    pub last_run: Option<Run>,
    /// Errors from the last poll (fetch failure and friends).
    pub last_error: Option<String>,
    /// Cancellation flag for the currently running pipeline (if any).
    pub cancel: Option<Arc<AtomicBool>>,
    /// Open GitHub Pull Requests observed during PR polling.
    pub open_prs: Vec<PrSummary>,
}

impl ProjectState {
    fn new(project: &ProjectCfg, state_dir: &Path) -> ProjectState {
        ProjectState {
            name: project.name.clone(),
            branch: project.branch.clone(),
            github_repo: resolve_github_repo(project),
            upstream_sha: None,
            last_sha: read_last_sha(state_dir, &project.name),
            running: false,
            current_run: None,
            last_run: crate::model::list_runs(state_dir, &project.name, 1)
                .into_iter()
                .next(),
            last_error: None,
            cancel: None,
            open_prs: Vec::new(),
        }
    }

    /// Public constructor for the API's config-reload path.
    pub fn new_public(project: &ProjectCfg, state_dir: &Path) -> ProjectState {
        ProjectState::new(project, state_dir)
    }

    /// Re-point at a changed config entry, keeping history.
    fn refresh(&mut self, project: &ProjectCfg, state_dir: &Path) {
        self.branch = project.branch.clone();
        self.github_repo = resolve_github_repo(project);
        if self.last_sha.is_none() {
            self.last_sha = read_last_sha(state_dir, &project.name);
        }
        if self.last_run.is_none() {
            self.last_run = crate::model::list_runs(state_dir, &project.name, 1)
                .into_iter()
                .next();
        }
        if !project.watch_prs {
            self.open_prs.clear();
        }
    }
}

/// A request from the API to the owner worker.
#[derive(Debug, Clone)]
pub enum Command {
    Trigger {
        sha: Option<String>,
        force: bool,
        trigger: Trigger,
        git_ref: Option<String>,
        pr: Option<PrContext>,
        deploy_only: bool,
        force_deploy: bool,
    },
    /// Worker should exit (config removed the project, or shutdown).
    Stop,
    /// Configuration changed; re-read it on the next poll.
    Reload,
}

pub struct WorkerHandle {
    pub cmd: Sender<Command>,
    pub join: Option<std::thread::JoinHandle<()>>,
}

/// Spawns and reconciles one worker per configured project.
pub struct Daemon {
    pub shared: Arc<Mutex<Shared>>,
    workers: HashMap<String, WorkerHandle>,
    config_path: PathBuf,
    shutdown: Arc<std::sync::atomic::AtomicBool>,
}

impl Daemon {
    pub fn new(config_path: PathBuf, cfg: Config) -> Daemon {
        let state_dir = cfg.state_dir();
        let _ = reconcile_orphaned_runs(&state_dir);
        let projects = cfg
            .projects
            .iter()
            .map(|p| ProjectState::new(p, &state_dir))
            .collect();
        let shared = Arc::new(Mutex::new(Shared {
            config: cfg,
            state_dir,
            projects,
            shutdown: false,
            webhook_stats: WebhookStats::default(),
            ws: WsHub::new(),
        }));
        Daemon {
            shared,
            workers: HashMap::new(),
            config_path,
            shutdown: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// Start (or restart) workers so each configured project has exactly one.
    pub fn reconcile(&mut self) {
        let cfg = self.shared.lock().expect("shared").config.clone();
        let wanted: Vec<String> = cfg.projects.iter().map(|p| p.name.clone()).collect();

        let stale: Vec<String> = self
            .workers
            .keys()
            .filter(|k| !wanted.contains(k))
            .cloned()
            .collect();
        for name in stale {
            if let Some(handle) = self.workers.remove(&name) {
                let _ = handle.cmd.send(Command::Stop);
                if let Some(j) = handle.join {
                    let _ = j.join();
                }
                log::info!("[{name}] worker stopped (project removed from config)");
            }
        }

        for project in cfg.projects.clone() {
            if self.workers.contains_key(&project.name) {
                if let Some(h) = self.workers.get(&project.name) {
                    let _ = h.cmd.send(Command::Reload);
                }
                continue;
            }
            let handle = spawn_worker(
                project.clone(),
                self.shared.clone(),
                self.shutdown.clone(),
                self.config_path.clone(),
            );
            self.workers.insert(project.name.clone(), handle);
            log::info!("[{}] worker started", project.name);
        }

        let ws = {
            let mut shared = self.shared.lock().expect("shared");
            let state_dir = shared.state_dir.clone();
            let mut old_projects = std::mem::take(&mut shared.projects);
            let mut ordered = Vec::with_capacity(cfg.projects.len());
            for project in &cfg.projects {
                if let Some(pos) = old_projects.iter().position(|p| p.name == project.name) {
                    let mut existing = old_projects.remove(pos);
                    existing.refresh(project, &state_dir);
                    ordered.push(existing);
                } else {
                    ordered.push(ProjectState::new(project, &state_dir));
                }
            }
            shared.projects = ordered;
            shared.ws.clone()
        };
        ws.emit_projects_updated();
    }

    /// Keep an API command map aligned with the currently active workers.
    pub fn sync_commands(
        &self,
        commands: &Arc<Mutex<std::collections::BTreeMap<String, Sender<Command>>>>,
    ) {
        let mut guard = commands.lock().expect("commands");
        guard.clear();
        for (name, handle) in &self.workers {
            guard.insert(name.clone(), handle.cmd.clone());
        }
    }

    /// Wait until shutdown is requested, reconciling workers whenever `reloads` bumps.
    pub fn wait_with_reloads(
        &mut self,
        reloads: &Arc<Mutex<u32>>,
        commands: &Arc<Mutex<std::collections::BTreeMap<String, Sender<Command>>>>,
    ) {
        let mut seen = *reloads.lock().expect("reloads");
        while !self.shutdown.load(std::sync::atomic::Ordering::SeqCst)
            && !self.shared.lock().map(|s| s.shutdown).unwrap_or(true)
        {
            let now = *reloads.lock().expect("reloads");
            if now != seen {
                seen = now;
                self.reconcile();
                self.sync_commands(commands);
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    pub fn shutdown(&mut self) {
        self.shutdown
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let handles: Vec<WorkerHandle> = std::mem::take(&mut self.workers).into_values().collect();
        for handle in &handles {
            let _ = handle.cmd.send(Command::Stop);
        }
        // A worker inside a pipeline step or a `git fetch` only sees Stop when that returns, so
        // don't wait past the service manager's stop timeout (systemd: 30 s); the process exit
        // ends what is left, and the next start retries.
        let deadline = Instant::now() + Duration::from_secs(20);
        for handle in handles {
            let Some(j) = handle.join else { continue };
            while !j.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(50));
            }
            if j.is_finished() {
                let _ = j.join();
            } else {
                log::warn!("a worker is still busy; exiting without waiting for it");
            }
        }
    }

    /// Command channel for one project's worker, for API triggers.
    pub fn command_sender(&self, name: &str) -> Option<Sender<Command>> {
        self.workers.get(name).map(|w| w.cmd.clone())
    }

    /// Block until `shutdown` is requested.
    pub fn wait(&self) {
        // The signal handler and the API request a stop through `Shared`.
        while !self.shutdown.load(std::sync::atomic::Ordering::SeqCst)
            && !self.shared.lock().map(|s| s.shutdown).unwrap_or(true)
        {
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}

fn spawn_worker(
    project: ProjectCfg,
    shared: Arc<Mutex<Shared>>,
    shutdown: Arc<std::sync::atomic::AtomicBool>,
    config_path: PathBuf,
) -> WorkerHandle {
    let (tx, rx) = std::sync::mpsc::channel::<Command>();
    let join = std::thread::Builder::new()
        .name(format!("lunacy-{}", project.name))
        .spawn(move || worker_loop(project, shared, shutdown, rx, config_path))
        .expect("spawning a project worker");
    WorkerHandle {
        cmd: tx,
        join: Some(join),
    }
}

#[derive(Debug, Clone)]
struct QueuedTrigger {
    sha: Option<String>,
    force: bool,
    trigger: Trigger,
    git_ref: Option<String>,
    pr: Option<PrContext>,
    deploy_only: bool,
    force_deploy: bool,
}

#[allow(clippy::too_many_lines)]
fn worker_loop(
    mut project: ProjectCfg,
    shared: Arc<Mutex<Shared>>,
    shutdown: Arc<std::sync::atomic::AtomicBool>,
    rx: Receiver<Command>,
    config_path: PathBuf,
) {
    let name = project.name.clone();
    let mut last_poll = Instant::now()
        .checked_sub(Duration::from_secs(3600))
        .unwrap_or_else(Instant::now);
    let mut queued: VecDeque<QueuedTrigger> = VecDeque::new();
    let mut want_reload = false;

    loop {
        if shutdown.load(std::sync::atomic::Ordering::SeqCst) {
            break;
        }

        // Config reload: take the new definition of this project (and daemon-wide settings).
        while let Ok(cmd) = rx.try_recv() {
            match cmd {
                Command::Stop => return,
                Command::Reload => want_reload = true,
                Command::Trigger {
                    sha,
                    force,
                    trigger,
                    git_ref,
                    pr,
                    deploy_only,
                    force_deploy,
                } => {
                    queued.push_back(QueuedTrigger {
                        sha,
                        force,
                        trigger,
                        git_ref,
                        pr,
                        deploy_only,
                        force_deploy,
                    });
                }
            }
        }
        if want_reload {
            want_reload = false;
            match Config::load(&config_path) {
                Ok(new_cfg) => match new_cfg.project(&name).cloned() {
                    Some(p) => {
                        project = p;
                        if let Ok(mut s) = shared.lock() {
                            s.config = new_cfg.clone();
                        }
                        log::info!("[{name}] configuration reloaded");
                    }
                    None => return,
                },
                Err(e) => log::warn!("[{name}] config reload failed, keeping the old one: {e:#}"),
            }
        }

        // A queued run (API trigger / webhook / force deploy) runs before polling.
        if let Some(q) = queued.pop_front() {
            let cfg = shared.lock().expect("shared").config.clone();
            handle_queued_trigger(&shared, &cfg, &project, q);
            last_poll = Instant::now();
            continue;
        }

        let cfg = shared.lock().expect("shared").config.clone();
        let paused = !project.is_enabled();
        let webhook_only = project.effective_trigger_mode(&cfg.general) == TriggerMode::Webhook;
        let interval = Duration::from_secs(project.poll_interval(&cfg.general).max(1));
        if (paused || webhook_only || last_poll.elapsed() < interval) && queued.is_empty() {
            match rx.recv_timeout(Duration::from_millis(250)) {
                Ok(Command::Stop) => return,
                Ok(Command::Reload) => {
                    want_reload = true;
                    continue;
                }
                Ok(Command::Trigger {
                    sha,
                    force,
                    trigger,
                    git_ref,
                    pr,
                    deploy_only,
                    force_deploy,
                }) => {
                    queued.push_back(QueuedTrigger {
                        sha,
                        force,
                        trigger,
                        git_ref,
                        pr,
                        deploy_only,
                        force_deploy,
                    });
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
            continue;
        }
        last_poll = Instant::now();

        // 1. Poll the default watched branch.
        if let Some(sha) = fetch_upstream(&cfg, &project, &shared) {
            let last = shared.lock().ok().and_then(|s| {
                s.projects
                    .iter()
                    .find(|p| p.name == name)
                    .and_then(|p| p.last_sha.clone())
            });
            if last.as_deref() == Some(sha.as_str()) {
                log::debug!("[{name}] {} unchanged", crate::model::short_sha(&sha));
            } else {
                let spec = RunSpec {
                    sha,
                    trigger: Trigger::Poll,
                    git_ref: project.branch.clone(),
                    pr: None,
                    deploy_only: false,
                    force_deploy: false,
                };
                execute_spec(&shared, &cfg, &project, &spec);
            }
        }

        if shutdown.load(std::sync::atomic::Ordering::SeqCst) {
            break;
        }

        // 2. Poll open GitHub PRs (CI-only) when `watch_prs` is enabled.
        if project.watch_prs {
            poll_open_prs(&shared, &cfg, &project, &shutdown);
        }

        if shutdown.load(std::sync::atomic::Ordering::SeqCst) {
            break;
        }

        // 3. Poll remote tags when `watch_tags` or tag `deploy_refs` are configured.
        if project.watches_tags() {
            poll_tags(&shared, &cfg, &project, &shutdown);
        }
    }
    log::info!("[{name}] worker exiting");
}

fn handle_queued_trigger(
    shared: &Arc<Mutex<Shared>>,
    cfg: &Config,
    project: &ProjectCfg,
    q: QueuedTrigger,
) {
    let name = &project.name;
    let git_ref = q
        .git_ref
        .filter(|r| !r.trim().is_empty())
        .unwrap_or_else(|| {
            q.pr.as_ref()
                .map(|p| {
                    if p.head_ref.is_empty() {
                        format!("pull/{}", p.number)
                    } else {
                        p.head_ref.clone()
                    }
                })
                .unwrap_or_else(|| project.branch.clone())
        });

    let target = resolve_queued_sha(
        shared,
        cfg,
        project,
        &git_ref,
        q.pr.as_ref(),
        q.sha,
        q.trigger,
    );
    let Some(target) = target.filter(|s| !s.is_empty()) else {
        log::warn!("[{name}] trigger with no resolvable sha; ignoring");
        return;
    };

    let is_default_branch = q.pr.is_none() && pipeline::is_default_branch_ref(project, &git_ref);
    if !q.force && is_default_branch && !q.deploy_only && !q.force_deploy {
        let last = shared.lock().ok().and_then(|s| {
            s.projects
                .iter()
                .find(|p| p.name == *name)
                .and_then(|p| p.last_sha.clone())
        });
        if last.as_deref() == Some(target.as_str()) {
            log::info!(
                "[{name}] {} already processed; no rerun",
                crate::model::short_sha(&target)
            );
            return;
        }
    }

    let spec = RunSpec {
        sha: target,
        trigger: q.trigger,
        git_ref,
        pr: q.pr,
        deploy_only: q.deploy_only,
        force_deploy: q.force_deploy,
    };
    execute_spec(shared, cfg, project, &spec);
}

fn resolve_queued_sha(
    shared: &Arc<Mutex<Shared>>,
    cfg: &Config,
    project: &ProjectCfg,
    git_ref: &str,
    pr: Option<&PrContext>,
    sha: Option<String>,
    trigger: Trigger,
) -> Option<String> {
    let sha = sha.filter(|s| !s.trim().is_empty());
    if let Some(pr) = pr {
        let hint = sha.as_deref().unwrap_or("");
        if let Err(e) = git::fetch_pr(&project.repo_path, &project.remote, pr.number, hint) {
            log::warn!("[{}] fetch PR #{} failed: {e:#}", project.name, pr.number);
            if let Some(s) = &sha {
                if git::rev_parse(&project.repo_path, s).is_ok() {
                    return Some(s.clone());
                }
            }
            return None;
        }
        return match sha {
            Some(s) => Some(s),
            None => git::rev_parse(&project.repo_path, "FETCH_HEAD").ok(),
        };
    }

    if git_ref.starts_with("tags/") || git_ref.starts_with("refs/tags/") {
        let short = git_ref
            .strip_prefix("refs/tags/")
            .or_else(|| git_ref.strip_prefix("tags/"))
            .unwrap_or(git_ref);
        if let Err(e) = git::fetch_tag(&project.repo_path, &project.remote, short) {
            log::warn!("[{}] fetch tag '{short}' failed: {e:#}", project.name);
        }
        return sha
            .or_else(|| git::rev_parse(&project.repo_path, &format!("refs/tags/{short}")).ok());
    }

    if !pipeline::is_default_branch_ref(project, git_ref) {
        let branch = git_ref.strip_prefix("refs/heads/").unwrap_or(git_ref);
        if let Err(e) = git::fetch(&project.repo_path, &project.remote, branch) {
            log::warn!("[{}] fetch branch '{branch}' failed: {e:#}", project.name);
        }
        return sha.or_else(|| git::upstream_sha(&project.repo_path, &project.remote, branch).ok());
    }

    match sha {
        Some(s) => {
            if trigger == Trigger::Webhook || git::rev_parse(&project.repo_path, &s).is_err() {
                let _ = fetch_upstream(cfg, project, shared);
            }
            Some(s)
        }
        None => fetch_upstream(cfg, project, shared),
    }
}

fn github_client(cfg: &Config, project_name: &str) -> Option<Github> {
    cfg.github.as_ref().and_then(|g| {
        if !g.enabled() {
            return None;
        }
        match Github::new(g) {
            Ok(gh) => Some(gh),
            Err(e) => {
                log::warn!("[{project_name}] github reporting disabled: {e:#}");
                None
            }
        }
    })
}

pub(crate) fn resolve_github_repo(project: &ProjectCfg) -> Option<String> {
    project.github_repo.clone().or_else(|| {
        git::remote_url(&project.repo_path, &project.remote)
            .as_deref()
            .and_then(config::repo_from_remote)
    })
}

fn poll_open_prs(
    shared: &Arc<Mutex<Shared>>,
    cfg: &Config,
    project: &ProjectCfg,
    shutdown: &AtomicBool,
) {
    let Some(gh) = github_client(cfg, &project.name) else {
        return;
    };
    let Some(repo) = resolve_github_repo(project) else {
        return;
    };
    let pulls = match gh.list_open_pulls(&repo) {
        Ok(p) => p,
        Err(e) => {
            log::warn!("[{}] polling open PRs failed: {e:#}", project.name);
            return;
        }
    };

    let state_dir = shared.lock().expect("shared").state_dir.clone();
    let mut seen = read_pr_shas(&state_dir, &project.name);
    let open_numbers: BTreeSet<u64> = pulls.iter().map(|p| p.number).collect();
    let old_len = seen.len();
    seen.retain(|num, _| open_numbers.contains(num));
    if seen.len() != old_len {
        if let Err(e) = write_pr_shas(&state_dir, &project.name, &seen) {
            log::warn!("[{}] writing pruned PR shas failed: {e}", project.name);
        }
    }

    // Refresh lightweight `open_prs` snapshot on `ProjectState`.
    let prs_changed_ws = if let Ok(mut s) = shared.lock() {
        let ws = s.ws.clone();
        let mut changed = false;
        if let Some(ps) = s.projects.iter_mut().find(|p| p.name == project.name) {
            let prev: HashMap<u64, PrSummary> =
                ps.open_prs.iter().cloned().map(|p| (p.number, p)).collect();
            let next: Vec<PrSummary> = pulls
                .iter()
                .map(|pr| {
                    let last_tested_sha = seen.get(&pr.number).cloned();
                    let status = prev.get(&pr.number).and_then(|old| {
                        if old.last_tested_sha == last_tested_sha {
                            old.status
                        } else {
                            None
                        }
                    });
                    PrSummary {
                        number: pr.number,
                        title: pr.title.clone(),
                        head_ref: pr.head_ref.clone(),
                        base_ref: pr.base_ref.clone(),
                        head_sha: pr.head_sha.clone(),
                        last_tested_sha,
                        status,
                    }
                })
                .collect();
            if ps.open_prs != next {
                ps.open_prs = next;
                changed = true;
            }
        }
        changed.then_some(ws)
    } else {
        None
    };
    if let Some(ws) = prs_changed_ws {
        ws.emit_projects_updated();
    }

    for pr in pulls {
        if shutdown.load(Ordering::SeqCst) {
            break;
        }
        if seen.get(&pr.number).map(String::as_str) == Some(pr.head_sha.as_str()) {
            continue;
        }
        if let Err(e) = git::fetch_pr(&project.repo_path, &project.remote, pr.number, &pr.head_sha)
        {
            log::warn!(
                "[{}] fetch PR #{} ({}) failed: {e:#}",
                project.name,
                pr.number,
                crate::model::short_sha(&pr.head_sha)
            );
            continue;
        }
        let git_ref = if pr.head_ref.is_empty() {
            format!("pull/{}", pr.number)
        } else {
            pr.head_ref.clone()
        };
        let spec = RunSpec {
            sha: pr.head_sha.clone(),
            trigger: Trigger::PullRequest,
            git_ref,
            pr: Some(PrContext {
                number: pr.number,
                title: pr.title.clone(),
                head_ref: pr.head_ref.clone(),
                base_ref: pr.base_ref.clone(),
                base_sha: pr.base_sha.clone(),
            }),
            deploy_only: false,
            force_deploy: false,
        };
        let run = execute_spec(shared, cfg, project, &spec);
        seen.insert(pr.number, pr.head_sha.clone());
        if let Err(e) = write_pr_shas(&state_dir, &project.name, &seen) {
            log::warn!("[{}] persisting PR shas failed: {e}", project.name);
        }
        let ws = if let Ok(mut s) = shared.lock() {
            if let Some(ps) = s.projects.iter_mut().find(|p| p.name == project.name) {
                if let Some(entry) = ps.open_prs.iter_mut().find(|p| p.number == pr.number) {
                    entry.last_tested_sha = Some(pr.head_sha.clone());
                    entry.status = Some(run.status);
                }
            }
            Some(s.ws.clone())
        } else {
            None
        };
        if let Some(ws) = ws {
            ws.emit_projects_updated();
        }
    }
}

fn poll_tags(
    shared: &Arc<Mutex<Shared>>,
    cfg: &Config,
    project: &ProjectCfg,
    shutdown: &AtomicBool,
) {
    let remote_tags = match git::list_remote_tags(&project.repo_path, &project.remote) {
        Ok(t) => t,
        Err(e) => {
            log::warn!("[{}] listing remote tags failed: {e:#}", project.name);
            return;
        }
    };
    let state_dir = shared.lock().expect("shared").state_dir.clone();
    let remote_map: BTreeMap<String, String> = remote_tags.into_iter().collect();

    // First run: seed all existing tags without building them.
    if !seen_tags_path(&state_dir, &project.name).is_file() {
        if let Err(e) = write_seen_tags(&state_dir, &project.name, &remote_map) {
            log::warn!("[{}] seeding seen tags failed: {e}", project.name);
        } else {
            log::info!(
                "[{}] seeded {} existing remote tag(s)",
                project.name,
                remote_map.len()
            );
        }
        return;
    }

    let mut seen = read_seen_tags(&state_dir, &project.name);
    let mut dirty = false;
    for (tag, remote_sha) in remote_map {
        if shutdown.load(Ordering::SeqCst) {
            break;
        }
        if seen.get(&tag).map(String::as_str) == Some(remote_sha.as_str()) {
            continue;
        }
        let tag_ref = format!("tags/{tag}");
        let should_build = if project.deploy_refs.is_empty() {
            project.watch_tags
        } else {
            config::matches_deploy_ref(project, &tag_ref, false)
        };
        if !should_build {
            seen.insert(tag, remote_sha);
            dirty = true;
            continue;
        }
        if let Err(e) = git::fetch_tag(&project.repo_path, &project.remote, &tag) {
            log::warn!("[{}] fetch tag '{tag}' failed: {e:#}", project.name);
            continue;
        }
        let commit_sha = git::rev_parse(&project.repo_path, &format!("refs/tags/{tag}"))
            .unwrap_or_else(|_| remote_sha.clone());
        let spec = RunSpec {
            sha: commit_sha,
            trigger: Trigger::Tag,
            git_ref: tag_ref,
            pr: None,
            deploy_only: false,
            force_deploy: false,
        };
        execute_spec(shared, cfg, project, &spec);
        seen.insert(tag, remote_sha);
        if let Err(e) = write_seen_tags(&state_dir, &project.name, &seen) {
            log::warn!("[{}] persisting seen tags failed: {e}", project.name);
        }
        dirty = false;
    }
    if dirty {
        if let Err(e) = write_seen_tags(&state_dir, &project.name, &seen) {
            log::warn!("[{}] persisting seen tags failed: {e}", project.name);
        }
    }
}

/// Fetch and return the upstream sha, recording errors for the API.
fn fetch_upstream(
    cfg: &Config,
    project: &ProjectCfg,
    shared: &Arc<Mutex<Shared>>,
) -> Option<String> {
    match git::fetch(&project.repo_path, &project.remote, &project.branch)
        .and_then(|_| git::upstream_sha(&project.repo_path, &project.remote, &project.branch))
    {
        Ok(sha) => {
            let notify_ws = if let Ok(mut s) = shared.lock() {
                let mut changed = false;
                if let Some(p) = s.projects.iter_mut().find(|p| p.name == project.name) {
                    if p.upstream_sha.as_deref() != Some(sha.as_str()) {
                        p.upstream_sha = Some(sha.clone());
                        changed = true;
                    }
                    if p.last_error.is_some() {
                        p.last_error = None;
                        changed = true;
                    }
                    if p.github_repo.is_none() && cfg.github.is_some() {
                        let resolved = resolve_github_repo(project);
                        if resolved.is_some() {
                            p.github_repo = resolved;
                            changed = true;
                        }
                    }
                }
                changed.then(|| s.ws.clone())
            } else {
                None
            };
            if let Some(ws) = notify_ws {
                ws.emit_projects_updated();
            }
            Some(sha)
        }
        Err(e) => {
            log::warn!("[{}] fetch failed: {e:#}", project.name);
            let msg = format!("fetch: {e:#}");
            let notify_ws = if let Ok(mut s) = shared.lock() {
                let mut changed = false;
                if let Some(p) = s.projects.iter_mut().find(|p| p.name == project.name) {
                    if p.last_error.as_deref() != Some(msg.as_str()) {
                        p.last_error = Some(msg);
                        changed = true;
                    }
                }
                changed.then(|| s.ws.clone())
            } else {
                None
            };
            if let Some(ws) = notify_ws {
                ws.emit_projects_updated();
            }
            None
        }
    }
}

fn should_advance_last_sha(
    is_default_branch: bool,
    trigger: Trigger,
    sha: &str,
    existing_last_sha: Option<&str>,
    upstream_sha: Option<&str>,
) -> bool {
    is_default_branch
        && (trigger == Trigger::Poll || existing_last_sha.is_none() || upstream_sha == Some(sha))
}

fn execute_spec(
    shared: &Arc<Mutex<Shared>>,
    cfg: &Config,
    project: &ProjectCfg,
    spec: &RunSpec,
) -> Run {
    log::info!(
        "[{}] running pipeline for {} (ref={}, trigger={:?})",
        project.name,
        crate::model::short_sha(&spec.sha),
        spec.git_ref,
        spec.trigger
    );
    let github = if cfg.github_enabled_for(project) {
        github_client(cfg, &project.name)
    } else {
        None
    };

    let (state_dir, ws) = {
        let s = shared.lock().expect("shared");
        (s.state_dir.clone(), s.ws.clone())
    };
    let act_lock = cfg.act_lock_file();

    let cancel = Arc::new(AtomicBool::new(false));
    let (existing_last_sha, upstream_sha) = if let Ok(mut s) = shared.lock() {
        if let Some(p) = s.projects.iter_mut().find(|p| p.name == project.name) {
            p.running = true;
            p.cancel = Some(cancel.clone());
            (p.last_sha.clone(), p.upstream_sha.clone())
        } else {
            (None, None)
        }
    } else {
        (None, None)
    };
    ws.emit_projects_updated();

    let is_default_branch =
        spec.pr.is_none() && pipeline::is_default_branch_ref(project, &spec.git_ref);
    let advance_last_sha = should_advance_last_sha(
        is_default_branch,
        spec.trigger,
        &spec.sha,
        existing_last_sha.as_deref(),
        upstream_sha.as_deref(),
    );
    let current_run_id: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
    let on_update = {
        let shared = shared.clone();
        let ws = ws.clone();
        let current_run_id = current_run_id.clone();
        move |run: &Run| {
            if let Ok(mut id_guard) = current_run_id.lock() {
                if id_guard.is_empty() {
                    *id_guard = run.id.clone();
                }
            }
            if let Ok(mut s) = shared.lock() {
                if let Some(p) = s.projects.iter_mut().find(|p| p.name == run.project) {
                    p.current_run = Some(run.clone());
                    if advance_last_sha && (run.trigger == Trigger::Poll || run.status.is_final()) {
                        p.last_sha = Some(run.sha.clone());
                    }
                }
            }
            ws.emit_run_updated(run);
            ws.emit_projects_updated();
        }
    };
    let on_log_chunk: LogChunkCallback = {
        let ws = ws.clone();
        let current_run_id = current_run_id.clone();
        let offsets: Arc<Mutex<HashMap<usize, u64>>> = Arc::new(Mutex::new(HashMap::new()));
        Arc::new(move |step: usize, chunk: &str| {
            if chunk.is_empty() {
                return;
            }
            let run_id = current_run_id.lock().map(|g| g.clone()).unwrap_or_default();
            if run_id.is_empty() {
                return;
            }
            let offset = {
                let mut map = offsets.lock().expect("log offsets");
                let entry = map.entry(step).or_insert(0);
                *entry = entry.saturating_add(chunk.len() as u64);
                *entry
            };
            ws.emit_log_chunk(run_id, step, chunk, offset);
        })
    };

    let ctx = Ctx {
        cfg,
        state_dir: &state_dir,
        os: crate::config::Os::current(),
        github: github.as_ref(),
        on_update: &on_update,
        act_lock: &act_lock,
        cancel: Some(cancel.as_ref()),
        on_log_chunk: Some(on_log_chunk),
    };
    let run = pipeline::run_pipeline_spec(&ctx, project, spec);

    // State semantics: the last processed sha advances when a default-branch pipeline
    // ends on a poll, first run, or current upstream head — whatever the result, so a
    // failing commit is not retried forever. Historical rollbacks / force-deploys of an
    // older commit, PRs, and tags/non-default branches do NOT overwrite `last_sha`.
    if advance_last_sha {
        if let Err(e) = write_last_sha(&state_dir, &project.name, &spec.sha) {
            log::warn!("[{}] persisting last sha failed: {e}", project.name);
        }
    } else if let Some(pr) = &spec.pr {
        let mut seen = read_pr_shas(&state_dir, &project.name);
        seen.insert(pr.number, spec.sha.clone());
        if let Err(e) = write_pr_shas(&state_dir, &project.name, &seen) {
            log::warn!("[{}] persisting PR shas failed: {e}", project.name);
        }
    }

    if let Ok(mut s) = shared.lock() {
        if let Some(p) = s.projects.iter_mut().find(|p| p.name == project.name) {
            p.running = false;
            p.cancel = None;
            p.current_run = None;
            if advance_last_sha {
                p.last_sha = Some(run.sha.clone());
            }
            if let Some(pr) = &spec.pr {
                if let Some(entry) = p.open_prs.iter_mut().find(|e| e.number == pr.number) {
                    entry.last_tested_sha = Some(run.sha.clone());
                    entry.status = Some(run.status);
                }
            }
            p.last_run = Some(run.clone());
        }
    }
    ws.emit_run_updated(&run);
    ws.emit_projects_updated();
    run
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Checkout, StepCfg, StepKind};

    #[test]
    fn should_advance_last_sha_preserves_head_on_historical_rollback() {
        let old = "1111111111111111111111111111111111111111";
        let new = "2222222222222222222222222222222222222222";

        // Poll on default branch always advances.
        assert!(should_advance_last_sha(
            true,
            Trigger::Poll,
            new,
            Some(old),
            Some(new)
        ));
        // First run when no last_sha exists yet advances.
        assert!(should_advance_last_sha(
            true,
            Trigger::ForceDeploy,
            old,
            None,
            Some(new)
        ));
        // ForceDeploy / Manual on the current upstream head advances.
        assert!(should_advance_last_sha(
            true,
            Trigger::ForceDeploy,
            new,
            Some(old),
            Some(new)
        ));
        // Historical rollback (ForceDeploy / Manual of an older commit) MUST NOT rewind last_sha!
        assert!(!should_advance_last_sha(
            true,
            Trigger::ForceDeploy,
            old,
            Some(new),
            Some(new)
        ));
        assert!(!should_advance_last_sha(
            true,
            Trigger::Manual,
            old,
            Some(new),
            Some(new)
        ));
        assert!(!should_advance_last_sha(
            true,
            Trigger::ForceDeploy,
            old,
            Some(new),
            None
        ));
        // Non-default branch / PR never advances default branch last_sha.
        assert!(!should_advance_last_sha(
            false,
            Trigger::Poll,
            new,
            None,
            Some(new)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn execute_spec_rollback_does_not_rewind_last_sha() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        let state_dir = dir.path().join("state");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&state_dir).unwrap();

        let run_git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(args)
                .output()
                .expect("git");
            assert!(
                out.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        run_git(&["init", "-b", "master"]);
        run_git(&["config", "user.email", "lunacy@example.invalid"]);
        run_git(&["config", "user.name", "lunacy"]);
        std::fs::write(repo.join("f.txt"), "v1\n").unwrap();
        run_git(&["add", "f.txt"]);
        run_git(&["commit", "-m", "v1"]);
        let old_sha = run_git(&["rev-parse", "HEAD"]);
        std::fs::write(repo.join("f.txt"), "v2\n").unwrap();
        run_git(&["add", "f.txt"]);
        run_git(&["commit", "-m", "v2"]);
        let new_sha = run_git(&["rev-parse", "HEAD"]);

        let step = StepCfg {
            name: "Deploy".into(),
            kind: StepKind::Shell,
            run: Some("true".into()),
            workflow: None,
            job: None,
            event: None,
            platforms: BTreeMap::new(),
            args: Vec::new(),
            secrets: BTreeMap::new(),
            vars: BTreeMap::new(),
            only_on: Vec::new(),
            timeout: Some(30),
            cwd: None,
            shell: Some(vec!["bash".into(), "-c".into()]),
            env: BTreeMap::new(),
        };
        let project = ProjectCfg {
            name: "demo".into(),
            repo_path: repo.clone(),
            remote: "origin".into(),
            branch: "master".into(),
            enabled: None,
            github_repo: None,
            github_enabled: None,
            checkout: Checkout::None,
            poll_interval: None,
            trigger_mode: None,
            step_timeout: None,
            timeout: None,
            env: BTreeMap::new(),
            contexts: Default::default(),
            discord_enabled: None,
            discord_webhook_url: None,
            discord_webhook_url_file: None,
            notify_on_start: None,
            discord_notify_on: None,
            discord_username: None,
            discord_mention_on_failure: None,
            shell: None,
            platforms: BTreeMap::new(),
            act_args: Vec::new(),
            watch_prs: false,
            deploy_refs: Vec::new(),
            watch_tags: false,
            cache: Some(false),
            ci: vec![step.clone()],
            deploy: vec![step],
        };
        let mut cfg = Config::default();
        cfg.general.state_dir = Some(state_dir.clone());
        cfg.projects = vec![project.clone()];

        write_last_sha(&state_dir, "demo", &new_sha).unwrap();
        let mut ps = ProjectState::new(&project, &state_dir);
        ps.upstream_sha = Some(new_sha.clone());
        assert_eq!(ps.last_sha.as_deref(), Some(new_sha.as_str()));

        let shared = Arc::new(Mutex::new(Shared {
            config: cfg.clone(),
            state_dir: state_dir.clone(),
            projects: vec![ps],
            shutdown: false,
            webhook_stats: WebhookStats::default(),
            ws: WsHub::new(),
        }));

        // Force-deploy older commit (rollback): must NOT rewind last_sha on disk or in memory.
        let rollback_spec = RunSpec {
            sha: old_sha.clone(),
            trigger: Trigger::ForceDeploy,
            git_ref: "master".into(),
            pr: None,
            deploy_only: true,
            force_deploy: true,
        };
        let run = execute_spec(&shared, &cfg, &project, &rollback_spec);
        assert_eq!(run.status, RunStatus::Success);
        assert_eq!(
            shared.lock().unwrap().projects[0].last_sha.as_deref(),
            Some(new_sha.as_str()),
            "in-memory last_sha must not rewind on rollback"
        );
        assert_eq!(
            read_last_sha(&state_dir, "demo").as_deref(),
            Some(new_sha.as_str()),
            "persisted last_sha must not rewind on rollback"
        );
    }

    #[test]
    fn daemon_new_reconciles_orphaned_running_runs_on_startup() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state_dir = dir.path().join("state");
        std::fs::create_dir_all(&state_dir).unwrap();

        let mut orphan = Run::new(
            "demo",
            "1111111111111111111111111111111111111111",
            "feat: orphaned".into(),
            Trigger::Manual,
            vec![crate::model::Step {
                phase: crate::model::Phase::Ci,
                name: "Test".into(),
                status: crate::model::StepStatus::Running,
                duration_ms: None,
                exit_code: None,
                kind: StepKind::Shell,
                substeps: Vec::new(),
            }],
        );
        orphan.status = RunStatus::Running;
        crate::model::persist_run(&state_dir, &orphan).unwrap();

        let mut cfg = Config::default();
        cfg.general.state_dir = Some(state_dir.clone());
        let _daemon = Daemon::new(dir.path().join("config.toml"), cfg);

        let updated = crate::model::read_run(&state_dir, &orphan.id).expect("run");
        assert_eq!(updated.status, RunStatus::Cancelled);
        assert!(updated.finished_at.is_some());
        assert_eq!(updated.steps[0].status, crate::model::StepStatus::Cancelled);
    }

    #[cfg(unix)]
    #[test]
    fn webhook_trigger_mode_skips_polling_but_runs_webhook_triggers() {
        let dir = tempfile::tempdir().expect("tempdir");
        let origin = dir.path().join("origin.git");
        let work = dir.path().join("work");
        let state_dir = dir.path().join("state");
        std::fs::create_dir_all(&origin).unwrap();
        std::fs::create_dir_all(&state_dir).unwrap();

        let run_git = |cwd: &Path, args: &[&str]| {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(cwd)
                .args(args)
                .output()
                .expect("git");
            assert!(
                out.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        run_git(&origin, &["init", "--bare", "-b", "master"]);
        let clone_out = std::process::Command::new("git")
            .args(["clone", origin.to_str().unwrap(), work.to_str().unwrap()])
            .output()
            .expect("clone");
        assert!(clone_out.status.success());
        run_git(&work, &["config", "user.email", "lunacy@example.invalid"]);
        run_git(&work, &["config", "user.name", "lunacy"]);
        std::fs::write(work.join("a.txt"), "v1\n").unwrap();
        run_git(&work, &["add", "a.txt"]);
        run_git(&work, &["commit", "-m", "v1"]);
        run_git(&work, &["push", "origin", "master"]);
        let sha1 = run_git(&work, &["rev-parse", "HEAD"]);

        let step = StepCfg {
            name: "Echo".into(),
            kind: StepKind::Shell,
            run: Some("echo webhook-ok".into()),
            workflow: None,
            job: None,
            event: None,
            platforms: BTreeMap::new(),
            args: Vec::new(),
            secrets: BTreeMap::new(),
            vars: BTreeMap::new(),
            only_on: Vec::new(),
            timeout: Some(30),
            cwd: None,
            shell: Some(vec!["bash".into(), "-c".into()]),
            env: BTreeMap::new(),
        };
        let project = ProjectCfg {
            name: "demo".into(),
            repo_path: work.clone(),
            remote: "origin".into(),
            branch: "master".into(),
            enabled: None,
            github_repo: Some("teppyboy/demo".into()),
            github_enabled: None,
            checkout: Checkout::None,
            poll_interval: Some(0),
            trigger_mode: Some(TriggerMode::Webhook),
            step_timeout: None,
            timeout: None,
            env: BTreeMap::new(),
            contexts: Default::default(),
            discord_enabled: None,
            discord_webhook_url: None,
            discord_webhook_url_file: None,
            notify_on_start: None,
            discord_notify_on: None,
            discord_username: None,
            discord_mention_on_failure: None,
            shell: None,
            platforms: BTreeMap::new(),
            act_args: Vec::new(),
            watch_prs: false,
            deploy_refs: Vec::new(),
            watch_tags: false,
            cache: Some(false),
            ci: vec![step],
            deploy: Vec::new(),
        };
        let mut cfg = Config::default();
        cfg.general.state_dir = Some(state_dir.clone());
        cfg.general.trigger_mode = Some(TriggerMode::Webhook);
        cfg.general.poll_interval = 0;
        cfg.projects = vec![project];

        let cfg_path = dir.path().join("config.toml");
        std::fs::write(&cfg_path, toml::to_string(&cfg).unwrap()).unwrap();

        let mut daemon = Daemon::new(cfg_path, cfg);
        daemon.reconcile();

        // Wait longer than poll tick: webhook-only worker MUST NOT poll or auto-run.
        std::thread::sleep(Duration::from_millis(600));
        assert!(
            crate::model::list_runs(&state_dir, "demo", 10).is_empty(),
            "webhook-only project must not poll"
        );
        assert!(
            daemon.shared.lock().unwrap().projects[0]
                .upstream_sha
                .is_none(),
            "webhook-only project must not fetch during idle loop"
        );

        // Now deliver an explicit webhook trigger: worker fetches and executes the run.
        let tx = daemon.command_sender("demo").expect("worker sender");
        tx.send(Command::Trigger {
            sha: Some(sha1.clone()),
            force: false,
            trigger: Trigger::Webhook,
            git_ref: Some("master".into()),
            pr: None,
            deploy_only: false,
            force_deploy: false,
        })
        .unwrap();

        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            let runs = crate::model::list_runs(&state_dir, "demo", 10);
            if runs.first().is_some_and(|r| r.status == RunStatus::Success) {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }

        let runs = crate::model::list_runs(&state_dir, "demo", 10);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, RunStatus::Success);
        assert_eq!(runs[0].trigger, Trigger::Webhook);
        assert_eq!(runs[0].sha, sha1);
        assert_eq!(
            daemon.shared.lock().unwrap().projects[0]
                .upstream_sha
                .as_deref(),
            Some(sha1.as_str()),
            "webhook trigger should fetch upstream_sha"
        );

        daemon.shutdown();
    }

    #[cfg(unix)]
    #[test]
    fn paused_project_skips_polling_but_allows_manual_trigger() {
        let dir = tempfile::tempdir().expect("tempdir");
        let origin = dir.path().join("origin.git");
        let work = dir.path().join("work");
        let state_dir = dir.path().join("state");
        std::fs::create_dir_all(&origin).unwrap();
        std::fs::create_dir_all(&state_dir).unwrap();

        let run_git = |cwd: &Path, args: &[&str]| {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(cwd)
                .args(args)
                .output()
                .expect("git");
            assert!(
                out.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        run_git(&origin, &["init", "--bare", "-b", "master"]);
        let clone_out = std::process::Command::new("git")
            .args(["clone", origin.to_str().unwrap(), work.to_str().unwrap()])
            .output()
            .expect("clone");
        assert!(clone_out.status.success());
        run_git(&work, &["config", "user.email", "lunacy@example.invalid"]);
        run_git(&work, &["config", "user.name", "lunacy"]);
        std::fs::write(work.join("a.txt"), "v1\n").unwrap();
        run_git(&work, &["add", "a.txt"]);
        run_git(&work, &["commit", "-m", "v1"]);
        run_git(&work, &["push", "origin", "master"]);
        let sha1 = run_git(&work, &["rev-parse", "HEAD"]);

        let step = StepCfg {
            name: "Echo".into(),
            kind: StepKind::Shell,
            run: Some("echo manual-ok".into()),
            workflow: None,
            job: None,
            event: None,
            platforms: BTreeMap::new(),
            args: Vec::new(),
            secrets: BTreeMap::new(),
            vars: BTreeMap::new(),
            only_on: Vec::new(),
            timeout: Some(30),
            cwd: None,
            shell: Some(vec!["bash".into(), "-c".into()]),
            env: BTreeMap::new(),
        };
        let project = ProjectCfg {
            name: "paused-demo".into(),
            repo_path: work.clone(),
            remote: "origin".into(),
            branch: "master".into(),
            enabled: Some(false),
            github_repo: Some("teppyboy/demo".into()),
            github_enabled: None,
            checkout: Checkout::None,
            poll_interval: Some(1),
            trigger_mode: Some(TriggerMode::Both),
            step_timeout: None,
            timeout: None,
            env: BTreeMap::new(),
            contexts: Default::default(),
            discord_enabled: None,
            discord_webhook_url: None,
            discord_webhook_url_file: None,
            notify_on_start: None,
            discord_notify_on: None,
            discord_username: None,
            discord_mention_on_failure: None,
            shell: None,
            platforms: BTreeMap::new(),
            act_args: Vec::new(),
            watch_prs: false,
            deploy_refs: Vec::new(),
            watch_tags: false,
            cache: Some(false),
            ci: vec![step],
            deploy: Vec::new(),
        };
        let mut cfg = Config::default();
        cfg.general.state_dir = Some(state_dir.clone());
        cfg.general.poll_interval = 1;
        cfg.projects = vec![project];

        let cfg_path = dir.path().join("config.toml");
        std::fs::write(&cfg_path, toml::to_string(&cfg).unwrap()).unwrap();

        let mut daemon = Daemon::new(cfg_path, cfg);
        daemon.reconcile();

        // Paused project must NOT poll even when trigger_mode = Both and poll_interval = 1s.
        std::thread::sleep(Duration::from_millis(600));
        assert!(
            crate::model::list_runs(&state_dir, "paused-demo", 10).is_empty(),
            "paused project must not auto-poll"
        );
        assert!(
            daemon.shared.lock().unwrap().projects[0]
                .upstream_sha
                .is_none(),
            "paused project must not fetch while idle"
        );

        // Manual trigger still runs on a paused project.
        let tx = daemon.command_sender("paused-demo").expect("worker sender");
        tx.send(Command::Trigger {
            sha: Some(sha1.clone()),
            force: true,
            trigger: Trigger::Manual,
            git_ref: Some("master".into()),
            pr: None,
            deploy_only: false,
            force_deploy: false,
        })
        .unwrap();

        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            let runs = crate::model::list_runs(&state_dir, "paused-demo", 10);
            if runs.first().is_some_and(|r| r.status == RunStatus::Success) {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }

        let runs = crate::model::list_runs(&state_dir, "paused-demo", 10);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, RunStatus::Success);
        assert_eq!(runs[0].trigger, Trigger::Manual);
        assert_eq!(runs[0].sha, sha1);

        daemon.shutdown();
    }
}
