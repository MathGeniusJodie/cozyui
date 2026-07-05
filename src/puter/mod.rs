use std::borrow::Cow;
use std::error::Error;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, EventLoopSender, Msg, State};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::{Config, Term, TermMode, point_to_viewport};
use alacritty_terminal::tty;
use alacritty_terminal::vte::ansi::{Color, NamedColor, Rgb};

use crate::palette_color;
use crate::text::KeyInput;
use crate::{
    CursorKind, Framebuffer, Index, Palette, Rect, Rgb as PaletteRgb, Sprite, TRANSPARENT,
};

mod keys;
use keys::{is_copy_shortcut, key_bytes, key_scroll};

const GLYPH_W: usize = 6;
const GLYPH_H: usize = 12;
const SHIFT_MASK: u16 = 1;

const SCREEN_SOURCE_X: usize = 49;
const SCREEN_SOURCE_Y: usize = 49;
const SCREEN_W: usize = 205;
const SCREEN_H: usize = 158;

const ART_CROP_X: usize = 19;
const ART_CROP_Y: usize = 17;

const COLOR_CURSOR: Index = palette_color::LAVENDER;
const COLOR_LIGHT_OFF: Index = palette_color::GUNMETAL;
const COLOR_LIGHT_OFF_CORE: Index = palette_color::PLUM;
const COLOR_LIGHT_OFF_TOP: Index = palette_color::PEACH;
const COLOR_LIGHT_RED: Index = palette_color::CRIMSON;
const COLOR_LIGHT_RED_CORE: Index = palette_color::ORANGE;
const COLOR_LIGHT_RED_TOP: Index = palette_color::ORANGE;
const COLOR_LIGHT_GREEN: Index = palette_color::GREEN;
const COLOR_LIGHT_GREEN_CORE: Index = palette_color::LIME;
const COLOR_LIGHT_GREEN_TOP: Index = palette_color::LIME;
const COLOR_ORANGE_TEXT: Index = palette_color::ORANGE;
const COLOR_ORANGE_GLOW: Index = palette_color::BROWN;
const COLOR_GREEN_TEXT: Index = palette_color::LIME;
const COLOR_GREEN_GLOW: Index = palette_color::GREEN;
const COLOR_SELECTION: Index = palette_color::GUNMETAL;

const TERM_COLOR_BLACK: Index = palette_color::BLACK;
const TERM_COLOR_RED: Index = palette_color::CRIMSON;
const TERM_COLOR_GREEN: Index = palette_color::GREEN;
const TERM_COLOR_YELLOW: Index = palette_color::ORANGE;
const TERM_COLOR_BLUE: Index = palette_color::BLUE;
const TERM_COLOR_MAGENTA: Index = palette_color::PURPLE;
const TERM_COLOR_CYAN: Index = palette_color::CYAN;
const TERM_COLOR_WHITE: Index = palette_color::CREAM;
const TERM_COLOR_BRIGHT_BLACK: Index = palette_color::GUNMETAL;
const TERM_COLOR_BRIGHT_RED: Index = palette_color::ROSE;
const TERM_COLOR_BRIGHT_GREEN: Index = palette_color::LIME;
const TERM_COLOR_BRIGHT_YELLOW: Index = palette_color::PEACH;
const TERM_COLOR_BRIGHT_BLUE: Index = palette_color::CYAN;
const TERM_COLOR_BRIGHT_MAGENTA: Index = palette_color::LAVENDER;
const TERM_COLOR_BRIGHT_CYAN: Index = palette_color::CYAN;
const TERM_COLOR_BRIGHT_WHITE: Index = palette_color::CREAM;
const TERM_COLOR_DIM_BLACK: Index = palette_color::PLUM;
const TERM_COLOR_DIM_RED: Index = palette_color::BROWN;
const TERM_COLOR_DIM_GREEN: Index = palette_color::PINE;
const TERM_COLOR_DIM_YELLOW: Index = palette_color::BROWN;
const TERM_COLOR_DIM_BLUE: Index = palette_color::PINE;
const TERM_COLOR_DIM_MAGENTA: Index = palette_color::PLUM;
const TERM_COLOR_DIM_CYAN: Index = palette_color::BLUE;
const TERM_COLOR_DIM_WHITE: Index = palette_color::LAVENDER;

const BUTTON_W: usize = 19;
const BUTTON_H: usize = 16;
const BUTTON_SPRITE_OFFSET_X: usize = 3;
const BUTTON_SPRITE_STRIDE: usize = 19;
const ICON_BUTTON_W: usize = 19;
const ICON_BUTTON_H: usize = 18;

const BRIGHTNESS_BUTTON_X: usize = 90;
const COLOR_BUTTON_X: usize = BRIGHTNESS_BUTTON_X + BUTTON_W;
const CONTRAST_BUTTON_X: usize = COLOR_BUTTON_X + BUTTON_W;

const LOCK_BUTTON_X: usize = 214;
const POWER_BUTTON_X: usize = 234;
const MODE_BUTTON_Y: usize = 222;
const ICON_BUTTON_Y: usize = 221;
const CONTROL_CLEAR_X: usize = 119;
const CONTROL_CLEAR_Y: usize = 218;
const CONTROL_CLEAR_W: usize = 119;
const CONTROL_CLEAR_H: usize = 21;
const BUTTON_HIT_OFFSET_X: isize = -2;
const SCROLL_LINES: i32 = 3;
const LIGHT_W: usize = 4;
const LIGHT_H: usize = 5;
const LIGHTS_X: usize = 88;
const LIGHTS: [Light; 3] = [
    Light {
        x: LIGHTS_X,
        y: 223,
        kind: LightKind::Brightness,
    },
    Light {
        x: LIGHTS_X + 9,
        y: 223,
        kind: LightKind::Color,
    },
    Light {
        x: LIGHTS_X + 9 + 9,
        y: 223,
        kind: LightKind::Contrast,
    },
];
const BUTTON_TARGETS: [Button; 5] = [
    Button {
        x: BRIGHTNESS_BUTTON_X,
        y: MODE_BUTTON_Y,
        w: BUTTON_W,
        h: BUTTON_H,
        pressed_sprite: Some(0),
        action: ButtonAction::Brightness,
    },
    Button {
        x: COLOR_BUTTON_X,
        y: MODE_BUTTON_Y,
        w: BUTTON_W,
        h: BUTTON_H,
        pressed_sprite: Some(1),
        action: ButtonAction::Color,
    },
    Button {
        x: CONTRAST_BUTTON_X,
        y: MODE_BUTTON_Y,
        w: BUTTON_W,
        h: BUTTON_H,
        pressed_sprite: Some(2),
        action: ButtonAction::Contrast,
    },
    Button {
        x: POWER_BUTTON_X,
        y: ICON_BUTTON_Y,
        w: ICON_BUTTON_W,
        h: ICON_BUTTON_H,
        pressed_sprite: None,
        action: ButtonAction::Power,
    },
    Button {
        x: LOCK_BUTTON_X,
        y: ICON_BUTTON_Y,
        w: ICON_BUTTON_W,
        h: ICON_BUTTON_H,
        pressed_sprite: None,
        action: ButtonAction::Lock,
    },
];

pub struct Puter {
    mode_images: ModeImages,
    button_sprites: Sprite,
    button_pressed_sprites: Sprite,
    power_button: Sprite,
    lock_button: Sprite,
    atlas: GlyphAtlas,
    terminal: Option<Terminal>,
    settings: DisplaySettings,
    active_button: Option<usize>,
    /// Set true only when a mouse-down was actually forwarded to the pty
    /// terminal (via `term.mouse_press`'s SGR-mouse-mode branch), so
    /// `release_button` knows whether it owes the terminal a matching
    /// mouse-up escape.
    mouse_down_forwarded: bool,
    selection_point: Option<Point>,
    /// `render_chrome`'s output, cached because it's static apart from the
    /// `DisplaySettings` it was last drawn with. Rebuilt (via interior
    /// mutability, since `render` only takes `&self`) whenever `settings`
    /// changes; blitted as-is otherwise.
    chrome_cache: std::cell::RefCell<Option<(DisplaySettings, Framebuffer)>>,
}

impl Puter {
    pub(crate) fn load(_palette: &Palette) -> Result<Self, Box<dyn Error>> {
        Ok(Self {
            mode_images: ModeImages::load(),
            button_sprites: crate::assets::buttons(),
            button_pressed_sprites: crate::assets::buttons_pressed(),
            power_button: crate::assets::puter_power(),
            lock_button: crate::assets::puter_lock(),
            atlas: GlyphAtlas::load(),
            terminal: None,
            settings: DisplaySettings::new(),
            active_button: None,
            mouse_down_forwarded: false,
            selection_point: None,
            chrome_cache: std::cell::RefCell::new(None),
        })
    }

    pub(crate) const fn width(&self) -> usize {
        self.mode_images.green_low_contrast.width
    }

    pub(crate) const fn height(&self) -> usize {
        self.mode_images.green_low_contrast.height
    }

    #[allow(clippy::unused_self)]
    pub(crate) const fn fill_color(&self, _palette: &Palette) -> Index {
        TRANSPARENT
    }

    pub(crate) fn press_button(&mut self, x: i16, y: i16, state: u16) {
        self.active_button = button_at(x, y);
        self.selection_point = None;
        self.mouse_down_forwarded = false;
        if self.active_button.is_some() {
            return;
        }

        let (selection_point, forwarded) = self
            .terminal()
            .map_or((None, false), |term| term.mouse_press(x, y, state));
        self.selection_point = selection_point;
        self.mouse_down_forwarded = forwarded;
    }

    /// Returns true when the power button was clicked and the app should quit.
    pub(crate) fn release_button(&mut self, x: i16, y: i16) -> bool {
        let mut quit = false;
        let released_button = button_at(x, y);
        if let (Some(pressed), Some(released)) = (self.active_button, released_button)
            && pressed == released
        {
            match BUTTON_TARGETS[pressed].action {
                ButtonAction::Power => quit = true,
                ButtonAction::Lock => {
                    if let Err(err) =
                        crate::util::spawn_and_reap(&mut std::process::Command::new("xflock4"))
                    {
                        eprintln!("puter: failed to spawn xflock4: {err}");
                    }
                }
                action => self.settings.toggle(action),
            }
        }
        self.active_button = None;
        if let Some(term) = self.terminal() {
            if self.selection_point.is_some() {
                term.selection_to_clipboard();
            } else if self.mouse_down_forwarded {
                // Only balance the pty's mouse-down with a mouse-up if we
                // actually forwarded a mouse-down in the first place (e.g.
                // not when the press landed on a chrome button).
                term.mouse_release(x, y);
            }
        }
        self.mouse_down_forwarded = false;
        self.selection_point = None;
        quit
    }

    /// Hand over the front-panel buttons, text over the terminal screen.
    pub(crate) fn cursor_at(&self, x: i16, y: i16) -> CursorKind {
        if button_at(x, y).is_some() {
            return CursorKind::Hand;
        }
        if x >= 0 && y >= 0 {
            let x = x as usize;
            let y = y as usize;
            let screen_x = art_x(SCREEN_SOURCE_X);
            let screen_y = art_y(SCREEN_SOURCE_Y);
            if x >= screen_x && x < screen_x + SCREEN_W && y >= screen_y && y < screen_y + SCREEN_H
            {
                return CursorKind::Text;
            }
        }
        CursorKind::Pointer
    }

    pub(crate) fn motion(&mut self, x: i16, y: i16) -> bool {
        if self.selection_point.is_none() {
            return false;
        }

        let Some(point) = self.terminal().and_then(|term| term.screen_point(x, y)) else {
            return false;
        };
        if self.selection_point == Some(point) {
            return false;
        }

        self.selection_point = Some(point);
        self.terminal().is_some_and(|term| term.mouse_motion(point))
    }

    pub(crate) fn start_terminal(&mut self, window_id: u64) -> Result<(), Box<dyn Error>> {
        tty::setup_env();
        self.terminal = Some(Terminal::open(window_id)?);
        Ok(())
    }

    pub(crate) fn drain_terminal_events(&self) -> TerminalEvents {
        self.terminal().map_or(
            TerminalEvents {
                running: true,
                dirty: false,
            },
            Terminal::drain_events,
        )
    }

    pub(crate) fn handle_key_press(
        &self,
        input: &KeyInput,
        clipboard_text: Option<&str>,
    ) -> Option<String> {
        self.terminal()?.handle_key_press(input, clipboard_text)
    }

    pub(crate) fn scroll_up(&self) {
        if let Some(term) = self.terminal() {
            term.scroll(Scroll::Delta(SCROLL_LINES));
        }
    }

    pub(crate) fn scroll_down(&self) {
        if let Some(term) = self.terminal() {
            term.scroll(Scroll::Delta(-SCROLL_LINES));
        }
    }

    pub(crate) fn shutdown_terminal(&mut self) {
        if let Some(terminal) = self.terminal.take() {
            terminal.shutdown();
        }
    }

    #[allow(clippy::significant_drop_tightening)]
    pub(crate) fn render(&self, fb: &mut Framebuffer, palette: &Palette) {
        self.blit_chrome(fb, palette);

        let Some(term) = self.terminal() else {
            return;
        };
        let term = term.term();

        let cell_w = GLYPH_W;
        let cell_h = GLYPH_H;
        let term = term.lock();
        let content = term.renderable_content();

        for indexed in content.display_iter {
            let Some(point) = point_to_viewport(content.display_offset, indexed.point) else {
                continue;
            };
            let cell = indexed.cell;
            if cell
                .flags
                .intersects(Flags::WIDE_CHAR_SPACER | Flags::HIDDEN)
            {
                continue;
            }
            let x = art_x(SCREEN_SOURCE_X) + point.column.0 * cell_w;
            let y = art_y(SCREEN_SOURCE_Y) + point.line * cell_h;
            let selected = content
                .selection
                .as_ref()
                .is_some_and(|selection| selection.contains(indexed.point));
            if selected {
                fb.fill_rect(x, y, cell_w, cell_h, COLOR_SELECTION);
            }
            let style = self.cell_style(cell, selected, palette);
            self.draw_cell(fb, cell, style, x, y);
        }

        if let Some(cursor_point) = point_to_viewport(content.display_offset, content.cursor.point)
        {
            let cursor_x = art_x(SCREEN_SOURCE_X) + cursor_point.column.0 * cell_w;
            let cursor_y = art_y(SCREEN_SOURCE_Y) + cursor_point.line * cell_h;
            fb.fill_rect(cursor_x, cursor_y, cell_w, cell_h, COLOR_CURSOR);
        }

        if let Some(index) = self.active_button {
            let button = BUTTON_TARGETS[index];
            if let Some(sprite_index) = button.pressed_sprite {
                fb.draw_sprite_region(
                    &self.button_pressed_sprites,
                    Rect::new(
                        BUTTON_SPRITE_OFFSET_X + sprite_index * BUTTON_SPRITE_STRIDE,
                        0,
                        BUTTON_W,
                        BUTTON_H,
                    ),
                    art_x(button.x) as isize,
                    art_y(button.y) as isize,
                    palette,
                );
            }
        }
    }

    /// Blit the cached chrome (rebuilding it first if `self.settings` has
    /// changed since it was last built) onto `fb`. `render_chrome` reads
    /// only `self.settings`, so that's the only thing that can invalidate
    /// the cache; everything else it draws (case art, control strip, mode
    /// buttons, lights, power/lock icons) is fully determined by it.
    fn blit_chrome(&self, fb: &mut Framebuffer, palette: &Palette) {
        let stale = self
            .chrome_cache
            .borrow()
            .as_ref()
            .is_none_or(|(cached_settings, _)| *cached_settings != self.settings);
        if stale {
            let mut chrome_fb = Framebuffer::new(fb.width, fb.height, TRANSPARENT);
            self.render_chrome(&mut chrome_fb, palette);
            *self.chrome_cache.borrow_mut() = Some((self.settings, chrome_fb));
        }
        let cache = self.chrome_cache.borrow();
        let (_, chrome_fb) = cache
            .as_ref()
            .expect("just populated above if it was stale");
        fb.blit_from(chrome_fb, 0, 0);
    }

    /// The static dressing around the screen: case art, control strip, mode
    /// buttons, lights, and the power/lock buttons.
    fn render_chrome(&self, fb: &mut Framebuffer, palette: &Palette) {
        fb.fill_from_sprite(self.mode_images.for_settings(self.settings), palette);
        fb.fill_rect(
            art_x(CONTROL_CLEAR_X),
            art_y(CONTROL_CLEAR_Y),
            CONTROL_CLEAR_W,
            CONTROL_CLEAR_H,
            palette_color::CREAM,
        );
        draw_mode_buttons(fb, &self.button_sprites, palette);
        draw_lights(fb, self.settings);
        fb.draw_sprite(
            &self.power_button,
            art_x(POWER_BUTTON_X) as isize,
            art_y(ICON_BUTTON_Y) as isize,
            palette,
        );
        fb.draw_sprite(
            &self.lock_button,
            art_x(LOCK_BUTTON_X) as isize,
            art_y(ICON_BUTTON_Y) as isize,
            palette,
        );
    }

    /// Resolve a cell's colors through the DIM, INVERSE, selection, and
    /// high-brightness layers, in that order.
    #[allow(clippy::similar_names)]
    fn cell_style(&self, cell: &Cell, selected: bool, palette: &Palette) -> CellStyle {
        let mut fg_color = cell.fg;
        let mut fg = self.terminal_color(fg_color, palette);
        let mut bg = self.terminal_background_color(cell.bg, palette);
        let mut glow = Some(self.terminal_glow_color(fg_color, palette));
        if cell.flags.contains(Flags::DIM) {
            let dim_fg = dim_color(cell.fg);
            fg_color = dim_fg;
            fg = self.terminal_color(fg_color, palette);
            glow = Some(self.terminal_glow_color(fg_color, palette));
            bg = (cell.bg != Color::Named(NamedColor::Background))
                .then(|| self.terminal_color(dim_color(cell.bg), palette));
        }
        if cell.flags.contains(Flags::INVERSE) {
            let inverse_bg = fg;
            let inverse_fg = if cell.bg == Color::Named(NamedColor::Background) {
                Color::Named(NamedColor::Foreground)
            } else {
                cell.bg
            };
            fg_color = inverse_fg;
            fg = bg.unwrap_or_else(|| self.background_terminal_bg_color(palette));
            glow = Some(self.terminal_glow_color(fg_color, palette));
            bg = Some(inverse_bg);
        }
        if selected {
            fg = palette_color::CREAM;
            bg = Some(COLOR_SELECTION);
            glow = Some(COLOR_SELECTION);
        } else if self.settings.high_brightness {
            let style = self.high_brightness_terminal_style(fg_color, palette);
            fg = style.fg;
            glow = style.glow;
        }
        CellStyle { fg, bg, glow }
    }

    /// Paint one cell at framebuffer position (x, y): background, glow halo,
    /// glyph, and the bold/underline/strikeout decorations.
    #[allow(clippy::needless_pass_by_value)]
    fn draw_cell(&self, fb: &mut Framebuffer, cell: &Cell, style: CellStyle, x: usize, y: usize) {
        let cell_w = GLYPH_W;
        let cell_h = GLYPH_H;
        if let Some(bg) = style.bg {
            fb.fill_rect(x, y, cell_w, cell_h, bg);
        }
        if cell.c == ' ' {
            return;
        }
        if self.settings.high_brightness
            && let Some(glow) = style.glow
        {
            draw_glyph(fb, &self.atlas, cell.c, x - 1, y, glow);
            draw_glyph(fb, &self.atlas, cell.c, x + 1, y, glow);
            draw_glyph(fb, &self.atlas, cell.c, x, y - 1, glow);
            draw_glyph(fb, &self.atlas, cell.c, x, y + 1, glow);
        }
        let fg = style.fg;
        draw_glyph(fb, &self.atlas, cell.c, x, y, fg);
        if cell.flags.contains(Flags::BOLD) {
            draw_glyph(fb, &self.atlas, cell.c, x + 1, y, fg);
        }
        if cell.flags.intersects(Flags::ALL_UNDERLINES) {
            fb.fill_rect(x, y + cell_h - 1, cell_w, 1, fg);
            if cell.flags.contains(Flags::DOUBLE_UNDERLINE) && cell_h > 2 {
                fb.fill_rect(x, y + cell_h - 3, cell_w, 1, fg);
            }
        }
        if cell.flags.contains(Flags::STRIKEOUT) {
            fb.fill_rect(x, y + cell_h / 2, cell_w, 1, fg);
        }
    }

    /// The active terminal, or `None` if it hasn't been started yet. Call
    /// sites degrade gracefully rather than panic.
    const fn terminal(&self) -> Option<&Terminal> {
        self.terminal.as_ref()
    }

    fn terminal_color(&self, color: Color, palette: &Palette) -> Index {
        match color {
            Color::Named(NamedColor::Foreground | NamedColor::BrightForeground) => {
                self.background_terminal_text_color(palette)
            }
            Color::Named(NamedColor::DimForeground) => self.settings.glow_color(palette),
            Color::Named(NamedColor::Background) => self.background_terminal_bg_color(palette),
            Color::Named(NamedColor::Cursor) => COLOR_CURSOR,
            Color::Named(named) => named_terminal_palette_index(named),
            Color::Indexed(index) => indexed_terminal_color(index, palette),
            Color::Spec(rgb) => palette.nearest_index(terminal_rgb(rgb)),
        }
    }

    fn terminal_background_color(&self, color: Color, palette: &Palette) -> Option<Index> {
        (color != Color::Named(NamedColor::Background)).then(|| self.terminal_color(color, palette))
    }

    fn terminal_glow_color(&self, color: Color, palette: &Palette) -> Index {
        match color {
            Color::Named(NamedColor::Foreground | NamedColor::BrightForeground) => {
                self.settings.glow_color(palette)
            }
            Color::Named(NamedColor::DimForeground) => TERM_COLOR_DIM_WHITE,
            Color::Named(NamedColor::Background) => self.settings.glow_color(palette),
            Color::Named(NamedColor::Cursor) => COLOR_CURSOR,
            Color::Named(named) => named_terminal_glow_palette_index(named),
            Color::Indexed(index) => indexed_terminal_glow_color(index, palette),
            Color::Spec(rgb) => palette.nearest_index(terminal_rgb(rgb)),
        }
    }

    fn high_brightness_terminal_style(&self, color: Color, palette: &Palette) -> TerminalTextStyle {
        match color {
            Color::Named(named) if normal_terminal_named_color(named).is_some() => {
                TerminalTextStyle {
                    fg: (named_terminal_palette_index(named.to_bright())),
                    glow: None,
                }
            }
            Color::Indexed(index @ 0..=7) => TerminalTextStyle {
                fg: (ANSI_16_TO_NA16[index as usize + 8]),
                glow: None,
            },
            Color::Named(named) if bright_terminal_named_color(named).is_some() => {
                TerminalTextStyle {
                    fg: (palette_color::CREAM),
                    glow: Some(named_terminal_glow_palette_index(named)),
                }
            }
            Color::Indexed(index @ 8..=15) => TerminalTextStyle {
                fg: (palette_color::CREAM),
                glow: Some(ANSI_16_GLOW_TO_NA16[index as usize]),
            },
            Color::Named(NamedColor::Foreground | NamedColor::BrightForeground) => {
                TerminalTextStyle {
                    fg: self.background_terminal_text_color(palette),
                    glow: Some(self.settings.glow_color(palette)),
                }
            }
            _ => TerminalTextStyle {
                fg: self.terminal_color(color, palette),
                glow: None,
            },
        }
    }

    fn background_terminal_text_color(&self, palette: &Palette) -> Index {
        self.settings.text_color(palette)
    }

    #[allow(clippy::unused_self)]
    const fn background_terminal_bg_color(&self, _palette: &Palette) -> Index {
        palette_color::BLACK
    }
}

impl crate::widget::Widget for Puter {
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
        state: u16,
    ) -> Result<crate::widget::ClickOutcome, Box<dyn Error>> {
        self.press_button(x, y, state);
        Ok(crate::widget::ClickOutcome::default())
    }

    fn motion(&mut self, x: i16, y: i16) -> bool {
        Self::motion(self, x, y)
    }

    fn scroll(&mut self, _x: i16, _y: i16, direction: crate::widget::ScrollDirection) -> bool {
        match direction {
            crate::widget::ScrollDirection::Up => self.scroll_up(),
            crate::widget::ScrollDirection::Down => self.scroll_down(),
        }
        true
    }

    fn cursor_at(&self, x: i16, y: i16) -> CursorKind {
        self.cursor_at(x, y)
    }

    fn handle_key_press(
        &mut self,
        input: &KeyInput,
        clipboard_text: Option<&str>,
    ) -> Result<Option<String>, Box<dyn Error>> {
        Ok(Self::handle_key_press(self, input, clipboard_text))
    }

    fn wants_clipboard(&self, input: &KeyInput) -> bool {
        input.is_paste_shortcut()
    }
}

struct TerminalTextStyle {
    fg: Index,
    glow: Option<Index>,
}

/// Final resolved colors for one terminal cell.
struct CellStyle {
    fg: Index,
    bg: Option<Index>,
    glow: Option<Index>,
}

struct GlyphAtlas {
    width: usize,
    pixels: Vec<bool>,
}

impl GlyphAtlas {
    /// Size checks and the ink threshold happen at build time (build.rs),
    /// which bakes the atlas to a 0/1 mask.
    fn load() -> Self {
        Self {
            width: crate::assets::GLYPH_ATLAS_WIDTH,
            pixels: crate::assets::GLYPH_ATLAS_MASK
                .iter()
                .map(|&b| b != 0)
                .collect(),
        }
    }

    fn is_on(&self, ch: char, x: usize, y: usize) -> bool {
        let code = ch as usize;
        if code >= 128 {
            return self.is_on('?', x, y);
        }

        let cols = self.width / GLYPH_W;
        let sx = (code % cols) * GLYPH_W + x;
        let sy = (code / cols) * GLYPH_H + y;
        // `ch` comes from arbitrary pty output; degrade to blank rather than
        // panic if the baked atlas ever disagrees with the sizing constants.
        self.pixels
            .get(sy * self.width + sx)
            .copied()
            .unwrap_or(false)
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
    clipboard: FairMutex<String>,
    /// Set after the first failed pty write so we only diagnose it once.
    pty_send_failed: crate::util::FailureLog,
}

impl Terminal {
    fn open(window_id: u64) -> Result<Self, Box<dyn Error>> {
        // One column is kept clear at the screen's right edge so bold/glow
        // overdraw (glyphs redrawn at x+1) stays inside the CRT screen art.
        const COLUMN_MARGIN: usize = 1;
        let size = TermSize {
            columns: SCREEN_W / GLYPH_W - COLUMN_MARGIN,
            lines: SCREEN_H / GLYPH_H,
        };
        let window_size = WindowSize {
            num_lines: size.lines as u16,
            num_cols: size.columns as u16,
            cell_width: GLYPH_W as u16,
            cell_height: GLYPH_H as u16,
        };

        let (ui_tx, rx) = mpsc::channel();
        let proxy = UiEventProxy(ui_tx);
        let config = Config {
            scrolling_history: 10_000,
            ..Config::default()
        };
        let term = Arc::new(FairMutex::new(Term::new(config, &size, proxy.clone())));
        // Run the shell inside a persistent abduco session so the terminal
        // survives cozyui restarts; -A reattaches if "puter" already exists.
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
        let options = tty::Options {
            shell: Some(tty::Shell::new(
                "abduco".to_string(),
                vec!["-A".to_string(), "puter".to_string(), shell],
            )),
            ..tty::Options::default()
        };
        let pty = tty::new(&options, window_size, window_id)?;
        let event_loop = EventLoop::new(term.clone(), proxy, pty, true, false)?;
        let tx = event_loop.channel();
        let event_thread = Some(event_loop.spawn());

        Ok(Self {
            rx,
            tx,
            pty_send_failed: crate::util::FailureLog::new(),
            term,
            window_size,
            event_thread,
            clipboard: FairMutex::new(String::new()),
        })
    }

    const fn term(&self) -> &Arc<FairMutex<Term<UiEventProxy>>> {
        &self.term
    }

    /// Send a message to the pty event loop, diagnosing (once) if it fails.
    fn send_pty(&self, msg: Msg) {
        if self.tx.send(msg).is_err() {
            self.pty_send_failed.record_err(|| {
                "puter: failed to write to pty (further failures will be suppressed)".to_string()
            });
        }
    }

    fn drain_events(&self) -> TerminalEvents {
        let mut running = true;
        let mut dirty = false;
        while let Ok(event) = self.rx.try_recv() {
            dirty = true;
            match event {
                Event::Exit | Event::ChildExit(_) => running = false,
                Event::PtyWrite(text) => {
                    self.send_pty(Msg::Input(Cow::Owned(text.into_bytes())));
                }
                Event::TextAreaSizeRequest(formatter) => {
                    let text = formatter(self.window_size);
                    self.send_pty(Msg::Input(Cow::Owned(text.into_bytes())));
                }
                _ => {}
            }
        }
        TerminalEvents { running, dirty }
    }

    fn handle_key_press(&self, input: &KeyInput, clipboard_text: Option<&str>) -> Option<String> {
        if let Some(scroll) = key_scroll(input) {
            self.scroll(scroll);
            None
        } else if is_copy_shortcut(input) {
            self.selection_to_clipboard()
        } else if input.is_paste_shortcut() {
            let fallback = self.clipboard.lock();
            let text =
                clipboard_text.or_else(|| (!fallback.is_empty()).then_some(fallback.as_str()))?;
            self.scroll(Scroll::Bottom);
            self.send_pty(Msg::Input(Cow::Owned(text.as_bytes().to_vec())));
            None
        } else if let Some(bytes) = key_bytes(input) {
            self.scroll(Scroll::Bottom);
            self.send_pty(Msg::Input(Cow::Owned(bytes.into_bytes())));
            None
        } else {
            None
        }
    }

    /// Returns the selection anchor point (if a text selection was started)
    /// and whether a mouse-down escape was forwarded to the pty. The two are
    /// mutually exclusive: a forwarded mouse-down never starts a selection.
    #[allow(clippy::significant_drop_tightening)]
    fn mouse_press(&self, x: i16, y: i16, state: u16) -> (Option<Point>, bool) {
        let Some(point) = screen_point(x, y, &self.window_size) else {
            return (None, false);
        };

        let mouse_mode = self.term.lock().mode().intersects(TermMode::MOUSE_MODE);
        if mouse_mode && state & SHIFT_MASK == 0 {
            self.send_mouse(point, 0, true);
            return (None, true);
        }

        self.scroll(Scroll::Bottom);
        let mut term = self.term.lock();
        term.selection = Some(Selection::new(SelectionType::Simple, point, Side::Left));
        (Some(point), false)
    }

    fn mouse_motion(&self, point: Point) -> bool {
        let mut term = self.term.lock();
        term.selection.as_mut().is_some_and(|selection| {
            selection.update(point, Side::Right);
            true
        })
    }

    fn screen_point(&self, x: i16, y: i16) -> Option<Point> {
        screen_point(x, y, &self.window_size)
    }

    fn mouse_release(&self, x: i16, y: i16) {
        // Clamped, not rejected: the caller only forwards a release here to
        // balance a mouse-down it already forwarded, so this must always
        // send the matching mouse-up even if the drag ended outside the grid.
        let point = clamped_screen_point(x, y, &self.window_size);

        if self.term.lock().mode().intersects(TermMode::MOUSE_MODE) {
            self.send_mouse(point, 0, false);
        }
    }

    fn send_mouse(&self, point: Point, button: usize, pressed: bool) {
        let suffix = if pressed { 'M' } else { 'm' };
        let text = format!(
            "\x1b[<{};{};{}{}",
            button,
            point.column.0 + 1,
            point.line.0 + 1,
            suffix
        );
        self.send_pty(Msg::Input(Cow::Owned(text.into_bytes())));
    }

    fn copy_selection(&self) -> Option<String> {
        self.term.lock().selection_to_string()
    }

    fn selection_to_clipboard(&self) -> Option<String> {
        let fallback = self.clipboard.lock();
        let text = self
            .copy_selection()
            .or_else(|| (!fallback.is_empty()).then_some(fallback.clone()))?;
        drop(fallback);
        self.clipboard.lock().clone_from(&text);
        Some(text)
    }

    fn scroll(&self, scroll: Scroll) {
        self.term.lock().scroll_display(scroll);
    }

    fn shutdown(mut self) {
        self.send_pty(Msg::Shutdown);
        if let Some(event_thread) = self.event_thread.take() {
            let _ = event_thread.join();
        }
    }
}

pub struct TerminalEvents {
    pub(crate) running: bool,
    pub(crate) dirty: bool,
}

#[derive(Clone, Copy)]
struct Button {
    x: usize,
    y: usize,
    w: usize,
    h: usize,
    pressed_sprite: Option<usize>,
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
    Power,
    Brightness,
    Color,
    Contrast,
    Lock,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TextMode {
    Green,
    Orange,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct DisplaySettings {
    high_brightness: bool,
    text_mode: TextMode,
    high_contrast: bool,
}

impl DisplaySettings {
    const fn new() -> Self {
        Self {
            high_brightness: true,
            text_mode: TextMode::Orange,
            high_contrast: false,
        }
    }

    const fn toggle(&mut self, action: ButtonAction) {
        match action {
            ButtonAction::Power | ButtonAction::Lock => {}
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

    #[allow(clippy::trivially_copy_pass_by_ref)]
    fn text_color(&self, palette: &Palette) -> Index {
        if self.high_brightness {
            return palette.closest_to_white_index();
        }

        match self.text_mode {
            TextMode::Green => COLOR_GREEN_TEXT,
            TextMode::Orange => COLOR_ORANGE_TEXT,
        }
    }

    #[allow(clippy::trivially_copy_pass_by_ref)]
    const fn glow_color(&self, _palette: &Palette) -> Index {
        match self.text_mode {
            TextMode::Green => COLOR_GREEN_GLOW,
            TextMode::Orange => COLOR_ORANGE_GLOW,
        }
    }

    #[allow(clippy::trivially_copy_pass_by_ref)]
    const fn light_state(&self, kind: LightKind) -> LightState {
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
    green_low_contrast: Sprite,
    orange_low_contrast: Sprite,
    high_contrast: Sprite,
}

impl ModeImages {
    fn load() -> Self {
        Self {
            green_low_contrast: crate::assets::puter_g_lc(),
            orange_low_contrast: crate::assets::puter_o_lc(),
            high_contrast: crate::assets::puter_hc(),
        }
    }

    const fn for_settings(&self, settings: DisplaySettings) -> &Sprite {
        if settings.high_contrast {
            return &self.high_contrast;
        }

        match settings.text_mode {
            TextMode::Green => &self.green_low_contrast,
            TextMode::Orange => &self.orange_low_contrast,
        }
    }
}

fn draw_mode_buttons(fb: &mut Framebuffer, button_sprites: &Sprite, palette: &Palette) {
    for button in BUTTON_TARGETS {
        let Some(sprite_index) = button.pressed_sprite else {
            continue;
        };

        fb.draw_sprite_region(
            button_sprites,
            Rect::new(
                BUTTON_SPRITE_OFFSET_X + sprite_index * BUTTON_SPRITE_STRIDE,
                0,
                BUTTON_W,
                BUTTON_H,
            ),
            art_x(button.x) as isize,
            art_y(button.y) as isize,
            palette,
        );
    }
}

fn draw_lights(fb: &mut Framebuffer, settings: DisplaySettings) {
    for light in LIGHTS {
        draw_light(fb, light, settings.light_state(light.kind));
    }
}

fn draw_light(fb: &mut Framebuffer, light: Light, state: LightState) {
    let (shell, core, top) = match state {
        LightState::Off => (
            (COLOR_LIGHT_OFF),
            (COLOR_LIGHT_OFF_CORE),
            (COLOR_LIGHT_OFF_TOP),
        ),
        LightState::Red => (
            (COLOR_LIGHT_RED),
            (COLOR_LIGHT_RED_CORE),
            (COLOR_LIGHT_RED_TOP),
        ),
        LightState::Green => (
            (COLOR_LIGHT_GREEN),
            (COLOR_LIGHT_GREEN_CORE),
            (COLOR_LIGHT_GREEN_TOP),
        ),
    };

    fb.fill_rect(light.x, light.y, LIGHT_W, LIGHT_H, shell);
    fb.fill_rect(light.x, light.y, LIGHT_W, 1, top);
    fb.fill_rect(light.x + 1, light.y + 2, LIGHT_W - 2, LIGHT_H - 3, core);
}

/// `NamedColor`'s first 16 discriminants (Black..=BrightWhite) line up
/// exactly with `ANSI_16_TO_NA16` / `ANSI_16_GLOW_TO_NA16`'s element order,
/// so the 0..=15 arms below reuse those arrays instead of hand-matching.
const fn named_terminal_palette_index(color: NamedColor) -> Index {
    match color {
        NamedColor::Foreground | NamedColor::BrightForeground | NamedColor::DimForeground => {
            TERM_COLOR_WHITE
        }
        NamedColor::Background => TERM_COLOR_BLACK,
        NamedColor::Cursor => COLOR_CURSOR,
        NamedColor::DimBlack => TERM_COLOR_DIM_BLACK,
        NamedColor::DimRed => TERM_COLOR_DIM_RED,
        NamedColor::DimGreen => TERM_COLOR_DIM_GREEN,
        NamedColor::DimYellow => TERM_COLOR_DIM_YELLOW,
        NamedColor::DimBlue => TERM_COLOR_DIM_BLUE,
        NamedColor::DimMagenta => TERM_COLOR_DIM_MAGENTA,
        NamedColor::DimCyan => TERM_COLOR_DIM_CYAN,
        NamedColor::DimWhite => TERM_COLOR_DIM_WHITE,
        named => ANSI_16_TO_NA16[named as usize],
    }
}

const fn named_terminal_glow_palette_index(color: NamedColor) -> Index {
    match color {
        NamedColor::Foreground | NamedColor::BrightForeground | NamedColor::DimForeground => {
            TERM_COLOR_DIM_WHITE
        }
        NamedColor::Background => TERM_COLOR_DIM_BLACK,
        NamedColor::Cursor => COLOR_CURSOR,
        NamedColor::DimBlack => TERM_COLOR_DIM_BLACK,
        NamedColor::DimRed => TERM_COLOR_DIM_RED,
        NamedColor::DimGreen => TERM_COLOR_DIM_GREEN,
        NamedColor::DimYellow => TERM_COLOR_DIM_YELLOW,
        NamedColor::DimBlue => TERM_COLOR_DIM_BLUE,
        NamedColor::DimMagenta => TERM_COLOR_DIM_MAGENTA,
        NamedColor::DimCyan => TERM_COLOR_DIM_CYAN,
        NamedColor::DimWhite => TERM_COLOR_DIM_WHITE,
        named => ANSI_16_GLOW_TO_NA16[named as usize],
    }
}

const fn normal_terminal_named_color(color: NamedColor) -> Option<NamedColor> {
    match color {
        NamedColor::Black
        | NamedColor::Red
        | NamedColor::Green
        | NamedColor::Yellow
        | NamedColor::Blue
        | NamedColor::Magenta
        | NamedColor::Cyan
        | NamedColor::White => Some(color),
        _ => None,
    }
}

const fn bright_terminal_named_color(color: NamedColor) -> Option<NamedColor> {
    match color {
        NamedColor::BrightBlack
        | NamedColor::BrightRed
        | NamedColor::BrightGreen
        | NamedColor::BrightYellow
        | NamedColor::BrightBlue
        | NamedColor::BrightMagenta
        | NamedColor::BrightCyan
        | NamedColor::BrightWhite => Some(color),
        _ => None,
    }
}

fn indexed_terminal_color(index: u8, palette: &Palette) -> Index {
    if index < 16 {
        return ANSI_16_TO_NA16[index as usize];
    }

    palette.nearest_index(indexed_terminal_rgb(index))
}

fn indexed_terminal_glow_color(index: u8, palette: &Palette) -> Index {
    if index < 16 {
        return ANSI_16_GLOW_TO_NA16[index as usize];
    }

    palette.nearest_index(indexed_terminal_rgb(index))
}

const fn indexed_terminal_rgb(index: u8) -> PaletteRgb {
    if index < 16 {
        return terminal_palette_rgb(ANSI_16_TO_NA16[index as usize]);
    }

    if index < 232 {
        let index = index - 16;
        let r = color_cube_value(index / 36);
        let g = color_cube_value((index / 6) % 6);
        let b = color_cube_value(index % 6);
        return PaletteRgb { r, g, b };
    }

    let value = 8 + (index - 232) * 10;
    PaletteRgb {
        r: value,
        g: value,
        b: value,
    }
}

const fn color_cube_value(value: u8) -> u8 {
    if value == 0 { 0 } else { 55 + value * 40 }
}

#[allow(clippy::match_wildcard_for_single_variants)]
fn dim_color(color: Color) -> Color {
    match color {
        Color::Named(named) => Color::Named(named.to_dim()),
        Color::Indexed(index @ 8..=15) => Color::Indexed(index - 8),
        Color::Spec(rgb) => Color::Spec(Rgb {
            r: rgb.r / 2,
            g: rgb.g / 2,
            b: rgb.b / 2,
        }),
        other => other,
    }
}

const fn terminal_rgb(rgb: Rgb) -> PaletteRgb {
    PaletteRgb {
        r: rgb.r,
        g: rgb.g,
        b: rgb.b,
    }
}

const fn terminal_palette_rgb(index: Index) -> PaletteRgb {
    let (r, g, b) = SOURCE_PALETTE_RGB[index as usize % SOURCE_PALETTE_RGB.len()];
    PaletteRgb { r, g, b }
}

const ANSI_16_TO_NA16: [Index; 16] = [
    TERM_COLOR_BLACK,
    TERM_COLOR_RED,
    TERM_COLOR_GREEN,
    TERM_COLOR_YELLOW,
    TERM_COLOR_BLUE,
    TERM_COLOR_MAGENTA,
    TERM_COLOR_CYAN,
    TERM_COLOR_WHITE,
    TERM_COLOR_BRIGHT_BLACK,
    TERM_COLOR_BRIGHT_RED,
    TERM_COLOR_BRIGHT_GREEN,
    TERM_COLOR_BRIGHT_YELLOW,
    TERM_COLOR_BRIGHT_BLUE,
    TERM_COLOR_BRIGHT_MAGENTA,
    TERM_COLOR_BRIGHT_CYAN,
    TERM_COLOR_BRIGHT_WHITE,
];

const ANSI_16_GLOW_TO_NA16: [Index; 16] = [
    TERM_COLOR_DIM_BLACK,
    TERM_COLOR_DIM_RED,
    TERM_COLOR_DIM_GREEN,
    TERM_COLOR_DIM_YELLOW,
    TERM_COLOR_DIM_BLUE,
    TERM_COLOR_DIM_MAGENTA,
    TERM_COLOR_DIM_CYAN,
    TERM_COLOR_DIM_WHITE,
    TERM_COLOR_DIM_BLACK,
    TERM_COLOR_DIM_RED,
    TERM_COLOR_DIM_GREEN,
    TERM_COLOR_DIM_YELLOW,
    TERM_COLOR_DIM_BLUE,
    TERM_COLOR_DIM_MAGENTA,
    TERM_COLOR_DIM_CYAN,
    TERM_COLOR_DIM_WHITE,
];

const SOURCE_PALETTE_RGB: [(u8, u8, u8); 16] = [
    (140, 143, 174),
    (88, 69, 99),
    (62, 33, 55),
    (154, 99, 72),
    (215, 155, 125),
    (245, 237, 186),
    (192, 199, 65),
    (100, 125, 52),
    (228, 148, 58),
    (157, 48, 59),
    (210, 100, 113),
    (112, 55, 127),
    (126, 196, 193),
    (52, 133, 157),
    (23, 67, 75),
    (31, 14, 28),
];

fn draw_glyph(
    fb: &mut Framebuffer,
    atlas: &GlyphAtlas,
    ch: char,
    x: usize,
    y: usize,
    color: Index,
) {
    for gy in 0..GLYPH_H {
        for gx in 0..GLYPH_W {
            if !atlas.is_on(ch, gx, gy) {
                continue;
            }
            fb.set_pixel(x + gx, y + gy, color);
        }
    }
}

fn button_at(x: i16, y: i16) -> Option<usize> {
    let x = x.max(0) as usize;
    let y = y.max(0) as usize;
    BUTTON_TARGETS.iter().position(|button| {
        let button_x = art_x(button.x).saturating_add_signed(BUTTON_HIT_OFFSET_X);
        let button_y = art_y(button.y);
        x >= button_x && x < button_x + button.w && y >= button_y && y < button_y + button.h
    })
}

const fn art_x(x: usize) -> usize {
    x.saturating_sub(ART_CROP_X)
}

const fn art_y(y: usize) -> usize {
    y.saturating_sub(ART_CROP_Y)
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn screen_point(x: i16, y: i16, size: &WindowSize) -> Option<Point> {
    if x < 0 || y < 0 {
        return None;
    }

    let x = x as usize;
    let y = y as usize;
    let screen_x = art_x(SCREEN_SOURCE_X);
    let screen_y = art_y(SCREEN_SOURCE_Y);
    if x < screen_x || y < screen_y || x >= screen_x + SCREEN_W || y >= screen_y + SCREEN_H {
        return None;
    }

    Some(cell_at(x, y, screen_x, screen_y, size))
}

/// Like `screen_point`, but clamps out-of-grid coordinates to the nearest
/// cell instead of rejecting them. Used for mouse-release: a drag can start
/// inside the grid (forwarding a mouse-down escape) and end past its edge,
/// and dropping that release (as `screen_point` would) leaves the remote pty
/// app thinking the button is still held, since the matching mouse-up escape
/// never arrives.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn clamped_screen_point(x: i16, y: i16, size: &WindowSize) -> Point {
    let screen_x = art_x(SCREEN_SOURCE_X);
    let screen_y = art_y(SCREEN_SOURCE_Y);
    let x = (x.max(0) as usize).clamp(screen_x, screen_x + SCREEN_W - 1);
    let y = (y.max(0) as usize).clamp(screen_y, screen_y + SCREEN_H - 1);
    cell_at(x, y, screen_x, screen_y, size)
}

/// Column/line math shared by `screen_point` and `clamped_screen_point`;
/// `x`/`y` must already be within the screen rect.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn cell_at(x: usize, y: usize, screen_x: usize, screen_y: usize, size: &WindowSize) -> Point {
    let column =
        ((x - screen_x) / size.cell_width as usize).min((size.num_cols as usize).saturating_sub(1));
    let line = ((y - screen_y) / size.cell_height as usize)
        .min((size.num_lines as usize).saturating_sub(1));
    Point::new(Line(line as i32), Column(column))
}

