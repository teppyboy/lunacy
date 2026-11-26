//! HTTP API & WebSocket server for the Rune UI (see `docs/api.md`) plus GitHub webhooks.

use crate::config::{self, Config, TriggerMode, MASK};
use crate::daemon::{upsert_open_pr, Command, Shared};
use crate::git;
use crate::model::{self, Run, Trigger};
use crate::pipeline::PrContext;

use anyhow::{Context, Result};
use axum::body::Body;
use axum::extract::State;
use axum::http::header::{
    ACCESS_CONTROL_ALLOW_HEADERS, ACCESS_CONTROL_ALLOW_METHODS, ACCESS_CONTROL_ALLOW_ORIGIN,
    AUTHORIZATION, CACHE_CONTROL, CONTENT_TYPE,
};
use axum::http::{HeaderMap, HeaderValue, Method, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use hmac::{Hmac, KeyInit, Mac};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::Sha256;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;
const DISCORD_TEST_COOLDOWN: Duration = Duration::from_secs(2);

/// In-flight and cooldown guard for `POST /api/discord/test`.
#[derive(Default)]
pub struct DiscordTestGuard {
    in_flight: bool,
    last_sent: Option<std::time::Instant>,
}

impl DiscordTestGuard {
    pub fn reset(&mut self) {
        self.in_flight = false;
        self.last_sent = None;
    }
}

/// One project's command channel plus its worker generation.
pub type Commands = Arc<Mutex<BTreeMap<String, Sender<Command>>>>;

pub struct ApiState {
    pub shared: Arc<Mutex<Shared>>,
    pub commands: Commands,
    pub config_path: PathBuf,
    /// Bumped whenever a reload happened; the server thread reports it.
    pub reloads: Arc<Mutex<u32>>,
    /// Rate-limits `POST /api/discord/test` against concurrent or rapid abuse.
    pub discord_test_guard: Mutex<DiscordTestGuard>,
}

pub fn build_router(state: Arc<ApiState>) -> Router {
    Router::new()
        .route(
            "/api/ws",
            get(crate::ws::ws_handler).options(options_response),
        )
        .fallback(dispatch_http)
        .with_state(state)
}

pub fn serve(state: Arc<ApiState>, listen: &str) -> Result<()> {
    let std_listener =
        std::net::TcpListener::bind(listen).with_context(|| format!("binding {listen}"))?;
    std_listener
        .set_nonblocking(true)
        .with_context(|| format!("setting nonblocking on {listen}"))?;

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(4)
        .thread_name("lunacy-api")
        .build()
        .context("building Tokio runtime for API server")?;

    rt.block_on(async move {
        let listener = tokio::net::TcpListener::from_std(std_listener)
            .with_context(|| format!("converting TCP listener for {listen}"))?;
        log::info!("API listening on http://{listen}");

        let reload_state = state.clone();
        let reload_task = tokio::spawn(async move {
            let mut last_reloads = *reload_state.reloads.lock().expect("reloads");
            let mut interval = tokio::time::interval(Duration::from_millis(250));
            loop {
                interval.tick().await;
                let shutdown = reload_state
                    .shared
                    .lock()
                    .map(|s| s.shutdown)
                    .unwrap_or(true);
                if shutdown {
                    break;
                }
                let now = *reload_state.reloads.lock().expect("reloads");
                if now != last_reloads {
                    last_reloads = now;
                    reload_state.apply_config_changes();
                }
            }
        });

        let shutdown_shared = state.shared.clone();
        let app = build_router(state);
        let serve_res = axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                loop {
                    let stop = shutdown_shared.lock().map(|s| s.shutdown).unwrap_or(true);
                    if stop {
                        if let Ok(s) = shutdown_shared.lock() {
                            s.ws.notify_shutdown();
                        }
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            })
            .await;

        reload_task.abort();
        serve_res.map_err(|e| anyhow::anyhow!("API server error: {e}"))
    })
}

impl ApiState {
    /// Push a new config (and worker set) after a PUT /api/config.
    fn apply_config_changes(&self) {
        let cfg = self.shared.lock().expect("shared").config.clone();
        let want: Vec<String> = cfg.projects.iter().map(|p| p.name.clone()).collect();
        let commands = self.commands.lock().expect("commands");
        for (name, tx) in commands.iter() {
            if want.contains(name) {
                let _ = tx.send(Command::Reload);
            }
        }
        log::info!("configuration applied to idle workers");
    }
}

/// Split `/path?query` into parts.
fn split_path(url: &str) -> (String, BTreeMap<String, String>) {
    let (path, query) = match url.split_once('?') {
        Some((p, q)) => (p, q),
        None => (url, ""),
    };
    let mut params = BTreeMap::new();
    for pair in query.split('&').filter(|s| !s.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        params.insert(percent_decode(k), percent_decode(v));
    }
    (path.to_string(), params)
}

fn percent_decode_impl(s: &str, plus_as_space: bool) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                match u8::from_str_radix(hex, 16) {
                    Ok(b) => {
                        out.push(b);
                        i += 3;
                    }
                    Err(_) => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            b'+' if plus_as_space => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

fn percent_decode(s: &str) -> String {
    percent_decode_impl(s, true)
}

fn percent_decode_path_segment(s: &str) -> String {
    percent_decode_impl(s, false)
}

async fn options_response() -> Response {
    cors_options_response()
}

fn cors_options_response() -> Response {
    let mut res = StatusCode::NO_CONTENT.into_response();
    let headers = res.headers_mut();
    headers.insert(ACCESS_CONTROL_ALLOW_ORIGIN, HeaderValue::from_static("*"));
    headers.insert(
        ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static("Authorization, Content-Type"),
    );
    headers.insert(
        ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("GET, PUT, POST, OPTIONS"),
    );
    res
}

fn json_response(status: u16, body: &Value) -> Response {
    let code = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let mut res = (code, body.to_string()).into_response();
    let headers = res.headers_mut();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(ACCESS_CONTROL_ALLOW_ORIGIN, HeaderValue::from_static("*"));
    headers.insert(
        ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static("Authorization, Content-Type"),
    );
    headers.insert(
        ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("GET, PUT, POST, OPTIONS"),
    );
    res
}

pub fn error_response(status: u16, message: &str) -> Response {
    json_response(status, &json!({ "error": message }))
}

fn text_response(content_type: &'static str, body: String) -> Response {
    let mut res = (StatusCode::OK, body).into_response();
    let headers = res.headers_mut();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
    headers.insert(ACCESS_CONTROL_ALLOW_ORIGIN, HeaderValue::from_static("*"));
    res
}

pub(crate) fn is_valid_segment(s: &str) -> bool {
    !s.is_empty()
        && s != "."
        && !s.contains("..")
        && !s.contains('/')
        && !s.contains('\\')
        && !s.chars().any(|c| c.is_ascii_control())
}

async fn dispatch_http(State(state): State<Arc<ApiState>>, req: Request<Body>) -> Response {
    let method = req.method().clone();
    if method == Method::OPTIONS {
        return cors_options_response();
    }

    let url = req
        .uri()
        .path_and_query()
        .map(|pq| pq.as_str().to_string())
        .unwrap_or_else(|| req.uri().path().to_string());
    let path = req.uri().path().to_string();
    let headers = req.headers().clone();

    if path == "/api/health" {
        let body = json!({"ok": true, "version": VERSION});
        return json_response(200, &body);
    }

    let is_static_get = !path.starts_with("/api") && method == Method::GET;
    let is_webhook_post = path == "/api/github/webhook" && method == Method::POST;

    if is_webhook_post {
        let has_secret = state
            .shared
            .lock()
            .expect("shared")
            .config
            .github
            .as_ref()
            .and_then(|g| g.webhook_secret_value())
            .filter(|s| !s.is_empty() && s != MASK)
            .is_some();
        if !has_secret {
            return error_response(404, "no webhook secret configured");
        }
        let has_sig_prefix = headers
            .get("x-hub-signature-256")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|s| s.starts_with("sha256="));
        if !has_sig_prefix {
            return error_response(401, "invalid signature");
        }
    } else if !is_static_get {
        if let Err((status, message)) = authorize(&state, &headers) {
            return error_response(status, &message);
        }
    }

    let body_bytes = if matches!(method, Method::POST | Method::PUT | Method::PATCH) {
        match axum::body::to_bytes(req.into_body(), MAX_BODY_BYTES).await {
            Ok(b) => b,
            Err(e) => {
                log::warn!("failed reading request body: {e}");
                return error_response(400, "failed reading request body");
            }
        }
    } else {
        axum::body::Bytes::new()
    };

    tokio::task::spawn_blocking(move || handle_sync(&state, method, &url, &headers, &body_bytes))
        .await
        .unwrap_or_else(|e| {
            log::error!("API worker task panicked: {e}");
            error_response(500, "internal server error")
        })
}

fn handle_sync(
    state: &Arc<ApiState>,
    method: Method,
    url: &str,
    headers: &HeaderMap,
    body_bytes: &[u8],
) -> Response {
    let (path, params) = split_path(url);

    // Static UI files at `/`.
    if !path.starts_with("/api") && method == Method::GET {
        return serve_static(state, &path);
    }

    if path == "/api/health" {
        let body = json!({"ok": true, "version": VERSION});
        return json_response(200, &body);
    }

    // The webhook authenticates with its own HMAC, not the bearer token.
    if path == "/api/github/webhook" && method == Method::POST {
        return handle_webhook(state, headers, body_bytes);
    }

    if let Err((status, message)) = authorize(state, headers) {
        return error_response(status, &message);
    }

    let segments_owned: Vec<String> = path
        .trim_matches('/')
        .split('/')
        .map(percent_decode_path_segment)
        .collect();
    if segments_owned
        .iter()
        .any(|s| !s.is_empty() && !is_valid_segment(s))
    {
        return error_response(400, "invalid path segment");
    }
    let segments: Vec<&str> = segments_owned.iter().map(String::as_str).collect();
    let body_str = String::from_utf8_lossy(body_bytes);

    match (method, segments.as_slice()) {
        (Method::GET, ["api", "projects"]) => {
            let body = projects_json(state);
            json_response(200, &body)
        }
        (Method::GET, ["api", "projects", name]) => {
            let body = projects_json(state);
            match body
                .as_array()
                .and_then(|a| a.iter().find(|p| p["name"] == *name).cloned())
            {
                Some(p) => json_response(200, &p),
                None => error_response(404, "unknown project"),
            }
        }
        (Method::GET, ["api", "projects", name, "runs"]) => {
            let limit = params
                .get("limit")
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(20)
                .clamp(1, 200);
            let state_dir = state.shared.lock().expect("shared").state_dir.clone();
            let runs = model::list_runs(&state_dir, name, limit);
            let body: Vec<Value> = runs.iter().map(run_json).collect();
            json_response(200, &Value::Array(body))
        }
        (Method::GET, ["api", "projects", name, "workflows"]) => {
            handle_workflows(state, name, &params)
        }
        (Method::POST, ["api", "projects", name, "trigger"]) => {
            handle_trigger(state, &body_str, name)
        }
        (Method::POST, ["api", "projects", name, "deploy"]) => {
            handle_deploy_project(state, &body_str, name)
        }
        (Method::POST, ["api", "projects", name, "cancel"]) => handle_cancel_project(state, name),
        (Method::POST, ["api", "projects", name, "cache", "clear"]) => {
            handle_clear_project_cache(state, name)
        }
        (Method::GET, ["api", "runs", id]) => {
            let state_dir = state.shared.lock().expect("shared").state_dir.clone();
            match lookup_run(&state_dir, id) {
                Some(run) => json_response(200, &run_json(&run)),
                None => error_response(404, "unknown run"),
            }
        }
        (Method::POST, ["api", "runs", id, "deploy"]) => handle_deploy_run(state, &body_str, id),
        (Method::POST, ["api", "runs", id, "retry"]) => handle_retry_run(state, &body_str, id),
        (Method::POST, ["api", "runs", id, "cancel"]) => handle_cancel_run(state, id),
        (Method::GET, ["api", "runs", id, "steps", index, "log"]) => {
            handle_step_log(state, id, index, &params)
        }
        (Method::POST, ["api", "cache", "clear"]) => handle_clear_cache(state),
        (Method::POST, ["api", "discord", "test"]) => handle_discord_test(state, &body_str),
        (Method::GET, ["api", "deploy", "checks"]) => handle_deploy_checks(state),
        (Method::GET, ["api", "config"]) => {
            let cfg = state.shared.lock().expect("shared").config.clone();
            match serde_json::to_value(cfg.masked()) {
                Ok(body) => json_response(200, &body),
                Err(e) => error_response(500, &format!("serialising config: {e}")),
            }
        }
        (Method::PUT, ["api", "config"]) => handle_put_config(state, &body_str),
        (Method::GET, ["api", "system"]) => {
            let body = system_json(state);
            json_response(200, &body)
        }
        _ => error_response(404, "not found"),
    }
}

fn authorize(state: &Arc<ApiState>, headers: &HeaderMap) -> Result<(), (u16, String)> {
    let token = state
        .shared
        .lock()
        .expect("shared")
        .config
        .server
        .as_ref()
        .map(|s| s.token_value())
        .unwrap_or_default();
    if token.is_empty() {
        return Ok(());
    }
    let header = headers
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let supplied = header.strip_prefix("Bearer ").unwrap_or("");
    if constant_time_eq(supplied.as_bytes(), token.as_bytes()) {
        Ok(())
    } else {
        Err((401, "missing or invalid token".into()))
    }
}

pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

pub fn projects_json(state: &Arc<ApiState>) -> Value {
    let shared = state.shared.lock().expect("shared");
    let state_dir = shared.state_dir.clone();
    let list: Vec<Value> = shared
        .projects
        .iter()
        .map(|p| {
            let cfg_p = shared.config.project(&p.name);
            let trigger_mode = cfg_p
                .map(|c| c.effective_trigger_mode(&shared.config.general))
                .unwrap_or_else(|| shared.config.general.effective_trigger_mode())
                .as_str();
            let max_concurrent_runs = cfg_p
                .map(|c| c.effective_max_concurrent_runs())
                .unwrap_or(1);
            let active_runs: Vec<Value> = if !p.active_runs.is_empty() {
                p.active_runs.iter().map(|a| run_json(&a.run)).collect()
            } else if p.running {
                p.current_run
                    .as_ref()
                    .map(|r| vec![run_json(r)])
                    .unwrap_or_default()
            } else {
                Vec::new()
            };
            let mut value = json!({
                "name": p.name,
                "enabled": cfg_p.map(|c| c.is_enabled()).unwrap_or(true),
                "branch": p.branch,
                "github_repo": p.github_repo,
                "upstream_sha": p.upstream_sha,
                "last_sha": p.last_sha,
                "running": p.running,
                "current_run": p.current_run.as_ref().map(run_json),
                "active_runs": active_runs,
                "max_concurrent_runs": max_concurrent_runs,
                "last_run": p
                    .last_run
                    .as_ref()
                    .map(run_json)
                    .or_else(|| {
                        model::list_runs(&state_dir, &p.name, 1)
                            .first()
                            .map(run_json)
                    }),
                "open_prs": p.open_prs,
                "watch_prs": cfg_p.map(|c| c.watch_prs).unwrap_or(false),
                "watch_tags": cfg_p.map(|c| c.watch_tags).unwrap_or(false),
                "deploy_refs": cfg_p.map(|c| c.deploy_refs.clone()).unwrap_or_default(),
                "trigger_mode": trigger_mode,
            });
            if let Some(err) = &p.last_error {
                value["last_error"] = json!(err);
            }
            value
        })
        .collect();
    Value::Array(list)
}

pub fn run_json(run: &Run) -> Value {
    let mut v = json!({
        "id": run.id,
        "project": run.project,
        "sha": run.sha,
        "subject": run.subject,
        "status": run.status,
        "started_at": run.started_at,
        "finished_at": run.finished_at,
        "failed_step": run.failed_step,
        "github_url": run.github_url,
        "trigger": run.trigger,
        "deploy_only": run.deploy_only,
        "steps": run.steps.iter().map(|s| json!({
            "phase": s.phase,
            "name": s.name,
            "status": s.status,
            "duration_ms": s.duration_ms,
            "exit_code": s.exit_code,
            "kind": s.kind,
            "substeps": s.substeps.iter().map(|sub| json!({
                "job": sub.job,
                "job_id": sub.job_id,
                "name": sub.name,
                "status": sub.status,
                "duration_ms": sub.duration_ms,
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
    });
    if let Some(reason) = &run.trigger_reason {
        v["trigger_reason"] = json!(reason);
    }
    if let Some(git_ref) = &run.git_ref {
        v["git_ref"] = json!(git_ref);
    }
    if let Some(pr_number) = run.pr_number {
        v["pr_number"] = json!(pr_number);
    }
    if let Some(pr_title) = &run.pr_title {
        v["pr_title"] = json!(pr_title);
    }
    if let Some(pr_head_ref) = &run.pr_head_ref {
        v["pr_head_ref"] = json!(pr_head_ref);
    }
    if let Some(pr_base_ref) = &run.pr_base_ref {
        v["pr_base_ref"] = json!(pr_base_ref);
    }
    if let Some(reason) = &run.deploy_skipped_reason {
        v["deploy_skipped_reason"] = json!(reason);
    }
    v
}

fn lookup_run(state_dir: &Path, id: &str) -> Option<Run> {
    model::read_run(state_dir, id)
}

fn handle_step_log(
    state: &Arc<ApiState>,
    id: &str,
    index: &str,
    params: &BTreeMap<String, String>,
) -> Response {
    let state_dir = state.shared.lock().expect("shared").state_dir.clone();
    let Some(run) = lookup_run(&state_dir, id) else {
        return error_response(404, "unknown run");
    };
    let Ok(index) = index.parse::<usize>() else {
        return error_response(404, "bad step index");
    };
    if index >= run.steps.len() {
        return error_response(404, "step index out of range");
    }
    let path = model::run_log_path(&state_dir, &run.project, &run.id, index);
    if !path.is_file() {
        return text_response("text/plain; charset=utf-8", String::new());
    }
    let tail = params.get("tail").and_then(|v| v.parse::<usize>().ok());
    let body = match tail {
        Some(n) => model::tail_lines(&path, n).unwrap_or_default(),
        None => std::fs::read_to_string(&path).unwrap_or_default(),
    };
    text_response("text/plain; charset=utf-8", body)
}

#[derive(Debug, Deserialize, Default)]
struct TriggerBody {
    #[serde(default)]
    sha: Option<String>,
    #[serde(default)]
    force: bool,
    #[serde(default)]
    deploy_only: bool,
    #[serde(default)]
    force_deploy: bool,
    #[serde(default, rename = "ref")]
    git_ref: Option<String>,
    #[serde(default)]
    pr_number: Option<u64>,
    #[serde(default)]
    reason: Option<String>,
}

fn release_reserved_slot(state: &Arc<ApiState>, project_name: &str, slot: usize) {
    let ws = {
        let mut shared = state.shared.lock().expect("shared");
        if let Some(p) = shared.projects.iter_mut().find(|p| p.name == project_name) {
            p.release_pending_slot(slot);
        }
        shared.ws.clone()
    };
    ws.emit_projects_updated();
}

fn handle_trigger(state: &Arc<ApiState>, body: &str, name: &str) -> Response {
    let parsed: TriggerBody = if body.trim().is_empty() {
        TriggerBody::default()
    } else {
        match serde_json::from_str(body) {
            Ok(b) => b,
            Err(e) => return error_response(400, &format!("invalid JSON: {e}")),
        }
    };

    let req_sha = parsed.sha.filter(|s| !s.is_empty());
    if let Some(ref s) = req_sha {
        if !git::is_valid_sha(s) {
            return error_response(400, "invalid commit sha");
        }
    }
    let req_git_ref = parsed.git_ref.filter(|r| !r.is_empty());
    if let Some(ref r) = req_git_ref {
        if !git::is_valid_git_ref(r) {
            return error_response(400, "invalid git ref");
        }
    }

    let tx_opt = state.commands.lock().expect("commands").get(name).cloned();
    let (tx, resolved_sha, git_ref, pr_ctx, reserved, ws) = {
        let mut shared = state.shared.lock().expect("shared");
        let max_concurrent = shared
            .config
            .project(name)
            .map(|c| c.effective_max_concurrent_runs())
            .unwrap_or(1);
        let ws = shared.ws.clone();
        let Some(p) = shared.projects.iter_mut().find(|p| p.name == name) else {
            return error_response(404, "unknown project");
        };
        let Some(tx) = tx_opt else {
            return error_response(503, "project worker is not running");
        };
        if p.active_run_count() >= max_concurrent {
            return error_response(409, "project is already running");
        }
        let branch = p.branch.clone();
        let pr_summary = parsed
            .pr_number
            .and_then(|num| p.open_prs.iter().find(|pr| pr.number == num).cloned());
        let pr_ctx = parsed.pr_number.map(|num| match &pr_summary {
            Some(pr) => PrContext {
                number: pr.number,
                title: pr.title.clone(),
                head_ref: pr.head_ref.clone(),
                base_ref: pr.base_ref.clone(),
                base_sha: String::new(),
            },
            None => PrContext {
                number: num,
                title: String::new(),
                head_ref: req_git_ref.clone().unwrap_or_else(|| format!("pull/{num}")),
                base_ref: branch.clone(),
                base_sha: String::new(),
            },
        });
        let resolved_sha = req_sha.or_else(|| pr_summary.as_ref().map(|pr| pr.head_sha.clone()));
        if let Some(ref s) = resolved_sha {
            if !git::is_valid_sha(s) {
                return error_response(400, "invalid commit sha");
            }
        }
        let git_ref = req_git_ref.or_else(|| {
            pr_ctx.as_ref().map(|p| {
                if p.head_ref.is_empty() {
                    format!("pull/{}", p.number)
                } else {
                    p.head_ref.clone()
                }
            })
        });
        if let Some(ref r) = git_ref {
            if !git::is_valid_git_ref(r) {
                return error_response(400, "invalid git ref");
            }
        }
        let Some(reserved) = p.try_reserve_pending_slot(
            max_concurrent,
            resolved_sha.clone(),
            git_ref.clone(),
            parsed.pr_number,
        ) else {
            return error_response(409, "project is already running");
        };
        (tx, resolved_sha, git_ref, pr_ctx, reserved, ws)
    };

    let trigger = if parsed.pr_number.is_some() {
        Trigger::PullRequest
    } else if parsed.deploy_only || parsed.force_deploy {
        Trigger::ForceDeploy
    } else {
        Trigger::Manual
    };

    let trigger_reason = model::sanitize_trigger_reason(parsed.reason.as_deref()).or_else(|| {
        Some(if let Some(n) = parsed.pr_number {
            format!("Manual PR #{n} trigger")
        } else if parsed.deploy_only || parsed.force_deploy {
            "Manual force deploy".to_string()
        } else if parsed.force {
            "Manual API trigger (force)".to_string()
        } else {
            "Manual API trigger".to_string()
        })
    });

    let slot = reserved.0;
    if tx
        .send(Command::Trigger {
            sha: resolved_sha,
            force: parsed.force || parsed.deploy_only || parsed.force_deploy,
            trigger,
            git_ref,
            pr: pr_ctx,
            deploy_only: parsed.deploy_only,
            force_deploy: parsed.force_deploy,
            trigger_reason,
            reserved: Some(reserved),
        })
        .is_err()
    {
        release_reserved_slot(state, name, slot);
        return error_response(503, "project worker is not running");
    }
    ws.emit_projects_updated();
    json_response(202, &json!({"queued": true}))
}

#[derive(Debug, Deserialize, Default)]
struct DeployBody {
    #[serde(default)]
    sha: Option<String>,
    #[serde(default)]
    skip_ci: Option<bool>,
    #[serde(default)]
    reason: Option<String>,
}

fn handle_deploy_project(state: &Arc<ApiState>, body: &str, name: &str) -> Response {
    let parsed: DeployBody = if body.trim().is_empty() {
        DeployBody::default()
    } else {
        match serde_json::from_str(body) {
            Ok(b) => b,
            Err(e) => return error_response(400, &format!("invalid JSON: {e}")),
        }
    };

    let req_sha = parsed.sha.filter(|s| !s.trim().is_empty());
    if let Some(ref s) = req_sha {
        if !git::is_valid_sha(s) {
            return error_response(400, "invalid commit sha");
        }
    }

    let (project_cfg, at_capacity, upstream, last_sha) = {
        let shared = state.shared.lock().expect("shared");
        let cfg_p = shared.config.project(name).cloned();
        let state_p = shared.projects.iter().find(|p| p.name == name);
        match (cfg_p, state_p) {
            (Some(c), Some(s)) => (
                c.clone(),
                s.active_run_count() >= c.effective_max_concurrent_runs(),
                s.upstream_sha.clone(),
                s.last_sha.clone(),
            ),
            (Some(c), None) => (c, false, None, None),
            (None, _) => return error_response(404, "unknown project"),
        }
    };

    if project_cfg.deploy.is_empty() {
        return error_response(400, "project has no deploy steps");
    }
    if at_capacity {
        return error_response(409, "project is already running");
    }
    let tx = state.commands.lock().expect("commands").get(name).cloned();
    let Some(tx) = tx else {
        return error_response(503, "project worker is not running");
    };

    let target_sha = req_sha
        .or(upstream)
        .or(last_sha)
        .or_else(|| git::rev_parse(&project_cfg.repo_path, "HEAD").ok());
    let Some(target_sha) = target_sha else {
        return error_response(
            409,
            "no resolvable sha yet; pass sha explicitly or wait for the first poll",
        );
    };
    if !git::is_valid_sha(&target_sha) {
        return error_response(400, "invalid commit sha");
    }

    let (reserved, ws) = {
        let mut shared = state.shared.lock().expect("shared");
        let max_concurrent = shared
            .config
            .project(name)
            .map(|c| c.effective_max_concurrent_runs())
            .unwrap_or(1);
        let ws = shared.ws.clone();
        let Some(p) = shared.projects.iter_mut().find(|p| p.name == name) else {
            return error_response(404, "unknown project");
        };
        let Some(reserved) = p.try_reserve_pending_slot(
            max_concurrent,
            Some(target_sha.clone()),
            Some(project_cfg.branch.clone()),
            None,
        ) else {
            return error_response(409, "project is already running");
        };
        (reserved, ws)
    };

    let deploy_only = parsed.skip_ci.unwrap_or(true);
    let trigger_reason = model::sanitize_trigger_reason(parsed.reason.as_deref())
        .or_else(|| Some("Force deploy".to_string()));
    let slot = reserved.0;
    if tx
        .send(Command::Trigger {
            sha: Some(target_sha.clone()),
            force: true,
            trigger: Trigger::ForceDeploy,
            git_ref: Some(project_cfg.branch.clone()),
            pr: None,
            deploy_only,
            force_deploy: true,
            trigger_reason,
            reserved: Some(reserved),
        })
        .is_err()
    {
        release_reserved_slot(state, name, slot);
        return error_response(503, "project worker is not running");
    }
    ws.emit_projects_updated();
    json_response(
        202,
        &json!({
            "queued": true,
            "sha": target_sha,
            "deploy_only": deploy_only,
        }),
    )
}

#[derive(Debug, Deserialize, Default)]
struct RunDeployBody {
    #[serde(default)]
    skip_ci: Option<bool>,
    #[serde(default)]
    allow_pr: bool,
    #[serde(default)]
    reason: Option<String>,
}

fn handle_deploy_run(state: &Arc<ApiState>, body: &str, id: &str) -> Response {
    let parsed: RunDeployBody = if body.trim().is_empty() {
        RunDeployBody::default()
    } else {
        match serde_json::from_str(body) {
            Ok(b) => b,
            Err(e) => return error_response(400, &format!("invalid JSON: {e}")),
        }
    };

    let state_dir = state.shared.lock().expect("shared").state_dir.clone();
    let Some(run) = model::read_run(&state_dir, id) else {
        return error_response(404, "unknown run");
    };

    if run.pr_number.is_some() && !parsed.allow_pr {
        return error_response(400, "cannot deploy a pull request build");
    }

    let tx_opt = state
        .commands
        .lock()
        .expect("commands")
        .get(&run.project)
        .cloned();
    let (tx, git_ref, pr, reserved, ws) = {
        let mut shared = state.shared.lock().expect("shared");
        let Some(cfg_p) = shared.config.project(&run.project).cloned() else {
            return error_response(404, "unknown project");
        };
        if cfg_p.deploy.is_empty() {
            return error_response(400, "project has no deploy steps");
        }
        let max_c = cfg_p.effective_max_concurrent_runs();
        let ws = shared.ws.clone();
        let Some(p) = shared.projects.iter_mut().find(|p| p.name == run.project) else {
            return error_response(404, "unknown project");
        };
        if p.active_run_count() >= max_c {
            return error_response(409, "project is already running");
        }
        let Some(tx) = tx_opt else {
            return error_response(503, "project worker is not running");
        };
        let git_ref = run.git_ref.clone().unwrap_or_else(|| cfg_p.branch.clone());
        let pr = run.pr_number.map(|number| PrContext {
            number,
            title: run.pr_title.clone().unwrap_or_default(),
            head_ref: run.pr_head_ref.clone().unwrap_or_default(),
            base_ref: run
                .pr_base_ref
                .clone()
                .unwrap_or_else(|| cfg_p.branch.clone()),
            base_sha: String::new(),
        });
        let Some(reserved) = p.try_reserve_pending_slot(
            max_c,
            Some(run.sha.clone()),
            Some(git_ref.clone()),
            pr.as_ref().map(|p| p.number),
        ) else {
            return error_response(409, "project is already running");
        };
        (tx, git_ref, pr, reserved, ws)
    };

    let deploy_only = parsed.skip_ci.unwrap_or(true);
    let trigger_reason = model::sanitize_trigger_reason(parsed.reason.as_deref())
        .or_else(|| Some(format!("Force deploy of {id}")));
    let slot = reserved.0;
    if tx
        .send(Command::Trigger {
            sha: Some(run.sha.clone()),
            force: true,
            trigger: Trigger::ForceDeploy,
            git_ref: Some(git_ref),
            pr,
            deploy_only,
            force_deploy: true,
            trigger_reason,
            reserved: Some(reserved),
        })
        .is_err()
    {
        release_reserved_slot(state, &run.project, slot);
        return error_response(503, "project worker is not running");
    }
    ws.emit_projects_updated();
    json_response(
        202,
        &json!({
            "queued": true,
            "sha": run.sha,
            "deploy_only": deploy_only,
            "run_id": run.id,
            "project": run.project,
        }),
    )
}

fn run_retry_context(run: &Run) -> (Option<String>, Option<PrContext>) {
    let git_ref = run.git_ref.clone().filter(|r| !r.is_empty());
    let pr = run.pr_number.map(|number| PrContext {
        number,
        title: run.pr_title.clone().unwrap_or_default(),
        head_ref: run
            .pr_head_ref
            .clone()
            .filter(|r| !r.is_empty())
            .unwrap_or_else(|| format!("pull/{number}")),
        base_ref: run.pr_base_ref.clone().unwrap_or_default(),
        base_sha: String::new(),
    });
    (git_ref, pr)
}

#[derive(Debug, Deserialize, Default)]
struct ReasonBody {
    #[serde(default)]
    reason: Option<String>,
}

fn handle_retry_run(state: &Arc<ApiState>, body: &str, id: &str) -> Response {
    let parsed: ReasonBody = if body.trim().is_empty() {
        ReasonBody::default()
    } else {
        match serde_json::from_str(body) {
            Ok(b) => b,
            Err(e) => return error_response(400, &format!("invalid JSON: {e}")),
        }
    };
    let (state_dir, live_run) = {
        let shared = state.shared.lock().expect("shared");
        let live = shared.projects.iter().find_map(|p| {
            if let Some(active) = p.active_runs.iter().find(|a| a.run.id == id) {
                return Some((active.run.clone(), true));
            }
            p.current_run
                .as_ref()
                .filter(|r| r.id == id)
                .map(|r| (r.clone(), p.running))
        });
        (shared.state_dir.clone(), live)
    };
    if live_run.as_ref().is_some_and(|(_, running)| *running) {
        return error_response(409, "project is already running");
    }
    let Some(run) = lookup_run(&state_dir, id).or_else(|| live_run.map(|(r, _)| r)) else {
        return error_response(404, "run not found");
    };

    let tx_opt = state
        .commands
        .lock()
        .expect("commands")
        .get(&run.project)
        .cloned();
    let (git_ref, pr) = run_retry_context(&run);
    let (tx, reserved, ws) = {
        let mut shared = state.shared.lock().expect("shared");
        let Some(cfg_p) = shared.config.project(&run.project) else {
            return error_response(404, "project not found");
        };
        let max_c = cfg_p.effective_max_concurrent_runs();
        let ws = shared.ws.clone();
        let Some(p) = shared.projects.iter_mut().find(|p| p.name == run.project) else {
            return error_response(404, "project not found");
        };
        if p.active_run_count() >= max_c {
            return error_response(409, "project is already running");
        }
        if !git::is_valid_sha(&run.sha) {
            return error_response(400, "invalid commit sha");
        }
        let Some(tx) = tx_opt else {
            return error_response(503, "project worker is not running");
        };
        let Some(reserved) = p.try_reserve_pending_slot(
            max_c,
            Some(run.sha.clone()),
            git_ref.clone(),
            pr.as_ref().map(|p| p.number),
        ) else {
            return error_response(409, "project is already running");
        };
        (tx, reserved, ws)
    };

    let trigger_reason = model::sanitize_trigger_reason(parsed.reason.as_deref())
        .or_else(|| Some(format!("Retry of {id}")));
    let slot = reserved.0;
    if tx
        .send(Command::Trigger {
            sha: Some(run.sha.clone()),
            force: true,
            trigger: Trigger::Manual,
            git_ref,
            pr,
            deploy_only: false,
            force_deploy: false,
            trigger_reason,
            reserved: Some(reserved),
        })
        .is_err()
    {
        release_reserved_slot(state, &run.project, slot);
        return error_response(503, "project worker is not running");
    }
    ws.emit_projects_updated();
    json_response(
        202,
        &json!({
            "queued": true,
            "project": run.project,
            "sha": run.sha,
            "run_id": id,
        }),
    )
}

fn remove_docker_volumes_with_prefix(prefix: &str, wsl: bool) -> Vec<String> {
    let mut removed = Vec::new();
    let mut ls = if wsl {
        let mut c = std::process::Command::new("wsl.exe");
        c.args(["-e", "docker"]);
        c
    } else {
        std::process::Command::new("docker")
    };
    let Ok(out) = ls
        .args(["volume", "ls", "-q", "--filter", &format!("name=^{prefix}")])
        .stdin(std::process::Stdio::null())
        .output()
    else {
        return removed;
    };
    if !out.status.success() {
        return removed;
    }
    let vols: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .filter(|v| v.starts_with(prefix))
        .map(String::from)
        .collect();
    for vol in vols {
        let mut rm = if wsl {
            let mut c = std::process::Command::new("wsl.exe");
            c.args(["-e", "docker"]);
            c
        } else {
            std::process::Command::new("docker")
        };
        if let Ok(res) = rm
            .args(["volume", "rm", "-f", &vol])
            .stdin(std::process::Stdio::null())
            .output()
        {
            if res.status.success() {
                removed.push(format!("docker-volume:{vol}"));
            }
        }
    }
    removed
}

fn remove_path_entry(path: &Path) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return false;
    };
    let res = if meta.file_type().is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    };
    res.is_ok()
}

fn handle_clear_cache(state: &Arc<ApiState>) -> Response {
    let (cache_dir, wsl) = {
        let shared = state.shared.lock().expect("shared");
        let act = shared.config.act();
        (act.effective_cache_dir(&shared.state_dir), act.is_wsl())
    };
    let mut removed = Vec::new();
    if cache_dir.is_dir() {
        if let Ok(entries) = std::fs::read_dir(&cache_dir) {
            let mut paths: Vec<PathBuf> = entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name().and_then(|n| n.to_str()).is_some_and(|name| {
                        name == "act-actions"
                            || name.starts_with("act-actions-slot-")
                            || name == "act-cache-server"
                            || name.starts_with("act-cache-server-slot-")
                            || name == "projects"
                    })
                })
                .collect();
            paths.sort();
            for path in paths {
                if remove_path_entry(&path) {
                    removed.push(path.to_string_lossy().to_string());
                }
            }
        }
    }
    removed.extend(remove_docker_volumes_with_prefix("lunacy-cache-", wsl));
    json_response(200, &json!({"cleared": true, "removed": removed}))
}

fn handle_clear_project_cache(state: &Arc<ApiState>, name: &str) -> Response {
    let (known, cache_dir, wsl) = {
        let shared = state.shared.lock().expect("shared");
        let known = shared.config.project(name).is_some();
        let act = shared.config.act();
        (
            known,
            act.effective_cache_dir(&shared.state_dir),
            act.is_wsl(),
        )
    };
    if !known {
        return error_response(404, "unknown project");
    }
    let slug = config::project_cache_slug(name);
    let mut removed = Vec::new();
    let projects_dir = cache_dir.join("projects");
    if projects_dir.is_dir() {
        let slot_prefix = format!("{slug}-slot-");
        if let Ok(entries) = std::fs::read_dir(&projects_dir) {
            let mut paths: Vec<PathBuf> = entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n == slug || n.starts_with(&slot_prefix))
                })
                .collect();
            paths.sort();
            for path in paths {
                if remove_path_entry(&path) {
                    removed.push(path.to_string_lossy().to_string());
                }
            }
        }
    }
    removed.extend(remove_docker_volumes_with_prefix(
        &format!("lunacy-cache-{slug}-"),
        wsl,
    ));
    json_response(
        200,
        &json!({"cleared": true, "project": name, "removed": removed}),
    )
}

fn handle_cancel_run(state: &Arc<ApiState>, id: &str) -> Response {
    let (matched, state_dir, ws) = {
        let shared = state.shared.lock().expect("shared");
        (
            shared.cancel_run(id),
            shared.state_dir.clone(),
            shared.ws.clone(),
        )
    };
    if let Some((project, run_id)) = matched {
        return json_response(
            202,
            &json!({
                "cancelling": true,
                "cancelled": false,
                "orphaned": false,
                "run_id": run_id,
                "project": project,
            }),
        );
    }
    let Some(run) = lookup_run(&state_dir, id) else {
        return error_response(404, "run not found");
    };
    if !run.status.is_running() {
        return error_response(409, "run is not running");
    }
    // Re-check under the shared lock right before touching disk in case the run became live.
    if let Some((project, run_id)) = state.shared.lock().expect("shared").cancel_run(&run.id) {
        return json_response(
            202,
            &json!({
                "cancelling": true,
                "cancelled": false,
                "orphaned": false,
                "run_id": run_id,
                "project": project,
            }),
        );
    }
    let (updated, appended_logs) = match model::reconcile_orphaned_run_with_logs(
        &state_dir,
        run,
        "cancelled via API (run was orphaned / no longer active)",
    ) {
        Ok(res) => res,
        Err(e) => {
            log::warn!("failed to reconcile orphaned run {id}: {e}");
            return error_response(500, "failed to cancel orphaned run");
        }
    };
    {
        let mut shared = state.shared.lock().expect("shared");
        if let Some(p) = shared
            .projects
            .iter_mut()
            .find(|p| p.name == updated.project)
        {
            if !p.running && p.current_run.as_ref().is_some_and(|r| r.id == updated.id) {
                p.current_run = None;
            }
            if p.last_run.as_ref().is_some_and(|r| r.id == updated.id) {
                p.last_run = Some(updated.clone());
            }
        }
    }
    for appended in appended_logs {
        ws.emit_log_chunk(&updated.id, appended.step, appended.chunk, appended.new_len);
    }
    ws.emit_run_updated(&updated);
    ws.emit_projects_updated();
    json_response(
        202,
        &json!({
            "cancelling": true,
            "cancelled": true,
            "orphaned": true,
            "run_id": updated.id,
            "project": updated.project,
        }),
    )
}

fn finish_reconciled_project_runs(
    state: &Arc<ApiState>,
    ws: &crate::ws::WsHub,
    name: &str,
    reconciled: &[(Run, Vec<model::AppendedLogChunk>)],
) {
    if reconciled.is_empty() {
        return;
    }
    {
        let mut shared = state.shared.lock().expect("shared");
        if let Some(p) = shared.projects.iter_mut().find(|p| p.name == name) {
            if !p.running {
                if let Some(cur) = &p.current_run {
                    if reconciled.iter().any(|(r, _)| r.id == cur.id) {
                        p.current_run = None;
                    }
                }
            }
            if let Some(lr) = &p.last_run {
                if let Some((updated, _)) = reconciled.iter().find(|(r, _)| r.id == lr.id) {
                    p.last_run = Some(updated.clone());
                }
            }
        }
    }
    for (run, appended_logs) in reconciled {
        for appended in appended_logs {
            ws.emit_log_chunk(
                &run.id,
                appended.step,
                appended.chunk.clone(),
                appended.new_len,
            );
        }
        ws.emit_run_updated(run);
    }
    ws.emit_projects_updated();
}

fn handle_cancel_project(state: &Arc<ApiState>, name: &str) -> Response {
    let (outcome, state_dir, last_run, ws) = {
        let shared = state.shared.lock().expect("shared");
        let outcome = shared.cancel_project(name);
        let last_run = shared
            .projects
            .iter()
            .find(|p| p.name == name)
            .and_then(|p| p.last_run.clone());
        (
            outcome,
            shared.state_dir.clone(),
            last_run,
            shared.ws.clone(),
        )
    };
    match outcome {
        crate::daemon::CancelProjectOutcome::Cancelling(run_id) => {
            if let Some(tx) = state.commands.lock().expect("commands").get(name).cloned() {
                let _ = tx.send(Command::CancelQueued);
            }
            json_response(
                202,
                &json!({
                    "cancelling": true,
                    "cancelled": false,
                    "orphaned": false,
                    "run_id": run_id,
                    "project": name,
                }),
            )
        }
        crate::daemon::CancelProjectOutcome::NotFound => error_response(404, "project not found"),
        crate::daemon::CancelProjectOutcome::NotRunning => {
            let mut candidates: Vec<Run> = model::list_runs(&state_dir, name, usize::MAX)
                .into_iter()
                .filter(|r| r.status.is_running())
                .collect();
            if let Some(lr) = last_run {
                if lr.status.is_running() && !candidates.iter().any(|r| r.id == lr.id) {
                    candidates.push(lr);
                }
            }
            if candidates.is_empty() {
                return error_response(409, "project is not running");
            }
            let mut reconciled: Vec<(Run, Vec<model::AppendedLogChunk>)> = Vec::new();
            for run in candidates {
                // Re-check under the shared lock right before reconciling each run on disk.
                let live_match = {
                    let shared = state.shared.lock().expect("shared");
                    shared
                        .projects
                        .iter()
                        .find(|p| p.name == name)
                        .and_then(|p| {
                            if let Some(active) = p.active_runs.iter().find(|a| a.run.id == run.id)
                            {
                                active
                                    .cancel
                                    .store(true, std::sync::atomic::Ordering::SeqCst);
                                Some(run.id.clone())
                            } else if p.running
                                && p.current_run.as_ref().is_some_and(|r| r.id == run.id)
                            {
                                if let Some(flag) = &p.cancel {
                                    flag.store(true, std::sync::atomic::Ordering::SeqCst);
                                }
                                Some(run.id.clone())
                            } else {
                                None
                            }
                        })
                };
                if let Some(live_run_id) = live_match {
                    finish_reconciled_project_runs(state, &ws, name, &reconciled);
                    return json_response(
                        202,
                        &json!({
                            "cancelling": true,
                            "cancelled": false,
                            "orphaned": false,
                            "run_id": live_run_id,
                            "project": name,
                        }),
                    );
                }
                match model::reconcile_orphaned_run_with_logs(
                    &state_dir,
                    run,
                    "cancelled via API (run was orphaned / no longer active)",
                ) {
                    Ok(res) => reconciled.push(res),
                    Err(e) => {
                        log::warn!("[{name}] failed to reconcile orphaned run: {e}");
                    }
                }
            }
            if reconciled.is_empty() {
                return error_response(500, "failed to cancel orphaned run");
            }
            let first_id = reconciled[0].0.id.clone();
            finish_reconciled_project_runs(state, &ws, name, &reconciled);
            json_response(
                202,
                &json!({
                    "cancelling": true,
                    "cancelled": true,
                    "orphaned": true,
                    "run_id": first_id,
                    "project": name,
                }),
            )
        }
    }
}

#[derive(Debug, Deserialize, Default)]
struct DiscordTestBody {
    #[serde(default)]
    project: Option<String>,
}

struct DiscordTestPermit<'a>(&'a Mutex<DiscordTestGuard>);

impl Drop for DiscordTestPermit<'_> {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.0.lock() {
            guard.in_flight = false;
            guard.last_sent = Some(std::time::Instant::now());
        }
    }
}

fn handle_discord_test(state: &Arc<ApiState>, body: &str) -> Response {
    let parsed: DiscordTestBody = if body.trim().is_empty() {
        DiscordTestBody::default()
    } else {
        match serde_json::from_str(body) {
            Ok(b) => b,
            Err(e) => return error_response(400, &format!("invalid JSON: {e}")),
        }
    };

    let cfg = state.shared.lock().expect("shared").config.clone();
    let project_name = parsed
        .project
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let resolved = match project_name {
        Some(name) => {
            let Some(proj) = cfg.project(name) else {
                return error_response(404, "project not found");
            };
            cfg.discord_for(proj)
        }
        None => cfg.discord_global(),
    };
    let Some(resolved) = resolved else {
        return error_response(400, "discord webhook is disabled or not configured");
    };
    let discord = match crate::discord::Discord::from_resolved(&resolved) {
        Ok(d) => d,
        Err(e) => return error_response(500, &format!("creating discord client: {e}")),
    };
    {
        let mut guard = state.discord_test_guard.lock().expect("discord_test_guard");
        if guard.in_flight
            || guard
                .last_sent
                .is_some_and(|t| t.elapsed() < DISCORD_TEST_COOLDOWN)
        {
            return error_response(429, "discord test already in flight; please wait a moment");
        }
        guard.in_flight = true;
    }
    let _permit = DiscordTestPermit(&state.discord_test_guard);
    match discord.send_test(project_name) {
        Ok(()) => json_response(200, &json!({ "ok": true, "source": resolved.source })),
        Err(e) => error_response(502, &format!("{e}")),
    }
}

fn handle_deploy_checks(state: &Arc<ApiState>) -> Response {
    let (cfg, snapshots, state_dir) = {
        let shared = state.shared.lock().expect("shared");
        let snaps: Vec<crate::deploy_check::ProjectSnapshot> =
            shared.projects.iter().map(Into::into).collect();
        (shared.config.clone(), snaps, shared.state_dir.clone())
    };
    let report = crate::deploy_check::build_deploy_checks(
        &cfg,
        &snapshots,
        &state_dir,
        crate::config::Os::current(),
    );
    match serde_json::to_value(report) {
        Ok(body) => json_response(200, &body),
        Err(e) => error_response(500, &format!("serialising deploy checks: {e}")),
    }
}

// ------------------------------------------------------------ workflows

fn handle_workflows(
    state: &Arc<ApiState>,
    name: &str,
    params: &BTreeMap<String, String>,
) -> Response {
    let repo_path = match params.get("repo_path") {
        Some(p) if !p.is_empty() => PathBuf::from(p),
        _ => {
            let shared = state.shared.lock().expect("shared");
            match shared.config.project(name) {
                Some(p) => p.repo_path.clone(),
                None => return error_response(404, "unknown project"),
            }
        }
    };
    let dir = repo_path.join(".github").join("workflows");
    let mut out: Vec<Value> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        let mut files: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.extension()
                    .map(|e| e == "yml" || e == "yaml")
                    .unwrap_or(false)
            })
            .collect();
        files.sort();
        for path in files {
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(doc) = crate::act::parse_workflow(&text) else {
                continue;
            };
            let rel = path
                .strip_prefix(&repo_path)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            out.push(json!({
                "file": rel,
                "name": doc.name.unwrap_or_else(|| {
                    path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default()
                }),
                "jobs": doc.jobs.iter().map(|(id, job)| json!({
                    "id": id,
                    "name": job.name.clone().unwrap_or_else(|| id.clone()),
                    "runs_on": job.runs_on_labels(),
                })).collect::<Vec<_>>(),
            }));
        }
    }
    json_response(200, &Value::Array(out))
}

// --------------------------------------------------------------- config

fn handle_put_config(state: &Arc<ApiState>, body: &str) -> Response {
    let old = state.shared.lock().expect("shared").config.clone();
    let mut incoming: Config = match serde_json::from_str(body) {
        Ok(c) => c,
        Err(e) => {
            return json_response(
                400,
                &json!({"error": format!("invalid config: {e}"), "field": Value::Null}),
            );
        }
    };
    incoming.unmask_from(&old);

    if let Err(e) = incoming.validate() {
        return json_response(400, &json!({"error": e.message, "field": e.field}));
    }

    let path = &state.config_path;
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    let text = match crate::configfile::render(&existing, &incoming) {
        Ok(t) => t,
        Err(e) => {
            return json_response(
                400,
                &json!({"error": format!("cannot serialise the config: {e:#}"), "field": Value::Null}),
            );
        }
    };

    if let Err(e) = crate::configfile::write_atomic(path, &text) {
        return json_response(
            500,
            &json!({"error": format!("writing {}: {e:#}", path.display()), "field": Value::Null}),
        );
    }

    // Reload: idle workers pick it up on their next turn; a running pipeline
    // keeps the config it started with.
    let (restarted, ws) = {
        let mut shared = state.shared.lock().expect("shared");
        shared
            .concurrency_gate
            .set_limit(incoming.general.max_concurrent_runs);
        shared.config = incoming.clone();
        // Refresh the read models for added/removed/reordered projects.
        let state_dir = shared.state_dir.clone();
        let mut old_projects = std::mem::take(&mut shared.projects);
        let mut ordered = Vec::with_capacity(incoming.projects.len());
        for project in &incoming.projects {
            if let Some(pos) = old_projects.iter().position(|p| p.name == project.name) {
                let mut existing = old_projects.remove(pos);
                existing.branch = project.branch.clone();
                existing.github_repo = crate::daemon::resolve_github_repo(project);
                if !project.watch_prs {
                    existing.open_prs.clear();
                }
                ordered.push(existing);
            } else {
                ordered.push(crate::daemon::ProjectState::new_public(project, &state_dir));
            }
        }
        shared.projects = ordered;
        (shared.server_restart_needed(), shared.ws.clone())
    };

    let commands: Vec<(String, Sender<Command>)> = state
        .commands
        .lock()
        .expect("commands")
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    for (name, tx) in commands {
        if incoming.project(&name).is_some() {
            let _ = tx.send(Command::Reload);
        }
    }
    if let Ok(mut n) = state.reloads.lock() {
        *n += 1;
    }

    ws.emit_config_updated();
    ws.emit_projects_updated();

    let mut out = match serde_json::to_value(incoming.masked()) {
        Ok(v) => v,
        Err(e) => return error_response(500, &format!("serialising config: {e}")),
    };
    if restarted {
        out["restart_required"] = json!(["server.listen"]);
    }
    json_response(200, &out)
}

// -------------------------------------------------------------- webhook

#[derive(Debug, Deserialize)]
struct WebhookPayload {
    #[serde(default)]
    action: Option<String>,
    #[serde(default)]
    zen: Option<String>,
    #[serde(default)]
    repository: Option<WebhookRepo>,
    #[serde(default)]
    check_run: Option<WebhookCheckRun>,
    #[serde(default)]
    check_suite: Option<WebhookCheckSuite>,
    #[serde(default)]
    requested_action: Option<WebhookRequestedAction>,
    #[serde(default)]
    pull_request: Option<WebhookPullRequest>,
    #[serde(default, rename = "ref")]
    git_ref: Option<String>,
    #[serde(default)]
    after: Option<String>,
    #[serde(default)]
    deleted: Option<bool>,
    #[serde(default)]
    sender: Option<WebhookUser>,
    #[serde(default)]
    pusher: Option<WebhookPusher>,
}

#[derive(Debug, Deserialize)]
struct WebhookUser {
    #[serde(default)]
    login: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WebhookPusher {
    #[serde(default)]
    name: Option<String>,
}

fn webhook_actor(payload: &WebhookPayload) -> Option<String> {
    payload
        .sender
        .as_ref()
        .and_then(|u| model::sanitize_trigger_reason(u.login.as_deref()))
        .or_else(|| {
            payload
                .pusher
                .as_ref()
                .and_then(|p| model::sanitize_trigger_reason(p.name.as_deref()))
        })
}

#[derive(Debug, Deserialize)]
struct WebhookRequestedAction {
    #[serde(default)]
    identifier: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WebhookRepo {
    #[serde(default)]
    full_name: Option<String>,
    #[serde(default)]
    fork: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct WebhookCheckRun {
    #[serde(default)]
    head_sha: Option<String>,
    #[serde(default)]
    external_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WebhookCheckSuite {
    #[serde(default)]
    head_sha: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WebhookPullRequest {
    #[serde(default)]
    number: Option<u64>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    head: Option<WebhookPrRef>,
    #[serde(default)]
    base: Option<WebhookPrRef>,
}

#[derive(Debug, Deserialize)]
struct WebhookPrRef {
    #[serde(default)]
    sha: Option<String>,
    #[serde(default, rename = "ref")]
    git_ref: Option<String>,
    #[serde(default)]
    repo: Option<WebhookRepo>,
}

/// Verify `X-Hub-Signature-256` = `sha256=<hex hmac>` over the raw body.
pub fn verify_signature(secret: &str, body: &[u8], header: &str) -> bool {
    let Some(hex) = header.strip_prefix("sha256=") else {
        return false;
    };
    let Ok(expected) = hex_decode(hex) else {
        return false;
    };
    let mut mac = match Hmac::<Sha256>::new_from_slice(secret.as_bytes()) {
        Ok(m) => m,
        Err(_) => return false,
    };
    mac.update(body);
    mac.verify_slice(&expected).is_ok()
}

fn hex_decode(s: &str) -> Result<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        anyhow::bail!("odd-length hex");
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    let bytes = s.as_bytes();
    for chunk in bytes.chunks(2) {
        let pair = std::str::from_utf8(chunk)?;
        out.push(u8::from_str_radix(pair, 16)?);
    }
    Ok(out)
}

/// Compute the `X-Hub-Signature-256` header value for a body (tests and docs).
pub fn signature_header(secret: &str, body: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("hmac key");
    mac.update(body);
    let bytes = mac.finalize().into_bytes();
    format!(
        "sha256={}",
        bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
    )
}

fn project_github_repo(p: &crate::daemon::ProjectState, cfg: &Config) -> Option<String> {
    p.github_repo.clone().or_else(|| {
        cfg.project(&p.name)
            .and_then(crate::daemon::resolve_github_repo)
    })
}

fn record_webhook(
    state: &ApiState,
    event: &str,
    repo: Option<String>,
    delivery_id: Option<String>,
    outcome: &str,
    queued: bool,
) {
    let received_at = model::now_iso();
    let ws = {
        let mut shared = state.shared.lock().expect("shared");
        shared.webhook_stats.deliveries = shared.webhook_stats.deliveries.saturating_add(1);
        shared.webhook_stats.last_event = Some(event.to_string());
        shared.webhook_stats.last_repo = repo.clone();
        shared.webhook_stats.last_delivery_id = delivery_id.clone();
        shared.webhook_stats.last_received_at = Some(received_at.clone());
        shared.webhook_stats.last_outcome = Some(outcome.to_string());
        shared.ws.clone()
    };
    ws.emit_webhook_received(event, repo, delivery_id, outcome, received_at, queued);
}

#[allow(clippy::too_many_lines)]
fn handle_webhook(state: &Arc<ApiState>, headers: &HeaderMap, body_bytes: &[u8]) -> Response {
    let secret = state
        .shared
        .lock()
        .expect("shared")
        .config
        .github
        .as_ref()
        .and_then(|g| g.webhook_secret_value())
        .filter(|s| !s.is_empty() && s != MASK);
    let Some(secret) = secret else {
        return error_response(404, "no webhook secret configured");
    };

    let sig_header = headers
        .get("x-hub-signature-256")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let event_header = headers
        .get("x-github-event")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().to_ascii_lowercase());
    let delivery_id = headers
        .get("x-github-delivery")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    if !verify_signature(&secret, body_bytes, sig_header) {
        return error_response(401, "invalid signature");
    }

    let body = String::from_utf8_lossy(body_bytes);
    let payload: WebhookPayload = match serde_json::from_str(&body) {
        Ok(p) => p,
        Err(e) => return error_response(400, &format!("invalid JSON: {e}")),
    };

    let event_kind = match event_header.as_deref() {
        Some(ev) if !ev.is_empty() => ev,
        _ if payload.pull_request.is_some() => "pull_request",
        _ if payload.git_ref.is_some() && payload.after.is_some() => "push",
        _ => "check_run",
    };

    let actor = webhook_actor(&payload);
    let full_name = payload.repository.and_then(|r| r.full_name);
    let commands = state.commands.lock().expect("commands").clone();

    match event_kind {
        "ping" => {
            let zen = payload.zen.unwrap_or_default();
            record_webhook(state, "ping", full_name, delivery_id, "pong", false);
            json_response(200, &json!({"ok": true, "pong": true, "zen": zen}))
        }
        "pull_request" => {
            let action = payload.action.as_deref().unwrap_or("");
            if !matches!(action, "opened" | "synchronize" | "reopened") {
                record_webhook(
                    state,
                    event_kind,
                    full_name,
                    delivery_id,
                    "ignored action",
                    false,
                );
                return json_response(202, &json!({"queued": false, "reason": "ignored action"}));
            }
            let Some(pr) = payload.pull_request else {
                record_webhook(
                    state,
                    event_kind,
                    full_name,
                    delivery_id,
                    "missing pull_request",
                    false,
                );
                return json_response(
                    202,
                    &json!({"queued": false, "reason": "missing pull_request"}),
                );
            };
            let Some(number) = pr.number else {
                record_webhook(
                    state,
                    event_kind,
                    full_name,
                    delivery_id,
                    "missing pull_request.number",
                    false,
                );
                return json_response(
                    202,
                    &json!({"queued": false, "reason": "missing pull_request.number"}),
                );
            };
            let head = pr.head.unwrap_or(WebhookPrRef {
                sha: None,
                git_ref: None,
                repo: None,
            });
            let base = pr.base.unwrap_or(WebhookPrRef {
                sha: None,
                git_ref: None,
                repo: None,
            });
            let is_same_repo = match head.repo.as_ref() {
                Some(hr) if hr.fork != Some(true) => {
                    match hr.full_name.as_deref().filter(|s| !s.is_empty()) {
                        Some(head_name) => {
                            let base_name = base
                                .repo
                                .as_ref()
                                .and_then(|r| r.full_name.as_deref())
                                .filter(|s| !s.is_empty());
                            let repo_name = full_name.as_deref().filter(|s| !s.is_empty());
                            let matches_base = base_name
                                .map(|b| head_name.eq_ignore_ascii_case(b))
                                .unwrap_or(true);
                            let matches_repo = repo_name
                                .map(|r| head_name.eq_ignore_ascii_case(r))
                                .unwrap_or(true);
                            (base_name.is_some() || repo_name.is_some())
                                && matches_base
                                && matches_repo
                        }
                        None => false,
                    }
                }
                _ => false,
            };
            if !is_same_repo {
                record_webhook(
                    state,
                    event_kind,
                    full_name,
                    delivery_id,
                    "fork pull request",
                    false,
                );
                return json_response(
                    200,
                    &json!({"ignored": true, "reason": "fork pull request"}),
                );
            }
            let Some(head_sha) = head.sha.filter(|s| !s.is_empty()) else {
                record_webhook(
                    state,
                    event_kind,
                    full_name,
                    delivery_id,
                    "no head_sha",
                    false,
                );
                return json_response(202, &json!({"queued": false, "reason": "no head_sha"}));
            };
            if !git::is_valid_sha(&head_sha) {
                return error_response(400, "invalid head_sha");
            }
            let head_ref = head
                .git_ref
                .filter(|r| !r.is_empty())
                .unwrap_or_else(|| format!("pull/{number}"));
            if !git::is_valid_git_ref(&head_ref) {
                return error_response(400, "invalid head ref");
            }
            let base_ref = base.git_ref.unwrap_or_default();
            if !base_ref.is_empty() && !git::is_valid_git_ref(&base_ref) {
                return error_response(400, "invalid base ref");
            }
            let base_sha = base.sha.unwrap_or_default();
            if !base_sha.is_empty() && !git::is_valid_sha(&base_sha) {
                return error_response(400, "invalid base_sha");
            }
            let title = pr.title.unwrap_or_default();

            let mut filtered_by_paused = false;
            let mut filtered_by_poll = false;
            let mut prs_changed = None;
            let targets: Vec<(String, Sender<Command>)> = {
                let mut guard = state.shared.lock().expect("shared");
                let shared = &mut *guard;
                let mut out = Vec::new();
                for p in shared.projects.iter_mut() {
                    let have = project_github_repo(p, &shared.config);
                    let repo_match = match (&full_name, &have) {
                        (Some(want), Some(have)) => want.eq_ignore_ascii_case(have),
                        _ => false,
                    };
                    if !repo_match {
                        continue;
                    }
                    let Some(cfg_p) = shared.config.project(&p.name) else {
                        continue;
                    };
                    if !cfg_p.watch_prs {
                        continue;
                    }
                    if !cfg_p.is_enabled() {
                        filtered_by_paused = true;
                        continue;
                    }
                    if cfg_p.effective_trigger_mode(&shared.config.general) == TriggerMode::Poll {
                        filtered_by_poll = true;
                        continue;
                    }
                    // Track the PR's new head now, not at the next poll: a finished run
                    // records its sha only when it matches the tracked head, else the next
                    // poll would build this commit a second time.
                    if upsert_open_pr(
                        &mut p.open_prs,
                        number,
                        &title,
                        &head_ref,
                        &base_ref,
                        &head_sha,
                    ) {
                        prs_changed = Some(shared.ws.clone());
                    }
                    if let Some(tx) = commands.get(&p.name) {
                        out.push((p.name.clone(), tx.clone()));
                    }
                }
                out
            };
            if let Some(ws) = prs_changed {
                ws.emit_projects_updated();
            }
            if targets.is_empty() {
                let reason = if filtered_by_paused {
                    "project is paused"
                } else if filtered_by_poll {
                    "poll-only mode"
                } else {
                    "no matching project"
                };
                record_webhook(state, event_kind, full_name, delivery_id, reason, false);
                return json_response(202, &json!({"queued": false, "reason": reason}));
            }
            let pr_ctx = PrContext {
                number,
                title,
                head_ref: head_ref.clone(),
                base_ref,
                base_sha,
            };
            let trigger_reason = model::sanitize_trigger_reason(Some(&match actor.as_deref() {
                Some(who) => format!("GitHub PR #{number} {action} by {who}"),
                None => format!("GitHub PR #{number} {action}"),
            }));
            let mut queued = Vec::new();
            for (name, tx) in targets {
                let _ = tx.send(Command::Trigger {
                    sha: Some(head_sha.clone()),
                    force: true,
                    trigger: Trigger::PullRequest,
                    git_ref: Some(head_ref.clone()),
                    pr: Some(pr_ctx.clone()),
                    deploy_only: false,
                    force_deploy: false,
                    trigger_reason: trigger_reason.clone(),
                    reserved: None,
                });
                queued.push(name);
            }
            let outcome = format!("queued PR #{number} for {}", queued.join(", "));
            record_webhook(state, event_kind, full_name, delivery_id, &outcome, true);
            json_response(
                202,
                &json!({"queued": true, "projects": queued, "pr_number": number}),
            )
        }
        "push" => {
            if payload.deleted == Some(true) {
                record_webhook(
                    state,
                    event_kind,
                    full_name,
                    delivery_id,
                    "branch or tag deleted",
                    false,
                );
                return json_response(
                    202,
                    &json!({"queued": false, "reason": "branch or tag deleted"}),
                );
            }
            let Some(raw_ref) = payload.git_ref.filter(|r| !r.is_empty()) else {
                record_webhook(state, event_kind, full_name, delivery_id, "no ref", false);
                return json_response(202, &json!({"queued": false, "reason": "no ref"}));
            };
            if !git::is_valid_git_ref(&raw_ref) {
                return error_response(400, "invalid git ref");
            }
            let Some(after) = payload
                .after
                .filter(|s| !s.is_empty() && s != crate::act::ZERO_SHA)
            else {
                record_webhook(
                    state,
                    event_kind,
                    full_name,
                    delivery_id,
                    "no after sha",
                    false,
                );
                return json_response(202, &json!({"queued": false, "reason": "no after sha"}));
            };
            if !git::is_valid_sha(&after) {
                return error_response(400, "invalid after sha");
            }
            let is_tag = raw_ref.starts_with("refs/tags/") || raw_ref.starts_with("tags/");
            let norm_ref = if let Some(tag) = raw_ref.strip_prefix("refs/tags/") {
                format!("tags/{tag}")
            } else if let Some(branch) = raw_ref.strip_prefix("refs/heads/") {
                branch.to_string()
            } else {
                raw_ref.clone()
            };
            let trigger = if is_tag {
                Trigger::Tag
            } else {
                Trigger::Webhook
            };

            let mut filtered_by_paused = false;
            let mut filtered_by_poll = false;
            let targets: Vec<(String, Sender<Command>)> = {
                let shared = state.shared.lock().expect("shared");
                let mut out = Vec::new();
                for p in &shared.projects {
                    let have = project_github_repo(p, &shared.config);
                    let repo_match = match (&full_name, &have) {
                        (Some(want), Some(have)) => want.eq_ignore_ascii_case(have),
                        _ => false,
                    };
                    if !repo_match {
                        continue;
                    }
                    let Some(cfg_p) = shared.config.project(&p.name) else {
                        continue;
                    };
                    let ref_match = raw_ref == format!("refs/heads/{}", cfg_p.branch)
                        || config::matches_deploy_ref(cfg_p, &raw_ref, false)
                        || (is_tag && cfg_p.watch_tags);
                    if !ref_match {
                        continue;
                    }
                    if !cfg_p.is_enabled() {
                        filtered_by_paused = true;
                        continue;
                    }
                    if cfg_p.effective_trigger_mode(&shared.config.general) == TriggerMode::Poll {
                        filtered_by_poll = true;
                        continue;
                    }
                    if let Some(tx) = commands.get(&p.name) {
                        out.push((p.name.clone(), tx.clone()));
                    }
                }
                out
            };
            if targets.is_empty() {
                let reason = if filtered_by_paused {
                    "project is paused"
                } else if filtered_by_poll {
                    "poll-only mode"
                } else {
                    "no matching project"
                };
                record_webhook(state, event_kind, full_name, delivery_id, reason, false);
                return json_response(202, &json!({"queued": false, "reason": reason}));
            }
            let trigger_reason = model::sanitize_trigger_reason(Some(&if is_tag {
                let tag = norm_ref.strip_prefix("tags/").unwrap_or(&norm_ref);
                match actor.as_deref() {
                    Some(who) => format!("GitHub tag webhook ({tag} by {who})"),
                    None => format!("GitHub tag webhook ({tag})"),
                }
            } else {
                match actor.as_deref() {
                    Some(who) => format!("GitHub push webhook ({norm_ref} by {who})"),
                    None => format!("GitHub push webhook ({norm_ref})"),
                }
            }));
            let mut queued = Vec::new();
            for (name, tx) in targets {
                let _ = tx.send(Command::Trigger {
                    sha: Some(after.clone()),
                    force: false,
                    trigger,
                    git_ref: Some(norm_ref.clone()),
                    pr: None,
                    deploy_only: false,
                    force_deploy: false,
                    trigger_reason: trigger_reason.clone(),
                    reserved: None,
                });
                queued.push(name);
            }
            let outcome = format!("queued {norm_ref} for {}", queued.join(", "));
            record_webhook(state, event_kind, full_name, delivery_id, &outcome, true);
            json_response(
                202,
                &json!({"queued": true, "projects": queued, "ref": norm_ref}),
            )
        }
        "check_run" | "check_suite" => {
            let (deploy_only, force_deploy, action_label, reason_base) =
                match payload.action.as_deref() {
                    Some("rerequested") => (false, false, "rerun", "GitHub check re-run"),
                    Some("requested_action") => {
                        match payload
                            .requested_action
                            .as_ref()
                            .and_then(|a| a.identifier.as_deref())
                        {
                            Some("retry_ci" | "retry") => {
                                (false, false, "retry_ci", "GitHub action: Retry CI")
                            }
                            Some("retry_deploy") => {
                                (true, true, "retry_deploy", "GitHub action: Retry Deploy")
                            }
                            _ => {
                                record_webhook(
                                    state,
                                    event_kind,
                                    full_name,
                                    delivery_id,
                                    "ignored requested_action",
                                    false,
                                );
                                return json_response(
                                    202,
                                    &json!({"queued": false, "reason": "ignored requested_action"}),
                                );
                            }
                        }
                    }
                    _ => {
                        record_webhook(
                            state,
                            event_kind,
                            full_name,
                            delivery_id,
                            "ignored action",
                            false,
                        );
                        return json_response(
                            202,
                            &json!({"queued": false, "reason": "ignored action"}),
                        );
                    }
                };
            let external_id = payload
                .check_run
                .as_ref()
                .and_then(|c| c.external_id.clone())
                .filter(|s| !s.is_empty());
            let sha = payload
                .check_run
                .and_then(|c| c.head_sha)
                .or_else(|| payload.check_suite.and_then(|c| c.head_sha));
            let Some(sha) = sha else {
                record_webhook(
                    state,
                    event_kind,
                    full_name,
                    delivery_id,
                    "no head_sha",
                    false,
                );
                return json_response(202, &json!({"queued": false, "reason": "no head_sha"}));
            };
            if !git::is_valid_sha(&sha) {
                return error_response(400, "invalid head_sha");
            }
            let mut filtered_by_paused = false;
            let (state_dir, targets): (PathBuf, Vec<(String, Sender<Command>)>) = {
                let shared = state.shared.lock().expect("shared");
                let mut out = Vec::new();
                for p in &shared.projects {
                    let have = project_github_repo(p, &shared.config);
                    let repo_match = match (&full_name, &have) {
                        (Some(want), Some(have)) => want.eq_ignore_ascii_case(have),
                        _ => false,
                    };
                    if !repo_match {
                        continue;
                    }
                    if let Some(cfg_p) = shared.config.project(&p.name) {
                        if !cfg_p.is_enabled() {
                            filtered_by_paused = true;
                            continue;
                        }
                    }
                    if let Some(tx) = commands.get(&p.name) {
                        out.push((p.name.clone(), tx.clone()));
                    }
                }
                (shared.state_dir.clone(), out)
            };
            if targets.is_empty() {
                let reason = if filtered_by_paused {
                    "project is paused"
                } else {
                    "no matching project"
                };
                record_webhook(state, event_kind, full_name, delivery_id, reason, false);
                return json_response(202, &json!({"queued": false, "reason": reason}));
            }
            let orig_run = external_id
                .as_deref()
                .filter(|id| is_valid_segment(id))
                .and_then(|id| lookup_run(&state_dir, id));
            let trigger_reason = model::sanitize_trigger_reason(Some(&match actor.as_deref() {
                Some(who) => format!("{reason_base} by {who}"),
                None => reason_base.to_string(),
            }));
            let mut queued = Vec::new();
            let mut blocked_pr_deploy = false;
            for (name, tx) in targets {
                let (git_ref, pr) = match orig_run.as_ref().filter(|orig| orig.project == name) {
                    Some(orig) => run_retry_context(orig),
                    None => (None, None),
                };
                if deploy_only && pr.is_some() {
                    blocked_pr_deploy = true;
                    continue;
                }
                let _ = tx.send(Command::Trigger {
                    sha: Some(sha.clone()),
                    force: true,
                    trigger: Trigger::Webhook,
                    git_ref,
                    pr,
                    deploy_only,
                    force_deploy,
                    trigger_reason: trigger_reason.clone(),
                    reserved: None,
                });
                queued.push(name);
            }
            if queued.is_empty() && blocked_pr_deploy {
                record_webhook(
                    state,
                    event_kind,
                    full_name,
                    delivery_id,
                    "pull requests cannot deploy",
                    false,
                );
                return json_response(
                    202,
                    &json!({"queued": false, "reason": "pull requests cannot deploy"}),
                );
            }
            let outcome = format!("queued {action_label} for {}", queued.join(", "));
            record_webhook(state, event_kind, full_name, delivery_id, &outcome, true);
            json_response(202, &json!({"queued": true, "projects": queued}))
        }
        _ => {
            record_webhook(
                state,
                event_kind,
                full_name,
                delivery_id,
                "ignored event",
                false,
            );
            json_response(202, &json!({"queued": false, "reason": "ignored event"}))
        }
    }
}

// -------------------------------------------------------------- system

fn probe_docker_daemon() -> (bool, Option<String>) {
    let mut child = match std::process::Command::new("docker")
        .args(["info", "--format", "{{.ServerVersion}}"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return (false, Some(e.to_string())),
    };
    let stderr = child.stderr.take();
    let reader = std::thread::spawn(move || {
        let mut err = String::new();
        if let Some(mut s) = stderr {
            let _ = std::io::Read::read_to_string(&mut s, &mut err);
        }
        err
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let err = reader.join().unwrap_or_default();
                if status.success() {
                    return (true, None);
                }
                return (false, Some(err.trim().to_string()));
            }
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = reader.join();
                    return (false, Some("docker info timed out".to_string()));
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                return (false, Some(e.to_string()));
            }
        }
    }
}

fn system_json(state: &Arc<ApiState>) -> Value {
    let (
        config_path,
        state_dir,
        webhook_configured,
        webhook_endpoint,
        webhook_stats,
        max_concurrent_runs,
        act_max_concurrent,
    ) = {
        let shared = state.shared.lock().expect("shared");
        let configured = shared
            .config
            .github
            .as_ref()
            .and_then(|g| g.webhook_secret_value())
            .filter(|s| !s.is_empty() && s != MASK)
            .is_some();
        let endpoint = match shared
            .config
            .server
            .as_ref()
            .and_then(|s| s.public_url.as_deref())
            .map(str::trim)
            .filter(|u| !u.is_empty())
        {
            Some(base) => format!("{}/api/github/webhook", base.trim_end_matches('/')),
            None => "/api/github/webhook".to_string(),
        };
        (
            state.config_path.clone(),
            shared.state_dir.clone(),
            configured,
            endpoint,
            shared.webhook_stats.clone(),
            shared.config.general.max_concurrent_runs,
            shared.config.effective_act_max_concurrent(),
        )
    };
    let act_path = crate::exec::which("act");
    let act_version = act_path
        .as_ref()
        .and_then(|_| crate::exec::first_line_of_stdout("act", &["--version"]))
        .map(|v| v.trim_start_matches("act version ").to_string());
    let (docker_ok, docker_error) = probe_docker_daemon();
    let git_version = crate::exec::first_line_of_stdout("git", &["--version"]);
    json!({
        "os": crate::config::Os::current().as_str(),
        "version": VERSION,
        "config_path": config_path.to_string_lossy(),
        "state_dir": state_dir.to_string_lossy(),
        "max_concurrent_runs": max_concurrent_runs,
        "act_max_concurrent": act_max_concurrent,
        "act": {
            "path": act_path.map(|p| p.to_string_lossy().to_string()),
            "version": act_version,
        },
        "docker": { "ok": docker_ok, "error": docker_error },
        "git": { "version": git_version },
        "webhook": {
            "configured": webhook_configured,
            "endpoint": webhook_endpoint,
            "deliveries": webhook_stats.deliveries,
            "last_event": webhook_stats.last_event,
            "last_repo": webhook_stats.last_repo,
            "last_delivery_id": webhook_stats.last_delivery_id,
            "last_received_at": webhook_stats.last_received_at,
            "last_outcome": webhook_stats.last_outcome,
        },
    })
}

// --------------------------------------------------------------- static

fn serve_static(state: &Arc<ApiState>, path: &str) -> Response {
    let root = state
        .shared
        .lock()
        .expect("shared")
        .config
        .server
        .as_ref()
        .and_then(|s| s.web_root.clone());
    let Some(root) = root else {
        return error_response(404, "no web_root configured");
    };
    let rel = path.trim_start_matches('/');
    let rel = if rel.is_empty() { "index.html" } else { rel };
    if rel.contains('\\')
        || !std::path::Path::new(rel)
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)))
    {
        return error_response(400, "bad path");
    }
    let mut file = root.join(rel);
    if !file.is_file() {
        // SPA fallback.
        file = root.join("index.html");
        if !file.is_file() {
            return error_response(404, "not found");
        }
    }
    let ext = file
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase());
    let mime = match ext.as_deref() {
        Some("html") => "text/html; charset=utf-8",
        Some("js") | Some("mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json",
        Some("webmanifest") => "application/manifest+json",
        Some("wasm") => "application/wasm",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        Some("ttf") => "font/ttf",
        _ => "application/octet-stream",
    };
    let Ok(body) = std::fs::read(&file) else {
        return error_response(500, "failed reading static file");
    };
    let mut res = (StatusCode::OK, body).into_response();
    let headers = res.headers_mut();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static(mime));
    if ext.as_deref() == Some("html") {
        headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    }
    headers.insert(ACCESS_CONTROL_ALLOW_ORIGIN, HeaderValue::from_static("*"));
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_verification() {
        let secret = "s3cret";
        let body = br#"{"action":"rerequested"}"#;
        let good = signature_header(secret, body);
        assert!(verify_signature(secret, body, &good));
        assert!(!verify_signature("other", body, &good));
        assert!(!verify_signature(secret, b"other body", &good));
        assert!(!verify_signature(secret, body, "sha256=zz"));
        assert!(!verify_signature(secret, body, ""));
        assert!(!verify_signature(
            secret,
            body,
            &good.replace("sha256=", "")
        ));
    }

    #[test]
    fn constant_time_compare() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
    }

    #[test]
    fn path_query_parsing() {
        let (path, params) = split_path("/api/projects/x/runs?limit=5&tail=10");
        assert_eq!(path, "/api/projects/x/runs");
        assert_eq!(params.get("limit").map(String::as_str), Some("5"));
        assert_eq!(params.get("tail").map(String::as_str), Some("10"));
        let (path, params) = split_path("/api/health");
        assert_eq!(path, "/api/health");
        assert!(params.is_empty());
        let (_, params) = split_path("/x?repo_path=%2Fhome%2Fme%2Frepo");
        assert_eq!(
            params.get("repo_path").map(String::as_str),
            Some("/home/me/repo")
        );
    }

    #[test]
    fn serve_static_mime_and_cache_control() {
        use std::io::{BufRead, BufReader, Write};
        use std::net::{TcpListener, TcpStream};
        use std::time::Instant;

        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("index.html"), b"<html>root</html>").unwrap();
        std::fs::write(dir.path().join("Inter-Regular.ttf"), b"font-bytes").unwrap();
        std::fs::write(dir.path().join("app.js"), b"console.log(1);").unwrap();
        std::fs::write(dir.path().join("site.webmanifest"), br#"{"name":"Lunacy"}"#).unwrap();
        std::fs::write(dir.path().join("favicon.ico"), b"ico-bytes").unwrap();
        std::fs::write(dir.path().join("favicon.svg"), b"<svg/>").unwrap();
        std::fs::write(dir.path().join("icon-192.png"), b"png-bytes").unwrap();

        let probe = TcpListener::bind("127.0.0.1:0").expect("probe");
        let addr = probe.local_addr().expect("addr");
        drop(probe);

        let cfg = Config {
            server: Some(crate::config::ServerCfg {
                listen: addr.to_string(),
                token: String::new(),
                token_file: None,
                web_root: Some(dir.path().to_path_buf()),
                public_url: None,
            }),
            ..Config::default()
        };
        let shared = Arc::new(Mutex::new(Shared {
            config: cfg,
            ..Shared::default()
        }));
        let state = Arc::new(ApiState {
            shared: shared.clone(),
            commands: Arc::new(Mutex::new(BTreeMap::new())),
            config_path: dir.path().join("config.toml"),
            reloads: Arc::new(Mutex::new(0)),
            discord_test_guard: Mutex::new(DiscordTestGuard::default()),
        });

        let listen = addr.to_string();
        let thread = std::thread::spawn(move || {
            let _ = serve(state, &listen);
        });

        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if TcpStream::connect(addr).is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }

        let fetch_headers = |path: &str| -> BTreeMap<String, String> {
            let mut stream = TcpStream::connect(addr).expect("connect");
            write!(
                stream,
                "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
            )
            .unwrap();
            stream.flush().unwrap();
            let mut reader = BufReader::new(stream);
            let mut status = String::new();
            reader.read_line(&mut status).unwrap();
            assert!(
                status.starts_with("HTTP/1.1 200"),
                "status for {path}: {status}"
            );
            let mut headers = BTreeMap::new();
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
                    break;
                }
                if let Some((k, v)) = line.split_once(':') {
                    headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
                }
            }
            headers
        };

        let root_hdrs = fetch_headers("/");
        assert_eq!(
            root_hdrs.get("content-type").map(String::as_str),
            Some("text/html; charset=utf-8")
        );
        assert_eq!(
            root_hdrs.get("cache-control").map(String::as_str),
            Some("no-cache")
        );

        let fallback_hdrs = fetch_headers("/runs/42");
        assert_eq!(
            fallback_hdrs.get("content-type").map(String::as_str),
            Some("text/html; charset=utf-8")
        );
        assert_eq!(
            fallback_hdrs.get("cache-control").map(String::as_str),
            Some("no-cache")
        );

        let ttf_hdrs = fetch_headers("/Inter-Regular.ttf");
        assert_eq!(
            ttf_hdrs.get("content-type").map(String::as_str),
            Some("font/ttf")
        );
        assert!(!ttf_hdrs.contains_key("cache-control"));

        let js_hdrs = fetch_headers("/app.js?v=0123456789ab");
        assert_eq!(
            js_hdrs.get("content-type").map(String::as_str),
            Some("text/javascript; charset=utf-8")
        );
        assert!(!js_hdrs.contains_key("cache-control"));

        let manifest_hdrs = fetch_headers("/site.webmanifest");
        assert_eq!(
            manifest_hdrs.get("content-type").map(String::as_str),
            Some("application/manifest+json")
        );
        assert!(!manifest_hdrs.contains_key("cache-control"));

        let ico_hdrs = fetch_headers("/favicon.ico");
        assert_eq!(
            ico_hdrs.get("content-type").map(String::as_str),
            Some("image/x-icon")
        );

        let svg_hdrs = fetch_headers("/favicon.svg");
        assert_eq!(
            svg_hdrs.get("content-type").map(String::as_str),
            Some("image/svg+xml")
        );

        let png_hdrs = fetch_headers("/icon-192.png");
        assert_eq!(
            png_hdrs.get("content-type").map(String::as_str),
            Some("image/png")
        );

        shared.lock().unwrap().shutdown = true;
        thread.join().expect("server thread");
    }

    #[test]
    fn webhook_actor_falls_back_to_pusher_when_sender_login_is_control_chars_only() {
        let payload: WebhookPayload = serde_json::from_value(json!({
            "sender": { "login": "\u{0000}\u{0007}\u{001b}\t\n" },
            "pusher": { "name": "  alice\tdev  " }
        }))
        .unwrap();
        assert_eq!(webhook_actor(&payload).as_deref(), Some("alice dev"));

        let both_bad: WebhookPayload = serde_json::from_value(json!({
            "sender": { "login": "\u{0001}\u{0002}" },
            "pusher": { "name": "\r\n\u{0000}" }
        }))
        .unwrap();
        assert_eq!(webhook_actor(&both_bad), None);

        let valid_sender: WebhookPayload = serde_json::from_value(json!({
            "sender": { "login": "octocat" },
            "pusher": { "name": "alice" }
        }))
        .unwrap();
        assert_eq!(webhook_actor(&valid_sender).as_deref(), Some("octocat"));
    }
}
