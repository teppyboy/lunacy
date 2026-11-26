//! Run/step model, persistence layout and log helpers shared by the daemon and the API.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Format unix seconds as UTC ISO-8601 (`YYYY-MM-DDTHH:MM:SSZ`).
pub fn format_iso_utc(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let hour = rem / 3600;
    let min = (rem % 3600) / 60;
    let sec = rem % 60;
    // Howard Hinnant's civil-from-days algorithm (days since 1970-01-01).
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let year = y + if m <= 2 { 1 } else { 0 };
    format!("{year:04}-{m:02}-{d:02}T{hour:02}:{min:02}:{sec:02}Z")
}

pub fn now_iso() -> String {
    format_iso_utc(now_secs())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunStatus {
    Running,
    Success,
    Failure,
    Error,
    Cancelled,
}

impl RunStatus {
    pub fn is_final(self) -> bool {
        !matches!(self, RunStatus::Running)
    }

    pub fn is_running(self) -> bool {
        matches!(self, RunStatus::Running)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StepStatus {
    Pending,
    Running,
    Success,
    Failure,
    Timeout,
    Skipped,
    Cancelled,
}

impl StepStatus {
    pub fn is_ok(self) -> bool {
        matches!(self, StepStatus::Success | StepStatus::Skipped)
    }

    pub fn emoji(self) -> &'static str {
        match self {
            StepStatus::Pending => "⏸️",
            StepStatus::Running => "🔄",
            StepStatus::Success => "✅",
            StepStatus::Failure => "❌",
            StepStatus::Timeout => "⏱️",
            StepStatus::Skipped => "⏭️",
            StepStatus::Cancelled => "🚫",
        }
    }

    /// Short label used in logs and messages.
    pub fn label(self) -> &'static str {
        match self {
            StepStatus::Pending => "pending",
            StepStatus::Running => "running",
            StepStatus::Success => "success",
            StepStatus::Failure => "failure",
            StepStatus::Timeout => "timeout",
            StepStatus::Skipped => "skipped",
            StepStatus::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    Ci,
    Deploy,
}

impl Phase {
    pub fn as_str(self) -> &'static str {
        match self {
            Phase::Ci => "ci",
            Phase::Deploy => "deploy",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Trigger {
    #[default]
    Poll,
    Manual,
    Webhook,
    PullRequest,
    ForceDeploy,
    Tag,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Substep {
    pub job: String,
    pub job_id: String,
    pub name: String,
    pub status: StepStatus,
    pub duration_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Step {
    pub phase: Phase,
    pub name: String,
    pub status: StepStatus,
    pub duration_ms: Option<u64>,
    pub exit_code: Option<i32>,
    pub kind: crate::config::StepKind,
    #[serde(default)]
    pub substeps: Vec<Substep>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Run {
    pub id: String,
    pub project: String,
    pub sha: String,
    #[serde(default)]
    pub subject: String,
    pub status: RunStatus,
    pub started_at: u64,
    pub finished_at: Option<u64>,
    pub failed_step: Option<String>,
    #[serde(default)]
    pub github_url: Option<String>,
    pub steps: Vec<Step>,
    #[serde(default)]
    pub trigger: Trigger,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr_number: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr_head_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr_base_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deploy_skipped_reason: Option<String>,
    #[serde(default)]
    pub deploy_only: bool,
}

impl Run {
    pub fn new(
        project: &str,
        sha: &str,
        subject: String,
        trigger: Trigger,
        steps: Vec<Step>,
    ) -> Run {
        let started_at = now_secs();
        Run {
            id: format!("{project}-{started_at}-{}", short_sha(sha)),
            project: project.to_string(),
            sha: sha.to_string(),
            subject,
            status: RunStatus::Running,
            started_at,
            finished_at: None,
            failed_step: None,
            github_url: None,
            steps,
            trigger,
            git_ref: None,
            pr_number: None,
            pr_title: None,
            pr_head_ref: None,
            pr_base_ref: None,
            deploy_skipped_reason: None,
            deploy_only: false,
        }
    }

    pub fn status_label(&self) -> &'static str {
        match self.status {
            RunStatus::Running => "running",
            RunStatus::Success => "success",
            RunStatus::Failure => "failure",
            RunStatus::Error => "error",
            RunStatus::Cancelled => "cancelled",
        }
    }

    /// The first step that failed (not skipped), for reporting.
    pub fn failed(&self) -> Option<&Step> {
        self.steps
            .iter()
            .find(|s| matches!(s.status, StepStatus::Failure | StepStatus::Timeout))
    }

    pub fn duration_secs(&self) -> u64 {
        self.finished_at
            .unwrap_or_else(now_secs)
            .saturating_sub(self.started_at)
    }
}

pub fn short_sha(sha: &str) -> &str {
    let n = sha.len().min(7);
    &sha[..n]
}

/// `state_dir/runs/<project>`
pub fn project_runs_dir(state_dir: &Path, project: &str) -> PathBuf {
    state_dir.join("runs").join(project)
}

pub fn run_json_path(state_dir: &Path, project: &str, id: &str) -> PathBuf {
    project_runs_dir(state_dir, project).join(format!("{id}.json"))
}

/// `state_dir/runs/<project>/<id>/<index>.log`
pub fn run_log_path(state_dir: &Path, project: &str, id: &str, index: usize) -> PathBuf {
    project_runs_dir(state_dir, project)
        .join(id)
        .join(format!("{index}.log"))
}

pub fn last_sha_path(state_dir: &Path, project: &str) -> PathBuf {
    state_dir.join("last-sha").join(project)
}

pub fn read_last_sha(state_dir: &Path, project: &str) -> Option<String> {
    let text = std::fs::read_to_string(last_sha_path(state_dir, project)).ok()?;
    let s = text.trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

pub fn write_last_sha(state_dir: &Path, project: &str, sha: &str) -> std::io::Result<()> {
    let path = last_sha_path(state_dir, project);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, format!("{sha}\n"))?;
    std::fs::rename(&tmp, &path)
}

pub fn pr_shas_path(state_dir: &Path, project: &str) -> PathBuf {
    state_dir
        .join("last-pr-shas")
        .join(format!("{project}.json"))
}

pub fn read_pr_shas(state_dir: &Path, project: &str) -> BTreeMap<u64, String> {
    let Ok(text) = std::fs::read_to_string(pr_shas_path(state_dir, project)) else {
        return BTreeMap::new();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

pub fn write_pr_shas(
    state_dir: &Path,
    project: &str,
    map: &BTreeMap<u64, String>,
) -> std::io::Result<()> {
    let path = pr_shas_path(state_dir, project);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(map)?)?;
    std::fs::rename(&tmp, &path)
}

pub fn seen_tags_path(state_dir: &Path, project: &str) -> PathBuf {
    state_dir.join("seen-tags").join(format!("{project}.json"))
}

pub fn read_seen_tags(state_dir: &Path, project: &str) -> BTreeMap<String, String> {
    let Ok(text) = std::fs::read_to_string(seen_tags_path(state_dir, project)) else {
        return BTreeMap::new();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

pub fn write_seen_tags(
    state_dir: &Path,
    project: &str,
    map: &BTreeMap<String, String>,
) -> std::io::Result<()> {
    let path = seen_tags_path(state_dir, project);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(map)?)?;
    std::fs::rename(&tmp, &path)
}

pub fn persist_run(state_dir: &Path, run: &Run) -> std::io::Result<()> {
    let path = run_json_path(state_dir, &run.project, &run.id);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(run)?)?;
    std::fs::rename(&tmp, &path)
}

/// Newest first, at most `limit`.
pub fn list_runs(state_dir: &Path, project: &str, limit: usize) -> Vec<Run> {
    let dir = project_runs_dir(state_dir, project);
    let mut runs: Vec<Run> = Vec::new();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return runs;
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().map(|e| e == "json").unwrap_or(false))
        .collect();
    files.sort();
    files.reverse();
    for path in files.into_iter().take(limit) {
        if let Ok(text) = std::fs::read_to_string(&path) {
            if let Ok(run) = serde_json::from_str::<Run>(&text) {
                runs.push(run);
            }
        }
    }
    runs
}

pub fn find_run(state_dir: &Path, project: &str, id: &str) -> Option<Run> {
    let path = run_json_path(state_dir, project, id);
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// Locate the project owning a run id by scanning run directories.
pub fn find_run_project(state_dir: &Path, id: &str) -> Option<String> {
    let dir = state_dir.join("runs");
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let p = entry.path();
        if !p.is_dir() {
            continue;
        }
        if p.join(format!("{id}.json")).is_file() {
            return p.file_name().map(|n| n.to_string_lossy().to_string());
        }
    }
    None
}

/// Read a persisted run by id across all project run directories.
pub fn read_run(state_dir: &Path, id: &str) -> Option<Run> {
    let project = find_run_project(state_dir, id)?;
    find_run(state_dir, &project, id)
}

/// A cancellation note appended to an orphaned run's step log file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppendedLogChunk {
    pub step: usize,
    pub chunk: String,
    pub new_len: u64,
}

/// Transition a persisted `RunStatus::Running` run that has no live worker into `RunStatus::Cancelled`,
/// finalizing any running steps/substeps, appending a best-effort note to existing step logs,
/// and persisting atomically.
pub fn reconcile_orphaned_run(state_dir: &Path, run: Run, reason: &str) -> std::io::Result<Run> {
    reconcile_orphaned_run_with_logs(state_dir, run, reason).map(|(updated, _)| updated)
}

/// Like [`reconcile_orphaned_run`], also returning each appended log chunk for existing step logs
/// that received the cancellation footer.
pub fn reconcile_orphaned_run_with_logs(
    state_dir: &Path,
    mut run: Run,
    reason: &str,
) -> std::io::Result<(Run, Vec<AppendedLogChunk>)> {
    if !run.status.is_running() {
        return Ok((run, Vec::new()));
    }
    let now = now_secs();
    run.status = RunStatus::Cancelled;
    run.finished_at = Some(now);
    let mut appended_logs = Vec::new();
    for (idx, step) in run.steps.iter_mut().enumerate() {
        if step.status == StepStatus::Running {
            step.status = StepStatus::Cancelled;
            for sub in &mut step.substeps {
                if sub.status == StepStatus::Running {
                    sub.status = StepStatus::Cancelled;
                }
            }
            let log_path = run_log_path(state_dir, &run.project, &run.id, idx);
            if log_path.is_file() {
                let note = format!("\n[lunacy] cancelled; {reason}\n");
                if let Ok(mut file) = std::fs::OpenOptions::new().append(true).open(&log_path) {
                    if file.write_all(note.as_bytes()).is_ok() {
                        let _ = file.flush();
                        let new_len = file
                            .metadata()
                            .map(|m| m.len())
                            .unwrap_or(note.len() as u64);
                        appended_logs.push(AppendedLogChunk {
                            step: idx,
                            chunk: note,
                            new_len,
                        });
                    }
                }
            }
        }
    }
    persist_run(state_dir, &run)?;
    Ok((run, appended_logs))
}

/// Scan `<state_dir>/runs/<project>/*.json` and reconcile any run still marked `running`
/// (e.g., left behind after a crash or unclean daemon restart) to `cancelled`.
pub fn reconcile_orphaned_runs(state_dir: &Path) -> Vec<Run> {
    let mut reconciled = Vec::new();
    let runs_root = state_dir.join("runs");
    let Ok(entries) = std::fs::read_dir(&runs_root) else {
        return reconciled;
    };
    let mut json_files: Vec<PathBuf> = Vec::new();
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            if let Ok(sub) = std::fs::read_dir(&p) {
                for f in sub.flatten() {
                    let fp = f.path();
                    if fp.extension().is_some_and(|e| e == "json") {
                        json_files.push(fp);
                    }
                }
            }
        } else if p.extension().is_some_and(|e| e == "json") {
            json_files.push(p);
        }
    }
    json_files.sort();
    for path in json_files {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(run) = serde_json::from_str::<Run>(&text) else {
            continue;
        };
        if !run.status.is_running() {
            continue;
        }
        let id = run.id.clone();
        let project = run.project.clone();
        match reconcile_orphaned_run(state_dir, run, "run was orphaned after daemon restart") {
            Ok(updated) => {
                log::warn!(
                    "[{project}] reconciled orphaned running run {id} to cancelled on startup"
                );
                reconciled.push(updated);
            }
            Err(e) => {
                log::warn!("[{project}] failed to reconcile orphaned run {id}: {e}");
            }
        }
    }
    reconciled
}

pub const LOG_TAIL_BYTES: u64 = 512 * 1024;

/// Last `lines` lines of a file, read from the end and capped at [`LOG_TAIL_BYTES`].
pub fn tail_lines(path: &Path, lines: usize) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let len = file.metadata()?.len();
    let start = len.saturating_sub(LOG_TAIL_BYTES);
    file.seek(SeekFrom::Start(start))?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)?;
    let text = String::from_utf8_lossy(&buf);
    if lines == 0 {
        return Ok(text.to_string());
    }
    let all: Vec<&str> = text.lines().collect();
    let from = all.len().saturating_sub(lines);
    // If we started mid-line, drop the partial first line.
    let slice_start = if start > 0 && all.len() > 1 {
        from.max(1)
    } else {
        from
    };
    Ok(all[slice_start..].join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tail_reads_last_lines() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("log");
        std::fs::write(&p, "one\ntwo\nthree\nfour\n").unwrap();
        assert_eq!(tail_lines(&p, 2).unwrap(), "three\nfour");
        assert_eq!(tail_lines(&p, 100).unwrap(), "one\ntwo\nthree\nfour");
    }

    #[test]
    fn last_sha_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_last_sha(dir.path(), "x").is_none());
        write_last_sha(dir.path(), "x", "abc").unwrap();
        assert_eq!(read_last_sha(dir.path(), "x").as_deref(), Some("abc"));
    }

    #[test]
    fn run_persistence_and_listing() {
        let dir = tempfile::tempdir().unwrap();
        let mut run = Run::new(
            "p",
            "0123456789abcdef",
            "subj".into(),
            Trigger::Poll,
            vec![],
        );
        persist_run(dir.path(), &run).unwrap();
        let listed = list_runs(dir.path(), "p", 10);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, run.id);
        run.status = RunStatus::Success;
        persist_run(dir.path(), &run).unwrap();
        assert_eq!(
            find_run(dir.path(), "p", &run.id).unwrap().status,
            RunStatus::Success
        );
        assert_eq!(find_run_project(dir.path(), &run.id).as_deref(), Some("p"));
        assert_eq!(read_run(dir.path(), &run.id).unwrap().id, run.id);
    }

    #[test]
    fn pr_shas_and_seen_tags_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_pr_shas(dir.path(), "demo").is_empty());
        let prs = BTreeMap::from([(12u64, "aaa".to_string()), (42u64, "bbb".to_string())]);
        write_pr_shas(dir.path(), "demo", &prs).unwrap();
        assert_eq!(read_pr_shas(dir.path(), "demo"), prs);

        assert!(!seen_tags_path(dir.path(), "demo").is_file());
        assert!(read_seen_tags(dir.path(), "demo").is_empty());
        let tags = BTreeMap::from([("v1.0.0".to_string(), "ccc".to_string())]);
        write_seen_tags(dir.path(), "demo", &tags).unwrap();
        assert!(seen_tags_path(dir.path(), "demo").is_file());
        assert_eq!(read_seen_tags(dir.path(), "demo"), tags);
    }

    #[test]
    fn trigger_serde_variants() {
        assert_eq!(
            serde_json::to_string(&Trigger::PullRequest).unwrap(),
            "\"pull_request\""
        );
        assert_eq!(
            serde_json::to_string(&Trigger::ForceDeploy).unwrap(),
            "\"force_deploy\""
        );
        assert_eq!(serde_json::to_string(&Trigger::Tag).unwrap(), "\"tag\"");
        assert_eq!(serde_json::to_string(&Trigger::Poll).unwrap(), "\"poll\"");
    }

    #[test]
    fn format_iso_utc_matches_known_timestamps() {
        assert_eq!(format_iso_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_iso_utc(1_700_000_000), "2023-11-14T22:13:20Z");
        assert_eq!(format_iso_utc(1_709_164_800), "2024-02-29T00:00:00Z");
    }

    #[test]
    fn reconcile_orphaned_runs_cancels_running_steps_substeps_and_appends_log() {
        let dir = tempfile::tempdir().unwrap();
        let mut orphan = Run::new(
            "demo",
            "0123456789abcdef0123456789abcdef01234567",
            "feat: crash".into(),
            Trigger::Manual,
            vec![
                Step {
                    phase: Phase::Ci,
                    name: "Build".into(),
                    status: StepStatus::Success,
                    duration_ms: Some(120),
                    exit_code: Some(0),
                    kind: crate::config::StepKind::Shell,
                    substeps: Vec::new(),
                },
                Step {
                    phase: Phase::Ci,
                    name: "Test".into(),
                    status: StepStatus::Running,
                    duration_ms: None,
                    exit_code: None,
                    kind: crate::config::StepKind::Act,
                    substeps: vec![
                        Substep {
                            job: "ci".into(),
                            job_id: "ci".into(),
                            name: "Checkout".into(),
                            status: StepStatus::Success,
                            duration_ms: Some(50),
                        },
                        Substep {
                            job: "ci".into(),
                            job_id: "ci".into(),
                            name: "Cargo test".into(),
                            status: StepStatus::Running,
                            duration_ms: None,
                        },
                    ],
                },
                Step {
                    phase: Phase::Deploy,
                    name: "Publish".into(),
                    status: StepStatus::Pending,
                    duration_ms: None,
                    exit_code: None,
                    kind: crate::config::StepKind::Shell,
                    substeps: Vec::new(),
                },
            ],
        );
        orphan.status = RunStatus::Running;
        persist_run(dir.path(), &orphan).unwrap();

        let log_path = run_log_path(dir.path(), "demo", &orphan.id, 1);
        std::fs::create_dir_all(log_path.parent().unwrap()).unwrap();
        std::fs::write(&log_path, "running tests...\n").unwrap();

        let mut finished = Run::new(
            "demo",
            "fedcba9876543210fedcba9876543210fedcba98",
            "feat: ok".into(),
            Trigger::Poll,
            vec![],
        );
        finished.id = "demo-999-fedcba9".into();
        finished.status = RunStatus::Success;
        finished.finished_at = Some(finished.started_at);
        persist_run(dir.path(), &finished).unwrap();

        let reconciled = reconcile_orphaned_runs(dir.path());
        assert_eq!(reconciled.len(), 1);
        let updated = &reconciled[0];
        assert_eq!(updated.id, orphan.id);
        assert_eq!(updated.status, RunStatus::Cancelled);
        assert!(updated.finished_at.is_some());
        assert_eq!(updated.steps[0].status, StepStatus::Success);
        assert_eq!(updated.steps[1].status, StepStatus::Cancelled);
        assert_eq!(updated.steps[1].substeps[0].status, StepStatus::Success);
        assert_eq!(updated.steps[1].substeps[1].status, StepStatus::Cancelled);
        assert_eq!(updated.steps[2].status, StepStatus::Pending);

        // Persisted on disk as cancelled.
        let reloaded = read_run(dir.path(), &orphan.id).unwrap();
        assert_eq!(reloaded.status, RunStatus::Cancelled);
        assert_eq!(reloaded.steps[1].status, StepStatus::Cancelled);

        // Log file got the cancellation note.
        let log_text = std::fs::read_to_string(&log_path).unwrap();
        assert!(
            log_text.contains("[lunacy] cancelled; run was orphaned after daemon restart"),
            "{log_text}"
        );

        // Running again is a no-op.
        assert!(reconcile_orphaned_runs(dir.path()).is_empty());
        // Already-finished run was untouched.
        assert_eq!(
            read_run(dir.path(), &finished.id).unwrap().status,
            RunStatus::Success
        );
    }
}
