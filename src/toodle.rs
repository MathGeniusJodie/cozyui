use std::error::Error;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::app_color;
use crate::palette_color;
use crate::peanut_money_font;
use crate::text::{
    BitmapFont, EditKey, KeyInput, LinePlacement, TextEditOutcome, TextField, TextLayout, edit_key,
};
use crate::{Framebuffer, Index, Paint, Palette, Rect, Sprite, Swap, TRANSPARENT};

/// toodle's two-line todo layout: a lone line sits on the baseline; once the
/// text wraps, the first line lifts and the second drops.
const TODO_LINE_PLACEMENT: LinePlacement = LinePlacement::Split {
    up: WRAPPED_FIRST_LINE_OFFSET_Y,
    down: WRAPPED_SECOND_LINE_OFFSET_Y,
    hit_threshold: WRAPPED_SECOND_LINE_OFFSET_Y - 3,
};

const LINE_COUNT: usize = 6;
const CHECK_VARIANTS: usize = 4;

const TOP_PAGE_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/toodle_top.png");
const SECOND_PAGE_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/toodle_2nd.png");
const THIRD_PAGE_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/toodle_page.png");
const CHECKBOXES_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/checkboxes.png");
const CHECKS_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/checks.png");
const ERASER_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/eraser.png");
const PRIORITY_URGENT_PATH: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/assets/priority_urgent.png");
const PRIORITY_FROG_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/priority_frog.png");
const PRIORITY_SNAIL_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/priority_snail.png");
const GOLDSTAR_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/goldstar.png");
const PENCIL_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/focus_pencil.png");
const PENCIL_SHADOW_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/toodle_pencil_shadow.png"
);
const DICE_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/dice.png");

const SECTION_COUNT: usize = 4;
const VISIBLE_PAGE_COUNT: usize = 4;
/// Cap on how many pages a single category can show; extra overflow pages are
/// simply not navigable.
const MAX_PAGES_PER_SECTION: usize = 4;
/// Config file naming the directory that holds every toodle markdown file. The
/// first non-blank, non-comment line is the root path (`~` expands to `$HOME`).
const TOODLE_CONF_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/toodle.conf");
/// Root used when `toodle.conf` is missing or blank.
const DEFAULT_TOODLE_ROOT: &str = "~/Desktop/RemoteVault/✅ Toodle/";
const TODO_FILE_NAMES: [&str; SECTION_COUNT] = [
    "toodle_urgent.md",
    "toodle_frog.md",
    "toodle_normal.md",
    "toodle_snail.md",
];
/// Completed todos are filed under here, one file per day they were finished.
const DONE_DIR_NAME: &str = "toodle_done";
const ARCHIVE_TRANSACTION_NAME: &str = "toodle_archive_transaction.json";
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
const CHECK_Y: [usize; LINE_COUNT] = [71, 93, 115, 137, 159, 181];
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
    todos: [TodoList; SECTION_COUNT],
    // Per-priority count of todos archived into today's done files, used for
    // the gold star.
    today_done_counts: [usize; SECTION_COUNT],
    page: usize,
    focused_line: Option<usize>,
    // The todo the dice last landed on, as (section, page, line). Drawn crimson
    // and fake-bolded until the dice is rolled again or no todo is eligible.
    highlighted: Option<(usize, usize, usize)>,
    field: TextField,
    eraser_hovered: bool,
    // Per-keystroke saves fsync and made typing lag; edits are saved after a
    // typing pause instead (and flushed on shutdown).
    dirty_sections: [bool; SECTION_COUNT],
    last_edit: Instant,
    // The static page-stack art (shadows + page images + checkboxes) is the
    // expensive part to draw and only changes when the stack geometry does.
    // We cache it and blit it each frame, painting the live front-page text and
    // cursor on top. `page_art_key` is the geometry signature it was built for.
    page_art: Option<Framebuffer>,
    page_art_key: Option<PageArtKey>,
}

/// Everything the cached page art depends on: which page is on top and how many
/// pages each section contributes (which fixes the stack height and per-page
/// section colors). Text, focus, checks, and hover are painted live on top.
type PageArtKey = (usize, [usize; SECTION_COUNT]);

const SAVE_DEBOUNCE: Duration = Duration::from_millis(500);

impl Toodle {
    pub(crate) fn load(palette: &Palette) -> Result<Self, Box<dyn Error>> {
        fs::create_dir_all(toodle_root())?;
        recover_archive_transaction(&archive_transaction_path())?;

        Ok(Self {
            pages: [
                Sprite::load_native(TOP_PAGE_PATH, palette)?,
                Sprite::load_native(SECOND_PAGE_PATH, palette)?,
                Sprite::load_native(THIRD_PAGE_PATH, palette)?,
                Sprite::load_native(THIRD_PAGE_PATH, palette)?,
            ],
            checkboxes: Sprite::load_native(CHECKBOXES_PATH, palette)?,
            checks: Sprite::load_native(CHECKS_PATH, palette)?,
            eraser: Sprite::load_native(ERASER_PATH, palette)?,
            priority_urgent: Sprite::load_native(PRIORITY_URGENT_PATH, palette)?,
            priority_frog: Sprite::load_native(PRIORITY_FROG_PATH, palette)?,
            priority_snail: Sprite::load_native(PRIORITY_SNAIL_PATH, palette)?,
            goldstar: Sprite::load_native(GOLDSTAR_PATH, palette)?,
            pencil: Sprite::load_native(PENCIL_PATH, palette)?,
            pencil_shadow: Sprite::load_native(PENCIL_SHADOW_PATH, palette)?,
            dice: Sprite::load_native(DICE_PATH, palette)?,
            font: BitmapFont::load_with_fallback(
                &peanut_money_font::PEANUT_MONEY_SPEC,
                &crate::fusion_pixel_10_font::FUSION_PIXEL_10_SPEC,
            )?,
            todos: [
                TodoList::load(&todo_file(0))?,
                TodoList::load(&todo_file(1))?,
                TodoList::load(&todo_file(2))?,
                TodoList::load(&todo_file(3))?,
            ],
            today_done_counts: today_done_counts()?,
            page: 0,
            focused_line: None,
            highlighted: None,
            // The fits predicate is governed entirely by the two-line wrap, so
            // there is no separate character cap.
            field: TextField::new(usize::MAX, 2),
            eraser_hovered: false,
            dirty_sections: [false; SECTION_COUNT],
            last_edit: Instant::now(),
            page_art: None,
            page_art_key: None,
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

    /// Geometry signature the cached page art depends on.
    fn page_art_key(&self) -> PageArtKey {
        let mut counts = [0usize; SECTION_COUNT];
        for (count, todos) in counts.iter_mut().zip(self.todos.iter()) {
            *count = todos.page_count();
        }
        (self.page, counts)
    }

    pub(crate) fn render(&mut self, fb: &mut Framebuffer, palette: &Palette) {
        let (w, h) = (self.width(), self.height());
        let key = self.page_art_key();
        let stale = self.page_art_key != Some(key)
            || self
                .page_art
                .as_ref()
                .is_none_or(|art| art.width != w || art.height != h);
        if stale {
            let mut art = match self.page_art.take() {
                Some(art) if art.width == w && art.height == h => art,
                _ => Framebuffer::new(w, h, self.fill_color(palette)),
            };
            self.build_page_art(&mut art, palette);
            self.page_art = Some(art);
            self.page_art_key = Some(key);
        }

        if let Some(art) = &self.page_art {
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

        self.draw_focused_pencil_shadow(fb, palette);

        let text_color = palette_color::BLACK;
        let completed_text_color = if self.eraser_hovered {
            palette_color::GUNMETAL
        } else {
            text_color
        };
        for (line, _) in LINE_Y.iter().enumerate().take(LINE_COUNT) {
            let todo = self.todos[section].item(page, line);
            if todo.renders_checkbox(self.focused_line == Some(line)) {
                self.draw_checkbox_box(fb, palette, line);
            }
            if todo.checked {
                self.draw_check(fb, palette, section, line, 0);
            }

            let layout = Self::line_layout(&self.font, line);
            let lines = layout.wrap(&todo.text);
            if self.focused_line == Some(line) {
                layout.draw_selection_lines(
                    fb,
                    &lines,
                    self.field.selection_range(),
                    palette_color::LAVENDER,
                );
            }
            if self.highlighted == Some((section, page, line)) {
                layout.draw_lines_bold(fb, &lines, palette_color::CRIMSON);
            } else {
                layout.draw_lines(
                    fb,
                    &lines,
                    if todo.checked {
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
        self.field.end_drag();
        if self.eraser_at(x, y) {
            self.archive_completed_todos()?;
            self.focused_line = None;
            return Ok(false);
        }

        if self.dice_at(x, y) {
            self.roll_highlight();
            self.focused_line = None;
            return Ok(false);
        }

        let abs_x = x.max(0) as usize;
        let abs_y = y.max(0) as usize;
        let Some(page_x) = abs_x.checked_sub(PAGE_OFFSET_X) else {
            self.focused_line = None;
            return Ok(false);
        };

        if page_x >= PAGE_CURL_X && abs_y >= PAGE_CURL_Y {
            self.page = (self.page + 1) % self.logical_page_count();
            self.focused_line = None;
            return Ok(false);
        }

        if let Some(line) = checkbox_at(page_x, abs_y)
            && self.todos[self.current_page_ref().section]
                .item(self.current_page_ref().page, line)
                .is_checkbox
        {
            let PageRef { section, page } = self.current_page_ref();
            let checked = {
                let item = self.todos[section].item_mut(page, line);
                item.checked = !item.checked;
                item.checked
            };
            if checked && self.highlighted == Some((section, page, line)) {
                self.highlighted = None;
            }
            self.save_current_section()?;
            return Ok(checked && self.twirl_on_check_page());
        }

        self.focused_line = line_at(abs_y);
        if let Some(line) = self.focused_line {
            self.load_focused_field();
            let cursor = self
                .field
                .index_at(&Self::line_layout(&self.font, line), abs_x, abs_y);
            self.field.begin_drag(cursor);
        }
        Ok(false)
    }

    pub(crate) fn drag_text(&mut self, x: i16, y: i16) -> bool {
        if !self.field.is_dragging() {
            return false;
        }

        let Some(line) = self.focused_line else {
            return false;
        };
        let abs_x = x.max(0) as usize;
        let abs_y = y.max(0) as usize;
        let cursor = self
            .field
            .index_at(&Self::line_layout(&self.font, line), abs_x, abs_y);
        self.field.drag_to(cursor)
    }

    pub(crate) const fn end_text_drag(&mut self) {
        self.field.end_drag();
    }

    pub(crate) const fn text_dragging(&self) -> bool {
        self.field.is_dragging()
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
        let Some(line) = self.focused_line else {
            return Ok(None);
        };

        let PageRef { section, page } = self.current_page_ref();
        if matches!(edit_key(input), EditKey::Backspace) && self.field.text().is_empty() {
            if self.todos[section].delete_item(page, line) {
                self.todos[section].save(&todo_file(section))?;
            }
            self.keep_section_page_visible(PageRef { section, page });
            self.focused_line = Some(line);
            self.load_focused_field();
            self.field.set_cursor(0);
            return Ok(None);
        }

        let layout = Self::line_layout(&self.font, line);
        let outcome = self.field.handle_key(input, clipboard_text, &layout);
        if let TextEditOutcome::Handled { changed, copy } = outcome {
            if changed {
                self.todos[section].item_mut(page, line).text = self.field.text().to_string();
                // Deferred save: an fsync per keystroke makes typing lag.
                self.dirty_sections[section] = true;
                self.last_edit = Instant::now();
                self.keep_section_page_visible(PageRef { section, page });
            }
            return Ok(copy);
        }

        match edit_key(input) {
            EditKey::Enter => {
                self.focused_line = Some((line + 1).min(LINE_COUNT - 1));
                self.load_focused_field();
                self.field.set_cursor_end();
            }
            EditKey::Escape => {
                self.focused_line = None;
            }
            EditKey::Tab
            | EditKey::Left
            | EditKey::Right
            | EditKey::Insert(_)
            | EditKey::Backspace
            | EditKey::None => return Ok(None),
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

    /// Load the focused line's text into the editing field (the field is the
    /// live buffer while a line is focused; edits are mirrored back into the
    /// todo item so the rest of the widget can keep rendering from `todos`).
    fn load_focused_field(&mut self) {
        if let Some(line) = self.focused_line {
            let PageRef { section, page } = self.current_page_ref();
            let text = self.todos[section].item(page, line).text.clone();
            self.field.set_text(&text);
        }
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

    fn draw_check(
        &self,
        fb: &mut Framebuffer,
        palette: &Palette,
        page: usize,
        line: usize,
        page_offset: usize,
    ) {
        let src_x = (page + line) % CHECK_VARIANTS * CHECK_SPRITE_W;
        let dest_x = PAGE_OFFSET_X + page_offset + CHECK_X - 1;
        let dest_y = page_offset + CHECK_Y[line] - 4;
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
        self.todos[current_page.section].save(&todo_file(current_page.section))?;
        self.dirty_sections[current_page.section] = false;
        self.keep_section_page_visible(current_page);
        Ok(())
    }

    /// Write debounced edits once typing has paused. Call regularly.
    pub(crate) fn maintain(&mut self) -> Result<(), Box<dyn Error>> {
        if self.dirty_sections.iter().any(|&dirty| dirty)
            && self.last_edit.elapsed() >= SAVE_DEBOUNCE
        {
            self.flush_saves()?;
        }
        Ok(())
    }

    /// Write all pending edits now (shutdown, structural changes).
    pub(crate) fn flush_saves(&mut self) -> Result<(), Box<dyn Error>> {
        for section in 0..SECTION_COUNT {
            if self.dirty_sections[section] {
                self.todos[section].save(&todo_file(section))?;
                self.dirty_sections[section] = false;
            }
        }
        Ok(())
    }

    fn archive_completed_todos(&mut self) -> Result<(), Box<dyn Error>> {
        let current_page = self.current_page_ref();
        let mut archived: [Vec<String>; SECTION_COUNT] = std::array::from_fn(|_| Vec::new());
        let mut changed_pages = [false; SECTION_COUNT];
        let mut staged_pages = self.todos.clone();

        for (page_index, page) in staged_pages.iter_mut().enumerate() {
            let mut remaining = Vec::new();
            for item in page.items.iter().cloned() {
                if item.checked {
                    if !item.text.trim().is_empty() {
                        archived[page_index].push(item.text.clone());
                    }
                    changed_pages[page_index] = true;
                } else {
                    remaining.push(item);
                }
            }

            if changed_pages[page_index] {
                page.items = remaining;
                page.trim_trailing_blank_items();
            }
        }

        let mut staged_writes = Vec::new();
        if archived.iter().any(|page| !page.is_empty()) {
            fs::create_dir_all(done_dir())?;
        }
        for (page_index, archived_page) in archived.iter().enumerate() {
            if archived_page.is_empty() {
                continue;
            }

            let done_path = daily_done_path(page_index);
            let mut done_text = if Path::new(&done_path).exists() {
                fs::read_to_string(&done_path)?
            } else {
                String::new()
            };
            if !done_text.is_empty() && !done_text.ends_with('\n') {
                done_text.push('\n');
            }
            for todo in archived_page {
                done_text.push_str("- [x] ");
                done_text.push_str(todo);
                done_text.push('\n');
            }
            staged_writes.push(AtomicWrite::stage(&done_path, done_text.into_bytes())?);
        }
        for (page_index, changed) in changed_pages.into_iter().enumerate() {
            if changed {
                staged_writes.push(AtomicWrite::stage(
                    &todo_file(page_index),
                    staged_pages[page_index].serialized_text().into_bytes(),
                )?);
            }
        }

        let marker_path = archive_transaction_path();
        write_archive_transaction_marker(&marker_path, &staged_writes)?;
        for staged_write in staged_writes {
            staged_write.commit()?;
        }
        fs::remove_file(&marker_path)?;

        self.todos = staged_pages;
        self.today_done_counts = today_done_counts()?;
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
        self.todos.iter().map(TodoList::page_count).sum()
    }

    fn current_page_ref(&self) -> PageRef {
        self.page_ref(self.page)
    }

    fn page_ref(&self, mut page: usize) -> PageRef {
        for (section, todos) in self.todos.iter().enumerate() {
            let section_pages = todos.page_count();
            if page < section_pages {
                return PageRef { section, page };
            }
            page -= section_pages;
        }

        PageRef {
            section: SECTION_COUNT - 1,
            page: self.todos[SECTION_COUNT - 1].page_count() - 1,
        }
    }

    fn page_index_for(&self, section: usize, section_page: usize) -> usize {
        self.todos
            .iter()
            .take(section)
            .map(TodoList::page_count)
            .sum::<usize>()
            + section_page.min(self.todos[section].page_count() - 1)
    }

    fn keep_section_page_visible(&mut self, page: PageRef) {
        let section_pages = self.todos[page.section].page_count();
        self.page = self.page_index_for(page.section, page.page.min(section_pages - 1));
    }

    /// Gold-star tally for the current priority: todos already archived into
    /// today's done file plus checked todos not yet swept there.
    fn goldstar_count(&self) -> usize {
        let section = self.current_page_ref().section;
        let checked_unarchived = self.todos[section]
            .items
            .iter()
            .filter(|item| item.checked && !item.text.trim().is_empty())
            .count();
        self.today_done_counts[section] + checked_unarchived
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
        let line = self.focused_line?;
        let PageRef { section, page } = self.current_page_ref();
        let todo = self.todos[section].item(page, line);
        let layout = Self::line_layout(&self.font, line);
        let (cursor_x, cursor_y) = layout.cursor_position(&todo.text, self.field.cursor());
        Some((line, cursor_x, cursor_y, layout.wrap(&todo.text).len() > 1))
    }

    fn should_draw_pencil_shadow(&self, line: usize) -> bool {
        let PageRef { section, page } = self.current_page_ref();
        let char_count = self.todos[section].item(page, line).text.chars().count();
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
        let y = page.height - self.dice.height;
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
                let item = self.todos[section].item(page, line);
                item.is_checkbox && !item.checked && !item.text.trim().is_empty()
            })
            .collect();
        self.highlighted = candidates
            .get(random_index(candidates.len()))
            .map(|&line| (section, page, line));
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

#[derive(Clone, Copy)]
struct PageRef {
    section: usize,
    page: usize,
}

/// Root directory for all toodle markdown files, configurable via `toodle.conf`.
/// Resolved once and cached; falls back to [`DEFAULT_TOODLE_ROOT`] when the
/// config is missing or contains no usable path.
fn toodle_root() -> &'static str {
    static ROOT: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    ROOT.get_or_init(|| {
        let configured = fs::read_to_string(TOODLE_CONF_PATH)
            .ok()
            .and_then(|text| {
                text.lines()
                    .map(str::trim)
                    .find(|line| !line.is_empty() && !line.starts_with('#'))
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| DEFAULT_TOODLE_ROOT.to_owned());
        let expanded = crate::paths::expand_tilde(&configured);
        expanded.trim_end_matches('/').to_owned()
    })
}

fn todo_file(section: usize) -> String {
    format!("{}/{}", toodle_root(), TODO_FILE_NAMES[section])
}

fn done_dir() -> String {
    format!("{}/{DONE_DIR_NAME}", toodle_root())
}

fn archive_transaction_path() -> String {
    format!("{}/{ARCHIVE_TRANSACTION_NAME}", toodle_root())
}

/// Path of the done-todo file for a given date and priority tag, the single
/// source of the `<root>/YYYY-MM-DD_<tag>.md` naming convention (shared with
/// the stats widget).
pub fn done_file_path(year: i32, month: i32, day: i32, tag: &str) -> String {
    format!("{}/{year:04}-{month:02}-{day:02}_{tag}.md", done_dir())
}

/// Path of the done-todo file for `section` today, named by date and priority.
fn daily_done_path(section: usize) -> String {
    let tm = crate::localtime::local_time().unwrap_or_default();
    done_file_path(
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        section_tag(section),
    )
}

/// Today's existing done file for `section`, or `None` if it does not exist.
fn existing_done_path(section: usize) -> Option<String> {
    let md = daily_done_path(section);
    Path::new(&md).exists().then_some(md)
}

/// Per-priority count of todos archived in today's done files.
fn today_done_counts() -> Result<[usize; SECTION_COUNT], Box<dyn Error>> {
    let mut counts = [0; SECTION_COUNT];
    for (section, count) in counts.iter_mut().enumerate() {
        *count = done_todo_count(section)?;
    }
    Ok(counts)
}

fn done_todo_count(section: usize) -> Result<usize, Box<dyn Error>> {
    let Some(path) = existing_done_path(section) else {
        return Ok(0);
    };

    Ok(fs::read_to_string(path)?
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count())
}

#[derive(Clone)]
struct TodoList {
    items: Vec<TodoItem>,
}

impl TodoList {
    fn load(path: &str) -> Result<Self, Box<dyn Error>> {
        let items = if Path::new(path).exists() {
            fs::read_to_string(path)?
                .lines()
                .map(TodoItem::parse)
                .collect()
        } else {
            Vec::new()
        };
        let mut list = Self { items };
        list.trim_trailing_blank_items();
        Ok(list)
    }

    fn save(&mut self, path: &str) -> Result<(), Box<dyn Error>> {
        self.trim_trailing_blank_items();
        atomic_write(path, self.serialized_text().as_bytes())?;
        Ok(())
    }

    fn page_count(&self) -> usize {
        let pages_with_items = self.items.len().div_ceil(LINE_COUNT).max(1);
        let last_page_start = (pages_with_items - 1) * LINE_COUNT;
        let last_page_full = !self.items.is_empty()
            && (last_page_start..last_page_start + LINE_COUNT)
                .all(|index| self.items.get(index).is_some_and(|item| !item.is_blank()));
        let count = if last_page_full {
            pages_with_items + 1
        } else {
            pages_with_items
        };
        count.min(MAX_PAGES_PER_SECTION)
    }

    fn item(&self, page: usize, line: usize) -> &TodoItem {
        static BLANK_ITEM: TodoItem = TodoItem {
            text: String::new(),
            checked: false,
            is_checkbox: true,
        };
        self.items
            .get(page * LINE_COUNT + line)
            .unwrap_or(&BLANK_ITEM)
    }

    fn item_mut(&mut self, page: usize, line: usize) -> &mut TodoItem {
        let index = page * LINE_COUNT + line;
        if self.items.len() <= index {
            self.items.resize_with(index + 1, TodoItem::default);
        }
        &mut self.items[index]
    }

    fn delete_item(&mut self, page: usize, line: usize) -> bool {
        let index = page * LINE_COUNT + line;
        if index >= self.items.len() {
            return false;
        }
        self.items.remove(index);
        true
    }

    fn trim_trailing_blank_items(&mut self) {
        while self.items.last().is_some_and(TodoItem::is_blank) {
            self.items.pop();
        }
    }

    fn serialized_text(&self) -> String {
        if self.items.is_empty() {
            return String::new();
        }

        let text = self
            .items
            .iter()
            .map(TodoItem::serialize)
            .collect::<Vec<_>>()
            .join("\n");
        format!("{text}\n")
    }
}

struct AtomicWrite {
    path: String,
    temp_path: String,
}

impl AtomicWrite {
    fn stage(path: &str, contents: impl AsRef<[u8]>) -> Result<Self, Box<dyn Error>> {
        let temp_path = format!("{path}.tmp.{}", std::process::id());
        {
            let mut file = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&temp_path)?;
            file.write_all(contents.as_ref())?;
            file.sync_all()?;
        }
        Ok(Self {
            path: path.to_string(),
            temp_path,
        })
    }

    fn commit(self) -> Result<(), Box<dyn Error>> {
        fs::rename(&self.temp_path, &self.path)?;
        Ok(())
    }
}

impl Drop for AtomicWrite {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.temp_path);
    }
}

fn atomic_write(path: &str, contents: &[u8]) -> Result<(), Box<dyn Error>> {
    AtomicWrite::stage(path, contents)?.commit()
}

fn write_archive_transaction_marker(
    marker_path: &str,
    staged_writes: &[AtomicWrite],
) -> Result<(), Box<dyn Error>> {
    let writes = staged_writes
        .iter()
        .map(|write| {
            serde_json::json!({
                "path": write.path.as_str(),
                "temp_path": write.temp_path.as_str(),
            })
        })
        .collect::<Vec<_>>();
    let marker = serde_json::to_vec(&writes)?;
    let transaction_marker = AtomicWrite::stage(marker_path, marker)?;
    transaction_marker.commit()?;
    Ok(())
}

fn recover_archive_transaction(marker_path: &str) -> Result<(), Box<dyn Error>> {
    if !Path::new(marker_path).exists() {
        return Ok(());
    }

    let marker = fs::read_to_string(marker_path)?;
    let writes = serde_json::from_str::<serde_json::Value>(&marker)?;
    let Some(records) = writes.as_array() else {
        return Err("archive transaction marker is not a JSON array".into());
    };

    for record in records.iter().map(ArchiveWriteRecord::from_json) {
        let record = record?;
        if Path::new(&record.temp_path).exists() {
            fs::rename(&record.temp_path, &record.path)?;
        }
    }

    fs::remove_file(marker_path)?;
    Ok(())
}

struct ArchiveWriteRecord {
    path: String,
    temp_path: String,
}

impl ArchiveWriteRecord {
    fn from_json(value: &serde_json::Value) -> Result<Self, Box<dyn Error>> {
        Ok(Self {
            path: value
                .get("path")
                .and_then(serde_json::Value::as_str)
                .ok_or("archive transaction marker is missing path")?
                .to_string(),
            temp_path: value
                .get("temp_path")
                .and_then(serde_json::Value::as_str)
                .ok_or("archive transaction marker is missing temp_path")?
                .to_string(),
        })
    }
}

#[derive(Clone)]
struct TodoItem {
    text: String,
    checked: bool,
    /// Whether this line is a markdown checkbox (`- [ ]` / `- [x]`). Plain lines
    /// from the file are kept verbatim and rendered without a checkbox to their
    /// left. New lines typed in the widget default to checkboxes.
    is_checkbox: bool,
}

/// New lines created in the widget are checkbox todos by default; plain lines
/// only arise from non-checkbox text in the backing file.
impl Default for TodoItem {
    fn default() -> Self {
        Self {
            text: String::new(),
            checked: false,
            is_checkbox: true,
        }
    }
}

impl TodoItem {
    fn parse(line: &str) -> Self {
        for (prefix, checked) in [("- [x]", true), ("- [X]", true), ("- [ ]", false)] {
            if let Some(rest) = line.strip_prefix(prefix) {
                return Self {
                    text: rest.strip_prefix(' ').unwrap_or(rest).to_string(),
                    checked,
                    is_checkbox: true,
                };
            }
        }
        Self {
            text: line.to_string(),
            checked: false,
            is_checkbox: false,
        }
    }

    fn serialize(&self) -> String {
        if !self.is_checkbox {
            self.text.clone()
        } else if self.checked {
            format!("- [x] {}", self.text)
        } else {
            format!("- [ ] {}", self.text)
        }
    }

    /// Whether a checkbox should be drawn to the left of this line. Plain lines
    /// never show one; a checkbox todo shows one once it has content, is
    /// checked, or is the line currently being edited.
    const fn renders_checkbox(&self, focused: bool) -> bool {
        self.is_checkbox && (focused || self.checked || !self.text.is_empty())
    }

    const fn is_blank(&self) -> bool {
        !self.checked && self.text.is_empty()
    }
}

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

/// Priority tag used in daily done filenames (`YYYY-MM-DD_<tag>.md`).
const fn section_tag(section: usize) -> &'static str {
    match section % SECTION_COUNT {
        0 => "urgent",
        1 => "frog",
        2 => "normal",
        _ => "snail",
    }
}

const fn section_color(section: usize) -> PageColor {
    match section % SECTION_COUNT {
        0 => PageColor::Pink,
        1 => PageColor::Green,
        2 => PageColor::Yellow,
        _ => PageColor::Blue,
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

/// A pseudo-random index in `0..len` (returns 0 when `len` is 0).
fn random_index(len: usize) -> usize {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .subsec_nanos() as usize;
    nanos % len.max(1)
}

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
        y >= line_y - 17 + LINE_CLICK_OFFSET_Y && y < line_y + 4 + LINE_CLICK_OFFSET_Y
    })
}

const fn max_text_width(line: usize) -> usize {
    if line == LINE_COUNT - 1 {
        LAST_LINE_MAX_TEXT_WIDTH
    } else {
        MAX_TEXT_WIDTH
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn todo_item_round_trips_checked_items() {
        let item = TodoItem::parse("- [x] ship the tiny desktop");

        assert!(item.checked);
        assert!(item.is_checkbox);
        assert_eq!(item.text, "ship the tiny desktop");
        assert_eq!(item.serialize(), "- [x] ship the tiny desktop");
    }

    #[test]
    fn todo_item_round_trips_unchecked_checkbox() {
        let item = TodoItem::parse("- [ ] water the plants");

        assert!(!item.checked);
        assert!(item.is_checkbox);
        assert_eq!(item.text, "water the plants");
        assert_eq!(item.serialize(), "- [ ] water the plants");
    }

    #[test]
    fn todo_item_keeps_plain_lines_verbatim() {
        let item = TodoItem::parse("## groceries");

        assert!(!item.is_checkbox);
        assert!(!item.checked);
        assert!(!item.renders_checkbox(false));
        assert!(!item.renders_checkbox(true));
        assert_eq!(item.serialize(), "## groceries");
    }

    #[test]
    fn todo_list_serializes_checkboxes_and_plain_lines() {
        let mut list = TodoList {
            items: vec![TodoItem::default(); LINE_COUNT],
        };
        list.items[0] = TodoItem::parse("- [x] done");
        list.items[1] = TodoItem::parse("a heading");
        list.trim_trailing_blank_items();

        assert_eq!(list.serialized_text().lines().count(), 2);
        assert_eq!(list.serialized_text(), "- [x] done\na heading\n");
    }

    #[test]
    fn todo_list_adds_blank_page_after_full_page() {
        let mut list = TodoList { items: Vec::new() };
        for line in 0..LINE_COUNT {
            list.item_mut(0, line).text = format!("todo {line}");
        }

        assert_eq!(list.page_count(), 2);

        list.items.pop();
        assert_eq!(list.page_count(), 1);
    }

    #[test]
    fn todo_list_does_not_add_page_after_sparse_page() {
        let mut list = TodoList {
            items: vec![TodoItem::default(); LINE_COUNT],
        };
        list.items[0].text = "first".to_string();
        list.items[LINE_COUNT - 1].text = "last".to_string();

        assert_eq!(list.page_count(), 1);
    }

    #[test]
    fn todo_list_uses_one_file_for_overflow_pages() {
        let mut list = TodoList { items: Vec::new() };
        list.item_mut(1, 0).text = "overflow".to_string();

        assert_eq!(list.serialized_text().lines().count(), LINE_COUNT + 1);
        assert!(list.serialized_text().ends_with("overflow\n"));
    }

    #[test]
    fn todo_list_delete_item_shifts_later_items_up() {
        let mut list = TodoList { items: Vec::new() };
        list.item_mut(0, 0).text = "first".to_string();
        list.item_mut(0, 1).text = String::new();
        list.item_mut(0, 2).text = "third".to_string();

        assert!(list.delete_item(0, 1));

        assert_eq!(list.item(0, 0).text, "first");
        assert_eq!(list.item(0, 1).text, "third");
    }

    #[test]
    fn archive_transaction_recovery_finishes_interrupted_commit() {
        let dir = unique_temp_dir();
        fs::create_dir_all(&dir).unwrap();

        let marker_path = dir.join("transaction.json");
        let done_path = dir.join("done.txt");
        let page_path = dir.join("page.txt");
        fs::write(&done_path, "old\n").unwrap();
        fs::write(&page_path, "[x] done\nnext\n\n\n\n\n").unwrap();

        let staged_writes = vec![
            AtomicWrite::stage(done_path.to_str().unwrap(), b"old\ndone\n").unwrap(),
            AtomicWrite::stage(page_path.to_str().unwrap(), b"next\n\n\n\n\n\n").unwrap(),
        ];
        write_archive_transaction_marker(marker_path.to_str().unwrap(), &staged_writes).unwrap();

        let marker = fs::read_to_string(&marker_path).unwrap();
        let writes = serde_json::from_str::<serde_json::Value>(&marker).unwrap();
        let writes = writes.as_array().unwrap();
        let done_temp_path = writes[0].get("temp_path").unwrap().as_str().unwrap();
        fs::rename(done_temp_path, &done_path).unwrap();

        recover_archive_transaction(marker_path.to_str().unwrap()).unwrap();

        assert_eq!(fs::read_to_string(&done_path).unwrap(), "old\ndone\n");
        assert_eq!(fs::read_to_string(&page_path).unwrap(), "next\n\n\n\n\n\n");
        assert!(!marker_path.exists());

        fs::remove_dir_all(dir).unwrap();
    }

    fn unique_temp_dir() -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("cozyui-toodle-test-{}-{nanos}", std::process::id()))
    }
}
