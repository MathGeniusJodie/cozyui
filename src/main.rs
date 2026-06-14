#![allow(clippy::cast_possible_wrap)]
#![allow(clippy::cast_sign_loss)]
#![allow(clippy::cast_precision_loss)]
#![allow(clippy::struct_field_names)]

use std::error::Error;
use std::time::Duration;

use x11rb::connection::Connection;
use x11rb::protocol::Event as XEvent;
use x11rb::protocol::xproto::ButtonIndex;
use xkbcommon::xkb::keysyms;

#[cfg(test)]
mod bench;
mod bitmap_font;
mod comicoro_font;
mod day;
mod emojimap;
mod fwends;
mod graphics;
mod peanut_money_font;
#[allow(dead_code)]
mod pixolde_bold_font;
#[allow(dead_code)]
mod pixolde_font;
#[allow(dead_code)]
mod pixolde_italic_font;
#[allow(dead_code)]
mod poco_font;
mod puter;
#[allow(dead_code)]
mod rozha_one_48_font;
mod senpai;
mod text_edit;
mod text_input;
mod text_wrap;
mod toodle;
mod twirl;
mod wavey;
mod x_window;

pub(crate) use graphics::{
    Framebuffer, Index, Paint, Palette, Rect, Rgb, Rgba, Sprite, Swap, TRANSPARENT,
    decode_png_with_size,
};
use x_window::XWindow;

const PALETTE_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/na16-1x.png");
const DESK_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/desk.png");

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
    use crate::Index;
    use crate::palette_color;

    pub const BACKGROUND: Index = palette_color::CYAN;
    pub const BACKGROUND_SHADOW: Index = palette_color::BLUE;
}

const WHEEL_UP: u8 = 4;
const WHEEL_DOWN: u8 = 5;

const WIDGET_GAP: usize = 16;
const APP_LEFT_PADDING: usize = 54;
const APP_BOTTOM_PADDING: usize = 74;
const SHOW_FWENDS: bool = true;
const FWENDS_LEFT_APRON: usize = 60;

#[derive(Clone, Copy, PartialEq, Eq)]
enum WidgetId {
    Puter,
    Toodle,
    Fwends,
    Twirl,
    Wavey,
    Day,
}

const WIDGET_COUNT: usize = 6;

impl WidgetId {
    const fn index(self) -> usize {
        self as usize
    }
}

impl WidgetId {
    const ALL: [Self; 6] = [
        Self::Wavey,
        Self::Puter,
        Self::Toodle,
        Self::Fwends,
        Self::Twirl,
        Self::Day,
    ];

    const VISIBLE_WITH_FWENDS: [Self; 6] = Self::ALL;
    const VISIBLE_WITHOUT_FWENDS: [Self; 5] = [
        Self::Wavey,
        Self::Puter,
        Self::Toodle,
        Self::Twirl,
        Self::Day,
    ];

    const fn visible() -> &'static [Self] {
        if SHOW_FWENDS {
            &Self::VISIBLE_WITH_FWENDS
        } else {
            &Self::VISIBLE_WITHOUT_FWENDS
        }
    }

    fn is_visible(self) -> bool {
        SHOW_FWENDS || self != Self::Fwends
    }
}

struct App {
    puter: puter::Puter,
    toodle: toodle::Toodle,
    fwends: fwends::Fwends,
    twirl: twirl::Twirl,
    wavey: wavey::Wavey,
    day: day::Day,
    desk: Sprite,
    // Indexed by WidgetId::index().
    fbs: [Framebuffer; WIDGET_COUNT],
    rects: [Rect; WIDGET_COUNT],
    focus: WidgetId,
    puter_pressed: bool,
    text_drag: Option<WidgetId>,
}

impl App {
    fn load(palette: &Palette) -> Result<Self, Box<dyn Error>> {
        let puter = puter::Puter::load(palette)?;
        let toodle = toodle::Toodle::load(palette)?;
        let fwends = fwends::Fwends::load(palette)?;
        let twirl = twirl::Twirl::load(palette)?;
        let wavey = wavey::Wavey::load(palette)?;
        let day = day::Day::load(palette)?;
        let desk = Sprite::load_native(DESK_PATH, palette)?;
        let positions = widget_positions(&puter, &toodle, &twirl, &wavey, &day);
        let sizes: [(usize, usize); WIDGET_COUNT] = [
            (puter.width(), puter.height()),
            (toodle.width(), toodle.height()),
            (fwends.width(), fwends.height()),
            (twirl.width(), twirl.height()),
            (wavey.width(), wavey.height()),
            (day.width(), day.height()),
        ];
        let fills: [Rgba; WIDGET_COUNT] = [
            puter.fill_color(palette),
            toodle.fill_color(palette),
            fwends.fill_color(palette),
            twirl.fill_color(palette),
            wavey.fill_color(palette),
            day.fill_color(palette),
        ];
        let rects = std::array::from_fn(|i| {
            let (x, y) = positions[i];
            let (w, h) = sizes[i];
            Rect { x, y, w, h }
        });
        let fbs = std::array::from_fn(|i| {
            let (w, h) = sizes[i];
            Framebuffer::new(w, h, fills[i])
        });

        let mut app = Self {
            puter,
            toodle,
            fwends,
            twirl,
            wavey,
            day,
            desk,
            fbs,
            rects,
            focus: WidgetId::Toodle,
            puter_pressed: false,
            text_drag: None,
        };
        app.sync_fwends_height(palette);
        Ok(app)
    }

    fn width(&self) -> usize {
        let rect = |widget: WidgetId| self.rect_for(widget);
        let width = (rect(WidgetId::Toodle)
            .x
            .saturating_add(rect(WidgetId::Toodle).w))
        .max(self.desk.width)
        .max(rect(WidgetId::Twirl).x + rect(WidgetId::Twirl).w)
        .max(rect(WidgetId::Wavey).x + rect(WidgetId::Wavey).w)
        .max(rect(WidgetId::Day).x + rect(WidgetId::Day).w);
        if SHOW_FWENDS {
            width.max(rect(WidgetId::Fwends).x + rect(WidgetId::Fwends).w)
        } else {
            width
        }
    }

    fn height(&self) -> usize {
        let height = self.target_app_height();
        if SHOW_FWENDS {
            let fwends = self.rect_for(WidgetId::Fwends);
            height.max(fwends.y + fwends.h)
        } else {
            height
        }
    }

    fn target_app_height(&self) -> usize {
        let rect = |widget: WidgetId| self.rect_for(widget);
        let height = rect(WidgetId::Puter)
            .h
            .max(self.desk.height)
            .max(rect(WidgetId::Twirl).y + rect(WidgetId::Twirl).h)
            .max(rect(WidgetId::Wavey).y + rect(WidgetId::Wavey).h)
            .max(rect(WidgetId::Day).y + rect(WidgetId::Day).h);
        let height = if SHOW_FWENDS {
            height.max(rect(WidgetId::Fwends).y + self.fwends.min_height())
        } else {
            height
        };
        height + APP_BOTTOM_PADDING
    }

    #[allow(clippy::unused_self)]
    fn fill_color(&self, palette: &Palette) -> Rgba {
        palette.color(app_color::BACKGROUND).into()
    }

    fn render_background(&self, fb: &mut Framebuffer, palette: &Palette) {
        fb.clear(self.fill_color(palette));
        let full = Rect::new(0, 0, fb.width, fb.height);
        draw_stretched_desk_region(fb, &self.desk, palette, full);
    }

    fn render_background_rect(&self, fb: &mut Framebuffer, palette: &Palette, rect: Rect) {
        fb.fill_rect(rect.x, rect.y, rect.w, rect.h, self.fill_color(palette));
        draw_stretched_desk_region(fb, &self.desk, palette, rect);
    }

    fn start(&mut self, window_id: u64) -> Result<(), Box<dyn Error>> {
        self.puter.start_terminal(window_id)
    }

    fn render(&mut self, fb: &mut Framebuffer, palette: &Palette) {
        self.render_background(fb, palette);
        for widget in WidgetId::visible().iter().copied() {
            self.render_widget(fb, palette, widget);
        }
    }

    fn render_widget(&mut self, fb: &mut Framebuffer, palette: &Palette, widget: WidgetId) {
        let widget_fb = &mut self.fbs[widget.index()];
        match widget {
            WidgetId::Puter => {
                widget_fb.clear(self.puter.fill_color(palette));
                self.puter.render(widget_fb, palette);
            }
            WidgetId::Toodle => {
                widget_fb.clear(self.toodle.fill_color(palette));
                self.toodle.render(widget_fb, palette);
            }
            WidgetId::Fwends => {
                widget_fb.clear(self.fwends.fill_color(palette));
                self.fwends.render(widget_fb, palette);
            }
            WidgetId::Twirl => {
                widget_fb.clear(self.twirl.fill_color(palette));
                self.twirl.render(widget_fb, palette);
            }
            WidgetId::Wavey => {
                widget_fb.clear(self.wavey.fill_color(palette));
                self.wavey.render(widget_fb, palette);
            }
            WidgetId::Day => {
                widget_fb.clear(self.day.fill_color(palette));
                self.day.render(widget_fb, palette);
            }
        }
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
        for widget in WidgetId::visible().iter().copied() {
            if rects_intersect(self.rect_for(widget), rect) {
                self.render_widget(fb, palette, widget);
            }
        }
    }

    fn sync_dynamic_layout(&mut self, palette: &Palette) -> bool {
        let mut changed = false;
        let toodle_w = self.toodle.width();
        let toodle_h = self.toodle.height();

        let toodle = &mut self.rects[WidgetId::Toodle.index()];
        if toodle.w != toodle_w || toodle.h != toodle_h {
            toodle.w = toodle_w;
            toodle.h = toodle_h;
            self.fbs[WidgetId::Toodle.index()] =
                Framebuffer::new(toodle_w, toodle_h, self.toodle.fill_color(palette));
            changed = true;
        }

        let positions = widget_positions(
            &self.puter,
            &self.toodle,
            &self.twirl,
            &self.wavey,
            &self.day,
        );
        for (rect, (x, y)) in self.rects.iter_mut().zip(positions) {
            changed |= move_rect(rect, x, y);
        }
        changed |= self.sync_fwends_height(palette);

        changed
    }

    fn sync_fwends_height(&mut self, palette: &Palette) -> bool {
        let height = self.target_app_height();
        let fwends_rect = self.rects[WidgetId::Fwends.index()];
        if !self.fwends.set_height(height) && fwends_rect.h == self.fwends.height() {
            return false;
        }

        self.rects[WidgetId::Fwends.index()].h = self.fwends.height();
        self.fbs[WidgetId::Fwends.index()] = Framebuffer::new(
            fwends_rect.w,
            self.fwends.height(),
            self.fwends.fill_color(palette),
        );
        true
    }

    const fn focused_rect(&self) -> Rect {
        self.rect_for(self.focus)
    }

    const fn rect_for(&self, widget: WidgetId) -> Rect {
        self.rects[widget.index()]
    }

    fn drain_events(&self) -> puter::TerminalEvents {
        self.puter.drain_terminal_events()
    }

    fn drain_replies(&mut self) -> bool {
        SHOW_FWENDS && self.fwends.drain_reply()
    }

    fn handle_key_press(
        &mut self,
        input: &text_input::KeyInput,
        clipboard_text: Option<&str>,
    ) -> Result<Option<String>, Box<dyn Error>> {
        match self.focus {
            WidgetId::Puter => Ok(self.puter.handle_key_press(input, clipboard_text)),
            WidgetId::Toodle => self.toodle.handle_key_press(input, clipboard_text),
            WidgetId::Fwends if SHOW_FWENDS => {
                Ok(self.fwends.handle_key_press(input, clipboard_text))
            }
            #[allow(clippy::match_same_arms)]
            WidgetId::Fwends => Ok(None),
            WidgetId::Twirl | WidgetId::Wavey | WidgetId::Day => Ok(None),
        }
    }

    /// Returns text the clicked widget wants copied to the clipboard.
    fn click(&mut self, x: i16, y: i16, state: u16) -> Result<Option<String>, Box<dyn Error>> {
        self.puter_pressed = false;
        self.text_drag = None;
        let Some((widget, x, y)) = self.widget_at(x, y) else {
            return Ok(None);
        };
        self.focus = widget;
        match widget {
            WidgetId::Puter => {
                self.puter_pressed = true;
                self.puter.press_button(x, y, state);
            }
            WidgetId::Toodle => {
                if self.toodle.click(x, y)? {
                    self.twirl.spin();
                }
                if self.toodle.text_dragging() {
                    self.text_drag = Some(WidgetId::Toodle);
                }
            }
            WidgetId::Fwends => {
                self.fwends.click(x, y);
                if self.fwends.text_dragging() {
                    self.text_drag = Some(WidgetId::Fwends);
                }
            }
            WidgetId::Twirl => self.twirl.click(x, y),
            WidgetId::Wavey => {
                return Ok(self.wavey.click(x, y));
            }
            WidgetId::Day => self.day.toggle_mode(),
        }
        Ok(None)
    }

    fn release(&mut self, x: i16, y: i16) -> Option<WidgetId> {
        if let Some(widget) = self.text_drag.take() {
            match widget {
                WidgetId::Toodle => self.toodle.end_text_drag(),
                WidgetId::Fwends => self.fwends.end_text_drag(),
                WidgetId::Puter | WidgetId::Twirl | WidgetId::Wavey | WidgetId::Day => {}
            }
            return Some(widget);
        }

        if self.focus == WidgetId::Wavey && self.wavey.release() {
            return Some(WidgetId::Wavey);
        }

        if self.puter_pressed {
            let (x, y) = self.rect_for(WidgetId::Puter).local(x, y);
            self.puter.release_button(x, y);
            self.puter_pressed = false;
            return Some(WidgetId::Puter);
        }

        None
    }

    fn motion(&mut self, x: i16, y: i16) -> Option<WidgetId> {
        if let Some(widget) = self.text_drag {
            let changed = match widget {
                WidgetId::Toodle => {
                    let (local_x, local_y) = self.rect_for(WidgetId::Toodle).local(x, y);
                    self.toodle.drag_text(local_x, local_y)
                }
                WidgetId::Fwends => {
                    let (local_x, local_y) = self.rect_for(WidgetId::Fwends).local(x, y);
                    self.fwends.drag_text(local_x, local_y)
                }
                WidgetId::Puter | WidgetId::Twirl | WidgetId::Wavey | WidgetId::Day => false,
            };
            return changed.then_some(widget);
        }

        if self.focus == WidgetId::Puter {
            let (local_x, local_y) = self.rect_for(WidgetId::Puter).local(x, y);
            if self.puter.motion(local_x, local_y) {
                return Some(WidgetId::Puter);
            }
        }

        if self.focus == WidgetId::Wavey {
            let (local_x, local_y) = self.rect_for(WidgetId::Wavey).local(x, y);
            if self.wavey.motion(local_x, local_y) {
                return Some(WidgetId::Wavey);
            }
        }

        if self.rect_for(WidgetId::Toodle).contains(x, y) {
            let (x, y) = self.rect_for(WidgetId::Toodle).local(x, y);
            self.toodle.hover(x, y).then_some(WidgetId::Toodle)
        } else {
            self.toodle.hover(-1, -1).then_some(WidgetId::Toodle)
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
        let handled = match (widget, direction) {
            (WidgetId::Fwends, ScrollDirection::Up) => {
                self.fwends.scroll_up(x, y);
                true
            }
            (WidgetId::Fwends, ScrollDirection::Down) => {
                self.fwends.scroll_down(x, y);
                true
            }
            (WidgetId::Puter, ScrollDirection::Up) => {
                self.puter.scroll_up();
                true
            }
            (WidgetId::Puter, ScrollDirection::Down) => {
                self.puter.scroll_down();
                true
            }
            (WidgetId::Wavey, ScrollDirection::Up) => self.wavey.scroll_up(x, y),
            (WidgetId::Wavey, ScrollDirection::Down) => self.wavey.scroll_down(x, y),
            (WidgetId::Toodle | WidgetId::Twirl | WidgetId::Day, _) => return None,
        };
        handled.then_some(widget)
    }

    fn widget_at(&self, x: i16, y: i16) -> Option<(WidgetId, i16, i16)> {
        [
            WidgetId::Wavey,
            WidgetId::Day,
            WidgetId::Twirl,
            WidgetId::Fwends,
            WidgetId::Toodle,
            WidgetId::Puter,
        ]
        .into_iter()
        .filter(|widget| widget.is_visible())
        .find_map(|widget| {
            let rect = self.rect_for(widget);
            rect.contains(x, y).then(|| {
                let (x, y) = rect.local(x, y);
                (widget, x, y)
            })
        })
    }

    fn shutdown(&mut self) {
        if let Err(err) = self.toodle.flush_saves() {
            eprintln!("toodle save failed on shutdown: {err}");
        }
        self.wavey.shutdown();
        self.puter.shutdown_terminal();
    }
}

const TOODLE_LEFT_OVERLAP: usize = 24;

/// Widget (x, y) positions, indexed by `WidgetId::index()`.
fn widget_positions(
    puter: &puter::Puter,
    toodle: &toodle::Toodle,
    twirl: &twirl::Twirl,
    wavey: &wavey::Wavey,
    day: &day::Day,
) -> [(usize, usize); WIDGET_COUNT] {
    let left_w = day.width().max(wavey.width());
    let middle_x = left_w + WIDGET_GAP;
    let middle_w = puter.width().max(toodle.width()).max(twirl.width());
    let left_h = day.height() + WIDGET_GAP + wavey.height();
    let middle_h = twirl.height() + WIDGET_GAP + toodle.height() + WIDGET_GAP + puter.height();
    let layout_h = left_h.max(middle_h);
    let wavey_y = layout_h - wavey.height();
    let day_y = wavey_y - WIDGET_GAP - day.height() - 30;
    let puter_y = layout_h - puter.height();
    let toodle_y = puter_y - WIDGET_GAP - toodle.height();
    let twirl_y = toodle_y - WIDGET_GAP - twirl.height();

    // Tweak widget positions here. These final coordinates are used both at startup and
    // after dynamic redraws, so edits in this block won't get snapped back later.
    let puter_x = middle_x + APP_LEFT_PADDING;
    let toodle_x = middle_x.saturating_sub(day.width() + TOODLE_LEFT_OVERLAP) + APP_LEFT_PADDING;
    let twirl_x = middle_x.saturating_sub(day.width()) + APP_LEFT_PADDING;
    let wavey_x = APP_LEFT_PADDING + 32;
    let wavey_y = wavey_y.saturating_sub(6);
    let day_x = wavey.width().saturating_sub(day.width()) + APP_LEFT_PADDING;
    let fwends_x = middle_x + middle_w + WIDGET_GAP + APP_LEFT_PADDING - FWENDS_LEFT_APRON;
    let fwends_y = 0;

    [
        (puter_x, puter_y),
        (toodle_x, toodle_y),
        (fwends_x, fwends_y),
        (twirl_x, twirl_y),
        (wavey_x, wavey_y),
        (day_x, day_y),
    ]
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
    let desk_y = fb.height.saturating_sub(desk.height);
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
            let index = desk_background_index(desk.at(source_x[x - x0], source_y));
            if let Some(color) = palette.resolve(index, x, y) {
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

const fn desk_background_index(index: Index) -> Index {
    match index {
        palette_color::ROSE => app_color::BACKGROUND,
        palette_color::CRIMSON => app_color::BACKGROUND_SHADOW,
        other => other,
    }
}

pub(crate) fn draw_filled_circle(
    fb: &mut Framebuffer,
    center_x: isize,
    center_y: isize,
    radius: isize,
    color: Rgb,
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
    color: Rgb,
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

#[derive(Clone, Copy)]
enum ScrollDirection {
    Up,
    Down,
}

#[allow(clippy::too_many_lines)]
fn main() -> Result<(), Box<dyn Error>> {
    let palette = Palette::load(PALETTE_PATH)?;
    let mut app = App::load(&palette)?;
    let width = app.width();
    let height = app.height();
    let mut fb = Framebuffer::new(width, height, app.fill_color(&palette));
    let mut xwin = XWindow::open(width, height)?;
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

        if app.twirl.update()? {
            app.render_and_draw_widget(&mut fb, &mut xwin, &palette, WidgetId::Twirl)?;
            drew_frame = true;
        }

        if app.wavey.update() {
            app.render_and_draw_widget(&mut fb, &mut xwin, &palette, WidgetId::Wavey)?;
            drew_frame = true;
        }

        if app.day.update() {
            app.render_and_draw_widget(&mut fb, &mut xwin, &palette, WidgetId::Day)?;
            drew_frame = true;
        }

        // Debounced toodle saves: edits hit disk shortly after typing pauses.
        app.toodle.maintain()?;

        let mut pending_motion_widget = None;
        // Coalesce input redraws: key auto-repeat and rapid clicks can deliver
        // many events in a single drain. Repainting per event piles up frames
        // faster than they display and makes typing lag. We update state for
        // every event but repaint the focused widget at most once per batch.
        let mut needs_input_redraw = false;
        while let Some(event) = xwin.conn.poll_for_event()? {
            match event {
                XEvent::Expose(_) => {
                    app.render(&mut fb, &palette);
                    xwin.draw(&fb)?;
                    drew_frame = true;
                    needs_input_redraw = false;
                    pending_motion_widget = None;
                }
                XEvent::KeyPress(event) => {
                    let input = xwin.keyboard.press(event.detail, event.state.into());
                    let paste_text = if should_load_clipboard_for_paste(app.focus, &input) {
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
                    if let Some(widget) = app.motion(event.event_x, event.event_y) {
                        pending_motion_widget = Some(widget);
                    }
                }
                XEvent::SelectionRequest(event) => xwin.handle_selection_request(event)?,
                XEvent::SelectionClear(event) => xwin.handle_selection_clear(event),
                XEvent::DestroyNotify(_) => running = false,
                _ => {}
            }
        }
        if needs_input_redraw {
            redraw_after_input(&mut app, &mut fb, &mut xwin, &palette)?;
            drew_frame = true;
        }
        if let Some(widget) = pending_motion_widget {
            app.render_and_draw_widget(&mut fb, &mut xwin, &palette, widget)?;
            drew_frame = true;
        }

        std::thread::sleep(Duration::from_millis(if drew_frame { 1 } else { 16 }));
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
    if sync_window_layout(app, fb, xwin, palette)? {
        app.render(fb, palette);
        xwin.draw(fb)?;
    } else {
        app.render_focused_widget(fb, palette);
        xwin.draw_rect(fb, app.focused_rect())?;
    }
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

fn is_paste_shortcut(input: &text_input::KeyInput) -> bool {
    input.ctrl()
        && input.shift()
        && (matches!(input.sym_raw(), keysyms::KEY_v | keysyms::KEY_V)
            || input.text().eq_ignore_ascii_case("v"))
}

fn is_plain_paste_shortcut(input: &text_input::KeyInput) -> bool {
    input.ctrl()
        && (matches!(input.sym_raw(), keysyms::KEY_v | keysyms::KEY_V)
            || input.text().eq_ignore_ascii_case("v"))
}

fn should_load_clipboard_for_paste(focus: WidgetId, input: &text_input::KeyInput) -> bool {
    match focus {
        WidgetId::Puter => is_paste_shortcut(input),
        WidgetId::Toodle | WidgetId::Fwends => is_plain_paste_shortcut(input),
        WidgetId::Twirl | WidgetId::Wavey | WidgetId::Day => false,
    }
}
