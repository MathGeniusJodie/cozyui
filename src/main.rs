use std::error::Error;
use std::fs::File;
use std::io::BufReader;
use std::time::Duration;

use x11rb::connection::Connection;
use x11rb::protocol::Event as XEvent;
use x11rb::protocol::xproto::ConnectionExt as XprotoConnectionExt;
use x11rb::protocol::xproto::{
    AtomEnum, ButtonIndex, ChangeWindowAttributesAux, CreateGCAux, CreateWindowAux, EventMask,
    Gcontext, ImageFormat, PropMode, Window, WindowClass,
};
use x11rb::wrapper::ConnectionExt as _;
use x11rb::xcb_ffi::XCBConnection;

mod bitmap_font;
mod comicoro_font;
mod emojimap;
mod fwends;
mod peanut_money_font;
mod puter;
mod text_input;
mod text_wrap;
mod toodle;

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
        if pixels.len() != width * height {
            return Err(format!(
                "PNG pixel count mismatch for {path}: got {}, expected {}",
                pixels.len(),
                width * height
            )
            .into());
        }
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
    pixels: Vec<u8>,
}

impl Framebuffer {
    const BYTES_PER_PIXEL: usize = 4;

    fn new(width: usize, height: usize, fill: Rgba) -> Self {
        Self::new_filled(width, height, fill)
    }

    fn color_bytes(color: Rgba) -> [u8; Self::BYTES_PER_PIXEL] {
        [color.b, color.g, color.r, 0]
    }

    fn pixel_offset(&self, x: usize, y: usize) -> usize {
        (y * self.width + x) * Self::BYTES_PER_PIXEL
    }

    fn set_pixel(&mut self, x: usize, y: usize, color: Rgba) {
        let offset = self.pixel_offset(x, y);
        self.pixels[offset..offset + Self::BYTES_PER_PIXEL]
            .copy_from_slice(&Self::color_bytes(color));
    }

    fn row_bytes(&self, y: usize, x: usize, width: usize) -> &[u8] {
        let start = self.pixel_offset(x, y);
        let end = start + width * Self::BYTES_PER_PIXEL;
        &self.pixels[start..end]
    }

    fn row_bytes_mut(&mut self, y: usize, x: usize, width: usize) -> &mut [u8] {
        let start = self.pixel_offset(x, y);
        let end = start + width * Self::BYTES_PER_PIXEL;
        &mut self.pixels[start..end]
    }

    fn ximage_bytes(&self) -> &[u8] {
        &self.pixels
    }

    fn filled_bytes(width: usize, height: usize, fill: Rgba) -> Vec<u8> {
        let mut pixels = vec![0; width * height * Self::BYTES_PER_PIXEL];
        let color = Self::color_bytes(fill);
        for pixel in pixels.chunks_exact_mut(Self::BYTES_PER_PIXEL) {
            pixel.copy_from_slice(&color);
        }
        pixels
    }

    fn new_filled(width: usize, height: usize, fill: Rgba) -> Self {
        Self {
            width,
            height,
            pixels: Self::filled_bytes(width, height, fill),
        }
    }

    pub(crate) fn clear(&mut self, color: Rgba) {
        let color = Self::color_bytes(color);
        for pixel in self.pixels.chunks_exact_mut(Self::BYTES_PER_PIXEL) {
            pixel.copy_from_slice(&color);
        }
    }

    pub(crate) fn clear_scaled(&mut self, image: &Image, scale: usize) {
        for y in 0..self.height {
            for x in 0..self.width {
                let sx = (x / scale).min(image.width - 1);
                let sy = (y / scale).min(image.height - 1);
                self.set_pixel(x, y, image.at(sx, sy));
            }
        }
    }

    pub(crate) fn fill_rect(&mut self, x: usize, y: usize, w: usize, h: usize, color: Rgba) {
        for py in y..(y + h).min(self.height) {
            for px in x..(x + w).min(self.width) {
                self.set_pixel(px, py, color);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
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
        if dest_x >= self.width || dest_y >= self.height {
            return;
        }

        let copy_width = src.width.min(self.width - dest_x);
        let copy_height = src.height.min(self.height - dest_y);
        for y in 0..copy_height {
            self.row_bytes_mut(dest_y + y, dest_x, copy_width)
                .copy_from_slice(src.row_bytes(y, 0, copy_width));
        }
    }
}

struct XWindow {
    conn: XCBConnection,
    window: Window,
    gc: Gcontext,
    depth: u8,
    keyboard: text_input::Keyboard,
}

impl XWindow {
    fn open(width: usize, height: usize) -> Result<Self, Box<dyn Error>> {
        let (conn, screen_num) = XCBConnection::connect(None)?;
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
                    | EventMask::KEY_RELEASE
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
                    | EventMask::KEY_RELEASE
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
        let keyboard = text_input::Keyboard::new(&conn)?;

        Ok(Self {
            conn,
            window,
            gc,
            depth,
            keyboard,
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
            data,
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
                if base + 2 >= palette.len() {
                    return Err(
                        format!("indexed PNG palette index {idx} out of bounds in {path}").into(),
                    );
                }
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
        self.render_puter(fb, palette);
        self.render_toodle(fb, palette);
        self.render_fwends(fb, palette);
    }

    fn render_puter(&self, fb: &mut Framebuffer, palette: &Palette) {
        let mut puter_fb = Framebuffer::new(
            self.puter_rect.w,
            self.puter_rect.h,
            self.puter.fill_color(palette),
        );
        self.puter.render(&mut puter_fb, palette);
        fb.blit_from(&puter_fb, self.puter_rect.x, self.puter_rect.y);
    }

    fn render_toodle(&self, fb: &mut Framebuffer, palette: &Palette) {
        let mut toodle_fb = Framebuffer::new(
            self.toodle_rect.w,
            self.toodle_rect.h,
            self.toodle.fill_color(palette),
        );
        self.toodle.render(&mut toodle_fb, palette);
        fb.blit_from(&toodle_fb, self.toodle_rect.x, self.toodle_rect.y);
    }

    fn render_fwends(&self, fb: &mut Framebuffer, palette: &Palette) {
        let mut fwends_fb = Framebuffer::new(
            self.fwends_rect.w,
            self.fwends_rect.h,
            self.fwends.fill_color(palette),
        );
        self.fwends.render(&mut fwends_fb, palette);
        fb.blit_from(&fwends_fb, self.fwends_rect.x, self.fwends_rect.y);
    }

    fn render_focused_widget(&self, fb: &mut Framebuffer, palette: &Palette) {
        match self.focus {
            FocusedWidget::Puter => self.render_puter(fb, palette),
            FocusedWidget::Toodle => self.render_toodle(fb, palette),
            FocusedWidget::Fwends => self.render_fwends(fb, palette),
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
            FocusedWidget::Puter => {
                self.puter.handle_key_press(input);
                Ok(())
            }
            FocusedWidget::Toodle => self.toodle.handle_key_press(input),
            FocusedWidget::Fwends => self.fwends.handle_key_press(input),
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
            app.render_puter(&mut fb, &palette);
            xwin.draw(&fb)?;
            drew_frame = true;
        }

        if app.drain_replies() {
            app.render_fwends(&mut fb, &palette);
            xwin.draw(&fb)?;
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
                    xwin.draw(&fb)?;
                    drew_frame = true;
                }
                XEvent::KeyRelease(event) => {
                    xwin.keyboard.release(event.detail);
                }
                XEvent::ButtonPress(event) => match event.detail {
                    WHEEL_UP => {
                        app.scroll_up(event.event_x, event.event_y);
                        app.render(&mut fb, &palette);
                        xwin.draw(&fb)?;
                        drew_frame = true;
                    }
                    WHEEL_DOWN => {
                        app.scroll_down(event.event_x, event.event_y);
                        app.render(&mut fb, &palette);
                        xwin.draw(&fb)?;
                        drew_frame = true;
                    }
                    detail if detail == u8::from(ButtonIndex::M1) => {
                        app.click(event.event_x, event.event_y)?;
                        app.render(&mut fb, &palette);
                        xwin.draw(&fb)?;
                        drew_frame = true;
                    }
                    _ => {}
                },
                XEvent::ButtonRelease(event) => {
                    if event.detail == u8::from(ButtonIndex::M1) {
                        app.release(event.event_x, event.event_y);
                        app.render(&mut fb, &palette);
                        xwin.draw(&fb)?;
                        drew_frame = true;
                    }
                }
                XEvent::MotionNotify(event) => {
                    if app.motion(event.event_x, event.event_y) {
                        app.render(&mut fb, &palette);
                        xwin.draw(&fb)?;
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
