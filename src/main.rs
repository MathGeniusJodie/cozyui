use std::error::Error;
use std::fs::File;
use std::io::BufReader;
use std::time::{Duration, Instant};

use x11rb::connection::Connection;
use x11rb::protocol::Event as XEvent;
use x11rb::protocol::xproto::ConnectionExt as XprotoConnectionExt;
use x11rb::protocol::xproto::{
    AtomEnum, ButtonIndex, ChangeWindowAttributesAux, CreateGCAux, CreateWindowAux, EventMask,
    Gcontext, ImageFormat, PropMode, Window, WindowClass,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

mod comicoro_font;
mod fwends;
mod peanut_money_font;
mod puter;
mod toodle;

const PALETTE_PATH: &str = "na16-1x.png";

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

#[derive(Clone, Copy)]
pub(crate) struct Rgba {
    pub(crate) r: u8,
    pub(crate) g: u8,
    pub(crate) b: u8,
    pub(crate) a: u8,
}

pub(crate) struct Palette {
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

    pub(crate) fn color(&self, index: usize) -> Rgba {
        self.colors[index % self.colors.len()]
    }

    pub(crate) fn nearest(&self, color: Rgba) -> Rgba {
        self.colors
            .iter()
            .copied()
            .min_by_key(|candidate| color_distance(*candidate, color))
            .unwrap_or(self.colors[0])
    }

    pub(crate) fn closest_to_white(&self) -> Rgba {
        self.nearest(Rgba {
            r: 255,
            g: 255,
            b: 255,
            a: 255,
        })
    }

    pub(crate) fn darkest(&self) -> Rgba {
        self.colors
            .iter()
            .copied()
            .min_by_key(|color| color.r as u16 + color.g as u16 + color.b as u16)
            .unwrap_or(self.colors[0])
    }
}

pub(crate) struct Image {
    pub(crate) width: usize,
    pub(crate) height: usize,
    pixels: Vec<Rgba>,
}

impl Image {
    pub(crate) fn load(path: &str, palette: &Palette) -> Result<Self, Box<dyn Error>> {
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

    pub(crate) fn at(&self, x: usize, y: usize) -> Rgba {
        self.pixels[y * self.width + x]
    }
}

pub(crate) struct Framebuffer {
    pub(crate) width: usize,
    pub(crate) height: usize,
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

    pub(crate) fn clear(&mut self, color: Rgba) {
        self.pixels.fill(color);
    }

    pub(crate) fn clear_scaled(&mut self, image: &Image, scale: usize) {
        for y in 0..self.height {
            for x in 0..self.width {
                let sx = (x / scale).min(image.width - 1);
                let sy = (y / scale).min(image.height - 1);
                self.pixels[y * self.width + x] = image.at(sx, sy);
            }
        }
    }

    pub(crate) fn fill_rect(&mut self, x: usize, y: usize, w: usize, h: usize, color: Rgba) {
        for py in y..(y + h).min(self.height) {
            for px in x..(x + w).min(self.width) {
                self.pixels[py * self.width + px] = color;
            }
        }
    }

    pub(crate) fn draw_scaled_region(
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
                if color.a == 0 {
                    continue;
                }
                self.fill_rect(dest_x + x * scale, dest_y + y * scale, scale, scale, color);
            }
        }
    }

    fn blit_from(&mut self, src: &Framebuffer, dest_x: usize, dest_y: usize) {
        for y in 0..src.height {
            for x in 0..src.width {
                let px = dest_x + x;
                let py = dest_y + y;
                if px >= self.width || py >= self.height {
                    continue;
                }
                self.pixels[py * self.width + px] = src.pixels[y * src.width + x];
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
                    | EventMask::POINTER_MOTION
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
                    | EventMask::POINTER_MOTION
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

fn decode_png(path: &str) -> Result<Vec<Rgba>, Box<dyn Error>> {
    Ok(decode_png_with_size(path)?.2)
}

pub(crate) fn decode_png_with_size(
    path: &str,
) -> Result<(usize, usize, Vec<Rgba>), Box<dyn Error>> {
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

const WIDGET_GAP: usize = 16;

#[derive(Clone, Copy)]
struct Rect {
    x: usize,
    y: usize,
    w: usize,
    h: usize,
}

impl Rect {
    fn contains(self, x: i16, y: i16) -> bool {
        let x = x.max(0) as usize;
        let y = y.max(0) as usize;
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }

    fn local(self, x: i16, y: i16) -> (i16, i16) {
        (x - self.x as i16, y - self.y as i16)
    }
}

#[derive(Clone, Copy)]
enum FocusedWidget {
    Puter,
    Toodle,
    Fwends,
}

struct App {
    puter: puter::Puter,
    toodle: toodle::Toodle,
    fwends: fwends::Fwends,
    puter_rect: Rect,
    toodle_rect: Rect,
    fwends_rect: Rect,
    focus: FocusedWidget,
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

        Ok(Self {
            puter,
            toodle,
            fwends,
            puter_rect,
            toodle_rect,
            fwends_rect,
            focus: FocusedWidget::Toodle,
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

    fn render(&self, fb: &mut Framebuffer, palette: &Palette) {
        fb.clear(self.fill_color(palette));

        let mut puter_fb = Framebuffer::new(
            self.puter_rect.w,
            self.puter_rect.h,
            self.puter.fill_color(palette),
        );
        self.puter.render(&mut puter_fb, palette);
        fb.blit_from(&puter_fb, self.puter_rect.x, self.puter_rect.y);

        let mut toodle_fb = Framebuffer::new(
            self.toodle_rect.w,
            self.toodle_rect.h,
            self.toodle.fill_color(palette),
        );
        self.toodle.render(&mut toodle_fb, palette);
        fb.blit_from(&toodle_fb, self.toodle_rect.x, self.toodle_rect.y);

        let mut fwends_fb = Framebuffer::new(
            self.fwends_rect.w,
            self.fwends_rect.h,
            self.fwends.fill_color(palette),
        );
        self.fwends.render(&mut fwends_fb, palette);
        fb.blit_from(&fwends_fb, self.fwends_rect.x, self.fwends_rect.y);
    }

    fn drain_events(&self) -> bool {
        self.puter.drain_terminal_events()
    }

    fn drain_replies(&mut self) -> bool {
        self.fwends.drain_reply()
    }

    fn handle_key_press(&mut self, keycode: u8, state: u16) -> Result<(), Box<dyn Error>> {
        match self.focus {
            FocusedWidget::Puter => {
                self.puter.handle_key_press(keycode, state);
                Ok(())
            }
            FocusedWidget::Toodle => self.toodle.handle_key_press(keycode, state),
            FocusedWidget::Fwends => self.fwends.handle_key_press(keycode, state),
        }
    }

    fn click(&mut self, x: i16, y: i16) -> Result<(), Box<dyn Error>> {
        self.puter_pressed = false;
        if self.fwends_rect.contains(x, y) {
            let (x, y) = self.fwends_rect.local(x, y);
            self.focus = FocusedWidget::Fwends;
            self.fwends.click(x, y);
            return Ok(());
        }

        if self.toodle_rect.contains(x, y) {
            let (x, y) = self.toodle_rect.local(x, y);
            self.focus = FocusedWidget::Toodle;
            self.toodle.click(x, y)?;
            return Ok(());
        }

        if self.puter_rect.contains(x, y) {
            let (x, y) = self.puter_rect.local(x, y);
            self.focus = FocusedWidget::Puter;
            self.puter_pressed = true;
            self.puter.press_button(x, y);
            return Ok(());
        }

        Ok(())
    }

    fn release(&mut self, x: i16, y: i16) {
        if self.puter_pressed {
            let (x, y) = self.puter_rect.local(x, y);
            self.puter.release_button(x, y);
            self.puter_pressed = false;
        }
    }

    fn motion(&mut self, x: i16, y: i16) -> bool {
        if self.toodle_rect.contains(x, y) {
            let (x, y) = self.toodle_rect.local(x, y);
            self.toodle.hover(x, y)
        } else {
            self.toodle.hover(-1, -1)
        }
    }

    fn scroll_up(&mut self, x: i16, y: i16) {
        if self.fwends_rect.contains(x, y) {
            let (x, y) = self.fwends_rect.local(x, y);
            self.fwends.scroll_up(x, y);
            return;
        }

        if self.puter_rect.contains(x, y) {
            self.puter.scroll_up();
        }
    }

    fn scroll_down(&mut self, x: i16, y: i16) {
        if self.fwends_rect.contains(x, y) {
            let (x, y) = self.fwends_rect.local(x, y);
            self.fwends.scroll_down(x, y);
            return;
        }

        if self.puter_rect.contains(x, y) {
            self.puter.scroll_down();
        }
    }

    fn shutdown(&mut self) {
        self.puter.shutdown_terminal();
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let palette = Palette::load(PALETTE_PATH)?;
    let mut app = App::load(&palette)?;
    let width = app.width();
    let height = app.height();
    let mut fb = Framebuffer::new(width, height, app.fill_color(&palette));
    let xwin = XWindow::open(width, height)?;
    app.start(xwin.window as u64)?;
    app.render(&mut fb, &palette);
    xwin.draw(&fb)?;

    let mut last_draw = Instant::now();
    let mut running = true;
    while running {
        running = app.drain_events();
        if app.drain_replies() {
            app.render(&mut fb, &palette);
            xwin.draw(&fb)?;
        }

        while let Some(event) = xwin.conn.poll_for_event()? {
            match event {
                XEvent::Expose(_) => {
                    app.render(&mut fb, &palette);
                    xwin.draw(&fb)?;
                }
                XEvent::KeyPress(event) => {
                    app.handle_key_press(event.detail, event.state.into())?;
                    app.render(&mut fb, &palette);
                    xwin.draw(&fb)?;
                }
                XEvent::ButtonPress(event) => match event.detail {
                    WHEEL_UP => {
                        app.scroll_up(event.event_x, event.event_y);
                        app.render(&mut fb, &palette);
                        xwin.draw(&fb)?;
                    }
                    WHEEL_DOWN => {
                        app.scroll_down(event.event_x, event.event_y);
                        app.render(&mut fb, &palette);
                        xwin.draw(&fb)?;
                    }
                    detail if detail == ButtonIndex::M1.into() => {
                        app.click(event.event_x, event.event_y)?;
                        app.render(&mut fb, &palette);
                        xwin.draw(&fb)?;
                    }
                    _ => {}
                },
                XEvent::ButtonRelease(event) => {
                    if event.detail == ButtonIndex::M1.into() {
                        app.release(event.event_x, event.event_y);
                        app.render(&mut fb, &palette);
                        xwin.draw(&fb)?;
                    }
                }
                XEvent::MotionNotify(event) => {
                    if app.motion(event.event_x, event.event_y) {
                        app.render(&mut fb, &palette);
                        xwin.draw(&fb)?;
                    }
                }
                XEvent::DestroyNotify(_) => running = false,
                _ => {}
            }
        }

        if last_draw.elapsed() >= Duration::from_millis(16) {
            app.render(&mut fb, &palette);
            xwin.draw(&fb)?;
            last_draw = Instant::now();
        }

        std::thread::sleep(Duration::from_millis(4));
    }

    app.shutdown();
    Ok(())
}
