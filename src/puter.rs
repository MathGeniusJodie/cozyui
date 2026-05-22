use std::borrow::Cow;
use std::error::Error;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, EventLoopSender, Msg, State};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Term, point_to_viewport};
use alacritty_terminal::tty;

use crate::palette_color;
use crate::text_input::printable_key;
use crate::{Framebuffer, Image, Palette, Rgba, decode_png_with_size};

const BG_SCALE: usize = 1;
const GLYPH_SCALE: usize = 1;
const GLYPH_W: usize = 6;
const GLYPH_H: usize = 12;

const SCREEN_SOURCE_X: usize = 49;
const SCREEN_SOURCE_Y: usize = 49;
const SCREEN_W: usize = 205 * BG_SCALE;
const SCREEN_H: usize = 158 * BG_SCALE;

const GREEN_LOW_CONTRAST_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/puter_g_lc.png");
const ORANGE_LOW_CONTRAST_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/puter_o_lc.png");
const HIGH_CONTRAST_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/puter_hc.png");
const ART_CROP_X: usize = 19;
const ART_CROP_Y: usize = 17;

const COLOR_CURSOR: usize = palette_color::LAVENDER;
const COLOR_LIGHT_OFF: usize = palette_color::GUNMETAL;
const COLOR_LIGHT_OFF_CORE: usize = palette_color::PLUM;
const COLOR_LIGHT_OFF_TOP: usize = palette_color::PEACH;
const COLOR_LIGHT_RED: usize = palette_color::CRIMSON;
const COLOR_LIGHT_RED_CORE: usize = palette_color::ORANGE;
const COLOR_LIGHT_RED_TOP: usize = palette_color::ORANGE;
const COLOR_LIGHT_GREEN: usize = palette_color::GREEN;
const COLOR_LIGHT_GREEN_CORE: usize = palette_color::LIME;
const COLOR_LIGHT_GREEN_TOP: usize = palette_color::LIME;
const COLOR_ORANGE_TEXT: usize = palette_color::ORANGE;
const COLOR_ORANGE_GLOW: usize = palette_color::BROWN;
const COLOR_GREEN_TEXT: usize = palette_color::LIME;
const COLOR_GREEN_GLOW: usize = palette_color::GREEN;

const BUTTON_SPRITES_PATH: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/assets/buttons-pressed.png");
const BUTTON_W: usize = 20;
const BUTTON_H: usize = 16;
const BUTTON_HIT_OFFSET_X: isize = -2;
const BUTTON_PRESSED_OFFSET_X: isize = -3;
const SCROLL_LINES: i32 = 3;
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

pub(crate) struct Puter {
    mode_images: ModeImages,
    button_sprites: Image,
    atlas: GlyphAtlas,
    terminal: Option<Terminal>,
    settings: DisplaySettings,
    active_button: Option<usize>,
}

impl Puter {
    pub(crate) fn load(palette: &Palette) -> Result<Self, Box<dyn Error>> {
        Ok(Self {
            mode_images: ModeImages::load(palette)?,
            button_sprites: Image::load(BUTTON_SPRITES_PATH, palette)?,
            atlas: GlyphAtlas::load()?,
            terminal: None,
            settings: DisplaySettings::new(),
            active_button: None,
        })
    }

    pub(crate) fn width(&self) -> usize {
        self.mode_images.green_low_contrast.width * BG_SCALE
    }

    pub(crate) fn height(&self) -> usize {
        self.mode_images.green_low_contrast.height * BG_SCALE
    }

    pub(crate) fn fill_color(&self, palette: &Palette) -> Rgba {
        palette.color(COLOR_CURSOR)
    }

    pub(crate) fn press_button(&mut self, x: i16, y: i16) {
        self.active_button = button_at(x, y);
    }

    pub(crate) fn release_button(&mut self, x: i16, y: i16) {
        let released_button = button_at(x, y);
        if let (Some(pressed), Some(released)) = (self.active_button, released_button)
            && pressed == released
        {
            self.settings.toggle(BUTTON_TARGETS[pressed].action);
        }
        self.active_button = None;
    }

    pub(crate) fn start_terminal(&mut self, window_id: u64) -> Result<(), Box<dyn Error>> {
        tty::setup_env();
        self.terminal = Some(Terminal::open(window_id)?);
        Ok(())
    }

    pub(crate) fn drain_terminal_events(&self) -> TerminalEvents {
        self.terminal().drain_events()
    }

    pub(crate) fn handle_key_press(&self, keycode: u8, state: u16) {
        self.terminal().handle_key_press(keycode, state);
    }

    pub(crate) fn scroll_up(&self) {
        self.terminal().scroll(Scroll::Delta(SCROLL_LINES));
    }

    pub(crate) fn scroll_down(&self) {
        self.terminal().scroll(Scroll::Delta(-SCROLL_LINES));
    }

    pub(crate) fn shutdown_terminal(&mut self) {
        if let Some(terminal) = self.terminal.take() {
            terminal.shutdown();
        }
    }

    pub(crate) fn render(&self, fb: &mut Framebuffer, palette: &Palette) {
        let term = self.terminal().term();

        fb.clear_scaled(self.mode_images.for_settings(self.settings), BG_SCALE);
        draw_lights(fb, self.settings, palette);

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
            let mut color = self.settings.text_color(palette);
            if cell.flags.contains(Flags::DIM) {
                color = self.settings.glow_color(palette);
            }
            if cell.flags.contains(Flags::BOLD) {
                color = self.settings.text_color(palette);
            }
            if self.settings.high_brightness {
                let glow = self.settings.glow_color(palette);
                draw_glyph(fb, &self.atlas, cell.c, x - 1, y, GLYPH_SCALE, glow);
                draw_glyph(fb, &self.atlas, cell.c, x + 1, y, GLYPH_SCALE, glow);
                draw_glyph(fb, &self.atlas, cell.c, x, y - 1, GLYPH_SCALE, glow);
                draw_glyph(fb, &self.atlas, cell.c, x, y + 1, GLYPH_SCALE, glow);
            }
            draw_glyph(fb, &self.atlas, cell.c, x, y, GLYPH_SCALE, color);
        }

        if let Some(cursor_point) = point_to_viewport(content.display_offset, content.cursor.point)
        {
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

        if let Some(index) = self.active_button {
            let button = BUTTON_TARGETS[index];
            fb.draw_scaled_region(
                &self.button_sprites,
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

    fn terminal(&self) -> &Terminal {
        self.terminal
            .as_ref()
            .expect("puter terminal must be started before use")
    }
}

struct GlyphAtlas {
    width: usize,
    pixels: Vec<bool>,
}

impl GlyphAtlas {
    fn load() -> Result<Self, Box<dyn Error>> {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/glyphs/0000-007F.png");
        let (width, height, pixels) = decode_png_with_size(path)?;
        if width < GLYPH_W {
            return Err(format!("glyph atlas {path} is too narrow for terminal glyphs").into());
        }
        let rows = 128_usize.div_ceil(width / GLYPH_W);
        if height < rows * GLYPH_H {
            return Err(format!("glyph atlas {path} is too small for 128 terminal glyphs").into());
        }
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

impl Drop for Puter {
    fn drop(&mut self) {
        self.shutdown_terminal();
    }
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

type TerminalEventLoop = EventLoop<tty::Pty, UiEventProxy>;
type TerminalThread = JoinHandle<(TerminalEventLoop, State)>;

struct Terminal {
    rx: Receiver<Event>,
    tx: EventLoopSender,
    term: Arc<FairMutex<Term<UiEventProxy>>>,
    window_size: WindowSize,
    event_thread: Option<TerminalThread>,
}

impl Terminal {
    fn open(window_id: u64) -> Result<Self, Box<dyn Error>> {
        let size = TermSize {
            columns: SCREEN_W / (GLYPH_W * GLYPH_SCALE) - 1,
            lines: SCREEN_H / (GLYPH_H * GLYPH_SCALE),
        };
        let window_size = WindowSize {
            num_lines: size.lines as u16,
            num_cols: size.columns as u16,
            cell_width: (GLYPH_W * GLYPH_SCALE) as u16,
            cell_height: (GLYPH_H * GLYPH_SCALE) as u16,
        };

        let (ui_tx, rx) = mpsc::channel();
        let proxy = UiEventProxy(ui_tx);
        let config = Config {
            scrolling_history: 10_000,
            ..Config::default()
        };
        let term = Arc::new(FairMutex::new(Term::new(config, &size, proxy.clone())));
        let pty = tty::new(&tty::Options::default(), window_size, window_id)?;
        let event_loop = EventLoop::new(term.clone(), proxy, pty, true, false)?;
        let tx = event_loop.channel();
        let event_thread = Some(event_loop.spawn());

        Ok(Self {
            rx,
            tx,
            term,
            window_size,
            event_thread,
        })
    }

    fn term(&self) -> &Arc<FairMutex<Term<UiEventProxy>>> {
        &self.term
    }

    fn drain_events(&self) -> TerminalEvents {
        let mut running = true;
        let mut dirty = false;
        while let Ok(event) = self.rx.try_recv() {
            dirty = true;
            match event {
                Event::Exit | Event::ChildExit(_) => running = false,
                Event::PtyWrite(text) => {
                    let _ = self.tx.send(Msg::Input(Cow::Owned(text.into_bytes())));
                }
                Event::TextAreaSizeRequest(formatter) => {
                    let text = formatter(self.window_size);
                    let _ = self.tx.send(Msg::Input(Cow::Owned(text.into_bytes())));
                }
                _ => {}
            }
        }
        TerminalEvents { running, dirty }
    }

    fn handle_key_press(&self, keycode: u8, state: u16) {
        if let Some(scroll) = key_scroll(keycode, state) {
            self.scroll(scroll);
        } else if let Some(bytes) = key_bytes(keycode, state) {
            self.scroll(Scroll::Bottom);
            let _ = self.tx.send(Msg::Input(bytes));
        }
    }

    fn scroll(&self, scroll: Scroll) {
        self.term.lock().scroll_display(scroll);
    }

    fn shutdown(mut self) {
        let _ = self.tx.send(Msg::Shutdown);
        if let Some(event_thread) = self.event_thread.take() {
            let _ = event_thread.join();
        }
    }
}

pub(crate) struct TerminalEvents {
    pub(crate) running: bool,
    pub(crate) dirty: bool,
}

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

    fill_source_rect(fb, light.x, light.y, LIGHT_W, LIGHT_H, shell);
    fill_source_rect(fb, light.x, light.y, LIGHT_W, 1, top);
    fill_source_rect(fb, light.x + 1, light.y + 2, LIGHT_W - 2, LIGHT_H - 3, core);
}

fn fill_source_rect(fb: &mut Framebuffer, x: usize, y: usize, w: usize, h: usize, color: Rgba) {
    fb.fill_rect(
        x * BG_SCALE,
        y * BG_SCALE,
        w * BG_SCALE,
        h * BG_SCALE,
        color,
    );
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
            let dx = x + gx * scale;
            let dy = y + gy * scale;
            fb.fill_rect(dx, dy, scale, scale, color);
        }
    }
}

fn is_glyph_ink(color: Rgba) -> bool {
    let luminance = color.r as u16 + color.g as u16 + color.b as u16;
    luminance >= 384
}

fn button_at(x: i16, y: i16) -> Option<usize> {
    let x = x.max(0) as usize / BG_SCALE;
    let y = y.max(0) as usize / BG_SCALE;
    BUTTON_TARGETS.iter().position(|button| {
        let button_x = (button.x as isize - ART_CROP_X as isize + BUTTON_HIT_OFFSET_X) as usize;
        let button_y = button.y - ART_CROP_Y;
        x >= button_x && x < button_x + BUTTON_W && y >= button_y && y < button_y + BUTTON_H
    })
}

fn art_x(x: usize) -> usize {
    (x - ART_CROP_X) * BG_SCALE
}

fn art_y(y: usize) -> usize {
    (y - ART_CROP_Y) * BG_SCALE
}

fn key_bytes(keycode: u8, state: u16) -> Option<Cow<'static, [u8]>> {
    let ctrl = state & 4 != 0;
    let text: Cow<'static, str> = match keycode {
        9 => Cow::Borrowed("\x1b"),
        22 => Cow::Borrowed("\x7f"),
        23 => Cow::Borrowed("\t"),
        36 => Cow::Borrowed("\r"),
        111 => Cow::Borrowed("\x1b[A"),
        116 => Cow::Borrowed("\x1b[B"),
        113 => Cow::Borrowed("\x1b[D"),
        114 => Cow::Borrowed("\x1b[C"),
        110 => Cow::Borrowed("\x1b[H"),
        115 => Cow::Borrowed("\x1b[F"),
        112 => Cow::Borrowed("\x1b[5~"),
        117 => Cow::Borrowed("\x1b[6~"),
        _ => Cow::Owned(printable_key(keycode, state)?.to_string()),
    };

    if ctrl && text.len() == 1 {
        let b = text.as_bytes()[0].to_ascii_lowercase();
        if b.is_ascii_lowercase() {
            return Some(Cow::Owned(vec![b - b'a' + 1]));
        }
    }

    Some(match text {
        Cow::Borrowed(text) => Cow::Borrowed(text.as_bytes()),
        Cow::Owned(text) => Cow::Owned(text.into_bytes()),
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
