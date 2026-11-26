//! Real-time WebSocket hub and `GET /api/ws` handler.

use crate::api::{self, ApiState, VERSION};
use crate::model::{self, Run};

use axum::body::Bytes;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::header::AUTHORIZATION;
use axum::http::HeaderMap;
use axum::response::Response;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{json, Value};
use std::io::{Read, Seek, SeekFrom};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, watch};

pub const MAX_WS_CONNECTIONS: usize = 64;
pub const WS_BROADCAST_CAPACITY: usize = 512;
const MAX_WS_MESSAGE_BYTES: usize = 64 * 1024;
const MAX_WS_LOG_SNAPSHOT_BYTES: u64 = 8 * 1024 * 1024;
const AUTH_TIMEOUT: Duration = Duration::from_secs(5);
const PING_INTERVAL: Duration = Duration::from_secs(25);
const SEND_TIMEOUT: Duration = Duration::from_secs(10);
const CLOSE_UNAUTHORIZED: u16 = 4001;

/// Events broadcast from the daemon, pipeline, and HTTP handlers to connected WebSocket clients.
#[derive(Debug, Clone)]
pub enum WsBroadcast {
    ProjectsUpdated,
    RunUpdated(Box<Run>),
    LogChunk {
        run_id: String,
        step: usize,
        chunk: String,
        offset: u64,
    },
    ConfigUpdated,
    WebhookReceived {
        event: String,
        repo: Option<String>,
        delivery_id: Option<String>,
        outcome: String,
        received_at: String,
        queued: bool,
    },
}

/// Thread-safe, lock-free broadcast hub for real-time WebSocket updates.
#[derive(Clone, Debug)]
pub struct WsHub {
    tx: broadcast::Sender<WsBroadcast>,
    active: Arc<AtomicUsize>,
    shutdown: Arc<watch::Sender<bool>>,
}

impl Default for WsHub {
    fn default() -> Self {
        Self::new()
    }
}

impl WsHub {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(WS_BROADCAST_CAPACITY);
        let (shutdown, _) = watch::channel(false);
        WsHub {
            tx,
            active: Arc::new(AtomicUsize::new(0)),
            shutdown: Arc::new(shutdown),
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<WsBroadcast> {
        self.tx.subscribe()
    }

    pub fn subscribe_shutdown(&self) -> watch::Receiver<bool> {
        self.shutdown.subscribe()
    }

    pub fn notify_shutdown(&self) {
        let _ = self.shutdown.send(true);
    }

    pub fn active_connections(&self) -> usize {
        self.active.load(Ordering::SeqCst)
    }

    /// Reserve one WebSocket connection slot up to [`MAX_WS_CONNECTIONS`].
    pub fn try_acquire_connection(&self) -> Option<WsConnectionGuard> {
        let mut cur = self.active.load(Ordering::SeqCst);
        loop {
            if cur >= MAX_WS_CONNECTIONS {
                return None;
            }
            match self
                .active
                .compare_exchange(cur, cur + 1, Ordering::SeqCst, Ordering::SeqCst)
            {
                Ok(_) => {
                    return Some(WsConnectionGuard {
                        active: self.active.clone(),
                    });
                }
                Err(actual) => cur = actual,
            }
        }
    }

    pub fn emit(&self, event: WsBroadcast) {
        let _ = self.tx.send(event);
    }

    pub fn emit_projects_updated(&self) {
        self.emit(WsBroadcast::ProjectsUpdated);
    }

    pub fn emit_run_updated(&self, run: &Run) {
        self.emit(WsBroadcast::RunUpdated(Box::new(run.clone())));
    }

    pub fn emit_log_chunk(
        &self,
        run_id: impl Into<String>,
        step: usize,
        chunk: impl Into<String>,
        offset: u64,
    ) {
        self.emit(WsBroadcast::LogChunk {
            run_id: run_id.into(),
            step,
            chunk: chunk.into(),
            offset,
        });
    }

    pub fn emit_config_updated(&self) {
        self.emit(WsBroadcast::ConfigUpdated);
    }

    pub fn emit_webhook_received(
        &self,
        event: impl Into<String>,
        repo: Option<String>,
        delivery_id: Option<String>,
        outcome: impl Into<String>,
        received_at: impl Into<String>,
        queued: bool,
    ) {
        self.emit(WsBroadcast::WebhookReceived {
            event: event.into(),
            repo,
            delivery_id,
            outcome: outcome.into(),
            received_at: received_at.into(),
            queued,
        });
    }
}

/// RAII guard that decrements the active WebSocket connection count on drop.
pub struct WsConnectionGuard {
    active: Arc<AtomicUsize>,
}

impl Drop for WsConnectionGuard {
    fn drop(&mut self) {
        self.active.fetch_sub(1, Ordering::SeqCst);
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientMsg {
    Auth {
        #[serde(default)]
        token: String,
    },
    SubscribeRun {
        run_id: String,
    },
    UnsubscribeRun,
    SubscribeLog {
        run_id: String,
        step: usize,
        #[serde(default)]
        tail: Option<usize>,
    },
    UnsubscribeLog,
    Ping,
}

struct LogSub {
    run_id: String,
    step: usize,
    tail: Option<usize>,
    last_offset: u64,
}

fn current_token(state: &ApiState) -> String {
    state
        .shared
        .lock()
        .expect("shared")
        .config
        .server
        .as_ref()
        .map(|s| s.token_value())
        .unwrap_or_default()
}

fn lookup_run_live_or_disk(state: &ApiState, id: &str) -> Option<Run> {
    if !api::is_valid_segment(id) {
        return None;
    }
    let state_dir = {
        let shared = state.shared.lock().expect("shared");
        for p in &shared.projects {
            if let Some(active) = p.active_runs.iter().find(|a| a.run.id == id) {
                return Some(active.run.clone());
            }
            if let Some(ref r) = p.current_run {
                if r.id == id {
                    return Some(r.clone());
                }
            }
        }
        shared.state_dir.clone()
    };
    model::read_run(&state_dir, id)
}

async fn lookup_run_live_or_disk_async(state: &Arc<ApiState>, id: String) -> Option<Run> {
    if !api::is_valid_segment(&id) {
        return None;
    }
    let state = state.clone();
    tokio::task::spawn_blocking(move || lookup_run_live_or_disk(&state, &id))
        .await
        .ok()
        .flatten()
}

async fn projects_json_async(state: &Arc<ApiState>) -> Value {
    let state = state.clone();
    tokio::task::spawn_blocking(move || api::projects_json(&state))
        .await
        .unwrap_or_else(|_| Value::Array(Vec::new()))
}

fn read_log_snapshot(
    state: &ApiState,
    run_id: &str,
    step: usize,
    tail: Option<usize>,
) -> (String, u64) {
    if !api::is_valid_segment(run_id) {
        return (String::new(), 0);
    }
    let state_dir = state.shared.lock().expect("shared").state_dir.clone();
    let Some(run) = lookup_run_live_or_disk(state, run_id) else {
        return (String::new(), 0);
    };
    if step >= run.steps.len() {
        return (String::new(), 0);
    }
    let path = model::run_log_path(&state_dir, &run.project, &run.id, step);
    read_log_snapshot_from_path(&path, tail)
}

fn read_log_snapshot_from_path(path: &std::path::Path, tail: Option<usize>) -> (String, u64) {
    let Ok(mut file) = std::fs::File::open(path) else {
        return (String::new(), 0);
    };
    let Ok(meta) = file.metadata() else {
        return (String::new(), 0);
    };
    if !meta.is_file() {
        return (String::new(), 0);
    }
    let len = meta.len();

    match tail {
        Some(n) => {
            let start = len.saturating_sub(model::LOG_TAIL_BYTES);
            let to_read = len.saturating_sub(start);
            if start > 0 && file.seek(SeekFrom::Start(start)).is_err() {
                return (String::new(), 0);
            }
            let mut buf = Vec::with_capacity(to_read.min(model::LOG_TAIL_BYTES) as usize);
            if (&mut file).take(to_read).read_to_end(&mut buf).is_err() {
                return (String::new(), 0);
            }
            let end_offset = start.saturating_add(buf.len() as u64);
            let ends_with_newline = buf.last() == Some(&b'\n');
            let raw = String::from_utf8_lossy(&buf);
            let text = if n == 0 {
                raw.into_owned()
            } else {
                let all: Vec<&str> = raw.lines().collect();
                let from = all.len().saturating_sub(n);
                // If we started mid-line, drop the partial first line.
                let slice_start = if start > 0 && all.len() > 1 {
                    from.max(1)
                } else {
                    from
                };
                let mut s = all[slice_start..].join("\n");
                // Preserve trailing newline when the underlying slice ends with one so
                // subsequent `log_chunk` frames concatenate cleanly without joining lines.
                if !s.is_empty() && ends_with_newline && !s.ends_with('\n') {
                    s.push('\n');
                }
                s
            };
            (text, end_offset)
        }
        None => {
            if len <= MAX_WS_LOG_SNAPSHOT_BYTES {
                let mut buf = Vec::with_capacity(len as usize);
                if (&mut file).take(len).read_to_end(&mut buf).is_err() {
                    return (String::new(), 0);
                }
                let end_offset = buf.len() as u64;
                let text = String::from_utf8_lossy(&buf).into_owned();
                (text, end_offset)
            } else {
                let start = len.saturating_sub(MAX_WS_LOG_SNAPSHOT_BYTES);
                let to_read = len.saturating_sub(start);
                if file.seek(SeekFrom::Start(start)).is_err() {
                    return (String::new(), 0);
                }
                let mut buf = Vec::with_capacity(to_read as usize);
                if (&mut file).take(to_read).read_to_end(&mut buf).is_err() {
                    return (String::new(), 0);
                }
                let end_offset = start.saturating_add(buf.len() as u64);
                let slice = match buf.iter().position(|&b| b == b'\n') {
                    Some(pos) if pos + 1 < buf.len() => &buf[pos + 1..],
                    _ => &buf[..],
                };
                let omitted = start.saturating_add((buf.len() - slice.len()) as u64);
                let body = String::from_utf8_lossy(slice);
                let text = format!(
                    "[lunacy] log exceeds 8 MiB; showing trailing output ({omitted} bytes omitted)\n{body}"
                );
                (text, end_offset)
            }
        }
    }
}

async fn read_log_snapshot_async(
    state: &Arc<ApiState>,
    run_id: String,
    step: usize,
    tail: Option<usize>,
) -> (String, u64) {
    if !api::is_valid_segment(&run_id) {
        return (String::new(), 0);
    }
    let state = state.clone();
    tokio::task::spawn_blocking(move || read_log_snapshot(&state, &run_id, step, tail))
        .await
        .unwrap_or_else(|_| (String::new(), 0))
}

/// Axum handler for `GET /api/ws`.
pub async fn ws_handler(
    State(state): State<Arc<ApiState>>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let (token, hub) = {
        let shared = state.shared.lock().expect("shared");
        let token = shared
            .config
            .server
            .as_ref()
            .map(|s| s.token_value())
            .unwrap_or_default();
        (token, shared.ws.clone())
    };

    let already_authed: Option<String> = if token.is_empty() {
        Some(String::new())
    } else if let Some(auth_val) = headers.get(AUTHORIZATION) {
        let raw = auth_val.to_str().unwrap_or("");
        let supplied = raw.strip_prefix("Bearer ").unwrap_or("");
        if api::constant_time_eq(supplied.as_bytes(), token.as_bytes()) {
            Some(supplied.to_string())
        } else {
            return api::error_response(401, "missing or invalid token");
        }
    } else {
        None
    };

    let Some(guard) = hub.try_acquire_connection() else {
        return api::error_response(503, "too many websocket connections");
    };

    ws.max_message_size(MAX_WS_MESSAGE_BYTES)
        .max_frame_size(MAX_WS_MESSAGE_BYTES)
        .on_upgrade(move |socket| handle_socket(socket, state, hub, guard, already_authed))
}

async fn send_msg(
    sink: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    msg: Message,
) -> Result<(), ()> {
    match tokio::time::timeout(SEND_TIMEOUT, sink.send(msg)).await {
        Ok(Ok(())) => Ok(()),
        _ => Err(()),
    }
}

async fn close_sink(sink: &mut futures_util::stream::SplitSink<WebSocket, Message>) {
    let _ = tokio::time::timeout(SEND_TIMEOUT, sink.close()).await;
}

async fn send_json(
    sink: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    val: &Value,
) -> Result<(), ()> {
    send_msg(sink, Message::Text(val.to_string().into())).await
}

async fn reject_unauth(sink: &mut futures_util::stream::SplitSink<WebSocket, Message>) {
    let _ = send_json(
        sink,
        &json!({
            "type": "auth_error",
            "error": "unauthorized",
        }),
    )
    .await;
    let _ = send_msg(
        sink,
        Message::Close(Some(CloseFrame {
            code: CLOSE_UNAUTHORIZED,
            reason: "unauthorized".into(),
        })),
    )
    .await;
    close_sink(sink).await;
}

async fn wait_for_first_frame_auth(
    sink: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    stream: &mut futures_util::stream::SplitStream<WebSocket>,
    state: &ApiState,
) -> Option<String> {
    let deadline = tokio::time::Instant::now() + AUTH_TIMEOUT;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            reject_unauth(sink).await;
            return None;
        }
        match tokio::time::timeout(remaining, stream.next()).await {
            Ok(Some(Ok(Message::Ping(payload)))) => {
                if send_msg(sink, Message::Pong(payload)).await.is_err() {
                    return None;
                }
            }
            Ok(Some(Ok(Message::Pong(_)))) => {}
            Ok(Some(Ok(Message::Text(text)))) => match serde_json::from_str::<ClientMsg>(&text) {
                Ok(ClientMsg::Auth { token: supplied }) => {
                    let expected = current_token(state);
                    if expected.is_empty()
                        || api::constant_time_eq(supplied.as_bytes(), expected.as_bytes())
                    {
                        return Some(supplied);
                    }
                    reject_unauth(sink).await;
                    return None;
                }
                _ => {
                    reject_unauth(sink).await;
                    return None;
                }
            },
            Ok(Some(Ok(Message::Binary(_)))) => {
                reject_unauth(sink).await;
                return None;
            }
            Ok(Some(Ok(Message::Close(_)))) | Ok(None) | Ok(Some(Err(_))) => {
                return None;
            }
            Err(_) => {
                reject_unauth(sink).await;
                return None;
            }
        }
    }
}

#[allow(clippy::too_many_lines)]
async fn handle_socket(
    socket: WebSocket,
    state: Arc<ApiState>,
    hub: WsHub,
    _guard: WsConnectionGuard,
    already_authed: Option<String>,
) {
    let (mut sink, mut stream) = socket.split();

    let mut authed_token = match already_authed {
        Some(t) => t,
        None => match wait_for_first_frame_auth(&mut sink, &mut stream, &state).await {
            Some(t) => t,
            None => return,
        },
    };

    // Subscribe before building the snapshot so concurrent updates are never missed.
    let mut broadcast_rx = hub.subscribe();
    let mut shutdown_rx = hub.subscribe_shutdown();

    if send_json(&mut sink, &json!({ "type": "auth_ok", "version": VERSION }))
        .await
        .is_err()
    {
        return;
    }

    let projects = projects_json_async(&state).await;
    if send_json(
        &mut sink,
        &json!({
            "type": "snapshot",
            "version": VERSION,
            "projects": projects,
        }),
    )
    .await
    .is_err()
    {
        return;
    }

    let mut sub_run_id: Option<String> = None;
    let mut sub_log: Option<LogSub> = None;
    let mut ping_interval = tokio::time::interval(PING_INTERVAL);
    ping_interval.tick().await; // consume immediate initial tick

    loop {
        tokio::select! {
            res = shutdown_rx.changed() => {
                if res.is_err() || *shutdown_rx.borrow() {
                    close_sink(&mut sink).await;
                    break;
                }
            }
            _ = ping_interval.tick() => {
                if send_msg(&mut sink, Message::Ping(Bytes::new())).await.is_err() {
                    break;
                }
            }
            incoming = stream.next() => {
                match incoming {
                    Some(Ok(Message::Text(text))) => {
                        let Ok(cmd) = serde_json::from_str::<ClientMsg>(&text) else {
                            log::debug!("ws: ignoring unrecognised client frame");
                            continue;
                        };
                        match cmd {
                            ClientMsg::Auth { token: supplied } => {
                                let expected = current_token(&state);
                                if expected.is_empty()
                                    || api::constant_time_eq(supplied.as_bytes(), expected.as_bytes())
                                {
                                    authed_token = supplied;
                                    if send_json(&mut sink, &json!({ "type": "auth_ok", "version": VERSION })).await.is_err() {
                                        break;
                                    }
                                } else {
                                    reject_unauth(&mut sink).await;
                                    break;
                                }
                            }
                            ClientMsg::SubscribeRun { run_id } => {
                                if !api::is_valid_segment(&run_id) {
                                    sub_run_id = None;
                                    if send_json(&mut sink, &json!({
                                        "type": "run_not_found",
                                        "run_id": run_id,
                                    })).await.is_err() {
                                        break;
                                    }
                                    continue;
                                }
                                sub_run_id = Some(run_id.clone());
                                match lookup_run_live_or_disk_async(&state, run_id.clone()).await {
                                    Some(run) => {
                                        if send_json(&mut sink, &json!({
                                            "type": "run_updated",
                                            "run": api::run_json(&run),
                                        })).await.is_err() {
                                            break;
                                        }
                                    }
                                    None => {
                                        if send_json(&mut sink, &json!({
                                            "type": "run_not_found",
                                            "run_id": run_id,
                                        })).await.is_err() {
                                            break;
                                        }
                                    }
                                }
                            }
                            ClientMsg::UnsubscribeRun => {
                                sub_run_id = None;
                            }
                            ClientMsg::SubscribeLog { run_id, step, tail } => {
                                if !api::is_valid_segment(&run_id) {
                                    sub_log = None;
                                    if send_json(&mut sink, &json!({
                                        "type": "log_snapshot",
                                        "run_id": run_id,
                                        "step": step,
                                        "tail": tail,
                                        "text": "",
                                        "offset": 0,
                                    })).await.is_err() {
                                        break;
                                    }
                                    continue;
                                }
                                let (text, offset) = read_log_snapshot_async(&state, run_id.clone(), step, tail).await;
                                sub_log = Some(LogSub {
                                    run_id: run_id.clone(),
                                    step,
                                    tail,
                                    last_offset: offset,
                                });
                                if send_json(&mut sink, &json!({
                                    "type": "log_snapshot",
                                    "run_id": run_id,
                                    "step": step,
                                    "tail": tail,
                                    "text": text,
                                    "offset": offset,
                                })).await.is_err() {
                                    break;
                                }
                            }
                            ClientMsg::UnsubscribeLog => {
                                sub_log = None;
                            }
                            ClientMsg::Ping => {
                                if send_json(&mut sink, &json!({ "type": "pong" })).await.is_err() {
                                    break;
                                }
                            }
                        }
                    }
                    Some(Ok(Message::Ping(payload))) => {
                        if send_msg(&mut sink, Message::Pong(payload)).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(Message::Pong(_))) | Some(Ok(Message::Binary(_))) => {}
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => {
                        break;
                    }
                }
            }
            ev = broadcast_rx.recv() => {
                match ev {
                    Ok(WsBroadcast::ProjectsUpdated) => {
                        let projects = projects_json_async(&state).await;
                        if send_json(&mut sink, &json!({
                            "type": "projects_updated",
                            "projects": projects,
                        })).await.is_err() {
                            break;
                        }
                    }
                    Ok(WsBroadcast::RunUpdated(run)) => {
                        if sub_run_id.as_deref() == Some(run.id.as_str())
                            && send_json(&mut sink, &json!({
                                "type": "run_updated",
                                "run": api::run_json(&run),
                            })).await.is_err()
                        {
                            break;
                        }
                    }
                    Ok(WsBroadcast::LogChunk { run_id, step, chunk, offset }) => {
                        if let Some(sub) = sub_log.as_mut() {
                            if sub.run_id == run_id && sub.step == step {
                                if offset > 0 && offset <= sub.last_offset {
                                    continue;
                                }
                                let emit_chunk: &str = if offset > sub.last_offset {
                                    let prev_end = offset.saturating_sub(chunk.len() as u64);
                                    let slice = if prev_end < sub.last_offset {
                                        let mut skip = (sub.last_offset - prev_end) as usize;
                                        while skip < chunk.len() && !chunk.is_char_boundary(skip) {
                                            skip += 1;
                                        }
                                        &chunk[skip.min(chunk.len())..]
                                    } else {
                                        chunk.as_str()
                                    };
                                    sub.last_offset = offset;
                                    slice
                                } else {
                                    chunk.as_str()
                                };
                                if emit_chunk.is_empty() {
                                    continue;
                                }
                                if send_json(&mut sink, &json!({
                                    "type": "log_chunk",
                                    "run_id": run_id,
                                    "step": step,
                                    "chunk": emit_chunk,
                                    "offset": offset,
                                })).await.is_err() {
                                    break;
                                }
                            }
                        }
                    }
                    Ok(WsBroadcast::ConfigUpdated) => {
                        let expected = current_token(&state);
                        if !expected.is_empty()
                            && !api::constant_time_eq(authed_token.as_bytes(), expected.as_bytes())
                        {
                            reject_unauth(&mut sink).await;
                            break;
                        }
                        if send_json(&mut sink, &json!({ "type": "config_updated" })).await.is_err() {
                            break;
                        }
                    }
                    Ok(WsBroadcast::WebhookReceived {
                        event,
                        repo,
                        delivery_id,
                        outcome,
                        received_at,
                        queued,
                    }) => {
                        if send_json(&mut sink, &json!({
                            "type": "webhook_received",
                            "event": event,
                            "repo": repo,
                            "delivery_id": delivery_id,
                            "outcome": outcome,
                            "received_at": received_at,
                            "queued": queued,
                        })).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        let expected = current_token(&state);
                        if !expected.is_empty()
                            && !api::constant_time_eq(authed_token.as_bytes(), expected.as_bytes())
                        {
                            reject_unauth(&mut sink).await;
                            break;
                        }
                        log::debug!("ws client lagged by {skipped} messages; resyncing");
                        let projects = projects_json_async(&state).await;
                        if send_json(&mut sink, &json!({
                            "type": "projects_updated",
                            "projects": projects,
                        })).await.is_err() {
                            break;
                        }
                        if let Some(ref id) = sub_run_id {
                            if let Some(run) = lookup_run_live_or_disk_async(&state, id.clone()).await {
                                if send_json(&mut sink, &json!({
                                    "type": "run_updated",
                                    "run": api::run_json(&run),
                                })).await.is_err() {
                                    break;
                                }
                            }
                        }
                        if let Some(ref mut sub) = sub_log {
                            let (text, offset) = read_log_snapshot_async(
                                &state,
                                sub.run_id.clone(),
                                sub.step,
                                sub.tail,
                            )
                            .await;
                            sub.last_offset = offset;
                            if send_json(&mut sink, &json!({
                                "type": "log_snapshot",
                                "run_id": sub.run_id,
                                "step": sub.step,
                                "tail": sub.tail,
                                "text": text,
                                "offset": offset,
                            })).await.is_err() {
                                break;
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_log_snapshot_exact_offset_and_full_cap() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("0.log");
        let content = "line1\nline2\nline3\n";
        std::fs::write(&path, content).unwrap();

        let (text, offset) = read_log_snapshot_from_path(&path, Some(2));
        assert_eq!(text, "line2\nline3\n");
        assert_eq!(offset, content.len() as u64);

        let (full_text, full_offset) = read_log_snapshot_from_path(&path, None);
        assert_eq!(full_text, content);
        assert_eq!(full_offset, content.len() as u64);

        // Oversized full log is capped to trailing 8 MiB with notice prefix and exact file end offset.
        let big_path = dir.path().join("big.log");
        let line = "0123456789abcdef0123456789abcdef\n"; // 33 bytes
        let repeats = (MAX_WS_LOG_SNAPSHOT_BYTES as usize / line.len()) + 500;
        let mut big = String::with_capacity(repeats * line.len() + 16);
        for _ in 0..repeats {
            big.push_str(line);
        }
        big.push_str("TAIL_MARKER\n");
        std::fs::write(&big_path, &big).unwrap();

        let (capped_text, capped_offset) = read_log_snapshot_from_path(&big_path, None);
        assert_eq!(capped_offset, big.len() as u64);
        assert!(capped_text.starts_with("[lunacy] log exceeds 8 MiB;"));
        assert!(capped_text.ends_with("TAIL_MARKER\n"));
        assert!(capped_text.len() <= (MAX_WS_LOG_SNAPSHOT_BYTES as usize) + 256);
    }
}
