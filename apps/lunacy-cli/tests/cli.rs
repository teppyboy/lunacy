//! `lunacy-cli` against a real `lunacy` daemon process: exit codes, JSON shapes, waiting.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output};
use std::time::{Duration, Instant};

const TOKEN: &str = "cli-test-token";

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The daemon binary cargo built for the `lunacy` dev-dependency, next to ours.
fn daemon_bin() -> PathBuf {
    let cli = PathBuf::from(env!("CARGO_BIN_EXE_lunacy-cli"));
    let bin = cli.with_file_name(format!("lunacy{}", std::env::consts::EXE_SUFFIX));
    if !bin.is_file() {
        let status = Command::new(env!("CARGO"))
            .args(["build", "-q", "-p", "lunacy", "--bin", "lunacy"])
            .status()
            .unwrap();
        assert!(status.success(), "building the daemon failed");
    }
    bin
}

struct Daemon {
    child: Child,
    port: u16,
    _dir: tempfile::TempDir,
    // A free port is only free until someone binds it: tests that start a daemon take turns,
    // so two never race for the same one.
    _turn: std::sync::MutexGuard<'static, ()>,
}

static DAEMON_TURN: std::sync::Mutex<()> = std::sync::Mutex::new(());

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Two projects: `ok` (CI `true`) and `bad` (CI `false`), plus `slow` (sleeps).
fn start() -> Daemon {
    let turn = DAEMON_TURN.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    git(
        root,
        &["init", "-q", "--bare", "-b", "master", "origin.git"],
    );
    git(root, &["clone", "-q", "origin.git", "work"]);
    let work = root.join("work");
    git(
        &work,
        &[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "feat: one",
        ],
    );
    git(&work, &["push", "-q", "origin", "HEAD:master"]);

    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let project = |name: &str, run: &str| {
        format!(
            "[[project]]\nname = \"{name}\"\nrepo_path = \"{}\"\nbranch = \"master\"\ncheckout = \"none\"\n\
             [[project.ci]]\nname = \"CI\"\nrun = \"{run}\"\ntimeout = 60\n",
            work.display()
        )
    };
    let config = format!(
        "[general]\nstate_dir = \"{}\"\npoll_interval = 3600\ntrigger_mode = \"webhook\"\n\n\
         [server]\nlisten = \"127.0.0.1:{port}\"\ntoken = \"{TOKEN}\"\n\n{}{}{}",
        root.join("state").display(),
        project("ok", "echo building; echo done"),
        project("bad", "echo boom; exit 3"),
        project("slow", "sleep 30"),
    );
    let config_path = root.join("config.toml");
    std::fs::write(&config_path, config).unwrap();

    let child = Command::new(daemon_bin())
        .args(["run", "-c"])
        .arg(&config_path)
        .env("RUST_LOG", "warn")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let mut d = Daemon {
        child,
        port,
        _dir: dir,
        _turn: turn,
    };
    // Up means *this* daemon answers with our token, not just that something holds the port.
    let deadline = Instant::now() + Duration::from_secs(20);
    while cli(port, Some(TOKEN), &["projects"]).code != 0 {
        if let Ok(Some(exit)) = d.child.try_wait() {
            panic!("daemon exited early: {exit}");
        }
        assert!(Instant::now() < deadline, "daemon did not start");
        std::thread::sleep(Duration::from_millis(100));
    }
    d
}

struct Out {
    code: i32,
    stdout: String,
    stderr: String,
}

impl Out {
    fn json(&self) -> Value {
        serde_json::from_str(&self.stdout).unwrap_or_else(|e| {
            panic!(
                "stdout is not JSON ({e}): {}\nstderr: {}",
                self.stdout, self.stderr
            )
        })
    }
    fn err_json(&self) -> Value {
        let line = self.stderr.lines().last().unwrap_or("");
        serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("stderr is not JSON ({e}): {}", self.stderr))
    }
}

fn cli(port: u16, token: Option<&str>, args: &[&str]) -> Out {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_lunacy-cli"));
    cmd.arg("--server")
        .arg(format!("http://127.0.0.1:{port}"))
        .arg("--json")
        .args(args)
        .env_remove("LUNACY_TOKEN")
        .env_remove("LUNACY_SERVER")
        // No real ~/.config/lunacy/api-token may leak into these tests.
        .env("HOME", std::env::temp_dir());
    match token {
        Some(t) => cmd.env("LUNACY_TOKEN", t),
        None => cmd.env("LUNACY_TOKEN_FILE", "/nonexistent"),
    };
    let Output {
        status,
        stdout,
        stderr,
    } = cmd.output().unwrap();
    Out {
        code: status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    }
}

#[test]
fn reads_and_exit_codes() {
    let d = start();
    let ok = |args: &[&str]| {
        let o = cli(d.port, Some(TOKEN), args);
        assert_eq!(o.code, 0, "{args:?}: {}{}", o.stdout, o.stderr);
        o
    };

    let status = ok(&["status"]).json();
    assert_eq!(status["projects"].as_array().unwrap().len(), 3);
    assert!(status["health"]["version"].is_string());

    let projects = ok(&["projects"]).json();
    assert_eq!(projects[0]["name"], "ok");
    assert_eq!(ok(&["project", "bad"]).json()["name"], "bad");
    assert_eq!(ok(&["runs", "ok"]).json(), Value::Array(vec![]));
    assert_eq!(
        ok(&["config", "get", "--path", "project.1.name"]).json(),
        "bad"
    );
    assert_eq!(ok(&["config", "get"]).json()["server"]["token"], "********");

    // 401 -> 4, unknown -> 3, unreachable -> 6, usage -> 2.
    let o = cli(d.port, Some("wrong"), &["projects"]);
    assert_eq!(o.code, 4, "{}", o.stderr);
    assert_eq!(o.err_json()["error"]["code"], "auth");
    assert_eq!(o.err_json()["error"]["http_status"], 401);
    assert_eq!(cli(d.port, None, &["projects"]).code, 4);
    let o = cli(d.port, Some(TOKEN), &["project", "nope"]);
    assert_eq!(o.code, 3, "{}", o.stderr);
    assert_eq!(o.err_json()["error"]["code"], "not_found");
    let free = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let o = cli(free, Some(TOKEN), &["status"]);
    assert_eq!(o.code, 6, "{}", o.stderr);
    assert_eq!(o.err_json()["error"]["code"], "unreachable");
    assert_eq!(
        cli(d.port, Some(TOKEN), &["trigger", "ok", "--timeout", "5"]).code,
        2
    );
    assert_eq!(cli(d.port, Some(TOKEN), &["no-such-command"]).code, 2);

    // doctor: auth failure is reported and exits 4.
    let o = cli(d.port, Some("wrong"), &["doctor"]);
    assert_eq!(o.code, 4);
    assert_eq!(o.json()["ok"], false);

    // schema is JSON and lists the exit codes.
    let schema = ok(&["schema"]).json();
    assert_eq!(schema["exit_codes"].as_array().unwrap().len(), 9);
    assert!(schema["commands"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["name"] == "trigger"));
}

#[test]
fn trigger_wait_logs_and_conflict() {
    let d = start();

    // Success: exit 0, the finished run on stdout, progress events on stderr.
    let o = cli(
        d.port,
        Some(TOKEN),
        &["trigger", "ok", "--force", "--wait", "--timeout", "60"],
    );
    assert_eq!(o.code, 0, "{}{}", o.stdout, o.stderr);
    let run = o.json();
    assert_eq!(run["status"], "success");
    assert!(
        o.stderr
            .lines()
            .any(|l| l.contains("\"event\":\"started\"")),
        "{}",
        o.stderr
    );
    let id = run["id"].as_str().unwrap().to_string();

    let o = cli(d.port, Some(TOKEN), &["logs", &id]);
    assert_eq!(o.code, 0);
    assert!(o.json()["log"].as_str().unwrap().contains("done"));
    assert_eq!(
        cli(d.port, Some(TOKEN), &["run", &id]).json()["id"],
        id.as_str()
    );
    assert_eq!(cli(d.port, Some(TOKEN), &["wait", &id]).code, 0);

    // Failure: exit 7, failed step named; `logs` defaults to the failed step.
    let o = cli(
        d.port,
        Some(TOKEN),
        &["trigger", "bad", "--force", "--wait", "--timeout", "60"],
    );
    assert_eq!(o.code, 7, "{}{}", o.stdout, o.stderr);
    let run = o.json();
    assert_eq!(run["status"], "failure");
    assert_eq!(run["failed_step"], "CI");
    let log = cli(
        d.port,
        Some(TOKEN),
        &["logs", run["id"].as_str().unwrap(), "--tail", "5"],
    );
    assert!(
        log.json()["log"].as_str().unwrap().contains("boom"),
        "{}",
        log.stdout
    );

    // A running project: a second trigger conflicts (409 -> 5); wait --timeout -> 8; cancel.
    let o = cli(d.port, Some(TOKEN), &["trigger", "slow", "--force"]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    let deadline = Instant::now() + Duration::from_secs(20);
    let slow_id = loop {
        let p = cli(d.port, Some(TOKEN), &["project", "slow"]).json();
        if let Some(id) = p["active_runs"][0]["id"].as_str() {
            break id.to_string();
        }
        assert!(Instant::now() < deadline, "slow run never started: {p}");
        std::thread::sleep(Duration::from_millis(200));
    };
    let o = cli(d.port, Some(TOKEN), &["trigger", "slow", "--force"]);
    assert_eq!(o.code, 5, "{}{}", o.stdout, o.stderr);
    assert_eq!(o.err_json()["error"]["code"], "conflict");
    let o = cli(d.port, Some(TOKEN), &["wait", &slow_id, "--timeout", "1"]);
    assert_eq!(o.code, 8, "{}", o.stderr);
    let o = cli(d.port, Some(TOKEN), &["cancel", &slow_id]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    let o = cli(d.port, Some(TOKEN), &["wait", &slow_id, "--timeout", "30"]);
    assert_eq!(o.code, 7, "{}", o.stderr);
    assert_eq!(o.json()["status"], "cancelled");
}

#[test]
fn config_check_is_local() {
    let example = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config.example.toml");
    // No daemon: port 9 is never a lunacy daemon.
    let o = cli(9, None, &["config", "check", example.to_str().unwrap()]);
    assert_eq!(o.code, 0, "{}{}", o.stdout, o.stderr);
    assert_eq!(o.json()["ok"], true);

    let dir = tempfile::tempdir().unwrap();
    let bad = dir.path().join("bad.toml");
    std::fs::write(&bad, "[general]\npoll_interval = 0\n").unwrap();
    let o = cli(9, None, &["config", "check", bad.to_str().unwrap()]);
    assert_eq!(o.code, 1, "{}{}", o.stdout, o.stderr);
    assert_eq!(o.json()["ok"], false);
}
