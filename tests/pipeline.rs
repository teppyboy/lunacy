//! Pipeline behaviour against a throwaway git repo with `checkout = "none"`.

mod common;

use lunacy::config::{Checkout, Config, Os, ProjectCfg, StepCfg, StepKind};
use lunacy::model::{read_last_sha, RunStatus, StepStatus, Trigger};
use lunacy::pipeline::{self, Ctx};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

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
        timeout: Some(60),
        cwd: None,
        shell: Some(vec!["bash".into(), "-c".into()]),
        env: Default::default(),
    }
}

/// A project whose CI runs `ci_run` and whose deploy touches `deploy_marker`.
fn project(fx: &common::Fixture, state_dir: &std::path::Path, ci_run: &str) -> ProjectCfg {
    let deploy_marker = state_dir.join("deploy-ran");
    ProjectCfg {
        name: "demo".into(),
        repo_path: fx.work.clone(),
        remote: "origin".into(),
        branch: "master".into(),
        github_repo: None,
        checkout: Checkout::None,
        poll_interval: None,
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
        // `Test` is the interesting step; `After` exists only to prove a
        // failing `Test` skips everything behind it.
        ci: vec![
            shell_step("Prepare", "true"),
            shell_step("Test", ci_run),
            shell_step("After", "true"),
        ],
        deploy: vec![shell_step(
            "Publish",
            &format!("touch {}", deploy_marker.display()),
        )],
    }
}

fn config(fx: &common::Fixture, state_dir: &std::path::Path) -> Config {
    let mut cfg = Config::default();
    cfg.general.state_dir = Some(state_dir.to_path_buf());
    cfg.server = None;
    cfg.github = None;
    cfg.discord = None;
    let _ = fx;
    cfg
}

fn run_pipeline(
    ctx_cfg: &Config,
    state_dir: &std::path::Path,
    p: &ProjectCfg,
    sha: &str,
) -> lunacy::model::Run {
    let updates = Arc::new(AtomicUsize::new(0));
    let counter = updates.clone();
    let on_update = move |_run: &lunacy::model::Run| {
        counter.fetch_add(1, Ordering::SeqCst);
    };
    let act_lock = state_dir.join("act.lock");
    let ctx = Ctx {
        cfg: ctx_cfg,
        state_dir,
        os: Os::current(),
        github: None,
        on_update: &on_update,
        act_lock: &act_lock,
        cancel: None,
    };
    let run = pipeline::run_pipeline(&ctx, p, sha, Trigger::Manual);
    assert!(
        updates.load(Ordering::SeqCst) > 0,
        "the API must see progress updates"
    );
    run
}

#[test]
fn success_runs_ci_then_deploy_and_writes_logs() {
    let fx = common::Fixture::new();
    let state = tempfile::tempdir().expect("state");
    let state_dir = state.path().to_path_buf();
    let sha = fx.commit("a.txt", "one\n", "feat: first");
    let cfg = config(&fx, &state_dir);
    let p = project(&fx, &state_dir, "true");

    lunacy::git::fetch(&p.repo_path, "origin", "master").expect("fetch");
    let run = run_pipeline(&cfg, &state_dir, &p, &sha);

    assert_eq!(run.status, RunStatus::Success, "steps: {:?}", run.steps);
    assert_eq!(run.failed_step, None);
    assert_eq!(run.subject, "feat: first");
    assert_eq!(run.sha, sha);
    assert!(run.started_at > 0);
    assert!(run.finished_at.is_some());

    // CI: all three steps ran, in order.
    let ci: Vec<&lunacy::model::Step> = run
        .steps
        .iter()
        .filter(|s| s.phase == lunacy::model::Phase::Ci)
        .collect();
    assert_eq!(ci.len(), 3);
    assert_eq!(ci[0].status, StepStatus::Success);
    assert_eq!(ci[1].status, StepStatus::Success);
    assert_eq!(ci[2].status, StepStatus::Success);
    assert_eq!(ci[2].name, "After");
    // Deploy ran because CI passed.
    let deploy: Vec<&lunacy::model::Step> = run
        .steps
        .iter()
        .filter(|s| s.phase == lunacy::model::Phase::Deploy)
        .collect();
    assert_eq!(deploy.len(), 1);
    assert_eq!(deploy[0].status, StepStatus::Success);
    assert!(
        state_dir.join("deploy-ran").is_file(),
        "deploy marker written"
    );

    // Each step's full output lives in the state dir, and the run JSON is persisted.
    for (i, _) in run.steps.iter().enumerate() {
        let log = lunacy::model::run_log_path(&state_dir, &run.project, &run.id, i);
        assert!(
            log.is_file(),
            "missing log for step {i} at {}",
            log.display()
        );
    }
    let json = lunacy::model::run_json_path(&state_dir, &run.project, &run.id);
    assert!(json.is_file());
    let reread = lunacy::model::find_run(&state_dir, "demo", &run.id).expect("persisted run");
    assert_eq!(reread.status, RunStatus::Success);
}

#[test]
fn ci_failure_skips_deploy_and_remaining_ci_steps() {
    let fx = common::Fixture::new();
    let state = tempfile::tempdir().expect("state");
    let state_dir = state.path().to_path_buf();
    let sha = fx.commit("a.txt", "two\n", "fix: breaks");
    let cfg = config(&fx, &state_dir);
    let p = project(&fx, &state_dir, "echo boom >&2; exit 3");

    lunacy::git::fetch(&p.repo_path, "origin", "master").expect("fetch");
    let run = run_pipeline(&cfg, &state_dir, &p, &sha);

    assert_eq!(run.status, RunStatus::Failure);
    assert_eq!(run.failed_step.as_deref(), Some("Test"));
    let ci: Vec<_> = run
        .steps
        .iter()
        .filter(|s| s.phase == lunacy::model::Phase::Ci)
        .collect();
    assert_eq!(ci[0].status, StepStatus::Success);
    assert_eq!(ci[1].status, StepStatus::Failure);
    assert_eq!(ci[1].exit_code, Some(3));
    assert_eq!(
        ci[2].status,
        StepStatus::Skipped,
        "later CI steps never run"
    ); // Deploy never starts: the step is recorded skipped, not run and not failed.
    let deploy: Vec<_> = run
        .steps
        .iter()
        .filter(|s| s.phase == lunacy::model::Phase::Deploy)
        .collect();
    assert_eq!(deploy[0].status, StepStatus::Skipped);
    assert!(
        !state_dir.join("deploy-ran").exists(),
        "deploy must not run"
    );

    // The failing step's output is captured.
    let log = lunacy::model::run_log_path(&state_dir, &run.project, &run.id, 1);
    let text = std::fs::read_to_string(&log).expect("log");
    assert!(text.contains("boom"), "{text}");
}

#[test]
fn last_sha_advances_once_for_a_failing_commit() {
    let fx = common::Fixture::new();
    let state = tempfile::tempdir().expect("state");
    let state_dir = state.path().to_path_buf();
    let cfg = config(&fx, &state_dir);
    let sha = fx.commit("a.txt", "three\n", "fix: still broken");
    let p = project(&fx, &state_dir, "exit 1");

    lunacy::git::fetch(&p.repo_path, "origin", "master").expect("fetch");

    // Nothing processed yet.
    assert!(read_last_sha(&state_dir, "demo").is_none());

    let run = run_pipeline(&cfg, &state_dir, &p, &sha);
    assert_eq!(run.status, RunStatus::Failure);
    // State semantics: the sha is recorded even though the pipeline failed, so
    // the poll loop does not retry this commit forever.
    lunacy::model::write_last_sha(&state_dir, "demo", &sha).expect("state");
    assert_eq!(
        read_last_sha(&state_dir, "demo").as_deref(),
        Some(sha.as_str())
    );

    // A later successful commit moves it forward.
    let next = fx.commit("a.txt", "four\n", "fix: green");
    lunacy::git::fetch(&p.repo_path, "origin", "master").expect("fetch");
    let ok_project = project(&fx, &state_dir, "true");
    let run2 = run_pipeline(&cfg, &state_dir, &ok_project, &next);
    assert_eq!(run2.status, RunStatus::Success, "{:?}", run2.steps);
    lunacy::model::write_last_sha(&state_dir, "demo", &next).expect("state");
    assert_eq!(
        read_last_sha(&state_dir, "demo").as_deref(),
        Some(next.as_str())
    );
}

#[test]
fn timeout_marks_the_step_and_kills_it() {
    let fx = common::Fixture::new();
    let state = tempfile::tempdir().expect("state");
    let state_dir = state.path().to_path_buf();
    let sha = fx.commit("a.txt", "five\n", "chore: hang");
    let cfg = config(&fx, &state_dir);

    let mut p = project(&fx, &state_dir, "sleep 30");
    p.ci[1].timeout = Some(1);

    lunacy::git::fetch(&p.repo_path, "origin", "master").expect("fetch");
    let started = std::time::Instant::now();
    let run = run_pipeline(&cfg, &state_dir, &p, &sha);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(20),
        "the timeout must kill the step promptly"
    );
    assert_eq!(run.status, RunStatus::Failure);
    assert_eq!(run.steps[1].status, StepStatus::Timeout);
    assert_eq!(run.steps[1].exit_code, None);
    let log = lunacy::model::run_log_path(&state_dir, &run.project, &run.id, 1);
    assert!(std::fs::read_to_string(&log).unwrap().contains("timed out"));
}

#[test]
fn only_on_skips_foreign_steps_without_failing_ci() {
    let fx = common::Fixture::new();
    let state = tempfile::tempdir().expect("state");
    let state_dir = state.path().to_path_buf();
    let sha = fx.commit("a.txt", "six\n", "test: only_on");
    let cfg = config(&fx, &state_dir);

    let mut p = project(&fx, &state_dir, "true");
    // `exit 99` would fail, but this host is not Windows, so it is skipped.
    p.ci.push({
        let mut s = shell_step("Windows only", "exit 99");
        s.only_on = vec![Os::Windows];
        s
    });
    lunacy::git::fetch(&p.repo_path, "origin", "master").expect("fetch");
    let run = run_pipeline(&cfg, &state_dir, &p, &sha);
    if Os::current() == Os::Windows {
        return; // the step runs there and this fixture uses bash
    }
    assert_eq!(run.status, RunStatus::Success, "{:?}", run.steps);
    let skipped = run
        .steps
        .iter()
        .find(|s| s.name == "Windows only")
        .expect("step");
    assert_eq!(skipped.status, StepStatus::Skipped);
}

#[test]
fn reporting_is_disabled_without_token_or_webhook() {
    let cfg = Config::default();
    assert!(cfg.github.is_none());
    assert!(cfg.discord.is_none());
    // No discord URL for a project with no global default.
    let p = ProjectCfg {
        name: "x".into(),
        repo_path: PathBuf::from("/tmp"),
        remote: "origin".into(),
        branch: "master".into(),
        github_repo: None,
        checkout: Checkout::None,
        poll_interval: None,
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
        ci: vec![shell_step("a", "true")],
        deploy: Vec::new(),
    };
    assert!(cfg.discord_for(&p).is_none());
    // A github section with neither token nor app reports nothing.
    let cfg = Config {
        github: Some(lunacy::config::GithubCfg {
            mode: None,
            token: None,
            token_file: None,
            api_url: None,
            app: None,
        }),
        ..Config::default()
    };
    assert!(!cfg.github.as_ref().unwrap().enabled());
    // A per-project webhook alone is enough for Discord.
    let mut p2 = p.clone();
    p2.discord_webhook_url = Some("https://discord.com/api/webhooks/x/y".into());
    assert!(cfg.discord_for(&p2).is_some());
}

#[test]
fn in_place_checkout_refuses_a_dirty_worktree() {
    let fx = common::Fixture::new();
    let state = tempfile::tempdir().expect("state");
    let state_dir = state.path().to_path_buf();
    let sha = fx.commit("a.txt", "seven\n", "test: dirty");
    let _ = &sha;
    let cfg = config(&fx, &state_dir);

    // An untracked file makes the worktree dirty.
    std::fs::write(fx.work.join("untracked.txt"), "x").expect("write");
    let mut p = project(&fx, &state_dir, "true");
    p.checkout = Checkout::InPlace;

    lunacy::git::fetch(&p.repo_path, "origin", "master").expect("fetch");
    let run = run_pipeline(&cfg, &state_dir, &p, &sha);
    assert_eq!(run.status, RunStatus::Error, "{:?}", run.steps);
    assert!(
        run.failed_step.as_deref().unwrap_or("").contains("dirty"),
        "got {:?}",
        run.failed_step
    );
}

#[test]
fn worktree_checkout_is_cleaned_up_and_sees_the_commit() {
    let fx = common::Fixture::new();
    let state = tempfile::tempdir().expect("state");
    let state_dir = state.path().to_path_buf();
    let sha = fx.commit("marker.txt", "in-tree\n", "feat: worktree");
    let cfg = config(&fx, &state_dir);

    let mut p = project(&fx, &state_dir, "test -f marker.txt");
    p.checkout = Checkout::Worktree;

    lunacy::git::fetch(&p.repo_path, "origin", "master").expect("fetch");
    let run = run_pipeline(&cfg, &state_dir, &p, &sha);
    assert_eq!(run.status, RunStatus::Success, "{:?}", run.steps);

    // No leftover worktree registrations or directories.
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(&fx.work)
        .args(["worktree", "list", "--porcelain"])
        .output()
        .expect("worktree list");
    let listing = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        listing.matches("worktree ").count(),
        1,
        "only the main worktree should remain: {listing}"
    );
}

/// A fake `act`: ignores its arguments, writes `build/web/index.html` into the
/// working directory (the run's clone, the way a real job with `--bind` would),
/// and prints one act JSON line so the substep parser runs too.
fn fake_act(dir: &std::path::Path) -> PathBuf {
    let path = dir.join("fake-act");
    std::fs::write(
        &path,
        "#!/usr/bin/env bash\nset -e\nmkdir -p build/web\nprintf 'built\\n' > build/web/index.html\n\
         printf '%s\\n' '{\"job\":\"CI/web\",\"jobID\":\"web\",\"step\":\"Build\",\"stepID\":[\"0\"],\"msg\":\"\\u2b50 Run Main Build\"}'\n\
         printf '%s\\n' '{\"step\":\"Build\",\"stepResult\":\"success\",\"executionTime\":1000}'\n",
    )
    .expect("write fake act");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    path
}

#[test]
fn act_clone_is_reused_for_deploy_and_removed_afterwards() {
    if cfg!(windows) {
        return; // the fixture steps use bash
    }
    let fx = common::Fixture::new();
    let state = tempfile::tempdir().expect("state");
    let state_dir = state.path().to_path_buf();
    let sha = fx.commit("a.txt", "eight\n", "feat: act clone");
    let act = fake_act(state.path());

    let mut cfg = config(&fx, &state_dir);
    cfg.act = Some(lunacy::config::ActCfg {
        command: Some(vec![act.to_string_lossy().to_string()]),
        ..lunacy::config::ActCfg::default()
    });

    let mut p = project(&fx, &state_dir, "true");
    p.ci = vec![StepCfg {
        name: "Workflow".into(),
        kind: StepKind::Act,
        run: None,
        workflow: Some(".github/workflows/ci.yml".into()),
        job: Some("web".into()),
        event: None,
        platforms: Default::default(),
        args: Vec::new(),
        secrets: Default::default(),
        vars: Default::default(),
        only_on: Vec::new(),
        timeout: Some(60),
        cwd: None,
        shell: None,
        env: Default::default(),
    }];
    // The deploy step sees the artifact the CI job wrote into the clone, and
    // records the directory so the test can check it is gone afterwards.
    let seen = state_dir.join("ci-dir-seen");
    p.deploy = vec![shell_step(
        "Publish",
        &format!(
            "set -eu; test -n \"${{LUNACY_CI_DIR:-}}\"; \
             test -f \"$LUNACY_CI_DIR/build/web/index.html\"; \
             printf '%s' \"$LUNACY_CI_DIR\" > {}",
            seen.display()
        ),
    )];

    lunacy::git::fetch(&p.repo_path, "origin", "master").expect("fetch");
    let run = run_pipeline(&cfg, &state_dir, &p, &sha);
    assert_eq!(run.status, RunStatus::Success, "steps: {:?}", run.steps);
    assert_eq!(run.steps[0].status, StepStatus::Success);
    assert!(!run.steps[0].substeps.is_empty(), "act JSON parsed");

    let ci_dir = std::fs::read_to_string(&seen).expect("deploy saw LUNACY_CI_DIR");
    assert_eq!(
        PathBuf::from(&ci_dir),
        state_dir.join("checkouts").join(&run.id).join("repo"),
        "the clone lives under the run's state dir"
    );
    assert!(
        !state_dir.join("checkouts").join(&run.id).exists(),
        "the act clone is removed when the pipeline finishes"
    );
}

#[test]
fn no_act_step_means_no_ci_dir_in_the_environment() {
    if cfg!(windows) {
        return;
    }
    let fx = common::Fixture::new();
    let state = tempfile::tempdir().expect("state");
    let state_dir = state.path().to_path_buf();
    let sha = fx.commit("a.txt", "nine\n", "feat: shell only");
    let cfg = config(&fx, &state_dir);
    let marker = state_dir.join("ci-dir-absent");
    let mut p = project(&fx, &state_dir, "true");
    p.deploy = vec![shell_step(
        "Publish",
        &format!(
            "test -z \"${{LUNACY_CI_DIR:-}}\" && touch {}",
            marker.display()
        ),
    )];

    lunacy::git::fetch(&p.repo_path, "origin", "master").expect("fetch");
    let run = run_pipeline(&cfg, &state_dir, &p, &sha);
    assert_eq!(run.status, RunStatus::Success, "steps: {:?}", run.steps);
    assert!(
        marker.is_file(),
        "LUNACY_CI_DIR is unset without an act step"
    );
}

#[cfg(unix)]
#[test]
fn cancelling_running_step_kills_process_and_skips_remaining() {
    use std::sync::atomic::AtomicBool;
    use std::time::{Duration, Instant};

    let fx = common::Fixture::new();
    let state = tempfile::tempdir().expect("state");
    let state_dir = state.path().to_path_buf();
    let sha = fx.commit("a.txt", "ten\n", "feat: cancellable");
    let cfg = config(&fx, &state_dir);

    let survivor = state_dir.join("survivor");
    let started_flag = state_dir.join("started");
    let script = format!(
        "( sleep 8; touch {} ) & touch {}; sleep 30",
        survivor.display(),
        started_flag.display()
    );
    let p = project(&fx, &state_dir, &script);
    lunacy::git::fetch(&p.repo_path, "origin", "master").expect("fetch");

    let cancel = Arc::new(AtomicBool::new(false));
    let cancel_trigger = cancel.clone();
    let started_flag_watch = started_flag.clone();
    std::thread::spawn(move || {
        let wait_deadline = Instant::now() + Duration::from_secs(10);
        while !started_flag_watch.is_file() && Instant::now() < wait_deadline {
            std::thread::sleep(Duration::from_millis(25));
        }
        std::thread::sleep(Duration::from_millis(50));
        cancel_trigger.store(true, Ordering::SeqCst);
    });

    let on_update = |_run: &lunacy::model::Run| {};
    let act_lock = state_dir.join("act.lock");
    let ctx = Ctx {
        cfg: &cfg,
        state_dir: &state_dir,
        os: Os::current(),
        github: None,
        on_update: &on_update,
        act_lock: &act_lock,
        cancel: Some(&cancel),
    };

    let started = Instant::now();
    let run = pipeline::run_pipeline(&ctx, &p, &sha, Trigger::Manual);
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "cancel must abort promptly"
    );
    assert_eq!(run.status, RunStatus::Cancelled, "steps: {:?}", run.steps);
    assert_eq!(run.failed_step, None);
    assert!(run.failed().is_none());
    assert_eq!(run.steps[0].status, StepStatus::Success);
    assert_eq!(run.steps[1].status, StepStatus::Cancelled);
    assert_eq!(run.steps[1].exit_code, None);
    assert_eq!(run.steps[2].status, StepStatus::Skipped);
    assert_eq!(run.steps[3].status, StepStatus::Skipped);
    assert!(!state_dir.join("deploy-ran").exists());

    let log = lunacy::model::run_log_path(&state_dir, &run.project, &run.id, 1);
    let text = std::fs::read_to_string(&log).expect("log");
    assert!(
        text.contains("[lunacy] cancelled; killed the process tree"),
        "{text}"
    );
    std::thread::sleep(Duration::from_millis(200));
    assert!(!survivor.exists(), "grandchild survived cancel");
}

#[cfg(unix)]
#[test]
fn project_timeout_enforces_pipeline_wide_deadline() {
    use std::time::{Duration, Instant};

    let fx = common::Fixture::new();
    let state = tempfile::tempdir().expect("state");
    let state_dir = state.path().to_path_buf();
    let sha = fx.commit("a.txt", "eleven\n", "chore: pipeline timeout");
    let cfg = config(&fx, &state_dir);

    let mut p = project(&fx, &state_dir, "sleep 30");
    // Step timeouts are generous (60s), but total pipeline timeout is 2s.
    p.timeout = Some(2);
    p.ci[0] = shell_step("Prepare", "sleep 1");
    p.ci[1] = shell_step("Test", "sleep 30");
    p.ci[2] = shell_step("After", "true");

    lunacy::git::fetch(&p.repo_path, "origin", "master").expect("fetch");
    let started = Instant::now();
    let run = run_pipeline(&cfg, &state_dir, &p, &sha);
    assert!(
        started.elapsed() < Duration::from_secs(12),
        "pipeline deadline must bound total runtime"
    );
    assert_eq!(run.status, RunStatus::Failure, "steps: {:?}", run.steps);
    assert_eq!(run.steps[0].status, StepStatus::Success);
    assert_eq!(run.steps[1].status, StepStatus::Timeout);
    assert_eq!(run.steps[2].status, StepStatus::Skipped);
    assert_eq!(run.steps[3].status, StepStatus::Skipped);
    assert!(!state_dir.join("deploy-ran").exists());
}

#[cfg(unix)]
#[test]
fn act_lock_wait_is_cancellable() {
    use fs2::FileExt;
    use std::sync::atomic::AtomicBool;
    use std::time::{Duration, Instant};

    let fx = common::Fixture::new();
    let state = tempfile::tempdir().expect("state");
    let state_dir = state.path().to_path_buf();
    let sha = fx.commit("a.txt", "twelve\n", "chore: lock cancel");
    let act = fake_act(state.path());
    let mut cfg = config(&fx, &state_dir);
    cfg.act = Some(lunacy::config::ActCfg {
        command: Some(vec![act.to_string_lossy().to_string()]),
        ..lunacy::config::ActCfg::default()
    });

    let mut p = project(&fx, &state_dir, "true");
    p.ci = vec![StepCfg {
        name: "Workflow".into(),
        kind: StepKind::Act,
        run: None,
        workflow: Some(".github/workflows/ci.yml".into()),
        job: Some("web".into()),
        event: None,
        platforms: Default::default(),
        args: Vec::new(),
        secrets: Default::default(),
        vars: Default::default(),
        only_on: Vec::new(),
        timeout: Some(60),
        cwd: None,
        shell: None,
        env: Default::default(),
    }];
    lunacy::git::fetch(&p.repo_path, "origin", "master").expect("fetch");

    // Hold the act lock exclusively so the pipeline blocks waiting for it.
    let act_lock = state_dir.join("act.lock");
    let held_file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&act_lock)
        .unwrap();
    held_file.lock_exclusive().unwrap();

    let cancel = Arc::new(AtomicBool::new(false));
    let cancel_trigger = cancel.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(250));
        cancel_trigger.store(true, Ordering::SeqCst);
    });

    let on_update = |_run: &lunacy::model::Run| {};
    let ctx = Ctx {
        cfg: &cfg,
        state_dir: &state_dir,
        os: Os::current(),
        github: None,
        on_update: &on_update,
        act_lock: &act_lock,
        cancel: Some(&cancel),
    };
    let started = Instant::now();
    let run = pipeline::run_pipeline(&ctx, &p, &sha, Trigger::Manual);
    drop(held_file);

    assert!(
        started.elapsed() < Duration::from_secs(5),
        "ActLock wait must respect cancel"
    );
    assert_eq!(run.status, RunStatus::Cancelled);
    assert_eq!(run.steps[0].status, StepStatus::Cancelled);
    assert_eq!(run.steps[1].status, StepStatus::Skipped);
}

#[cfg(unix)]
#[test]
fn pr_run_is_ci_only_uses_worktree_and_skips_deploy_github_check() {
    use lunacy::config::{GithubCfg, GithubMode};
    use lunacy::pipeline::{PrContext, RunSpec};
    use lunacy::report::Github;
    use std::sync::Mutex;

    let fx = common::Fixture::new();
    let state = tempfile::tempdir().expect("state");
    let state_dir = state.path().to_path_buf();
    let base_sha = fx.commit("base.txt", "base\n", "chore: base");

    // Create a PR commit on a separate branch and push it to `refs/pull/42/head` in bare origin,
    // then switch the working clone back to `master` so `fx.work` does NOT have `pr.txt`.
    common::run(&fx.work, &["checkout", "-q", "-b", "feat/pr-42"]);
    std::fs::write(fx.work.join("pr.txt"), "from-pr\n").unwrap();
    common::run(&fx.work, &["add", "pr.txt"]);
    common::run(&fx.work, &["commit", "-q", "-m", "feat: pr change"]);
    let pr_sha = {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&fx.work)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    common::run(
        &fx.work,
        &["push", "-q", "origin", "HEAD:refs/pull/42/head"],
    );
    common::run(&fx.work, &["checkout", "-q", "master"]);
    assert!(!fx.work.join("pr.txt").exists());

    // Verify `git::fetch_pr` works against `refs/pull/42/head`.
    lunacy::git::fetch_pr(&fx.work, "origin", 42, &pr_sha).expect("fetch_pr");

    // Mock GitHub status server recording every POSTed context.
    let gh_server = tiny_http::Server::http("127.0.0.1:0").expect("mock gh");
    let gh_addr = gh_server.server_addr().to_ip().expect("addr");
    let recorded_contexts: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let recorded_clone = recorded_contexts.clone();
    let stop_gh = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop_gh_clone = stop_gh.clone();
    let gh_thread = std::thread::spawn(move || {
        while !stop_gh_clone.load(Ordering::SeqCst) {
            if let Ok(Some(mut req)) = gh_server.recv_timeout(std::time::Duration::from_millis(50))
            {
                let mut body = String::new();
                let _ = req.as_reader().read_to_string(&mut body);
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) {
                    if let Some(ctx) = v.get("context").and_then(|c| c.as_str()) {
                        recorded_clone.lock().unwrap().push(ctx.to_string());
                    }
                }
                let _ = req.respond(tiny_http::Response::from_string("{}").with_status_code(201));
            }
        }
    });

    let mut cfg = config(&fx, &state_dir);
    let gh_cfg = GithubCfg {
        mode: Some(GithubMode::Status),
        token: Some("test-token".into()),
        token_file: None,
        api_url: Some(format!("http://{gh_addr}")),
        app: None,
    };
    let gh = Github::new(&gh_cfg).expect("gh client");
    cfg.github = Some(gh_cfg);

    let workdir_seen = state_dir.join("pr-workdir");
    let mut p = project(
        &fx,
        &state_dir,
        &format!(
            "test -f pr.txt && test -n \"${{LUNACY_CACHE_DIR:-}}\" && test \"${{DOCKER_BUILDKIT:-}}\" = 1 && printf '%s' \"$LUNACY_WORKDIR\" > {}",
            workdir_seen.display()
        ),
    );
    // Explicitly `checkout = "none"` — PR must STILL run in an isolated worktree!
    p.checkout = Checkout::None;
    p.github_repo = Some("teppyboy/demo".into());

    let on_update = |_run: &lunacy::model::Run| {};
    let act_lock = state_dir.join("act.lock");
    let ctx = Ctx {
        cfg: &cfg,
        state_dir: &state_dir,
        os: Os::current(),
        github: Some(&gh),
        on_update: &on_update,
        act_lock: &act_lock,
        cancel: None,
    };
    let spec = RunSpec {
        sha: pr_sha.clone(),
        trigger: Trigger::PullRequest,
        git_ref: "feat/pr-42".into(),
        pr: Some(PrContext {
            number: 42,
            title: "feat: pr change".into(),
            head_ref: "feat/pr-42".into(),
            base_ref: "master".into(),
            base_sha,
        }),
        deploy_only: false,
        force_deploy: false,
    };
    let run = pipeline::run_pipeline_spec(&ctx, &p, &spec);

    stop_gh.store(true, Ordering::SeqCst);
    let _ = gh_thread.join();

    assert_eq!(run.status, RunStatus::Success, "steps: {:?}", run.steps);
    assert_eq!(run.trigger, Trigger::PullRequest);
    assert_eq!(run.pr_number, Some(42));
    assert_eq!(run.pr_title.as_deref(), Some("feat: pr change"));
    assert_eq!(run.pr_head_ref.as_deref(), Some("feat/pr-42"));
    assert_eq!(run.pr_base_ref.as_deref(), Some("master"));
    assert!(
        run.deploy_skipped_reason
            .as_deref()
            .unwrap_or("")
            .contains("PR #42"),
        "reason: {:?}",
        run.deploy_skipped_reason
    );

    // CI ran inside an isolated worktree (NOT fx.work), and fx.work was untouched.
    let seen_wd = std::fs::read_to_string(&workdir_seen).expect("workdir recorded");
    assert_ne!(PathBuf::from(&seen_wd), fx.work);
    assert!(!fx.work.join("pr.txt").exists());

    // Deploy step was skipped and marker never written.
    let deploy_step = run
        .steps
        .iter()
        .find(|s| s.phase == lunacy::model::Phase::Deploy)
        .unwrap();
    assert_eq!(deploy_step.status, StepStatus::Skipped);
    assert!(!state_dir.join("deploy-ran").exists());

    // GitHub received CI context statuses, NEVER deploy context!
    let contexts = recorded_contexts.lock().unwrap();
    assert!(
        contexts.iter().any(|c| c == "lunacy/ci"),
        "expected CI status: {contexts:?}"
    );
    assert!(
        !contexts.iter().any(|c| c == "lunacy/deploy"),
        "PR run must NOT post deploy status: {contexts:?}"
    );
}

#[cfg(unix)]
#[test]
fn force_deploy_skips_ci_standalone_and_runs_act_when_ci_dir_referenced() {
    use lunacy::pipeline::RunSpec;

    let fx = common::Fixture::new();
    let state = tempfile::tempdir().expect("state");
    let state_dir = state.path().to_path_buf();
    let sha = fx.commit("a.txt", "deploy-only\n", "feat: deploy only");
    lunacy::git::fetch(&fx.work, "origin", "master").expect("fetch");

    // 1. Standalone deploy_only (no deploy step references LUNACY_CI_DIR): skips ALL CI steps!
    let ci_marker = state_dir.join("ci-ran");
    let deploy_marker = state_dir.join("deploy-ran");
    let cfg = config(&fx, &state_dir);
    let mut p = project(&fx, &state_dir, &format!("touch {}", ci_marker.display()));
    p.ci = vec![shell_step(
        "Test",
        &format!("touch {}", ci_marker.display()),
    )];
    p.deploy = vec![shell_step(
        "Publish",
        &format!("touch {}", deploy_marker.display()),
    )];

    let on_update = |_run: &lunacy::model::Run| {};
    let act_lock = state_dir.join("act.lock");
    let ctx = Ctx {
        cfg: &cfg,
        state_dir: &state_dir,
        os: Os::current(),
        github: None,
        on_update: &on_update,
        act_lock: &act_lock,
        cancel: None,
    };
    let spec = RunSpec {
        sha: sha.clone(),
        trigger: Trigger::ForceDeploy,
        git_ref: "master".into(),
        pr: None,
        deploy_only: true,
        force_deploy: true,
    };
    let run = pipeline::run_pipeline_spec(&ctx, &p, &spec);
    assert_eq!(run.status, RunStatus::Success, "steps: {:?}", run.steps);
    assert!(run.deploy_only);
    assert_eq!(run.steps[0].status, StepStatus::Skipped);
    assert_eq!(run.steps[1].status, StepStatus::Success);
    assert!(
        !ci_marker.exists(),
        "CI must not run for standalone deploy_only"
    );
    assert!(deploy_marker.is_file(), "deploy must run");

    // 2. Deploy step references LUNACY_CI_DIR: runs ONLY act CI step(s), skips shell CI step, then deploys!
    let _ = std::fs::remove_file(&deploy_marker);
    let act_bin = fake_act(state.path());
    let mut cfg2 = config(&fx, &state_dir);
    cfg2.act = Some(lunacy::config::ActCfg {
        command: Some(vec![act_bin.to_string_lossy().to_string()]),
        ..lunacy::config::ActCfg::default()
    });
    let mut p2 = project(&fx, &state_dir, "true");
    p2.ci = vec![
        shell_step("ShellLint", &format!("touch {}", ci_marker.display())),
        StepCfg {
            name: "WebAct".into(),
            kind: StepKind::Act,
            run: None,
            workflow: Some(".github/workflows/ci.yml".into()),
            job: Some("web".into()),
            event: None,
            platforms: Default::default(),
            args: Vec::new(),
            secrets: Default::default(),
            vars: Default::default(),
            only_on: Vec::new(),
            timeout: Some(60),
            cwd: None,
            shell: None,
            env: Default::default(),
        },
    ];
    p2.deploy = vec![shell_step(
        "Publish",
        &format!(
            "test -f \"$LUNACY_CI_DIR/build/web/index.html\" && touch {}",
            deploy_marker.display()
        ),
    )];

    let ctx2 = Ctx {
        cfg: &cfg2,
        state_dir: &state_dir,
        os: Os::current(),
        github: None,
        on_update: &on_update,
        act_lock: &act_lock,
        cancel: None,
    };
    let run2 = pipeline::run_pipeline_spec(&ctx2, &p2, &spec);
    assert_eq!(run2.status, RunStatus::Success, "steps: {:?}", run2.steps);
    assert_eq!(
        run2.steps[0].status,
        StepStatus::Skipped,
        "shell CI skipped"
    );
    assert_eq!(
        run2.steps[1].status,
        StepStatus::Success,
        "act CI ran for LUNACY_CI_DIR"
    );
    assert_eq!(run2.steps[2].status, StepStatus::Success, "deploy ran");
    assert!(!ci_marker.exists());
    assert!(deploy_marker.is_file());
}

#[cfg(unix)]
#[test]
fn remote_tags_and_deploy_refs_gating() {
    use lunacy::pipeline::RunSpec;

    let fx = common::Fixture::new();
    let state = tempfile::tempdir().expect("state");
    let state_dir = state.path().to_path_buf();
    let sha = fx.commit("v.txt", "1.0\n", "release: v1.0.0");

    // Push a tag to the bare remote and verify list_remote_tags + fetch_tag.
    common::run(&fx.work, &["tag", "v1.0.0", &sha]);
    common::run(&fx.work, &["push", "-q", "origin", "refs/tags/v1.0.0"]);
    common::run(&fx.work, &["tag", "-d", "v1.0.0"]);

    let tags = lunacy::git::list_remote_tags(&fx.work, "origin").expect("list_remote_tags");
    assert!(
        tags.iter().any(|(t, s)| t == "v1.0.0" && s == &sha),
        "{tags:?}"
    );
    lunacy::git::fetch_tag(&fx.work, "origin", "v1.0.0").expect("fetch_tag");
    assert_eq!(
        lunacy::git::rev_parse(&fx.work, "refs/tags/v1.0.0").unwrap(),
        sha
    );

    // Project deploys only on tags/v*.
    let cfg = config(&fx, &state_dir);
    let mut p = project(&fx, &state_dir, "true");
    p.deploy_refs = vec!["tags/v*".into()];
    let on_update = |_run: &lunacy::model::Run| {};
    let act_lock = state_dir.join("act.lock");
    let ctx = Ctx {
        cfg: &cfg,
        state_dir: &state_dir,
        os: Os::current(),
        github: None,
        on_update: &on_update,
        act_lock: &act_lock,
        cancel: None,
    };

    // master does NOT match tags/v* -> deploy skipped with reason.
    let run_branch = pipeline::run_pipeline_spec(
        &ctx,
        &p,
        &RunSpec {
            sha: sha.clone(),
            trigger: Trigger::Poll,
            git_ref: "master".into(),
            pr: None,
            deploy_only: false,
            force_deploy: false,
        },
    );
    assert_eq!(run_branch.status, RunStatus::Success);
    assert!(run_branch
        .deploy_skipped_reason
        .as_deref()
        .unwrap_or("")
        .contains("does not match deploy_refs"));
    assert!(!state_dir.join("deploy-ran").exists());

    // tags/v1.0.0 MATCHES tags/v* -> deploy runs!
    let run_tag = pipeline::run_pipeline_spec(
        &ctx,
        &p,
        &RunSpec {
            sha,
            trigger: Trigger::Tag,
            git_ref: "tags/v1.0.0".into(),
            pr: None,
            deploy_only: false,
            force_deploy: false,
        },
    );
    assert_eq!(run_tag.status, RunStatus::Success, "{:?}", run_tag.steps);
    assert!(run_tag.deploy_skipped_reason.is_none());
    assert!(state_dir.join("deploy-ran").is_file());
}

#[cfg(unix)]
#[test]
fn startup_emits_on_update_before_checkout_and_pr_base_sha_falls_back() {
    use lunacy::pipeline::{PrContext, RunSpec};
    use std::sync::Mutex;

    let fx = common::Fixture::new();
    let state = tempfile::tempdir().expect("state");
    let state_dir = state.path().to_path_buf();
    let base_sha = fx.commit("base.txt", "base\n", "chore: base");
    lunacy::git::fetch(&fx.work, "origin", "master").expect("fetch");

    // 1. Even when checkout fails, `on_update` MUST already have fired with RunStatus::Running
    //    before `prepare_checkout`, eliminating the cancel startup race!
    let statuses = Arc::new(Mutex::new(Vec::<RunStatus>::new()));
    let statuses_clone = statuses.clone();
    let on_update = move |run: &lunacy::model::Run| {
        statuses_clone.lock().unwrap().push(run.status);
    };
    let cfg = config(&fx, &state_dir);
    let mut p = project(&fx, &state_dir, "true");
    p.checkout = Checkout::Worktree;
    let act_lock = state_dir.join("act.lock");
    let ctx = Ctx {
        cfg: &cfg,
        state_dir: &state_dir,
        os: Os::current(),
        github: None,
        on_update: &on_update,
        act_lock: &act_lock,
        cancel: None,
    };
    // Valid hex SHA that does not exist in git -> prepare_checkout fails.
    let missing_sha = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
    let err_run = pipeline::run_pipeline(&ctx, &p, missing_sha, Trigger::Manual);
    assert_eq!(err_run.status, RunStatus::Error);
    let seen = statuses.lock().unwrap().clone();
    assert!(
        seen.len() >= 2,
        "expected initial Running + final Error callbacks, got {seen:?}"
    );
    assert_eq!(seen[0], RunStatus::Running);
    assert_eq!(*seen.last().unwrap(), RunStatus::Error);

    // 2. When `pr.base_sha` is empty (e.g. manual PR trigger), `run_act_step` resolves
    //    `origin/<base_ref>` before writing `event.json`.
    let event_dump = state_dir.join("captured-event.json");
    let fake_act_script = state.path().join("capture-act");
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(
            &fake_act_script,
            format!(
                "#!/usr/bin/env bash\nset -e\nevent_file=\"\"\nwhile [ $# -gt 0 ]; do\n  if [ \"$1\" = \"-e\" ]; then event_file=\"$2\"; shift 2; else shift; fi\ndone\ncp \"$event_file\" \"{}\"\n",
                event_dump.display()
            ),
        )
        .unwrap();
        let mut perms = std::fs::metadata(&fake_act_script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&fake_act_script, perms).unwrap();
    }

    let mut cfg2 = config(&fx, &state_dir);
    cfg2.act = Some(lunacy::config::ActCfg {
        command: Some(vec![fake_act_script.to_string_lossy().to_string()]),
        cache: false,
        ..lunacy::config::ActCfg::default()
    });
    let mut p2 = project(&fx, &state_dir, "true");
    p2.ci = vec![StepCfg {
        name: "PRAct".into(),
        kind: StepKind::Act,
        run: None,
        workflow: Some(".github/workflows/ci.yml".into()),
        job: Some("web".into()),
        event: None,
        platforms: Default::default(),
        args: Vec::new(),
        secrets: Default::default(),
        vars: Default::default(),
        only_on: Vec::new(),
        timeout: Some(60),
        cwd: None,
        shell: None,
        env: Default::default(),
    }];
    p2.deploy.clear();

    let noop = |_run: &lunacy::model::Run| {};
    let ctx2 = Ctx {
        cfg: &cfg2,
        state_dir: &state_dir,
        os: Os::current(),
        github: None,
        on_update: &noop,
        act_lock: &act_lock,
        cancel: None,
    };
    let pr_spec = RunSpec {
        sha: base_sha.clone(),
        trigger: Trigger::PullRequest,
        git_ref: "feat/manual-pr".into(),
        pr: Some(PrContext {
            number: 55,
            title: "manual pr".into(),
            head_ref: "feat/manual-pr".into(),
            base_ref: "master".into(),
            base_sha: String::new(),
        }),
        deploy_only: false,
        force_deploy: false,
    };
    let pr_run = pipeline::run_pipeline_spec(&ctx2, &p2, &pr_spec);
    assert_eq!(pr_run.status, RunStatus::Success, "{:?}", pr_run.steps);
    let captured: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&event_dump).expect("event.json")).unwrap();
    assert_eq!(
        captured["pull_request"]["base"]["sha"], base_sha,
        "base_sha fallback should resolve origin/master: {captured}"
    );
}
