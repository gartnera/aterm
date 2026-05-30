# aterm

A minimal tabbed terminal built on [`alacritty_terminal`](https://crates.io/crates/alacritty_terminal), `winit`, and `wgpu`.

Reads your existing `alacritty.toml` for fonts, colors, and keybindings.

## Configuration

aterm looks for an `alacritty.toml` in the standard alacritty locations
(`$XDG_CONFIG_HOME/alacritty/alacritty.toml`,
`~/.config/alacritty/alacritty.toml`, `~/.alacritty.toml`, then the OS
config dir). Missing files are not an error — built-in defaults are used.

Only a **subset** of the alacritty schema is read; unknown keys are silently
ignored so an existing config will load even if you use sections aterm does
not understand. Top-level `import = ["..."]` (and `[general] import`) is
honored up to a depth of 4.

| Section | Keys read |
| --- | --- |
| `[font]` | `size`, `normal.family` |
| `[window]` | `padding.x`, `padding.y` |
| `[colors.primary]` | `background`, `foreground` |
| `[colors.cursor]` | `cursor` |
| `[colors.normal]` | `black` `red` `green` `yellow` `blue` `magenta` `cyan` `white` |
| `[colors.bright]` | (same eight) |
| `[[keyboard.bindings]]` | `key`, `mods`, `action` or `chars` |

Colors accept `#RRGGBB`, `0xRRGGBB`, or `RRGGBB`. `#RRGGBBAA` is accepted
but the alpha is discarded (the renderer doesn't composite translucent
content). The shell is taken from `$SHELL` (falling back to `/bin/zsh` on
macOS and `/bin/sh` elsewhere); this is not configurable via the file.

### New tabs inherit the active tab's cwd

Cmd+T (or any binding mapped to `CreateTab`) opens a new tab in the
working directory of the currently active shell. The lookup uses
`/proc/<pid>/cwd` on Linux and `proc_pidinfo` on macOS — no shell
configuration is required. On other platforms the new tab spawns
wherever aterm itself was launched from.

### Keybindings

User bindings layer on top of the defaults; a user entry whose
`(key, mods)` matches a default replaces it. Use `action = "ReceiveChar"`
to suppress a default binding and let the keystroke flow to the PTY.

A binding carries either an `action` or, like alacritty, a `chars` string
of literal bytes to send to the PTY (`chars` wins if both are given). TOML
basic-string escapes apply, so `"\u001b"` is ESC. This is how you get
macOS-style Option+Left/Right word movement, which works in any shell
without an `~/.inputrc`:

```toml
[[keyboard.bindings]]
key = "Left"
mods = "Alt"
chars = "\u001bb"   # ESC b → backward-word

[[keyboard.bindings]]
key = "Right"
mods = "Alt"
chars = "\u001bf"   # ESC f → forward-word
```

Recognized actions: `CreateTab`, `CloseTab`, `SelectTab1`..`SelectTab9`,
`PrevTab`/`NextTab`, `Copy`, `Paste`, `ScrollLineUp`/`ScrollLineDown`,
`ScrollPageUp`/`ScrollPageDown`, `ScrollToTop`/`ScrollToBottom`,
`IncreaseFontSize` (aka `ZoomIn`), `DecreaseFontSize` (`ZoomOut`),
`ResetFontSize` (`ZoomReset`), `ReceiveChar`.

Recognized modifier tokens (split by `|`, `+`, or whitespace): `Command`
(aka `Super`/`Win`/`Cmd`), `Control`/`Ctrl`, `Shift`, `Alt`/`Option`.

Defaults (Cmd-based on macOS, mapped to Super elsewhere): Cmd+T new tab,
Cmd+W close tab, Cmd+C/V copy/paste, Cmd+1..9 select tab, Cmd+Left/Right
prev/next tab, Cmd+=/- font size, Cmd+0 reset, Shift+PageUp/PageDown to
scroll, Shift+Home/End to jump to top/bottom.

## Build

```
cargo build --release
```

## Run

```
cargo run --release
```

## Install

Quick install to `~/.cargo/bin` (any platform):

```
cargo install --path .
```

### macOS .app bundle

Signed, notarized arm64 (Apple Silicon) DMGs are attached to each
[GitHub release](https://github.com/gartnera/aterm/releases) — download,
open, and drag `aterm.app` to `/Applications`.

To build the bundle yourself:

```
cargo install cargo-bundle
cargo bundle --release
cp -r target/release/bundle/osx/aterm.app /Applications/
```

### Linux

Prebuilt `x86_64` and `aarch64` binaries are attached to each
[GitHub release](https://github.com/gartnera/aterm/releases) as
`aterm-<version>-<target>.tar.gz`. Or build it yourself — the release
binary at `target/release/aterm` is self-contained; copy it anywhere on
`$PATH`. A `.desktop` entry is not bundled yet.

## Releases

`.github/workflows/release.yml` cuts releases. Bump `version` in
`Cargo.toml`, then run the workflow manually (*Actions → Release → Run
workflow*); it tags `v<version>`, builds an arm64 (Apple Silicon) macOS app
— code-signed, wrapped in a DMG and notarized — plus `x86_64` and `aarch64`
Linux binary tarballs, then publishes a GitHub release with all of them
attached.

macOS signing/notarization needs these repository secrets
(*Settings → Secrets and variables → Actions*):

| Secret | What it is |
| --- | --- |
| `APPLE_CERTIFICATE_BASE64` | base64 of your *Developer ID Application* `.p12` (`base64 -i cert.p12 \| pbcopy`) |
| `APPLE_CERTIFICATE_PASSWORD` | password set when exporting that `.p12` |
| `APPLE_SIGNING_IDENTITY` | e.g. `Developer ID Application: Your Name (TEAMID)` |
| `APPLE_ID` | Apple ID email used for notarization |
| `APPLE_APP_PASSWORD` | app-specific password for that Apple ID |
| `APPLE_TEAM_ID` | 10-character Apple Developer Team ID |
| `KEYCHAIN_PASSWORD` | any throwaway string securing the temp build keychain |
