use std::error::Error;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

use crate::app_color;
use crate::bitmap_font::BitmapFont;
use crate::palette_color;
use crate::peanut_money_font;
use crate::text_edit::{TextEdit, TextEditOutcome};
use crate::text_input::{EditKey, KeyInput, edit_key};
use crate::{Framebuffer, Image, Palette, Rect, Rgba};

const SCALE: usize = 1;
const GLYPH_SCALE: usize = 1;
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

const SECTION_COUNT: usize = 4;
const VISIBLE_PAGE_COUNT: usize = 4;
const TODO_FILES: [&str; SECTION_COUNT] = [
    concat!(env!("CARGO_MANIFEST_DIR"), "/toodle_urgent.txt"),
    concat!(env!("CARGO_MANIFEST_DIR"), "/toodle_frog.txt"),
    concat!(env!("CARGO_MANIFEST_DIR"), "/toodle_normal.txt"),
    concat!(env!("CARGO_MANIFEST_DIR"), "/toodle_snail.txt"),
];
const DONE_TODO_FILES: [&str; SECTION_COUNT] = [
    concat!(env!("CARGO_MANIFEST_DIR"), "/toodle_urgent_done.txt"),
    concat!(env!("CARGO_MANIFEST_DIR"), "/toodle_frog_done.txt"),
    concat!(env!("CARGO_MANIFEST_DIR"), "/toodle_normal_done.txt"),
    concat!(env!("CARGO_MANIFEST_DIR"), "/toodle_snail_done.txt"),
];
const ARCHIVE_TRANSACTION_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/toodle_archive_transaction.json"
);
const PAGE_OFFSET_X: usize = 14;
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

pub(crate) struct Toodle {
    pages: [Image; VISIBLE_PAGE_COUNT],
    checkboxes: Image,
    checks: Image,
    eraser: Image,
    priority_urgent: Image,
    priority_frog: Image,
    priority_snail: Image,
    goldstar: Image,
    pencil: Image,
    pencil_shadow: Image,
    font: BitmapFont,
    todos: [TodoList; SECTION_COUNT],
    done_counts: [usize; SECTION_COUNT],
    page: usize,
    focused_line: Option<usize>,
    text_edit: TextEdit,
    eraser_hovered: bool,
}

impl Toodle {
    pub(crate) fn load(palette: &Palette) -> Result<Self, Box<dyn Error>> {
        recover_archive_transaction(ARCHIVE_TRANSACTION_PATH)?;

        Ok(Self {
            pages: [
                Image::load(TOP_PAGE_PATH, palette)?,
                Image::load(SECOND_PAGE_PATH, palette)?,
                Image::load(THIRD_PAGE_PATH, palette)?,
                Image::load(THIRD_PAGE_PATH, palette)?,
            ],
            checkboxes: Image::load(CHECKBOXES_PATH, palette)?,
            checks: Image::load(CHECKS_PATH, palette)?,
            eraser: Image::load(ERASER_PATH, palette)?,
            priority_urgent: Image::load(PRIORITY_URGENT_PATH, palette)?,
            priority_frog: Image::load(PRIORITY_FROG_PATH, palette)?,
            priority_snail: Image::load(PRIORITY_SNAIL_PATH, palette)?,
            goldstar: Image::load(GOLDSTAR_PATH, palette)?,
            pencil: Image::load(PENCIL_PATH, palette)?,
            pencil_shadow: Image::load(PENCIL_SHADOW_PATH, palette)?,
            font: BitmapFont::load(&peanut_money_font::PEANUT_MONEY_SPEC)?,
            todos: [
                TodoList::load(TODO_FILES[0])?,
                TodoList::load(TODO_FILES[1])?,
                TodoList::load(TODO_FILES[2])?,
                TodoList::load(TODO_FILES[3])?,
            ],
            done_counts: [
                done_todo_count(DONE_TODO_FILES[0])?,
                done_todo_count(DONE_TODO_FILES[1])?,
                done_todo_count(DONE_TODO_FILES[2])?,
                done_todo_count(DONE_TODO_FILES[3])?,
            ],
            page: 0,
            focused_line: None,
            text_edit: TextEdit::default(),
            eraser_hovered: false,
        })
    }

    pub(crate) fn width(&self) -> usize {
        (PAGE_OFFSET_X + self.pages[0].width + self.stack_offset()) * SCALE
            + SHADOW_X_OFFSET as usize
    }

    pub(crate) fn height(&self) -> usize {
        (self.pages[0].height + self.stack_offset()) * SCALE + SHADOW_Y_OFFSET as usize
    }

    pub(crate) fn fill_color(&self, palette: &Palette) -> Rgba {
        palette.color(palette_color::BLACK).transparent()
    }

    pub(crate) fn render(&self, fb: &mut Framebuffer, palette: &Palette) {
        fb.clear(self.fill_color(palette));

        let page_count = self.logical_page_count();
        for visual_page in (0..page_count).rev() {
            self.draw_page_shadow(fb, palette, visual_page);
        }
        for visual_page in (0..page_count).rev() {
            let logical_page = (self.page + visual_page) % page_count;
            self.render_page(fb, palette, logical_page, visual_page);
        }

        fb.draw_image(
            &self.eraser,
            (ERASER_X * SCALE) as isize,
            (ERASER_Y * SCALE) as isize,
            SCALE,
        );
        self.draw_priority_icon(fb);
        self.draw_goldstar(fb, palette);
        self.draw_focused_pencil(fb);
    }

    fn draw_page_shadow(&self, fb: &mut Framebuffer, palette: &Palette, visual_page: usize) {
        let page_image = &self.pages[visual_page.min(self.pages.len() - 1)];
        let page_offset = visual_page * PAGE_STACK_OFFSET;
        let page_x = PAGE_OFFSET_X + page_offset;
        let page_y = page_offset;
        fb.draw_image_shadow(
            page_image,
            (page_x * SCALE) as isize + SHADOW_X_OFFSET,
            (page_y * SCALE) as isize + SHADOW_Y_OFFSET,
            SCALE,
            palette.color(app_color::BACKGROUND_SHADOW),
        );
    }

    fn render_page(
        &self,
        fb: &mut Framebuffer,
        palette: &Palette,
        logical_page: usize,
        visual_page: usize,
    ) {
        let page_image = &self.pages[visual_page.min(self.pages.len() - 1)];
        let PageRef { section, page } = self.page_ref(logical_page);
        let page_offset = visual_page * PAGE_STACK_OFFSET;
        let page_x = PAGE_OFFSET_X + page_offset;
        let page_y = page_offset;
        draw_page_image(
            fb,
            page_image,
            section_color(section),
            palette,
            page_x,
            page_y,
        );
        fb.draw_image(
            &self.checkboxes,
            (page_x * SCALE) as isize,
            (page_y * SCALE) as isize,
            SCALE,
        );
        if visual_page == 0 {
            self.draw_focused_pencil_shadow(fb, palette);
        }

        let text_color = palette.color(palette_color::BLACK);
        let completed_text_color = if self.eraser_hovered {
            palette.color(palette_color::GUNMETAL)
        } else {
            text_color
        };
        for (line, _) in LINE_Y.iter().enumerate().take(LINE_COUNT) {
            let todo = self.todos[section].item(page, line);
            if todo.checked {
                self.draw_check(fb, palette, section, line, page_offset);
            }

            if visual_page == 0 && self.focused_line == Some(line) {
                self.draw_todo_selection(
                    fb,
                    &todo.text,
                    line,
                    page_x + TEXT_X,
                    page_y + LINE_Y[line] - TEXT_Y_OFFSET,
                    palette.color(palette_color::LAVENDER),
                );
            }
            draw_todo_text(
                fb,
                &self.font,
                &todo.text,
                (page_x + TEXT_X) * SCALE,
                (page_y + LINE_Y[line] - TEXT_Y_OFFSET) * SCALE,
                GLYPH_SCALE * SCALE,
                if todo.checked {
                    completed_text_color
                } else {
                    text_color
                },
                text_chars_per_row(line),
            );
        }
    }

    pub(crate) fn click(&mut self, x: i16, y: i16) -> Result<bool, Box<dyn Error>> {
        self.text_edit.end_drag();
        if self.eraser_at(x, y) {
            self.archive_completed_todos()?;
            self.focused_line = None;
            return Ok(false);
        }

        let x = x.max(0) as usize / SCALE;
        let y = y.max(0) as usize / SCALE;
        let Some(x) = x.checked_sub(PAGE_OFFSET_X) else {
            self.focused_line = None;
            return Ok(false);
        };

        if x >= PAGE_CURL_X && y >= PAGE_CURL_Y {
            self.page = (self.page + 1) % self.logical_page_count();
            self.focused_line = None;
            return Ok(false);
        }

        if let Some(line) = checkbox_at(x, y) {
            let PageRef { section, page } = self.current_page_ref();
            let checked = {
                let item = self.todos[section].item_mut(page, line);
                item.checked = !item.checked;
                item.checked
            };
            self.save_current_section()?;
            return Ok(checked && self.twirl_on_check_page());
        }

        self.focused_line = line_at(y);
        if let Some(line) = self.focused_line {
            let PageRef { section, page } = self.current_page_ref();
            let text = &self.todos[section].item(page, line).text;
            let cursor = todo_text_index_at(&self.font, line, text, x.saturating_sub(TEXT_X), y);
            self.text_edit.begin_drag(cursor, text);
        }
        Ok(false)
    }

    pub(crate) fn drag_text(&mut self, x: i16, y: i16) -> bool {
        if !self.text_edit.is_dragging() {
            return false;
        }

        let Some(line) = self.focused_line else {
            return false;
        };
        let x = x.max(0) as usize / SCALE;
        let y = y.max(0) as usize / SCALE;
        let Some(x) = x.checked_sub(PAGE_OFFSET_X) else {
            return false;
        };

        let PageRef { section, page } = self.current_page_ref();
        let text = &self.todos[section].item(page, line).text;
        let cursor = todo_text_index_at(&self.font, line, text, x.saturating_sub(TEXT_X), y);
        self.text_edit.drag_to(cursor, text)
    }

    pub(crate) fn end_text_drag(&mut self) {
        self.text_edit.end_drag();
    }

    pub(crate) fn text_dragging(&self) -> bool {
        self.text_edit.is_dragging()
    }

    pub(crate) fn hover(&mut self, x: i16, y: i16) -> bool {
        let was_hovered = self.eraser_hovered;
        if x < 0 || y < 0 {
            self.eraser_hovered = false;
            return was_hovered != self.eraser_hovered;
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
        let mut text = self.todos[section].item(page, line).text.clone();
        let outcome = self
            .text_edit
            .handle_key(input, &mut text, clipboard_text, |candidate| {
                todo_text_fits(&self.font, line, candidate)
            });
        if let TextEditOutcome::Handled { changed, copy } = outcome {
            if changed {
                self.todos[section].item_mut(page, line).text = text;
                self.save_current_section()?;
            }
            return Ok(copy);
        }

        match edit_key(input) {
            EditKey::Enter => {
                self.focused_line = Some((line + 1).min(LINE_COUNT - 1));
                let PageRef { section, page } = self.current_page_ref();
                let text = &self.todos[section]
                    .item(page, self.focused_line.unwrap_or(line))
                    .text;
                self.text_edit.set_cursor(text.chars().count(), text);
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

    fn draw_check(
        &self,
        fb: &mut Framebuffer,
        palette: &Palette,
        page: usize,
        line: usize,
        page_offset: usize,
    ) {
        let src_x = (page + line) % CHECK_VARIANTS * CHECK_SPRITE_W;
        let dest_x = (PAGE_OFFSET_X + page_offset + CHECK_X - 1) * SCALE;
        let dest_y = (page_offset + CHECK_Y[line] - 4) * SCALE;

        if self.eraser_hovered {
            let tint = palette.color(palette_color::GUNMETAL);
            fb.draw_image_region_mapped(
                &self.checks,
                Rect::new(src_x, 0, CHECK_SPRITE_W, CHECK_SPRITE_H),
                dest_x as isize,
                dest_y as isize,
                SCALE,
                None,
                |color| (color.a != 0).then_some(tint),
            );
        } else {
            fb.draw_scaled_region(
                &self.checks,
                src_x,
                0,
                dest_x,
                dest_y,
                CHECK_SPRITE_W,
                CHECK_SPRITE_H,
                SCALE,
            );
        }
    }

    fn save_current_section(&mut self) -> Result<(), Box<dyn Error>> {
        let current_page = self.current_page_ref();
        self.todos[current_page.section].save(TODO_FILES[current_page.section])?;
        self.keep_section_page_visible(current_page);
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
        for (page_index, archived_page) in archived.iter().enumerate() {
            if archived_page.is_empty() {
                continue;
            }

            let done_path = DONE_TODO_FILES[page_index];
            let mut done_text = if Path::new(done_path).exists() {
                fs::read_to_string(done_path)?
            } else {
                String::new()
            };
            if !done_text.is_empty() && !done_text.ends_with('\n') {
                done_text.push('\n');
            }
            for todo in archived_page {
                done_text.push_str(todo);
                done_text.push('\n');
            }
            staged_writes.push(AtomicWrite::stage(done_path, done_text.into_bytes())?);
        }
        for (page_index, changed) in changed_pages.into_iter().enumerate() {
            if changed {
                staged_writes.push(AtomicWrite::stage(
                    TODO_FILES[page_index],
                    staged_pages[page_index].serialized_text().into_bytes(),
                )?);
            }
        }

        write_archive_transaction_marker(ARCHIVE_TRANSACTION_PATH, &staged_writes)?;
        for staged_write in staged_writes {
            staged_write.commit()?;
        }
        fs::remove_file(ARCHIVE_TRANSACTION_PATH)?;

        self.todos = staged_pages;
        for (page_index, archived_page) in archived.iter().enumerate() {
            self.done_counts[page_index] += archived_page.len();
        }
        self.keep_section_page_visible(current_page);

        Ok(())
    }

    fn draw_priority_icon(&self, fb: &mut Framebuffer) {
        let Some(icon) = self.priority_icon() else {
            return;
        };
        let icon_x = ERASER_X + self.eraser.width + PRIORITY_ICON_GAP + PRIORITY_ICON_OFFSET_X;
        let icon_y = ERASER_Y + PRIORITY_ICON_OFFSET_Y;
        fb.draw_image(
            icon,
            (icon_x * SCALE) as isize,
            (icon_y * SCALE) as isize,
            SCALE,
        );
    }

    fn priority_icon(&self) -> Option<&Image> {
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

    fn draw_goldstar(&self, fb: &mut Framebuffer, palette: &Palette) {
        let star_x = PAGE_OFFSET_X + self.pages[0].width - self.goldstar.width;
        fb.draw_image(
            &self.goldstar,
            (star_x * SCALE) as isize,
            (GOLDSTAR_Y * SCALE) as isize,
            SCALE,
        );

        let count = self.done_counts[self.current_page_ref().section].to_string();
        let text_scale = GLYPH_SCALE * SCALE;
        let text_w = self.font.text_width(&count) * text_scale;
        let text_h = self.font.cell_h() * text_scale;
        let star_w = self.goldstar.width * SCALE;
        let star_h = self.goldstar.height * SCALE;
        let text_x = star_x * SCALE + star_w.saturating_sub(text_w) / 2;
        let text_y = GOLDSTAR_Y * SCALE + star_h.saturating_sub(text_h) / 2;

        self.font.draw_text_limited(
            fb,
            &count,
            text_x,
            text_y,
            text_scale,
            palette.color(palette_color::BLACK),
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
        let dest_x = (PAGE_OFFSET_X + x).saturating_sub(PENCIL_TIP_X) * SCALE;
        let dest_y = y.saturating_sub(PENCIL_TIP_Y) * SCALE;

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

    fn draw_pencil_cursor(&self, fb: &mut Framebuffer, x: usize, y: usize) {
        let dest_x = (PAGE_OFFSET_X + x).saturating_sub(PENCIL_TIP_X) * SCALE;
        let dest_y = y.saturating_sub(PENCIL_TIP_Y) * SCALE;

        fb.draw_image(&self.pencil, dest_x as isize, dest_y as isize, SCALE);
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

    fn draw_focused_pencil(&self, fb: &mut Framebuffer) {
        let Some((_, cursor_x, cursor_y, _)) = self.focused_pencil_position() else {
            return;
        };

        self.draw_pencil_cursor(fb, cursor_x, cursor_y);
    }

    fn focused_pencil_position(&self) -> Option<(usize, usize, usize, bool)> {
        let line = self.focused_line?;
        let PageRef { section, page } = self.current_page_ref();
        let todo = self.todos[section].item(page, line);
        let (cursor_x, cursor_y) =
            pencil_cursor_position(&self.font, line, &todo.text, self.text_edit.cursor());
        Some((
            line,
            cursor_x,
            cursor_y,
            self.font.wrap_lines(&todo.text, max_text_width(line)).len() > 1,
        ))
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

    fn eraser_at(&self, x: i16, y: i16) -> bool {
        let x = x.max(0) as usize / SCALE;
        let y = y.max(0) as usize / SCALE;
        (ERASER_X..ERASER_X + self.eraser.width).contains(&x)
            && (ERASER_Y..ERASER_Y + self.eraser.height).contains(&y)
    }

    fn draw_todo_selection(
        &self,
        fb: &mut Framebuffer,
        text: &str,
        line: usize,
        x: usize,
        y: usize,
        color: Rgba,
    ) {
        draw_wrapped_selection(
            fb,
            &self.font,
            text,
            self.text_edit.selection_range(),
            x * SCALE,
            y * SCALE,
            max_text_width(line),
            SCALE,
            color,
        );
    }
}

#[derive(Clone, Copy)]
struct PageRef {
    section: usize,
    page: usize,
}

fn done_todo_count(path: &str) -> Result<usize, Box<dyn Error>> {
    if !Path::new(path).exists() {
        return Ok(0);
    }

    Ok(fs::read_to_string(path)?.lines().count())
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
        let last_page_full = self.items.len() > 0
            && (last_page_start..last_page_start + LINE_COUNT)
                .all(|index| self.items.get(index).is_some_and(|item| !item.is_blank()));
        if last_page_full {
            pages_with_items + 1
        } else {
            pages_with_items
        }
    }

    fn item(&self, page: usize, line: usize) -> &TodoItem {
        static BLANK_ITEM: TodoItem = TodoItem {
            text: String::new(),
            checked: false,
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

#[derive(Clone, Default)]
struct TodoItem {
    text: String,
    checked: bool,
}

impl TodoItem {
    fn parse(line: &str) -> Self {
        if let Some(text) = line.strip_prefix("[x] ") {
            Self {
                text: text.to_string(),
                checked: true,
            }
        } else if let Some(text) = line.strip_prefix("[ ] ") {
            Self {
                text: text.to_string(),
                checked: false,
            }
        } else {
            Self {
                text: line.to_string(),
                checked: false,
            }
        }
    }

    fn serialize(&self) -> String {
        if self.checked {
            format!("[x] {}", self.text)
        } else {
            self.text.clone()
        }
    }

    fn is_blank(&self) -> bool {
        !self.checked && self.text.is_empty()
    }
}

fn draw_page_image(
    fb: &mut Framebuffer,
    image: &Image,
    page_color: PageColor,
    palette: &Palette,
    x: usize,
    y: usize,
) {
    fb.draw_image_region_mapped(
        image,
        Rect::new(0, 0, image.width, image.height),
        (x * SCALE) as isize,
        (y * SCALE) as isize,
        SCALE,
        None,
        |color| Some(swap_page_color(color, page_color, palette)),
    );
}

fn draw_pencil_shadow(
    fb: &mut Framebuffer,
    image: &Image,
    dest_x: usize,
    dest_y: usize,
    page_color: PageColor,
    palette: &Palette,
    pencil_on_second_line: bool,
) {
    fb.draw_image_region_mapped(
        image,
        Rect::new(0, 0, image.width, image.height),
        dest_x as isize,
        dest_y as isize,
        SCALE,
        None,
        |color| {
            Some(swap_pencil_shadow_color(
                color,
                page_color,
                palette,
                pencil_on_second_line,
            ))
        },
    );
}

fn section_color(section: usize) -> PageColor {
    match section % SECTION_COUNT {
        0 => PageColor::Pink,
        1 => PageColor::Green,
        2 => PageColor::Yellow,
        _ => PageColor::Blue,
    }
}

fn swap_page_color(color: Rgba, page_color: PageColor, palette: &Palette) -> Rgba {
    let Some(source_color) = source_palette_color(color) else {
        return color;
    };

    palette.color(mapped_page_color(page_color, source_color))
}

fn swap_pencil_shadow_color(
    color: Rgba,
    page_color: PageColor,
    palette: &Palette,
    pencil_on_second_line: bool,
) -> Rgba {
    let Some(mut source_color) = source_palette_color(color) else {
        return color;
    };

    if pencil_on_second_line && source_color == palette_color::LIME {
        source_color = palette_color::ROSE;
    }

    palette.color(mapped_page_color(page_color, source_color))
}

fn source_palette_color(color: Rgba) -> Option<usize> {
    match (color.r, color.g, color.b, color.a) {
        (_, _, _, 0) => None,
        (140, 143, 174, _) => Some(palette_color::LAVENDER),
        (88, 69, 99, _) => Some(palette_color::GUNMETAL),
        (62, 33, 55, _) => Some(palette_color::PLUM),
        (154, 99, 72, _) => Some(palette_color::BROWN),
        (215, 155, 125, _) => Some(palette_color::PEACH),
        (245, 237, 186, _) => Some(palette_color::CREAM),
        (192, 199, 65, _) => Some(palette_color::LIME),
        (100, 125, 52, _) => Some(palette_color::GREEN),
        (228, 148, 58, _) => Some(palette_color::ORANGE),
        (157, 48, 59, _) => Some(palette_color::CRIMSON),
        (210, 100, 113, _) => Some(palette_color::ROSE),
        (112, 55, 127, _) => Some(palette_color::PURPLE),
        (126, 196, 193, _) => Some(palette_color::CYAN),
        (52, 133, 157, _) => Some(palette_color::BLUE),
        (23, 67, 75, _) => Some(palette_color::PINE),
        (31, 14, 28, _) => Some(palette_color::BLACK),
        _ => None,
    }
}

fn mapped_page_color(page_color: PageColor, source_color: usize) -> usize {
    let remap = match page_color {
        PageColor::Pink => &PINK_PAGE_REMAP,
        PageColor::Yellow => &YELLOW_PAGE_REMAP,
        PageColor::Green => &GREEN_PAGE_REMAP,
        PageColor::Blue => &BLUE_PAGE_REMAP,
    };
    remap.get(source_color).copied().unwrap_or(source_color)
}

const IDENTITY_PAGE_REMAP: [usize; 16] = [
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

const PINK_PAGE_REMAP: [usize; 16] = {
    let mut remap = IDENTITY_PAGE_REMAP;
    remap[palette_color::LIME] = palette_color::CRIMSON;
    remap[palette_color::PINE] = palette_color::ROSE;
    remap
};

const YELLOW_PAGE_REMAP: [usize; 16] = {
    let mut remap = IDENTITY_PAGE_REMAP;
    remap[palette_color::LAVENDER] = palette_color::CYAN;
    remap[palette_color::PEACH] = palette_color::CREAM;
    remap[palette_color::LIME] = palette_color::CRIMSON;
    remap[palette_color::CRIMSON] = palette_color::BROWN;
    remap[palette_color::ROSE] = palette_color::ORANGE;
    remap[palette_color::PINE] = palette_color::ROSE;
    remap
};

const GREEN_PAGE_REMAP: [usize; 16] = {
    let mut remap = IDENTITY_PAGE_REMAP;
    remap[palette_color::PEACH] = palette_color::LIME;
    remap[palette_color::LIME] = palette_color::GUNMETAL;
    remap[palette_color::ORANGE] = palette_color::GREEN;
    remap[palette_color::CRIMSON] = palette_color::PINE;
    remap[palette_color::ROSE] = palette_color::GREEN;
    remap[palette_color::PINE] = palette_color::BROWN;
    remap
};

const BLUE_PAGE_REMAP: [usize; 16] = {
    let mut remap = IDENTITY_PAGE_REMAP;
    remap[palette_color::PEACH] = palette_color::CYAN;
    remap[palette_color::LIME] = palette_color::BLUE;
    remap[palette_color::ORANGE] = palette_color::PINE;
    remap[palette_color::CRIMSON] = palette_color::PINE;
    remap[palette_color::ROSE] = palette_color::BLUE;
    remap[palette_color::PINE] = palette_color::GUNMETAL;
    remap
};

#[allow(clippy::too_many_arguments)]
fn draw_todo_text(
    fb: &mut Framebuffer,
    font: &BitmapFont,
    text: &str,
    x: usize,
    y: usize,
    scale: usize,
    color: Rgba,
    chars_per_row: usize,
) {
    let max_width = chars_per_row * 6;
    let lines = font.wrap_lines(text, max_width);
    if lines.len() <= 1 {
        font.draw_text(fb, &lines[0], x, y, scale, color);
        return;
    }

    font.draw_text(
        fb,
        &lines[0],
        x,
        y - WRAPPED_FIRST_LINE_OFFSET_Y * SCALE,
        scale,
        color,
    );
    font.draw_text(
        fb,
        &lines[1],
        x,
        y + WRAPPED_SECOND_LINE_OFFSET_Y * SCALE,
        scale,
        color,
    );
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

fn text_chars_per_row(line: usize) -> usize {
    if line == LINE_COUNT - 1 {
        LAST_LINE_MAX_TEXT_CHARS
    } else {
        MAX_TEXT_CHARS
    }
}

fn max_text_width(line: usize) -> usize {
    if line == LINE_COUNT - 1 {
        LAST_LINE_MAX_TEXT_WIDTH
    } else {
        MAX_TEXT_WIDTH
    }
}

fn pencil_cursor_position(
    font: &BitmapFont,
    line: usize,
    text: &str,
    cursor: usize,
) -> (usize, usize) {
    let base_y = LINE_Y[line] - TEXT_Y_OFFSET;
    let lines = font.wrap_lines(text, max_text_width(line));
    let (line_index, line_start, line_text) = line_for_char_index(&lines, cursor);
    let x = TEXT_X + font.text_width(prefix_chars(line_text, cursor - line_start));

    if line_index == 0 {
        let y = if lines.len() <= 1 {
            base_y
        } else {
            base_y - WRAPPED_FIRST_LINE_OFFSET_Y
        };
        (x, y)
    } else {
        (
            x.min(TEXT_X + max_text_width(line)),
            base_y + WRAPPED_SECOND_LINE_OFFSET_Y,
        )
    }
}

fn todo_text_fits(font: &BitmapFont, line: usize, text: &str) -> bool {
    font.wrap_lines(text, max_text_width(line)).len() <= 2
}

fn todo_text_index_at(font: &BitmapFont, line: usize, text: &str, x: usize, y: usize) -> usize {
    let lines = font.wrap_lines(text, max_text_width(line));
    let base_y = LINE_Y[line] - TEXT_Y_OFFSET;
    let line_index = if y >= base_y + WRAPPED_SECOND_LINE_OFFSET_Y.saturating_sub(3) {
        1
    } else {
        0
    };
    let max_width = max_text_width(line);
    text_index_at(font, &lines, line_index, x.min(max_width))
}

fn draw_wrapped_selection(
    fb: &mut Framebuffer,
    font: &BitmapFont,
    text: &str,
    selection: Option<(usize, usize)>,
    x: usize,
    y: usize,
    max_width: usize,
    scale: usize,
    color: Rgba,
) {
    let Some((selection_start, selection_end)) = selection else {
        return;
    };
    let lines = font.wrap_lines(text, max_width);
    let mut line_start = 0;
    for (line_index, line) in lines.iter().enumerate().take(2) {
        let line_len = line.chars().count();
        let line_end = line_start + line_len;
        let start = selection_start.max(line_start);
        let end = selection_end.min(line_end);
        if start < end {
            let prefix = prefix_chars(line, start - line_start);
            let selected_prefix = prefix_chars(line, end - line_start);
            let sel_x = x + font.text_width(prefix) * scale;
            let sel_w = font
                .text_width(selected_prefix)
                .saturating_sub(font.text_width(prefix))
                * scale;
            let line_y = if line_index == 0 && lines.len() > 1 {
                y - WRAPPED_FIRST_LINE_OFFSET_Y * scale
            } else if line_index == 1 {
                y + WRAPPED_SECOND_LINE_OFFSET_Y * scale
            } else {
                y
            };
            fb.fill_rect(sel_x, line_y, sel_w.max(1), font.cell_h() * scale, color);
        }
        line_start = line_end;
    }
}

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
    lines
        .last()
        .map(|line| (lines.len().saturating_sub(1), line_start, line.as_str()))
        .unwrap_or((0, 0, ""))
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
    let byte = text
        .char_indices()
        .nth(len)
        .map(|(index, _)| index)
        .unwrap_or(text.len());
    &text[..byte]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn todo_item_round_trips_checked_items() {
        let item = TodoItem::parse("[x] ship the tiny desktop");

        assert!(item.checked);
        assert_eq!(item.text, "ship the tiny desktop");
        assert_eq!(item.serialize(), "[x] ship the tiny desktop");
    }

    #[test]
    fn todo_list_serializes_only_needed_lines() {
        let mut list = TodoList {
            items: vec![TodoItem::default(); LINE_COUNT],
        };
        list.items[0] = TodoItem::parse("[x] done");
        list.items[1] = TodoItem::parse("next");
        list.trim_trailing_blank_items();

        assert_eq!(list.serialized_text().lines().count(), 2);
        assert_eq!(list.serialized_text(), "[x] done\nnext\n");
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
