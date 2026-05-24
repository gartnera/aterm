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
| `[[keyboard.bindings]]` | `key`, `mods`, `action` |

Colors accept `#RRGGBB`, `0xRRGGBB`, or `RRGGBB`. `#RRGGBBAA` is accepted
but the alpha is discarded (the renderer doesn't composite translucent
content). The shell is taken from `$SHELL` (falling back to `/bin/zsh` on
macOS and `/bin/sh` elsewhere); this is not configurable via the file.

### Keybindings

User bindings layer on top of the defaults; a user entry whose
`(key, mods)` matches a default replaces it. Use `action = "ReceiveChar"`
to suppress a default binding and let the keystroke flow to the PTY.

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

```
cargo install cargo-bundle
cargo bundle --release
cp -r target/release/bundle/osx/aterm.app /Applications/
```

### Linux

The release binary at `target/release/aterm` is self-contained; copy it
anywhere on `$PATH`. A `.desktop` entry is not bundled yet.
