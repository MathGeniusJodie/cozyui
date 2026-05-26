use std::error::Error;
use std::time::Duration;

use x11rb::connection::Connection;
use x11rb::protocol::Event as XEvent;
use x11rb::protocol::xproto::ButtonIndex;

mod alarm;
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
mod text_input;
mod text_wrap;
mod toodle;
mod twirl;
mod x_window;

pub(crate) use graphics::{Framebuffer, Image, Palette, Rect, Rgba, decode_png_with_size};
use x_window::XWindow;

const PALETTE_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/na16-1x.png");

#[allow(dead_code)]
pub(crate) mod palette_color {
    pub(super) const LAVENDER: usize = 0;
    pub(super) const GUNMETAL: usize = 1;
    pub(super) const PLUM: usize = 2;
    pub(super) const BROWN: usize = 3;
    pub(super) const PEACH: usize = 4;
    pub(super) const CREAM: usize = 5;
    pub(super) const LIME: usize = 6;
    pub(super) const GREEN: usize = 7;
    pub(super) const ORANGE: usize = 8;
    pub(super) const CRIMSON: usize = 9;
    pub(super) const ROSE: usize = 10;
    pub(super) const PURPLE: usize = 11;
    pub(super) const CYAN: usize = 12;
    pub(super) const BLUE: usize = 13;
    pub(super) const PINE: usize = 14;
    pub(super) const BLACK: usize = 15;
}

const WHEEL_UP: u8 = 4;
const WHEEL_DOWN: u8 = 5;

const WIDGET_GAP: usize = 16;

#[derive(Clone, Copy, PartialEq, Eq)]
enum WidgetId {
    Puter,
    Toodle,
    Fwends,
    Twirl,
    Alarm,
    Day,
}

impl WidgetId {
    const ALL: [Self; 6] = [
        Self::Puter,
        Self::Toodle,
        Self::Fwends,
        Self::Twirl,
        Self::Alarm,
        Self::Day,
    ];
}

struct App {
    puter: puter::Puter,
    toodle: toodle::Toodle,
    fwends: fwends::Fwends,
    twirl: twirl::Twirl,
    alarm: alarm::Alarm,
    day: day::Day,
    puter_fb: Framebuffer,
    toodle_fb: Framebuffer,
    fwends_fb: Framebuffer,
    twirl_fb: Framebuffer,
    alarm_fb: Framebuffer,
    day_fb: Framebuffer,
    puter_rect: Rect,
    toodle_rect: Rect,
    fwends_rect: Rect,
    twirl_rect: Rect,
    alarm_rect: Rect,
    day_rect: Rect,
    focus: WidgetId,
    puter_pressed: bool,
}

impl App {
    fn load(palette: &Palette) -> Result<Self, Box<dyn Error>> {
        let puter = puter::Puter::load(palette)?;
        let toodle = toodle::Toodle::load(palette)?;
        let fwends = fwends::Fwends::load(palette)?;
        let twirl = twirl::Twirl::load(palette)?;
        let alarm = alarm::Alarm::load(palette)?;
        let day = day::Day::load()?;
        let puter_rect = Rect {
            x: 0,
            y: 0,
            w: puter.width(),
            h: puter.height(),
        };
        let toodle_rect = Rect {
            x: puter.width() + WIDGET_GAP,
            y: 0,
            w: toodle.width(),
            h: toodle.height(),
        };
        let fwends_rect = Rect {
            x: puter.width() + WIDGET_GAP,
            y: toodle.height() + WIDGET_GAP,
            w: fwends.width(),
            h: fwends.height(),
        };
        let twirl_rect = Rect {
            x: toodle_rect.x + toodle.width() + WIDGET_GAP,
            y: 0,
            w: twirl.width(),
            h: twirl.height(),
        };
        let alarm_rect = Rect {
            x: twirl_rect.x,
            y: twirl.height() + WIDGET_GAP,
            w: alarm.width(),
            h: alarm.height(),
        };
        let day_rect = Rect {
            x: twirl_rect.x,
            y: alarm_rect.y + alarm.height() + WIDGET_GAP,
            w: day.width(),
            h: day.height(),
        };
        let puter_fb = Framebuffer::new(puter_rect.w, puter_rect.h, puter.fill_color(palette));
        let toodle_fb = Framebuffer::new(toodle_rect.w, toodle_rect.h, toodle.fill_color(palette));
        let fwends_fb = Framebuffer::new(fwends_rect.w, fwends_rect.h, fwends.fill_color(palette));
        let twirl_fb = Framebuffer::new(twirl_rect.w, twirl_rect.h, twirl.fill_color(palette));
        let alarm_fb = Framebuffer::new(alarm_rect.w, alarm_rect.h, alarm.fill_color(palette));
        let day_fb = Framebuffer::new(day_rect.w, day_rect.h, day.fill_color(palette));

        Ok(Self {
            puter,
            toodle,
            fwends,
            twirl,
            alarm,
            day,
            puter_fb,
            toodle_fb,
            fwends_fb,
            twirl_fb,
            alarm_fb,
            day_fb,
            puter_rect,
            toodle_rect,
            fwends_rect,
            twirl_rect,
            alarm_rect,
            day_rect,
            focus: WidgetId::Toodle,
            puter_pressed: false,
        })
    }

    fn width(&self) -> usize {
        self.toodle_rect
            .x
            .saturating_add(self.toodle_rect.w)
            .max(self.fwends_rect.x + self.fwends_rect.w)
            .max(self.twirl_rect.x + self.twirl_rect.w)
            .max(self.alarm_rect.x + self.alarm_rect.w)
            .max(self.day_rect.x + self.day_rect.w)
    }

    fn height(&self) -> usize {
        self.puter_rect
            .h
            .max(self.fwends_rect.y + self.fwends_rect.h)
            .max(self.twirl_rect.y + self.twirl_rect.h)
            .max(self.alarm_rect.y + self.alarm_rect.h)
            .max(self.day_rect.y + self.day_rect.h)
    }

    fn fill_color(&self, palette: &Palette) -> Rgba {
        palette.color(palette_color::BLACK)
    }

    fn start(&mut self, window_id: u64) -> Result<(), Box<dyn Error>> {
        self.puter.start_terminal(window_id)
    }

    fn render(&mut self, fb: &mut Framebuffer, palette: &Palette) {
        fb.clear(self.fill_color(palette));
        for widget in WidgetId::ALL {
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
            WidgetId::Alarm => {
                self.alarm_fb.clear(self.alarm.fill_color(palette));
                self.alarm.render(&mut self.alarm_fb, palette);
                fb.blit_from(&self.alarm_fb, self.alarm_rect.x, self.alarm_rect.y);
            }
            WidgetId::Day => {
                self.day_fb.clear(self.day.fill_color(palette));
                self.day.render(&mut self.day_fb, palette);
                fb.blit_from(&self.day_fb, self.day_rect.x, self.day_rect.y);
            }
        }
    }

    fn render_focused_widget(&mut self, fb: &mut Framebuffer, palette: &Palette) {
        self.render_widget(fb, palette, self.focus);
    }

    fn render_and_draw_widget(
        &mut self,
        fb: &mut Framebuffer,
        xwin: &mut XWindow,
        palette: &Palette,
        widget: WidgetId,
    ) -> Result<(), Box<dyn Error>> {
        self.render_widget(fb, palette, widget);
        xwin.draw_rect(fb, self.rect_for(widget))
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

        let fwends_y = self.toodle_rect.y + self.toodle_rect.h + WIDGET_GAP;
        if self.fwends_rect.y != fwends_y {
            self.fwends_rect.y = fwends_y;
            changed = true;
        }

        let twirl_x = self.toodle_rect.x + self.toodle_rect.w + WIDGET_GAP;
        if self.twirl_rect.x != twirl_x {
            self.twirl_rect.x = twirl_x;
            changed = true;
        }

        let alarm_x = self.twirl_rect.x;
        let alarm_y = self.twirl_rect.y + self.twirl_rect.h + WIDGET_GAP;
        if self.alarm_rect.x != alarm_x || self.alarm_rect.y != alarm_y {
            self.alarm_rect.x = alarm_x;
            self.alarm_rect.y = alarm_y;
            changed = true;
        }

        let day_x = self.twirl_rect.x;
        let day_y = self.alarm_rect.y + self.alarm_rect.h + WIDGET_GAP;
        if self.day_rect.x != day_x || self.day_rect.y != day_y {
            self.day_rect.x = day_x;
            self.day_rect.y = day_y;
            changed = true;
        }

        changed
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
            WidgetId::Alarm => self.alarm_rect,
            WidgetId::Day => self.day_rect,
        }
    }

    fn drain_events(&self) -> puter::TerminalEvents {
        self.puter.drain_terminal_events()
    }

    fn drain_replies(&mut self) -> bool {
        self.fwends.drain_reply()
    }

    fn handle_key_press(&mut self, input: &text_input::KeyInput) -> Result<(), Box<dyn Error>> {
        match self.focus {
            WidgetId::Puter => {
                self.puter.handle_key_press(input);
                Ok(())
            }
            WidgetId::Toodle => self.toodle.handle_key_press(input),
            WidgetId::Fwends => self.fwends.handle_key_press(input),
            WidgetId::Twirl | WidgetId::Alarm | WidgetId::Day => Ok(()),
        }
    }

    fn click(&mut self, x: i16, y: i16) -> Result<(), Box<dyn Error>> {
        self.puter_pressed = false;
        let Some((widget, x, y)) = self.widget_at(x, y) else {
            return Ok(());
        };
        self.focus = widget;
        match widget {
            WidgetId::Puter => {
                self.puter_pressed = true;
                self.puter.press_button(x, y);
            }
            WidgetId::Toodle => {
                if self.toodle.click(x, y)? {
                    self.twirl.spin();
                }
            }
            WidgetId::Fwends => self.fwends.click(x, y),
            WidgetId::Twirl => self.twirl.click(x, y),
            WidgetId::Alarm => {
                self.alarm.click(x, y);
            }
            WidgetId::Day => {}
        }
        Ok(())
    }

    fn release(&mut self, x: i16, y: i16) -> Option<WidgetId> {
        if self.focus == WidgetId::Alarm && self.alarm.release() {
            return Some(WidgetId::Alarm);
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
        if self.focus == WidgetId::Alarm {
            let (local_x, local_y) = self.alarm_rect.local(x, y);
            if self.alarm.motion(local_x, local_y) {
                return Some(WidgetId::Alarm);
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
            (WidgetId::Alarm, ScrollDirection::Up) => self.alarm.scroll_up(x, y),
            (WidgetId::Alarm, ScrollDirection::Down) => self.alarm.scroll_down(x, y),
            (WidgetId::Toodle | WidgetId::Twirl | WidgetId::Day, _) => return None,
        };
        handled.then_some(widget)
    }

    fn widget_at(&self, x: i16, y: i16) -> Option<(WidgetId, i16, i16)> {
        [
            WidgetId::Alarm,
            WidgetId::Day,
            WidgetId::Twirl,
            WidgetId::Fwends,
            WidgetId::Toodle,
            WidgetId::Puter,
        ]
        .into_iter()
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

    fn update_alarm(&mut self) -> bool {
        self.alarm.update()
    }

    fn update_day(&mut self) -> bool {
        self.day.update()
    }

    fn shutdown(&mut self) {
        self.alarm.shutdown();
        self.puter.shutdown_terminal();
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

        if app.update_alarm() {
            app.render_and_draw_widget(&mut fb, &mut xwin, &palette, WidgetId::Alarm)?;
            drew_frame = true;
        }

        if app.update_day() {
            app.render_and_draw_widget(&mut fb, &mut xwin, &palette, WidgetId::Day)?;
            drew_frame = true;
        }

        while let Some(event) = xwin.conn.poll_for_event()? {
            match event {
                XEvent::Expose(_) => {
                    app.render(&mut fb, &palette);
                    xwin.draw(&fb)?;
                    drew_frame = true;
                }
                XEvent::KeyPress(event) => {
                    let input = xwin.keyboard.press(event.detail, event.state.into());
                    app.handle_key_press(&input)?;
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
                        app.click(event.event_x, event.event_y)?;
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
                        app.render_and_draw_widget(&mut fb, &mut xwin, &palette, widget)?;
                        drew_frame = true;
                    }
                }
                XEvent::MotionNotify(event) => {
                    if let Some(widget) = app.motion(event.event_x, event.event_y) {
                        app.render_and_draw_widget(&mut fb, &mut xwin, &palette, widget)?;
                        drew_frame = true;
                    }
                }
                XEvent::DestroyNotify(_) => running = false,
                _ => {}
            }
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
