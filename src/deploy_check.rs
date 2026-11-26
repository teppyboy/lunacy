//! Local deploy-readiness diagnostics for `GET /api/deploy/checks`.
//!
//! Strictly local and fast: never runs `git fetch` or makes network calls.

use crate::act;
use crate::config::{
    self, Checkout, Config, GithubMode, Os, ProjectCfg, StepCfg, StepKind, TriggerMode,
    DEFAULT_STEP_TIMEOUT,
};
use crate::daemon::ProjectState;
use crate::exec;
use crate::git;
use crate::model::{self, Phase, RunStatus, StepStatus};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Command, Stdio};

#[derive(Debug, Clone, Serialize)]
pub struct DeployChecksResponse {
    pub os: String,
    pub default_step_timeout: u64,
    pub github_enabled: bool,
    pub github_mode: String,
    pub discord_global: bool,
    pub cache_enabled: bool,
    pub cache_dir: String,
    pub projects: Vec<ProjectDeployCheck>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProjectDeployCheck {
    pub name: String,
    pub repo_path: String,
    pub remote: String,
    pub branch: String,
    pub checkout: String,
    pub github_repo: Option<String>,
    pub poll_interval: u64,
    pub trigger_mode: String,
    pub pipeline_timeout: Option<u64>,
    pub step_timeout: u64,
    pub watch_prs: bool,
    pub deploy_refs: Vec<String>,
    pub cache_enabled: bool,
    pub overall: String,
    pub git: GitCheck,
    pub reporting: ReportingCheck,
    pub ci_active_steps: usize,
    pub ci_total_steps: usize,
    pub deploy_active_steps: usize,
    pub deploy_total_steps: usize,
    pub deploy_steps: Vec<DeployStepCheck>,
    pub checks: Vec<CheckItem>,
    pub last_deploy: Option<LastDeploySummary>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GitCheck {
    pub repo_exists: bool,
    pub is_git_repo: bool,
    pub remote_ok: bool,
    pub remote_url: Option<String>,
    pub dirty: Option<bool>,
    pub head_sha: Option<String>,
    pub upstream_sha: Option<String>,
    pub last_sha: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReportingCheck {
    pub github_enabled: bool,
    pub ci_context: String,
    pub deploy_context: String,
    pub discord_source: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeployStepCheck {
    pub name: String,
    pub kind: String,
    pub applies_to_host: bool,
    pub only_on: Vec<String>,
    pub effective_timeout: u64,
    pub command_summary: String,
    pub checks: Vec<CheckItem>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CheckItem {
    pub level: String,
    pub label: String,
    pub detail: String,
}

impl CheckItem {
    fn ok(label: impl Into<String>, detail: impl Into<String>) -> CheckItem {
        CheckItem {
            level: "ok".to_string(),
            label: label.into(),
            detail: detail.into(),
        }
    }

    fn warn(label: impl Into<String>, detail: impl Into<String>) -> CheckItem {
        CheckItem {
            level: "warn".to_string(),
            label: label.into(),
            detail: detail.into(),
        }
    }

    fn error(label: impl Into<String>, detail: impl Into<String>) -> CheckItem {
        CheckItem {
            level: "error".to_string(),
            label: label.into(),
            detail: detail.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct LastDeploySummary {
    pub run_id: String,
    pub sha: String,
    pub status: RunStatus,
    pub finished_at: Option<u64>,
    pub steps: Vec<LastDeployStep>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LastDeployStep {
    pub name: String,
    pub status: StepStatus,
    pub duration_ms: Option<u64>,
}

/// Snapshot of runtime state for one project needed to render deploy checks
/// without holding the `Shared` mutex while inspecting the filesystem.
pub struct ProjectSnapshot {
    pub name: String,
    pub upstream_sha: Option<String>,
    pub last_sha: Option<String>,
    pub current_run: Option<model::Run>,
    pub last_run: Option<model::Run>,
}

impl From<&ProjectState> for ProjectSnapshot {
    fn from(p: &ProjectState) -> Self {
        ProjectSnapshot {
            name: p.name.clone(),
            upstream_sha: p.upstream_sha.clone(),
            last_sha: p.last_sha.clone(),
            current_run: p.current_run.clone(),
            last_run: p.last_run.clone(),
        }
    }
}

pub fn build_deploy_checks(
    cfg: &Config,
    snapshots: &[ProjectSnapshot],
    state_dir: &Path,
    os: Os,
) -> DeployChecksResponse {
    let default_step_timeout = cfg.general.step_timeout.unwrap_or(DEFAULT_STEP_TIMEOUT);
    let github_enabled = cfg.github.as_ref().is_some_and(|g| g.enabled());
    let github_mode = match cfg
        .github
        .as_ref()
        .map(|g| g.effective_mode())
        .unwrap_or(GithubMode::Status)
    {
        GithubMode::Checks => "checks".to_string(),
        GithubMode::Status => "status".to_string(),
    };
    let discord_global = cfg
        .discord
        .as_ref()
        .and_then(|d| d.webhook_url_value())
        .is_some();

    let mut docker_cache: BTreeMap<bool, Result<String, String>> = BTreeMap::new();
    let projects = cfg
        .projects
        .iter()
        .map(|project| {
            let snap = snapshots.iter().find(|s| s.name == project.name);
            check_project(
                cfg,
                project,
                snap,
                state_dir,
                os,
                github_enabled,
                discord_global,
                &mut docker_cache,
            )
        })
        .collect();

    let act_cfg = cfg.act();
    let cache_enabled = act_cfg.cache;
    let cache_dir = act_cfg
        .effective_cache_dir(state_dir)
        .to_string_lossy()
        .to_string();

    DeployChecksResponse {
        os: os.as_str().to_string(),
        default_step_timeout,
        github_enabled,
        github_mode,
        discord_global,
        cache_enabled,
        cache_dir,
        projects,
    }
}

#[allow(clippy::too_many_arguments)]
fn check_project(
    cfg: &Config,
    project: &ProjectCfg,
    snap: Option<&ProjectSnapshot>,
    state_dir: &Path,
    os: Os,
    github_enabled: bool,
    discord_global: bool,
    docker_cache: &mut BTreeMap<bool, Result<String, String>>,
) -> ProjectDeployCheck {
    let repo_path = &project.repo_path;
    let repo_exists = repo_path.is_dir();
    let is_git_repo = repo_exists && is_git_work_tree(repo_path);
    let remote_url = if is_git_repo {
        git::remote_url(repo_path, &project.remote)
    } else {
        None
    };
    let remote_ok = remote_url.is_some();
    let dirty = if is_git_repo {
        git::is_dirty(repo_path).ok()
    } else {
        None
    };
    let head_sha = if is_git_repo {
        git::rev_parse(repo_path, "HEAD").ok()
    } else {
        None
    };
    let upstream_sha = if is_git_repo {
        git::upstream_sha(repo_path, &project.remote, &project.branch)
            .ok()
            .or_else(|| snap.and_then(|s| s.upstream_sha.clone()))
    } else {
        snap.and_then(|s| s.upstream_sha.clone())
    };
    let last_sha = snap
        .and_then(|s| s.last_sha.clone())
        .or_else(|| model::read_last_sha(state_dir, &project.name));

    let github_repo = project
        .github_repo
        .clone()
        .or_else(|| remote_url.as_deref().and_then(config::repo_from_remote));

    let checkout_str = match project.checkout {
        Checkout::None => "none",
        Checkout::Worktree => "worktree",
        Checkout::InPlace => "in-place",
    };

    let ci_total_steps = project.ci.len();
    let ci_active_steps = project.ci.iter().filter(|s| s.applies_to(os)).count();
    let deploy_total_steps = project.deploy.len();
    let deploy_active_steps = project.deploy.iter().filter(|s| s.applies_to(os)).count();

    let mut checks = Vec::new();

    // 1. Repository path & git check.
    if !repo_exists {
        checks.push(CheckItem::error(
            "Repository",
            format!("path does not exist: {}", repo_path.display()),
        ));
    } else if !is_git_repo {
        checks.push(CheckItem::error(
            "Repository",
            format!("not a git repository: {}", repo_path.display()),
        ));
    } else {
        checks.push(CheckItem::ok(
            "Repository",
            format!("git repo at {}", repo_path.display()),
        ));
    }

    // 2. Remote URL & github_repo resolution.
    if is_git_repo {
        if !remote_ok {
            checks.push(CheckItem::error(
                "Remote",
                format!("remote '{}' is not configured", project.remote),
            ));
        } else if github_enabled && github_repo.is_none() {
            checks.push(CheckItem::warn(
                "Remote",
                format!(
                    "remote '{}' ({}) does not resolve to a GitHub owner/repo",
                    project.remote,
                    remote_url.as_deref().unwrap_or("")
                ),
            ));
        } else {
            let detail = match &github_repo {
                Some(repo) => format!("remote '{}' -> {repo}", project.remote),
                None => format!(
                    "remote '{}' -> {}",
                    project.remote,
                    remote_url.as_deref().unwrap_or("")
                ),
            };
            checks.push(CheckItem::ok("Remote", detail));
        }
    } else if github_enabled && github_repo.is_none() {
        checks.push(CheckItem::warn(
            "GitHub repo",
            "GitHub reporting is enabled but github_repo could not be resolved",
        ));
    }

    // 3. Working tree cleanliness.
    if is_git_repo {
        match (dirty, project.checkout) {
            (Some(true), Checkout::InPlace) => checks.push(CheckItem::error(
                "Worktree",
                "working tree is dirty; checkout = \"in-place\" will refuse to fast-forward",
            )),
            (Some(true), Checkout::None) => checks.push(CheckItem::warn(
                "Worktree",
                "working tree has uncommitted changes (checkout = \"none\")",
            )),
            (Some(true), Checkout::Worktree) => checks.push(CheckItem::ok(
                "Worktree",
                "working tree has local changes, but checkout = \"worktree\" runs isolated",
            )),
            (Some(false), _) => checks.push(CheckItem::ok("Worktree", "working tree is clean")),
            (None, _) => {}
        }
    }

    // 4. Deploy step host applicability.
    if deploy_total_steps == 0 {
        checks.push(CheckItem::ok(
            "Deploy steps",
            "no deploy steps configured (CI-only project)",
        ));
    } else if deploy_active_steps == 0 {
        checks.push(CheckItem::warn(
            "Deploy steps",
            format!(
                "{deploy_total_steps} deploy step(s) configured, but none apply to {}",
                os.as_str()
            ),
        ));
    } else {
        checks.push(CheckItem::ok(
            "Deploy steps",
            format!(
                "{deploy_active_steps}/{deploy_total_steps} deploy step(s) active on {}",
                os.as_str()
            ),
        ));
    }

    // 5. $LUNACY_CI_DIR coupling check.
    let active_deploy_uses_ci_dir = project
        .deploy
        .iter()
        .filter(|s| s.applies_to(os))
        .any(step_references_ci_dir);
    let has_active_act_ci = project
        .ci
        .iter()
        .any(|s| s.applies_to(os) && s.kind == StepKind::Act);
    if active_deploy_uses_ci_dir && !has_active_act_ci {
        checks.push(CheckItem::warn(
            "LUNACY_CI_DIR",
            format!(
                "active deploy step references LUNACY_CI_DIR, but no active act CI step runs on {}",
                os.as_str()
            ),
        ));
    }

    // 6. Webhook readiness when trigger_mode = "webhook".
    if project.effective_trigger_mode(&cfg.general) == TriggerMode::Webhook {
        let has_secret = cfg
            .github
            .as_ref()
            .and_then(|g| g.webhook_secret_value())
            .is_some();
        let resolved_repo = crate::daemon::resolve_github_repo(project);
        if !has_secret {
            checks.push(CheckItem::error(
                "Webhook",
                "webhook secret is not configured",
            ));
        } else if resolved_repo.is_none() {
            checks.push(CheckItem::error(
                "Webhook",
                "cannot determine GitHub repo for webhook matching",
            ));
        } else {
            checks.push(CheckItem::ok(
                "Webhook",
                format!(
                    "webhook secret configured for {}",
                    resolved_repo.as_deref().unwrap_or("")
                ),
            ));
        }
    }

    // Per-deploy-step checks.
    let deploy_steps: Vec<DeployStepCheck> = project
        .deploy
        .iter()
        .map(|step| check_deploy_step(cfg, project, step, os, docker_cache))
        .collect();

    let overall = compute_overall(&checks, &deploy_steps);

    let discord_source = if project
        .discord_webhook_url
        .as_deref()
        .is_some_and(|u| !u.trim().is_empty())
    {
        "project"
    } else if discord_global {
        "global"
    } else {
        "none"
    };

    let reporting = ReportingCheck {
        github_enabled: github_enabled && github_repo.is_some(),
        ci_context: project.contexts.ci.clone(),
        deploy_context: project.contexts.deploy.clone(),
        discord_source: discord_source.to_string(),
    };

    let last_deploy = resolve_last_deploy(snap, state_dir, &project.name);
    let cache_enabled = project.cache.unwrap_or_else(|| cfg.act().cache);

    ProjectDeployCheck {
        name: project.name.clone(),
        repo_path: repo_path.to_string_lossy().to_string(),
        remote: project.remote.clone(),
        branch: project.branch.clone(),
        checkout: checkout_str.to_string(),
        github_repo,
        poll_interval: project.poll_interval(&cfg.general),
        trigger_mode: project
            .effective_trigger_mode(&cfg.general)
            .as_str()
            .to_string(),
        pipeline_timeout: project.timeout,
        step_timeout: project.effective_step_timeout(&cfg.general),
        watch_prs: project.watch_prs,
        deploy_refs: project.deploy_refs.clone(),
        cache_enabled,
        overall,
        git: GitCheck {
            repo_exists,
            is_git_repo,
            remote_ok,
            remote_url,
            dirty,
            head_sha,
            upstream_sha,
            last_sha,
        },
        reporting,
        ci_active_steps,
        ci_total_steps,
        deploy_active_steps,
        deploy_total_steps,
        deploy_steps,
        checks,
        last_deploy,
    }
}

fn check_deploy_step(
    cfg: &Config,
    project: &ProjectCfg,
    step: &StepCfg,
    os: Os,
    docker_cache: &mut BTreeMap<bool, Result<String, String>>,
) -> DeployStepCheck {
    let applies_to_host = step.applies_to(os);
    let kind = match step.kind {
        StepKind::Shell => "shell",
        StepKind::Act => "act",
    };
    let effective_timeout =
        config::resolve_step_timeout(&cfg.general, Some(project), step).as_secs();
    let only_on: Vec<String> = step
        .only_on
        .iter()
        .map(|o| o.as_str().to_string())
        .collect();
    let command_summary = summarize_step(step);
    let mut checks = Vec::new();

    if !applies_to_host {
        checks.push(CheckItem::ok(
            "Host",
            format!(
                "skipped on {} (only_on: {})",
                os.as_str(),
                only_on.join(", ")
            ),
        ));
        return DeployStepCheck {
            name: step.name.clone(),
            kind: kind.to_string(),
            applies_to_host,
            only_on,
            effective_timeout,
            command_summary,
            checks,
        };
    }

    match step.kind {
        StepKind::Shell => {
            let shell = config::resolve_shell(&cfg.general, Some(project), Some(step), os);
            if let Some(program) = shell.first() {
                if binary_exists(program) {
                    checks.push(CheckItem::ok("Shell", format!("{program} available")));
                } else {
                    checks.push(CheckItem::error(
                        "Shell",
                        format!("shell binary '{program}' not found on PATH"),
                    ));
                }
            }

            let step_cwd = match &step.cwd {
                Some(c) => {
                    let expanded = config::expand_tilde(c);
                    let resolved = if expanded.is_absolute() {
                        expanded
                    } else {
                        project.repo_path.join(expanded)
                    };
                    if resolved.is_dir() {
                        checks.push(CheckItem::ok(
                            "Working dir",
                            format!("{} exists", resolved.display()),
                        ));
                    } else {
                        checks.push(CheckItem::error(
                            "Working dir",
                            format!("directory not found: {}", resolved.display()),
                        ));
                    }
                    resolved
                }
                None => project.repo_path.clone(),
            };

            if let Some(run_cmd) = &step.run {
                for script_ref in extract_script_paths(run_cmd) {
                    let expanded = config::expand_tilde(Path::new(&script_ref));
                    let resolved = if expanded.is_absolute() {
                        expanded
                    } else {
                        step_cwd.join(&expanded)
                    };
                    if resolved.is_file() {
                        checks.push(CheckItem::ok("Script", format!("{script_ref} exists")));
                    } else {
                        checks.push(CheckItem::error(
                            "Script",
                            format!("script not found: {}", resolved.display()),
                        ));
                    }
                }
            }
        }
        StepKind::Act => {
            let act_cfg = cfg.act();
            let workflow = step.workflow.as_deref().unwrap_or("");
            let wf_path = project.repo_path.join(workflow);
            let workflow_text = std::fs::read_to_string(&wf_path).ok();
            let platform_map =
                act::resolve_platforms(os, &act_cfg, &project.platforms, &step.platforms);
            let runs_on = act::job_runs_on(workflow_text.as_deref(), step.job.as_deref());
            let self_hosted = act::uses_self_hosted(&platform_map, &runs_on);
            let argv = act::act_command(&act_cfg, self_hosted);
            let wsl = !self_hosted && act_cfg.is_wsl();

            if let Some(program) = argv.first() {
                if binary_exists(program) {
                    checks.push(CheckItem::ok("act", format!("{program} available")));
                } else {
                    checks.push(CheckItem::error(
                        "act",
                        format!("act binary '{program}' not found on PATH"),
                    ));
                }
            }

            if !self_hosted {
                let docker = docker_cache.entry(wsl).or_insert_with(|| check_docker(wsl));
                match docker {
                    Ok(ver) => {
                        checks.push(CheckItem::ok("Docker", format!("Docker reachable ({ver})")))
                    }
                    Err(err) => checks.push(CheckItem::error(
                        "Docker",
                        format!("Docker not reachable: {err}"),
                    )),
                }
            }

            if !wf_path.is_file() {
                checks.push(CheckItem::error(
                    "Workflow",
                    format!("workflow file not found: {}", wf_path.display()),
                ));
            } else if let Some(text) = &workflow_text {
                match act::parse_workflow(text) {
                    Ok(doc) => match &step.job {
                        Some(job) if doc.jobs.contains_key(job) => checks.push(CheckItem::ok(
                            "Workflow",
                            format!("{workflow} job '{job}' found"),
                        )),
                        Some(job) => checks.push(CheckItem::error(
                            "Workflow",
                            format!("job '{job}' not found in {workflow}"),
                        )),
                        None => checks.push(CheckItem::ok(
                            "Workflow",
                            format!("{workflow} ({} job(s))", doc.jobs.len()),
                        )),
                    },
                    Err(e) => checks.push(CheckItem::error(
                        "Workflow",
                        format!("invalid workflow YAML in {workflow}: {e}"),
                    )),
                }
            }
        }
    }

    DeployStepCheck {
        name: step.name.clone(),
        kind: kind.to_string(),
        applies_to_host,
        only_on,
        effective_timeout,
        command_summary,
        checks,
    }
}

fn summarize_step(step: &StepCfg) -> String {
    match step.kind {
        StepKind::Shell => {
            let first = step
                .run
                .as_deref()
                .unwrap_or("")
                .lines()
                .map(str::trim)
                .find(|l| !l.is_empty() && !l.starts_with('#'))
                .unwrap_or("");
            truncate_str(first, 120)
        }
        StepKind::Act => {
            let wf = step.workflow.as_deref().unwrap_or("");
            match &step.job {
                Some(j) if !j.is_empty() => format!("{wf} ({j})"),
                _ => wf.to_string(),
            }
        }
    }
}

fn truncate_str(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

fn compute_overall(checks: &[CheckItem], deploy_steps: &[DeployStepCheck]) -> String {
    let all = checks
        .iter()
        .chain(deploy_steps.iter().flat_map(|s| s.checks.iter()));
    let mut has_warn = false;
    for item in all {
        if item.level == "error" {
            return "error".to_string();
        }
        if item.level == "warn" {
            has_warn = true;
        }
    }
    if has_warn {
        "warn".to_string()
    } else {
        "ok".to_string()
    }
}

fn resolve_last_deploy(
    snap: Option<&ProjectSnapshot>,
    state_dir: &Path,
    project_name: &str,
) -> Option<LastDeploySummary> {
    let candidate = snap
        .and_then(|s| s.current_run.as_ref())
        .filter(|r| has_deploy_steps(r))
        .cloned()
        .or_else(|| {
            snap.and_then(|s| s.last_run.as_ref())
                .filter(|r| has_deploy_steps(r))
                .cloned()
        })
        .or_else(|| {
            model::list_runs(state_dir, project_name, 1)
                .into_iter()
                .next()
                .filter(has_deploy_steps)
        })?;
    let steps = candidate
        .steps
        .iter()
        .filter(|s| s.phase == Phase::Deploy)
        .map(|s| LastDeployStep {
            name: s.name.clone(),
            status: s.status,
            duration_ms: s.duration_ms,
        })
        .collect();
    Some(LastDeploySummary {
        run_id: candidate.id,
        sha: candidate.sha,
        status: candidate.status,
        finished_at: candidate.finished_at,
        steps,
    })
}

fn has_deploy_steps(run: &model::Run) -> bool {
    run.steps.iter().any(|s| s.phase == Phase::Deploy)
}

fn step_references_ci_dir(step: &StepCfg) -> bool {
    crate::pipeline::step_references_ci_dir(step)
}

fn is_git_work_tree(path: &Path) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["rev-parse", "--is-inside-work-tree"])
        .stdin(Stdio::null())
        .output()
        .map(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).trim() == "true")
        .unwrap_or(false)
}

fn binary_exists(program: &str) -> bool {
    let p = Path::new(program);
    if p.is_absolute() || program.contains('/') || program.contains('\\') {
        config::expand_tilde(p).is_file()
    } else {
        exec::which(program).is_some()
    }
}

fn check_docker(wsl: bool) -> Result<String, String> {
    let mut cmd = if wsl {
        let mut c = Command::new("wsl.exe");
        c.args(["-e", "docker"]);
        c
    } else {
        Command::new("docker")
    };
    match cmd
        .args(["info", "--format", "{{.ServerVersion}}"])
        .stdin(Stdio::null())
        .output()
    {
        Ok(out) if out.status.success() => {
            let ver = String::from_utf8_lossy(&out.stdout).trim().to_string();
            Ok(if ver.is_empty() {
                "ok".to_string()
            } else {
                ver
            })
        }
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
            Err(if stderr.is_empty() {
                "docker info failed".to_string()
            } else {
                stderr
            })
        }
        Err(e) => Err(e.to_string()),
    }
}

/// Extract static script file paths referenced by a shell step's `run` command
/// (e.g. `bash scripts/deploy.sh`, `sh ./run.sh`, `./scripts/deploy.sh`, `~/bin/deploy.sh`).
pub(crate) fn extract_script_paths(run: &str) -> Vec<String> {
    let mut paths = Vec::new();
    let mut changed_dir = false;

    for raw_line in run.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // Split simple chained commands on `&&` / `;` while preserving lines.
        for segment in line.split("&&").flat_map(|s| s.split(';')) {
            let seg = segment.trim();
            if seg.is_empty() || seg.starts_with('#') {
                continue;
            }
            let tokens = split_simple_tokens(seg);
            if tokens.is_empty() {
                continue;
            }
            // Skip leading env assignments like `FOO=1 bash scripts/deploy.sh`.
            let mut idx = 0;
            while idx < tokens.len() && is_env_assignment(&tokens[idx]) {
                idx += 1;
            }
            if idx >= tokens.len() {
                continue;
            }
            let first = tokens[idx].as_str();
            if first == "cd" {
                changed_dir = true;
                continue;
            }
            let candidate = match first {
                "bash" | "sh" | "zsh" | "dash" => {
                    let mut j = idx + 1;
                    let mut inline_code = false;
                    while j < tokens.len() && tokens[j].starts_with('-') {
                        let flag = &tokens[j];
                        if flag == "--" {
                            j += 1;
                            break;
                        }
                        if flag == "-c"
                            || (flag.starts_with('-')
                                && !flag.starts_with("--")
                                && flag.contains('c'))
                        {
                            inline_code = true;
                            break;
                        }
                        if flag == "-o" || flag == "+o" {
                            j += 2;
                            continue;
                        }
                        j += 1;
                    }
                    if inline_code || j >= tokens.len() {
                        None
                    } else {
                        Some(tokens[j].clone())
                    }
                }
                _ if first.starts_with("./") || first.starts_with("~/") => Some(first.to_string()),
                _ => None,
            };

            if let Some(path) = candidate {
                if !is_static_path(&path) {
                    continue;
                }
                let is_relative = !path.starts_with('/') && !path.starts_with("~/");
                if changed_dir && is_relative {
                    continue;
                }
                if !paths.contains(&path) {
                    paths.push(path);
                }
            }
        }
    }

    paths
}

fn is_env_assignment(tok: &str) -> bool {
    let Some((k, _)) = tok.split_once('=') else {
        return false;
    };
    !k.is_empty()
        && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !k.starts_with(|c: char| c.is_ascii_digit())
}

fn is_static_path(tok: &str) -> bool {
    !tok.is_empty()
        && !tok.contains('$')
        && !tok.contains('`')
        && !tok.contains('*')
        && !tok.contains('?')
        && !tok.contains('|')
        && !tok.contains('<')
        && !tok.contains('>')
}

fn split_simple_tokens(s: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    for ch in s.chars() {
        match quote {
            Some(q) if ch == q => {
                quote = None;
            }
            Some(_) => cur.push(ch),
            None if ch == '\'' || ch == '"' => {
                quote = Some(ch);
            }
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

/// Convenience helper for converting `PathBuf` checks in tests.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{DiscordCfg, GithubCfg};
    use std::path::PathBuf;

    fn shell_step(name: &str, run: &str) -> StepCfg {
        StepCfg {
            name: name.into(),
            kind: StepKind::Shell,
            run: Some(run.into()),
            workflow: None,
            job: None,
            event: None,
            platforms: Default::default(),
            args: Vec::new(),
            secrets: Default::default(),
            vars: Default::default(),
            only_on: Vec::new(),
            timeout: None,
            cwd: None,
            shell: Some(vec!["bash".into(), "-c".into()]),
            env: Default::default(),
        }
    }

    fn init_git_repo(dir: &Path, remote_url: Option<&str>) {
        let run = |args: &[&str]| {
            let out = Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(args)
                .output()
                .expect("git");
            assert!(out.status.success(), "git {args:?} failed");
        };
        std::fs::create_dir_all(dir).unwrap();
        run(&["init", "-b", "master"]);
        run(&["config", "user.email", "test@example.invalid"]);
        run(&["config", "user.name", "Test"]);
        std::fs::write(dir.join("README.md"), "hello\n").unwrap();
        run(&["add", "README.md"]);
        run(&["commit", "-m", "init"]);
        if let Some(url) = remote_url {
            run(&["remote", "add", "origin", url]);
        }
    }

    fn base_project(repo_path: PathBuf) -> ProjectCfg {
        ProjectCfg {
            name: "demo".into(),
            repo_path,
            remote: "origin".into(),
            branch: "master".into(),
            github_repo: None,
            checkout: Checkout::None,
            poll_interval: None,
            trigger_mode: None,
            step_timeout: None,
            timeout: None,
            env: Default::default(),
            contexts: Default::default(),
            discord_webhook_url: None,
            notify_on_start: None,
            shell: None,
            platforms: Default::default(),
            watch_prs: false,
            deploy_refs: Vec::new(),
            watch_tags: false,
            cache: None,
            ci: vec![shell_step("Test", "true")],
            deploy: Vec::new(),
        }
    }

    #[test]
    fn script_path_extraction() {
        assert_eq!(
            extract_script_paths("bash scripts/deploy.sh --fast"),
            vec!["scripts/deploy.sh"]
        );
        assert_eq!(
            extract_script_paths("FOO=1 sh -eu ./deploy.sh"),
            vec!["./deploy.sh"]
        );
        assert_eq!(
            extract_script_paths("./scripts/run.sh"),
            vec!["./scripts/run.sh"]
        );
        assert_eq!(
            extract_script_paths("~/bin/publish.sh"),
            vec!["~/bin/publish.sh"]
        );
        assert!(extract_script_paths("bash -c 'echo hi'").is_empty());
        assert!(extract_script_paths("bash \"$LUNACY_CI_DIR/x.sh\"").is_empty());
        assert!(extract_script_paths("cd sub && bash local.sh").is_empty());
    }

    #[test]
    fn dirty_worktree_in_place_is_error_and_none_is_warn() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        init_git_repo(&repo, Some("https://github.com/owner/demo.git"));
        let state_dir = dir.path().join("state");

        // Clean repo -> ok
        let mut cfg = Config {
            projects: vec![base_project(repo.clone())],
            ..Config::default()
        };
        let res = build_deploy_checks(&cfg, &[], &state_dir, Os::Linux);
        assert_eq!(
            res.projects[0].overall, "ok",
            "{:?}",
            res.projects[0].checks
        );
        assert_eq!(res.projects[0].git.dirty, Some(false));
        assert_eq!(res.projects[0].github_repo.as_deref(), Some("owner/demo"));

        // Dirty repo with checkout = "none" -> warn
        std::fs::write(repo.join("dirty.txt"), "untracked").unwrap();
        let res = build_deploy_checks(&cfg, &[], &state_dir, Os::Linux);
        assert_eq!(res.projects[0].overall, "warn");
        assert_eq!(res.projects[0].git.dirty, Some(true));

        // Dirty repo with checkout = "in-place" -> error
        cfg.projects[0].checkout = Checkout::InPlace;
        let res = build_deploy_checks(&cfg, &[], &state_dir, Os::Linux);
        assert_eq!(res.projects[0].overall, "error");
        assert!(res.projects[0]
            .checks
            .iter()
            .any(|c| c.label == "Worktree" && c.level == "error"));
    }

    #[test]
    fn missing_script_only_on_and_ci_dir_warnings() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        init_git_repo(&repo, Some("https://github.com/owner/demo.git"));
        let state_dir = dir.path().join("state");

        let mut proj = base_project(repo.clone());
        proj.deploy = vec![shell_step(
            "Publish",
            "test -d \"$LUNACY_CI_DIR\" && bash scripts/missing.sh",
        )];

        let mut cfg = Config::default();
        cfg.general.step_timeout = Some(900);
        cfg.github = Some(GithubCfg {
            mode: None,
            token: Some("tok".into()),
            token_file: None,
            api_url: None,
            webhook_secret: None,
            webhook_secret_file: None,
            app: None,
        });
        cfg.discord = Some(DiscordCfg {
            webhook_url: Some("https://discord.com/api/webhooks/1/2".into()),
            webhook_url_file: None,
            notify_on_start: false,
        });
        cfg.projects = vec![proj];

        let res = build_deploy_checks(&cfg, &[], &state_dir, Os::Linux);
        assert_eq!(res.default_step_timeout, 900);
        assert!(res.github_enabled);
        assert_eq!(res.github_mode, "status");
        assert!(res.discord_global);

        let p = &res.projects[0];
        assert_eq!(p.overall, "error");
        assert_eq!(p.reporting.discord_source, "global");
        assert!(p.reporting.github_enabled);
        assert!(p
            .checks
            .iter()
            .any(|c| c.label == "LUNACY_CI_DIR" && c.level == "warn"));
        assert!(p.deploy_steps[0]
            .checks
            .iter()
            .any(|c| c.label == "Script" && c.level == "error"));

        // Create the script -> Script check becomes ok.
        std::fs::create_dir_all(repo.join("scripts")).unwrap();
        std::fs::write(repo.join("scripts/missing.sh"), "#!/bin/sh\ntrue\n").unwrap();
        let res = build_deploy_checks(&cfg, &[], &state_dir, Os::Linux);
        let p = &res.projects[0];
        assert!(p.deploy_steps[0]
            .checks
            .iter()
            .any(|c| c.label == "Script" && c.level == "ok"));
        assert_eq!(p.overall, "warn"); // still LUNACY_CI_DIR warn

        // Restrict deploy step to Windows while checking on Linux -> warn on Deploy steps, no LUNACY_CI_DIR warn.
        cfg.projects[0].deploy[0].only_on = vec![Os::Windows];
        cfg.projects[0].discord_webhook_url = Some("https://discord.com/api/webhooks/3/4".into());
        let res = build_deploy_checks(&cfg, &[], &state_dir, Os::Linux);
        let p = &res.projects[0];
        assert_eq!(p.deploy_active_steps, 0);
        assert_eq!(p.deploy_total_steps, 1);
        assert!(!p.deploy_steps[0].applies_to_host);
        assert_eq!(p.reporting.discord_source, "project");
        assert!(p
            .checks
            .iter()
            .any(|c| c.label == "Deploy steps" && c.level == "warn"));
        assert!(!p.checks.iter().any(|c| c.label == "LUNACY_CI_DIR"));
    }

    #[test]
    fn last_deploy_summary_included_when_deploy_steps_exist() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        init_git_repo(&repo, Some("https://github.com/owner/demo.git"));
        let state_dir = dir.path().join("state");

        let mut run = model::Run::new(
            "demo",
            "0123456789abcdef0123456789abcdef01234567",
            "feat: deploy".into(),
            model::Trigger::Manual,
            vec![
                model::Step {
                    phase: Phase::Ci,
                    name: "Test".into(),
                    status: StepStatus::Success,
                    duration_ms: Some(100),
                    exit_code: Some(0),
                    kind: StepKind::Shell,
                    substeps: Vec::new(),
                },
                model::Step {
                    phase: Phase::Deploy,
                    name: "Publish".into(),
                    status: StepStatus::Success,
                    duration_ms: Some(250),
                    exit_code: Some(0),
                    kind: StepKind::Shell,
                    substeps: Vec::new(),
                },
            ],
        );
        run.status = RunStatus::Success;
        run.finished_at = Some(run.started_at + 10);

        let snap = ProjectSnapshot {
            name: "demo".into(),
            upstream_sha: Some(run.sha.clone()),
            last_sha: Some(run.sha.clone()),
            current_run: None,
            last_run: Some(run.clone()),
        };

        let cfg = Config {
            projects: vec![base_project(repo)],
            ..Config::default()
        };
        let res = build_deploy_checks(&cfg, &[snap], &state_dir, Os::Linux);
        let summary = res.projects[0].last_deploy.as_ref().expect("last_deploy");
        assert_eq!(summary.run_id, run.id);
        assert_eq!(summary.status, RunStatus::Success);
        assert_eq!(summary.steps.len(), 1);
        assert_eq!(summary.steps[0].name, "Publish");
        assert_eq!(summary.steps[0].duration_ms, Some(250));
    }

    #[test]
    fn webhook_mode_emits_webhook_readiness_check() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        init_git_repo(&repo, None);
        let state_dir = dir.path().join("state");

        // Non-webhook mode -> no "Webhook" check item.
        let mut proj = base_project(repo.clone());
        let mut cfg = Config {
            projects: vec![proj.clone()],
            ..Config::default()
        };
        let res = build_deploy_checks(&cfg, &[], &state_dir, Os::Linux);
        assert!(!res.projects[0].checks.iter().any(|c| c.label == "Webhook"));

        // Webhook mode without secret -> error("webhook secret is not configured").
        proj.trigger_mode = Some(TriggerMode::Webhook);
        cfg.projects = vec![proj.clone()];
        let res = build_deploy_checks(&cfg, &[], &state_dir, Os::Linux);
        let wh = res.projects[0]
            .checks
            .iter()
            .find(|c| c.label == "Webhook")
            .expect("Webhook check");
        assert_eq!(wh.level, "error");
        assert_eq!(wh.detail, "webhook secret is not configured");

        // Secret configured, but no remote/github_repo -> error("cannot determine GitHub repo for webhook matching").
        cfg.github = Some(GithubCfg {
            mode: None,
            token: None,
            token_file: None,
            api_url: None,
            webhook_secret: Some("wh-secret".into()),
            webhook_secret_file: None,
            app: None,
        });
        let res = build_deploy_checks(&cfg, &[], &state_dir, Os::Linux);
        let wh = res.projects[0]
            .checks
            .iter()
            .find(|c| c.label == "Webhook")
            .expect("Webhook check");
        assert_eq!(wh.level, "error");
        assert_eq!(
            wh.detail,
            "cannot determine GitHub repo for webhook matching"
        );

        // Add GitHub remote -> Webhook check becomes ok!
        let out = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args([
                "remote",
                "add",
                "origin",
                "https://github.com/owner/demo.git",
            ])
            .output()
            .expect("git");
        assert!(out.status.success());
        let res = build_deploy_checks(&cfg, &[], &state_dir, Os::Linux);
        let wh = res.projects[0]
            .checks
            .iter()
            .find(|c| c.label == "Webhook")
            .expect("Webhook check");
        assert_eq!(wh.level, "ok");
        assert!(wh.detail.contains("owner/demo"), "{}", wh.detail);
    }
}
