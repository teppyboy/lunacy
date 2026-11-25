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

fn request(
    port: u16,
    method: &str,
    path: &str,
    token: Option<&str>,
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
        env: Default::default(),
        contexts: Default::default(),
        discord_webhook_url: None,
        notify_on_start: None,
        shell: None,
        platforms: Default::default(),
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
            api_url: None,
            app: None,
        });
        guard.config.discord = Some(lunacy::config::DiscordCfg {
            webhook_url: Some("https://discord.com/api/webhooks/a/b".into()),
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
            api_url: None,
            app: Some(lunacy::config::AppCfg {
                app_id: 1,
                private_key_path: None,
                private_key: Some("k".into()),
                installation_id: Some(1),
                webhook_secret: Some("hooksecret".into()),
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
