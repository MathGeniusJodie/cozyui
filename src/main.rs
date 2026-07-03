#![allow(clippy::cast_possible_wrap)]
#![allow(clippy::cast_sign_loss)]
#![allow(clippy::cast_precision_loss)]
#![allow(clippy::struct_field_names)]

use std::error::Error;
use std::time::Duration;

use x11rb::protocol::Event as XEvent;
use x11rb::protocol::xproto::ButtonIndex;

mod assets;
#[cfg(test)]
mod bench;
mod budgit;
mod day;
mod emojimap;
mod fizzle;
mod fwends;
mod gauges;
mod hunger;
mod localtime;
mod openrouter;
#[allow(dead_code)]
mod paths;
mod puter;
mod stats;
mod text;
mod toodle;
mod twirl;
mod util;
mod wavey;
mod widget;
mod x_window;

pub(crate) use pixel_graphics::{
    Framebuffer, Index, Paint, Palette, Rect, Rgb, Sprite, Swap, TRANSPARENT,
};
use widget::{ScrollDirection, Widget};
use x_window::XWindow;

#[allow(dead_code)]
pub(crate) mod palette_color {
    use crate::Index;

    pub const LAVENDER: Index = 0;
    pub const GUNMETAL: Index = 1;
    pub const PLUM: Index = 2;
    pub const BROWN: Index = 3;
    pub const PEACH: Index = 4;
    pub const CREAM: Index = 5;
    pub const LIME: Index = 6;
    pub const GREEN: Index = 7;
    pub const ORANGE: Index = 8;
    pub const CRIMSON: Index = 9;
    pub const ROSE: Index = 10;
    pub const PURPLE: Index = 11;
    pub const CYAN: Index = 12;
    pub const BLUE: Index = 13;
    pub const PINE: Index = 14;
    pub const BLACK: Index = 15;
}

#[allow(dead_code)]
pub(crate) mod app_color {
    use crate::palette_color;
    use crate::{Index, Paint};

    /// Solid under-paint color for the root/widget framebuffers; the desk
    /// background checker is drawn over it.
    pub const BACKGROUND: Index = palette_color::CYAN;

    /// The background and its cast shadows are the only thing that paints with
    /// these checkers. Everything else uses literal palette colors. With
    /// `TRANSPARENT_BACKGROUND` the background collapses to `TRANSPARENT`
    /// holes instead, which the X SHAPE mask then cuts out of the window.
    pub const BACKGROUND_PAINT: Paint = if crate::TRANSPARENT_BACKGROUND {
        Paint::Solid(crate::TRANSPARENT)
    } else {
        Paint::Checker(palette_color::LIME, palette_color::CYAN)
    };
    /// Shadows stay visible on a transparent background so widgets still cast
    /// onto the desktop below.
    pub const BACKGROUND_SHADOW_PAINT: Paint = if crate::TRANSPARENT_BACKGROUND {
        Paint::Solid(palette_color::PINE)
    } else {
        Paint::Checker(palette_color::BLUE, palette_color::GREEN)
    };
}

/// Mouse cursor shapes, drawn from the baked `cursor_*` sprites. `Disabled`
/// is baked and ready but nothing is disabled yet, so no hit-test maps to it.
#[derive(Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum CursorKind {
    Pointer,
    Text,
    Hand,
    Disabled,
}

pub(crate) const CURSOR_KIND_COUNT: usize = 4;

const WHEEL_UP: u8 = 4;
const WHEEL_DOWN: u8 = 5;

const SHOW_FWENDS: bool = true;
/// Cut the desk background out of the window with the X SHAPE extension so
/// whatever is underneath shows through (no compositor needed). The holes are
/// fully click-through.
const TRANSPARENT_BACKGROUND: bool = true;

#[derive(Clone, Copy, PartialEq, Eq)]
enum WidgetId {
    Puter,
    Toodle,
    Fwends,
    Twirl,
    Wavey,
    Fizzle,
    Day,
    Budgit,
    Stats,
    Hunger,
    Gauges,
}

const WIDGET_COUNT: usize = WidgetId::ALL.len();

impl WidgetId {
    const fn index(self) -> usize {
        self as usize
    }

    /// Paint order: later entries render on top. Hit-testing (`widget_at`)
    /// walks this reversed, so there is exactly one z-order to maintain.
    const ALL: [Self; 11] = [
        Self::Wavey,
        Self::Puter,
        Self::Toodle,
        Self::Fwends,
        Self::Twirl,
        Self::Fizzle,
        Self::Day,
        Self::Budgit,
        Self::Stats,
        Self::Hunger,
        Self::Gauges,
    ];

    fn visible() -> impl Iterator<Item = Self> {
        Self::ALL.into_iter().filter(|widget| widget.is_visible())
    }

    fn is_visible(self) -> bool {
        SHOW_FWENDS || self != Self::Fwends
    }
}

struct App {
    widgets: Widgets,
    desk: Sprite,
    // Indexed by WidgetId::index().
    fbs: [Framebuffer; WIDGET_COUNT],
    rects: [Rect; WIDGET_COUNT],
    focus: WidgetId,
    puter_pressed: bool,
    quit_requested: bool,
    text_drag: Option<WidgetId>,
    /// Widget that just lost focus and needs one repaint to drop its
    /// focus-only visuals (text cursor, selection highlight).
    blurred: Option<WidgetId>,
    /// Floor imposed by the actual X window size (set from ConfigureNotify
    /// when the WM/user makes the window larger than the content needs), so
    /// the bottom-anchored widgets stay pinned to the real bottom edge and
    /// the backing buffer covers the real width instead of leaving
    /// undrawn strips.
    min_width: usize,
    min_height: usize,
}

/// The concrete widgets. Everything generic goes through `get`/`get_mut`
/// (`&dyn Widget`); widget-specific plumbing (terminal events, reply channel,
/// toodle maintenance, twirl spin) uses the fields directly.
struct Widgets {
    puter: puter::Puter,
    toodle: toodle::Toodle,
    fwends: fwends::Fwends,
    twirl: twirl::Twirl,
    wavey: wavey::Wavey,
    fizzle: fizzle::Fizzle,
    day: day::Day,
    budgit: budgit::Budgit,
    stats: stats::Stats,
    hunger: hunger::Hunger,
    gauges: gauges::Gauges,
}

impl Widgets {
    fn get(&self, id: WidgetId) -> &dyn Widget {
        match id {
            WidgetId::Puter => &self.puter,
            WidgetId::Toodle => &self.toodle,
            WidgetId::Fwends => &self.fwends,
            WidgetId::Twirl => &self.twirl,
            WidgetId::Wavey => &self.wavey,
            WidgetId::Fizzle => &self.fizzle,
            WidgetId::Day => &self.day,
            WidgetId::Budgit => &self.budgit,
            WidgetId::Stats => &self.stats,
            WidgetId::Hunger => &self.hunger,
            WidgetId::Gauges => &self.gauges,
        }
    }

    fn get_mut(&mut self, id: WidgetId) -> &mut dyn Widget {
        match id {
            WidgetId::Puter => &mut self.puter,
            WidgetId::Toodle => &mut self.toodle,
            WidgetId::Fwends => &mut self.fwends,
            WidgetId::Twirl => &mut self.twirl,
            WidgetId::Wavey => &mut self.wavey,
            WidgetId::Fizzle => &mut self.fizzle,
            WidgetId::Day => &mut self.day,
            WidgetId::Budgit => &mut self.budgit,
            WidgetId::Stats => &mut self.stats,
            WidgetId::Hunger => &mut self.hunger,
            WidgetId::Gauges => &mut self.gauges,
        }
    }
}

impl App {
    fn load(palette: &Palette) -> Result<Self, Box<dyn Error>> {
        let widgets = Widgets {
            puter: puter::Puter::load(palette)?,
            toodle: toodle::Toodle::load(palette)?,
            fwends: fwends::Fwends::load(palette)?,
            twirl: twirl::Twirl::load(palette)?,
            wavey: wavey::Wavey::load(palette)?,
            fizzle: fizzle::Fizzle::load(palette)?,
            day: day::Day::load(palette)?,
            budgit: budgit::Budgit::load(palette)?,
            stats: stats::Stats::load(palette)?,
            hunger: hunger::Hunger::load(palette)?,
            gauges: gauges::Gauges::load(palette)?,
        };
        let desk = assets::desk();
        let mut sizes = [(0, 0); WIDGET_COUNT];
        let mut fills: [Index; WIDGET_COUNT] = [0; WIDGET_COUNT];
        for id in WidgetId::ALL {
            let widget = widgets.get(id);
            sizes[id.index()] = (widget.width(), widget.height());
            fills[id.index()] = widget.fill_color(palette);
        }
        // Positions start at (0, 0); sync_dynamic_layout below lays everything
        // out through the same path used after every dynamic change.
        let rects = std::array::from_fn(|i| {
            let (w, h) = sizes[i];
            Rect { x: 0, y: 0, w, h }
        });
        let fbs = std::array::from_fn(|i| {
            let (w, h) = sizes[i];
            Framebuffer::new(w, h, fills[i])
        });

        let mut app = Self {
            widgets,
            desk,
            fbs,
            rects,
            focus: WidgetId::Toodle,
            puter_pressed: false,
            quit_requested: false,
            text_drag: None,
            blurred: None,
            min_width: 0,
            min_height: 0,
        };
        app.sync_dynamic_layout(palette);
        Ok(app)
    }

    fn width(&self) -> usize {
        let mut width = self.desk.width;
        for widget in WidgetId::visible() {
            let rect = self.rect_for(widget);
            width = width.max(rect.x.saturating_add(rect.w));
        }
        width.max(self.min_width)
    }

    fn height(&self) -> usize {
        required_screen_height(&self.widget_heights(), self.desk.height, self.min_height)
    }

    /// Widget layout heights, indexed by `WidgetId::index()`. Fwends
    /// contributes its minimum height since its actual height is sized to the
    /// window.
    fn widget_heights(&self) -> [usize; WIDGET_COUNT] {
        let mut heights = [0; WIDGET_COUNT];
        for id in WidgetId::ALL {
            heights[id.index()] = self.widgets.get(id).layout_height();
        }
        heights
    }

    /// Records the actual X window size (from a ConfigureNotify) so the
    /// bottom-anchored layout can grow to fill it and the backing buffer
    /// matches the real geometry. Returns whether the floor changed and a
    /// relayout is needed.
    fn set_min_size(&mut self, width: usize, height: usize) -> bool {
        if self.min_width == width && self.min_height == height {
            return false;
        }
        self.min_width = width;
        self.min_height = height;
        true
    }

    #[allow(clippy::unused_self)]
    const fn fill_color(&self, _palette: &Palette) -> Index {
        app_color::BACKGROUND
    }

    fn render_background(&self, fb: &mut Framebuffer, palette: &Palette) {
        self.render_background_rect(fb, palette, Rect::new(0, 0, fb.width, fb.height));
    }

    fn render_background_rect(&self, fb: &mut Framebuffer, palette: &Palette, rect: Rect) {
        fb.fill_rect_paint(rect.x, rect.y, rect.w, rect.h, app_color::BACKGROUND_PAINT);
        draw_stretched_desk_region(fb, &self.desk, palette, rect);
    }

    fn start(&mut self, window_id: u64) -> Result<(), Box<dyn Error>> {
        self.widgets.puter.start_terminal(window_id)
    }

    fn render(&mut self, fb: &mut Framebuffer, palette: &Palette) {
        self.render_background(fb, palette);
        for widget in WidgetId::visible() {
            self.render_widget(fb, palette, widget);
        }
    }

    fn render_widget(&mut self, fb: &mut Framebuffer, palette: &Palette, widget: WidgetId) {
        let widget_fb = &mut self.fbs[widget.index()];
        let w = self.widgets.get_mut(widget);
        widget_fb.clear(w.fill_color(palette));
        w.render(widget_fb, palette);
        let rect = self.rects[widget.index()];
        fb.blit_from(&self.fbs[widget.index()], rect.x, rect.y);
    }

    fn render_focused_widget(&mut self, fb: &mut Framebuffer, palette: &Palette) {
        self.render_rect(fb, palette, self.focused_rect());
    }

    fn render_and_draw_widget(
        &mut self,
        fb: &mut Framebuffer,
        xwin: &mut XWindow,
        palette: &Palette,
        widget: WidgetId,
    ) -> Result<(), Box<dyn Error>> {
        if !widget.is_visible() {
            return Ok(());
        }
        let rect = self.rect_for(widget);
        self.render_rect(fb, palette, rect);
        xwin.draw_rect(fb, rect)
    }

    fn render_rect(&mut self, fb: &mut Framebuffer, palette: &Palette, rect: Rect) {
        self.render_background_rect(fb, palette, rect);
        for widget in WidgetId::visible() {
            if rects_intersect(self.rect_for(widget), rect) {
                self.render_widget(fb, palette, widget);
            }
        }
    }

    fn sync_dynamic_layout(&mut self, palette: &Palette) -> bool {
        let mut changed = false;
        let toodle_w = self.widgets.toodle.width();
        let toodle_h = self.widgets.toodle.height();

        let toodle = &mut self.rects[WidgetId::Toodle.index()];
        if toodle.w != toodle_w || toodle.h != toodle_h {
            toodle.w = toodle_w;
            toodle.h = toodle_h;
            self.fbs[WidgetId::Toodle.index()] =
                Framebuffer::new(toodle_w, toodle_h, self.widgets.toodle.fill_color(palette));
            changed = true;
        }

        let heights = self.widget_heights();
        let screen_h = required_screen_height(&heights, self.desk.height, self.min_height);
        let positions = widget_positions(&heights, screen_h);
        for (rect, (x, y)) in self.rects.iter_mut().zip(positions) {
            changed |= move_rect(rect, x, y);
        }
        changed |= self.sync_fwends_height(screen_h, palette);

        changed
    }

    fn sync_fwends_height(&mut self, screen_h: usize, palette: &Palette) -> bool {
        let fwends = &mut self.widgets.fwends;
        let fwends_rect = self.rects[WidgetId::Fwends.index()];
        // Fwends' top is pinned FWENDS_TOP below the window's top edge, so its
        // height must leave that much room or its rect overruns the screen.
        if !fwends.set_height(screen_h.saturating_sub(FWENDS_TOP))
            && fwends_rect.h == fwends.height()
        {
            return false;
        }

        self.rects[WidgetId::Fwends.index()].h = fwends.height();
        self.fbs[WidgetId::Fwends.index()] =
            Framebuffer::new(fwends_rect.w, fwends.height(), fwends.fill_color(palette));
        true
    }

    const fn focused_rect(&self) -> Rect {
        self.rect_for(self.focus)
    }

    const fn rect_for(&self, widget: WidgetId) -> Rect {
        self.rects[widget.index()]
    }

    fn drain_events(&self) -> puter::TerminalEvents {
        self.widgets.puter.drain_terminal_events()
    }

    fn drain_replies(&mut self) -> bool {
        SHOW_FWENDS && self.widgets.fwends.drain_reply()
    }

    fn handle_key_press(
        &mut self,
        input: &text::KeyInput,
        clipboard_text: Option<&str>,
    ) -> Result<Option<String>, Box<dyn Error>> {
        if !self.focus.is_visible() {
            return Ok(None);
        }
        self.widgets
            .get_mut(self.focus)
            .handle_key_press(input, clipboard_text)
    }

    /// Whether this key press needs the clipboard fetched for the focused
    /// widget (it is that widget's paste shortcut).
    fn wants_clipboard(&self, input: &text::KeyInput) -> bool {
        self.focus.is_visible() && self.widgets.get(self.focus).wants_clipboard(input)
    }

    /// Returns text the clicked widget wants copied to the clipboard.
    fn click(&mut self, x: i16, y: i16, state: u16) -> Result<Option<String>, Box<dyn Error>> {
        self.puter_pressed = false;
        self.text_drag = None;
        let Some((widget, x, y)) = self.widget_at(x, y) else {
            return Ok(None);
        };
        if self.focus != widget {
            self.widgets.get_mut(self.focus).blur();
            self.blurred = Some(self.focus);
        }
        self.focus = widget;
        self.puter_pressed = widget == WidgetId::Puter;
        let outcome = self.widgets.get_mut(widget).click(x, y, state)?;
        if outcome.spin_twirl {
            self.widgets.twirl.spin();
        }
        if outcome.text_drag {
            self.text_drag = Some(widget);
        }
        Ok(outcome.copy_text)
    }

    /// The widget that just lost focus and needs a repaint, if any.
    fn take_blurred(&mut self) -> Option<WidgetId> {
        self.blurred.take()
    }

    fn release(&mut self, x: i16, y: i16) -> Option<WidgetId> {
        if let Some(widget) = self.text_drag.take() {
            self.widgets.get_mut(widget).end_text_drag();
            return Some(widget);
        }

        if self.focus == WidgetId::Wavey && self.widgets.wavey.release() {
            return Some(WidgetId::Wavey);
        }

        if self.puter_pressed {
            let (x, y) = self.rect_for(WidgetId::Puter).local(x, y);
            if self.widgets.puter.release_button(x, y) {
                self.quit_requested = true;
            }
            self.puter_pressed = false;
            return Some(WidgetId::Puter);
        }

        None
    }

    fn motion(&mut self, x: i16, y: i16) -> Option<WidgetId> {
        if let Some(widget) = self.text_drag {
            let (local_x, local_y) = self.rect_for(widget).local(x, y);
            return self
                .widgets
                .get_mut(widget)
                .drag_text(local_x, local_y)
                .then_some(widget);
        }

        // Focus-follow motion (puter text selection, wavey knob drag).
        let (local_x, local_y) = self.rect_for(self.focus).local(x, y);
        if self.widgets.get_mut(self.focus).motion(local_x, local_y) {
            return Some(self.focus);
        }

        if self.rect_for(WidgetId::Toodle).contains(x, y) {
            let (x, y) = self.rect_for(WidgetId::Toodle).local(x, y);
            self.widgets.toodle.hover(x, y).then_some(WidgetId::Toodle)
        } else {
            self.widgets
                .toodle
                .hover(-1, -1)
                .then_some(WidgetId::Toodle)
        }
    }

    fn scroll_up(&mut self, x: i16, y: i16) -> Option<WidgetId> {
        self.scroll(x, y, ScrollDirection::Up)
    }

    fn scroll_down(&mut self, x: i16, y: i16) -> Option<WidgetId> {
        self.scroll(x, y, ScrollDirection::Down)
    }

    fn scroll(&mut self, x: i16, y: i16, direction: ScrollDirection) -> Option<WidgetId> {
        let (widget, x, y) = self.widget_at(x, y)?;
        self.widgets
            .get_mut(widget)
            .scroll(x, y, direction)
            .then_some(widget)
    }

    /// Which cursor shape fits whatever is under the pointer.
    fn cursor_at(&self, x: i16, y: i16) -> CursorKind {
        if self.text_drag.is_some() {
            return CursorKind::Text;
        }
        let Some((widget, x, y)) = self.widget_at(x, y) else {
            return CursorKind::Pointer;
        };
        self.widgets.get(widget).cursor_at(x, y)
    }

    /// The topmost visible widget under `(x, y)`: paint order reversed, so
    /// hit-testing always agrees with what is drawn on top.
    fn widget_at(&self, x: i16, y: i16) -> Option<(WidgetId, i16, i16)> {
        WidgetId::ALL
            .into_iter()
            .rev()
            .filter(|widget| widget.is_visible())
            .find_map(|widget| {
                let rect = self.rect_for(widget);
                rect.contains(x, y).then(|| {
                    let (x, y) = rect.local(x, y);
                    (widget, x, y)
                })
            })
    }

    /// Ticks one widget's periodic update; returns whether it needs a redraw.
    /// Puter, Fwends, and Toodle keep the trait's do-nothing default: they are
    /// driven separately in the main loop (terminal events, reply channel, and
    /// save/relayout housekeeping).
    fn update_widget(&mut self, widget: WidgetId) -> Result<bool, Box<dyn Error>> {
        self.widgets.get_mut(widget).update()
    }

    fn shutdown(&mut self) {
        if let Err(err) = self.widgets.toodle.flush_saves() {
            eprintln!("toodle save failed on shutdown: {err}");
        }
        self.widgets.wavey.shutdown();
        self.widgets.puter.shutdown_terminal();
    }
}

/// Where every widget sits. Tweak positions here.
///
/// `(x, y)`: x is pixels from the window's left edge to the widget's left
/// edge; y is pixels from the BOTTOM of the window up to the widget's bottom
/// edge (y = 0 puts the widget flush with the bottom; negative y hangs that
/// many pixels of it below the bottom edge). Widgets keep their bottom-left
/// corner pinned, so ones with dynamic sizes grow up/right from here.
///
/// Fwends is the one exception: it is pinned to the window's TOP edge and
/// only its x is used.
const fn widget_xy(widget: WidgetId) -> (usize, isize) {
    match widget {
        WidgetId::Puter => (463, 39),
        WidgetId::Toodle => (322, 301),
        WidgetId::Fwends => (713, -10),
        WidgetId::Twirl => (587, 321),
        WidgetId::Wavey => (113, 31),
        WidgetId::Fizzle => (393, 31),
        WidgetId::Day => (330, 197),
        WidgetId::Budgit => (322, 752),
        WidgetId::Stats => (322, 604),
        WidgetId::Hunger => (463, 3),
        WidgetId::Gauges => (335, 915),
    }
}

/// The desk background's bottom edge, in the same bottom-up coordinate as
/// `widget_xy` (negative = hangs below the window's bottom edge).
const DESK_Y: isize = -39;

/// Empty space kept above the tallest widget when the window height is
/// content-driven rather than set by the window manager.
const TOP_PADDING: usize = 25;

/// How far below the window's top edge Fwends' top is pinned.
const FWENDS_TOP: usize = 10;

/// Widget (x, y) top-left positions, indexed by `WidgetId::index()`.
/// Converts the bottom-anchored `widget_xy` table for a `screen_h`-tall
/// window: top = screen_h - y - height.
fn widget_positions(
    heights: &[usize; WIDGET_COUNT],
    screen_h: usize,
) -> [(usize, usize); WIDGET_COUNT] {
    let mut positions = [(0, 0); WIDGET_COUNT];
    for widget in WidgetId::ALL {
        let (x, y) = widget_xy(widget);
        let h = heights[widget.index()] as isize;
        let top = (screen_h as isize - y - h).max(0) as usize;
        positions[widget.index()] = (x, top);
    }
    // Fwends stays pinned just below the window's top edge.
    positions[WidgetId::Fwends.index()].1 = FWENDS_TOP;
    positions
}

/// Window height needed to show every bottom-anchored widget and the desk,
/// plus TOP_PADDING; the actual window height (`min_h`) wins if larger.
/// Fwends contributes its `widget_heights` entry (its minimum height) at
/// y = 0 like everything else, even though it ends up pinned to the top.
fn required_screen_height(heights: &[usize; WIDGET_COUNT], desk_h: usize, min_h: usize) -> usize {
    let mut needed = (desk_h as isize + DESK_Y).max(0) as usize;
    for widget in WidgetId::ALL {
        if !widget.is_visible() {
            continue;
        }
        let (_, y) = widget_xy(widget);
        let h = heights[widget.index()] as isize;
        needed = needed.max((h + y).max(0) as usize);
    }
    (needed + TOP_PADDING).max(min_h)
}

const fn move_rect(rect: &mut Rect, x: usize, y: usize) -> bool {
    if rect.x == x && rect.y == y {
        return false;
    }

    rect.x = x;
    rect.y = y;
    true
}

const fn rects_intersect(a: Rect, b: Rect) -> bool {
    a.x < b.x.saturating_add(b.w)
        && b.x < a.x.saturating_add(a.w)
        && a.y < b.y.saturating_add(b.h)
        && b.y < a.y.saturating_add(a.h)
}

fn draw_stretched_desk_region(fb: &mut Framebuffer, desk: &Sprite, palette: &Palette, rect: Rect) {
    // The desk is bottom-anchored like the widgets: its bottom edge sits
    // DESK_Y above the window's bottom (below it when negative, cutting off
    // its lowest rows).
    let desk_y = ((fb.height as isize - DESK_Y).max(0) as usize).saturating_sub(desk.height);
    let x0 = rect.x.min(fb.width);
    let x1 = rect.x.saturating_add(rect.w).min(fb.width);
    let y0 = rect.y.max(desk_y).min(fb.height);
    let y1 = rect
        .y
        .saturating_add(rect.h)
        .min(desk_y.saturating_add(desk.height))
        .min(fb.height);

    if x0 >= x1 || y0 >= y1 || desk.width == 0 || desk.height == 0 {
        return;
    }

    let source_x: Vec<usize> = (x0..x1)
        .map(|x| stretched_desk_source_x(x, fb.width, desk.width))
        .collect();
    for y in y0..y1 {
        let source_y = y - desk_y;
        for x in x0..x1 {
            let paint = desk_background_paint(desk.at(source_x[x - x0], source_y));
            if let Some(color) = palette.resolve_paint_index(paint, x, y) {
                fb.set_pixel(x, y, color);
            }
        }
    }
}

fn stretched_desk_source_x(x: usize, target_w: usize, source_w: usize) -> usize {
    if source_w <= 1 || target_w <= source_w {
        return x.min(source_w.saturating_sub(1));
    }

    let middle = source_w / 2;
    let left_w = middle;
    let right_w = source_w - middle - 1;
    let middle_w = target_w.saturating_sub(left_w + right_w).max(1);

    if x < left_w {
        x
    } else if x < left_w + middle_w {
        middle
    } else {
        middle + 1 + (x - left_w - middle_w).min(right_w.saturating_sub(1))
    }
}

const fn desk_background_paint(index: Index) -> Paint {
    match index {
        palette_color::ROSE => app_color::BACKGROUND_PAINT,
        palette_color::CRIMSON => app_color::BACKGROUND_SHADOW_PAINT,
        other => Paint::Solid(other),
    }
}

pub(crate) fn draw_filled_circle(
    fb: &mut Framebuffer,
    center_x: isize,
    center_y: isize,
    radius: isize,
    color: Index,
) {
    draw_filled_ellipse(fb, center_x, center_y, radius, radius, color);
}

#[allow(clippy::similar_names)]
pub(crate) fn draw_filled_ellipse(
    fb: &mut Framebuffer,
    center_x: isize,
    center_y: isize,
    radius_x: isize,
    radius_y: isize,
    color: Index,
) {
    if radius_x <= 0 || radius_y <= 0 {
        return;
    }

    let radius_x_sq = radius_x * radius_x;
    let radius_y_sq = radius_y * radius_y;
    let ellipse_sq = radius_x_sq * radius_y_sq;
    for dy in -radius_y..=radius_y {
        let y_term = dy * dy * radius_x_sq;
        let mut dx = 0;
        while (dx + 1) * (dx + 1) * radius_y_sq + y_term <= ellipse_sq {
            dx += 1;
        }
        for x in center_x - dx..=center_x + dx {
            let y = center_y + dy;
            if x >= 0 && y >= 0 {
                fb.set_pixel(x as usize, y as usize, color);
            }
        }
    }
}

#[allow(clippy::too_many_lines)]
fn main() -> Result<(), Box<dyn Error>> {
    let palette = assets::palette();
    let mut app = App::load(&palette)?;
    let width = app.width();
    let height = app.height();
    let mut fb = Framebuffer::new(width, height, app.fill_color(&palette));
    let mut xwin = XWindow::open(width, height, TRANSPARENT_BACKGROUND)?;
    xwin.set_palette(&palette);
    xwin.load_cursors(&palette)?;
    app.start(u64::from(xwin.window))?;
    app.render(&mut fb, &palette);
    xwin.draw(&fb)?;

    let mut running = true;
    while running {
        let mut drew_frame = false;
        let terminal_events = app.drain_events();
        running = terminal_events.running;
        if terminal_events.dirty {
            app.render_and_draw_widget(&mut fb, &mut xwin, &palette, WidgetId::Puter)?;
            drew_frame = true;
        }

        if app.drain_replies() {
            app.render_and_draw_widget(&mut fb, &mut xwin, &palette, WidgetId::Fwends)?;
            drew_frame = true;
        }

        for widget in WidgetId::ALL {
            if app.update_widget(widget)? {
                app.render_and_draw_widget(&mut fb, &mut xwin, &palette, widget)?;
                drew_frame = true;
            }
        }

        // Toodle housekeeping: debounced saves hit disk shortly after typing
        // pauses, and external edits to the todo files are folded in. The
        // latter can change the page-stack size, so it may need a relayout.
        if app.widgets.toodle.maintain()? {
            if sync_window_layout(&mut app, &mut fb, &mut xwin, &palette)? {
                app.render(&mut fb, &palette);
                xwin.draw(&fb)?;
            } else {
                app.render_and_draw_widget(&mut fb, &mut xwin, &palette, WidgetId::Toodle)?;
            }
            drew_frame = true;
        }

        let mut pending_motion_widget = None;
        // Coalesce input redraws: key auto-repeat and rapid clicks can deliver
        // many events in a single drain. Repainting per event piles up frames
        // faster than they display and makes typing lag. We update state for
        // every event but repaint the focused widget at most once per batch.
        let mut needs_input_redraw = false;
        while let Some(event) = xwin.poll_event()? {
            match event {
                XEvent::Expose(event) => {
                    // Exposures arrive in batches; `count` is how many more
                    // follow. Repaint once, on the last one.
                    if event.count == 0 {
                        app.render(&mut fb, &palette);
                        xwin.draw(&fb)?;
                        drew_frame = true;
                        needs_input_redraw = false;
                        pending_motion_widget = None;
                    }
                }
                XEvent::KeyPress(event) => {
                    let input = xwin.keyboard.press(event.detail, event.state.into());
                    let paste_text = if app.wants_clipboard(&input) {
                        xwin.clipboard_text()?
                    } else {
                        None
                    };
                    if let Some(copy_text) = app.handle_key_press(&input, paste_text.as_deref())? {
                        xwin.set_clipboard_text(copy_text)?;
                    }
                    needs_input_redraw = true;
                }
                XEvent::KeyRelease(event) => {
                    xwin.keyboard.release(event.detail);
                }
                XEvent::ButtonPress(event) => match event.detail {
                    WHEEL_UP => {
                        if let Some(widget) = app.scroll_up(event.event_x, event.event_y) {
                            app.render_and_draw_widget(&mut fb, &mut xwin, &palette, widget)?;
                            drew_frame = true;
                        }
                    }
                    WHEEL_DOWN => {
                        if let Some(widget) = app.scroll_down(event.event_x, event.event_y) {
                            app.render_and_draw_widget(&mut fb, &mut xwin, &palette, widget)?;
                            drew_frame = true;
                        }
                    }
                    detail if detail == u8::from(ButtonIndex::M1) => {
                        if let Some(copy_text) =
                            app.click(event.event_x, event.event_y, event.state.into())?
                        {
                            xwin.set_clipboard_text(copy_text)?;
                        }
                        needs_input_redraw = true;
                    }
                    _ => {}
                },
                XEvent::ButtonRelease(event) => {
                    if event.detail == u8::from(ButtonIndex::M1)
                        && let Some(widget) = app.release(event.event_x, event.event_y)
                    {
                        pending_motion_widget =
                            pending_motion_widget.filter(|pending| *pending != widget);
                        app.render_and_draw_widget(&mut fb, &mut xwin, &palette, widget)?;
                        drew_frame = true;
                    }
                }
                XEvent::MotionNotify(event) => {
                    xwin.set_cursor(app.cursor_at(event.event_x, event.event_y))?;
                    if let Some(widget) = app.motion(event.event_x, event.event_y) {
                        pending_motion_widget = Some(widget);
                    }
                }
                XEvent::SelectionRequest(event) => xwin.handle_selection_request(event)?,
                XEvent::SelectionClear(event) => xwin.handle_selection_clear(event),
                XEvent::ConfigureNotify(event) => {
                    if app.set_min_size(event.width as usize, event.height as usize) {
                        app.sync_dynamic_layout(&palette);
                        fb = Framebuffer::new(app.width(), app.height(), app.fill_color(&palette));
                        xwin.resize_backing(fb.width, fb.height)?;
                        app.render(&mut fb, &palette);
                        xwin.draw(&fb)?;
                        drew_frame = true;
                    }
                }
                XEvent::DestroyNotify(_) => running = false,
                _ => {}
            }
        }
        if app.quit_requested {
            running = false;
        }
        if needs_input_redraw {
            redraw_after_input(&mut app, &mut fb, &mut xwin, &palette)?;
            drew_frame = true;
        }
        if let Some(widget) = pending_motion_widget {
            app.render_and_draw_widget(&mut fb, &mut xwin, &palette, widget)?;
            drew_frame = true;
        }

        // After drawing, come back at ~60fps for whatever is still animating
        // (spinning wheel, terminal bursts) instead of spinning the loop as
        // fast as it can go. Otherwise sleep on the X socket: input wakes us
        // instantly, and 250ms bounds the latency of the non-X sources
        // checked above (channels and widget timers).
        xwin.wait_for_event(Duration::from_millis(if drew_frame { 16 } else { 250 }))?;
    }

    app.shutdown();
    Ok(())
}

/// After a key or click: if the layout changed, resize and redraw everything;
/// otherwise repaint just the focused widget.
fn redraw_after_input(
    app: &mut App,
    fb: &mut Framebuffer,
    xwin: &mut XWindow,
    palette: &Palette,
) -> Result<(), Box<dyn Error>> {
    let blurred = app.take_blurred();
    if sync_window_layout(app, fb, xwin, palette)? {
        app.render(fb, palette);
        xwin.draw(fb)?;
        return Ok(());
    }
    // The widget that just lost focus repaints once so its cursor disappears.
    if let Some(widget) = blurred {
        app.render_and_draw_widget(fb, xwin, palette, widget)?;
    }
    app.render_focused_widget(fb, palette);
    xwin.draw_rect(fb, app.focused_rect())?;
    Ok(())
}

fn sync_window_layout(
    app: &mut App,
    fb: &mut Framebuffer,
    xwin: &mut XWindow,
    palette: &Palette,
) -> Result<bool, Box<dyn Error>> {
    if !app.sync_dynamic_layout(palette) {
        return Ok(false);
    }

    *fb = Framebuffer::new(app.width(), app.height(), app.fill_color(palette));
    xwin.resize(fb.width, fb.height)?;
    Ok(true)
}
