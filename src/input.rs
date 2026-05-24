//! Translate winit key events to PTY byte sequences.
//!
//! This intentionally covers the common case (printable text, named editor
//! keys, Ctrl-letter, Alt-as-meta). It does not yet handle: keypad numeric
//! mode, vt220 function keys with modifier encoding. Those can be added as
//! needed.
//!
//! Mouse-reporting encoding (DECSET 1000/1002/1003 with SGR 1006) lives at
//! the bottom of this module.

use winit::keyboard::{Key, ModifiersState, NamedKey};

#[derive(Clone, Copy)]
pub struct TermKeyMode {
    pub app_cursor: bool,
}

pub fn encode_key(
    logical_key: &Key,
    text: Option<&str>,
    mods: ModifiersState,
    term_mode: TermKeyMode,
) -> Option<Vec<u8>> {
    if let Key::Named(named) = logical_key {
        // Ctrl+Shift modifier-encoded form takes precedence over the bare
        // sequence so shells get Ctrl+Left as a word-jump etc.
        if let Some(seq) = encode_named_modified(*named, mods) {
            return Some(seq);
        }
        if let Some(seq) = encode_named(*named, term_mode) {
            return Some(prefix_alt(seq.to_vec(), mods));
        }
    }

    // For character keys, prefer `text` (it accounts for layout + shift), and
    // fall back to the Character variant if text is missing (some platforms).
    let chars: &str = match text {
        Some(s) if !s.is_empty() => s,
        _ => match logical_key {
            Key::Character(s) => s.as_str(),
            _ => return None,
        },
    };

    if mods.control_key() {
        // Ctrl-letter → byte & 0x1f. Only meaningful for single ASCII chars.
        if let Some(c) = chars.chars().next() {
            if chars.chars().count() == 1 && c.is_ascii() {
                let byte = match c {
                    '@'..='_' => (c as u8) & 0x1f,
                    'a'..='z' => c as u8 - b'a' + 1,
                    ' ' => 0,
                    '?' => 0x7f,
                    _ => return None,
                };
                return Some(prefix_alt(vec![byte], mods));
            }
        }
        return None;
    }

    Some(prefix_alt(chars.as_bytes().to_vec(), mods))
}

fn prefix_alt(mut bytes: Vec<u8>, mods: ModifiersState) -> Vec<u8> {
    // Alacritty's convention: Alt acts as Meta and prefixes input with ESC.
    if mods.alt_key() {
        bytes.insert(0, 0x1b);
    }
    bytes
}

/// xterm-style modifier-encoded sequence for named keys. Returns None only
/// when no Shift/Alt/Ctrl modifier is held — in that case the caller falls
/// back to the plain sequence path. With any of those modifiers held
/// (including Alt-only), we emit the `CSI 1;{m}…` / `CSI {key};{m}~` form,
/// which is how xterm reports modified named keys.
fn encode_named_modified(named: NamedKey, mods: ModifiersState) -> Option<Vec<u8>> {
    // bit 0 = shift, bit 1 = alt, bit 2 = ctrl. Result = 1 + that value.
    let mut bits = 0u8;
    if mods.shift_key() {
        bits |= 1;
    }
    if mods.alt_key() {
        bits |= 2;
    }
    if mods.control_key() {
        bits |= 4;
    }
    if bits == 0 {
        return None;
    }
    let m = bits + 1;
    // CSI 1;{m}{letter} keys
    let letter = match named {
        NamedKey::ArrowUp => b'A',
        NamedKey::ArrowDown => b'B',
        NamedKey::ArrowRight => b'C',
        NamedKey::ArrowLeft => b'D',
        NamedKey::Home => b'H',
        NamedKey::End => b'F',
        NamedKey::F1 => b'P',
        NamedKey::F2 => b'Q',
        NamedKey::F3 => b'R',
        NamedKey::F4 => b'S',
        _ => 0,
    };
    if letter != 0 {
        return Some(format!("\x1b[1;{m}{}", letter as char).into_bytes());
    }
    // CSI {key};{m}~ keys
    let key_num: u32 = match named {
        NamedKey::Insert => 2,
        NamedKey::Delete => 3,
        NamedKey::PageUp => 5,
        NamedKey::PageDown => 6,
        NamedKey::F5 => 15,
        NamedKey::F6 => 17,
        NamedKey::F7 => 18,
        NamedKey::F8 => 19,
        NamedKey::F9 => 20,
        NamedKey::F10 => 21,
        NamedKey::F11 => 23,
        NamedKey::F12 => 24,
        _ => return None,
    };
    Some(format!("\x1b[{key_num};{m}~").into_bytes())
}

fn encode_named(named: NamedKey, mode: TermKeyMode) -> Option<&'static [u8]> {
    // In DECCKM (application cursor) mode, arrow + Home/End emit SS3 (ESC O)
    // sequences instead of CSI (ESC [). htop, vim, less, etc. switch into
    // this mode and use it to recognise arrow keys.
    let app = mode.app_cursor;
    Some(match named {
        NamedKey::Enter => b"\r",
        NamedKey::Tab => b"\t",
        NamedKey::Backspace => b"\x7f",
        NamedKey::Escape => b"\x1b",
        NamedKey::Space => b" ",
        NamedKey::ArrowUp => {
            if app {
                b"\x1bOA"
            } else {
                b"\x1b[A"
            }
        }
        NamedKey::ArrowDown => {
            if app {
                b"\x1bOB"
            } else {
                b"\x1b[B"
            }
        }
        NamedKey::ArrowRight => {
            if app {
                b"\x1bOC"
            } else {
                b"\x1b[C"
            }
        }
        NamedKey::ArrowLeft => {
            if app {
                b"\x1bOD"
            } else {
                b"\x1b[D"
            }
        }
        NamedKey::Home => {
            if app {
                b"\x1bOH"
            } else {
                b"\x1b[H"
            }
        }
        NamedKey::End => {
            if app {
                b"\x1bOF"
            } else {
                b"\x1b[F"
            }
        }
        NamedKey::PageUp => b"\x1b[5~",
        NamedKey::PageDown => b"\x1b[6~",
        NamedKey::Insert => b"\x1b[2~",
        NamedKey::Delete => b"\x1b[3~",
        NamedKey::F1 => b"\x1bOP",
        NamedKey::F2 => b"\x1bOQ",
        NamedKey::F3 => b"\x1bOR",
        NamedKey::F4 => b"\x1bOS",
        NamedKey::F5 => b"\x1b[15~",
        NamedKey::F6 => b"\x1b[17~",
        NamedKey::F7 => b"\x1b[18~",
        NamedKey::F8 => b"\x1b[19~",
        NamedKey::F9 => b"\x1b[20~",
        NamedKey::F10 => b"\x1b[21~",
        NamedKey::F11 => b"\x1b[23~",
        NamedKey::F12 => b"\x1b[24~",
        _ => return None,
    })
}

/// One of the buttons the terminal will report. Wheel events are encoded
/// as buttons too, so this enum covers both press/release and wheel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    /// Wheel up / scroll backward.
    WheelUp,
    /// Wheel down / scroll forward.
    WheelDown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseAction {
    /// Button has just been pressed.
    Press,
    /// Button has just been released.
    Release,
    /// Mouse moved while one or more buttons are held; encoded with the
    /// motion bit set. Wheel buttons never produce Motion.
    Motion,
}

/// Encode a mouse event in xterm's SGR (1006) format.
///
/// Layout: `CSI < Cb ; Cx ; Cy ; M` for press / motion, `m` for release.
/// `Cb` is the base button code plus modifier flags plus the motion bit.
/// `Cx`/`Cy` are 1-based cell coordinates clamped to `u16::MAX`.
///
/// Shift held during a press is intentionally *not* encoded — callers are
/// expected to bypass mouse reporting entirely when Shift is down so the
/// user can still select text out of a full-screen app. Ctrl and Alt are
/// passed through as the conventional modifier bits.
pub fn encode_mouse_sgr(
    button: MouseButton,
    action: MouseAction,
    col: usize,
    row: usize,
    mods: ModifiersState,
) -> Vec<u8> {
    let base: u32 = match button {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
        MouseButton::WheelUp => 64,
        MouseButton::WheelDown => 65,
    };
    let mut cb = base;
    if action == MouseAction::Motion {
        cb += 32;
    }
    if mods.alt_key() {
        cb += 8;
    }
    if mods.control_key() {
        cb += 16;
    }
    // SGR uses 1-based cells; cap at u16::MAX so we don't emit absurd ints
    // for an out-of-grid pointer (it should already be clamped by caller).
    let cx = (col.saturating_add(1)).min(u16::MAX as usize);
    let cy = (row.saturating_add(1)).min(u16::MAX as usize);
    let trailer = if action == MouseAction::Release {
        'm'
    } else {
        'M'
    };
    format!("\x1b[<{cb};{cx};{cy}{trailer}").into_bytes()
}

/// Normalize text for paste delivery to a PTY. Converts CRLF/LF to CR
/// (since terminals interpret CR as Enter) and strips any embedded
/// bracketed-paste end markers — an attacker who can stage text on the
/// clipboard would otherwise be able to break out of paste mode and have
/// the rest of the payload treated as typed input by the receiving shell.
pub fn normalize_paste(text: &str) -> String {
    text.replace("\r\n", "\r")
        .replace('\n', "\r")
        .replace("\x1b[201~", "")
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::keyboard::{Key, NamedKey, SmolStr};

    fn no_app() -> TermKeyMode {
        TermKeyMode { app_cursor: false }
    }
    fn app() -> TermKeyMode {
        TermKeyMode { app_cursor: true }
    }
    fn key_char(c: &str) -> Key {
        Key::Character(SmolStr::new(c))
    }

    #[test]
    fn plain_ascii_passes_through() {
        let got = encode_key(&key_char("a"), Some("a"), ModifiersState::empty(), no_app());
        assert_eq!(got, Some(b"a".to_vec()));
    }

    #[test]
    fn enter_is_carriage_return() {
        let got = encode_key(
            &Key::Named(NamedKey::Enter),
            None,
            ModifiersState::empty(),
            no_app(),
        );
        assert_eq!(got, Some(b"\r".to_vec()));
    }

    #[test]
    fn backspace_emits_del() {
        let got = encode_key(
            &Key::Named(NamedKey::Backspace),
            None,
            ModifiersState::empty(),
            no_app(),
        );
        assert_eq!(got, Some(b"\x7f".to_vec()));
    }

    #[test]
    fn tab_and_escape() {
        assert_eq!(
            encode_key(
                &Key::Named(NamedKey::Tab),
                None,
                ModifiersState::empty(),
                no_app()
            ),
            Some(b"\t".to_vec())
        );
        assert_eq!(
            encode_key(
                &Key::Named(NamedKey::Escape),
                None,
                ModifiersState::empty(),
                no_app()
            ),
            Some(b"\x1b".to_vec())
        );
    }

    #[test]
    fn arrows_normal_use_csi() {
        let got = encode_key(
            &Key::Named(NamedKey::ArrowLeft),
            None,
            ModifiersState::empty(),
            no_app(),
        );
        assert_eq!(got, Some(b"\x1b[D".to_vec()));
    }

    #[test]
    fn arrows_app_cursor_use_ss3() {
        // DECCKM (vim/htop/less mode) switches CSI -> SS3 for the cursor keys.
        let got = encode_key(
            &Key::Named(NamedKey::ArrowLeft),
            None,
            ModifiersState::empty(),
            app(),
        );
        assert_eq!(got, Some(b"\x1bOD".to_vec()));
        let got = encode_key(
            &Key::Named(NamedKey::Home),
            None,
            ModifiersState::empty(),
            app(),
        );
        assert_eq!(got, Some(b"\x1bOH".to_vec()));
    }

    #[test]
    fn ctrl_letter_maps_to_control_byte() {
        // Ctrl+A = 0x01, Ctrl+C = 0x03, Ctrl+M = 0x0d.
        for (input, byte) in [("a", 0x01), ("c", 0x03), ("m", 0x0d), ("z", 0x1a)] {
            let got = encode_key(
                &key_char(input),
                Some(input),
                ModifiersState::CONTROL,
                no_app(),
            );
            assert_eq!(got, Some(vec![byte]), "ctrl+{input}");
        }
    }

    #[test]
    fn ctrl_space_is_nul_and_ctrl_question_is_del() {
        let got = encode_key(&key_char(" "), Some(" "), ModifiersState::CONTROL, no_app());
        assert_eq!(got, Some(vec![0x00]));
        let got = encode_key(&key_char("?"), Some("?"), ModifiersState::CONTROL, no_app());
        assert_eq!(got, Some(vec![0x7f]));
    }

    #[test]
    fn ctrl_uppercase_punctuation() {
        // Ctrl+[ = ESC (0x1b), used by vim/readline as the escape replacement.
        let got = encode_key(&key_char("["), Some("["), ModifiersState::CONTROL, no_app());
        assert_eq!(got, Some(vec![0x1b]));
    }

    #[test]
    fn alt_prefixes_with_esc() {
        let got = encode_key(&key_char("a"), Some("a"), ModifiersState::ALT, no_app());
        assert_eq!(got, Some(b"\x1ba".to_vec()));
    }

    #[test]
    fn alt_plus_ctrl_letter_prefixes_control_byte() {
        let got = encode_key(
            &key_char("a"),
            Some("a"),
            ModifiersState::ALT | ModifiersState::CONTROL,
            no_app(),
        );
        assert_eq!(got, Some(vec![0x1b, 0x01]));
    }

    #[test]
    fn shift_arrow_is_modifier_encoded() {
        // CSI 1;{m}D where m = 1 + bits. Shift only → m=2.
        let got = encode_key(
            &Key::Named(NamedKey::ArrowLeft),
            None,
            ModifiersState::SHIFT,
            no_app(),
        );
        assert_eq!(got, Some(b"\x1b[1;2D".to_vec()));
    }

    #[test]
    fn ctrl_right_arrow_word_jump_sequence() {
        // bash/zsh use CSI 1;5C for ctrl+right (word jump). Ctrl-only → m=5.
        let got = encode_key(
            &Key::Named(NamedKey::ArrowRight),
            None,
            ModifiersState::CONTROL,
            no_app(),
        );
        assert_eq!(got, Some(b"\x1b[1;5C".to_vec()));
    }

    #[test]
    fn shift_f5_uses_tilde_form() {
        // F5 = key_num 15. Shift only → m=2. Result: CSI 15;2~.
        let got = encode_key(
            &Key::Named(NamedKey::F5),
            None,
            ModifiersState::SHIFT,
            no_app(),
        );
        assert_eq!(got, Some(b"\x1b[15;2~".to_vec()));
    }

    #[test]
    fn shift_pageup_modifier_encoded() {
        // PageUp = key_num 5, Shift → m=2.
        let got = encode_key(
            &Key::Named(NamedKey::PageUp),
            None,
            ModifiersState::SHIFT,
            no_app(),
        );
        assert_eq!(got, Some(b"\x1b[5;2~".to_vec()));
    }

    #[test]
    fn unmodified_function_keys_use_named_form() {
        // F1 → SS3 P; F5 → CSI 15 ~.
        let got = encode_key(
            &Key::Named(NamedKey::F1),
            None,
            ModifiersState::empty(),
            no_app(),
        );
        assert_eq!(got, Some(b"\x1bOP".to_vec()));
        let got = encode_key(
            &Key::Named(NamedKey::F5),
            None,
            ModifiersState::empty(),
            no_app(),
        );
        assert_eq!(got, Some(b"\x1b[15~".to_vec()));
    }

    #[test]
    fn text_wins_over_logical_character_when_present() {
        // On a shifted layout the text reflects the shifted glyph; ensure we
        // pass `text` through rather than the unshifted Key::Character.
        let got = encode_key(&key_char("2"), Some("@"), ModifiersState::SHIFT, no_app());
        assert_eq!(got, Some(b"@".to_vec()));
    }

    #[test]
    fn modifier_encoded_takes_precedence_over_plain_named() {
        // Even Alt-only on an arrow should produce CSI 1;3D, not ESC ESC[D.
        let got = encode_key(
            &Key::Named(NamedKey::ArrowLeft),
            None,
            ModifiersState::ALT,
            no_app(),
        );
        assert_eq!(got, Some(b"\x1b[1;3D".to_vec()));
    }

    #[test]
    fn unsupported_named_keys_return_none() {
        let got = encode_key(
            &Key::Named(NamedKey::ContextMenu),
            None,
            ModifiersState::empty(),
            no_app(),
        );
        assert_eq!(got, None);
    }

    #[test]
    fn ctrl_with_multichar_text_returns_none() {
        // Composed input under Ctrl is meaningless; encoder should bail.
        let got = encode_key(
            &key_char("ab"),
            Some("ab"),
            ModifiersState::CONTROL,
            no_app(),
        );
        assert_eq!(got, None);
    }

    #[test]
    fn sgr_mouse_left_press_at_origin() {
        // 1-based coords: cell (0,0) is reported as ;1;1.
        let got = encode_mouse_sgr(
            MouseButton::Left,
            MouseAction::Press,
            0,
            0,
            ModifiersState::empty(),
        );
        assert_eq!(got, b"\x1b[<0;1;1M");
    }

    #[test]
    fn sgr_mouse_release_uses_lowercase_m() {
        let got = encode_mouse_sgr(
            MouseButton::Left,
            MouseAction::Release,
            10,
            5,
            ModifiersState::empty(),
        );
        assert_eq!(got, b"\x1b[<0;11;6m");
    }

    #[test]
    fn sgr_mouse_motion_sets_motion_bit() {
        // Motion adds 32 to the button code. Left + motion -> 32.
        let got = encode_mouse_sgr(
            MouseButton::Left,
            MouseAction::Motion,
            4,
            4,
            ModifiersState::empty(),
        );
        assert_eq!(got, b"\x1b[<32;5;5M");
    }

    #[test]
    fn sgr_mouse_wheel_uses_64_and_65() {
        let up = encode_mouse_sgr(
            MouseButton::WheelUp,
            MouseAction::Press,
            0,
            0,
            ModifiersState::empty(),
        );
        assert_eq!(up, b"\x1b[<64;1;1M");
        let down = encode_mouse_sgr(
            MouseButton::WheelDown,
            MouseAction::Press,
            0,
            0,
            ModifiersState::empty(),
        );
        assert_eq!(down, b"\x1b[<65;1;1M");
    }

    #[test]
    fn sgr_mouse_modifiers_add_bits() {
        // Ctrl-click on left = base 0 + 16 = 16.
        let got = encode_mouse_sgr(
            MouseButton::Left,
            MouseAction::Press,
            0,
            0,
            ModifiersState::CONTROL,
        );
        assert_eq!(got, b"\x1b[<16;1;1M");
        // Alt-click on right = base 2 + 8 = 10.
        let got = encode_mouse_sgr(
            MouseButton::Right,
            MouseAction::Press,
            2,
            3,
            ModifiersState::ALT,
        );
        assert_eq!(got, b"\x1b[<10;3;4M");
    }

    #[test]
    fn normalize_paste_converts_line_endings() {
        // CRLF and bare LF both collapse to CR (terminals interpret CR as Enter).
        assert_eq!(normalize_paste("a\nb"), "a\rb");
        assert_eq!(normalize_paste("a\r\nb"), "a\rb");
        // Existing CRs pass through.
        assert_eq!(normalize_paste("a\rb"), "a\rb");
    }

    #[test]
    fn normalize_paste_strips_embedded_end_marker() {
        // \x1b[201~ in clipboard contents would break out of bracketed-paste
        // mode on the receiving side. The normalizer must remove it so the
        // remaining bytes can't be reinterpreted as typed input.
        let payload = "harmless\x1b[201~rm -rf /";
        assert_eq!(normalize_paste(payload), "harmlessrm -rf /");
    }
}
