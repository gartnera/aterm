//! Optional debug/scripting socket for end-to-end tests.
//!
//! Enabled only when the environment variable `ATERM_DEBUG_SOCK` is set to a
//! filesystem path. When enabled, a Unix domain socket is bound at that path
//! and accepts line-delimited JSON requests. Each connection can issue any
//! number of requests; the server processes them one at a time and writes a
//! single JSON response per request.
//!
//! This is a back door for tests — never enable it in user-facing builds. It
//! lets the test harness query the terminal's state (visible text, tab list,
//! window title, hovered URL) and invoke the same actions a user could trigger
//! (typing, tab manipulation, font size), without depending on pixel-level
//! event injection.
//!
//! Protocol example:
//!     -> {"cmd":"snapshot_text"}
//!     <- {"ok":true,"data":{"lines":["$ ","",""]}}
//!     -> {"cmd":"type_bytes","bytes":[101,99,104,111,13]}
//!     <- {"ok":true}

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::time::Duration;

use crossbeam_channel::{bounded, unbounded, Receiver, Sender};
use serde::{Deserialize, Serialize};
use winit::event_loop::EventLoopProxy;

use crate::WakeEvent;

/// Hard cap on how long the socket thread will wait for the main loop to
/// respond. If the main loop is stuck, the client gets a "timeout" error
/// rather than hanging forever.
const REPLY_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    /// Return the grid contents as one string per row (trailing whitespace
    /// trimmed). The active tab's viewport is used.
    SnapshotText,
    /// List all tabs with their (1-based) display index, title, and whether
    /// they are active.
    Tabs,
    /// Return the current OS window title (mirrors the active tab's title).
    Title,
    /// Inject bytes into the active tab's PTY, as if the user typed them.
    TypeBytes { bytes: Vec<u8> },
    /// Spawn a new tab sized to the current window.
    CreateTab,
    /// Close the active tab. If this empties the tab list, aterm will exit.
    CloseTab,
    /// Switch to the given zero-based tab index, if it exists.
    SelectTab { index: usize },
    /// Adjust the font size by `delta` points (clamped to [6, 72]).
    FontSize { delta: f32 },
    /// Restore the font size to the value loaded from config (or the default).
    FontSizeReset,
    /// Resolve the URL (if any) at the given viewport cell. When `ctrl` is
    /// true, the URL regex sweep is performed; otherwise only OSC 8 links are
    /// reported (mirroring the modifier-gated UI behavior).
    HoverUrl { row: usize, col: usize, ctrl: bool },
}

#[derive(Debug, Serialize)]
pub struct Response {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Response {
    pub fn ok_empty() -> Self {
        Self {
            ok: true,
            data: None,
            error: None,
        }
    }
    pub fn ok_data(data: serde_json::Value) -> Self {
        Self {
            ok: true,
            data: Some(data),
            error: None,
        }
    }
    pub fn err(msg: impl Into<String>) -> Self {
        Self {
            ok: false,
            data: None,
            error: Some(msg.into()),
        }
    }
}

/// One pending request handed from the socket thread to the main event loop.
/// The main loop processes it and sends a Response back through `reply`.
pub struct PendingRequest {
    pub request: Request,
    pub reply: Sender<Response>,
}

/// Bind the debug socket and spawn the listener thread. Returns the receiving
/// end of the request channel that the main event loop should drain on every
/// wake. Returns None if `ATERM_DEBUG_SOCK` is not set; that is the normal
/// case for user-facing builds.
pub fn start_if_enabled(proxy: EventLoopProxy<WakeEvent>) -> Option<Receiver<PendingRequest>> {
    let path = std::env::var("ATERM_DEBUG_SOCK")
        .ok()
        .filter(|s| !s.is_empty())?;
    // Best-effort cleanup of a stale socket from a previous run.
    let _ = std::fs::remove_file(&path);
    let listener = match UnixListener::bind(&path) {
        Ok(l) => l,
        Err(e) => {
            log::error!("debug socket bind {path}: {e}");
            return None;
        }
    };
    log::info!("debug socket listening at {path}");

    let (req_tx, req_rx) = unbounded::<PendingRequest>();
    std::thread::Builder::new()
        .name("aterm-debug-ipc".into())
        .spawn(move || accept_loop(listener, req_tx, proxy))
        .ok()?;
    Some(req_rx)
}

fn accept_loop(
    listener: UnixListener,
    req_tx: Sender<PendingRequest>,
    proxy: EventLoopProxy<WakeEvent>,
) {
    for incoming in listener.incoming() {
        let stream = match incoming {
            Ok(s) => s,
            Err(e) => {
                log::warn!("debug socket accept: {e}");
                continue;
            }
        };
        let req_tx = req_tx.clone();
        let proxy = proxy.clone();
        std::thread::spawn(move || handle_client(stream, req_tx, proxy));
    }
}

fn handle_client(
    stream: UnixStream,
    req_tx: Sender<PendingRequest>,
    proxy: EventLoopProxy<WakeEvent>,
) {
    let read_stream = match stream.try_clone() {
        Ok(s) => s,
        Err(e) => {
            log::warn!("debug socket clone: {e}");
            return;
        }
    };
    let reader = BufReader::new(read_stream);
    let mut writer = stream;
    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        if line.trim().is_empty() {
            continue;
        }
        let resp = process_one(&line, &req_tx, &proxy);
        let json = serde_json::to_string(&resp)
            .unwrap_or_else(|_| r#"{"ok":false,"error":"failed to serialize response"}"#.into());
        if writeln!(writer, "{json}").is_err() {
            break;
        }
    }
}

fn process_one(
    line: &str,
    req_tx: &Sender<PendingRequest>,
    proxy: &EventLoopProxy<WakeEvent>,
) -> Response {
    let request: Request = match serde_json::from_str(line) {
        Ok(r) => r,
        Err(e) => return Response::err(format!("parse: {e}")),
    };
    let (reply_tx, reply_rx) = bounded(1);
    if req_tx
        .send(PendingRequest {
            request,
            reply: reply_tx,
        })
        .is_err()
    {
        return Response::err("event loop is gone");
    }
    // Wake the winit loop so it drains the channel. Failure here likely means
    // the event loop has exited; the recv_timeout below will also fail, and
    // the client gets a useful error.
    let _ = proxy.send_event(WakeEvent);
    match reply_rx.recv_timeout(REPLY_TIMEOUT) {
        Ok(r) => r,
        Err(_) => Response::err("timed out waiting for event loop"),
    }
}
