//! Discord webhook messages (embeds), best-effort.

use crate::config::{DiscordNotifyOn, ResolvedDiscord};
use crate::model::{Run, RunStatus, StepStatus};
use anyhow::Result;
use serde_json::json;
use std::time::Duration;

const HTTP_TIMEOUT: Duration = Duration::from_secs(10);
/// Discord embed and content limits.
#[cfg(test)]
const MAX_DESCRIPTION: usize = 2000;
const MAX_CONTENT: usize = 2000;
const MAX_FIELD: usize = 1000;
const LOG_TAIL_LINES: usize = 30;

pub struct Discord {
    url: String,
    notify_on: DiscordNotifyOn,
    username: Option<String>,
    mention_on_failure: Option<String>,
    agent: ureq::Agent,
}

impl Discord {
    pub fn new(url: &str) -> Result<Discord> {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(HTTP_TIMEOUT))
            .http_status_as_error(false)
            .max_redirects(0)
            .build()
            .into();
        Ok(Discord {
            url: url.to_string(),
            notify_on: DiscordNotifyOn::All,
            username: None,
            mention_on_failure: None,
            agent,
        })
    }

    pub fn from_resolved(resolved: &ResolvedDiscord) -> Result<Discord> {
        let mut discord = Self::new(&resolved.url)?;
        discord.notify_on = resolved.notify_on;
        discord.username = resolved.username.clone();
        discord.mention_on_failure = resolved.mention_on_failure.clone();
        Ok(discord)
    }

    pub fn should_notify_finish(&self, status: RunStatus) -> bool {
        match self.notify_on {
            DiscordNotifyOn::All => true,
            DiscordNotifyOn::Failures => matches!(status, RunStatus::Failure | RunStatus::Error),
        }
    }

    pub fn notify_start(&self, run: &Run, repo: Option<&str>) -> Result<()> {
        self.post(start_payload(run, repo, self.username.as_deref()))
    }

    pub fn notify_finish(
        &self,
        run: &Run,
        repo: Option<&str>,
        state_dir: &std::path::Path,
    ) -> Result<()> {
        if !self.should_notify_finish(run.status) {
            return Ok(());
        }
        self.post(finish_payload_with_options(
            run,
            repo,
            state_dir,
            self.username.as_deref(),
            self.mention_on_failure.as_deref(),
        ))
    }

    pub fn send_test(&self, project: Option<&str>) -> Result<()> {
        self.post(test_payload(project, self.username.as_deref()))
    }

    fn redact(&self, text: &str) -> String {
        if self.url.is_empty() {
            text.to_string()
        } else {
            text.replace(&self.url, crate::config::MASK)
        }
    }

    fn post(&self, payload: serde_json::Value) -> Result<()> {
        let body = payload.to_string();
        if body.len() > 200_000 {
            anyhow::bail!("discord payload too large");
        }
        let mut response = match self
            .agent
            .post(&self.url)
            .header("Content-Type", "application/json")
            .header("User-Agent", "lunacy")
            .send(body)
        {
            Ok(resp) => resp,
            Err(
                ureq::Error::BadUri(_) | ureq::Error::Http(_) | ureq::Error::RequireHttpsOnly(_),
            ) => {
                anyhow::bail!("invalid discord webhook URL");
            }
            Err(err) => {
                anyhow::bail!("{}", self.redact(&err.to_string()));
            }
        };
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            let text = response.body_mut().read_to_string().unwrap_or_default();
            let redacted = self.redact(&text);
            anyhow::bail!(
                "discord webhook returned {status}: {}",
                redacted.chars().take(200).collect::<String>()
            );
        }
        Ok(())
    }
}

fn attach_username(payload: &mut serde_json::Value, username: Option<&str>) {
    if let Some(u) = username.map(str::trim).filter(|s| !s.is_empty()) {
        payload["username"] = json!(u);
    }
}

pub(crate) fn start_payload(
    run: &Run,
    repo: Option<&str>,
    username: Option<&str>,
) -> serde_json::Value {
    let mut payload = json!({
        "embeds": [{
            "title": format!("{} started", run.project),
            "color": 0x3498DB,
            "description": format!(
                "{} `{}` — {}",
                repo.map(|r| format!("{r} ")).unwrap_or_default(),
                &run.sha[..run.sha.len().min(7)],
                truncate(&run.subject, 200)
            ),
        }],
        "allowed_mentions": { "parse": [] },
    });
    attach_username(&mut payload, username);
    payload
}

pub(crate) fn test_payload(project: Option<&str>, username: Option<&str>) -> serde_json::Value {
    let description = match project {
        Some(name) => format!("Test webhook notification for project `{name}`."),
        None => "Test webhook notification for global Discord configuration.".to_string(),
    };
    let mut payload = json!({
        "embeds": [{
            "title": "[lunacy] test notification",
            "color": 0x3498DB,
            "description": description,
        }],
        "allowed_mentions": { "parse": [] },
    });
    attach_username(&mut payload, username);
    payload
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn finish_payload(
    run: &Run,
    repo: Option<&str>,
    state_dir: &std::path::Path,
) -> serde_json::Value {
    finish_payload_with_options(run, repo, state_dir, None, None)
}

pub(crate) fn finish_payload_with_options(
    run: &Run,
    repo: Option<&str>,
    state_dir: &std::path::Path,
    username: Option<&str>,
    mention_on_failure: Option<&str>,
) -> serde_json::Value {
    let (status_text, color) = match run.status {
        RunStatus::Success => ("success", 0x2ECC71),
        RunStatus::Failure => ("failure", 0xE74C3C),
        RunStatus::Error => ("error", 0xE67E22),
        RunStatus::Cancelled => ("cancelled", 0x95A5A6),
        RunStatus::Running => ("running", 0x3498DB),
    };
    let link = match repo {
        Some(repo) => format!("[{repo}](https://github.com/{repo}/commit/{})", run.sha),
        None => format!("`{}`", &run.sha[..run.sha.len().min(7)]),
    };
    let mut fields = vec![
        json!({ "name": "Commit", "value": format!("{} `{}`", link, &run.sha[..run.sha.len().min(7)]), "inline": true }),
        json!({ "name": "Result", "value": status_text, "inline": true }),
        json!({ "name": "Duration", "value": format!("{}s", run.duration_secs()), "inline": true }),
    ];
    let highlight = run
        .failed()
        .or_else(|| run.steps.iter().find(|s| s.status == StepStatus::Cancelled));
    if let Some(step) = highlight {
        let mut where_ = step.name.clone();
        if let Some(sub) = step.substeps.iter().find(|s| {
            matches!(
                s.status,
                StepStatus::Failure | StepStatus::Timeout | StepStatus::Cancelled
            )
        }) {
            where_ = format!("{} / {} ({})", step.name, sub.name, sub.job);
        }
        fields.push(json!({
            "name": step_status_label(step.status),
            "value": truncate(&where_, MAX_FIELD),
            "inline": false,
        }));
    }
    let log = failing_tail(state_dir, run, LOG_TAIL_LINES);
    if !log.is_empty() {
        fields.push(json!({
            "name": format!("Log tail ({LOG_TAIL_LINES} lines)"),
            "value": truncate(&log, MAX_FIELD),
            "inline": false,
        }));
    }
    fields.push(json!({
        "name": "Steps",
        "value": truncate(
            &run.steps
                .iter()
                .map(|s| format!("{} {}", s.status.emoji(), s.name))
                .collect::<Vec<_>>()
                .join("\n"),
            MAX_FIELD
        ),
        "inline": false,
    }));

    let mut payload = json!({
        "embeds": [{
            "title": truncate(&format!("{} {} — {}", run.project, status_text, truncate(&run.subject, 120)), 256),
            "color": color,
            "fields": fields,
            "footer": { "text": format!("{} · {}", run.project, run.id) },
        }],
        "allowed_mentions": { "parse": [] },
    });
    attach_username(&mut payload, username);
    if matches!(run.status, RunStatus::Failure | RunStatus::Error) {
        if let Some(mention) = mention_on_failure.map(str::trim).filter(|s| !s.is_empty()) {
            payload["content"] = json!(truncate(mention, MAX_CONTENT));
            payload["allowed_mentions"] = json!({ "parse": ["users", "roles"] });
        }
    }
    payload
}

fn step_status_label(status: StepStatus) -> &'static str {
    match status {
        StepStatus::Timeout => "Timed out in",
        StepStatus::Cancelled => "Cancelled at",
        _ => "Failed at",
    }
}

fn failing_tail(state_dir: &std::path::Path, run: &Run, lines: usize) -> String {
    let Some(step) = run.failed() else {
        return String::new();
    };
    let Some(index) = run.steps.iter().position(|s| std::ptr::eq(s, step)) else {
        return String::new();
    };
    let path = crate::model::run_log_path(state_dir, &run.project, &run.id, index);
    let Ok(text) = crate::model::tail_lines(&path, lines) else {
        return String::new();
    };
    if text.trim().is_empty() {
        return String::new();
    }
    format!("```\n{}\n```", text.replace("```", "'''"))
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Phase, Run, Step, Trigger};

    fn failed_run() -> Run {
        let steps = vec![
            Step {
                phase: Phase::Ci,
                name: "Format".into(),
                status: StepStatus::Success,
                duration_ms: Some(10),
                exit_code: Some(0),
                kind: crate::config::StepKind::Shell,
                substeps: Vec::new(),
            },
            Step {
                phase: Phase::Ci,
                name: "Test".into(),
                status: StepStatus::Failure,
                duration_ms: Some(20),
                exit_code: Some(1),
                kind: crate::config::StepKind::Shell,
                substeps: Vec::new(),
            },
        ];
        let mut run = Run::new(
            "frostedge",
            "abcdef1234567890",
            "fix: things".into(),
            Trigger::Poll,
            steps,
        );
        run.status = RunStatus::Failure;
        run.finished_at = Some(run.started_at + 42);
        run
    }

    #[test]
    fn tail_is_fenced_and_capped() {
        let dir = tempfile::tempdir().unwrap();
        let run = failed_run();
        let path = crate::model::run_log_path(dir.path(), &run.project, &run.id, 1);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            (0..100).map(|i| format!("out {i}\n")).collect::<String>(),
        )
        .unwrap();
        let tail = failing_tail(dir.path(), &run, 30);
        assert!(
            tail.starts_with("```\n") && tail.ends_with("\n```"),
            "{tail}"
        );
        assert!(tail.contains("out 99"));
        assert_eq!(tail.lines().count(), 32);
        // Embed field limit respected even for a long log.
        let long = "y".repeat(5000);
        std::fs::write(&path, &long).unwrap();
        assert!(
            truncate(&failing_tail(dir.path(), &run, 30), MAX_FIELD)
                .chars()
                .count()
                <= MAX_FIELD
        );
    }

    #[test]
    fn payload_is_valid_json_within_limits() {
        let fields = vec![
            json!({"name": "Commit", "value": "x", "inline": true}),
            json!({"name": "Log tail", "value": truncate(&"z".repeat(3000), MAX_FIELD), "inline": false}),
        ];
        let payload = json!({"embeds": [{"title": "t", "fields": fields, "description": truncate(&"d".repeat(3000), MAX_DESCRIPTION)}]});
        let text = payload.to_string();
        assert!(text.len() > 1000);
        assert!(serde_json::from_str::<serde_json::Value>(&text).is_ok());
        let embed = &payload["embeds"][0];
        assert!(
            embed["fields"][1]["value"]
                .as_str()
                .unwrap()
                .chars()
                .count()
                <= MAX_FIELD
        );
        assert!(embed["description"].as_str().unwrap().chars().count() <= MAX_DESCRIPTION);
    }

    #[test]
    fn cancelled_run_embed_uses_grey_and_cancelled_at_field() {
        let dir = tempfile::tempdir().unwrap();
        let steps = vec![
            Step {
                phase: Phase::Ci,
                name: "Lint".into(),
                status: StepStatus::Success,
                duration_ms: Some(10),
                exit_code: Some(0),
                kind: crate::config::StepKind::Shell,
                substeps: Vec::new(),
            },
            Step {
                phase: Phase::Ci,
                name: "Build".into(),
                status: StepStatus::Cancelled,
                duration_ms: Some(25),
                exit_code: None,
                kind: crate::config::StepKind::Shell,
                substeps: Vec::new(),
            },
            Step {
                phase: Phase::Deploy,
                name: "Publish".into(),
                status: StepStatus::Skipped,
                duration_ms: None,
                exit_code: None,
                kind: crate::config::StepKind::Shell,
                substeps: Vec::new(),
            },
        ];
        let mut run = Run::new(
            "demo",
            "abcdef1234567890",
            "feat: cancel me".into(),
            Trigger::Manual,
            steps,
        );
        run.status = RunStatus::Cancelled;
        run.finished_at = Some(run.started_at + 12);

        let payload = finish_payload(&run, Some("owner/demo"), dir.path());
        let embed = &payload["embeds"][0];
        assert_eq!(embed["color"], 0x95A5A6);
        assert!(embed["title"].as_str().unwrap().contains("demo cancelled"));
        let fields = embed["fields"].as_array().unwrap();
        assert!(
            fields
                .iter()
                .any(|f| f["name"] == "Result" && f["value"] == "cancelled"),
            "{fields:?}"
        );
        assert!(
            fields
                .iter()
                .any(|f| f["name"] == "Cancelled at" && f["value"] == "Build"),
            "{fields:?}"
        );
        let steps_val = fields.iter().find(|f| f["name"] == "Steps").unwrap()["value"]
            .as_str()
            .unwrap();
        assert!(steps_val.contains("🚫 Build"), "{steps_val}");
    }

    #[test]
    fn username_and_mention_on_failure_payload_behaviour() {
        let dir = tempfile::tempdir().unwrap();
        let fail = failed_run();

        // Start payload includes username and blocks all mentions.
        let start = start_payload(&fail, Some("owner/demo"), Some("LunacyBot"));
        assert_eq!(start["username"], "LunacyBot");
        assert_eq!(start["allowed_mentions"]["parse"], json!([]));

        // Test payload includes username and "[lunacy] test notification".
        let test = test_payload(Some("frostedge"), Some("LunacyBot"));
        assert_eq!(test["username"], "LunacyBot");
        assert_eq!(test["embeds"][0]["title"], "[lunacy] test notification");
        assert!(test["embeds"][0]["description"]
            .as_str()
            .unwrap()
            .contains("frostedge"));
        assert_eq!(test["allowed_mentions"]["parse"], json!([]));

        // Failure finish payload includes username, content mention, and users/roles allowed_mentions (never everyone).
        let fail_payload = finish_payload_with_options(
            &fail,
            Some("owner/demo"),
            dir.path(),
            Some("LunacyBot"),
            Some("<@&999> build broke"),
        );
        assert_eq!(fail_payload["username"], "LunacyBot");
        assert_eq!(fail_payload["content"], "<@&999> build broke");
        assert_eq!(
            fail_payload["allowed_mentions"]["parse"],
            json!(["users", "roles"])
        );

        // Error finish payload also mentions.
        let mut err_run = failed_run();
        err_run.status = RunStatus::Error;
        let err_payload = finish_payload_with_options(
            &err_run,
            Some("owner/demo"),
            dir.path(),
            None,
            Some("<@123>"),
        );
        assert!(err_payload.get("username").is_none());
        assert_eq!(err_payload["content"], "<@123>");
        assert_eq!(
            err_payload["allowed_mentions"]["parse"],
            json!(["users", "roles"])
        );

        // Success and Cancelled omit content and keep allowed_mentions.parse = [].
        for status in [RunStatus::Success, RunStatus::Cancelled] {
            let mut ok_run = failed_run();
            ok_run.status = status;
            let payload = finish_payload_with_options(
                &ok_run,
                Some("owner/demo"),
                dir.path(),
                Some("LunacyBot"),
                Some("<@&999>"),
            );
            assert_eq!(payload["username"], "LunacyBot");
            assert!(
                payload.get("content").is_none(),
                "status={status:?} must not include content"
            );
            assert_eq!(payload["allowed_mentions"]["parse"], json!([]));
        }
    }

    #[test]
    fn notify_on_failures_skips_success_and_cancelled_but_posts_failure_and_error() {
        use std::io::{BufRead, BufReader, Read, Write};
        use std::net::TcpListener;
        use std::sync::{Arc, Mutex};

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().unwrap();
        let received: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(Vec::new()));
        let rec_clone = received.clone();

        // Accept exactly 2 requests (Failure + Error).
        let server = std::thread::spawn(move || {
            for _ in 0..2 {
                let Ok((mut stream, _)) = listener.accept() else {
                    break;
                };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut content_length = 0usize;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                    if let Some((k, v)) = line.split_once(':') {
                        if k.trim().eq_ignore_ascii_case("content-length") {
                            content_length = v.trim().parse().unwrap_or(0);
                        }
                    }
                }
                let mut body = vec![0u8; content_length];
                if content_length > 0 {
                    let _ = reader.read_exact(&mut body);
                }
                if let Ok(val) = serde_json::from_slice::<serde_json::Value>(&body) {
                    rec_clone.lock().unwrap().push(val);
                }
                let _ = stream.write_all(
                    b"HTTP/1.1 204 No Content\r\nConnection: close\r\nContent-Length: 0\r\n\r\n",
                );
            }
        });

        let resolved = ResolvedDiscord {
            url: format!("http://{addr}/webhook"),
            notify_on_start: false,
            notify_on: DiscordNotifyOn::Failures,
            username: Some("FailureBot".into()),
            mention_on_failure: Some("<@&42>".into()),
            source: "project",
        };
        let discord = Discord::from_resolved(&resolved).unwrap();
        let dir = tempfile::tempdir().unwrap();

        assert!(!discord.should_notify_finish(RunStatus::Success));
        assert!(!discord.should_notify_finish(RunStatus::Cancelled));
        assert!(discord.should_notify_finish(RunStatus::Failure));
        assert!(discord.should_notify_finish(RunStatus::Error));

        // Success and Cancelled are skipped without hitting the server.
        let mut success_run = failed_run();
        success_run.status = RunStatus::Success;
        discord
            .notify_finish(&success_run, Some("owner/demo"), dir.path())
            .unwrap();
        let mut cancelled_run = failed_run();
        cancelled_run.status = RunStatus::Cancelled;
        discord
            .notify_finish(&cancelled_run, Some("owner/demo"), dir.path())
            .unwrap();
        assert!(received.lock().unwrap().is_empty());

        // Failure and Error post to the server.
        let fail_run = failed_run();
        discord
            .notify_finish(&fail_run, Some("owner/demo"), dir.path())
            .unwrap();
        let mut err_run = failed_run();
        err_run.status = RunStatus::Error;
        discord
            .notify_finish(&err_run, Some("owner/demo"), dir.path())
            .unwrap();

        server.join().unwrap();
        let payloads = received.lock().unwrap();
        assert_eq!(payloads.len(), 2);
        assert_eq!(payloads[0]["username"], "FailureBot");
        assert_eq!(payloads[0]["content"], "<@&42>");
        assert_eq!(
            payloads[0]["allowed_mentions"]["parse"],
            json!(["users", "roles"])
        );
        assert_eq!(payloads[1]["username"], "FailureBot");
        assert_eq!(payloads[1]["content"], "<@&42>");
    }

    #[test]
    fn bad_uri_does_not_leak_webhook_secret_and_errors_redact_url() {
        use std::io::{BufRead, BufReader, Read, Write};
        use std::net::TcpListener;

        // 1. Malformed URI containing a secret token must NOT appear in the error.
        let secret = "super-secret-webhook-token-xyz";
        let bad_url = format!("not a url https://discord.com/api/webhooks/1/{secret}");
        let discord = Discord::new(&bad_url).unwrap();
        let err = discord.send_test(None).unwrap_err().to_string();
        assert_eq!(err, "invalid discord webhook URL");
        assert!(!err.contains(secret), "secret leaked: {err}");

        // 2. Non-2xx upstream response echoing the webhook URL must be redacted with MASK.
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().unwrap();
        let hook_url = format!("http://{addr}/webhooks/999/{secret}");
        let echo_body = format!("upstream rejected {hook_url} due to error");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut content_length = 0usize;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
                if let Some((k, v)) = line.split_once(':') {
                    if k.trim().eq_ignore_ascii_case("content-length") {
                        content_length = v.trim().parse().unwrap_or(0);
                    }
                }
            }
            if content_length > 0 {
                let mut buf = vec![0u8; content_length];
                let _ = reader.read_exact(&mut buf);
            }
            let resp = format!(
                "HTTP/1.1 500 Internal Server Error\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
                echo_body.len(),
                echo_body
            );
            let _ = stream.write_all(resp.as_bytes());
        });

        let discord = Discord::new(&hook_url).unwrap();
        let err = discord.send_test(None).unwrap_err().to_string();
        server.join().unwrap();
        assert!(err.contains("500"), "{err}");
        assert!(err.contains(crate::config::MASK), "{err}");
        assert!(!err.contains(secret), "secret leaked in error: {err}");
    }

    #[test]
    fn discord_client_does_not_follow_redirects() {
        use std::io::{BufRead, BufReader, Read, Write};
        use std::net::TcpListener;
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        // Target server that must NEVER be hit.
        let target_listener = TcpListener::bind("127.0.0.1:0").expect("bind target");
        target_listener.set_nonblocking(true).unwrap();
        let target_addr = target_listener.local_addr().unwrap();
        let target_hit = Arc::new(AtomicBool::new(false));
        let target_hit_clone = target_hit.clone();
        let stop_target = Arc::new(AtomicBool::new(false));
        let stop_target_clone = stop_target.clone();
        let target_thread = std::thread::spawn(move || {
            while !stop_target_clone.load(Ordering::SeqCst) {
                match target_listener.accept() {
                    Ok((mut s, _)) => {
                        target_hit_clone.store(true, Ordering::SeqCst);
                        let _ = s.write_all(
                            b"HTTP/1.1 204 No Content\r\nConnection: close\r\nContent-Length: 0\r\n\r\n",
                        );
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });

        // Redirecting server returning 302 -> target_addr.
        let redir_listener = TcpListener::bind("127.0.0.1:0").expect("bind redir");
        let redir_addr = redir_listener.local_addr().unwrap();
        let redir_thread = std::thread::spawn(move || {
            let (mut stream, _) = redir_listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut content_length = 0usize;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
                if let Some((k, v)) = line.split_once(':') {
                    if k.trim().eq_ignore_ascii_case("content-length") {
                        content_length = v.trim().parse().unwrap_or(0);
                    }
                }
            }
            if content_length > 0 {
                let mut buf = vec![0u8; content_length];
                let _ = reader.read_exact(&mut buf);
            }
            let resp = format!(
                "HTTP/1.1 302 Found\r\nLocation: http://{target_addr}/secret-target\r\nConnection: close\r\nContent-Length: 0\r\n\r\n"
            );
            let _ = stream.write_all(resp.as_bytes());
        });

        let discord = Discord::new(&format!("http://{redir_addr}/hook")).unwrap();
        let res = discord.send_test(None);
        redir_thread.join().unwrap();
        stop_target.store(true, Ordering::SeqCst);
        target_thread.join().unwrap();

        assert!(res.is_err(), "redirect must fail rather than succeed");
        assert!(
            !target_hit.load(Ordering::SeqCst),
            "redirect target must never be contacted"
        );
    }

    #[test]
    fn mention_on_failure_is_defensively_truncated_to_2000_chars() {
        let dir = tempfile::tempdir().unwrap();
        let fail = failed_run();
        let huge = "a".repeat(2500);
        let payload =
            finish_payload_with_options(&fail, Some("owner/demo"), dir.path(), None, Some(&huge));
        let content = payload["content"].as_str().expect("content");
        assert_eq!(content.chars().count(), MAX_CONTENT);
        assert!(content.ends_with('…'));
    }
}
