//! The pipeline: checkout, CI steps, then deploy steps when CI passed.

use crate::act::{self, ActPlan, ActProgress};
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
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tempfile::TempDir;

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

fn prepare_checkout(project: &ProjectCfg, sha: &str, state_dir: &Path) -> Result<CheckedOut> {
    let repo = &project.repo_path;
    match project.checkout {
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
            let tmp = TempDir::new_in(state_dir.join("checkouts"))
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

/// Run the whole pipeline for one commit. Step failures are results, not errors.
pub fn run_pipeline(ctx: &Ctx<'_>, project: &ProjectCfg, sha: &str, trigger: Trigger) -> Run {
    let subject = git::subject(&project.repo_path, sha);
    let steps: Vec<Step> = project
        .ci
        .iter()
        .map(|s| new_step(Phase::Ci, s))
        .chain(project.deploy.iter().map(|s| new_step(Phase::Deploy, s)))
        .collect();
    let mut run = Run::new(&project.name, sha, subject, trigger, steps);
    let repo = project.github_repo.clone().or_else(|| derive_repo(project));
    persist(ctx, &run);

    let discord = ctx
        .cfg
        .discord_for(project)
        .and_then(|(url, notify_start)| {
            Discord::new(&url).ok().map(|d| (Arc::new(d), notify_start))
        });
    if let Some((d, true)) = &discord {
        if let Err(e) = d.notify_start(&run, repo.as_deref()) {
            log::warn!("[{}] discord start notification failed: {e}", project.name);
        }
    }

    let checkout = match prepare_checkout(project, sha, ctx.state_dir) {
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
    let workdir = checkout.workdir.clone();
    let throttle = Throttle::default();
    // Created by the first act step, reused by every later step, removed at the end.
    let mut ci_dir: Option<PathBuf> = None;

    let ci_check = start_report(ctx, project, &run, repo.as_deref(), Phase::Ci);
    let ci_ok = run_steps(
        ctx,
        project,
        &mut run,
        Phase::Ci,
        &workdir,
        &mut ci_dir,
        &throttle,
        &ci_check,
        repo.as_deref(),
    );
    finish_report(ctx, project, &run, &ci_check, Phase::Ci, repo.as_deref());
    // The CI check run's page is what the UI and Discord link to.
    run.github_url = ci_check.as_ref().and_then(|c| c.url.clone());

    if !project.deploy.is_empty() {
        if ci_ok {
            let deploy_check = start_report(ctx, project, &run, repo.as_deref(), Phase::Deploy);
            run_steps(
                ctx,
                project,
                &mut run,
                Phase::Deploy,
                &workdir,
                &mut ci_dir,
                &throttle,
                &deploy_check,
                repo.as_deref(),
            );
            finish_report(
                ctx,
                project,
                &run,
                &deploy_check,
                Phase::Deploy,
                repo.as_deref(),
            );
        } else {
            // CI failed: deploy was never attempted.
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
    run: &mut Run,
    phase: Phase,
    workdir: &Path,
    ci_dir: &mut Option<PathBuf>,
    throttle: &Throttle,
    check: &Option<CheckHandle>,
    repo: Option<&str>,
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

        set_step(run, index, StepStatus::Running, None, None);
        publish(ctx, project, run, check, throttle, i, total, repo);
        let started = Instant::now();

        let outcome = match step_cfg.kind {
            StepKind::Shell => run_shell_step(
                ctx,
                project,
                step_cfg,
                run,
                index,
                workdir,
                ci_dir.as_deref(),
            ),
            StepKind::Act => {
                run_act_step(ctx, project, step_cfg, run, index, workdir, ci_dir, repo)
            }
        };

        let duration_ms = started.elapsed().as_millis() as u64;
        let (status, code) = match &outcome {
            Ok((Outcome::Success, _)) => (StepStatus::Success, Some(0)),
            Ok((Outcome::Failed(c), _)) => (StepStatus::Failure, Some(*c)),
            Ok((Outcome::Timeout, _)) => (StepStatus::Timeout, None),
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
            run.failed_step = Some(step_cfg.name.clone());
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

fn base_env(
    run: &Run,
    project: &ProjectCfg,
    workdir: &Path,
    ci_dir: Option<&Path>,
) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    env.insert("LUNACY_SHA".into(), run.sha.clone());
    env.insert("LUNACY_BRANCH".into(), project.branch.clone());
    env.insert("LUNACY_PROJECT".into(), project.name.clone());
    env.insert(
        "LUNACY_WORKDIR".into(),
        workdir.to_string_lossy().to_string(),
    );
    env.insert("LUNACY_RUN_ID".into(), run.id.clone());
    // The act clone, so a deploy step can pick up what the CI job built in it.
    if let Some(dir) = ci_dir {
        env.insert("LUNACY_CI_DIR".into(), dir.to_string_lossy().to_string());
    }
    env
}

fn run_shell_step(
    ctx: &Ctx<'_>,
    project: &ProjectCfg,
    step: &StepCfg,
    run: &Run,
    index: usize,
    workdir: &Path,
    ci_dir: Option<&Path>,
) -> Result<(Outcome, Vec<Substep>)> {
    let shell = crate::config::resolve_shell(&ctx.cfg.general, Some(project), Some(step), ctx.os);
    let (program, args) = shell.split_first().context("empty shell argv")?;
    let mut argv: Vec<String> = args.to_vec();
    argv.push(step.run.clone().unwrap_or_default());

    let mut env = base_env(run, project, workdir, ci_dir);
    env.extend(project.env.clone());
    env.extend(step.env.clone());

    let cwd = match &step.cwd {
        Some(c) if c.is_absolute() => c.clone(),
        Some(c) => workdir.join(c),
        None => workdir.to_path_buf(),
    };

    let spec = ExecSpec {
        program: program.clone(),
        args: argv,
        cwd,
        env,
        timeout: step.timeout(),
        log_path: run_log_path(ctx.state_dir, &project.name, &run.id, index),
    };
    Ok((exec::exec(&spec, None)?.outcome, Vec::new()))
}

#[allow(clippy::too_many_arguments)]
fn run_act_step(
    ctx: &Ctx<'_>,
    project: &ProjectCfg,
    step: &StepCfg,
    run: &Run,
    index: usize,
    workdir: &Path,
    ci_dir: &mut Option<PathBuf>,
    repo: Option<&str>,
) -> Result<(Outcome, Vec<Substep>)> {
    let act_cfg = ctx.cfg.act();
    let platform_map =
        act::resolve_platforms(ctx.os, &act_cfg, &project.platforms, &step.platforms);
    let workflow = step.workflow.clone().unwrap_or_default();

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

    let workflow_text = fs::read_to_string(clone_path.join(&workflow)).ok();
    let runs_on = act::job_runs_on(workflow_text.as_deref(), step.job.as_deref());
    let self_hosted = act::uses_self_hosted(&platform_map, &runs_on);
    let argv = act::act_command(&act_cfg, self_hosted);
    let wsl = !self_hosted && act_cfg.is_wsl();

    let tmp = TempDir::new().context("creating act temp files")?;
    let event_file = tmp.path().join("event.json");
    fs::write(
        &event_file,
        push_event(ctx, project, workdir, &run.sha, repo),
    )?;

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

    let plan = ActPlan {
        event_file,
        secret_file,
        var_file,
        artifact_dir,
        workflow,
        job: step.job.clone(),
        event: step.event().to_string(),
    };
    let args = act::build_args(&plan, step, &platform_map, &act_cfg, wsl);

    let (program, prefix) = argv.split_first().context("empty act command")?;
    let mut full_args: Vec<String> = prefix.to_vec();
    full_args.extend(args);
    if wsl {
        // act runs inside WSL, so the paths it receives must be WSL paths too.
        full_args = act::wsl_translate_args(&full_args);
    }

    let mut env = base_env(run, project, workdir, Some(&clone_path));
    env.extend(project.env.clone());
    env.extend(step.env.clone());

    let log_path = run_log_path(ctx.state_dir, &project.name, &run.id, index);
    if let Some(parent) = log_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let human_log = Arc::new(Mutex::new(fs::File::create(&log_path)?));
    let progress = Arc::new(Mutex::new(ActProgress::new()));

    // act writes JSON events; the per-step log keeps their `msg` text instead.
    let sink: LineSink = {
        let progress = progress.clone();
        let human_log = human_log.clone();
        Arc::new(move |line: &str| {
            let Ok(mut p) = progress.lock() else { return };
            let text = match p.feed(line) {
                Some(msg) => p.human_line(msg.trim_end()),
                None => line.to_string(),
            };
            if text.is_empty() {
                return;
            }
            if let Ok(mut f) = human_log.lock() {
                let _ = writeln!(f, "{text}");
            }
        })
    };

    // One act run at a time on this machine: act names containers after the
    // workflow and job, so parallel runs destroy each other.
    let _lock = ActLock::acquire(ctx.act_lock)?;
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
        timeout: step.timeout(),
        // Raw act output is throwaway; the human log above is the real one.
        log_path: tmp.path().join("act-raw.log"),
    };
    let result = exec::exec(&spec, Some(sink))?;

    let substeps = progress.lock().map(|p| p.substeps()).unwrap_or_default();
    Ok((result.outcome, substeps))
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
    act::event_json(
        sha,
        &before,
        &project.branch,
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

impl ActLock {
    fn acquire(path: &Path) -> Result<ActLock> {
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
        file.lock_exclusive()
            .with_context(|| format!("locking {}", path.display()))?;
        Ok(ActLock { file })
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
            RunStatus::Running => "still running",
        },
        run.duration_secs()
    );
}
