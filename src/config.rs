//! Minimal loader for the user's existing alacritty config.
//!
//! We deliberately accept a subset of the schema. Unknown keys are ignored so
//! a real-world alacritty.toml will load even if we don't understand all of it.

use std::path::PathBuf;

use serde::Deserialize;

#[derive(Clone, Debug)]
pub struct Config {
    pub font_family: String,
    pub font_size: f32,
    pub colors: Colors,
    pub padding_x: f32,
    pub padding_y: f32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            font_family: default_font_family().to_string(),
            font_size: 13.0,
            colors: Colors::default(),
            padding_x: 6.0,
            padding_y: 6.0,
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
        Self {
            background: [0x10, 0x10, 0x14],
            foreground: [0xd0, 0xd0, 0xd0],
            cursor: [0xd0, 0xd0, 0xd0],
            normal: AnsiPalette {
                black: [0x00, 0x00, 0x00],
                red: [0xcc, 0x33, 0x33],
                green: [0x33, 0xcc, 0x33],
                yellow: [0xcc, 0xcc, 0x33],
                blue: [0x33, 0x66, 0xcc],
                magenta: [0xcc, 0x33, 0xcc],
                cyan: [0x33, 0xcc, 0xcc],
                white: [0xcc, 0xcc, 0xcc],
            },
            bright: AnsiPalette {
                black: [0x66, 0x66, 0x66],
                red: [0xff, 0x66, 0x66],
                green: [0x66, 0xff, 0x66],
                yellow: [0xff, 0xff, 0x66],
                blue: [0x66, 0x99, 0xff],
                magenta: [0xff, 0x66, 0xff],
                cyan: [0x66, 0xff, 0xff],
                white: [0xff, 0xff, 0xff],
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
    let body = match std::fs::read_to_string(&path) {
        Ok(b) => b,
        Err(e) => {
            log::warn!("could not read {}: {e}", path.display());
            return cfg;
        }
    };
    let raw: RawConfig = match toml::from_str(&body) {
        Ok(r) => r,
        Err(e) => {
            log::warn!("failed to parse {}: {e}", path.display());
            return cfg;
        }
    };
    log::info!("loaded alacritty config from {}", path.display());
    apply_raw(&mut cfg, raw);
    cfg
}

fn apply_raw(cfg: &mut Config, raw: RawConfig) {
    if let Some(font) = raw.font {
        if let Some(size) = font.size {
            cfg.font_size = size;
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
    let s = s.strip_prefix("0x").or_else(|| s.strip_prefix('#')).unwrap_or(s);
    if s.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&s[0..2], 16).ok()?;
    let g = u8::from_str_radix(&s[2..4], 16).ok()?;
    let b = u8::from_str_radix(&s[4..6], 16).ok()?;
    Some([r, g, b])
}

fn find_config_path() -> Option<PathBuf> {
    let cfg_dir = dirs::config_dir()?;
    for name in ["alacritty.toml", "alacritty/alacritty.toml"] {
        let p = cfg_dir.join(name);
        if p.exists() {
            return Some(p);
        }
    }
    None
}
