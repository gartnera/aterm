//! Minimal loader for the user's existing alacritty config.
//!
//! We deliberately accept a subset of the schema. Unknown keys are ignored so
//! a real-world alacritty.toml will load even if we don't understand all of it.

use std::path::PathBuf;

use serde::Deserialize;

use crate::binding::{self, Keybinding};

#[derive(Clone, Debug)]
pub struct Config {
    pub font_family: String,
    pub font_size: f32,
    /// Original font size as configured by the user. `font_size` is the
    /// current (possibly zoomed) size; this is what ResetFontSize restores.
    pub font_size_initial: Option<f32>,
    /// The palette currently in effect. Equals `colors_dark` or `colors_light`
    /// depending on the resolved system theme (see [`follow_system_theme`]).
    pub colors: Colors,
    /// Palette used when the system theme is dark (or theme detection is
    /// unavailable). Standard alacritty `[colors]` keys customize this one.
    pub colors_dark: Colors,
    /// Palette used when the system reports a light appearance.
    pub colors_light: Colors,
    /// When true, aterm follows the OS light/dark appearance and swaps between
    /// `colors_dark` and `colors_light` live as the system theme changes.
    pub follow_system_theme: bool,
    pub padding_x: f32,
    pub padding_y: f32,
    pub bindings: Vec<Keybinding>,
    /// When false, OSC 0/1/2 sequences from the shell are ignored and the
    /// window/tab title stays at its initial value. Mirrors alacritty's
    /// `[window].dynamic_title` option.
    pub dynamic_title: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            font_family: default_font_family().to_string(),
            font_size: 13.0,
            font_size_initial: None,
            colors: Colors::default_dark(),
            colors_dark: Colors::default_dark(),
            colors_light: Colors::default_light(),
            // Off by default: with no `[colors]` config this still resolves to
            // the dark palette (matching the historical look), but a user who
            // opts in — or who defines a `[colors.light]`/`[colors.dark]`
            // table — gets live theme switching. `load()` flips this on when
            // appropriate.
            follow_system_theme: false,
            padding_x: 6.0,
            padding_y: 6.0,
            bindings: binding::defaults(),
            dynamic_title: true,
        }
    }
}

#[cfg(target_os = "macos")]
fn default_font_family() -> &'static str {
    "Menlo"
}
#[cfg(not(target_os = "macos"))]
fn default_font_family() -> &'static str {
    "monospace"
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Colors {
    pub background: [u8; 3],
    pub foreground: [u8; 3],
    pub cursor: [u8; 3],
    pub normal: AnsiPalette,
    pub bright: AnsiPalette,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnsiPalette {
    pub black: [u8; 3],
    pub red: [u8; 3],
    pub green: [u8; 3],
    pub yellow: [u8; 3],
    pub blue: [u8; 3],
    pub magenta: [u8; 3],
    pub cyan: [u8; 3],
    pub white: [u8; 3],
}

impl Default for Colors {
    fn default() -> Self {
        Self::default_dark()
    }
}

impl Colors {
    /// Matches alacritty's built-in default scheme so the look is identical
    /// when no `[colors]` table is provided. Used when the system theme is
    /// dark or when theme detection is unavailable.
    pub fn default_dark() -> Self {
        Self {
            background: [0x18, 0x18, 0x18],
            foreground: [0xd8, 0xd8, 0xd8],
            cursor: [0xd8, 0xd8, 0xd8],
            normal: AnsiPalette {
                black: [0x18, 0x18, 0x18],
                red: [0xac, 0x42, 0x42],
                green: [0x90, 0xa9, 0x59],
                yellow: [0xf4, 0xbf, 0x75],
                blue: [0x6a, 0x9f, 0xb5],
                magenta: [0xaa, 0x75, 0x9f],
                cyan: [0x75, 0xb5, 0xaa],
                white: [0xd8, 0xd8, 0xd8],
            },
            bright: AnsiPalette {
                black: [0x6b, 0x6b, 0x6b],
                red: [0xc5, 0x55, 0x55],
                green: [0xaa, 0xc4, 0x74],
                yellow: [0xfe, 0xca, 0x88],
                blue: [0x82, 0xb8, 0xc8],
                magenta: [0xc2, 0x8c, 0xb8],
                cyan: [0x93, 0xd3, 0xc3],
                white: [0xf8, 0xf8, 0xf8],
            },
        }
    }

    /// Built-in light scheme (base16 "Default Light" by Chris Kempson), the
    /// natural companion to the dark default above. Used when the system
    /// reports a light appearance and the user hasn't supplied their own
    /// `[colors.light]` table.
    pub fn default_light() -> Self {
        Self {
            background: [0xf8, 0xf8, 0xf8],
            foreground: [0x38, 0x38, 0x38],
            cursor: [0x38, 0x38, 0x38],
            normal: AnsiPalette {
                black: [0xf8, 0xf8, 0xf8],
                red: [0xab, 0x46, 0x42],
                green: [0xa1, 0xb5, 0x6c],
                yellow: [0xf7, 0xca, 0x88],
                blue: [0x7c, 0xaf, 0xc2],
                magenta: [0xba, 0x8b, 0xaf],
                cyan: [0x86, 0xc1, 0xb9],
                white: [0x38, 0x38, 0x38],
            },
            bright: AnsiPalette {
                black: [0xb8, 0xb8, 0xb8],
                red: [0xab, 0x46, 0x42],
                green: [0xa1, 0xb5, 0x6c],
                yellow: [0xf7, 0xca, 0x88],
                blue: [0x7c, 0xaf, 0xc2],
                magenta: [0xba, 0x8b, 0xaf],
                cyan: [0x86, 0xc1, 0xb9],
                white: [0x18, 0x18, 0x18],
            },
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct RawConfig {
    #[serde(default)]
    font: Option<RawFont>,
    #[serde(default)]
    colors: Option<RawColors>,
    #[serde(default)]
    window: Option<RawWindow>,
    #[serde(default)]
    keyboard: Option<RawKeyboard>,
}

#[derive(Debug, Default, Deserialize)]
struct RawKeyboard {
    #[serde(default)]
    bindings: Vec<RawBinding>,
}

#[derive(Debug, Default, Deserialize)]
struct RawBinding {
    key: String,
    #[serde(default)]
    mods: Option<String>,
    #[serde(default)]
    action: Option<String>,
    /// Literal bytes to send to the PTY, mirroring alacritty's `chars`.
    /// TOML basic-string escapes apply, e.g. `chars = "\u001bb"` sends
    /// ESC then `b` (readline backward-word). Mutually exclusive with
    /// `action`; `chars` wins if both are present.
    #[serde(default)]
    chars: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct RawFont {
    #[serde(default)]
    normal: Option<RawFontFamily>,
    #[serde(default)]
    size: Option<f32>,
}

#[derive(Debug, Default, Deserialize)]
struct RawFontFamily {
    family: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct RawColors {
    #[serde(default)]
    primary: Option<RawPrimary>,
    #[serde(default)]
    cursor: Option<RawCursor>,
    #[serde(default)]
    normal: Option<RawAnsi>,
    #[serde(default)]
    bright: Option<RawAnsi>,
    /// aterm extension: overrides applied to the dark palette only. Same shape
    /// as the standard `[colors]` table (primary/cursor/normal/bright).
    #[serde(default)]
    dark: Option<Box<RawColors>>,
    /// aterm extension: overrides applied to the light palette only.
    #[serde(default)]
    light: Option<Box<RawColors>>,
    /// aterm extension: explicitly enable/disable following the OS appearance.
    /// When unset, aterm infers a sensible default (follow when the user
    /// hasn't pinned a single explicit scheme; see [`apply_raw`]).
    #[serde(default)]
    auto_theme: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
struct RawPrimary {
    background: Option<String>,
    foreground: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct RawCursor {
    cursor: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct RawAnsi {
    black: Option<String>,
    red: Option<String>,
    green: Option<String>,
    yellow: Option<String>,
    blue: Option<String>,
    magenta: Option<String>,
    cyan: Option<String>,
    white: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct RawWindow {
    #[serde(default)]
    padding: Option<RawPadding>,
    #[serde(default)]
    dynamic_title: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
struct RawPadding {
    x: Option<f32>,
    y: Option<f32>,
}

pub fn load() -> Config {
    let mut cfg = Config::default();
    let Some(path) = find_config_path() else {
        log::info!("no alacritty config found; using defaults");
        return cfg;
    };
    let Some(merged) = load_value(&path, 0) else {
        log::warn!("could not load {}", path.display());
        return cfg;
    };
    let raw: RawConfig = match merged.try_into() {
        Ok(r) => r,
        Err(e) => {
            log::warn!("failed to interpret {}: {e}", path.display());
            return cfg;
        }
    };
    log::info!("loaded alacritty config from {}", path.display());
    apply_raw(&mut cfg, raw);
    cfg
}

/// Recursively load a config file, honouring `import = [...]` (both at the
/// top level and under `[general]`). Imports are merged underneath the
/// current file so the current file's keys win.
fn load_value(path: &std::path::Path, depth: usize) -> Option<toml::Value> {
    if depth > 4 {
        log::warn!(
            "config import depth limit (4) exceeded at {}; check for cycles",
            path.display()
        );
        return None;
    }
    let body = std::fs::read_to_string(path).ok()?;
    let mut value: toml::Value = toml::from_str(&body).ok()?;

    let imports = take_imports(&mut value);
    let base_dir = path.parent().unwrap_or_else(|| std::path::Path::new(""));

    let mut merged = toml::Value::Table(Default::default());
    for imp in imports {
        let resolved = expand_path(&imp, base_dir);
        if let Some(v) = load_value(&resolved, depth + 1) {
            merge_toml(&mut merged, v);
        }
    }
    merge_toml(&mut merged, value);
    Some(merged)
}

fn take_imports(value: &mut toml::Value) -> Vec<String> {
    let mut out = Vec::new();
    let top_level = value.as_table_mut().and_then(|t| t.remove("import"));
    let nested = value
        .get_mut("general")
        .and_then(|g| g.as_table_mut())
        .and_then(|t| t.remove("import"));
    for entry in [top_level, nested].into_iter().flatten() {
        if let toml::Value::Array(arr) = entry {
            for v in arr {
                if let toml::Value::String(s) = v {
                    out.push(s);
                }
            }
        }
    }
    out
}

fn merge_toml(base: &mut toml::Value, overlay: toml::Value) {
    match (base, overlay) {
        (toml::Value::Table(b), toml::Value::Table(o)) => {
            for (k, v) in o {
                match b.get_mut(&k) {
                    Some(existing) => merge_toml(existing, v),
                    None => {
                        b.insert(k, v);
                    }
                }
            }
        }
        (slot, overlay) => {
            *slot = overlay;
        }
    }
}

fn expand_path(s: &str, base_dir: &std::path::Path) -> PathBuf {
    let expanded = if let Some(rest) = s.strip_prefix("~/") {
        match dirs::home_dir() {
            Some(home) => home.join(rest),
            None => PathBuf::from(s),
        }
    } else if s == "~" {
        dirs::home_dir().unwrap_or_else(|| PathBuf::from(s))
    } else {
        PathBuf::from(s)
    };
    if expanded.is_absolute() {
        expanded
    } else {
        base_dir.join(expanded)
    }
}

fn apply_raw(cfg: &mut Config, raw: RawConfig) {
    if let Some(font) = raw.font {
        if let Some(size) = font.size {
            cfg.font_size = size;
            cfg.font_size_initial = Some(size);
        }
        if let Some(family) = font.normal.and_then(|n| n.family) {
            cfg.font_family = family;
        }
    }
    if let Some(window) = raw.window {
        if let Some(pad) = window.padding {
            if let Some(x) = pad.x {
                cfg.padding_x = x;
            }
            if let Some(y) = pad.y {
                cfg.padding_y = y;
            }
        }
        if let Some(dt) = window.dynamic_title {
            cfg.dynamic_title = dt;
        }
    }
    if let Some(colors) = raw.colors {
        // Standard alacritty `[colors]` keys customize the dark palette — that
        // preserves the historical look for users who configured a single
        // (typically dark) scheme.
        let has_flat = colors.primary.is_some()
            || colors.cursor.is_some()
            || colors.normal.is_some()
            || colors.bright.is_some();
        apply_palette_overrides(
            &mut cfg.colors_dark,
            colors.primary,
            colors.cursor,
            colors.normal,
            colors.bright,
        );
        // aterm extensions: per-scheme overrides.
        let has_split = colors.dark.is_some() || colors.light.is_some();
        if let Some(dark) = colors.dark {
            apply_palette_overrides(
                &mut cfg.colors_dark,
                dark.primary,
                dark.cursor,
                dark.normal,
                dark.bright,
            );
        }
        if let Some(light) = colors.light {
            apply_palette_overrides(
                &mut cfg.colors_light,
                light.primary,
                light.cursor,
                light.normal,
                light.bright,
            );
        }
        // Follow the OS appearance unless the user pinned a single explicit
        // scheme via a flat `[colors]` table. An explicit `auto_theme` always
        // wins; defining a `[colors.dark]`/`[colors.light]` split is itself a
        // request to switch.
        cfg.follow_system_theme = match colors.auto_theme {
            Some(v) => v,
            None => has_split || !has_flat,
        };
    } else {
        // No `[colors]` table at all: follow the system out of the box.
        cfg.follow_system_theme = true;
    }
    // Keep the active palette in sync as a pre-theme-resolution default. The
    // event loop overrides this from the real OS theme once a window exists.
    cfg.colors = cfg.colors_dark.clone();
    if let Some(kb) = raw.keyboard {
        let mut user = Vec::with_capacity(kb.bindings.len());
        for rb in kb.bindings {
            let Some(key) = binding::parse_key(&rb.key) else {
                log::warn!("ignoring binding: unknown key {:?}", rb.key);
                continue;
            };
            let action = match (rb.chars, rb.action) {
                (Some(chars), _) if !chars.is_empty() => {
                    binding::Action::SendChars(chars.into_bytes())
                }
                (_, Some(action)) => {
                    let Some(parsed) = binding::parse_action(&action) else {
                        log::warn!("ignoring binding: unknown action {action:?}");
                        continue;
                    };
                    parsed
                }
                _ => {
                    log::warn!("ignoring binding for {:?}: no action or chars", rb.key);
                    continue;
                }
            };
            let mods = rb
                .mods
                .as_deref()
                .map(binding::parse_mods)
                .unwrap_or_else(winit::keyboard::ModifiersState::empty);
            user.push(Keybinding { key, mods, action });
        }
        if !user.is_empty() {
            cfg.bindings = binding::merge(user, binding::defaults());
        }
    }
}

/// Apply the standard alacritty color keys (primary/cursor/normal/bright) onto
/// a single [`Colors`] palette. Shared by the flat `[colors]` table and the
/// aterm-specific `[colors.dark]`/`[colors.light]` sub-tables.
fn apply_palette_overrides(
    target: &mut Colors,
    primary: Option<RawPrimary>,
    cursor: Option<RawCursor>,
    normal: Option<RawAnsi>,
    bright: Option<RawAnsi>,
) {
    if let Some(primary) = primary {
        if let Some(bg) = primary.background.as_deref().and_then(parse_hex) {
            target.background = bg;
        }
        if let Some(fg) = primary.foreground.as_deref().and_then(parse_hex) {
            target.foreground = fg;
        }
    }
    if let Some(cur) = cursor {
        if let Some(c) = cur.cursor.as_deref().and_then(parse_hex) {
            target.cursor = c;
        }
    }
    apply_ansi(&mut target.normal, normal);
    apply_ansi(&mut target.bright, bright);
}

fn apply_ansi(pal: &mut AnsiPalette, raw: Option<RawAnsi>) {
    let Some(raw) = raw else { return };
    if let Some(c) = raw.black.as_deref().and_then(parse_hex) {
        pal.black = c;
    }
    if let Some(c) = raw.red.as_deref().and_then(parse_hex) {
        pal.red = c;
    }
    if let Some(c) = raw.green.as_deref().and_then(parse_hex) {
        pal.green = c;
    }
    if let Some(c) = raw.yellow.as_deref().and_then(parse_hex) {
        pal.yellow = c;
    }
    if let Some(c) = raw.blue.as_deref().and_then(parse_hex) {
        pal.blue = c;
    }
    if let Some(c) = raw.magenta.as_deref().and_then(parse_hex) {
        pal.magenta = c;
    }
    if let Some(c) = raw.cyan.as_deref().and_then(parse_hex) {
        pal.cyan = c;
    }
    if let Some(c) = raw.white.as_deref().and_then(parse_hex) {
        pal.white = c;
    }
}

fn parse_hex(s: &str) -> Option<[u8; 3]> {
    let s = s.trim();
    let s = s
        .strip_prefix("0x")
        .or_else(|| s.strip_prefix('#'))
        .unwrap_or(s);
    // Accept #RRGGBB and #RRGGBBAA; alpha is discarded since the renderer
    // doesn't compose translucent colors.
    if s.len() != 6 && s.len() != 8 {
        return None;
    }
    let r = u8::from_str_radix(&s[0..2], 16).ok()?;
    let g = u8::from_str_radix(&s[2..4], 16).ok()?;
    let b = u8::from_str_radix(&s[4..6], 16).ok()?;
    Some([r, g, b])
}

fn find_config_path() -> Option<PathBuf> {
    let home = dirs::home_dir();
    // Alacritty's canonical search path: $XDG_CONFIG_HOME (when set) and
    // ~/.config/alacritty/alacritty.toml first, then platform-specific
    // config_dir() variants as fallbacks. On macOS dirs::config_dir()
    // returns ~/Library/Application Support which is not where alacritty
    // looks, so the ~/.config check is required even there.
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        if !xdg.is_empty() {
            candidates.push(PathBuf::from(&xdg).join("alacritty/alacritty.toml"));
        }
    }
    if let Some(h) = home.as_ref() {
        candidates.push(h.join(".config/alacritty/alacritty.toml"));
        candidates.push(h.join(".alacritty.toml"));
    }
    if let Some(cfg) = dirs::config_dir() {
        candidates.push(cfg.join("alacritty/alacritty.toml"));
        candidates.push(cfg.join("alacritty.toml"));
    }
    candidates.into_iter().find(|p| p.exists())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_hex_six_digits() {
        assert_eq!(parse_hex("#1a2b3c"), Some([0x1a, 0x2b, 0x3c]));
        assert_eq!(parse_hex("0x1A2B3C"), Some([0x1a, 0x2b, 0x3c]));
        assert_eq!(parse_hex("1a2b3c"), Some([0x1a, 0x2b, 0x3c]));
    }

    #[test]
    fn parse_hex_eight_digits_drops_alpha() {
        assert_eq!(parse_hex("#1a2b3cff"), Some([0x1a, 0x2b, 0x3c]));
        assert_eq!(parse_hex("0x1a2b3c80"), Some([0x1a, 0x2b, 0x3c]));
    }

    #[test]
    fn parse_hex_rejects_other_lengths() {
        assert_eq!(parse_hex("#abc"), None);
        assert_eq!(parse_hex("#1a2b3"), None);
        assert_eq!(parse_hex(""), None);
        assert_eq!(parse_hex("zzzzzz"), None);
    }

    #[test]
    fn merge_toml_overlays_keys() {
        let mut base: toml::Value = toml::from_str("a = 1\nb = 2\n").unwrap();
        let overlay: toml::Value = toml::from_str("b = 99\nc = 3\n").unwrap();
        merge_toml(&mut base, overlay);
        let table = base.as_table().unwrap();
        assert_eq!(table.get("a").unwrap().as_integer(), Some(1));
        assert_eq!(table.get("b").unwrap().as_integer(), Some(99));
        assert_eq!(table.get("c").unwrap().as_integer(), Some(3));
    }

    #[test]
    fn merge_toml_recurses_into_subtables() {
        let mut base: toml::Value = toml::from_str("[t]\nx = 1\ny = 2\n").unwrap();
        let overlay: toml::Value = toml::from_str("[t]\ny = 99\nz = 3\n").unwrap();
        merge_toml(&mut base, overlay);
        let t = base.get("t").unwrap().as_table().unwrap();
        assert_eq!(t.get("x").unwrap().as_integer(), Some(1));
        assert_eq!(t.get("y").unwrap().as_integer(), Some(99));
        assert_eq!(t.get("z").unwrap().as_integer(), Some(3));
    }

    #[test]
    fn dynamic_title_defaults_true_and_can_be_disabled() {
        let mut cfg = Config::default();
        assert!(cfg.dynamic_title);
        let raw: RawConfig = toml::from_str("[window]\ndynamic_title = false\n").unwrap();
        apply_raw(&mut cfg, raw);
        assert!(!cfg.dynamic_title);
    }

    #[test]
    fn chars_binding_parses_to_send_chars() {
        let mut cfg = Config::default();
        let raw: RawConfig = toml::from_str(
            "[[keyboard.bindings]]\nkey = \"Left\"\nmods = \"Alt\"\nchars = \"\\u001bb\"\n",
        )
        .unwrap();
        apply_raw(&mut cfg, raw);
        let hit = binding::find(
            &cfg.bindings,
            winit::keyboard::KeyCode::ArrowLeft,
            winit::keyboard::ModifiersState::ALT,
        )
        .expect("Alt+Left binding present");
        assert_eq!(hit.action, binding::Action::SendChars(vec![0x1b, b'b']));
    }

    #[test]
    fn binding_without_action_or_chars_is_ignored() {
        let mut cfg = Config::default();
        let before = cfg.bindings.len();
        let raw: RawConfig =
            toml::from_str("[[keyboard.bindings]]\nkey = \"Left\"\nmods = \"Alt\"\n").unwrap();
        apply_raw(&mut cfg, raw);
        // The bogus binding is dropped; defaults are left untouched.
        assert_eq!(cfg.bindings.len(), before);
    }

    #[test]
    fn no_colors_table_follows_system() {
        let mut cfg = Config::default();
        let raw: RawConfig = toml::from_str("[font]\nsize = 12.0\n").unwrap();
        apply_raw(&mut cfg, raw);
        // Out of the box (no [colors] customization) aterm follows the OS.
        assert!(cfg.follow_system_theme);
        // Built-in light scheme is available even without explicit config.
        assert_eq!(cfg.colors_light, Colors::default_light());
    }

    #[test]
    fn flat_colors_table_pins_dark_scheme() {
        let mut cfg = Config::default();
        let raw: RawConfig =
            toml::from_str("[colors.primary]\nbackground = \"#102030\"\n").unwrap();
        apply_raw(&mut cfg, raw);
        // A single explicit scheme opts out of following the system, and the
        // override lands on the dark palette (the historical behavior).
        assert!(!cfg.follow_system_theme);
        assert_eq!(cfg.colors_dark.background, [0x10, 0x20, 0x30]);
        assert_eq!(cfg.colors.background, [0x10, 0x20, 0x30]);
    }

    #[test]
    fn light_table_enables_following_and_overrides_light_only() {
        let mut cfg = Config::default();
        let raw: RawConfig = toml::from_str(
            "[colors.primary]\nbackground = \"#102030\"\n\
             [colors.light.primary]\nbackground = \"#fafbfc\"\n",
        )
        .unwrap();
        apply_raw(&mut cfg, raw);
        // Defining a per-scheme table is a request to switch.
        assert!(cfg.follow_system_theme);
        assert_eq!(cfg.colors_dark.background, [0x10, 0x20, 0x30]);
        assert_eq!(cfg.colors_light.background, [0xfa, 0xfb, 0xfc]);
    }

    #[test]
    fn auto_theme_explicit_override_wins() {
        let mut cfg = Config::default();
        // Flat table alone would pin to dark, but auto_theme = true overrides.
        let raw: RawConfig = toml::from_str(
            "[colors]\nauto_theme = true\n[colors.primary]\nbackground = \"#102030\"\n",
        )
        .unwrap();
        apply_raw(&mut cfg, raw);
        assert!(cfg.follow_system_theme);

        let mut cfg = Config::default();
        // Conversely, a light/dark split would follow, but auto_theme = false
        // pins it off.
        let raw: RawConfig = toml::from_str(
            "[colors]\nauto_theme = false\n[colors.light.primary]\nbackground = \"#fafbfc\"\n",
        )
        .unwrap();
        apply_raw(&mut cfg, raw);
        assert!(!cfg.follow_system_theme);
    }

    #[test]
    fn take_imports_pulls_top_level_and_general() {
        let mut v: toml::Value =
            toml::from_str("import = [\"a.toml\"]\n[general]\nimport = [\"b.toml\"]\n").unwrap();
        let mut got = take_imports(&mut v);
        got.sort();
        assert_eq!(got, vec!["a.toml".to_string(), "b.toml".to_string()]);
        assert!(v.as_table().unwrap().get("import").is_none());
        assert!(v
            .get("general")
            .unwrap()
            .as_table()
            .unwrap()
            .get("import")
            .is_none());
    }
}
