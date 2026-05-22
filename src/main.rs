use std::error::Error;
use std::time::Duration;

use x11rb::connection::Connection;
use x11rb::protocol::Event as XEvent;
use x11rb::protocol::xproto::ButtonIndex;

mod bitmap_font;
mod comicoro_font;
mod emojimap;
mod fwends;
mod graphics;
mod peanut_money_font;
mod puter;
mod text_input;
mod text_wrap;
mod toodle;
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

#[derive(Clone, Copy)]
enum WidgetId {
    Puter,
    Toodle,
    Fwends,
}

impl WidgetId {
    const ALL: [Self; 3] = [Self::Puter, Self::Toodle, Self::Fwends];
}

struct App {
    puter: puter::Puter,
    toodle: toodle::Toodle,
    fwends: fwends::Fwends,
    puter_fb: Framebuffer,
    toodle_fb: Framebuffer,
    fwends_fb: Framebuffer,
    puter_rect: Rect,
    toodle_rect: Rect,
    fwends_rect: Rect,
    focus: WidgetId,
    puter_pressed: bool,
}

impl App {
    fn load(palette: &Palette) -> Result<Self, Box<dyn Error>> {
        let puter = puter::Puter::load(palette)?;
        let toodle = toodle::Toodle::load(palette)?;
        let fwends = fwends::Fwends::load(palette)?;
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
        let puter_fb = Framebuffer::new(puter_rect.w, puter_rect.h, puter.fill_color(palette));
        let toodle_fb = Framebuffer::new(toodle_rect.w, toodle_rect.h, toodle.fill_color(palette));
        let fwends_fb = Framebuffer::new(fwends_rect.w, fwends_rect.h, fwends.fill_color(palette));

        Ok(Self {
            puter,
            toodle,
            fwends,
            puter_fb,
            toodle_fb,
            fwends_fb,
            puter_rect,
            toodle_rect,
            fwends_rect,
            focus: WidgetId::Toodle,
            puter_pressed: false,
        })
    }

    fn width(&self) -> usize {
        self.toodle_rect
            .x
            .saturating_add(self.toodle_rect.w)
            .max(self.fwends_rect.x + self.fwends_rect.w)
    }

    fn height(&self) -> usize {
        self.puter_rect
            .h
            .max(self.fwends_rect.y + self.fwends_rect.h)
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

    fn focused_rect(&self) -> Rect {
        self.rect_for(self.focus)
    }

    fn rect_for(&self, widget: WidgetId) -> Rect {
        match widget {
            WidgetId::Puter => self.puter_rect,
            WidgetId::Toodle => self.toodle_rect,
            WidgetId::Fwends => self.fwends_rect,
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
            WidgetId::Toodle => self.toodle.click(x, y)?,
            WidgetId::Fwends => self.fwends.click(x, y),
        }
        Ok(())
    }

    fn release(&mut self, x: i16, y: i16) -> bool {
        if self.puter_pressed {
            let (x, y) = self.puter_rect.local(x, y);
            self.puter.release_button(x, y);
            self.puter_pressed = false;
            return true;
        }

        false
    }

    fn motion(&mut self, x: i16, y: i16) -> bool {
        if self.toodle_rect.contains(x, y) {
            let (x, y) = self.toodle_rect.local(x, y);
            self.toodle.hover(x, y)
        } else {
            self.toodle.hover(-1, -1)
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
        match (widget, direction) {
            (WidgetId::Fwends, ScrollDirection::Up) => self.fwends.scroll_up(x, y),
            (WidgetId::Fwends, ScrollDirection::Down) => self.fwends.scroll_down(x, y),
            (WidgetId::Puter, ScrollDirection::Up) => self.puter.scroll_up(),
            (WidgetId::Puter, ScrollDirection::Down) => self.puter.scroll_down(),
            (WidgetId::Toodle, _) => return None,
        }
        Some(widget)
    }

    fn widget_at(&self, x: i16, y: i16) -> Option<(WidgetId, i16, i16)> {
        [WidgetId::Fwends, WidgetId::Toodle, WidgetId::Puter]
            .into_iter()
            .find_map(|widget| {
                let rect = self.rect_for(widget);
                rect.contains(x, y).then(|| {
                    let (x, y) = rect.local(x, y);
                    (widget, x, y)
                })
            })
    }

    fn shutdown(&mut self) {
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
                    app.render_focused_widget(&mut fb, &palette);
                    xwin.draw_rect(&fb, app.focused_rect())?;
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
                        app.render_focused_widget(&mut fb, &palette);
                        xwin.draw_rect(&fb, app.focused_rect())?;
                        drew_frame = true;
                    }
                    _ => {}
                },
                XEvent::ButtonRelease(event) => {
                    if event.detail == u8::from(ButtonIndex::M1)
                        && app.release(event.event_x, event.event_y)
                    {
                        app.render_and_draw_widget(&mut fb, &mut xwin, &palette, WidgetId::Puter)?;
                        drew_frame = true;
                    }
                }
                XEvent::MotionNotify(event) => {
                    if app.motion(event.event_x, event.event_y) {
                        app.render_and_draw_widget(&mut fb, &mut xwin, &palette, WidgetId::Toodle)?;
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
