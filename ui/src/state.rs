//! App state, the polling machine, navigation, and every handler the screens call.
//!
//! The UI thread never blocks: reads go through [`Bus`], which uses `rune::Latest` on native (a
//! shared worker pool) and `spawn_local` + `fetch` on web. Answers land in `poll`, and one
//! `tx.wake_at` for the next deadline is what keeps an idle window from redrawing.

use std::collections::{BTreeSet, HashMap};

#[cfg(not(target_arch = "wasm32"))]
use rune::Latest;
use rune::{TextEdit, Tx};
use serde_json::Value;

use crate::api::{self, Answer, Call};
use crate::config::{Project, Step};
use crate::model::{
    DeployChecksReport, Project as ProjectView, ProjectState, Run, StepStatus, Substep, SystemInfo,
    Workflow,
};
use crate::net::Failure;
use crate::paths::{self, Key};
use crate::updater::{UpdateStatus, Updater};

/// The projects list's poll period (`docs/api.md`: "every ~2 s").
pub const PROJECTS_EVERY: f64 = 2.0;
/// The open run's period while it runs ("every ~1 s").
pub const RUN_EVERY: f64 = 1.0;
/// A finished run is re-read a few times, in case the daemon settles late.
pub const RUN_SETTLE_EVERY: f64 = 6.0;
/// The open step's log, and the run tree carrying its substeps.
pub const LOG_EVERY: f64 = 1.5;
/// A project's run history.
pub const RUNS_EVERY: f64 = 5.0;

pub const DEFAULT_SERVER: &str = "http://127.0.0.1:7878";

// ---------------------------------------------------------------------------------------------
// Connection

/// The daemon the user picked: a base URL and an optional bearer token.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Conn {
    pub server: String,
    pub token: String,
}

/// Web: the page's own origin, because the daemon serves `ui/dist` from `web_root`. Native:
/// localhost, where the daemon listens by default.
pub fn default_server() -> String {
    #[cfg(target_arch = "wasm32")]
    {
        if let Some(origin) = web_origin() {
            return origin;
        }
    }
    DEFAULT_SERVER.to_string()
}

/// The run a `#/runs/{id}` address names at startup, if any.
#[cfg(target_arch = "wasm32")]
fn initial_run_id() -> Option<String> {
    let hash = web_sys::window()?.location().hash().ok()?;
    crate::net::hash_run_id(&hash)
}

#[cfg(not(target_arch = "wasm32"))]
fn initial_run_id() -> Option<String> {
    None
}

#[cfg(target_arch = "wasm32")]
fn web_origin() -> Option<String> {
    let loc = web_sys::window()?.location();
    let proto = loc.protocol().ok()?;
    let host = loc.host().ok()?;
    // A `file:` page has no origin to call an API on.
    if host.is_empty() || (proto != "http:" && proto != "https:") {
        return None;
    }
    Some(format!("{proto}//{host}"))
}

/// A URL the user typed, as `format!("{server}/api…")` builds on.
pub fn normalize_server(raw: &str) -> String {
    let t = raw.trim();
    if t.is_empty() {
        return String::new();
    }
    let with_scheme = if t.contains("://") {
        t.to_string()
    } else {
        format!("http://{t}")
    };
    with_scheme.trim_end_matches('/').to_string()
}

/// The connection the Settings fields describe. A pasted token often brings a newline.
pub fn conn_from_fields(server: &str, token: &str) -> Conn {
    Conn {
        server: normalize_server(server),
        token: token.trim().to_string(),
    }
}

/// Launch settings: `--server URL --token T`, then `LUNACY_SERVER` / `LUNACY_TOKEN`.
pub fn conn_from_env() -> Conn {
    let mut conn = Conn {
        server: default_server(),
        token: String::new(),
    };
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--server" => {
                if let Some(v) = args.next() {
                    conn.server = normalize_server(&v);
                }
            }
            "--token" => {
                if let Some(v) = args.next() {
                    conn.token = v;
                }
            }
            _ => {
                if let Some(v) = a.strip_prefix("--server=") {
                    conn.server = normalize_server(v);
                } else if let Some(v) = a.strip_prefix("--token=") {
                    conn.token = v.to_string();
                }
            }
        }
    }
    // The environment wins over the command line: it is what a service manager sets.
    for (var, field) in [("LUNACY_SERVER", 0), ("LUNACY_TOKEN", 1)] {
        if let Ok(v) = std::env::var(var) {
            if v.trim().is_empty() {
                continue;
            }
            if field == 0 {
                conn.server = normalize_server(&v);
            } else {
                conn.token = v;
            }
        }
    }
    // Web: the token this browser saved last time (the page has no command line).
    if conn.token.is_empty() {
        if let Some(t) = stored_token() {
            conn.token = t;
        }
    }
    conn
}

#[cfg(target_arch = "wasm32")]
const TOKEN_KEY: &str = "lunacy.token";
#[cfg(target_arch = "wasm32")]
const THEME_STORAGE_KEY: &str = "lunacy.theme";
#[cfg(target_arch = "wasm32")]
const ZOOM_STORAGE_KEY: &str = "lunacy.zoom";
#[cfg(target_arch = "wasm32")]
const LOG_WRAP_STORAGE_KEY: &str = "lunacy.log_wrap";
#[cfg(not(target_arch = "wasm32"))]
const PREFS_APP_ID: &str = "lunacy";
#[cfg(not(target_arch = "wasm32"))]
const PREF_THEME_KEY: &str = "theme";
#[cfg(not(target_arch = "wasm32"))]
const PREF_ZOOM_KEY: &str = "zoom";
#[cfg(not(target_arch = "wasm32"))]
const PREF_LOG_WRAP_KEY: &str = "log_wrap";
#[cfg(not(target_arch = "wasm32"))]
const PREF_AUTO_UPDATE_KEY: &str = "auto_update_check";

/// Supported discrete UI zoom stops shown in Appearance and stepped by `Ctrl/Cmd` `=`/`-`.
pub const ZOOM_STEPS: [f32; 5] = [0.9, 1.0, 1.1, 1.25, 1.5];

fn sanitize_zoom(z: f32) -> Option<f32> {
    (z.is_finite() && (0.5..=2.5).contains(&z)).then_some(z)
}

#[cfg(target_arch = "wasm32")]
fn local_storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok()?
}

#[cfg(target_arch = "wasm32")]
fn stored_token() -> Option<String> {
    local_storage()?
        .get_item(TOKEN_KEY)
        .ok()?
        .filter(|t| !t.is_empty())
}

#[cfg(not(target_arch = "wasm32"))]
fn stored_token() -> Option<String> {
    None
}

/// Web: remember the token in this browser so a reload or a GitHub link stays signed in.
#[cfg(target_arch = "wasm32")]
fn store_token(token: &str) {
    if let Some(s) = local_storage() {
        let _ = if token.is_empty() {
            s.remove_item(TOKEN_KEY)
        } else {
            s.set_item(TOKEN_KEY, token)
        };
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn store_token(_: &str) {}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn load_prefs_from(
    prefs: &rune::prefs::Prefs,
) -> (Option<crate::theme::ThemeMode>, Option<f32>, Option<bool>) {
    let theme = prefs
        .get::<String>(PREF_THEME_KEY)
        .as_deref()
        .and_then(crate::theme::ThemeMode::parse);
    let zoom = prefs.get::<f32>(PREF_ZOOM_KEY).and_then(sanitize_zoom);
    let log_wrap = prefs.get::<bool>(PREF_LOG_WRAP_KEY);
    (theme, zoom, log_wrap)
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn save_theme_to(prefs: &mut rune::prefs::Prefs, mode: crate::theme::ThemeMode) {
    let _ = prefs.set(PREF_THEME_KEY, mode.as_str());
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn save_zoom_to(prefs: &mut rune::prefs::Prefs, zoom: Option<f32>) {
    match zoom.and_then(sanitize_zoom) {
        Some(z) => {
            let _ = prefs.set(PREF_ZOOM_KEY, &z);
        }
        None => {
            let _ = prefs.remove(PREF_ZOOM_KEY);
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn save_log_wrap_to(prefs: &mut rune::prefs::Prefs, wrap: bool) {
    let _ = prefs.set(PREF_LOG_WRAP_KEY, &wrap);
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn load_auto_update_from(prefs: &rune::prefs::Prefs) -> Option<bool> {
    prefs.get::<bool>(PREF_AUTO_UPDATE_KEY)
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn save_auto_update_to(prefs: &mut rune::prefs::Prefs, enabled: bool) {
    let _ = prefs.set(PREF_AUTO_UPDATE_KEY, &enabled);
}

fn load_auto_update_pref() -> Option<bool> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let prefs = rune::prefs::Prefs::open(PREFS_APP_ID);
        load_auto_update_from(&prefs)
    }
    #[cfg(target_arch = "wasm32")]
    {
        None
    }
}

fn save_auto_update_pref(enabled: bool) {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let mut prefs = rune::prefs::Prefs::open(PREFS_APP_ID);
        save_auto_update_to(&mut prefs, enabled);
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = enabled;
    }
}

fn load_window_prefs() -> (Option<crate::theme::ThemeMode>, Option<f32>, Option<bool>) {
    #[cfg(target_arch = "wasm32")]
    {
        let Some(s) = local_storage() else {
            return (None, None, None);
        };
        let theme = s
            .get_item(THEME_STORAGE_KEY)
            .ok()
            .flatten()
            .as_deref()
            .and_then(crate::theme::ThemeMode::parse);
        let zoom = s
            .get_item(ZOOM_STORAGE_KEY)
            .ok()
            .flatten()
            .and_then(|v| v.parse::<f32>().ok())
            .and_then(sanitize_zoom);
        let log_wrap = s
            .get_item(LOG_WRAP_STORAGE_KEY)
            .ok()
            .flatten()
            .and_then(|v| match v.trim() {
                "true" => Some(true),
                "false" => Some(false),
                _ => None,
            });
        (theme, zoom, log_wrap)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let prefs = rune::prefs::Prefs::open(PREFS_APP_ID);
        load_prefs_from(&prefs)
    }
}

fn save_theme_pref(mode: crate::theme::ThemeMode) {
    #[cfg(target_arch = "wasm32")]
    if let Some(s) = local_storage() {
        let _ = s.set_item(THEME_STORAGE_KEY, mode.as_str());
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let mut prefs = rune::prefs::Prefs::open(PREFS_APP_ID);
        save_theme_to(&mut prefs, mode);
    }
}

fn save_zoom_pref(zoom: Option<f32>) {
    #[cfg(target_arch = "wasm32")]
    if let Some(s) = local_storage() {
        let _ = match zoom.and_then(sanitize_zoom) {
            Some(z) => s.set_item(ZOOM_STORAGE_KEY, &format!("{z}")),
            None => s.remove_item(ZOOM_STORAGE_KEY),
        };
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let mut prefs = rune::prefs::Prefs::open(PREFS_APP_ID);
        save_zoom_to(&mut prefs, zoom);
    }
}

fn save_log_wrap_pref(wrap: bool) {
    #[cfg(target_arch = "wasm32")]
    if let Some(s) = local_storage() {
        let _ = s.set_item(LOG_WRAP_STORAGE_KEY, if wrap { "true" } else { "false" });
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let mut prefs = rune::prefs::Prefs::open(PREFS_APP_ID);
        save_log_wrap_to(&mut prefs, wrap);
    }
}

// ---------------------------------------------------------------------------------------------
// Requests

/// One latest-wins slot per kind of read, so a log poll never cancels the projects poll.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    Projects,
    Runs,
    Run,
    Log,
    Config,
    System,
    Workflows,
    Trigger,
    Cancel,
    DeployChecks,
    ForceDeploy,
    ClearCache,
}

const SLOTS: usize = 12;

fn slot_of(slot: Slot) -> usize {
    match slot {
        Slot::Projects => 0,
        Slot::Runs => 1,
        Slot::Run => 2,
        Slot::Log => 3,
        Slot::Config => 4,
        Slot::System => 5,
        Slot::Workflows => 6,
        Slot::Trigger => 7,
        Slot::Cancel => 8,
        Slot::DeployChecks => 9,
        Slot::ForceDeploy => 10,
        Slot::ClearCache => 11,
    }
}

fn slot_index(slot: Slot) -> usize {
    slot_of(slot)
}

pub type Outcome = Result<Answer, Failure>;

/// The request layer: fire a `Call`, drain the answers later. Never blocks the UI thread.
pub struct Bus {
    #[cfg(not(target_arch = "wasm32"))]
    latest: [Latest<(Call, Outcome)>; SLOTS],
    #[cfg(target_arch = "wasm32")]
    handle: Option<rune::AppHandle<State>>,
    #[cfg(target_arch = "wasm32")]
    seq: [u64; SLOTS],
    #[cfg(target_arch = "wasm32")]
    live: [u64; SLOTS],
    #[cfg(target_arch = "wasm32")]
    next: u64,
}

impl Default for Bus {
    fn default() -> Self {
        Bus {
            #[cfg(not(target_arch = "wasm32"))]
            latest: std::array::from_fn(|_| Latest::default()),
            #[cfg(target_arch = "wasm32")]
            handle: None,
            #[cfg(target_arch = "wasm32")]
            seq: [0; SLOTS],
            #[cfg(target_arch = "wasm32")]
            live: [0; SLOTS],
            #[cfg(target_arch = "wasm32")]
            next: 0,
        }
    }
}

impl Bus {
    /// Native: the runtime's waker, so a landing answer runs `poll`.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn set_waker(&self, waker: rune::Waker) {
        for latest in &self.latest {
            latest.set_waker(waker.clone());
        }
    }

    #[cfg(target_arch = "wasm32")]
    pub fn attach(&mut self, handle: rune::AppHandle<State>) {
        self.handle = Some(handle);
    }

    pub fn wants_any(&self, slot: Slot) -> bool {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.latest[slot_of(slot)].is_pending()
        }
        #[cfg(target_arch = "wasm32")]
        {
            self.live[slot_of(slot)] != 0
        }
    }

    pub fn cancel(&mut self) {
        #[cfg(not(target_arch = "wasm32"))]
        for latest in &self.latest {
            latest.cancel();
        }
        #[cfg(target_arch = "wasm32")]
        {
            for s in &mut self.seq {
                self.next += 1;
                *s = self.next;
            }
            self.live = [0; SLOTS];
        }
    }

    pub fn send(&mut self, slot: Slot, base: &str, token: &str, call: Call) {
        let (base, token) = (base.to_string(), token.to_string());
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.latest[slot_index(slot)].request(call.clone(), move |c| {
                let out = api::run(&base, &token, &c);
                (c, out)
            });
        }
        #[cfg(target_arch = "wasm32")]
        {
            let Some(handle) = self.handle.clone() else {
                return;
            };
            self.next += 1;
            let id = self.next;
            let index = slot_index(slot);
            self.seq[index] = id;
            self.live[index] = id;
            let target = call.clone();
            wasm_bindgen_futures::spawn_local(async move {
                let out = api::run(&base, &token, &target).await;
                let (call, seq) = (target, id);
                handle.update_if(move |s, tx| {
                    let index = slot_index(slot);
                    // A newer request in this slot already replaced this one.
                    if s.bus.seq[index] != seq {
                        return false;
                    }
                    s.bus.live[index] = 0;
                    s.wants(&call) && s.answer(call, out, tx)
                });
            });
        }
    }

    /// Every answer that landed since the last drain.
    fn drain(&self) -> Vec<(Call, Outcome)> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let mut out = Vec::new();
            for latest in &self.latest {
                while let Some(answer) = latest.take() {
                    out.push(answer);
                }
            }
            out
        }
        #[cfg(target_arch = "wasm32")]
        {
            Vec::new()
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Small value types

/// Where the window is.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum View {
    #[default]
    Projects,
    Project {
        name: String,
    },
    Run {
        id: String,
    },
    Settings {
        page: SettingsPage,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SettingsPage {
    #[default]
    General,
    Server,
    Github,
    Discord,
    Act,
    Projects,
    Deploy,
    Diagnostics,
    Connection,
    Appearance,
}

impl SettingsPage {
    /// The daemon's config pages, in sidebar order.
    pub const CONFIG: [SettingsPage; 6] = [
        SettingsPage::General,
        SettingsPage::Server,
        SettingsPage::Github,
        SettingsPage::Discord,
        SettingsPage::Act,
        SettingsPage::Projects,
    ];

    /// Window-local / inspection pages (not written to `config.toml`), in sidebar order.
    pub const WINDOW: [SettingsPage; 4] = [
        SettingsPage::Deploy,
        SettingsPage::Diagnostics,
        SettingsPage::Connection,
        SettingsPage::Appearance,
    ];

    /// Whether this page belongs to this window or read-only inspection rather than `config.toml`.
    pub fn is_local(self) -> bool {
        matches!(
            self,
            SettingsPage::Deploy
                | SettingsPage::Diagnostics
                | SettingsPage::Connection
                | SettingsPage::Appearance
        )
    }

    pub fn title(self) -> &'static str {
        match self {
            SettingsPage::General => "General",
            SettingsPage::Server => "Server",
            SettingsPage::Github => "GitHub",
            SettingsPage::Discord => "Discord",
            SettingsPage::Act => "act",
            SettingsPage::Projects => "Projects",
            SettingsPage::Deploy => "Deploy",
            SettingsPage::Diagnostics => "Diagnostics",
            SettingsPage::Connection => "Connection",
            SettingsPage::Appearance => "Appearance",
        }
    }

    pub fn blurb(self) -> &'static str {
        match self {
            SettingsPage::General => {
                "State directory, poll interval, step timeouts and the default shell."
            }
            SettingsPage::Server => {
                "Where the HTTP API listens, its token, and the web build's root."
            }
            SettingsPage::Github => "How check runs are reported: a token, or a GitHub App.",
            SettingsPage::Discord => "Webhook notifications when a pipeline starts or finishes.",
            SettingsPage::Act => {
                "The act binary, its image mapping, its arguments and the lock it holds."
            }
            SettingsPage::Projects => {
                "Repositories to watch, and the CI and deploy steps for each."
            }
            SettingsPage::Deploy => {
                "Pre-flight deploy checks, checkout safety, timeouts, and step readiness."
            }
            SettingsPage::Diagnostics => "The daemon's host, tools and config file.",
            SettingsPage::Connection => {
                "This window's server URL and token, not part of config.toml."
            }
            SettingsPage::Appearance => "Theme mode, interface scale, and desktop updates.",
        }
    }
}

/// Which half of a step's log the reader asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogMode {
    Tail,
    Full,
}

impl LogMode {
    pub fn tail(self) -> Option<usize> {
        match self {
            LogMode::Tail => Some(2000),
            LogMode::Full => None,
        }
    }
}

/// One step's log, as far as it has loaded.
#[derive(Default, Clone)]
pub struct LogState {
    pub text: String,
    pub loading: bool,
    pub error: Option<String>,
    pub mode: Option<LogMode>,
}

/// How reachable the daemon is.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Link {
    /// Nothing has answered yet.
    #[default]
    Connecting,
    Ok,
    Down {
        why: String,
        failures: u32,
    },
    Unauthorized,
}

impl Link {
    pub fn is_ok(&self) -> bool {
        matches!(self, Link::Ok)
    }
    pub fn is_broken(&self) -> bool {
        matches!(self, Link::Down { .. } | Link::Unauthorized)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    Info,
    Success,
    Error,
}

#[derive(Debug, Clone)]
pub struct Toast {
    pub id: u64,
    pub title: String,
    pub body: String,
    pub kind: ToastKind,
    pub until: f64,
}

/// The config editor. The draft is the daemon's JSON kept verbatim, so unknown keys survive a
/// save and a rejected field is addressed by the daemon's own path.
pub struct Editor {
    /// The config as last loaded or saved (secrets masked).
    pub base: Value,
    /// What the form edits.
    pub draft: Value,
    pub loaded: bool,
    pub saving: bool,
    /// The daemon's message for the rejected field.
    pub error: Option<String>,
    pub field: Option<String>,
    /// `project[i]` of the project whose detail is open; `None` shows the list.
    pub project: Option<usize>,
    /// `(project, deploy, index)` of the step being edited.
    pub step: Option<(usize, bool, usize)>,
    /// Text fields by config path. Seeded from `draft`, then the user's own.
    pub text: HashMap<String, TextEdit>,
    pub seeded: HashMap<String, String>,
    pub workflows: Vec<Workflow>,
    /// The step path the loaded `workflows` belong to.
    pub workflows_for: Option<String>,
    pub workflows_loading: bool,
    /// A project delete waiting for confirmation.
    pub confirm_delete: Option<usize>,
    /// Whether this project was just added and has never been saved.
    pub new_project: bool,
    /// The settings page the sidebar last selected, kept across navigation.
    pub page: SettingsPage,
}

impl Default for Editor {
    fn default() -> Self {
        Editor {
            base: Value::Null,
            draft: Value::Null,
            loaded: false,
            saving: false,
            error: None,
            field: None,
            project: None,
            step: None,
            text: HashMap::new(),
            seeded: HashMap::new(),
            workflows: Vec::new(),
            workflows_for: None,
            workflows_loading: false,
            confirm_delete: None,
            new_project: false,
            page: SettingsPage::General,
        }
    }
}

impl Editor {
    pub fn dirty(&self) -> bool {
        self.loaded && self.base != self.draft
    }

    /// Whether the daemon blamed this field.
    pub fn invalid(&self, path: &str) -> bool {
        self.field
            .as_deref()
            .is_some_and(|f| paths::same_field(f, path))
    }

    /// The inline error for a field, when the daemon blamed it.
    pub fn error_for(&self, path: &str) -> Option<&str> {
        if self.invalid(path) {
            self.error.as_deref()
        } else {
            None
        }
    }

    /// A field's `TextEdit`, seeded from the draft when the draft changed under it.
    /// The `TextEdit` bound to `path`, seeded from the draft when the draft changed under it.
    ///
    /// Returning a borrow (not a clone) is what `on_edit` requires: the control mutates this
    /// field in place. Re-seeding only when the underlying value moved keeps a caret from being
    /// reset on every frame.
    pub fn text_for(&mut self, path: &str, seeded: String) -> &mut TextEdit {
        if self.seeded.get(path).is_none_or(|s| *s != seeded) || !self.text.contains_key(path) {
            self.seeded.insert(path.to_string(), seeded.clone());
            self.text.insert(path.to_string(), TextEdit::new(seeded));
        }
        self.text.get_mut(path).expect("just inserted")
    }

    /// Drops the cached edits so every field re-seeds from the draft.
    pub fn forget_text(&mut self) {
        self.text.clear();
        self.seeded.clear();
    }

    /// Records what a field's handler wrote, so the view keeps it.
    pub fn set_text(&mut self, path: &str, edit: TextEdit) {
        self.text.insert(path.to_string(), edit);
    }

    /// Records the seed value corresponding to an active edit so `text_for` does not reset the
    /// cursor when `on_change` writes back to `draft`.
    pub fn sync_seeded(&mut self, path: &str, seeded: String) {
        self.seeded.insert(path.to_string(), seeded);
    }

    /// Rebuilds every field from the draft (after a load, a save or a discard).
    pub fn reseed(&mut self) {
        self.forget_text();
    }

    pub fn s(&self, path: &str) -> String {
        paths::get_str(&self.draft, path)
    }

    pub fn set(&mut self, path: &str, value: Value) {
        paths::set(&mut self.draft, path, value);
    }

    pub fn set_str(&mut self, path: &str, s: &str) {
        paths::set_opt_str(&mut self.draft, path, s);
    }

    pub fn set_str_mode(&mut self, path: &str, s: &str, optional: bool) {
        paths::set_str(&mut self.draft, path, s, optional);
    }

    pub fn count(&self, path: &str) -> usize {
        paths::array_len(&self.draft, path)
    }
}

// ---------------------------------------------------------------------------------------------
// State

pub struct State {
    pub conn: Conn,
    /// The Connection page's editable fields, kept apart from `conn` until Connect is pressed.
    pub ui_server: TextEdit,
    pub ui_token: TextEdit,
    /// The connection the daemon last answered on.
    pub live: Conn,
    pub link: Link,
    pub version: String,

    pub projects: Vec<ProjectView>,
    pub loaded: bool,
    pub projects_error: Option<String>,

    pub view: View,
    pub runs: Vec<Run>,
    pub runs_for: Option<String>,
    pub runs_loading: bool,
    pub runs_error: Option<String>,
    pub run: Option<Run>,
    pub run_error: Option<String>,
    /// The run detail's open step.
    pub open_step: Option<usize>,
    pub logs: HashMap<(String, usize), LogState>,

    pub editor: Editor,
    pub system: Option<SystemInfo>,
    pub system_error: Option<String>,
    pub deploy_checks: Option<DeployChecksReport>,
    pub deploy_checks_error: Option<String>,
    pub deploy_checks_loading: bool,

    pub toasts: Vec<Toast>,
    pub(crate) next_toast: u64,
    /// A "Run now" in flight, by project.
    pub triggering: Option<String>,
    /// A "Cancel" in flight, by run id or project name.
    pub cancelling: Option<String>,
    /// A "Force deploy" in flight, by project name or run id.
    pub force_deploying: Option<String>,
    /// A cache-clear request in flight.
    pub clearing_cache: bool,
    /// The force toggle beside "Run now".
    pub force: bool,

    pub bus: Bus,
    pub update_status: UpdateStatus,
    pub update_dismissed: bool,
    pub auto_update_check: bool,
    pub(crate) updater: Updater,
    #[cfg(not(target_arch = "wasm32"))]
    auto_update_triggered: bool,

    // Deadlines on the runtime's clock.
    next_projects: f64,
    next_runs: f64,
    next_run: f64,
    next_log: f64,
    next_system: f64,
    /// Whether the one-off version read has been sent.
    asked_system: bool,

    pub clock: f64,
    /// Set after the first frame, so network work starts once there is something to show.
    pub ready: bool,
    /// True for static `--scene` fixtures: keeps `poll` from talking to any network daemon.
    pub demo: bool,
    /// Compact navigation drawer open state (used below ~720pt width).
    pub drawer_open: bool,
    /// User-chosen window theme (`System`, `Light`, `Dark`, or `Oled`).
    pub theme_mode: crate::theme::ThemeMode,
    /// User-chosen UI zoom factor (`None` = responsive default: 1.0 compact, 1.1 desktop).
    pub user_zoom: Option<f32>,
    /// Whether step logs wrap long lines to fit the pane (persisted; default `true`).
    pub log_wrap: bool,
    /// Whether `act` steps show the raw log stream instead of the expandable Steps summary accordion.
    pub log_view_raw: bool,
    /// Tracks `(run_id, step_index, substep_index)` toggled open by the user in the Steps accordion.
    pub expanded_substeps: BTreeSet<(String, usize, usize)>,
    /// Tracks `(run_id, step_index)` where the user has manually interacted with the accordion.
    pub substep_toggled: BTreeSet<(String, usize)>,
    /// Lets users collapse/expand a shell step's log summary header cleanly.
    pub shell_log_collapsed: bool,
    /// Tracks `(commit_gen, target_zoom)` so responsive default zoom never oscillates.
    pub(crate) zoom_track: std::cell::Cell<(u64, f32)>,
    /// Re-scroll run log to bottom once the next active `Answer::Log` arrives (run open / Tail<->Full).
    pub(crate) stick_log_pending: bool,
}

impl State {
    pub fn new(conn: Conn) -> State {
        let mut state = State {
            // Requests read `live`; start from the launch connection (flags, env, or the
            // browser's saved token) so the first poll is already authenticated.
            live: Conn {
                server: normalize_server(&conn.server),
                token: conn.token.clone(),
            },
            conn,
            ui_server: TextEdit::new(""),
            ui_token: TextEdit::new(""),
            link: Link::Connecting,
            version: String::new(),
            projects: Vec::new(),
            loaded: false,
            projects_error: None,
            view: View::Projects,
            runs: Vec::new(),
            runs_for: None,
            runs_loading: false,
            runs_error: None,
            run: None,
            run_error: None,
            open_step: None,
            logs: HashMap::new(),
            editor: Editor::default(),
            system: None,
            system_error: None,
            deploy_checks: None,
            deploy_checks_error: None,
            deploy_checks_loading: false,
            toasts: Vec::new(),
            next_toast: 1,
            triggering: None,
            cancelling: None,
            force_deploying: None,
            clearing_cache: false,
            force: false,
            bus: Bus::default(),
            update_status: UpdateStatus::Idle,
            update_dismissed: false,
            auto_update_check: true,
            updater: Updater::default(),
            #[cfg(not(target_arch = "wasm32"))]
            auto_update_triggered: false,
            next_projects: 0.0,
            next_runs: 0.0,
            next_run: 0.0,
            next_log: 0.0,
            next_system: 0.0,
            asked_system: false,
            clock: 0.0,
            ready: false,
            demo: false,
            drawer_open: false,
            theme_mode: crate::theme::ThemeMode::System,
            user_zoom: None,
            log_wrap: true,
            log_view_raw: false,
            expanded_substeps: BTreeSet::new(),
            substep_toggled: BTreeSet::new(),
            shell_log_collapsed: false,
            zoom_track: std::cell::Cell::new((0, 1.0)),
            stick_log_pending: false,
        };
        state.ui_server = TextEdit::new(state.conn.server.clone());
        state.ui_token = TextEdit::new(state.conn.token.clone());
        if let Some(id) = initial_run_id() {
            state.view = View::Run { id };
            state.stick_log_pending = true;
        }
        state
    }

    /// Loads persisted window appearance, zoom, log-wrap, and update preferences (`localStorage` on web, `Prefs` natively).
    pub fn load_prefs(&mut self) {
        let (theme, zoom, log_wrap) = load_window_prefs();
        if let Some(mode) = theme {
            self.theme_mode = mode;
        }
        if let Some(z) = zoom {
            self.user_zoom = Some(z);
        }
        if let Some(wrap) = log_wrap {
            self.log_wrap = wrap;
        }
        if let Some(auto_update) = load_auto_update_pref() {
            self.auto_update_check = auto_update;
        }
    }

    pub fn set_auto_update_check(&mut self, enabled: bool, tx: &mut Tx) {
        self.auto_update_check = enabled;
        save_auto_update_pref(enabled);
        let _ = tx;
    }

    pub fn toggle_auto_update_check(&mut self, tx: &mut Tx) {
        self.set_auto_update_check(!self.auto_update_check, tx);
    }

    pub fn check_for_updates(&mut self, tx: &mut Tx) {
        if self.update_status.is_busy() {
            return;
        }
        self.update_status = UpdateStatus::Checking;
        if !self.demo {
            self.updater.check_now();
        }
        self.refresh(tx);
    }

    pub fn install_update(&mut self, tx: &mut Tx) {
        if self.update_status.is_busy() {
            return;
        }
        let Some(info) = self.update_status.info().cloned() else {
            return;
        };
        if info.asset_url.is_none() {
            if !info.html_url.is_empty() {
                let _ = tx.open_url(&info.html_url);
            }
            return;
        }
        self.update_status = UpdateStatus::Installing {
            info: info.clone(),
            stage: "Starting download…".into(),
        };
        if !self.demo {
            self.updater.install(info);
        }
        self.refresh(tx);
    }

    pub fn restart_for_update(&mut self, tx: &mut Tx) {
        match crate::updater::relaunch_current_exe() {
            Ok(()) => tx.close_window(),
            Err(err) => {
                self.toast(tx, ToastKind::Error, "Could not restart", &err);
            }
        }
    }

    pub fn dismiss_update_banner(&mut self) {
        self.update_dismissed = true;
    }

    /// Updates window theme mode and persists it immediately, smoothly crossfading colors.
    pub fn set_theme_mode(&mut self, mode: crate::theme::ThemeMode, tx: &mut Tx) {
        self.theme_mode = mode;
        save_theme_pref(mode);
        tx.color_motion(rune::tok::motion::SETTLE);
    }

    /// Updates log word wrapping and persists it immediately.
    pub fn set_log_wrap(&mut self, wrap: bool, tx: &mut Tx) {
        self.log_wrap = wrap;
        save_log_wrap_pref(wrap);
        let _ = tx;
    }

    /// Toggles log word wrapping and persists it immediately.
    pub fn toggle_log_wrap(&mut self, tx: &mut Tx) {
        self.set_log_wrap(!self.log_wrap, tx);
    }

    /// Updates explicit UI zoom (`None` resets to the responsive default) and persists it.
    pub fn set_user_zoom(&mut self, zoom: Option<f32>, tx: &mut Tx) {
        let cleaned = zoom.and_then(sanitize_zoom);
        self.user_zoom = cleaned;
        save_zoom_pref(cleaned);
        tx.instant();
    }

    /// Effective zoom currently shown in the UI.
    pub fn current_zoom(&self) -> f32 {
        self.user_zoom.unwrap_or_else(|| self.zoom_track.get().1)
    }

    /// Steps explicit UI zoom up (`delta > 0`) or down (`delta < 0`) along [`ZOOM_STEPS`].
    pub fn step_zoom(&mut self, delta: i32, tx: &mut Tx) {
        let cur = self.current_zoom();
        let next = if delta > 0 {
            ZOOM_STEPS
                .iter()
                .copied()
                .find(|&z| z > cur + 0.01)
                .unwrap_or(*ZOOM_STEPS.last().unwrap_or(&1.5))
        } else {
            ZOOM_STEPS
                .iter()
                .rev()
                .copied()
                .find(|&z| z < cur - 0.01)
                .unwrap_or(*ZOOM_STEPS.first().unwrap_or(&0.9))
        };
        self.set_user_zoom(Some(next), tx);
    }

    // -- connection ------------------------------------------------------------------------

    /// Adopts the settings fields' URL and token and refreshes everything.
    pub fn reconnect(&mut self, tx: &mut Tx) {
        // The Connection fields edit `ui_*`; take what the user typed.
        let prev_server = self.live.server.clone();
        self.conn = conn_from_fields(&self.ui_server.text, &self.ui_token.text);
        self.live = self.conn.clone();
        self.live.server = normalize_server(&self.live.server);
        store_token(&self.live.token);
        self.link = Link::Connecting;
        self.demo = false;
        self.loaded = false;
        self.projects_error = None;
        self.run_error = None;
        self.runs_error = None;
        self.system_error = None;
        self.deploy_checks_error = None;
        self.cancelling = None;
        self.force_deploying = None;
        self.clearing_cache = false;
        if self.live.server != prev_server {
            self.projects.clear();
            self.runs.clear();
            self.runs_for = None;
            self.run = None;
            self.open_step = None;
            self.logs.clear();
            self.expanded_substeps.clear();
            self.substep_toggled.clear();
            self.shell_log_collapsed = false;
            self.system = None;
            self.deploy_checks = None;
        }
        self.editor = Editor {
            page: self.editor.page,
            text: HashMap::new(),
            ..Editor::default()
        };
        self.bus.cancel();
        self.next_projects = 0.0;
        self.next_runs = 0.0;
        self.next_run = 0.0;
        self.next_log = 0.0;
        self.next_system = 0.0;
        self.asked_system = false;
        self.request_projects();
        self.request_config();
        self.request_system();
        if matches!(
            self.view,
            View::Settings {
                page: SettingsPage::Deploy
            }
        ) {
            self.request_deploy_checks();
        }
        self.refresh(tx);
    }

    /// The runtime's waker, from `AppSpec::setup`: native answers wake `poll` with it.
    pub fn attach(&mut self, waker: rune::Waker) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.bus.set_waker(waker.clone());
            self.updater.set_waker(waker);
        }
        #[cfg(target_arch = "wasm32")]
        let _ = waker;
    }

    /// Web: the handle `spawn_local` answers use to land back in `poll`.
    #[cfg(target_arch = "wasm32")]
    pub fn adopt(&mut self, handle: rune::AppHandle<State>) {
        self.bus.attach(handle);
    }

    /// The server URL as the rail shows it: no scheme, so it fits.
    pub fn server_display(&self) -> String {
        let server = self.server();
        server
            .trim_start_matches("http://")
            .trim_start_matches("https://")
            .to_string()
    }

    pub fn server(&self) -> String {
        if self.live.server.is_empty() {
            normalize_server(&self.conn.server)
        } else {
            self.live.server.clone()
        }
    }

    fn token(&self) -> String {
        self.live.token.clone()
    }

    /// Whether an answer for `call` is still what the view wants.
    pub fn wants(&self, call: &Call) -> bool {
        match call {
            Call::Projects | Call::Config | Call::System | Call::DeployChecks | Call::Health => {
                true
            }
            Call::Runs(name, _) => {
                matches!(&self.view, View::Project { name: open } if open == name)
            }
            Call::Run(id) => match &self.view {
                View::Run { id: open } => open == id,
                View::Project { .. } => self.run.as_ref().is_some_and(|r| &r.id == id),
                _ => false,
            },
            Call::Log { run, index, .. } => match (&self.view, self.open_step) {
                (View::Run { id }, Some(open)) => id == run && open == *index,
                _ => false,
            },
            // The workflows answer carries no project, so it is kept unless the editor closed.
            Call::Workflows { .. } => self.editor.loaded,
            Call::Project(_) => true,
            Call::Trigger { .. }
            | Call::CancelRun { .. }
            | Call::CancelProject { .. }
            | Call::ForceDeployProject { .. }
            | Call::ForceDeployRun { .. }
            | Call::ClearCache { .. }
            | Call::SaveConfig(_) => true,
        }
    }

    fn send(&mut self, slot: Slot, call: Call) {
        let (base, token) = (self.server(), self.token());
        if base.is_empty() {
            return;
        }
        self.bus.send(slot, &base, &token, call);
    }

    // -- requests --------------------------------------------------------------------------

    pub fn request_projects(&mut self) {
        self.send(Slot::Projects, Call::Projects);
    }

    pub fn request_runs(&mut self, name: &str) {
        self.runs_loading = true;
        self.runs_error = None;
        self.send(Slot::Runs, Call::Runs(name.to_string(), 50));
    }

    pub fn request_run(&mut self, id: &str) {
        self.send(Slot::Run, Call::Run(id.to_string()));
    }

    pub fn request_config(&mut self) {
        self.send(Slot::Config, Call::Config);
    }

    pub fn request_system(&mut self) {
        self.send(Slot::System, Call::System);
    }

    pub fn request_deploy_checks(&mut self) {
        self.deploy_checks_loading = true;
        self.deploy_checks_error = None;
        if !self.demo {
            self.send(Slot::DeployChecks, Call::DeployChecks);
        } else {
            self.deploy_checks_loading = false;
            self.deploy_checks = Some(crate::demo::deploy_checks());
        }
    }

    pub fn request_log(&mut self, run: &str, index: usize, mode: LogMode) {
        let entry = self.logs.entry((run.to_string(), index)).or_default();
        entry.loading = true;
        entry.error = None;
        entry.mode = Some(mode);
        self.send(
            Slot::Log,
            Call::Log {
                run: run.to_string(),
                index,
                tail: mode.tail(),
            },
        );
    }

    /// Loads a project's GitHub workflows for an act step's picker.
    pub fn request_workflows(
        &mut self,
        step_key: &str,
        project: String,
        repo_path: Option<String>,
    ) {
        self.editor.workflows_loading = true;
        self.editor.workflows_for = Some(step_key.to_string());
        let call = Call::Workflows {
            project: (!project.trim().is_empty()).then_some(project),
            repo_path: repo_path.filter(|p| !p.trim().is_empty()),
        };
        self.send(Slot::Workflows, call);
    }

    pub fn trigger(&mut self, project: &str) {
        self.triggering = Some(project.to_string());
        self.send(
            Slot::Trigger,
            Call::Trigger {
                project: project.to_string(),
                force: self.force,
            },
        );
    }

    pub fn cancel_run(&mut self, id: &str, tx: &mut Tx) {
        if self.cancelling.as_deref() == Some(id) {
            return;
        }
        self.cancelling = Some(id.to_string());
        if !self.demo {
            self.send(Slot::Cancel, Call::CancelRun { id: id.to_string() });
        }
        self.refresh(tx);
    }

    pub fn cancel_project(&mut self, name: &str, tx: &mut Tx) {
        if self.cancelling.as_deref() == Some(name) {
            return;
        }
        self.cancelling = Some(name.to_string());
        if !self.demo {
            self.send(
                Slot::Cancel,
                Call::CancelProject {
                    name: name.to_string(),
                },
            );
        }
        self.refresh(tx);
    }

    pub fn force_deploy_project(&mut self, name: &str, tx: &mut Tx) {
        if self.force_deploying.as_deref() == Some(name) {
            return;
        }
        self.force_deploying = Some(name.to_string());
        if !self.demo {
            self.send(
                Slot::ForceDeploy,
                Call::ForceDeployProject {
                    name: name.to_string(),
                },
            );
        }
        self.refresh(tx);
    }

    pub fn force_deploy_run(&mut self, id: &str, tx: &mut Tx) {
        if self.force_deploying.as_deref() == Some(id) {
            return;
        }
        self.force_deploying = Some(id.to_string());
        if !self.demo {
            self.send(
                Slot::ForceDeploy,
                Call::ForceDeployRun { id: id.to_string() },
            );
        }
        self.refresh(tx);
    }

    pub fn clear_cache(&mut self, project: Option<&str>, tx: &mut Tx) {
        if self.clearing_cache {
            return;
        }
        self.clearing_cache = true;
        if !self.demo {
            self.send(
                Slot::ClearCache,
                Call::ClearCache {
                    project: project.map(str::to_string),
                },
            );
        }
        self.refresh(tx);
    }

    /// Whether a substep card in the Steps accordion is expanded.
    /// Before the user interacts with a step, running and failed/timed-out substeps auto-expand
    /// while passed/pending/skipped substeps stay compactly collapsed.
    pub fn is_substep_expanded(
        &self,
        run_id: &str,
        step_index: usize,
        sub_idx: usize,
        status: StepStatus,
    ) -> bool {
        let step_key = (run_id.to_string(), step_index);
        if self.substep_toggled.contains(&step_key) {
            self.expanded_substeps
                .contains(&(run_id.to_string(), step_index, sub_idx))
        } else {
            matches!(
                status,
                StepStatus::Running | StepStatus::Failure | StepStatus::Timeout
            )
        }
    }

    /// Toggles one substep card in the Steps accordion while preserving any initially auto-expanded cards.
    pub fn toggle_substep(
        &mut self,
        run_id: &str,
        step_index: usize,
        sub_idx: usize,
        substeps: &[Substep],
        preamble_auto_open: bool,
    ) {
        let step_key = (run_id.to_string(), step_index);
        if self.substep_toggled.insert(step_key) {
            for (idx, sub) in substeps.iter().enumerate() {
                if matches!(
                    sub.status,
                    StepStatus::Running | StepStatus::Failure | StepStatus::Timeout
                ) {
                    self.expanded_substeps
                        .insert((run_id.to_string(), step_index, idx));
                }
            }
            if preamble_auto_open {
                self.expanded_substeps
                    .insert((run_id.to_string(), step_index, substeps.len()));
            }
        }
        let item_key = (run_id.to_string(), step_index, sub_idx);
        if !self.expanded_substeps.remove(&item_key) {
            self.expanded_substeps.insert(item_key);
        }
    }

    /// Expands or collapses every substep card (plus optional preamble bucket) for a step.
    pub fn set_all_substeps_expanded(
        &mut self,
        run_id: &str,
        step_index: usize,
        total_cards: usize,
        expand: bool,
    ) {
        self.substep_toggled
            .insert((run_id.to_string(), step_index));
        for sub_idx in 0..total_cards {
            let item_key = (run_id.to_string(), step_index, sub_idx);
            if expand {
                self.expanded_substeps.insert(item_key);
            } else {
                self.expanded_substeps.remove(&item_key);
            }
        }
    }

    pub fn save(&mut self) {
        if self.editor.saving || !self.editor.loaded {
            return;
        }
        self.editor.saving = true;
        self.editor.error = None;
        self.editor.field = None;
        self.send(
            Slot::Config,
            Call::SaveConfig(self.editor.draft.to_string()),
        );
    }

    // -- answers ---------------------------------------------------------------------------

    /// Applies one landing answer; returns whether the view must re-run.
    fn answer(&mut self, call: Call, result: Outcome, tx: &mut Tx) -> bool {
        match result {
            Ok(answer) => {
                self.note_reachable(tx);
                self.apply(call, answer, tx)
            }
            Err(failure) => self.fail(call, failure, tx),
        }
    }

    fn note_reachable(&mut self, tx: &mut Tx) {
        let was_broken = self.link.is_broken();
        if !matches!(self.link, Link::Ok) {
            self.link = Link::Ok;
        }
        if was_broken {
            tx.animated();
            self.toast(
                tx,
                ToastKind::Success,
                "Reconnected",
                &format!("{} answered again.", self.server()),
            );
            // A reconnected daemon may have moved on: re-read from scratch.
            self.next_projects = 0.0;
            self.next_run = 0.0;
            self.next_runs = 0.0;
        }
    }

    fn fail(&mut self, call: Call, failure: Failure, tx: &mut Tx) -> bool {
        let was_broken = self.link.is_broken();
        let mut toast: Option<(ToastKind, String, String)> = None;
        match &failure {
            Failure::Down(why) => {
                let failures = match &self.link {
                    Link::Down {
                        why: prev,
                        failures,
                    } if prev == why => failures + 1,
                    _ => 1,
                };
                let first = !matches!(self.link, Link::Down { .. });
                self.link = Link::Down {
                    why: why.clone(),
                    failures,
                };
                if first {
                    toast = Some((ToastKind::Error, "Daemon unreachable".into(), why.clone()));
                }
            }
            Failure::Unauthorized => {
                let first = self.link != Link::Unauthorized;
                self.link = Link::Unauthorized;
                if first {
                    toast = Some((ToastKind::Error, "Unauthorized".into(), failure.message()));
                }
            }
            Failure::Api { .. } => {}
        }
        if self.link.is_broken() != was_broken {
            tx.animated();
        }
        match call {
            Call::Projects => {
                self.projects_error = Some(failure.message());
            }
            Call::Runs(..) => {
                self.runs_loading = false;
                self.runs_error = Some(failure.message());
            }
            Call::Run(_) => {
                self.run_error = Some(failure.message());
            }
            Call::Log { run, index, .. } => {
                if let Some(entry) = self.logs.get_mut(&(run, index)) {
                    entry.loading = false;
                    entry.error = Some(failure.message());
                }
            }
            Call::Config | Call::SaveConfig(_) => {
                self.editor.saving = false;
                match failure.field_path() {
                    Some(field) => {
                        // The daemon names the offending field: show it inline, and jump to the
                        // page and project that hold it.
                        self.editor.field = Some(field.to_string());
                        self.editor.error = Some(failure.message());
                        self.reveal_field(field);
                        toast = Some((
                            ToastKind::Error,
                            "Config rejected".into(),
                            format!("{field}: {}", failure.message()),
                        ));
                    }
                    None => {
                        toast = Some((
                            ToastKind::Error,
                            "Config rejected".into(),
                            failure.message(),
                        ))
                    }
                }
            }
            Call::System => {
                self.system_error = Some(failure.message());
            }
            Call::DeployChecks => {
                self.deploy_checks_loading = false;
                self.deploy_checks_error = Some(failure.message());
            }
            Call::Workflows { .. } => {
                self.editor.workflows_loading = false;
                toast = Some((
                    ToastKind::Error,
                    "Workflows unavailable".into(),
                    failure.message(),
                ));
            }
            Call::Trigger { project, .. } => {
                self.triggering = None;
                if matches!(failure, Failure::Api { status: 409, .. }) {
                    toast = Some((
                        ToastKind::Info,
                        "Already running".into(),
                        format!(
                            "{project} is still running. Wait for it to finish, or force a run."
                        ),
                    ));
                } else {
                    toast = Some((
                        ToastKind::Error,
                        format!("Could not start {project}"),
                        failure.message(),
                    ));
                }
            }
            Call::CancelRun { .. } | Call::CancelProject { .. } => {
                self.cancelling = None;
                toast = Some((
                    ToastKind::Error,
                    "Could not cancel run".into(),
                    failure.message(),
                ));
            }
            Call::ForceDeployProject { name } | Call::ForceDeployRun { id: name } => {
                self.force_deploying = None;
                if matches!(failure, Failure::Api { status: 409, .. }) {
                    toast = Some((
                        ToastKind::Info,
                        "Already running".into(),
                        format!("{name} already has a run in progress."),
                    ));
                } else {
                    toast = Some((
                        ToastKind::Error,
                        "Force deploy failed".into(),
                        failure.message(),
                    ));
                }
            }
            Call::ClearCache { .. } => {
                self.clearing_cache = false;
                toast = Some((
                    ToastKind::Error,
                    "Could not clear cache".into(),
                    failure.message(),
                ));
            }
            Call::Project(_) | Call::Health => {}
        }
        if let Some((kind, title, body)) = toast {
            self.toast(tx, kind, &title, &body);
        }
        // The banner says the daemon is unreachable; the panel keeps showing what it had.
        true
    }

    fn apply(&mut self, call: Call, answer: Answer, tx: &mut Tx) -> bool {
        match answer {
            Answer::Projects(projects) => {
                self.loaded = true;
                self.projects_error = None;
                let before: Vec<(String, ProjectState)> = self
                    .projects
                    .iter()
                    .map(|p| (p.name.clone(), p.state()))
                    .collect();
                let after: Vec<(String, ProjectState)> = projects
                    .iter()
                    .map(|p| (p.name.clone(), p.state()))
                    .collect();
                if self.ready && before != after {
                    for (name, state) in &after {
                        let prev = before.iter().find(|(n, _)| n == name).map(|(_, s)| *s);
                        if *state == ProjectState::Running && prev != Some(ProjectState::Running) {
                            self.toast(
                                tx,
                                ToastKind::Info,
                                "Run started",
                                &format!("{name} picked up work."),
                            );
                        }
                    }
                }
                self.projects = projects;
                if let Some(cid) = self.cancelling.as_deref() {
                    let still_running = self.projects.iter().any(|p| {
                        p.running
                            && (p.name == cid
                                || p.current_run
                                    .as_ref()
                                    .is_some_and(|r| r.id == cid && r.status.is_running()))
                    });
                    let finished_seen = self.projects.iter().any(|p| {
                        (!p.running && p.name == cid)
                            || p.last_run.as_ref().is_some_and(|r| r.id == cid)
                    });
                    if !still_running && finished_seen {
                        self.cancelling = None;
                    }
                }
                // A project that vanished from the config takes the detail view with it.
                if let View::Project { name } = &self.view {
                    if !self.projects.iter().any(|p| &p.name == name) {
                        self.view = View::Projects;
                    }
                }
                true
            }
            Answer::Project(p) => {
                if let Some(slot) = self.projects.iter_mut().find(|x| x.name == p.name) {
                    *slot = *p;
                }
                true
            }
            Answer::Runs(runs) => {
                self.runs_loading = false;
                self.runs_error = None;
                if let Call::Runs(name, _) = call {
                    self.runs_for = Some(name);
                }
                self.runs = runs;
                true
            }
            Answer::Run(run) => {
                let prev_open = self.open_step;
                let open = self.open_step.filter(|i| *i < run.steps.len());
                let step_finished = open.is_some_and(|idx| {
                    self.run
                        .as_ref()
                        .and_then(|r| r.steps.get(idx))
                        .is_some_and(|s| s.status.is_live())
                        && !run.steps[idx].status.is_live()
                });
                let changed = self.run.as_ref() != Some(run.as_ref());
                self.open_step = open;
                if !run.status.is_running() && self.cancelling.as_deref() == Some(run.id.as_str()) {
                    self.cancelling = None;
                }
                self.run = Some(*run);
                self.run_error = None;
                let in_steps_accordion = !self.log_view_raw
                    && open.is_some_and(|idx| {
                        self.run
                            .as_ref()
                            .and_then(|r| r.steps.get(idx))
                            .is_some_and(|st| !st.substeps.is_empty())
                    });
                if prev_open != open && open.is_some() && !in_steps_accordion {
                    self.stick_log_pending = true;
                    tx.scroll_to_bottom_instant("run-log-scroll");
                }
                if step_finished {
                    if let (Some(r), Some(idx)) = (self.run.clone(), open) {
                        let mode = self
                            .logs
                            .get(&(r.id.clone(), idx))
                            .and_then(|e| e.mode)
                            .unwrap_or(LogMode::Tail);
                        self.request_log(&r.id, idx, mode);
                    }
                } else {
                    self.request_next_log();
                }
                changed || step_finished
            }
            Answer::Log {
                run,
                index,
                text,
                tail,
            } => {
                let active =
                    self.run.as_ref().is_some_and(|r| r.id == run) && self.open_step == Some(index);
                let in_steps_accordion = !self.log_view_raw
                    && self
                        .run
                        .as_ref()
                        .and_then(|r| r.steps.get(index))
                        .is_some_and(|st| !st.substeps.is_empty());
                match self.logs.get_mut(&(run, index)) {
                    Some(entry)
                        if entry.mode
                            == Some(if tail.is_some() {
                                LogMode::Tail
                            } else {
                                LogMode::Full
                            }) =>
                    {
                        let was_empty = entry.text.is_empty();
                        let grew = entry.text != text;
                        entry.text = text;
                        entry.loading = false;
                        entry.error = None;
                        if active && !in_steps_accordion && (was_empty || self.stick_log_pending) {
                            self.stick_log_pending = false;
                            tx.scroll_to_bottom_instant("run-log-scroll");
                        }
                        grew
                    }
                    // A tail answer for a log the reader asked in full: drop it.
                    _ => false,
                }
            }
            Answer::Triggered => {
                self.triggering = None;
                let project = match &call {
                    Call::Trigger { project, .. } => project.clone(),
                    _ => self.current_project_name().unwrap_or_default(),
                };
                self.toast(
                    tx,
                    ToastKind::Success,
                    "Queued",
                    &format!("{project}: a run was queued."),
                );
                self.next_projects = 0.0;
                self.next_runs = 0.0;
                self.next_run = 0.0;
                true
            }
            Answer::Cancelled { id, ok } => {
                self.cancelling = None;
                match ok {
                    Ok(res) => {
                        let target = if res.project.is_empty() {
                            id
                        } else {
                            res.project
                        };
                        self.toast(
                            tx,
                            ToastKind::Info,
                            "Cancelling run",
                            &format!("{target}: cancellation requested."),
                        );
                        self.next_projects = 0.0;
                        self.next_runs = 0.0;
                        self.next_run = 0.0;
                        self.refresh(tx);
                    }
                    Err(err) => {
                        self.toast(tx, ToastKind::Error, "Could not cancel run", &err.error);
                    }
                }
                true
            }
            Answer::ForceDeployed { target, result } => {
                self.force_deploying = None;
                let body = if result.sha.is_empty() {
                    format!("{target}: deploy-only run queued.")
                } else {
                    format!(
                        "{target}: deploy-only run queued for {}.",
                        crate::widgets::short_sha(&result.sha)
                    )
                };
                self.toast(tx, ToastKind::Success, "Deploy queued", &body);
                self.next_projects = 0.0;
                self.next_runs = 0.0;
                self.next_run = 0.0;
                self.refresh(tx);
                true
            }
            Answer::CacheCleared { project, result } => {
                self.clearing_cache = false;
                let scope = project.as_deref().unwrap_or("All projects");
                let count = result.removed.len();
                let body = if count == 0 {
                    format!("{scope}: build cache was already empty.")
                } else {
                    format!(
                        "{scope}: cleared {count} cached {}.",
                        if count == 1 { "entry" } else { "entries" }
                    )
                };
                self.toast(tx, ToastKind::Success, "Cache cleared", &body);
                true
            }
            Answer::Config(cfg) => {
                self.editor.base = serde_json::to_value(&*cfg).unwrap_or(Value::Null);
                // A reload never clobbers an edit in progress.
                if !self.editor.dirty() {
                    self.editor.draft = self.editor.base.clone();
                    self.editor.reseed();
                }
                self.editor.loaded = true;
                self.editor.saving = false;
                self.after_draft_change();
                true
            }
            Answer::Saved(outcome) => {
                self.editor.saving = false;
                self.editor.error = None;
                self.editor.field = None;
                self.editor.base = serde_json::to_value(&outcome.config).unwrap_or(Value::Null);
                self.editor.draft = self.editor.base.clone();
                self.editor.reseed();
                self.editor.new_project = false;
                self.editor.loaded = true;
                self.after_draft_change();
                let body = if outcome.restart_required.is_empty() {
                    "Written to config.toml; idle workers reloaded. Running pipelines finish on the old config.".to_string()
                } else {
                    format!(
                        "Written to config.toml; restart needed for {}.",
                        outcome.restart_required.join(", ")
                    )
                };
                self.toast(tx, ToastKind::Success, "Config saved", &body);
                if self.deploy_checks.is_some()
                    || matches!(
                        self.view,
                        View::Settings {
                            page: SettingsPage::Deploy
                        }
                    )
                {
                    self.request_deploy_checks();
                }
                true
            }
            Answer::System(info) => {
                if !info.version.is_empty() {
                    self.version = info.version.clone();
                }
                self.system = Some(*info);
                self.system_error = None;
                true
            }
            Answer::DeployChecks(report) => {
                self.deploy_checks_loading = false;
                self.deploy_checks = Some(*report);
                self.deploy_checks_error = None;
                true
            }
            Answer::Workflows {
                workflows,
                repo_path,
            } => {
                let _ = repo_path;
                self.editor.workflows_loading = false;
                self.editor.workflows = workflows;
                true
            }
            Answer::Health(h) => {
                self.version = h.version;
                true
            }
        }
    }

    /// Keeps the open project and step valid after a reload, save, reorder or delete, and loads
    /// the workflow list an act step's picker needs.
    fn after_draft_change(&mut self) {
        let projects = self.editor.count("project");
        if self.editor.project.is_some_and(|i| i >= projects) {
            self.editor.project = None;
            self.editor.step = None;
        }
        if let Some((p, deploy, s)) = self.editor.step {
            let path = step_path(p, deploy, s);
            if p >= projects
                || s >= self
                    .editor
                    .count(&format!("project[{p}].{}", phase_key(deploy)))
            {
                self.editor.step = None;
            } else if self.editor.s(&format!("{path}.kind")) == "act"
                && self.editor.workflows_for.as_deref() != Some(path.as_str())
            {
                self.load_workflows_for(p, &path);
            }
        }
    }

    fn load_workflows_for(&mut self, project: usize, path: &str) {
        let name = self.editor.s(&format!("project[{project}].name"));
        let repo_path = self.editor.s(&format!("project[{project}].repo_path"));
        let repo_path = (!repo_path.trim().is_empty()).then_some(repo_path);
        self.request_workflows(path, name, repo_path);
    }

    /// Each answer's `Call` is chosen by the requesting code, except a read the daemon answers
    /// with a different shape; `wants` then drops it. Nothing else can go stale here.
    fn reveal_field(&mut self, field: &str) {
        if let Some(page) = page_of(field) {
            self.editor.page = page;
            if matches!(self.view, View::Settings { .. }) {
                self.view = View::Settings { page };
            }
        }
        if field.starts_with("project") {
            let keys = paths::parse(field);
            if let Some(Key::Index(i)) = keys.get(1) {
                self.editor.project = Some(*i);
            }
        }
    }

    fn open_step_needs_log_poll(&self) -> bool {
        let Some(run) = self.run.as_ref() else {
            return false;
        };
        let Some(index) = self.open_step else {
            return false;
        };
        let Some(step) = run.steps.get(index) else {
            return false;
        };
        step.status.is_live() || !self.logs.contains_key(&(run.id.clone(), index))
    }

    fn request_next_log(&mut self) {
        let Some(run) = self.run.clone() else { return };
        let Some(index) = self.open_step else { return };
        let Some(step) = run.steps.get(index) else {
            return;
        };
        let entry = self.logs.get(&(run.id.clone(), index));
        let live = step.status.is_live();
        let stale = live && self.clock >= self.next_log;
        if entry.is_some() && !stale {
            return;
        }
        let mode = entry.and_then(|e| e.mode).unwrap_or(LogMode::Tail);
        self.request_log(&run.id, index, mode);
        self.next_log = self.clock + LOG_EVERY;
    }

    // -- poll ------------------------------------------------------------------------------

    /// The whole polling machine; returns whether the view must re-run.
    pub fn poll(&mut self, clock: f64, tx: &mut Tx) -> bool {
        self.clock = clock;
        let mut changed = false;

        if self.demo {
            if !self.toasts.is_empty() {
                let before = self.toasts.len();
                self.toasts.retain(|t| t.until > self.clock);
                if before != self.toasts.len() {
                    tx.animated();
                    changed = true;
                }
                if let Some(earliest) = self.toasts.iter().map(|t| t.until).reduce(f64::min) {
                    tx.wake_at(if earliest > self.clock {
                        earliest
                    } else {
                        self.clock + 0.02
                    });
                }
            }
            return changed;
        }

        let was_broken = self.link.is_broken();
        for (call, result) in self.bus.drain() {
            if self.wants(&call) {
                changed |= self.answer(call, result, tx);
            }
        }
        if self.link.is_broken() != was_broken {
            tx.animated();
        }

        // Drain desktop auto-updater events.
        for status in self.updater.drain() {
            match &status {
                UpdateStatus::Available(info) => {
                    let same = self
                        .update_status
                        .info()
                        .is_some_and(|prev| prev.sha == info.sha);
                    if !same {
                        self.update_dismissed = false;
                    }
                    tx.animated();
                }
                UpdateStatus::ReadyToRestart(info) => {
                    self.update_dismissed = false;
                    self.toast(
                        tx,
                        ToastKind::Success,
                        "Update ready",
                        &format!(
                            "Version {} installed. Restart lunacy-ui to apply.",
                            info.short_sha
                        ),
                    );
                }
                UpdateStatus::Error(msg) => {
                    self.toast(tx, ToastKind::Error, "Update check failed", msg);
                }
                _ => {}
            }
            self.update_status = status;
            changed = true;
        }

        #[cfg(not(target_arch = "wasm32"))]
        if !self.auto_update_triggered && self.auto_update_check && self.clock >= 1.5 {
            self.auto_update_triggered = true;
            if crate::updater::should_auto_check() {
                self.check_for_updates(tx);
                changed = true;
            }
        }

        // `poll` runs only after the first frame, which is exactly when a live window may start
        // talking to the daemon. Demo scenes set `ready` themselves.
        self.ready = true;

        // The daemon version is stable: ask once, so the rail can show it from the first frame.
        if !self.asked_system {
            self.asked_system = true;
            self.request_system();
        }

        // Config and diagnostics are read on demand: they only change when the user saves or a
        // page opens. Projects, the open run, its log and a project's history are polled.
        if self.clock >= self.next_projects {
            self.next_projects = self.clock + PROJECTS_EVERY;
            self.request_projects();
        }

        let running = match (&self.view, &self.run) {
            (View::Run { id }, Some(run)) if &run.id == id => run.status.is_running(),
            (View::Run { .. }, _) => true,
            _ => false,
        };
        if matches!(self.view, View::Run { .. }) && self.clock >= self.next_run {
            self.next_run = self.clock + if running { RUN_EVERY } else { RUN_SETTLE_EVERY };
            if let View::Run { id } = self.view.clone() {
                self.request_run(&id);
            }
        }
        if matches!(self.view, View::Run { .. }) {
            self.request_next_log();
        }
        if let View::Project { name } = self.view.clone() {
            if self.clock >= self.next_runs {
                self.next_runs = self.clock + RUNS_EVERY;
                self.request_runs(&name);
            }
        }

        if !self.toasts.is_empty() {
            let before = self.toasts.len();
            self.toasts.retain(|t| t.until > self.clock);
            if before != self.toasts.len() {
                tx.animated();
                changed = true;
            }
        }

        // One wake for the nearest deadline, so an idle window draws nothing.
        let mut due = self.next_projects;
        match &self.view {
            View::Run { .. } => {
                due = due.min(self.next_run);
                if self.open_step_needs_log_poll() {
                    due = due.min(self.next_log);
                }
            }
            View::Project { .. } => due = due.min(self.next_runs),
            _ => {}
        }
        if let Some(toast_due) = self.toasts.iter().map(|t| t.until).reduce(f64::min) {
            due = due.min(toast_due);
        }
        if due.is_finite() {
            tx.wake_at(if due > self.clock {
                due
            } else {
                self.clock + 0.02
            });
        }
        changed
    }

    /// Wakes `poll` right away without suppressing UI transitions.
    pub fn refresh(&mut self, tx: &mut Tx) {
        tx.wake_after(std::time::Duration::ZERO);
    }

    // -- navigation ------------------------------------------------------------------------

    pub fn current_project(&self) -> Option<&ProjectView> {
        match &self.view {
            View::Project { name } => self.projects.iter().find(|p| &p.name == name),
            View::Run { id } => self
                .run
                .as_ref()
                .filter(|r| &r.id == id)
                .and_then(|r| self.projects.iter().find(|p| p.name == r.project)),
            _ => None,
        }
    }

    pub fn current_project_name(&self) -> Option<String> {
        self.current_project().map(|p| p.name.clone())
    }

    pub fn open_projects(&mut self, tx: &mut Tx) {
        self.view = View::Projects;
        self.drawer_open = false;
        self.set_hash("");
        self.next_projects = 0.0;
        self.refresh(tx);
    }

    pub fn open_project(&mut self, name: &str, tx: &mut Tx) {
        self.view = View::Project {
            name: name.to_string(),
        };
        self.drawer_open = false;
        self.runs.clear();
        self.runs_for = None;
        self.runs_error = None;
        self.next_runs = 0.0;
        self.set_hash("");
        self.refresh(tx);
    }

    pub fn open_run(&mut self, id: &str, tx: &mut Tx) {
        self.view = View::Run { id: id.to_string() };
        self.drawer_open = false;
        self.run = None;
        self.open_step = None;
        self.stick_log_pending = true;
        self.run_error = None;
        self.logs.clear();
        self.expanded_substeps.clear();
        self.substep_toggled.clear();
        self.shell_log_collapsed = false;
        self.next_run = 0.0;
        self.next_log = 0.0;
        self.set_hash(&format!("#/runs/{id}"));
        self.refresh(tx);
    }

    /// Back out one level: run → its project, project or settings → the list.
    pub fn back(&mut self, tx: &mut Tx) {
        match self.view.clone() {
            View::Run { .. } => {
                let project = self
                    .run
                    .as_ref()
                    .map(|r| r.project.clone())
                    .or_else(|| self.current_project_name());
                match project {
                    Some(p) => self.open_project(&p, tx),
                    None => self.open_projects(tx),
                }
            }
            View::Project { .. } | View::Settings { .. } => self.open_projects(tx),
            View::Projects => {}
        }
    }

    pub fn open_settings(&mut self, page: SettingsPage, tx: &mut Tx) {
        self.view = View::Settings { page };
        self.drawer_open = false;
        self.editor.page = page;
        self.set_hash("");
        if !page.is_local() && !self.editor.loaded {
            self.request_config();
        }
        if page == SettingsPage::Diagnostics {
            self.request_system();
            self.next_system = self.clock + 30.0;
        }
        if page == SettingsPage::Deploy {
            self.request_deploy_checks();
        }
        self.refresh(tx);
    }

    pub fn open_settings_page(&mut self, page: SettingsPage, tx: &mut Tx) {
        self.editor.page = page;
        self.open_settings(page, tx);
    }

    /// Web: keep the address bar shareable — `#/runs/{id}` is the daemon's check `details_url`.
    fn set_hash(&self, hash: &str) {
        #[cfg(target_arch = "wasm32")]
        {
            if let Some(window) = web_sys::window() {
                let loc = window.location();
                if loc.hash().unwrap_or_default() != hash {
                    let _ = loc.set_hash(hash);
                }
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let _ = hash;
        }
    }

    // -- toasts ----------------------------------------------------------------------------

    pub fn toast(&mut self, tx: &mut Tx, kind: ToastKind, title: &str, body: &str) {
        let id = self.next_toast;
        self.next_toast += 1;
        let life = if matches!(kind, ToastKind::Error) {
            9.0
        } else {
            5.0
        };
        let until = self.clock.max(tx.clock()) + life;
        self.toasts.push(Toast {
            id,
            title: title.to_string(),
            body: body.to_string(),
            kind,
            until,
        });
        while self.toasts.len() > 3 {
            self.toasts.remove(0);
        }
        tx.animated();
        tx.wake_at(until);
    }

    pub fn dismiss_toast(&mut self, id: u64) {
        self.toasts.retain(|t| t.id != id);
    }

    // -- editor ----------------------------------------------------------------------------

    pub fn open_project_form(&mut self, project: Option<usize>, tx: &mut Tx) {
        self.editor.page = SettingsPage::Projects;
        self.editor.project = project;
        self.editor.step = None;
        self.view = View::Settings {
            page: SettingsPage::Projects,
        };
        if !self.editor.loaded {
            self.request_config();
        }
        self.refresh(tx);
    }

    pub fn add_project(&mut self, tx: &mut Tx) {
        let n = self.editor.count("project");
        let fresh = Project::new("");
        paths::set(
            &mut self.editor.draft,
            &format!("project[{n}]"),
            serde_json::to_value(&fresh).unwrap_or(Value::Null),
        );
        self.editor.project = Some(n);
        self.editor.step = None;
        self.editor.new_project = true;
        self.editor.reseed();
        self.refresh(tx);
    }

    pub fn duplicate_project(&mut self, i: usize, tx: &mut Tx) {
        let Some(mut source) = paths::get(&self.editor.draft, &format!("project[{i}]")).cloned()
        else {
            return;
        };
        scrub_masked_project_secrets(&mut source);
        let n = self.editor.count("project");
        paths::set(&mut self.editor.draft, &format!("project[{n}]"), source);
        let name = self.editor.s(&format!("project[{n}].name"));
        paths::set_opt_str(
            &mut self.editor.draft,
            &format!("project[{n}].name"),
            &format!("{name}-copy"),
        );
        self.editor.project = Some(n);
        self.editor.new_project = true;
        self.editor.reseed();
        self.refresh(tx);
    }

    pub fn delete_project(&mut self, i: usize, tx: &mut Tx) {
        if let Some(arr) = paths::get_mut_array(&mut self.editor.draft, "project") {
            if i < arr.len() {
                arr.remove(i);
            }
        }
        self.editor.project = match self.editor.project {
            Some(p) if p == i => None,
            Some(p) if p > i => Some(p - 1),
            other => other,
        };
        self.editor.confirm_delete = None;
        self.editor.new_project = false;
        self.editor.workflows_for = None;
        self.editor.reseed();
        self.refresh(tx);
    }

    pub fn move_project(&mut self, i: usize, forward: bool, tx: &mut Tx) {
        let j = if forward { i + 1 } else { i.wrapping_sub(1) };
        if i == j {
            return;
        }
        if let Some(arr) = paths::get_mut_array(&mut self.editor.draft, "project") {
            if j < arr.len() {
                arr.swap(i, j);
                self.editor.project = match self.editor.project {
                    Some(p) if p == i => Some(j),
                    Some(p) if p == j => Some(i),
                    other => other,
                };
                // Field text follows its project, so a swap must not leave stale values behind.
                self.editor.workflows_for = None;
                self.editor.reseed();
            }
        }
        self.refresh(tx);
    }

    pub fn add_step(&mut self, project: usize, deploy: bool, act: bool, tx: &mut Tx) {
        let array = format!("project[{project}].{}", phase_key(deploy));
        paths::ensure_array(&mut self.editor.draft, &array);
        let n = paths::array_len(&self.editor.draft, &array);
        let step = if act {
            Step {
                event: Some("push".into()),
                workflow: Some(".github/workflows/ci.yml".into()),
                ..Step::new_act("act")
            }
        } else {
            Step {
                run: Some("echo hello".into()),
                ..Step::new_shell("New step")
            }
        };
        paths::set(
            &mut self.editor.draft,
            &format!("{array}[{n}]"),
            serde_json::to_value(&step).unwrap_or(Value::Null),
        );
        self.editor.step = Some((project, deploy, n));
        self.editor.reseed();
        if act {
            self.load_workflows_for(project, &step_path(project, deploy, n));
        }
        self.refresh(tx);
    }

    pub fn remove_step(&mut self, project: usize, deploy: bool, i: usize, tx: &mut Tx) {
        let array = format!("project[{project}].{}", phase_key(deploy));
        if let Some(arr) = paths::get_mut_array(&mut self.editor.draft, &array) {
            if i < arr.len() {
                arr.remove(i);
            }
        }
        self.editor.step = None;
        self.editor.workflows_for = None;
        self.editor.reseed();
        self.refresh(tx);
    }

    pub fn move_step(
        &mut self,
        project: usize,
        deploy: bool,
        i: usize,
        forward: bool,
        tx: &mut Tx,
    ) {
        let j = if forward { i + 1 } else { i.wrapping_sub(1) };
        if i == j {
            return;
        }
        let array = format!("project[{project}].{}", phase_key(deploy));
        if let Some(arr) = paths::get_mut_array(&mut self.editor.draft, &array) {
            if j < arr.len() {
                arr.swap(i, j);
                self.editor.step = match self.editor.step {
                    Some((p, d, s)) if p == project && d == deploy && s == i => Some((p, d, j)),
                    Some((p, d, s)) if p == project && d == deploy && s == j => Some((p, d, i)),
                    other => other,
                };
                self.editor.workflows_for = None;
                self.editor.reseed();
            }
        }
        self.refresh(tx);
    }

    pub fn select_step(&mut self, project: usize, deploy: bool, i: usize, tx: &mut Tx) {
        self.editor.step = Some((project, deploy, i));
        let path = step_path(project, deploy, i);
        if self.editor.s(&format!("{path}.kind")) == "act"
            && self.editor.workflows_for.as_deref() != Some(path.as_str())
        {
            self.load_workflows_for(project, &path);
        }
        self.refresh(tx);
    }

    pub fn discard(&mut self, tx: &mut Tx) {
        self.editor.draft = self.editor.base.clone();
        self.editor.error = None;
        self.editor.field = None;
        self.editor.workflows_for = None;
        self.editor.reseed();
        self.toast(
            tx,
            ToastKind::Info,
            "Changes discarded",
            "The form shows the saved config again.",
        );
    }
}

/// Drops or blanks masked `"********"` secrets in a duplicated project so saving the copy does not
/// persist literal placeholder text (the daemon only unmasks by matching an existing project).
fn scrub_masked_project_secrets(project: &mut Value) {
    let Some(obj) = project.as_object_mut() else {
        return;
    };
    if obj.get("discord_webhook_url").and_then(Value::as_str) == Some(crate::config::MASK) {
        obj.remove("discord_webhook_url");
    }
    for phase in ["ci", "deploy"] {
        let Some(steps) = obj.get_mut(phase).and_then(Value::as_array_mut) else {
            continue;
        };
        for step in steps {
            let Some(secrets) = step.get_mut("secrets").and_then(Value::as_object_mut) else {
                continue;
            };
            for val in secrets.values_mut() {
                if val.as_str() == Some(crate::config::MASK) {
                    *val = Value::String(String::new());
                }
            }
        }
    }
}

/// `project[0].ci[1]`
pub fn step_path(project: usize, deploy: bool, index: usize) -> String {
    let key = phase_key(deploy);
    format!("project[{project}].{key}[{index}]")
}

pub fn phase_key(deploy: bool) -> &'static str {
    if deploy {
        "deploy"
    } else {
        "ci"
    }
}

/// Which settings page a validation `field` belongs to, so the error can be shown where the
/// user is looking.
pub fn page_of(field: &str) -> Option<SettingsPage> {
    let head = field.split(['.', '[']).next().unwrap_or(field);
    match head {
        "project" => Some(SettingsPage::Projects),
        "server" => Some(SettingsPage::Server),
        "github" => Some(SettingsPage::Github),
        "discord" => Some(SettingsPage::Discord),
        "act" => Some(SettingsPage::Act),
        "general" => Some(SettingsPage::General),
        _ => None,
    }
}

/// The step rows for one phase of a project draft, for the editor's list.
pub fn phase_steps(editor: &Editor, project: usize, deploy: bool) -> &[Value] {
    paths::array(
        &editor.draft,
        &format!("project[{project}].{}", phase_key(deploy)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn servers_normalize() {
        assert_eq!(normalize_server("127.0.0.1:7878"), "http://127.0.0.1:7878");
        assert_eq!(normalize_server("http://host:1/"), "http://host:1");
        assert_eq!(normalize_server("  "), "");
    }

    #[test]
    fn the_launch_token_is_sent_from_the_first_request() {
        let s = State::new(Conn {
            server: "127.0.0.1:7878".into(),
            token: "abc".into(),
        });
        assert_eq!(s.token(), "abc");
        assert_eq!(s.server(), "http://127.0.0.1:7878");
    }

    #[test]
    fn connection_fields_become_the_connection() {
        let c = conn_from_fields("lunacy.example:443/", "  abc123\n");
        assert_eq!(c.server, "http://lunacy.example:443");
        assert_eq!(c.token, "abc123");
    }

    #[test]
    fn field_paths_map_to_pages() {
        assert_eq!(
            page_of("project[1].ci[0].run"),
            Some(SettingsPage::Projects)
        );
        assert_eq!(
            page_of("github.app.private_key"),
            Some(SettingsPage::Github)
        );
        assert_eq!(page_of("server.listen"), Some(SettingsPage::Server));
        assert_eq!(page_of("weird"), None);
    }

    #[test]
    fn step_paths_build() {
        assert_eq!(step_path(2, true, 1), "project[2].deploy[1]");
        assert_eq!(step_path(0, false, 3), "project[0].ci[3]");
    }

    #[test]
    fn the_editor_tracks_dirt() {
        let mut e = Editor {
            loaded: true,
            ..Editor::default()
        };
        e.base = serde_json::json!({"project": []});
        e.draft = e.base.clone();
        assert!(!e.dirty());
        e.set_str("server.listen", "0.0.0.0:1");
        assert!(e.dirty());
        assert_eq!(e.s("server.listen"), "0.0.0.0:1");
        e.draft = e.base.clone();
        assert!(!e.dirty());
    }

    #[test]
    fn trigger_toast_names_project_from_projects_view() {
        let mut s = State::new(Conn {
            server: "127.0.0.1:7990".into(),
            token: "tok".into(),
        });
        let mut tx = Tx::default();
        s.apply(
            Call::Trigger {
                project: "alpha".into(),
                force: false,
            },
            Answer::Triggered,
            &mut tx,
        );
        assert_eq!(s.toasts.len(), 1);
        assert_eq!(s.toasts[0].title, "Queued");
        assert!(
            s.toasts[0].body.starts_with("alpha:"),
            "got toast body {:?}",
            s.toasts[0].body
        );
    }

    #[test]
    fn finished_step_logs_do_not_repoll_until_explicit() {
        use crate::model::{Phase, Run, RunStatus, Step, StepKind, StepStatus};

        let mut s = State::new(Conn::default());
        let mut tx = Tx::default();
        s.open_run("r1", &mut tx);
        s.open_step = Some(0);
        s.run = Some(Run {
            id: "r1".into(),
            project: "alpha".into(),
            status: RunStatus::Success,
            steps: vec![Step {
                phase: Phase::Ci,
                name: "Build".into(),
                status: StepStatus::Success,
                kind: StepKind::Shell,
                ..Step::default()
            }],
            ..Run::default()
        });
        s.logs.insert(
            ("r1".into(), 0),
            LogState {
                text: "done\n".into(),
                loading: false,
                error: None,
                mode: Some(LogMode::Tail),
            },
        );
        s.clock = 100.0;
        s.next_log = 10.0;
        s.request_next_log();
        // Because the step is finished and already has an entry, next_log stays untouched.
        assert_eq!(s.next_log, 10.0);
    }

    #[test]
    fn duplicate_project_scrubs_masked_secrets_and_marks_new() {
        let mut s = State::new(Conn::default());
        let mut tx = Tx::default();
        s.editor.loaded = true;
        s.editor.base = serde_json::json!({
            "project": [{
                "name": "alpha",
                "branch": "master",
                "repo_path": "/tmp/alpha",
                "discord_webhook_url": crate::config::MASK,
                "ci": [{
                    "name": "CI",
                    "kind": "act",
                    "workflow": ".github/workflows/ci.yml",
                    "secrets": {"MY_SECRET": crate::config::MASK, "RAW": "plain"}
                }]
            }]
        });
        s.editor.draft = s.editor.base.clone();
        s.duplicate_project(0, &mut tx);
        assert!(s.editor.new_project);
        assert_eq!(s.editor.project, Some(1));
        assert_eq!(s.editor.s("project[1].name"), "alpha-copy");
        assert!(paths::get(&s.editor.draft, "project[1].discord_webhook_url").is_none());
        assert_eq!(
            paths::get_str(&s.editor.draft, "project[1].ci[0].secrets.MY_SECRET"),
            ""
        );
        assert_eq!(
            paths::get_str(&s.editor.draft, "project[1].ci[0].secrets.RAW"),
            "plain"
        );
    }

    #[test]
    fn theme_zoom_and_log_wrap_prefs_round_trip() {
        let dir = std::env::temp_dir().join(format!("lunacy-ui-prefs-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("prefs.json");

        {
            let mut prefs = rune::prefs::Prefs::at(&path);
            save_theme_to(&mut prefs, crate::theme::ThemeMode::Oled);
            save_zoom_to(&mut prefs, Some(1.25));
            save_log_wrap_to(&mut prefs, false);
        }

        let reopened = rune::prefs::Prefs::at(&path);
        assert_eq!(
            load_prefs_from(&reopened),
            (Some(crate::theme::ThemeMode::Oled), Some(1.25), Some(false))
        );

        {
            let mut prefs = rune::prefs::Prefs::at(&path);
            save_zoom_to(&mut prefs, None);
            save_log_wrap_to(&mut prefs, true);
        }
        let reset = rune::prefs::Prefs::at(&path);
        assert_eq!(
            load_prefs_from(&reset),
            (Some(crate::theme::ThemeMode::Oled), None, Some(true))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn log_wrap_defaults_true_and_toggles() {
        let mut s = State::new(Conn::default());
        assert!(s.log_wrap);
        let mut tx = Tx::default();
        s.toggle_log_wrap(&mut tx);
        assert!(!s.log_wrap);
        s.set_log_wrap(true, &mut tx);
        assert!(s.log_wrap);
    }

    #[test]
    fn step_zoom_advances_clamps_and_resets() {
        let mut s = State::new(Conn::default());
        let mut tx = Tx::default();
        s.zoom_track.set((1, 1.1));
        assert_eq!(s.user_zoom, None);
        assert!((s.current_zoom() - 1.1).abs() < 1e-4);

        s.step_zoom(1, &mut tx);
        assert_eq!(s.user_zoom, Some(1.25));
        s.step_zoom(1, &mut tx);
        assert_eq!(s.user_zoom, Some(1.5));
        s.step_zoom(1, &mut tx);
        assert_eq!(s.user_zoom, Some(1.5));

        s.step_zoom(-1, &mut tx);
        assert_eq!(s.user_zoom, Some(1.25));
        s.step_zoom(-1, &mut tx);
        assert_eq!(s.user_zoom, Some(1.1));
        s.step_zoom(-1, &mut tx);
        assert_eq!(s.user_zoom, Some(1.0));
        s.step_zoom(-1, &mut tx);
        assert_eq!(s.user_zoom, Some(0.9));
        s.step_zoom(-1, &mut tx);
        assert_eq!(s.user_zoom, Some(0.9));

        s.set_user_zoom(None, &mut tx);
        assert_eq!(s.user_zoom, None);
    }

    #[test]
    fn finished_cached_step_does_not_keep_polling_logs() {
        let mut s = State::new(Conn::default());
        let run = crate::demo::runs()
            .into_iter()
            .find(|r| !r.status.is_running() && !r.steps.is_empty())
            .expect("finished demo run");
        let run_id = run.id.clone();
        s.view = View::Run { id: run_id.clone() };
        s.run = Some(run);
        s.open_step = Some(0);
        // Uncached finished step still needs an initial fetch.
        assert!(s.open_step_needs_log_poll());
        // Once cached, finished step must not keep clamping `due` to `next_log`.
        s.logs.insert(
            (run_id, 0),
            LogState {
                text: "done\n".into(),
                loading: false,
                error: None,
                mode: Some(LogMode::Tail),
            },
        );
        assert!(!s.open_step_needs_log_poll());
    }

    #[test]
    fn cancel_run_sets_and_clears_state_on_answer_and_final_run() {
        use crate::model::{CancelResult, Run, RunStatus};

        let mut s = State::new(Conn::default());
        s.demo = true;
        let mut tx = Tx::default();
        s.cancel_run("r-live", &mut tx);
        assert_eq!(s.cancelling.as_deref(), Some("r-live"));

        s.apply(
            Call::CancelRun {
                id: "r-live".into(),
            },
            Answer::Cancelled {
                id: "r-live".into(),
                ok: Ok(CancelResult {
                    cancelling: true,
                    run_id: "r-live".into(),
                    project: "lunarum".into(),
                }),
            },
            &mut tx,
        );
        assert_eq!(s.cancelling, None);
        assert_eq!(s.toasts.len(), 1);
        assert_eq!(s.toasts[0].title, "Cancelling run");

        s.cancelling = Some("r-live".into());
        s.apply(
            Call::Run("r-live".into()),
            Answer::Run(Box::new(Run {
                id: "r-live".into(),
                project: "lunarum".into(),
                status: RunStatus::Cancelled,
                ..Run::default()
            })),
            &mut tx,
        );
        assert_eq!(s.cancelling, None);

        s.cancel_project("lunarum", &mut tx);
        assert_eq!(s.cancelling.as_deref(), Some("lunarum"));
        s.apply(
            Call::CancelProject {
                name: "lunarum".into(),
            },
            Answer::Cancelled {
                id: "lunarum".into(),
                ok: Ok(CancelResult {
                    cancelling: true,
                    run_id: String::new(),
                    project: "lunarum".into(),
                }),
            },
            &mut tx,
        );
        assert_eq!(s.cancelling, None);
    }

    #[test]
    fn answer_run_redraws_on_step_and_substep_updates() {
        use crate::model::{Phase, Run, RunStatus, Step, StepKind, StepStatus, Substep};

        let mut s = State::new(Conn::default());
        let mut tx = Tx::default();
        s.open_run("r-steps", &mut tx);
        s.open_step = Some(0);

        let initial = Run {
            id: "r-steps".into(),
            project: "alpha".into(),
            status: RunStatus::Running,
            steps: vec![
                Step {
                    phase: Phase::Ci,
                    name: "Build".into(),
                    status: StepStatus::Running,
                    kind: StepKind::Shell,
                    ..Step::default()
                },
                Step {
                    phase: Phase::Ci,
                    name: "act".into(),
                    status: StepStatus::Pending,
                    kind: StepKind::Act,
                    substeps: vec![],
                    ..Step::default()
                },
            ],
            ..Run::default()
        };

        assert!(s.apply(
            Call::Run("r-steps".into()),
            Answer::Run(Box::new(initial.clone())),
            &mut tx,
        ));
        // Identical poll should not force a redraw.
        assert!(!s.apply(
            Call::Run("r-steps".into()),
            Answer::Run(Box::new(initial.clone())),
            &mut tx,
        ));

        // Non-open step changes status: must redraw immediately.
        let mut step_updated = initial.clone();
        step_updated.steps[1].status = StepStatus::Running;
        assert!(s.apply(
            Call::Run("r-steps".into()),
            Answer::Run(Box::new(step_updated.clone())),
            &mut tx,
        ));

        // Substep added to non-open step: must redraw immediately.
        let mut substep_added = step_updated.clone();
        substep_added.steps[1].substeps.push(Substep {
            job: "ci".into(),
            job_id: "ci".into(),
            name: "Checkout".into(),
            status: StepStatus::Running,
            duration_ms: None,
        });
        assert!(s.apply(
            Call::Run("r-steps".into()),
            Answer::Run(Box::new(substep_added.clone())),
            &mut tx,
        ));

        // Substep status changes: must redraw immediately.
        let mut substep_done = substep_added;
        substep_done.steps[1].substeps[0].status = StepStatus::Success;
        assert!(s.apply(
            Call::Run("r-steps".into()),
            Answer::Run(Box::new(substep_done)),
            &mut tx,
        ));
    }

    #[test]
    fn auto_update_pref_round_trips() {
        let dir = std::env::temp_dir().join(format!(
            "lunacy-ui-autoupdate-pref-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("prefs.json");
        {
            let mut prefs = rune::prefs::Prefs::at(&path);
            save_auto_update_to(&mut prefs, false);
        }
        let reopened = rune::prefs::Prefs::at(&path);
        assert_eq!(load_auto_update_from(&reopened), Some(false));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn deploy_checks_page_is_local_and_receives_report() {
        assert!(SettingsPage::Deploy.is_local());
        assert_eq!(SettingsPage::WINDOW[0], SettingsPage::Deploy);

        let mut s = State::new(Conn::default());
        let mut tx = Tx::default();
        s.deploy_checks_loading = true;
        let report = crate::demo::deploy_checks();
        assert!(!report.projects.is_empty());
        s.apply(
            Call::DeployChecks,
            Answer::DeployChecks(Box::new(report)),
            &mut tx,
        );
        assert!(!s.deploy_checks_loading);
        assert!(s.deploy_checks.is_some());
        assert_eq!(s.deploy_checks_error, None);
    }

    #[test]
    fn force_deploy_and_clear_cache_update_state_and_emit_toasts() {
        use crate::model::{CacheClearResult, ForceDeployResult};

        let mut s = State::new(Conn::default());
        s.demo = true;
        let mut tx = Tx::default();

        s.force_deploy_project("lunarum", &mut tx);
        assert_eq!(s.force_deploying.as_deref(), Some("lunarum"));
        s.apply(
            Call::ForceDeployProject {
                name: "lunarum".into(),
            },
            Answer::ForceDeployed {
                target: "lunarum".into(),
                result: ForceDeployResult {
                    queued: true,
                    sha: "4f1c2b9e8a7d6c5b".into(),
                    deploy_only: true,
                },
            },
            &mut tx,
        );
        assert_eq!(s.force_deploying, None);
        assert_eq!(
            s.toasts.last().map(|t| t.title.as_str()),
            Some("Deploy queued")
        );

        s.clear_cache(Some("lunarum"), &mut tx);
        assert!(s.clearing_cache);
        s.apply(
            Call::ClearCache {
                project: Some("lunarum".into()),
            },
            Answer::CacheCleared {
                project: Some("lunarum".into()),
                result: CacheClearResult {
                    cleared: true,
                    removed: vec!["lunarum".into()],
                },
            },
            &mut tx,
        );
        assert!(!s.clearing_cache);
        assert_eq!(
            s.toasts.last().map(|t| t.title.as_str()),
            Some("Cache cleared")
        );
    }
}

#[cfg(test)]
mod demo_tests {
    /// Screenshots must not say "365d ago": the demo fixtures are re-anchored to now.
    #[test]
    fn demo_runs_are_recent() {
        let runs = crate::demo::runs();
        let now = crate::model::now_unix();
        let newest = runs.iter().map(|r| r.started_at).max().unwrap_or(0);
        assert!(newest > 0, "fixtures carry timestamps");
        assert!(
            now.saturating_sub(newest) <= 3600,
            "newest run is within the hour, got {}",
            now.saturating_sub(newest)
        );
        // Spacing between runs survives the shift.
        let mut times: Vec<u64> = runs.iter().map(|r| r.started_at).collect();
        times.sort_unstable();
        assert!(
            times.windows(2).any(|w| w[1] > w[0]),
            "runs keep distinct start times"
        );
    }
}
