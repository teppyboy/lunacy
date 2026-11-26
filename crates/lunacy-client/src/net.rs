//! HTTP transport. Every call is blocking and runs off the UI thread: `rune::Latest` on native
//! (a shared worker pool) and `spawn_local` + `web_sys` `fetch` on web, exactly the pair
//! Rune's own `examples/fetch` documents.

use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::model::ApiError;

/// What a request came back with.
#[derive(Debug, Clone)]
pub enum Reply {
    /// A 2xx body.
    Ok(String),
    /// A non-2xx answer, with the daemon's error body parsed when it is one.
    Err {
        status: u16,
        message: String,
        field: Option<String>,
    },
    /// Nothing answered: connection refused, DNS, TLS, timeout, CORS.
    Down(String),
}

impl Reply {
    pub fn into_json<T: DeserializeOwned>(self) -> std::result::Result<T, Failure> {
        match self {
            Reply::Ok(body) => serde_json::from_str(&body)
                .map_err(|e| Failure::Down(format!("unexpected response: {e}"))),
            Reply::Err {
                status,
                message,
                field,
            } => Err(Failure::Api {
                status,
                message,
                field,
            }),
            Reply::Down(why) => Err(Failure::Down(why)),
        }
    }

    pub fn into_text(self) -> std::result::Result<String, Failure> {
        match self {
            Reply::Ok(body) => Ok(body),
            Reply::Err {
                status,
                message,
                field,
            } => Err(Failure::Api {
                status,
                message,
                field,
            }),
            Reply::Down(why) => Err(Failure::Down(why)),
        }
    }
}

/// A failed request, split by what the UI should say about it.
#[derive(Debug, Clone)]
pub enum Failure {
    /// The daemon answered with an error: show its message, and put it on `field`.
    Api {
        status: u16,
        message: String,
        field: Option<String>,
    },
    /// The daemon is unreachable (or the answer never arrived).
    Down(String),
    /// A 401/403: connected, but the token is missing or wrong.
    Unauthorized,
}

impl Failure {
    pub fn from_reply(reply: &Reply) -> Option<Failure> {
        match reply {
            Reply::Ok(_) => None,
            Reply::Err {
                status,
                message,
                field,
            } => Some(match status {
                401 | 403 => Failure::Unauthorized,
                _ => Failure::Api {
                    status: *status,
                    message: message.clone(),
                    field: field.clone(),
                },
            }),
            Reply::Down(why) => Some(Failure::Down(why.clone())),
        }
    }

    pub fn message(&self) -> String {
        match self {
            Failure::Api {
                status, message, ..
            } => {
                if message.is_empty() {
                    format!("HTTP {status}")
                } else {
                    message.clone()
                }
            }
            Failure::Down(why) => format!("Cannot reach the daemon: {why}"),
            Failure::Unauthorized => {
                "The daemon rejected the token (401). Check it in Connection.".into()
            }
        }
    }

    pub fn field_path(&self) -> Option<&str> {
        match self {
            Failure::Api { field, .. } => field.as_deref(),
            _ => None,
        }
    }
}

pub type Result<T> = std::result::Result<T, Failure>;

/// Parses a non-2xx body as the daemon's `{"error", "field"}` if it is one.
fn error_reply(status: u16, body: &str) -> Reply {
    match serde_json::from_str::<ApiError>(body) {
        Ok(e) if !e.error.is_empty() => Reply::Err {
            status,
            message: e.error,
            field: e.field,
        },
        _ => {
            let trimmed = body.trim();
            let message = if trimmed.is_empty() || trimmed.starts_with('<') {
                // A proxy's HTML error page says nothing useful.
                format!("HTTP {status}")
            } else {
                // Free text from an unknown route is still the best message available.
                let short: String = trimmed.chars().take(300).collect();
                short
            };
            Reply::Err {
                status,
                message,
                field: None,
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Native

#[cfg(not(target_arch = "wasm32"))]
pub fn request(method: &str, url: &str, token: &str, body: Option<Value>) -> Reply {
    let config = || {
        ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(std::time::Duration::from_secs(15)))
            .build()
    };
    let agent = ureq::Agent::new_with_config(config());
    let auth = !token.is_empty();
    // `ureq`'s request builder is type-state: a body changes the type, so each branch builds its
    // own and the two never have to unify.
    let called = match (method, body) {
        ("GET", None) => {
            let mut req = agent.get(url);
            if auth {
                req = req.header("Authorization", format!("Bearer {token}"));
            }
            req.header("Accept", "application/json").call()
        }
        ("PUT", Some(body)) | ("POST", Some(body)) => {
            let mut req = if method == "PUT" {
                agent.put(url)
            } else {
                agent.post(url)
            };
            if auth {
                req = req.header("Authorization", format!("Bearer {token}"));
            }
            req.send_json(body)
        }
        (other, _) => return Reply::Down(format!("unsupported {other} request")),
    };
    let mut response = match called {
        Ok(r) => r,
        Err(e) => return Reply::Down(describe(&e)),
    };
    let status = response.status().as_u16();
    let text = match response.body_mut().read_to_string() {
        Ok(t) => t,
        Err(e) => return Reply::Down(describe(&e)),
    };
    if (200..300).contains(&status) {
        Reply::Ok(text)
    } else {
        error_reply(status, &text)
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn describe(e: &ureq::Error) -> String {
    match e {
        ureq::Error::Io(io) => io.to_string(),
        ureq::Error::Timeout(what) => format!("timed out ({what:?})"),
        ureq::Error::HostNotFound => "host not found".to_string(),
        ureq::Error::StatusCode(code) => format!("HTTP {code}"),
        other => other.to_string(),
    }
}

/// `GET` and parse; the shape every read path uses.
#[cfg(not(target_arch = "wasm32"))]
pub fn get_json<T: DeserializeOwned>(url: &str, token: &str) -> Result<T> {
    let body = request("GET", url, token, None).into_text()?;
    serde_json::from_str(&body).map_err(|e| Failure::Down(format!("unexpected response: {e}")))
}

#[cfg(not(target_arch = "wasm32"))]
pub fn get_text(url: &str, token: &str) -> Result<String> {
    request("GET", url, token, None).into_text()
}

#[cfg(not(target_arch = "wasm32"))]
pub fn send_json(url: &str, token: &str, method: &str, body: Value) -> Reply {
    request(method, url, token, Some(body))
}

// ---------------------------------------------------------------------------------------------
// Web

#[cfg(target_arch = "wasm32")]
pub async fn request(
    method: &'static str,
    url: String,
    token: String,
    body: Option<Value>,
) -> Reply {
    use wasm_bindgen::JsCast;
    use wasm_bindgen_futures::JsFuture;

    let Some(window) = web_sys::window() else {
        return Reply::Down("no browser window".into());
    };
    let headers = match web_sys::Headers::new() {
        Ok(h) => h,
        Err(e) => return Reply::Down(format!("{e:?}")),
    };
    let _ = headers.set("Accept", "application/json");
    if !token.is_empty() {
        if let Err(e) = headers.set("Authorization", &format!("Bearer {token}")) {
            return Reply::Down(format!("{e:?}"));
        }
    }
    let init = web_sys::RequestInit::new();
    init.set_method(method);
    init.set_headers_headers(&headers);
    if let Some(body) = body {
        let _ = headers.set("Content-Type", "application/json");
        init.set_body(&wasm_bindgen::JsValue::from_str(&body.to_string()));
    }
    let promise = window.fetch_with_str_and_init(&url, &init);
    let response: web_sys::Response = match JsFuture::from(promise).await {
        Ok(v) => match v.dyn_into() {
            Ok(r) => r,
            Err(_) => return Reply::Down("unexpected fetch response".into()),
        },
        // Browsers hide why (CORS, DNS, refused) behind one TypeError; there is nothing finer
        // to report than this.
        Err(e) => return Reply::Down(format!("fetch failed ({})", js_message(&e))),
    };
    let status = response.status();
    let text = match response.text() {
        Ok(p) => match JsFuture::from(p).await {
            Ok(v) => v.as_string().unwrap_or_default(),
            Err(e) => return Reply::Down(format!("reading the body failed ({})", js_message(&e))),
        },
        Err(e) => return Reply::Down(format!("{e:?}")),
    };
    if (200..300).contains(&status) {
        Reply::Ok(text)
    } else {
        error_reply(status, &text)
    }
}

#[cfg(target_arch = "wasm32")]
pub fn js_message(e: &wasm_bindgen::JsValue) -> String {
    e.as_string()
        .or_else(|| {
            js_sys::Reflect::get(e, &wasm_bindgen::JsValue::from_str("message"))
                .ok()
                .and_then(|v| v.as_string())
        })
        .unwrap_or_else(|| format!("{e:?}"))
}

/// Query strings are built by the caller; this only joins a base and a path.
pub fn join(base: &str, path: &str) -> String {
    let base = base.trim_end_matches('/');
    if path.starts_with('/') {
        format!("{base}{path}")
    } else {
        format!("{base}/{path}")
    }
}

/// Percent-encodes one path segment (project names and run ids are usually safe already, but a
/// name with a space or a slash must not reshape the URL).
pub fn seg(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// The run id in a `#/runs/{id}` hash, for the daemon's GitHub check `details_url` deep link.
pub fn hash_run_id(hash: &str) -> Option<String> {
    let rest = hash.strip_prefix("#/runs/")?;
    let id = rest
        .split(['?', '&'])
        .next()
        .unwrap_or(rest)
        .trim_end_matches('/');
    let id = percent_decode(id);
    (!id.is_empty()).then_some(id)
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
            if let Some(b) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    #[test]
    fn run_links_decode_from_the_hash() {
        assert_eq!(
            hash_run_id("#/runs/lunarum-1764067200-abc1234").as_deref(),
            Some("lunarum-1764067200-abc1234")
        );
        assert_eq!(hash_run_id("#/runs/a%2Fb").as_deref(), Some("a/b"));
        assert_eq!(hash_run_id("#/runs/").as_deref(), None);
        assert_eq!(hash_run_id("#/projects").as_deref(), None);
        assert_eq!(hash_run_id("").as_deref(), None);
    }

    #[test]
    fn error_bodies_become_messages() {
        match error_reply(400, r#"{"error":"bad run","field":"project[1].ci[0].run"}"#) {
            Reply::Err {
                status,
                message,
                field,
            } => {
                assert_eq!(status, 400);
                assert_eq!(message, "bad run");
                assert_eq!(field.as_deref(), Some("project[1].ci[0].run"));
            }
            other => panic!("{other:?}"),
        }
        match error_reply(502, "<html>bad gateway</html>") {
            Reply::Err { message, .. } => assert_eq!(message, "HTTP 502"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn paths_keep_their_segments() {
        assert_eq!(
            join("http://127.0.0.1:7878/", "/api/projects"),
            "http://127.0.0.1:7878/api/projects"
        );
        assert_eq!(join("http://h:1", "x"), "http://h:1/x");
        assert_eq!(seg("a b/c"), "a%20b%2Fc");
    }

    /// The deep link the daemon puts in a check's `details_url`.
    #[test]
    fn run_hashes_round_trip() {
        assert_eq!(hash_run_id("#/runs/abc-123"), Some("abc-123".into()));
        assert_eq!(hash_run_id("#/runs/abc-123/"), Some("abc-123".into()));
        assert_eq!(hash_run_id("#/runs/abc%20123"), Some("abc 123".into()));
        assert_eq!(hash_run_id("#/runs/"), None);
        assert_eq!(hash_run_id("#/projects/x"), None);
        assert_eq!(hash_run_id(""), None);
    }

    /// A project name or run id must not be able to reshape the request path.
    #[test]
    fn segments_escape_anything_that_could_reshape_a_url() {
        assert_eq!(seg("lunarum"), "lunarum");
        assert_eq!(seg("a/b"), "a%2Fb");
        assert_eq!(seg("a b"), "a%20b");
        assert_eq!(seg("q?x=1"), "q%3Fx%3D1");
    }

    #[test]
    fn joins_avoid_double_slashes() {
        assert_eq!(join("http://h:1/", "/api/x"), "http://h:1/api/x");
        assert_eq!(join("http://h:1", "api/x"), "http://h:1/api/x");
    }
}
