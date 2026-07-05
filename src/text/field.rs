//! The generic, reusable text widget shared by editable inputs and read-only
//! wrapped text blocks across cozyui.
//!
//! - [`TextLayout`] is a cheap, borrowed view over a font and a piece of
//!   geometry. It knows how to wrap, measure, draw, locate the caret, and
//!   hit-test a click — the logic that used to be copy-pasted into each widget.
//! - [`TextField`] owns an editable buffer plus its [`TextEdit`] state and routes
//!   key handling / rendering / hit-testing through a `TextLayout` the caller
//!   builds from its current geometry.
//! - [`LinePlacement`] describes how wrapped lines stack vertically (and the
//!   inverse, for turning a click's y into a line index).

use pixel_fonts::TextLayout;
use pixel_graphics::{Framebuffer, Index};

use super::edit::{TextEdit, TextEditOutcome, char_len};
use super::input::KeyInput;

/// An editable single-field text buffer with cursor, selection, undo, and
/// mouse-drag selection. Rendering and hit-testing go through a [`TextLayout`]
/// the caller supplies from its current geometry.
#[derive(Clone)]
pub struct TextField {
    text: String,
    edit: TextEdit,
    max_chars: usize,
    max_lines: usize,
}

impl TextField {
    pub(crate) fn new(max_chars: usize, max_lines: usize) -> Self {
        Self {
            text: String::new(),
            edit: TextEdit::default(),
            max_chars,
            max_lines,
        }
    }

    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    /// Replace the buffer (e.g. when focus moves to a different field), clamping
    /// the cursor and clearing any selection or in-progress drag.
    pub(crate) fn set_text(&mut self, text: &str) {
        self.text = text.to_string();
        let cursor = self.edit.cursor().min(char_len(&self.text));
        self.edit.set_cursor(cursor, &self.text);
        self.edit.clear_undo();
    }

    pub(crate) fn clear(&mut self) {
        self.text.clear();
        self.edit.set_cursor(0, &self.text);
        self.edit.clear_undo();
    }

    pub(crate) fn set_cursor(&mut self, cursor: usize) {
        self.edit.set_cursor(cursor, &self.text);
    }

    pub(crate) fn set_cursor_end(&mut self) {
        self.edit.set_cursor(char_len(&self.text), &self.text);
    }

    pub(crate) const fn cursor(&self) -> usize {
        self.edit.cursor()
    }

    pub(crate) fn selection_range(&self) -> Option<(usize, usize)> {
        self.edit.selection_range()
    }

    pub(crate) fn handle_key(
        &mut self,
        input: &KeyInput,
        clipboard_text: Option<&str>,
        layout: &TextLayout,
    ) -> TextEditOutcome {
        let max_chars = self.max_chars;
        let max_lines = self.max_lines;
        self.edit
            .handle_key(input, &mut self.text, clipboard_text, |candidate| {
                char_len(candidate) <= max_chars && layout.wrap(candidate).len() <= max_lines
            })
    }

    pub(crate) fn begin_drag(&mut self, index: usize) {
        self.edit.begin_drag(index, &self.text);
    }

    pub(crate) fn drag_to(&mut self, index: usize) -> bool {
        self.edit.drag_to(index, &self.text)
    }

    pub(crate) const fn end_drag(&mut self) {
        self.edit.end_drag();
    }

    pub(crate) const fn is_dragging(&self) -> bool {
        self.edit.is_dragging()
    }

    pub(crate) fn index_at(&self, layout: &TextLayout, x: isize, y: isize) -> usize {
        layout.index_at(&self.text, x, y)
    }

    pub(crate) fn cursor_position(&self, layout: &TextLayout) -> (isize, isize) {
        layout.cursor_position(&self.text, self.edit.cursor())
    }

    /// Draw the selection highlight (if any) and then the text.
    pub(crate) fn draw(
        &self,
        fb: &mut Framebuffer,
        layout: &TextLayout,
        text_color: Index,
        selection_color: Index,
    ) {
        let lines = layout.wrap(&self.text);
        layout.draw_selection_lines(fb, &lines, self.edit.selection_range(), selection_color);
        layout.draw_lines(fb, &lines, text_color);
    }
}

#[cfg(test)]
mod tests {
    use pixel_fonts::{BitmapFont, LinePlacement, PEANUT_MONEY_SPEC};

    use super::*;

    fn font() -> BitmapFont {
        BitmapFont::load(&PEANUT_MONEY_SPEC).unwrap()
    }

    fn uniform_layout(font: &BitmapFont) -> TextLayout<'_> {
        TextLayout::new(font, 0, 0, 6 * 8, LinePlacement::Uniform { line_h: 10 })
    }

    /// Type one letter through the real key-handling path.
    fn type_letter(field: &mut TextField, layout: &TextLayout, letter: char) {
        let sym = u32::from(letter as u8);
        let input = KeyInput::new_for_test(sym, letter.to_string(), 0);
        field.handle_key(&input, None, layout);
    }

    #[test]
    fn field_enforces_char_limit_when_typing() {
        let font = font();
        let mut field = TextField::new(3, 1);
        let layout = uniform_layout(&font);
        // Typing up to max_chars lands in the buffer...
        for letter in ['a', 'b', 'c'] {
            type_letter(&mut field, &layout, letter);
        }
        assert_eq!(field.text(), "abc");
        assert_eq!(field.cursor(), 3);
        // ...and anything past it is rejected.
        type_letter(&mut field, &layout, 'd');
        assert_eq!(field.text(), "abc");
        assert_eq!(field.cursor(), 3);
        // Selection round-trips through the layout.
        let (x, y) = field.cursor_position(&layout);
        assert_eq!(field.index_at(&layout, x, y), field.cursor());
    }
}
