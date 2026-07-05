//! Pure VT100/xterm key-to-escape-sequence encoding: given a `KeyInput`, what
//! (if anything) should be written to the pty or done locally (scrollback,
//! copy). No rendering or IO here, so this is unit-tested directly rather
//! than only exercised by clicking around in a real terminal.

use alacritty_terminal::grid::Scroll;
use xkbcommon::xkb::keysyms;

use crate::text::KeyInput;

pub(super) fn is_copy_shortcut(input: &KeyInput) -> bool {
    input.ctrl() && input.shift() && input.is_letter(keysyms::KEY_c, keysyms::KEY_C, "c")
}

pub(super) fn key_bytes(input: &KeyInput) -> Option<String> {
    if input.ctrl() {
        if let Some(seq) = modified_nav_sequence(input) {
            return Some(seq);
        }
        if let Some(byte) = control_byte(input) {
            return String::from_utf8(vec![byte]).ok();
        }
    }

    let text = match input.sym_raw() {
        keysyms::KEY_Escape => "\x1b",
        keysyms::KEY_BackSpace => "\x7f",
        keysyms::KEY_Tab | keysyms::KEY_KP_Tab => "\t",
        keysyms::KEY_Return | keysyms::KEY_KP_Enter => "\r",
        keysyms::KEY_Up | keysyms::KEY_KP_Up => "\x1b[A",
        keysyms::KEY_Down | keysyms::KEY_KP_Down => "\x1b[B",
        keysyms::KEY_Left | keysyms::KEY_KP_Left => "\x1b[D",
        keysyms::KEY_Right | keysyms::KEY_KP_Right => "\x1b[C",
        keysyms::KEY_Home | keysyms::KEY_KP_Home => "\x1b[H",
        keysyms::KEY_End | keysyms::KEY_KP_End => "\x1b[F",
        keysyms::KEY_Prior | keysyms::KEY_KP_Prior => "\x1b[5~",
        keysyms::KEY_Next | keysyms::KEY_KP_Next => "\x1b[6~",
        _ => input.text(),
    };

    if text.is_empty() {
        None
    } else {
        Some(text.to_string())
    }
}

/// xterm-style modified CSI sequence for Ctrl (optionally with Shift) held
/// with a navigation key, e.g. Ctrl+Left -> `"\x1b[1;5D"` (readline
/// word-jump, vim Ctrl+Home, etc.). Without this, `control_byte` doesn't
/// cover these keysyms, so the caller would fall through to the plain,
/// unmodified sequence and silently drop the Ctrl entirely. The modifier
/// parameter follows the widely-honored `1 + shift(1) + ctrl(4)` xterm
/// encoding; this codebase doesn't track Alt, so that term is omitted.
fn modified_nav_sequence(input: &KeyInput) -> Option<String> {
    let modifier = 1 + if input.shift() { 1 } else { 0 } + 4;
    match input.sym_raw() {
        keysyms::KEY_Up | keysyms::KEY_KP_Up => Some(format!("\x1b[1;{modifier}A")),
        keysyms::KEY_Down | keysyms::KEY_KP_Down => Some(format!("\x1b[1;{modifier}B")),
        keysyms::KEY_Left | keysyms::KEY_KP_Left => Some(format!("\x1b[1;{modifier}D")),
        keysyms::KEY_Right | keysyms::KEY_KP_Right => Some(format!("\x1b[1;{modifier}C")),
        keysyms::KEY_Home | keysyms::KEY_KP_Home => Some(format!("\x1b[1;{modifier}H")),
        keysyms::KEY_End | keysyms::KEY_KP_End => Some(format!("\x1b[1;{modifier}F")),
        keysyms::KEY_Prior | keysyms::KEY_KP_Prior => Some(format!("\x1b[5;{modifier}~")),
        keysyms::KEY_Next | keysyms::KEY_KP_Next => Some(format!("\x1b[6;{modifier}~")),
        _ => None,
    }
}

fn control_byte(input: &KeyInput) -> Option<u8> {
    let raw = input.sym_raw();
    if (keysyms::KEY_a..=keysyms::KEY_z).contains(&raw) {
        return Some((raw - keysyms::KEY_a + 1) as u8);
    }
    if (keysyms::KEY_A..=keysyms::KEY_Z).contains(&raw) {
        return Some((raw - keysyms::KEY_A + 1) as u8);
    }
    // '[', '\', ']', '^', '_' sit at ASCII values whose low 5 bits are
    // already the C0 control byte they map to (e.g. '[' is 0x5B, and
    // 0x5B & 0x1F == 0x1B == Esc).
    match raw {
        keysyms::KEY_bracketleft
        | keysyms::KEY_backslash
        | keysyms::KEY_bracketright
        | keysyms::KEY_asciicircum
        | keysyms::KEY_underscore => Some((raw as u8) & 0x1F),
        keysyms::KEY_space => Some(0x00),
        _ => None,
    }
}

pub(super) const fn key_scroll(input: &KeyInput) -> Option<Scroll> {
    // Ctrl+Shift+Home/End/Prior/Next is a modified nav sequence meant for the
    // pty program (see `modified_nav_sequence`), not local scrollback.
    if !input.shift() || input.ctrl() {
        return None;
    }

    match input.sym_raw() {
        keysyms::KEY_Home | keysyms::KEY_KP_Home => Some(Scroll::Top),
        keysyms::KEY_Prior | keysyms::KEY_KP_Prior => Some(Scroll::PageUp),
        keysyms::KEY_End | keysyms::KEY_KP_End => Some(Scroll::Bottom),
        keysyms::KEY_Next | keysyms::KEY_KP_Next => Some(Scroll::PageDown),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Mirror `text::input`'s private event-state bit encoding.
    const SHIFT: u16 = 1;
    const CTRL: u16 = 4;

    #[test]
    fn plain_arrow_is_unmodified_csi() {
        let left = KeyInput::new_for_test(keysyms::KEY_Left, "", 0);
        assert_eq!(key_bytes(&left).as_deref(), Some("\x1b[D"));
    }

    #[test]
    fn printable_text_passes_through() {
        let q = KeyInput::new_for_test(keysyms::KEY_q, "q", 0);
        assert_eq!(key_bytes(&q).as_deref(), Some("q"));
    }

    #[test]
    fn ctrl_arrow_sends_modified_nav_sequence_not_plain_arrow() {
        let ctrl_left = KeyInput::new_for_test(keysyms::KEY_Left, "", CTRL);
        assert_eq!(key_bytes(&ctrl_left).as_deref(), Some("\x1b[1;5D"));
    }

    #[test]
    fn ctrl_shift_arrow_adds_shift_to_the_modifier() {
        let ctrl_shift_left = KeyInput::new_for_test(keysyms::KEY_Left, "", CTRL | SHIFT);
        assert_eq!(key_bytes(&ctrl_shift_left).as_deref(), Some("\x1b[1;6D"));
    }

    #[test]
    fn ctrl_letter_sends_control_byte() {
        let ctrl_a = KeyInput::new_for_test(keysyms::KEY_a, "", CTRL);
        assert_eq!(key_bytes(&ctrl_a).as_deref(), Some("\x01"));
    }

    #[test]
    fn is_copy_shortcut_requires_ctrl_shift_c() {
        let ctrl_shift_c = KeyInput::new_for_test(keysyms::KEY_c, "c", CTRL | SHIFT);
        let ctrl_c = KeyInput::new_for_test(keysyms::KEY_c, "c", CTRL);
        assert!(is_copy_shortcut(&ctrl_shift_c));
        assert!(!is_copy_shortcut(&ctrl_c));
    }

    #[test]
    fn shift_home_scrolls_locally() {
        let shift_home = KeyInput::new_for_test(keysyms::KEY_Home, "", SHIFT);
        assert!(matches!(key_scroll(&shift_home), Some(Scroll::Top)));
    }

    #[test]
    fn ctrl_shift_home_is_left_to_the_pty_not_local_scroll() {
        let ctrl_shift_home = KeyInput::new_for_test(keysyms::KEY_Home, "", CTRL | SHIFT);
        assert!(key_scroll(&ctrl_shift_home).is_none());
    }
}
