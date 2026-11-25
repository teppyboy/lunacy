//! Configuration: TOML shape, validation, secret masking, comment-preserving writes.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const MASK: &str = "********";
pub const DEFAULT_STEP_TIMEOUT: u64 = 3600;
pub const DEFAULT_POLL_INTERVAL: u64 = 300;
pub const DEFAULT_API_URL: &str = "https://api.github.com";

/// Host operating system, as used by `only_on` and the platform defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Os {
    Linux,
    Windows,
    Macos,
}

impl Os {
    pub fn current() -> Os {
        if cfg!(windows) {
            Os::Windows
        } else if cfg!(target_os = "macos") {
            Os::Macos
        } else {
            Os::Linux
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Os::Linux => "linux",
            Os::Windows => "windows",
            Os::Macos => "macos",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Checkout {
    /// Run commands in `repo_path`; the steps (or their scripts) fetch what they need.
    #[default]
    None,
    /// `git worktree add --detach <tmp> <sha>`, removed afterwards.
    Worktree,
    /// Fast-forward `repo_path` in place; refuses a dirty worktree.
    #[serde(rename = "in-place")]
    InPlace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum StepKind {
    #[default]
    Shell,
    Act,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GithubMode {
    Status,
    Checks,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[derive(Default)]
pub struct Config {
    #[serde(default)]
    pub general: General,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<ServerCfg>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github: Option<GithubCfg>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discord: Option<DiscordCfg>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub act: Option<ActCfg>,
    #[serde(default, rename = "project")]
    pub projects: Vec<ProjectCfg>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct General {
    /// Defaults to the platform data dir (`~/.local/share/lunacy` and friends).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state_dir: Option<PathBuf>,
    #[serde(default = "default_poll_interval")]
    pub poll_interval: u64,
    #[serde(default = "default_log_level")]
    pub log_level: String,
    /// argv prefix used to run a `shell` step, e.g. `["bash", "-c"]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell: Option<Vec<String>>,
}

impl Default for General {
    fn default() -> Self {
        General {
            state_dir: None,
            poll_interval: DEFAULT_POLL_INTERVAL,
            log_level: default_log_level(),
            shell: None,
        }
    }
}

fn default_poll_interval() -> u64 {
    DEFAULT_POLL_INTERVAL
}

fn default_log_level() -> String {
    "info".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerCfg {
    pub listen: String,
    #[serde(default)]
    pub token: String,
    /// File holding the token instead of the inline value; read trimmed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_file: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_root: Option<PathBuf>,
    /// Used for GitHub check-run `details_url` links back into the UI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_url: Option<String>,
}

impl ServerCfg {
    pub fn token_value(&self) -> String {
        if !self.token.trim().is_empty() {
            return self.token.trim().to_string();
        }
        self.token_file
            .as_deref()
            .and_then(read_trimmed)
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GithubCfg {
    /// `checks` needs `[github.app]`; `status` works with a token.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<GithubMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    /// File holding the token instead of the inline value; read trimmed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_file: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<AppCfg>,
}

impl GithubCfg {
    pub fn api_url(&self) -> &str {
        self.api_url.as_deref().unwrap_or(DEFAULT_API_URL)
    }

    pub fn effective_mode(&self) -> GithubMode {
        match self.mode {
            Some(m) => m,
            None if self.app.is_some() => GithubMode::Checks,
            None => GithubMode::Status,
        }
    }

    /// True when there is anything to report with.
    pub fn enabled(&self) -> bool {
        non_empty(&self.token) || self.token_file.is_some() || self.app.is_some()
    }

    /// The inline token, else the trimmed contents of `token_file`.
    pub fn token_value(&self) -> Option<String> {
        match self.token.as_deref().map(str::trim) {
            Some(t) if !t.is_empty() => Some(t.to_string()),
            _ => self.token_file.as_deref().and_then(read_trimmed),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppCfg {
    pub app_id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub private_key_path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub private_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installation_id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webhook_secret: Option<String>,
    /// File holding the webhook secret instead of the inline value; read trimmed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webhook_secret_file: Option<PathBuf>,
}

impl AppCfg {
    pub fn key_pem(&self) -> Result<String> {
        if let Some(path) = &self.private_key_path {
            let path = expand_tilde(path);
            return std::fs::read_to_string(&path)
                .map(|pem| pem.trim().to_string())
                .with_context(|| format!("reading github app key {}", path.display()));
        }
        if let Some(k) = &self.private_key {
            if !k.trim().is_empty() {
                return Ok(k.trim().to_string());
            }
        }
        anyhow::bail!("github.app needs private_key_path or private_key")
    }

    /// The inline webhook secret, else the trimmed contents of `webhook_secret_file`.
    pub fn webhook_secret_value(&self) -> Option<String> {
        match self.webhook_secret.as_deref().map(str::trim) {
            Some(s) if !s.is_empty() => Some(s.to_string()),
            _ => self.webhook_secret_file.as_deref().and_then(read_trimmed),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiscordCfg {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webhook_url: Option<String>,
    /// File holding the webhook URL instead of the inline value; read trimmed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webhook_url_file: Option<PathBuf>,
    #[serde(default)]
    pub notify_on_start: bool,
}

impl DiscordCfg {
    /// The inline webhook URL, else the trimmed contents of `webhook_url_file`.
    pub fn webhook_url_value(&self) -> Option<String> {
        match self.webhook_url.as_deref().map(str::trim) {
            Some(u) if !u.is_empty() => Some(u.to_string()),
            _ => self.webhook_url_file.as_deref().and_then(read_trimmed),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActCfg {
    /// e.g. `["act"]`, or `["wsl.exe", "-e", "act"]` on Windows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<Vec<String>>,
    /// Used for self-hosted jobs on Windows (native, never through WSL).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_command: Option<Vec<String>>,
    #[serde(default)]
    pub platforms: BTreeMap<String, String>,
    #[serde(default)]
    pub args: Vec<String>,
    /// Lock every act run takes; `flock(2)`, so the `flock` CLI can share it.
    /// Defaults to `state_dir/act.lock`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lock_file: Option<PathBuf>,
}

impl ActCfg {
    pub fn command(&self) -> Vec<String> {
        self.command
            .clone()
            .unwrap_or_else(|| vec!["act".to_string()])
    }

    pub fn native_command(&self) -> Vec<String> {
        self.native_command
            .clone()
            .unwrap_or_else(|| vec!["act".to_string()])
    }

    pub fn is_wsl(&self) -> bool {
        let argv = self.command();
        argv.first()
            .map(|p| {
                let name = Path::new(p)
                    .file_name()
                    .map(|s| s.to_string_lossy().to_ascii_lowercase())
                    .unwrap_or_default();
                name == "wsl" || name == "wsl.exe"
            })
            .unwrap_or(false)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectCfg {
    pub name: String,
    pub repo_path: PathBuf,
    #[serde(default = "default_remote")]
    pub remote: String,
    #[serde(default = "default_branch")]
    pub branch: String,
    /// `owner/name`; derived from the remote URL when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github_repo: Option<String>,
    #[serde(default)]
    pub checkout: Checkout,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub poll_interval: Option<u64>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub contexts: Contexts,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discord_webhook_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notify_on_start: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell: Option<Vec<String>>,
    #[serde(default)]
    pub platforms: BTreeMap<String, String>,
    #[serde(default)]
    pub ci: Vec<StepCfg>,
    #[serde(default)]
    pub deploy: Vec<StepCfg>,
}

fn default_remote() -> String {
    "origin".to_string()
}

fn default_branch() -> String {
    "master".to_string()
}

impl ProjectCfg {
    pub fn poll_interval(&self, general: &General) -> u64 {
        self.poll_interval.unwrap_or(general.poll_interval).max(1)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Contexts {
    #[serde(default = "default_ci_context")]
    pub ci: String,
    #[serde(default = "default_deploy_context")]
    pub deploy: String,
}

impl Default for Contexts {
    fn default() -> Self {
        Contexts {
            ci: default_ci_context(),
            deploy: default_deploy_context(),
        }
    }
}

fn default_ci_context() -> String {
    "lunacy/ci".to_string()
}

fn default_deploy_context() -> String {
    "lunacy/deploy".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StepCfg {
    pub name: String,
    #[serde(default)]
    pub kind: StepKind,
    /// `shell` steps: the script handed to the configured shell.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    /// `act` steps: workflow path relative to the repo root.
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
    /// Run only on these hosts; on any other host the step is `skipped`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub only_on: Vec<Os>,
    /// Seconds; default 3600.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell: Option<Vec<String>>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

impl StepCfg {
    pub fn timeout(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.timeout.unwrap_or(DEFAULT_STEP_TIMEOUT).max(1))
    }

    pub fn event(&self) -> &str {
        self.event.as_deref().unwrap_or("push")
    }

    pub fn applies_to(&self, os: Os) -> bool {
        self.only_on.is_empty() || self.only_on.contains(&os)
    }
}

pub fn default_shell(os: Os) -> Vec<String> {
    match os {
        Os::Windows => vec!["cmd".to_string(), "/C".to_string()],
        _ => vec!["bash".to_string(), "-c".to_string()],
    }
}

pub fn resolve_shell(
    general: &General,
    project: Option<&ProjectCfg>,
    step: Option<&StepCfg>,
    os: Os,
) -> Vec<String> {
    step.and_then(|s| s.shell.clone())
        .or_else(|| project.and_then(|p| p.shell.clone()))
        .or_else(|| general.shell.clone())
        .unwrap_or_else(|| default_shell(os))
}

impl Config {
    pub fn load(path: &Path) -> Result<Config> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let cfg: Config =
            toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        cfg.validate().map_err(|e| {
            anyhow::anyhow!(
                "{}: {}: {}",
                path.display(),
                e.field.unwrap_or_else(|| "config".into()),
                e.message
            )
        })?;
        Ok(cfg)
    }

    pub fn validate(&self) -> std::result::Result<(), ConfigError> {
        if self.general.poll_interval == 0 {
            return Err(ConfigError::new(
                "general.poll_interval",
                "must be at least 1 second",
            ));
        }
        if let Some(server) = &self.server {
            if server.listen.trim().is_empty() {
                return Err(ConfigError::new("server.listen", "must not be empty"));
            }
            conflict(
                "server.token",
                !server.token.trim().is_empty(),
                "server.token_file",
                server.token_file.is_some(),
            )?;
            check_secret_file("server.token_file", server.token_file.as_deref())?;
        }
        if let Some(github) = &self.github {
            conflict(
                "github.token",
                non_empty(&github.token),
                "github.token_file",
                github.token_file.is_some(),
            )?;
            check_secret_file("github.token_file", github.token_file.as_deref())?;
            if let Some(mode) = github.mode {
                if mode == GithubMode::Checks && github.app.is_none() {
                    return Err(ConfigError::new(
                        "github.mode",
                        "mode = \"checks\" needs [github.app] (the Checks API only accepts App tokens)",
                    ));
                }
            }
            if let Some(app) = &github.app {
                if app.private_key.is_none() && app.private_key_path.is_none() {
                    return Err(ConfigError::new(
                        "github.app",
                        "needs private_key_path or private_key",
                    ));
                }
                conflict(
                    "github.app.private_key",
                    non_empty(&app.private_key),
                    "github.app.private_key_path",
                    app.private_key_path.is_some(),
                )?;
                conflict(
                    "github.app.webhook_secret",
                    non_empty(&app.webhook_secret),
                    "github.app.webhook_secret_file",
                    app.webhook_secret_file.is_some(),
                )?;
                check_secret_file(
                    "github.app.private_key_path",
                    app.private_key_path.as_deref(),
                )?;
                check_secret_file(
                    "github.app.webhook_secret_file",
                    app.webhook_secret_file.as_deref(),
                )?;
            }
        }
        if let Some(discord) = &self.discord {
            conflict(
                "discord.webhook_url",
                non_empty(&discord.webhook_url),
                "discord.webhook_url_file",
                discord.webhook_url_file.is_some(),
            )?;
            check_secret_file(
                "discord.webhook_url_file",
                discord.webhook_url_file.as_deref(),
            )?;
        }
        if let Some(act) = &self.act {
            if act.command().is_empty() {
                return Err(ConfigError::new("act.command", "must not be empty"));
            }
            if let Some(lock) = &act.lock_file {
                if lock.as_os_str().is_empty() {
                    return Err(ConfigError::new("act.lock_file", "must not be empty"));
                }
            }
        }

        let mut seen = std::collections::BTreeSet::new();
        for (i, p) in self.projects.iter().enumerate() {
            let base = format!("project[{i}]");
            if p.name.trim().is_empty() {
                return Err(ConfigError::new(
                    format!("{base}.name"),
                    "must not be empty",
                ));
            }
            if !seen.insert(p.name.clone()) {
                return Err(ConfigError::new(
                    format!("{base}.name"),
                    format!("duplicate project name {:?}", p.name),
                ));
            }
            if p.repo_path.as_os_str().is_empty() {
                return Err(ConfigError::new(
                    format!("{base}.repo_path"),
                    "must not be empty",
                ));
            }
            if p.branch.trim().is_empty() {
                return Err(ConfigError::new(
                    format!("{base}.branch"),
                    "must not be empty",
                ));
            }
            if p.ci.is_empty() && p.deploy.is_empty() {
                return Err(ConfigError::new(
                    format!("{base}.ci"),
                    "project has neither ci nor deploy steps",
                ));
            }
            if let Some(g) = &p.github_repo {
                if !is_owner_name(g) {
                    return Err(ConfigError::new(
                        format!("{base}.github_repo"),
                        "expected \"owner/name\"",
                    ));
                }
            }
            for (kind, steps) in [("ci", &p.ci), ("deploy", &p.deploy)] {
                for (j, s) in steps.iter().enumerate() {
                    let sb = format!("{base}.{kind}[{j}]");
                    if s.name.trim().is_empty() {
                        return Err(ConfigError::new(format!("{sb}.name"), "must not be empty"));
                    }
                    match s.kind {
                        StepKind::Shell => {
                            if s.run.as_deref().unwrap_or("").trim().is_empty() {
                                return Err(ConfigError::new(
                                    format!("{sb}.run"),
                                    "a shell step needs a `run` script",
                                ));
                            }
                        }
                        StepKind::Act => {
                            if s.workflow.as_deref().unwrap_or("").trim().is_empty() {
                                return Err(ConfigError::new(
                                    format!("{sb}.workflow"),
                                    "an act step needs a `workflow` path",
                                ));
                            }
                        }
                    }
                    if s.timeout == Some(0) {
                        return Err(ConfigError::new(
                            format!("{sb}.timeout"),
                            "must be at least 1",
                        ));
                    }
                    if let Some(shell) = &s.shell {
                        if shell.is_empty() {
                            return Err(ConfigError::new(
                                format!("{sb}.shell"),
                                "must not be empty",
                            ));
                        }
                    }
                }
            }
        }
        Ok(())
    }

    pub fn state_dir(&self) -> PathBuf {
        if let Some(dir) = &self.general.state_dir {
            return dir.clone();
        }
        if let Some(base) = directories::BaseDirs::new() {
            return base.data_local_dir().join("lunacy");
        }
        std::env::temp_dir().join("lunacy")
    }

    pub fn project(&self, name: &str) -> Option<&ProjectCfg> {
        self.projects.iter().find(|p| p.name == name)
    }

    pub fn discord_for(&self, project: &ProjectCfg) -> Option<(String, bool)> {
        let url = project
            .discord_webhook_url
            .clone()
            .filter(|u| !u.trim().is_empty())
            .or_else(|| self.discord.as_ref().and_then(|d| d.webhook_url_value()))?;
        let url = url.trim().to_string();
        if url.is_empty() {
            return None;
        }
        let start = project
            .notify_on_start
            .or_else(|| self.discord.as_ref().map(|d| d.notify_on_start))
            .unwrap_or(false);
        Some((url, start))
    }

    pub fn act(&self) -> ActCfg {
        self.act.clone().unwrap_or_default()
    }

    /// The act lock every act run takes: `[act] lock_file`, else `state_dir/act.lock`.
    pub fn act_lock_file(&self) -> PathBuf {
        match self.act.as_ref().and_then(|a| a.lock_file.clone()) {
            Some(path) => expand_tilde(&path),
            None => self.state_dir().join("act.lock"),
        }
    }

    /// Copy with secrets replaced by [`MASK`]; empty values stay empty.
    pub fn masked(&self) -> Config {
        let mut c = self.clone();
        if let Some(g) = c.github.as_mut() {
            mask_opt(&mut g.token);
            if let Some(app) = g.app.as_mut() {
                mask_opt(&mut app.private_key);
                mask_opt(&mut app.webhook_secret);
            }
        }
        if let Some(d) = c.discord.as_mut() {
            mask_opt(&mut d.webhook_url);
        }
        if let Some(s) = c.server.as_mut() {
            mask_str(&mut s.token);
        }
        for p in c.projects.iter_mut() {
            mask_opt(&mut p.discord_webhook_url);
            for s in p.ci.iter_mut().chain(p.deploy.iter_mut()) {
                for v in s.secrets.values_mut() {
                    mask_str(v);
                }
            }
        }
        c
    }

    /// Treat [`MASK`] in `self` as "keep the value from `old`".
    pub fn unmask_from(&mut self, old: &Config) {
        if let (Some(new), Some(old)) = (self.github.as_mut(), old.github.as_ref()) {
            unmask_opt(&mut new.token, &old.token);
            if let (Some(na), Some(oa)) = (new.app.as_mut(), old.app.as_ref()) {
                unmask_opt(&mut na.private_key, &oa.private_key);
                unmask_opt(&mut na.webhook_secret, &oa.webhook_secret);
            }
        }
        if let (Some(new), Some(old)) = (self.discord.as_mut(), old.discord.as_ref()) {
            unmask_opt(&mut new.webhook_url, &old.webhook_url);
        }
        if let (Some(new), Some(old)) = (self.server.as_mut(), old.server.as_ref()) {
            if new.token == MASK {
                new.token = old.token.clone();
            }
        }
        for p in self.projects.iter_mut() {
            let Some(old_p) = old.project(&p.name) else {
                continue;
            };
            unmask_opt(&mut p.discord_webhook_url, &old_p.discord_webhook_url);
            let old_steps: Vec<&StepCfg> = old_p.ci.iter().chain(old_p.deploy.iter()).collect();
            for s in p.ci.iter_mut().chain(p.deploy.iter_mut()) {
                let Some(os) = old_steps
                    .iter()
                    .find(|o| o.name == s.name && o.kind == s.kind)
                else {
                    continue;
                };
                for (k, v) in s.secrets.iter_mut() {
                    if *v == MASK {
                        if let Some(old_v) = os.secrets.get(k) {
                            *v = old_v.clone();
                        }
                    }
                }
            }
        }
    }
}

/// `~/x` -> the home directory plus `x`; anything else is returned unchanged.
pub fn expand_tilde(path: &Path) -> PathBuf {
    let Some(rest) = path
        .to_string_lossy()
        .strip_prefix("~/")
        .map(str::to_string)
    else {
        return path.to_path_buf();
    };
    match directories::BaseDirs::new() {
        Some(base) => base.home_dir().join(rest),
        None => path.to_path_buf(),
    }
}

/// Read a secret file and trim it; `None` when it cannot be read (the caller
/// then behaves as if the secret were absent, and `check` reports the path).
fn read_trimmed(path: &Path) -> Option<String> {
    let path = expand_tilde(path);
    let text = std::fs::read_to_string(&path).ok()?;
    let text = text.trim().to_string();
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

fn mask_str(v: &mut String) {
    if !v.is_empty() {
        *v = MASK.to_string();
    }
}

fn mask_opt(v: &mut Option<String>) {
    if let Some(s) = v.as_mut() {
        mask_str(s);
    }
}

fn unmask_opt(new: &mut Option<String>, old: &Option<String>) {
    if new.as_deref() == Some(MASK) {
        *new = old.clone();
    }
}

fn non_empty(v: &Option<String>) -> bool {
    v.as_deref().map(|s| !s.trim().is_empty()).unwrap_or(false)
}

/// The inline value and its `*_file` alternative are mutually exclusive.
fn conflict(
    inline: &str,
    has_inline: bool,
    file: &str,
    has_file: bool,
) -> std::result::Result<(), ConfigError> {
    if has_inline && has_file {
        return Err(ConfigError::new(
            file,
            format!("set either {inline} or {file}, not both"),
        ));
    }
    Ok(())
}

/// A secret file must exist and hold something; the value itself is read at load.
fn check_secret_file(field: &str, path: Option<&Path>) -> std::result::Result<(), ConfigError> {
    let Some(path) = path else {
        return Ok(());
    };
    let expanded = expand_tilde(path);
    let text = std::fs::read_to_string(&expanded)
        .map_err(|e| ConfigError::new(field, format!("cannot read {}: {e}", expanded.display())))?;
    if text.trim().is_empty() {
        return Err(ConfigError::new(
            field,
            format!("{} is empty", expanded.display()),
        ));
    }
    Ok(())
}

fn is_owner_name(v: &str) -> bool {
    let parts: Vec<&str> = v.split('/').collect();
    parts.len() == 2 && !parts[0].is_empty() && !parts[1].is_empty()
}

/// Derive `owner/name` from a git remote URL (`https`, `ssh://`, `git@host:path`).
pub fn repo_from_remote(url: &str) -> Option<String> {
    let url = url.trim();
    if url.is_empty() {
        return None;
    }
    let (host, path) = if let Some((_, rest)) = url.split_once("://") {
        let (host, path) = rest.split_once('/')?;
        (host.to_string(), path.to_string())
    } else if let Some((left, right)) = url.split_once(':') {
        if left.contains('/') || left.contains('\\') {
            return None; // Windows drive path like C:\repo
        }
        (left.to_string(), right.to_string())
    } else {
        return None;
    };
    let host = host.rsplit('@').next().unwrap_or(&host);
    let host = host.split(':').next().unwrap_or(host);
    if !host.contains('.') {
        return None; // not a host: file:// URLs and bare local paths
    }
    let path = path.trim_start_matches('/').trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    if parts.len() < 2 {
        return None;
    }
    let n = parts.len();
    Some(format!("{}/{}", parts[n - 2], parts[n - 1]))
}

#[derive(Debug, Clone, Serialize)]
pub struct ConfigError {
    pub message: String,
    pub field: Option<String>,
}

impl ConfigError {
    fn new(field: impl Into<String>, message: impl Into<String>) -> ConfigError {
        ConfigError {
            message: message.into(),
            field: Some(field.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn example() -> Config {
        Config::load(Path::new("config.example.toml")).expect("example config parses")
    }

    #[test]
    fn example_config_parses_and_validates() {
        let cfg = example();
        assert!(cfg.projects.len() >= 2);
        assert!(cfg.project("lunarum").is_some());
        assert!(cfg.project("frostedge").is_some());
        assert!(cfg.server.is_some());
        cfg.validate().unwrap();
    }

    #[test]
    fn unknown_key_is_rejected() {
        let text = r#"
[general]
poll_intervl = 30
"#;
        let err = toml::from_str::<Config>(text).unwrap_err().to_string();
        assert!(err.contains("unknown field"), "got {err}");
        assert!(err.contains("poll_intervl"), "got {err}");
    }

    #[test]
    fn unknown_project_key_is_rejected() {
        let text = r#"
[[project]]
name = "x"
repo_path = "/tmp/x"
checkout = "none"
chekout = "worktree"
"#;
        let err = toml::from_str::<Config>(text).unwrap_err().to_string();
        assert!(err.contains("unknown field"), "got {err}");
    }

    #[test]
    fn empty_project_name_fails_validation_with_field() {
        let mut cfg = example();
        cfg.projects[0].name = "  ".into();
        let err = cfg.validate().unwrap_err();
        assert_eq!(err.field.as_deref(), Some("project[0].name"));
    }

    #[test]
    fn duplicate_project_names_are_rejected() {
        let mut cfg = example();
        cfg.projects[1].name = cfg.projects[0].name.clone();
        let err = cfg.validate().unwrap_err();
        assert!(err.message.contains("duplicate"), "{}", err.message);
    }

    #[test]
    fn repo_derivation() {
        let cases = [
            (
                "https://github.com/teppyboy/lunarum.git",
                Some("teppyboy/lunarum"),
            ),
            (
                "https://github.com/teppyboy/lunarum",
                Some("teppyboy/lunarum"),
            ),
            (
                "https://github.com/teppyboy/lunarum/",
                Some("teppyboy/lunarum"),
            ),
            (
                "git@github.com:teppyboy/frostedge.git",
                Some("teppyboy/frostedge"),
            ),
            (
                "git@github.com:teppyboy/frostedge",
                Some("teppyboy/frostedge"),
            ),
            (
                "ssh://git@github.com/teppyboy/frostedge.git",
                Some("teppyboy/frostedge"),
            ),
            (
                "ssh://git@github.com:22/teppyboy/frostedge.git",
                Some("teppyboy/frostedge"),
            ),
            ("https://user:pw@github.com/o/r.git", Some("o/r")),
            ("/home/tretrauit/workspace/lunarum", None),
            ("file:///home/tretrauit/repos/x.git", None),
            ("C:\\Users\\x\\repo", None),
            ("", None),
        ];
        for (url, want) in cases {
            assert_eq!(repo_from_remote(url).as_deref(), want, "url {url}");
        }
    }

    #[test]
    fn mode_defaults() {
        let plain = GithubCfg {
            mode: None,
            token: Some("t".into()),
            token_file: None,
            api_url: None,
            app: None,
        };
        assert_eq!(plain.effective_mode(), GithubMode::Status);
        let mut with_app = plain.clone();
        with_app.app = Some(AppCfg {
            app_id: 1,
            private_key_path: None,
            private_key: Some("k".into()),
            installation_id: None,
            webhook_secret: None,
            webhook_secret_file: None,
        });
        assert_eq!(with_app.effective_mode(), GithubMode::Checks);
    }

    #[test]
    fn checks_mode_without_app_is_rejected() {
        let mut cfg = example();
        cfg.github = Some(GithubCfg {
            mode: Some(GithubMode::Checks),
            token: Some("x".into()),
            token_file: None,
            api_url: None,
            app: None,
        });
        let err = cfg.validate().unwrap_err();
        assert_eq!(err.field.as_deref(), Some("github.mode"));
    }

    #[test]
    fn mask_unmask_roundtrip() {
        let mut cfg = example();
        cfg.github = Some(GithubCfg {
            mode: None,
            token: Some("ghp_secret".into()),
            token_file: None,
            api_url: None,
            app: None,
        });
        cfg.discord = Some(DiscordCfg {
            webhook_url: Some("https://discord.com/api/webhooks/x".into()),
            webhook_url_file: None,
            notify_on_start: false,
        });
        if let Some(s) = cfg.server.as_mut() {
            s.token = "ui-token".into();
        }
        let masked = cfg.masked();
        let json = serde_json::to_string(&masked).unwrap();
        assert!(!json.contains("ghp_secret"));
        assert!(!json.contains("ui-token"));
        assert!(json.contains(MASK));

        // Round-tripping the masked JSON keeps every secret.
        let mut back: Config = serde_json::from_str(&json).unwrap();
        back.unmask_from(&cfg);
        assert_eq!(
            back.github.as_ref().unwrap().token.as_deref(),
            Some("ghp_secret")
        );
        assert_eq!(back.server.as_ref().unwrap().token, "ui-token");
        assert_eq!(
            back.discord.as_ref().unwrap().webhook_url.as_deref(),
            Some("https://discord.com/api/webhooks/x")
        );

        // An empty secret stays empty and does not adopt the old value.
        let mut empty = cfg.masked();
        empty.discord.as_mut().unwrap().webhook_url = Some(String::new());
        empty.unmask_from(&cfg);
        assert_eq!(empty.discord.unwrap().webhook_url.as_deref(), Some(""));
    }

    #[test]
    fn act_secrets_are_masked_and_kept() {
        let mut cfg = example();
        cfg.projects[0].ci.push(StepCfg {
            name: "act with secret".into(),
            kind: StepKind::Act,
            run: None,
            workflow: Some(".github/workflows/ci.yml".into()),
            job: None,
            event: None,
            platforms: BTreeMap::new(),
            args: Vec::new(),
            secrets: BTreeMap::from([("TOKEN".to_string(), "s3cret".to_string())]),
            vars: BTreeMap::new(),
            only_on: Vec::new(),
            timeout: None,
            cwd: None,
            shell: None,
            env: BTreeMap::new(),
        });
        let masked = cfg.masked();
        let json = serde_json::to_string(&masked).unwrap();
        assert!(!json.contains("s3cret"), "{json}");
        let mut back: Config = serde_json::from_str(&json).unwrap();
        back.unmask_from(&cfg);
        assert_eq!(
            back.projects[0]
                .ci
                .last()
                .unwrap()
                .secrets
                .get("TOKEN")
                .map(String::as_str),
            Some("s3cret")
        );
    }

    #[test]
    fn shell_resolution_order() {
        let general = General::default();
        let mut project = example().projects.remove(0);
        project.shell = Some(vec!["project-sh".into()]);
        let mut step = project.ci[0].clone();
        step.shell = None;
        assert_eq!(
            resolve_shell(&general, Some(&project), Some(&step), Os::Linux),
            vec!["project-sh"]
        );
        step.shell = Some(vec!["step-sh".into()]);
        assert_eq!(
            resolve_shell(&general, Some(&project), Some(&step), Os::Linux),
            vec!["step-sh"]
        );
        assert_eq!(
            resolve_shell(&General::default(), None, None, Os::Windows),
            vec!["cmd".to_string(), "/C".to_string()]
        );
        assert_eq!(
            resolve_shell(&General::default(), None, None, Os::Linux),
            vec!["bash".to_string(), "-c".to_string()]
        );
        assert_eq!(
            resolve_shell(&General::default(), None, None, Os::Macos),
            vec!["bash".to_string(), "-c".to_string()]
        );
    }

    #[test]
    fn secret_files_are_read_and_conflict_with_inline_values() {
        let dir = tempfile::tempdir().expect("tempdir");
        let token = dir.path().join("token");
        std::fs::write(&token, "  ghp_from_file\n").expect("write");
        let hook = dir.path().join("hook");
        std::fs::write(&hook, "hooksecret").expect("write");
        let url = dir.path().join("url");
        std::fs::write(&url, "https://discord.com/api/webhooks/f/g").expect("write");

        let mut cfg = example();
        cfg.github = Some(GithubCfg {
            mode: Some(GithubMode::Checks),
            token: None,
            token_file: Some(token.clone()),
            api_url: None,
            app: Some(AppCfg {
                app_id: 1,
                private_key_path: Some(dir.path().join("missing.pem")),
                private_key: None,
                installation_id: Some(1),
                webhook_secret: None,
                webhook_secret_file: Some(hook.clone()),
            }),
        });
        cfg.discord = Some(DiscordCfg {
            webhook_url: None,
            webhook_url_file: Some(url.clone()),
            notify_on_start: false,
        });
        cfg.server = Some(ServerCfg {
            listen: "127.0.0.1:1".into(),
            token: String::new(),
            token_file: Some(token.clone()),
            web_root: None,
            public_url: None,
        });

        // Trimmed at load.
        assert_eq!(
            cfg.github.as_ref().unwrap().token_value().as_deref(),
            Some("ghp_from_file")
        );
        assert_eq!(
            cfg.github
                .as_ref()
                .unwrap()
                .app
                .as_ref()
                .unwrap()
                .webhook_secret_value()
                .as_deref(),
            Some("hooksecret")
        );
        assert_eq!(
            cfg.discord_for(&cfg.projects[0].clone())
                .map(|(u, _)| u)
                .as_deref(),
            Some("https://discord.com/api/webhooks/f/g")
        );
        assert_eq!(cfg.server.as_ref().unwrap().token_value(), "ghp_from_file");
        // A missing key file is a validation error on that field, not a panic.
        let err = cfg.validate().unwrap_err();
        assert_eq!(err.field.as_deref(), Some("github.app.private_key_path"));
        assert!(err.message.contains("cannot read"), "{}", err.message);

        // Give it a real key file: now it validates, and the *_file paths are
        // exposed as plain strings.
        let pem = dir.path().join("key.pem");
        std::fs::write(&pem, "-----BEGIN KEY-----\n").expect("write");
        if let Some(app) = cfg.github.as_mut().and_then(|g| g.app.as_mut()) {
            app.private_key_path = Some(pem);
        }
        cfg.validate().expect("valid");
        let json = serde_json::to_string(&cfg.masked()).expect("json");
        assert!(json.contains("token_file"), "{json}");
        assert!(json.contains("webhook_url_file"), "{json}");
        assert!(!json.contains("ghp_from_file"), "{json}");
        assert!(!json.contains("hooksecret"), "{json}");
        assert!(!json.contains("webhooks/f/g"), "{json}");

        // Both spellings of one secret is a hard error naming the file field.
        let mut bad = cfg.clone();
        bad.github.as_mut().unwrap().token = Some("inline".into());
        let err = bad.validate().unwrap_err();
        assert_eq!(err.field.as_deref(), Some("github.token_file"));
        assert!(err.message.contains("not both"), "{}", err.message);

        let mut bad = cfg.clone();
        bad.discord.as_mut().unwrap().webhook_url = Some("inline".into());
        assert_eq!(
            bad.validate().unwrap_err().field.as_deref(),
            Some("discord.webhook_url_file")
        );
        let mut bad = cfg.clone();
        bad.server.as_mut().unwrap().token = "inline".into();
        assert_eq!(
            bad.validate().unwrap_err().field.as_deref(),
            Some("server.token_file")
        );

        // An empty file is reported instead of silently meaning "no secret".
        let empty = dir.path().join("empty");
        std::fs::write(&empty, "  \n").expect("write");
        let mut bad = cfg.clone();
        bad.github.as_mut().unwrap().token_file = Some(empty);
        bad.github.as_mut().unwrap().token = None;
        let err = bad.validate().unwrap_err();
        assert!(err.message.contains("is empty"), "{}", err.message);
    }

    #[test]
    fn tilde_and_lock_file() {
        let home = directories::BaseDirs::new()
            .expect("home")
            .home_dir()
            .to_path_buf();
        assert_eq!(expand_tilde(Path::new("~/x")), home.join("x"));
        assert_eq!(expand_tilde(Path::new("/abs/x")), PathBuf::from("/abs/x"));

        // Default stays state_dir/act.lock.
        let mut cfg = Config::default();
        cfg.general.state_dir = Some(PathBuf::from("/state"));
        assert_eq!(cfg.act_lock_file(), PathBuf::from("/state/act.lock"));
        cfg.act = Some(ActCfg {
            lock_file: Some(PathBuf::from("~/act-gate.lock")),
            ..ActCfg::default()
        });
        assert_eq!(cfg.act_lock_file(), home.join("act-gate.lock"));
    }

    #[test]
    fn only_on_gate() {
        let mut step = example().projects[0].ci[0].clone();
        assert!(step.applies_to(Os::Linux));
        step.only_on = vec![Os::Windows];
        assert!(!step.applies_to(Os::Linux));
        assert!(step.applies_to(Os::Windows));
    }
}
