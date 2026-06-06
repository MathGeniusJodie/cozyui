use std::error::Error;
use std::time::Duration;

use x11rb::connection::Connection;
use x11rb::protocol::Event as XEvent;
use x11rb::protocol::xproto::ButtonIndex;
use xkbcommon::xkb::keysyms;

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
mod text_edit;
mod text_input;
mod text_wrap;
mod toodle;
mod twirl;
mod wavey;
mod x_window;

pub(crate) use graphics::{Framebuffer, Image, Palette, Rect, Rgba, decode_png_with_size};
use x_window::XWindow;

const PALETTE_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/na16-1x.png");
const DESK_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/desk.png");

#[allow(dead_code)]
pub(crate) mod palette_color {
    pub(crate) const LAVENDER: usize = 0;
    pub(crate) const GUNMETAL: usize = 1;
    pub(crate) const PLUM: usize = 2;
    pub(crate) const BROWN: usize = 3;
    pub(crate) const PEACH: usize = 4;
    pub(crate) const CREAM: usize = 5;
    pub(crate) const LIME: usize = 6;
    pub(crate) const GREEN: usize = 7;
    pub(crate) const ORANGE: usize = 8;
    pub(crate) const CRIMSON: usize = 9;
    pub(crate) const ROSE: usize = 10;
    pub(crate) const PURPLE: usize = 11;
    pub(crate) const CYAN: usize = 12;
    pub(crate) const BLUE: usize = 13;
    pub(crate) const PINE: usize = 14;
    pub(crate) const BLACK: usize = 15;
}

#[allow(dead_code)]
pub(crate) mod app_color {
    use crate::palette_color;

    pub(crate) const BACKGROUND: usize = palette_color::ROSE;
    pub(crate) const BACKGROUND_SHADOW: usize = palette_color::CRIMSON;
}

const WHEEL_UP: u8 = 4;
const WHEEL_DOWN: u8 = 5;

const WIDGET_GAP: usize = 16;
const APP_LEFT_PADDING: usize = 54;
const APP_BOTTOM_PADDING: usize = 54;
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

    fn visible() -> &'static [Self] {
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
    desk: Image,
    puter_fb: Framebuffer,
    toodle_fb: Framebuffer,
    fwends_fb: Framebuffer,
    twirl_fb: Framebuffer,
    wavey_fb: Framebuffer,
    day_fb: Framebuffer,
    puter_rect: Rect,
    toodle_rect: Rect,
    fwends_rect: Rect,
    twirl_rect: Rect,
    wavey_rect: Rect,
    day_rect: Rect,
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
        let desk = Image::load(DESK_PATH, palette)?;
        let layout = WidgetLayout::new(&puter, &toodle, &twirl, &wavey, &day);
        let puter_rect = Rect {
            x: layout.puter_x,
            y: layout.puter_y,
            w: puter.width(),
            h: puter.height(),
        };
        let toodle_rect = Rect {
            x: layout.toodle_x,
            y: layout.toodle_y,
            w: toodle.width(),
            h: toodle.height(),
        };
        let fwends_rect = Rect {
            x: layout.fwends_x,
            y: layout.fwends_y,
            w: fwends.width(),
            h: fwends.height(),
        };
        let twirl_rect = Rect {
            x: layout.twirl_x,
            y: layout.twirl_y,
            w: twirl.width(),
            h: twirl.height(),
        };
        let wavey_rect = Rect {
            x: layout.wavey_x,
            y: layout.wavey_y,
            w: wavey.width(),
            h: wavey.height(),
        };
        let day_rect = Rect {
            x: layout.day_x,
            y: layout.day_y,
            w: day.width(),
            h: day.height(),
        };
        let puter_fb = Framebuffer::new(puter_rect.w, puter_rect.h, puter.fill_color(palette));
        let toodle_fb = Framebuffer::new(toodle_rect.w, toodle_rect.h, toodle.fill_color(palette));
        let fwends_fb = Framebuffer::new(fwends_rect.w, fwends_rect.h, fwends.fill_color(palette));
        let twirl_fb = Framebuffer::new(twirl_rect.w, twirl_rect.h, twirl.fill_color(palette));
        let wavey_fb = Framebuffer::new(wavey_rect.w, wavey_rect.h, wavey.fill_color(palette));
        let day_fb = Framebuffer::new(day_rect.w, day_rect.h, day.fill_color(palette));

        let mut app = Self {
            puter,
            toodle,
            fwends,
            twirl,
            wavey,
            day,
            desk,
            puter_fb,
            toodle_fb,
            fwends_fb,
            twirl_fb,
            wavey_fb,
            day_fb,
            puter_rect,
            toodle_rect,
            fwends_rect,
            twirl_rect,
            wavey_rect,
            day_rect,
            focus: WidgetId::Toodle,
            puter_pressed: false,
            text_drag: None,
        };
        app.sync_fwends_height(palette);
        Ok(app)
    }

    fn width(&self) -> usize {
        let width = self
            .toodle_rect
            .x
            .saturating_add(self.toodle_rect.w)
            .max(self.desk.width)
            .max(self.twirl_rect.x + self.twirl_rect.w)
            .max(self.wavey_rect.x + self.wavey_rect.w)
            .max(self.day_rect.x + self.day_rect.w);
        if SHOW_FWENDS {
            width.max(self.fwends_rect.x + self.fwends_rect.w)
        } else {
            width
        }
    }

    fn height(&self) -> usize {
        let height = self.target_app_height();
        if SHOW_FWENDS {
            height.max(self.fwends_rect.y + self.fwends_rect.h)
        } else {
            height
        }
    }

    fn target_app_height(&self) -> usize {
        let height = self
            .puter_rect
            .h
            .max(self.desk.height)
            .max(self.twirl_rect.y + self.twirl_rect.h)
            .max(self.wavey_rect.y + self.wavey_rect.h)
            .max(self.day_rect.y + self.day_rect.h);
        let height = if SHOW_FWENDS {
            height.max(self.fwends_rect.y + self.fwends.min_height())
        } else {
            height
        };
        height + APP_BOTTOM_PADDING
    }

    fn fill_color(&self, palette: &Palette) -> Rgba {
        palette.color(app_color::BACKGROUND)
    }

    fn render_background(&self, fb: &mut Framebuffer, palette: &Palette) {
        fb.clear(self.fill_color(palette));
        let y = fb.height.saturating_sub(self.desk.height) as isize;
        fb.draw_image(&self.desk, 0, y, 1);
    }

    fn render_background_rect(&self, fb: &mut Framebuffer, palette: &Palette, rect: Rect) {
        fb.fill_rect(rect.x, rect.y, rect.w, rect.h, self.fill_color(palette));

        let desk_y = fb.height.saturating_sub(self.desk.height);
        let x0 = rect.x.min(self.desk.width);
        let x1 = rect.x.saturating_add(rect.w).min(self.desk.width);
        let y0 = rect.y.max(desk_y);
        let y1 = rect
            .y
            .saturating_add(rect.h)
            .min(desk_y.saturating_add(self.desk.height));
        if x0 < x1 && y0 < y1 {
            fb.draw_image_region(
                &self.desk,
                Rect::new(x0, y0 - desk_y, x1 - x0, y1 - y0),
                x0 as isize,
                y0 as isize,
                1,
            );
        }
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
        match widget {
            WidgetId::Puter => {
                self.puter_fb.clear(self.puter.fill_color(palette));
                self.puter.render(&mut self.puter_fb, palette);
                fb.blit_from(&self.puter_fb, self.puter_rect.x, self.puter_rect.y);
            }
            WidgetId::Toodle => {
                self.toodle_fb.clear(self.toodle.fill_color(palette));
                self.toodle.render(&mut self.toodle_fb, palette);
                fb.blit_from(&self.toodle_fb, self.toodle_rect.x, self.toodle_rect.y);
            }
            WidgetId::Fwends => {
                self.fwends_fb.clear(self.fwends.fill_color(palette));
                self.fwends.render(&mut self.fwends_fb, palette);
                fb.blit_from(&self.fwends_fb, self.fwends_rect.x, self.fwends_rect.y);
            }
            WidgetId::Twirl => {
                self.twirl_fb.clear(self.twirl.fill_color(palette));
                self.twirl.render(&mut self.twirl_fb, palette);
                fb.blit_from(&self.twirl_fb, self.twirl_rect.x, self.twirl_rect.y);
            }
            WidgetId::Wavey => {
                self.wavey_fb.clear(self.wavey.fill_color(palette));
                self.wavey.render(&mut self.wavey_fb, palette);
                fb.blit_from(&self.wavey_fb, self.wavey_rect.x, self.wavey_rect.y);
            }
            WidgetId::Day => {
                self.day_fb.clear(self.day.fill_color(palette));
                self.day.render(&mut self.day_fb, palette);
                fb.blit_from(&self.day_fb, self.day_rect.x, self.day_rect.y);
            }
        }
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

        if self.toodle_rect.w != toodle_w || self.toodle_rect.h != toodle_h {
            self.toodle_rect.w = toodle_w;
            self.toodle_rect.h = toodle_h;
            self.toodle_fb = Framebuffer::new(toodle_w, toodle_h, self.toodle.fill_color(palette));
            changed = true;
        }

        let layout = WidgetLayout::new(
            &self.puter,
            &self.toodle,
            &self.twirl,
            &self.wavey,
            &self.day,
        );
        changed |= move_rect(&mut self.puter_rect, layout.puter_x, layout.puter_y);
        changed |= move_rect(&mut self.toodle_rect, layout.toodle_x, layout.toodle_y);
        changed |= move_rect(&mut self.fwends_rect, layout.fwends_x, layout.fwends_y);
        changed |= move_rect(&mut self.twirl_rect, layout.twirl_x, layout.twirl_y);
        changed |= move_rect(&mut self.wavey_rect, layout.wavey_x, layout.wavey_y);
        changed |= move_rect(&mut self.day_rect, layout.day_x, layout.day_y);
        changed |= self.sync_fwends_height(palette);

        changed
    }

    fn sync_fwends_height(&mut self, palette: &Palette) -> bool {
        let height = self.target_app_height();
        if !self.fwends.set_height(height) && self.fwends_rect.h == self.fwends.height() {
            return false;
        }

        self.fwends_rect.h = self.fwends.height();
        self.fwends_fb = Framebuffer::new(
            self.fwends_rect.w,
            self.fwends_rect.h,
            self.fwends.fill_color(palette),
        );
        true
    }

    fn focused_rect(&self) -> Rect {
        self.rect_for(self.focus)
    }

    fn rect_for(&self, widget: WidgetId) -> Rect {
        match widget {
            WidgetId::Puter => self.puter_rect,
            WidgetId::Toodle => self.toodle_rect,
            WidgetId::Fwends => self.fwends_rect,
            WidgetId::Twirl => self.twirl_rect,
            WidgetId::Wavey => self.wavey_rect,
            WidgetId::Day => self.day_rect,
        }
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
            WidgetId::Fwends if SHOW_FWENDS => self.fwends.handle_key_press(input, clipboard_text),
            WidgetId::Fwends => Ok(None),
            WidgetId::Twirl | WidgetId::Wavey | WidgetId::Day => Ok(None),
        }
    }

    fn click(&mut self, x: i16, y: i16, state: u16) -> Result<(), Box<dyn Error>> {
        self.puter_pressed = false;
        self.text_drag = None;
        let Some((widget, x, y)) = self.widget_at(x, y) else {
            return Ok(());
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
                self.wavey.click(x, y);
            }
            WidgetId::Day => self.day.toggle_mode(),
        }
        Ok(())
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
            let (x, y) = self.puter_rect.local(x, y);
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
                    let (local_x, local_y) = self.toodle_rect.local(x, y);
                    self.toodle.drag_text(local_x, local_y)
                }
                WidgetId::Fwends => {
                    let (local_x, local_y) = self.fwends_rect.local(x, y);
                    self.fwends.drag_text(local_x, local_y)
                }
                WidgetId::Puter | WidgetId::Twirl | WidgetId::Wavey | WidgetId::Day => false,
            };
            return changed.then_some(widget);
        }

        if self.focus == WidgetId::Puter {
            let (local_x, local_y) = self.puter_rect.local(x, y);
            if self.puter.motion(local_x, local_y) {
                return Some(WidgetId::Puter);
            }
        }

        if self.focus == WidgetId::Wavey {
            let (local_x, local_y) = self.wavey_rect.local(x, y);
            if self.wavey.motion(local_x, local_y) {
                return Some(WidgetId::Wavey);
            }
        }

        if self.toodle_rect.contains(x, y) {
            let (x, y) = self.toodle_rect.local(x, y);
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

    fn update_twirl(&mut self) -> Result<bool, Box<dyn Error>> {
        self.twirl.update()
    }

    fn update_wavey(&mut self) -> bool {
        self.wavey.update()
    }

    fn update_day(&mut self) -> bool {
        self.day.update()
    }

    fn shutdown(&mut self) {
        self.wavey.shutdown();
        self.puter.shutdown_terminal();
    }
}

const TOODLE_LEFT_OVERLAP: usize = 24;

struct WidgetLayout {
    puter_x: usize,
    toodle_x: usize,
    fwends_x: usize,
    fwends_y: usize,
    twirl_x: usize,
    wavey_x: usize,
    day_x: usize,
    twirl_y: usize,
    toodle_y: usize,
    puter_y: usize,
    day_y: usize,
    wavey_y: usize,
}

impl WidgetLayout {
    fn new(
        puter: &puter::Puter,
        toodle: &toodle::Toodle,
        twirl: &twirl::Twirl,
        wavey: &wavey::Wavey,
        day: &day::Day,
    ) -> Self {
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
        let toodle_x =
            middle_x.saturating_sub(day.width() + TOODLE_LEFT_OVERLAP) + APP_LEFT_PADDING;
        let twirl_x = middle_x.saturating_sub(day.width()) + APP_LEFT_PADDING;
        let wavey_x = APP_LEFT_PADDING + 32;
        let wavey_y = wavey_y + 14;
        let day_x = wavey.width().saturating_sub(day.width()) + APP_LEFT_PADDING;
        let fwends_x = middle_x + middle_w + WIDGET_GAP + APP_LEFT_PADDING - FWENDS_LEFT_APRON;
        let fwends_y = 0;

        Self {
            puter_x,
            toodle_x,
            fwends_x,
            fwends_y,
            twirl_x,
            wavey_x,
            day_x,
            twirl_y,
            toodle_y,
            puter_y,
            day_y,
            wavey_y,
        }
    }
}

fn move_rect(rect: &mut Rect, x: usize, y: usize) -> bool {
    if rect.x == x && rect.y == y {
        return false;
    }

    rect.x = x;
    rect.y = y;
    true
}

fn rects_intersect(a: Rect, b: Rect) -> bool {
    a.x < b.x.saturating_add(b.w)
        && b.x < a.x.saturating_add(a.w)
        && a.y < b.y.saturating_add(b.h)
        && b.y < a.y.saturating_add(a.h)
}

pub(crate) fn draw_filled_circle(
    fb: &mut Framebuffer,
    center_x: isize,
    center_y: isize,
    radius: isize,
    color: Rgba,
) {
    draw_filled_ellipse(fb, center_x, center_y, radius, radius, color);
}

pub(crate) fn draw_filled_ellipse(
    fb: &mut Framebuffer,
    center_x: isize,
    center_y: isize,
    radius_x: isize,
    radius_y: isize,
    color: Rgba,
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
                fb.fill_rect(x as usize, y as usize, 1, 1, color);
            }
        }
    }
}

#[derive(Clone, Copy)]
enum ScrollDirection {
    Up,
    Down,
}

fn main() -> Result<(), Box<dyn Error>> {
    let palette = Palette::load(PALETTE_PATH)?;
    let mut app = App::load(&palette)?;
    let width = app.width();
    let height = app.height();
    let mut fb = Framebuffer::new(width, height, app.fill_color(&palette));
    let mut xwin = XWindow::open(width, height)?;
    app.start(xwin.window as u64)?;
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

        if app.update_twirl()? {
            app.render_and_draw_widget(&mut fb, &mut xwin, &palette, WidgetId::Twirl)?;
            drew_frame = true;
        }

        if app.update_wavey() {
            app.render_and_draw_widget(&mut fb, &mut xwin, &palette, WidgetId::Wavey)?;
            drew_frame = true;
        }

        if app.update_day() {
            app.render_and_draw_widget(&mut fb, &mut xwin, &palette, WidgetId::Day)?;
            drew_frame = true;
        }

        let mut pending_motion_widget = None;
        while let Some(event) = xwin.conn.poll_for_event()? {
            match event {
                XEvent::Expose(_) => {
                    app.render(&mut fb, &palette);
                    xwin.draw(&fb)?;
                    drew_frame = true;
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
                    if sync_window_layout(&mut app, &mut fb, &mut xwin, &palette)? {
                        app.render(&mut fb, &palette);
                        xwin.draw(&fb)?;
                    } else {
                        app.render_focused_widget(&mut fb, &palette);
                        xwin.draw_rect(&fb, app.focused_rect())?;
                    }
                    drew_frame = true;
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
                        app.click(event.event_x, event.event_y, event.state.into())?;
                        if sync_window_layout(&mut app, &mut fb, &mut xwin, &palette)? {
                            app.render(&mut fb, &palette);
                            xwin.draw(&fb)?;
                        } else {
                            app.render_focused_widget(&mut fb, &palette);
                            xwin.draw_rect(&fb, app.focused_rect())?;
                        }
                        drew_frame = true;
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
        if let Some(widget) = pending_motion_widget {
            app.render_and_draw_widget(&mut fb, &mut xwin, &palette, widget)?;
            drew_frame = true;
        }

        std::thread::sleep(Duration::from_millis(if drew_frame { 1 } else { 16 }));
    }

    app.shutdown();
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
