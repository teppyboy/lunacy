//! The daemon's config, as the same JSON shape `config.toml` has (`docs/api.md` § `Config`).
//!
//! Only the fields the UI edits are typed: everything else stays in `extra` and travels back to
//! the daemon untouched, so a config written by a newer daemon keeps its settings. Fields are
//! `Option`/`default` exactly where the daemon's serde is, so a partial PUT is valid.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// `"********"` in a secret field means "keep the stored value".
pub const MASK: &str = "********";

fn extra() -> Map<String, Value> {
    Map::new()
}

fn default_poll() -> u64 {
    300
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Config {
    #[serde(default)]
    pub general: General,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<Server>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github: Option<Github>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discord: Option<Discord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub act: Option<Act>,
    #[serde(rename = "project", default)]
    pub projects: Vec<Project>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct General {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state_dir: Option<String>,
    #[serde(default = "default_poll")]
    pub poll_interval: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell: Option<Vec<String>>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Default for General {
    fn default() -> Self {
        General {
            state_dir: None,
            poll_interval: default_poll(),
            shell: None,
            extra: extra(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Server {
    #[serde(default)]
    pub listen: String,
    #[serde(default)]
    pub token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_root: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Github {
    /// `status` or `checks`; absent means the daemon decides.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<App>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct App {
    #[serde(default)]
    pub app_id: u64,
    /// Multi-line PEM. Masked once stored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub private_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webhook_secret: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installation_id: Option<u64>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Default for App {
    fn default() -> Self {
        App {
            app_id: 0,
            private_key: None,
            webhook_secret: None,
            installation_id: None,
            extra: extra(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Discord {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webhook_url: Option<String>,
    #[serde(default)]
    pub notify_on_start: bool,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Act {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<Vec<String>>,
    /// Lock every act run takes; defaults to `state_dir/act.lock`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lock_file: Option<String>,
    #[serde(default)]
    pub platforms: BTreeMap<String, String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Project {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub repo_path: String,
    #[serde(default = "default_remote")]
    pub remote: String,
    #[serde(default = "default_branch")]
    pub branch: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github_repo: Option<String>,
    /// `none`, `worktree` or `in-place`.
    #[serde(default = "default_checkout")]
    pub checkout: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub poll_interval: Option<u64>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discord_webhook_url: Option<String>,
    #[serde(default)]
    pub ci: Vec<Step>,
    #[serde(default)]
    pub deploy: Vec<Step>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

fn default_remote() -> String {
    "origin".into()
}
fn default_branch() -> String {
    "master".into()
}
fn default_checkout() -> String {
    "none".into()
}

impl Project {
    pub fn new(name: impl Into<String>) -> Project {
        Project {
            name: name.into(),
            remote: default_remote(),
            branch: default_branch(),
            checkout: default_checkout(),
            ..Project::default()
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Step {
    #[serde(default)]
    pub name: String,
    /// `shell` or `act`.
    #[serde(default = "default_kind")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event: Option<String>,
    #[serde(default)]
    pub platforms: BTreeMap<String, String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub secrets: BTreeMap<String, String>,
    #[serde(default)]
    pub vars: BTreeMap<String, String>,
    /// `["linux"]`, `["windows"]`, `["macos"]`, or empty for every host.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub only_on: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell: Option<Vec<String>>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

fn default_kind() -> String {
    "shell".into()
}

impl Step {
    pub fn new_shell(name: impl Into<String>) -> Step {
        Step {
            name: name.into(),
            kind: default_kind(),
            ..Step::default()
        }
    }

    pub fn new_act(name: impl Into<String>) -> Step {
        Step {
            name: name.into(),
            kind: "act".into(),
            event: Some("push".into()),
            ..Step::default()
        }
    }

    pub fn is_act(&self) -> bool {
        self.kind == "act"
    }
}

/// What `PUT /api/config` answers with on success.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ConfigOutcome {
    /// The config the daemon now holds, masked. Absent fields fall back to the stored draft.
    #[serde(flatten)]
    pub config: Config,
    /// Present when the daemon reports the save separately from the config body.
    #[serde(default)]
    pub ok: Option<bool>,
    #[serde(default)]
    pub reloaded: Option<bool>,
    /// Fields whose change requires restarting the daemon (stripped from `config.extra`).
    #[serde(default)]
    pub restart_required: Vec<String>,
}

/// Splits a `shell`-style argv string the way a shell-less command line would: whitespace
/// separates, double quotes group.
pub fn split_argv(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut any = false;
    for c in s.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                any = true;
            }
            c if c.is_whitespace() && !quoted => {
                if any || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            c => {
                cur.push(c);
                any = true;
            }
        }
    }
    if any || !cur.is_empty() {
        out.push(cur);
    }
    out
}

pub fn join_argv(argv: &[String]) -> String {
    argv.iter()
        .map(|a| {
            if a.is_empty() || a.contains(char::is_whitespace) {
                format!("\"{a}\"")
            } else {
                a.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// CSV in a text field for a `BTreeMap<String, String>` table, one `KEY=VALUE` a line.
pub fn table_to_text(table: &BTreeMap<String, String>) -> String {
    table
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn text_to_table(text: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        match line.split_once('=') {
            Some((k, v)) => {
                let k = k.trim();
                if !k.is_empty() {
                    out.insert(k.to_string(), v.trim().to_string());
                }
            }
            None => {
                out.insert(line.to_string(), String::new());
            }
        }
    }
    out
}

pub fn lines_to_list(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_keys_survive_a_round_trip() {
        let json = serde_json::json!({
            "general": {"poll_interval": 60, "log_level": "debug"},
            "project": [{"name": "p", "repo_path": "/srv/p", "contexts": {"ci": "x"}, "ci": [
                {"name": "Build", "kind": "shell", "run": "make", "some_future_field": 1}
            ]}]
        });
        let cfg: Config = serde_json::from_value(json).unwrap();
        assert_eq!(
            cfg.general.extra.get("log_level").and_then(Value::as_str),
            Some("debug")
        );
        assert!(cfg.projects[0].extra.get("contexts").is_some());
        assert_eq!(
            cfg.projects[0].ci[0]
                .extra
                .get("some_future_field")
                .and_then(Value::as_u64),
            Some(1)
        );
        let back = serde_json::to_value(&cfg).unwrap();
        assert_eq!(back["general"]["log_level"], "debug");
        assert_eq!(back["project"][0]["ci"][0]["some_future_field"], 1);
    }

    #[test]
    fn argv_splits_like_a_command_line() {
        assert_eq!(split_argv("bash -c"), ["bash", "-c"]);
        assert_eq!(
            split_argv("wsl.exe -e \"my act\""),
            ["wsl.exe", "-e", "my act"]
        );
        assert_eq!(
            join_argv(&split_argv("wsl.exe -e \"my act\"")),
            "wsl.exe -e \"my act\""
        );
        assert!(split_argv("").is_empty());
    }

    #[test]
    fn tables_round_trip() {
        let t = text_to_table("A=1\nB = two words\nbroken line\n# nope\n");
        assert_eq!(t.get("A").map(String::as_str), Some("1"));
        assert_eq!(t.get("B").map(String::as_str), Some("two words"));
        assert_eq!(t.get("broken line").map(String::as_str), Some(""));
        assert!(!t.contains_key("# nope"));
        assert_eq!(table_to_text(&t), "A=1\nB=two words\nbroken line=");
    }

    #[test]
    fn every_editable_field_survives_the_mask_rule() {
        // A secret the daemon masked goes back verbatim, which the daemon reads as "keep".
        let cfg = Config {
            github: Some(Github {
                token: Some(MASK.into()),
                ..Github::default()
            }),
            ..Config::default()
        };
        let json = serde_json::to_value(&cfg).unwrap();
        assert_eq!(json["github"]["token"], MASK);
    }

    #[test]
    fn restart_required_is_stripped_from_saved_config() {
        let payload = serde_json::json!({
            "general": {"poll_interval": 300, "log_level": "info"},
            "server": {"listen": "127.0.0.1:7878", "token": "********"},
            "project": [],
            "restart_required": ["server.listen"]
        });
        let outcome: ConfigOutcome = serde_json::from_value(payload).unwrap();
        assert_eq!(outcome.restart_required, vec!["server.listen"]);
        assert!(!outcome.config.extra.contains_key("restart_required"));
        let back = serde_json::to_value(&outcome.config).unwrap();
        assert!(back.get("restart_required").is_none());
    }
}
