//! Everything painted onto the puter widget: the static CRT case dressing
//! (mode buttons, indicator lights, power/lock icons) and turning terminal
//! cells into glyphs on screen, including the ANSI-to-palette color mapping.
//! Terminal *state* (the pty adapter) lives in `terminal`; this module only
//! draws.

use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::vte::ansi::{Color, NamedColor, Rgb};

use super::{
    BUTTON_H, BUTTON_HIT_OFFSET_X, BUTTON_SPRITE_OFFSET_X, BUTTON_SPRITE_STRIDE, BUTTON_W, GLYPH_H,
    GLYPH_W, PressState, SCREEN_SOURCE_X, SCREEN_SOURCE_Y, art_x, art_y,
};
use crate::palette_color;
use crate::{Framebuffer, Index, Palette, Rect, Rgb as PaletteRgb, Sprite};

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
pub(super) const BUTTON_TARGETS: [Button; 5] = [
    Button {
        x: BRIGHTNESS_BUTTON_X,
        y: MODE_BUTTON_Y,
        w: BUTTON_W,
        h: BUTTON_H,
        kind: ButtonKind::Setting {
            sprite_index: 0,
            action: SettingAction::Brightness,
        },
    },
    Button {
        x: COLOR_BUTTON_X,
        y: MODE_BUTTON_Y,
        w: BUTTON_W,
        h: BUTTON_H,
        kind: ButtonKind::Setting {
            sprite_index: 1,
            action: SettingAction::Color,
        },
    },
    Button {
        x: CONTRAST_BUTTON_X,
        y: MODE_BUTTON_Y,
        w: BUTTON_W,
        h: BUTTON_H,
        kind: ButtonKind::Setting {
            sprite_index: 2,
            action: SettingAction::Contrast,
        },
    },
    Button {
        x: POWER_BUTTON_X,
        y: ICON_BUTTON_Y,
        w: ICON_BUTTON_W,
        h: ICON_BUTTON_H,
        kind: ButtonKind::Icon(IconAction::Power),
    },
    Button {
        x: LOCK_BUTTON_X,
        y: ICON_BUTTON_Y,
        w: ICON_BUTTON_W,
        h: ICON_BUTTON_H,
        kind: ButtonKind::Icon(IconAction::Lock),
    },
];

pub(super) struct TerminalTextStyle {
    pub(super) fg: Index,
    pub(super) glow: Option<Index>,
}

/// Final resolved colors for one terminal cell.
pub(super) struct CellStyle {
    pub(super) fg: Index,
    pub(super) bg: Option<Index>,
    pub(super) glow: Option<Index>,
}

pub(super) struct GlyphAtlas {
    width: usize,
    pixels: Vec<bool>,
}

impl GlyphAtlas {
    /// Size checks and the ink threshold happen at build time (build.rs),
    /// which bakes the atlas to a 0/1 mask.
    pub(super) fn load() -> Self {
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

#[derive(Clone, Copy)]
pub(super) struct Button {
    x: usize,
    y: usize,
    w: usize,
    h: usize,
    pub(super) kind: ButtonKind,
}

/// A front-panel button is either an icon button (power/lock: no "pressed"
/// sprite swap, and a side effect instead of a `DisplaySettings` toggle) or a
/// setting button (one of the mode buttons: swaps to `sprite_index`'s pressed
/// sprite and toggles a `DisplaySettings` field). Structurally distinct so
/// `DisplaySettings::toggle` only has to accept the setting-capable variant.
#[derive(Clone, Copy)]
pub(super) enum ButtonKind {
    Icon(IconAction),
    Setting {
        sprite_index: usize,
        action: SettingAction,
    },
}

#[derive(Clone, Copy)]
pub(super) enum IconAction {
    Power,
    Lock,
}

#[derive(Clone, Copy)]
pub(super) enum SettingAction {
    Brightness,
    Color,
    Contrast,
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum TextMode {
    Green,
    Orange,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct DisplaySettings {
    pub(super) high_brightness: bool,
    text_mode: TextMode,
    pub(super) high_contrast: bool,
}

impl DisplaySettings {
    pub(super) const fn new() -> Self {
        Self {
            high_brightness: true,
            text_mode: TextMode::Orange,
            high_contrast: false,
        }
    }

    pub(super) const fn toggle(&mut self, action: SettingAction) {
        match action {
            SettingAction::Brightness => self.high_brightness = !self.high_brightness,
            SettingAction::Color => {
                self.text_mode = match self.text_mode {
                    TextMode::Green => TextMode::Orange,
                    TextMode::Orange => TextMode::Green,
                };
            }
            SettingAction::Contrast => self.high_contrast = !self.high_contrast,
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

pub(super) struct ModeImages {
    pub(super) green_low_contrast: Sprite,
    orange_low_contrast: Sprite,
    high_contrast: Sprite,
}

impl ModeImages {
    pub(super) fn load() -> Self {
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
        let ButtonKind::Setting { sprite_index, .. } = button.kind else {
            continue;
        };

        fb.draw_sprite_region(
            button_sprites,
            Rect::new(
                (BUTTON_SPRITE_OFFSET_X + sprite_index * BUTTON_SPRITE_STRIDE) as isize,
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

    let (x, y) = (light.x as isize, light.y as isize);
    fb.fill_rect(x, y, LIGHT_W, LIGHT_H, shell);
    fb.fill_rect(x, y, LIGHT_W, 1, top);
    fb.fill_rect(x + 1, y + 2, LIGHT_W - 2, LIGHT_H - 3, core);
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
pub(super) fn dim_color(color: Color) -> Color {
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
    x: isize,
    y: isize,
    color: Index,
) {
    for gy in 0..GLYPH_H {
        for gx in 0..GLYPH_W {
            if !atlas.is_on(ch, gx, gy) {
                continue;
            }
            fb.set_pixel(x + gx as isize, y + gy as isize, color);
        }
    }
}

pub(super) fn button_at(x: isize, y: isize) -> Option<usize> {
    let x = x.max(0) as usize;
    let y = y.max(0) as usize;
    BUTTON_TARGETS.iter().position(|button| {
        let button_x = art_x(button.x).saturating_add_signed(BUTTON_HIT_OFFSET_X);
        let button_y = art_y(button.y);
        x >= button_x && x < button_x + button.w && y >= button_y && y < button_y + button.h
    })
}

impl super::Puter {
    #[allow(clippy::significant_drop_tightening)]
    pub(super) fn render(&self, fb: &mut Framebuffer, palette: &Palette) {
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
            let Some(point) =
                alacritty_terminal::term::point_to_viewport(content.display_offset, indexed.point)
            else {
                continue;
            };
            let cell = indexed.cell;
            if cell
                .flags
                .intersects(Flags::WIDE_CHAR_SPACER | Flags::HIDDEN)
            {
                continue;
            }
            let x = (art_x(SCREEN_SOURCE_X) + point.column.0 * cell_w) as isize;
            let y = (art_y(SCREEN_SOURCE_Y) + point.line * cell_h) as isize;
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

        if let Some(cursor_point) = alacritty_terminal::term::point_to_viewport(
            content.display_offset,
            content.cursor.point,
        ) {
            let cursor_x = (art_x(SCREEN_SOURCE_X) + cursor_point.column.0 * cell_w) as isize;
            let cursor_y = (art_y(SCREEN_SOURCE_Y) + cursor_point.line * cell_h) as isize;
            fb.fill_rect(cursor_x, cursor_y, cell_w, cell_h, COLOR_CURSOR);
        }

        if let PressState::Chrome(index) = self.press_state {
            let button = BUTTON_TARGETS[index];
            if let ButtonKind::Setting { sprite_index, .. } = button.kind {
                fb.draw_sprite_region(
                    &self.button_pressed_sprites,
                    Rect::new(
                        (BUTTON_SPRITE_OFFSET_X + sprite_index * BUTTON_SPRITE_STRIDE) as isize,
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
        let mut cache = self.chrome_cache.borrow_mut();
        if cache
            .as_ref()
            .is_some_and(|(cached_settings, _)| *cached_settings != self.settings)
        {
            cache.take();
        }
        let (_, chrome_fb) = cache.get_or_insert_with(|| {
            let mut chrome_fb = Framebuffer::new(fb.width, fb.height, crate::TRANSPARENT);
            self.render_chrome(&mut chrome_fb, palette);
            (self.settings, chrome_fb)
        });
        fb.blit_from(chrome_fb, 0, 0);
    }

    /// The static dressing around the screen: case art, control strip, mode
    /// buttons, lights, and the power/lock buttons.
    fn render_chrome(&self, fb: &mut Framebuffer, palette: &Palette) {
        fb.fill_from_sprite(self.mode_images.for_settings(self.settings), palette);
        fb.fill_rect(
            art_x(CONTROL_CLEAR_X) as isize,
            art_y(CONTROL_CLEAR_Y) as isize,
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
    fn draw_cell(&self, fb: &mut Framebuffer, cell: &Cell, style: CellStyle, x: isize, y: isize) {
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
            fb.fill_rect(x, y + cell_h as isize - 1, cell_w, 1, fg);
            if cell.flags.contains(Flags::DOUBLE_UNDERLINE) && cell_h > 2 {
                fb.fill_rect(x, y + cell_h as isize - 3, cell_w, 1, fg);
            }
        }
        if cell.flags.contains(Flags::STRIKEOUT) {
            fb.fill_rect(x, y + cell_h as isize / 2, cell_w, 1, fg);
        }
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
