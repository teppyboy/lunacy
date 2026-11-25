//! App state, the polling machine, navigation, and every handler the screens call.
//!
//! The UI thread never blocks: reads go through [`Bus`], which uses `rune::Latest` on native (a
//! shared worker pool) and `spawn_local` + `fetch` on web. Answers land in `poll`, and one
//! `tx.wake_at` for the next deadline is what keeps an idle window from redrawing.

use std::collections::HashMap;

use rune::{Latest, TextEdit, Tx};
use serde_json::Value;

use crate::api::{self, Answer, Call};
use crate::config::{Project, Step};
use crate::model::{Project as ProjectView, ProjectState, Run, SystemInfo, Workflow};
use crate::net::Failure;
use crate::paths::{self, Key};

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
    conn
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
}

const SLOTS: usize = 8;

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
    }
}

fn slot_index(slot: Slot) -> usize {
    slot_of(slot)
}

pub type Outcome = Result<Answer, Failure>;

/// The request layer: fire a `Call`, drain the answers later. Never blocks the UI thread.
pub struct Bus {
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

    pub fn cancel(&self) {
        #[cfg(not(target_arch = "wasm32"))]
        for latest in &self.latest {
            latest.cancel();
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
                    s.answer(call, out, tx)
                });
            });
        }
    }

    /// Every answer that landed since the last drain.
    fn drain(&self) -> Vec<(Call, Outcome)> {
        let mut out = Vec::new();
        #[cfg(not(target_arch = "wasm32"))]
        for latest in &self.latest {
            while let Some(answer) = latest.take() {
                out.push(answer);
            }
        }
        out
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
    Diagnostics,
    Connection,
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

    pub fn title(self) -> &'static str {
        match self {
            SettingsPage::General => "General",
            SettingsPage::Server => "Server",
            SettingsPage::Github => "GitHub",
            SettingsPage::Discord => "Discord",
            SettingsPage::Act => "act",
            SettingsPage::Projects => "Projects",
            SettingsPage::Diagnostics => "Diagnostics",
            SettingsPage::Connection => "Connection",
        }
    }

    pub fn blurb(self) -> &'static str {
        match self {
            SettingsPage::General => "State directory, poll interval and the default shell.",
            SettingsPage::Server => {
                "Where the HTTP API listens, its token, and the web build's root."
            }
            SettingsPage::Github => "How check runs are reported: a token, or a GitHub App.",
            SettingsPage::Discord => "Webhook notifications when a pipeline starts or finishes.",
            SettingsPage::Act => "The act binary, its image mapping and default arguments.",
            SettingsPage::Projects => {
                "Repositories to watch, and the CI and deploy steps for each."
            }
            SettingsPage::Diagnostics => "The daemon's host, tools and config file.",
            SettingsPage::Connection => "This window's server URL and token. Stored nowhere.",
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

    pub toasts: Vec<Toast>,
    next_toast: u64,
    /// A "Run now" in flight, by project.
    pub triggering: Option<String>,
    /// The force toggle beside "Run now".
    pub force: bool,

    pub bus: Bus,

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
}

impl State {
    pub fn new(conn: Conn) -> State {
        let mut state = State {
            live: Conn {
                server: String::new(),
                token: String::new(),
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
            toasts: Vec::new(),
            next_toast: 1,
            triggering: None,
            force: false,
            bus: Bus::default(),
            next_projects: 0.0,
            next_runs: 0.0,
            next_run: 0.0,
            next_log: 0.0,
            next_system: 0.0,
            asked_system: false,
            clock: 0.0,
            ready: false,
        };
        state.ui_server = TextEdit::new(state.conn.server.clone());
        state.ui_token = TextEdit::new(state.conn.token.clone());
        if let Some(id) = initial_run_id() {
            state.view = View::Run { id };
        }
        state
    }

    // -- connection ------------------------------------------------------------------------

    /// Adopts the settings fields' URL and token and refreshes everything.
    pub fn reconnect(&mut self, tx: &mut Tx) {
        self.live = self.conn.clone();
        self.live.server = normalize_server(&self.live.server);
        self.link = Link::Connecting;
        self.loaded = false;
        self.projects_error = None;
        self.run_error = None;
        self.runs_error = None;
        self.system_error = None;
        self.editor = Editor {
            text: HashMap::new(),
            ..Editor::default()
        };
        self.bus.cancel();
        self.next_projects = 0.0;
        self.next_runs = 0.0;
        self.next_run = 0.0;
        self.next_system = 0.0;
        self.asked_system = false;
        self.request_projects();
        self.request_config();
        self.request_system();
        self.refresh(tx);
    }

    /// The runtime's waker, from `AppSpec::setup`: native answers wake `poll` with it.
    pub fn attach(&mut self, waker: rune::Waker) {
        #[cfg(not(target_arch = "wasm32"))]
        self.bus.set_waker(waker);
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
            Call::Projects | Call::Config | Call::System | Call::Health => true,
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
            Call::Trigger { .. } | Call::SaveConfig(_) => true,
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
                let open = self.open_step.filter(|i| *i < run.steps.len());
                let changed = self
                    .run
                    .as_ref()
                    .map(|r| (r.status, r.finished_at, r.steps.len()))
                    != Some((run.status, run.finished_at, run.steps.len()));
                self.open_step = open;
                self.run = Some(*run);
                self.run_error = None;
                self.request_next_log();
                changed
            }
            Answer::Log {
                run,
                index,
                text,
                tail,
            } => {
                let clock = self.clock;
                let _ = clock;
                match self.logs.get_mut(&(run, index)) {
                    Some(entry)
                        if entry.mode
                            == Some(if tail.is_some() {
                                LogMode::Tail
                            } else {
                                LogMode::Full
                            }) =>
                    {
                        let grew = entry.text != text;
                        entry.text = text;
                        entry.loading = false;
                        entry.error = None;
                        grew
                    }
                    // A tail answer for a log the reader asked in full: drop it.
                    _ => false,
                }
            }
            Answer::Triggered => {
                self.triggering = None;
                let project = self.current_project_name().unwrap_or_default();
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
                self.toast(
                    tx,
                    ToastKind::Success,
                    "Config saved",
                    "Written to config.toml; idle workers reloaded. Running pipelines finish on the old config.",
                );
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
        self.request_workflows(path, name, None);
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

    fn request_next_log(&mut self) {
        let Some(run) = self.run.clone() else { return };
        let Some(index) = self.open_step else { return };
        let Some(step) = run.steps.get(index) else {
            return;
        };
        let entry = self.logs.get(&(run.id.clone(), index));
        let live = step.status.is_live();
        let stale = !live || self.clock >= self.next_log;
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

        for (call, result) in self.bus.drain() {
            let wanted = self.wants(&call);
            let applied = self.answer(call, result, tx);
            changed |= wanted && applied;
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

        // One wake for the nearest deadline, so an idle window draws nothing.
        let mut due = self.next_projects;
        match &self.view {
            View::Run { .. } => {
                due = due.min(self.next_run);
                if self.open_step.is_some() {
                    due = due.min(self.next_log);
                }
            }
            View::Project { .. } => due = due.min(self.next_runs),
            _ => {}
        }
        if due.is_finite() {
            tx.wake_at(if due > self.clock {
                due
            } else {
                self.clock + 0.02
            });
        }

        if !self.toasts.is_empty() {
            let before = self.toasts.len();
            self.toasts.retain(|t| t.until > self.clock);
            changed |= before != self.toasts.len();
        }
        changed
    }

    /// Asks for the next frame now (navigation, a toast, a save).
    pub fn refresh(&mut self, tx: &mut Tx) {
        tx.instant();
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
        self.set_hash("");
        self.next_projects = 0.0;
        self.refresh(tx);
    }

    pub fn open_project(&mut self, name: &str, tx: &mut Tx) {
        self.view = View::Project {
            name: name.to_string(),
        };
        self.runs.clear();
        self.runs_for = None;
        self.runs_error = None;
        self.next_runs = 0.0;
        self.set_hash("");
        self.refresh(tx);
    }

    pub fn open_run(&mut self, id: &str, tx: &mut Tx) {
        self.view = View::Run { id: id.to_string() };
        self.run = None;
        self.open_step = None;
        self.run_error = None;
        self.logs.clear();
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
        self.editor.page = page;
        self.set_hash("");
        if !matches!(page, SettingsPage::Connection | SettingsPage::Diagnostics)
            && !self.editor.loaded
        {
            self.request_config();
        }
        if page == SettingsPage::Diagnostics {
            self.request_system();
            self.next_system = self.clock + 30.0;
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
        self.toasts.push(Toast {
            id,
            title: title.to_string(),
            body: body.to_string(),
            kind,
            until: self.clock + life,
        });
        while self.toasts.len() > 3 {
            self.toasts.remove(0);
        }
        tx.instant();
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
        let Some(source) = paths::get(&self.editor.draft, &format!("project[{i}]")).cloned() else {
            return;
        };
        let n = self.editor.count("project");
        paths::set(&mut self.editor.draft, &format!("project[{n}]"), source);
        let name = self.editor.s(&format!("project[{n}].name"));
        paths::set_opt_str(
            &mut self.editor.draft,
            &format!("project[{n}].name"),
            &format!("{name}-copy"),
        );
        self.editor.project = Some(n);
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
        self.editor.reseed();
        self.toast(
            tx,
            ToastKind::Info,
            "Changes discarded",
            "The form shows the saved config again.",
        );
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
