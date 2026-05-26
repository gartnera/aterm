//! Integration tests that drive a real aterm process via the debug IPC.
//!
//! These tests require an X display (typically Xvfb). If $DISPLAY is unset,
//! each test logs and returns early instead of panicking, so `cargo test`
//! still passes on machines without an X server.

mod common;

use common::AtermTest;

#[test]
fn boots_and_shows_prompt() {
    require_display!();
    let mut t = AtermTest::spawn();
    // wait_for_prompt is already called by spawn(); double-check the snapshot
    // exposes a non-empty grid that contains a prompt-looking line.
    let lines = t.snapshot_text();
    assert!(
        lines
            .iter()
            .any(|l| l.trim_end().ends_with('$') || l.trim_end().ends_with('#')),
        "no shell prompt in initial grid:\n{}",
        lines.join("\n")
    );
}

#[test]
fn type_bytes_lands_in_pty() {
    require_display!();
    let mut t = AtermTest::spawn();
    t.type_line("echo hello-from-ipc");
    t.wait_for_text("hello-from-ipc");
    let lines = t.snapshot_text();
    // The string appears twice: once as the typed command echoed by the
    // shell, once as the echo's stdout. We just need at least one match.
    assert!(
        lines.iter().any(|l| l.contains("hello-from-ipc")),
        "expected 'hello-from-ipc' in:\n{}",
        lines.join("\n")
    );
}

#[test]
fn create_close_and_select_tabs() {
    require_display!();
    let mut t = AtermTest::spawn();

    assert_eq!(t.tabs().len(), 1, "should start with one tab");
    assert!(t.tabs()[0].active);

    t.create_tab();
    let tabs = t.tabs();
    assert_eq!(tabs.len(), 2);
    assert_eq!(tabs[1].active, true, "new tab should be active");

    t.create_tab();
    assert_eq!(t.tabs().len(), 3);

    // Switch back to tab 0 and verify activation flips.
    t.select_tab(0);
    let tabs = t.tabs();
    assert_eq!(tabs.len(), 3);
    assert!(tabs[0].active);
    assert!(!tabs[1].active);
    assert!(!tabs[2].active);

    // Close the active tab.
    t.close_tab();
    assert_eq!(t.tabs().len(), 2);
}

#[test]
fn font_size_clamped_and_resettable() {
    require_display!();
    let mut t = AtermTest::spawn();

    let initial = t.font_size(0.0);
    assert!(initial > 0.0);

    let larger = t.font_size(3.0);
    assert!(larger > initial, "increasing delta should grow font");

    let huge = t.font_size(200.0);
    assert!(huge <= 72.0, "font size must clamp to 72.0, got {huge}");

    let reset = t.font_size_reset();
    assert!(
        (reset - initial).abs() < 0.001,
        "reset should restore initial"
    );
}

#[test]
fn url_regex_matches_printed_url() {
    require_display!();
    let mut t = AtermTest::spawn();

    t.type_line("echo \"see https://example.com/foo for docs\"");
    t.wait_for_text("https://example.com/foo");

    // Find the row + column where the URL starts in the rendered grid.
    let lines = t.snapshot_text();
    let (row, col) = lines
        .iter()
        .enumerate()
        .find_map(|(r, l)| l.find("https://").map(|c| (r, c)))
        .expect("URL not found in snapshot");

    // Without ctrl, only OSC 8 hyperlinks are reported — plain text URLs
    // should NOT match (the regex is gated on the modifier).
    assert_eq!(t.hover_url(row, col, false), None);

    // With ctrl, the URL regex sweep runs and should hit the printed URI.
    let uri = t.hover_url(row, col, true);
    assert_eq!(uri.as_deref(), Some("https://example.com/foo"));
}

#[test]
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn new_tab_inherits_cwd_from_active() {
    require_display!();
    use std::time::Duration;
    let mut t = AtermTest::spawn();

    // Pick a directory the active tab definitely isn't already in. We use
    // /tmp + a unique suffix so two parallel test runs don't race.
    let dir = format!("/tmp/aterm-cwd-{}-{}", std::process::id(), rand_suffix());
    std::fs::create_dir_all(&dir).expect("mkdir target");

    // cd in the active tab and confirm the shell has applied the change.
    // Echoing a sentinel after `cd` lets us wait without racing on the
    // prompt repaint, which on slow CI can lag the actual chdir.
    t.type_line(&format!("cd {dir}"));
    t.type_line("echo CWD_READY_TAG");
    t.wait_for_text("CWD_READY_TAG");

    // Spawn a new tab — it should be parented at `dir`.
    t.create_tab();
    t.type_line("pwd");
    // Allow extra time on the very first PTY spawn in a CI container.
    t.wait_for_text_within(&dir, Duration::from_secs(8));

    let lines = t.snapshot_text();
    assert!(
        lines.iter().any(|l| l.contains(&dir)),
        "new tab did not inherit cwd {dir:?}; grid was:\n{}",
        lines.join("\n")
    );

    let _ = std::fs::remove_dir(&dir);
}

#[test]
#[cfg(target_os = "linux")]
fn tab_title_shows_cwd_of_foreground_process() {
    require_display!();
    use std::time::{Duration, Instant};
    let mut t = AtermTest::spawn();

    let dir = format!("/tmp/aterm-fgcwd-{}-{}", std::process::id(), rand_suffix());
    std::fs::create_dir_all(&dir).expect("mkdir target");

    // cd into the target dir, then launch a foreground process that just
    // blocks (cat with no args reads stdin forever). Its cwd is `dir`, so
    // the tab label should become "cat (<dir>)" while it runs.
    t.type_line(&format!("cd {dir}"));
    t.type_line("echo FG_READY_TAG");
    t.wait_for_text("FG_READY_TAG");
    t.type_line("cat");

    // We match on the parenthesised cwd specifically because the shell's
    // own OSC-set title may also include the cwd path (bash sets
    // "user@host: <cwd>"); we only want to assert on what *we* add for the
    // foreground process.
    let suffix = format!("({dir})");

    // tabs() recomputes the label live (reads /proc), so poll it directly.
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut last = String::new();
    let mut seen = false;
    while Instant::now() < deadline {
        last = t.tabs()[0].title.clone();
        if last.contains(&suffix) {
            seen = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        seen,
        "tab title did not show foreground cwd suffix {suffix:?}; last title was {last:?}"
    );

    // Quit the foreground process; the label should drop the cwd suffix
    // once the shell is back in the foreground.
    t.type_bytes(&[0x04]); // Ctrl-D closes cat's stdin
    t.type_line("echo FG_DONE_TAG");
    t.wait_for_text("FG_DONE_TAG");

    let deadline = Instant::now() + Duration::from_secs(8);
    let mut cleared = false;
    while Instant::now() < deadline {
        if !t.tabs()[0].title.contains(&suffix) {
            cleared = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(cleared, "tab title kept cwd suffix after foreground process exited");

    let _ = std::fs::remove_dir(&dir);
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn rand_suffix() -> String {
    // Tiny non-crypto entropy source: the low bits of the monotonic clock
    // are plenty to avoid collisions inside one test run.
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    format!("{nanos:08x}")
}

#[test]
fn snapshot_reflects_typed_command_before_enter() {
    require_display!();
    let mut t = AtermTest::spawn();

    // Type without sending CR. The shell should echo each char as we type so
    // it appears in the grid before the command runs.
    t.type_str("ls -la");
    // Give the PTY a moment to echo back.
    t.wait_for_text("ls -la");

    let lines = t.snapshot_text();
    assert!(
        lines.iter().any(|l| l.contains("ls -la")),
        "typed command not visible before Enter:\n{}",
        lines.join("\n")
    );
}
