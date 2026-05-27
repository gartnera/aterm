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
fn new_tab_opens_to_right_of_active() {
    require_display!();
    let mut t = AtermTest::spawn();

    // Build up three tabs: [0, 1, 2], active on 2.
    t.create_tab();
    t.create_tab();
    assert_eq!(t.tabs().len(), 3);

    // Activate the leftmost tab, then open a new one. It should be inserted
    // immediately to the right of tab 0 (index 1), not appended at the end.
    t.select_tab(0);
    t.create_tab();

    let tabs = t.tabs();
    assert_eq!(tabs.len(), 4);
    let active = tabs
        .iter()
        .find(|tb| tb.active)
        .expect("a tab should be active");
    assert_eq!(
        active.index, 1,
        "new tab should open to the right of the active tab, got index {}",
        active.index
    );
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
fn double_click_selects_word() {
    require_display!();
    let mut t = AtermTest::spawn();

    t.type_line("echo lemon:melon kiwi");
    t.wait_for_text("lemon:melon kiwi");

    // Target the echo *output* line, not the echoed command: it sits at
    // column 0 with no prompt prefix, so it never wraps and the column math
    // is independent of the CI shell's prompt length.
    let lines = t.snapshot_text();
    let (row, line) = lines
        .iter()
        .enumerate()
        .find_map(|(r, l)| (l.trim_end() == "lemon:melon kiwi").then(|| (r, l.clone())))
        .expect("echo output line not found in snapshot");

    // Space bounds the word: clicking "kiwi" selects just "kiwi".
    let kiwi_col = line.find("kiwi").expect("kiwi column");
    assert_eq!(t.select_word(row, kiwi_col).as_deref(), Some("kiwi"));

    // ':' is a semantic escape char (alacritty default), so it bounds the
    // word too: clicking "lemon" selects "lemon", not "lemon:melon".
    let lemon_col = line.find("lemon").expect("lemon column");
    assert_eq!(t.select_word(row, lemon_col).as_deref(), Some("lemon"));
}

#[test]
fn url_regex_trims_trailing_punctuation() {
    require_display!();
    let mut t = AtermTest::spawn();

    // URL wrapped in parentheses and followed by a period, as it would appear
    // in prose. The regex greedily includes the trailing `).`, which must be
    // trimmed so the click target is the bare URL.
    t.type_line("echo \"(see https://example.com/foo).\"");
    t.wait_for_text("https://example.com/foo");

    let lines = t.snapshot_text();
    let (row, col) = lines
        .iter()
        .enumerate()
        .find_map(|(r, l)| l.find("https://").map(|c| (r, c)))
        .expect("URL not found in snapshot");

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
    assert!(
        cleared,
        "tab title kept cwd suffix after foreground process exited"
    );

    let _ = std::fs::remove_dir(&dir);
}

#[test]
#[cfg(target_os = "linux")]
fn foreground_cwd_overrides_program_osc_title() {
    // A program setting its own OSC title must NOT suppress the synthesised
    // "name (cwd)". We can't attribute a title to the program that set it: a
    // shell's preexec title (zsh sets one per command) is written just before
    // it hands the tty to the job, so it races the handoff and looks identical
    // to a title the job set itself. So every non-ssh foreground program shows
    // name (cwd) regardless of any title it (or the shell) emitted. This is the
    // regression test for the bug where the cwd suffix vanished for all
    // commands under a title-setting shell.
    require_display!();
    use std::time::Duration;
    let mut t = AtermTest::spawn();

    let dir = format!("/tmp/aterm-osc-{}-{}", std::process::id(), rand_suffix());
    std::fs::create_dir_all(&dir).expect("mkdir target");

    t.type_line(&format!("cd {dir}"));
    t.type_line("echo OSC_READY_TAG");
    t.wait_for_text("OSC_READY_TAG");

    // Launch a foreground subprocess that sets an OSC 0 title and then blocks.
    // Despite the title, tab_label should show "sh (<dir>)".
    t.type_line("sh -c 'printf \"\\033]0;OSC_TITLE_TAG\\007\"; sleep 60'");

    let suffix = format!("({dir})");
    let seen = wait_until(Duration::from_secs(8), || {
        t.tabs()[0].title.contains(&suffix)
    });
    let last = t.tabs()[0].title.clone();
    assert!(
        seen,
        "foreground cwd suffix {suffix:?} should override the program's OSC \
         title; last title was {last:?}"
    );
    assert!(
        last != "OSC_TITLE_TAG",
        "the program's OSC title should not win for a non-ssh program: {last:?}"
    );

    let _ = std::fs::remove_dir(&dir);
}

#[test]
#[cfg(target_os = "linux")]
fn ssh_keeps_its_own_title() {
    // ssh is the one exception: its local cwd is meaningless, so we honour the
    // title forwarded from the remote shell verbatim instead of annotating it
    // with the local cwd. We stand in for the ssh binary with a copy of /bin/sh
    // named "ssh" so process_name() reports "ssh".
    require_display!();
    use std::time::Duration;
    let mut t = AtermTest::spawn();

    let dir = format!("/tmp/aterm-ssh-{}-{}", std::process::id(), rand_suffix());
    std::fs::create_dir_all(&dir).expect("mkdir target");
    let fake_ssh = format!("{dir}/ssh");
    std::fs::copy("/bin/sh", &fake_ssh).expect("copy sh -> ssh");
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake_ssh, std::fs::Permissions::from_mode(0o755))
            .expect("chmod ssh");
    }

    t.type_line(&format!("cd {dir}"));
    t.type_line("echo SSH_READY_TAG");
    t.wait_for_text("SSH_READY_TAG");

    // Run our "ssh" so it sets a title (as a remote shell would, forwarded
    // through ssh) and then blocks while in the foreground.
    t.type_line("./ssh -c 'printf \"\\033]0;user@remote:~/proj\\007\"; sleep 60'");

    let seen = wait_until(Duration::from_secs(8), || {
        t.tabs()[0].title == "user@remote:~/proj"
    });
    let last = t.tabs()[0].title.clone();
    assert!(
        seen,
        "ssh's forwarded title should be shown verbatim, got {last:?}"
    );
    // The local cwd must not be appended for ssh.
    assert!(
        !last.contains(&dir) && !last.contains('('),
        "ssh title should not be annotated with the local cwd: {last:?}"
    );

    let _ = std::fs::remove_file(&fake_ssh);
    let _ = std::fs::remove_dir(&dir);
}

/// Poll `cond` every 100ms until it returns true or `timeout` elapses.
/// Returns whether the condition was observed true.
#[cfg(target_os = "linux")]
fn wait_until(timeout: std::time::Duration, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if cond() {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
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
