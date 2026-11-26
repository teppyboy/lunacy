//! A real `act` run, exercising the act backend end to end.
//!
//! Ignored by default because it needs `act` and a running Docker daemon.
//! Run with `cargo test --test act_integration -- --ignored --nocapture`.

mod common;

use lunacy::config::{ActCfg, Checkout, Config, Os, ProjectCfg, StepCfg, StepKind};
use lunacy::model::{RunStatus, StepStatus, Trigger};
use lunacy::pipeline::{self, Ctx};

fn have(command: &str, args: &[&str]) -> bool {
    std::process::Command::new(command)
        .args(args)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[test]
#[ignore = "needs act and a running docker daemon"]
fn act_step_runs_the_workflow_and_records_substeps() {
    if !have("act", &["--version"]) {
        eprintln!("skipping: act is not installed");
        return;
    }
    if !have("docker", &["info", "--format", "{{.ServerVersion}}"]) {
        eprintln!("skipping: docker is not reachable");
        return;
    }

    let fx = common::Fixture::new();
    let state = tempfile::tempdir().expect("state");
    let state_dir = state.path().to_path_buf();

    // Minimal workflow: no actions/checkout, no network, just echo.
    fx.commit(
        ".github/workflows/ci.yml",
        "name: lunacy integration\non: [push]\njobs:\n  build:\n    runs-on: ubuntu-latest\n    steps:\n      - name: Say hello\n        run: echo hello-from-act\n      - name: Fail on purpose? no\n        run: echo second-step\n",
        "ci: add a workflow",
    );
    let sha = fx.tip();
    lunacy::git::fetch(&fx.work, "origin", "master").expect("fetch");

    let mut cfg = Config::default();
    cfg.general.state_dir = Some(state_dir.clone());
    // Everything runs inside the container image, so `-self-hosted` keeps act
    // from pulling a runner image it cannot map.
    cfg.act = Some(ActCfg {
        command: Some(vec!["act".into()]),
        native_command: None,
        platforms: {
            let mut m = std::collections::BTreeMap::new();
            m.insert("ubuntu-latest".to_string(), "-self-hosted".to_string());
            m
        },
        args: Vec::new(),
        lock_file: None,
        cache: true,
        cache_dir: None,
    });

    let project = ProjectCfg {
        name: "actdemo".into(),
        repo_path: fx.work.clone(),
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
        ci: vec![StepCfg {
            name: "Workflow".into(),
            kind: StepKind::Act,
            run: None,
            workflow: Some(".github/workflows/ci.yml".into()),
            job: Some("build".into()),
            event: None,
            platforms: Default::default(),
            args: Vec::new(),
            secrets: Default::default(),
            vars: Default::default(),
            only_on: Vec::new(),
            timeout: Some(900),
            cwd: None,
            shell: None,
            env: Default::default(),
        }],
        deploy: Vec::new(),
    };

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
        on_log_chunk: None,
    };
    let run = pipeline::run_pipeline(&ctx, &project, &sha, Trigger::Manual);

    assert_eq!(run.status, RunStatus::Success, "steps: {:?}", run.steps);
    assert_eq!(run.steps[0].status, StepStatus::Success);
    let substeps = &run.steps[0].substeps;
    assert!(!substeps.is_empty(), "act output must turn into substeps");
    assert!(
        substeps.iter().any(|s| s.name.contains("Say hello")),
        "substep names: {:?}",
        substeps.iter().map(|s| &s.name).collect::<Vec<_>>()
    );

    // The artifact server directory is created per run.
    let artifacts = state_dir.join("artifacts");
    assert!(artifacts.is_dir(), "{}", artifacts.display());
    let log = lunacy::model::run_log_path(&state_dir, &run.project, &run.id, 0);
    let text = std::fs::read_to_string(&log).expect("log");
    assert!(text.contains("hello-from-act") || !text.is_empty());
}
