//! Daemon: one worker thread per project, poll loop, state, trigger channel.

use crate::config::{self, Config, ProjectCfg};
use crate::git;
use crate::model::{
    read_last_sha, read_pr_shas, read_seen_tags, seen_tags_path, write_last_sha, write_pr_shas,
    write_seen_tags, Run, RunStatus, Trigger,
};
use crate::pipeline::{self, Ctx, PrContext, RunSpec};
use crate::report::Github;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Live state the API reads: everything is behind one mutex for simplicity.
#[derive(Default)]
pub struct Shared {
    pub config: Config,
    pub state_dir: PathBuf,
    pub projects: Vec<ProjectState>,
    /// Set by the API to stop the daemon.
    pub shutdown: bool,
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
            github_repo: project.github_repo.clone(),
            upstream_sha: None,
            last_sha: read_last_sha(state_dir, &project.name),
            running: false,
            current_run: None,
            last_run: None,
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
        self.github_repo = project.github_repo.clone();
        if self.last_sha.is_none() {
            self.last_sha = read_last_sha(state_dir, &project.name);
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

    loop {
        if shutdown.load(std::sync::atomic::Ordering::SeqCst) {
            break;
        }

        // Config reload: take the new definition of this project (and daemon-wide settings).
        let mut want_reload = false;
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
        let interval = Duration::from_secs(project.poll_interval(&cfg.general));
        if last_poll.elapsed() < interval && queued.is_empty() {
            match rx.recv_timeout(Duration::from_millis(250)) {
                Ok(Command::Stop) => return,
                Ok(Command::Reload) => {
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

    let target = resolve_queued_sha(shared, cfg, project, &git_ref, q.pr.as_ref(), q.sha);
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
            if git::rev_parse(&project.repo_path, &s).is_err() {
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

fn resolve_github_repo(project: &ProjectCfg) -> Option<String> {
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
    if let Ok(mut s) = shared.lock() {
        if let Some(ps) = s.projects.iter_mut().find(|p| p.name == project.name) {
            let prev: HashMap<u64, PrSummary> =
                ps.open_prs.iter().cloned().map(|p| (p.number, p)).collect();
            ps.open_prs = pulls
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
        }
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
        if let Ok(mut s) = shared.lock() {
            if let Some(ps) = s.projects.iter_mut().find(|p| p.name == project.name) {
                if let Some(entry) = ps.open_prs.iter_mut().find(|p| p.number == pr.number) {
                    entry.last_tested_sha = Some(pr.head_sha.clone());
                    entry.status = Some(run.status);
                }
            }
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
            if let Ok(mut s) = shared.lock() {
                if let Some(p) = s.projects.iter_mut().find(|p| p.name == project.name) {
                    p.upstream_sha = Some(sha.clone());
                    p.last_error = None;
                    if p.github_repo.is_none() && cfg.github.is_some() {
                        p.github_repo = resolve_github_repo(project);
                    }
                }
            }
            Some(sha)
        }
        Err(e) => {
            log::warn!("[{}] fetch failed: {e:#}", project.name);
            if let Ok(mut s) = shared.lock() {
                if let Some(p) = s.projects.iter_mut().find(|p| p.name == project.name) {
                    p.last_error = Some(format!("fetch: {e:#}"));
                }
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
    let github = github_client(cfg, &project.name);

    let state_dir = shared.lock().expect("shared").state_dir.clone();
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

    let is_default_branch =
        spec.pr.is_none() && pipeline::is_default_branch_ref(project, &spec.git_ref);
    let advance_last_sha = should_advance_last_sha(
        is_default_branch,
        spec.trigger,
        &spec.sha,
        existing_last_sha.as_deref(),
        upstream_sha.as_deref(),
    );
    let on_update = {
        let shared = shared.clone();
        move |run: &Run| {
            if let Ok(mut s) = shared.lock() {
                if let Some(p) = s.projects.iter_mut().find(|p| p.name == run.project) {
                    p.current_run = Some(run.clone());
                    if advance_last_sha && (run.trigger == Trigger::Poll || run.status.is_final()) {
                        p.last_sha = Some(run.sha.clone());
                    }
                }
            }
        }
    };

    let ctx = Ctx {
        cfg,
        state_dir: &state_dir,
        os: crate::config::Os::current(),
        github: github.as_ref(),
        on_update: &on_update,
        act_lock: &act_lock,
        cancel: Some(cancel.as_ref()),
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
            github_repo: None,
            checkout: Checkout::None,
            poll_interval: None,
            step_timeout: None,
            timeout: None,
            env: BTreeMap::new(),
            contexts: Default::default(),
            discord_webhook_url: None,
            notify_on_start: None,
            shell: None,
            platforms: BTreeMap::new(),
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
}
