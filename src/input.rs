//! Translate winit key events to PTY byte sequences.
//!
//! This intentionally covers the common case (printable text, named editor
//! keys, Ctrl-letter, Alt-as-meta). It does not yet handle: keypad numeric
//! mode, application cursor mode, mouse reporting, vt220 function keys with
//! modifier encoding. Those can be added as needed.

use winit::keyboard::{Key, ModifiersState, NamedKey};

pub fn encode_key(
    logical_key: &Key,
    text: Option<&str>,
    mods: ModifiersState,
) -> Option<Vec<u8>> {
    if let Key::Named(named) = logical_key {
        if let Some(seq) = encode_named(*named) {
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

fn encode_named(named: NamedKey) -> Option<&'static [u8]> {
    Some(match named {
        NamedKey::Enter => b"\r",
        NamedKey::Tab => b"\t",
        NamedKey::Backspace => b"\x7f",
        NamedKey::Escape => b"\x1b",
        NamedKey::Space => b" ",
        NamedKey::ArrowUp => b"\x1b[A",
        NamedKey::ArrowDown => b"\x1b[B",
        NamedKey::ArrowRight => b"\x1b[C",
        NamedKey::ArrowLeft => b"\x1b[D",
        NamedKey::Home => b"\x1b[H",
        NamedKey::End => b"\x1b[F",
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
