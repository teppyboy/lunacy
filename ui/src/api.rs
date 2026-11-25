//! Every endpoint the UI calls, one function each, matching `docs/api.md` § Endpoints.
//!
//! Native functions block and are meant for `rune::Latest`/`rune::task::spawn`; the web ones are
//! `async` and run under `spawn_local`. `app.rs` owns the plumbing; nothing here stores state.

use serde_json::Value;

use crate::config::{Config, ConfigOutcome};
use crate::model::{Health, Project, Run, SystemInfo, Workflow};
use crate::net::{self, Failure, Reply, Result};

/// A request the UI wants made.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Call {
    Projects,
    Project(String),
    Runs(String, usize),
    Run(String),
    Log {
        run: String,
        index: usize,
        tail: Option<usize>,
    },
    Trigger {
        project: String,
        force: bool,
    },
    Config,
    SaveConfig(String),
    System,
    Workflows {
        project: Option<String>,
        repo_path: Option<String>,
    },
    Health,
}

impl Call {
    /// A stable key for `Latest`'s latests-wins bookkeeping.
    pub fn key(&self) -> String {
        format!("{self:?}")
    }
}

fn path(base: &str, call: &Call) -> String {
    match call {
        Call::Projects => net::join(base, "/api/projects"),
        Call::Project(name) => net::join(base, &format!("/api/projects/{}", net::seg(name))),
        Call::Runs(name, limit) => net::join(
            base,
            &format!("/api/projects/{}/runs?limit={limit}", net::seg(name)),
        ),
        Call::Run(id) => net::join(base, &format!("/api/runs/{}", net::seg(id))),
        Call::Log { run, index, tail } => match tail {
            Some(tail) => net::join(
                base,
                &format!("/api/runs/{}/steps/{index}/log?tail={tail}", net::seg(run)),
            ),
            None => net::join(
                base,
                &format!("/api/runs/{}/steps/{index}/log", net::seg(run)),
            ),
        },
        Call::Trigger { project, .. } => net::join(
            base,
            &format!("/api/projects/{}/trigger", net::seg(project)),
        ),
        Call::Config => net::join(base, "/api/config"),
        Call::SaveConfig(_) => net::join(base, "/api/config"),
        Call::System => net::join(base, "/api/system"),
        Call::Workflows {
            project,
            repo_path: Some(path),
        } => {
            let name = project.as_deref().filter(|s| !s.is_empty()).unwrap_or("_");
            net::join(
                base,
                &format!(
                    "/api/projects/{}/workflows?repo_path={}",
                    net::seg(name),
                    net::seg(path)
                ),
            )
        }
        Call::Workflows {
            project: Some(name),
            repo_path: None,
        } => net::join(base, &format!("/api/projects/{}/workflows", net::seg(name))),
        // Nothing to ask for: the caller has no project name and no repo path yet.
        Call::Workflows { .. } => net::join(base, "/api/projects/_/workflows"),
        Call::Health => net::join(base, "/api/health"),
    }
}

/// What one `Call` answers with.
#[derive(Debug)]
pub enum Answer {
    Projects(Vec<Project>),
    Project(Box<Project>),
    Runs(Vec<Run>),
    Run(Box<Run>),
    Log {
        run: String,
        index: usize,
        text: String,
        tail: Option<usize>,
    },
    Triggered,
    Config(Box<Config>),
    Saved(Box<ConfigOutcome>),
    System(Box<SystemInfo>),
    Workflows {
        workflows: Vec<Workflow>,
        repo_path: Option<String>,
    },
    Health(Box<Health>),
}

impl Answer {
    /// Identifies which read an answer belongs to, so a late answer can be dropped when the view
    /// moved on.
    pub fn call(&self) -> Call {
        match self {
            Answer::Projects(_) => Call::Projects,
            Answer::Project(p) => Call::Project(p.name.clone()),
            Answer::Runs(r) => Call::Runs(
                r.first().map(|r| r.project.clone()).unwrap_or_default(),
                r.len(),
            ),
            Answer::Run(r) => Call::Run(r.id.clone()),
            Answer::Log {
                run, index, tail, ..
            } => Call::Log {
                run: run.clone(),
                index: *index,
                tail: *tail,
            },
            Answer::Triggered => Call::Projects,
            Answer::Config(_) => Call::Config,
            Answer::Saved(_) => Call::Config,
            Answer::System(_) => Call::System,
            Answer::Workflows {
                workflows,
                repo_path,
            } => Call::Workflows {
                project: None,
                repo_path: repo_path
                    .clone()
                    .or_else(|| workflows.first().map(|w| w.file.clone())),
            },
            Answer::Health(_) => Call::Health,
        }
    }

    pub fn failure(&self) -> Option<Failure> {
        None
    }
}

/// Maps a `Reply` onto the typed answer for `call`.
fn parse(call: &Call, reply: Reply) -> std::result::Result<Answer, Failure> {
    let failure = Failure::from_reply(&reply);
    if let Some(f) = failure {
        return Err(f);
    }
    match call {
        Call::Projects => reply.into_json().map(Answer::Projects),
        Call::Project(_) => reply.into_json().map(|p| Answer::Project(Box::new(p))),
        Call::Runs(..) => reply.into_json().map(Answer::Runs),
        Call::Run(_) => reply.into_json().map(|r| Answer::Run(Box::new(r))),
        Call::Log { run, index, tail } => reply.into_text().map(|text| Answer::Log {
            run: run.clone(),
            index: *index,
            text,
            tail: *tail,
        }),
        Call::Trigger { .. } => reply.into_text().map(|_| Answer::Triggered),
        Call::Config => reply.into_json().map(|c| Answer::Config(Box::new(c))),
        Call::SaveConfig(_) => reply
            .into_json()
            .map(|c: ConfigOutcome| Answer::Saved(Box::new(c))),
        Call::System => reply.into_json().map(|s| Answer::System(Box::new(s))),
        Call::Workflows { repo_path, .. } => reply.into_json().map(|w| Answer::Workflows {
            workflows: w,
            repo_path: repo_path.clone(),
        }),
        Call::Health => reply.into_json().map(|h| Answer::Health(Box::new(h))),
    }
}

/// The body a `Call` sends, if it sends one.
fn body(call: &Call) -> Option<(String, Value)> {
    match call {
        Call::Trigger { project: _, force } => Some((
            "POST".into(),
            serde_json::json!({ "sha": Value::Null, "force": force }),
        )),
        Call::SaveConfig(cfg) => match serde_json::from_str::<Value>(cfg) {
            Ok(v) => Some(("PUT".into(), v)),
            // The caller validates before asking; anything else is a bug worth surfacing.
            Err(_) => Some(("PUT".into(), Value::Null)),
        },
        _ => None,
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub fn run(base: &str, token: &str, call: &Call) -> Result<Answer> {
    let url = path(base, call);
    match body(call) {
        Some((method, json)) => {
            let reply = net::send_json(&url, token, &method, json);
            parse(call, reply)
        }
        None => {
            let reply = net::request("GET", &url, token, None);
            parse(call, reply)
        }
    }
}

#[cfg(target_arch = "wasm32")]
pub async fn run(base: &str, token: &str, call: &Call) -> Result<Answer> {
    let url = path(base, call);
    let (method, json) = match body(call) {
        Some((m, j)) => (m, Some(j)),
        None => ("GET".to_string(), None),
    };
    let reply = net::request(
        Box::leak(method.into_boxed_str()),
        url,
        token.to_string(),
        json,
    )
    .await;
    parse(call, reply)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints_match_the_contract() {
        let b = "http://127.0.0.1:7878/";
        assert_eq!(
            path(b, &Call::Projects),
            "http://127.0.0.1:7878/api/projects"
        );
        assert_eq!(
            path(b, &Call::Project("lunarum".into())),
            "http://127.0.0.1:7878/api/projects/lunarum"
        );
        assert_eq!(
            path(
                b,
                &Call::Log {
                    run: "r-1".into(),
                    index: 2,
                    tail: Some(200)
                }
            ),
            "http://127.0.0.1:7878/api/runs/r-1/steps/2/log?tail=200"
        );
        assert_eq!(
            path(
                b,
                &Call::Log {
                    run: "r-1".into(),
                    index: 2,
                    tail: None
                }
            ),
            "http://127.0.0.1:7878/api/runs/r-1/steps/2/log"
        );
        assert_eq!(
            path(b, &Call::Runs("a b".into(), 20)),
            "http://127.0.0.1:7878/api/projects/a%20b/runs?limit=20"
        );
        assert_eq!(
            path(
                b,
                &Call::Workflows {
                    project: Some("p".into()),
                    repo_path: None
                }
            ),
            "http://127.0.0.1:7878/api/projects/p/workflows"
        );
        assert_eq!(
            path(
                b,
                &Call::Workflows {
                    project: None,
                    repo_path: Some("/srv/x".into())
                }
            ),
            "http://127.0.0.1:7878/api/projects/_/workflows?repo_path=%2Fsrv%2Fx"
        );
        assert_eq!(
            path(
                b,
                &Call::Workflows {
                    project: Some("unsaved".into()),
                    repo_path: Some("/srv/x".into())
                }
            ),
            "http://127.0.0.1:7878/api/projects/unsaved/workflows?repo_path=%2Fsrv%2Fx"
        );
    }
}
