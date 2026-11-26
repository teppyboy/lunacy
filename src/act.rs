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
        "--action-cache-path",
        "--cache-server-path",
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
    /// "Pre" / "Main" / "Post"; absent on setup/teardown lines.
    #[serde(default)]
    stage: Option<String>,
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

    /// Mark any substeps still in `StepStatus::Running` with `status` (e.g. on cancel or timeout).
    pub fn finalize_running(&mut self, status: StepStatus) {
        for s in self.substeps.iter_mut() {
            if s.status == StepStatus::Running {
                s.status = status;
            }
        }
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

        // A composite action reports its inner steps under the outer step's name with a
        // longer stepID (`["1","0"]`), and Pre/Post stages reuse the step name. Only the
        // top-level Main step is a row, like the Actions UI; a failing Pre/Post still is.
        let nested = step_id.is_some_and(|ids| ids.len() > 1);
        let side_stage = matches!(parsed.stage.as_deref(), Some("Pre" | "Post"));
        let failed = parsed
            .step_result
            .as_deref()
            .is_some_and(|r| r != "success" && r != "skipped");
        let ignored = nested || (side_stage && !failed);

        if let Some(step_name) = parsed.step.as_ref().filter(|_| !ignored) {
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

/// Persistent named Docker cache volumes shared across all projects.
pub const SHARED_CACHE_VOLUMES: &[(&str, &str)] = &[
    ("lunacy-cache-cargo-registry", "/root/.cargo/registry"),
    ("lunacy-cache-cargo-git", "/root/.cargo/git"),
    ("lunacy-cache-rustup", "/root/.rustup"),
    ("lunacy-cache-pub", "/root/.pub-cache"),
    ("lunacy-cache-dart-server", "/root/.dartServer"),
    ("lunacy-cache-npm", "/root/.npm"),
    ("lunacy-cache-pnpm", "/root/.local/share/pnpm/store"),
    ("lunacy-cache-go", "/root/go/pkg/mod"),
    ("lunacy-cache-pip", "/root/.cache/pip"),
    ("lunacy-cache-uv", "/root/.cache/uv"),
    ("lunacy-cache-ccache", "/root/.cache/ccache"),
];

/// Default container env vars injected when aggressive caching is enabled.
pub const DEFAULT_CACHE_ENVS: &[(&str, &str)] = &[
    ("CARGO_TARGET_DIR", "/cache/cargo-target"),
    ("CARGO_INCREMENTAL", "0"),
    ("PUB_CACHE", "/root/.pub-cache"),
];

/// Project-scoped Docker cache volumes for a sanitized project slug.
pub fn project_cache_volumes(slug: &str) -> Vec<(String, String)> {
    vec![
        (
            format!("lunacy-cache-{slug}-cargo-target"),
            "/cache/cargo-target".to_string(),
        ),
        (
            format!("lunacy-cache-{slug}-build"),
            "/cache/build".to_string(),
        ),
    ]
}

/// Aggressive caching settings resolved for a single `act` invocation.
#[derive(Debug, Clone)]
pub struct ActCachePlan {
    pub action_cache_path: PathBuf,
    pub cache_server_path: PathBuf,
    pub project_slug: String,
    pub pull_false: bool,
    pub include_container_volumes: bool,
    pub project_env: BTreeMap<String, String>,
}

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
    pub project_args: Vec<String>,
    pub cache: Option<ActCachePlan>,
}

fn has_flag(args: &[String], flag: &str) -> bool {
    let prefix = format!("{flag}=");
    args.iter().any(|a| a == flag || a.starts_with(&prefix))
}

pub fn has_pull_flag(args: &[String]) -> bool {
    has_flag(args, "--pull") || has_flag(args, "-p")
}

fn args_has_env_key(args: &[String], key: &str) -> bool {
    let mut expect_val = false;
    let key_eq = format!("{key}=");
    for arg in args {
        if expect_val {
            expect_val = false;
            if arg == key || arg.starts_with(&key_eq) {
                return true;
            }
            continue;
        }
        if arg == "--env" {
            expect_val = true;
            continue;
        }
        if let Some(val) = arg.strip_prefix("--env=") {
            if val == key || val.starts_with(&key_eq) {
                return true;
            }
        }
    }
    false
}

/// Extract all `--container-options <val>` / `--container-options=<val>` occurrences from `args`,
/// returning `(remaining_args, extracted_option_strings)`.
fn extract_container_options(args: &[String]) -> (Vec<String>, Vec<String>) {
    let mut rest = Vec::with_capacity(args.len());
    let mut extracted = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if arg == "--container-options" {
            if i + 1 < args.len() {
                let val = args[i + 1].trim();
                if !val.is_empty() {
                    extracted.push(val.to_string());
                }
                i += 2;
                continue;
            }
        } else if let Some(val) = arg.strip_prefix("--container-options=") {
            let val = val.trim();
            if !val.is_empty() {
                extracted.push(val.to_string());
            }
            i += 1;
            continue;
        }
        rest.push(arg.clone());
        i += 1;
    }
    (rest, extracted)
}

/// Tokenize a `--container-options` string into whitespace-delimited shell-like tokens.
fn split_option_tokens(s: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    for ch in s.chars() {
        match quote {
            Some(q) if ch == q => quote = None,
            Some(_) => cur.push(ch),
            None if ch == '\'' || ch == '"' => quote = Some(ch),
            None if ch.is_whitespace() => {
                if !cur.is_empty() {
                    tokens.push(std::mem::take(&mut cur));
                }
            }
            None => cur.push(ch),
        }
    }
    if !cur.is_empty() {
        tokens.push(cur);
    }
    tokens
}

fn normalize_mount_path(p: &str) -> &str {
    let trimmed = p.trim_end_matches('/');
    if trimmed.is_empty() {
        "/"
    } else {
        trimmed
    }
}

/// Return true if a Docker `--container-options` string already mounts `target_path`.
fn container_opts_has_mount(opts: &str, target_path: &str) -> bool {
    let target = normalize_mount_path(target_path);
    let tokens = split_option_tokens(opts);
    let mut i = 0;
    while i < tokens.len() {
        let tok = &tokens[i];
        let vol_spec = if tok == "-v" || tok == "--volume" {
            i += 1;
            tokens.get(i).map(String::as_str)
        } else {
            tok.strip_prefix("--volume=")
                .or_else(|| tok.strip_prefix("-v="))
        };
        if let Some(spec) = vol_spec {
            let parts: Vec<&str> = spec.split(':').collect();
            let mount_dest = if parts.len() >= 3
                && parts[0].len() == 1
                && parts[0].chars().all(|c| c.is_ascii_alphabetic())
            {
                // Windows host path like `C:\path:/container/path`
                parts.get(2).copied()
            } else if parts.len() >= 2 {
                parts.get(1).copied()
            } else {
                parts.first().copied()
            };
            if let Some(dest) = mount_dest {
                if normalize_mount_path(dest) == target {
                    return true;
                }
            }
            i += 1;
            continue;
        }

        let mount_spec = if tok == "--mount" {
            i += 1;
            tokens.get(i).map(String::as_str)
        } else {
            tok.strip_prefix("--mount=")
        };
        if let Some(spec) = mount_spec {
            for part in spec.split(',') {
                for key in ["target=", "dst=", "destination="] {
                    if let Some(dest) = part.strip_prefix(key) {
                        if normalize_mount_path(dest) == target {
                            return true;
                        }
                    }
                }
            }
        }
        i += 1;
    }
    false
}

/// Merge existing user `--container-options` with cache `-v <vol>:<mount>` mounts,
/// skipping any mount whose container target path is already configured by the user.
pub fn merge_container_options(existing: &[String], volumes: &[(String, String)]) -> String {
    let mut parts: Vec<String> = existing
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect();
    let baseline = parts.join(" ");
    for (vol, mount) in volumes {
        if !container_opts_has_mount(&baseline, mount) {
            parts.push(format!("-v {vol}:{mount}"));
        }
    }
    parts.join(" ")
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

    let mut user_args: Vec<String> =
        Vec::with_capacity(act.args.len() + plan.project_args.len() + step.args.len());
    user_args.extend(act.args.iter().cloned());
    user_args.extend(plan.project_args.iter().cloned());
    user_args.extend(step.args.iter().cloned());

    if let Some(cache) = &plan.cache {
        if !has_flag(&user_args, "--use-new-action-cache") {
            args.push("--use-new-action-cache".into());
        }
        if !has_flag(&user_args, "--action-cache-path") {
            args.push("--action-cache-path".into());
            args.push(cache.action_cache_path.to_string_lossy().to_string());
        }
        if !has_flag(&user_args, "--action-offline-mode") {
            args.push("--action-offline-mode".into());
        }
        if !has_flag(&user_args, "--cache-server-path")
            && !has_flag(&user_args, "--no-cache-server")
        {
            args.push("--cache-server-path".into());
            args.push(cache.cache_server_path.to_string_lossy().to_string());
        }
        if cache.pull_false && !has_pull_flag(&user_args) {
            args.push("--pull=false".into());
        }

        let (remaining_user_args, existing_container_opts) = extract_container_options(&user_args);
        if cache.include_container_volumes {
            let mut volumes: Vec<(String, String)> = SHARED_CACHE_VOLUMES
                .iter()
                .map(|(v, m)| ((*v).to_string(), (*m).to_string()))
                .collect();
            volumes.extend(project_cache_volumes(&cache.project_slug));
            let merged = merge_container_options(&existing_container_opts, &volumes);
            if !merged.is_empty() {
                args.push("--container-options".into());
                args.push(merged);
            }
            for (k, v) in DEFAULT_CACHE_ENVS {
                if !args_has_env_key(&remaining_user_args, k)
                    && !step.env.contains_key(*k)
                    && !cache.project_env.contains_key(*k)
                {
                    args.push("--env".into());
                    args.push(format!("{k}={v}"));
                }
            }
        } else if !existing_container_opts.is_empty() {
            args.push("--container-options".into());
            args.push(existing_container_opts.join(" "));
        }

        args.extend(remaining_user_args);
    } else {
        args.extend(user_args);
    }

    if wsl {
        wsl_translate_args(&args)
    } else {
        args
    }
}

/// All-zero sha: GitHub's "no previous commit", e.g. the first push of a branch.
pub const ZERO_SHA: &str = "0000000000000000000000000000000000000000";

/// Choose the `before` sha of a push event.
///
/// A workflow that reads commit messages over `before..after` treats an all-zero
/// `before` as "walk from the repo root", which re-checks the whole history, so
/// prefer the last sha lunacy processed when it is an ancestor of `after`, then
/// the commit's first parent, and only then zeros (a root commit).
pub fn pick_before(
    last: Option<&str>,
    last_is_ancestor: bool,
    first_parent: Option<&str>,
) -> String {
    if let Some(last) = last {
        if last_is_ancestor {
            return last.to_string();
        }
    }
    match first_parent {
        Some(parent) => parent.to_string(),
        None => ZERO_SHA.to_string(),
    }
}

/// The `event.json` body act expects for push events; the same shape lunarum's deploy worker builds.
#[allow(clippy::too_many_arguments)]
pub fn event_json(
    sha: &str,
    before: &str,
    branch: &str,
    repo: Option<&str>,
    message: &str,
    pusher: (&str, &str),
) -> String {
    let repo = repo.unwrap_or("local/repo");
    let (owner, name) = repo.split_once('/').unwrap_or(("local", repo));
    let (pusher_name, pusher_email) = pusher;
    let full_ref = if let Some(tag) = branch.strip_prefix("refs/tags/") {
        format!("refs/tags/{tag}")
    } else if let Some(tag) = branch.strip_prefix("tags/") {
        format!("refs/tags/{tag}")
    } else if branch.starts_with("refs/heads/") {
        branch.to_string()
    } else {
        format!("refs/heads/{branch}")
    };
    let head = serde_json::json!({
        "id": sha,
        "message": message,
        "author": { "name": pusher_name, "email": pusher_email },
        "committer": { "name": pusher_name, "email": pusher_email },
    });
    serde_json::json!({
        "after": sha,
        "before": before,
        "ref": full_ref,
        "repository": {
            "full_name": repo,
            "name": name,
            "owner": { "login": owner },
            "default_branch": branch,
        },
        "head_commit": head,
        "commits": [head],
        "pusher": { "name": pusher_name, "email": pusher_email },
        "sender": { "login": owner, "type": "User" },
    })
    .to_string()
}

/// The `event.json` body act expects for `pull_request` events.
#[allow(clippy::too_many_arguments)]
pub fn pull_request_event_json(
    number: u64,
    title: &str,
    head_sha: &str,
    head_ref: &str,
    base_sha: &str,
    base_ref: &str,
    default_branch: &str,
    repo: Option<&str>,
) -> String {
    let repo = repo.unwrap_or("local/repo");
    let (owner, name) = repo.split_once('/').unwrap_or(("local", repo));
    serde_json::json!({
        "action": "synchronize",
        "number": number,
        "pull_request": {
            "number": number,
            "title": title,
            "head": {
                "sha": head_sha,
                "ref": head_ref,
            },
            "base": {
                "sha": base_sha,
                "ref": base_ref,
            },
        },
        "repository": {
            "full_name": repo,
            "name": name,
            "owner": { "login": owner },
            "default_branch": default_branch,
        },
        "sender": { "login": owner, "type": "User" },
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
    fn composite_and_pre_post_steps_are_one_row() {
        // Recorded from act 0.2.97: a composite action (`Set up thing`) with two inner
        // steps, plus its Post stage; lunarum's `Set up Flutter` showed up six times.
        let lines = [
            r#"{"job":"T/b","jobID":"b","stage":"Pre","step":"Set up thing","stepID":["1"],"msg":"⭐ Run Pre Set up thing"}"#,
            r#"{"job":"T/b","jobID":"b","stage":"Pre","step":"Set up thing","stepID":["1"],"stepResult":"success","executionTime":1000000}"#,
            r#"{"job":"T/b","jobID":"b","stage":"Main","step":"Set up thing","stepID":["1"],"msg":"⭐ Run Main Set up thing"}"#,
            r#"{"job":"T/b","jobID":"b","stage":"Main","step":"Set up thing","stepID":["1","0"],"msg":"⭐ Run Main Inner one"}"#,
            r#"{"job":"T/b","jobID":"b","stage":"Main","step":"Set up thing","stepID":["1","0"],"stepResult":"success","executionTime":5000000}"#,
            r#"{"job":"T/b","jobID":"b","stage":"Main","step":"Set up thing","stepID":["1","1"],"msg":"⭐ Run Main Inner two"}"#,
            r#"{"job":"T/b","jobID":"b","stage":"Main","step":"Set up thing","stepID":["1","1"],"stepResult":"success","executionTime":5000000}"#,
            r#"{"job":"T/b","jobID":"b","stage":"Main","step":"Set up thing","stepID":["1"],"stepResult":"success","executionTime":28000000}"#,
            r#"{"job":"T/b","jobID":"b","stage":"Main","step":"After","stepID":["2"],"msg":"⭐ Run Main After"}"#,
            r#"{"job":"T/b","jobID":"b","stage":"Main","step":"After","stepID":["2"],"stepResult":"success","executionTime":6000000}"#,
            r#"{"job":"T/b","jobID":"b","stage":"Post","step":"Set up thing","stepID":["1"],"msg":"⭐ Run Post Set up thing"}"#,
            r#"{"job":"T/b","jobID":"b","stage":"Post","step":"Set up thing","stepID":["1"],"stepResult":"success","executionTime":20000}"#,
        ];
        let mut p = ActProgress::new();
        for l in lines {
            p.feed(l);
        }
        let rows: Vec<_> = p
            .substeps()
            .into_iter()
            .map(|s| (s.name, s.status, s.duration_ms))
            .collect();
        assert_eq!(
            rows,
            vec![
                ("Set up thing".to_string(), StepStatus::Success, Some(28)),
                ("After".to_string(), StepStatus::Success, Some(6)),
            ]
        );
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
            "--action-cache-path".to_string(),
            r"D:\state\cache\act-actions".to_string(),
            r"--cache-server-path=D:\state\cache\act-cache-server".to_string(),
            "-P".to_string(),
            "ubuntu-latest=node:16".to_string(),
        ];
        let out = wsl_translate_args(&args);
        assert_eq!(out[3], ".github/workflows/ci.yml");
        assert_eq!(out[5], "/mnt/c/Temp/event.json");
        assert_eq!(out[7], "/mnt/c/Temp/secrets");
        assert_eq!(out[8], "--artifact-server-path=/mnt/d/state/artifacts");
        assert_eq!(out[10], "/mnt/d/state/cache/act-actions");
        assert_eq!(
            out[11],
            "--cache-server-path=/mnt/d/state/cache/act-cache-server"
        );
        assert_eq!(out[13], "ubuntu-latest=node:16");
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
        let v: serde_json::Value = serde_json::from_str(&event_json(
            "abc123",
            "def456",
            "master",
            Some("teppyboy/lunarum"),
            "feat: a thing",
            ("Teppy", "teppy@example.invalid"),
        ))
        .unwrap();
        assert_eq!(v["after"], "abc123");
        assert_eq!(v["before"], "def456");
        assert_eq!(v["head_commit"]["id"], "abc123");
        assert_eq!(v["head_commit"]["message"], "feat: a thing");
        assert_eq!(v["commits"][0]["id"], "abc123");
        assert_eq!(v["ref"], "refs/heads/master");
        assert_eq!(v["repository"]["full_name"], "teppyboy/lunarum");
        assert_eq!(v["repository"]["owner"]["login"], "teppyboy");
        assert_eq!(v["pusher"]["email"], "teppy@example.invalid");
        assert_eq!(v["sender"]["login"], "teppyboy");
    }

    #[test]
    fn before_prefers_the_last_processed_ancestor() {
        // A known ancestor wins: the range is exactly the new commits.
        assert_eq!(
            pick_before(Some("aaaa"), true, Some("bbbb")),
            "aaaa".to_string()
        );
        // Not an ancestor (force push, or another branch): fall back to the parent.
        assert_eq!(
            pick_before(Some("aaaa"), false, Some("bbbb")),
            "bbbb".to_string()
        );
        // Nothing processed yet: the first parent still gives a one-commit range.
        assert_eq!(pick_before(None, false, Some("bbbb")), "bbbb".to_string());
        // Root commit: zeros, which is what GitHub sends for a new branch.
        assert_eq!(pick_before(None, false, None), ZERO_SHA.to_string());
        assert_eq!(pick_before(Some("aaaa"), false, None), ZERO_SHA.to_string());
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
            project_args: Vec::new(),
            cache: None,
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

    #[test]
    fn aggressive_cache_injects_flags_volumes_and_envs_cleanly() {
        let plan = ActPlan {
            event_file: PathBuf::from("/tmp/event.json"),
            secret_file: None,
            var_file: None,
            artifact_dir: PathBuf::from("/tmp/art"),
            workflow: ".github/workflows/ci.yml".into(),
            job: Some("validate".into()),
            event: "push".into(),
            project_args: Vec::new(),
            cache: Some(ActCachePlan {
                action_cache_path: PathBuf::from("/state/cache/act-actions"),
                cache_server_path: PathBuf::from("/state/cache/act-cache-server"),
                project_slug: "frostedge".into(),
                pull_false: true,
                include_container_volumes: true,
                project_env: BTreeMap::from([("CARGO_INCREMENTAL".to_string(), "1".to_string())]),
            }),
        };
        let step = StepCfg {
            name: "Validate".into(),
            kind: crate::config::StepKind::Act,
            run: None,
            workflow: Some(".github/workflows/ci.yml".into()),
            job: Some("validate".into()),
            event: None,
            platforms: BTreeMap::new(),
            args: vec![
                "--container-options".into(),
                "--shm-size=2g -v custom-cargo:/root/.cargo/registry".into(),
                "--env".into(),
                "PUB_CACHE=/custom/pub".into(),
            ],
            secrets: BTreeMap::new(),
            vars: BTreeMap::new(),
            only_on: Vec::new(),
            timeout: None,
            cwd: None,
            shell: None,
            env: BTreeMap::new(),
        };
        let act = ActCfg {
            args: vec!["--container-options=--add-host=host.docker.internal:host-gateway".into()],
            ..ActCfg::default()
        };
        let platforms = BTreeMap::from([("ubuntu-latest".to_string(), "img".to_string())]);
        let args = build_args(&plan, &step, &platforms, &act, false);

        // Action cache, offline mode, cache server, and pull=false injected.
        assert!(args.contains(&"--use-new-action-cache".to_string()));
        assert!(args.contains(&"--action-offline-mode".to_string()));
        assert!(args.contains(&"--pull=false".to_string()));
        let acp_idx = args
            .iter()
            .position(|a| a == "--action-cache-path")
            .expect("--action-cache-path");
        assert_eq!(args[acp_idx + 1], "/state/cache/act-actions");
        let csp_idx = args
            .iter()
            .position(|a| a == "--cache-server-path")
            .expect("--cache-server-path");
        assert_eq!(args[csp_idx + 1], "/state/cache/act-cache-server");

        // Exactly ONE --container-options flag containing both user options AND cache mounts,
        // without duplicating /root/.cargo/registry!
        let co_indices: Vec<usize> = args
            .iter()
            .enumerate()
            .filter(|(_, a)| *a == "--container-options" || a.starts_with("--container-options="))
            .map(|(i, _)| i)
            .collect();
        assert_eq!(
            co_indices.len(),
            1,
            "must emit single --container-options: {args:?}"
        );
        let co_val = &args[co_indices[0] + 1];
        assert!(
            co_val.contains("--add-host=host.docker.internal:host-gateway"),
            "{co_val}"
        );
        assert!(co_val.contains("--shm-size=2g"), "{co_val}");
        assert!(
            co_val.contains("-v custom-cargo:/root/.cargo/registry"),
            "{co_val}"
        );
        assert!(
            !co_val.contains("lunacy-cache-cargo-registry:/root/.cargo/registry"),
            "must not duplicate already-mounted target: {co_val}"
        );
        assert!(
            co_val.contains("-v lunacy-cache-cargo-git:/root/.cargo/git"),
            "{co_val}"
        );
        assert!(
            co_val.contains("-v lunacy-cache-npm:/root/.npm"),
            "{co_val}"
        );
        assert!(
            co_val.contains("-v lunacy-cache-frostedge-cargo-target:/cache/cargo-target"),
            "{co_val}"
        );
        assert!(
            co_val.contains("-v lunacy-cache-frostedge-build:/cache/build"),
            "{co_val}"
        );

        // CARGO_TARGET_DIR auto-injected; CARGO_INCREMENTAL skipped (in project_env);
        // PUB_CACHE skipped (already in step.args).
        let joined = args.join(" ");
        assert!(
            joined.contains("--env CARGO_TARGET_DIR=/cache/cargo-target"),
            "{joined}"
        );
        assert!(!joined.contains("CARGO_INCREMENTAL=0"), "{joined}");
        assert!(!joined.contains("PUB_CACHE=/root/.pub-cache"), "{joined}");
        assert!(joined.contains("--env PUB_CACHE=/custom/pub"), "{joined}");
    }

    #[test]
    fn cache_flags_do_not_duplicate_user_overrides() {
        let plan = ActPlan {
            event_file: PathBuf::from("/tmp/event.json"),
            secret_file: None,
            var_file: None,
            artifact_dir: PathBuf::from("/tmp/art"),
            workflow: ".github/workflows/ci.yml".into(),
            job: None,
            event: "push".into(),
            project_args: Vec::new(),
            cache: Some(ActCachePlan {
                action_cache_path: PathBuf::from("/state/cache/act-actions"),
                cache_server_path: PathBuf::from("/state/cache/act-cache-server"),
                project_slug: "demo".into(),
                pull_false: true,
                include_container_volumes: false,
                project_env: BTreeMap::new(),
            }),
        };
        let step = StepCfg {
            name: "S".into(),
            kind: crate::config::StepKind::Act,
            run: None,
            workflow: Some(".github/workflows/ci.yml".into()),
            job: None,
            event: None,
            platforms: BTreeMap::new(),
            args: vec![
                "--action-cache-path=/custom/actions".into(),
                "--no-cache-server".into(),
                "--pull=true".into(),
                "--action-offline-mode=false".into(),
                "--use-new-action-cache".into(),
            ],
            secrets: BTreeMap::new(),
            vars: BTreeMap::new(),
            only_on: Vec::new(),
            timeout: None,
            cwd: None,
            shell: None,
            env: BTreeMap::from([("CARGO_TARGET_DIR".to_string(), "/custom/target".to_string())]),
        };
        let args = build_args(&plan, &step, &BTreeMap::new(), &ActCfg::default(), false);
        assert_eq!(
            args.iter()
                .filter(|a| a.starts_with("--action-cache-path"))
                .count(),
            1
        );
        assert!(!args.iter().any(|a| a.starts_with("--cache-server-path")));
        assert_eq!(args.iter().filter(|a| a.starts_with("--pull")).count(), 1);
        assert_eq!(
            args.iter()
                .filter(|a| a.starts_with("--action-offline-mode"))
                .count(),
            1
        );
        assert_eq!(
            args.iter()
                .filter(|a| *a == "--use-new-action-cache")
                .count(),
            1
        );
        assert!(!args
            .iter()
            .any(|a| a.contains("CARGO_TARGET_DIR=/cache/cargo-target")));
    }

    #[test]
    fn self_hosted_cache_omits_container_volumes_and_container_envs() {
        let plan = ActPlan {
            event_file: PathBuf::from("/tmp/event.json"),
            secret_file: None,
            var_file: None,
            artifact_dir: PathBuf::from("/tmp/art"),
            workflow: ".github/workflows/ci.yml".into(),
            job: Some("windows".into()),
            event: "push".into(),
            project_args: Vec::new(),
            cache: Some(ActCachePlan {
                action_cache_path: PathBuf::from("/state/cache/act-actions"),
                cache_server_path: PathBuf::from("/state/cache/act-cache-server"),
                project_slug: "demo".into(),
                pull_false: false,
                include_container_volumes: false,
                project_env: BTreeMap::new(),
            }),
        };
        let step = StepCfg {
            name: "WinBuild".into(),
            kind: crate::config::StepKind::Act,
            run: None,
            workflow: Some(".github/workflows/ci.yml".into()),
            job: Some("windows".into()),
            event: None,
            platforms: BTreeMap::new(),
            args: Vec::new(),
            secrets: BTreeMap::new(),
            vars: BTreeMap::new(),
            only_on: Vec::new(),
            timeout: None,
            cwd: None,
            shell: None,
            env: BTreeMap::new(),
        };
        let platforms = BTreeMap::from([("windows-latest".to_string(), SELF_HOSTED.to_string())]);
        let args = build_args(&plan, &step, &platforms, &ActCfg::default(), false);
        let joined = args.join(" ");

        assert!(args.contains(&"--use-new-action-cache".to_string()));
        assert!(args.contains(&"--action-cache-path".to_string()));
        assert!(args.contains(&"--cache-server-path".to_string()));
        assert!(
            !joined.contains("--container-options"),
            "self-hosted should not inject docker volumes: {joined}"
        );
        assert!(
            !joined.contains("/cache/cargo-target"),
            "self-hosted must not receive container CARGO_TARGET_DIR: {joined}"
        );
        assert!(
            !joined.contains("CARGO_INCREMENTAL=0"),
            "self-hosted must not receive container CARGO_INCREMENTAL: {joined}"
        );
        assert!(
            !joined.contains("/root/.pub-cache"),
            "self-hosted must not receive container PUB_CACHE: {joined}"
        );
    }

    #[test]
    fn pull_request_event_json_shape() {
        let raw = pull_request_event_json(
            77,
            "feat: pr ci",
            "aaaa1111",
            "feat/pr-ci",
            "bbbb2222",
            "master",
            "master",
            Some("teppyboy/frostedge"),
        );
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(v["action"], "synchronize");
        assert_eq!(v["number"], 77);
        assert_eq!(v["pull_request"]["number"], 77);
        assert_eq!(v["pull_request"]["title"], "feat: pr ci");
        assert_eq!(v["pull_request"]["head"]["sha"], "aaaa1111");
        assert_eq!(v["pull_request"]["head"]["ref"], "feat/pr-ci");
        assert_eq!(v["pull_request"]["base"]["sha"], "bbbb2222");
        assert_eq!(v["pull_request"]["base"]["ref"], "master");
        assert_eq!(v["repository"]["full_name"], "teppyboy/frostedge");
    }

    #[test]
    fn act_args_order_is_global_then_project_then_step() {
        let plan = ActPlan {
            event_file: PathBuf::from("/tmp/event.json"),
            secret_file: None,
            var_file: None,
            artifact_dir: PathBuf::from("/tmp/art"),
            workflow: ".github/workflows/ci.yml".into(),
            job: Some("build".into()),
            event: "push".into(),
            project_args: vec!["--project-flag".into(), "p1".into()],
            cache: None,
        };
        let act = ActCfg {
            args: vec!["--global-flag".into(), "g1".into()],
            ..ActCfg::default()
        };
        let step = StepCfg {
            name: "s".into(),
            kind: crate::config::StepKind::Act,
            run: None,
            workflow: Some(".github/workflows/ci.yml".into()),
            job: Some("build".into()),
            event: None,
            platforms: BTreeMap::new(),
            args: vec!["--step-flag".into(), "s1".into()],
            secrets: BTreeMap::new(),
            vars: BTreeMap::new(),
            only_on: Vec::new(),
            timeout: None,
            cwd: None,
            shell: None,
            env: BTreeMap::new(),
        };
        let args = build_args(&plan, &step, &BTreeMap::new(), &act, false);
        let g = args.iter().position(|a| a == "--global-flag").unwrap();
        let p = args.iter().position(|a| a == "--project-flag").unwrap();
        let s = args.iter().position(|a| a == "--step-flag").unwrap();
        assert!(g < p && p < s, "expected global < project < step: {args:?}");
    }
}
