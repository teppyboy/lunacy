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
        env: Default::default(),
        contexts: Default::default(),
        discord_webhook_url: None,
        notify_on_start: None,
        shell: None,
        platforms: Default::default(),
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
        env: Default::default(),
        contexts: Default::default(),
        discord_webhook_url: None,
        notify_on_start: None,
        shell: None,
        platforms: Default::default(),
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
