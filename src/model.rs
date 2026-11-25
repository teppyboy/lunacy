//! Run/step model, persistence layout and log helpers shared by the daemon and the API.

use serde::{Deserialize, Serialize};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunStatus {
    Running,
    Success,
    Failure,
    Error,
}

impl RunStatus {
    pub fn is_final(self) -> bool {
        !matches!(self, RunStatus::Running)
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
#[serde(rename_all = "lowercase")]
pub enum Trigger {
    #[default]
    Poll,
    Manual,
    Webhook,
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
        }
    }

    pub fn status_label(&self) -> &'static str {
        match self.status {
            RunStatus::Running => "running",
            RunStatus::Success => "success",
            RunStatus::Failure => "failure",
            RunStatus::Error => "error",
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
    }
}
