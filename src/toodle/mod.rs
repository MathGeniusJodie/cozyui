use std::error::Error;
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::app_color;
use crate::palette_color;
use crate::text::{
    BitmapFont, EditKey, KeyInput, LinePlacement, TextEditOutcome, TextField, TextLayout, edit_key,
};
use crate::{CursorKind, Framebuffer, Index, Paint, Palette, Rect, Sprite, Swap, TRANSPARENT};

mod store;

use store::{
    AtomicWrite, DoneCounts, LINE_COUNT, Priority, SECTION_COUNT, SaveWorker, SectionStore,
    TodoList, archive_transaction_path, daily_done_path, done_dir, recover_archive_transaction,
    todo_file, toodle_root, write_archive_transaction_marker,
};
pub(crate) use store::{PRIORITY_TAGS, done_file_path};

/// toodle's two-line todo layout: a lone line sits on the baseline; once the
/// text wraps, the first line lifts and the second drops.
const TODO_LINE_PLACEMENT: LinePlacement = LinePlacement::Split {
    up: WRAPPED_FIRST_LINE_OFFSET_Y,
    down: WRAPPED_SECOND_LINE_OFFSET_Y,
    hit_threshold: WRAPPED_SECOND_LINE_OFFSET_Y - 3,
};

const CHECK_VARIANTS: usize = 4;

const VISIBLE_PAGE_COUNT: usize = 3;
const PAGE_OFFSET_X: usize = 14;
/// Gap between the front page's right edge and the dice button.
const DICE_GAP: usize = 8;
const PAGE_STACK_OFFSET: usize = 4;
const SHADOW_X_OFFSET: isize = 1;
const SHADOW_Y_OFFSET: isize = 4;
const ERASER_X: usize = 0;
const ERASER_Y: usize = 21;
const PRIORITY_ICON_GAP: usize = 2;
const PRIORITY_ICON_OFFSET_X: usize = 62;
const PRIORITY_ICON_OFFSET_Y: usize = 4;
const GOLDSTAR_Y: usize = 24;
const LINE_Y: [usize; LINE_COUNT] = [73, 95, 117, 139, 161, 183];
const TEXT_X: usize = 31;
const TEXT_Y_OFFSET: usize = 2;
const CHECK_X: usize = 10;
/// Always `LINE_Y[i] - 2`; derived so the two arrays can't drift apart.
const CHECK_Y: [usize; LINE_COUNT] = {
    let mut check_y = [0usize; LINE_COUNT];
    let mut i = 0;
    while i < LINE_COUNT {
        check_y[i] = LINE_Y[i] - 2;
        i += 1;
    }
    check_y
};
/// Half-height of the horizontal band used to isolate one line's checkbox box
/// when blitting it from the combined checkboxes sprite. Half the line pitch so
/// adjacent boxes fall outside the band.
const CHECK_BAND_HALF: usize = 11;
const CHECK_W: usize = 13;
const CHECK_H: usize = 13;
const CHECK_SPRITE_W: usize = 16;
const CHECK_SPRITE_H: usize = 16;
const PAGE_CURL_X: usize = 140;
const PAGE_CURL_Y: usize = 168;
const MAX_TEXT_CHARS: usize = 22;
const LAST_LINE_MAX_TEXT_CHARS: usize = 18;
const WRAPPED_FIRST_LINE_OFFSET_Y: usize = 2;
const WRAPPED_SECOND_LINE_OFFSET_Y: usize = 7;
const PENULTIMATE_LINE_SHADOW_MAX_CHARS: usize = 17;
const LAST_LINE_SHADOW_MAX_CHARS: usize = 14;
const LINE_CLICK_OFFSET_Y: usize = 9;
const PENCIL_TIP_X: usize = 0;
const PENCIL_TIP_Y: usize = 24;
const MAX_TEXT_WIDTH: usize = MAX_TEXT_CHARS * 6;
const LAST_LINE_MAX_TEXT_WIDTH: usize = LAST_LINE_MAX_TEXT_CHARS * 6;

#[derive(Clone, Copy)]
enum PageColor {
    Pink,
    Yellow,
    Green,
    Blue,
}

/// Which todo line (if any) is being edited, paired with the live text-edit
/// buffer for it. These used to be two separate fields (`focused_line:
/// Option<usize>` and a `field: TextField`) kept in sync by convention: every
/// site that focused a line had to remember to also call
/// `load_focused_field()`, and nothing stopped one half from happening
/// without the other. Merging them means a line can't be focused without its
/// text being loaded, and the field can't be inspected without a line being
/// focused — see `Toodle::focus_line`, the only way to move focus onto a
/// line.
enum Focus {
    None,
    Line { line: usize, field: TextField },
}

impl Focus {
    const fn line(&self) -> Option<usize> {
        match self {
            Self::None => None,
            Self::Line { line, .. } => Some(*line),
        }
    }

    /// Retarget the currently-focused line without touching its field (used
    /// when an external edit shifts a followed todo to a new position). A
    /// no-op if nothing is focused.
    fn set_line(&mut self, new_line: usize) {
        if let Self::Line { line, .. } = self {
            *line = new_line;
        }
    }

    /// The focused line's edit buffer. Every call site here is only reached
    /// after confirming (via `line()` or right after `Toodle::focus_line`)
    /// that a line is focused, so `Focus::None` reaching this is a bug.
    fn field(&self) -> &TextField {
        match self {
            Self::None => unreachable!("Focus::field called with nothing focused"),
            Self::Line { field, .. } => field,
        }
    }

    fn field_mut(&mut self) -> &mut TextField {
        match self {
            Self::None => unreachable!("Focus::field_mut called with nothing focused"),
            Self::Line { field, .. } => field,
        }
    }

    fn text(&self) -> &str {
        self.field().text()
    }

    fn cursor(&self) -> usize {
        self.field().cursor()
    }

    fn set_cursor(&mut self, cursor: usize) {
        self.field_mut().set_cursor(cursor);
    }

    fn set_cursor_end(&mut self) {
        self.field_mut().set_cursor_end();
    }

    fn selection_range(&self) -> Option<(usize, usize)> {
        match self {
            Self::None => None,
            Self::Line { field, .. } => field.selection_range(),
        }
    }

    fn handle_key(
        &mut self,
        input: &KeyInput,
        clipboard_text: Option<&str>,
        layout: &TextLayout,
    ) -> TextEditOutcome {
        self.field_mut().handle_key(input, clipboard_text, layout)
    }

    fn index_at(&self, layout: &TextLayout, x: usize, y: usize) -> usize {
        self.field().index_at(layout, x, y)
    }

    fn begin_drag(&mut self, cursor: usize) {
        if let Self::Line { field, .. } = self {
            field.begin_drag(cursor);
        }
    }

    fn drag_to(&mut self, index: usize) -> bool {
        self.field_mut().drag_to(index)
    }

    fn end_drag(&mut self) {
        if let Self::Line { field, .. } = self {
            field.end_drag();
        }
    }

    fn is_dragging(&self) -> bool {
        matches!(self, Self::Line { field, .. } if field.is_dragging())
    }
}

/// The todo the dice last landed on, identified by its position (not its
/// content) so it can be tracked across edits made elsewhere. Drawn crimson
/// and fake-bolded until the dice is rolled again or it stops being eligible.
#[derive(Clone, Copy, PartialEq, Eq)]
struct TodoRef {
    section: Priority,
    page: usize,
    line: usize,
}

pub struct Toodle {
    pages: [Sprite; VISIBLE_PAGE_COUNT],
    checkboxes: Sprite,
    checks: Sprite,
    eraser: Sprite,
    priority_urgent: Sprite,
    priority_frog: Sprite,
    priority_snail: Sprite,
    goldstar: Sprite,
    pencil: Sprite,
    pencil_shadow: Sprite,
    dice: Sprite,
    font: BitmapFont,
    // Per-priority todo lists, each kept in lockstep with its backing markdown
    // file (which other programs may rewrite at any time).
    sections: [SectionStore; SECTION_COUNT],
    // Per-priority count of todos archived into today's done files, used for
    // the gold star.
    done_counts: DoneCounts,
    // Runs section saves (write + fsync + rename) off the UI thread; results
    // are folded back in by maintain().
    save_worker: SaveWorker,
    page: usize,
    // Which line (if any) is focused for editing, paired with its live
    // text-edit buffer; see `Focus`'s doc comment.
    focus: Focus,
    // The todo the dice last landed on; see `TodoRef`'s doc comment.
    highlighted: Option<TodoRef>,
    eraser_hovered: bool,
    // Per-keystroke saves fsync and made typing lag; edits are saved after a
    // typing pause instead (and flushed on shutdown).
    last_edit: Instant,
    // External edits to the backing files are picked up by polling their
    // fingerprints; this throttles the stat calls.
    last_poll: Instant,
    // The static page-stack art (shadows + page images + checkboxes) is the
    // expensive part to draw and only changes when the stack geometry does.
    // We cache it and blit it each frame, painting the live front-page text and
    // cursor on top. The paired `PageArtKey` is the geometry signature it was
    // built for.
    page_art: Option<(Framebuffer, PageArtKey)>,
}

/// Everything the cached page art depends on: which page is on top and how many
/// pages each section contributes (which fixes the stack height and per-page
/// section colors). Text, focus, checks, and hover are painted live on top.
type PageArtKey = (usize, [usize; SECTION_COUNT]);

const SAVE_DEBOUNCE: Duration = Duration::from_millis(500);
const DISK_POLL_INTERVAL: Duration = Duration::from_secs(1);

impl Toodle {
    pub(crate) fn load(_palette: &Palette) -> Result<Self, Box<dyn Error>> {
        fs::create_dir_all(toodle_root())?;
        // A failure here (e.g. a marker truncated by a mid-write sync) must
        // not take the whole app down: the marker file lives in the same
        // sync-editable directory as the todo files themselves, so a
        // corrupt copy is something an external tool can actually produce.
        // Worst case, some already-staged archive writes are left as inert
        // temp files instead of being recovered.
        if let Err(err) = recover_archive_transaction(&archive_transaction_path()) {
            eprintln!(
                "toodle: failed to recover archive transaction, continuing without it: {err}"
            );
        }

        Ok(Self {
            pages: [
                crate::assets::toodle_top(),
                crate::assets::toodle_2nd(),
                crate::assets::toodle_page(),
            ],
            checkboxes: crate::assets::checkboxes(),
            checks: crate::assets::checks(),
            eraser: crate::assets::eraser(),
            priority_urgent: crate::assets::priority_urgent(),
            priority_frog: crate::assets::priority_frog(),
            priority_snail: crate::assets::priority_snail(),
            goldstar: crate::assets::goldstar(),
            pencil: crate::assets::focus_pencil(),
            pencil_shadow: crate::assets::toodle_pencil_shadow(),
            dice: crate::assets::dice(),
            font: BitmapFont::load_with_fallback(
                &pixel_fonts::PEANUT_MONEY_SPEC,
                &pixel_fonts::FUSION_PIXEL_10_SPEC,
            )?,
            sections: [
                SectionStore::load(&todo_file(Priority::Urgent))?,
                SectionStore::load(&todo_file(Priority::Frog))?,
                SectionStore::load(&todo_file(Priority::Normal))?,
                SectionStore::load(&todo_file(Priority::Snail))?,
            ],
            done_counts: DoneCounts::load()?,
            save_worker: SaveWorker::spawn(),
            page: 0,
            focus: Focus::None,
            highlighted: None,
            eraser_hovered: false,
            last_edit: Instant::now(),
            last_poll: Instant::now(),
            page_art: None,
        })
    }

    pub(crate) fn width(&self) -> usize {
        let stack_right =
            (PAGE_OFFSET_X + self.pages[0].width + self.stack_offset()) + SHADOW_X_OFFSET as usize;
        let dice_right = PAGE_OFFSET_X + self.pages[0].width + DICE_GAP + self.dice.width;
        stack_right.max(dice_right)
    }

    pub(crate) fn height(&self) -> usize {
        (self.pages[0].height + self.stack_offset()) + SHADOW_Y_OFFSET as usize
    }

    #[allow(clippy::unused_self)]
    pub(crate) const fn fill_color(&self, _palette: &Palette) -> Index {
        TRANSPARENT
    }

    fn list(&self, section: Priority) -> &TodoList {
        self.sections[section.index()].list()
    }

    /// Geometry signature the cached page art depends on.
    fn page_art_key(&self) -> PageArtKey {
        let mut counts = [0usize; SECTION_COUNT];
        for (count, section) in counts.iter_mut().zip(self.sections.iter()) {
            *count = section.list().page_count();
        }
        (self.page, counts)
    }

    pub(crate) fn render(&mut self, fb: &mut Framebuffer, palette: &Palette) {
        let (w, h) = (self.width(), self.height());
        let key = self.page_art_key();
        let stale = self
            .page_art
            .as_ref()
            .is_none_or(|(art, art_key)| *art_key != key || art.width != w || art.height != h);
        if stale {
            let mut art = match self.page_art.take() {
                Some((art, _)) if art.width == w && art.height == h => art,
                _ => Framebuffer::new(w, h, self.fill_color(palette)),
            };
            self.build_page_art(&mut art, palette);
            self.page_art = Some((art, key));
        }

        if let Some((art, _)) = &self.page_art {
            fb.blit_from(art, 0, 0);
        }
        self.draw_front_overlay(fb, palette);
    }

    /// The expensive, rarely-changing layer: the stacked page shadows, page
    /// images, and checkboxes. Built only when the stack geometry changes.
    fn build_page_art(&self, fb: &mut Framebuffer, palette: &Palette) {
        fb.clear(self.fill_color(palette));

        let page_count = self.logical_page_count();
        for visual_page in (0..page_count).rev() {
            self.draw_page_shadow(fb, palette, visual_page);
        }
        for visual_page in (0..page_count).rev() {
            let logical_page = (self.page + visual_page) % page_count;
            let PageRef { section, .. } = self.page_ref(logical_page);
            let page_offset = visual_page * PAGE_STACK_OFFSET;
            let page_x = PAGE_OFFSET_X + page_offset;
            let page_y = page_offset;
            let page_image = &self.pages[visual_page.min(self.pages.len() - 1)];
            draw_page_image(
                fb,
                page_image,
                section_color(section),
                palette,
                page_x,
                page_y,
            );
            // The front page's checkboxes are painted live (per line, only for
            // checkbox lines) in `draw_front_overlay`; here we only bake the
            // checkboxes of the partially-covered pages beneath it.
            if visual_page != 0 {
                fb.draw_sprite(&self.checkboxes, page_x as isize, page_y as isize, palette);
            }
        }
    }

    /// The live layer, painted on top of the cached art every frame. Only the
    /// front page is interactive and its interior is the only thing not hidden
    /// behind the page on top of it, so this is O(1) in the page count.
    fn draw_front_overlay(&self, fb: &mut Framebuffer, palette: &Palette) {
        let page_count = self.logical_page_count();
        let logical_page = self.page % page_count;
        let PageRef { section, page } = self.page_ref(logical_page);
        let focused_line = self.focus.line();

        self.draw_focused_pencil_shadow(fb, palette);

        let text_color = palette_color::BLACK;
        let completed_text_color = if self.eraser_hovered {
            palette_color::GUNMETAL
        } else {
            text_color
        };
        for (line, _) in LINE_Y.iter().enumerate().take(LINE_COUNT) {
            let todo = self.list(section).item(page, line);
            if todo.renders_checkbox(focused_line == Some(line)) {
                self.draw_checkbox_box(fb, palette, line);
            }
            if todo.checked() {
                self.draw_check(fb, palette, section, line);
            }

            let layout = Self::line_layout(&self.font, line);
            // Externally-rewritten todo files can hold text that wraps past
            // the two-line layout; clamp what we draw so it can't spill into
            // the todo below, and mark the crop with dots ("..." because the
            // fonts have no '…' glyph), shedding characters until they fit.
            // Cursor placement and hit-testing still use the full wrap, so
            // overflow text is reachable but not painted.
            let mut lines = layout.wrap(todo.text());
            if lines.len() > 2 {
                lines.truncate(2);
                let dots_width = self.font.text_width("...");
                let last = &mut lines[1];
                while !last.is_empty()
                    && self.font.text_width(last) + dots_width > max_text_width(line)
                {
                    last.pop();
                }
                last.push_str("...");
            }
            if focused_line == Some(line) {
                layout.draw_selection_lines(
                    fb,
                    &lines,
                    self.focus.selection_range(),
                    palette_color::LAVENDER,
                );
            }
            if self.highlighted
                == Some(TodoRef {
                    section,
                    page,
                    line,
                })
            {
                layout.draw_lines_bold(fb, &lines, palette_color::CRIMSON);
            } else {
                layout.draw_lines(
                    fb,
                    &lines,
                    if todo.checked() {
                        completed_text_color
                    } else {
                        text_color
                    },
                );
            }
        }

        fb.draw_sprite(&self.eraser, ERASER_X as isize, ERASER_Y as isize, palette);
        let (dice_x, dice_y) = self.dice_pos();
        fb.draw_sprite(&self.dice, dice_x as isize, dice_y as isize, palette);
        self.draw_priority_icon(fb, palette);
        self.draw_goldstar(fb, palette);
        self.draw_focused_pencil(fb, palette);
    }

    fn draw_page_shadow(&self, fb: &mut Framebuffer, palette: &Palette, visual_page: usize) {
        let page_image = &self.pages[visual_page.min(self.pages.len() - 1)];
        let page_offset = visual_page * PAGE_STACK_OFFSET;
        let page_x = PAGE_OFFSET_X + page_offset;
        let page_y = page_offset;
        fb.draw_sprite_silhouette(
            page_image,
            page_x as isize + SHADOW_X_OFFSET,
            page_y as isize + SHADOW_Y_OFFSET,
            palette,
            app_color::BACKGROUND_SHADOW_PAINT,
        );
    }

    pub(crate) fn click(&mut self, x: i16, y: i16) -> Result<bool, Box<dyn Error>> {
        self.focus.end_drag();
        if self.eraser_at(x, y) {
            self.archive_completed_todos()?;
            self.focus = Focus::None;
            return Ok(false);
        }

        if self.dice_at(x, y) {
            self.roll_highlight();
            self.focus = Focus::None;
            return Ok(false);
        }

        let abs_x = x.max(0) as usize;
        let abs_y = y.max(0) as usize;
        let Some(page_x) = abs_x.checked_sub(PAGE_OFFSET_X) else {
            self.focus = Focus::None;
            return Ok(false);
        };

        if page_x >= PAGE_CURL_X && abs_y >= PAGE_CURL_Y {
            self.page = (self.page + 1) % self.logical_page_count();
            self.focus = Focus::None;
            return Ok(false);
        }

        if let Some(line) = checkbox_at(page_x, abs_y)
            && self.checkbox_clickable(line)
        {
            let PageRef { section, page } = self.current_page_ref();
            let checked = self.sections[section.index()]
                .list_mut()
                .item_mut(page, line)
                .toggle_checked();
            let todo_ref = TodoRef {
                section,
                page,
                line,
            };
            if checked && self.highlighted == Some(todo_ref) {
                self.highlighted = None;
            }
            self.save_current_section()?;
            return Ok(checked && self.twirl_on_check_page());
        }

        match line_at(abs_y) {
            Some(line) => {
                self.focus_line(line);
                let cursor =
                    self.focus
                        .index_at(&Self::line_layout(&self.font, line), abs_x, abs_y);
                self.focus.begin_drag(cursor);
            }
            None => self.focus = Focus::None,
        }
        Ok(false)
    }

    pub(crate) fn drag_text(&mut self, x: i16, y: i16) -> bool {
        if !self.focus.is_dragging() {
            return false;
        }

        let Some(line) = self.focus.line() else {
            return false;
        };
        let abs_x = x.max(0) as usize;
        let abs_y = y.max(0) as usize;
        let cursor = self
            .focus
            .index_at(&Self::line_layout(&self.font, line), abs_x, abs_y);
        self.focus.drag_to(cursor)
    }

    pub(crate) fn end_text_drag(&mut self) {
        self.focus.end_drag();
    }

    pub(crate) fn text_dragging(&self) -> bool {
        self.focus.is_dragging()
    }

    /// Mirrors the hit-testing in `click`, without side effects.
    pub(crate) fn cursor_at(&self, x: i16, y: i16) -> CursorKind {
        if self.eraser_at(x, y) || self.dice_at(x, y) {
            return CursorKind::Hand;
        }
        let abs_x = x.max(0) as usize;
        let abs_y = y.max(0) as usize;
        let Some(page_x) = abs_x.checked_sub(PAGE_OFFSET_X) else {
            return CursorKind::Pointer;
        };
        if page_x >= PAGE_CURL_X && abs_y >= PAGE_CURL_Y {
            return CursorKind::Hand;
        }
        if let Some(line) = checkbox_at(page_x, abs_y)
            && self.checkbox_clickable(line)
        {
            return CursorKind::Hand;
        }
        if line_at(abs_y).is_some() {
            CursorKind::Text
        } else {
            CursorKind::Pointer
        }
    }

    /// Whether `line` on the front page has a clickable checkbox. Requiring
    /// content (or an existing check) keeps a click on the empty checkbox
    /// area of a blank line from materializing a phantom `- [x] ` todo (the
    /// blank-item sentinel is a checkbox, so `is_checkbox` alone passes).
    fn checkbox_clickable(&self, line: usize) -> bool {
        let PageRef { section, page } = self.current_page_ref();
        let item = self.list(section).item(page, line);
        item.is_checkbox() && (item.checked() || !item.text().is_empty())
    }

    pub(crate) fn hover(&mut self, x: i16, y: i16) -> bool {
        let was_hovered = self.eraser_hovered;
        if x < 0 || y < 0 {
            self.eraser_hovered = false;
            return was_hovered;
        }
        self.eraser_hovered = self.eraser_at(x, y);
        was_hovered != self.eraser_hovered
    }

    pub(crate) fn handle_key_press(
        &mut self,
        input: &KeyInput,
        clipboard_text: Option<&str>,
    ) -> Result<Option<String>, Box<dyn Error>> {
        let Some(line) = self.focus.line() else {
            return Ok(None);
        };

        let PageRef { section, page } = self.current_page_ref();
        if matches!(edit_key(input), EditKey::Backspace) && self.focus.text().is_empty() {
            // Deleting goes through the async save path like every other
            // edit, so a backspace never blocks the UI thread on fsyncs. If a
            // save is already in flight, begin_save (inside
            // save_current_section) leaves the section dirty and the debounce
            // retries once it completes — no rename race, no lost delete.
            if self.sections[section.index()]
                .list_mut()
                .delete_item(page, line)
            {
                self.sections[section.index()].mark_dirty();
                self.last_edit = Instant::now();
                self.save_current_section()?;
            }
            self.keep_section_page_visible(PageRef { section, page });
            self.focus_line(line);
            self.focus.set_cursor(0);
            return Ok(None);
        }

        let layout = Self::line_layout(&self.font, line);
        let outcome = self.focus.handle_key(input, clipboard_text, &layout);
        if let TextEditOutcome::Handled { changed, copy } = outcome {
            if changed {
                let text = self.focus.text().to_string();
                self.sections[section.index()]
                    .list_mut()
                    .item_mut(page, line)
                    .set_text(text);
                // Deferred save: an fsync per keystroke makes typing lag.
                self.sections[section.index()].mark_dirty();
                self.last_edit = Instant::now();
                self.keep_section_page_visible(PageRef { section, page });
            }
            return Ok(copy);
        }

        match edit_key(input) {
            EditKey::Enter => {
                self.focus_line((line + 1).min(LINE_COUNT - 1));
                self.focus.set_cursor_end();
            }
            EditKey::Escape => {
                self.focus = Focus::None;
            }
            // No line-editing action for these: Tab (no focus navigation
            // here) or an unrecognized/textless key press.
            EditKey::Tab | EditKey::None => return Ok(None),
            // `self.focus.handle_key` above always consumes these itself
            // (returning `Handled`, which returns early before this match),
            // so they can never actually reach here. Panicking rather than
            // silently no-opping means a change that broke that invariant
            // gets noticed immediately instead of just quietly dropping
            // input.
            EditKey::Left | EditKey::Right | EditKey::Insert(_) | EditKey::Backspace => {
                unreachable!("TextEdit::handle_key already consumes Left/Right/Insert/Backspace")
            }
        }

        Ok(None)
    }

    /// The text layout for todo `line` on the front page, in absolute
    /// framebuffer coordinates.
    const fn line_layout(font: &BitmapFont, line: usize) -> TextLayout<'_> {
        TextLayout::new(
            font,
            PAGE_OFFSET_X + TEXT_X,
            LINE_Y[line] - TEXT_Y_OFFSET,
            max_text_width(line),
            TODO_LINE_PLACEMENT,
        )
    }

    /// Focus `line` on the current page and load its text into a fresh
    /// edit buffer. This is the only way to move focus onto a line — see
    /// `Focus`'s doc comment for why that pairing matters.
    fn focus_line(&mut self, line: usize) {
        let PageRef { section, page } = self.current_page_ref();
        let mut field = TextField::new(usize::MAX, 2);
        field.set_text(self.list(section).item(page, line).text());
        self.focus = Focus::Line { line, field };
    }

    /// Re-sync the focused line's edit buffer with the todo it currently
    /// points at (its content may have changed via an external edit). The
    /// cursor is clamped in place rather than reset, so the user's position
    /// survives when the text is unchanged. A no-op when nothing is focused.
    fn reload_focused_field(&mut self) {
        let Some(line) = self.focus.line() else {
            return;
        };
        let PageRef { section, page } = self.current_page_ref();
        let text = self.list(section).item(page, line).text().to_string();
        self.focus.field_mut().set_text(&text);
    }

    /// Blit just the front page's checkbox box for one line out of the combined
    /// (page-sized) checkboxes sprite. The box rows are isolated by a horizontal
    /// band centered on the line so neighbouring boxes are untouched.
    fn draw_checkbox_box(&self, fb: &mut Framebuffer, palette: &Palette, line: usize) {
        let band_top = CHECK_Y[line].saturating_sub(CHECK_BAND_HALF);
        let src = Rect::new(0, band_top, self.checkboxes.width, CHECK_BAND_HALF * 2);
        fb.draw_sprite_full(
            &self.checkboxes,
            src,
            PAGE_OFFSET_X as isize,
            band_top as isize,
            None,
            palette,
            None,
        );
    }

    /// `section` seeds which of `CHECK_VARIANTS` check-mark sprites gets
    /// drawn (varied per section+line so checks don't all look identical),
    /// not a page index — only the front page's checks are ever live-drawn
    /// (see `draw_front_overlay`'s doc comment), so there's no page-relative
    /// offset to apply here either.
    fn draw_check(&self, fb: &mut Framebuffer, palette: &Palette, section: Priority, line: usize) {
        let src_x = (section.index() + line) % CHECK_VARIANTS * CHECK_SPRITE_W;
        let dest_x = PAGE_OFFSET_X + CHECK_X - 1;
        let dest_y = CHECK_Y[line] - 4;
        let src = Rect::new(src_x, 0, CHECK_SPRITE_W, CHECK_SPRITE_H);

        let swap = if self.eraser_hovered {
            Swap::uniform(Paint::Solid(palette_color::GUNMETAL))
        } else {
            Swap::identity()
        };
        fb.draw_sprite_full(
            &self.checks,
            src,
            dest_x as isize,
            dest_y as isize,
            None,
            palette,
            Some(&swap),
        );
    }

    fn save_current_section(&mut self) -> Result<(), Box<dyn Error>> {
        let current_page = self.current_page_ref();
        let section = current_page.section;
        // Async save: the fsync-heavy disk work happens on the save worker.
        // If a save for this section is already in flight the section stays
        // dirty and maintain()'s debounce retries once it completes.
        let (externally_changed, text) =
            self.sections[section.index()].begin_save(&todo_file(section))?;
        if let Some(text) = text {
            self.save_worker.submit(section, todo_file(section), text);
        }
        if externally_changed {
            self.fix_ui_after_sync(current_page);
        } else {
            self.keep_section_page_visible(current_page);
        }
        Ok(())
    }

    /// Keep the widget in lockstep with disk: absorb external edits to the
    /// backing files and write debounced edits once typing has paused. Call
    /// regularly; returns whether anything visible changed (redraw needed).
    pub(crate) fn maintain(&mut self) -> Result<bool, Box<dyn Error>> {
        self.drain_save_results();
        let mut changed = false;
        if self.last_poll.elapsed() >= DISK_POLL_INTERVAL {
            self.last_poll = Instant::now();
            changed = self.sync_with_disk()?;
        }
        if self.sections.iter().any(SectionStore::is_dirty)
            && self.last_edit.elapsed() >= SAVE_DEBOUNCE
        {
            changed |= self.queue_saves()?;
        }
        Ok(changed)
    }

    /// Fold in finished background saves: success adopts the written version
    /// as the synced base, failure re-flags the section dirty for a retry.
    /// Neither changes anything visible.
    fn drain_save_results(&mut self) {
        while let Some((section, result)) = self.save_worker.try_result() {
            self.sections[section.index()].complete_save(result);
        }
    }

    /// Block until no background save is in flight; the synchronous flush and
    /// archive paths must not race the worker's renames.
    fn wait_for_saves(&mut self) {
        while self.sections.iter().any(SectionStore::is_saving) {
            let Some((section, result)) = self.save_worker.wait_result() else {
                return;
            };
            self.sections[section.index()].complete_save(result);
        }
    }

    /// Queue an async save for every dirty section without one in flight.
    /// Each save first absorbs unseen external changes to its file, so this
    /// can alter the lists; returns whether that happened.
    fn queue_saves(&mut self) -> Result<bool, Box<dyn Error>> {
        let current_page = self.current_page_ref();
        let mut externally_changed = false;
        for section in Priority::ALL {
            if !self.sections[section.index()].is_dirty() {
                continue;
            }
            let (changed, text) = self.sections[section.index()].begin_save(&todo_file(section))?;
            externally_changed |= changed;
            if let Some(text) = text {
                self.save_worker.submit(section, todo_file(section), text);
            }
        }
        if externally_changed {
            self.fix_ui_after_sync(current_page);
        }
        Ok(externally_changed)
    }

    /// One reconciliation pass: fold in external edits to the four todo files
    /// and refresh the gold-star counts. Returns whether anything changed.
    fn sync_with_disk(&mut self) -> Result<bool, Box<dyn Error>> {
        let current_page = self.current_page_ref();
        let mut lists_changed = false;
        for section in Priority::ALL {
            lists_changed |= self.sections[section.index()].absorb_external(&todo_file(section))?;
        }
        if lists_changed {
            self.fix_ui_after_sync(current_page);
        }
        let counts_changed = self.done_counts.refresh()?;
        Ok(lists_changed || counts_changed)
    }

    /// Write all pending edits now, synchronously (shutdown). Waits out any
    /// in-flight background saves first. Each save absorbs any unseen
    /// external change to its file, so this can also alter the lists;
    /// returns whether that happened.
    pub(crate) fn flush_saves(&mut self) -> Result<bool, Box<dyn Error>> {
        self.wait_for_saves();
        let current_page = self.current_page_ref();
        let mut externally_changed = false;
        for section in Priority::ALL {
            if self.sections[section.index()].is_dirty() {
                externally_changed |= self.sections[section.index()].save(&todo_file(section))?;
            }
        }
        if externally_changed {
            self.fix_ui_after_sync(current_page);
        }
        Ok(externally_changed)
    }

    /// After external changes are folded in, page counts and item positions
    /// may have shifted; keep the visible page, the focused line, and the dice
    /// highlight pointing at sensible things. `current_page` is the page that
    /// was showing before the sync.
    fn fix_ui_after_sync(&mut self, current_page: PageRef) {
        self.keep_section_page_visible(current_page);

        if let Some(line) = self.focus.line() {
            let text = self.focus.text();
            let index = if text.is_empty() {
                // A blank focused line has no identity to follow; any blank
                // spot is as good as another, so stay put.
                None
            } else {
                let old_index = current_page.page * LINE_COUNT + line;
                self.list(current_page.section)
                    .items
                    .iter()
                    .enumerate()
                    .filter(|(_, item)| item.text() == text)
                    .map(|(index, _)| index)
                    // Several todos can share text; follow whichever match sits
                    // closest to where focus used to be, preferring the earlier
                    // one on a tie, rather than always snapping to the first.
                    .min_by_key(|&index| (index as isize - old_index as isize).unsigned_abs())
            };
            if let Some(index) = index {
                // Follow the focused todo to wherever it landed.
                self.focus.set_line(index % LINE_COUNT);
                self.keep_section_page_visible(PageRef {
                    section: current_page.section,
                    page: index / LINE_COUNT,
                });
            }
            // Show whatever is at the focused spot now (the todo may have been
            // edited or removed on disk); the cursor is clamped by the field.
            self.reload_focused_field();
        }

        if let Some(todo_ref) = self.highlighted
            && !self.still_eligible(todo_ref)
        {
            self.highlighted = None;
        }
    }

    fn archive_completed_todos(&mut self) -> Result<(), Box<dyn Error>> {
        // Finish off any transaction left half-committed by an earlier failed
        // attempt in this same session (see the commit loop below) before
        // starting a new one: otherwise the marker written below would
        // overwrite the old one, silently orphaning whatever temp files it
        // still referenced.
        if let Err(err) = recover_archive_transaction(&archive_transaction_path()) {
            eprintln!(
                "toodle: failed to recover archive transaction, continuing without it: {err}"
            );
        }
        // The transaction below renames section files itself; it must not
        // interleave with the save worker's renames.
        self.wait_for_saves();
        // Fold in external edits first so the sweep operates on what is really
        // on disk, not a stale view.
        self.sync_with_disk()?;
        let current_page = self.current_page_ref();

        let mut archived: [Vec<String>; SECTION_COUNT] = std::array::from_fn(|_| Vec::new());
        let mut changed_sections = [false; SECTION_COUNT];
        let mut staged_lists: Vec<TodoList> = self
            .sections
            .iter()
            .map(|section| section.list().clone())
            .collect();

        for (section, list) in Priority::ALL.into_iter().zip(staged_lists.iter_mut()) {
            let mut remaining = Vec::new();
            for item in std::mem::take(&mut list.items) {
                if item.checked() {
                    if !item.text().trim().is_empty() {
                        archived[section.index()].push(item.text().to_string());
                    }
                    changed_sections[section.index()] = true;
                } else {
                    remaining.push(item);
                }
            }

            list.items = remaining;
            if changed_sections[section.index()] {
                list.trim_trailing_blank_items();
            }
        }

        // Stage every write up front (per section: an optional done-file write
        // and an optional section-file write) without committing anything yet,
        // so the transaction marker below can describe the whole operation
        // before any rename happens.
        if archived.iter().any(|section| !section.is_empty()) {
            fs::create_dir_all(done_dir())?;
        }
        let mut staged: StagedArchiveWrites = std::array::from_fn(|_| (None, None));
        for (section, archived_section) in Priority::ALL.into_iter().zip(archived.iter()) {
            if archived_section.is_empty() {
                continue;
            }

            let done_path = daily_done_path(section);
            let mut done_text = if Path::new(&done_path).exists() {
                fs::read_to_string(&done_path)?
            } else {
                String::new()
            };
            if !done_text.is_empty() && !done_text.ends_with('\n') {
                done_text.push('\n');
            }
            for todo in archived_section {
                done_text.push_str("- [x] ");
                done_text.push_str(todo);
                done_text.push('\n');
            }
            staged[section.index()].0 =
                Some(AtomicWrite::stage(&done_path, done_text.into_bytes())?);
        }
        for (section, changed) in Priority::ALL.into_iter().zip(changed_sections) {
            if changed {
                let text = staged_lists[section.index()].serialized_text();
                let write = AtomicWrite::stage(&todo_file(section), text.as_bytes())?;
                staged[section.index()].1 = Some((text, write));
            }
        }

        // The marker covers every staged write across all sections, since
        // recovery only ever finishes renames whose temp file still exists
        // (see `recover_archive_transaction`): once a section's writes are
        // committed, their temp files are gone and recovery silently skips
        // them, so writing the marker once up front and only removing it
        // after the whole loop below finishes is still correct even though
        // the loop commits (and adopts) one section at a time.
        let marker_path = archive_transaction_path();
        let marker_writes: Vec<&AtomicWrite> = staged
            .iter()
            .flat_map(|(done_write, section_write)| {
                done_write
                    .iter()
                    .chain(section_write.iter().map(|(_, write)| write))
            })
            .collect();
        write_archive_transaction_marker(&marker_path, &marker_writes)?;
        // From here on the marker is the sole authority on whether a staged
        // temp file still needs committing (see `AtomicWrite::disarm`): if an
        // error below abandons some of these writes, they must be left on
        // disk for `recover_archive_transaction` to find on restart, not
        // deleted by an ordinary `Drop`.
        for (done_write, section_write) in &mut staged {
            if let Some(write) = done_write {
                write.disarm();
            }
            if let Some((_, write)) = section_write {
                write.disarm();
            }
        }

        // Commit and adopt one section at a time: the done write, then the
        // section write, then folding the in-memory list into place. If a
        // later section's commit fails, every earlier section is already
        // fully committed *and* adopted, so nothing is left half-applied
        // except (at most) the section that was in flight when the error
        // hit, and the marker above lets a subsequent restart finish that
        // section's rename if it had made it to disk.
        for (section, list) in Priority::ALL.into_iter().zip(staged_lists) {
            let (done_write, section_write) = &mut staged[section.index()];
            if let Some(write) = done_write.take() {
                write.commit()?;
            }
            if let Some((text, write)) = section_write.take() {
                // Once the done-file write above commits, the sweep is
                // irreversible: fold the trimmed list into memory even if
                // this section-file write fails, so a same-session retry
                // never re-collects (and re-appends) the same items into the
                // done file a second time. The stale on-disk section file is
                // left for the marker-based recovery above (on the next
                // retry or restart) or the ordinary dirty-save path to catch.
                match write.commit() {
                    Ok(fingerprint) => {
                        self.sections[section.index()].adopt(list, text, fingerprint);
                    }
                    Err(err) => {
                        *self.sections[section.index()].list_mut() = list;
                        self.sections[section.index()].mark_dirty();
                        return Err(err);
                    }
                }
            }
        }
        fs::remove_file(&marker_path)?;

        self.done_counts.refresh()?;
        self.keep_section_page_visible(current_page);

        Ok(())
    }

    fn draw_priority_icon(&self, fb: &mut Framebuffer, palette: &Palette) {
        let Some(icon) = self.priority_icon() else {
            return;
        };
        let icon_x = ERASER_X + self.eraser.width + PRIORITY_ICON_GAP + PRIORITY_ICON_OFFSET_X;
        let icon_y = ERASER_Y + PRIORITY_ICON_OFFSET_Y;
        fb.draw_sprite(icon, icon_x as isize, icon_y as isize, palette);
    }

    fn priority_icon(&self) -> Option<&Sprite> {
        match section_color(self.current_page_ref().section) {
            PageColor::Pink => Some(&self.priority_urgent),
            PageColor::Yellow => None,
            PageColor::Green => Some(&self.priority_frog),
            PageColor::Blue => Some(&self.priority_snail),
        }
    }

    fn twirl_on_check_page(&self) -> bool {
        matches!(
            section_color(self.current_page_ref().section),
            PageColor::Pink | PageColor::Green
        )
    }

    fn stack_offset(&self) -> usize {
        self.logical_page_count().saturating_sub(1) * PAGE_STACK_OFFSET
    }

    fn logical_page_count(&self) -> usize {
        self.sections
            .iter()
            .map(|section| section.list().page_count())
            .sum()
    }

    fn current_page_ref(&self) -> PageRef {
        self.page_ref(self.page)
    }

    fn page_ref(&self, mut page: usize) -> PageRef {
        for (section, store) in Priority::ALL.into_iter().zip(self.sections.iter()) {
            let section_pages = store.list().page_count();
            if page < section_pages {
                return PageRef { section, page };
            }
            page -= section_pages;
        }

        PageRef {
            section: Priority::Snail,
            page: self.list(Priority::Snail).page_count().saturating_sub(1),
        }
    }

    fn page_index_for(&self, section: Priority, section_page: usize) -> usize {
        self.sections
            .iter()
            .take(section.index())
            .map(|store| store.list().page_count())
            .sum::<usize>()
            + section_page.min(self.list(section).page_count().saturating_sub(1))
    }

    fn keep_section_page_visible(&mut self, page: PageRef) {
        let section_pages = self.list(page.section).page_count();
        self.page =
            self.page_index_for(page.section, page.page.min(section_pages.saturating_sub(1)));
    }

    /// Whether `todo_ref` still points at a valid dice-roll target: a
    /// checkbox line, unchecked, with non-blank text, on a page that still
    /// exists. The single source of this predicate so revalidating the
    /// highlight after an external edit can't drift from the predicate that
    /// picked it in the first place (see `roll_highlight`).
    fn still_eligible(&self, todo_ref: TodoRef) -> bool {
        let list = self.list(todo_ref.section);
        if todo_ref.page >= list.page_count() {
            return false;
        }
        let item = list.item(todo_ref.page, todo_ref.line);
        item.is_checkbox() && !item.checked() && !item.text().trim().is_empty()
    }

    /// Gold-star tally for the current priority: todos already archived into
    /// today's done file plus checked todos not yet swept there.
    fn goldstar_count(&self) -> usize {
        let section = self.current_page_ref().section;
        let checked_unarchived = self
            .list(section)
            .items
            .iter()
            .filter(|item| item.checked() && !item.text().trim().is_empty())
            .count();
        self.done_counts.count(section) + checked_unarchived
    }

    fn draw_goldstar(&self, fb: &mut Framebuffer, palette: &Palette) {
        let star_x = PAGE_OFFSET_X + self.pages[0].width - self.goldstar.width;
        fb.draw_sprite(
            &self.goldstar,
            star_x as isize,
            GOLDSTAR_Y as isize,
            palette,
        );

        let count = self.goldstar_count().to_string();
        let text_w = self.font.text_width(&count);
        let text_h = self.font.cell_h();
        let text_x = star_x + self.goldstar.width.saturating_sub(text_w) / 2;
        let text_y = GOLDSTAR_Y + self.goldstar.height.saturating_sub(text_h) / 2;

        self.font.draw_text_limited(
            fb,
            &count,
            text_x,
            text_y,
            palette_color::BLACK,
            count.chars().count(),
        );
    }

    fn draw_pencil_shadow_cursor(
        &self,
        fb: &mut Framebuffer,
        palette: &Palette,
        line: usize,
        x: usize,
        y: usize,
        pencil_on_second_line: bool,
    ) {
        // `x`/`y` already include the page offset (they come from the absolute
        // text layout).
        let dest_x = x.saturating_sub(PENCIL_TIP_X);
        let dest_y = y.saturating_sub(PENCIL_TIP_Y);

        if self.should_draw_pencil_shadow(line) {
            draw_pencil_shadow(
                fb,
                &self.pencil_shadow,
                dest_x,
                dest_y,
                section_color(self.current_page_ref().section),
                palette,
                pencil_on_second_line,
            );
        }
    }

    fn draw_pencil_cursor(&self, fb: &mut Framebuffer, palette: &Palette, x: usize, y: usize) {
        // `x`/`y` already include the page offset (they come from the absolute
        // text layout).
        let dest_x = x.saturating_sub(PENCIL_TIP_X);
        let dest_y = y.saturating_sub(PENCIL_TIP_Y);

        fb.draw_sprite(&self.pencil, dest_x as isize, dest_y as isize, palette);
    }

    fn draw_focused_pencil_shadow(&self, fb: &mut Framebuffer, palette: &Palette) {
        let Some((line, cursor_x, cursor_y, pencil_on_second_line)) =
            self.focused_pencil_position()
        else {
            return;
        };

        self.draw_pencil_shadow_cursor(
            fb,
            palette,
            line,
            cursor_x,
            cursor_y,
            line == LINE_COUNT - 1 || pencil_on_second_line,
        );
    }

    fn draw_focused_pencil(&self, fb: &mut Framebuffer, palette: &Palette) {
        let Some((_, cursor_x, cursor_y, _)) = self.focused_pencil_position() else {
            return;
        };

        self.draw_pencil_cursor(fb, palette, cursor_x, cursor_y);
    }

    fn focused_pencil_position(&self) -> Option<(usize, usize, usize, bool)> {
        let line = self.focus.line()?;
        let PageRef { section, page } = self.current_page_ref();
        let todo = self.list(section).item(page, line);
        let layout = Self::line_layout(&self.font, line);
        let (cursor_x, cursor_y) = layout.cursor_position(todo.text(), self.focus.cursor());
        // A cursor sitting in overflow text (wrapped past the two drawn
        // lines) would place the pencil inside the todo below; pin it to the
        // second line's row instead.
        let cursor_y = cursor_y.min(LINE_Y[line] - TEXT_Y_OFFSET + WRAPPED_SECOND_LINE_OFFSET_Y);
        Some((line, cursor_x, cursor_y, layout.wrap(todo.text()).len() > 1))
    }

    fn should_draw_pencil_shadow(&self, line: usize) -> bool {
        let PageRef { section, page } = self.current_page_ref();
        let char_count = self.list(section).item(page, line).text().chars().count();
        match line {
            line if line == LINE_COUNT - 2 => char_count <= PENULTIMATE_LINE_SHADOW_MAX_CHARS,
            line if line == LINE_COUNT - 1 => char_count <= LAST_LINE_SHADOW_MAX_CHARS,
            _ => true,
        }
    }

    /// Top-left corner of the dice button: just past the front page's right
    /// edge (clear of the page-curl hotspot), aligned to the page's bottom.
    const fn dice_pos(&self) -> (usize, usize) {
        let page = &self.pages[0];
        let x = PAGE_OFFSET_X + page.width + DICE_GAP;
        let y = page.height.saturating_sub(self.dice.height);
        (x, y)
    }

    fn dice_at(&self, x: i16, y: i16) -> bool {
        let (dice_x, dice_y) = self.dice_pos();
        point_in_rect(x, y, dice_x, dice_y, self.dice.width, self.dice.height)
    }

    /// Highlight a random non-blank todo on the current page; clears the
    /// highlight if the page has no eligible todos.
    fn roll_highlight(&mut self) {
        let PageRef { section, page } = self.current_page_ref();
        let candidates: Vec<usize> = (0..LINE_COUNT)
            .filter(|&line| {
                self.still_eligible(TodoRef {
                    section,
                    page,
                    line,
                })
            })
            .collect();
        self.highlighted = candidates
            .get(crate::util::random_index(candidates.len()))
            .map(|&line| TodoRef {
                section,
                page,
                line,
            });
    }

    fn eraser_at(&self, x: i16, y: i16) -> bool {
        point_in_rect(
            x,
            y,
            ERASER_X,
            ERASER_Y,
            self.eraser.width,
            self.eraser.height,
        )
    }
}

impl crate::widget::Widget for Toodle {
    fn width(&self) -> usize {
        self.width()
    }

    fn height(&self) -> usize {
        self.height()
    }

    fn fill_color(&self, palette: &Palette) -> Index {
        self.fill_color(palette)
    }

    fn render(&mut self, fb: &mut Framebuffer, palette: &Palette) {
        Self::render(self, fb, palette);
    }

    fn click(
        &mut self,
        x: i16,
        y: i16,
        _state: u16,
    ) -> Result<crate::widget::ClickOutcome, Box<dyn Error>> {
        let spin_twirl = Self::click(self, x, y)?;
        Ok(crate::widget::ClickOutcome {
            spin_twirl,
            text_drag: self.text_dragging(),
            copy_text: None,
        })
    }

    fn blur(&mut self) {
        self.focus = Focus::None;
    }

    fn hover(&mut self, x: i16, y: i16) -> bool {
        Self::hover(self, x, y)
    }

    fn cursor_at(&self, x: i16, y: i16) -> CursorKind {
        self.cursor_at(x, y)
    }

    fn handle_key_press(
        &mut self,
        input: &KeyInput,
        clipboard_text: Option<&str>,
    ) -> Result<Option<String>, Box<dyn Error>> {
        Self::handle_key_press(self, input, clipboard_text)
    }

    fn wants_clipboard(&self, input: &KeyInput) -> bool {
        input.is_plain_paste_shortcut()
    }

    fn drag_text(&mut self, x: i16, y: i16) -> bool {
        Self::drag_text(self, x, y)
    }

    fn end_text_drag(&mut self) {
        Self::end_text_drag(self);
    }
}

#[derive(Clone, Copy)]
struct PageRef {
    section: Priority,
    page: usize,
}

/// Per-section staged archive writes: an optional done-file write and an
/// optional (serialized text, section-file write) pair.
type StagedArchiveWrites = [(Option<AtomicWrite>, Option<(String, AtomicWrite)>); SECTION_COUNT];

fn draw_page_image(
    fb: &mut Framebuffer,
    image: &Sprite,
    page_color: PageColor,
    palette: &Palette,
    x: usize,
    y: usize,
) {
    fb.draw_sprite_swapped(
        image,
        (x) as isize,
        (y) as isize,
        palette,
        &page_swap(page_color, false),
    );
}

fn draw_pencil_shadow(
    fb: &mut Framebuffer,
    image: &Sprite,
    dest_x: usize,
    dest_y: usize,
    page_color: PageColor,
    palette: &Palette,
    pencil_on_second_line: bool,
) {
    fb.draw_sprite_swapped(
        image,
        dest_x as isize,
        dest_y as isize,
        palette,
        &page_swap(page_color, pencil_on_second_line),
    );
}

fn page_swap(page_color: PageColor, pencil_on_second_line: bool) -> Swap {
    let remap = page_remap(page_color);
    let mut indices = *remap;
    if pencil_on_second_line {
        indices[palette_color::LIME as usize] = remap[palette_color::ROSE as usize];
    }
    Swap::from_indices(&indices)
}

const fn section_color(section: Priority) -> PageColor {
    match section {
        Priority::Urgent => PageColor::Pink,
        Priority::Frog => PageColor::Green,
        Priority::Normal => PageColor::Yellow,
        Priority::Snail => PageColor::Blue,
    }
}

const fn page_remap(page_color: PageColor) -> &'static [Index; 16] {
    match page_color {
        PageColor::Pink => &PINK_PAGE_REMAP,
        PageColor::Yellow => &YELLOW_PAGE_REMAP,
        PageColor::Green => &GREEN_PAGE_REMAP,
        PageColor::Blue => &BLUE_PAGE_REMAP,
    }
}

const IDENTITY_PAGE_REMAP: [Index; 16] = [
    palette_color::LAVENDER,
    palette_color::GUNMETAL,
    palette_color::PLUM,
    palette_color::BROWN,
    palette_color::PEACH,
    palette_color::CREAM,
    palette_color::LIME,
    palette_color::GREEN,
    palette_color::ORANGE,
    palette_color::CRIMSON,
    palette_color::ROSE,
    palette_color::PURPLE,
    palette_color::CYAN,
    palette_color::BLUE,
    palette_color::PINE,
    palette_color::BLACK,
];

const PINK_PAGE_REMAP: [Index; 16] = {
    let mut remap = IDENTITY_PAGE_REMAP;
    remap[palette_color::LIME as usize] = palette_color::CRIMSON;
    remap[palette_color::PINE as usize] = palette_color::ROSE;
    remap
};

const YELLOW_PAGE_REMAP: [Index; 16] = {
    let mut remap = IDENTITY_PAGE_REMAP;
    remap[palette_color::LAVENDER as usize] = palette_color::CYAN;
    remap[palette_color::PEACH as usize] = palette_color::CREAM;
    remap[palette_color::LIME as usize] = palette_color::CRIMSON;
    remap[palette_color::CRIMSON as usize] = palette_color::BROWN;
    remap[palette_color::ROSE as usize] = palette_color::ORANGE;
    remap[palette_color::PINE as usize] = palette_color::ROSE;
    remap
};

const GREEN_PAGE_REMAP: [Index; 16] = {
    let mut remap = IDENTITY_PAGE_REMAP;
    remap[palette_color::PEACH as usize] = palette_color::LIME;
    remap[palette_color::LIME as usize] = palette_color::GUNMETAL;
    remap[palette_color::ORANGE as usize] = palette_color::GREEN;
    remap[palette_color::CRIMSON as usize] = palette_color::PINE;
    remap[palette_color::ROSE as usize] = palette_color::GREEN;
    remap[palette_color::PINE as usize] = palette_color::BROWN;
    remap
};

const BLUE_PAGE_REMAP: [Index; 16] = {
    let mut remap = IDENTITY_PAGE_REMAP;
    remap[palette_color::PEACH as usize] = palette_color::CYAN;
    remap[palette_color::LIME as usize] = palette_color::BLUE;
    remap[palette_color::ORANGE as usize] = palette_color::PINE;
    remap[palette_color::CRIMSON as usize] = palette_color::PINE;
    remap[palette_color::ROSE as usize] = palette_color::BLUE;
    remap[palette_color::PINE as usize] = palette_color::GUNMETAL;
    remap
};

/// Whether `(x, y)` (negative values clamped to 0) falls inside the rectangle at
/// `(left, top)` of the given size.
fn point_in_rect(x: i16, y: i16, left: usize, top: usize, width: usize, height: usize) -> bool {
    let x = x.max(0) as usize;
    let y = y.max(0) as usize;
    (left..left + width).contains(&x) && (top..top + height).contains(&y)
}

fn checkbox_at(x: usize, y: usize) -> Option<usize> {
    CHECK_Y.iter().position(|&check_y| {
        (CHECK_X..CHECK_X + CHECK_W).contains(&x) && (check_y..check_y + CHECK_H).contains(&y)
    })
}

fn line_at(y: usize) -> Option<usize> {
    LINE_Y.iter().position(|&line_y| {
        let low = (line_y as isize - 17 + LINE_CLICK_OFFSET_Y as isize).max(0) as usize;
        y >= low && y < line_y + 4 + LINE_CLICK_OFFSET_Y
    })
}

const fn max_text_width(line: usize) -> usize {
    if line == LINE_COUNT - 1 {
        LAST_LINE_MAX_TEXT_WIDTH
    } else {
        MAX_TEXT_WIDTH
    }
}
