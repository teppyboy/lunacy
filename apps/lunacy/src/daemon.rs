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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// Thread-safe daemon-wide concurrency gate with hot-reloadable limit and cancellable waits.
#[derive(Debug)]
pub struct ConcurrencyGate {
    state: Mutex<(usize, usize)>,
    cvar: Condvar,
}

/// RAII permit acquired from a [`ConcurrencyGate`]; releases the slot on drop.
#[derive(Debug)]
pub struct ConcurrencyPermit {
    gate: Arc<ConcurrencyGate>,
}

impl Drop for ConcurrencyPermit {
    fn drop(&mut self) {
        let mut guard = self.gate.state.lock().unwrap_or_else(|e| e.into_inner());
        guard.0 = guard.0.saturating_sub(1);
        self.gate.cvar.notify_all();
    }
}

impl Default for ConcurrencyGate {
    fn default() -> Self {
        Self::new(config::DEFAULT_MAX_CONCURRENT_RUNS)
    }
}

impl ConcurrencyGate {
    pub fn new(limit: usize) -> Self {
        Self {
            state: Mutex::new((0, limit.max(1))),
            cvar: Condvar::new(),
        }
    }

    pub fn limit(&self) -> usize {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).1
    }

    pub fn active(&self) -> usize {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).0
    }

    /// Hot-reload the concurrency limit and wake any waiters immediately.
    pub fn set_limit(&self, new_limit: usize) {
        let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
        guard.1 = new_limit.max(1);
        self.cvar.notify_all();
    }

    /// Acquire a permit, waiting up to 150 ms per check while respecting `cancel` and `shutdown`.
    pub fn acquire(
        self: &Arc<Self>,
        cancel: &AtomicBool,
        shutdown: &AtomicBool,
    ) -> Option<ConcurrencyPermit> {
        let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if cancel.load(Ordering::SeqCst) || shutdown.load(Ordering::SeqCst) {
                return None;
            }
            if guard.0 < guard.1 {
                guard.0 += 1;
                return Some(ConcurrencyPermit {
                    gate: Arc::clone(self),
                });
            }
            let (g, _) = self
                .cvar
                .wait_timeout(guard, Duration::from_millis(150))
                .unwrap_or_else(|e| e.into_inner());
            guard = g;
        }
    }
}

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
    /// Global concurrency gate shared across all project workers.
    pub concurrency_gate: Arc<ConcurrencyGate>,
    /// Serializes read-modify-write state file updates (`last_sha`, `pr_shas`, `seen_tags`).
    pub state_io_lock: Arc<Mutex<()>>,
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
    /// Cancels only the matching run without aborting sibling concurrent runs on the same project.
    /// Returns `Some((project_name, run_id))` when found.
    pub fn cancel_run(&self, run_id: &str) -> Option<(String, String)> {
        for p in &self.projects {
            if let Some(active) = p.active_runs.iter().find(|a| a.run.id == run_id) {
                active.cancel.store(true, Ordering::SeqCst);
                return Some((p.name.clone(), run_id.to_string()));
            }
            if p.active_runs.is_empty()
                && p.running
                && p.current_run.as_ref().is_some_and(|r| r.id == run_id)
            {
                if let Some(flag) = &p.cancel {
                    flag.store(true, Ordering::SeqCst);
                }
                return Some((p.name.clone(), run_id.to_string()));
            }
        }
        None
    }

    /// Signal cancellation for ALL currently active runs on a project.
    pub fn cancel_project(&self, name: &str) -> CancelProjectOutcome {
        let Some(p) = self.projects.iter().find(|p| p.name == name) else {
            return CancelProjectOutcome::NotFound;
        };
        if p.active_run_count() == 0 {
            return CancelProjectOutcome::NotRunning;
        }
        p.cancel_generation.fetch_add(1, Ordering::SeqCst);
        for active in &p.active_runs {
            active.cancel.store(true, Ordering::SeqCst);
        }
        for pending in &p.pending_slots {
            pending.cancel.store(true, Ordering::SeqCst);
        }
        if let Some(flag) = &p.cancel {
            flag.store(true, Ordering::SeqCst);
        }
        let run_id = p
            .active_runs
            .last()
            .map(|a| a.run.id.clone())
            .or_else(|| p.current_run.as_ref().map(|r| r.id.clone()))
            .unwrap_or_default();
        CancelProjectOutcome::Cancelling(run_id)
    }
}

/// One currently active pipeline run on a project.
#[derive(Debug, Clone)]
pub struct ActiveRun {
    pub run: Run,
    pub cancel: Arc<AtomicBool>,
    pub slot: usize,
}

/// A reserved slot for a trigger/poll run before `pipeline::run_pipeline_spec` emits its `Run`.
#[derive(Debug, Clone)]
struct PendingSlot {
    slot: usize,
    cancel: Arc<AtomicBool>,
    sha: Option<String>,
    git_ref: Option<String>,
    pr_number: Option<u64>,
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

/// Insert or refresh an open PR from a webhook. A new head clears the stale result; the last
/// tested sha stays so the poller still knows what it built. Returns whether anything changed.
pub fn upsert_open_pr(
    prs: &mut Vec<PrSummary>,
    number: u64,
    title: &str,
    head_ref: &str,
    base_ref: &str,
    head_sha: &str,
) -> bool {
    let fresh = PrSummary {
        number,
        title: title.to_string(),
        head_ref: head_ref.to_string(),
        base_ref: base_ref.to_string(),
        head_sha: head_sha.to_string(),
        last_tested_sha: None,
        status: None,
    };
    match prs.iter_mut().find(|p| p.number == number) {
        Some(entry) => {
            let next = PrSummary {
                // An empty webhook field keeps what the poller knew.
                title: if title.is_empty() {
                    entry.title.clone()
                } else {
                    fresh.title
                },
                base_ref: if base_ref.is_empty() {
                    entry.base_ref.clone()
                } else {
                    fresh.base_ref
                },
                last_tested_sha: entry.last_tested_sha.clone(),
                status: if entry.head_sha == head_sha {
                    entry.status
                } else {
                    None
                },
                ..fresh
            };
            let changed = *entry != next;
            *entry = next;
            changed
        }
        None => {
            prs.push(fresh);
            true
        }
    }
}

pub struct ProjectState {
    pub name: String,
    pub branch: String,
    pub github_repo: Option<String>,
    pub upstream_sha: Option<String>,
    pub last_sha: Option<String>,
    pub running: bool,
    pub current_run: Option<Run>,
    pub active_runs: Vec<ActiveRun>,
    pending_slots: Vec<PendingSlot>,
    cancel_generation: Arc<AtomicU64>,
    pub last_run: Option<Run>,
    /// Errors from the last poll (fetch failure and friends).
    pub last_error: Option<String>,
    /// Cancellation flag for the latest/primary running pipeline (if any).
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
            active_runs: Vec::new(),
            pending_slots: Vec::new(),
            cancel_generation: Arc::new(AtomicU64::new(0)),
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

    /// Number of currently active (running or starting) runs on this project.
    pub fn active_run_count(&self) -> usize {
        let tracked = self.active_runs.len() + self.pending_slots.len();
        if tracked > 0 {
            tracked
        } else if self.running {
            1
        } else {
            0
        }
    }

    fn next_free_slot(&self, busy_slots: &[usize]) -> usize {
        let mut slot = 0usize;
        while self.active_runs.iter().any(|a| a.slot == slot)
            || self.pending_slots.iter().any(|ps| ps.slot == slot)
            || busy_slots.contains(&slot)
        {
            slot += 1;
        }
        slot
    }

    /// Atomically reserve the next free pending slot if the project is below `max_concurrent`.
    pub fn try_reserve_pending_slot(
        &mut self,
        max_concurrent: usize,
        sha: Option<String>,
        git_ref: Option<String>,
        pr_number: Option<u64>,
    ) -> Option<(usize, Arc<AtomicBool>)> {
        if self.active_run_count() >= max_concurrent.max(1) {
            return None;
        }
        let slot = self.next_free_slot(&[]);
        let cancel = Arc::new(AtomicBool::new(false));
        self.pending_slots.push(PendingSlot {
            slot,
            cancel: cancel.clone(),
            sha,
            git_ref,
            pr_number,
        });
        self.sync_compat();
        Some((slot, cancel))
    }

    /// Release a previously reserved pending slot (e.g. if channel send failed or queued trigger was cancelled).
    pub fn release_pending_slot(&mut self, slot: usize) {
        self.pending_slots.retain(|ps| ps.slot != slot);
        self.sync_compat();
    }

    fn sync_compat(&mut self) {
        self.running = !self.active_runs.is_empty() || !self.pending_slots.is_empty();
        self.current_run = self.active_runs.last().map(|a| a.run.clone());
        self.cancel = self
            .active_runs
            .last()
            .map(|a| a.cancel.clone())
            .or_else(|| self.pending_slots.last().map(|ps| ps.cancel.clone()));
    }

    fn register_pending_slot(
        &mut self,
        slot: usize,
        cancel: Arc<AtomicBool>,
        sha: Option<String>,
        git_ref: Option<String>,
        pr_number: Option<u64>,
    ) {
        self.pending_slots.retain(|ps| ps.slot != slot);
        self.active_runs.retain(|a| a.slot != slot);
        self.pending_slots.push(PendingSlot {
            slot,
            cancel,
            sha,
            git_ref,
            pr_number,
        });
        self.sync_compat();
    }

    fn update_pending_slot(
        &mut self,
        slot: usize,
        sha: Option<String>,
        git_ref: Option<String>,
        pr_number: Option<u64>,
    ) {
        if let Some(ps) = self.pending_slots.iter_mut().find(|ps| ps.slot == slot) {
            if sha.is_some() {
                ps.sha = sha;
            }
            if git_ref.is_some() {
                ps.git_ref = git_ref;
            }
            if pr_number.is_some() {
                ps.pr_number = pr_number;
            }
        }
    }

    fn upsert_active_run(&mut self, slot: usize, cancel: Arc<AtomicBool>, run: Run) {
        self.pending_slots.retain(|ps| ps.slot != slot);
        if let Some(existing) = self.active_runs.iter_mut().find(|a| a.slot == slot) {
            existing.run = run;
            existing.cancel = cancel;
        } else {
            self.active_runs.push(ActiveRun { run, cancel, slot });
        }
        self.sync_compat();
    }

    fn finish_slot(&mut self, slot: usize) {
        self.pending_slots.retain(|ps| ps.slot != slot);
        self.active_runs.retain(|a| a.slot != slot);
        self.sync_compat();
    }

    fn is_branch_sha_active(&self, project: &ProjectCfg, sha: &str) -> bool {
        self.active_runs.iter().any(|a| {
            a.run.pr_number.is_none()
                && a.run.sha == sha
                && a.run
                    .git_ref
                    .as_deref()
                    .is_none_or(|r| pipeline::is_default_branch_ref(project, r))
        }) || self.pending_slots.iter().any(|ps| {
            ps.pr_number.is_none()
                && ps.sha.as_deref() == Some(sha)
                && ps
                    .git_ref
                    .as_deref()
                    .is_none_or(|r| pipeline::is_default_branch_ref(project, r))
        })
    }

    fn is_pr_sha_active(&self, pr_number: u64, sha: &str) -> bool {
        self.active_runs
            .iter()
            .any(|a| a.run.pr_number == Some(pr_number) && a.run.sha == sha)
            || self
                .pending_slots
                .iter()
                .any(|ps| ps.pr_number == Some(pr_number) && ps.sha.as_deref() == Some(sha))
    }

    fn is_tag_ref_active(&self, tag_ref: &str) -> bool {
        self.active_runs
            .iter()
            .any(|a| a.run.git_ref.as_deref() == Some(tag_ref))
            || self
                .pending_slots
                .iter()
                .any(|ps| ps.git_ref.as_deref() == Some(tag_ref))
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
        trigger_reason: Option<String>,
        reserved: Option<(usize, Arc<AtomicBool>)>,
    },
    /// Discard any queued triggers waiting on this project worker and release their reserved slots.
    CancelQueued,
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
        let gate = Arc::new(ConcurrencyGate::new(cfg.general.max_concurrent_runs));
        let shared = Arc::new(Mutex::new(Shared {
            config: cfg,
            state_dir,
            projects,
            shutdown: false,
            webhook_stats: WebhookStats::default(),
            ws: WsHub::new(),
            concurrency_gate: gate,
            state_io_lock: Arc::new(Mutex::new(())),
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
            shared
                .concurrency_gate
                .set_limit(cfg.general.max_concurrent_runs);
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
    trigger_reason: Option<String>,
    reserved: Option<(usize, Arc<AtomicBool>)>,
}

struct RunningTask {
    slot: usize,
    handle: std::thread::JoinHandle<()>,
    cancel: Arc<AtomicBool>,
}

struct SlotGuard {
    shared: Arc<Mutex<Shared>>,
    project_name: String,
    slot: usize,
    disarmed: bool,
}

impl SlotGuard {
    fn disarm(&mut self) {
        self.disarmed = true;
    }
}

impl Drop for SlotGuard {
    fn drop(&mut self) {
        if self.disarmed {
            return;
        }
        let ws = if let Ok(mut s) = self.shared.lock() {
            if let Some(p) = s.projects.iter_mut().find(|p| p.name == self.project_name) {
                p.finish_slot(self.slot);
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

fn reap_finished(running: &mut Vec<RunningTask>) {
    let mut i = 0;
    while i < running.len() {
        if running[i].handle.is_finished() {
            let task = running.remove(i);
            let _ = task.handle.join();
        } else {
            i += 1;
        }
    }
}

fn stop_running_tasks(running: &mut Vec<RunningTask>) {
    for task in running.iter() {
        task.cancel.store(true, Ordering::SeqCst);
    }
    let deadline = Instant::now() + Duration::from_secs(15);
    while !running.is_empty() && Instant::now() < deadline {
        reap_finished(running);
        if !running.is_empty() {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

fn register_pending_slot_shared(
    shared: &Arc<Mutex<Shared>>,
    project_name: &str,
    slot: usize,
    cancel: &Arc<AtomicBool>,
    sha: Option<String>,
    git_ref: Option<String>,
    pr_number: Option<u64>,
) {
    let ws = if let Ok(mut s) = shared.lock() {
        if let Some(p) = s.projects.iter_mut().find(|p| p.name == project_name) {
            p.register_pending_slot(slot, cancel.clone(), sha, git_ref, pr_number);
        }
        Some(s.ws.clone())
    } else {
        None
    };
    if let Some(ws) = ws {
        ws.emit_projects_updated();
    }
}

fn allocate_pending_slot_shared(
    shared: &Arc<Mutex<Shared>>,
    project_name: &str,
    busy_slots: &[usize],
    sha: Option<String>,
    git_ref: Option<String>,
    pr_number: Option<u64>,
) -> (usize, Arc<AtomicBool>) {
    let cancel = Arc::new(AtomicBool::new(false));
    let mut slot = 0usize;
    let ws = if let Ok(mut s) = shared.lock() {
        if let Some(p) = s.projects.iter_mut().find(|p| p.name == project_name) {
            slot = p.next_free_slot(busy_slots);
            p.register_pending_slot(slot, cancel.clone(), sha, git_ref, pr_number);
        } else {
            while busy_slots.contains(&slot) {
                slot += 1;
            }
        }
        Some(s.ws.clone())
    } else {
        while busy_slots.contains(&slot) {
            slot += 1;
        }
        None
    };
    if let Some(ws) = ws {
        ws.emit_projects_updated();
    }
    (slot, cancel)
}

fn try_allocate_pending_slot_shared(
    shared: &Arc<Mutex<Shared>>,
    project_name: &str,
    max_concurrent: usize,
    busy_slots: &[usize],
    sha: Option<String>,
    git_ref: Option<String>,
    pr_number: Option<u64>,
) -> Option<(usize, Arc<AtomicBool>)> {
    let mut ws = None;
    let reserved = if let Ok(mut s) = shared.lock() {
        let res = s
            .projects
            .iter_mut()
            .find(|p| p.name == project_name)
            .and_then(|p| {
                if p.active_run_count() >= max_concurrent.max(1) {
                    return None;
                }
                let slot = p.next_free_slot(busy_slots);
                let cancel = Arc::new(AtomicBool::new(false));
                p.register_pending_slot(slot, cancel.clone(), sha, git_ref, pr_number);
                Some((slot, cancel))
            });
        if res.is_some() {
            ws = Some(s.ws.clone());
        }
        res
    } else {
        None
    };
    if let Some(ws) = ws {
        ws.emit_projects_updated();
    }
    reserved
}

fn update_pending_slot_shared(
    shared: &Arc<Mutex<Shared>>,
    project_name: &str,
    slot: usize,
    sha: Option<String>,
    git_ref: Option<String>,
    pr_number: Option<u64>,
) {
    if let Ok(mut s) = shared.lock() {
        if let Some(p) = s.projects.iter_mut().find(|p| p.name == project_name) {
            p.update_pending_slot(slot, sha, git_ref, pr_number);
        }
    }
}

fn release_reserved_slot_shared(shared: &Arc<Mutex<Shared>>, project_name: &str, slot: usize) {
    let ws = if let Ok(mut s) = shared.lock() {
        if let Some(p) = s.projects.iter_mut().find(|p| p.name == project_name) {
            p.release_pending_slot(slot);
        }
        Some(s.ws.clone())
    } else {
        None
    };
    if let Some(ws) = ws {
        ws.emit_projects_updated();
    }
}

fn discard_queued_triggers(
    shared: &Arc<Mutex<Shared>>,
    project_name: &str,
    queued: &mut VecDeque<QueuedTrigger>,
) {
    if queued.is_empty() {
        return;
    }
    let slots: Vec<usize> = queued
        .drain(..)
        .filter_map(|q| q.reserved.map(|(slot, _)| slot))
        .collect();
    if slots.is_empty() {
        return;
    }
    let ws = if let Ok(mut s) = shared.lock() {
        if let Some(p) = s.projects.iter_mut().find(|p| p.name == project_name) {
            for slot in slots {
                p.release_pending_slot(slot);
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

#[allow(clippy::too_many_arguments)]
fn spawn_queued_trigger_task(
    shared: &Arc<Mutex<Shared>>,
    cfg: Config,
    project: ProjectCfg,
    mut q: QueuedTrigger,
    deploy_lock: &Arc<Mutex<()>>,
    repo_git_lock: &Arc<Mutex<()>>,
    shutdown: &Arc<AtomicBool>,
    running_tasks: &mut Vec<RunningTask>,
) {
    let (slot, cancel) = match q.reserved.take() {
        Some((slot, cancel)) => {
            if cancel.load(Ordering::SeqCst) {
                release_reserved_slot_shared(shared, &project.name, slot);
                return;
            }
            (slot, cancel)
        }
        None => {
            let busy: Vec<usize> = running_tasks.iter().map(|t| t.slot).collect();
            allocate_pending_slot_shared(
                shared,
                &project.name,
                &busy,
                q.sha.clone(),
                q.git_ref.clone(),
                q.pr.as_ref().map(|p| p.number),
            )
        }
    };

    let shared_clone = shared.clone();
    let deploy_lock = deploy_lock.clone();
    let repo_git_lock = repo_git_lock.clone();
    let shutdown = shutdown.clone();
    let cancel_clone = cancel.clone();
    let thread_name = format!("lunacy-{}-run-{slot}", project.name);
    let handle = std::thread::Builder::new()
        .name(thread_name)
        .spawn(move || {
            let mut guard = SlotGuard {
                shared: shared_clone.clone(),
                project_name: project.name.clone(),
                slot,
                disarmed: false,
            };
            handle_queued_trigger_with_slot(
                &shared_clone,
                &cfg,
                &project,
                q,
                slot,
                cancel_clone,
                Some(&mut guard),
                &deploy_lock,
                &repo_git_lock,
                &shutdown,
            );
        })
        .expect("spawning concurrent project run thread");
    running_tasks.push(RunningTask {
        slot,
        handle,
        cancel,
    });
}

type RunDoneCallback = Box<dyn FnOnce(&Run) + Send>;

#[allow(clippy::too_many_arguments)]
fn spawn_spec_task(
    shared: &Arc<Mutex<Shared>>,
    cfg: Config,
    project: ProjectCfg,
    spec: RunSpec,
    deploy_lock: &Arc<Mutex<()>>,
    repo_git_lock: &Arc<Mutex<()>>,
    shutdown: &Arc<AtomicBool>,
    running_tasks: &mut Vec<RunningTask>,
    on_done: Option<RunDoneCallback>,
) {
    let busy: Vec<usize> = running_tasks.iter().map(|t| t.slot).collect();
    let project_max = project.effective_max_concurrent_runs();
    let Some((slot, cancel)) = try_allocate_pending_slot_shared(
        shared,
        &project.name,
        project_max,
        &busy,
        Some(spec.sha.clone()),
        Some(spec.git_ref.clone()),
        spec.pr.as_ref().map(|p| p.number),
    ) else {
        return;
    };

    let shared_clone = shared.clone();
    let deploy_lock = deploy_lock.clone();
    let repo_git_lock = repo_git_lock.clone();
    let shutdown = shutdown.clone();
    let cancel_clone = cancel.clone();
    let thread_name = format!("lunacy-{}-run-{slot}", project.name);
    let handle = std::thread::Builder::new()
        .name(thread_name)
        .spawn(move || {
            let mut guard = SlotGuard {
                shared: shared_clone.clone(),
                project_name: project.name.clone(),
                slot,
                disarmed: false,
            };
            if let Some(run) = execute_spec_with_slot(
                &shared_clone,
                &cfg,
                &project,
                &spec,
                slot,
                cancel_clone,
                Some(&mut guard),
                &deploy_lock,
                &repo_git_lock,
                &shutdown,
            ) {
                if let Some(cb) = on_done {
                    cb(&run);
                }
            }
        })
        .expect("spawning concurrent project run thread");
    running_tasks.push(RunningTask {
        slot,
        handle,
        cancel,
    });
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
    let deploy_lock = Arc::new(Mutex::new(()));
    let repo_git_lock = Arc::new(Mutex::new(()));
    let mut running_tasks: Vec<RunningTask> = Vec::new();
    let mut seen_cancel_gen: u64 = shared
        .lock()
        .ok()
        .and_then(|s| {
            s.projects
                .iter()
                .find(|p| p.name == name)
                .map(|p| p.cancel_generation.load(Ordering::SeqCst))
        })
        .unwrap_or(0);

    loop {
        reap_finished(&mut running_tasks);

        if shutdown.load(std::sync::atomic::Ordering::SeqCst) {
            discard_queued_triggers(&shared, &name, &mut queued);
            stop_running_tasks(&mut running_tasks);
            break;
        }

        // If cancel_project() was invoked directly on Shared, discard any previously queued triggers.
        let cur_cancel_gen = shared
            .lock()
            .ok()
            .and_then(|s| {
                s.projects
                    .iter()
                    .find(|p| p.name == name)
                    .map(|p| p.cancel_generation.load(Ordering::SeqCst))
            })
            .unwrap_or(seen_cancel_gen);
        if cur_cancel_gen != seen_cancel_gen {
            seen_cancel_gen = cur_cancel_gen;
            discard_queued_triggers(&shared, &name, &mut queued);
        }

        // Config reload: take the new definition of this project (and daemon-wide settings).
        while let Ok(cmd) = rx.try_recv() {
            match cmd {
                Command::Stop => {
                    discard_queued_triggers(&shared, &name, &mut queued);
                    stop_running_tasks(&mut running_tasks);
                    return;
                }
                Command::Reload => want_reload = true,
                Command::CancelQueued => {
                    discard_queued_triggers(&shared, &name, &mut queued);
                }
                Command::Trigger {
                    sha,
                    force,
                    trigger,
                    git_ref,
                    pr,
                    deploy_only,
                    force_deploy,
                    trigger_reason,
                    reserved,
                } => {
                    if let Some((slot, ref cancel)) = reserved {
                        if cancel.load(Ordering::SeqCst) {
                            release_reserved_slot_shared(&shared, &name, slot);
                            continue;
                        }
                    }
                    queued.push_back(QueuedTrigger {
                        sha,
                        force,
                        trigger,
                        git_ref,
                        pr,
                        deploy_only,
                        force_deploy,
                        trigger_reason,
                        reserved,
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
                            s.concurrency_gate
                                .set_limit(new_cfg.general.max_concurrent_runs);
                            s.config = new_cfg.clone();
                        }
                        log::info!("[{name}] configuration reloaded");
                    }
                    None => {
                        discard_queued_triggers(&shared, &name, &mut queued);
                        stop_running_tasks(&mut running_tasks);
                        return;
                    }
                },
                Err(e) => log::warn!("[{name}] config reload failed, keeping the old one: {e:#}"),
            }
        }

        let project_max = project.effective_max_concurrent_runs();

        // A queued run (API trigger / webhook / force deploy) runs before polling.
        if !queued.is_empty() && running_tasks.len() < project_max {
            if project_max == 1 && running_tasks.is_empty() {
                let mut q = queued.pop_front().expect("non-empty queued");
                let (slot, cancel) = match q.reserved.take() {
                    Some((slot, cancel)) => {
                        if cancel.load(Ordering::SeqCst) {
                            release_reserved_slot_shared(&shared, &name, slot);
                            continue;
                        }
                        (slot, cancel)
                    }
                    None => allocate_pending_slot_shared(
                        &shared,
                        &name,
                        &[],
                        q.sha.clone(),
                        q.git_ref.clone(),
                        q.pr.as_ref().map(|p| p.number),
                    ),
                };
                let mut guard = SlotGuard {
                    shared: shared.clone(),
                    project_name: name.clone(),
                    slot,
                    disarmed: false,
                };
                let cfg = shared.lock().expect("shared").config.clone();
                handle_queued_trigger_with_slot(
                    &shared,
                    &cfg,
                    &project,
                    q,
                    slot,
                    cancel,
                    Some(&mut guard),
                    &deploy_lock,
                    &repo_git_lock,
                    &shutdown,
                );
                last_poll = Instant::now();
                continue;
            }
            while running_tasks.len() < project_max {
                let Some(q) = queued.pop_front() else {
                    break;
                };
                if let Some((slot, ref cancel)) = q.reserved {
                    if cancel.load(Ordering::SeqCst) {
                        release_reserved_slot_shared(&shared, &name, slot);
                        continue;
                    }
                }
                let cfg = shared.lock().expect("shared").config.clone();
                spawn_queued_trigger_task(
                    &shared,
                    cfg,
                    project.clone(),
                    q,
                    &deploy_lock,
                    &repo_git_lock,
                    &shutdown,
                    &mut running_tasks,
                );
                last_poll = Instant::now();
            }
            continue;
        }

        let cfg = shared.lock().expect("shared").config.clone();
        let paused = !project.is_enabled();
        let webhook_only = project.effective_trigger_mode(&cfg.general) == TriggerMode::Webhook;
        let interval = Duration::from_secs(project.poll_interval(&cfg.general).max(1));
        let at_capacity = running_tasks.len() >= project_max;
        if at_capacity
            || ((paused || webhook_only || last_poll.elapsed() < interval) && queued.is_empty())
        {
            let wait_ms = if running_tasks.is_empty() { 250 } else { 100 };
            match rx.recv_timeout(Duration::from_millis(wait_ms)) {
                Ok(Command::Stop) => {
                    discard_queued_triggers(&shared, &name, &mut queued);
                    stop_running_tasks(&mut running_tasks);
                    return;
                }
                Ok(Command::Reload) => {
                    want_reload = true;
                    continue;
                }
                Ok(Command::CancelQueued) => {
                    discard_queued_triggers(&shared, &name, &mut queued);
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
                    trigger_reason,
                    reserved,
                }) => {
                    if let Some((slot, ref cancel)) = reserved {
                        if cancel.load(Ordering::SeqCst) {
                            release_reserved_slot_shared(&shared, &name, slot);
                            continue;
                        }
                    }
                    queued.push_back(QueuedTrigger {
                        sha,
                        force,
                        trigger,
                        git_ref,
                        pr,
                        deploy_only,
                        force_deploy,
                        trigger_reason,
                        reserved,
                    });
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    discard_queued_triggers(&shared, &name, &mut queued);
                    stop_running_tasks(&mut running_tasks);
                    return;
                }
            }
            continue;
        }
        last_poll = Instant::now();

        // 1. Poll the default watched branch.
        if let Some(sha) = fetch_upstream(&cfg, &project, &shared, &repo_git_lock) {
            let (last, already_active) = shared
                .lock()
                .ok()
                .and_then(|s| {
                    s.projects
                        .iter()
                        .find(|p| p.name == name)
                        .map(|p| (p.last_sha.clone(), p.is_branch_sha_active(&project, &sha)))
                })
                .unwrap_or((None, false));
            if last.as_deref() == Some(sha.as_str()) || already_active {
                log::debug!("[{name}] {} unchanged", crate::model::short_sha(&sha));
            } else {
                let spec = RunSpec {
                    sha,
                    trigger: Trigger::Poll,
                    git_ref: project.branch.clone(),
                    pr: None,
                    deploy_only: false,
                    force_deploy: false,
                    trigger_reason: Some(format!("Poll detected new commit on {}", project.branch)),
                };
                if project_max == 1 && running_tasks.is_empty() {
                    if let Some((slot, cancel)) = try_allocate_pending_slot_shared(
                        &shared,
                        &name,
                        project_max,
                        &[],
                        Some(spec.sha.clone()),
                        Some(spec.git_ref.clone()),
                        None,
                    ) {
                        let mut guard = SlotGuard {
                            shared: shared.clone(),
                            project_name: name.clone(),
                            slot,
                            disarmed: false,
                        };
                        let _ = execute_spec_with_slot(
                            &shared,
                            &cfg,
                            &project,
                            &spec,
                            slot,
                            cancel,
                            Some(&mut guard),
                            &deploy_lock,
                            &repo_git_lock,
                            &shutdown,
                        );
                    }
                } else if running_tasks.len() < project_max {
                    spawn_spec_task(
                        &shared,
                        cfg.clone(),
                        project.clone(),
                        spec,
                        &deploy_lock,
                        &repo_git_lock,
                        &shutdown,
                        &mut running_tasks,
                        None,
                    );
                }
            }
        }

        if shutdown.load(std::sync::atomic::Ordering::SeqCst) {
            discard_queued_triggers(&shared, &name, &mut queued);
            stop_running_tasks(&mut running_tasks);
            break;
        }

        // 2. Poll open GitHub PRs (CI-only) when `watch_prs` is enabled.
        if project.watch_prs {
            poll_open_prs(
                &shared,
                &cfg,
                &project,
                &shutdown,
                &deploy_lock,
                &repo_git_lock,
                &mut running_tasks,
            );
        }

        if shutdown.load(std::sync::atomic::Ordering::SeqCst) {
            discard_queued_triggers(&shared, &name, &mut queued);
            stop_running_tasks(&mut running_tasks);
            break;
        }

        // 3. Poll remote tags when `watch_tags` or tag `deploy_refs` are configured.
        if project.watches_tags() {
            poll_tags(
                &shared,
                &cfg,
                &project,
                &shutdown,
                &deploy_lock,
                &repo_git_lock,
                &mut running_tasks,
            );
        }
    }
    log::info!("[{name}] worker exiting");
}

#[allow(clippy::too_many_arguments)]
fn handle_queued_trigger_with_slot(
    shared: &Arc<Mutex<Shared>>,
    cfg: &Config,
    project: &ProjectCfg,
    q: QueuedTrigger,
    slot: usize,
    cancel: Arc<AtomicBool>,
    slot_guard: Option<&mut SlotGuard>,
    deploy_lock: &Arc<Mutex<()>>,
    repo_git_lock: &Arc<Mutex<()>>,
    shutdown: &AtomicBool,
) {
    if cancel.load(Ordering::SeqCst) || shutdown.load(Ordering::SeqCst) {
        log::info!(
            "[{}] queued trigger cancelled before SHA resolution",
            project.name
        );
        return;
    }

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
        repo_git_lock,
    );
    if cancel.load(Ordering::SeqCst) || shutdown.load(Ordering::SeqCst) {
        log::info!(
            "[{}] queued trigger cancelled during SHA resolution",
            project.name
        );
        return;
    }
    let Some(target) = target.filter(|s| !s.is_empty()) else {
        log::warn!("[{name}] trigger with no resolvable sha; ignoring");
        return;
    };

    update_pending_slot_shared(
        shared,
        name,
        slot,
        Some(target.clone()),
        Some(git_ref.clone()),
        q.pr.as_ref().map(|p| p.number),
    );

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
        trigger_reason: q.trigger_reason,
    };
    let _ = execute_spec_with_slot(
        shared,
        cfg,
        project,
        &spec,
        slot,
        cancel,
        slot_guard,
        deploy_lock,
        repo_git_lock,
        shutdown,
    );
}

#[allow(clippy::too_many_arguments)]
fn resolve_queued_sha(
    shared: &Arc<Mutex<Shared>>,
    cfg: &Config,
    project: &ProjectCfg,
    git_ref: &str,
    pr: Option<&PrContext>,
    sha: Option<String>,
    trigger: Trigger,
    repo_git_lock: &Mutex<()>,
) -> Option<String> {
    let sha = sha.filter(|s| !s.trim().is_empty());
    if let Some(pr) = pr {
        let hint = sha.as_deref().unwrap_or("");
        let _git = repo_git_lock.lock().unwrap_or_else(|e| e.into_inner());
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
        let _git = repo_git_lock.lock().unwrap_or_else(|e| e.into_inner());
        if let Err(e) = git::fetch_tag(&project.repo_path, &project.remote, short) {
            log::warn!("[{}] fetch tag '{short}' failed: {e:#}", project.name);
        }
        return sha
            .or_else(|| git::rev_parse(&project.repo_path, &format!("refs/tags/{short}")).ok());
    }

    if !pipeline::is_default_branch_ref(project, git_ref) {
        let branch = git_ref.strip_prefix("refs/heads/").unwrap_or(git_ref);
        let _git = repo_git_lock.lock().unwrap_or_else(|e| e.into_inner());
        if let Err(e) = git::fetch(&project.repo_path, &project.remote, branch) {
            log::warn!("[{}] fetch branch '{branch}' failed: {e:#}", project.name);
        }
        return sha.or_else(|| git::upstream_sha(&project.repo_path, &project.remote, branch).ok());
    }

    match sha {
        Some(s) => {
            let missing_locally = {
                let _git = repo_git_lock.lock().unwrap_or_else(|e| e.into_inner());
                git::rev_parse(&project.repo_path, &s).is_err()
            };
            if trigger == Trigger::Webhook || missing_locally {
                let _ = fetch_upstream(cfg, project, shared, repo_git_lock);
            }
            Some(s)
        }
        None => fetch_upstream(cfg, project, shared, repo_git_lock),
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

pub(crate) use crate::pipeline::resolve_github_repo;

impl From<&ProjectState> for crate::deploy_check::ProjectSnapshot {
    fn from(p: &ProjectState) -> Self {
        crate::deploy_check::ProjectSnapshot {
            name: p.name.clone(),
            upstream_sha: p.upstream_sha.clone(),
            last_sha: p.last_sha.clone(),
            current_run: p.current_run.clone(),
            last_run: p.last_run.clone(),
        }
    }
}

fn poll_open_prs(
    shared: &Arc<Mutex<Shared>>,
    cfg: &Config,
    project: &ProjectCfg,
    shutdown: &Arc<AtomicBool>,
    deploy_lock: &Arc<Mutex<()>>,
    repo_git_lock: &Arc<Mutex<()>>,
    running_tasks: &mut Vec<RunningTask>,
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

    let (state_dir, state_io_lock) = {
        let s = shared.lock().expect("shared");
        (s.state_dir.clone(), s.state_io_lock.clone())
    };
    let seen = {
        let _io = state_io_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut seen = read_pr_shas(&state_dir, &project.name);
        let open_numbers: BTreeSet<u64> = pulls.iter().map(|p| p.number).collect();
        let old_len = seen.len();
        seen.retain(|num, _| open_numbers.contains(num));
        if seen.len() != old_len {
            if let Err(e) = write_pr_shas(&state_dir, &project.name, &seen) {
                log::warn!("[{}] writing pruned PR shas failed: {e}", project.name);
            }
        }
        seen
    };

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

    let project_max = project.effective_max_concurrent_runs();
    for pr in pulls {
        reap_finished(running_tasks);
        if shutdown.load(Ordering::SeqCst) {
            break;
        }
        if seen.get(&pr.number).map(String::as_str) == Some(pr.head_sha.as_str()) {
            continue;
        }
        let already_active = shared.lock().ok().is_some_and(|s| {
            s.projects
                .iter()
                .find(|p| p.name == project.name)
                .is_some_and(|p| p.is_pr_sha_active(pr.number, &pr.head_sha))
        });
        if already_active {
            continue;
        }
        if project_max > 1 && running_tasks.len() >= project_max {
            break;
        }
        let fetch_res = {
            let _git = repo_git_lock.lock().unwrap_or_else(|e| e.into_inner());
            git::fetch_pr(&project.repo_path, &project.remote, pr.number, &pr.head_sha)
        };
        if let Err(e) = fetch_res {
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
            trigger_reason: Some(format!("Poll detected PR #{} update", pr.number)),
        };
        if project_max == 1 && running_tasks.is_empty() {
            if let Some((slot, cancel)) = try_allocate_pending_slot_shared(
                shared,
                &project.name,
                project_max,
                &[],
                Some(spec.sha.clone()),
                Some(spec.git_ref.clone()),
                Some(pr.number),
            ) {
                let mut guard = SlotGuard {
                    shared: shared.clone(),
                    project_name: project.name.clone(),
                    slot,
                    disarmed: false,
                };
                let _ = execute_spec_with_slot(
                    shared,
                    cfg,
                    project,
                    &spec,
                    slot,
                    cancel,
                    Some(&mut guard),
                    deploy_lock,
                    repo_git_lock,
                    shutdown,
                );
            }
        } else if running_tasks.len() < project_max {
            spawn_spec_task(
                shared,
                cfg.clone(),
                project.clone(),
                spec,
                deploy_lock,
                repo_git_lock,
                shutdown,
                running_tasks,
                None,
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn poll_tags(
    shared: &Arc<Mutex<Shared>>,
    cfg: &Config,
    project: &ProjectCfg,
    shutdown: &Arc<AtomicBool>,
    deploy_lock: &Arc<Mutex<()>>,
    repo_git_lock: &Arc<Mutex<()>>,
    running_tasks: &mut Vec<RunningTask>,
) {
    let remote_tags = match git::list_remote_tags(&project.repo_path, &project.remote) {
        Ok(t) => t,
        Err(e) => {
            log::warn!("[{}] listing remote tags failed: {e:#}", project.name);
            return;
        }
    };
    let (state_dir, state_io_lock) = {
        let s = shared.lock().expect("shared");
        (s.state_dir.clone(), s.state_io_lock.clone())
    };
    let remote_map: BTreeMap<String, String> = remote_tags.into_iter().collect();

    // First run: seed all existing tags without building them.
    if !seen_tags_path(&state_dir, &project.name).is_file() {
        let _io = state_io_lock.lock().unwrap_or_else(|e| e.into_inner());
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

    let mut seen = {
        let _io = state_io_lock.lock().unwrap_or_else(|e| e.into_inner());
        read_seen_tags(&state_dir, &project.name)
    };
    let mut dirty = false;
    let project_max = project.effective_max_concurrent_runs();
    for (tag, remote_sha) in remote_map {
        reap_finished(running_tasks);
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
        let already_active = shared.lock().ok().is_some_and(|s| {
            s.projects
                .iter()
                .find(|p| p.name == project.name)
                .is_some_and(|p| p.is_tag_ref_active(&tag_ref))
        });
        if already_active {
            continue;
        }
        if project_max > 1 && running_tasks.len() >= project_max {
            break;
        }
        let commit_sha = {
            let _git = repo_git_lock.lock().unwrap_or_else(|e| e.into_inner());
            if let Err(e) = git::fetch_tag(&project.repo_path, &project.remote, &tag) {
                log::warn!("[{}] fetch tag '{tag}' failed: {e:#}", project.name);
                continue;
            }
            git::rev_parse(&project.repo_path, &format!("refs/tags/{tag}"))
                .unwrap_or_else(|_| remote_sha.clone())
        };
        let spec = RunSpec {
            sha: commit_sha,
            trigger: Trigger::Tag,
            git_ref: tag_ref,
            pr: None,
            deploy_only: false,
            force_deploy: false,
            trigger_reason: Some(format!("Poll detected new tag {tag}")),
        };
        if project_max == 1 && running_tasks.is_empty() {
            if let Some((slot, cancel)) = try_allocate_pending_slot_shared(
                shared,
                &project.name,
                project_max,
                &[],
                Some(spec.sha.clone()),
                Some(spec.git_ref.clone()),
                None,
            ) {
                let mut guard = SlotGuard {
                    shared: shared.clone(),
                    project_name: project.name.clone(),
                    slot,
                    disarmed: false,
                };
                if execute_spec_with_slot(
                    shared,
                    cfg,
                    project,
                    &spec,
                    slot,
                    cancel,
                    Some(&mut guard),
                    deploy_lock,
                    repo_git_lock,
                    shutdown,
                )
                .is_some()
                {
                    let _io = state_io_lock.lock().unwrap_or_else(|e| e.into_inner());
                    let mut latest = read_seen_tags(&state_dir, &project.name);
                    latest.insert(tag.clone(), remote_sha.clone());
                    seen.insert(tag, remote_sha);
                    if let Err(e) = write_seen_tags(&state_dir, &project.name, &latest) {
                        log::warn!("[{}] persisting seen tags failed: {e}", project.name);
                    }
                    dirty = false;
                }
            }
        } else if running_tasks.len() < project_max {
            let io_lock = state_io_lock.clone();
            let sd = state_dir.clone();
            let proj_name = project.name.clone();
            let tag_done = tag.clone();
            let sha_done = remote_sha.clone();
            spawn_spec_task(
                shared,
                cfg.clone(),
                project.clone(),
                spec,
                deploy_lock,
                repo_git_lock,
                shutdown,
                running_tasks,
                Some(Box::new(move |_run| {
                    let _io = io_lock.lock().unwrap_or_else(|e| e.into_inner());
                    let mut latest = read_seen_tags(&sd, &proj_name);
                    latest.insert(tag_done, sha_done);
                    if let Err(e) = write_seen_tags(&sd, &proj_name, &latest) {
                        log::warn!("[{proj_name}] persisting seen tags failed: {e}");
                    }
                })),
            );
        }
    }
    if dirty {
        let _io = state_io_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut latest = read_seen_tags(&state_dir, &project.name);
        for (k, v) in seen {
            latest.entry(k).or_insert(v);
        }
        if let Err(e) = write_seen_tags(&state_dir, &project.name, &latest) {
            log::warn!("[{}] persisting seen tags failed: {e}", project.name);
        }
    }
}

/// Fetch and return the upstream sha, recording errors for the API.
fn fetch_upstream(
    cfg: &Config,
    project: &ProjectCfg,
    shared: &Arc<Mutex<Shared>>,
    repo_git_lock: &Mutex<()>,
) -> Option<String> {
    let fetch_result = {
        let _git = repo_git_lock.lock().unwrap_or_else(|e| e.into_inner());
        git::fetch(&project.repo_path, &project.remote, &project.branch)
            .and_then(|_| git::upstream_sha(&project.repo_path, &project.remote, &project.branch))
    };
    match fetch_result {
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
    if !is_default_branch {
        return false;
    }
    if let Some(up) = upstream_sha {
        if existing_last_sha == Some(up) && up != sha {
            return false;
        }
    }
    trigger == Trigger::Poll || existing_last_sha.is_none() || upstream_sha == Some(sha)
}

fn is_strict_ancestor_locked(
    repo_path: &Path,
    candidate_older: &str,
    existing: &str,
    repo_git_lock: &Mutex<()>,
) -> bool {
    if candidate_older == existing {
        return false;
    }
    let _git = repo_git_lock.lock().unwrap_or_else(|e| e.into_inner());
    git::is_ancestor(repo_path, candidate_older, existing).unwrap_or(false)
}

#[cfg(test)]
fn execute_spec(
    shared: &Arc<Mutex<Shared>>,
    cfg: &Config,
    project: &ProjectCfg,
    spec: &RunSpec,
) -> Run {
    let deploy_lock = Arc::new(Mutex::new(()));
    let repo_git_lock = Arc::new(Mutex::new(()));
    let shutdown = AtomicBool::new(false);
    execute_spec_with_slot(
        shared,
        cfg,
        project,
        spec,
        0,
        Arc::new(AtomicBool::new(false)),
        None,
        &deploy_lock,
        &repo_git_lock,
        &shutdown,
    )
    .expect("run executed")
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn execute_spec_with_slot(
    shared: &Arc<Mutex<Shared>>,
    cfg: &Config,
    project: &ProjectCfg,
    spec: &RunSpec,
    slot: usize,
    cancel: Arc<AtomicBool>,
    external_guard: Option<&mut SlotGuard>,
    deploy_lock: &Arc<Mutex<()>>,
    repo_git_lock: &Arc<Mutex<()>>,
    shutdown: &AtomicBool,
) -> Option<Run> {
    let (state_dir, ws, gate, state_io_lock) = {
        let s = shared.lock().expect("shared");
        (
            s.state_dir.clone(),
            s.ws.clone(),
            s.concurrency_gate.clone(),
            s.state_io_lock.clone(),
        )
    };

    let pr_number = spec.pr.as_ref().map(|p| p.number);
    let mut local_guard = if external_guard.is_none() {
        register_pending_slot_shared(
            shared,
            &project.name,
            slot,
            &cancel,
            Some(spec.sha.clone()),
            Some(spec.git_ref.clone()),
            pr_number,
        );
        Some(SlotGuard {
            shared: shared.clone(),
            project_name: project.name.clone(),
            slot,
            disarmed: false,
        })
    } else {
        update_pending_slot_shared(
            shared,
            &project.name,
            slot,
            Some(spec.sha.clone()),
            Some(spec.git_ref.clone()),
            pr_number,
        );
        None
    };

    // Acquire a global concurrency permit before starting the pipeline.
    let _permit = match gate.acquire(&cancel, shutdown) {
        Some(permit) => permit,
        None => {
            log::info!(
                "[{}] cancelled or shutting down while waiting for global concurrency slot",
                project.name
            );
            return None;
        }
    };

    if let Some(reason) = crate::model::sanitize_trigger_reason(spec.trigger_reason.as_deref()) {
        log::info!(
            "[{}] running pipeline for {} (ref={}, trigger={:?}, reason={})",
            project.name,
            crate::model::short_sha(&spec.sha),
            spec.git_ref,
            spec.trigger,
            reason
        );
    } else {
        log::info!(
            "[{}] running pipeline for {} (ref={}, trigger={:?})",
            project.name,
            crate::model::short_sha(&spec.sha),
            spec.git_ref,
            spec.trigger
        );
    }
    let github = if cfg.github_enabled_for(project) {
        github_client(cfg, &project.name)
    } else {
        None
    };
    let act_lock = cfg.act_lock_file();

    let is_default_branch =
        spec.pr.is_none() && pipeline::is_default_branch_ref(project, &spec.git_ref);
    let current_run_id: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
    let on_update = {
        let shared = shared.clone();
        let ws = ws.clone();
        let current_run_id = current_run_id.clone();
        let cancel_clone = cancel.clone();
        let trigger = spec.trigger;
        let repo_path = project.repo_path.clone();
        let repo_git_lock = repo_git_lock.clone();
        move |run: &Run| {
            if let Ok(mut id_guard) = current_run_id.lock() {
                if id_guard.is_empty() {
                    *id_guard = run.id.clone();
                }
            }
            let maybe_check = if let Ok(mut s) = shared.lock() {
                if let Some(p) = s.projects.iter_mut().find(|p| p.name == run.project) {
                    p.upsert_active_run(slot, cancel_clone.clone(), run.clone());
                    let should_advance = should_advance_last_sha(
                        is_default_branch,
                        trigger,
                        &run.sha,
                        p.last_sha.as_deref(),
                        p.upstream_sha.as_deref(),
                    );
                    if should_advance && (run.trigger == Trigger::Poll || run.status.is_final()) {
                        Some(p.last_sha.clone())
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            };
            if let Some(existing_last) = maybe_check {
                let is_older = existing_last.as_deref().is_some_and(|ex| {
                    is_strict_ancestor_locked(&repo_path, &run.sha, ex, &repo_git_lock)
                });
                if !is_older {
                    if let Ok(mut s) = shared.lock() {
                        if let Some(p) = s.projects.iter_mut().find(|p| p.name == run.project) {
                            if p.last_sha == existing_last {
                                p.last_sha = Some(run.sha.clone());
                            }
                        }
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
        deploy_lock: Some(deploy_lock.as_ref()),
        repo_git_lock: Some(repo_git_lock.as_ref()),
        project_run_slot: slot,
    };
    let run = pipeline::run_pipeline_spec(&ctx, project, spec);

    // State semantics: the last processed sha advances when a default-branch pipeline
    // ends on a poll, first run, or current upstream head — whatever the result, so a
    // failing commit is not retried forever. Historical rollbacks / force-deploys of an
    // older commit, PRs, and tags/non-default branches do NOT overwrite `last_sha`.
    // Out-of-order concurrent completions never regress `last_sha` or `pr_shas`/`open_prs`.
    {
        let _io = state_io_lock.lock().unwrap_or_else(|e| e.into_inner());
        let pr_is_older_ancestor = if let Some(pr) = &spec.pr {
            let existing_pr_sha = read_pr_shas(&state_dir, &project.name)
                .get(&pr.number)
                .cloned();
            existing_pr_sha.as_deref().is_some_and(|ex| {
                is_strict_ancestor_locked(&project.repo_path, &run.sha, ex, repo_git_lock)
            })
        } else {
            false
        };

        let (advance_now, record_pr_sha) = loop {
            let snapshot = shared.lock().ok().and_then(|s| {
                s.projects
                    .iter()
                    .find(|p| p.name == project.name)
                    .map(|p| (p.last_sha.clone(), p.upstream_sha.clone()))
            });
            let Some((cur_last, cur_up)) = snapshot else {
                break (false, false);
            };
            let prelim = should_advance_last_sha(
                is_default_branch,
                spec.trigger,
                &run.sha,
                cur_last.as_deref(),
                cur_up.as_deref(),
            );
            let can_advance = prelim
                && !cur_last.as_deref().is_some_and(|ex| {
                    is_strict_ancestor_locked(&project.repo_path, &run.sha, ex, repo_git_lock)
                });

            let mut retry = false;
            let result = if let Ok(mut s) = shared.lock() {
                if let Some(p) = s.projects.iter_mut().find(|p| p.name == project.name) {
                    if p.last_sha != cur_last {
                        retry = true;
                        (false, false)
                    } else {
                        if can_advance {
                            p.last_sha = Some(run.sha.clone());
                        }
                        let mut record_pr = false;
                        if let Some(pr) = &spec.pr {
                            if let Some(entry) =
                                p.open_prs.iter_mut().find(|e| e.number == pr.number)
                            {
                                if entry.head_sha == run.sha {
                                    entry.last_tested_sha = Some(run.sha.clone());
                                    entry.status = Some(run.status);
                                    record_pr = true;
                                }
                            } else if !pr_is_older_ancestor {
                                record_pr = true;
                            }
                        }
                        p.last_run = Some(run.clone());
                        p.finish_slot(slot);
                        (can_advance, record_pr)
                    }
                } else {
                    (false, false)
                }
            } else {
                (false, false)
            };
            if !retry {
                break result;
            }
        };

        if let Some(g) = external_guard {
            g.disarm();
        }
        if let Some(g) = local_guard.as_mut() {
            g.disarm();
        }

        if advance_now {
            if let Err(e) = write_last_sha(&state_dir, &project.name, &spec.sha) {
                log::warn!("[{}] persisting last sha failed: {e}", project.name);
            }
        } else if record_pr_sha {
            if let Some(pr) = &spec.pr {
                let mut seen = read_pr_shas(&state_dir, &project.name);
                seen.insert(pr.number, spec.sha.clone());
                if let Err(e) = write_pr_shas(&state_dir, &project.name, &seen) {
                    log::warn!("[{}] persisting PR shas failed: {e}", project.name);
                }
            }
        }
    }

    ws.emit_run_updated(&run);
    ws.emit_projects_updated();
    drop(_permit);
    Some(run)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Checkout, StepCfg, StepKind};

    #[test]
    fn webhook_pr_updates_track_the_new_head() {
        let mut prs = Vec::new();
        assert!(upsert_open_pr(&mut prs, 42, "t", "feat", "master", "c1"));
        prs[0].last_tested_sha = Some("c1".into());
        prs[0].status = Some(RunStatus::Success);
        // Same head again: nothing changes, the result stays.
        assert!(!upsert_open_pr(&mut prs, 42, "t", "feat", "master", "c1"));
        assert_eq!(prs[0].status, Some(RunStatus::Success));
        // New head (the B4-R1 case): tracked now, so the finished run can record it.
        assert!(upsert_open_pr(&mut prs, 42, "", "feat", "", "c2"));
        assert_eq!(prs.len(), 1);
        assert_eq!(prs[0].head_sha, "c2");
        assert_eq!(prs[0].status, None);
        assert_eq!(prs[0].last_tested_sha.as_deref(), Some("c1"));
        assert_eq!(
            (prs[0].title.as_str(), prs[0].base_ref.as_str()),
            ("t", "master")
        );
    }

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
            max_concurrent_runs: None,
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
            ..Shared::default()
        }));

        // Force-deploy older commit (rollback): must NOT rewind last_sha on disk or in memory.
        let rollback_spec = RunSpec {
            sha: old_sha.clone(),
            trigger: Trigger::ForceDeploy,
            git_ref: "master".into(),
            pr: None,
            deploy_only: true,
            force_deploy: true,
            trigger_reason: Some("Force deploy".into()),
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
            max_concurrent_runs: None,
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
            trigger_reason: Some("GitHub push webhook (master by octocat)".into()),
            reserved: None,
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
            max_concurrent_runs: None,
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
            trigger_reason: Some("Manual API trigger (force)".into()),
            reserved: None,
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
        assert_eq!(
            runs[0].trigger_reason.as_deref(),
            Some("Manual API trigger (force)")
        );
        assert_eq!(runs[0].sha, sha1);

        daemon.shutdown();
    }

    #[cfg(unix)]
    #[test]
    fn poller_populates_trigger_reason_for_branch_and_tag_runs() {
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
        run_git(&work, &["tag", "v1.0.0"]);
        run_git(&work, &["push", "origin", "refs/tags/v1.0.0"]);

        std::fs::write(work.join("a.txt"), "v2\n").unwrap();
        run_git(&work, &["add", "a.txt"]);
        run_git(&work, &["commit", "-m", "v2"]);
        run_git(&work, &["push", "origin", "master"]);

        // Pre-seed seen_tags empty so the pushed tag is picked up by poll_tags.
        write_seen_tags(&state_dir, "poll-demo", &BTreeMap::new()).unwrap();

        let step = StepCfg {
            name: "Echo".into(),
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
            name: "poll-demo".into(),
            repo_path: work.clone(),
            remote: "origin".into(),
            branch: "master".into(),
            enabled: None,
            github_repo: None,
            github_enabled: None,
            checkout: Checkout::None,
            poll_interval: Some(1),
            trigger_mode: Some(TriggerMode::Poll),
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
            watch_tags: true,
            cache: Some(false),
            max_concurrent_runs: None,
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

        let deadline = Instant::now() + Duration::from_secs(10);
        let mut found_poll = false;
        let mut found_tag = false;
        while Instant::now() < deadline {
            let runs = crate::model::list_runs(&state_dir, "poll-demo", 10);
            found_poll = runs.iter().any(|r| {
                r.status == RunStatus::Success
                    && r.trigger == Trigger::Poll
                    && r.trigger_reason.as_deref() == Some("Poll detected new commit on master")
            });
            found_tag = runs.iter().any(|r| {
                r.status == RunStatus::Success
                    && r.trigger == Trigger::Tag
                    && r.trigger_reason.as_deref() == Some("Poll detected new tag v1.0.0")
            });
            if found_poll && found_tag {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }

        daemon.shutdown();
        let runs = crate::model::list_runs(&state_dir, "poll-demo", 10);
        assert!(found_poll, "expected branch poll reason in runs: {runs:?}");
        assert!(found_tag, "expected tag poll reason in runs: {runs:?}");
    }

    #[test]
    fn concurrency_gate_limits_releases_hot_reloads_and_cancels() {
        let gate = Arc::new(ConcurrencyGate::new(1));
        let cancel = Arc::new(AtomicBool::new(false));
        let shutdown = Arc::new(AtomicBool::new(false));

        let p1 = gate.acquire(&cancel, &shutdown).expect("first permit");
        assert_eq!(gate.active(), 1);
        assert_eq!(gate.limit(), 1);

        // Second waiter blocks until limit is raised via hot-reload.
        let gate2 = gate.clone();
        let cancel2 = Arc::new(AtomicBool::new(false));
        let cancel2_t = cancel2.clone();
        let shutdown2 = shutdown.clone();
        let acquired2 = Arc::new(AtomicBool::new(false));
        let acquired2_t = acquired2.clone();
        let h2 = std::thread::spawn(move || {
            let permit = gate2.acquire(&cancel2_t, &shutdown2).expect("permit 2");
            acquired2_t.store(true, Ordering::SeqCst);
            permit
        });

        std::thread::sleep(Duration::from_millis(80));
        assert!(
            !acquired2.load(Ordering::SeqCst),
            "second waiter must block while limit == 1"
        );

        // Hot-reload limit upward -> waiter 2 unblocks immediately.
        gate.set_limit(2);
        let p2 = h2.join().expect("waiter 2 joined");
        assert!(acquired2.load(Ordering::SeqCst));
        assert_eq!(gate.active(), 2);

        // Third waiter blocks while 2 permits are held, and cancels cleanly.
        let gate3 = gate.clone();
        let cancel3 = Arc::new(AtomicBool::new(false));
        let cancel3_t = cancel3.clone();
        let shutdown3 = shutdown.clone();
        let h3 = std::thread::spawn(move || gate3.acquire(&cancel3_t, &shutdown3));
        std::thread::sleep(Duration::from_millis(80));
        cancel3.store(true, Ordering::SeqCst);
        assert!(
            h3.join().unwrap().is_none(),
            "cancelled waiter returns None"
        );

        // Fourth waiter blocks while 2 permits are held, then acquires when p1 drops.
        let gate4 = gate.clone();
        let cancel4 = Arc::new(AtomicBool::new(false));
        let shutdown4 = shutdown.clone();
        let h4 = std::thread::spawn(move || gate4.acquire(&cancel4, &shutdown4));
        std::thread::sleep(Duration::from_millis(80));
        drop(p1);
        let p4 = h4.join().unwrap().expect("waiter 4 acquired after drop");
        assert_eq!(gate.active(), 2);

        drop(p2);
        drop(p4);
        assert_eq!(gate.active(), 0);
    }

    #[cfg(unix)]
    fn make_local_git_repo(dir: &Path, name: &str) -> (PathBuf, String) {
        let origin = dir.join(format!("{name}-origin.git"));
        let work = dir.join(format!("{name}-work"));
        std::fs::create_dir_all(&origin).unwrap();
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
        std::fs::write(work.join("f.txt"), format!("{name}-v1\n")).unwrap();
        run_git(&work, &["add", "f.txt"]);
        run_git(&work, &["commit", "-m", "v1"]);
        run_git(&work, &["push", "origin", "master"]);
        let sha = run_git(&work, &["rev-parse", "HEAD"]);
        (work, sha)
    }

    #[cfg(unix)]
    fn make_shell_step(name: &str, script: &str) -> StepCfg {
        StepCfg {
            name: name.into(),
            kind: StepKind::Shell,
            run: Some(script.into()),
            workflow: None,
            job: None,
            event: None,
            platforms: BTreeMap::new(),
            args: Vec::new(),
            secrets: BTreeMap::new(),
            vars: BTreeMap::new(),
            only_on: Vec::new(),
            timeout: Some(20),
            cwd: None,
            shell: Some(vec!["bash".into(), "-c".into()]),
            env: BTreeMap::new(),
        }
    }

    #[cfg(unix)]
    fn make_test_project(
        name: &str,
        repo_path: PathBuf,
        checkout: Checkout,
        max_concurrent_runs: Option<usize>,
        ci: Vec<StepCfg>,
        deploy: Vec<StepCfg>,
    ) -> ProjectCfg {
        ProjectCfg {
            name: name.into(),
            repo_path,
            remote: "origin".into(),
            branch: "master".into(),
            enabled: None,
            github_repo: None,
            github_enabled: None,
            checkout,
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
            max_concurrent_runs,
            ci,
            deploy,
        }
    }

    #[cfg(unix)]
    #[test]
    fn parallel_two_projects_overlap_when_global_limit_is_two() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state_dir = dir.path().join("state");
        let sync_dir = dir.path().join("sync");
        std::fs::create_dir_all(&state_dir).unwrap();
        std::fs::create_dir_all(&sync_dir).unwrap();

        let (work_a, sha_a) = make_local_git_repo(dir.path(), "proj-a");
        let (work_b, sha_b) = make_local_git_repo(dir.path(), "proj-b");

        // Each project signals arrival and waits for the OTHER project to arrive.
        // If execution were sequential, both would fail the barrier timeout!
        let script_a = format!(
            "touch '{sync}/a.started'\n\
             for i in $(seq 1 100); do [ -f '{sync}/b.started' ] && exit 0; sleep 0.05; done\n\
             echo 'proj-b never overlapped with proj-a' >&2; exit 1",
            sync = sync_dir.display()
        );
        let script_b = format!(
            "touch '{sync}/b.started'\n\
             for i in $(seq 1 100); do [ -f '{sync}/a.started' ] && exit 0; sleep 0.05; done\n\
             echo 'proj-a never overlapped with proj-b' >&2; exit 1",
            sync = sync_dir.display()
        );

        let proj_a = make_test_project(
            "proj-a",
            work_a,
            Checkout::None,
            None,
            vec![make_shell_step("BarrierA", &script_a)],
            Vec::new(),
        );
        let proj_b = make_test_project(
            "proj-b",
            work_b,
            Checkout::None,
            None,
            vec![make_shell_step("BarrierB", &script_b)],
            Vec::new(),
        );

        let mut cfg = Config::default();
        cfg.general.state_dir = Some(state_dir.clone());
        cfg.general.trigger_mode = Some(TriggerMode::Webhook);
        cfg.general.poll_interval = 0;
        cfg.general.max_concurrent_runs = 2;
        cfg.projects = vec![proj_a, proj_b];

        let cfg_path = dir.path().join("config.toml");
        std::fs::write(&cfg_path, toml::to_string(&cfg).unwrap()).unwrap();

        let mut daemon = Daemon::new(cfg_path, cfg);
        daemon.reconcile();

        let tx_a = daemon.command_sender("proj-a").unwrap();
        let tx_b = daemon.command_sender("proj-b").unwrap();
        tx_a.send(Command::Trigger {
            sha: Some(sha_a),
            force: true,
            trigger: Trigger::Manual,
            git_ref: Some("master".into()),
            pr: None,
            deploy_only: false,
            force_deploy: false,
            trigger_reason: Some("parallel test a".into()),
            reserved: None,
        })
        .unwrap();
        tx_b.send(Command::Trigger {
            sha: Some(sha_b),
            force: true,
            trigger: Trigger::Manual,
            git_ref: Some("master".into()),
            pr: None,
            deploy_only: false,
            force_deploy: false,
            trigger_reason: Some("parallel test b".into()),
            reserved: None,
        })
        .unwrap();

        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            let ra = crate::model::list_runs(&state_dir, "proj-a", 5);
            let rb = crate::model::list_runs(&state_dir, "proj-b", 5);
            if ra.first().is_some_and(|r| r.status.is_final())
                && rb.first().is_some_and(|r| r.status.is_final())
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }

        daemon.shutdown();
        let ra = crate::model::list_runs(&state_dir, "proj-a", 5);
        let rb = crate::model::list_runs(&state_dir, "proj-b", 5);
        assert_eq!(ra.len(), 1);
        assert_eq!(rb.len(), 1);
        assert_eq!(
            ra[0].status,
            RunStatus::Success,
            "proj-a failed: {:?}",
            ra[0]
        );
        assert_eq!(
            rb[0].status,
            RunStatus::Success,
            "proj-b failed: {:?}",
            rb[0]
        );
    }

    #[cfg(unix)]
    #[test]
    fn parallel_same_project_ci_overlaps_while_deploy_serializes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state_dir = dir.path().join("state");
        let sync_dir = dir.path().join("sync");
        std::fs::create_dir_all(&state_dir).unwrap();
        std::fs::create_dir_all(&sync_dir).unwrap();

        let (work, sha) = make_local_git_repo(dir.path(), "concurrent-app");

        // CI step: both runs record a unique file in sync/ci/ and wait until 2 files exist,
        // proving both CI steps overlap in wall-clock time on the SAME project!
        let ci_script = format!(
            "mkdir -p '{sync}/ci'\n\
             touch \"{sync}/ci/$LUNACY_RUN_ID\"\n\
             for i in $(seq 1 100); do\n\
               count=$(ls -1 '{sync}/ci' | wc -l)\n\
               [ \"$count\" -ge 2 ] && exit 0\n\
               sleep 0.05\n\
             done\n\
             echo 'CI runs did not overlap concurrently' >&2; exit 1",
            sync = sync_dir.display()
        );

        // Deploy step: uses atomic `mkdir` to detect any concurrent deploy overlap!
        // If two deploys ever ran at the same time, `mkdir` would fail and exit 1.
        let deploy_script = format!(
            "if ! mkdir '{sync}/deploy.active' 2>/dev/null; then\n\
               echo 'ERROR: concurrent deploy overlap detected!' >&2\n\
               exit 1\n\
             fi\n\
             echo \"start $LUNACY_RUN_ID\" >> '{sync}/deploy.log'\n\
             sleep 0.2\n\
             echo \"end $LUNACY_RUN_ID\" >> '{sync}/deploy.log'\n\
             rmdir '{sync}/deploy.active'",
            sync = sync_dir.display()
        );

        let proj = make_test_project(
            "concurrent-app",
            work,
            Checkout::Worktree,
            Some(2),
            vec![make_shell_step("ParallelCI", &ci_script)],
            vec![make_shell_step("SerialDeploy", &deploy_script)],
        );

        let mut cfg = Config::default();
        cfg.general.state_dir = Some(state_dir.clone());
        cfg.general.trigger_mode = Some(TriggerMode::Webhook);
        cfg.general.poll_interval = 0;
        cfg.general.max_concurrent_runs = 2;
        cfg.projects = vec![proj];

        let cfg_path = dir.path().join("config.toml");
        std::fs::write(&cfg_path, toml::to_string(&cfg).unwrap()).unwrap();

        let mut daemon = Daemon::new(cfg_path, cfg);
        daemon.reconcile();

        let tx = daemon.command_sender("concurrent-app").unwrap();
        for n in 1..=2 {
            tx.send(Command::Trigger {
                sha: Some(sha.clone()),
                force: true,
                trigger: Trigger::Manual,
                git_ref: Some("master".into()),
                pr: None,
                deploy_only: false,
                force_deploy: false,
                trigger_reason: Some(format!("concurrent run {n}")),
                reserved: None,
            })
            .unwrap();
        }

        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            let runs = crate::model::list_runs(&state_dir, "concurrent-app", 10);
            if runs.len() == 2 && runs.iter().all(|r| r.status.is_final()) {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }

        daemon.shutdown();
        let runs = crate::model::list_runs(&state_dir, "concurrent-app", 10);
        assert_eq!(runs.len(), 2, "expected 2 runs: {runs:?}");
        for r in &runs {
            assert_eq!(r.status, RunStatus::Success, "run failed: {r:?}");
        }

        // Verify deploy log shows strictly serialized start/end pairs: start A, end A, start B, end B.
        let deploy_log = std::fs::read_to_string(sync_dir.join("deploy.log")).unwrap();
        let lines: Vec<&str> = deploy_log.lines().collect();
        assert_eq!(lines.len(), 4, "deploy log:\n{deploy_log}");
        assert!(lines[0].starts_with("start "), "{deploy_log}");
        assert!(lines[1].starts_with("end "), "{deploy_log}");
        assert_eq!(
            lines[0].strip_prefix("start "),
            lines[1].strip_prefix("end "),
            "first deploy must finish before second starts:\n{deploy_log}"
        );
        assert!(lines[2].starts_with("start "), "{deploy_log}");
        assert!(lines[3].starts_with("end "), "{deploy_log}");
        assert_eq!(
            lines[2].strip_prefix("start "),
            lines[3].strip_prefix("end "),
            "second deploy must match its start/end:\n{deploy_log}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn cancel_run_only_aborts_matching_concurrent_run_on_same_project() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state_dir = dir.path().join("state");
        let sync_dir = dir.path().join("sync");
        std::fs::create_dir_all(&state_dir).unwrap();
        std::fs::create_dir_all(&sync_dir).unwrap();

        let (work, sha) = make_local_git_repo(dir.path(), "cancel-app");

        // Each run waits until released by a `release` marker file.
        let ci_script = format!(
            "for i in $(seq 1 200); do [ -f '{sync}/release' ] && exit 0; sleep 0.05; done\nexit 1",
            sync = sync_dir.display()
        );

        let proj = make_test_project(
            "cancel-app",
            work,
            Checkout::Worktree,
            Some(2),
            vec![make_shell_step("Wait", &ci_script)],
            Vec::new(),
        );

        let mut cfg = Config::default();
        cfg.general.state_dir = Some(state_dir.clone());
        cfg.general.trigger_mode = Some(TriggerMode::Webhook);
        cfg.general.poll_interval = 0;
        cfg.general.max_concurrent_runs = 2;
        cfg.projects = vec![proj];

        let cfg_path = dir.path().join("config.toml");
        std::fs::write(&cfg_path, toml::to_string(&cfg).unwrap()).unwrap();

        let mut daemon = Daemon::new(cfg_path, cfg);
        daemon.reconcile();

        let tx = daemon.command_sender("cancel-app").unwrap();
        for n in 1..=2 {
            tx.send(Command::Trigger {
                sha: Some(sha.clone()),
                force: true,
                trigger: Trigger::Manual,
                git_ref: Some("master".into()),
                pr: None,
                deploy_only: false,
                force_deploy: false,
                trigger_reason: Some(format!("run {n}")),
                reserved: None,
            })
            .unwrap();
        }

        // Wait until BOTH runs are actively registered in ProjectState.active_runs.
        let mut active_ids = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            let ids: Vec<String> = daemon
                .shared
                .lock()
                .unwrap()
                .projects
                .iter()
                .find(|p| p.name == "cancel-app")
                .map(|p| p.active_runs.iter().map(|a| a.run.id.clone()).collect())
                .unwrap_or_default();
            if ids.len() == 2 {
                active_ids = ids;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(active_ids.len(), 2, "expected 2 concurrent active runs");
        let id1 = active_ids[0].clone();
        let id2 = active_ids[1].clone();

        // Cancel ONLY id1!
        let cancelled = daemon.shared.lock().unwrap().cancel_run(&id1);
        assert_eq!(cancelled, Some(("cancel-app".to_string(), id1.clone())));

        // Wait until id1 finishes as Cancelled, while id2 is STILL active!
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if let Some(r1) = crate::model::read_run(&state_dir, &id1) {
                if r1.status.is_final() {
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let r1 = crate::model::read_run(&state_dir, &id1).expect("run 1");
        assert_eq!(r1.status, RunStatus::Cancelled);

        // Verify id2 is still running in active_runs!
        let remaining_ids: Vec<String> = daemon
            .shared
            .lock()
            .unwrap()
            .projects
            .iter()
            .find(|p| p.name == "cancel-app")
            .map(|p| p.active_runs.iter().map(|a| a.run.id.clone()).collect())
            .unwrap_or_default();
        assert_eq!(
            remaining_ids,
            vec![id2.clone()],
            "sibling run id2 must remain active when id1 is cancelled"
        );

        // Now release id2 and verify it finishes cleanly with Success.
        std::fs::write(sync_dir.join("release"), "go\n").unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if let Some(r2) = crate::model::read_run(&state_dir, &id2) {
                if r2.status.is_final() {
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        daemon.shutdown();

        let r2 = crate::model::read_run(&state_dir, &id2).expect("run 2");
        assert_eq!(r2.status, RunStatus::Success);
    }

    #[cfg(unix)]
    #[test]
    fn pending_slot_waiting_on_gate_prevents_duplicate_branch_pr_and_tag_poll_spawn() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state_dir = dir.path().join("state");
        std::fs::create_dir_all(&state_dir).unwrap();

        let (work, sha1) = make_local_git_repo(dir.path(), "b1-poll");
        let run_git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(&work)
                .args(args)
                .output()
                .expect("git");
            assert!(
                out.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        run_git(&["tag", "v1.0.0"]);
        run_git(&["push", "origin", "refs/tags/v1.0.0"]);
        write_seen_tags(&state_dir, "b1-poll", &BTreeMap::new()).unwrap();

        let mut proj = make_test_project(
            "b1-poll",
            work.clone(),
            Checkout::Worktree,
            Some(4),
            vec![make_shell_step("Ok", "true")],
            Vec::new(),
        );
        proj.trigger_mode = Some(TriggerMode::Poll);
        proj.poll_interval = Some(1);
        proj.watch_tags = true;

        // 1. Direct helper checks on PendingSlot metadata for branch, PR, and tag.
        {
            let mut ps = ProjectState::new(&proj, &state_dir);
            let cancel = Arc::new(AtomicBool::new(false));
            ps.register_pending_slot(
                0,
                cancel.clone(),
                Some(sha1.clone()),
                Some("master".into()),
                None,
            );
            assert!(ps.is_branch_sha_active(&proj, &sha1));
            assert!(!ps.is_branch_sha_active(&proj, "2222222222222222222222222222222222222222"));

            ps.register_pending_slot(
                1,
                cancel.clone(),
                Some(sha1.clone()),
                Some("feat/pr-9".into()),
                Some(9),
            );
            assert!(ps.is_pr_sha_active(9, &sha1));
            assert!(!ps.is_pr_sha_active(10, &sha1));

            ps.register_pending_slot(
                2,
                cancel,
                Some(sha1.clone()),
                Some("tags/v1.0.0".into()),
                None,
            );
            assert!(ps.is_tag_ref_active("tags/v1.0.0"));
            assert!(!ps.is_tag_ref_active("tags/v2.0.0"));
        }

        // 2. Live daemon poll loop blocked on ConcurrencyGate must NOT storm duplicate runs
        // across multiple poll ticks for the same branch commit and tag.
        let mut cfg = Config::default();
        cfg.general.state_dir = Some(state_dir.clone());
        cfg.general.poll_interval = 1;
        cfg.general.max_concurrent_runs = 1;
        cfg.projects = vec![proj];

        let cfg_path = dir.path().join("config.toml");
        std::fs::write(&cfg_path, toml::to_string(&cfg).unwrap()).unwrap();

        let mut daemon = Daemon::new(cfg_path, cfg);
        let gate = daemon.shared.lock().unwrap().concurrency_gate.clone();
        let hold_cancel = AtomicBool::new(false);
        let hold_shutdown = AtomicBool::new(false);
        let held_permit = gate
            .acquire(&hold_cancel, &hold_shutdown)
            .expect("hold global concurrency slot");

        daemon.reconcile();

        // Wait across multiple 1s poll cycles while the gate is saturated.
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            let pending_count = daemon.shared.lock().unwrap().projects[0]
                .pending_slots
                .len();
            if pending_count >= 2 {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        // Sleep another 1.3s to ensure a second poll cycle fires while both tasks wait on the gate.
        std::thread::sleep(Duration::from_millis(1300));
        let pending_count = daemon.shared.lock().unwrap().projects[0]
            .pending_slots
            .len();
        assert_eq!(
            pending_count, 2,
            "expected exactly 1 branch + 1 tag pending slot without duplicate storm"
        );

        // Release the global gate and verify exactly 2 runs complete (1 Poll + 1 Tag).
        drop(held_permit);
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            let runs = crate::model::list_runs(&state_dir, "b1-poll", 10);
            if runs.len() >= 2 && runs.iter().all(|r| r.status.is_final()) {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        // Give one extra poll tick to confirm no extra runs appear.
        std::thread::sleep(Duration::from_millis(250));
        daemon.shutdown();

        let runs = crate::model::list_runs(&state_dir, "b1-poll", 10);
        assert_eq!(runs.len(), 2, "must not create duplicate runs: {runs:?}");
        assert_eq!(
            runs.iter().filter(|r| r.trigger == Trigger::Poll).count(),
            1
        );
        assert_eq!(runs.iter().filter(|r| r.trigger == Trigger::Tag).count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn cancel_project_aborts_pending_and_queued_triggers_before_execution() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state_dir = dir.path().join("state");
        std::fs::create_dir_all(&state_dir).unwrap();

        let (work, sha) = make_local_git_repo(dir.path(), "b2-cancel-queued");
        let proj = make_test_project(
            "b2-cancel-queued",
            work,
            Checkout::Worktree,
            Some(1),
            vec![make_shell_step("Ok", "true")],
            Vec::new(),
        );

        let mut cfg = Config::default();
        cfg.general.state_dir = Some(state_dir.clone());
        cfg.general.trigger_mode = Some(TriggerMode::Webhook);
        cfg.general.poll_interval = 0;
        cfg.general.max_concurrent_runs = 1;
        cfg.projects = vec![proj];

        let cfg_path = dir.path().join("config.toml");
        std::fs::write(&cfg_path, toml::to_string(&cfg).unwrap()).unwrap();

        let mut daemon = Daemon::new(cfg_path, cfg);
        let gate = daemon.shared.lock().unwrap().concurrency_gate.clone();
        let hold_cancel = AtomicBool::new(false);
        let hold_shutdown = AtomicBool::new(false);
        let held_permit = gate
            .acquire(&hold_cancel, &hold_shutdown)
            .expect("hold gate");

        daemon.reconcile();
        let tx = daemon.command_sender("b2-cancel-queued").unwrap();

        // Reserve slot 0 (as the API handler does) and queue a second webhook trigger behind it.
        let reserved = daemon.shared.lock().unwrap().projects[0]
            .try_reserve_pending_slot(1, Some(sha.clone()), Some("master".into()), None)
            .expect("reserve slot 0");
        tx.send(Command::Trigger {
            sha: Some(sha.clone()),
            force: true,
            trigger: Trigger::Manual,
            git_ref: Some("master".into()),
            pr: None,
            deploy_only: false,
            force_deploy: false,
            trigger_reason: Some("first".into()),
            reserved: Some(reserved),
        })
        .unwrap();
        tx.send(Command::Trigger {
            sha: Some(sha),
            force: true,
            trigger: Trigger::Webhook,
            git_ref: Some("master".into()),
            pr: None,
            deploy_only: false,
            force_deploy: false,
            trigger_reason: Some("second queued".into()),
            reserved: None,
        })
        .unwrap();

        // Give the worker time to receive trigger 1 (blocked on gate) and queue trigger 2.
        std::thread::sleep(Duration::from_millis(200));

        // Cancel project + send Command::CancelQueued (matching POST /api/projects/{name}/cancel).
        assert!(matches!(
            daemon
                .shared
                .lock()
                .unwrap()
                .cancel_project("b2-cancel-queued"),
            CancelProjectOutcome::Cancelling(_)
        ));
        tx.send(Command::CancelQueued).unwrap();

        // Wait until slot 0 is released, then drop the global gate permit.
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if daemon.shared.lock().unwrap().projects[0].active_run_count() == 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        drop(held_permit);
        std::thread::sleep(Duration::from_millis(400));
        daemon.shutdown();

        assert_eq!(
            daemon.shared.lock().unwrap().projects[0].active_run_count(),
            0
        );
        assert!(
            crate::model::list_runs(&state_dir, "b2-cancel-queued", 10).is_empty(),
            "neither the gate-blocked trigger nor the queued trigger may launch after cancel"
        );
    }

    #[cfg(unix)]
    #[test]
    fn concurrent_worktree_runs_succeed_with_unique_leaf_dirs_and_git_lock() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state_dir = dir.path().join("state");
        let sync_dir = dir.path().join("sync");
        std::fs::create_dir_all(&state_dir).unwrap();
        std::fs::create_dir_all(&sync_dir).unwrap();

        let (work, sha) = make_local_git_repo(dir.path(), "b3-wt");

        // Verify each concurrent run gets a unique `tree-<run_id>` worktree leaf and overlaps.
        let ci_script = format!(
            "leaf=$(basename \"$LUNACY_WORKDIR\")\n\
             expected=\"tree-$LUNACY_RUN_ID\"\n\
             if [ \"$leaf\" != \"$expected\" ]; then\n\
               echo \"unexpected leaf: $leaf != $expected\" >&2\n\
               exit 1\n\
             fi\n\
             touch \"{sync}/$LUNACY_RUN_ID\"\n\
             for i in $(seq 1 100); do\n\
               count=$(ls -1 '{sync}' | wc -l)\n\
               [ \"$count\" -ge 4 ] && exit 0\n\
               sleep 0.05\n\
             done\n\
             echo 'all 4 worktree runs did not overlap' >&2; exit 1",
            sync = sync_dir.display()
        );

        let proj = make_test_project(
            "b3-wt",
            work.clone(),
            Checkout::Worktree,
            Some(4),
            vec![make_shell_step("WorktreeCheck", &ci_script)],
            Vec::new(),
        );

        let mut cfg = Config::default();
        cfg.general.state_dir = Some(state_dir.clone());
        cfg.general.trigger_mode = Some(TriggerMode::Webhook);
        cfg.general.poll_interval = 0;
        cfg.general.max_concurrent_runs = 4;
        cfg.projects = vec![proj];

        let cfg_path = dir.path().join("config.toml");
        std::fs::write(&cfg_path, toml::to_string(&cfg).unwrap()).unwrap();

        let mut daemon = Daemon::new(cfg_path, cfg);
        daemon.reconcile();

        let tx = daemon.command_sender("b3-wt").unwrap();
        for n in 1..=4 {
            tx.send(Command::Trigger {
                sha: Some(sha.clone()),
                force: true,
                trigger: Trigger::Manual,
                git_ref: Some("master".into()),
                pr: None,
                deploy_only: false,
                force_deploy: false,
                trigger_reason: Some(format!("worktree run {n}")),
                reserved: None,
            })
            .unwrap();
        }

        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            let runs = crate::model::list_runs(&state_dir, "b3-wt", 10);
            if runs.len() == 4 && runs.iter().all(|r| r.status.is_final()) {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        daemon.shutdown();

        let runs = crate::model::list_runs(&state_dir, "b3-wt", 10);
        assert_eq!(runs.len(), 4, "expected 4 runs: {runs:?}");
        for r in &runs {
            assert_eq!(r.status, RunStatus::Success, "worktree run failed: {r:?}");
        }

        // Verify all temporary worktrees were cleanly removed from git's admin state.
        let wt_list = std::process::Command::new("git")
            .arg("-C")
            .arg(&work)
            .args(["worktree", "list", "--porcelain"])
            .output()
            .expect("git worktree list");
        let wt_text = String::from_utf8_lossy(&wt_list.stdout);
        assert_eq!(
            wt_text.matches("worktree ").count(),
            1,
            "stale worktrees left behind:\n{wt_text}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn out_of_order_default_branch_and_pr_completion_never_regresses_state() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state_dir = dir.path().join("state");
        let sync_dir = dir.path().join("sync");
        std::fs::create_dir_all(&state_dir).unwrap();
        std::fs::create_dir_all(&sync_dir).unwrap();

        let (work, c1) = make_local_git_repo(dir.path(), "b4-order");
        let run_git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(&work)
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
        std::fs::write(work.join("f.txt"), "c2\n").unwrap();
        run_git(&["add", "f.txt"]);
        run_git(&["commit", "-m", "c2"]);
        let c2 = run_git(&["rev-parse", "HEAD"]);
        std::fs::write(work.join("f.txt"), "c3\n").unwrap();
        run_git(&["add", "f.txt"]);
        run_git(&["commit", "-m", "c3"]);
        let c3 = run_git(&["rev-parse", "HEAD"]);

        // Script: for commit C1, wait until `release_c1` exists (and optionally fail if `fail_c1` exists).
        // For commit C2, finish immediately with success!
        let script = format!(
            "if [ \"$LUNACY_SHA\" = '{c1}' ]; then\n\
               touch '{sync}/c1.started'\n\
               for i in $(seq 1 200); do [ -f '{sync}/release_c1' ] && break; sleep 0.05; done\n\
               [ -f '{sync}/fail_c1' ] && exit 1\n\
             fi\n\
             exit 0",
            sync = sync_dir.display()
        );
        let proj = make_test_project(
            "b4-order",
            work,
            Checkout::Worktree,
            Some(2),
            vec![make_shell_step("Step", &script)],
            Vec::new(),
        );
        let mut cfg = Config::default();
        cfg.general.state_dir = Some(state_dir.clone());
        cfg.general.max_concurrent_runs = 2;
        cfg.projects = vec![proj.clone()];

        let mut ps = ProjectState::new(&proj, &state_dir);
        ps.upstream_sha = Some(c3.clone());
        let shared = Arc::new(Mutex::new(Shared {
            config: cfg.clone(),
            state_dir: state_dir.clone(),
            projects: vec![ps],
            concurrency_gate: Arc::new(ConcurrencyGate::new(2)),
            ..Shared::default()
        }));
        let deploy_lock = Arc::new(Mutex::new(()));
        let repo_git_lock = Arc::new(Mutex::new(()));
        let shutdown = Arc::new(AtomicBool::new(false));

        // 1. Default-branch out-of-order completion: C2 finishes before C1 while upstream is C3.
        let spec_c1 = RunSpec {
            sha: c1.clone(),
            trigger: Trigger::Poll,
            git_ref: "master".into(),
            pr: None,
            deploy_only: false,
            force_deploy: false,
            trigger_reason: None,
        };
        let spec_c2 = RunSpec {
            sha: c2.clone(),
            trigger: Trigger::Poll,
            git_ref: "master".into(),
            pr: None,
            deploy_only: false,
            force_deploy: false,
            trigger_reason: None,
        };

        let s1 = shared.clone();
        let cfg1 = cfg.clone();
        let p1 = proj.clone();
        let dl1 = deploy_lock.clone();
        let gl1 = repo_git_lock.clone();
        let sh1 = shutdown.clone();
        let h_c1 = std::thread::spawn(move || {
            execute_spec_with_slot(
                &s1,
                &cfg1,
                &p1,
                &spec_c1,
                0,
                Arc::new(AtomicBool::new(false)),
                None,
                &dl1,
                &gl1,
                &sh1,
            )
        });

        let deadline = Instant::now() + Duration::from_secs(10);
        while !sync_dir.join("c1.started").is_file() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(25));
        }
        assert!(sync_dir.join("c1.started").is_file());

        // C2 finishes first -> last_sha advances to C2.
        let run_c2 = execute_spec_with_slot(
            &shared,
            &cfg,
            &proj,
            &spec_c2,
            1,
            Arc::new(AtomicBool::new(false)),
            None,
            &deploy_lock,
            &repo_git_lock,
            &shutdown,
        )
        .expect("c2 run");
        assert_eq!(run_c2.status, RunStatus::Success);
        assert_eq!(
            shared.lock().unwrap().projects[0].last_sha.as_deref(),
            Some(c2.as_str())
        );
        assert_eq!(
            read_last_sha(&state_dir, "b4-order").as_deref(),
            Some(c2.as_str())
        );

        // Now let older ancestor C1 finish -> MUST NOT regress last_sha back to C1!
        std::fs::write(sync_dir.join("release_c1"), "go\n").unwrap();
        let run_c1 = h_c1.join().unwrap().expect("c1 run");
        assert_eq!(run_c1.status, RunStatus::Success);
        assert_eq!(
            shared.lock().unwrap().projects[0].last_sha.as_deref(),
            Some(c2.as_str()),
            "in-memory last_sha regressed from C2 to older ancestor C1"
        );
        assert_eq!(
            read_last_sha(&state_dir, "b4-order").as_deref(),
            Some(c2.as_str()),
            "persisted last_sha regressed from C2 to older ancestor C1"
        );

        // 2. PR out-of-order completion: PR #42 at head_sha = C2 finishes before older C1 (failing).
        let _ = std::fs::remove_file(sync_dir.join("c1.started"));
        let _ = std::fs::remove_file(sync_dir.join("release_c1"));
        std::fs::write(sync_dir.join("fail_c1"), "1\n").unwrap();
        shared.lock().unwrap().projects[0].open_prs = vec![PrSummary {
            number: 42,
            title: "PR 42".into(),
            head_ref: "feat/pr-42".into(),
            base_ref: "master".into(),
            head_sha: c2.clone(),
            last_tested_sha: None,
            status: None,
        }];
        let pr_ctx = PrContext {
            number: 42,
            title: "PR 42".into(),
            head_ref: "feat/pr-42".into(),
            base_ref: "master".into(),
            base_sha: c1.clone(),
        };
        let pr_spec_c1 = RunSpec {
            sha: c1.clone(),
            trigger: Trigger::PullRequest,
            git_ref: "feat/pr-42".into(),
            pr: Some(pr_ctx.clone()),
            deploy_only: false,
            force_deploy: false,
            trigger_reason: None,
        };
        let pr_spec_c2 = RunSpec {
            sha: c2.clone(),
            trigger: Trigger::PullRequest,
            git_ref: "feat/pr-42".into(),
            pr: Some(pr_ctx),
            deploy_only: false,
            force_deploy: false,
            trigger_reason: None,
        };

        let s1 = shared.clone();
        let cfg1 = cfg.clone();
        let p1 = proj.clone();
        let dl1 = deploy_lock.clone();
        let gl1 = repo_git_lock.clone();
        let sh1 = shutdown.clone();
        let pr_spec_c1_clone = pr_spec_c1.clone();
        let h_pr1 = std::thread::spawn(move || {
            execute_spec_with_slot(
                &s1,
                &cfg1,
                &p1,
                &pr_spec_c1_clone,
                0,
                Arc::new(AtomicBool::new(false)),
                None,
                &dl1,
                &gl1,
                &sh1,
            )
        });

        let deadline = Instant::now() + Duration::from_secs(10);
        while !sync_dir.join("c1.started").is_file() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(25));
        }
        assert!(sync_dir.join("c1.started").is_file());

        // Newer PR head C2 finishes first with Success.
        let run_pr2 = execute_spec_with_slot(
            &shared,
            &cfg,
            &proj,
            &pr_spec_c2,
            1,
            Arc::new(AtomicBool::new(false)),
            None,
            &deploy_lock,
            &repo_git_lock,
            &shutdown,
        )
        .expect("pr c2");
        assert_eq!(run_pr2.status, RunStatus::Success);
        {
            let guard = shared.lock().unwrap();
            let entry = &guard.projects[0].open_prs[0];
            assert_eq!(entry.last_tested_sha.as_deref(), Some(c2.as_str()));
            assert_eq!(entry.status, Some(RunStatus::Success));
        }
        assert_eq!(
            read_pr_shas(&state_dir, "b4-order")
                .get(&42)
                .map(String::as_str),
            Some(c2.as_str())
        );

        // Older PR commit C1 finishes second with Failure -> must NOT overwrite open_prs or pr_shas!
        std::fs::write(sync_dir.join("release_c1"), "go\n").unwrap();
        let run_pr1 = h_pr1.join().unwrap().expect("pr c1");
        assert_eq!(run_pr1.status, RunStatus::Failure);
        {
            let guard = shared.lock().unwrap();
            let entry = &guard.projects[0].open_prs[0];
            assert_eq!(
                entry.last_tested_sha.as_deref(),
                Some(c2.as_str()),
                "open_prs.last_tested_sha regressed to stale PR sha"
            );
            assert_eq!(
                entry.status,
                Some(RunStatus::Success),
                "open_prs.status overwritten by stale PR run"
            );
        }
        assert_eq!(
            read_pr_shas(&state_dir, "b4-order")
                .get(&42)
                .map(String::as_str),
            Some(c2.as_str()),
            "persisted pr_shas regressed to stale PR sha"
        );

        // 3. Fallback when open_prs has no entry: ancestor C1 still must not regress pr_shas[42] = C2.
        shared.lock().unwrap().projects[0].open_prs.clear();
        let run_pr1_again = execute_spec_with_slot(
            &shared,
            &cfg,
            &proj,
            &pr_spec_c1,
            0,
            Arc::new(AtomicBool::new(false)),
            None,
            &deploy_lock,
            &repo_git_lock,
            &shutdown,
        )
        .expect("pr c1 fallback");
        assert_eq!(run_pr1_again.status, RunStatus::Failure);
        assert_eq!(
            read_pr_shas(&state_dir, "b4-order")
                .get(&42)
                .map(String::as_str),
            Some(c2.as_str()),
            "persisted pr_shas regressed when open_prs was empty"
        );
    }
}
