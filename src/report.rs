//! GitHub reporting: commit statuses (PAT) and check runs (GitHub App).

use crate::config::{GithubCfg, GithubMode, MASK};
use crate::github_app::{
    self, parse_installation_id, parse_token_response, InstallationTokenCache,
};
use crate::model::{Phase, Run, RunStatus, Step, StepStatus};
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const HTTP_TIMEOUT: Duration = Duration::from_secs(15);

pub struct Github {
    mode: GithubMode,
    token: Option<String>,
    app: Option<AppState>,
    api_url: String,
    agent: ureq::Agent,
}

struct AppState {
    cfg: crate::config::AppCfg,
    key_pem: String,
    installations: Mutex<std::collections::HashMap<String, u64>>,
    tokens: Mutex<std::collections::HashMap<u64, InstallationTokenCache>>,
}

/// Summary of an open GitHub pull request returned by `GET /repos/{repo}/pulls`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PullRequestInfo {
    pub number: u64,
    pub title: String,
    pub head_sha: String,
    pub head_ref: String,
    pub base_ref: String,
    pub base_sha: String,
}

fn parse_open_pulls(body: &Value, target_repo: &str) -> Result<Vec<PullRequestInfo>> {
    let items = body
        .as_array()
        .context("GitHub pulls response was not a JSON array")?;
    let mut pulls = Vec::with_capacity(items.len());
    for item in items {
        let Some(number) = item.get("number").and_then(Value::as_u64) else {
            continue;
        };
        let Some(head_repo) = item
            .get("head")
            .and_then(|h| h.get("repo"))
            .filter(|v| v.is_object())
        else {
            continue;
        };
        if head_repo.get("fork").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let Some(head_full_name) = head_repo
            .get("full_name")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        let Some(base_full_name) = item
            .get("base")
            .and_then(|b| b.get("repo"))
            .and_then(|r| r.get("full_name"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        if !head_full_name.eq_ignore_ascii_case(base_full_name)
            || !head_full_name.eq_ignore_ascii_case(target_repo)
        {
            continue;
        }
        let Some(head_sha) = item
            .get("head")
            .and_then(|h| h.get("sha"))
            .and_then(Value::as_str)
            .filter(|s| crate::git::is_valid_sha(s))
        else {
            continue;
        };
        let title = item
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let head_ref = item
            .get("head")
            .and_then(|h| h.get("ref"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if !head_ref.is_empty() && !crate::git::is_valid_git_ref(&head_ref) {
            continue;
        }
        let base_ref = item
            .get("base")
            .and_then(|b| b.get("ref"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if !base_ref.is_empty() && !crate::git::is_valid_git_ref(&base_ref) {
            continue;
        }
        let base_sha = item
            .get("base")
            .and_then(|b| b.get("sha"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if !base_sha.is_empty() && !crate::git::is_valid_sha(&base_sha) {
            continue;
        }
        pulls.push(PullRequestInfo {
            number,
            title,
            head_sha: head_sha.to_string(),
            head_ref,
            base_ref,
            base_sha,
        });
    }
    Ok(pulls)
}

impl Github {
    pub fn new(cfg: &GithubCfg) -> Result<Github> {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(HTTP_TIMEOUT))
            .http_status_as_error(false)
            .build()
            .into();
        let app = match &cfg.app {
            Some(app) => Some(AppState {
                cfg: app.clone(),
                key_pem: app.key_pem()?,
                installations: Mutex::new(Default::default()),
                tokens: Mutex::new(Default::default()),
            }),
            None => None,
        };
        Ok(Github {
            mode: cfg.effective_mode(),
            token: cfg.token_value().filter(|t| !t.is_empty() && t != MASK),
            app,
            api_url: cfg.api_url().trim_end_matches('/').to_string(),
            agent,
        })
    }

    /// Bearer token for the REST API, minting/refreshing an App token if needed.
    fn bearer(&self, repo: &str) -> Result<String> {
        if let Some(app) = &self.app {
            return self.installation_token(app, repo);
        }
        self.token
            .clone()
            .context("no github token and no github.app configured")
    }

    fn installation_token(&self, app: &AppState, repo: &str) -> Result<String> {
        let now = github_app::unix_now();
        let id = match app.cfg.installation_id {
            Some(id) => id,
            None => {
                // Scoped so the guard is released before the request below.
                let cached = app
                    .installations
                    .lock()
                    .ok()
                    .and_then(|g| g.get(repo).copied());
                match cached {
                    Some(id) => id,
                    None => {
                        let jwt = github_app::app_jwt(&app.key_pem, app.cfg.app_id)?;
                        let url = format!("{}/repos/{repo}/installation", self.api_url);
                        let body = self.request_json("GET", &url, Some(&jwt), None)?;
                        let id = parse_installation_id(&body)?;
                        if let Ok(mut g) = app.installations.lock() {
                            g.insert(repo.to_string(), id);
                        }
                        id
                    }
                }
            }
        };

        let cache = app
            .tokens
            .lock()
            .ok()
            .and_then(|mut g| g.entry(id).or_default().get(now));
        if let Some(t) = cache {
            return Ok(t.token);
        }
        let jwt = github_app::app_jwt(&app.key_pem, app.cfg.app_id)?;
        let url = format!("{}/app/installations/{id}/access_tokens", self.api_url);
        let body = self.request_json("POST", &url, Some(&jwt), Some(json!({})))?;
        let token = parse_token_response(&body)?;
        let out = token.token.clone();
        if let Ok(mut g) = app.tokens.lock() {
            g.entry(id).or_default().store(token);
        }
        Ok(out)
    }

    fn request_json(
        &self,
        method: &str,
        url: &str,
        bearer: Option<&str>,
        body: Option<Value>,
    ) -> Result<Value> {
        fn authorize<B>(
            request: ureq::RequestBuilder<B>,
            bearer: Option<&str>,
        ) -> ureq::RequestBuilder<B> {
            let request = request
                .header("Accept", "application/vnd.github+json")
                .header("X-GitHub-Api-Version", "2022-11-28")
                .header("User-Agent", "lunacy");
            match bearer {
                Some(token) => request.header("Authorization", format!("Bearer {token}")),
                None => request,
            }
        }

        let response = match (method, body) {
            ("GET", _) => authorize(self.agent.get(url), bearer).call(),
            ("POST", Some(b)) => authorize(self.agent.post(url), bearer).send_json(&b),
            ("POST", None) => authorize(self.agent.post(url), bearer).send(""),
            ("PATCH", Some(b)) => authorize(self.agent.patch(url), bearer).send_json(&b),
            ("PATCH", None) => authorize(self.agent.patch(url), bearer).send(""),
            (other, _) => anyhow::bail!("unsupported method {other}"),
        };
        let mut response = match response {
            Ok(r) => r,
            // With `http_status_as_error(false)` this should not happen.
            Err(e) => anyhow::bail!("GitHub {method} {url} failed: {e}"),
        };
        let status = response.status().as_u16();
        let text = response.body_mut().read_to_string().unwrap_or_default();
        if !(200..300).contains(&status) {
            anyhow::bail!(
                "GitHub {method} {url} -> {status} {}",
                text.chars().take(300).collect::<String>()
            );
        }
        if text.trim().is_empty() {
            return Ok(Value::Null);
        }
        Ok(serde_json::from_str(&text).unwrap_or(Value::String(text)))
    }

    /// `POST /repos/{owner}/{repo}/statuses/{sha}`
    pub fn post_status(
        &self,
        repo: &str,
        sha: &str,
        state: &str,
        context: &str,
        description: &str,
    ) -> Result<()> {
        let token = self.bearer(repo)?;
        let url = format!("{}/repos/{repo}/statuses/{sha}", self.api_url);
        self.request_json(
            "POST",
            &url,
            Some(&token),
            Some(json!({
                "state": state,
                "context": context,
                "description": truncate(description, 140),
            })),
        )?;
        Ok(())
    }

    /// `POST /repos/{owner}/{repo}/check-runs`
    pub fn create_check(
        &self,
        repo: &str,
        sha: &str,
        name: &str,
        run_id: &str,
        details_url: Option<&str>,
    ) -> Result<Value> {
        let token = self.bearer(repo)?;
        let url = format!("{}/repos/{repo}/check-runs", self.api_url);
        let mut body = json!({
            "name": name,
            "head_sha": sha,
            "status": "in_progress",
            "started_at": iso8601_now(),
            "external_id": run_id,
            "output": { "title": format!("{name}: starting"), "summary": "_starting…_" },
        });
        if let Some(link) = details_url {
            body["details_url"] = json!(link);
        }
        self.request_json("POST", &url, Some(&token), Some(body))
    }

    /// `PATCH /repos/{owner}/{repo}/check-runs/{id}`
    pub fn update_check(&self, repo: &str, check_id: u64, body: Value) -> Result<Value> {
        let token = self.bearer(repo)?;
        let url = format!("{}/repos/{repo}/check-runs/{check_id}", self.api_url);
        self.request_json("PATCH", &url, Some(&token), Some(body))
    }

    /// `GET /repos/{owner}/{repo}/pulls?state=open&per_page=30`
    pub fn list_open_pulls(&self, repo: &str) -> Result<Vec<PullRequestInfo>> {
        let token = self.bearer(repo)?;
        let url = format!("{}/repos/{repo}/pulls?state=open&per_page=30", self.api_url);
        let body = self.request_json("GET", &url, Some(&token), None)?;
        parse_open_pulls(&body, repo)
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// RFC 3339 in UTC, second precision.
pub fn iso8601_now() -> String {
    iso8601(github_app::unix_now())
}

pub fn iso8601(unix: u64) -> String {
    let days = (unix / 86_400) as i64;
    let rem = unix % 86_400;
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Markdown table of a run's steps, the way the Actions UI summarises a job.
pub fn steps_markdown(run: &Run, limit: usize) -> String {
    let mut out = String::from("| Step | Status | Duration |\n|---|---|---|\n");
    for step in run.steps.iter().take(limit) {
        let duration = step
            .duration_ms
            .map(|ms| format!("{:.1}s", ms as f64 / 1000.0))
            .unwrap_or_else(|| "—".into());
        out.push_str(&format!(
            "| {} {} | {} | {} |\n",
            step.status.emoji(),
            step.name.replace('|', "\\|"),
            step.status.word(),
            duration
        ));
    }
    if run.steps.len() > limit {
        out.push_str(&format!(
            "\n_{} more step(s) not shown_\n",
            run.steps.len() - limit
        ));
    }
    // Nested per-job detail for act steps.
    for step in &run.steps {
        if step.substeps.is_empty() {
            continue;
        }
        out.push_str(&format!("\n<details><summary>{}</summary>\n\n", step.name));
        let mut job = String::new();
        for sub in &step.substeps {
            if sub.job != job {
                job = sub.job.clone();
                out.push_str(&format!(
                    "\n**{}** (`{}`)\n\n| Step | Status | Duration |\n|---|---|---|\n",
                    if job.is_empty() { "job" } else { &job },
                    sub.job_id
                ));
            }
            let duration = sub
                .duration_ms
                .map(|ms| format!("{:.1}s", ms as f64 / 1000.0))
                .unwrap_or_else(|| "—".into());
            out.push_str(&format!(
                "| {} {} | {} | {} |\n",
                sub.status.emoji(),
                sub.name.replace('|', "\\|"),
                sub.status.word(),
                duration
            ));
        }
        out.push_str("\n</details>\n");
    }
    out
}

/// The last `tail` lines of the failing step's log, in a fenced block,
/// kept under GitHub's 65535-character output limit.
pub fn failing_log_markdown(state_dir: &std::path::Path, run: &Run, tail: usize) -> String {
    let Some(step) = run.failed() else {
        return String::new();
    };
    let Some(index) = run.steps.iter().position(|s| std::ptr::eq(s, step)) else {
        return String::new();
    };
    let path = crate::model::run_log_path(state_dir, &run.project, &run.id, index);
    let Ok(text) = crate::model::tail_lines(&path, tail) else {
        return String::new();
    };
    if text.trim().is_empty() {
        return String::new();
    }
    let budget = 60_000usize;
    let body = if text.len() > budget {
        let mut start = text.len() - budget;
        while start < text.len() && !text.is_char_boundary(start) {
            start += 1;
        }
        format!("…(truncated)\n{}", &text[start..])
    } else {
        text
    };
    format!(
        "**{} {} — last {} lines**\n\n```text\n{}\n```\n",
        step.status.emoji(),
        step.name,
        tail,
        body.replace("```", "'''")
    )
}

impl StepStatus {
    pub fn word(self) -> &'static str {
        match self {
            StepStatus::Pending => "pending",
            StepStatus::Running => "running",
            StepStatus::Success => "success",
            StepStatus::Failure => "failure",
            StepStatus::Timeout => "timed out",
            StepStatus::Skipped => "skipped",
            StepStatus::Cancelled => "cancelled",
        }
    }
}

/// Check conclusion for a phase.
pub fn conclusion(run: &Run, phase: Phase) -> &'static str {
    let steps: Vec<&Step> = run.steps.iter().filter(|s| s.phase == phase).collect();
    if steps.is_empty() {
        return "success";
    }
    let mut any = false;
    let mut any_skipped = false;
    for s in &steps {
        match s.status {
            StepStatus::Skipped => any_skipped = true,
            StepStatus::Timeout => return "timed_out",
            StepStatus::Failure => return "failure",
            StepStatus::Cancelled => return "cancelled",
            StepStatus::Success => any = true,
            StepStatus::Pending | StepStatus::Running => return "cancelled",
        }
    }
    if run.status == RunStatus::Cancelled {
        let cancelled_in_other_phase = run
            .steps
            .iter()
            .any(|s| s.phase != phase && s.status == StepStatus::Cancelled);
        if !cancelled_in_other_phase && (!any || any_skipped || phase == Phase::Deploy) {
            return "cancelled";
        }
    }
    if any || !steps.is_empty() {
        "success"
    } else {
        "cancelled"
    }
}

pub fn status_state(run: &Run, phase: Phase) -> &'static str {
    match conclusion(run, phase) {
        "success" => "success",
        "timed_out" | "cancelled" => "error",
        _ => "failure",
    }
}

/// Throttles in-progress check updates: at most one every ~10s, unless forced
/// (every step transition forces one).
pub struct Throttle {
    last: Mutex<Option<Instant>>,
    interval: Duration,
}

impl Default for Throttle {
    fn default() -> Self {
        Throttle {
            last: Mutex::new(None),
            interval: Duration::from_secs(10),
        }
    }
}

impl Throttle {
    pub fn allows(&self, force: bool) -> bool {
        let mut last = match self.last.lock() {
            Ok(g) => g,
            Err(_) => return true,
        };
        let now = Instant::now();
        let due = match *last {
            None => true,
            Some(t) => now.duration_since(t) >= self.interval,
        };
        if force || due {
            *last = Some(now);
            true
        } else {
            false
        }
    }
}

/// Everything needed to report one phase of one run to GitHub.
#[derive(Clone, Debug)]
pub struct PhaseReporter {
    pub repo: String,
    pub sha: String,
    pub context: String,
    pub phase: Phase,
    pub run_id: String,
    pub details_url: Option<String>,
    pub state_dir: PathBuf,
}

/// A live check run, if one was created.
#[derive(Debug, Clone)]
pub struct CheckHandle {
    pub id: u64,
    pub url: Option<String>,
}

impl Github {
    pub fn start_phase(&self, r: &PhaseReporter) -> Result<Option<CheckHandle>> {
        match self.mode {
            GithubMode::Status => {
                self.post_status(&r.repo, &r.sha, "pending", &r.context, "queued")?;
                Ok(None)
            }
            GithubMode::Checks => {
                let body = self.create_check(
                    &r.repo,
                    &r.sha,
                    &r.context,
                    &r.run_id,
                    r.details_url.as_deref(),
                )?;
                let id = body
                    .get("id")
                    .and_then(Value::as_u64)
                    .context("check run has no id")?;
                let url = body
                    .get("html_url")
                    .and_then(Value::as_str)
                    .map(|s| s.to_string());
                Ok(Some(CheckHandle { id, url }))
            }
        }
    }

    /// Progress update while the phase runs.
    pub fn progress(
        &self,
        r: &PhaseReporter,
        check: &CheckHandle,
        run: &Run,
        title: String,
    ) -> Result<()> {
        let summary = steps_markdown(run, 25);
        match check.id {
            // Status mode has nothing to update until the end.
            0 => Ok(()),
            id => {
                self.update_check(
                    &r.repo,
                    id,
                    json!({
                        "status": "in_progress",
                        "output": { "title": truncate(&title, 250), "summary": summary },
                    }),
                )?;
                Ok(())
            }
        }
    }

    pub fn finish_phase(&self, r: &PhaseReporter, check: &CheckHandle, run: &Run) -> Result<()> {
        let conclusion = conclusion(run, r.phase);
        let mut description = phase_description(run, r.phase, conclusion);
        if conclusion == "success" && run.status == RunStatus::Error {
            description.push_str(" (pipeline error)");
        }
        match check.id {
            0 => {
                self.post_status(
                    &r.repo,
                    &r.sha,
                    status_state(run, r.phase),
                    &r.context,
                    &description,
                )?;
            }
            id => {
                let title = format!(
                    "{} {}: {}",
                    if r.phase == Phase::Ci { "CI" } else { "Deploy" },
                    conclusion,
                    truncate(&description, 200)
                );
                let mut summary = steps_markdown(run, 50);
                let log = failing_log_markdown(&r.state_dir, run, 150);
                if !log.is_empty() {
                    summary.push('\n');
                    summary.push_str(&log);
                }
                let mut body = json!({
                    "status": "completed",
                    "conclusion": conclusion,
                    "completed_at": iso8601(run.finished_at.unwrap_or_else(github_app::unix_now)),
                    "output": {
                        "title": truncate(&title, 250),
                        "summary": truncate_chars(&summary, 65_000),
                    },
                });
                if let Some(actions) = check_run_actions(r.phase, conclusion) {
                    body["actions"] = Value::Array(actions);
                }
                self.update_check(&r.repo, id, body)?;
            }
        }
        Ok(())
    }
}

/// Action buttons shown on completed GitHub Check Runs when a phase did not succeed.
///
/// Strictly respects GitHub's Check Runs API limits: at most 3 actions, `label <= 20`,
/// `description <= 40`, `identifier <= 20`.
pub(crate) fn check_run_actions(phase: Phase, conclusion: &str) -> Option<Vec<Value>> {
    if conclusion == "success" {
        return None;
    }
    let actions = match phase {
        Phase::Ci => vec![json!({
            "label": "Retry CI",
            "description": "Re-run CI for this commit in Lunacy",
            "identifier": "retry_ci",
        })],
        Phase::Deploy => vec![
            json!({
                "label": "Retry Deploy",
                "description": "Re-run deploy steps for this commit",
                "identifier": "retry_deploy",
            }),
            json!({
                "label": "Retry All",
                "description": "Re-run full CI + deploy pipeline",
                "identifier": "retry_ci",
            }),
        ],
    };
    Some(actions)
}

/// Human-readable description for a finished phase (used for commit statuses and check-run titles).
pub(crate) fn phase_description(run: &Run, phase: Phase, conclusion: &str) -> String {
    if conclusion == "cancelled" {
        let cancelled_step = run
            .steps
            .iter()
            .find(|s| s.phase == phase && s.status == StepStatus::Cancelled)
            .or_else(|| run.steps.iter().find(|s| s.status == StepStatus::Cancelled));
        return match cancelled_step {
            Some(step) => format!("{} cancelled", step.name),
            None => "cancelled".into(),
        };
    }
    match run.failed() {
        Some(step) if step.status == StepStatus::Timeout => format!("{} timed out", step.name),
        Some(step) => format!("{} failed", step.name),
        None if run.status == RunStatus::Error && conclusion == "success" => {
            "pipeline error".into()
        }
        None => "passed".into(),
    }
}

/// Truncate on a char boundary, keeping the tail informative.
fn truncate_chars(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = s[..end].to_string();
    out.push_str("\n\n_(truncated)_");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Phase, Run, Step, StepStatus, Trigger};

    fn step(phase: Phase, name: &str, status: StepStatus) -> Step {
        Step {
            phase,
            name: name.into(),
            status,
            duration_ms: Some(1500),
            exit_code: None,
            kind: crate::config::StepKind::Shell,
            substeps: Vec::new(),
        }
    }

    fn run_with(steps: Vec<Step>) -> Run {
        let mut run = Run::new("p", "0123456789abcdef", "subj".into(), Trigger::Poll, steps);
        run.finished_at = Some(run.started_at + 90);
        run
    }

    #[test]
    fn summary_has_a_row_per_step() {
        let run = run_with(vec![
            step(Phase::Ci, "Fmt", StepStatus::Success),
            step(Phase::Ci, "Test", StepStatus::Failure),
        ]);
        let md = steps_markdown(&run, 50);
        assert!(md.contains("| ✅ Fmt | success | 1.5s |"), "{md}");
        assert!(md.contains("| ❌ Test | failure |"), "{md}");
        assert_eq!(md.matches("\n| ").count(), 2);
    }

    #[test]
    fn summary_nests_act_substeps_by_job() {
        let mut s = step(Phase::Ci, "Web", StepStatus::Success);
        s.substeps = vec![
            crate::model::Substep {
                job: "Analyze".into(),
                job_id: "web".into(),
                name: "Test".into(),
                status: StepStatus::Success,
                duration_ms: Some(2000),
            },
            crate::model::Substep {
                job: "Audit".into(),
                job_id: "audit".into(),
                name: "Scan".into(),
                status: StepStatus::Skipped,
                duration_ms: None,
            },
        ];
        let run = run_with(vec![s]);
        let md = steps_markdown(&run, 50);
        assert!(md.contains("**Analyze** (`web`)"), "{md}");
        assert!(md.contains("**Audit** (`audit`)"), "{md}");
        assert!(md.contains("⏭️ Scan | skipped"), "{md}");
    }

    #[test]
    fn failing_log_is_fenced_and_truncated() {
        let dir = tempfile::tempdir().unwrap();
        let run = run_with(vec![
            step(Phase::Ci, "Fmt", StepStatus::Success),
            step(Phase::Ci, "Test", StepStatus::Failure),
        ]);
        let path = crate::model::run_log_path(dir.path(), &run.project, &run.id, 1);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let body: String = (0..500).map(|i| format!("line {i}\n")).collect();
        std::fs::write(&path, body).unwrap();
        let md = failing_log_markdown(dir.path(), &run, 30);
        assert!(md.starts_with("**❌ Test — last 30 lines**"), "{md}");
        assert!(md.contains("```text\n"), "{md}");
        assert!(md.contains("line 499"), "{md}");
        assert!(!md.contains("line 0\n"), "tail only: {md}");
    }

    #[test]
    fn huge_log_stays_under_the_api_limit() {
        let dir = tempfile::tempdir().unwrap();
        let run = run_with(vec![step(Phase::Ci, "Test", StepStatus::Failure)]);
        let path = crate::model::run_log_path(dir.path(), &run.project, &run.id, 0);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "x".repeat(400 * 1024)).unwrap();
        let md = failing_log_markdown(dir.path(), &run, 150);
        assert!(md.len() < 65_535, "len {}", md.len());
        assert!(md.contains("truncated"));
    }

    #[test]
    fn conclusions() {
        let ok = run_with(vec![step(Phase::Ci, "A", StepStatus::Success)]);
        assert_eq!(conclusion(&ok, Phase::Ci), "success");
        assert_eq!(status_state(&ok, Phase::Ci), "success");

        let timed = run_with(vec![step(Phase::Ci, "A", StepStatus::Timeout)]);
        assert_eq!(conclusion(&timed, Phase::Ci), "timed_out");
        assert_eq!(status_state(&timed, Phase::Ci), "error");

        let failed = run_with(vec![
            step(Phase::Ci, "A", StepStatus::Success),
            step(Phase::Ci, "B", StepStatus::Failure),
        ]);
        assert_eq!(conclusion(&failed, Phase::Ci), "failure");
        assert_eq!(status_state(&failed, Phase::Ci), "failure");

        // Deploy not run -> its phase is success (nothing to fail).
        assert_eq!(conclusion(&failed, Phase::Deploy), "success");

        // A skipped-only phase passes.
        let skipped = run_with(vec![step(Phase::Ci, "A", StepStatus::Skipped)]);
        assert_eq!(conclusion(&skipped, Phase::Ci), "success");

        // A cancelled step reports "cancelled" and status_state "error".
        let mut cancelled = run_with(vec![
            step(Phase::Ci, "A", StepStatus::Success),
            step(Phase::Ci, "B", StepStatus::Cancelled),
            step(Phase::Deploy, "D", StepStatus::Skipped),
        ]);
        cancelled.status = RunStatus::Cancelled;
        assert_eq!(conclusion(&cancelled, Phase::Ci), "cancelled");
        assert_eq!(status_state(&cancelled, Phase::Ci), "error");
        assert_eq!(
            phase_description(&cancelled, Phase::Ci, "cancelled"),
            "B cancelled"
        );
        let md = steps_markdown(&cancelled, 50);
        assert!(md.contains("| 🚫 B | cancelled |"), "{md}");

        // Cancelled between CI steps (all remaining skipped) -> cancelled, not passed.
        let mut between = run_with(vec![
            step(Phase::Ci, "A", StepStatus::Success),
            step(Phase::Ci, "B", StepStatus::Skipped),
        ]);
        between.status = RunStatus::Cancelled;
        assert_eq!(conclusion(&between, Phase::Ci), "cancelled");
        assert_eq!(
            phase_description(&between, Phase::Ci, "cancelled"),
            "cancelled"
        );
    }

    #[test]
    fn throttle_forces_on_transitions() {
        let t = Throttle::default();
        assert!(t.allows(false), "first update always goes");
        assert!(!t.allows(false));
        assert!(t.allows(true), "forced transitions bypass the interval");
    }

    #[test]
    fn iso8601_formatting() {
        assert_eq!(iso8601(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso8601(1_704_067_800), "2024-01-01T00:10:00Z");
        assert_eq!(iso8601(1_700_000_000), "2023-11-14T22:13:20Z");
    }

    #[test]
    fn truncation_helpers() {
        assert_eq!(truncate("abcdef", 3), "ab…");
        assert_eq!(truncate("abc", 10), "abc");
        let s = "é".repeat(100);
        let t = truncate_chars(&s, 51);
        assert!(t.len() <= 51 + 16);
    }

    #[test]
    fn parses_open_pulls_response_and_rejects_forks() {
        let body = json!([
            {
                "number": 42,
                "title": "feat: add pr builds",
                "head": {
                    "sha": "1111111111111111111111111111111111111111",
                    "ref": "feat/pr",
                    "repo": { "full_name": "TeppyBoy/Demo", "fork": false }
                },
                "base": {
                    "sha": "0000000000000000000000000000000000000000",
                    "ref": "master",
                    "repo": { "full_name": "teppyboy/demo", "fork": false }
                }
            },
            {
                "number": 43,
                "title": "fork pr with fork=true",
                "head": {
                    "sha": "2222222222222222222222222222222222222222",
                    "ref": "evil",
                    "repo": { "full_name": "teppyboy/demo", "fork": true }
                },
                "base": {
                    "sha": "0000000000000000000000000000000000000000",
                    "ref": "master",
                    "repo": { "full_name": "teppyboy/demo", "fork": false }
                }
            },
            {
                "number": 44,
                "title": "cross-repo fork pr",
                "head": {
                    "sha": "3333333333333333333333333333333333333333",
                    "ref": "feat/fork",
                    "repo": { "full_name": "attacker/demo", "fork": false }
                },
                "base": {
                    "sha": "0000000000000000000000000000000000000000",
                    "ref": "master",
                    "repo": { "full_name": "teppyboy/demo", "fork": false }
                }
            },
            {
                "number": 45,
                "title": "deleted head repo",
                "head": {
                    "sha": "4444444444444444444444444444444444444444",
                    "ref": "deleted",
                    "repo": null
                },
                "base": {
                    "sha": "0000000000000000000000000000000000000000",
                    "ref": "master",
                    "repo": { "full_name": "teppyboy/demo", "fork": false }
                }
            },
            {
                "number": 46,
                "title": "mismatched target repo",
                "head": {
                    "sha": "5555555555555555555555555555555555555555",
                    "ref": "feat/other",
                    "repo": { "full_name": "other/repo", "fork": false }
                },
                "base": {
                    "sha": "0000000000000000000000000000000000000000",
                    "ref": "master",
                    "repo": { "full_name": "other/repo", "fork": false }
                }
            },
            {
                "number": 99,
                "title": "incomplete"
            }
        ]);
        let pulls = parse_open_pulls(&body, "teppyboy/demo").unwrap();
        assert_eq!(
            pulls.len(),
            1,
            "expected only same-repo non-fork PR: {pulls:?}"
        );
        assert_eq!(pulls[0].number, 42);
        assert_eq!(pulls[0].title, "feat: add pr builds");
        assert_eq!(
            pulls[0].head_sha,
            "1111111111111111111111111111111111111111"
        );
        assert_eq!(pulls[0].head_ref, "feat/pr");
        assert_eq!(pulls[0].base_ref, "master");
        assert_eq!(
            pulls[0].base_sha,
            "0000000000000000000000000000000000000000"
        );
    }

    #[test]
    fn check_run_actions_respect_github_limits_and_phase_conclusions() {
        // Passing check runs omit actions to keep the GitHub UI clean.
        assert!(check_run_actions(Phase::Ci, "success").is_none());
        assert!(check_run_actions(Phase::Deploy, "success").is_none());

        // Non-success CI conclusions expose a single "Retry CI" button.
        for conclusion in ["failure", "timed_out", "cancelled"] {
            let actions = check_run_actions(Phase::Ci, conclusion).expect("CI actions");
            assert_eq!(actions.len(), 1);
            assert_eq!(actions[0]["label"], "Retry CI");
            assert_eq!(actions[0]["identifier"], "retry_ci");
            for a in &actions {
                assert!(a["label"].as_str().unwrap().chars().count() <= 20);
                assert!(a["description"].as_str().unwrap().chars().count() <= 40);
                assert!(a["identifier"].as_str().unwrap().chars().count() <= 20);
            }
        }

        // Non-success Deploy conclusions expose "Retry Deploy" and "Retry All".
        for conclusion in ["failure", "timed_out", "cancelled"] {
            let actions = check_run_actions(Phase::Deploy, conclusion).expect("Deploy actions");
            assert_eq!(actions.len(), 2);
            assert_eq!(actions[0]["label"], "Retry Deploy");
            assert_eq!(actions[0]["identifier"], "retry_deploy");
            assert_eq!(actions[1]["label"], "Retry All");
            assert_eq!(actions[1]["identifier"], "retry_ci");
            assert!(actions.len() <= 3);
            for a in &actions {
                assert!(a["label"].as_str().unwrap().chars().count() <= 20);
                assert!(a["description"].as_str().unwrap().chars().count() <= 40);
                assert!(a["identifier"].as_str().unwrap().chars().count() <= 20);
            }
        }
    }

    #[test]
    fn finish_phase_includes_actions_on_failure_and_omits_on_success() {
        use std::io::{BufRead, BufReader, Read, Write};
        use std::net::TcpListener;
        use std::sync::{Arc, Mutex};

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().unwrap();
        let captured: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
        let cap_clone = captured.clone();

        // Accept 2 PATCH requests: 1 failing deploy check, 1 passing CI check.
        let server = std::thread::spawn(move || {
            for _ in 0..2 {
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
                let mut body = vec![0u8; content_length];
                if content_length > 0 {
                    let _ = reader.read_exact(&mut body);
                }
                if let Ok(val) = serde_json::from_slice::<Value>(&body) {
                    cap_clone.lock().unwrap().push(val);
                }
                let _ = stream.write_all(
                    b"HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}",
                );
            }
        });

        let cfg = GithubCfg {
            mode: Some(GithubMode::Status),
            token: Some("ghp_test".into()),
            token_file: None,
            api_url: Some(format!("http://{addr}")),
            webhook_secret: None,
            webhook_secret_file: None,
            app: None,
        };
        let gh = Github::new(&cfg).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let handle = CheckHandle { id: 42, url: None };

        let fail_deploy_run = run_with(vec![step(Phase::Deploy, "Publish", StepStatus::Failure)]);
        let deploy_rep = PhaseReporter {
            repo: "owner/repo".into(),
            sha: fail_deploy_run.sha.clone(),
            context: "lunacy/deploy".into(),
            phase: Phase::Deploy,
            run_id: fail_deploy_run.id.clone(),
            details_url: None,
            state_dir: dir.path().to_path_buf(),
        };
        gh.finish_phase(&deploy_rep, &handle, &fail_deploy_run)
            .unwrap();

        let ok_ci_run = run_with(vec![step(Phase::Ci, "Test", StepStatus::Success)]);
        let ci_rep = PhaseReporter {
            repo: "owner/repo".into(),
            sha: ok_ci_run.sha.clone(),
            context: "lunacy/ci".into(),
            phase: Phase::Ci,
            run_id: ok_ci_run.id.clone(),
            details_url: None,
            state_dir: dir.path().to_path_buf(),
        };
        gh.finish_phase(&ci_rep, &handle, &ok_ci_run).unwrap();

        server.join().unwrap();
        let payloads = captured.lock().unwrap();
        assert_eq!(payloads.len(), 2);
        assert_eq!(payloads[0]["conclusion"], "failure");
        let actions = payloads[0]["actions"].as_array().expect("deploy actions");
        assert_eq!(actions.len(), 2);
        assert_eq!(actions[0]["identifier"], "retry_deploy");
        assert_eq!(actions[1]["identifier"], "retry_ci");

        assert_eq!(payloads[1]["conclusion"], "success");
        assert!(payloads[1].get("actions").is_none());
    }
}
