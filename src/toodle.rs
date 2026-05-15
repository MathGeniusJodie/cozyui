use std::error::Error;
use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::palette_color;
use crate::{Framebuffer, Image, Palette, Rgba, decode_png_with_size};

const SCALE: usize = 1;
const GLYPH_SCALE: usize = 1;
const GLYPH_W: usize = 6;
const GLYPH_H: usize = 12;
const LINE_COUNT: usize = 6;
const CHECK_VARIANTS: usize = 4;

const TOP_PAGE_PATH: &str = "assets/toodle_top.png";
const SECOND_PAGE_PATH: &str = "assets/toodle_2nd.png";
const THIRD_PAGE_PATH: &str = "assets/toodle_page.png";
const CHECKBOXES_PATH: &str = "assets/checkboxes.png";
const CHECKS_PATH: &str = "assets/checks.png";
const FONT_PATH: &str = "glyphs/0000-007F.png";

const TODO_FILES: [&str; 3] = ["toodle_top.txt", "toodle_second.txt", "toodle_third.txt"];
const LINE_Y: [usize; LINE_COUNT] = [73, 95, 117, 139, 161, 183];
const TEXT_X: usize = 34;
const TEXT_Y_OFFSET: usize = 2;
const CHECK_X: usize = 14;
const CHECK_Y: [usize; LINE_COUNT] = [71, 93, 115, 137, 159, 181];
const CHECK_W: usize = 13;
const CHECK_H: usize = 13;
const CHECK_SPRITE_W: usize = 16;
const CHECK_SPRITE_H: usize = 16;
const PAGE_CURL_X: usize = 140;
const PAGE_CURL_Y: usize = 168;
const MAX_TEXT_CHARS: usize = 22;
const CURSOR_W: usize = 5;
const CURSOR_H: usize = 10;
const CURSOR_BLINK: Duration = Duration::from_millis(500);

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
    font: GlyphAtlas,
    todos: [TodoPage; 3],
    page: usize,
    focused_line: Option<usize>,
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
            font: GlyphAtlas::load()?,
            todos: [
                TodoPage::load(TODO_FILES[0])?,
                TodoPage::load(TODO_FILES[1])?,
                TodoPage::load(TODO_FILES[2])?,
            ],
            page: 0,
            focused_line: None,
        })
    }

    pub(crate) fn width(&self) -> usize {
        self.pages[0].width * SCALE
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
            0,
            0,
            self.checkboxes.width,
            self.checkboxes.height,
            SCALE,
        );

        let text_color = palette.color(palette_color::GUNMETAL);
        let focus_color = palette.color(palette_color::CYAN);
        let is_top_page = visual_page == 0;
        for line in 0..LINE_COUNT {
            let todo = &self.todos[logical_page].items[line];
            if todo.checked {
                self.draw_check(fb, logical_page, line);
            }

            draw_text(
                fb,
                &self.font,
                &todo.text,
                TEXT_X * SCALE,
                (LINE_Y[line] - TEXT_Y_OFFSET) * SCALE,
                GLYPH_SCALE * SCALE,
                text_color,
                MAX_TEXT_CHARS,
            );

            if is_top_page && self.focused_line == Some(line) && cursor_visible() {
                let cursor_x = TEXT_X + todo.text.chars().count().min(MAX_TEXT_CHARS) * GLYPH_W;
                fb.fill_rect(
                    cursor_x * SCALE,
                    (LINE_Y[line] - TEXT_Y_OFFSET) * SCALE,
                    CURSOR_W * SCALE,
                    CURSOR_H * SCALE,
                    focus_color,
                );
            }
        }
    }

    pub(crate) fn click(&mut self, x: i16, y: i16) -> Result<(), Box<dyn Error>> {
        let x = x.max(0) as usize / SCALE;
        let y = y.max(0) as usize / SCALE;

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
                if text.chars().count() < MAX_TEXT_CHARS {
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
            EditKey::None => return Ok(()),
        }

        self.save_current_page()
    }

    fn draw_check(&self, fb: &mut Framebuffer, page: usize, line: usize) {
        fb.draw_scaled_region(
            &self.checks,
            (page + line) % CHECK_VARIANTS * CHECK_SPRITE_W,
            0,
            (CHECK_X - 1) * SCALE,
            (CHECK_Y[line] - 4) * SCALE,
            CHECK_SPRITE_W,
            CHECK_SPRITE_H,
            SCALE,
        );
    }

    fn save_current_page(&self) -> Result<(), Box<dyn Error>> {
        self.todos[self.page].save(TODO_FILES[self.page])
    }
}

#[derive(Clone)]
struct TodoPage {
    items: [TodoItem; LINE_COUNT],
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
    for y in 0..image.height {
        for x in 0..image.width {
            let color = image.at(x, y);
            if color.a == 0 {
                continue;
            }

            fb.fill_rect(
                x * SCALE,
                y * SCALE,
                SCALE,
                SCALE,
                swap_page_color(color, page_color, palette),
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
    let role = match (color.r, color.g, color.b, color.a) {
        (_, _, _, 0) => return color,
        (245, 237, 186, _) => PageColorRole::Light,
        (215, 155, 125, _) | (228, 148, 58, _) | (100, 125, 52, _) => PageColorRole::Body,
        (210, 100, 113, _) | (154, 99, 72, _) | (192, 199, 65, _) => PageColorRole::Bright,
        (157, 48, 59, _) | (23, 67, 75, _) => PageColorRole::Dark,
        (126, 196, 193, _) | (140, 143, 174, _) => PageColorRole::Rule,
        (52, 133, 157, _) | (112, 55, 127, _) | (88, 69, 99, _) => PageColorRole::RuleDark,
        _ => return color,
    };

    palette.color(match (page_color, role) {
        (PageColor::Pink, PageColorRole::Light) => palette_color::CREAM,
        (PageColor::Pink, PageColorRole::Body) => palette_color::PEACH,
        (PageColor::Pink, PageColorRole::Bright) => palette_color::ROSE,
        (PageColor::Pink, PageColorRole::Dark) => palette_color::CRIMSON,
        (PageColor::Pink, PageColorRole::Rule) => palette_color::LAVENDER,
        (PageColor::Pink, PageColorRole::RuleDark) => palette_color::PURPLE,

        (PageColor::Yellow, PageColorRole::Light) => palette_color::CREAM,
        (PageColor::Yellow, PageColorRole::Body) => palette_color::CREAM,
        (PageColor::Yellow, PageColorRole::Bright) => palette_color::PEACH,
        (PageColor::Yellow, PageColorRole::Dark) => palette_color::BROWN,
        (PageColor::Yellow, PageColorRole::Rule) => palette_color::CYAN,
        (PageColor::Yellow, PageColorRole::RuleDark) => palette_color::BLUE,

        (PageColor::Green, PageColorRole::Light) => palette_color::LIME,
        (PageColor::Green, PageColorRole::Body) => palette_color::LIME,
        (PageColor::Green, PageColorRole::Bright) => palette_color::LIME,
        (PageColor::Green, PageColorRole::Dark) => palette_color::PINE,
        (PageColor::Green, PageColorRole::Rule) => palette_color::LAVENDER,
        (PageColor::Green, PageColorRole::RuleDark) => palette_color::GUNMETAL,
    })
}

#[derive(Clone, Copy)]
enum PageColorRole {
    Light,
    Body,
    Bright,
    Dark,
    Rule,
    RuleDark,
}

struct GlyphAtlas {
    width: usize,
    pixels: Vec<bool>,
}

impl GlyphAtlas {
    fn load() -> Result<Self, Box<dyn Error>> {
        let (width, _height, pixels) = decode_png_with_size(FONT_PATH)?;
        let pixels = pixels.into_iter().map(is_glyph_ink).collect();
        Ok(Self { width, pixels })
    }

    fn is_on(&self, ch: char, x: usize, y: usize) -> bool {
        let code = ch as usize;
        if code >= 128 {
            return self.is_on('?', x, y);
        }

        let cols = self.width / GLYPH_W;
        let sx = (code % cols) * GLYPH_W + x;
        let sy = (code / cols) * GLYPH_H + y;
        self.pixels[sy * self.width + sx]
    }
}

fn draw_text(
    fb: &mut Framebuffer,
    atlas: &GlyphAtlas,
    text: &str,
    x: usize,
    y: usize,
    scale: usize,
    color: Rgba,
    max_chars: usize,
) {
    for (index, ch) in text.chars().take(max_chars).enumerate() {
        draw_glyph(fb, atlas, ch, x + index * GLYPH_W * scale, y, scale, color);
    }
}

fn draw_glyph(
    fb: &mut Framebuffer,
    atlas: &GlyphAtlas,
    ch: char,
    x: usize,
    y: usize,
    scale: usize,
    color: Rgba,
) {
    for gy in 0..GLYPH_H {
        for gx in 0..GLYPH_W {
            if !atlas.is_on(ch, gx, gy) {
                continue;
            }
            fb.fill_rect(x + gx * scale, y + gy * scale, scale, scale, color);
        }
    }
}

fn is_glyph_ink(color: Rgba) -> bool {
    let luminance = color.r as u16 + color.g as u16 + color.b as u16;
    luminance >= 384
}

fn checkbox_at(x: usize, y: usize) -> Option<usize> {
    CHECK_Y.iter().position(|&check_y| {
        x >= CHECK_X && x < CHECK_X + CHECK_W && y >= check_y && y < check_y + CHECK_H
    })
}

fn line_at(y: usize) -> Option<usize> {
    LINE_Y
        .iter()
        .position(|&line_y| y >= line_y - 17 && y < line_y + 4)
}

fn cursor_visible() -> bool {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    (elapsed.as_millis() / CURSOR_BLINK.as_millis()) % 2 == 0
}

enum EditKey {
    Insert(char),
    Backspace,
    Enter,
    Escape,
    None,
}

fn edit_key(keycode: u8, state: u16) -> EditKey {
    let shift = state & 1 != 0;
    match (keycode, shift) {
        (9, _) => EditKey::Escape,
        (22, _) => EditKey::Backspace,
        (36, _) => EditKey::Enter,
        (65, _) => EditKey::Insert(' '),
        (10, false) => EditKey::Insert('1'),
        (10, true) => EditKey::Insert('!'),
        (11, false) => EditKey::Insert('2'),
        (11, true) => EditKey::Insert('@'),
        (12, false) => EditKey::Insert('3'),
        (12, true) => EditKey::Insert('#'),
        (13, false) => EditKey::Insert('4'),
        (13, true) => EditKey::Insert('$'),
        (14, false) => EditKey::Insert('5'),
        (14, true) => EditKey::Insert('%'),
        (15, false) => EditKey::Insert('6'),
        (15, true) => EditKey::Insert('^'),
        (16, false) => EditKey::Insert('7'),
        (16, true) => EditKey::Insert('&'),
        (17, false) => EditKey::Insert('8'),
        (17, true) => EditKey::Insert('*'),
        (18, false) => EditKey::Insert('9'),
        (18, true) => EditKey::Insert('('),
        (19, false) => EditKey::Insert('0'),
        (19, true) => EditKey::Insert(')'),
        (20, false) => EditKey::Insert('-'),
        (20, true) => EditKey::Insert('_'),
        (21, false) => EditKey::Insert('='),
        (21, true) => EditKey::Insert('+'),
        (24, false) => EditKey::Insert('q'),
        (24, true) => EditKey::Insert('Q'),
        (25, false) => EditKey::Insert('w'),
        (25, true) => EditKey::Insert('W'),
        (26, false) => EditKey::Insert('e'),
        (26, true) => EditKey::Insert('E'),
        (27, false) => EditKey::Insert('r'),
        (27, true) => EditKey::Insert('R'),
        (28, false) => EditKey::Insert('t'),
        (28, true) => EditKey::Insert('T'),
        (29, false) => EditKey::Insert('y'),
        (29, true) => EditKey::Insert('Y'),
        (30, false) => EditKey::Insert('u'),
        (30, true) => EditKey::Insert('U'),
        (31, false) => EditKey::Insert('i'),
        (31, true) => EditKey::Insert('I'),
        (32, false) => EditKey::Insert('o'),
        (32, true) => EditKey::Insert('O'),
        (33, false) => EditKey::Insert('p'),
        (33, true) => EditKey::Insert('P'),
        (34, false) => EditKey::Insert('['),
        (34, true) => EditKey::Insert('{'),
        (35, false) => EditKey::Insert(']'),
        (35, true) => EditKey::Insert('}'),
        (38, false) => EditKey::Insert('a'),
        (38, true) => EditKey::Insert('A'),
        (39, false) => EditKey::Insert('s'),
        (39, true) => EditKey::Insert('S'),
        (40, false) => EditKey::Insert('d'),
        (40, true) => EditKey::Insert('D'),
        (41, false) => EditKey::Insert('f'),
        (41, true) => EditKey::Insert('F'),
        (42, false) => EditKey::Insert('g'),
        (42, true) => EditKey::Insert('G'),
        (43, false) => EditKey::Insert('h'),
        (43, true) => EditKey::Insert('H'),
        (44, false) => EditKey::Insert('j'),
        (44, true) => EditKey::Insert('J'),
        (45, false) => EditKey::Insert('k'),
        (45, true) => EditKey::Insert('K'),
        (46, false) => EditKey::Insert('l'),
        (46, true) => EditKey::Insert('L'),
        (47, false) => EditKey::Insert(';'),
        (47, true) => EditKey::Insert(':'),
        (48, false) => EditKey::Insert('\''),
        (48, true) => EditKey::Insert('"'),
        (49, false) => EditKey::Insert('`'),
        (49, true) => EditKey::Insert('~'),
        (51, false) => EditKey::Insert('\\'),
        (51, true) => EditKey::Insert('|'),
        (52, false) => EditKey::Insert('z'),
        (52, true) => EditKey::Insert('Z'),
        (53, false) => EditKey::Insert('x'),
        (53, true) => EditKey::Insert('X'),
        (54, false) => EditKey::Insert('c'),
        (54, true) => EditKey::Insert('C'),
        (55, false) => EditKey::Insert('v'),
        (55, true) => EditKey::Insert('V'),
        (56, false) => EditKey::Insert('b'),
        (56, true) => EditKey::Insert('B'),
        (57, false) => EditKey::Insert('n'),
        (57, true) => EditKey::Insert('N'),
        (58, false) => EditKey::Insert('m'),
        (58, true) => EditKey::Insert('M'),
        (59, false) => EditKey::Insert(','),
        (59, true) => EditKey::Insert('<'),
        (60, false) => EditKey::Insert('.'),
        (60, true) => EditKey::Insert('>'),
        (61, false) => EditKey::Insert('/'),
        (61, true) => EditKey::Insert('?'),
        _ => EditKey::None,
    }
}
