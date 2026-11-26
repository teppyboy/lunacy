//! The pipeline: checkout, CI steps, then deploy steps when CI passed.

use crate::act::{self, ActCachePlan, ActPlan, ActProgress};
use crate::config::{Checkout, Config, Os, ProjectCfg, StepCfg, StepKind};
use crate::discord::Discord;
use crate::exec::{self, ExecSpec, LineSink, Outcome};
use crate::git;
use crate::model::{
    now_secs, persist_run, run_log_path, short_sha, Phase, Run, RunStatus, Step, StepStatus,
    Substep, Trigger,
};
use crate::report::{CheckHandle, Github, PhaseReporter, Throttle};
use anyhow::{Context, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tempfile::TempDir;

/// Pull request metadata passed into a CI-only PR pipeline run.
#[derive(Debug, Clone)]
pub struct PrContext {
    pub number: u64,
    pub title: String,
    pub head_ref: String,
    pub base_ref: String,
    pub base_sha: String,
}

/// Full specification for a single pipeline execution.
#[derive(Debug, Clone)]
pub struct RunSpec {
    pub sha: String,
    pub trigger: Trigger,
    pub git_ref: String,
    pub pr: Option<PrContext>,
    pub deploy_only: bool,
    pub force_deploy: bool,
}

/// True if a step references `LUNACY_CI_DIR` in its script, env, args, or cwd.
pub fn step_references_ci_dir(step: &StepCfg) -> bool {
    step.run
        .as_deref()
        .is_some_and(|r| r.contains("LUNACY_CI_DIR"))
        || step.env.values().any(|v| v.contains("LUNACY_CI_DIR"))
        || step.args.iter().any(|a| a.contains("LUNACY_CI_DIR"))
        || step
            .cwd
            .as_ref()
            .is_some_and(|c| c.to_string_lossy().contains("LUNACY_CI_DIR"))
}

/// True when any active deploy step on `os` references `LUNACY_CI_DIR`.
pub fn deploy_needs_ci_dir(project: &ProjectCfg, os: Os) -> bool {
    project
        .deploy
        .iter()
        .filter(|s| s.applies_to(os))
        .any(step_references_ci_dir)
}

/// True when `git_ref` refers to the project's default watched branch (not a PR or tag).
pub fn is_default_branch_ref(project: &ProjectCfg, git_ref: &str) -> bool {
    let trimmed = git_ref.strip_prefix("refs/heads/").unwrap_or(git_ref);
    !git_ref.starts_with("tags/") && !git_ref.starts_with("refs/tags/") && trimmed == project.branch
}

/// Callback receiving `(step_index, chunk)` for every live log chunk appended to a step's log.
pub type LogChunkCallback = Arc<dyn Fn(usize, &str) + Send + Sync>;

/// Everything a pipeline needs from the daemon.
pub struct Ctx<'a> {
    pub cfg: &'a Config,
    pub state_dir: &'a Path,
    pub os: Os,
    pub github: Option<&'a Github>,
    /// Called on every step transition so the API shows live progress.
    pub on_update: &'a (dyn Fn(&Run) + Send + Sync),
    /// Global act lock: one act run at a time per machine.
    pub act_lock: &'a Path,
    /// Optional cancellation flag; when set to `true`, the pipeline aborts promptly.
    pub cancel: Option<&'a AtomicBool>,
    /// Optional live log-chunk callback for streaming step output (e.g. over WebSockets).
    pub on_log_chunk: Option<LogChunkCallback>,
}

fn is_cancelled(ctx: &Ctx<'_>) -> bool {
    ctx.cancel.is_some_and(|flag| flag.load(Ordering::SeqCst))
}

/// Held for the lifetime of a checkout; a `worktree` is pruned on drop.
struct CheckedOut {
    workdir: PathBuf,
    /// `(repo, worktree path, temp dir)`; dropped after `Drop::drop` runs.
    worktree: Option<(PathBuf, PathBuf, TempDir)>,
}

impl Drop for CheckedOut {
    fn drop(&mut self) {
        if let Some((repo, path, _tmp)) = &self.worktree {
            // Unregister and delete the tree while its temp dir still exists.
            git::remove_worktree(repo, path);
        }
    }
}

fn prepare_checkout(
    project: &ProjectCfg,
    sha: &str,
    checkout_mode: Checkout,
    state_dir: &Path,
) -> Result<CheckedOut> {
    let repo = &project.repo_path;
    match checkout_mode {
        Checkout::None => Ok(CheckedOut {
            workdir: repo.clone(),
            worktree: None,
        }),
        Checkout::InPlace => {
            git::fast_forward(repo, &project.remote, &project.branch, sha)
                .with_context(|| format!("fast-forwarding {}", repo.display()))?;
            Ok(CheckedOut {
                workdir: repo.clone(),
                worktree: None,
            })
        }
        Checkout::Worktree => {
            let checkouts_dir = state_dir.join("checkouts");
            let _ = fs::create_dir_all(&checkouts_dir);
            let tmp = TempDir::new_in(&checkouts_dir)
                .or_else(|_| TempDir::new())
                .context("creating a temp dir for the worktree")?;
            let path = tmp.path().join("tree");
            git::add_worktree(repo, &path, sha)?;
            Ok(CheckedOut {
                workdir: path.clone(),
                worktree: Some((repo.clone(), path, tmp)),
            })
        }
    }
}

/// Run the whole pipeline for one commit on the default branch.
/// Step failures are results, not errors.
pub fn run_pipeline(ctx: &Ctx<'_>, project: &ProjectCfg, sha: &str, trigger: Trigger) -> Run {
    let spec = RunSpec {
        sha: sha.to_string(),
        trigger,
        git_ref: project.branch.clone(),
        pr: None,
        deploy_only: false,
        force_deploy: false,
    };
    run_pipeline_spec(ctx, project, &spec)
}

fn deploy_target_summary(project: &ProjectCfg) -> String {
    if project.deploy_refs.is_empty() {
        project.branch.clone()
    } else {
        project.deploy_refs.join(", ")
    }
}

/// Run the pipeline for a full [`RunSpec`] (branch, PR, tag, or forced deploy).
#[allow(clippy::too_many_lines)]
pub fn run_pipeline_spec(ctx: &Ctx<'_>, project: &ProjectCfg, spec: &RunSpec) -> Run {
    let sha = spec.sha.as_str();
    let pipeline_deadline = project
        .timeout
        .map(|secs| Instant::now() + Duration::from_secs(secs));
    let subject = git::subject(&project.repo_path, sha);
    let steps: Vec<Step> = project
        .ci
        .iter()
        .map(|s| new_step(Phase::Ci, s))
        .chain(project.deploy.iter().map(|s| new_step(Phase::Deploy, s)))
        .collect();
    let mut run = Run::new(&project.name, sha, subject, spec.trigger, steps);
    if !spec.git_ref.is_empty() {
        run.git_ref = Some(spec.git_ref.clone());
    }
    run.deploy_only = spec.deploy_only;
    if let Some(pr) = &spec.pr {
        run.pr_number = Some(pr.number);
        run.pr_title = Some(pr.title.clone());
        run.pr_head_ref = Some(pr.head_ref.clone());
        run.pr_base_ref = Some(pr.base_ref.clone());
    }
    let repo = project.github_repo.clone().or_else(|| derive_repo(project));
    (ctx.on_update)(&run);
    persist(ctx, &run);

    let discord = ctx.cfg.discord_for(project).and_then(|resolved| {
        Discord::from_resolved(&resolved)
            .ok()
            .map(|d| (Arc::new(d), resolved.notify_on_start))
    });
    if let Some((d, true)) = &discord {
        if let Err(e) = d.notify_start(&run, repo.as_deref()) {
            log::warn!("[{}] discord start notification failed: {e}", project.name);
        }
    }

    // PRs and non-default branches/tags ALWAYS run in an isolated git worktree so the
    // production checkout is never mutated.
    let isolated = spec.pr.is_some() || !is_default_branch_ref(project, &spec.git_ref);
    let checkout_mode = if isolated {
        Checkout::Worktree
    } else {
        project.checkout
    };

    let checkout = match prepare_checkout(project, sha, checkout_mode, ctx.state_dir) {
        Ok(c) => c,
        Err(e) => {
            log::error!("[{}] checkout failed: {e:#}", project.name);
            run.status = RunStatus::Error;
            run.failed_step = Some(format!("checkout: {e:#}"));
            for step in run.steps.iter_mut() {
                step.status = StepStatus::Skipped;
            }
            finish(ctx, &mut run, repo.as_deref(), discord.as_ref());
            return run;
        }
    };
    if is_cancelled(ctx) {
        run.status = RunStatus::Cancelled;
        for step in run.steps.iter_mut() {
            step.status = StepStatus::Skipped;
        }
        drop(checkout);
        finish(ctx, &mut run, repo.as_deref(), discord.as_ref());
        return run;
    }

    let workdir = checkout.workdir.clone();
    let throttle = Throttle::default();
    // Created by the first act step, reused by every later step, removed at the end.
    let mut ci_dir: Option<PathBuf> = None;

    let needs_ci_dir = spec.deploy_only && deploy_needs_ci_dir(project, ctx.os);
    let ci_ok = if spec.deploy_only && !needs_ci_dir {
        for step in run.steps.iter_mut().filter(|s| s.phase == Phase::Ci) {
            step.status = StepStatus::Skipped;
        }
        (ctx.on_update)(&run);
        persist(ctx, &run);
        true
    } else {
        if needs_ci_dir {
            log::info!(
                "[{}] deploy_only: active deploy step references LUNACY_CI_DIR; running act CI step(s) to produce build artifacts",
                project.name
            );
        }
        let ci_check = start_report(ctx, project, &run, repo.as_deref(), Phase::Ci);
        let ok = run_steps(
            ctx,
            project,
            spec,
            &mut run,
            Phase::Ci,
            &workdir,
            &mut ci_dir,
            &throttle,
            &ci_check,
            repo.as_deref(),
            pipeline_deadline,
            needs_ci_dir,
        );
        finish_report(ctx, project, &run, &ci_check, Phase::Ci, repo.as_deref());
        // The CI check run's page is what the UI and Discord link to.
        run.github_url = ci_check.as_ref().and_then(|c| c.url.clone());
        ok
    };

    let allow_deploy = spec.force_deploy
        || crate::config::matches_deploy_ref(project, &spec.git_ref, spec.pr.is_some());

    if !project.deploy.is_empty() {
        if !allow_deploy {
            let reason = match &spec.pr {
                Some(pr) => format!(
                    "PR #{} — CI only (deploy runs on {})",
                    pr.number,
                    deploy_target_summary(project)
                ),
                None => format!("Ref '{}' does not match deploy_refs", spec.git_ref),
            };
            log::info!("[{}] skipping deploy: {reason}", project.name);
            run.deploy_skipped_reason = Some(reason);
            for step in run.steps.iter_mut().filter(|s| s.phase == Phase::Deploy) {
                step.status = StepStatus::Skipped;
            }
            (ctx.on_update)(&run);
            persist(ctx, &run);
        } else if ci_ok && !is_cancelled(ctx) {
            let deploy_check = start_report(ctx, project, &run, repo.as_deref(), Phase::Deploy);
            run_steps(
                ctx,
                project,
                spec,
                &mut run,
                Phase::Deploy,
                &workdir,
                &mut ci_dir,
                &throttle,
                &deploy_check,
                repo.as_deref(),
                pipeline_deadline,
                false,
            );
            finish_report(
                ctx,
                project,
                &run,
                &deploy_check,
                Phase::Deploy,
                repo.as_deref(),
            );
            if run.github_url.is_none() {
                run.github_url = deploy_check.as_ref().and_then(|c| c.url.clone());
            }
        } else {
            if ci_ok && is_cancelled(ctx) {
                run.status = RunStatus::Cancelled;
            }
            // CI failed or run was cancelled: deploy was never attempted.
            for step in run.steps.iter_mut().filter(|s| s.phase == Phase::Deploy) {
                step.status = StepStatus::Skipped;
            }
        }
    }

    drop(checkout);
    cleanup_ci_dir(ctx.state_dir, &run.id);
    finish(ctx, &mut run, repo.as_deref(), discord.as_ref());
    run
}

fn derive_repo(project: &ProjectCfg) -> Option<String> {
    git::remote_url(&project.repo_path, &project.remote)
        .as_deref()
        .and_then(crate::config::repo_from_remote)
}

fn new_step(phase: Phase, cfg: &StepCfg) -> Step {
    Step {
        phase,
        name: cfg.name.clone(),
        status: StepStatus::Pending,
        duration_ms: None,
        exit_code: None,
        kind: cfg.kind,
        substeps: Vec::new(),
    }
}

fn step_index(run: &Run, phase: Phase, name: &str) -> Option<usize> {
    run.steps
        .iter()
        .position(|s| s.phase == phase && s.name == name)
}

#[allow(clippy::too_many_lines, clippy::too_many_arguments)]
fn run_steps(
    ctx: &Ctx<'_>,
    project: &ProjectCfg,
    spec: &RunSpec,
    run: &mut Run,
    phase: Phase,
    workdir: &Path,
    ci_dir: &mut Option<PathBuf>,
    throttle: &Throttle,
    check: &Option<CheckHandle>,
    repo: Option<&str>,
    pipeline_deadline: Option<Instant>,
    only_act: bool,
) -> bool {
    let steps: Vec<StepCfg> = match phase {
        Phase::Ci => project.ci.clone(),
        Phase::Deploy => project.deploy.clone(),
    };
    let total = steps.len();
    let mut ok = true;

    for (i, step_cfg) in steps.iter().enumerate() {
        let Some(index) = step_index(run, phase, &step_cfg.name) else {
            continue;
        };

        if is_cancelled(ctx) {
            run.status = RunStatus::Cancelled;
            for remaining in &steps[i..] {
                if let Some(pos) = step_index(run, phase, &remaining.name) {
                    set_step(run, pos, StepStatus::Skipped, None, None);
                }
            }
            publish(ctx, project, run, check, throttle, i, total, repo);
            return false;
        }

        if only_act && step_cfg.kind != StepKind::Act {
            set_step(run, index, StepStatus::Skipped, None, None);
            log::info!(
                "[{}] {} {}: skipped (deploy_only: only running act CI steps for LUNACY_CI_DIR)",
                project.name,
                phase.as_str(),
                step_cfg.name
            );
            publish(ctx, project, run, check, throttle, i, total, repo);
            continue;
        }

        if !step_cfg.applies_to(ctx.os) {
            set_step(run, index, StepStatus::Skipped, None, None);
            log::info!(
                "[{}] {} {}: skipped (not for {})",
                project.name,
                phase.as_str(),
                step_cfg.name,
                ctx.os.as_str()
            );
            publish(ctx, project, run, check, throttle, i, total, repo);
            continue;
        }

        let step_timeout =
            crate::config::resolve_step_timeout(&ctx.cfg.general, Some(project), step_cfg);
        let effective_timeout = match pipeline_deadline {
            Some(deadline) => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    let secs = project.timeout.unwrap_or(0);
                    let log_path = run_log_path(ctx.state_dir, &project.name, &run.id, index);
                    if let Some(parent) = log_path.parent() {
                        let _ = fs::create_dir_all(parent);
                    }
                    let msg = format!("[lunacy] pipeline timed out after {secs}s\n");
                    let _ = fs::write(&log_path, &msg);
                    if let Some(cb) = &ctx.on_log_chunk {
                        cb(index, &msg);
                    }
                    set_step(run, index, StepStatus::Timeout, Some(0), None);
                    log::info!(
                        "[{}] {} {}: timeout (pipeline timed out after {}s)",
                        project.name,
                        phase.as_str(),
                        step_cfg.name,
                        secs
                    );
                    ok = false;
                    run.failed_step = Some(step_cfg.name.clone());
                    for later in &steps[i + 1..] {
                        if let Some(pos) = step_index(run, phase, &later.name) {
                            set_step(run, pos, StepStatus::Skipped, None, None);
                        }
                    }
                    publish(ctx, project, run, check, throttle, i, total, repo);
                    break;
                }
                step_timeout.min(remaining)
            }
            None => step_timeout,
        };

        set_step(run, index, StepStatus::Running, None, None);
        publish(ctx, project, run, check, throttle, i, total, repo);
        let started = Instant::now();

        let outcome = match step_cfg.kind {
            StepKind::Shell => run_shell_step(
                ctx,
                project,
                spec,
                step_cfg,
                run,
                index,
                workdir,
                ci_dir.as_deref(),
                effective_timeout,
            ),
            StepKind::Act => run_act_step(
                ctx,
                project,
                spec,
                step_cfg,
                run,
                index,
                workdir,
                ci_dir,
                repo,
                effective_timeout,
            ),
        };

        let duration_ms = started.elapsed().as_millis() as u64;
        let (status, code) = match &outcome {
            Ok((Outcome::Success, _)) => (StepStatus::Success, Some(0)),
            Ok((Outcome::Failed(c), _)) => (StepStatus::Failure, Some(*c)),
            Ok((Outcome::Timeout, _)) => (StepStatus::Timeout, None),
            Ok((Outcome::Cancelled, _)) => (StepStatus::Cancelled, None),
            Ok((Outcome::Spawn, _)) => (StepStatus::Failure, None),
            Err(e) => {
                log::error!(
                    "[{}] {} {}: {e:#}",
                    project.name,
                    phase.as_str(),
                    step_cfg.name
                );
                (StepStatus::Failure, None)
            }
        };
        if let Ok((_, substeps)) = outcome {
            if let Some(step) = run.steps.get_mut(index) {
                step.substeps = substeps;
            }
        }
        set_step(run, index, status, Some(duration_ms), code);
        if status == StepStatus::Cancelled {
            run.status = RunStatus::Cancelled;
        }
        log::info!(
            "[{}] {} {}: {} ({}ms)",
            project.name,
            phase.as_str(),
            step_cfg.name,
            status.label(),
            duration_ms
        );
        publish(ctx, project, run, check, throttle, i, total, repo);

        if !status.is_ok() {
            ok = false;
            if status != StepStatus::Cancelled {
                run.failed_step = Some(step_cfg.name.clone());
            }
            for later in &steps[i + 1..] {
                if let Some(pos) = step_index(run, phase, &later.name) {
                    set_step(run, pos, StepStatus::Skipped, None, None);
                }
            }
            break;
        }
    }
    ok
}

fn set_step(
    run: &mut Run,
    index: usize,
    status: StepStatus,
    duration: Option<u64>,
    code: Option<i32>,
) {
    if let Some(step) = run.steps.get_mut(index) {
        step.status = status;
        step.duration_ms = duration;
        step.exit_code = code;
    }
}

#[allow(clippy::too_many_arguments)]
fn publish(
    ctx: &Ctx<'_>,
    project: &ProjectCfg,
    run: &Run,
    check: &Option<CheckHandle>,
    throttle: &Throttle,
    index: usize,
    total: usize,
    repo: Option<&str>,
) {
    (ctx.on_update)(run);
    persist(ctx, run);
    if !ctx.cfg.github_enabled_for(project) {
        return;
    }
    let (Some(gh), Some(repo), Some(check)) = (ctx.github, repo, check.as_ref()) else {
        return;
    };
    // Every step transition forces an update; otherwise the throttle keeps it
    // to roughly one per 10 seconds.
    if !throttle.allows(true) {
        return;
    }
    let phase = run.steps.get(index).map(|s| s.phase).unwrap_or(Phase::Ci);
    let name = run
        .steps
        .get(index)
        .map(|s| s.name.as_str())
        .unwrap_or_default();
    let label = match phase {
        Phase::Ci => "CI",
        Phase::Deploy => "Deploy",
    };
    let title = format!("{label}: step {}/{total} {name}", index + 1);
    let reporter = reporter_for(ctx, project, run, phase, repo);
    if let Err(e) = gh.progress(&reporter, check, run, title) {
        log::warn!("[{}] github progress update failed: {e:#}", project.name);
    }
}

fn reporter_for(
    ctx: &Ctx<'_>,
    project: &ProjectCfg,
    run: &Run,
    phase: Phase,
    repo: &str,
) -> PhaseReporter {
    PhaseReporter {
        repo: repo.to_string(),
        sha: run.sha.clone(),
        context: match phase {
            Phase::Ci => project.contexts.ci.clone(),
            Phase::Deploy => project.contexts.deploy.clone(),
        },
        phase,
        run_id: run.id.clone(),
        details_url: ctx
            .cfg
            .server
            .as_ref()
            .and_then(|s| s.public_url.as_ref())
            .map(|base| format!("{}/#/runs/{}", base.trim_end_matches('/'), run.id)),
        state_dir: ctx.state_dir.to_path_buf(),
    }
}

fn start_report(
    ctx: &Ctx<'_>,
    project: &ProjectCfg,
    run: &Run,
    repo: Option<&str>,
    phase: Phase,
) -> Option<CheckHandle> {
    if !ctx.cfg.github_enabled_for(project) {
        return None;
    }
    let gh = ctx.github?;
    let repo = repo.or(project.github_repo.as_deref())?;
    if project.deploy.is_empty() && phase == Phase::Deploy {
        return None;
    }
    let reporter = reporter_for(ctx, project, run, phase, repo);
    match gh.start_phase(&reporter) {
        Ok(handle) => handle,
        Err(e) => {
            log::warn!(
                "[{}] github {} start failed: {e:#}",
                project.name,
                phase.as_str()
            );
            None
        }
    }
}

fn finish_report(
    ctx: &Ctx<'_>,
    project: &ProjectCfg,
    run: &Run,
    check: &Option<CheckHandle>,
    phase: Phase,
    repo: Option<&str>,
) {
    if !ctx.cfg.github_enabled_for(project) {
        return;
    }
    let (Some(gh), Some(check), Some(repo)) = (ctx.github, check.as_ref(), repo) else {
        return;
    };
    let reporter = reporter_for(ctx, project, run, phase, repo);
    if let Err(e) = gh.finish_phase(&reporter, check, run) {
        log::warn!(
            "[{}] github {} finish failed: {e:#}",
            project.name,
            phase.as_str()
        );
    }
}

fn cache_enabled(ctx: &Ctx<'_>, project: &ProjectCfg) -> bool {
    project.cache.unwrap_or_else(|| ctx.cfg.act().cache)
}

fn base_env(
    ctx: &Ctx<'_>,
    run: &Run,
    project: &ProjectCfg,
    spec: &RunSpec,
    workdir: &Path,
    ci_dir: Option<&Path>,
) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    env.insert("LUNACY_SHA".into(), run.sha.clone());
    env.insert("LUNACY_BRANCH".into(), project.branch.clone());
    env.insert("LUNACY_REF".into(), spec.git_ref.clone());
    env.insert("LUNACY_PROJECT".into(), project.name.clone());
    env.insert(
        "LUNACY_WORKDIR".into(),
        workdir.to_string_lossy().to_string(),
    );
    env.insert("LUNACY_RUN_ID".into(), run.id.clone());
    if let Some(pr) = &spec.pr {
        env.insert("LUNACY_PR_NUMBER".into(), pr.number.to_string());
    }
    // The act clone, so a deploy step can pick up what the CI job built in it.
    if let Some(dir) = ci_dir {
        env.insert("LUNACY_CI_DIR".into(), dir.to_string_lossy().to_string());
    }
    if cache_enabled(ctx, project) {
        let cache_root = ctx.cfg.act().effective_cache_dir(ctx.state_dir);
        let project_cache = cache_root
            .join("projects")
            .join(crate::config::project_cache_slug(&project.name));
        let _ = fs::create_dir_all(&project_cache);
        env.insert(
            "LUNACY_CACHE_DIR".into(),
            project_cache.to_string_lossy().to_string(),
        );
        env.insert("DOCKER_BUILDKIT".into(), "1".into());
        env.insert("COMPOSE_DOCKER_CLI_BUILD".into(), "1".into());
    }
    env
}

#[allow(clippy::too_many_arguments)]
fn run_shell_step(
    ctx: &Ctx<'_>,
    project: &ProjectCfg,
    spec: &RunSpec,
    step: &StepCfg,
    run: &Run,
    index: usize,
    workdir: &Path,
    ci_dir: Option<&Path>,
    effective_timeout: Duration,
) -> Result<(Outcome, Vec<Substep>)> {
    let shell = crate::config::resolve_shell(&ctx.cfg.general, Some(project), Some(step), ctx.os);
    let (program, args) = shell.split_first().context("empty shell argv")?;
    let mut argv: Vec<String> = args.to_vec();
    argv.push(step.run.clone().unwrap_or_default());

    let mut env = base_env(ctx, run, project, spec, workdir, ci_dir);
    env.extend(project.env.clone());
    env.extend(step.env.clone());

    let cwd = match &step.cwd {
        Some(c) if c.is_absolute() => c.clone(),
        Some(c) => workdir.join(c),
        None => workdir.to_path_buf(),
    };

    let exec_spec = ExecSpec {
        program: program.clone(),
        args: argv,
        cwd,
        env,
        timeout: effective_timeout,
        log_path: run_log_path(ctx.state_dir, &project.name, &run.id, index),
    };
    let log_hook: Option<LineSink> = ctx.on_log_chunk.as_ref().map(|cb| {
        let cb = cb.clone();
        Arc::new(move |chunk: &str| cb(index, chunk)) as LineSink
    });
    Ok((
        exec::exec_with_log_hook(&exec_spec, None, log_hook, ctx.cancel)?.outcome,
        Vec::new(),
    ))
}

#[allow(clippy::too_many_arguments)]
fn run_act_step(
    ctx: &Ctx<'_>,
    project: &ProjectCfg,
    spec: &RunSpec,
    step: &StepCfg,
    run: &Run,
    index: usize,
    workdir: &Path,
    ci_dir: &mut Option<PathBuf>,
    repo: Option<&str>,
    effective_timeout: Duration,
) -> Result<(Outcome, Vec<Substep>)> {
    let act_cfg = ctx.cfg.act();
    let platform_map =
        act::resolve_platforms(ctx.os, &act_cfg, &project.platforms, &step.platforms);
    let workflow = step.workflow.clone().unwrap_or_default();
    let step_deadline = Instant::now() + effective_timeout;

    // act copies the working tree and needs a usable `.git`, so no git worktree.
    // One clone per run, kept under the state dir so the CI job's `build/`
    // output survives for a later deploy step (`LUNACY_CI_DIR`).
    let clone_path = match ci_dir.clone() {
        Some(path) => path,
        None => {
            let path = ctx.state_dir.join("checkouts").join(&run.id).join("repo");
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("creating {}", parent.display()))?;
            }
            git::clone_at(workdir, &path, "origin", &run.sha)
                .with_context(|| format!("cloning {} for act", workdir.display()))?;
            *ci_dir = Some(path.clone());
            path
        }
    };

    let log_path = run_log_path(ctx.state_dir, &project.name, &run.id, index);
    if let Some(parent) = log_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let human_log = Arc::new(Mutex::new(fs::File::create(&log_path)?));
    let emit_human_line = |line: &str| {
        let mut chunk = String::with_capacity(line.len() + 1);
        chunk.push_str(line);
        chunk.push('\n');
        if let Ok(mut f) = human_log.lock() {
            let _ = f.write_all(chunk.as_bytes());
            let _ = f.flush();
            if let Some(cb) = &ctx.on_log_chunk {
                cb(index, &chunk);
            }
        }
    };

    if is_cancelled(ctx) {
        emit_human_line("[lunacy] cancelled before starting act");
        return Ok((Outcome::Cancelled, Vec::new()));
    }

    let workflow_text = fs::read_to_string(clone_path.join(&workflow)).ok();
    let runs_on = act::job_runs_on(workflow_text.as_deref(), step.job.as_deref());
    let self_hosted = act::uses_self_hosted(&platform_map, &runs_on);
    let argv = act::act_command(&act_cfg, self_hosted);
    let wsl = !self_hosted && act_cfg.is_wsl();

    let tmp = TempDir::new().context("creating act temp files")?;
    let event_file = tmp.path().join("event.json");
    let (event_name, event_body) = match &spec.pr {
        Some(pr) => {
            let base_sha = if pr.base_sha.is_empty() && !pr.base_ref.is_empty() {
                let base_branch = pr
                    .base_ref
                    .strip_prefix("refs/heads/")
                    .unwrap_or(&pr.base_ref);
                git::rev_parse(workdir, &format!("{}/{}", project.remote, base_branch))
                    .unwrap_or_default()
            } else {
                pr.base_sha.clone()
            };
            (
                "pull_request".to_string(),
                act::pull_request_event_json(
                    pr.number,
                    &pr.title,
                    &run.sha,
                    &pr.head_ref,
                    &base_sha,
                    &pr.base_ref,
                    &project.branch,
                    repo.or(project.github_repo.as_deref()),
                ),
            )
        }
        None => (
            step.event().to_string(),
            push_event(ctx, project, workdir, &run.sha, &spec.git_ref, repo),
        ),
    };
    fs::write(&event_file, event_body)?;

    let secret_file = if step.secrets.is_empty() {
        None
    } else {
        let path = tmp.path().join("secrets");
        write_kv_file(&path, &step.secrets)?;
        Some(path)
    };
    let var_file = if step.vars.is_empty() {
        None
    } else {
        let path = tmp.path().join("vars");
        write_kv_file(&path, &step.vars)?;
        Some(path)
    };
    let artifact_dir = ctx
        .state_dir
        .join("artifacts")
        .join(&run.id)
        .join(format!("step{index}"));
    fs::create_dir_all(&artifact_dir)?;

    let cache_plan = if cache_enabled(ctx, project) {
        let cache_root = act_cfg.effective_cache_dir(ctx.state_dir);
        let action_cache_path = cache_root.join("act-actions");
        let cache_server_path = cache_root.join("act-cache-server");
        fs::create_dir_all(&action_cache_path)?;
        fs::create_dir_all(&cache_server_path)?;

        let mut combined_user_args = act_cfg.args.clone();
        combined_user_args.extend(project.act_args.iter().cloned());
        combined_user_args.extend(step.args.iter().cloned());
        let pull_false = !self_hosted
            && !act::has_pull_flag(&combined_user_args)
            && all_platform_images_present_locally(&platform_map, wsl);

        Some(ActCachePlan {
            action_cache_path,
            cache_server_path,
            project_slug: crate::config::project_cache_slug(&project.name),
            pull_false,
            include_container_volumes: !self_hosted,
            project_env: project.env.clone(),
        })
    } else {
        None
    };

    let plan = ActPlan {
        event_file,
        secret_file,
        var_file,
        artifact_dir,
        workflow,
        job: step.job.clone(),
        event: event_name,
        project_args: project.act_args.clone(),
        cache: cache_plan,
    };
    let args = act::build_args(&plan, step, &platform_map, &act_cfg, wsl);

    let (program, prefix) = argv.split_first().context("empty act command")?;
    let mut full_args: Vec<String> = prefix.to_vec();
    full_args.extend(args);
    if wsl {
        // act runs inside WSL, so the paths it receives must be WSL paths too.
        full_args = act::wsl_translate_args(&full_args);
    }

    let mut env = base_env(ctx, run, project, spec, workdir, Some(&clone_path));
    env.extend(project.env.clone());
    env.extend(step.env.clone());

    let progress = Arc::new(Mutex::new(ActProgress::new()));

    // act writes JSON events; the per-step log keeps their `msg` text instead.
    let sink: LineSink = {
        let progress = progress.clone();
        let human_log = human_log.clone();
        let on_log_chunk = ctx.on_log_chunk.clone();
        Arc::new(move |line: &str| {
            let text = {
                let Ok(mut p) = progress.lock() else { return };
                match p.feed(line) {
                    Some(msg) => p.human_line(msg.trim_end()),
                    None => line.to_string(),
                }
            };
            if text.is_empty() {
                return;
            }
            let mut chunk = String::with_capacity(text.len() + 1);
            chunk.push_str(&text);
            chunk.push('\n');
            if let Ok(mut f) = human_log.lock() {
                let _ = f.write_all(chunk.as_bytes());
                let _ = f.flush();
                if let Some(cb) = &on_log_chunk {
                    cb(index, &chunk);
                }
            }
        })
    };

    // One act run at a time on this machine: act names containers after the
    // workflow and job, so parallel runs destroy each other.
    let _lock = match ActLock::acquire(ctx.act_lock, ctx.cancel, Some(step_deadline))? {
        LockOutcome::Acquired(lock) => lock,
        LockOutcome::Cancelled => {
            emit_human_line("[lunacy] cancelled while waiting for act lock");
            return Ok((Outcome::Cancelled, Vec::new()));
        }
        LockOutcome::TimedOut => {
            emit_human_line(&format!(
                "[lunacy] timed out after {}s while waiting for act lock",
                effective_timeout.as_secs()
            ));
            return Ok((Outcome::Timeout, Vec::new()));
        }
    };

    let remaining = step_deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        emit_human_line(&format!(
            "[lunacy] timed out after {}s",
            effective_timeout.as_secs()
        ));
        return Ok((Outcome::Timeout, Vec::new()));
    }

    let containers_before = list_act_containers(wsl);

    let spec = ExecSpec {
        program: program.clone(),
        args: full_args,
        // WSL mode mounts Windows drives under /mnt, so the clone is reached
        // through its translated path.
        cwd: if wsl {
            act::wsl_path(&clone_path)
        } else {
            clone_path.clone()
        },
        env,
        timeout: remaining,
        // Raw act output is throwaway; the human log above is the real one.
        log_path: tmp.path().join("act-raw.log"),
    };
    let result = exec::exec(&spec, Some(sink), ctx.cancel)?;

    if matches!(result.outcome, Outcome::Cancelled | Outcome::Timeout) {
        cleanup_new_act_containers(wsl, &containers_before);
        if let Ok(mut p) = progress.lock() {
            let final_status = match result.outcome {
                Outcome::Cancelled => StepStatus::Cancelled,
                Outcome::Timeout => StepStatus::Timeout,
                _ => unreachable!(),
            };
            p.finalize_running(final_status);
        }
    }

    let substeps = progress.lock().map(|p| p.substeps()).unwrap_or_default();
    Ok((result.outcome, substeps))
}

fn docker_command(wsl: bool) -> std::process::Command {
    if wsl {
        let mut cmd = std::process::Command::new("wsl.exe");
        cmd.args(["-e", "docker"]);
        cmd
    } else {
        std::process::Command::new("docker")
    }
}

/// True when `platforms` has at least one container image mapping and every mapped
/// container image already exists locally (`docker image inspect <img_ref>` succeeds).
fn all_platform_images_present_locally(platforms: &BTreeMap<String, String>, wsl: bool) -> bool {
    let images: BTreeSet<&str> = platforms
        .values()
        .map(String::as_str)
        .filter(|v| !v.is_empty() && *v != act::SELF_HOSTED)
        .collect();
    if images.is_empty() {
        return false;
    }
    images.into_iter().all(|img| {
        docker_command(wsl)
            .args(["image", "inspect", img])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    })
}

fn list_act_containers(wsl: bool) -> Option<BTreeSet<String>> {
    let out = docker_command(wsl)
        .args(["ps", "-aq", "--filter", "name=^/?act-"])
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(String::from)
            .collect(),
    )
}

fn cleanup_new_act_containers(wsl: bool, before: &Option<BTreeSet<String>>) {
    let Some(before) = before else { return };
    let Some(after) = list_act_containers(wsl) else {
        return;
    };
    let new_ids: Vec<&String> = after.difference(before).collect();
    if new_ids.is_empty() {
        return;
    }
    log::info!(
        "cleaning up {} leftover act container(s): {:?}",
        new_ids.len(),
        new_ids
    );
    let mut cmd = docker_command(wsl);
    cmd.args(["rm", "-f"]);
    for id in new_ids {
        cmd.arg(id);
    }
    let _ = cmd
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output();
}

/// Remove the run's act clone. Files act created with `--bind` can be root-owned,
/// so a failure only warns: the run already happened and must not be failed by it.
fn cleanup_ci_dir(state_dir: &Path, run_id: &str) {
    let dir = state_dir.join("checkouts").join(run_id);
    if !dir.exists() {
        return;
    }
    if let Err(e) = fs::remove_dir_all(&dir) {
        log::warn!(
            "could not remove the act clone {} ({e}); remove it with `sudo rm -rf {}`",
            dir.display(),
            dir.display()
        );
    }
}

/// The push event for `sha`: a realistic `before` so workflows that read
/// `before..after` see only this commit, plus the message and pusher.
fn push_event(
    ctx: &Ctx<'_>,
    project: &ProjectCfg,
    repo_dir: &Path,
    sha: &str,
    git_ref: &str,
    repo: Option<&str>,
) -> String {
    let last = crate::model::read_last_sha(ctx.state_dir, &project.name);
    let last_is_ancestor = last
        .as_deref()
        .map(|l| l != sha && git::is_ancestor(repo_dir, l, sha).unwrap_or(false))
        .unwrap_or(false);
    let first_parent = git::rev_parse(repo_dir, &format!("{sha}^")).ok();
    let before = act::pick_before(last.as_deref(), last_is_ancestor, first_parent.as_deref());
    let (message, name, email) = git::commit_details(repo_dir, sha);
    let target_ref = if git_ref.is_empty() {
        project.branch.as_str()
    } else {
        git_ref
    };
    act::event_json(
        sha,
        &before,
        target_ref,
        repo.or(project.github_repo.as_deref()),
        &message,
        (&name, &email),
    )
}

fn write_kv_file(path: &Path, map: &BTreeMap<String, String>) -> Result<()> {
    let mut text = String::new();
    for (k, v) in map {
        text.push_str(&format!("{k}={v}\n"));
    }
    fs::write(path, text)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

struct ActLock {
    file: fs::File,
}

enum LockOutcome {
    Acquired(ActLock),
    Cancelled,
    TimedOut,
}

impl ActLock {
    fn acquire(
        path: &Path,
        cancel: Option<&AtomicBool>,
        deadline: Option<Instant>,
    ) -> Result<LockOutcome> {
        use fs2::FileExt;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path)
            .with_context(|| format!("opening act lock {}", path.display()))?;
        let contended_os = fs2::lock_contended_error().raw_os_error();
        loop {
            if cancel.is_some_and(|flag| flag.load(Ordering::SeqCst)) {
                return Ok(LockOutcome::Cancelled);
            }
            if deadline.is_some_and(|d| Instant::now() >= d) {
                return Ok(LockOutcome::TimedOut);
            }
            match file.try_lock_exclusive() {
                Ok(()) => return Ok(LockOutcome::Acquired(ActLock { file })),
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || (contended_os.is_some() && e.raw_os_error() == contended_os) =>
                {
                    let sleep_for = match deadline {
                        Some(d) => d
                            .saturating_duration_since(Instant::now())
                            .min(Duration::from_millis(100)),
                        None => Duration::from_millis(100),
                    };
                    if sleep_for.is_zero() {
                        return Ok(LockOutcome::TimedOut);
                    }
                    std::thread::sleep(sleep_for);
                }
                Err(e) => return Err(e).with_context(|| format!("locking {}", path.display())),
            }
        }
    }
}

impl Drop for ActLock {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.file);
    }
}

fn persist(ctx: &Ctx<'_>, run: &Run) {
    if let Err(e) = persist_run(ctx.state_dir, run) {
        log::warn!("persisting run {}: {e}", run.id);
    }
}

fn finish(
    ctx: &Ctx<'_>,
    run: &mut Run,
    repo: Option<&str>,
    discord: Option<&(Arc<Discord>, bool)>,
) {
    run.finished_at = Some(now_secs());
    if run.status == RunStatus::Running {
        let any_failed = run
            .steps
            .iter()
            .any(|s| matches!(s.status, StepStatus::Failure | StepStatus::Timeout));
        run.status = if any_failed {
            RunStatus::Failure
        } else {
            RunStatus::Success
        };
    }
    if run.status == RunStatus::Success {
        run.failed_step = None;
    }
    persist(ctx, run);
    (ctx.on_update)(run);
    if let Some((d, _)) = discord {
        if let Err(e) = d.notify_finish(run, repo, ctx.state_dir) {
            log::warn!("discord finish notification failed: {e:#}");
        }
    }
    log::info!(
        "[{}] {} {} in {}s",
        run.project,
        short_sha(&run.sha),
        match run.status {
            RunStatus::Success => "succeeded",
            RunStatus::Failure => "failed",
            RunStatus::Error => "errored",
            RunStatus::Cancelled => "cancelled",
            RunStatus::Running => "still running",
        },
        run.duration_secs()
    );
}
