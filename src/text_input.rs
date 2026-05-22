use std::error::Error;

use x11rb::xcb_ffi::XCBConnection;
use xkbcommon::xkb;
use xkbcommon::xkb::keysyms;

pub(crate) enum EditKey {
    Insert(char),
    Backspace,
    Enter,
    Escape,
    Tab,
    Left,
    Right,
    None,
}

#[derive(Clone)]
pub(crate) struct KeyInput {
    sym: xkb::Keysym,
    text: String,
    ctrl: bool,
    shift: bool,
}

impl KeyInput {
    #[cfg(test)]
    fn new_for_test(sym: u32, text: impl Into<String>, state: u16) -> Self {
        Self {
            sym: xkb::Keysym::new(sym),
            text: text.into(),
            ctrl: state & CONTROL_MASK != 0,
            shift: state & SHIFT_MASK != 0,
        }
    }

    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    pub(crate) fn ctrl(&self) -> bool {
        self.ctrl
    }

    pub(crate) fn shift(&self) -> bool {
        self.shift
    }

    pub(crate) fn sym_raw(&self) -> u32 {
        self.sym.raw()
    }
}

pub(crate) struct Keyboard {
    state: xkb::State,
}

impl Keyboard {
    pub(crate) fn new(connection: &XCBConnection) -> Result<Self, Box<dyn Error>> {
        let mut major = 0;
        let mut minor = 0;
        let mut base_event = 0;
        let mut base_error = 0;
        if !xkb::x11::setup_xkb_extension(
            connection,
            xkb::x11::MIN_MAJOR_XKB_VERSION,
            xkb::x11::MIN_MINOR_XKB_VERSION,
            xkb::x11::SetupXkbExtensionFlags::NoFlags,
            &mut major,
            &mut minor,
            &mut base_event,
            &mut base_error,
        ) {
            return Err("XKB extension is not available".into());
        }

        let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        let device_id = xkb::x11::get_core_keyboard_device_id(connection);
        if device_id == -1 {
            return Err("XKB core keyboard device is not available".into());
        }
        let keymap = xkb::x11::keymap_new_from_device(
            &context,
            connection,
            device_id,
            xkb::KEYMAP_COMPILE_NO_FLAGS,
        );
        let state = xkb::x11::state_new_from_device(&keymap, connection, device_id);
        Ok(Self { state })
    }

    pub(crate) fn press(&mut self, keycode: u8, event_state: u16) -> KeyInput {
        let keycode = xkb::Keycode::new(keycode as u32);
        let input = KeyInput {
            sym: self.state.key_get_one_sym(keycode),
            text: self.state.key_get_utf8(keycode),
            ctrl: event_state & CONTROL_MASK != 0,
            shift: event_state & SHIFT_MASK != 0,
        };
        self.state.update_key(keycode, xkb::KeyDirection::Down);
        input
    }

    pub(crate) fn release(&mut self, keycode: u8) {
        self.state
            .update_key(xkb::Keycode::new(keycode as u32), xkb::KeyDirection::Up);
    }
}

pub(crate) fn edit_key(input: &KeyInput) -> EditKey {
    match input.sym_raw() {
        keysyms::KEY_Escape => EditKey::Escape,
        keysyms::KEY_BackSpace => EditKey::Backspace,
        keysyms::KEY_Tab | keysyms::KEY_KP_Tab => EditKey::Tab,
        keysyms::KEY_Return | keysyms::KEY_KP_Enter => EditKey::Enter,
        keysyms::KEY_Left | keysyms::KEY_KP_Left => EditKey::Left,
        keysyms::KEY_Right | keysyms::KEY_KP_Right => EditKey::Right,
        _ => input
            .text()
            .chars()
            .next()
            .filter(|_| input.text().chars().count() == 1)
            .map(EditKey::Insert)
            .unwrap_or(EditKey::None),
    }
}

const SHIFT_MASK: u16 = 1;
const CONTROL_MASK: u16 = 4;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edit_key_inserts_printable_text_from_xkb() {
        let input = KeyInput::new_for_test(keysyms::KEY_q, "q", 0);

        assert!(matches!(edit_key(&input), EditKey::Insert('q')));
    }

    #[test]
    fn edit_key_keeps_controls_separate() {
        let backspace = KeyInput::new_for_test(keysyms::KEY_BackSpace, "", 0);
        let letter = KeyInput::new_for_test(keysyms::KEY_q, "q", 0);

        assert!(matches!(edit_key(&backspace), EditKey::Backspace));
        assert!(matches!(edit_key(&letter), EditKey::Insert('q')));
    }
}
