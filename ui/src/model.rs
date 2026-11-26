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
    /// Not in the v1 contract (`poll` / `manual` / `webhook`).
    #[serde(default)]
    pub trigger: Option<String>,
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Project {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub branch: String,
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
    pub last_run: Option<Run>,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub open_prs: Vec<PrSummary>,
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

/// Response from `POST /api/cache/clear` or `POST /api/projects/{name}/cache/clear`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CacheClearResult {
    #[serde(default)]
    pub cleared: bool,
    #[serde(default)]
    pub removed: Vec<String>,
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeployProjectCheck {
    #[serde(default)]
    pub name: String,
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
