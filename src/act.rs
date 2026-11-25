//! Driving nektos/act: platform maps, WSL path translation, `--json` line parsing.

use crate::config::{ActCfg, Os, StepCfg};
use crate::model::{StepStatus, Substep};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Plans for hosts that cannot run container jobs; act maps them to the host.
pub const SELF_HOSTED: &str = "-self-hosted";

/// Per-OS default platform map, before the global/project/step overrides.
pub fn default_platforms(os: Os) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    match os {
        // Linux can run ubuntu-* in Docker; windows/macos jobs are not supported here.
        Os::Linux => {}
        Os::Macos => {
            for key in ["macos-latest", "macos-14", "macos-15"] {
                map.insert(key.to_string(), SELF_HOSTED.to_string());
            }
        }
        Os::Windows => {
            for key in ["windows-latest", "windows-2022", "windows-2025"] {
                map.insert(key.to_string(), SELF_HOSTED.to_string());
            }
        }
    }
    map
}

/// Merge order: OS defaults < `[act]` < project < step. Later wins.
pub fn resolve_platforms(
    os: Os,
    act: &ActCfg,
    project: &BTreeMap<String, String>,
    step: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut map = default_platforms(os);
    for source in [&act.platforms, project, step] {
        for (k, v) in source {
            map.insert(k.clone(), v.clone());
        }
    }
    map
}

/// True when the job behind this act step would run on the host itself,
/// which on Windows must use native act (WSL is Linux and cannot be self-hosted).
///
/// When `job_runs_on` is empty (no job configured, unreadable workflow, or an
/// unknown job) the answer is `false`: better to use the configured command
/// than to guess.
pub fn uses_self_hosted(platforms: &BTreeMap<String, String>, job_runs_on: &[String]) -> bool {
    job_runs_on.iter().any(|label| {
        // act runs a job natively when it asks for `self-hosted` itself, or
        // when lunacy mapped its label to `-self-hosted`.
        label == "self-hosted"
            || platforms
                .get(label)
                .map(|v| v == SELF_HOSTED)
                .unwrap_or(false)
    })
}

/// Pick the act argv prefix for a step.
pub fn act_command(act: &ActCfg, self_hosted: bool) -> Vec<String> {
    if cfg!(windows) && self_hosted {
        act.native_command()
    } else {
        act.command()
    }
}

/// `C:\Users\x\repo` -> `/mnt/c/Users/x/repo`. Non-Windows paths pass through.
pub fn wsl_path(path: &Path) -> PathBuf {
    let s = path.to_string_lossy();
    let bytes = s.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
        let drive = (bytes[0] as char).to_ascii_lowercase();
        let rest = s[2..].replace('\\', "/");
        let rest = rest.trim_start_matches('/');
        return PathBuf::from(format!("/mnt/{drive}/{rest}"));
    }
    if s.starts_with(r"\\") {
        // UNC path: WSL mounts it under /mnt/<host>/<share> only when configured;
        // leave it alone rather than emitting something wrong.
        return path.to_path_buf();
    }
    if s.contains('\\') {
        // A relative Windows path is still a POSIX-unfriendly path in WSL.
        return PathBuf::from(s.replace('\\', "/"));
    }
    path.to_path_buf()
}

/// Translate the arguments of an act command line for WSL mode.
///
/// Only path-shaped values are touched: `-W`/`-e`/`--secret-file`/`--var-file`,
/// `--artifact-server-path`, and any `--flag=<path>` form.
pub fn wsl_translate_args(args: &[String]) -> Vec<String> {
    const PATH_FLAGS: &[&str] = &[
        "-W",
        "--workflows",
        "-e",
        "--eventpath",
        "--secret-file",
        "--var-file",
        "--artifact-server-path",
        "-C",
        "--directory",
    ];
    let mut out = Vec::with_capacity(args.len());
    let mut expect_path = false;
    for arg in args {
        if expect_path {
            out.push(wsl_path(Path::new(arg)).to_string_lossy().to_string());
            expect_path = false;
            continue;
        }
        if PATH_FLAGS.contains(&arg.as_str()) {
            out.push(arg.clone());
            expect_path = true;
            continue;
        }
        if let Some((flag, value)) = arg.split_once('=') {
            if PATH_FLAGS.contains(&flag) || flag == "--input" {
                out.push(format!(
                    "{flag}={}",
                    wsl_path(Path::new(value)).to_string_lossy()
                ));
                continue;
            }
        }
        out.push(arg.clone());
    }
    out
}

// ---------------------------------------------------------- act --json

/// One `act --json` line. act emits several shapes; unknown fields are ignored.
#[derive(Debug, Clone, Deserialize, Default)]
struct ActLine {
    #[serde(default)]
    job: Option<String>,
    #[serde(rename = "jobID", default)]
    job_id: Option<String>,
    #[serde(default)]
    step: Option<String>,
    #[serde(rename = "stepID", default)]
    step_id: Option<Vec<String>>,
    #[serde(rename = "stepid", default)]
    stepid_lower: Option<Vec<String>>,
    #[serde(default)]
    msg: Option<String>,
    #[serde(rename = "stepResult", default)]
    step_result: Option<String>,
    #[serde(rename = "jobResult", default)]
    job_result: Option<String>,
    #[serde(rename = "executionTime", default)]
    execution_time: Option<u64>,
    #[serde(rename = "raw_output", default)]
    raw_output: Option<bool>,
}

/// Live progress accumulator built from act's JSON lines.
#[derive(Debug, Default)]
pub struct ActProgress {
    substeps: Vec<Substep>,
    current_job: Option<String>,
    current_job_id: Option<String>,
    jobs_seen: Vec<String>,
    /// Step names act reported as `--setup-job` / `--complete-job`; the later
    /// `stepResult` line for those carries no `stepID`, so the name is the only
    /// way to keep them out of the substep list.
    bookkeeping_steps: Vec<String>,
    /// Name of the job that just finished, so its final line keeps its prefix.
    finished_job: Option<String>,
}

impl ActProgress {
    pub fn new() -> ActProgress {
        ActProgress::default()
    }

    pub fn substeps(&self) -> Vec<Substep> {
        self.substeps.clone()
    }

    /// Feed one output line. Returns the human-readable text act intended
    /// (`msg`) when the line was a JSON event, `None` for non-JSON noise.
    pub fn feed(&mut self, line: &str) -> Option<String> {
        let trimmed = line.trim();
        if !trimmed.starts_with('{') {
            return None;
        }
        let parsed: ActLine = serde_json::from_str(trimmed).ok()?;

        if let Some(job) = &parsed.job {
            // act prefixes the job name with the workflow name: "CI/Validate".
            let (_, name) = job.split_once('/').unwrap_or((job.as_str(), ""));
            let short = if name.is_empty() {
                job.clone()
            } else {
                name.to_string()
            };
            self.current_job = Some(short.clone());
            self.finished_job = None;
            if !self.jobs_seen.contains(&short) {
                self.jobs_seen.push(short);
            }
        }
        if let Some(id) = &parsed.job_id {
            self.current_job_id = Some(id.clone());
        }

        let step_id = parsed.step_id.as_ref().or(parsed.stepid_lower.as_ref());
        let is_bookkeeping = step_id
            .map(|ids| {
                ids.iter()
                    .any(|id| id == "--setup-job" || id == "--complete-job")
            })
            .unwrap_or(false);

        // act emits four line shapes; `raw_output` is plain log text and
        // `job_result` closes a job. Only start/result lines touch substeps.
        if parsed.raw_output == Some(true) {
            return parsed.msg;
        }

        if let Some(step_name) = &parsed.step {
            if is_bookkeeping {
                // `stepResult` for these carries no `stepID`, so remember the name.
                if !self.bookkeeping_steps.contains(step_name) {
                    self.bookkeeping_steps.push(step_name.clone());
                }
            } else if self.bookkeeping_steps.contains(step_name) {
                // Closing result of a setup/teardown step: never a substep.
            } else if let Some(result) = &parsed.step_result {
                let status = match result.as_str() {
                    "success" => StepStatus::Success,
                    "skipped" => StepStatus::Skipped,
                    _ => StepStatus::Failure,
                };
                let duration_ms = parsed.execution_time.map(|ns| ns / 1_000_000);
                if let Some(existing) = self
                    .substeps
                    .iter_mut()
                    .rev()
                    .find(|s| s.name == *step_name && s.status == StepStatus::Running)
                {
                    existing.status = status;
                    existing.duration_ms = duration_ms;
                } else {
                    self.substeps.push(Substep {
                        job: self.current_job.clone().unwrap_or_default(),
                        job_id: self.current_job_id.clone().unwrap_or_default(),
                        name: step_name.clone(),
                        status,
                        duration_ms,
                    });
                }
            } else if step_id.is_some()
                && parsed
                    .msg
                    .as_deref()
                    .is_some_and(|m| m.trim_start().starts_with('⭐'))
            {
                // A step start line ("⭐ Run Main X"). Other lines with a stepID, such as
                // the "exit status 1" error after a failure, are not starts.
                self.substeps.push(Substep {
                    job: self.current_job.clone().unwrap_or_default(),
                    job_id: self.current_job_id.clone().unwrap_or_default(),
                    name: step_name.clone(),
                    status: StepStatus::Running,
                    duration_ms: None,
                });
            }
        }

        if parsed.job_result.is_some() {
            // A finished job leaves nothing running, and a later job reuses the
            // same step names, so drop anything still marked running.
            let stale: Vec<String> = self
                .substeps
                .iter()
                .filter(|s| s.status == StepStatus::Running)
                .map(|s| s.name.clone())
                .collect();
            for s in self.substeps.iter_mut() {
                if s.status == StepStatus::Running {
                    s.status = StepStatus::Failure;
                }
            }
            self.bookkeeping_steps.retain(|n| !stale.contains(n));
            // Keep the name for this line's prefix; the next job start replaces it.
            self.finished_job = self.current_job.take();
            self.current_job_id = None;
        }

        parsed.msg
    }

    /// Prefix human lines with the job name when the run has several jobs.
    pub fn human_line(&self, msg: &str) -> String {
        if self.jobs_seen.len() > 1 {
            if let Some(job) = self.current_job.as_ref().or(self.finished_job.as_ref()) {
                return format!("[{job}] {msg}");
            }
        }
        msg.to_string()
    }
}

// ---------------------------------------------------------- workflow YAML

#[derive(Debug, Clone, Deserialize)]
pub struct WorkflowJob {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default, rename = "runs-on")]
    pub runs_on: Option<serde_yaml::Value>,
}

impl WorkflowJob {
    /// `runs-on` as a list of labels (`runs-on: [self-hosted, linux]` and the
    /// scalar form both work; mapping forms are ignored).
    pub fn runs_on_labels(&self) -> Vec<String> {
        match &self.runs_on {
            Some(serde_yaml::Value::String(s)) => vec![s.clone()],
            Some(serde_yaml::Value::Sequence(seq)) => seq
                .iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect(),
            _ => Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct WorkflowDoc {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub jobs: BTreeMap<String, WorkflowJob>,
}

pub fn parse_workflow(text: &str) -> Result<WorkflowDoc, serde_yaml::Error> {
    serde_yaml::from_str(text)
}

/// Labels `runs-on` for a job in a workflow file; empty when it cannot be read.
pub fn job_runs_on(workflow_text: Option<&str>, job: Option<&str>) -> Vec<String> {
    let (Some(text), Some(job_id)) = (workflow_text, job) else {
        return Vec::new();
    };
    let Ok(doc) = parse_workflow(text) else {
        return Vec::new();
    };
    doc.jobs
        .get(job_id)
        .map(|j| j.runs_on_labels())
        .unwrap_or_default()
}

// ---------------------------------------------------------- invocation

/// Everything a step needs, resolved from config plus the repo checkout.
pub struct ActPlan {
    /// Files act needs on the command line; created by the caller, deleted after.
    pub event_file: PathBuf,
    pub secret_file: Option<PathBuf>,
    pub var_file: Option<PathBuf>,
    pub artifact_dir: PathBuf,
    pub workflow: String,
    pub job: Option<String>,
    pub event: String,
}

/// Build the act argv (without the program, which is returned separately).
pub fn build_args(
    plan: &ActPlan,
    step: &StepCfg,
    platforms: &BTreeMap<String, String>,
    act: &ActCfg,
    wsl: bool,
) -> Vec<String> {
    let mut args = vec![plan.event.clone(), "--json".to_string()];
    args.push("-W".into());
    args.push(plan.workflow.clone());
    if let Some(job) = &plan.job {
        args.push("-j".into());
        args.push(job.clone());
    }
    args.push("-e".into());
    args.push(plan.event_file.to_string_lossy().to_string());
    for (label, image) in platforms {
        args.push("-P".into());
        args.push(format!("{label}={image}"));
    }
    if let Some(secret) = &plan.secret_file {
        args.push("--secret-file".into());
        args.push(secret.to_string_lossy().to_string());
    }
    if let Some(vars) = &plan.var_file {
        args.push("--var-file".into());
        args.push(vars.to_string_lossy().to_string());
    }
    args.push("--artifact-server-path".into());
    args.push(plan.artifact_dir.to_string_lossy().to_string());
    args.extend(act.args.iter().cloned());
    args.extend(step.args.iter().cloned());
    if wsl {
        wsl_translate_args(&args)
    } else {
        args
    }
}

/// The `event.json` body act expects; the same shape lunarum's deploy worker builds.
pub fn event_json(sha: &str, branch: &str, repo: Option<&str>) -> String {
    let repo = repo.unwrap_or("local/repo");
    let (owner, name) = repo.split_once('/').unwrap_or(("local", repo));
    serde_json::json!({
        "after": sha,
        "before": "0000000000000000000000000000000000000000",
        "ref": format!("refs/heads/{branch}"),
        "repository": {
            "full_name": repo,
            "name": name,
            "owner": { "login": owner },
            "default_branch": branch,
        },
        "head_commit": { "id": sha, "message": "" },
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"{"job":"Rust CI/Validate workspace","jobID":"validate","stage":"Main","step":"Build it","stepID":["0"],"msg":"⭐ Run Main Build it"}
{"step":"Build it","raw_output":true,"msg":"hi\n"}
{"step":"Build it","raw_output":true,"msg":"there\n"}
{"step":"Build it","stepResult":"success","executionTime":5501845}
{"job":"Rust CI/Validate workspace","jobID":"validate","stage":"Main","step":"Run tests","stepID":["1"],"msg":"⭐ Run Main Run tests"}
{"step":"Run tests","raw_output":true,"msg":"assertion failed\n"}
{"step":"Run tests","stepResult":"failure","executionTime":2500000000}
{"job":"Rust CI/Validate workspace","jobID":"validate","level":"error","stage":"Main","step":"Run tests","stepID":["1"],"msg":"exit status 1"}
{"jobID":"validate","jobResult":"failure","msg":"🏁  Job failed"}
not json at all
{"job":"Rust CI/Audit dependencies","jobID":"audit","stage":"Main","step":"Scan","stepID":["0"],"msg":"⭐ Run Main Scan"}
{"step":"Scan","stepResult":"skipped"}
{"jobID":"audit","jobResult":"success","msg":"🏁  Job succeeded"}
"#;

    #[test]
    fn parses_recorded_act_json() {
        let mut p = ActProgress::new();
        let mut human = Vec::new();
        for line in FIXTURE.lines() {
            if let Some(msg) = p.feed(line) {
                if !line.contains("raw_output") {
                    human.push(p.human_line(&msg));
                }
            }
        }
        let subs = p.substeps();
        assert_eq!(subs.len(), 3, "{subs:?}");
        assert_eq!(subs[0].name, "Build it");
        assert_eq!(subs[0].job, "Validate workspace");
        assert_eq!(subs[0].job_id, "validate");
        assert_eq!(subs[0].status, StepStatus::Success);
        assert_eq!(subs[0].duration_ms, Some(5));
        assert_eq!(subs[1].status, StepStatus::Failure);
        assert_eq!(subs[1].duration_ms, Some(2500));
        assert_eq!(subs[2].status, StepStatus::Skipped);
        // Two jobs -> messages get a job prefix. Streaming means job 1's lines
        // were already written before job 2 was seen, so only later lines can
        // be prefixed; that is called out here rather than faked.
        assert_eq!(human.len(), 6, "{human:?}");
        assert!(
            human[0].starts_with("⭐"),
            "unprefixed first job: {human:?}"
        );
        assert!(human[4].starts_with("[Audit dependencies] "), "{human:?}");
        assert!(human[5].starts_with("[Audit dependencies] "), "{human:?}");
    }

    #[test]
    fn non_json_lines_are_passed_through_as_noise() {
        let mut p = ActProgress::new();
        assert!(p.feed("just build output").is_none());
        assert!(p.substeps().is_empty());
    }

    #[test]
    fn setup_and_teardown_steps_are_not_substeps() {
        let mut p = ActProgress::new();
        p.feed(r#"{"job":"CI/build","jobID":"build","step":"Set up job","stepID":["--setup-job"],"msg":"x"}"#);
        p.feed(r#"{"step":"Set up job","stepResult":"success","executionTime":1000}"#);
        p.feed(r#"{"job":"CI/build","jobID":"build","step":"Complete job","stepid":["--complete-job"],"msg":"x"}"#);
        p.feed(r#"{"step":"Complete job","stepResult":"success","executionTime":1000}"#);
        assert!(p.substeps().is_empty(), "{:?}", p.substeps());
    }

    #[test]
    fn platform_defaults_per_os() {
        assert!(default_platforms(Os::Linux).is_empty());
        let win = default_platforms(Os::Windows);
        assert_eq!(
            win.get("windows-latest").map(String::as_str),
            Some(SELF_HOSTED)
        );
        assert_eq!(
            win.get("windows-2022").map(String::as_str),
            Some(SELF_HOSTED)
        );
        assert_eq!(
            win.get("windows-2025").map(String::as_str),
            Some(SELF_HOSTED)
        );
        assert!(!win.contains_key("macos-latest"));
        let mac = default_platforms(Os::Macos);
        assert_eq!(
            mac.get("macos-latest").map(String::as_str),
            Some(SELF_HOSTED)
        );
        assert_eq!(mac.get("macos-14").map(String::as_str), Some(SELF_HOSTED));
        assert_eq!(mac.get("macos-15").map(String::as_str), Some(SELF_HOSTED));
        assert!(!mac.contains_key("windows-latest"));
    }

    #[test]
    fn platform_override_order() {
        let act = ActCfg {
            platforms: BTreeMap::from([("ubuntu-latest".to_string(), "img".to_string())]),
            ..ActCfg::default()
        };
        let project = BTreeMap::from([("windows-latest".to_string(), "custom".to_string())]);
        let step = BTreeMap::from([("ubuntu-latest".to_string(), "step-img".to_string())]);
        let merged = resolve_platforms(Os::Windows, &act, &project, &step);
        assert_eq!(
            merged.get("ubuntu-latest").map(String::as_str),
            Some("step-img")
        );
        assert_eq!(
            merged.get("windows-latest").map(String::as_str),
            Some("custom")
        );
    }

    #[test]
    fn self_hosted_detection_from_workflow() {
        let wf = r#"
name: Flutter CI
jobs:
  ubuntu:
    name: Web
    runs-on: ubuntu-latest
  windows:
    name: Build Windows
    runs-on: windows-latest
  multi:
    runs-on: [self-hosted, windows, x64]
"#;
        let platforms = default_platforms(Os::Windows);
        assert!(!uses_self_hosted(
            &platforms,
            &job_runs_on(Some(wf), Some("ubuntu"))
        ));
        assert!(uses_self_hosted(
            &platforms,
            &job_runs_on(Some(wf), Some("windows"))
        ));
        assert!(uses_self_hosted(
            &platforms,
            &job_runs_on(Some(wf), Some("multi"))
        ));
        // Unknown job / unreadable workflow -> no false positive.
        assert!(!uses_self_hosted(
            &platforms,
            &job_runs_on(Some(wf), Some("nope"))
        ));
        assert!(!uses_self_hosted(
            &platforms,
            &job_runs_on(None, Some("windows"))
        ));
    }

    #[test]
    fn wsl_path_translation() {
        assert_eq!(
            wsl_path(Path::new(r"C:\Users\me\repo")),
            PathBuf::from("/mnt/c/Users/me/repo")
        );
        assert_eq!(wsl_path(Path::new(r"d:/x/y")), PathBuf::from("/mnt/d/x/y"));
        assert_eq!(
            wsl_path(Path::new("/home/me/repo")),
            PathBuf::from("/home/me/repo")
        );
        assert_eq!(
            wsl_path(Path::new("relative/path")),
            PathBuf::from("relative/path")
        );
        // UNC paths are left alone rather than mangled.
        assert_eq!(
            wsl_path(Path::new(r"\\server\share\x")),
            PathBuf::from(r"\\server\share\x")
        );
    }

    #[test]
    fn wsl_arg_translation() {
        let args = vec![
            "push".to_string(),
            "--json".to_string(),
            "-W".to_string(),
            r".github\workflows\ci.yml".to_string(),
            "-e".to_string(),
            r"C:\Temp\event.json".to_string(),
            "--secret-file".to_string(),
            r"C:\Temp\secrets".to_string(),
            r"--artifact-server-path=D:\state\artifacts".to_string(),
            "-P".to_string(),
            "ubuntu-latest=node:16".to_string(),
        ];
        let out = wsl_translate_args(&args);
        assert_eq!(out[3], ".github/workflows/ci.yml");
        assert_eq!(out[5], "/mnt/c/Temp/event.json");
        assert_eq!(out[7], "/mnt/c/Temp/secrets");
        assert_eq!(out[8], "--artifact-server-path=/mnt/d/state/artifacts");
        assert_eq!(out[10], "ubuntu-latest=node:16");
    }

    #[test]
    fn act_command_prefers_native_for_self_hosted_on_windows() {
        let act = ActCfg {
            command: Some(vec!["wsl.exe".into(), "-e".into(), "act".into()]),
            native_command: Some(vec!["act".into()]),
            ..ActCfg::default()
        };
        let chosen = act_command(&act, true);
        if cfg!(windows) {
            assert_eq!(chosen, vec!["act".to_string()]);
        } else {
            assert_eq!(chosen[0], "wsl.exe");
        }
        assert!(act.is_wsl());
    }

    #[test]
    fn event_json_shape() {
        let v: serde_json::Value =
            serde_json::from_str(&event_json("abc123", "master", Some("teppyboy/lunarum")))
                .unwrap();
        assert_eq!(v["after"], "abc123");
        assert_eq!(v["head_commit"]["id"], "abc123");
        assert_eq!(v["ref"], "refs/heads/master");
        assert_eq!(v["repository"]["full_name"], "teppyboy/lunarum");
        assert_eq!(v["repository"]["owner"]["login"], "teppyboy");
    }

    #[test]
    fn argv_shape() {
        let plan = ActPlan {
            event_file: PathBuf::from("/tmp/event.json"),
            secret_file: Some(PathBuf::from("/tmp/secrets")),
            var_file: None,
            artifact_dir: PathBuf::from("/tmp/art"),
            workflow: ".github/workflows/ci.yml".into(),
            job: Some("web".into()),
            event: "push".into(),
        };
        let step = StepCfg {
            name: "s".into(),
            kind: crate::config::StepKind::Act,
            run: None,
            workflow: Some(".github/workflows/ci.yml".into()),
            job: Some("web".into()),
            event: None,
            platforms: BTreeMap::new(),
            args: vec!["--pull=false".into()],
            secrets: BTreeMap::new(),
            vars: BTreeMap::new(),
            only_on: Vec::new(),
            timeout: None,
            cwd: None,
            shell: None,
            env: BTreeMap::new(),
        };
        let platforms = BTreeMap::from([("ubuntu-latest".to_string(), "img".to_string())]);
        let args = build_args(&plan, &step, &platforms, &ActCfg::default(), false);
        let joined = args.join(" ");
        assert!(
            joined.starts_with("push --json -W .github/workflows/ci.yml -j web -e /tmp/event.json")
        );
        assert!(joined.contains("-P ubuntu-latest=img"));
        assert!(joined.contains("--secret-file /tmp/secrets"));
        assert!(joined.contains("--artifact-server-path /tmp/art"));
        assert!(joined.ends_with("--pull=false"));
    }
}
