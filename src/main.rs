use std::borrow::Cow;
use std::error::Error;
use std::fs::File;
use std::io::BufReader;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, EventLoopSender, Msg};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Term, point_to_viewport};
use alacritty_terminal::tty;
use x11rb::connection::Connection;
use x11rb::protocol::Event as XEvent;
use x11rb::protocol::xproto::ConnectionExt as XprotoConnectionExt;
use x11rb::protocol::xproto::{
    AtomEnum, ButtonIndex, ChangeWindowAttributesAux, CreateGCAux, CreateWindowAux, EventMask,
    Gcontext, ImageFormat, PropMode, Window, WindowClass,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

const BG_SCALE: usize = 1;
const GLYPH_SCALE: usize = 1;
const GLYPH_W: usize = 6;
const GLYPH_H: usize = 12;

const SCREEN_SOURCE_X: usize = 49;
const SCREEN_SOURCE_Y: usize = 49;
const SCREEN_W: usize = 205 * BG_SCALE;
const SCREEN_H: usize = 158 * BG_SCALE;

const PALETTE_PATH: &str = "na16-1x.png";
const GREEN_LOW_CONTRAST_PATH: &str = "puter_g_lc.png";
const ORANGE_LOW_CONTRAST_PATH: &str = "puter_o_lc.png";
const HIGH_CONTRAST_PATH: &str = "puter_hc.png";
const ART_CROP_X: usize = 19;
const ART_CROP_Y: usize = 17;

const COLOR_CURSOR: usize = 0;
const COLOR_LIGHT_OFF: usize = 2;
const COLOR_LIGHT_OFF_CORE: usize = 1;
const COLOR_LIGHT_OFF_TOP: usize = 4;
const COLOR_LIGHT_RED: usize = 9;
const COLOR_LIGHT_RED_CORE: usize = 8;
const COLOR_LIGHT_RED_TOP: usize = 8;
const COLOR_LIGHT_GREEN: usize = 7;
const COLOR_LIGHT_GREEN_CORE: usize = 6;
const COLOR_LIGHT_GREEN_TOP: usize = 6;
const COLOR_ORANGE_TEXT: usize = 8;
const COLOR_ORANGE_GLOW: usize = 3;
const COLOR_GREEN_TEXT: usize = 6;
const COLOR_GREEN_GLOW: usize = 7;

const BUTTON_SPRITES_PATH: &str = "assets/buttons-pressed.png";
const BUTTON_W: usize = 20;
const BUTTON_H: usize = 16;
const BUTTON_PRESSED_OFFSET_X: isize = -3;
const SCROLL_LINES: i32 = 3;
const WHEEL_UP: u8 = 4;
const WHEEL_DOWN: u8 = 5;
const LIGHT_W: usize = 4;
const LIGHT_H: usize = 5;
const LIGHTS: [Light; 3] = [
    Light {
        x: 155,
        y: 222,
        kind: LightKind::Brightness,
    },
    Light {
        x: 164,
        y: 222,
        kind: LightKind::Color,
    },
    Light {
        x: 173,
        y: 222,
        kind: LightKind::Contrast,
    },
];
const BUTTON_TARGETS: [Button; 3] = [
    Button {
        x: 160,
        y: 220,
        action: ButtonAction::Brightness,
    },
    Button {
        x: 179,
        y: 220,
        action: ButtonAction::Color,
    },
    Button {
        x: 198,
        y: 220,
        action: ButtonAction::Contrast,
    },
];

#[derive(Clone, Copy)]
struct Button {
    x: usize,
    y: usize,
    action: ButtonAction,
}

#[derive(Clone, Copy)]
struct Light {
    x: usize,
    y: usize,
    kind: LightKind,
}

#[derive(Clone, Copy)]
enum LightKind {
    Brightness,
    Color,
    Contrast,
}

#[derive(Clone, Copy)]
enum LightState {
    Off,
    Red,
    Green,
}

#[derive(Clone, Copy)]
enum ButtonAction {
    Brightness,
    Color,
    Contrast,
}

#[derive(Clone, Copy)]
enum TextMode {
    Green,
    Orange,
}

#[derive(Clone, Copy)]
struct DisplaySettings {
    high_brightness: bool,
    text_mode: TextMode,
    high_contrast: bool,
}

impl DisplaySettings {
    fn new() -> Self {
        Self {
            high_brightness: true,
            text_mode: TextMode::Orange,
            high_contrast: false,
        }
    }

    fn toggle(&mut self, action: ButtonAction) {
        match action {
            ButtonAction::Brightness => self.high_brightness = !self.high_brightness,
            ButtonAction::Color => {
                self.text_mode = match self.text_mode {
                    TextMode::Green => TextMode::Orange,
                    TextMode::Orange => TextMode::Green,
                };
            }
            ButtonAction::Contrast => self.high_contrast = !self.high_contrast,
        }
    }

    fn text_color(&self, palette: &Palette) -> Rgba {
        if self.high_brightness {
            return palette.closest_to_white();
        }

        match self.text_mode {
            TextMode::Green => palette.color(COLOR_GREEN_TEXT),
            TextMode::Orange => palette.color(COLOR_ORANGE_TEXT),
        }
    }

    fn glow_color(&self, palette: &Palette) -> Rgba {
        match self.text_mode {
            TextMode::Green => palette.color(COLOR_GREEN_GLOW),
            TextMode::Orange => palette.color(COLOR_ORANGE_GLOW),
        }
    }

    fn light_state(&self, kind: LightKind) -> LightState {
        match kind {
            LightKind::Brightness => {
                if self.high_brightness {
                    LightState::Green
                } else {
                    LightState::Off
                }
            }
            LightKind::Color => match self.text_mode {
                TextMode::Green => LightState::Green,
                TextMode::Orange => LightState::Red,
            },
            LightKind::Contrast => {
                if self.high_contrast {
                    LightState::Red
                } else {
                    LightState::Off
                }
            }
        }
    }
}

struct ModeImages {
    green_low_contrast: Image,
    orange_low_contrast: Image,
    high_contrast: Image,
}

impl ModeImages {
    fn load(palette: &Palette) -> Result<Self, Box<dyn Error>> {
        Ok(Self {
            green_low_contrast: Image::load(GREEN_LOW_CONTRAST_PATH, palette)?,
            orange_low_contrast: Image::load(ORANGE_LOW_CONTRAST_PATH, palette)?,
            high_contrast: Image::load(HIGH_CONTRAST_PATH, palette)?,
        })
    }

    fn for_settings(&self, settings: DisplaySettings) -> &Image {
        if settings.high_contrast {
            return &self.high_contrast;
        }

        match settings.text_mode {
            TextMode::Green => &self.green_low_contrast,
            TextMode::Orange => &self.orange_low_contrast,
        }
    }
}

fn art_x(x: usize) -> usize {
    (x - ART_CROP_X) * BG_SCALE
}

fn art_y(y: usize) -> usize {
    (y - ART_CROP_Y) * BG_SCALE
}

#[derive(Clone)]
struct UiEventProxy(Sender<Event>);

impl EventListener for UiEventProxy {
    fn send_event(&self, event: Event) {
        let _ = self.0.send(event);
    }
}

struct TermSize {
    columns: usize,
    lines: usize,
}

impl Dimensions for TermSize {
    fn total_lines(&self) -> usize {
        self.lines
    }

    fn screen_lines(&self) -> usize {
        self.lines
    }

    fn columns(&self) -> usize {
        self.columns
    }
}

#[derive(Clone, Copy)]
struct Rgba {
    r: u8,
    g: u8,
    b: u8,
    a: u8,
}

struct Palette {
    colors: Vec<Rgba>,
}

impl Palette {
    fn load(path: &str) -> Result<Self, Box<dyn Error>> {
        let pixels = decode_png(path)?;
        let colors = pixels
            .into_iter()
            .map(|mut color| {
                color.a = 255;
                color
            })
            .collect::<Vec<_>>();

        if colors.is_empty() {
            return Err(format!("palette PNG has no colors: {path}").into());
        }

        Ok(Self { colors })
    }

    fn color(&self, index: usize) -> Rgba {
        self.colors[index % self.colors.len()]
    }

    fn nearest(&self, color: Rgba) -> Rgba {
        self.colors
            .iter()
            .copied()
            .min_by_key(|candidate| color_distance(*candidate, color))
            .unwrap_or(self.colors[0])
    }

    fn closest_to_white(&self) -> Rgba {
        self.nearest(Rgba {
            r: 255,
            g: 255,
            b: 255,
            a: 255,
        })
    }

    fn darkest(&self) -> Rgba {
        self.colors
            .iter()
            .copied()
            .min_by_key(|color| color.r as u16 + color.g as u16 + color.b as u16)
            .unwrap_or(self.colors[0])
    }
}

struct Image {
    width: usize,
    height: usize,
    pixels: Vec<Rgba>,
}

impl Image {
    fn load(path: &str, palette: &Palette) -> Result<Self, Box<dyn Error>> {
        let (width, height, pixels) = decode_png_with_size(path)?;
        let pixels = pixels
            .into_iter()
            .map(|color| {
                if color.a == 0 {
                    let mut transparent = palette.darkest();
                    transparent.a = 0;
                    transparent
                } else {
                    palette.nearest(color)
                }
            })
            .collect();
        Ok(Self {
            width,
            height,
            pixels,
        })
    }

    fn at(&self, x: usize, y: usize) -> Rgba {
        self.pixels[y * self.width + x]
    }
}

struct GlyphAtlas {
    width: usize,
    pixels: Vec<bool>,
}

impl GlyphAtlas {
    fn load() -> Result<Self, Box<dyn Error>> {
        let (width, _height, pixels) = decode_png_with_size("glyphs/0000-007F.png")?;
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

struct Framebuffer {
    width: usize,
    height: usize,
    pixels: Vec<Rgba>,
}

impl Framebuffer {
    fn new(width: usize, height: usize, fill: Rgba) -> Self {
        Self {
            width,
            height,
            pixels: vec![fill; width * height],
        }
    }

    fn clear_scaled(&mut self, image: &Image, scale: usize) {
        for y in 0..self.height {
            for x in 0..self.width {
                let sx = (x / scale).min(image.width - 1);
                let sy = (y / scale).min(image.height - 1);
                self.pixels[y * self.width + x] = image.at(sx, sy);
            }
        }
    }

    fn fill_rect(&mut self, x: usize, y: usize, w: usize, h: usize, color: Rgba) {
        for py in y..(y + h).min(self.height) {
            for px in x..(x + w).min(self.width) {
                self.pixels[py * self.width + px] = color;
            }
        }
    }

    fn fill_source_rect(&mut self, x: usize, y: usize, w: usize, h: usize, color: Rgba) {
        self.fill_rect(
            x * BG_SCALE,
            y * BG_SCALE,
            w * BG_SCALE,
            h * BG_SCALE,
            color,
        );
    }

    fn draw_glyph(&mut self, atlas: &GlyphAtlas, ch: char, x: usize, y: usize, color: Rgba) {
        for gy in 0..GLYPH_H {
            for gx in 0..GLYPH_W {
                if !atlas.is_on(ch, gx, gy) {
                    continue;
                }
                let dx = x + gx * GLYPH_SCALE;
                let dy = y + gy * GLYPH_SCALE;
                self.fill_rect(dx, dy, GLYPH_SCALE, GLYPH_SCALE, color);
            }
        }
    }

    fn draw_scaled_region(
        &mut self,
        image: &Image,
        src_x: usize,
        src_y: usize,
        dest_x: usize,
        dest_y: usize,
        width: usize,
        height: usize,
        scale: usize,
    ) {
        for y in 0..height {
            for x in 0..width {
                let color = image.at(src_x + x, src_y + y);
                self.fill_rect(dest_x + x * scale, dest_y + y * scale, scale, scale, color);
            }
        }
    }

    fn ximage_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(self.width * self.height * 4);
        for p in &self.pixels {
            bytes.extend_from_slice(&[p.b, p.g, p.r, 0]);
        }
        bytes
    }
}

struct XWindow {
    conn: RustConnection,
    window: Window,
    gc: Gcontext,
    depth: u8,
}

impl XWindow {
    fn open(width: usize, height: usize) -> Result<Self, Box<dyn Error>> {
        let (conn, screen_num) = RustConnection::connect(None)?;
        let screen = &conn.setup().roots[screen_num];
        let root = screen.root;
        let depth = screen.root_depth;
        let window = conn.generate_id()?;
        let gc = conn.generate_id()?;

        conn.create_window(
            0,
            window,
            root,
            0,
            0,
            width as u16,
            height as u16,
            0,
            WindowClass::INPUT_OUTPUT,
            0,
            &CreateWindowAux::new().event_mask(
                EventMask::EXPOSURE
                    | EventMask::KEY_PRESS
                    | EventMask::BUTTON_PRESS
                    | EventMask::BUTTON_RELEASE
                    | EventMask::STRUCTURE_NOTIFY,
            ),
        )?;
        conn.change_window_attributes(
            window,
            &ChangeWindowAttributesAux::new().event_mask(
                EventMask::EXPOSURE
                    | EventMask::KEY_PRESS
                    | EventMask::BUTTON_PRESS
                    | EventMask::BUTTON_RELEASE
                    | EventMask::STRUCTURE_NOTIFY,
            ),
        )?;
        conn.create_gc(gc, window, &CreateGCAux::new())?;

        let title = b"cozyui";
        conn.change_property8(
            PropMode::REPLACE,
            window,
            AtomEnum::WM_NAME,
            AtomEnum::STRING,
            title,
        )?;
        conn.map_window(window)?;
        conn.flush()?;

        Ok(Self {
            conn,
            window,
            gc,
            depth,
        })
    }

    fn draw(&self, fb: &Framebuffer) -> Result<(), Box<dyn Error>> {
        let data = fb.ximage_bytes();
        self.conn.put_image(
            ImageFormat::Z_PIXMAP,
            self.window,
            self.gc,
            fb.width as u16,
            fb.height as u16,
            0,
            0,
            0,
            self.depth,
            &data,
        )?;
        self.conn.flush()?;
        Ok(())
    }
}

fn render(
    fb: &mut Framebuffer,
    mode_images: &ModeImages,
    atlas: &GlyphAtlas,
    button_sprites: &Image,
    active_button: Option<usize>,
    settings: DisplaySettings,
    palette: &Palette,
    term: &Arc<FairMutex<Term<UiEventProxy>>>,
) {
    fb.clear_scaled(mode_images.for_settings(settings), BG_SCALE);
    draw_lights(fb, settings, palette);

    let cell_w = GLYPH_W * GLYPH_SCALE;
    let cell_h = GLYPH_H * GLYPH_SCALE;
    let term = term.lock();
    let content = term.renderable_content();

    for indexed in content.display_iter {
        let Some(point) = point_to_viewport(content.display_offset, indexed.point) else {
            continue;
        };
        let cell = indexed.cell;
        if cell.flags.contains(Flags::WIDE_CHAR_SPACER | Flags::HIDDEN) || cell.c == ' ' {
            continue;
        }
        let x = art_x(SCREEN_SOURCE_X) + point.column.0 * cell_w;
        let y = art_y(SCREEN_SOURCE_Y) + point.line * cell_h;
        let mut color = settings.text_color(palette);
        if cell.flags.contains(Flags::DIM) {
            color = settings.glow_color(palette);
        }
        if cell.flags.contains(Flags::BOLD) {
            color = settings.text_color(palette);
        }
        if settings.high_brightness {
            let glow = settings.glow_color(palette);
            fb.draw_glyph(atlas, cell.c, x - 1, y, glow);
            fb.draw_glyph(atlas, cell.c, x + 1, y, glow);
            fb.draw_glyph(atlas, cell.c, x, y - 1, glow);
            fb.draw_glyph(atlas, cell.c, x, y + 1, glow);
        }
        fb.draw_glyph(atlas, cell.c, x, y, color);
    }

    if let Some(cursor_point) = point_to_viewport(content.display_offset, content.cursor.point) {
        let cursor_x = art_x(SCREEN_SOURCE_X) + cursor_point.column.0 * cell_w;
        let cursor_y = art_y(SCREEN_SOURCE_Y) + cursor_point.line * cell_h;
        fb.fill_rect(
            cursor_x,
            cursor_y,
            cell_w,
            cell_h,
            palette.color(COLOR_CURSOR),
        );
    }

    if let Some(index) = active_button {
        let button = BUTTON_TARGETS[index];
        fb.draw_scaled_region(
            button_sprites,
            index * BUTTON_W,
            0,
            ((button.x as isize - ART_CROP_X as isize + BUTTON_PRESSED_OFFSET_X) as usize)
                * BG_SCALE,
            art_y(button.y),
            BUTTON_W,
            BUTTON_H,
            BG_SCALE,
        );
    }
}

fn draw_lights(fb: &mut Framebuffer, settings: DisplaySettings, palette: &Palette) {
    for light in LIGHTS {
        draw_light(fb, light, settings.light_state(light.kind), palette);
    }
}

fn draw_light(fb: &mut Framebuffer, light: Light, state: LightState, palette: &Palette) {
    let (shell, core, top) = match state {
        LightState::Off => (
            palette.color(COLOR_LIGHT_OFF),
            palette.color(COLOR_LIGHT_OFF_CORE),
            palette.color(COLOR_LIGHT_OFF_TOP),
        ),
        LightState::Red => (
            palette.color(COLOR_LIGHT_RED),
            palette.color(COLOR_LIGHT_RED_CORE),
            palette.color(COLOR_LIGHT_RED_TOP),
        ),
        LightState::Green => (
            palette.color(COLOR_LIGHT_GREEN),
            palette.color(COLOR_LIGHT_GREEN_CORE),
            palette.color(COLOR_LIGHT_GREEN_TOP),
        ),
    };

    fb.fill_source_rect(light.x, light.y, LIGHT_W, LIGHT_H, shell);
    fb.fill_source_rect(light.x, light.y, LIGHT_W, 1, top);
    fb.fill_source_rect(light.x + 1, light.y + 2, LIGHT_W - 2, LIGHT_H - 3, core);
}

fn decode_png(path: &str) -> Result<Vec<Rgba>, Box<dyn Error>> {
    Ok(decode_png_with_size(path)?.2)
}

fn decode_png_with_size(path: &str) -> Result<(usize, usize, Vec<Rgba>), Box<dyn Error>> {
    let file = File::open(path)?;
    let mut decoder = png::Decoder::new(BufReader::new(file));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info()?;
    let mut data = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut data)?;
    let bytes = &data[..info.buffer_size()];

    let mut pixels = Vec::with_capacity((info.width * info.height) as usize);
    match info.color_type {
        png::ColorType::Rgb => {
            for chunk in bytes.chunks_exact(3) {
                pixels.push(Rgba {
                    r: chunk[0],
                    g: chunk[1],
                    b: chunk[2],
                    a: 255,
                });
            }
        }
        png::ColorType::Rgba => {
            for chunk in bytes.chunks_exact(4) {
                pixels.push(Rgba {
                    r: chunk[0],
                    g: chunk[1],
                    b: chunk[2],
                    a: chunk[3],
                });
            }
        }
        png::ColorType::Indexed => {
            let palette = reader
                .info()
                .palette
                .as_ref()
                .ok_or("indexed PNG has no palette")?;
            let trns = reader.info().trns.as_deref().unwrap_or(&[]);
            for &idx in bytes {
                let base = idx as usize * 3;
                let a = trns.get(idx as usize).copied().unwrap_or(255);
                pixels.push(Rgba {
                    r: palette[base],
                    g: palette[base + 1],
                    b: palette[base + 2],
                    a,
                });
            }
        }
        other => return Err(format!("unsupported PNG color type: {other:?}").into()),
    }

    Ok((info.width as usize, info.height as usize, pixels))
}

fn color_distance(a: Rgba, b: Rgba) -> u32 {
    let dr = a.r as i32 - b.r as i32;
    let dg = a.g as i32 - b.g as i32;
    let db = a.b as i32 - b.b as i32;
    (dr * dr + dg * dg + db * db) as u32
}

fn is_glyph_ink(color: Rgba) -> bool {
    let luminance = color.r as u16 + color.g as u16 + color.b as u16;
    luminance >= 384
}

fn key_bytes(keycode: u8, state: u16) -> Option<Cow<'static, [u8]>> {
    let shift = state & 1 != 0;
    let ctrl = state & 4 != 0;
    let text = match (keycode, shift) {
        (9, _) => "\x1b",
        (22, _) => "\x7f",
        (23, _) => "\t",
        (36, _) => "\r",
        (111, _) => "\x1b[A",
        (116, _) => "\x1b[B",
        (113, _) => "\x1b[D",
        (114, _) => "\x1b[C",
        (110, _) => "\x1b[H",
        (115, _) => "\x1b[F",
        (112, _) => "\x1b[5~",
        (117, _) => "\x1b[6~",
        (24, false) => "q",
        (24, true) => "Q",
        (25, false) => "w",
        (25, true) => "W",
        (26, false) => "e",
        (26, true) => "E",
        (27, false) => "r",
        (27, true) => "R",
        (28, false) => "t",
        (28, true) => "T",
        (29, false) => "y",
        (29, true) => "Y",
        (30, false) => "u",
        (30, true) => "U",
        (31, false) => "i",
        (31, true) => "I",
        (32, false) => "o",
        (32, true) => "O",
        (33, false) => "p",
        (33, true) => "P",
        (38, false) => "a",
        (38, true) => "A",
        (39, false) => "s",
        (39, true) => "S",
        (40, false) => "d",
        (40, true) => "D",
        (41, false) => "f",
        (41, true) => "F",
        (42, false) => "g",
        (42, true) => "G",
        (43, false) => "h",
        (43, true) => "H",
        (44, false) => "j",
        (44, true) => "J",
        (45, false) => "k",
        (45, true) => "K",
        (46, false) => "l",
        (46, true) => "L",
        (52, false) => "z",
        (52, true) => "Z",
        (53, false) => "x",
        (53, true) => "X",
        (54, false) => "c",
        (54, true) => "C",
        (55, false) => "v",
        (55, true) => "V",
        (56, false) => "b",
        (56, true) => "B",
        (57, false) => "n",
        (57, true) => "N",
        (58, false) => "m",
        (58, true) => "M",
        (65, _) => " ",
        (10, false) => "1",
        (10, true) => "!",
        (11, false) => "2",
        (11, true) => "@",
        (12, false) => "3",
        (12, true) => "#",
        (13, false) => "4",
        (13, true) => "$",
        (14, false) => "5",
        (14, true) => "%",
        (15, false) => "6",
        (15, true) => "^",
        (16, false) => "7",
        (16, true) => "&",
        (17, false) => "8",
        (17, true) => "*",
        (18, false) => "9",
        (18, true) => "(",
        (19, false) => "0",
        (19, true) => ")",
        (20, false) => "-",
        (20, true) => "_",
        (21, false) => "=",
        (21, true) => "+",
        (34, false) => "[",
        (34, true) => "{",
        (35, false) => "]",
        (35, true) => "}",
        (47, false) => ";",
        (47, true) => ":",
        (48, false) => "'",
        (48, true) => "\"",
        (49, false) => "`",
        (49, true) => "~",
        (51, false) => "\\",
        (51, true) => "|",
        (59, false) => ",",
        (59, true) => "<",
        (60, false) => ".",
        (60, true) => ">",
        (61, false) => "/",
        (61, true) => "?",
        _ => return None,
    };

    if ctrl && text.len() == 1 {
        let b = text.as_bytes()[0].to_ascii_lowercase();
        if b.is_ascii_lowercase() {
            return Some(Cow::Owned(vec![b - b'a' + 1]));
        }
    }

    Some(Cow::Borrowed(text.as_bytes()))
}

fn drain_ui_events(
    rx: &Receiver<Event>,
    pty_tx: &EventLoopSender,
    window_size: WindowSize,
) -> bool {
    let mut running = true;
    while let Ok(event) = rx.try_recv() {
        match event {
            Event::Exit | Event::ChildExit(_) => running = false,
            Event::PtyWrite(text) => {
                let _ = pty_tx.send(Msg::Input(Cow::Owned(text.into_bytes())));
            }
            Event::TextAreaSizeRequest(formatter) => {
                let text = formatter(window_size);
                let _ = pty_tx.send(Msg::Input(Cow::Owned(text.into_bytes())));
            }
            _ => {}
        }
    }
    running
}

fn button_at(x: i16, y: i16) -> Option<usize> {
    let x = x.max(0) as usize / BG_SCALE;
    let y = y.max(0) as usize / BG_SCALE;
    BUTTON_TARGETS.iter().position(|button| {
        let button_x = button.x - ART_CROP_X;
        let button_y = button.y - ART_CROP_Y;
        x >= button_x && x < button_x + BUTTON_W && y >= button_y && y < button_y + BUTTON_H
    })
}

fn key_scroll(keycode: u8, state: u16) -> Option<Scroll> {
    let shift = state & 1 != 0;
    if !shift {
        return None;
    }

    match keycode {
        110 => Some(Scroll::Top),
        112 => Some(Scroll::PageUp),
        115 => Some(Scroll::Bottom),
        117 => Some(Scroll::PageDown),
        _ => None,
    }
}

fn scroll_display(term: &Arc<FairMutex<Term<UiEventProxy>>>, scroll: Scroll) {
    term.lock().scroll_display(scroll);
}

fn main() -> Result<(), Box<dyn Error>> {
    tty::setup_env();

    let palette = Palette::load(PALETTE_PATH)?;
    let mode_images = ModeImages::load(&palette)?;
    let atlas = GlyphAtlas::load()?;
    let button_sprites = Image::load(BUTTON_SPRITES_PATH, &palette)?;
    let width = mode_images.green_low_contrast.width * BG_SCALE;
    let height = mode_images.green_low_contrast.height * BG_SCALE;
    let mut settings = DisplaySettings::new();
    let mut fb = Framebuffer::new(width, height, palette.color(COLOR_CURSOR));
    let xwin = XWindow::open(width, height)?;

    let cell_w = (GLYPH_W * GLYPH_SCALE) as u16;
    let cell_h = (GLYPH_H * GLYPH_SCALE) as u16;
    let size = TermSize {
        columns: SCREEN_W / (cell_w as usize) - 1,
        lines: SCREEN_H / (cell_h as usize),
    };
    let window_size = WindowSize {
        num_lines: size.lines as u16,
        num_cols: size.columns as u16,
        cell_width: cell_w,
        cell_height: cell_h,
    };

    let (ui_tx, ui_rx) = mpsc::channel();
    let proxy = UiEventProxy(ui_tx);
    let config = Config {
        scrolling_history: 10_000,
        ..Config::default()
    };
    let term = Arc::new(FairMutex::new(Term::new(config, &size, proxy.clone())));
    let pty = tty::new(&tty::Options::default(), window_size, xwin.window as u64)?;
    let event_loop = EventLoop::new(term.clone(), proxy, pty, true, false)?;
    let pty_tx = event_loop.channel();
    let event_thread = event_loop.spawn();

    let mut active_button = None;

    render(
        &mut fb,
        &mode_images,
        &atlas,
        &button_sprites,
        active_button,
        settings,
        &palette,
        &term,
    );
    xwin.draw(&fb)?;

    let mut last_draw = Instant::now();
    let mut running = true;
    while running {
        running = drain_ui_events(&ui_rx, &pty_tx, window_size);

        while let Some(event) = xwin.conn.poll_for_event()? {
            match event {
                XEvent::Expose(_) => {
                    render(
                        &mut fb,
                        &mode_images,
                        &atlas,
                        &button_sprites,
                        active_button,
                        settings,
                        &palette,
                        &term,
                    );
                    xwin.draw(&fb)?;
                }
                XEvent::KeyPress(event) => {
                    if let Some(scroll) = key_scroll(event.detail, event.state.into()) {
                        scroll_display(&term, scroll);
                    } else if let Some(bytes) = key_bytes(event.detail, event.state.into()) {
                        scroll_display(&term, Scroll::Bottom);
                        let _ = pty_tx.send(Msg::Input(bytes));
                    }
                }
                XEvent::ButtonPress(event) => match event.detail {
                    WHEEL_UP => {
                        scroll_display(&term, Scroll::Delta(SCROLL_LINES));
                        render(
                            &mut fb,
                            &mode_images,
                            &atlas,
                            &button_sprites,
                            active_button,
                            settings,
                            &palette,
                            &term,
                        );
                        xwin.draw(&fb)?;
                    }
                    WHEEL_DOWN => {
                        scroll_display(&term, Scroll::Delta(-SCROLL_LINES));
                        render(
                            &mut fb,
                            &mode_images,
                            &atlas,
                            &button_sprites,
                            active_button,
                            settings,
                            &palette,
                            &term,
                        );
                        xwin.draw(&fb)?;
                    }
                    detail if detail == ButtonIndex::M1.into() => {
                        active_button = button_at(event.event_x, event.event_y);
                        render(
                            &mut fb,
                            &mode_images,
                            &atlas,
                            &button_sprites,
                            active_button,
                            settings,
                            &palette,
                            &term,
                        );
                        xwin.draw(&fb)?;
                    }
                    _ => {}
                },
                XEvent::ButtonRelease(event) => {
                    if event.detail == ButtonIndex::M1.into() {
                        let released_button = button_at(event.event_x, event.event_y);
                        if let (Some(pressed), Some(released)) = (active_button, released_button) {
                            if pressed == released {
                                settings.toggle(BUTTON_TARGETS[pressed].action);
                            }
                        }
                        active_button = None;
                        render(
                            &mut fb,
                            &mode_images,
                            &atlas,
                            &button_sprites,
                            active_button,
                            settings,
                            &palette,
                            &term,
                        );
                        xwin.draw(&fb)?;
                    }
                }
                XEvent::DestroyNotify(_) => running = false,
                _ => {}
            }
        }

        if last_draw.elapsed() >= Duration::from_millis(16) {
            render(
                &mut fb,
                &mode_images,
                &atlas,
                &button_sprites,
                active_button,
                settings,
                &palette,
                &term,
            );
            xwin.draw(&fb)?;
            last_draw = Instant::now();
        }

        std::thread::sleep(Duration::from_millis(4));
    }

    let _ = pty_tx.send(Msg::Shutdown);
    let _ = event_thread.join();
    Ok(())
}
