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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TriggerMode {
    Both,
    Webhook,
    Poll,
}

impl TriggerMode {
    pub fn as_str(self) -> &'static str {
        match self {
            TriggerMode::Both => "both",
            TriggerMode::Webhook => "webhook",
            TriggerMode::Poll => "poll",
        }
    }
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
    /// Trigger mode across projects: "both" (default), "webhook" (skip polling), or "poll".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_mode: Option<TriggerMode>,
    #[serde(default = "default_log_level")]
    pub log_level: String,
    /// argv prefix used to run a `shell` step, e.g. `["bash", "-c"]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell: Option<Vec<String>>,
    /// Default per-step timeout in seconds (fallback when project/step don't set one).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_timeout: Option<u64>,
}

impl Default for General {
    fn default() -> Self {
        General {
            state_dir: None,
            poll_interval: DEFAULT_POLL_INTERVAL,
            trigger_mode: None,
            log_level: default_log_level(),
            shell: None,
            step_timeout: None,
        }
    }
}

impl General {
    pub fn effective_trigger_mode(&self) -> TriggerMode {
        self.trigger_mode.unwrap_or(TriggerMode::Both)
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
    /// Standalone webhook HMAC secret for `POST /api/github/webhook` (no GitHub App needed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webhook_secret: Option<String>,
    /// File holding the standalone webhook secret instead of the inline value; read trimmed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webhook_secret_file: Option<PathBuf>,
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

    /// Standalone `webhook_secret` / `webhook_secret_file`, falling back to `[github.app]`.
    pub fn webhook_secret_value(&self) -> Option<String> {
        let standalone = match self.webhook_secret.as_deref().map(str::trim) {
            Some(s) if !s.is_empty() => Some(s.to_string()),
            _ => self.webhook_secret_file.as_deref().and_then(read_trimmed),
        };
        standalone.or_else(|| self.app.as_ref().and_then(|a| a.webhook_secret_value()))
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

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// Enable aggressive build caching by default across act, Docker volumes, and shell env.
    #[serde(default = "default_true")]
    pub cache: bool,
    /// Root directory for disk caches (action cache, cache server, per-project cache dirs).
    /// Defaults to `state_dir/cache`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_dir: Option<PathBuf>,
}

impl Default for ActCfg {
    fn default() -> Self {
        ActCfg {
            command: None,
            native_command: None,
            platforms: BTreeMap::new(),
            args: Vec::new(),
            lock_file: None,
            cache: true,
            cache_dir: None,
        }
    }
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

    pub fn effective_cache_dir(&self, state_dir: &Path) -> PathBuf {
        match &self.cache_dir {
            Some(path) => expand_tilde(path),
            None => state_dir.join("cache"),
        }
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
    /// Override global trigger mode: "both", "webhook" (skip polling), or "poll".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_mode: Option<TriggerMode>,
    /// Default per-step timeout in seconds for this project.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_timeout: Option<u64>,
    /// Total pipeline timeout in seconds across all steps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<u64>,
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
    /// Poll/build open GitHub Pull Requests (CI-only; PRs never deploy).
    #[serde(default)]
    pub watch_prs: bool,
    /// Optional branch/tag patterns allowed to trigger deploy (glob `*` supported).
    /// Empty defaults to `[branch]`. Patterns starting with `tags/` or `refs/tags/` match tags.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deploy_refs: Vec<String>,
    /// Watch remote tags even if `deploy_refs` has no `tags/` pattern.
    #[serde(default)]
    pub watch_tags: bool,
    /// Override global `[act] cache` for this project.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache: Option<bool>,
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

    pub fn effective_trigger_mode(&self, general: &General) -> TriggerMode {
        self.trigger_mode
            .unwrap_or_else(|| general.effective_trigger_mode())
    }

    pub fn effective_step_timeout(&self, general: &General) -> u64 {
        self.step_timeout
            .or(general.step_timeout)
            .unwrap_or(DEFAULT_STEP_TIMEOUT)
            .max(1)
    }

    pub fn watches_tags(&self) -> bool {
        self.watch_tags
            || self
                .deploy_refs
                .iter()
                .any(|r| r.starts_with("tags/") || r.starts_with("refs/tags/"))
    }
}

/// Sanitize a project name into a lowercase ASCII alnum/`-` slug safe for Docker volumes and dirs.
pub fn project_cache_slug(name: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash && !out.is_empty() {
            out.push('-');
            prev_dash = true;
        }
    }
    let trimmed = out.trim_matches('-');
    if trimmed.is_empty() {
        "project".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Normalize a git ref or pattern into `(is_tag, short_name)`.
fn normalize_ref(raw: &str) -> (bool, &str) {
    if let Some(tag) = raw.strip_prefix("refs/tags/") {
        (true, tag)
    } else if let Some(tag) = raw.strip_prefix("tags/") {
        (true, tag)
    } else if let Some(branch) = raw.strip_prefix("refs/heads/") {
        (false, branch)
    } else {
        (false, raw)
    }
}

/// Simple glob matcher supporting `*` (matches zero or more characters).
fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let mut star_pi: Option<usize> = None;
    let mut star_ti: usize = 0;

    while ti < t.len() {
        if pi < p.len() && p[pi] == t[ti] {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star_pi = Some(pi);
            star_ti = ti;
            pi += 1;
        } else if let Some(sp) = star_pi {
            pi = sp + 1;
            star_ti += 1;
            ti = star_ti;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// True when `git_ref` is allowed to run deploy steps for `project`.
///
/// Pull requests (`is_pr == true`) NEVER deploy.
pub fn matches_deploy_ref(project: &ProjectCfg, git_ref: &str, is_pr: bool) -> bool {
    if is_pr {
        return false;
    }
    let (is_tag, short_name) = normalize_ref(git_ref);
    if project.deploy_refs.is_empty() {
        return !is_tag && short_name == project.branch;
    }
    project.deploy_refs.iter().any(|pattern| {
        let (pat_is_tag, pat_short) = normalize_ref(pattern);
        pat_is_tag == is_tag && glob_match(pat_short, short_name)
    })
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

/// Effective timeout for a single step: `step.timeout -> project.step_timeout -> general.step_timeout -> DEFAULT_STEP_TIMEOUT`.
pub fn resolve_step_timeout(
    general: &General,
    project: Option<&ProjectCfg>,
    step: &StepCfg,
) -> std::time::Duration {
    let secs = step
        .timeout
        .or_else(|| project.and_then(|p| p.step_timeout))
        .or(general.step_timeout)
        .unwrap_or(DEFAULT_STEP_TIMEOUT)
        .max(1);
    std::time::Duration::from_secs(secs)
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
        if self.general.poll_interval == 0
            && self.general.effective_trigger_mode() != TriggerMode::Webhook
        {
            return Err(ConfigError::new(
                "general.poll_interval",
                "must be at least 1 second",
            ));
        }
        if self.general.step_timeout == Some(0) {
            return Err(ConfigError::new(
                "general.step_timeout",
                "must be at least 1",
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
            conflict(
                "github.webhook_secret",
                non_empty(&github.webhook_secret),
                "github.webhook_secret_file",
                github.webhook_secret_file.is_some(),
            )?;
            check_secret_file(
                "github.webhook_secret_file",
                github.webhook_secret_file.as_deref(),
            )?;
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
            if let Some(cache_dir) = &act.cache_dir {
                if cache_dir.as_os_str().is_empty() || cache_dir.to_string_lossy().trim().is_empty()
                {
                    return Err(ConfigError::new("act.cache_dir", "must not be empty"));
                }
                let norm_cache = normalize_path(&expand_tilde(cache_dir));
                if norm_cache.parent().is_none() || norm_cache == Path::new("/") {
                    return Err(ConfigError::new(
                        "act.cache_dir",
                        "must not be the filesystem root",
                    ));
                }
                let norm_state = normalize_path(&expand_tilde(&self.state_dir()));
                if norm_cache == norm_state {
                    return Err(ConfigError::new(
                        "act.cache_dir",
                        "must not equal general.state_dir",
                    ));
                }
                for p in &self.projects {
                    if !p.repo_path.as_os_str().is_empty()
                        && norm_cache == normalize_path(&expand_tilde(&p.repo_path))
                    {
                        return Err(ConfigError::new(
                            "act.cache_dir",
                            format!("must not equal project {:?} repo_path", p.name),
                        ));
                    }
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
            if p.name == "."
                || p.name.contains('/')
                || p.name.contains('\\')
                || p.name.contains("..")
                || p.name.chars().any(|c| c.is_ascii_control())
            {
                return Err(ConfigError::new(
                    format!("{base}.name"),
                    "must not contain path separators or '..'",
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
            let effective_poll_interval = p.poll_interval.unwrap_or(self.general.poll_interval);
            if effective_poll_interval == 0
                && p.effective_trigger_mode(&self.general) != TriggerMode::Webhook
            {
                return Err(ConfigError::new(
                    format!("{base}.poll_interval"),
                    "must be at least 1 second when trigger_mode polls",
                ));
            }
            if p.step_timeout == Some(0) {
                return Err(ConfigError::new(
                    format!("{base}.step_timeout"),
                    "must be at least 1",
                ));
            }
            if p.timeout == Some(0) {
                return Err(ConfigError::new(
                    format!("{base}.timeout"),
                    "must be at least 1",
                ));
            }
            if p.deploy_refs.iter().any(|r| r.trim().is_empty()) {
                return Err(ConfigError::new(
                    format!("{base}.deploy_refs"),
                    "must not contain empty entries",
                ));
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
            mask_opt(&mut g.webhook_secret);
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
            unmask_opt(&mut new.webhook_secret, &old.webhook_secret);
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

/// Lexically normalize `.` and `..` segments and strip trailing separators.
fn normalize_path(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(out.components().next_back(), Some(Component::Normal(_))) {
                    out.pop();
                } else if !matches!(
                    out.components().next_back(),
                    Some(Component::RootDir | Component::Prefix(_))
                ) {
                    out.push(comp.as_os_str());
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    if out.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        out
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
            webhook_secret: None,
            webhook_secret_file: None,
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
            webhook_secret: None,
            webhook_secret_file: None,
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
            webhook_secret: Some("wh_standalone_secret".into()),
            webhook_secret_file: None,
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
        assert!(!json.contains("wh_standalone_secret"));
        assert!(!json.contains("ui-token"));
        assert!(json.contains(MASK));

        // Round-tripping the masked JSON keeps every secret.
        let mut back: Config = serde_json::from_str(&json).unwrap();
        back.unmask_from(&cfg);
        assert_eq!(
            back.github.as_ref().unwrap().token.as_deref(),
            Some("ghp_secret")
        );
        assert_eq!(
            back.github.as_ref().unwrap().webhook_secret.as_deref(),
            Some("wh_standalone_secret")
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
            webhook_secret: None,
            webhook_secret_file: None,
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

    #[test]
    fn timeout_resolution_and_validation() {
        let mut general = General::default();
        let mut project = example().projects.remove(0);
        project.step_timeout = None;
        project.timeout = None;
        let mut step = project.ci[0].clone();
        step.timeout = None;

        // Default fallback -> 3600s
        assert_eq!(
            resolve_step_timeout(&general, Some(&project), &step),
            std::time::Duration::from_secs(DEFAULT_STEP_TIMEOUT)
        );
        // General override
        general.step_timeout = Some(1200);
        assert_eq!(
            resolve_step_timeout(&general, Some(&project), &step),
            std::time::Duration::from_secs(1200)
        );
        assert_eq!(project.effective_step_timeout(&general), 1200);
        // Project override
        project.step_timeout = Some(900);
        assert_eq!(
            resolve_step_timeout(&general, Some(&project), &step),
            std::time::Duration::from_secs(900)
        );
        assert_eq!(project.effective_step_timeout(&general), 900);
        // Step override
        step.timeout = Some(45);
        assert_eq!(
            resolve_step_timeout(&general, Some(&project), &step),
            std::time::Duration::from_secs(45)
        );

        // Validation rejects 0 on general.step_timeout, project[i].step_timeout, project[i].timeout
        let mut cfg = example();
        cfg.general.step_timeout = Some(0);
        let err = cfg.validate().unwrap_err();
        assert_eq!(err.field.as_deref(), Some("general.step_timeout"));

        let mut cfg = example();
        cfg.projects[0].step_timeout = Some(0);
        let err = cfg.validate().unwrap_err();
        assert_eq!(err.field.as_deref(), Some("project[0].step_timeout"));

        let mut cfg = example();
        cfg.projects[0].timeout = Some(0);
        let err = cfg.validate().unwrap_err();
        assert_eq!(err.field.as_deref(), Some("project[0].timeout"));
    }

    #[test]
    fn deploy_ref_matching_and_pr_never_deploys() {
        let mut p = example().projects.remove(0);
        p.branch = "master".into();
        p.deploy_refs.clear();
        p.watch_tags = false;

        // Default (empty deploy_refs): only watched branch deploys.
        assert!(!p.watches_tags());
        assert!(matches_deploy_ref(&p, "master", false));
        assert!(matches_deploy_ref(&p, "refs/heads/master", false));
        assert!(!matches_deploy_ref(&p, "main", false));
        assert!(!matches_deploy_ref(&p, "develop", false));
        assert!(!matches_deploy_ref(&p, "tags/master", false));
        assert!(!matches_deploy_ref(&p, "refs/tags/master", false));
        // PRs NEVER deploy, even if head_ref happens to be named `master`.
        assert!(!matches_deploy_ref(&p, "master", true));
        assert!(!matches_deploy_ref(&p, "refs/heads/master", true));

        // Explicit branch & tag globs.
        p.deploy_refs = vec![
            "main".into(),
            "release/*".into(),
            "refs/heads/hotfix-*".into(),
            "tags/v*".into(),
            "refs/tags/prod-*".into(),
        ];
        assert!(p.watches_tags());
        assert!(!matches_deploy_ref(&p, "master", false));
        assert!(matches_deploy_ref(&p, "main", false));
        assert!(matches_deploy_ref(&p, "refs/heads/main", false));
        assert!(matches_deploy_ref(&p, "release/1.2", false));
        assert!(matches_deploy_ref(&p, "refs/heads/release/2.0-rc1", false));
        assert!(matches_deploy_ref(&p, "hotfix-urgent", false));
        assert!(!matches_deploy_ref(&p, "feature/x", false));
        // Tags match tag patterns only (both tags/ and refs/tags/ forms).
        assert!(matches_deploy_ref(&p, "tags/v1.0.0", false));
        assert!(matches_deploy_ref(&p, "refs/tags/v2.3.4-beta", false));
        assert!(matches_deploy_ref(&p, "tags/prod-2026", false));
        assert!(matches_deploy_ref(&p, "refs/tags/prod-1", false));
        assert!(!matches_deploy_ref(&p, "tags/nightly", false));
        assert!(!matches_deploy_ref(&p, "v1.0.0", false)); // branch named v1.0.0 != tag
                                                           // Still NEVER deploys on PR!
        assert!(!matches_deploy_ref(&p, "main", true));
        assert!(!matches_deploy_ref(&p, "release/1.0", true));
        assert!(!matches_deploy_ref(&p, "tags/v1.0.0", true));

        // watch_tags alone enables tag watching.
        p.deploy_refs = vec!["master".into()];
        assert!(!p.watches_tags());
        p.watch_tags = true;
        assert!(p.watches_tags());
    }

    #[test]
    fn act_cache_defaults_and_slug() {
        let act = ActCfg::default();
        assert!(act.cache);
        assert_eq!(
            act.effective_cache_dir(Path::new("/state")),
            PathBuf::from("/state/cache")
        );
        let custom = ActCfg {
            cache: false,
            cache_dir: Some(PathBuf::from("/var/cache/lunacy")),
            ..ActCfg::default()
        };
        assert!(!custom.cache);
        assert_eq!(
            custom.effective_cache_dir(Path::new("/state")),
            PathBuf::from("/var/cache/lunacy")
        );

        assert_eq!(project_cache_slug("FrostEdge"), "frostedge");
        assert_eq!(project_cache_slug("my_cool app/1"), "my-cool-app-1");
        assert_eq!(project_cache_slug("___"), "project");
    }

    #[test]
    fn project_name_rejects_path_traversal_and_control_chars() {
        for bad in [
            "foo/bar",
            "foo\\bar",
            "..",
            "a..b",
            ".",
            "bad\nname",
            "a\0b",
        ] {
            let mut cfg = example();
            cfg.projects[0].name = bad.into();
            let err = cfg.validate().unwrap_err();
            assert_eq!(
                err.field.as_deref(),
                Some("project[0].name"),
                "name={bad:?}"
            );
            assert!(
                err.message.contains("path separators") || err.message.contains(".."),
                "name={bad:?} msg={}",
                err.message
            );
        }
    }

    #[test]
    fn act_cache_dir_rejects_root_state_dir_and_repo_path() {
        for bad in ["", "   ", "/", "///", "/tmp/.."] {
            let mut cfg = example();
            cfg.act = Some(ActCfg {
                cache_dir: Some(PathBuf::from(bad)),
                ..ActCfg::default()
            });
            let err = cfg.validate().unwrap_err();
            assert_eq!(err.field.as_deref(), Some("act.cache_dir"), "bad={bad:?}");
        }

        // Equal to general.state_dir (even with trailing slash).
        let mut cfg = example();
        cfg.general.state_dir = Some(PathBuf::from("/var/lib/lunacy"));
        cfg.act = Some(ActCfg {
            cache_dir: Some(PathBuf::from("/var/lib/lunacy/")),
            ..ActCfg::default()
        });
        let err = cfg.validate().unwrap_err();
        assert_eq!(err.field.as_deref(), Some("act.cache_dir"));

        // Equal to a project's repo_path.
        let mut cfg = example();
        let repo = cfg.projects[0].repo_path.clone();
        cfg.act = Some(ActCfg {
            cache_dir: Some(repo),
            ..ActCfg::default()
        });
        let err = cfg.validate().unwrap_err();
        assert_eq!(err.field.as_deref(), Some("act.cache_dir"));

        // Distinct non-root cache_dir is accepted.
        let mut cfg = example();
        cfg.act = Some(ActCfg {
            cache_dir: Some(PathBuf::from("/var/cache/lunacy")),
            ..ActCfg::default()
        });
        cfg.validate().expect("valid cache_dir");
    }

    #[test]
    fn trigger_mode_and_standalone_webhook_secret_validation() {
        let dir = tempfile::tempdir().expect("tempdir");
        let secret_file = dir.path().join("wh.secret");
        std::fs::write(&secret_file, "  file-secret\n").unwrap();

        // Default poll_interval == 0 is rejected (Both mode).
        let mut cfg = example();
        cfg.general.poll_interval = 0;
        let err = cfg.validate().unwrap_err();
        assert_eq!(err.field.as_deref(), Some("general.poll_interval"));
        assert!(err.message.contains("at least 1 second"), "{}", err.message);

        // Webhook mode allows poll_interval == 0 globally and per-project.
        cfg.general.trigger_mode = Some(TriggerMode::Webhook);
        cfg.projects[0].poll_interval = Some(0);
        cfg.validate().expect("webhook mode allows poll_interval=0");

        // Overriding a project back to Poll rejects project poll_interval == 0.
        cfg.projects[0].trigger_mode = Some(TriggerMode::Poll);
        let err = cfg.validate().unwrap_err();
        assert_eq!(err.field.as_deref(), Some("project[0].poll_interval"));
        assert_eq!(
            cfg.projects[0].effective_trigger_mode(&cfg.general),
            TriggerMode::Poll
        );
        assert_eq!(
            cfg.projects[1].effective_trigger_mode(&cfg.general),
            TriggerMode::Webhook
        );

        // Standalone github.webhook_secret takes precedence over github.app.webhook_secret.
        let gh = GithubCfg {
            mode: None,
            token: None,
            token_file: None,
            api_url: None,
            webhook_secret: None,
            webhook_secret_file: Some(secret_file.clone()),
            app: Some(AppCfg {
                app_id: 1,
                private_key_path: None,
                private_key: Some("pem".into()),
                installation_id: None,
                webhook_secret: Some("app-secret".into()),
                webhook_secret_file: None,
            }),
        };
        assert_eq!(gh.webhook_secret_value().as_deref(), Some("file-secret"));

        // Falls back to app secret when standalone is unset.
        let mut gh_fallback = gh.clone();
        gh_fallback.webhook_secret_file = None;
        assert_eq!(
            gh_fallback.webhook_secret_value().as_deref(),
            Some("app-secret")
        );

        // Mutual exclusion between github.webhook_secret and github.webhook_secret_file.
        let mut cfg2 = example();
        cfg2.github = Some(GithubCfg {
            mode: None,
            token: None,
            token_file: None,
            api_url: None,
            webhook_secret: Some("inline".into()),
            webhook_secret_file: Some(secret_file),
            app: None,
        });
        let err = cfg2.validate().unwrap_err();
        assert_eq!(err.field.as_deref(), Some("github.webhook_secret_file"));
    }

    #[test]
    fn inherited_zero_poll_interval_rejected_when_project_overrides_to_poll_or_both() {
        let mut cfg = example();
        cfg.general.poll_interval = 0;
        cfg.general.trigger_mode = Some(TriggerMode::Webhook);
        cfg.projects[0].poll_interval = None;
        cfg.projects[0].trigger_mode = None;
        cfg.projects[1].poll_interval = None;
        cfg.projects[1].trigger_mode = None;
        cfg.validate()
            .expect("webhook mode globally allows general.poll_interval = 0");

        // Project inherits poll_interval = 0 from general, overrides trigger_mode to Poll -> error.
        cfg.projects[0].trigger_mode = Some(TriggerMode::Poll);
        let err = cfg.validate().unwrap_err();
        assert_eq!(err.field.as_deref(), Some("project[0].poll_interval"));
        assert!(
            err.message
                .contains("must be at least 1 second when trigger_mode polls"),
            "{}",
            err.message
        );

        // Same for trigger_mode = Both.
        cfg.projects[0].trigger_mode = Some(TriggerMode::Both);
        let err = cfg.validate().unwrap_err();
        assert_eq!(err.field.as_deref(), Some("project[0].poll_interval"));
        assert!(
            err.message
                .contains("must be at least 1 second when trigger_mode polls"),
            "{}",
            err.message
        );

        // Explicit positive project poll_interval fixes it.
        cfg.projects[0].poll_interval = Some(60);
        cfg.validate()
            .expect("explicit positive project poll_interval is valid");
    }
}
