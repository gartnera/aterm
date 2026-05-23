# aterm

A minimal tabbed terminal built on [`alacritty_terminal`](https://crates.io/crates/alacritty_terminal), `winit`, and `wgpu`.

Reads your existing `alacritty.toml` for fonts, colors, and shell.

## Build

```
cargo build --release
```

## Run

```
cargo run --release
```

## Install (macOS)

Quick install to `~/.cargo/bin`:

```
cargo install --path .
```

Or build a `.app` bundle:

```
cargo install cargo-bundle
cargo bundle --release
cp -r target/release/bundle/osx/aterm.app /Applications/
```
