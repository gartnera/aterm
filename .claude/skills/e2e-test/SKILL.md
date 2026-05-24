---
name: e2e-test
description: Drive aterm in a real window under Xvfb on Linux. Two driving modes: (1) the debug IPC socket — semantic JSON commands for typing, tab control, snapshot-as-text, hover-url; use this for assertions and `cargo test --test integration`. (2) xdotool synthetic input — use this when you specifically need to verify rendering or the input-event path; pairs with screenshot inspection (no automated diffing). Prefer IPC when both work.
---

# E2E testing for aterm

Two complementary harnesses sit on top of the same Xvfb + openbox + aterm
stack:

| Concern | Use |
|---|---|
| "Does the PTY/state/title/URL detection behave correctly?" | **Debug IPC** (`scripts/e2e.sh ipc …` or Rust integration tests) |
| "Does the actual rendering look right?" | **xdotool + screenshot** (`scripts/e2e.sh type/key/snap`) |
| "Does an input event flow through winit correctly?" | xdotool path |
| "Did I break tab-switching logic?" | IPC — far less brittle than coord-based clicks |

IPC is the default. Screenshots are reserved for changes that touch render
or input dispatch.

## When to invoke

- After a render-path change (`src/gfx.rs`).
- After an input change (`src/input.rs`, `src/main.rs` event handling).
- After a tab / window-title / URL / selection change.
- Whenever the user asks to "try it", "open a window", "screenshot", or
  "verify it actually works".

Don't invoke for: pure parser tweaks, config-loading-only changes, CI/doc
changes. Run `cargo test` for those.

## Setup

For the **Rust integration tests** you don't need to install anything —
just run `./scripts/test-docker.sh`. Docker handles every dependency.

For the **screenshot/xdotool driver** on a Linux host (when you actually
want to look at the rendered window during development), install the
apt deps once:

```
sudo scripts/e2e.sh setup
```

Installs: `xvfb`, `xdotool`, `imagemagick`, `openbox`, `x11-utils`,
`libxkbcommon-x11-0`, `mesa-vulkan-drivers`. Without `mesa-vulkan-drivers`
wgpu fails to create a surface under Xvfb; without `openbox` xdotool key
events have nowhere to deliver to.

## Lifecycle

```
scripts/e2e.sh start         # boots Xvfb + openbox + aterm (builds if needed)
scripts/e2e.sh stop          # tear down
scripts/e2e.sh restart       # stop + start
```

`start` writes runtime state (display, window id, pids, screenshot
counter) to `/tmp/aterm-e2e/state.env`. Every subsequent command reads
that file, so subcommands work across separate Bash invocations.

## Interacting

```
scripts/e2e.sh type "echo hello"     # types literal text (does NOT press Enter)
scripts/e2e.sh key Return            # press a key or chord
scripts/e2e.sh key ctrl+l            # standard xdotool chord syntax
scripts/e2e.sh key super+t           # Super == Cmd on macOS keybindings
scripts/e2e.sh click [X Y]           # left-click at window-relative coords (default = center)
scripts/e2e.sh hover [-m MOD] X Y    # move mouse to window-relative X Y, optionally holding MOD
scripts/e2e.sh hover -m ctrl -l url_hover 150 55
                                     # ^ holds Ctrl during the move, snaps a screenshot
                                     #   labeled "url_hover", then releases Ctrl
```

X / Y are **window-relative** in physical pixels (origin = top-left of the
aterm window). The script auto-translates them to root coordinates using
the live window position (openbox repositions windows on map).

## Screenshots

```
scripts/e2e.sh snap LABEL
```

Writes `/tmp/aterm-e2e/NNN_LABEL.png` where NNN is a zero-padded counter
that increments per snap. Prints the resulting path.

You (Claude) should inspect each PNG via SendUserFile to the user, with
a caption describing what scenario it shows. There is no automated diff —
the human verifies the visual.

## Observing state

```
scripts/e2e.sh title        # current OS window title (proves OSC 0/2 sync)
scripts/e2e.sh log [N]      # tail aterm.log (default 30 lines)
scripts/e2e.sh state        # dump the saved state.env
```

## Typical session

```
sudo scripts/e2e.sh setup        # if not done already
scripts/e2e.sh start
scripts/e2e.sh snap baseline

scripts/e2e.sh type 'echo hello' && scripts/e2e.sh key Return
scripts/e2e.sh snap after_echo

scripts/e2e.sh key super+t       # new tab
scripts/e2e.sh snap two_tabs

scripts/e2e.sh type 'echo "Try https://example.com"' && scripts/e2e.sh key Return
scripts/e2e.sh hover -m ctrl -l url_preview 150 55

scripts/e2e.sh stop
```

## Debug IPC (preferred for assertions)

`start` exports `ATERM_DEBUG_SOCK=/tmp/aterm-e2e/aterm.sock` automatically.
aterm binds that socket and accepts line-delimited JSON requests of the
form `{"cmd": "...", ...args}` → `{"ok": true, "data": {...}}` or
`{"ok": false, "error": "..."}`.

Commands (all snake_case):

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

From the shell:

```
scripts/e2e.sh ipc snapshot_text
scripts/e2e.sh ipc type_bytes bytes='[101,99,104,111,13]'   # "echo\r"
scripts/e2e.sh ipc create_tab
scripts/e2e.sh ipc select_tab index=0
scripts/e2e.sh ipc hover_url row=2 col=10 ctrl=true
scripts/e2e.sh ipc raw '{"cmd":"font_size","delta":2}'
```

The script auto-coerces args that look like numbers, booleans, or JSON
arrays; everything else is sent as a string.

## Rust integration tests

`tests/integration.rs` drives the IPC from Rust. The **preferred** way to
run them — and the only way that works on macOS — is in Docker:

```
./scripts/test-docker.sh                    # integration tests only (default)
./scripts/test-docker.sh --                 # ALL tests (unit + integration)
./scripts/test-docker.sh --test integration url_regex_matches_printed_url
```

The container ships Xvfb + Mesa's software Vulkan driver, so it doesn't
need any GPU passthrough. Cargo caches are kept in named Docker volumes
(`aterm-cargo-cache`, `aterm-cargo-target`) so repeated runs are fast.
Failure screenshots are bind-mounted out to `./target/test-artifacts/`.

To run directly on a Linux host with an existing X server:

```
DISPLAY=:99 cargo test --test integration
```

Without an X display the tests no-op via `require_display!()` so a plain
`cargo test` still passes on machines without Xvfb.

### CI

`.github/workflows/ci.yml` runs the integration suite on every push and PR.
It uses the same package list as `scripts/Dockerfile.test`, starts Xvfb +
openbox natively on the ubuntu-latest runner (no Docker layer needed in
CI), and uploads `target/test-artifacts/` as a workflow artifact when any
test fails. Pull the artifact from the failing workflow run to see the
PNGs and per-test stderr logs.

### Failure artifacts

On a panicking test, `AtermTest::drop` saves three things to
`$ATERM_TEST_ARTIFACTS/` (default `target/test-artifacts/`, bind-mounted
out of Docker):

1. **`<test_name>_failure.png`** — screenshot of the X root captured via
   `import` at the moment of failure.
2. **`<test_name>.log`** — the full aterm child's stderr, tee'd in real
   time while the test runs.
3. **Inline tail** — the last 20 lines of stderr are also printed under
   `--- last 20 lines of aterm stderr ---` inside the cargo test output,
   so a quick scroll-back is enough for most diagnosis without leaving
   the terminal.

Default log level is `info,wgpu_core=warn,wgpu_hal=warn` — quiet enough
that the relevant lines aren't buried, verbose enough to catch surface
errors, PTY exits, debug-socket lifecycle. Set `ATERM_LOG=debug` (or any
env_logger spec) before running tests to get more detail.

You can also pull the log mid-test:

```rust
let recent = t.recent_log();         // last ~64 KiB of stderr (String)
let path = t.log_path();             // &Path to the on-disk log
let snap = t.screenshot("after_x");  // Option<PathBuf>, useful for happy-path captures
```

The scaffolding in `tests/common/mod.rs` exposes an `AtermTest` helper
that spawns a fresh aterm with its own socket, exposes `snapshot_text()`,
`type_line()`, `tabs()`, `create_tab()`, `wait_for_text(needle)`, etc.,
and kills the child on drop. Each test gets a clean process — no shared
scrollback or tab state across tests.

Write a new test by copying the pattern in `tests/integration.rs`:

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

Prefer `wait_for_text` over `thread::sleep` — it polls the IPC up to a
5s deadline and panics with the visible grid attached if the text never
appears.

## Grid coordinates cheat-sheet

The default font is 13px with 1.25 line-height → cells are ~8px wide and
16px tall under Xvfb (scale_factor 1.0). The tab bar is 28px, with 4px
padding below. So:

- Tab bar: y = 0..28
- Row 0 (first grid row): y ≈ 32..48
- Row 1: y ≈ 48..64
- Column 0: x ≈ 6..14
- Column 10: x ≈ 86..94

When targeting text printed by the shell, remember the shell prompt
occupies the first ~25 chars before your command's output.

## Caveats

- Xvfb uses Mesa llvmpipe; shader-only bugs on real GPUs won't reproduce.
- xdotool sends synthesized events. Some apps reject them; aterm via
  winit accepts them as normal X events.
- `xdg-open` (the Linux URL opener) isn't installed by `setup`; Ctrl-click
  on a URL will fail with a logged warning. That's expected — it proves
  the URL detection + open path fired.
- Always `stop` between scenarios that should not share scrollback / tabs.
