//! `lunacy-cli`: the Lunacy daemon from the command line, for scripts and AI agents.
//!
//! Every command prints one JSON document (or NDJSON lines for streams) when stdout is not a
//! terminal or with `--json`, and exits with a code from [`Exit`]. `lunacy-cli schema` prints
//! the whole interface as JSON. See docs/cli.md.

use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use clap::{Parser, Subcommand, ValueEnum};
use lunacy_client::model::{Project, Run, RunStatus};
use lunacy_client::net::{self, Reply};
use serde_json::{json, Value};

/// Exit codes. Stable: agents branch on them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum Exit {
    Ok = 0,
    Error = 1,
    Usage = 2,
    NotFound = 3,
    Auth = 4,
    Conflict = 5,
    Unreachable = 6,
    RunFailed = 7,
    Timeout = 8,
}

const EXIT_CODES: &[(Exit, &str, &str)] = &[
    (Exit::Ok, "ok", "success; a waited-for run succeeded"),
    (
        Exit::Error,
        "error",
        "any other error (bad response, invalid config, ...)",
    ),
    (Exit::Usage, "usage", "invalid arguments"),
    (
        Exit::NotFound,
        "not_found",
        "unknown project, run or step (HTTP 404)",
    ),
    (Exit::Auth, "auth", "missing or wrong token (HTTP 401/403)"),
    (
        Exit::Conflict,
        "conflict",
        "not possible right now, e.g. already running (HTTP 409)",
    ),
    (
        Exit::Unreachable,
        "unreachable",
        "the daemon did not answer",
    ),
    (
        Exit::RunFailed,
        "run_failed",
        "a waited-for run finished as failure, error or cancelled",
    ),
    (
        Exit::Timeout,
        "timeout",
        "--timeout passed before the run finished",
    ),
];

impl Exit {
    fn code(self) -> &'static str {
        EXIT_CODES
            .iter()
            .find(|(e, ..)| *e == self)
            .map(|(_, c, _)| *c)
            .unwrap_or("error")
    }
}

const AFTER_HELP: &str = "\
Exit codes: 0 ok, 1 error, 2 usage, 3 not found, 4 auth, 5 conflict, 6 daemon unreachable,
7 waited-for run did not succeed, 8 timeout.
Output is JSON when stdout is not a terminal (or with --json). `lunacy-cli schema` describes
every command as JSON.";

#[derive(Debug, Parser)]
#[command(name = "lunacy-cli", version, about = "Talk to a Lunacy daemon (built for scripts and agents)", after_help = AFTER_HELP)]
struct Cli {
    /// Daemon URL.
    #[arg(
        long,
        env = "LUNACY_SERVER",
        default_value = "http://127.0.0.1:7878",
        global = true
    )]
    server: String,
    /// API token. Prefer LUNACY_TOKEN or --token-file: a flag shows up in `ps`.
    #[arg(long, env = "LUNACY_TOKEN", hide_env_values = true, global = true)]
    token: Option<String>,
    /// File holding the API token [default: ~/.config/lunacy/api-token if it exists].
    #[arg(long, env = "LUNACY_TOKEN_FILE", global = true)]
    token_file: Option<PathBuf>,
    /// Output format [default: text on a terminal, json otherwise].
    #[arg(long, value_enum, global = true)]
    format: Option<Format>,
    /// Same as --format json.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Format {
    Text,
    Json,
}

#[derive(Debug, Subcommand)]
enum Cmd {
    /// Daemon health, versions, tools, and a one-line summary per project.
    Status,
    /// Reachability, auth and the daemon's tools; exits non-zero when something is wrong.
    Doctor,
    /// All projects.
    Projects,
    /// One project.
    Project { name: String },
    /// A project's recent runs, newest first.
    Runs {
        project: String,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// One run, with its steps and act substeps.
    Run { id: String },
    /// A run's step log. Without --step: the failed step, else the last one.
    Logs {
        run_id: String,
        /// Step index (0-based, as in `run`).
        #[arg(long)]
        step: Option<usize>,
        /// Only the last N lines.
        #[arg(long)]
        tail: Option<usize>,
        /// Keep printing new lines until the run finishes (NDJSON in JSON mode).
        #[arg(long)]
        follow: bool,
    },
    /// Start a run.
    Trigger {
        project: String,
        /// Commit to run (default: the branch's upstream head).
        #[arg(long)]
        sha: Option<String>,
        /// Run even if this commit was already processed.
        #[arg(long)]
        force: bool,
        /// Skip CI and run only the deploy steps.
        #[arg(long)]
        deploy_only: bool,
        /// Why; shown in the UI, GitHub checks and Discord.
        #[arg(long)]
        reason: Option<String>,
        /// Wait for the run to finish: progress on stderr, the final run on stdout.
        #[arg(long)]
        wait: bool,
        /// With --wait: give up after this many seconds (exit 8).
        #[arg(long)]
        timeout: Option<u64>,
    },
    /// Wait until a run finishes; exit 0 if it succeeded, 7 if not, 8 on timeout.
    Wait {
        run_id: String,
        #[arg(long)]
        timeout: Option<u64>,
    },
    /// Cancel a run (by id) or a project's running and queued runs (by name).
    Cancel { target: String },
    /// Run a finished run's commit again.
    Retry {
        run_id: String,
        #[arg(long)]
        reason: Option<String>,
    },
    /// Read or validate config.
    Config {
        #[command(subcommand)]
        cmd: ConfigCmd,
    },
    /// A project's workflows and jobs, for act steps.
    Workflows { project: String },
    /// The GitHub webhook endpoint and its last delivery.
    Webhooks,
    /// Print every command, argument, exit code and output shape as JSON.
    Schema,
}

#[derive(Debug, Subcommand)]
enum ConfigCmd {
    /// The daemon's config, secrets masked; --path picks one value (e.g. `general.poll_interval`).
    Get {
        #[arg(long)]
        path: Option<String>,
    },
    /// Validate a config file locally, without the daemon.
    Check { file: PathBuf },
}

/// A failed command: what to print and how to exit.
struct Fail {
    exit: Exit,
    message: String,
    http_status: Option<u16>,
}

impl Fail {
    fn new(exit: Exit, message: impl Into<String>) -> Self {
        Fail {
            exit,
            message: message.into(),
            http_status: None,
        }
    }
}

type Res<T> = Result<T, Fail>;

struct Ctx {
    server: String,
    token: String,
    json: bool,
}

impl Ctx {
    fn url(&self, path: &str) -> String {
        net::join(&self.server, path)
    }

    fn call(&self, method: &str, path: &str, body: Option<Value>) -> Res<String> {
        match net::request(method, &self.url(path), &self.token, body) {
            Reply::Ok(text) => Ok(text),
            Reply::Err {
                status,
                message,
                field,
            } => {
                let exit = match status {
                    401 | 403 => Exit::Auth,
                    404 => Exit::NotFound,
                    409 => Exit::Conflict,
                    _ => Exit::Error,
                };
                let message = match field {
                    Some(f) => format!("{message} (field: {f})"),
                    None => message,
                };
                Err(Fail {
                    exit,
                    message,
                    http_status: Some(status),
                })
            }
            Reply::Down(why) => Err(Fail::new(
                Exit::Unreachable,
                format!("cannot reach the daemon at {}: {why}", self.server),
            )),
        }
    }

    fn get<T: serde::de::DeserializeOwned>(&self, path: &str) -> Res<T> {
        let text = self.call("GET", path, None)?;
        serde_json::from_str(&text)
            .map_err(|e| Fail::new(Exit::Error, format!("unexpected response from {path}: {e}")))
    }

    fn get_value(&self, path: &str) -> Res<Value> {
        self.get(path)
    }

    fn post(&self, path: &str, body: Value) -> Res<Value> {
        let text = self.call("POST", path, Some(body))?;
        Ok(serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    /// Print the command's result: JSON, or `text` on a terminal.
    fn out(&self, value: &Value, text: impl FnOnce() -> String) {
        if self.json {
            println!("{value}");
        } else {
            println!("{}", text());
        }
    }

    /// Progress for long commands; stderr so stdout stays one document.
    fn progress(&self, value: Value, text: impl FnOnce() -> String) {
        if self.json {
            eprintln!("{value}");
        } else {
            eprintln!("{}", text());
        }
    }
}

fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => {
            // --help and --version are not errors.
            let usage = e.use_stderr();
            let _ = e.print();
            return ExitCode::from(if usage { Exit::Usage as u8 } else { 0 });
        }
    };
    let json = cli.json
        || match cli.format {
            Some(f) => f == Format::Json,
            None => !std::io::stdout().is_terminal(),
        };
    let ctx = Ctx {
        server: cli.server.trim_end_matches('/').to_string(),
        token: resolve_token(cli.token, cli.token_file),
        json,
    };
    match dispatch(&ctx, cli.cmd) {
        Ok(exit) => ExitCode::from(exit as u8),
        Err(f) => {
            if ctx.json {
                let err = json!({"error": {
                    "code": f.exit.code(),
                    "exit": f.exit as u8,
                    "message": f.message,
                    "http_status": f.http_status,
                }});
                eprintln!("{err}");
            } else {
                eprintln!("lunacy-cli: {}", f.message);
            }
            ExitCode::from(f.exit as u8)
        }
    }
}

/// `--token`/`LUNACY_TOKEN`, else `--token-file`/`LUNACY_TOKEN_FILE`, else
/// `~/.config/lunacy/api-token` when it exists; else no token.
fn resolve_token(token: Option<String>, file: Option<PathBuf>) -> String {
    if let Some(t) = token
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
    {
        return t;
    }
    let file = file.or_else(|| {
        std::env::var_os("HOME")
            .map(|h| PathBuf::from(h).join(".config/lunacy/api-token"))
            .filter(|p| p.is_file())
    });
    file.and_then(|p| std::fs::read_to_string(p).ok())
        .map(|t| t.trim().to_string())
        .unwrap_or_default()
}

fn dispatch(ctx: &Ctx, cmd: Cmd) -> Res<Exit> {
    match cmd {
        Cmd::Status => status(ctx),
        Cmd::Doctor => doctor(ctx),
        Cmd::Projects => {
            let raw = ctx.get_value("/api/projects")?;
            let list: Vec<Project> = decode(&raw)?;
            ctx.out(&raw, || {
                list.iter().map(project_line).collect::<Vec<_>>().join("\n")
            });
            Ok(Exit::Ok)
        }
        Cmd::Project { name } => {
            let raw = ctx.get_value(&format!("/api/projects/{}", net::seg(&name)))?;
            let p: Project = decode(&raw)?;
            ctx.out(&raw, || project_text(&p));
            Ok(Exit::Ok)
        }
        Cmd::Runs { project, limit } => {
            let raw = ctx.get_value(&format!(
                "/api/projects/{}/runs?limit={limit}",
                net::seg(&project)
            ))?;
            let runs: Vec<Run> = decode(&raw)?;
            ctx.out(&raw, || {
                runs.iter().map(run_line).collect::<Vec<_>>().join("\n")
            });
            Ok(Exit::Ok)
        }
        Cmd::Run { id } => {
            let raw = ctx.get_value(&format!("/api/runs/{}", net::seg(&id)))?;
            let run: Run = decode(&raw)?;
            ctx.out(&raw, || run_text(&run));
            Ok(Exit::Ok)
        }
        Cmd::Logs {
            run_id,
            step,
            tail,
            follow,
        } => logs(ctx, &run_id, step, tail, follow),
        Cmd::Trigger {
            project,
            sha,
            force,
            deploy_only,
            reason,
            wait,
            timeout,
        } => {
            if timeout.is_some() && !wait {
                return Err(Fail::new(Exit::Usage, "--timeout needs --wait"));
            }
            trigger(
                ctx,
                &project,
                sha,
                force,
                deploy_only,
                reason,
                wait.then_some(timeout),
            )
        }
        Cmd::Wait { run_id, timeout } => {
            let run = wait_for(ctx, &run_id, timeout)?;
            finish(ctx, &run)
        }
        Cmd::Cancel { target } => cancel(ctx, &target),
        Cmd::Retry { run_id, reason } => {
            let reason = reason.unwrap_or_else(|| format!("Retry of {run_id} (lunacy-cli)"));
            let v = ctx.post(
                &format!("/api/runs/{}/retry", net::seg(&run_id)),
                json!({ "reason": reason }),
            )?;
            ctx.out(&v, || format!("queued retry of {run_id}"));
            Ok(Exit::Ok)
        }
        Cmd::Config {
            cmd: ConfigCmd::Get { path },
        } => {
            let cfg = ctx.get_value("/api/config")?;
            let value = match &path {
                Some(p) => pick(&cfg, p).cloned().ok_or_else(|| {
                    Fail::new(Exit::NotFound, format!("no config value at `{p}`"))
                })?,
                None => cfg,
            };
            ctx.out(&value, || match &value {
                Value::String(s) => s.clone(),
                other => serde_json::to_string_pretty(other).unwrap_or_default(),
            });
            Ok(Exit::Ok)
        }
        Cmd::Config {
            cmd: ConfigCmd::Check { file },
        } => config_check(ctx, &file),
        Cmd::Workflows { project } => {
            let raw = ctx.get_value(&format!("/api/projects/{}/workflows", net::seg(&project)))?;
            let list: Vec<lunacy_client::model::Workflow> = decode(&raw)?;
            ctx.out(&raw, || {
                list.iter()
                    .map(|w| {
                        let jobs: Vec<&str> = w.jobs.iter().map(|j| j.id.as_str()).collect();
                        format!("{}  {}  jobs: {}", w.file, w.name, jobs.join(", "))
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            });
            Ok(Exit::Ok)
        }
        Cmd::Webhooks => {
            let sys = ctx.get_value("/api/system")?;
            let hook = sys.get("webhook").cloned().unwrap_or(Value::Null);
            ctx.out(&hook, || {
                if hook.is_null() {
                    return "no webhook info (the daemon does not report one)".into();
                }
                let s = |k: &str| {
                    hook.get(k)
                        .and_then(Value::as_str)
                        .unwrap_or("-")
                        .to_string()
                };
                format!(
                    "configured: {}\nendpoint: {}\ndeliveries: {}\nlast: {} {} ({}) at {}",
                    hook.get("configured")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                    s("endpoint"),
                    hook.get("deliveries").and_then(Value::as_u64).unwrap_or(0),
                    s("last_event"),
                    s("last_repo"),
                    s("last_outcome"),
                    s("last_received_at"),
                )
            });
            Ok(Exit::Ok)
        }
        Cmd::Schema => {
            println!(
                "{}",
                serde_json::to_string_pretty(&schema()).unwrap_or_default()
            );
            Ok(Exit::Ok)
        }
    }
}

fn decode<T: serde::de::DeserializeOwned>(v: &Value) -> Res<T> {
    serde_json::from_value(v.clone())
        .map_err(|e| Fail::new(Exit::Error, format!("unexpected response: {e}")))
}

fn status(ctx: &Ctx) -> Res<Exit> {
    let health = ctx.get_value("/api/health")?;
    let system = ctx.get_value("/api/system")?;
    let projects_raw = ctx.get_value("/api/projects")?;
    let projects: Vec<Project> = decode(&projects_raw)?;
    let summary: Vec<Value> = projects
        .iter()
        .map(|p| {
            let last = p.last_run.as_ref();
            json!({
                "name": p.name,
                "enabled": p.enabled,
                "running": p.running,
                "active_runs": p.active_runs.iter().map(|r| &r.id).collect::<Vec<_>>(),
                "upstream_sha": p.upstream_sha,
                "last_sha": p.last_sha,
                "last_run": last.map(|r| json!({
                    "id": r.id, "status": r.status, "sha": r.sha,
                    "failed_step": r.failed_step, "finished_at": r.finished_at,
                })),
                "last_error": p.last_error,
            })
        })
        .collect();
    let out = json!({ "health": health, "system": system, "projects": summary });
    ctx.out(&out, || {
        let mut lines = vec![format!(
            "daemon {} at {} ({})",
            health["version"].as_str().unwrap_or("?"),
            ctx.server,
            system["os"].as_str().unwrap_or("?")
        )];
        lines.extend(projects.iter().map(project_line));
        lines.join("\n")
    });
    Ok(Exit::Ok)
}

fn doctor(ctx: &Ctx) -> Res<Exit> {
    let mut checks: Vec<Value> = Vec::new();
    let mut add = |name: &str, ok: bool, detail: String| {
        checks.push(json!({ "check": name, "ok": ok, "detail": detail }));
    };
    let health = ctx.get_value("/api/health");
    let reachable = health.is_ok();
    add(
        "reachable",
        reachable,
        match &health {
            Ok(h) => format!(
                "daemon {} at {}",
                h["version"].as_str().unwrap_or("?"),
                ctx.server
            ),
            Err(f) => f.message.clone(),
        },
    );
    if reachable {
        match ctx.get_value("/api/system") {
            Ok(sys) => {
                add("auth", true, "token accepted".into());
                let tool = |v: &Value| {
                    v["version"]
                        .as_str()
                        .map(str::to_string)
                        .filter(|s| !s.is_empty())
                };
                let act = tool(&sys["act"]);
                add(
                    "act",
                    act.is_some(),
                    act.unwrap_or_else(|| "act not found on the daemon's PATH".into()),
                );
                let git = tool(&sys["git"]);
                add(
                    "git",
                    git.is_some(),
                    git.unwrap_or_else(|| "git not found".into()),
                );
                let docker_ok = sys["docker"]["ok"].as_bool().unwrap_or(false);
                add(
                    "docker",
                    docker_ok,
                    if docker_ok {
                        "docker daemon reachable".into()
                    } else {
                        sys["docker"]["error"]
                            .as_str()
                            .unwrap_or("docker unreachable")
                            .to_string()
                    },
                );
            }
            Err(f) => add(
                "auth",
                false,
                if f.exit == Exit::Auth {
                    "token missing or rejected: set LUNACY_TOKEN or --token-file".into()
                } else {
                    f.message
                },
            ),
        }
    }
    let ok = checks.iter().all(|c| c["ok"] == true);
    let out = json!({ "ok": ok, "checks": checks });
    ctx.out(&out, || {
        checks
            .iter()
            .map(|c| {
                format!(
                    "{} {:<9} {}",
                    if c["ok"] == true { "ok  " } else { "FAIL" },
                    c["check"].as_str().unwrap_or(""),
                    c["detail"].as_str().unwrap_or("")
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    });
    Ok(if ok {
        Exit::Ok
    } else if !reachable {
        Exit::Unreachable
    } else if checks
        .iter()
        .any(|c| c["check"] == "auth" && c["ok"] == false)
    {
        Exit::Auth
    } else {
        Exit::Error
    })
}

fn trigger(
    ctx: &Ctx,
    project: &str,
    sha: Option<String>,
    force: bool,
    deploy_only: bool,
    reason: Option<String>,
    wait: Option<Option<u64>>,
) -> Res<Exit> {
    let path = format!("/api/projects/{}", net::seg(project));
    // The trigger answer carries no run id: remember what exists, then watch for the new run.
    let before: Vec<Run> = ctx.get(&format!("{path}/runs?limit=50"))?;
    let reason = reason.unwrap_or_else(|| "lunacy-cli trigger".to_string());
    let (endpoint, body) = if deploy_only {
        ("deploy", json!({ "sha": sha, "reason": reason }))
    } else {
        (
            "trigger",
            json!({ "sha": sha, "force": force, "reason": reason }),
        )
    };
    let queued = ctx.post(&format!("{path}/{endpoint}"), body)?;
    let Some(timeout) = wait else {
        ctx.out(&queued, || format!("queued a run of {project}"));
        return Ok(Exit::Ok);
    };
    let started = Instant::now();
    let deadline = timeout.map(|s| started + Duration::from_secs(s));
    let seen: Vec<&str> = before.iter().map(|r| r.id.as_str()).collect();
    let run_id = loop {
        let runs: Vec<Run> = ctx.get(&format!("{path}/runs?limit=50"))?;
        if let Some(r) = runs.iter().find(|r| !seen.contains(&r.id.as_str())) {
            break r.id.clone();
        }
        if deadline.is_some_and(|d| Instant::now() >= d) {
            return Err(Fail::new(
                Exit::Timeout,
                "timed out waiting for the run to start",
            ));
        }
        std::thread::sleep(Duration::from_millis(500));
    };
    ctx.progress(json!({ "event": "started", "run_id": run_id }), || {
        format!("started {run_id}")
    });
    let left = deadline.map(|d| d.saturating_duration_since(Instant::now()).as_secs().max(1));
    let run = wait_for(ctx, &run_id, left)?;
    finish(ctx, &run)
}

/// Poll a run until it finishes, reporting each step change on stderr.
fn wait_for(ctx: &Ctx, run_id: &str, timeout: Option<u64>) -> Res<Run> {
    let deadline = timeout.map(|s| Instant::now() + Duration::from_secs(s));
    let path = format!("/api/runs/{}", net::seg(run_id));
    let mut last_states: Vec<String> = Vec::new();
    loop {
        let run: Run = ctx.get(&path)?;
        let states: Vec<String> = run
            .steps
            .iter()
            .map(|s| format!("{}:{:?}", s.name, s.status))
            .collect();
        if states != last_states {
            for (i, s) in run.steps.iter().enumerate() {
                if last_states.get(i) != states.get(i) {
                    ctx.progress(
                        json!({ "event": "step", "run_id": run.id, "index": i,
                                "name": s.name, "status": s.status }),
                        || format!("  step {i} {}: {}", s.name, s.status.label()),
                    );
                }
            }
            last_states = states;
        }
        if !run.status.is_running() {
            return Ok(run);
        }
        if deadline.is_some_and(|d| Instant::now() >= d) {
            return Err(Fail::new(
                Exit::Timeout,
                format!("run {run_id} still running after {}s", timeout.unwrap_or(0)),
            ));
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

/// Print a finished run; exit 0 if it succeeded, 7 otherwise.
fn finish(ctx: &Ctx, run: &Run) -> Res<Exit> {
    let value = serde_json::to_value(run).unwrap_or(Value::Null);
    ctx.out(&value, || run_text(run));
    Ok(if run.status == RunStatus::Success {
        Exit::Ok
    } else {
        Exit::RunFailed
    })
}

fn logs(
    ctx: &Ctx,
    run_id: &str,
    step: Option<usize>,
    tail: Option<usize>,
    follow: bool,
) -> Res<Exit> {
    let run_path = format!("/api/runs/{}", net::seg(run_id));
    let run: Run = ctx.get(&run_path)?;
    let index = match step {
        Some(i) if i < run.steps.len() => i,
        Some(i) => {
            return Err(Fail::new(
                Exit::NotFound,
                format!("run {run_id} has no step {i}"),
            ))
        }
        None => run
            .failed_step
            .as_deref()
            .and_then(|name| run.steps.iter().position(|s| s.name == name))
            .or_else(|| {
                run.steps
                    .iter()
                    .rposition(|s| s.status != lunacy_client::model::StepStatus::Pending)
            })
            .unwrap_or(0),
    };
    let log_path = |tail: Option<usize>| match tail {
        Some(n) => format!("{run_path}/steps/{index}/log?tail={n}"),
        None => format!("{run_path}/steps/{index}/log"),
    };
    let step_name = run
        .steps
        .get(index)
        .map(|s| s.name.clone())
        .unwrap_or_default();
    if !follow {
        let text = ctx.call("GET", &log_path(tail), None)?;
        let out = json!({ "run_id": run.id, "step": index, "name": step_name, "log": text });
        ctx.out(&out, || text.trim_end().to_string());
        return Ok(Exit::Ok);
    }
    // Follow: print lines as the log grows, until the run is over and the log stops growing.
    let mut printed = 0usize;
    let mut first = true;
    loop {
        let text = ctx.call("GET", &log_path(None), None)?;
        let lines: Vec<&str> = text.lines().collect();
        let start = if first {
            first = false;
            tail.map(|n| lines.len().saturating_sub(n)).unwrap_or(0)
        } else {
            printed.min(lines.len())
        };
        for line in &lines[start..] {
            if ctx.json {
                println!(
                    "{}",
                    json!({ "run_id": run.id, "step": index, "line": line })
                );
            } else {
                println!("{line}");
            }
        }
        printed = lines.len();
        let now: Run = ctx.get(&run_path)?;
        let step_live = now.steps.get(index).is_some_and(|s| s.status.is_live());
        if !now.status.is_running()
            || (!step_live
                && now
                    .steps
                    .get(index)
                    .is_some_and(|s| s.duration_ms.is_some()))
        {
            let tail_text = ctx.call("GET", &log_path(None), None)?;
            for line in tail_text.lines().skip(printed) {
                if ctx.json {
                    println!(
                        "{}",
                        json!({ "run_id": run.id, "step": index, "line": line })
                    );
                } else {
                    println!("{line}");
                }
            }
            return Ok(Exit::Ok);
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

fn cancel(ctx: &Ctx, target: &str) -> Res<Exit> {
    // A run id contains the project name plus a timestamp and sha; try the run first.
    let as_run = ctx.post(&format!("/api/runs/{}/cancel", net::seg(target)), json!({}));
    let value = match as_run {
        Ok(v) => v,
        Err(f) if f.exit == Exit::NotFound => ctx.post(
            &format!("/api/projects/{}/cancel", net::seg(target)),
            json!({}),
        )?,
        Err(f) => return Err(f),
    };
    ctx.out(&value, || format!("cancel requested for {target}"));
    Ok(Exit::Ok)
}

fn config_check(ctx: &Ctx, file: &std::path::Path) -> Res<Exit> {
    // Parse without validating (`Config::load` would), so a validation error keeps its field.
    let text = std::fs::read_to_string(file)
        .map_err(|e| Fail::new(Exit::Error, format!("reading {}: {e}", file.display())))?;
    let cfg: lunacy_core::config::Config = match toml::from_str(&text) {
        Ok(cfg) => cfg,
        Err(e) => {
            let out = json!({ "ok": false, "file": file.display().to_string(),
                              "field": Value::Null, "message": e.to_string() });
            ctx.out(&out, || format!("{}: {e}", file.display()));
            return Ok(Exit::Error);
        }
    };
    match cfg.validate() {
        Ok(()) => {
            let out = json!({
                "ok": true,
                "file": file.display().to_string(),
                "projects": cfg.projects.iter().map(|p| &p.name).collect::<Vec<_>>(),
            });
            ctx.out(&out, || {
                format!(
                    "config ok: {} ({} projects)",
                    file.display(),
                    cfg.projects.len()
                )
            });
            Ok(Exit::Ok)
        }
        Err(e) => {
            let out = json!({ "ok": false, "file": file.display().to_string(),
                              "field": e.field, "message": e.message });
            ctx.out(&out, || {
                format!(
                    "{}: {}: {}",
                    file.display(),
                    e.field.as_deref().unwrap_or("config"),
                    e.message
                )
            });
            Ok(Exit::Error)
        }
    }
}

/// `a.b.0.c` into a JSON value.
fn pick<'a>(v: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.')
        .filter(|s| !s.is_empty())
        .try_fold(v, |cur, key| match cur {
            Value::Object(map) => map.get(key),
            Value::Array(arr) => key.parse::<usize>().ok().and_then(|i| arr.get(i)),
            _ => None,
        })
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(7)]
}

fn project_line(p: &Project) -> String {
    let state = if !p.enabled {
        "paused".to_string()
    } else if p.running {
        format!("running ({})", p.active_runs.len().max(1))
    } else {
        p.last_run
            .as_ref()
            .map(|r| r.status.label().to_lowercase())
            .unwrap_or_else(|| "idle".into())
    };
    let last = p
        .last_run
        .as_ref()
        .map(|r| format!(" last {} {}", short(&r.sha), r.id))
        .unwrap_or_default();
    format!("{:<16} {:<14}{}", p.name, state, last)
}

fn project_text(p: &Project) -> String {
    let mut s = project_line(p);
    s.push_str(&format!(
        "\nbranch {}  upstream {}  processed {}",
        p.branch,
        p.upstream_sha.as_deref().map(short).unwrap_or("-"),
        p.last_sha.as_deref().map(short).unwrap_or("-"),
    ));
    if let Some(e) = &p.last_error {
        s.push_str(&format!("\nlast error: {e}"));
    }
    s
}

fn run_line(r: &Run) -> String {
    format!(
        "{:<36} {:<9} {} {}{}",
        r.id,
        r.status.label().to_lowercase(),
        short(&r.sha),
        r.subject,
        r.failed_step
            .as_deref()
            .map(|s| format!("  [failed: {s}]"))
            .unwrap_or_default()
    )
}

fn run_text(r: &Run) -> String {
    let mut s = run_line(r);
    if let Some(reason) = r.trigger_display() {
        s.push_str(&format!("\n{reason}"));
    }
    for (i, step) in r.steps.iter().enumerate() {
        s.push_str(&format!(
            "\n  {i} {:<8} {:<24} {}",
            format!("{:?}", step.phase).to_lowercase(),
            step.name,
            step.status.label()
        ));
        for sub in &step.substeps {
            s.push_str(&format!(
                "\n       {} / {}: {}",
                sub.job,
                sub.name,
                sub.status.label()
            ));
        }
    }
    s
}

/// The whole interface, for agents to read instead of `--help`.
fn schema() -> Value {
    let arg =
        |name: &str, kind: &str, help: &str| json!({ "name": name, "type": kind, "help": help });
    let cmd = |name: &str, about: &str, args: Vec<Value>, output: &str| json!({ "name": name, "about": about, "args": args, "output": output });
    json!({
        "name": "lunacy-cli",
        "version": env!("CARGO_PKG_VERSION"),
        "global_args": [
            arg("--server", "url", "daemon URL; env LUNACY_SERVER; default http://127.0.0.1:7878"),
            arg("--token", "string", "API token; env LUNACY_TOKEN"),
            arg("--token-file", "path", "token file; env LUNACY_TOKEN_FILE; default ~/.config/lunacy/api-token"),
            arg("--format", "text|json", "default: json when stdout is not a terminal"),
            arg("--json", "flag", "same as --format json"),
        ],
        "output": "one JSON document on stdout per command; streams (logs --follow) print NDJSON. \
                   Progress of trigger --wait / wait goes to stderr as NDJSON events \
                   ({\"event\":\"started\"|\"step\", ...}). Errors in JSON mode: stderr \
                   {\"error\":{\"code\",\"exit\",\"message\",\"http_status\"}}.",
        "exit_codes": EXIT_CODES.iter().map(|(e, code, help)| json!({
            "exit": *e as u8, "code": code, "help": help,
        })).collect::<Vec<_>>(),
        "commands": [
            cmd("status", "health, system info and a summary per project", vec![],
                "{health, system, projects:[{name, enabled, running, active_runs, upstream_sha, last_sha, last_run:{id,status,sha,failed_step,finished_at}, last_error}]}"),
            cmd("doctor", "reachability, auth, act/git/docker on the daemon; non-zero exit on problems", vec![],
                "{ok, checks:[{check, ok, detail}]}"),
            cmd("projects", "all projects", vec![], "[Project] (docs/api.md)"),
            cmd("project", "one project", vec![arg("name", "string", "project name")], "Project"),
            cmd("runs", "a project's runs, newest first",
                vec![arg("project", "string", "project name"), arg("--limit", "int", "default 20, max 200")], "[Run]"),
            cmd("run", "one run with steps and act substeps", vec![arg("id", "string", "run id")], "Run"),
            cmd("logs", "a step's log",
                vec![arg("run_id", "string", "run id"),
                     arg("--step", "int", "step index; default: the failed step, else the last started one"),
                     arg("--tail", "int", "last N lines"),
                     arg("--follow", "flag", "stream until the run finishes")],
                "{run_id, step, name, log}; with --follow: NDJSON {run_id, step, line}"),
            cmd("trigger", "start a run",
                vec![arg("project", "string", "project name"),
                     arg("--sha", "string", "commit; default upstream head"),
                     arg("--force", "flag", "run even if already processed"),
                     arg("--deploy-only", "flag", "skip CI, run deploy steps"),
                     arg("--reason", "string", "shown in UI, GitHub and Discord"),
                     arg("--wait", "flag", "wait for the run; exit 0 success, 7 otherwise"),
                     arg("--timeout", "seconds", "with --wait; exit 8 when exceeded")],
                "{queued:true}; with --wait: the finished Run"),
            cmd("wait", "wait for a run to finish",
                vec![arg("run_id", "string", "run id"), arg("--timeout", "seconds", "exit 8 when exceeded")],
                "the finished Run; exit 0 success, 7 otherwise"),
            cmd("cancel", "cancel a run, or a project's running and queued runs",
                vec![arg("target", "string", "run id or project name")],
                "{cancelling, cancelled, orphaned, run_id, project}"),
            cmd("retry", "re-run a finished run's commit",
                vec![arg("run_id", "string", "run id"), arg("--reason", "string", "why")],
                "{queued, project, sha, run_id}"),
            cmd("config get", "the daemon's config, secrets masked",
                vec![arg("--path", "string", "dotted path, e.g. project.0.ci.0.run")], "Config or the value at --path"),
            cmd("config check", "validate a config file locally (no daemon)",
                vec![arg("file", "path", "config.toml")],
                "{ok, file, projects} or {ok:false, file, field, message} with exit 1"),
            cmd("workflows", "a project's workflows and jobs",
                vec![arg("project", "string", "project name")], "[{file, name, jobs:[{id, name, runs_on}]}]"),
            cmd("webhooks", "GitHub webhook endpoint and last delivery", vec![],
                "{configured, endpoint, deliveries, last_event, last_repo, last_delivery_id, last_received_at, last_outcome} or null"),
            cmd("schema", "this description", vec![], "this document"),
        ],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_lists_every_subcommand() {
        use clap::CommandFactory;
        let names: Vec<String> = schema()["commands"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["name"].as_str().unwrap().to_string())
            .collect();
        for sub in Cli::command().get_subcommands() {
            let name = sub.get_name();
            if sub.has_subcommands() {
                for inner in sub.get_subcommands() {
                    let full = format!("{name} {}", inner.get_name());
                    assert!(names.contains(&full), "schema misses `{full}`");
                }
            } else {
                assert!(names.contains(&name.to_string()), "schema misses `{name}`");
            }
        }
        Cli::command().debug_assert();
    }

    #[test]
    fn pick_walks_objects_and_arrays() {
        let v = json!({"project": [{"name": "a", "ci": [{"run": "make"}]}]});
        assert_eq!(pick(&v, "project.0.ci.0.run"), Some(&json!("make")));
        assert_eq!(pick(&v, "project.1"), None);
        assert_eq!(pick(&v, "nope"), None);
    }
}
