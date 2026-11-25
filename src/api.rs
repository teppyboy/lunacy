//! HTTP API for the Rune UI (see `docs/api.md`) plus the GitHub App webhook.

use crate::config::{Config, MASK};
use crate::daemon::{Command, Shared};
use crate::model::{self, Run, Trigger};

use anyhow::{Context, Result};
use hmac::{Hmac, Mac};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::Sha256;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use tiny_http::{Header, Method, Response, Server};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// One project's command channel plus its worker generation.
pub type Commands = Arc<Mutex<BTreeMap<String, Sender<Command>>>>;

pub struct ApiState {
    pub shared: Arc<Mutex<Shared>>,
    pub commands: Commands,
    pub config_path: PathBuf,
    /// Bumped whenever a reload happened; the server thread reports it.
    pub reloads: Arc<Mutex<u32>>,
}

pub fn serve(state: Arc<ApiState>, listen: &str) -> Result<()> {
    let server = Server::http(listen).map_err(|e| anyhow::anyhow!("binding {listen}: {e}"))?;
    log::info!("API listening on http://{listen}");
    let mut last_reloads = 0u32;
    loop {
        let request = match server.recv_timeout(std::time::Duration::from_millis(250)) {
            Ok(Some(r)) => r,
            Ok(None) => {
                let now = *state.reloads.lock().expect("reloads");
                if now != last_reloads {
                    last_reloads = now;
                    state.apply_config_changes();
                }
                continue;
            }
            Err(e) => {
                log::warn!("API accept error: {e}");
                continue;
            }
        };
        if let Err(e) = handle(&state, request) {
            log::warn!("API request failed: {e:#}");
        }
    }
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

fn percent_decode(s: &str) -> String {
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
            b'+' => {
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

fn json_response(status: u16, body: &Value) -> Response<std::io::Cursor<Vec<u8>>> {
    let text = body.to_string();
    let mut response = Response::from_string(text).with_status_code(status);
    for (k, v) in [
        ("Content-Type", "application/json"),
        ("Access-Control-Allow-Origin", "*"),
        (
            "Access-Control-Allow-Headers",
            "Authorization, Content-Type",
        ),
        ("Access-Control-Allow-Methods", "GET, PUT, POST, OPTIONS"),
    ] {
        if let Ok(h) = Header::from_bytes(k, v) {
            response.add_header(h);
        }
    }
    response
}

fn error_response(status: u16, message: &str) -> Response<std::io::Cursor<Vec<u8>>> {
    json_response(status, &json!({ "error": message }))
}

fn text_response(content_type: &str, body: String) -> Response<std::io::Cursor<Vec<u8>>> {
    let mut response = Response::from_string(body).with_status_code(200);
    if let Ok(h) = Header::from_bytes("Content-Type", content_type) {
        response.add_header(h);
    }
    if let Ok(h) = Header::from_bytes("Access-Control-Allow-Origin", "*") {
        response.add_header(h);
    }
    response
}

fn handle(state: &Arc<ApiState>, request: tiny_http::Request) -> Result<()> {
    let url = request.url().to_string();
    let method = request.method().clone();
    let (path, params) = split_path(&url);

    if method == Method::Options {
        let mut response = Response::empty(204);
        for (k, v) in [
            ("Access-Control-Allow-Origin", "*"),
            (
                "Access-Control-Allow-Headers",
                "Authorization, Content-Type",
            ),
            ("Access-Control-Allow-Methods", "GET, PUT, POST, OPTIONS"),
        ] {
            if let Ok(h) = Header::from_bytes(k, v) {
                response.add_header(h);
            }
        }
        return request.respond(response).map_err(Into::into);
    }

    // Static UI files at `/`.
    if !path.starts_with("/api") && method == Method::Get {
        return serve_static(state, request, &path);
    }

    if path == "/api/health" {
        let body = json!({"ok": true, "version": VERSION});
        return request
            .respond(json_response(200, &body))
            .map_err(Into::into);
    }

    // The webhook authenticates with its own HMAC, not the bearer token.
    if path == "/api/github/webhook" && method == Method::Post {
        return handle_webhook(state, request);
    }

    if let Err((status, message)) = authorize(state, &request) {
        return request
            .respond(error_response(status, &message))
            .map_err(Into::into);
    }

    let segments: Vec<&str> = path.trim_matches('/').split('/').collect();
    match (method, segments.as_slice()) {
        (Method::Get, ["api", "projects"]) => {
            let body = projects_json(state);
            request
                .respond(json_response(200, &body))
                .map_err(Into::into)
        }
        (Method::Get, ["api", "projects", name]) => {
            let body = projects_json(state);
            match body
                .as_array()
                .and_then(|a| a.iter().find(|p| p["name"] == *name).cloned())
            {
                Some(p) => request.respond(json_response(200, &p)).map_err(Into::into),
                None => request
                    .respond(error_response(404, "unknown project"))
                    .map_err(Into::into),
            }
        }
        (Method::Get, ["api", "projects", name, "runs"]) => {
            let limit = params
                .get("limit")
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(20)
                .clamp(1, 200);
            let state_dir = state.shared.lock().expect("shared").state_dir.clone();
            let runs = model::list_runs(&state_dir, name, limit);
            let body: Vec<Value> = runs.iter().map(run_json).collect();
            request
                .respond(json_response(200, &Value::Array(body)))
                .map_err(Into::into)
        }
        (Method::Get, ["api", "projects", name, "workflows"]) => {
            handle_workflows(state, request, name, &params)
        }
        (Method::Post, ["api", "projects", name, "trigger"]) => {
            handle_trigger(state, request, name)
        }
        (Method::Get, ["api", "runs", id]) => {
            let state_dir = state.shared.lock().expect("shared").state_dir.clone();
            match lookup_run(&state_dir, id) {
                Some(run) => request
                    .respond(json_response(200, &run_json(&run)))
                    .map_err(Into::into),
                None => request
                    .respond(error_response(404, "unknown run"))
                    .map_err(Into::into),
            }
        }
        (Method::Get, ["api", "runs", id, "steps", index, "log"]) => {
            handle_step_log(state, request, id, index, &params)
        }
        (Method::Get, ["api", "config"]) => {
            let cfg = state.shared.lock().expect("shared").config.clone();
            let body = serde_json::to_value(cfg.masked())?;
            request
                .respond(json_response(200, &body))
                .map_err(Into::into)
        }
        (Method::Put, ["api", "config"]) => handle_put_config(state, request),
        (Method::Get, ["api", "system"]) => {
            let body = system_json(state);
            request
                .respond(json_response(200, &body))
                .map_err(Into::into)
        }
        _ => request
            .respond(error_response(404, "not found"))
            .map_err(Into::into),
    }
}

fn authorize(state: &Arc<ApiState>, request: &tiny_http::Request) -> Result<(), (u16, String)> {
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
    let header = request
        .headers()
        .iter()
        .find(|h| h.field.equiv("Authorization"))
        .map(|h| h.value.as_str().to_string())
        .unwrap_or_default();
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

fn read_body(request: &mut tiny_http::Request) -> Result<String> {
    let mut text = String::new();
    request
        .as_reader()
        .read_to_string(&mut text)
        .context("reading the request body")?;
    Ok(text)
}

fn projects_json(state: &Arc<ApiState>) -> Value {
    let shared = state.shared.lock().expect("shared");
    let state_dir = shared.state_dir.clone();
    let list: Vec<Value> = shared
        .projects
        .iter()
        .map(|p| {
            let mut value = json!({
                "name": p.name,
                "branch": p.branch,
                "github_repo": p.github_repo,
                "upstream_sha": p.upstream_sha,
                "last_sha": p.last_sha,
                "running": p.running,
                "current_run": p.current_run.as_ref().map(run_json),
                "last_run": p
                    .last_run
                    .as_ref()
                    .map(run_json)
                    .or_else(|| {
                        model::list_runs(&state_dir, &p.name, 1)
                            .first()
                            .map(run_json)
                    }),
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
    json!({
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
    })
}

fn lookup_run(state_dir: &Path, id: &str) -> Option<Run> {
    let project = model::find_run_project(state_dir, id)?;
    model::find_run(state_dir, &project, id)
}

fn handle_step_log(
    state: &Arc<ApiState>,
    request: tiny_http::Request,
    id: &str,
    index: &str,
    params: &BTreeMap<String, String>,
) -> Result<()> {
    let state_dir = state.shared.lock().expect("shared").state_dir.clone();
    let Some(run) = lookup_run(&state_dir, id) else {
        return request
            .respond(error_response(404, "unknown run"))
            .map_err(Into::into);
    };
    let Ok(index) = index.parse::<usize>() else {
        return request
            .respond(error_response(404, "bad step index"))
            .map_err(Into::into);
    };
    if index >= run.steps.len() {
        return request
            .respond(error_response(404, "step index out of range"))
            .map_err(Into::into);
    }
    let path = model::run_log_path(&state_dir, &run.project, &run.id, index);
    if !path.is_file() {
        return request
            .respond(text_response("text/plain; charset=utf-8", String::new()))
            .map_err(Into::into);
    }
    let tail = params.get("tail").and_then(|v| v.parse::<usize>().ok());
    let body = match tail {
        Some(n) => model::tail_lines(&path, n).unwrap_or_default(),
        None => std::fs::read_to_string(&path).unwrap_or_default(),
    };
    request
        .respond(text_response("text/plain; charset=utf-8", body))
        .map_err(Into::into)
}

#[derive(Debug, Deserialize, Default)]
struct TriggerBody {
    #[serde(default)]
    sha: Option<String>,
    #[serde(default)]
    force: bool,
}

fn handle_trigger(
    state: &Arc<ApiState>,
    mut request: tiny_http::Request,
    name: &str,
) -> Result<()> {
    let body = read_body(&mut request).unwrap_or_default();
    let parsed: TriggerBody = if body.trim().is_empty() {
        TriggerBody::default()
    } else {
        match serde_json::from_str(&body) {
            Ok(b) => b,
            Err(e) => {
                return request
                    .respond(error_response(400, &format!("invalid JSON: {e}")))
                    .map_err(Into::into)
            }
        }
    };

    let (running, known, upstream) = {
        let shared = state.shared.lock().expect("shared");
        match shared.projects.iter().find(|p| p.name == name) {
            Some(p) => (p.running, true, p.upstream_sha.clone()),
            None => (false, false, None),
        }
    };
    if !known {
        return request
            .respond(error_response(404, "unknown project"))
            .map_err(Into::into);
    }
    let tx = state.commands.lock().expect("commands").get(name).cloned();
    let Some(tx) = tx else {
        return request
            .respond(error_response(503, "project worker is not running"))
            .map_err(Into::into);
    };
    if running {
        return request
            .respond(error_response(409, "project is already running"))
            .map_err(Into::into);
    }
    if parsed.sha.is_none() && upstream.is_none() {
        return request
            .respond(error_response(
                409,
                "no upstream sha yet; wait for the first poll",
            ))
            .map_err(Into::into);
    }
    let _ = tx.send(Command::Trigger {
        sha: parsed.sha,
        force: parsed.force,
        trigger: Trigger::Manual,
    });
    request
        .respond(json_response(202, &json!({"queued": true})))
        .map_err(Into::into)
}

// ------------------------------------------------------------ workflows

fn handle_workflows(
    state: &Arc<ApiState>,
    request: tiny_http::Request,
    name: &str,
    params: &BTreeMap<String, String>,
) -> Result<()> {
    let repo_path = match params.get("repo_path") {
        Some(p) if !p.is_empty() => PathBuf::from(p),
        _ => {
            let shared = state.shared.lock().expect("shared");
            match shared.config.project(name) {
                Some(p) => p.repo_path.clone(),
                None => {
                    drop(shared);
                    return request
                        .respond(error_response(404, "unknown project"))
                        .map_err(Into::into);
                }
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
    request
        .respond(json_response(200, &Value::Array(out)))
        .map_err(Into::into)
}

// --------------------------------------------------------------- config

fn handle_put_config(state: &Arc<ApiState>, mut request: tiny_http::Request) -> Result<()> {
    let body = read_body(&mut request)?;
    let old = state.shared.lock().expect("shared").config.clone();
    let mut incoming: Config = match serde_json::from_str(&body) {
        Ok(c) => c,
        Err(e) => {
            return request
                .respond(json_response(
                    400,
                    &json!({"error": format!("invalid config: {e}"), "field": Value::Null}),
                ))
                .map_err(Into::into)
        }
    };
    incoming.unmask_from(&old);

    if let Err(e) = incoming.validate() {
        return request
            .respond(json_response(
                400,
                &json!({"error": e.message, "field": e.field}),
            ))
            .map_err(Into::into);
    }

    let path = &state.config_path;
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    let text = match crate::configfile::render(&existing, &incoming) {
        Ok(t) => t,
        Err(e) => {
            return request
                .respond(json_response(
                    400,
                    &json!({"error": format!("cannot serialise the config: {e:#}"), "field": Value::Null}),
                ))
                .map_err(Into::into)
        }
    };

    if let Err(e) = crate::configfile::write_atomic(path, &text) {
        return request
            .respond(json_response(
                500,
                &json!({"error": format!("writing {}: {e:#}", path.display()), "field": Value::Null}),
            ))
            .map_err(Into::into);
    }

    // Reload: idle workers pick it up on their next turn; a running pipeline
    // keeps the config it started with.
    let restarted = {
        let mut shared = state.shared.lock().expect("shared");
        shared.config = incoming.clone();
        let names: Vec<String> = incoming.projects.iter().map(|p| p.name.clone()).collect();
        // Refresh the read models for renamed/added projects.
        let state_dir = shared.state_dir.clone();
        shared.projects.retain(|p| names.contains(&p.name));
        for (i, project) in incoming.projects.iter().enumerate() {
            if !shared.projects.iter().any(|p| p.name == project.name) {
                let fresh = crate::daemon::ProjectState::new_public(project, &state_dir);
                let at = i.min(shared.projects.len());
                shared.projects.insert(at, fresh);
            }
        }
        shared.server_restart_needed()
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

    let mut out = serde_json::to_value(incoming.masked())?;
    if restarted {
        out["restart_required"] = json!(["server.listen"]);
    }
    request
        .respond(json_response(200, &out))
        .map_err(Into::into)
}

// -------------------------------------------------------------- webhook

#[derive(Debug, Deserialize)]
struct WebhookPayload {
    #[serde(default)]
    action: Option<String>,
    #[serde(default)]
    repository: Option<WebhookRepo>,
    #[serde(default)]
    check_run: Option<WebhookCheckRun>,
    #[serde(default)]
    check_suite: Option<WebhookCheckSuite>,
}

#[derive(Debug, Deserialize)]
struct WebhookRepo {
    #[serde(default)]
    full_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WebhookCheckRun {
    #[serde(default)]
    head_sha: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WebhookCheckSuite {
    #[serde(default)]
    head_sha: Option<String>,
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

/// Compute the header value for a body (tests and docs).
#[cfg(test)]
pub fn signature_header(secret: &str, body: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("hmac key");
    mac.update(body);
    let bytes = mac.finalize().into_bytes();
    format!(
        "sha256={}",
        bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
    )
}

fn handle_webhook(state: &Arc<ApiState>, mut request: tiny_http::Request) -> Result<()> {
    let secret = state
        .shared
        .lock()
        .expect("shared")
        .config
        .github
        .as_ref()
        .and_then(|g| g.app.as_ref())
        .and_then(|a| a.webhook_secret_value())
        .filter(|s| !s.is_empty() && s != MASK);
    let Some(secret) = secret else {
        return request
            .respond(error_response(404, "no webhook secret configured"))
            .map_err(Into::into);
    };

    let mut body = String::new();
    request
        .as_reader()
        .read_to_string(&mut body)
        .context("reading the webhook body")?;
    let header = request
        .headers()
        .iter()
        .find(|h| h.field.equiv("X-Hub-Signature-256"))
        .map(|h| h.value.as_str().to_string())
        .unwrap_or_default();

    if !verify_signature(&secret, body.as_bytes(), &header) {
        return request
            .respond(error_response(401, "invalid signature"))
            .map_err(Into::into);
    }

    let payload: WebhookPayload = match serde_json::from_str(&body) {
        Ok(p) => p,
        Err(e) => {
            return request
                .respond(error_response(400, &format!("invalid JSON: {e}")))
                .map_err(Into::into)
        }
    };
    if payload.action.as_deref() != Some("rerequested") {
        return request
            .respond(json_response(
                202,
                &json!({"queued": false, "reason": "ignored action"}),
            ))
            .map_err(Into::into);
    }
    let sha = payload
        .check_run
        .and_then(|c| c.head_sha)
        .or_else(|| payload.check_suite.and_then(|c| c.head_sha));
    let Some(sha) = sha else {
        return request
            .respond(json_response(
                202,
                &json!({"queued": false, "reason": "no head_sha"}),
            ))
            .map_err(Into::into);
    };
    let full_name = payload.repository.and_then(|r| r.full_name);

    let commands = state.commands.lock().expect("commands").clone();
    let targets: Vec<(String, Sender<Command>)> = {
        let shared = state.shared.lock().expect("shared");
        shared
            .projects
            .iter()
            .filter(|p| match (&full_name, &p.github_repo) {
                (Some(want), Some(have)) => want == have,
                _ => false,
            })
            .filter_map(|p| commands.get(&p.name).map(|tx| (p.name.clone(), tx.clone())))
            .collect()
    };
    if targets.is_empty() {
        return request
            .respond(json_response(
                202,
                &json!({"queued": false, "reason": "no matching project"}),
            ))
            .map_err(Into::into);
    }
    let mut queued = Vec::new();
    for (name, tx) in targets {
        let _ = tx.send(Command::Trigger {
            sha: Some(sha.clone()),
            force: true,
            trigger: Trigger::Webhook,
        });
        queued.push(name);
    }
    request
        .respond(json_response(
            202,
            &json!({"queued": true, "projects": queued}),
        ))
        .map_err(Into::into)
}

// -------------------------------------------------------------- system

fn system_json(state: &Arc<ApiState>) -> Value {
    let (config_path, state_dir) = {
        let shared = state.shared.lock().expect("shared");
        (state.config_path.clone(), shared.state_dir.clone())
    };
    let act_path = crate::exec::which("act");
    let act_version = act_path
        .as_ref()
        .and_then(|_| crate::exec::first_line_of_stdout("act", &["--version"]))
        .map(|v| v.trim_start_matches("act version ").to_string());
    let (docker_ok, docker_error) = match std::process::Command::new("docker")
        .args(["info", "--format", "{{.ServerVersion}}"])
        .output()
    {
        Ok(out) if out.status.success() => (true, None),
        Ok(out) => (
            false,
            Some(String::from_utf8_lossy(&out.stderr).trim().to_string()),
        ),
        Err(e) => (false, Some(e.to_string())),
    };
    let git_version = crate::exec::first_line_of_stdout("git", &["--version"]);
    json!({
        "os": crate::config::Os::current().as_str(),
        "version": VERSION,
        "config_path": config_path.to_string_lossy(),
        "state_dir": state_dir.to_string_lossy(),
        "act": {
            "path": act_path.map(|p| p.to_string_lossy().to_string()),
            "version": act_version,
        },
        "docker": { "ok": docker_ok, "error": docker_error },
        "git": { "version": git_version },
    })
}

// --------------------------------------------------------------- static

fn serve_static(state: &Arc<ApiState>, request: tiny_http::Request, path: &str) -> Result<()> {
    let root = state
        .shared
        .lock()
        .expect("shared")
        .config
        .server
        .as_ref()
        .and_then(|s| s.web_root.clone());
    let Some(root) = root else {
        return request
            .respond(error_response(404, "no web_root configured"))
            .map_err(Into::into);
    };
    let rel = path.trim_start_matches('/');
    let rel = if rel.is_empty() { "index.html" } else { rel };
    let mut file = root.join(rel);
    if rel.contains("..") || file.components().any(|c| c.as_os_str() == "..") {
        return request
            .respond(error_response(400, "bad path"))
            .map_err(Into::into);
    }
    if !file.is_file() {
        // SPA fallback.
        file = root.join("index.html");
        if !file.is_file() {
            return request
                .respond(error_response(404, "not found"))
                .map_err(Into::into);
        }
    }
    let mime = match file
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .as_deref()
    {
        Some("html") => "text/html; charset=utf-8",
        Some("js") | Some("mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json",
        Some("wasm") => "application/wasm",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        _ => "application/octet-stream",
    };
    let body = std::fs::read(&file)?;
    let mut response = Response::from_data(body)
        .with_header(Header::from_bytes("Content-Type", mime).expect("static header"));
    if let Ok(h) = Header::from_bytes("Access-Control-Allow-Origin", "*") {
        response.add_header(h);
    }
    request.respond(response).map_err(Into::into)
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
}
