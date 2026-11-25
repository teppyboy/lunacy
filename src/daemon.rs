//! Daemon: one worker thread per project, poll loop, state, trigger channel.

use crate::config::{Config, ProjectCfg};
use crate::git;
use crate::model::{read_last_sha, write_last_sha, Run, Trigger};
use crate::pipeline::{self, Ctx};
use crate::report::Github;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
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

impl Shared {
    /// Most settings hot-reload; the listen address does not.
    pub fn server_restart_needed(&self) -> bool {
        true
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
    pub last_run: Option<Run>,
    /// Errors from the last poll (fetch failure and friends).
    pub last_error: Option<String>,
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
    }
}

/// A request from the API to the owner worker.
#[derive(Debug, Clone)]
pub enum Command {
    Trigger {
        sha: Option<String>,
        force: bool,
        trigger: Trigger,
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
        for (i, project) in cfg.projects.iter().enumerate() {
            match shared.projects.iter().position(|p| p.name == project.name) {
                Some(pos) => shared.projects[pos].refresh(project, &state_dir),
                None => {
                    let at = i.min(shared.projects.len());
                    let state = ProjectState::new(project, &state_dir);
                    shared.projects.insert(at, state);
                }
            }
        }
        shared.projects.retain(|p| wanted.contains(&p.name));
    }

    pub fn shutdown(&mut self) {
        self.shutdown
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let names: Vec<String> = self.workers.keys().cloned().collect();
        for name in names {
            if let Some(handle) = self.workers.remove(&name) {
                let _ = handle.cmd.send(Command::Stop);
                if let Some(j) = handle.join {
                    let _ = j.join();
                }
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
    let mut pending_run = false;
    let mut queued: Option<(String, bool, Trigger)> = None;

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
                } => {
                    queued = Some((sha.unwrap_or_default(), force, trigger));
                    pending_run = true;
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

        // A queued run (API trigger / webhook) runs before polling.
        if let Some((sha, force, trigger)) = queued.take() {
            pending_run = false;
            let cfg = shared.lock().expect("shared").config.clone();
            let target = if sha.is_empty() {
                fetch_upstream(&cfg, &project, &shared).unwrap_or_default()
            } else {
                sha
            };
            if target.is_empty() {
                log::warn!("[{name}] trigger with no resolvable sha; ignoring");
            } else {
                let last = shared.lock().ok().and_then(|s| {
                    s.projects
                        .iter()
                        .find(|p| p.name == name)
                        .and_then(|p| p.last_sha.clone())
                });
                if !force && last.as_deref() == Some(target.as_str()) {
                    log::info!(
                        "[{name}] {} already processed; no rerun",
                        crate::model::short_sha(&target)
                    );
                } else {
                    execute(&shared, &cfg, &project, &target, trigger);
                }
            }
            last_poll = Instant::now();
            continue;
        }

        let cfg = shared.lock().expect("shared").config.clone();
        let interval = Duration::from_secs(project.poll_interval(&cfg.general));
        if last_poll.elapsed() < interval && !pending_run {
            match rx.recv_timeout(Duration::from_millis(250)) {
                Ok(Command::Stop) => return,
                Ok(Command::Reload) => {
                    pending_run = false;
                    continue;
                }
                Ok(Command::Trigger {
                    sha,
                    force,
                    trigger,
                }) => {
                    queued = Some((sha.unwrap_or_default(), force, trigger));
                    pending_run = true;
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
            continue;
        }
        last_poll = Instant::now();

        let sha = match fetch_upstream(&cfg, &project, &shared) {
            Some(s) => s,
            None => continue,
        };
        let last = shared.lock().ok().and_then(|s| {
            s.projects
                .iter()
                .find(|p| p.name == name)
                .and_then(|p| p.last_sha.clone())
        });
        if last.as_deref() == Some(sha.as_str()) {
            log::debug!("[{name}] {} unchanged", crate::model::short_sha(&sha));
            continue;
        }
        execute(&shared, &cfg, &project, &sha, Trigger::Poll);
    }
    log::info!("[{name}] worker exiting");
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
                        p.github_repo = project.github_repo.clone().or_else(|| {
                            git::remote_url(&project.repo_path, &project.remote)
                                .as_deref()
                                .and_then(crate::config::repo_from_remote)
                        });
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

fn execute(
    shared: &Arc<Mutex<Shared>>,
    cfg: &Config,
    project: &ProjectCfg,
    sha: &str,
    trigger: Trigger,
) {
    log::info!(
        "[{}] running pipeline for {}",
        project.name,
        crate::model::short_sha(sha)
    );
    let github = cfg.github.as_ref().and_then(|g| {
        if !g.enabled() {
            return None;
        }
        match Github::new(g) {
            Ok(gh) => Some(gh),
            Err(e) => {
                log::warn!("[{}] github reporting disabled: {e:#}", project.name);
                None
            }
        }
    });

    let state_dir = shared.lock().expect("shared").state_dir.clone();
    let act_lock = cfg.act_lock_file();

    if let Ok(mut s) = shared.lock() {
        if let Some(p) = s.projects.iter_mut().find(|p| p.name == project.name) {
            p.running = true;
        }
    }

    let on_update = {
        let shared = shared.clone();
        move |run: &Run| {
            if let Ok(mut s) = shared.lock() {
                if let Some(p) = s.projects.iter_mut().find(|p| p.name == run.project) {
                    p.current_run = Some(run.clone());
                    if run.trigger == Trigger::Poll || run.status.is_final() {
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
    };
    let run = pipeline::run_pipeline(&ctx, project, sha, trigger);

    // State semantics: the last processed sha advances when the pipeline ends,
    // whatever the result, so a failing commit is not retried forever.
    if let Err(e) = write_last_sha(&state_dir, &project.name, sha) {
        log::warn!("[{}] persisting last sha failed: {e}", project.name);
    }

    if let Ok(mut s) = shared.lock() {
        if let Some(p) = s.projects.iter_mut().find(|p| p.name == project.name) {
            p.running = false;
            p.current_run = None;
            p.last_sha = Some(run.sha.clone());
            p.last_run = Some(run.clone());
        }
    }
}
