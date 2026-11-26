//! HTTP API for the Rune UI (see `docs/api.md`) plus the GitHub App webhook.

use crate::config::{self, Config, MASK};
use crate::daemon::{Command, Shared};
use crate::git;
use crate::model::{self, Run, Trigger};
use crate::pipeline::PrContext;

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

    let segments_owned: Vec<String> = path
        .trim_matches('/')
        .split('/')
        .map(percent_decode_path_segment)
        .collect();
    if segments_owned.iter().any(|s| {
        s == "."
            || s.contains("..")
            || s.contains('/')
            || s.contains('\\')
            || s.chars().any(|c| c.is_ascii_control())
    }) {
        return request
            .respond(error_response(400, "invalid path segment"))
            .map_err(Into::into);
    }
    let segments: Vec<&str> = segments_owned.iter().map(String::as_str).collect();
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
        (Method::Post, ["api", "projects", name, "deploy"]) => {
            handle_deploy_project(state, request, name)
        }
        (Method::Post, ["api", "projects", name, "cancel"]) => {
            handle_cancel_project(state, request, name)
        }
        (Method::Post, ["api", "projects", name, "cache", "clear"]) => {
            handle_clear_project_cache(state, request, name)
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
        (Method::Post, ["api", "runs", id, "deploy"]) => handle_deploy_run(state, request, id),
        (Method::Post, ["api", "runs", id, "cancel"]) => handle_cancel_run(state, request, id),
        (Method::Get, ["api", "runs", id, "steps", index, "log"]) => {
            handle_step_log(state, request, id, index, &params)
        }
        (Method::Post, ["api", "cache", "clear"]) => handle_clear_cache(state, request),
        (Method::Get, ["api", "deploy", "checks"]) => handle_deploy_checks(state, request),
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
            let cfg_p = shared.config.project(&p.name);
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
                "open_prs": p.open_prs,
                "watch_prs": cfg_p.map(|c| c.watch_prs).unwrap_or(false),
                "watch_tags": cfg_p.map(|c| c.watch_tags).unwrap_or(false),
                "deploy_refs": cfg_p.map(|c| c.deploy_refs.clone()).unwrap_or_default(),
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
    #[serde(default)]
    deploy_only: bool,
    #[serde(default)]
    force_deploy: bool,
    #[serde(default, rename = "ref")]
    git_ref: Option<String>,
    #[serde(default)]
    pr_number: Option<u64>,
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

    let req_sha = parsed.sha.filter(|s| !s.is_empty());
    if let Some(ref s) = req_sha {
        if !git::is_valid_sha(s) {
            return request
                .respond(error_response(400, "invalid commit sha"))
                .map_err(Into::into);
        }
    }
    let req_git_ref = parsed.git_ref.filter(|r| !r.is_empty());
    if let Some(ref r) = req_git_ref {
        if !git::is_valid_git_ref(r) {
            return request
                .respond(error_response(400, "invalid git ref"))
                .map_err(Into::into);
        }
    }

    let (running, known, upstream, branch, pr_summary) = {
        let shared = state.shared.lock().expect("shared");
        match shared.projects.iter().find(|p| p.name == name) {
            Some(p) => (
                p.running,
                true,
                p.upstream_sha.clone(),
                p.branch.clone(),
                parsed
                    .pr_number
                    .and_then(|num| p.open_prs.iter().find(|pr| pr.number == num).cloned()),
            ),
            None => (false, false, None, String::new(), None),
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
            return request
                .respond(error_response(400, "invalid commit sha"))
                .map_err(Into::into);
        }
    }

    if resolved_sha.is_none()
        && upstream.is_none()
        && parsed.pr_number.is_none()
        && req_git_ref.is_none()
    {
        return request
            .respond(error_response(
                409,
                "no upstream sha yet; wait for the first poll",
            ))
            .map_err(Into::into);
    }

    let trigger = if parsed.pr_number.is_some() {
        Trigger::PullRequest
    } else if parsed.deploy_only || parsed.force_deploy {
        Trigger::ForceDeploy
    } else {
        Trigger::Manual
    };
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
            return request
                .respond(error_response(400, "invalid git ref"))
                .map_err(Into::into);
        }
    }

    let _ = tx.send(Command::Trigger {
        sha: resolved_sha,
        force: parsed.force || parsed.deploy_only || parsed.force_deploy,
        trigger,
        git_ref,
        pr: pr_ctx,
        deploy_only: parsed.deploy_only,
        force_deploy: parsed.force_deploy,
    });
    request
        .respond(json_response(202, &json!({"queued": true})))
        .map_err(Into::into)
}

#[derive(Debug, Deserialize, Default)]
struct DeployBody {
    #[serde(default)]
    sha: Option<String>,
    #[serde(default)]
    skip_ci: Option<bool>,
}

fn handle_deploy_project(
    state: &Arc<ApiState>,
    mut request: tiny_http::Request,
    name: &str,
) -> Result<()> {
    let body = read_body(&mut request).unwrap_or_default();
    let parsed: DeployBody = if body.trim().is_empty() {
        DeployBody::default()
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

    let req_sha = parsed.sha.filter(|s| !s.trim().is_empty());
    if let Some(ref s) = req_sha {
        if !git::is_valid_sha(s) {
            return request
                .respond(error_response(400, "invalid commit sha"))
                .map_err(Into::into);
        }
    }

    let (project_cfg, running, upstream, last_sha) = {
        let shared = state.shared.lock().expect("shared");
        let cfg_p = shared.config.project(name).cloned();
        let state_p = shared.projects.iter().find(|p| p.name == name);
        match (cfg_p, state_p) {
            (Some(c), Some(s)) => (c, s.running, s.upstream_sha.clone(), s.last_sha.clone()),
            (Some(c), None) => (c, false, None, None),
            (None, _) => {
                return request
                    .respond(error_response(404, "unknown project"))
                    .map_err(Into::into);
            }
        }
    };

    if project_cfg.deploy.is_empty() {
        return request
            .respond(error_response(400, "project has no deploy steps"))
            .map_err(Into::into);
    }
    if running {
        return request
            .respond(error_response(409, "project is already running"))
            .map_err(Into::into);
    }
    let tx = state.commands.lock().expect("commands").get(name).cloned();
    let Some(tx) = tx else {
        return request
            .respond(error_response(503, "project worker is not running"))
            .map_err(Into::into);
    };

    let target_sha = req_sha
        .or(upstream)
        .or(last_sha)
        .or_else(|| git::rev_parse(&project_cfg.repo_path, "HEAD").ok());
    let Some(target_sha) = target_sha else {
        return request
            .respond(error_response(
                409,
                "no resolvable sha yet; pass sha explicitly or wait for the first poll",
            ))
            .map_err(Into::into);
    };
    if !git::is_valid_sha(&target_sha) {
        return request
            .respond(error_response(400, "invalid commit sha"))
            .map_err(Into::into);
    }

    let deploy_only = parsed.skip_ci.unwrap_or(true);
    let _ = tx.send(Command::Trigger {
        sha: Some(target_sha.clone()),
        force: true,
        trigger: Trigger::ForceDeploy,
        git_ref: Some(project_cfg.branch.clone()),
        pr: None,
        deploy_only,
        force_deploy: true,
    });
    request
        .respond(json_response(
            202,
            &json!({
                "queued": true,
                "sha": target_sha,
                "deploy_only": deploy_only,
            }),
        ))
        .map_err(Into::into)
}

#[derive(Debug, Deserialize, Default)]
struct RunDeployBody {
    #[serde(default)]
    skip_ci: Option<bool>,
    #[serde(default)]
    allow_pr: bool,
}

fn handle_deploy_run(
    state: &Arc<ApiState>,
    mut request: tiny_http::Request,
    id: &str,
) -> Result<()> {
    let body = read_body(&mut request).unwrap_or_default();
    let parsed: RunDeployBody = if body.trim().is_empty() {
        RunDeployBody::default()
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

    let state_dir = state.shared.lock().expect("shared").state_dir.clone();
    let Some(run) = model::read_run(&state_dir, id) else {
        return request
            .respond(error_response(404, "unknown run"))
            .map_err(Into::into);
    };

    if run.pr_number.is_some() && !parsed.allow_pr {
        return request
            .respond(error_response(400, "cannot deploy a pull request build"))
            .map_err(Into::into);
    }

    let (project_cfg, running) = {
        let shared = state.shared.lock().expect("shared");
        let cfg_p = shared.config.project(&run.project).cloned();
        let is_running = shared
            .projects
            .iter()
            .find(|p| p.name == run.project)
            .is_some_and(|p| p.running);
        match cfg_p {
            Some(c) => (c, is_running),
            None => {
                return request
                    .respond(error_response(404, "unknown project"))
                    .map_err(Into::into);
            }
        }
    };

    if project_cfg.deploy.is_empty() {
        return request
            .respond(error_response(400, "project has no deploy steps"))
            .map_err(Into::into);
    }
    if running {
        return request
            .respond(error_response(409, "project is already running"))
            .map_err(Into::into);
    }
    let tx = state
        .commands
        .lock()
        .expect("commands")
        .get(&run.project)
        .cloned();
    let Some(tx) = tx else {
        return request
            .respond(error_response(503, "project worker is not running"))
            .map_err(Into::into);
    };

    let deploy_only = parsed.skip_ci.unwrap_or(true);
    let git_ref = run
        .git_ref
        .clone()
        .unwrap_or_else(|| project_cfg.branch.clone());
    let pr = run.pr_number.map(|number| PrContext {
        number,
        title: run.pr_title.clone().unwrap_or_default(),
        head_ref: run.pr_head_ref.clone().unwrap_or_default(),
        base_ref: run
            .pr_base_ref
            .clone()
            .unwrap_or_else(|| project_cfg.branch.clone()),
        base_sha: String::new(),
    });

    let _ = tx.send(Command::Trigger {
        sha: Some(run.sha.clone()),
        force: true,
        trigger: Trigger::ForceDeploy,
        git_ref: Some(git_ref),
        pr,
        deploy_only,
        force_deploy: true,
    });
    request
        .respond(json_response(
            202,
            &json!({
                "queued": true,
                "sha": run.sha,
                "deploy_only": deploy_only,
                "run_id": run.id,
                "project": run.project,
            }),
        ))
        .map_err(Into::into)
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

fn handle_clear_cache(state: &Arc<ApiState>, request: tiny_http::Request) -> Result<()> {
    const MANAGED_CACHE_SUBDIRS: &[&str] = &["act-actions", "act-cache-server", "projects"];
    let (cache_dir, wsl) = {
        let shared = state.shared.lock().expect("shared");
        let act = shared.config.act();
        (act.effective_cache_dir(&shared.state_dir), act.is_wsl())
    };
    let mut removed = Vec::new();
    if cache_dir.is_dir() {
        for sub in MANAGED_CACHE_SUBDIRS {
            let path = cache_dir.join(sub);
            if let Ok(meta) = std::fs::symlink_metadata(&path) {
                let res = if meta.file_type().is_dir() {
                    std::fs::remove_dir_all(&path)
                } else {
                    std::fs::remove_file(&path)
                };
                if res.is_ok() {
                    removed.push(path.to_string_lossy().to_string());
                }
            }
        }
    }
    removed.extend(remove_docker_volumes_with_prefix("lunacy-cache-", wsl));
    request
        .respond(json_response(
            200,
            &json!({"cleared": true, "removed": removed}),
        ))
        .map_err(Into::into)
}

fn handle_clear_project_cache(
    state: &Arc<ApiState>,
    request: tiny_http::Request,
    name: &str,
) -> Result<()> {
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
        return request
            .respond(error_response(404, "unknown project"))
            .map_err(Into::into);
    }
    let slug = config::project_cache_slug(name);
    let mut removed = Vec::new();
    let project_cache = cache_dir.join("projects").join(&slug);
    if project_cache.exists() && std::fs::remove_dir_all(&project_cache).is_ok() {
        removed.push(project_cache.to_string_lossy().to_string());
    }
    removed.extend(remove_docker_volumes_with_prefix(
        &format!("lunacy-cache-{slug}-"),
        wsl,
    ));
    request
        .respond(json_response(
            200,
            &json!({"cleared": true, "project": name, "removed": removed}),
        ))
        .map_err(Into::into)
}

fn handle_cancel_run(state: &Arc<ApiState>, request: tiny_http::Request, id: &str) -> Result<()> {
    let (matched, state_dir) = {
        let shared = state.shared.lock().expect("shared");
        (shared.cancel_run(id), shared.state_dir.clone())
    };
    if let Some((project, run_id)) = matched {
        return request
            .respond(json_response(
                202,
                &json!({
                    "cancelling": true,
                    "run_id": run_id,
                    "project": project,
                }),
            ))
            .map_err(Into::into);
    }
    if lookup_run(&state_dir, id).is_some() {
        return request
            .respond(error_response(409, "run is not running"))
            .map_err(Into::into);
    }
    request
        .respond(error_response(404, "run not found"))
        .map_err(Into::into)
}

fn handle_cancel_project(
    state: &Arc<ApiState>,
    request: tiny_http::Request,
    name: &str,
) -> Result<()> {
    let outcome = {
        let shared = state.shared.lock().expect("shared");
        shared.cancel_project(name)
    };
    match outcome {
        crate::daemon::CancelProjectOutcome::Cancelling(run_id) => request
            .respond(json_response(
                202,
                &json!({
                    "cancelling": true,
                    "run_id": run_id,
                    "project": name,
                }),
            ))
            .map_err(Into::into),
        crate::daemon::CancelProjectOutcome::NotRunning => request
            .respond(error_response(409, "project is not running"))
            .map_err(Into::into),
        crate::daemon::CancelProjectOutcome::NotFound => request
            .respond(error_response(404, "project not found"))
            .map_err(Into::into),
    }
}

fn handle_deploy_checks(state: &Arc<ApiState>, request: tiny_http::Request) -> Result<()> {
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
    let body = serde_json::to_value(report)?;
    request
        .respond(json_response(200, &body))
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
        // Refresh the read models for added/removed/reordered projects.
        let state_dir = shared.state_dir.clone();
        let mut old_projects = std::mem::take(&mut shared.projects);
        let mut ordered = Vec::with_capacity(incoming.projects.len());
        for project in &incoming.projects {
            if let Some(pos) = old_projects.iter().position(|p| p.name == project.name) {
                let mut existing = old_projects.remove(pos);
                existing.branch = project.branch.clone();
                existing.github_repo = project.github_repo.clone();
                if !project.watch_prs {
                    existing.open_prs.clear();
                }
                ordered.push(existing);
            } else {
                ordered.push(crate::daemon::ProjectState::new_public(project, &state_dir));
            }
        }
        shared.projects = ordered;
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
    #[serde(default)]
    pull_request: Option<WebhookPullRequest>,
    #[serde(default, rename = "ref")]
    git_ref: Option<String>,
    #[serde(default)]
    after: Option<String>,
    #[serde(default)]
    deleted: Option<bool>,
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

#[allow(clippy::too_many_lines)]
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
    let sig_header = request
        .headers()
        .iter()
        .find(|h| h.field.equiv("X-Hub-Signature-256"))
        .map(|h| h.value.as_str().to_string())
        .unwrap_or_default();
    let event_header = request
        .headers()
        .iter()
        .find(|h| h.field.equiv("X-GitHub-Event"))
        .map(|h| h.value.as_str().trim().to_ascii_lowercase());

    if !verify_signature(&secret, body.as_bytes(), &sig_header) {
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

    let event_kind = match event_header.as_deref() {
        Some(ev) if !ev.is_empty() => ev,
        _ if payload.pull_request.is_some() => "pull_request",
        _ if payload.git_ref.is_some() && payload.after.is_some() => "push",
        _ => "check_run",
    };

    let full_name = payload.repository.and_then(|r| r.full_name);
    let commands = state.commands.lock().expect("commands").clone();

    match event_kind {
        "pull_request" => {
            let action = payload.action.as_deref().unwrap_or("");
            if !matches!(action, "opened" | "synchronize" | "reopened") {
                return request
                    .respond(json_response(
                        202,
                        &json!({"queued": false, "reason": "ignored action"}),
                    ))
                    .map_err(Into::into);
            }
            let Some(pr) = payload.pull_request else {
                return request
                    .respond(json_response(
                        202,
                        &json!({"queued": false, "reason": "missing pull_request"}),
                    ))
                    .map_err(Into::into);
            };
            let Some(number) = pr.number else {
                return request
                    .respond(json_response(
                        202,
                        &json!({"queued": false, "reason": "missing pull_request.number"}),
                    ))
                    .map_err(Into::into);
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
                return request
                    .respond(json_response(
                        200,
                        &json!({"ignored": true, "reason": "fork pull request"}),
                    ))
                    .map_err(Into::into);
            }
            let Some(head_sha) = head.sha.filter(|s| !s.is_empty()) else {
                return request
                    .respond(json_response(
                        202,
                        &json!({"queued": false, "reason": "no head_sha"}),
                    ))
                    .map_err(Into::into);
            };
            if !git::is_valid_sha(&head_sha) {
                return request
                    .respond(error_response(400, "invalid head_sha"))
                    .map_err(Into::into);
            }
            let head_ref = head
                .git_ref
                .filter(|r| !r.is_empty())
                .unwrap_or_else(|| format!("pull/{number}"));
            if !git::is_valid_git_ref(&head_ref) {
                return request
                    .respond(error_response(400, "invalid head ref"))
                    .map_err(Into::into);
            }
            let base_ref = base.git_ref.unwrap_or_default();
            if !base_ref.is_empty() && !git::is_valid_git_ref(&base_ref) {
                return request
                    .respond(error_response(400, "invalid base ref"))
                    .map_err(Into::into);
            }
            let base_sha = base.sha.unwrap_or_default();
            if !base_sha.is_empty() && !git::is_valid_sha(&base_sha) {
                return request
                    .respond(error_response(400, "invalid base_sha"))
                    .map_err(Into::into);
            }
            let title = pr.title.unwrap_or_default();

            let targets: Vec<(String, Sender<Command>)> = {
                let shared = state.shared.lock().expect("shared");
                shared
                    .projects
                    .iter()
                    .filter(|p| match (&full_name, &p.github_repo) {
                        (Some(want), Some(have)) => want.eq_ignore_ascii_case(have),
                        _ => false,
                    })
                    .filter(|p| {
                        shared
                            .config
                            .project(&p.name)
                            .is_some_and(|cfg_p| cfg_p.watch_prs)
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
            let pr_ctx = PrContext {
                number,
                title,
                head_ref: head_ref.clone(),
                base_ref,
                base_sha,
            };
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
                });
                queued.push(name);
            }
            request
                .respond(json_response(
                    202,
                    &json!({"queued": true, "projects": queued, "pr_number": number}),
                ))
                .map_err(Into::into)
        }
        "push" => {
            if payload.deleted == Some(true) {
                return request
                    .respond(json_response(
                        202,
                        &json!({"queued": false, "reason": "branch or tag deleted"}),
                    ))
                    .map_err(Into::into);
            }
            let Some(raw_ref) = payload.git_ref.filter(|r| !r.is_empty()) else {
                return request
                    .respond(json_response(
                        202,
                        &json!({"queued": false, "reason": "no ref"}),
                    ))
                    .map_err(Into::into);
            };
            if !git::is_valid_git_ref(&raw_ref) {
                return request
                    .respond(error_response(400, "invalid git ref"))
                    .map_err(Into::into);
            }
            let Some(after) = payload
                .after
                .filter(|s| !s.is_empty() && s != crate::act::ZERO_SHA)
            else {
                return request
                    .respond(json_response(
                        202,
                        &json!({"queued": false, "reason": "no after sha"}),
                    ))
                    .map_err(Into::into);
            };
            if !git::is_valid_sha(&after) {
                return request
                    .respond(error_response(400, "invalid after sha"))
                    .map_err(Into::into);
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

            let targets: Vec<(String, Sender<Command>)> = {
                let shared = state.shared.lock().expect("shared");
                shared
                    .projects
                    .iter()
                    .filter(|p| match (&full_name, &p.github_repo) {
                        (Some(want), Some(have)) => want == have,
                        _ => false,
                    })
                    .filter(|p| {
                        shared.config.project(&p.name).is_some_and(|cfg_p| {
                            raw_ref == format!("refs/heads/{}", cfg_p.branch)
                                || config::matches_deploy_ref(cfg_p, &raw_ref, false)
                                || (is_tag && cfg_p.watch_tags)
                        })
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
                    sha: Some(after.clone()),
                    force: false,
                    trigger,
                    git_ref: Some(norm_ref.clone()),
                    pr: None,
                    deploy_only: false,
                    force_deploy: false,
                });
                queued.push(name);
            }
            request
                .respond(json_response(
                    202,
                    &json!({"queued": true, "projects": queued, "ref": norm_ref}),
                ))
                .map_err(Into::into)
        }
        "check_run" | "check_suite" => {
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
            if !git::is_valid_sha(&sha) {
                return request
                    .respond(error_response(400, "invalid head_sha"))
                    .map_err(Into::into);
            }
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
                    git_ref: None,
                    pr: None,
                    deploy_only: false,
                    force_deploy: false,
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
        _ => request
            .respond(json_response(
                202,
                &json!({"queued": false, "reason": "ignored event"}),
            ))
            .map_err(Into::into),
    }
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
    if rel.contains('\\')
        || !std::path::Path::new(rel)
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)))
    {
        return request
            .respond(error_response(400, "bad path"))
            .map_err(Into::into);
    }
    let mut file = root.join(rel);
    if !file.is_file() {
        // SPA fallback.
        file = root.join("index.html");
        if !file.is_file() {
            return request
                .respond(error_response(404, "not found"))
                .map_err(Into::into);
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
        Some("wasm") => "application/wasm",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        Some("ttf") => "font/ttf",
        _ => "application/octet-stream",
    };
    let body = std::fs::read(&file)?;
    let mut response = Response::from_data(body)
        .with_header(Header::from_bytes("Content-Type", mime).expect("static header"));
    if ext.as_deref() == Some("html") {
        if let Ok(h) = Header::from_bytes("Cache-Control", "no-cache") {
            response.add_header(h);
        }
    }
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

    #[test]
    fn serve_static_mime_and_cache_control() {
        use std::io::{BufRead, BufReader, Write};
        use std::net::TcpStream;

        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("index.html"), b"<html>root</html>").unwrap();
        std::fs::write(dir.path().join("Inter-Regular.ttf"), b"font-bytes").unwrap();
        std::fs::write(dir.path().join("app.js"), b"console.log(1);").unwrap();

        let cfg = Config {
            server: Some(crate::config::ServerCfg {
                listen: "127.0.0.1:0".into(),
                token: String::new(),
                token_file: None,
                web_root: Some(dir.path().to_path_buf()),
                public_url: None,
            }),
            ..Config::default()
        };
        let state = Arc::new(ApiState {
            shared: Arc::new(Mutex::new(Shared {
                config: cfg,
                ..Shared::default()
            })),
            commands: Arc::new(Mutex::new(BTreeMap::new())),
            config_path: dir.path().join("config.toml"),
            reloads: Arc::new(Mutex::new(0)),
        });

        let server = Server::http("127.0.0.1:0").expect("bind tiny_http");
        let addr = server.server_addr().to_ip().expect("socket addr");
        let worker_state = Arc::clone(&state);
        let thread = std::thread::spawn(move || {
            for _ in 0..4 {
                let req = server.recv().expect("recv");
                handle(&worker_state, req).expect("handle static");
            }
        });

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

        thread.join().expect("server thread");
    }
}
