//! Real-time WebSocket client (`GET /api/ws`) for both Native Desktop and Web WASM.
//!
//! - Native uses `tungstenite` (with `rustls-tls-webpki-roots`, pure Rust TLS for Linux and
//!   Windows) on a dedicated background thread, authenticating via `Authorization: Bearer <token>`
//!   during the HTTP upgrade handshake (or sending an immediate empty `{"type":"auth","token":""}`
//!   frame when unconfigured so token-protected daemons reject immediately without waiting for
//!   `AUTH_TIMEOUT`) and waking the UI via `rune::Waker`.
//! - Web uses `web_sys::WebSocket` and always authenticates on `onopen` with
//!   `{"type":"auth","token":...}` (never placing the token in the URL query string), delivering
//!   events directly into `State` via `rune::AppHandle`.

use serde::{Deserialize, Serialize};

use crate::model::{Project, Run};

/// Derives the daemon WebSocket endpoint (`/api/ws`) from an HTTP(S) base URL.
pub fn ws_url(base: &str) -> Option<String> {
    let trimmed = base.trim().trim_end_matches('/');
    if let Some(rest) = trimmed.strip_prefix("https://") {
        (!rest.is_empty()).then(|| format!("wss://{rest}/api/ws"))
    } else if let Some(rest) = trimmed.strip_prefix("http://") {
        (!rest.is_empty()).then(|| format!("ws://{rest}/api/ws"))
    } else if let Some(rest) = trimmed.strip_prefix("wss://") {
        (!rest.is_empty()).then(|| format!("wss://{rest}/api/ws"))
    } else if let Some(rest) = trimmed.strip_prefix("ws://") {
        (!rest.is_empty()).then(|| format!("ws://{rest}/api/ws"))
    } else {
        None
    }
}

/// Client → Server WebSocket JSON frames.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMsg {
    Auth {
        token: String,
    },
    SubscribeRun {
        run_id: String,
    },
    UnsubscribeRun,
    SubscribeLog {
        run_id: String,
        step: usize,
        tail: Option<usize>,
    },
    UnsubscribeLog,
    Ping,
}

/// Server → Client WebSocket JSON frames.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMsg {
    AuthOk {
        #[serde(default)]
        version: String,
    },
    AuthError {
        #[serde(default)]
        error: String,
    },
    Snapshot {
        #[serde(default)]
        version: String,
        #[serde(default)]
        projects: Vec<Project>,
    },
    ProjectsUpdated {
        #[serde(default)]
        projects: Vec<Project>,
    },
    RunUpdated {
        run: Box<Run>,
    },
    #[serde(rename = "run_not_found")]
    RunNotFound {
        #[serde(default)]
        run_id: String,
    },
    LogSnapshot {
        #[serde(default)]
        run_id: String,
        #[serde(default)]
        step: usize,
        #[serde(default)]
        tail: Option<usize>,
        #[serde(default)]
        text: String,
        #[serde(default)]
        offset: u64,
    },
    LogChunk {
        #[serde(default)]
        run_id: String,
        #[serde(default)]
        step: usize,
        #[serde(default)]
        chunk: String,
        #[serde(default)]
        offset: u64,
    },
    ConfigUpdated,
    WebhookReceived {
        #[serde(default)]
        event: String,
        #[serde(default)]
        repo: Option<String>,
        #[serde(default)]
        received_at: Option<String>,
        #[serde(default)]
        queued: bool,
        #[serde(default)]
        delivery_id: Option<String>,
        #[serde(default)]
        outcome: Option<String>,
    },
    Pong,
    #[serde(other)]
    Unknown,
}

/// Events delivered from the WebSocket transport into `State`.
#[derive(Debug, Clone)]
pub enum WsEvent {
    /// Socket handshake succeeded; waiting for `auth_ok` / `snapshot`.
    Opened,
    /// Decoded JSON frame from the daemon.
    Message(ServerMsg),
    /// Server rejected auth (HTTP 401/403, WS close code 4001, or `auth_error`).
    Unauthorized,
    /// Endpoint unsupported (HTTP 404/405/501); fall back to HTTP polling until reconnect.
    Unsupported,
    /// Transient error or disconnect; eligible for backoff reconnect.
    Closed { why: String },
}

/// Keeps at most the last `max_lines` lines in `text` (used when appending `log_chunk` in Tail mode).
pub fn clamp_tail_lines(text: &mut String, max_lines: usize) {
    if max_lines == 0 {
        return;
    }
    let trailing_nl = text.ends_with('\n');
    let line_count = text.lines().count();
    if line_count <= max_lines {
        return;
    }
    let skip = line_count - max_lines;
    let mut cut_at = 0;
    let mut seen = 0;
    for (idx, b) in text.bytes().enumerate() {
        if b == b'\n' {
            seen += 1;
            if seen == skip {
                cut_at = idx + 1;
                break;
            }
        }
    }
    if cut_at > 0 && cut_at <= text.len() {
        text.drain(..cut_at);
        if trailing_nl && !text.ends_with('\n') {
            text.push('\n');
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Native implementation (tungstenite + rustls-tls-webpki-roots)

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use std::io::ErrorKind;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use tungstenite::client::IntoClientRequest;
    use tungstenite::http::header::{HeaderValue, AUTHORIZATION};
    use tungstenite::stream::MaybeTlsStream;
    use tungstenite::{Error as WsError, Message};

    use super::{ws_url, ClientMsg, ServerMsg, WsEvent};

    enum Command {
        Send(ClientMsg),
        Shutdown,
    }

    pub struct WsClient {
        gen: u64,
        cmd_tx: Option<Sender<Command>>,
        shutdown: Option<Arc<AtomicBool>>,
        ev_tx: Sender<(u64, WsEvent)>,
        ev_rx: Receiver<(u64, WsEvent)>,
        waker: Option<rune::Waker>,
    }

    impl Default for WsClient {
        fn default() -> Self {
            let (ev_tx, ev_rx) = mpsc::channel();
            Self {
                gen: 0,
                cmd_tx: None,
                shutdown: None,
                ev_tx,
                ev_rx,
                waker: None,
            }
        }
    }

    impl Drop for WsClient {
        fn drop(&mut self) {
            self.disconnect();
        }
    }

    impl WsClient {
        pub fn set_waker(&mut self, waker: rune::Waker) {
            self.waker = Some(waker);
        }

        pub fn generation(&self) -> u64 {
            self.gen
        }

        pub fn connect(&mut self, base: &str, token: &str) -> bool {
            self.disconnect();
            let Some(url) = ws_url(base) else {
                return false;
            };
            self.gen += 1;
            let gen = self.gen;
            let (cmd_tx, cmd_rx) = mpsc::channel();
            let shutdown = Arc::new(AtomicBool::new(false));
            self.cmd_tx = Some(cmd_tx);
            self.shutdown = Some(Arc::clone(&shutdown));

            let ev_tx = self.ev_tx.clone();
            let waker = self.waker.clone();
            let token = token.to_string();

            if std::thread::Builder::new()
                .name(format!("lunacy-ws-{gen}"))
                .spawn(move || {
                    run_worker(gen, url, token, cmd_rx, shutdown, ev_tx, waker);
                })
                .is_err()
            {
                self.cmd_tx = None;
                self.shutdown = None;
                return false;
            }
            true
        }

        pub fn disconnect(&mut self) {
            if let Some(flag) = self.shutdown.take() {
                flag.store(true, Ordering::Relaxed);
            }
            if let Some(tx) = self.cmd_tx.take() {
                let _ = tx.send(Command::Shutdown);
            }
            // Bump generation so any queued events from the closing thread are dropped.
            self.gen += 1;
        }

        pub fn send(&self, msg: ClientMsg) {
            if let Some(tx) = &self.cmd_tx {
                let _ = tx.send(Command::Send(msg));
            }
        }

        pub fn drain(&self) -> Vec<(u64, WsEvent)> {
            let mut out = Vec::new();
            while let Ok((gen, ev)) = self.ev_rx.try_recv() {
                if gen == self.gen {
                    out.push((gen, ev));
                }
            }
            out
        }
    }

    fn emit(gen: u64, ev: WsEvent, ev_tx: &Sender<(u64, WsEvent)>, waker: &Option<rune::Waker>) {
        let _ = ev_tx.send((gen, ev));
        if let Some(w) = waker {
            w();
        }
    }

    fn run_worker(
        gen: u64,
        url: String,
        token: String,
        cmd_rx: Receiver<Command>,
        shutdown: Arc<AtomicBool>,
        ev_tx: Sender<(u64, WsEvent)>,
        waker: Option<rune::Waker>,
    ) {
        let mut req = match url.into_client_request() {
            Ok(r) => r,
            Err(e) => {
                emit(
                    gen,
                    WsEvent::Closed {
                        why: format!("invalid ws url: {e}"),
                    },
                    &ev_tx,
                    &waker,
                );
                return;
            }
        };
        if !token.is_empty() {
            match HeaderValue::from_str(&format!("Bearer {token}")) {
                Ok(val) => {
                    req.headers_mut().insert(AUTHORIZATION, val);
                }
                Err(e) => {
                    emit(
                        gen,
                        WsEvent::Closed {
                            why: format!("invalid auth header: {e}"),
                        },
                        &ev_tx,
                        &waker,
                    );
                    return;
                }
            }
        }

        let (mut socket, _) = match tungstenite::connect(req) {
            Ok(pair) => pair,
            Err(WsError::Http(resp)) => {
                let code = resp.status().as_u16();
                let ev = match code {
                    401 | 403 => WsEvent::Unauthorized,
                    404 | 405 | 501 => WsEvent::Unsupported,
                    _ => WsEvent::Closed {
                        why: format!("HTTP {code}"),
                    },
                };
                if !shutdown.load(Ordering::Relaxed) {
                    emit(gen, ev, &ev_tx, &waker);
                }
                return;
            }
            Err(e) => {
                if !shutdown.load(Ordering::Relaxed) {
                    emit(gen, WsEvent::Closed { why: e.to_string() }, &ev_tx, &waker);
                }
                return;
            }
        };

        // Short read timeout keeps incoming frames instant while letting outgoing commands &
        // shutdown execute promptly without busy-spinning.
        match socket.get_mut() {
            MaybeTlsStream::Plain(tcp) => {
                let _ = tcp.set_read_timeout(Some(Duration::from_millis(25)));
            }
            MaybeTlsStream::Rustls(tls) => {
                let _ = tls
                    .get_mut()
                    .set_read_timeout(Some(Duration::from_millis(25)));
            }
            _ => {}
        }

        // When no Bearer token was sent on the HTTP upgrade, send an immediate empty auth frame so
        // token-protected daemons reply with `auth_error` + close `4001` right away instead of
        // waiting for the 5-second `AUTH_TIMEOUT`. Daemons without a token accept it cleanly.
        if token.is_empty() {
            if let Ok(json) = serde_json::to_string(&ClientMsg::Auth {
                token: String::new(),
            }) {
                if let Err(e) = socket.send(Message::Text(json.into())) {
                    if !shutdown.load(Ordering::Relaxed) {
                        emit(gen, WsEvent::Closed { why: e.to_string() }, &ev_tx, &waker);
                    }
                    return;
                }
            }
        }

        emit(gen, WsEvent::Opened, &ev_tx, &waker);
        let mut last_ping = Instant::now();

        loop {
            if shutdown.load(Ordering::Relaxed) {
                let _ = socket.close(None);
                return;
            }

            // Drain any pending outgoing client commands.
            loop {
                match cmd_rx.try_recv() {
                    Ok(Command::Shutdown) => {
                        let _ = socket.close(None);
                        return;
                    }
                    Ok(Command::Send(msg)) => {
                        if let Ok(json) = serde_json::to_string(&msg) {
                            if let Err(e) = socket.send(Message::Text(json.into())) {
                                if !shutdown.load(Ordering::Relaxed) {
                                    emit(
                                        gen,
                                        WsEvent::Closed { why: e.to_string() },
                                        &ev_tx,
                                        &waker,
                                    );
                                }
                                return;
                            }
                        }
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        let _ = socket.close(None);
                        return;
                    }
                }
            }

            // Periodic application-level keepalive ping.
            if last_ping.elapsed() >= Duration::from_secs(25) {
                last_ping = Instant::now();
                if let Ok(json) = serde_json::to_string(&ClientMsg::Ping) {
                    if let Err(e) = socket.send(Message::Text(json.into())) {
                        if !shutdown.load(Ordering::Relaxed) {
                            emit(gen, WsEvent::Closed { why: e.to_string() }, &ev_tx, &waker);
                        }
                        return;
                    }
                }
            }

            match socket.read() {
                Ok(Message::Text(txt)) => {
                    if let Ok(msg) = serde_json::from_str::<ServerMsg>(txt.as_str()) {
                        let is_auth_err = matches!(msg, ServerMsg::AuthError { .. });
                        emit(
                            gen,
                            if is_auth_err {
                                WsEvent::Unauthorized
                            } else {
                                WsEvent::Message(msg)
                            },
                            &ev_tx,
                            &waker,
                        );
                        if is_auth_err {
                            let _ = socket.close(None);
                            return;
                        }
                    }
                }
                Ok(Message::Ping(payload)) => {
                    let _ = socket.send(Message::Pong(payload));
                }
                Ok(Message::Pong(_)) | Ok(Message::Binary(_)) | Ok(Message::Frame(_)) => {}
                Ok(Message::Close(frame)) => {
                    let unauthorized = frame.as_ref().is_some_and(|f| u16::from(f.code) == 4001);
                    if !shutdown.load(Ordering::Relaxed) {
                        emit(
                            gen,
                            if unauthorized {
                                WsEvent::Unauthorized
                            } else {
                                WsEvent::Closed {
                                    why: "connection closed".into(),
                                }
                            },
                            &ev_tx,
                            &waker,
                        );
                    }
                    return;
                }
                Err(WsError::Io(ref e))
                    if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
                Err(WsError::ConnectionClosed | WsError::AlreadyClosed) => {
                    if !shutdown.load(Ordering::Relaxed) {
                        emit(
                            gen,
                            WsEvent::Closed {
                                why: "connection closed".into(),
                            },
                            &ev_tx,
                            &waker,
                        );
                    }
                    return;
                }
                Err(e) => {
                    if !shutdown.load(Ordering::Relaxed) {
                        emit(gen, WsEvent::Closed { why: e.to_string() }, &ev_tx, &waker);
                    }
                    return;
                }
            }
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub use native::WsClient;

// ---------------------------------------------------------------------------------------------
// Web WASM implementation (web_sys::WebSocket)

#[cfg(target_arch = "wasm32")]
mod web {
    use wasm_bindgen::closure::Closure;
    use wasm_bindgen::JsCast;

    use crate::state::State;

    use super::{ws_url, ClientMsg, ServerMsg, WsEvent};

    struct ActiveSocket {
        socket: web_sys::WebSocket,
        _on_open: Closure<dyn FnMut(web_sys::Event)>,
        _on_message: Closure<dyn FnMut(web_sys::MessageEvent)>,
        _on_error: Closure<dyn FnMut(web_sys::ErrorEvent)>,
        _on_close: Closure<dyn FnMut(web_sys::CloseEvent)>,
    }

    impl Drop for ActiveSocket {
        fn drop(&mut self) {
            self.socket.set_onopen(None);
            self.socket.set_onmessage(None);
            self.socket.set_onerror(None);
            self.socket.set_onclose(None);
            let _ = self.socket.close();
        }
    }

    #[derive(Default)]
    pub struct WsClient {
        gen: u64,
        handle: Option<rune::AppHandle<State>>,
        active: Option<ActiveSocket>,
    }

    impl WsClient {
        pub fn attach(&mut self, handle: rune::AppHandle<State>) {
            self.handle = Some(handle);
        }

        pub fn generation(&self) -> u64 {
            self.gen
        }

        pub fn connect(&mut self, base: &str, token: &str) -> bool {
            self.disconnect();
            let Some(handle) = self.handle.clone() else {
                return false;
            };
            let Some(url) = ws_url(base) else {
                return false;
            };
            let socket = match web_sys::WebSocket::new(&url) {
                Ok(ws) => ws,
                Err(_) => return false,
            };

            self.gen += 1;
            let gen = self.gen;

            // onopen: always send {"type":"auth","token":"..."} (even when empty) so
            // token-protected daemons reject immediately with 4001 instead of stalling 5s on
            // AUTH_TIMEOUT, while open daemons accept it cleanly.
            let open_socket = socket.clone();
            let open_token = token.to_string();
            let open_handle = handle.clone();
            let on_open = Closure::<dyn FnMut(web_sys::Event)>::new(move |_ev: web_sys::Event| {
                if let Ok(json) = serde_json::to_string(&ClientMsg::Auth {
                    token: open_token.clone(),
                }) {
                    let _ = open_socket.send_with_str(&json);
                }
                open_handle.update_if(move |s, tx| s.handle_ws_event(gen, WsEvent::Opened, tx));
            });

            // onmessage: decode JSON ServerMsg and deliver to State.
            let msg_handle = handle.clone();
            let on_message = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(
                move |ev: web_sys::MessageEvent| {
                    let Some(txt) = ev.data().as_string() else {
                        return;
                    };
                    if let Ok(msg) = serde_json::from_str::<ServerMsg>(&txt) {
                        let ws_ev = if matches!(msg, ServerMsg::AuthError { .. }) {
                            WsEvent::Unauthorized
                        } else {
                            WsEvent::Message(msg)
                        };
                        msg_handle.update_if(move |s, tx| s.handle_ws_event(gen, ws_ev, tx));
                    }
                },
            );

            // onerror: browsers always follow onerror with onclose, so keep silent here to avoid
            // double-reporting or swallowing close code 4001.
            let on_error =
                Closure::<dyn FnMut(web_sys::ErrorEvent)>::new(move |_ev: web_sys::ErrorEvent| {});

            // onclose: map close code 4001 -> Unauthorized, else Closed.
            let close_handle = handle;
            let on_close =
                Closure::<dyn FnMut(web_sys::CloseEvent)>::new(move |ev: web_sys::CloseEvent| {
                    let code = ev.code();
                    let ws_ev = if code == 4001 {
                        WsEvent::Unauthorized
                    } else {
                        WsEvent::Closed {
                            why: format!("websocket closed ({code})"),
                        }
                    };
                    close_handle.update_if(move |s, tx| s.handle_ws_event(gen, ws_ev, tx));
                });

            socket.set_onopen(Some(on_open.as_ref().unchecked_ref()));
            socket.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
            socket.set_onerror(Some(on_error.as_ref().unchecked_ref()));
            socket.set_onclose(Some(on_close.as_ref().unchecked_ref()));

            self.active = Some(ActiveSocket {
                socket,
                _on_open: on_open,
                _on_message: on_message,
                _on_error: on_error,
                _on_close: on_close,
            });
            true
        }

        pub fn disconnect(&mut self) {
            self.active = None;
            self.gen += 1;
        }

        pub fn send(&self, msg: ClientMsg) {
            let Some(active) = &self.active else {
                return;
            };
            if active.socket.ready_state() != web_sys::WebSocket::OPEN {
                return;
            }
            if let Ok(json) = serde_json::to_string(&msg) {
                let _ = active.socket.send_with_str(&json);
            }
        }

        pub fn drain(&self) -> Vec<(u64, WsEvent)> {
            Vec::new()
        }
    }
}

#[cfg(target_arch = "wasm32")]
pub use web::WsClient;

// ---------------------------------------------------------------------------------------------
// Tests

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ws_url_converts_http_and_https_cleanly() {
        assert_eq!(
            ws_url("http://127.0.0.1:7878").as_deref(),
            Some("ws://127.0.0.1:7878/api/ws")
        );
        assert_eq!(
            ws_url("https://lunacy.example.com/").as_deref(),
            Some("wss://lunacy.example.com/api/ws")
        );
        assert_eq!(
            ws_url("https://lunacy.example.com/prefix/").as_deref(),
            Some("wss://lunacy.example.com/prefix/api/ws")
        );
        assert_eq!(ws_url(""), None);
        assert_eq!(ws_url("http://"), None);
    }

    #[test]
    fn client_messages_serialize_to_exact_wire_contract() {
        assert_eq!(
            serde_json::to_value(ClientMsg::Auth {
                token: "tok".into()
            })
            .unwrap(),
            serde_json::json!({"type": "auth", "token": "tok"})
        );
        assert_eq!(
            serde_json::to_value(ClientMsg::SubscribeRun {
                run_id: "r-1".into()
            })
            .unwrap(),
            serde_json::json!({"type": "subscribe_run", "run_id": "r-1"})
        );
        assert_eq!(
            serde_json::to_value(ClientMsg::UnsubscribeRun).unwrap(),
            serde_json::json!({"type": "unsubscribe_run"})
        );
        assert_eq!(
            serde_json::to_value(ClientMsg::SubscribeLog {
                run_id: "r-1".into(),
                step: 2,
                tail: Some(400),
            })
            .unwrap(),
            serde_json::json!({"type": "subscribe_log", "run_id": "r-1", "step": 2, "tail": 400})
        );
        assert_eq!(
            serde_json::to_value(ClientMsg::SubscribeLog {
                run_id: "r-1".into(),
                step: 2,
                tail: None,
            })
            .unwrap(),
            serde_json::json!({"type": "subscribe_log", "run_id": "r-1", "step": 2, "tail": null})
        );
        assert_eq!(
            serde_json::to_value(ClientMsg::UnsubscribeLog).unwrap(),
            serde_json::json!({"type": "unsubscribe_log"})
        );
        assert_eq!(
            serde_json::to_value(ClientMsg::Ping).unwrap(),
            serde_json::json!({"type": "ping"})
        );
    }

    #[test]
    fn server_messages_deserialize_all_variants_and_unknown() {
        let auth_ok: ServerMsg =
            serde_json::from_str(r#"{"type":"auth_ok","version":"0.2.0"}"#).unwrap();
        assert!(matches!(auth_ok, ServerMsg::AuthOk { version } if version == "0.2.0"));

        let auth_err: ServerMsg =
            serde_json::from_str(r#"{"type":"auth_error","error":"unauthorized"}"#).unwrap();
        assert!(matches!(auth_err, ServerMsg::AuthError { error } if error == "unauthorized"));

        let snap: ServerMsg =
            serde_json::from_str(r#"{"type":"snapshot","version":"0.2.0","projects":[]}"#).unwrap();
        assert!(
            matches!(snap, ServerMsg::Snapshot { version, projects } if version == "0.2.0" && projects.is_empty())
        );

        let log_snap: ServerMsg = serde_json::from_str(
            r#"{"type":"log_snapshot","run_id":"r1","step":0,"tail":400,"text":"hi\n","offset":3}"#,
        )
        .unwrap();
        assert!(matches!(
            log_snap,
            ServerMsg::LogSnapshot { run_id, step: 0, tail: Some(400), text, offset: 3 }
                if run_id == "r1" && text == "hi\n"
        ));

        let log_chunk: ServerMsg = serde_json::from_str(
            r#"{"type":"log_chunk","run_id":"r1","step":0,"chunk":"next\n","offset":8}"#,
        )
        .unwrap();
        assert!(matches!(
            log_chunk,
            ServerMsg::LogChunk { run_id, step: 0, chunk, offset: 8 }
                if run_id == "r1" && chunk == "next\n"
        ));

        let wh: ServerMsg = serde_json::from_str(
            r#"{"type":"webhook_received","event":"push","repo":"a/b","received_at":"2026-01-01T00:00:00Z","queued":true}"#,
        )
        .unwrap();
        assert!(matches!(
            wh,
            ServerMsg::WebhookReceived { event, queued: true, .. } if event == "push"
        ));

        let not_found: ServerMsg =
            serde_json::from_str(r#"{"type":"run_not_found","run_id":"r-missing"}"#).unwrap();
        assert!(matches!(
            not_found,
            ServerMsg::RunNotFound { run_id } if run_id == "r-missing"
        ));

        let future: ServerMsg =
            serde_json::from_str(r#"{"type":"brand_new_event","foo":1}"#).unwrap();
        assert!(matches!(future, ServerMsg::Unknown));
    }

    #[test]
    fn clamp_tail_lines_preserves_last_n_lines() {
        let mut s = "1\n2\n3\n4\n5\n".to_string();
        clamp_tail_lines(&mut s, 3);
        assert_eq!(s, "3\n4\n5\n");

        let mut no_nl = "a\nb\nc\nd".to_string();
        clamp_tail_lines(&mut no_nl, 2);
        assert_eq!(no_nl, "c\nd");

        let mut short = "x\ny\n".to_string();
        clamp_tail_lines(&mut short, 5);
        assert_eq!(short, "x\ny\n");
    }
}
