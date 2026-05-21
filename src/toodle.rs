use std::error::Error;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

use crate::bitmap_font::BitmapFont;
use crate::palette_color;
use crate::peanut_money_font;
use crate::text_input::{EditKey, edit_key};
use crate::{Framebuffer, Image, Palette, Rgba};

const SCALE: usize = 2;
const GLYPH_SCALE: usize = 1;
const LINE_COUNT: usize = 6;
const CHECK_VARIANTS: usize = 4;

const TOP_PAGE_PATH: &str = "assets/toodle_top.png";
const SECOND_PAGE_PATH: &str = "assets/toodle_2nd.png";
const THIRD_PAGE_PATH: &str = "assets/toodle_page.png";
const CHECKBOXES_PATH: &str = "assets/checkboxes.png";
const CHECKS_PATH: &str = "assets/checks.png";
const ERASER_PATH: &str = "assets/eraser.png";
const GOLDSTAR_PATH: &str = "assets/goldstar.png";
const PENCIL_PATH: &str = "assets/toodle_pencil.png";
const PENCIL_SHADOW_PATH: &str = "assets/toodle_pencil_shadow.png";

const TODO_FILES: [&str; 3] = ["toodle_top.txt", "toodle_second.txt", "toodle_third.txt"];
const DONE_TODOS_PATH: &str = "toodle_done.txt";
const PAGE_OFFSET_X: usize = 14;
const ERASER_X: usize = 0;
const ERASER_Y: usize = 21;
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
}

pub(crate) struct Toodle {
    pages: [Image; 3],
    checkboxes: Image,
    checks: Image,
    eraser: Image,
    goldstar: Image,
    pencil: Image,
    pencil_shadow: Image,
    font: BitmapFont,
    todos: [TodoPage; 3],
    done_count: usize,
    page: usize,
    focused_line: Option<usize>,
    eraser_hovered: bool,
}

impl Toodle {
    pub(crate) fn load(palette: &Palette) -> Result<Self, Box<dyn Error>> {
        Ok(Self {
            pages: [
                Image::load(TOP_PAGE_PATH, palette)?,
                Image::load(SECOND_PAGE_PATH, palette)?,
                Image::load(THIRD_PAGE_PATH, palette)?,
            ],
            checkboxes: Image::load(CHECKBOXES_PATH, palette)?,
            checks: Image::load(CHECKS_PATH, palette)?,
            eraser: Image::load(ERASER_PATH, palette)?,
            goldstar: Image::load(GOLDSTAR_PATH, palette)?,
            pencil: Image::load(PENCIL_PATH, palette)?,
            pencil_shadow: Image::load(PENCIL_SHADOW_PATH, palette)?,
            font: BitmapFont::load(&peanut_money_font::PEANUT_MONEY_SPEC)?,
            todos: [
                TodoPage::load(TODO_FILES[0])?,
                TodoPage::load(TODO_FILES[1])?,
                TodoPage::load(TODO_FILES[2])?,
            ],
            done_count: done_todo_count(DONE_TODOS_PATH)?,
            page: 0,
            focused_line: None,
            eraser_hovered: false,
        })
    }

    pub(crate) fn width(&self) -> usize {
        (PAGE_OFFSET_X + self.pages[0].width) * SCALE
    }

    pub(crate) fn height(&self) -> usize {
        self.pages[0].height * SCALE
    }

    pub(crate) fn fill_color(&self, palette: &Palette) -> Rgba {
        palette.color(palette_color::BLACK)
    }

    pub(crate) fn render(&self, fb: &mut Framebuffer, palette: &Palette) {
        fb.clear(self.fill_color(palette));

        for visual_page in (0..self.pages.len()).rev() {
            let logical_page = (self.page + visual_page) % self.pages.len();
            self.render_page(fb, palette, logical_page, visual_page);
        }

        fb.draw_scaled_region(
            &self.eraser,
            0,
            0,
            ERASER_X * SCALE,
            ERASER_Y * SCALE,
            self.eraser.width,
            self.eraser.height,
            SCALE,
        );
        self.draw_goldstar(fb, palette);
        self.draw_focused_pencil(fb);
    }

    fn render_page(
        &self,
        fb: &mut Framebuffer,
        palette: &Palette,
        logical_page: usize,
        visual_page: usize,
    ) {
        let page_image = &self.pages[visual_page];
        draw_page_image(fb, page_image, page_color(logical_page), palette);
        fb.draw_scaled_region(
            &self.checkboxes,
            0,
            0,
            PAGE_OFFSET_X * SCALE,
            0,
            self.checkboxes.width,
            self.checkboxes.height,
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
        for line in 0..LINE_COUNT {
            let todo = &self.todos[logical_page].items[line];
            if todo.checked {
                self.draw_check(fb, palette, logical_page, line);
            }

            draw_todo_text(
                fb,
                &self.font,
                &todo.text,
                (PAGE_OFFSET_X + TEXT_X) * SCALE,
                (LINE_Y[line] - TEXT_Y_OFFSET) * SCALE,
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

    pub(crate) fn click(&mut self, x: i16, y: i16) -> Result<(), Box<dyn Error>> {
        if self.eraser_at(x, y) {
            self.archive_completed_todos()?;
            self.focused_line = None;
            return Ok(());
        }

        let x = x.max(0) as usize / SCALE;
        let y = y.max(0) as usize / SCALE;
        let Some(x) = x.checked_sub(PAGE_OFFSET_X) else {
            self.focused_line = None;
            return Ok(());
        };

        if x >= PAGE_CURL_X && y >= PAGE_CURL_Y {
            self.page = (self.page + 1) % self.pages.len();
            self.focused_line = None;
            return Ok(());
        }

        if let Some(line) = checkbox_at(x, y) {
            self.todos[self.page].items[line].checked = !self.todos[self.page].items[line].checked;
            self.save_current_page()?;
            return Ok(());
        }

        self.focused_line = line_at(y);
        Ok(())
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
        keycode: u8,
        state: u16,
    ) -> Result<(), Box<dyn Error>> {
        let Some(line) = self.focused_line else {
            return Ok(());
        };

        match edit_key(keycode, state) {
            EditKey::Insert(ch) => {
                let text = &mut self.todos[self.page].items[line].text;
                if self.font.fits_with_insert(text, ch, max_text_width(line)) {
                    text.push(ch);
                }
            }
            EditKey::Backspace => {
                self.todos[self.page].items[line].text.pop();
            }
            EditKey::Enter => {
                self.focused_line = Some((line + 1).min(LINE_COUNT - 1));
            }
            EditKey::Escape => {
                self.focused_line = None;
            }
            EditKey::Tab | EditKey::Left | EditKey::Right | EditKey::None => return Ok(()),
        }

        self.save_current_page()
    }

    fn draw_check(&self, fb: &mut Framebuffer, palette: &Palette, page: usize, line: usize) {
        let src_x = (page + line) % CHECK_VARIANTS * CHECK_SPRITE_W;
        let dest_x = (PAGE_OFFSET_X + CHECK_X - 1) * SCALE;
        let dest_y = (CHECK_Y[line] - 4) * SCALE;

        if self.eraser_hovered {
            draw_tinted_scaled_region(
                fb,
                &self.checks,
                (src_x, 0),
                (dest_x, dest_y),
                (CHECK_SPRITE_W, CHECK_SPRITE_H),
                SCALE,
                palette.color(palette_color::GUNMETAL),
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

    fn save_current_page(&self) -> Result<(), Box<dyn Error>> {
        self.todos[self.page].save(TODO_FILES[self.page])
    }

    fn archive_completed_todos(&mut self) -> Result<(), Box<dyn Error>> {
        let mut archived = Vec::new();
        let mut changed_pages = [false; 3];

        for (page_index, page) in self.todos.iter_mut().enumerate() {
            let mut remaining = Vec::new();
            for item in page.items.iter().cloned() {
                if item.checked {
                    if !item.text.trim().is_empty() {
                        archived.push(item.text.clone());
                    }
                    changed_pages[page_index] = true;
                } else {
                    remaining.push(item);
                }
            }

            if changed_pages[page_index] {
                page.items =
                    std::array::from_fn(|index| remaining.get(index).cloned().unwrap_or_default());
            }
        }

        if !archived.is_empty() {
            let archived_count = archived.len();
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(DONE_TODOS_PATH)?;
            for todo in archived {
                writeln!(file, "{todo}")?;
            }
            self.done_count += archived_count;
        }

        for (page_index, changed) in changed_pages.into_iter().enumerate() {
            if changed {
                self.todos[page_index].save(TODO_FILES[page_index])?;
            }
        }

        Ok(())
    }

    fn draw_goldstar(&self, fb: &mut Framebuffer, palette: &Palette) {
        let star_x = PAGE_OFFSET_X + self.pages[0].width - self.goldstar.width;
        fb.draw_scaled_region(
            &self.goldstar,
            0,
            0,
            star_x * SCALE,
            GOLDSTAR_Y * SCALE,
            self.goldstar.width,
            self.goldstar.height,
            SCALE,
        );

        let count = self.done_count.to_string();
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
                page_color(self.page),
                palette,
                pencil_on_second_line,
            );
        }
    }

    fn draw_pencil_cursor(&self, fb: &mut Framebuffer, x: usize, y: usize) {
        let dest_x = (PAGE_OFFSET_X + x).saturating_sub(PENCIL_TIP_X) * SCALE;
        let dest_y = y.saturating_sub(PENCIL_TIP_Y) * SCALE;

        fb.draw_scaled_region(
            &self.pencil,
            0,
            0,
            dest_x,
            dest_y,
            self.pencil.width,
            self.pencil.height,
            SCALE,
        );
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
        let todo = &self.todos[self.page].items[line];
        let (cursor_x, cursor_y) = pencil_cursor_position(&self.font, line, &todo.text);
        Some((
            line,
            cursor_x,
            cursor_y,
            self.font.wrap_lines(&todo.text, max_text_width(line)).len() > 1,
        ))
    }

    fn should_draw_pencil_shadow(&self, line: usize) -> bool {
        let char_count = self.todos[self.page].items[line].text.chars().count();
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
}

#[derive(Clone)]
struct TodoPage {
    items: [TodoItem; LINE_COUNT],
}

fn done_todo_count(path: &str) -> Result<usize, Box<dyn Error>> {
    if !Path::new(path).exists() {
        return Ok(0);
    }

    Ok(fs::read_to_string(path)?.lines().count())
}

impl TodoPage {
    fn load(path: &str) -> Result<Self, Box<dyn Error>> {
        let mut items = std::array::from_fn(|_| TodoItem::default());
        if Path::new(path).exists() {
            for (index, line) in fs::read_to_string(path)?
                .lines()
                .take(LINE_COUNT)
                .enumerate()
            {
                items[index] = TodoItem::parse(line);
            }
        }
        Ok(Self { items })
    }

    fn save(&self, path: &str) -> Result<(), Box<dyn Error>> {
        let text = self
            .items
            .iter()
            .map(TodoItem::serialize)
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(path, format!("{text}\n"))?;
        Ok(())
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
}

fn draw_page_image(fb: &mut Framebuffer, image: &Image, page_color: PageColor, palette: &Palette) {
    draw_swapped_scaled_image(fb, image, PAGE_OFFSET_X * SCALE, 0, page_color, palette);
}

fn draw_swapped_scaled_image(
    fb: &mut Framebuffer,
    image: &Image,
    dest_x: usize,
    dest_y: usize,
    page_color: PageColor,
    palette: &Palette,
) {
    for y in 0..image.height {
        for x in 0..image.width {
            let color = image.at(x, y);
            if color.a == 0 {
                continue;
            }

            fb.fill_rect(
                dest_x + x * SCALE,
                dest_y + y * SCALE,
                SCALE,
                SCALE,
                swap_page_color(color, page_color, palette),
            );
        }
    }
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
    for y in 0..image.height {
        for x in 0..image.width {
            let color = image.at(x, y);
            if color.a == 0 {
                continue;
            }

            fb.fill_rect(
                dest_x + x * SCALE,
                dest_y + y * SCALE,
                SCALE,
                SCALE,
                swap_pencil_shadow_color(color, page_color, palette, pencil_on_second_line),
            );
        }
    }
}

fn page_color(page: usize) -> PageColor {
    match page % 3 {
        0 => PageColor::Pink,
        1 => PageColor::Yellow,
        _ => PageColor::Green,
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
    match page_color {
        PageColor::Pink => match source_color {
            palette_color::LIME => palette_color::CRIMSON,
            palette_color::PINE => palette_color::ROSE,
            _ => source_color,
        },
        PageColor::Yellow => match source_color {
            palette_color::LAVENDER => palette_color::CYAN,
            palette_color::GUNMETAL => palette_color::GUNMETAL,
            palette_color::PLUM => palette_color::PLUM,
            palette_color::BROWN => palette_color::BROWN,
            palette_color::PEACH => palette_color::CREAM,
            palette_color::CREAM => palette_color::CREAM,
            palette_color::LIME => palette_color::CRIMSON,
            palette_color::GREEN => palette_color::GREEN,
            palette_color::ORANGE => palette_color::ORANGE,
            palette_color::CRIMSON => palette_color::BROWN,
            palette_color::ROSE => palette_color::ORANGE,
            palette_color::PURPLE => palette_color::PURPLE,
            palette_color::CYAN => palette_color::CYAN,
            palette_color::BLUE => palette_color::BLUE,
            palette_color::PINE => palette_color::ROSE,
            palette_color::BLACK => palette_color::BLACK,
            _ => source_color,
        },
        PageColor::Green => match source_color {
            palette_color::LAVENDER => palette_color::LAVENDER,
            palette_color::GUNMETAL => palette_color::GUNMETAL,
            palette_color::PLUM => palette_color::PLUM,
            palette_color::BROWN => palette_color::BROWN,
            palette_color::PEACH => palette_color::LIME,
            palette_color::CREAM => palette_color::CREAM,
            palette_color::LIME => palette_color::GUNMETAL,
            palette_color::GREEN => palette_color::GREEN,
            palette_color::ORANGE => palette_color::GREEN,
            palette_color::CRIMSON => palette_color::PINE,
            palette_color::ROSE => palette_color::GREEN,
            palette_color::PURPLE => palette_color::PURPLE,
            palette_color::CYAN => palette_color::CYAN,
            palette_color::BLUE => palette_color::BLUE,
            palette_color::PINE => palette_color::BROWN,
            palette_color::BLACK => palette_color::BLACK,
            _ => source_color,
        },
    }
}

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

fn draw_tinted_scaled_region(
    fb: &mut Framebuffer,
    image: &Image,
    src: (usize, usize),
    dest: (usize, usize),
    size: (usize, usize),
    scale: usize,
    tint: Rgba,
) {
    let (src_x, src_y) = src;
    let (dest_x, dest_y) = dest;
    let (width, height) = size;

    for y in 0..height {
        for x in 0..width {
            if image.at(src_x + x, src_y + y).a == 0 {
                continue;
            }
            fb.fill_rect(dest_x + x * scale, dest_y + y * scale, scale, scale, tint);
        }
    }
}

fn checkbox_at(x: usize, y: usize) -> Option<usize> {
    CHECK_Y.iter().position(|&check_y| {
        x >= CHECK_X && x < CHECK_X + CHECK_W && y >= check_y && y < check_y + CHECK_H
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

fn pencil_cursor_position(font: &BitmapFont, line: usize, text: &str) -> (usize, usize) {
    let base_y = LINE_Y[line] - TEXT_Y_OFFSET;
    let lines = font.wrap_lines(text, max_text_width(line));

    if lines.len() <= 1 {
        (TEXT_X + font.text_width(&lines[0]), base_y)
    } else {
        (
            TEXT_X + font.text_width(&lines[1]).min(max_text_width(line)),
            base_y + WRAPPED_SECOND_LINE_OFFSET_Y,
        )
    }
}
