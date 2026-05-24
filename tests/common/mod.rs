//! Shared scaffolding for aterm integration tests.
//!
//! Each test spawns the release binary under an existing X display (typically
//! Xvfb provided by the caller via $DISPLAY), connects to the binary's debug
//! IPC socket, and drives it from Rust. Tests are skipped with a clear log
//! line when DISPLAY is not set, so `cargo test` still passes on a developer
//! laptop without an X server.

#![allow(dead_code)]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

/// Check whether the test environment has an X display available. Tests
/// requiring a window should early-return when this is false.
pub fn has_display() -> bool {
    std::env::var("DISPLAY").map(|s| !s.is_empty()).unwrap_or(false)
}

/// Macro that turns a test into a no-op (with a printed reason) if no display
/// is available. Use at the top of every test that calls AtermTest::spawn.
#[macro_export]
macro_rules! require_display {
    () => {
        if !$crate::common::has_display() {
            eprintln!("DISPLAY not set — skipping {}", module_path!());
            return;
        }
    };
}

fn aterm_binary() -> PathBuf {
    // Built by the test harness (cargo test depends on the bin).
    let exe = env!("CARGO_BIN_EXE_aterm");
    PathBuf::from(exe)
}

/// Directory where screenshots from failed tests are written. Override with
/// `ATERM_TEST_ARTIFACTS=/some/path` if you want a different location (e.g.
/// to bind-mount it out of a Docker container).
fn artifacts_dir() -> PathBuf {
    let p = std::env::var("ATERM_TEST_ARTIFACTS")
        .unwrap_or_else(|_| "target/test-artifacts".to_string());
    let pb = PathBuf::from(p);
    let _ = std::fs::create_dir_all(&pb);
    pb
}

/// A running aterm process exposed via its debug IPC socket.
///
/// Drop kills the child and removes the socket. Each test gets its own
/// socket path so they don't collide. If the test was panicking when this
/// struct was dropped, a screenshot of the window is captured to
/// `target/test-artifacts/<test_name>.png` before the child is killed.
pub struct AtermTest {
    child: Child,
    sock_path: PathBuf,
    stream: UnixStream,
    reader: BufReader<UnixStream>,
    /// Filled in by `spawn()` so Drop can name the artifact after the test
    /// that owned this handle.
    test_name: String,
}

impl AtermTest {
    /// Spawn aterm and wait for its debug socket to come up. Panics on
    /// timeout or spawn failure.
    #[track_caller]
    pub fn spawn() -> Self {
        Self::spawn_named(std::thread::current().name().unwrap_or("aterm_test").to_string())
    }

    /// Same as `spawn`, but lets the caller pick the test name used in
    /// failure-screenshot filenames.
    pub fn spawn_named(test_name: String) -> Self {
        let dir = tempfile::tempdir().expect("mktemp");
        let sock_path = dir.path().join("aterm.sock");
        // Leak the tempdir handle: we want the directory to outlive this fn
        // so the socket path stays valid while the child runs. Drop removes
        // the socket file explicitly.
        let _ = dir.keep();

        let mut cmd = Command::new(aterm_binary());
        cmd.env("ATERM_DEBUG_SOCK", &sock_path);
        cmd.env("RUST_LOG", "warn"); // quiet, but surface errors
        cmd.stdout(Stdio::null());
        cmd.stderr(Stdio::piped());
        let child = cmd.spawn().expect("spawn aterm");

        // Poll for the socket to appear and accept connections.
        let deadline = Instant::now() + Duration::from_secs(15);
        let stream = loop {
            if let Ok(s) = UnixStream::connect(&sock_path) {
                break s;
            }
            if Instant::now() > deadline {
                let mut child = child;
                let _ = child.kill();
                let stderr = child
                    .stderr
                    .take()
                    .and_then(|mut s| {
                        let mut buf = String::new();
                        std::io::Read::read_to_string(&mut s, &mut buf).ok().map(|_| buf)
                    })
                    .unwrap_or_default();
                panic!(
                    "aterm debug socket did not appear at {sock_path:?}\naterm stderr:\n{stderr}"
                );
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        stream
            .set_read_timeout(Some(Duration::from_secs(15)))
            .unwrap();
        let reader_stream = stream.try_clone().expect("clone stream");
        let reader = BufReader::new(reader_stream);

        let mut t = AtermTest { child, sock_path, stream, reader, test_name };
        // Wait for the shell to print its initial prompt before handing back
        // control. Tests can then issue commands and trust that the PTY is
        // alive.
        t.wait_for_prompt();
        t
    }

    /// Save a screenshot of the X root window — most useful when a test fails
    /// and you want to see what was on screen. Requires ImageMagick's `import`
    /// to be on PATH and a DISPLAY pointing at an X server we can read.
    /// Returns the path written, or None if the capture failed.
    pub fn screenshot(&self, label: &str) -> Option<PathBuf> {
        let dir = artifacts_dir();
        let path = dir.join(format!("{}_{label}.png", self.test_name));
        let status = Command::new("import")
            .args([
                "-window",
                "root",
                "-silent",
                path.to_str().unwrap_or("/tmp/aterm-snap.png"),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        match status {
            Ok(s) if s.success() => Some(path),
            _ => None,
        }
    }

    /// Send a JSON request and parse the response. Panics on transport error.
    pub fn request(&mut self, req: Value) -> Value {
        let line = serde_json::to_string(&req).expect("serialize");
        writeln!(self.stream, "{line}").expect("write request");
        let mut buf = String::new();
        self.reader.read_line(&mut buf).expect("read response");
        let resp: Value = serde_json::from_str(buf.trim()).expect("parse response");
        if !resp.get("ok").and_then(Value::as_bool).unwrap_or(false) {
            panic!(
                "request {req} failed: {}",
                resp.get("error").and_then(Value::as_str).unwrap_or("?")
            );
        }
        resp.get("data").cloned().unwrap_or(Value::Null)
    }

    pub fn snapshot_text(&mut self) -> Vec<String> {
        let data = self.request(json!({ "cmd": "snapshot_text" }));
        data["lines"]
            .as_array()
            .expect("lines array")
            .iter()
            .map(|v| v.as_str().unwrap_or("").to_string())
            .collect()
    }

    pub fn tabs(&mut self) -> Vec<TabInfo> {
        let data = self.request(json!({ "cmd": "tabs" }));
        data["tabs"]
            .as_array()
            .expect("tabs array")
            .iter()
            .map(|v| TabInfo {
                index: v["index"].as_u64().unwrap_or(0) as usize,
                title: v["title"].as_str().unwrap_or("").to_string(),
                active: v["active"].as_bool().unwrap_or(false),
            })
            .collect()
    }

    pub fn title(&mut self) -> String {
        let data = self.request(json!({ "cmd": "title" }));
        data["title"].as_str().unwrap_or("").to_string()
    }

    pub fn type_bytes(&mut self, bytes: &[u8]) {
        self.request(json!({
            "cmd": "type_bytes",
            "bytes": bytes,
        }));
    }

    pub fn type_str(&mut self, s: &str) {
        self.type_bytes(s.as_bytes());
    }

    /// Type s followed by carriage return — typical "enter the command" call.
    pub fn type_line(&mut self, s: &str) {
        let mut v = s.as_bytes().to_vec();
        v.push(b'\r');
        self.type_bytes(&v);
    }

    pub fn create_tab(&mut self) {
        self.request(json!({ "cmd": "create_tab" }));
    }

    pub fn close_tab(&mut self) {
        self.request(json!({ "cmd": "close_tab" }));
    }

    pub fn select_tab(&mut self, index: usize) {
        self.request(json!({ "cmd": "select_tab", "index": index }));
    }

    pub fn font_size(&mut self, delta: f32) -> f32 {
        let data = self.request(json!({ "cmd": "font_size", "delta": delta }));
        data["font_size"].as_f64().unwrap_or(0.0) as f32
    }

    pub fn font_size_reset(&mut self) -> f32 {
        let data = self.request(json!({ "cmd": "font_size_reset" }));
        data["font_size"].as_f64().unwrap_or(0.0) as f32
    }

    /// Probe the URL detection logic at (row, col). Returns the matched URI
    /// if any. `ctrl=true` runs the URL regex; `false` checks OSC 8 only.
    pub fn hover_url(&mut self, row: usize, col: usize, ctrl: bool) -> Option<String> {
        let data = self.request(json!({
            "cmd": "hover_url",
            "row": row,
            "col": col,
            "ctrl": ctrl,
        }));
        data.get("uri").and_then(Value::as_str).map(str::to_string)
    }

    /// Block until the visible grid contains `needle`. Useful for waiting on
    /// shell output without sleeping a fixed duration. Polls every 50ms.
    pub fn wait_for_text(&mut self, needle: &str) {
        self.wait_for_text_within(needle, Duration::from_secs(5));
    }

    pub fn wait_for_text_within(&mut self, needle: &str, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        loop {
            let lines = self.snapshot_text();
            if lines.iter().any(|l| l.contains(needle)) {
                return;
            }
            if Instant::now() > deadline {
                panic!(
                    "wait_for_text({needle:?}) timed out after {:?}; last grid was:\n{}",
                    timeout,
                    lines.join("\n")
                );
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Wait until the shell's first prompt has appeared. The first prompt is
    /// the one printed before the user has typed anything; we detect it by
    /// looking for a line ending in '$' or '#' (the standard sh/bash markers).
    pub fn wait_for_prompt(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let lines = self.snapshot_text();
            if lines.iter().any(|l| {
                let t = l.trim_end();
                t.ends_with('$') || t.ends_with('#') || t.ends_with("$ ") || t.ends_with("# ")
            }) {
                return;
            }
            if Instant::now() > deadline {
                panic!(
                    "wait_for_prompt timed out; visible grid:\n{}",
                    lines.join("\n")
                );
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for AtermTest {
    fn drop(&mut self) {
        // If the test is unwinding from a panic, grab a screenshot first —
        // the assertion failure will print a path to it so the CI artifact
        // viewer (or the developer on macOS pulling from Docker) can see
        // what the terminal actually looked like at the time of failure.
        if std::thread::panicking() {
            if let Some(p) = self.screenshot("failure") {
                eprintln!("aterm failure screenshot saved: {}", p.display());
            } else {
                eprintln!(
                    "aterm failure: screenshot capture failed (no `import` on PATH \
                     or DISPLAY unset?)"
                );
            }
        }
        // SIGKILL keeps teardown fast; aterm doesn't write any state that
        // needs a graceful shutdown.
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.sock_path);
        // Best-effort: remove the parent tempdir we kept earlier.
        if let Some(parent) = Path::new(&self.sock_path).parent() {
            let _ = std::fs::remove_dir_all(parent);
        }
    }
}

#[derive(Debug, Clone)]
pub struct TabInfo {
    pub index: usize,
    pub title: String,
    pub active: bool,
}
