//! Discord webhook messages (embeds), best-effort.

use crate::model::{Run, RunStatus, StepStatus};
use anyhow::Result;
use serde_json::json;
use std::time::Duration;

const HTTP_TIMEOUT: Duration = Duration::from_secs(10);
/// Discord embed limits.
#[cfg(test)]
const MAX_DESCRIPTION: usize = 2000;
const MAX_FIELD: usize = 1000;
const LOG_TAIL_LINES: usize = 30;

pub struct Discord {
    url: String,
    agent: ureq::Agent,
}

impl Discord {
    pub fn new(url: &str) -> Result<Discord> {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(HTTP_TIMEOUT))
            .http_status_as_error(false)
            .build()
            .into();
        Ok(Discord {
            url: url.to_string(),
            agent,
        })
    }

    pub fn notify_start(&self, run: &Run, repo: Option<&str>) -> Result<()> {
        self.post(json!({
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
        }))
    }

    pub fn notify_finish(
        &self,
        run: &Run,
        repo: Option<&str>,
        state_dir: &std::path::Path,
    ) -> Result<()> {
        let (status_text, color) = match run.status {
            RunStatus::Success => ("success", 0x2ECC71),
            RunStatus::Failure => ("failure", 0xE74C3C),
            RunStatus::Error => ("error", 0xE67E22),
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
        if let Some(step) = run.failed() {
            let mut where_ = step.name.clone();
            if let Some(sub) = step
                .substeps
                .iter()
                .find(|s| matches!(s.status, StepStatus::Failure | StepStatus::Timeout))
            {
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

        self.post(json!({
            "embeds": [{
                "title": truncate(&format!("{} {} — {}", run.project, status_text, truncate(&run.subject, 120)), 256),
                "color": color,
                "fields": fields,
                "footer": { "text": format!("{} · {}", run.project, run.id) },
            }],
            "allowed_mentions": { "parse": [] },
        }))
    }

    fn post(&self, payload: serde_json::Value) -> Result<()> {
        let body = payload.to_string();
        if body.len() > 200_000 {
            anyhow::bail!("discord payload too large");
        }
        let mut response = self
            .agent
            .post(&self.url)
            .header("Content-Type", "application/json")
            .header("User-Agent", "lunacy")
            .send(body)?;
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            let text = response.body_mut().read_to_string().unwrap_or_default();
            anyhow::bail!(
                "discord webhook returned {status}: {}",
                text.chars().take(200).collect::<String>()
            );
        }
        Ok(())
    }
}

fn step_status_label(status: StepStatus) -> &'static str {
    match status {
        StepStatus::Timeout => "Timed out in",
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

impl StepStatus {
    /// Short label used in messages.
    pub fn label(self) -> &'static str {
        match self {
            StepStatus::Pending => "pending",
            StepStatus::Running => "running",
            StepStatus::Success => "success",
            StepStatus::Failure => "failure",
            StepStatus::Timeout => "timeout",
            StepStatus::Skipped => "skipped",
        }
    }
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
}
