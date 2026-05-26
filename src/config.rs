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
    pub colors: Colors,
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
            colors: Colors::default(),
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

#[derive(Clone, Debug)]
pub struct Colors {
    pub background: [u8; 3],
    pub foreground: [u8; 3],
    pub cursor: [u8; 3],
    pub normal: AnsiPalette,
    pub bright: AnsiPalette,
}

#[derive(Clone, Debug)]
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
        // Matches alacritty's built-in default scheme so the look is
        // identical when no [colors] table is provided.
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
    action: String,
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
        if let Some(primary) = colors.primary {
            if let Some(bg) = primary.background.as_deref().and_then(parse_hex) {
                cfg.colors.background = bg;
            }
            if let Some(fg) = primary.foreground.as_deref().and_then(parse_hex) {
                cfg.colors.foreground = fg;
            }
        }
        if let Some(cur) = colors.cursor {
            if let Some(c) = cur.cursor.as_deref().and_then(parse_hex) {
                cfg.colors.cursor = c;
            }
        }
        apply_ansi(&mut cfg.colors.normal, colors.normal);
        apply_ansi(&mut cfg.colors.bright, colors.bright);
    }
    if let Some(kb) = raw.keyboard {
        let mut user = Vec::with_capacity(kb.bindings.len());
        for rb in kb.bindings {
            let Some(key) = binding::parse_key(&rb.key) else {
                log::warn!("ignoring binding: unknown key {:?}", rb.key);
                continue;
            };
            let Some(action) = binding::parse_action(&rb.action) else {
                log::warn!("ignoring binding: unknown action {:?}", rb.action);
                continue;
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
