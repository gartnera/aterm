---
name: e2e-test
description: End-to-end testing for aterm. Primary path is Rust integration tests under tests/integration.rs that drive aterm through its debug IPC socket — use these for any assertion about behavior (PTY output, tab state, OSC titles, URL detection, font size, etc.). INVOKE this skill BEFORE running `cargo test --test integration` or any aterm test under Xvfb — even if you only intend to run existing tests, not write new ones. It documents the prerequisites (Xvfb + libxkbcommon-x11 + Mesa, installable via `scripts/e2e.sh setup` or the Dockerfile), the canonical runner (`scripts/test-docker.sh`), and the native-Linux fallback (`DISPLAY=:99 cargo test --test integration`). If integration tests fail with "debug socket did not appear", "XOpenDisplayFailed", or a missing `libxkbcommon-x11.so.0`, the environment isn't set up — invoke this skill and follow the setup steps instead of declaring the failure environmental and skipping.
---

# E2E testing for aterm

All assertion-style testing lives in `tests/integration.rs`. The shell
harness in `scripts/e2e.sh` exists only for interactive exploration
(start aterm, send a couple of IPC requests by hand, screenshot the
window). If you're writing a check that should run repeatedly, it
belongs in `tests/integration.rs`.

## Writing a new test

Copy the pattern from `tests/integration.rs`:

```rust
#[test]
fn my_scenario() {
    require_display!();
    let mut t = AtermTest::spawn();
    t.type_line("echo specific-output");
    t.wait_for_text("specific-output");
    let lines = t.snapshot_text();
    assert!(lines.iter().any(|l| l.contains("specific-output")));
}
```

The `AtermTest` helper in `tests/common/mod.rs` exposes:

| Method | Purpose |
|---|---|
| `spawn()` | start aterm with its own debug socket; waits for the first shell prompt |
| `snapshot_text()` | grid contents as `Vec<String>` (one per row, trailing whitespace trimmed) |
| `tabs()` | `Vec<TabInfo>` with `{index, title, active}` |
| `title()` | OS window title |
| `type_bytes(&[u8])` / `type_str(&str)` / `type_line(&str)` | inject into the active PTY (`type_line` appends `\r`) |
| `create_tab()` / `close_tab()` / `select_tab(i)` | tab manipulation |
| `font_size(delta)` / `font_size_reset()` | adjust + reset, returns the new size |
| `hover_url(row, col, ctrl)` | probe URL detection, returns `Option<String>` |
| `wait_for_text(needle)` | poll the grid up to 5s, panic with the grid if it never appears |
| `wait_for_text_within(needle, dur)` | same with a caller-supplied deadline |
| `screenshot(label)` | save a PNG to `$ATERM_TEST_ARTIFACTS/` for happy-path captures |
| `recent_log()` / `log_path()` | inspect aterm's stderr from inside a test |

Prefer `wait_for_text` over `thread::sleep` — it polls every 50ms and on
timeout panics with the visible grid embedded in the message.

## Running the tests

### Docker (works on macOS too)

```
./scripts/test-docker.sh                                   # integration only
./scripts/test-docker.sh --                                # all tests
./scripts/test-docker.sh --test integration url_regex_matches_printed_url
```

The image ships Xvfb + Mesa software Vulkan, so no GPU passthrough is
needed. Cargo caches live in named Docker volumes (`aterm-cargo-cache`,
`aterm-cargo-target`) so repeat runs are fast.

### Native Linux

```
DISPLAY=:99 cargo test --test integration
```

Without an X display the tests no-op via `require_display!()` so plain
`cargo test` still passes on machines without Xvfb.

### First-run setup on a fresh machine (or remote container)

If `cargo test --test integration` panics with `XOpenDisplayFailed`,
`libxkbcommon-x11.so.0: cannot open shared object file`, or
`aterm debug socket did not appear` — the runtime deps aren't installed
and/or Xvfb isn't running. This is **not** an environmental failure
to skip past; do this instead:

```
# 1. Install Xvfb + libxkbcommon-x11 + Mesa software Vulkan (one-shot, needs root).
sudo scripts/e2e.sh setup

# 2. Boot Xvfb on :99 (background; openbox + a "ready" log come for free).
scripts/e2e.sh start && scripts/e2e.sh stop   # leaves Xvfb running, kills the ad-hoc aterm

# 3. Run the suite.
DISPLAY=:99 cargo test --test integration --release
```

Inside a remote sandbox where Docker isn't available, the
`scripts/e2e.sh setup` path is the supported way to install the same
package list the Dockerfile uses. Don't skip the integration suite just
because Docker isn't present — installing the apt packages takes ~30s
and the tests run in well under a minute after that.

### CI

`.github/workflows/ci.yml` runs clippy, unit tests, and integration
tests on every push and PR. On failure, `target/test-artifacts/` is
uploaded as a workflow artifact (screenshots + per-test aterm logs).

## Failure artifacts

When an integration test panics, `AtermTest::drop` saves three things to
`$ATERM_TEST_ARTIFACTS/` (default `target/test-artifacts/`, bind-mounted
out of Docker):

1. **`<test_name>_failure.png`** — screenshot of the X root at the moment
   of failure, captured via `import`.
2. **`<test_name>.log`** — the aterm child's stderr, tee'd in real time.
3. **Inline tail** — the last 20 lines of stderr are printed under
   `--- last 20 lines of aterm stderr ---` inside the cargo test output,
   so a quick scroll-back is enough for most diagnosis without leaving
   the terminal.

Default log level is `info,wgpu_core=warn,wgpu_hal=warn`. Override with
`ATERM_LOG=debug` (or any `env_logger` spec) before running tests.

## Debug IPC protocol

Tests use this internally via the helper above; you only need to know
the protocol if you're adding a new command or scripting from a
non-Rust caller. Aterm binds a Unix socket at `$ATERM_DEBUG_SOCK` when
the env var is set and accepts line-delimited JSON requests with a
single-line JSON response per request:

| Cmd | Args | Returns |
|---|---|---|
| `snapshot_text` | – | `{lines: [string]}` |
| `tabs` | – | `{tabs: [{index, title, active}]}` |
| `title` | – | `{title}` |
| `type_bytes` | `bytes: [u8]` | – |
| `create_tab` | – | `{created, active}` |
| `close_tab` | – | `{tabs_remaining}` |
| `select_tab` | `index: usize` | `{active}` |
| `font_size` | `delta: f32` | `{font_size}` |
| `font_size_reset` | – | `{font_size}` |
| `hover_url` | `row, col, ctrl` | `{uri}` or `null` |

## Ad-hoc poking from the shell

For exploratory work (boot aterm, send a couple of IPC requests by
hand), `scripts/e2e.sh` is a tiny wrapper with four subcommands:

```
sudo scripts/e2e.sh setup      # one-time apt install on Linux
scripts/e2e.sh start           # boots Xvfb + openbox + aterm (build first if needed)
scripts/e2e.sh ipc <cmd> [k=v ...]
scripts/e2e.sh ipc raw '<json>'
scripts/e2e.sh stop
```

Examples:

```
scripts/e2e.sh ipc tabs
scripts/e2e.sh ipc type_bytes bytes='[101,99,104,111,32,104,105,13]'   # "echo hi\r"
scripts/e2e.sh ipc snapshot_text
scripts/e2e.sh ipc create_tab
scripts/e2e.sh ipc raw '{"cmd":"font_size","delta":2}'
```

The `ipc` subcommand coerces args that look like numbers / booleans /
JSON arrays; everything else is sent as a string. Use `ipc raw` when
that heuristic guesses wrong.

The log lives at `/tmp/aterm-e2e/aterm.log`; if you want a quick PNG of
the window, `import -window root /tmp/snap.png` is one command and does
not need its own subcommand.
