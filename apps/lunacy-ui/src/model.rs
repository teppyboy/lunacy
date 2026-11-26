//! The daemon's JSON shapes, exactly as `docs/api.md` documents them. Unknown fields and
//! unknown enum values are tolerated, so a newer daemon doesn't break an older client.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunStatus {
    #[default]
    Running,
    Success,
    Failure,
    Error,
    Cancelled,
    #[serde(other)]
    Other,
}

impl RunStatus {
    pub fn is_running(self) -> bool {
        matches!(self, RunStatus::Running)
    }
    pub fn label(self) -> &'static str {
        match self {
            RunStatus::Running => "Running",
            RunStatus::Success => "Success",
            RunStatus::Failure => "Failure",
            RunStatus::Error => "Error",
            RunStatus::Cancelled => "Cancelled",
            RunStatus::Other => "Unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StepStatus {
    #[default]
    Pending,
    Running,
    Success,
    Failure,
    Timeout,
    Skipped,
    Cancelled,
    #[serde(other)]
    Other,
}

impl StepStatus {
    pub fn label(self) -> &'static str {
        match self {
            StepStatus::Pending => "Pending",
            StepStatus::Running => "Running",
            StepStatus::Success => "Success",
            StepStatus::Failure => "Failed",
            StepStatus::Timeout => "Timed out",
            StepStatus::Skipped => "Skipped",
            StepStatus::Cancelled => "Cancelled",
            StepStatus::Other => "Unknown",
        }
    }

    /// Whether the step still has work to do, so a log is worth re-reading.
    pub fn is_live(self) -> bool {
        matches!(self, StepStatus::Running)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    #[default]
    Ci,
    Deploy,
}

impl Phase {
    pub fn label(self) -> &'static str {
        match self {
            Phase::Ci => "CI",
            Phase::Deploy => "Deploy",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StepKind {
    #[default]
    Shell,
    Act,
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Substep {
    #[serde(default)]
    pub job: String,
    #[serde(default)]
    pub job_id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub status: StepStatus,
    #[serde(default)]
    pub duration_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Step {
    #[serde(default = "phase_ci")]
    pub phase: Phase,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub status: StepStatus,
    #[serde(default)]
    pub duration_ms: Option<u64>,
    #[serde(default)]
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub kind: StepKind,
    #[serde(default)]
    pub substeps: Vec<Substep>,
    /// `only_on` of the step's config, so the UI can say why it was skipped. Not in the v1
    /// contract; absent from a daemon that doesn't send it.
    #[serde(default)]
    pub only_on: Vec<String>,
}

fn phase_ci() -> Phase {
    Phase::Ci
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Run {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub project: String,
    #[serde(default)]
    pub sha: String,
    #[serde(default)]
    pub subject: String,
    #[serde(default)]
    pub status: RunStatus,
    #[serde(default)]
    pub started_at: u64,
    #[serde(default)]
    pub finished_at: Option<u64>,
    #[serde(default)]
    pub failed_step: Option<String>,
    #[serde(default)]
    pub github_url: Option<String>,
    #[serde(default)]
    pub steps: Vec<Step>,
    /// Trigger source (`poll` / `manual` / `webhook` / `pull_request` / `force_deploy` / `tag`).
    #[serde(default)]
    pub trigger: Option<String>,
    /// Human-readable reason explaining how and why this run was triggered.
    #[serde(default)]
    pub trigger_reason: Option<String>,
    #[serde(default)]
    pub git_ref: Option<String>,
    #[serde(default)]
    pub pr_number: Option<u64>,
    #[serde(default)]
    pub pr_title: Option<String>,
    #[serde(default)]
    pub pr_head_ref: Option<String>,
    #[serde(default)]
    pub pr_base_ref: Option<String>,
    #[serde(default)]
    pub deploy_only: bool,
    #[serde(default)]
    pub deploy_skipped_reason: Option<String>,
}

impl Run {
    pub fn duration_ms(&self, now: u64) -> u64 {
        let end = self.finished_at.unwrap_or(now);
        end.saturating_sub(self.started_at) * 1000
    }

    /// Short pill label for the trigger source.
    pub fn trigger_label(&self) -> Option<&'static str> {
        match self.trigger.as_deref() {
            Some("webhook") => Some("Webhook"),
            Some("manual") => Some("Manual"),
            Some("poll") => Some("Poll"),
            Some("pull_request") => Some("PR"),
            Some("force_deploy") => Some("Force deploy"),
            Some("tag") => Some("Tag"),
            _ => None,
        }
    }

    /// Trimmed non-empty `trigger_reason` if present, otherwise a readable fallback derived from
    /// `trigger` so historical runs from older daemon state files still explain how they started.
    pub fn trigger_display(&self) -> Option<String> {
        if let Some(reason) = self.trigger_reason.as_deref().map(str::trim) {
            if !reason.is_empty() {
                return Some(reason.to_string());
            }
        }
        match self.trigger.as_deref() {
            Some("webhook") => Some("Triggered by webhook".to_string()),
            Some("manual") => Some("Triggered manually".to_string()),
            Some("poll") => Some("Triggered by poll".to_string()),
            Some("pull_request") => Some("Triggered by pull request".to_string()),
            Some("force_deploy") => Some("Force deploy".to_string()),
            Some("tag") => Some("Triggered by tag".to_string()),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PrSummary {
    #[serde(default)]
    pub number: u64,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub head_ref: String,
    #[serde(default)]
    pub base_ref: String,
    #[serde(default)]
    pub head_sha: String,
    #[serde(default)]
    pub last_tested_sha: Option<String>,
    #[serde(default)]
    pub status: Option<RunStatus>,
}

fn default_true() -> bool {
    true
}

fn default_one() -> usize {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub branch: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub github_repo: Option<String>,
    #[serde(default)]
    pub upstream_sha: Option<String>,
    #[serde(default)]
    pub last_sha: Option<String>,
    #[serde(default)]
    pub running: bool,
    #[serde(default)]
    pub current_run: Option<Run>,
    #[serde(default)]
    pub active_runs: Vec<Run>,
    #[serde(default = "default_one")]
    pub max_concurrent_runs: usize,
    #[serde(default)]
    pub last_run: Option<Run>,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub open_prs: Vec<PrSummary>,
}

impl Default for Project {
    fn default() -> Self {
        Self {
            name: String::new(),
            branch: String::new(),
            enabled: true,
            github_repo: None,
            upstream_sha: None,
            last_sha: None,
            running: false,
            current_run: None,
            active_runs: Vec::new(),
            max_concurrent_runs: default_one(),
            last_run: None,
            last_error: None,
            open_prs: Vec::new(),
        }
    }
}

impl Project {
    /// Upstream is ahead of the last processed commit (or has never been processed).
    pub fn pending_update(&self) -> bool {
        match (&self.upstream_sha, &self.last_sha) {
            (Some(up), Some(last)) => up != last,
            (Some(_), None) => true,
            _ => false,
        }
    }

    /// All currently active runs for this project, falling back to `current_run` when connected
    /// to an older daemon that does not populate `active_runs`.
    pub fn active_run_list(&self) -> Vec<&Run> {
        if !self.active_runs.is_empty() {
            self.active_runs.iter().collect()
        } else {
            self.current_run.iter().collect()
        }
    }

    /// Number of runs currently active on this project.
    pub fn active_run_count(&self) -> usize {
        let listed = self.active_run_list().len();
        if listed > 0 {
            listed
        } else if self.running {
            1
        } else {
            0
        }
    }

    /// Whether this project has reached its per-project concurrency cap.
    pub fn at_concurrency_limit(&self) -> bool {
        let limit = self.max_concurrent_runs.max(1);
        self.active_run_count() >= limit
    }
}

/// The one status a project card shows: running beats its last result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectState {
    Running,
    Success,
    Failure,
    Error,
    Cancelled,
    Idle,
}

impl Project {
    pub fn state(&self) -> ProjectState {
        if self.running
            || !self.active_runs.is_empty()
            || self
                .current_run
                .as_ref()
                .is_some_and(|r| r.status.is_running())
        {
            return ProjectState::Running;
        }
        match self.last_run.as_ref().map(|r| r.status) {
            Some(RunStatus::Success) => ProjectState::Success,
            Some(RunStatus::Failure) => ProjectState::Failure,
            Some(RunStatus::Error) => ProjectState::Error,
            Some(RunStatus::Cancelled) => ProjectState::Cancelled,
            _ => ProjectState::Idle,
        }
    }

    pub fn short_name(&self) -> &str {
        &self.name
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Health {
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub version: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct WebhookInfo {
    #[serde(default)]
    pub configured: bool,
    #[serde(default)]
    pub endpoint: String,
    #[serde(default)]
    pub deliveries: u64,
    #[serde(default)]
    pub last_event: Option<String>,
    #[serde(default)]
    pub last_repo: Option<String>,
    #[serde(default)]
    pub last_delivery_id: Option<String>,
    #[serde(default)]
    pub last_received_at: Option<String>,
    #[serde(default)]
    pub last_outcome: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SystemInfo {
    #[serde(default)]
    pub os: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub config_path: String,
    #[serde(default)]
    pub state_dir: Option<String>,
    #[serde(default)]
    pub act: ToolInfo,
    #[serde(default)]
    pub docker: DockerInfo,
    #[serde(default)]
    pub git: ToolInfo,
    #[serde(default)]
    pub webhook: Option<WebhookInfo>,
    #[serde(default)]
    pub max_concurrent_runs: Option<usize>,
    #[serde(default)]
    pub act_max_concurrent: Option<usize>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ToolInfo {
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct DockerInfo {
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Workflow {
    #[serde(default)]
    pub file: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub jobs: Vec<WorkflowJob>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct WorkflowJob {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub runs_on: Vec<String>,
}

/// The daemon's error body: `{"error": "...", "field": "project[1].ci[0].run" | null}`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ApiError {
    #[serde(default)]
    pub error: String,
    #[serde(default)]
    pub field: Option<String>,
}

/// Response from `POST /api/runs/{id}/cancel` or `POST /api/projects/{name}/cancel`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CancelResult {
    #[serde(default)]
    pub cancelling: bool,
    #[serde(default)]
    pub cancelled: bool,
    #[serde(default)]
    pub orphaned: bool,
    #[serde(default)]
    pub run_id: String,
    #[serde(default)]
    pub project: String,
}

/// Response from `POST /api/projects/{name}/deploy` or `POST /api/runs/{id}/deploy`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ForceDeployResult {
    #[serde(default)]
    pub queued: bool,
    #[serde(default)]
    pub sha: String,
    #[serde(default)]
    pub deploy_only: bool,
}

/// Response from `POST /api/runs/{id}/retry`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RetryResult {
    #[serde(default)]
    pub queued: bool,
    #[serde(default)]
    pub project: String,
    #[serde(default)]
    pub sha: String,
    #[serde(default)]
    pub run_id: String,
}

/// Response from `POST /api/cache/clear` or `POST /api/projects/{name}/cache/clear`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CacheClearResult {
    #[serde(default)]
    pub cleared: bool,
    #[serde(default)]
    pub removed: Vec<String>,
}

/// Response from `POST /api/discord/test`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DiscordTestResult {
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub source: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckLevel {
    #[default]
    Ok,
    Warn,
    Error,
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CheckItem {
    #[serde(default)]
    pub level: CheckLevel,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub detail: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeployGitStatus {
    #[serde(default)]
    pub repo_exists: bool,
    #[serde(default)]
    pub is_git_repo: bool,
    #[serde(default)]
    pub remote_ok: bool,
    #[serde(default)]
    pub remote_url: Option<String>,
    #[serde(default)]
    pub dirty: Option<bool>,
    #[serde(default)]
    pub head_sha: Option<String>,
    #[serde(default)]
    pub upstream_sha: Option<String>,
    #[serde(default)]
    pub last_sha: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeployReporting {
    #[serde(default)]
    pub github_enabled: bool,
    #[serde(default)]
    pub ci_context: String,
    #[serde(default)]
    pub deploy_context: String,
    #[serde(default)]
    pub discord_source: String,
    #[serde(default)]
    pub discord_notify_on: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeployStepCheck {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub applies_to_host: bool,
    #[serde(default)]
    pub only_on: Vec<String>,
    #[serde(default)]
    pub effective_timeout: u64,
    #[serde(default)]
    pub command_summary: String,
    #[serde(default)]
    pub checks: Vec<CheckItem>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LastDeployStep {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub status: StepStatus,
    #[serde(default)]
    pub duration_ms: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LastDeploySummary {
    #[serde(default)]
    pub run_id: String,
    #[serde(default)]
    pub sha: String,
    #[serde(default)]
    pub status: RunStatus,
    #[serde(default)]
    pub finished_at: Option<u64>,
    #[serde(default)]
    pub steps: Vec<LastDeployStep>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeployProjectCheck {
    #[serde(default)]
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub repo_path: String,
    #[serde(default)]
    pub remote: String,
    #[serde(default)]
    pub branch: String,
    #[serde(default)]
    pub checkout: String,
    #[serde(default)]
    pub github_repo: Option<String>,
    #[serde(default)]
    pub poll_interval: Option<u64>,
    #[serde(default)]
    pub pipeline_timeout: Option<u64>,
    #[serde(default)]
    pub step_timeout: u64,
    #[serde(default)]
    pub overall: CheckLevel,
    #[serde(default)]
    pub git: DeployGitStatus,
    #[serde(default)]
    pub reporting: DeployReporting,
    #[serde(default)]
    pub ci_active_steps: usize,
    #[serde(default)]
    pub ci_total_steps: usize,
    #[serde(default)]
    pub deploy_active_steps: usize,
    #[serde(default)]
    pub deploy_total_steps: usize,
    #[serde(default)]
    pub deploy_steps: Vec<DeployStepCheck>,
    #[serde(default)]
    pub checks: Vec<CheckItem>,
    #[serde(default)]
    pub last_deploy: Option<LastDeploySummary>,
    #[serde(default)]
    pub watch_prs: bool,
    #[serde(default)]
    pub deploy_refs: Vec<String>,
    #[serde(default)]
    pub cache_enabled: bool,
    #[serde(default)]
    pub trigger_mode: Option<String>,
}

impl Default for DeployProjectCheck {
    fn default() -> Self {
        Self {
            name: String::new(),
            enabled: true,
            repo_path: String::new(),
            remote: String::new(),
            branch: String::new(),
            checkout: String::new(),
            github_repo: None,
            poll_interval: None,
            pipeline_timeout: None,
            step_timeout: 0,
            overall: CheckLevel::default(),
            git: DeployGitStatus::default(),
            reporting: DeployReporting::default(),
            ci_active_steps: 0,
            ci_total_steps: 0,
            deploy_active_steps: 0,
            deploy_total_steps: 0,
            deploy_steps: Vec::new(),
            checks: Vec::new(),
            last_deploy: None,
            watch_prs: false,
            deploy_refs: Vec::new(),
            cache_enabled: false,
            trigger_mode: None,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeployChecksReport {
    #[serde(default)]
    pub os: String,
    #[serde(default)]
    pub default_step_timeout: u64,
    #[serde(default)]
    pub github_enabled: bool,
    #[serde(default)]
    pub github_mode: Option<String>,
    #[serde(default)]
    pub discord_global: bool,
    #[serde(default)]
    pub cache_enabled: bool,
    #[serde(default)]
    pub cache_dir: String,
    #[serde(default)]
    pub projects: Vec<DeployProjectCheck>,
}

/// Unix seconds now, for "3m ago" and a running run's elapsed time. The daemon's own clock
/// drives the run timestamps; this only measures the distance to them.
///
/// `std::time` is unsupported on `wasm32-unknown-unknown` (it panics), so the browser reads
/// `Date.now()` instead.
pub fn now_unix() -> u64 {
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }
    #[cfg(target_arch = "wasm32")]
    {
        (js_sys::Date::now() / 1000.0) as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trigger_label_maps_known_sources() {
        for (raw, expected) in [
            (Some("webhook"), Some("Webhook")),
            (Some("manual"), Some("Manual")),
            (Some("poll"), Some("Poll")),
            (Some("pull_request"), Some("PR")),
            (Some("force_deploy"), Some("Force deploy")),
            (Some("tag"), Some("Tag")),
            (Some("other"), None),
            (None, None),
        ] {
            let run = Run {
                trigger: raw.map(str::to_string),
                ..Run::default()
            };
            assert_eq!(run.trigger_label(), expected, "trigger={raw:?}");
        }
    }

    #[test]
    fn trigger_display_prefers_trimmed_reason_and_falls_back_for_historical_runs() {
        let explicit = Run {
            trigger: Some("webhook".into()),
            trigger_reason: Some("  GitHub push webhook (master by teppyboy)  ".into()),
            ..Run::default()
        };
        assert_eq!(
            explicit.trigger_display().as_deref(),
            Some("GitHub push webhook (master by teppyboy)")
        );

        let blank_reason = Run {
            trigger: Some("manual".into()),
            trigger_reason: Some("   ".into()),
            ..Run::default()
        };
        assert_eq!(
            blank_reason.trigger_display().as_deref(),
            Some("Triggered manually")
        );

        for (raw, expected) in [
            (Some("webhook"), Some("Triggered by webhook")),
            (Some("manual"), Some("Triggered manually")),
            (Some("poll"), Some("Triggered by poll")),
            (Some("pull_request"), Some("Triggered by pull request")),
            (Some("force_deploy"), Some("Force deploy")),
            (Some("tag"), Some("Triggered by tag")),
            (Some("unknown"), None),
            (None, None),
        ] {
            let run = Run {
                trigger: raw.map(str::to_string),
                trigger_reason: None,
                ..Run::default()
            };
            assert_eq!(
                run.trigger_display().as_deref(),
                expected,
                "fallback trigger={raw:?}"
            );
        }
    }

    #[test]
    fn run_serde_tolerates_missing_or_present_trigger_reason() {
        let old: Run = serde_json::from_str(r#"{"id":"r-1","trigger":"poll"}"#).unwrap();
        assert_eq!(old.trigger.as_deref(), Some("poll"));
        assert_eq!(old.trigger_reason, None);
        assert_eq!(old.trigger_display().as_deref(), Some("Triggered by poll"));

        let new: Run = serde_json::from_str(
            r#"{"id":"r-2","trigger":"manual","trigger_reason":"Retry of r-1 (UI)"}"#,
        )
        .unwrap();
        assert_eq!(new.trigger_reason.as_deref(), Some("Retry of r-1 (UI)"));
        assert_eq!(new.trigger_display().as_deref(), Some("Retry of r-1 (UI)"));
    }

    #[test]
    fn project_active_runs_fallback_and_concurrency_limit() {
        // Older daemon omits active_runs & max_concurrent_runs -> defaults to 1 and falls back to current_run.
        let legacy: Project = serde_json::from_str(
            r#"{"name":"alpha","running":true,"current_run":{"id":"r-1","status":"running"}}"#,
        )
        .unwrap();
        assert_eq!(legacy.max_concurrent_runs, 1);
        assert_eq!(legacy.active_run_list().len(), 1);
        assert_eq!(legacy.active_run_list()[0].id, "r-1");
        assert_eq!(legacy.active_run_count(), 1);
        assert!(legacy.at_concurrency_limit());
        assert_eq!(legacy.state(), ProjectState::Running);

        // Multi-run project below limit stays open for another run.
        let mut multi = Project {
            name: "alpha".into(),
            running: true,
            max_concurrent_runs: 2,
            active_runs: vec![Run {
                id: "r-1".into(),
                status: RunStatus::Running,
                ..Run::default()
            }],
            ..Project::default()
        };
        assert_eq!(multi.active_run_count(), 1);
        assert!(!multi.at_concurrency_limit());

        // Reaching limit disables further triggers.
        multi.active_runs.push(Run {
            id: "r-2".into(),
            status: RunStatus::Running,
            ..Run::default()
        });
        assert_eq!(multi.active_run_count(), 2);
        assert!(multi.at_concurrency_limit());

        // SystemInfo deserializes optional concurrency limits cleanly.
        let sys: SystemInfo = serde_json::from_str(
            r#"{"os":"linux","max_concurrent_runs":4,"act_max_concurrent":2}"#,
        )
        .unwrap();
        assert_eq!(sys.max_concurrent_runs, Some(4));
        assert_eq!(sys.act_max_concurrent, Some(2));
    }
}
