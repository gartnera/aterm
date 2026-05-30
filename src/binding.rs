//! App-level keybindings.
//!
//! Modeled on alacritty's `[[keyboard.bindings]]` schema: each binding is a
//! (physical key, modifier set, action) triple. Defaults supply the standard
//! Cmd-based shortcuts; user entries from the config are layered on top and
//! win when they share the same `(key, mods)` trigger — matching alacritty's
//! "user binding replaces default" semantics.

use winit::keyboard::{KeyCode, ModifiersState};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    CreateTab,
    CloseTab,
    /// One-based tab index (1..=9).
    SelectTab(u8),
    PrevTab,
    NextTab,
    Copy,
    Paste,
    ScrollLineUp,
    ScrollLineDown,
    ScrollPageUp,
    ScrollPageDown,
    ScrollToTop,
    ScrollToBottom,
    IncreaseFontSize,
    DecreaseFontSize,
    ResetFontSize,
    /// Send a literal byte sequence to the PTY. Mirrors alacritty's `chars`
    /// binding field — e.g. `chars = "b"` for Alt+Left → backward-word.
    SendChars(Vec<u8>),
    /// Suppress a default binding without doing anything — the keystroke
    /// is passed through to the PTY. Mirrors alacritty's `ReceiveChar`.
    ReceiveChar,
}

#[derive(Clone, Debug)]
pub struct Keybinding {
    pub key: KeyCode,
    pub mods: ModifiersState,
    pub action: Action,
}

pub fn defaults() -> Vec<Keybinding> {
    let cmd = ModifiersState::SUPER;
    let shift = ModifiersState::SHIFT;
    let mut v = vec![
        Keybinding {
            key: KeyCode::KeyT,
            mods: cmd,
            action: Action::CreateTab,
        },
        Keybinding {
            key: KeyCode::KeyW,
            mods: cmd,
            action: Action::CloseTab,
        },
        Keybinding {
            key: KeyCode::KeyC,
            mods: cmd,
            action: Action::Copy,
        },
        Keybinding {
            key: KeyCode::KeyV,
            mods: cmd,
            action: Action::Paste,
        },
        Keybinding {
            key: KeyCode::ArrowLeft,
            mods: cmd,
            action: Action::PrevTab,
        },
        Keybinding {
            key: KeyCode::ArrowRight,
            mods: cmd,
            action: Action::NextTab,
        },
        Keybinding {
            key: KeyCode::PageUp,
            mods: shift,
            action: Action::ScrollPageUp,
        },
        Keybinding {
            key: KeyCode::PageDown,
            mods: shift,
            action: Action::ScrollPageDown,
        },
        Keybinding {
            key: KeyCode::Home,
            mods: shift,
            action: Action::ScrollToTop,
        },
        Keybinding {
            key: KeyCode::End,
            mods: shift,
            action: Action::ScrollToBottom,
        },
        Keybinding {
            key: KeyCode::Equal,
            mods: cmd,
            action: Action::IncreaseFontSize,
        },
        Keybinding {
            key: KeyCode::Equal,
            mods: cmd | shift,
            action: Action::IncreaseFontSize,
        },
        Keybinding {
            key: KeyCode::Minus,
            mods: cmd,
            action: Action::DecreaseFontSize,
        },
        Keybinding {
            key: KeyCode::Digit0,
            mods: cmd,
            action: Action::ResetFontSize,
        },
    ];
    let digits = [
        KeyCode::Digit1,
        KeyCode::Digit2,
        KeyCode::Digit3,
        KeyCode::Digit4,
        KeyCode::Digit5,
        KeyCode::Digit6,
        KeyCode::Digit7,
        KeyCode::Digit8,
        KeyCode::Digit9,
    ];
    for (i, &k) in digits.iter().enumerate() {
        v.push(Keybinding {
            key: k,
            mods: cmd,
            action: Action::SelectTab((i + 1) as u8),
        });
    }
    v
}

/// Layer `user` over `defaults`: any default whose (key, mods) is also in
/// `user` is dropped, then `user` is appended.
pub fn merge(user: Vec<Keybinding>, mut defaults: Vec<Keybinding>) -> Vec<Keybinding> {
    defaults.retain(|d| !user.iter().any(|u| u.key == d.key && u.mods == d.mods));
    defaults.extend(user);
    defaults
}

pub fn find(bindings: &[Keybinding], key: KeyCode, mods: ModifiersState) -> Option<&Keybinding> {
    bindings.iter().find(|b| b.key == key && b.mods == mods)
}

/// Parse a config key string like "T", "1", "Left", "PageUp", "F5" into a
/// `KeyCode`. Case-insensitive for letters. Returns None for unknown names.
pub fn parse_key(s: &str) -> Option<KeyCode> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(c) = single_char(trimmed) {
        if c.is_ascii_alphabetic() {
            return ascii_letter_keycode(c.to_ascii_uppercase());
        }
        if c.is_ascii_digit() {
            return Some(ascii_digit_keycode(c));
        }
        if let Some(k) = punct_keycode(c) {
            return Some(k);
        }
    }
    named_keycode(trimmed)
}

fn single_char(s: &str) -> Option<char> {
    let mut chars = s.chars();
    let c = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    Some(c)
}

fn ascii_letter_keycode(c: char) -> Option<KeyCode> {
    let kc = match c {
        'A' => KeyCode::KeyA,
        'B' => KeyCode::KeyB,
        'C' => KeyCode::KeyC,
        'D' => KeyCode::KeyD,
        'E' => KeyCode::KeyE,
        'F' => KeyCode::KeyF,
        'G' => KeyCode::KeyG,
        'H' => KeyCode::KeyH,
        'I' => KeyCode::KeyI,
        'J' => KeyCode::KeyJ,
        'K' => KeyCode::KeyK,
        'L' => KeyCode::KeyL,
        'M' => KeyCode::KeyM,
        'N' => KeyCode::KeyN,
        'O' => KeyCode::KeyO,
        'P' => KeyCode::KeyP,
        'Q' => KeyCode::KeyQ,
        'R' => KeyCode::KeyR,
        'S' => KeyCode::KeyS,
        'T' => KeyCode::KeyT,
        'U' => KeyCode::KeyU,
        'V' => KeyCode::KeyV,
        'W' => KeyCode::KeyW,
        'X' => KeyCode::KeyX,
        'Y' => KeyCode::KeyY,
        'Z' => KeyCode::KeyZ,
        _ => return None,
    };
    Some(kc)
}

fn ascii_digit_keycode(c: char) -> KeyCode {
    match c {
        '0' => KeyCode::Digit0,
        '1' => KeyCode::Digit1,
        '2' => KeyCode::Digit2,
        '3' => KeyCode::Digit3,
        '4' => KeyCode::Digit4,
        '5' => KeyCode::Digit5,
        '6' => KeyCode::Digit6,
        '7' => KeyCode::Digit7,
        '8' => KeyCode::Digit8,
        _ => KeyCode::Digit9,
    }
}

fn punct_keycode(c: char) -> Option<KeyCode> {
    Some(match c {
        '-' => KeyCode::Minus,
        '=' => KeyCode::Equal,
        '[' => KeyCode::BracketLeft,
        ']' => KeyCode::BracketRight,
        ';' => KeyCode::Semicolon,
        '\'' => KeyCode::Quote,
        '`' => KeyCode::Backquote,
        ',' => KeyCode::Comma,
        '.' => KeyCode::Period,
        '/' => KeyCode::Slash,
        '\\' => KeyCode::Backslash,
        _ => return None,
    })
}

fn named_keycode(name: &str) -> Option<KeyCode> {
    let lower = name.to_ascii_lowercase();
    let kc = match lower.as_str() {
        "left" | "arrowleft" => KeyCode::ArrowLeft,
        "right" | "arrowright" => KeyCode::ArrowRight,
        "up" | "arrowup" => KeyCode::ArrowUp,
        "down" | "arrowdown" => KeyCode::ArrowDown,
        "pageup" => KeyCode::PageUp,
        "pagedown" => KeyCode::PageDown,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "insert" => KeyCode::Insert,
        "delete" => KeyCode::Delete,
        "backspace" => KeyCode::Backspace,
        "tab" => KeyCode::Tab,
        "enter" | "return" => KeyCode::Enter,
        "space" => KeyCode::Space,
        "escape" | "esc" => KeyCode::Escape,
        "f1" => KeyCode::F1,
        "f2" => KeyCode::F2,
        "f3" => KeyCode::F3,
        "f4" => KeyCode::F4,
        "f5" => KeyCode::F5,
        "f6" => KeyCode::F6,
        "f7" => KeyCode::F7,
        "f8" => KeyCode::F8,
        "f9" => KeyCode::F9,
        "f10" => KeyCode::F10,
        "f11" => KeyCode::F11,
        "f12" => KeyCode::F12,
        _ => return None,
    };
    Some(kc)
}

/// Parse a mods string like "Command|Shift" into a `ModifiersState`. Accepts
/// alacritty's spellings: Command/Super/Win, Control/Ctrl, Shift, Alt/Option.
/// Empty input returns an empty mods set. Unknown tokens are logged and
/// skipped so a typo doesn't drop the whole binding.
pub fn parse_mods(s: &str) -> ModifiersState {
    let mut state = ModifiersState::empty();
    for tok in s.split(|c: char| c == '|' || c == '+' || c.is_whitespace()) {
        if tok.is_empty() {
            continue;
        }
        match tok.to_ascii_lowercase().as_str() {
            "command" | "super" | "win" | "cmd" => state |= ModifiersState::SUPER,
            "control" | "ctrl" => state |= ModifiersState::CONTROL,
            "shift" => state |= ModifiersState::SHIFT,
            "alt" | "option" | "opt" => state |= ModifiersState::ALT,
            other => log::warn!("unknown key modifier: {other}"),
        }
    }
    state
}

pub fn parse_action(s: &str) -> Option<Action> {
    let trimmed = s.trim();
    if let Some(rest) = trimmed.strip_prefix("SelectTab") {
        if let Ok(n) = rest.parse::<u8>() {
            if (1..=9).contains(&n) {
                return Some(Action::SelectTab(n));
            }
        }
        return None;
    }
    Some(match trimmed {
        "CreateTab" | "CreateNewTab" => Action::CreateTab,
        "CloseTab" => Action::CloseTab,
        "PrevTab" | "PreviousTab" | "SelectPrevTab" | "SelectPreviousTab" => Action::PrevTab,
        "NextTab" | "SelectNextTab" => Action::NextTab,
        "Copy" => Action::Copy,
        "Paste" => Action::Paste,
        "ScrollLineUp" => Action::ScrollLineUp,
        "ScrollLineDown" => Action::ScrollLineDown,
        "ScrollPageUp" => Action::ScrollPageUp,
        "ScrollPageDown" => Action::ScrollPageDown,
        "ScrollToTop" => Action::ScrollToTop,
        "ScrollToBottom" => Action::ScrollToBottom,
        "IncreaseFontSize" | "ZoomIn" => Action::IncreaseFontSize,
        "DecreaseFontSize" | "ZoomOut" => Action::DecreaseFontSize,
        "ResetFontSize" | "ZoomReset" => Action::ResetFontSize,
        "ReceiveChar" => Action::ReceiveChar,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_key_letters_case_insensitive() {
        assert_eq!(parse_key("t"), Some(KeyCode::KeyT));
        assert_eq!(parse_key("T"), Some(KeyCode::KeyT));
    }

    #[test]
    fn parse_key_digits_and_punct() {
        assert_eq!(parse_key("1"), Some(KeyCode::Digit1));
        assert_eq!(parse_key("0"), Some(KeyCode::Digit0));
        assert_eq!(parse_key("-"), Some(KeyCode::Minus));
        assert_eq!(parse_key("="), Some(KeyCode::Equal));
    }

    #[test]
    fn parse_key_named_aliases() {
        assert_eq!(parse_key("Left"), Some(KeyCode::ArrowLeft));
        assert_eq!(parse_key("ArrowLeft"), Some(KeyCode::ArrowLeft));
        assert_eq!(parse_key("pageup"), Some(KeyCode::PageUp));
        assert_eq!(parse_key("F5"), Some(KeyCode::F5));
        assert_eq!(parse_key("Esc"), Some(KeyCode::Escape));
        assert_eq!(parse_key("Return"), Some(KeyCode::Enter));
    }

    #[test]
    fn parse_key_unknown() {
        assert_eq!(parse_key(""), None);
        assert_eq!(parse_key("Bogus"), None);
        assert_eq!(parse_key("ab"), None);
    }

    #[test]
    fn parse_mods_combinations() {
        assert_eq!(parse_mods(""), ModifiersState::empty());
        assert_eq!(parse_mods("Command"), ModifiersState::SUPER);
        assert_eq!(
            parse_mods("Cmd|Shift"),
            ModifiersState::SUPER | ModifiersState::SHIFT
        );
        assert_eq!(
            parse_mods("Ctrl+Alt"),
            ModifiersState::CONTROL | ModifiersState::ALT
        );
        assert_eq!(parse_mods("super win cmd"), ModifiersState::SUPER);
        assert_eq!(parse_mods("Option"), ModifiersState::ALT);
    }

    #[test]
    fn parse_mods_unknown_token_does_not_drop_others() {
        let got = parse_mods("Cmd|Bogus|Shift");
        assert_eq!(got, ModifiersState::SUPER | ModifiersState::SHIFT);
    }

    #[test]
    fn parse_action_known_and_aliases() {
        assert_eq!(parse_action("CreateTab"), Some(Action::CreateTab));
        assert_eq!(parse_action("PreviousTab"), Some(Action::PrevTab));
        assert_eq!(parse_action("SelectPrevTab"), Some(Action::PrevTab));
        assert_eq!(parse_action("ZoomIn"), Some(Action::IncreaseFontSize));
        assert_eq!(parse_action("ReceiveChar"), Some(Action::ReceiveChar));
        assert_eq!(parse_action("SelectTab3"), Some(Action::SelectTab(3)));
    }

    #[test]
    fn parse_action_rejects_out_of_range_tabs() {
        assert_eq!(parse_action("SelectTab0"), None);
        assert_eq!(parse_action("SelectTab10"), None);
        assert_eq!(parse_action("SelectTabX"), None);
        assert_eq!(parse_action("Bogus"), None);
    }

    #[test]
    fn merge_user_overrides_default() {
        let defaults = defaults();
        let default_t = defaults
            .iter()
            .find(|b| b.key == KeyCode::KeyT && b.mods == ModifiersState::SUPER)
            .expect("default Cmd+T present");
        assert_eq!(default_t.action, Action::CreateTab);

        let user = vec![Keybinding {
            key: KeyCode::KeyT,
            mods: ModifiersState::SUPER,
            action: Action::CloseTab,
        }];
        let merged = merge(user, defaults);
        let hit = find(&merged, KeyCode::KeyT, ModifiersState::SUPER).unwrap();
        assert_eq!(hit.action, Action::CloseTab);
        let count = merged
            .iter()
            .filter(|b| b.key == KeyCode::KeyT && b.mods == ModifiersState::SUPER)
            .count();
        assert_eq!(count, 1, "default should be replaced, not duplicated");
    }

    #[test]
    fn merge_receive_char_clears_default() {
        let user = vec![Keybinding {
            key: KeyCode::KeyT,
            mods: ModifiersState::SUPER,
            action: Action::ReceiveChar,
        }];
        let merged = merge(user, defaults());
        let hit = find(&merged, KeyCode::KeyT, ModifiersState::SUPER).unwrap();
        assert_eq!(hit.action, Action::ReceiveChar);
    }
}
