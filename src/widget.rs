//! The one interface every desk widget presents to the app shell. `App`
//! dispatches through `&dyn Widget` instead of a hand-written match per
//! operation, so adding a widget means implementing this trait and adding one
//! `WidgetId` arm, not touching every event handler.

use std::error::Error;

use crate::text::KeyInput;
use crate::{CursorKind, Framebuffer, Index, Palette};

/// What a left click on a widget produced, beyond taking focus.
#[derive(Default)]
pub(crate) struct ClickOutcome {
    /// Text the widget wants copied to the clipboard.
    pub(crate) copy_text: Option<String>,
    /// A todo was checked on a celebratory page; the twirl should spin.
    pub(crate) spin_twirl: bool,
    /// The click started a text-selection drag inside the widget.
    pub(crate) text_drag: bool,
}

#[derive(Clone, Copy)]
pub(crate) enum ScrollDirection {
    Up,
    Down,
}

/// Coordinates passed to the pointer methods are widget-local (the app shell
/// subtracts the widget's rect); they can be negative when a drag leaves the
/// widget.
pub(crate) trait Widget {
    fn width(&self) -> usize;
    fn height(&self) -> usize;
    /// Height contributed to the required-window-height computation; widgets
    /// sized to the window (fwends) report their minimum instead.
    fn layout_height(&self) -> usize {
        self.height()
    }
    fn fill_color(&self, palette: &Palette) -> Index;
    fn render(&mut self, fb: &mut Framebuffer, palette: &Palette);
    /// Periodic tick; returns whether a redraw is needed. Widgets driven by
    /// their own channels in the main loop (puter, fwends, toodle) keep the
    /// do-nothing default.
    fn update(&mut self) -> Result<bool, Box<dyn Error>> {
        Ok(false)
    }
    fn click(&mut self, _x: i16, _y: i16, _state: u16) -> Result<ClickOutcome, Box<dyn Error>> {
        Ok(ClickOutcome::default())
    }
    /// Focus moved to another widget; drop any focus-only visuals (cursors,
    /// active selections).
    fn blur(&mut self) {}
    /// Pointer moved while this widget has focus; returns whether a redraw is
    /// needed.
    fn motion(&mut self, _x: i16, _y: i16) -> bool {
        false
    }
    /// Pointer-position report for hover effects, regardless of focus:
    /// widget-local coordinates when this widget is topmost under the
    /// pointer, `(-1, -1)` otherwise (so a hover can clear when the pointer
    /// leaves or an overlapping widget is on top). Returns whether a redraw
    /// is needed.
    fn hover(&mut self, _x: i16, _y: i16) -> bool {
        false
    }
    /// Wheel scroll over the widget; returns whether it was handled.
    fn scroll(&mut self, _x: i16, _y: i16, _direction: ScrollDirection) -> bool {
        false
    }
    fn cursor_at(&self, _x: i16, _y: i16) -> CursorKind {
        CursorKind::Pointer
    }
    /// Returns text to copy to the clipboard, if the key asked for a copy.
    fn handle_key_press(
        &mut self,
        _input: &KeyInput,
        _clipboard_text: Option<&str>,
    ) -> Result<Option<String>, Box<dyn Error>> {
        Ok(None)
    }
    /// Whether this key press needs the clipboard fetched (it is a paste).
    fn wants_clipboard(&self, _input: &KeyInput) -> bool {
        false
    }
    /// Continue a text-selection drag; returns whether a redraw is needed.
    fn drag_text(&mut self, _x: i16, _y: i16) -> bool {
        false
    }
    fn end_text_drag(&mut self) {}
}
