//! `lunacy` — a small local CI/deploy daemon.

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use lunacy::api::{self};
use lunacy::config::{self, Config};
use lunacy::daemon::{Command, Daemon, Shared};
use lunacy::git;
use lunacy::model::{self, read_last_sha, Trigger};
use lunacy::pipeline;
use lunacy::report;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[derive(Debug, Parser)]
#[command(name = "lunacy", version, about = "Local CI/deploy daemon")]
struct Cli {
    /// Log level: trace, debug, info, warn, error. Overrides `general.log_level`.
    #[arg(long, global = true)]
    log_level: Option<String>,
    /// Log to stderr exactly as configured; the default is auto (compact lines).
    #[arg(long, global = true, hide = true)]
    log_style: Option<String>,
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Debug, Subcommand)]
enum Cmd {
    /// Run the daemon: poll every project and report to GitHub/Discord.
    Run {
        #[arg(short, long, default_value = "config.toml")]
        config: PathBuf,
    },
    /// Validate the config and list the projects.
    Check {
        #[arg(short, long, default_value = "config.toml")]
        config: PathBuf,
    },
    /// Run one pipeline now, for testing.
    Once {
        #[arg(short, long, default_value = "config.toml")]
        config: PathBuf,
        /// Project name.
        project: String,
        /// Commit to run (default: current upstream).
        #[arg(long)]
        sha: Option<String>,
        /// Re-run even when the sha was already processed.
        #[arg(long)]
        force: bool,
    },
}

fn main() {
    let cli = Cli::parse();
    // The log level can come from the file, so the config is loaded first.
    let config_path = match &cli.command {
        Cmd::Run { config } => config.clone(),
        Cmd::Check { config } => config.clone(),
        Cmd::Once { config, .. } => config.clone(),
    };
    let preloaded = Config::load(&config_path).ok();
    let level = cli
        .log_level
        .clone()
        .or_else(|| preloaded.as_ref().map(|c| c.general.log_level.clone()))
        .unwrap_or_else(|| "info".into());
    let mut builder = env_logger::Builder::new();
    builder
        .filter_level(parse_level(&level))
        .target(env_logger::Target::Stderr);
    if cli.log_style.as_deref() != Some("full") {
        builder.format_timestamp_secs();
    }
    builder.init();

    let result = match cli.command {
        Cmd::Run { config } => run(config),
        Cmd::Check { config } => check(config),
        Cmd::Once {
            config,
            project,
            sha,
            force,
        } => once(config, project, sha, force),
    };
    if let Err(e) = result {
        eprintln!("lunacy: {e:#}");
        std::process::exit(1);
    }
}

fn parse_level(level: &str) -> log::LevelFilter {
    match level.trim().to_ascii_lowercase().as_str() {
        "trace" => log::LevelFilter::Trace,
        "debug" => log::LevelFilter::Debug,
        "warn" | "warning" => log::LevelFilter::Warn,
        "error" => log::LevelFilter::Error,
        "off" => log::LevelFilter::Off,
        _ => log::LevelFilter::Info,
    }
}

fn load(path: &Path) -> Result<Config> {
    let cfg = Config::load(path)?;
    cfg.validate().map_err(|e| {
        anyhow::anyhow!(
            "{}: {}: {}",
            path.display(),
            e.field.unwrap_or_else(|| "config".into()),
            e.message
        )
    })?;
    Ok(cfg)
}

fn check(path: PathBuf) -> Result<()> {
    let cfg = load(&path)?;
    println!("config ok: {}", path.display());
    println!(
        "state_dir: {}  poll_interval: {}s  log_level: {}",
        cfg.state_dir().display(),
        cfg.general.poll_interval,
        cfg.general.log_level
    );
    if let Some(g) = cfg.github.as_ref() {
        println!(
            "github: mode={:?} api={} token={} app={}",
            g.effective_mode(),
            g.api_url(),
            if g.token.as_deref().map(|t| !t.is_empty()).unwrap_or(false) {
                "set"
            } else {
                "unset"
            },
            if g.app.is_some() { "yes" } else { "no" }
        );
    }
    if let Some(d) = cfg.discord.as_ref() {
        println!(
            "discord: webhook={} notify_on_start={}",
            if d.webhook_url
                .as_deref()
                .map(|u| !u.is_empty())
                .unwrap_or(false)
            {
                "set"
            } else {
                "unset"
            },
            d.notify_on_start
        );
    }
    if let Some(s) = cfg.server.as_ref() {
        println!("server: {} web_root={:?}", s.listen, s.web_root);
    }
    println!("projects ({}):", cfg.projects.len());
    for p in &cfg.projects {
        let repo = p
            .github_repo
            .clone()
            .or_else(|| {
                git::remote_url(&p.repo_path, &p.remote)
                    .as_deref()
                    .and_then(config::repo_from_remote)
            })
            .unwrap_or_else(|| "-".into());
        println!(
            "  {:<12} branch={:<8} checkout={:<8} github={:<22} ci={} deploy={} poll={}s",
            p.name,
            p.branch,
            format!("{:?}", p.checkout).to_lowercase(),
            repo,
            p.ci.len(),
            p.deploy.len(),
            p.poll_interval(&cfg.general)
        );
        for s in p.ci.iter().chain(p.deploy.iter()) {
            let what = match s.kind {
                config::StepKind::Shell => s.run.clone().unwrap_or_default(),
                config::StepKind::Act => format!(
                    "{} {}",
                    s.workflow.clone().unwrap_or_default(),
                    s.job.clone().unwrap_or_default()
                ),
            };
            let only = if s.only_on.is_empty() {
                String::new()
            } else {
                format!(
                    " only_on={:?}",
                    s.only_on.iter().map(|o| o.as_str()).collect::<Vec<_>>()
                )
            };
            println!(
                "      [{}] {:<24} {}{}",
                format!("{:?}", s.kind).to_lowercase(),
                s.name,
                truncate(&what, 60),
                only
            );
        }
    }
    Ok(())
}

fn truncate(s: &str, max: usize) -> String {
    let first = s.lines().next().unwrap_or("");
    if first.chars().count() <= max {
        first.to_string()
    } else {
        first
            .chars()
            .take(max.saturating_sub(1))
            .collect::<String>()
            + "…"
    }
}

fn once(path: PathBuf, project_name: String, sha: Option<String>, force: bool) -> Result<()> {
    let cfg = load(&path)?;
    let project = cfg.project(&project_name).cloned().with_context(|| {
        format!(
            "unknown project {:?}; configured: {:?}",
            project_name,
            cfg.projects.iter().map(|p| &p.name).collect::<Vec<_>>()
        )
    })?;
    let state_dir = cfg.state_dir();
    std::fs::create_dir_all(&state_dir)?;

    let sha = match sha {
        Some(s) => s,
        None => {
            git::fetch(&project.repo_path, &project.remote, &project.branch)?;
            git::upstream_sha(&project.repo_path, &project.remote, &project.branch)?
        }
    };
    if sha.trim().len() < 7 {
        bail!("--sha looks wrong: {sha:?}");
    }
    if !force {
        if let Some(last) = read_last_sha(&state_dir, &project.name) {
            if last == sha {
                println!(
                    "{}: {} was already processed; nothing to do (use --force to rerun)",
                    project.name,
                    model::short_sha(&sha)
                );
                return Ok(());
            }
        }
    }

    let github = cfg.github.as_ref().and_then(|g| {
        if !g.enabled() {
            return None;
        }
        match report::Github::new(g) {
            Ok(gh) => Some(gh),
            Err(e) => {
                eprintln!("lunacy: github reporting disabled: {e:#}");
                None
            }
        }
    });
    let act_lock = state_dir.join("act.lock");
    let on_update = |run: &model::Run| {
        println!(
            "[{}] {}: {} ({})",
            run.project,
            model::short_sha(&run.sha),
            run.status_label(),
            run.steps
                .iter()
                .map(|s| format!("{}={}", s.name, s.status.label()))
                .collect::<Vec<_>>()
                .join(" ")
        );
    };
    let ctx = pipeline::Ctx {
        cfg: &cfg,
        state_dir: &state_dir,
        os: config::Os::current(),
        github: github.as_ref(),
        on_update: &on_update,
        act_lock: &act_lock,
    };
    let run = pipeline::run_pipeline(&ctx, &project, &sha, Trigger::Manual);
    model::write_last_sha(&state_dir, &project.name, &sha)?;
    println!(
        "\n{} {} {} in {}s (run {})",
        run.project,
        model::short_sha(&run.sha),
        run.status_label(),
        run.duration_secs(),
        run.id
    );
    for (i, step) in run.steps.iter().enumerate() {
        println!(
            "  {:<7} {:<24} {:<8} {}",
            step.phase.as_str(),
            step.name,
            step.status.label(),
            step.duration_ms
                .map(|ms| format!("{ms}ms"))
                .unwrap_or_default()
        );
        let path = model::run_log_path(&state_dir, &run.project, &run.id, i);
        if step.status.is_ok() {
            continue;
        }
        if let Ok(tail) = model::tail_lines(&path, 20) {
            if !tail.trim().is_empty() {
                println!("\n--- {} ({}) ---", step.name, path.display());
                println!("{tail}");
            }
        }
    }
    let failed = matches!(
        run.status,
        model::RunStatus::Failure | model::RunStatus::Error
    );
    if failed {
        std::process::exit(3);
    }
    Ok(())
}

fn run(path: PathBuf) -> Result<()> {
    let cfg = load(&path)?;
    let state_dir = cfg.state_dir();
    std::fs::create_dir_all(&state_dir)
        .with_context(|| format!("creating state dir {}", state_dir.display()))?;
    log::info!(
        "lunacy {} starting with {} project(s); state in {}",
        api::VERSION,
        cfg.projects.len(),
        state_dir.display()
    );

    let mut daemon = Daemon::new(path.clone(), cfg.clone());
    daemon.reconcile();

    // Wire the API to the workers so triggers reach them.
    let commands: api::Commands = Arc::new(Mutex::new(std::collections::BTreeMap::new()));
    {
        let shared = daemon.shared.clone();
        let commands = commands.clone();
        // Workers are spawned inside Daemon; take their senders by name.
        for project in cfg.projects.iter() {
            if let Some(tx) = daemon_command_sender(&daemon, &project.name) {
                commands
                    .lock()
                    .expect("commands")
                    .insert(project.name.clone(), tx);
            }
        }
        drop(shared);
    }

    let listen = cfg.server.as_ref().map(|s| s.listen.clone());
    let api_state = Arc::new(api::ApiState {
        shared: daemon.shared.clone(),
        commands,
        config_path: path.clone(),
        reloads: Arc::new(Mutex::new(0)),
    });

    let mut api_thread = None;
    if let Some(listen) = listen {
        let state = api_state.clone();
        let handle = std::thread::Builder::new()
            .name("lunacy-api".into())
            .spawn(move || {
                if let Err(e) = api::serve(state, &listen) {
                    log::error!("API server stopped: {e:#}");
                }
            })
            .context("spawning the API thread")?;
        api_thread = Some(handle);
    } else {
        log::info!("no [server] section: the HTTP API is disabled");
    }

    // Ctrl-C: shut the workers down cleanly.
    let shared = daemon.shared.clone();
    let _ = ctrlc_handler(shared);

    log::info!("daemon running; press Ctrl-C to stop");
    daemon.wait();
    daemon.shutdown();
    if let Some(handle) = api_thread {
        // The API thread parks on accept; it ends with the process.
        let _ = handle;
    }
    log::info!("stopped");
    Ok(())
}

/// Reach into a `Daemon` for a worker's command channel.
fn daemon_command_sender(daemon: &Daemon, name: &str) -> Option<std::sync::mpsc::Sender<Command>> {
    daemon.command_sender(name)
}

fn ctrlc_handler(shared: Arc<Mutex<Shared>>) -> Result<()> {
    #[cfg(unix)]
    {
        use std::sync::atomic::AtomicBool;
        static STOP: AtomicBool = AtomicBool::new(false);
        extern "C" fn handle(_: libc::c_int) {
            STOP.store(true, std::sync::atomic::Ordering::SeqCst);
        }
        unsafe {
            libc::signal(libc::SIGINT, handle as *const () as libc::sighandler_t);
            libc::signal(libc::SIGTERM, handle as *const () as libc::sighandler_t);
        }
        std::thread::Builder::new()
            .name("lunacy-signals".into())
            .spawn(move || loop {
                if STOP.load(std::sync::atomic::Ordering::SeqCst) {
                    if let Ok(mut s) = shared.lock() {
                        s.shutdown = true;
                    }
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            })?;
    }
    #[cfg(not(unix))]
    {
        let _ = shared;
    }
    Ok(())
}
