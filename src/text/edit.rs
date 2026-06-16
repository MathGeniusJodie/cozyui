use xkbcommon::xkb::keysyms;

use super::input::{EditKey, KeyInput, edit_key};

#[derive(Clone, Default)]
pub struct TextEdit {
    cursor: usize,
    anchor: Option<usize>,
    undo: Option<String>,
    drag_anchor: Option<usize>,
}

pub enum TextEditOutcome {
    Handled { changed: bool, copy: Option<String> },
    Unhandled,
}

impl TextEdit {
    pub(crate) const fn cursor(&self) -> usize {
        self.cursor
    }

    pub(crate) fn set_cursor(&mut self, cursor: usize, text: &str) {
        self.cursor = cursor.min(char_len(text));
        self.anchor = None;
        self.drag_anchor = None;
    }

    pub(crate) fn begin_drag(&mut self, cursor: usize, text: &str) {
        let cursor = cursor.min(char_len(text));
        self.cursor = cursor;
        self.anchor = None;
        self.drag_anchor = Some(cursor);
    }

    pub(crate) fn drag_to(&mut self, cursor: usize, text: &str) -> bool {
        let Some(anchor) = self.drag_anchor else {
            return false;
        };
        let cursor = cursor.min(char_len(text));
        let old_cursor = self.cursor;
        let old_anchor = self.anchor;
        self.cursor = cursor;
        self.anchor = (anchor != cursor).then_some(anchor);
        old_cursor != self.cursor || old_anchor != self.anchor
    }

    pub(crate) const fn end_drag(&mut self) {
        self.drag_anchor = None;
    }

    pub(crate) const fn is_dragging(&self) -> bool {
        self.drag_anchor.is_some()
    }

    pub(crate) fn select_all(&mut self, text: &str) {
        self.anchor = Some(0);
        self.cursor = char_len(text);
    }

    pub(crate) fn selection_range(&self) -> Option<(usize, usize)> {
        let anchor = self.anchor?;
        if anchor == self.cursor {
            return None;
        }
        Some((anchor.min(self.cursor), anchor.max(self.cursor)))
    }

    pub(crate) fn selected_text(&self, text: &str) -> Option<String> {
        let (start, end) = self.selection_range()?;
        Some(slice_chars(text, start, end).to_string())
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn handle_key<F>(
        &mut self,
        input: &KeyInput,
        text: &mut String,
        clipboard_text: Option<&str>,
        mut can_replace: F,
    ) -> TextEditOutcome
    where
        F: FnMut(&str) -> bool,
    {
        self.clamp(text);

        if input.ctrl() {
            match input.sym_raw() {
                keysyms::KEY_a | keysyms::KEY_A => {
                    self.select_all(text);
                    return TextEditOutcome::Handled {
                        changed: false,
                        copy: None,
                    };
                }
                keysyms::KEY_c | keysyms::KEY_C => {
                    return TextEditOutcome::Handled {
                        changed: false,
                        copy: self.selected_text(text),
                    };
                }
                keysyms::KEY_x | keysyms::KEY_X => {
                    let Some(copy) = self.selected_text(text) else {
                        return TextEditOutcome::Handled {
                            changed: false,
                            copy: None,
                        };
                    };
                    self.save_undo(text);
                    self.delete_selection(text);
                    return TextEditOutcome::Handled {
                        changed: true,
                        copy: Some(copy),
                    };
                }
                keysyms::KEY_v | keysyms::KEY_V => {
                    let Some(paste) = clipboard_text else {
                        return TextEditOutcome::Handled {
                            changed: false,
                            copy: None,
                        };
                    };
                    let changed = self.insert_text(text, paste, &mut can_replace);
                    return TextEditOutcome::Handled {
                        changed,
                        copy: None,
                    };
                }
                keysyms::KEY_z | keysyms::KEY_Z => {
                    let Some(previous) = self.undo.take() else {
                        return TextEditOutcome::Handled {
                            changed: false,
                            copy: None,
                        };
                    };
                    *text = previous;
                    self.cursor = char_len(text);
                    self.anchor = None;
                    return TextEditOutcome::Handled {
                        changed: true,
                        copy: None,
                    };
                }
                _ => {}
            }
        }

        match input.sym_raw() {
            keysyms::KEY_Left | keysyms::KEY_KP_Left => {
                self.move_cursor(text, -1, input.shift());
                TextEditOutcome::Handled {
                    changed: false,
                    copy: None,
                }
            }
            keysyms::KEY_Right | keysyms::KEY_KP_Right => {
                self.move_cursor(text, 1, input.shift());
                TextEditOutcome::Handled {
                    changed: false,
                    copy: None,
                }
            }
            keysyms::KEY_Home | keysyms::KEY_KP_Home => {
                self.move_to(text, 0, input.shift());
                TextEditOutcome::Handled {
                    changed: false,
                    copy: None,
                }
            }
            keysyms::KEY_End | keysyms::KEY_KP_End => {
                self.move_to(text, char_len(text), input.shift());
                TextEditOutcome::Handled {
                    changed: false,
                    copy: None,
                }
            }
            _ => match edit_key(input) {
                EditKey::Insert(ch) => {
                    let changed = self.insert_char(text, ch, &mut can_replace);
                    TextEditOutcome::Handled {
                        changed,
                        copy: None,
                    }
                }
                EditKey::Backspace => {
                    let changed = self.backspace(text);
                    TextEditOutcome::Handled {
                        changed,
                        copy: None,
                    }
                }
                _ => TextEditOutcome::Unhandled,
            },
        }
    }

    fn clamp(&mut self, text: &str) {
        let len = char_len(text);
        self.cursor = self.cursor.min(len);
        if let Some(anchor) = &mut self.anchor {
            *anchor = (*anchor).min(len);
        }
    }

    fn save_undo(&mut self, text: &str) {
        self.undo = Some(text.to_string());
    }

    fn move_cursor(&mut self, text: &str, delta: isize, selecting: bool) {
        let len = char_len(text);
        let cursor = if delta < 0 {
            self.cursor.saturating_sub(delta.unsigned_abs())
        } else {
            (self.cursor + delta as usize).min(len)
        };
        self.move_to(text, cursor, selecting);
    }

    fn move_to(&mut self, text: &str, cursor: usize, selecting: bool) {
        let cursor = cursor.min(char_len(text));
        if selecting {
            self.anchor.get_or_insert(self.cursor);
        } else if let Some((start, end)) = self.selection_range() {
            self.cursor = if cursor < self.cursor { start } else { end };
            self.anchor = None;
            return;
        } else {
            self.anchor = None;
        }
        self.cursor = cursor;
        if self.anchor == Some(self.cursor) {
            self.anchor = None;
        }
    }

    fn insert_char<F>(&mut self, text: &mut String, ch: char, can_replace: &mut F) -> bool
    where
        F: FnMut(&str) -> bool,
    {
        if ch == '\n' || ch == '\r' || ch.is_control() {
            return false;
        }
        let mut insert = String::new();
        insert.push(ch);
        self.insert_text(text, &insert, can_replace)
    }

    fn insert_text<F>(&mut self, text: &mut String, insert: &str, can_replace: &mut F) -> bool
    where
        F: FnMut(&str) -> bool,
    {
        let filtered = insert
            .chars()
            .filter(|ch| *ch != '\n' && *ch != '\r' && !ch.is_control())
            .collect::<String>();
        if filtered.is_empty() {
            return false;
        }

        let mut changed = false;
        for ch in filtered.chars() {
            let candidate = self.candidate_with_insert(text, ch);
            if !can_replace(&candidate) {
                continue;
            }
            if !changed {
                self.save_undo(text);
                if self.selection_range().is_some() {
                    self.delete_selection(text);
                }
            }
            let byte = char_to_byte(text, self.cursor);
            text.insert(byte, ch);
            self.cursor += 1;
            self.anchor = None;
            changed = true;
        }
        changed
    }

    fn candidate_with_insert(&self, text: &str, ch: char) -> String {
        let mut candidate = text.to_string();
        if let Some((start, end)) = self.selection_range() {
            candidate.replace_range(char_to_byte(text, start)..char_to_byte(text, end), "");
            let byte = char_to_byte(&candidate, start);
            candidate.insert(byte, ch);
        } else {
            candidate.insert(char_to_byte(text, self.cursor), ch);
        }
        candidate
    }

    fn backspace(&mut self, text: &mut String) -> bool {
        if self.selection_range().is_some() {
            self.save_undo(text);
            self.delete_selection(text);
            return true;
        }
        if self.cursor == 0 {
            return false;
        }

        self.save_undo(text);
        let start = char_to_byte(text, self.cursor - 1);
        let end = char_to_byte(text, self.cursor);
        text.replace_range(start..end, "");
        self.cursor -= 1;
        true
    }

    fn delete_selection(&mut self, text: &mut String) {
        let Some((start, end)) = self.selection_range() else {
            return;
        };
        text.replace_range(char_to_byte(text, start)..char_to_byte(text, end), "");
        self.cursor = start;
        self.anchor = None;
    }
}

pub fn char_len(text: &str) -> usize {
    text.chars().count()
}

pub fn char_to_byte(text: &str, char_index: usize) -> usize {
    text.char_indices()
        .nth(char_index)
        .map_or(text.len(), |(index, _)| index)
}

pub fn slice_chars(text: &str, start: usize, end: usize) -> &str {
    &text[char_to_byte(text, start)..char_to_byte(text, end)]
}
