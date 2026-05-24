//! Translate winit key events to PTY byte sequences.
//!
//! This intentionally covers the common case (printable text, named editor
//! keys, Ctrl-letter, Alt-as-meta). It does not yet handle: keypad numeric
//! mode, application cursor mode, mouse reporting, vt220 function keys with
//! modifier encoding. Those can be added as needed.

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
    if mods.shift_key() { bits |= 1; }
    if mods.alt_key() { bits |= 2; }
    if mods.control_key() { bits |= 4; }
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
        NamedKey::ArrowUp => if app { b"\x1bOA" } else { b"\x1b[A" },
        NamedKey::ArrowDown => if app { b"\x1bOB" } else { b"\x1b[B" },
        NamedKey::ArrowRight => if app { b"\x1bOC" } else { b"\x1b[C" },
        NamedKey::ArrowLeft => if app { b"\x1bOD" } else { b"\x1b[D" },
        NamedKey::Home => if app { b"\x1bOH" } else { b"\x1b[H" },
        NamedKey::End => if app { b"\x1bOF" } else { b"\x1b[F" },
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
            encode_key(&Key::Named(NamedKey::Tab), None, ModifiersState::empty(), no_app()),
            Some(b"\t".to_vec())
        );
        assert_eq!(
            encode_key(&Key::Named(NamedKey::Escape), None, ModifiersState::empty(), no_app()),
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
        let got = encode_key(
            &key_char("2"),
            Some("@"),
            ModifiersState::SHIFT,
            no_app(),
        );
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
}
