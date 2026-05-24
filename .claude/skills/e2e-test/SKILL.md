---
name: e2e-test
description: Drive aterm in a real window under Xvfb on Linux, then capture screenshots for visual inspection. Use when verifying terminal behavior that needs an actual window — input handling, rendering, tab/title sync, URL hover, font zoom, scroll. Skip for pure logic changes that `cargo test` covers. This skill does NOT perform automated screenshot diffing; the user inspects the captured PNGs.
---

# E2E testing for aterm

A headless harness for exercising aterm with synthetic keyboard/mouse events
and grabbing PNGs of the actual window. Useful any time a change might
affect what the user sees or how the window reacts to input, but cannot be
verified by `cargo test` alone.

## When to invoke

- After a render-path change (`src/gfx.rs`).
- After an input change (`src/input.rs`, `src/main.rs` event handling).
- After a tab / window-title / URL / selection change.
- Whenever the user asks to "try it", "open a window", "screenshot", or
  "verify it actually works".

Don't invoke for: pure parser tweaks, config-loading-only changes, CI/doc
changes. Run `cargo test` for those.

## Setup (once per fresh container)

```
sudo scripts/e2e.sh setup
```

Installs apt packages: `xvfb`, `xdotool`, `imagemagick`, `openbox`,
`x11-utils`, `libxkbcommon-x11-0`, `mesa-vulkan-drivers`. Without
`mesa-vulkan-drivers` wgpu fails to create a surface under Xvfb; without
`openbox` xdotool key events have nowhere to deliver to.

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
