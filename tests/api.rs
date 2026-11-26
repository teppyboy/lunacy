//! HTTP API behaviour against a real `tiny_http` server on a loopback port.
#![allow(dead_code)]

use lunacy::api::{self, ApiState, Commands};
use lunacy::config::{Checkout, Config, ProjectCfg, StepCfg, StepKind};
use lunacy::daemon::{Command, ProjectState, Shared};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

struct HttpReply {
    status: u16,
    body: String,
    headers: String,
}

fn request_with_headers(
    port: u16,
    method: &str,
    path: &str,
    token: Option<&str>,
    extra_headers: &[(&str, &str)],
    body: Option<&str>,
) -> HttpReply {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n");
    if let Some(t) = token {
        req.push_str(&format!("Authorization: Bearer {t}\r\n"));
    }
    for (k, v) in extra_headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    if let Some(b) = body {
        req.push_str("Content-Type: application/json\r\n");
        req.push_str(&format!("Content-Length: {}\r\n", b.len()));
    }
    req.push_str("\r\n");
    if let Some(b) = body {
        req.push_str(b);
    }
    stream.write_all(req.as_bytes()).expect("write");
    stream.flush().ok();

    let mut reader = BufReader::new(stream);
    let mut status_line = String::new();
    reader.read_line(&mut status_line).expect("status line");
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    let mut headers = String::new();
    let mut content_length: Option<usize> = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            break;
        }
        if line == "\r\n" {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            if k.trim().eq_ignore_ascii_case("content-length") {
                content_length = v.trim().parse().ok();
            }
        }
        headers.push_str(&line);
    }
    let mut body = String::new();
    match content_length {
        Some(n) => {
            let mut buf = vec![0u8; n];
            reader.read_exact(&mut buf).ok();
            body = String::from_utf8_lossy(&buf).to_string();
        }
        None => {
            reader.read_to_string(&mut body).ok();
        }
    }
    HttpReply {
        status,
        body,
        headers,
    }
}

fn request(
    port: u16,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Option<&str>,
) -> HttpReply {
    request_with_headers(port, method, path, token, &[], body)
}

struct Server {
    port: u16,
    root: PathBuf,
    state_dir: PathBuf,
    config_path: PathBuf,
    shared: Arc<Mutex<Shared>>,
    commands: Commands,
    _dir: tempfile::TempDir,
    _thread: std::thread::JoinHandle<()>,
}

fn shell_step(name: &str, run: &str) -> StepCfg {
    StepCfg {
        name: name.into(),
        kind: StepKind::Shell,
        run: Some(run.into()),
        workflow: None,
        job: None,
        event: None,
        platforms: Default::default(),
        args: Vec::new(),
        secrets: Default::default(),
        vars: Default::default(),
        only_on: Vec::new(),
        timeout: Some(60),
        cwd: None,
        shell: Some(vec!["bash".into(), "-c".into()]),
        env: Default::default(),
    }
}

fn project(root: &std::path::Path) -> ProjectCfg {
    ProjectCfg {
        name: "demo".into(),
        repo_path: root.join("repo"),
        remote: "origin".into(),
        branch: "master".into(),
        github_repo: Some("teppyboy/demo".into()),
        checkout: Checkout::None,
        poll_interval: None,
        step_timeout: None,
        timeout: None,
        env: Default::default(),
        contexts: Default::default(),
        discord_webhook_url: None,
        notify_on_start: None,
        shell: None,
        platforms: Default::default(),
        watch_prs: false,
        deploy_refs: Vec::new(),
        watch_tags: false,
        cache: None,
        ci: vec![shell_step("Test", "true")],
        deploy: Vec::new(),
    }
}

/// Start a daemon-less API server with one configured project.
fn start(token: &str) -> Server {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().to_path_buf();
    let state_dir = root.join("state");
    std::fs::create_dir_all(&state_dir).unwrap();

    let mut cfg = Config::default();
    cfg.general.state_dir = Some(state_dir.clone());
    cfg.server = Some(lunacy::config::ServerCfg {
        listen: "127.0.0.1:0".into(),
        token: token.into(),
        token_file: None,
        web_root: None,
        public_url: None,
    });
    cfg.projects = vec![project(&root)];
    let config_path = root.join("config.toml");
    std::fs::write(&config_path, toml::to_string(&cfg).unwrap()).unwrap();

    // A real port, picked by the OS, then handed to tiny_http.
    let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = probe.local_addr().unwrap().port();
    drop(probe);

    let shared = Arc::new(Mutex::new(Shared {
        config: cfg.clone(),
        state_dir: state_dir.clone(),
        projects: vec![ProjectState::new_public(&cfg.projects[0], &state_dir)],
        shutdown: false,
    }));
    let commands: Commands = Arc::new(Mutex::new(BTreeMap::new()));
    let state = Arc::new(ApiState {
        shared: shared.clone(),
        commands: commands.clone(),
        config_path: config_path.clone(),
        reloads: Arc::new(Mutex::new(0)),
    });
    let thread = std::thread::spawn(move || {
        let _ = api::serve(state, &format!("127.0.0.1:{port}"));
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    Server {
        port,
        root,
        state_dir,
        config_path,
        shared,
        commands,
        _dir: dir,
        _thread: thread,
    }
}

fn json(v: &str) -> serde_json::Value {
    serde_json::from_str(v).unwrap_or_else(|e| panic!("invalid JSON ({e}): {v}"))
}

#[test]
fn health_needs_no_token() {
    let s = start("secret-token");
    let r = request(s.port, "GET", "/api/health", None, None);
    assert_eq!(r.status, 200);
    let v = json(&r.body);
    assert_eq!(v["ok"], true);
    assert_eq!(v["version"], lunacy::VERSION);
    assert!(r.headers.contains("Access-Control-Allow-Origin: *"));
}

#[test]
fn auth_is_required_when_a_token_is_configured() {
    let s = start("secret-token");
    let r = request(s.port, "GET", "/api/projects", None, None);
    assert_eq!(r.status, 401, "{}", r.body);
    assert!(json(&r.body)["error"].is_string(), "{}", r.body);
    let r = request(s.port, "GET", "/api/projects", Some("nope"), None);
    assert_eq!(r.status, 401);
    let r = request(s.port, "GET", "/api/projects", Some("secret-token"), None);
    assert_eq!(r.status, 200, "{}", r.body);
}

#[test]
fn no_token_means_open_access() {
    let s = start("");
    let r = request(s.port, "GET", "/api/projects", None, None);
    assert_eq!(r.status, 200, "{}", r.body);
}

#[test]
fn projects_shape_matches_the_contract() {
    let s = start("");
    let r = request(s.port, "GET", "/api/projects", None, None);
    assert_eq!(r.status, 200, "{}", r.body);
    let v = json(&r.body);
    let list = v.as_array().expect("array");
    assert_eq!(list.len(), 1);
    let p = &list[0];
    assert_eq!(p["name"], "demo");
    assert_eq!(p["branch"], "master");
    assert_eq!(p["github_repo"], "teppyboy/demo");
    assert_eq!(p["running"], false);
    assert!(p["upstream_sha"].is_null());
    assert!(p["last_sha"].is_null());
    assert!(p["current_run"].is_null());
    assert!(p["last_run"].is_null());

    let r = request(s.port, "GET", "/api/projects/demo", None, None);
    assert_eq!(r.status, 200);
    assert_eq!(json(&r.body)["name"], "demo");
    let r = request(s.port, "GET", "/api/projects/nope", None, None);
    assert_eq!(r.status, 404);

    let r = request(s.port, "GET", "/api/projects/demo/runs", None, None);
    assert_eq!(r.status, 200);
    assert_eq!(r.body.trim(), "[]");
}

#[test]
fn trigger_queues_a_run_and_it_shows_up() {
    let s = start("");
    let sha = "0123456789abcdef0123456789abcdef01234567";

    // No worker attached yet: 503.
    let r = request(
        s.port,
        "POST",
        "/api/projects/demo/trigger",
        None,
        Some(&format!(r#"{{"sha": "{sha}", "force": true}}"#)),
    );
    assert_eq!(r.status, 503, "{}", r.body);

    // An unknown project is 404 before any worker lookup.
    let r = request(
        s.port,
        "POST",
        "/api/projects/nope/trigger",
        None,
        Some("{}"),
    );
    assert_eq!(r.status, 404);

    // With a worker attached and a known upstream sha, the trigger is accepted.
    {
        let mut guard = s.shared.lock().unwrap();
        guard.projects[0].upstream_sha = Some(sha.to_string());
    }
    let (tx, rx) = std::sync::mpsc::channel();
    s.commands.lock().unwrap().insert("demo".into(), tx);

    let r = request(
        s.port,
        "POST",
        "/api/projects/demo/trigger",
        None,
        Some(r#"{"sha": null, "force": false}"#),
    );
    assert_eq!(r.status, 202, "{}", r.body);
    assert_eq!(json(&r.body)["queued"], true);
    let cmd = rx.recv_timeout(Duration::from_secs(5)).expect("command");
    match cmd {
        Command::Trigger {
            sha: target,
            trigger,
            ..
        } => {
            assert_eq!(target, None, "the worker picks its own upstream sha");
            assert_eq!(trigger, lunacy::model::Trigger::Manual);
        }
        other => panic!("unexpected command: {other:?}"),
    }

    // A run written to the state dir appears in the run list.
    let run = lunacy::model::Run::new(
        "demo",
        sha,
        "feat: from the api".into(),
        lunacy::model::Trigger::Manual,
        vec![lunacy::model::Step {
            phase: lunacy::model::Phase::Ci,
            name: "Test".into(),
            status: lunacy::model::StepStatus::Success,
            duration_ms: Some(12),
            exit_code: Some(0),
            kind: StepKind::Shell,
            substeps: Vec::new(),
        }],
    );
    lunacy::model::persist_run(&s.state_dir, &run).unwrap();
    let r = request(s.port, "GET", "/api/projects/demo/runs?limit=5", None, None);
    assert_eq!(r.status, 200);
    let list = json(&r.body);
    let list = list.as_array().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["id"], run.id);
    assert_eq!(list[0]["sha"], sha);
    assert_eq!(list[0]["subject"], "feat: from the api");
    assert_eq!(list[0]["status"], "running");
    assert_eq!(list[0]["steps"][0]["name"], "Test");
    assert_eq!(list[0]["steps"][0]["kind"], "shell");
}

#[test]
fn run_detail_and_log_tail() {
    let s = start("");
    let run = lunacy::model::Run::new(
        "demo",
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "feat: logs".into(),
        lunacy::model::Trigger::Poll,
        vec![lunacy::model::Step {
            phase: lunacy::model::Phase::Ci,
            name: "Test".into(),
            status: lunacy::model::StepStatus::Failure,
            duration_ms: Some(30),
            exit_code: Some(1),
            kind: StepKind::Shell,
            substeps: Vec::new(),
        }],
    );
    let log = lunacy::model::run_log_path(&s.state_dir, "demo", &run.id, 0);
    std::fs::create_dir_all(log.parent().unwrap()).unwrap();
    let body: String = (0..100).map(|i| format!("line {i}\n")).collect();
    std::fs::write(&log, &body).unwrap();
    lunacy::model::persist_run(&s.state_dir, &run).unwrap();

    let r = request(s.port, "GET", &format!("/api/runs/{}", run.id), None, None);
    assert_eq!(r.status, 200, "{}", r.body);
    let v = json(&r.body);
    assert_eq!(v["id"], run.id);
    assert_eq!(v["steps"][0]["status"], "failure");

    let r = request(
        s.port,
        "GET",
        &format!("/api/runs/{}/steps/0/log", run.id),
        None,
        None,
    );
    assert_eq!(r.status, 200);
    assert!(
        r.headers
            .to_lowercase()
            .contains("content-type: text/plain"),
        "{}",
        r.headers
    );
    assert!(r.body.contains("line 0"), "{}", r.body);
    assert!(r.body.contains("line 99"));

    let r = request(
        s.port,
        "GET",
        &format!("/api/runs/{}/steps/0/log?tail=5", run.id),
        None,
        None,
    );
    assert_eq!(r.status, 200);
    assert_eq!(r.body.lines().count(), 5, "{}", r.body);
    assert!(r.body.contains("line 99"));

    let r = request(
        s.port,
        "GET",
        &format!("/api/runs/{}/steps/99/log", run.id),
        None,
        None,
    );
    assert_eq!(r.status, 404);
    let r = request(s.port, "GET", "/api/runs/nope", None, None);
    assert_eq!(r.status, 404);
}

#[test]
fn config_get_masks_secrets_and_put_roundtrips() {
    let s = start("ui-token");
    {
        let mut guard = s.shared.lock().unwrap();
        guard.config.github = Some(lunacy::config::GithubCfg {
            mode: None,
            token: Some("ghp_realtoken".into()),
            token_file: None,
            api_url: None,
            app: None,
        });
        guard.config.discord = Some(lunacy::config::DiscordCfg {
            webhook_url: Some("https://discord.com/api/webhooks/a/b".into()),
            webhook_url_file: None,
            notify_on_start: false,
        });
    }

    let r = request(s.port, "GET", "/api/config", Some("ui-token"), None);
    assert_eq!(r.status, 200, "{}", r.body);
    assert!(!r.body.contains("ghp_realtoken"), "{}", r.body);
    assert!(!r.body.contains("api/webhooks/a/b"), "{}", r.body);
    assert!(r.body.contains("********"), "{}", r.body);
    let mut sent = json(&r.body);

    // PUT the masked document back with an edit; the secrets must be kept.
    sent["general"]["poll_interval"] = serde_json::json!(77);
    let r = request(
        s.port,
        "PUT",
        "/api/config",
        Some("ui-token"),
        Some(&sent.to_string()),
    );
    assert_eq!(r.status, 200, "{}", r.body);
    let written = std::fs::read_to_string(&s.config_path).unwrap();
    assert!(written.contains("poll_interval = 77"), "{written}");
    assert!(
        written.contains("ghp_realtoken"),
        "secret kept on disk: {written}"
    );
    assert!(!written.contains("********"), "{written}");
    assert_eq!(s.shared.lock().unwrap().config.general.poll_interval, 77);
    // The previous file is kept as a backup.
    assert!(s.config_path.with_extension("toml.bak").is_file());
}

#[test]
fn config_put_rejects_invalid_input_with_a_field() {
    let s = start("");
    let r = request(s.port, "GET", "/api/config", None, None);
    let mut doc = json(&r.body);
    // Break a documented rule: checks mode without an app.
    doc["github"] = serde_json::json!({"mode": "checks", "token": "x"});
    let r = request(s.port, "PUT", "/api/config", None, Some(&doc.to_string()));
    assert_eq!(r.status, 400, "{}", r.body);
    assert_eq!(json(&r.body)["field"], "github.mode", "{}", r.body);

    let r = request(s.port, "PUT", "/api/config", None, Some("{not json"));
    assert_eq!(r.status, 400);
    assert!(json(&r.body)["field"].is_null());
}

#[test]
fn workflows_endpoint_parses_the_repo() {
    let s = start("");
    let dir = s.root.join("repo").join(".github").join("workflows");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("ci.yml"),
        "name: Flutter CI\njobs:\n  ubuntu:\n    name: Analyze, test and build Web\n    runs-on: ubuntu-latest\n  windows:\n    name: Build Windows\n    runs-on: windows-latest\n",
    )
    .unwrap();

    let r = request(s.port, "GET", "/api/projects/demo/workflows", None, None);
    assert_eq!(r.status, 200, "{}", r.body);
    let v = json(&r.body);
    let list = v.as_array().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["file"], ".github/workflows/ci.yml");
    assert_eq!(list[0]["name"], "Flutter CI");
    let jobs = list[0]["jobs"].as_array().unwrap();
    assert_eq!(jobs.len(), 2);
    assert!(
        jobs.iter().any(|j| j["id"] == "ubuntu"
            && j["name"] == "Analyze, test and build Web"
            && j["runs_on"][0] == "ubuntu-latest"),
        "{jobs:?}"
    );
    assert!(
        jobs.iter()
            .any(|j| j["id"] == "windows" && j["runs_on"][0] == "windows-latest"),
        "{jobs:?}"
    );

    // `?repo_path=` also works, for the "new project" form.
    let other = s.root.join("other");
    std::fs::create_dir_all(other.join(".github/workflows")).unwrap();
    std::fs::write(
        other.join(".github/workflows/x.yml"),
        "jobs:\n  a:\n    runs-on: [self-hosted, linux]\n",
    )
    .unwrap();
    let r = request(
        s.port,
        "GET",
        &format!(
            "/api/projects/anything/workflows?repo_path={}",
            other.to_string_lossy().replace('/', "%2F")
        ),
        None,
        None,
    );
    assert_eq!(r.status, 200, "{}", r.body);
    let v = json(&r.body);
    assert_eq!(v[0]["jobs"][0]["runs_on"][0], "self-hosted", "{v}");
}

#[test]
fn system_endpoint_reports_diagnostics() {
    let s = start("");
    let r = request(s.port, "GET", "/api/system", None, None);
    assert_eq!(r.status, 200, "{}", r.body);
    let v = json(&r.body);
    assert!(v["os"].is_string());
    assert_eq!(v["version"], lunacy::VERSION);
    assert!(v["config_path"].is_string());
    assert!(v["act"].is_object());
    assert!(v["docker"]["ok"].is_boolean());
    assert!(v["git"].is_object());
}

#[test]
fn webhook_404s_without_a_secret_and_rejects_a_bad_signature() {
    let s = start("");
    let r = request(s.port, "POST", "/api/github/webhook", None, Some("{}"));
    assert_eq!(r.status, 404, "{}", r.body);

    {
        let mut guard = s.shared.lock().unwrap();
        guard.config.github = Some(lunacy::config::GithubCfg {
            mode: None,
            token: Some("x".into()),
            token_file: None,
            api_url: None,
            app: Some(lunacy::config::AppCfg {
                app_id: 1,
                private_key_path: None,
                private_key: Some("k".into()),
                installation_id: Some(1),
                webhook_secret: Some("hooksecret".into()),
                webhook_secret_file: None,
            }),
        });
    }
    let r = request(
        s.port,
        "POST",
        "/api/github/webhook",
        None,
        Some(r#"{"action":"rerequested"}"#),
    );
    assert_eq!(r.status, 401, "{}", r.body);

    // A valid signature is covered by the unit test
    // `api::tests::webhook_signature_roundtrip`.
}

#[test]
fn other_paths_are_404() {
    let s = start("");
    let r = request(s.port, "GET", "/api/does-not-exist", None, None);
    assert_eq!(r.status, 404);
    assert!(json(&r.body)["error"].is_string());
    let r = request(s.port, "DELETE", "/api/projects", None, None);
    assert_eq!(r.status, 404);
}

#[test]
fn cancel_run_and_project_endpoints() {
    use std::sync::atomic::{AtomicBool, Ordering};

    let s = start("secret-token");

    // Auth is required on both cancel endpoints.
    let r = request(s.port, "POST", "/api/projects/demo/cancel", None, None);
    assert_eq!(r.status, 401, "{}", r.body);
    let r = request(s.port, "POST", "/api/runs/nope/cancel", None, None);
    assert_eq!(r.status, 401, "{}", r.body);

    // Unknown project -> 404; idle project -> 409.
    let r = request(
        s.port,
        "POST",
        "/api/projects/unknown/cancel",
        Some("secret-token"),
        None,
    );
    assert_eq!(r.status, 404, "{}", r.body);
    assert_eq!(json(&r.body)["error"], "project not found");

    let r = request(
        s.port,
        "POST",
        "/api/projects/demo/cancel",
        Some("secret-token"),
        None,
    );
    assert_eq!(r.status, 409, "{}", r.body);
    assert_eq!(json(&r.body)["error"], "project is not running");

    // Unknown run -> 404.
    let r = request(
        s.port,
        "POST",
        "/api/runs/does-not-exist/cancel",
        Some("secret-token"),
        None,
    );
    assert_eq!(r.status, 404, "{}", r.body);
    assert_eq!(json(&r.body)["error"], "run not found");

    // Finished run on disk -> 409.
    let mut finished = lunacy::model::Run::new(
        "demo",
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "feat: done".into(),
        lunacy::model::Trigger::Manual,
        Vec::new(),
    );
    finished.status = lunacy::model::RunStatus::Success;
    lunacy::model::persist_run(&s.state_dir, &finished).unwrap();

    let r = request(
        s.port,
        "POST",
        &format!("/api/runs/{}/cancel", finished.id),
        Some("secret-token"),
        None,
    );
    assert_eq!(r.status, 409, "{}", r.body);
    assert_eq!(json(&r.body)["error"], "run is not running");

    // Active run -> 202 and sets the cancel flag!
    let active = lunacy::model::Run::new(
        "demo",
        "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        "feat: running".into(),
        lunacy::model::Trigger::Manual,
        Vec::new(),
    );
    let cancel_flag = Arc::new(AtomicBool::new(false));
    {
        let mut guard = s.shared.lock().unwrap();
        guard.projects[0].running = true;
        guard.projects[0].current_run = Some(active.clone());
        guard.projects[0].cancel = Some(cancel_flag.clone());
    }

    let r = request(
        s.port,
        "POST",
        &format!("/api/runs/{}/cancel", active.id),
        Some("secret-token"),
        None,
    );
    assert_eq!(r.status, 202, "{}", r.body);
    let v = json(&r.body);
    assert_eq!(v["cancelling"], true);
    assert_eq!(v["run_id"], active.id);
    assert_eq!(v["project"], "demo");
    assert!(cancel_flag.load(Ordering::SeqCst));

    // Reset flag and cancel via project endpoint.
    cancel_flag.store(false, Ordering::SeqCst);
    let r = request(
        s.port,
        "POST",
        "/api/projects/demo/cancel",
        Some("secret-token"),
        None,
    );
    assert_eq!(r.status, 202, "{}", r.body);
    let v = json(&r.body);
    assert_eq!(v["cancelling"], true);
    assert_eq!(v["run_id"], active.id);
    assert_eq!(v["project"], "demo");
    assert!(cancel_flag.load(Ordering::SeqCst));
}

#[test]
fn deploy_checks_endpoint_reports_project_readiness() {
    let s = start("tok");

    // Auth required.
    let r = request(s.port, "GET", "/api/deploy/checks", None, None);
    assert_eq!(r.status, 401, "{}", r.body);

    // Initialize a real local git repo at s.root/repo with a remote and deploy script.
    let repo = s.root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(args)
            .output()
            .expect("git");
        assert!(out.status.success(), "git {args:?} failed");
    };
    git(&["init", "-b", "master"]);
    git(&["config", "user.email", "lunacy@example.invalid"]);
    git(&["config", "user.name", "lunacy"]);
    std::fs::write(repo.join("README.md"), "hi\n").unwrap();
    git(&["add", "README.md"]);
    git(&["commit", "-m", "init"]);
    git(&[
        "remote",
        "add",
        "origin",
        "https://github.com/teppyboy/demo.git",
    ]);

    {
        let mut guard = s.shared.lock().unwrap();
        guard.config.general.step_timeout = Some(1800);
        guard.config.projects[0].timeout = Some(3600);
        guard.config.projects[0].deploy = vec![shell_step("Publish", "bash scripts/deploy.sh")];
    }

    let r = request(s.port, "GET", "/api/deploy/checks", Some("tok"), None);
    assert_eq!(r.status, 200, "{}", r.body);
    let v = json(&r.body);
    assert_eq!(v["os"], lunacy::config::Os::current().as_str());
    assert_eq!(v["default_step_timeout"], 1800);
    assert_eq!(v["github_enabled"], false);
    assert_eq!(v["github_mode"], "status");
    assert_eq!(v["discord_global"], false);
    assert_eq!(v["cache_enabled"], true);
    assert!(v["cache_dir"].as_str().unwrap_or("").ends_with("cache"));

    let p = &v["projects"][0];
    assert_eq!(p["name"], "demo");
    assert_eq!(p["checkout"], "none");
    assert_eq!(p["watch_prs"], false);
    assert_eq!(p["deploy_refs"], serde_json::json!([]));
    assert_eq!(p["cache_enabled"], true);
    assert_eq!(p["pipeline_timeout"], 3600);
    assert_eq!(p["step_timeout"], 1800);
    assert_eq!(p["git"]["repo_exists"], true);
    assert_eq!(p["git"]["is_git_repo"], true);
    assert_eq!(p["git"]["remote_ok"], true);
    assert_eq!(p["git"]["dirty"], false);
    assert_eq!(p["overall"], "error"); // scripts/deploy.sh missing
    assert_eq!(p["deploy_steps"][0]["name"], "Publish");
    assert_eq!(p["deploy_steps"][0]["kind"], "shell");
    assert_eq!(p["deploy_steps"][0]["effective_timeout"], 60);

    // Add and commit the missing script -> overall becomes "ok".
    std::fs::create_dir_all(repo.join("scripts")).unwrap();
    std::fs::write(repo.join("scripts/deploy.sh"), "#!/bin/sh\ntrue\n").unwrap();
    git(&["add", "scripts/deploy.sh"]);
    git(&["commit", "-m", "add deploy script"]);
    let r = request(s.port, "GET", "/api/deploy/checks", Some("tok"), None);
    assert_eq!(r.status, 200, "{}", r.body);
    let v = json(&r.body);
    assert_eq!(v["projects"][0]["overall"], "ok", "{v}");
}

#[test]
fn force_deploy_project_and_run_endpoints() {
    let s = start("tok");
    let sha = "1111111111111111111111111111111111111111";

    // Auth required on both deploy endpoints.
    assert_eq!(
        request(s.port, "POST", "/api/projects/demo/deploy", None, None).status,
        401
    );
    assert_eq!(
        request(s.port, "POST", "/api/runs/nope/deploy", None, None).status,
        401
    );

    // Unknown project -> 404.
    assert_eq!(
        request(
            s.port,
            "POST",
            "/api/projects/nope/deploy",
            Some("tok"),
            None
        )
        .status,
        404
    );

    // Project with no deploy steps -> 400.
    let r = request(
        s.port,
        "POST",
        "/api/projects/demo/deploy",
        Some("tok"),
        None,
    );
    assert_eq!(r.status, 400, "{}", r.body);

    // Add a deploy step and attach a worker.
    {
        let mut guard = s.shared.lock().unwrap();
        guard.config.projects[0].deploy = vec![shell_step("Publish", "true")];
        guard.projects[0].upstream_sha = Some(sha.to_string());
    }
    let (tx, rx) = std::sync::mpsc::channel();
    s.commands.lock().unwrap().insert("demo".into(), tx);

    // Project currently running -> 409.
    s.shared.lock().unwrap().projects[0].running = true;
    assert_eq!(
        request(
            s.port,
            "POST",
            "/api/projects/demo/deploy",
            Some("tok"),
            None
        )
        .status,
        409
    );
    s.shared.lock().unwrap().projects[0].running = false;

    // Default skip_ci = true -> deploy_only = true, force_deploy = true.
    let r = request(
        s.port,
        "POST",
        "/api/projects/demo/deploy",
        Some("tok"),
        None,
    );
    assert_eq!(r.status, 202, "{}", r.body);
    let v = json(&r.body);
    assert_eq!(v["queued"], true);
    assert_eq!(v["sha"], sha);
    assert_eq!(v["deploy_only"], true);
    match rx.recv_timeout(Duration::from_secs(5)).expect("cmd") {
        Command::Trigger {
            sha: got_sha,
            force,
            trigger,
            deploy_only,
            force_deploy,
            ..
        } => {
            assert_eq!(got_sha.as_deref(), Some(sha));
            assert!(force);
            assert_eq!(trigger, lunacy::model::Trigger::ForceDeploy);
            assert!(deploy_only);
            assert!(force_deploy);
        }
        other => panic!("unexpected command: {other:?}"),
    }

    // Explicit sha + skip_ci = false -> deploy_only = false.
    let sha2 = "2222222222222222222222222222222222222222";
    let r = request(
        s.port,
        "POST",
        "/api/projects/demo/deploy",
        Some("tok"),
        Some(&format!(r#"{{"sha": "{sha2}", "skip_ci": false}}"#)),
    );
    assert_eq!(r.status, 202, "{}", r.body);
    assert_eq!(json(&r.body)["deploy_only"], false);
    match rx.recv_timeout(Duration::from_secs(5)).expect("cmd") {
        Command::Trigger {
            sha: got_sha,
            deploy_only,
            force_deploy,
            ..
        } => {
            assert_eq!(got_sha.as_deref(), Some(sha2));
            assert!(!deploy_only);
            assert!(force_deploy);
        }
        other => panic!("unexpected command: {other:?}"),
    }

    // POST /api/runs/{id}/deploy: unknown run -> 404.
    assert_eq!(
        request(
            s.port,
            "POST",
            "/api/runs/missing/deploy",
            Some("tok"),
            None
        )
        .status,
        404
    );

    // PR run rejected with 400 by default!
    let mut pr_run = lunacy::model::Run::new(
        "demo",
        sha2,
        "feat: pr".into(),
        lunacy::model::Trigger::PullRequest,
        Vec::new(),
    );
    pr_run.pr_number = Some(19);
    pr_run.pr_head_ref = Some("feat/pr-19".into());
    pr_run.git_ref = Some("feat/pr-19".into());
    lunacy::model::persist_run(&s.state_dir, &pr_run).unwrap();

    let r = request(
        s.port,
        "POST",
        &format!("/api/runs/{}/deploy", pr_run.id),
        Some("tok"),
        None,
    );
    assert_eq!(r.status, 400, "{}", r.body);
    assert_eq!(json(&r.body)["error"], "cannot deploy a pull request build");

    // Normal run deploys on exact sha and git_ref.
    let mut tag_run = lunacy::model::Run::new(
        "demo",
        sha,
        "release: v1.0.0".into(),
        lunacy::model::Trigger::Tag,
        Vec::new(),
    );
    tag_run.git_ref = Some("tags/v1.0.0".into());
    lunacy::model::persist_run(&s.state_dir, &tag_run).unwrap();

    let r = request(
        s.port,
        "POST",
        &format!("/api/runs/{}/deploy", tag_run.id),
        Some("tok"),
        None,
    );
    assert_eq!(r.status, 202, "{}", r.body);
    let v = json(&r.body);
    assert_eq!(v["queued"], true);
    assert_eq!(v["sha"], sha);
    assert_eq!(v["deploy_only"], true);
    assert_eq!(v["run_id"], tag_run.id);
    match rx.recv_timeout(Duration::from_secs(5)).expect("cmd") {
        Command::Trigger {
            sha: got_sha,
            trigger,
            git_ref,
            deploy_only,
            force_deploy,
            ..
        } => {
            assert_eq!(got_sha.as_deref(), Some(sha));
            assert_eq!(trigger, lunacy::model::Trigger::ForceDeploy);
            assert_eq!(git_ref.as_deref(), Some("tags/v1.0.0"));
            assert!(deploy_only);
            assert!(force_deploy);
        }
        other => panic!("unexpected command: {other:?}"),
    }

    // Extended POST /api/projects/{name}/trigger with pr_number & deploy_only.
    let r = request(
        s.port,
        "POST",
        "/api/projects/demo/trigger",
        Some("tok"),
        Some(r#"{"pr_number": 7, "ref": "feat/seven"}"#),
    );
    assert_eq!(r.status, 202, "{}", r.body);
    match rx.recv_timeout(Duration::from_secs(5)).expect("cmd") {
        Command::Trigger {
            trigger,
            pr,
            git_ref,
            ..
        } => {
            assert_eq!(trigger, lunacy::model::Trigger::PullRequest);
            assert_eq!(pr.as_ref().map(|p| p.number), Some(7));
            assert_eq!(git_ref.as_deref(), Some("feat/seven"));
        }
        other => panic!("unexpected command: {other:?}"),
    }
}

#[test]
fn cache_clear_endpoints_remove_disk_caches() {
    let s = start("tok");
    let cache_root = s.state_dir.join("cache");
    let act_actions = cache_root.join("act-actions");
    let act_cache_server = cache_root.join("act-cache-server");
    let demo_cache = cache_root.join("projects").join("demo");
    let other_cache = cache_root.join("projects").join("other");
    let unrelated_file = cache_root.join("keep.txt");
    let unrelated_dir = cache_root.join("unrelated-dir");
    std::fs::create_dir_all(&act_actions).unwrap();
    std::fs::create_dir_all(&act_cache_server).unwrap();
    std::fs::create_dir_all(&demo_cache).unwrap();
    std::fs::create_dir_all(&other_cache).unwrap();
    std::fs::create_dir_all(&unrelated_dir).unwrap();
    std::fs::write(act_actions.join("a"), "1").unwrap();
    std::fs::write(act_cache_server.join("s"), "4").unwrap();
    std::fs::write(demo_cache.join("b"), "2").unwrap();
    std::fs::write(other_cache.join("c"), "3").unwrap();
    std::fs::write(&unrelated_file, "do-not-delete").unwrap();
    std::fs::write(unrelated_dir.join("note.txt"), "keep-me").unwrap();

    // Auth required.
    assert_eq!(
        request(s.port, "POST", "/api/cache/clear", None, None).status,
        401
    );
    assert_eq!(
        request(s.port, "POST", "/api/projects/demo/cache/clear", None, None).status,
        401
    );

    // Unknown project -> 404.
    assert_eq!(
        request(
            s.port,
            "POST",
            "/api/projects/nope/cache/clear",
            Some("tok"),
            None
        )
        .status,
        404
    );

    // Clear single project cache -> removes projects/demo, leaves projects/other and act-actions.
    let r = request(
        s.port,
        "POST",
        "/api/projects/demo/cache/clear",
        Some("tok"),
        None,
    );
    assert_eq!(r.status, 200, "{}", r.body);
    let v = json(&r.body);
    assert_eq!(v["cleared"], true);
    assert_eq!(v["project"], "demo");
    assert!(!demo_cache.exists());
    assert!(other_cache.is_dir());
    assert!(act_actions.is_dir());

    // Clear all caches -> removes managed subdirs only, leaves unrelated sibling files/dirs untouched.
    let r = request(s.port, "POST", "/api/cache/clear", Some("tok"), None);
    assert_eq!(r.status, 200, "{}", r.body);
    let v = json(&r.body);
    assert_eq!(v["cleared"], true);
    assert!(!other_cache.exists());
    assert!(!act_actions.exists());
    assert!(!act_cache_server.exists());
    assert!(unrelated_file.is_file(), "unrelated file must remain");
    assert!(
        unrelated_dir.join("note.txt").is_file(),
        "unrelated sibling directory must remain"
    );
}

#[test]
fn webhooks_pull_request_and_push_dispatch() {
    let s = start("");
    let secret = "hooksecret";
    {
        let mut guard = s.shared.lock().unwrap();
        guard.config.github = Some(lunacy::config::GithubCfg {
            mode: None,
            token: Some("x".into()),
            token_file: None,
            api_url: None,
            app: Some(lunacy::config::AppCfg {
                app_id: 1,
                private_key_path: None,
                private_key: Some("k".into()),
                installation_id: Some(1),
                webhook_secret: Some(secret.into()),
                webhook_secret_file: None,
            }),
        });
        guard.config.projects[0].watch_prs = false;
        guard.config.projects[0].deploy_refs = vec!["master".into(), "tags/v*".into()];
    }
    let (tx, rx) = std::sync::mpsc::channel();
    s.commands.lock().unwrap().insert("demo".into(), tx);

    let send_hook = |event: &str, body: &str| {
        let sig = api::signature_header(secret, body.as_bytes());
        request_with_headers(
            s.port,
            "POST",
            "/api/github/webhook",
            None,
            &[("X-Hub-Signature-256", &sig), ("X-GitHub-Event", event)],
            Some(body),
        )
    };

    let pr_body = r#"{
        "action": "opened",
        "repository": {"full_name": "teppyboy/demo"},
        "pull_request": {
            "number": 15,
            "title": "feat: pr webhook",
            "head": {
                "sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "ref": "feat/pr15",
                "repo": {"full_name": "TeppyBoy/Demo", "fork": false}
            },
            "base": {
                "sha": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                "ref": "master",
                "repo": {"full_name": "teppyboy/demo", "fork": false}
            }
        }
    }"#;

    // watch_prs = false -> ignored.
    let r = send_hook("pull_request", pr_body);
    assert_eq!(r.status, 202, "{}", r.body);
    assert_eq!(json(&r.body)["queued"], false);

    // Enable watch_prs -> queued as PullRequest!
    s.shared.lock().unwrap().config.projects[0].watch_prs = true;
    let r = send_hook("pull_request", pr_body);
    assert_eq!(r.status, 202, "{}", r.body);
    assert_eq!(json(&r.body)["queued"], true);
    assert_eq!(json(&r.body)["pr_number"], 15);
    match rx.recv_timeout(Duration::from_secs(5)).expect("pr cmd") {
        Command::Trigger {
            sha,
            trigger,
            git_ref,
            pr,
            ..
        } => {
            assert_eq!(
                sha.as_deref(),
                Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
            );
            assert_eq!(trigger, lunacy::model::Trigger::PullRequest);
            assert_eq!(git_ref.as_deref(), Some("feat/pr15"));
            let pr = pr.expect("pr ctx");
            assert_eq!(pr.number, 15);
            assert_eq!(pr.title, "feat: pr webhook");
            assert_eq!(pr.base_ref, "master");
        }
        other => panic!("unexpected command: {other:?}"),
    }

    // pull_request closed -> ignored.
    let closed_body = pr_body.replace("\"opened\"", "\"closed\"");
    let r = send_hook("pull_request", &closed_body);
    assert_eq!(r.status, 202);
    assert_eq!(json(&r.body)["queued"], false);

    // push to refs/heads/master -> queued as Webhook!
    let push_master = r#"{
        "ref": "refs/heads/master",
        "after": "cccccccccccccccccccccccccccccccccccccccc",
        "repository": {"full_name": "teppyboy/demo"}
    }"#;
    let r = send_hook("push", push_master);
    assert_eq!(r.status, 202, "{}", r.body);
    assert_eq!(json(&r.body)["queued"], true);
    match rx
        .recv_timeout(Duration::from_secs(5))
        .expect("push master cmd")
    {
        Command::Trigger {
            sha,
            trigger,
            git_ref,
            pr,
            ..
        } => {
            assert_eq!(
                sha.as_deref(),
                Some("cccccccccccccccccccccccccccccccccccccccc")
            );
            assert_eq!(trigger, lunacy::model::Trigger::Webhook);
            assert_eq!(git_ref.as_deref(), Some("master"));
            assert!(pr.is_none());
        }
        other => panic!("unexpected command: {other:?}"),
    }

    // push to refs/tags/v1.2.3 (matches tags/v*) -> queued as Tag!
    let push_tag = r#"{
        "ref": "refs/tags/v1.2.3",
        "after": "dddddddddddddddddddddddddddddddddddddddd",
        "repository": {"full_name": "teppyboy/demo"}
    }"#;
    let r = send_hook("push", push_tag);
    assert_eq!(r.status, 202, "{}", r.body);
    assert_eq!(json(&r.body)["queued"], true);
    match rx
        .recv_timeout(Duration::from_secs(5))
        .expect("push tag cmd")
    {
        Command::Trigger {
            sha,
            trigger,
            git_ref,
            ..
        } => {
            assert_eq!(
                sha.as_deref(),
                Some("dddddddddddddddddddddddddddddddddddddddd")
            );
            assert_eq!(trigger, lunacy::model::Trigger::Tag);
            assert_eq!(git_ref.as_deref(), Some("tags/v1.2.3"));
        }
        other => panic!("unexpected command: {other:?}"),
    }

    // push to unmatched branch -> ignored.
    let push_other = r#"{
        "ref": "refs/heads/experimental",
        "after": "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
        "repository": {"full_name": "teppyboy/demo"}
    }"#;
    let r = send_hook("push", push_other);
    assert_eq!(r.status, 202);
    assert_eq!(json(&r.body)["queued"], false);

    // push deleted -> ignored.
    let push_deleted = r#"{
        "ref": "refs/heads/master",
        "after": "0000000000000000000000000000000000000000",
        "deleted": true,
        "repository": {"full_name": "teppyboy/demo"}
    }"#;
    let r = send_hook("push", push_deleted);
    assert_eq!(r.status, 202);
    assert_eq!(json(&r.body)["queued"], false);

    // Fork PRs and deleted-repo PRs MUST be ignored with 200 {"ignored": true, "reason": "fork pull request"}.
    let fork_cases = [
        // fork: true
        r#"{
            "action": "opened",
            "repository": {"full_name": "teppyboy/demo"},
            "pull_request": {
                "number": 16,
                "title": "evil fork",
                "head": {
                    "sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "ref": "evil",
                    "repo": {"full_name": "attacker/demo", "fork": true}
                },
                "base": {
                    "sha": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                    "ref": "master",
                    "repo": {"full_name": "teppyboy/demo", "fork": false}
                }
            }
        }"#,
        // mismatched full_name even if fork=false
        r#"{
            "action": "synchronize",
            "repository": {"full_name": "teppyboy/demo"},
            "pull_request": {
                "number": 17,
                "title": "cross repo",
                "head": {
                    "sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "ref": "feat/x",
                    "repo": {"full_name": "other/demo", "fork": false}
                },
                "base": {
                    "sha": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                    "ref": "master",
                    "repo": {"full_name": "teppyboy/demo", "fork": false}
                }
            }
        }"#,
        // deleted head repo (head.repo == null)
        r#"{
            "action": "opened",
            "repository": {"full_name": "teppyboy/demo"},
            "pull_request": {
                "number": 18,
                "title": "deleted repo",
                "head": {
                    "sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "ref": "feat/deleted",
                    "repo": null
                },
                "base": {
                    "sha": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                    "ref": "master",
                    "repo": {"full_name": "teppyboy/demo", "fork": false}
                }
            }
        }"#,
    ];
    for payload in fork_cases {
        let r = send_hook("pull_request", payload);
        assert_eq!(r.status, 200, "{}", r.body);
        let v = json(&r.body);
        assert_eq!(v["ignored"], true, "{v}");
        assert_eq!(v["reason"], "fork pull request", "{v}");
    }

    // Malicious / invalid sha or ref in webhooks -> 400 and never queued.
    let bad_pr_sha = pr_body.replace(
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "--upload-pack=id",
    );
    assert_eq!(send_hook("pull_request", &bad_pr_sha).status, 400);

    let bad_pr_ref = pr_body.replace("feat/pr15", "--upload-pack=id");
    assert_eq!(send_hook("pull_request", &bad_pr_ref).status, 400);

    let bad_push_ref = r#"{
        "ref": "-bad-ref",
        "after": "cccccccccccccccccccccccccccccccccccccccc",
        "repository": {"full_name": "teppyboy/demo"}
    }"#;
    assert_eq!(send_hook("push", bad_push_ref).status, 400);

    let bad_push_sha = r#"{
        "ref": "refs/heads/master",
        "after": "not-a-hex-sha",
        "repository": {"full_name": "teppyboy/demo"}
    }"#;
    assert_eq!(send_hook("push", bad_push_sha).status, 400);

    let bad_check_run = r#"{
        "action": "rerequested",
        "repository": {"full_name": "teppyboy/demo"},
        "check_run": {"head_sha": "--exec=bad"}
    }"#;
    assert_eq!(send_hook("check_run", bad_check_run).status, 400);

    assert!(
        rx.try_recv().is_err(),
        "no fork or invalid webhook should ever queue a command"
    );
}

#[test]
fn percent_encoded_segments_and_input_validation() {
    let s = start("tok");
    // Add a project whose name contains a space (`my app`), matching `ui/src/net.rs::seg`.
    let mut spaced = project(&s.root);
    spaced.name = "my app".into();
    spaced.deploy = vec![shell_step("Deploy", "true")];
    {
        let mut guard = s.shared.lock().unwrap();
        guard.config.projects.push(spaced.clone());
        guard
            .projects
            .push(ProjectState::new_public(&spaced, &s.state_dir));
        guard.projects[1].upstream_sha = Some("1111111111111111111111111111111111111111".into());
    }
    let (tx, rx) = std::sync::mpsc::channel();
    s.commands.lock().unwrap().insert("my app".into(), tx);

    // Percent-encoded project lookup, runs list, and trigger.
    let r = request(s.port, "GET", "/api/projects/my%20app", Some("tok"), None);
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(json(&r.body)["name"], "my app");

    let run = lunacy::model::Run::new(
        "my app",
        "1111111111111111111111111111111111111111",
        "feat: spaced".into(),
        lunacy::model::Trigger::Manual,
        Vec::new(),
    );
    lunacy::model::persist_run(&s.state_dir, &run).unwrap();

    let r = request(
        s.port,
        "GET",
        "/api/projects/my%20app/runs",
        Some("tok"),
        None,
    );
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(json(&r.body).as_array().unwrap().len(), 1);

    let encoded_run_id = run.id.replace(' ', "%20");
    let r = request(
        s.port,
        "GET",
        &format!("/api/runs/{encoded_run_id}"),
        Some("tok"),
        None,
    );
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(json(&r.body)["id"], run.id);

    // Invalid trigger sha / ref -> 400.
    let r = request(
        s.port,
        "POST",
        "/api/projects/my%20app/trigger",
        Some("tok"),
        Some(r#"{"sha": "--upload-pack=id"}"#),
    );
    assert_eq!(r.status, 400, "{}", r.body);

    let r = request(
        s.port,
        "POST",
        "/api/projects/my%20app/trigger",
        Some("tok"),
        Some(r#"{"ref": "-bad-branch"}"#),
    );
    assert_eq!(r.status, 400, "{}", r.body);

    let r = request(
        s.port,
        "POST",
        "/api/projects/my%20app/deploy",
        Some("tok"),
        Some(r#"{"sha": "not-hex"}"#),
    );
    assert_eq!(r.status, 400, "{}", r.body);

    // Valid percent-encoded trigger succeeds.
    let r = request(
        s.port,
        "POST",
        "/api/projects/my%20app/trigger",
        Some("tok"),
        Some(r#"{"sha": "1111111111111111111111111111111111111111", "ref": "master"}"#),
    );
    assert_eq!(r.status, 202, "{}", r.body);
    assert!(rx.recv_timeout(Duration::from_secs(5)).is_ok());

    // Percent-encoded traversal segment -> 400.
    let r = request(s.port, "GET", "/api/projects/..%2Fetc", Some("tok"), None);
    assert_eq!(r.status, 400, "{}", r.body);
}
