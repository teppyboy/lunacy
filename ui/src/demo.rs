//! Offline data for the `--scene` snapshots: the same fixtures `tools/mock_server.py` serves,
//! baked in so a screenshot never depends on a daemon, a port or a token.
//!
//! `init` starts empty (a real launch reads the daemon); a scene calls [`load`] to fill the state
//! as if the daemon had answered, so the screens render in a fixed, reviewable state.

use rune::TextEdit;
use serde_json::Value;

use crate::config::Config;
use crate::model::{DeployChecksReport, Project, Run, SystemInfo, Workflow};
use crate::state::{Conn, LogMode, LogState, SettingsPage, State, View};

/// The fixtures exactly as `tools/fixtures.json` holds them.
pub const FIXTURES: &str = include_str!("../tools/fixtures.json");

fn fixtures() -> Value {
    serde_json::from_str(FIXTURES).unwrap_or(Value::Null)
}

fn parse<T: serde::de::DeserializeOwned>(v: &Value, key: &str) -> Vec<T> {
    v.get(key)
        .and_then(|x| serde_json::from_value(x.clone()).ok())
        .unwrap_or_default()
}

/// The run fixtures, newest first.
pub fn runs() -> Vec<Run> {
    rebase(parse(&fixtures(), "runs"))
}

/// The fixtures are written once and read years later, so their absolute timestamps are shifted
/// to keep the screens plausible: the newest run is "a few minutes ago", and the rest keep their
/// spacing. Without this every screen says "365d ago".
fn rebase(mut runs: Vec<Run>) -> Vec<Run> {
    let newest = runs.iter().map(|r| r.started_at).max().unwrap_or(0);
    if newest == 0 {
        return runs;
    }
    // Move the newest run to "four minutes ago"; older runs keep their spacing. The fixtures are
    // usually older than now, so the offset is added rather than subtracted.
    let target = crate::model::now_unix().saturating_sub(240);
    let move_to = |t: u64| -> u64 {
        if newest >= target {
            t.saturating_sub(newest - target)
        } else {
            t + (target - newest)
        }
    };
    for run in &mut runs {
        run.started_at = move_to(run.started_at);
        if let Some(finished) = run.finished_at {
            run.finished_at = Some(move_to(finished));
        }
    }
    runs
}

pub fn projects() -> Vec<Project> {
    parse(&fixtures(), "projects")
}

pub fn system() -> SystemInfo {
    serde_json::from_value(fixtures()["system"].clone()).unwrap_or_default()
}

pub fn deploy_checks() -> DeployChecksReport {
    serde_json::from_value(fixtures()["deploy_checks"].clone()).unwrap_or_default()
}

pub fn workflows() -> Vec<Workflow> {
    parse(&fixtures(), "workflows")
}

pub fn config() -> Config {
    serde_json::from_value(fixtures()["config"].clone()).unwrap_or_default()
}

pub fn version() -> String {
    fixtures()["version"]
        .as_str()
        .unwrap_or("0.1.0")
        .to_string()
}

/// The fixtures' log for one step, if they have one.
pub fn log(run: &str, index: usize) -> Option<String> {
    fixtures()["logs"]
        .get(format!("{run}/{index}"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// The id of the fixture project that is running.
pub fn running_run() -> Option<Run> {
    runs().into_iter().find(|r| r.status.is_running())
}

/// The newest successful fixture run.
pub fn finished_run() -> Option<Run> {
    runs()
        .into_iter()
        .find(|r| matches!(r.status, crate::model::RunStatus::Success))
}

/// The newest failed fixture run.
pub fn failed_run() -> Option<Run> {
    runs()
        .into_iter()
        .find(|r| matches!(r.status, crate::model::RunStatus::Failure))
}

/// Fills `s` as if the daemon had answered everything, with the view on `view`.
pub fn load(s: &mut State, view: View) {
    s.conn = Conn {
        server: "http://127.0.0.1:7878".into(),
        token: "dev".into(),
    };
    s.live = s.conn.clone();
    s.ui_server = TextEdit::new(s.conn.server.clone());
    s.ui_token = TextEdit::new(s.conn.token.clone());
    s.link = crate::state::Link::Ok;
    s.version = version();
    s.projects = projects();
    s.loaded = true;
    s.ready = true;
    s.demo = true;
    s.view = view.clone();

    let cfg = config();
    s.editor.base = serde_json::to_value(&cfg).unwrap_or(Value::Null);
    s.editor.draft = s.editor.base.clone();
    s.editor.loaded = true;
    s.editor.workflows = workflows();
    s.system = Some(system());
    s.deploy_checks = Some(deploy_checks());

    match view {
        View::Projects => {}
        View::Project { ref name } => {
            s.runs = runs().into_iter().filter(|r| &r.project == name).collect();
            s.runs_for = Some(name.clone());
        }
        View::Run { ref id } => {
            s.run = runs().into_iter().find(|r| &r.id == id);
            if let Some(run) = &s.run {
                // Open the act step when there is one: it is the interesting one.
                s.open_step = run
                    .steps
                    .iter()
                    .position(|st| {
                        matches!(st.kind, crate::model::StepKind::Act)
                            && matches!(st.status, crate::model::StepStatus::Running)
                    })
                    .or_else(|| {
                        run.steps
                            .iter()
                            .position(|st| matches!(st.kind, crate::model::StepKind::Act))
                    });
                if let Some(index) = s.open_step {
                    if let Some(text) = log(&run.id, index) {
                        s.logs.insert(
                            (run.id.clone(), index),
                            LogState {
                                text,
                                loading: false,
                                error: None,
                                mode: Some(LogMode::Tail),
                                offset: 0,
                            },
                        );
                    }
                }
            }
            s.runs = runs()
                .into_iter()
                .filter(|r| {
                    r.project
                        == s.run
                            .as_ref()
                            .map(|x| x.project.clone())
                            .unwrap_or_default()
                })
                .collect();
        }
        View::Settings { page } => {
            s.editor.page = page;
            match page {
                SettingsPage::Projects => s.editor.project = Some(0),
                SettingsPage::Diagnostics | SettingsPage::Connection | SettingsPage::Appearance => {
                }
                _ => {}
            }
        }
    }
}

/// The step editor scene: project 0, its CI phase, the act step open.
pub fn editor_step(s: &mut State) {
    load(
        s,
        View::Settings {
            page: SettingsPage::Projects,
        },
    );
    s.editor.project = Some(0);
    s.editor.step = Some((0, false, 1));
    s.editor.workflows_for = Some(crate::state::step_path(0, false, 1));
}

/// A rejected save: the daemon's 400 on a field, with its message.
pub fn rejected(s: &mut State) {
    editor_step(s);
    s.editor.field = Some(format!("{}.run", crate::state::step_path(0, false, 0)));
    s.editor.error = Some("step needs a run line".into());
    s.editor.draft["project"][0]["ci"][0]["run"] = Value::String(String::new());
    s.editor.reseed();
}

/// The projects list with a toast, as a run start or a trigger failure shows it.
pub fn with_toast(s: &mut State) {
    load(s, View::Projects);
    s.toasts.clear();
    s.next_toast = 2;
    s.toasts.push(crate::state::Toast {
        id: 1,
        title: "Run started".into(),
        body: "lunarum picked up work.".into(),
        kind: crate::state::ToastKind::Info,
        until: 3_600.0,
    });
}

/// The unreachable-daemon state, so the banner and the retry affordance are reviewable.
pub fn offline(s: &mut State) {
    load(s, View::Projects);
    s.link = crate::state::Link::Down {
        why: "connection refused (os error 111)".into(),
        failures: 3,
    };
    s.projects.clear();
    s.loaded = false;
    s.projects_error = Some("Cannot reach the daemon: connection refused (os error 111)".into());
}

/// The wrong-token state.
pub fn unauthorized(s: &mut State) {
    load(s, View::Projects);
    s.link = crate::state::Link::Unauthorized;
    s.projects_error = Some("The daemon rejected the token (401). Check it in Connection.".into());
}

/// The GitHub repo a demo project deploys from, so a run without `github_url` can still link its
/// commit. Live runs use the project's own `github_repo` instead.
pub fn repo_for(project: &str) -> Option<String> {
    fixtures()
        .get("projects")
        .and_then(|p| p.as_array())
        .and_then(|p| {
            p.iter()
                .find(|p| p.get("name").and_then(|n| n.as_str()) == Some(project))
        })
        .and_then(|p| p.get("github_repo"))
        .and_then(|r| r.as_str())
        .filter(|r| !r.is_empty())
        .map(str::to_string)
}
