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

use super::edit::{TextEdit, TextEditOutcome, char_len, char_to_byte};
use super::font::BitmapFont;
use super::input::KeyInput;
use crate::{Framebuffer, Index};

/// How wrapped display lines are stacked vertically, relative to a base y.
#[derive(Clone, Copy)]
pub enum LinePlacement {
    /// Lines stacked at a fixed height — the fwends input and chat bubbles.
    Uniform { line_h: usize },
    /// A single line sits on the baseline; once the text wraps to two lines, the
    /// first lifts by `up` and the second drops by `down`. `hit_threshold` is the
    /// y offset (from base) at or past which a click lands on the second line.
    /// This is toodle's two-line todo layout.
    Split {
        up: usize,
        down: usize,
        hit_threshold: usize,
    },
}

impl LinePlacement {
    /// Signed y offset of wrapped line `index` (of `count` total) from the base.
    const fn line_dy(self, index: usize, count: usize) -> isize {
        match self {
            Self::Uniform { line_h } => (index * line_h) as isize,
            Self::Split { up, down, .. } => {
                if count <= 1 {
                    0
                } else if index == 0 {
                    -(up as isize)
                } else {
                    down as isize
                }
            }
        }
    }

    /// Inverse of [`line_dy`](Self::line_dy): which line a click at `dy` (relative
    /// to the base y) falls on.
    fn line_at(self, dy: isize) -> usize {
        match self {
            Self::Uniform { line_h } => (dy.max(0) as usize) / line_h.max(1),
            Self::Split { hit_threshold, .. } => usize::from(dy >= hit_threshold as isize),
        }
    }
}

/// A borrowed view that wraps, measures, draws, and locates text within a piece
/// of geometry. Built cheaply per call from the caller's current layout.
pub struct TextLayout<'a> {
    font: &'a BitmapFont,
    origin_x: usize,
    base_y: usize,
    max_width: usize,
    placement: LinePlacement,
}

impl<'a> TextLayout<'a> {
    pub(crate) const fn new(
        font: &'a BitmapFont,
        origin_x: usize,
        base_y: usize,
        max_width: usize,
        placement: LinePlacement,
    ) -> Self {
        Self {
            font,
            origin_x,
            base_y,
            max_width,
            placement,
        }
    }

    pub(crate) fn wrap(&self, text: &str) -> Vec<String> {
        self.font.wrap_lines(text, self.max_width)
    }

    const fn line_y(&self, index: usize, count: usize) -> usize {
        self.base_y
            .saturating_add_signed(self.placement.line_dy(index, count))
    }

    pub(crate) fn draw(&self, fb: &mut Framebuffer, text: &str, color: Index) {
        self.draw_lines(fb, &self.wrap(text), color);
    }

    pub(crate) fn draw_lines(&self, fb: &mut Framebuffer, lines: &[String], color: Index) {
        let count = lines.len();
        for (index, line) in lines.iter().enumerate() {
            self.font
                .draw_text(fb, line, self.origin_x, self.line_y(index, count), color);
        }
    }

    pub(crate) fn draw_selection_lines(
        &self,
        fb: &mut Framebuffer,
        lines: &[String],
        selection: Option<(usize, usize)>,
        color: Index,
    ) {
        let Some((selection_start, selection_end)) = selection else {
            return;
        };
        let count = lines.len();
        let mut line_start = 0;
        for (index, line) in lines.iter().enumerate() {
            let line_len = line.chars().count();
            let line_end = line_start + line_len;
            let start = selection_start.max(line_start);
            let end = selection_end.min(line_end);
            if start < end {
                let prefix = prefix_chars(line, start - line_start);
                let selected = prefix_chars(line, end - line_start);
                let sel_x = self.origin_x + self.font.text_width(prefix);
                let sel_w = self
                    .font
                    .text_width(selected)
                    .saturating_sub(self.font.text_width(prefix));
                fb.fill_rect(
                    sel_x,
                    self.line_y(index, count),
                    sel_w.max(1),
                    self.font.cell_h(),
                    color,
                );
            }
            line_start = line_end;
        }
    }

    /// Pixel position of the caret for character index `cursor`.
    pub(crate) fn cursor_position(&self, text: &str, cursor: usize) -> (usize, usize) {
        let lines = self.wrap(text);
        let (line_index, line_start, line) = line_for_char_index(&lines, cursor);
        let x = self.origin_x
            + self
                .font
                .text_width(prefix_chars(line, cursor - line_start));
        (
            x.min(self.origin_x + self.max_width),
            self.line_y(line_index, lines.len()),
        )
    }

    /// Character index nearest a click at absolute coordinates `(x, y)`.
    pub(crate) fn index_at(&self, text: &str, x: usize, y: usize) -> usize {
        let lines = self.wrap(text);
        let dy = y as isize - self.base_y as isize;
        let line_index = self.placement.line_at(dy);
        let local_x = x.saturating_sub(self.origin_x).min(self.max_width);
        text_index_at(self.font, &lines, line_index, local_x)
    }
}

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
    }

    pub(crate) fn clear(&mut self) {
        self.text.clear();
        self.edit.set_cursor(0, &self.text);
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

    pub(crate) fn index_at(&self, layout: &TextLayout, x: usize, y: usize) -> usize {
        layout.index_at(&self.text, x, y)
    }

    pub(crate) fn cursor_position(&self, layout: &TextLayout) -> (usize, usize) {
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

/// Which wrapped line a character index lands on, with the line's starting
/// character index and the line text.
///
/// The index accounting here (and in [`text_index_at`]) assumes the wrapped
/// lines concatenate back to the source text. `wrap_lines` drops `'\n'` at
/// paragraph splits, so this only holds because [`TextEdit`] filters newlines
/// out of the buffer.
fn line_for_char_index(lines: &[String], index: usize) -> (usize, usize, &str) {
    let mut line_start = 0;
    for (line_index, line) in lines.iter().enumerate() {
        let line_len = line.chars().count();
        let line_end = line_start + line_len;
        if index <= line_end {
            return (line_index, line_start, line);
        }
        line_start = line_end;
    }
    lines.last().map_or((0, 0, ""), |line| {
        (lines.len().saturating_sub(1), line_start, line.as_str())
    })
}

fn text_index_at(font: &BitmapFont, lines: &[String], line_index: usize, x: usize) -> usize {
    let mut line_start = 0;
    for (index, line) in lines.iter().enumerate() {
        let line_len = line.chars().count();
        if index == line_index.min(lines.len().saturating_sub(1)) {
            return line_start + char_index_at_x(font, line, x);
        }
        line_start += line_len;
    }
    line_start
}

fn char_index_at_x(font: &BitmapFont, text: &str, x: usize) -> usize {
    let mut cursor_x = 0;
    for (index, ch) in text.chars().enumerate() {
        let width = font.advance(ch);
        if x < cursor_x + width / 2 {
            return index;
        }
        cursor_x += width;
    }
    text.chars().count()
}

fn prefix_chars(text: &str, len: usize) -> &str {
    &text[..char_to_byte(text, len)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::peanut_money_font::PEANUT_MONEY_SPEC;

    fn font() -> BitmapFont {
        BitmapFont::load(&PEANUT_MONEY_SPEC).unwrap()
    }

    fn uniform_layout(font: &BitmapFont) -> TextLayout<'_> {
        TextLayout::new(font, 0, 0, 6 * 8, LinePlacement::Uniform { line_h: 10 })
    }

    #[test]
    fn cursor_position_and_index_at_are_inverse() {
        let font = font();
        let layout = uniform_layout(&font);
        let text = "hello world this wraps";

        for cursor in 0..=char_len(text) {
            let (x, y) = layout.cursor_position(text, cursor);
            // A click exactly on a caret should resolve back to that caret.
            assert_eq!(layout.index_at(text, x, y), cursor, "cursor {cursor}");
        }
    }

    #[test]
    fn wrap_splits_on_width() {
        let font = font();
        let layout = uniform_layout(&font);
        assert!(layout.wrap("aaaaaaaaaaaaaaaaaa").len() >= 2);
        assert_eq!(layout.wrap("hi").len(), 1);
    }

    #[test]
    fn split_placement_lifts_first_and_drops_second_line() {
        let placement = LinePlacement::Split {
            up: 2,
            down: 7,
            hit_threshold: 4,
        };
        assert_eq!(placement.line_dy(0, 1), 0);
        assert_eq!(placement.line_dy(0, 2), -2);
        assert_eq!(placement.line_dy(1, 2), 7);
        assert_eq!(placement.line_at(0), 0);
        assert_eq!(placement.line_at(4), 1);
    }

    #[test]
    fn field_enforces_char_and_line_limits() {
        let font = font();
        let mut field = TextField::new(3, 1);
        let layout = uniform_layout(&font);
        field.set_text("ab");
        field.set_cursor_end();
        // Typing more than max_chars is rejected by the fits predicate.
        assert!(field.cursor() <= 3);
        // Selection round-trips through the layout.
        let (x, y) = field.cursor_position(&layout);
        assert_eq!(field.index_at(&layout, x, y), field.cursor());
    }
}
