use std::error::Error;
use std::fs::File;
#[cfg(unix)]
use std::os::fd::OwnedFd;
use std::time::{Duration, Instant};

use memmap2::MmapMut;
use x11rb::connection::{Connection, RequestConnection};
use x11rb::protocol::Event as XEvent;
use x11rb::protocol::shape::{self, ConnectionExt as ShapeConnectionExt};
use x11rb::protocol::shm::{ConnectionExt as ShmConnectionExt, Seg};
use x11rb::protocol::xproto::ConnectionExt as XprotoConnectionExt;
use x11rb::protocol::xproto::{
    Atom, AtomEnum, BackingStore, ChangeWindowAttributesAux, ClipOrdering, ConfigureWindowAux,
    CreateGCAux, CreateWindowAux, EventMask, Gcontext, Gravity, ImageFormat, PropMode, Rectangle,
    SELECTION_NOTIFY_EVENT, SelectionClearEvent, SelectionNotifyEvent, SelectionRequestEvent, Time,
    Window, WindowClass,
};
use x11rb::wrapper::ConnectionExt as _;
use x11rb::xcb_ffi::XCBConnection;

use crate::text::input as text_input;
use crate::{Framebuffer, Palette, Rect, TRANSPARENT};
use pixel_graphics::PresentLut;

pub struct XWindow {
    pub(crate) conn: XCBConnection,
    pub(crate) window: Window,
    gc: Gcontext,
    depth: u8,
    pub(crate) keyboard: text_input::Keyboard,
    upload_buffer: Vec<u8>,
    shm_image: Option<ShmImage>,
    clipboard_atoms: ClipboardAtoms,
    clipboard_text: Option<String>,
    /// Index -> BGRA table applied when presenting; refreshed via `set_palette`.
    lut: Box<PresentLut>,
    /// SHAPE-based transparency: cut `TRANSPARENT` framebuffer pixels out of
    /// the window. `false` when disabled or the server lacks the extension.
    shaped: bool,
    /// Last bounding-shape rectangles sent, to skip redundant updates.
    shape_rects: Option<Vec<Rectangle>>,
}

struct ShmImage {
    seg: Seg,
    mmap: MmapMut,
}

struct ClipboardAtoms {
    clipboard: Atom,
    targets: Atom,
    timestamp: Atom,
    save_targets: Atom,
    multiple: Atom,
    utf8_string: Atom,
    text: Atom,
    text_plain: Atom,
    text_plain_utf8: Atom,
    compound_text: Atom,
    cozy_clipboard: Atom,
}

impl XWindow {
    pub(crate) fn open(
        width: usize,
        height: usize,
        transparent: bool,
    ) -> Result<Self, Box<dyn Error>> {
        let (conn, screen_num) = XCBConnection::connect(None)?;
        let shaped = transparent
            && conn
                .extension_information(shape::X11_EXTENSION_NAME)?
                .is_some();
        let screen = &conn.setup().roots[screen_num];
        let root = screen.root;
        let depth = screen.root_depth;
        let window = conn.generate_id()?;
        let gc = conn.generate_id()?;
        let event_mask = EventMask::EXPOSURE
            | EventMask::KEY_PRESS
            | EventMask::KEY_RELEASE
            | EventMask::BUTTON_PRESS
            | EventMask::BUTTON_RELEASE
            | EventMask::POINTER_MOTION
            | EventMask::STRUCTURE_NOTIFY;

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
            // Backing store + bit gravity ask the server to retain and
            // restore obscured content itself, so moves don't flash
            // undefined (black) pixels while our Expose redraw is in flight.
            &CreateWindowAux::new()
                .event_mask(event_mask)
                .backing_store(BackingStore::WHEN_MAPPED)
                .bit_gravity(Gravity::NORTH_WEST),
        )?;
        conn.change_window_attributes(
            window,
            &ChangeWindowAttributesAux::new().event_mask(event_mask),
        )?;
        conn.create_gc(gc, window, &CreateGCAux::new())?;

        conn.change_property8(
            PropMode::REPLACE,
            window,
            AtomEnum::WM_NAME,
            AtomEnum::STRING,
            b"cozyui",
        )?;
        conn.map_window(window)?;
        conn.flush()?;
        let keyboard = text_input::Keyboard::new(&conn)?;
        let clipboard_atoms = ClipboardAtoms::load(&conn)?;
        let shm_image = Self::open_shm_image(&conn, width, height).ok();

        Ok(Self {
            conn,
            window,
            gc,
            depth,
            keyboard,
            upload_buffer: Vec::new(),
            shm_image,
            clipboard_atoms,
            clipboard_text: None,
            lut: Box::new([[0, 0, 0, 0xFF]; 256]),
            shaped,
            shape_rects: None,
        })
    }

    /// Refresh the index->BGRA present table from the active palette.
    pub(crate) fn set_palette(&mut self, palette: &Palette) {
        self.lut = palette.present_lut();
    }

    #[cfg(unix)]
    fn open_shm_image(
        conn: &XCBConnection,
        width: usize,
        height: usize,
    ) -> Result<ShmImage, Box<dyn Error>> {
        let version = conn.shm_query_version()?.reply()?;
        if version.major_version < 1 || (version.major_version == 1 && version.minor_version < 2) {
            return Err("MIT-SHM is too old for fd-backed segments".into());
        }

        let seg = conn.generate_id()?;
        let size = (width * height * Framebuffer::BYTES_PER_PIXEL) as u32;
        let reply = conn.shm_create_segment(seg, size, false)?.reply()?;
        let fd: OwnedFd = reply.shm_fd;
        let file = File::from(fd);
        let mmap = unsafe { MmapMut::map_mut(&file)? };
        Ok(ShmImage { seg, mmap })
    }

    #[cfg(not(unix))]
    fn open_shm_image(
        _conn: &XCBConnection,
        _width: usize,
        _height: usize,
    ) -> Result<ShmImage, Box<dyn Error>> {
        Err("MIT-SHM fd passing requires Unix".into())
    }

    /// Sync the window's bounding shape to the framebuffer's `TRANSPARENT`
    /// pixels, if shaping is enabled and the mask actually changed.
    fn update_shape(&mut self, fb: &Framebuffer) -> Result<(), Box<dyn Error>> {
        if !self.shaped {
            return Ok(());
        }
        let rects = opaque_rects(fb);
        if self
            .shape_rects
            .as_ref()
            .is_some_and(|cached| rects_equal(cached, &rects))
        {
            return Ok(());
        }
        self.conn.shape_rectangles(
            shape::SO::SET,
            shape::SK::BOUNDING,
            ClipOrdering::UNSORTED,
            self.window,
            0,
            0,
            &rects,
        )?;
        self.shape_rects = Some(rects);
        Ok(())
    }

    pub(crate) fn draw(&mut self, fb: &Framebuffer) -> Result<(), Box<dyn Error>> {
        self.update_shape(fb)?;
        let frame_len = fb.width * fb.height * Framebuffer::BYTES_PER_PIXEL;
        if let Some(shm_image) = &mut self.shm_image {
            fb.present_into(&mut shm_image.mmap[..frame_len], &self.lut);
            self.conn.shm_put_image(
                self.window,
                self.gc,
                fb.width as u16,
                fb.height as u16,
                0,
                0,
                fb.width as u16,
                fb.height as u16,
                0,
                0,
                self.depth,
                u8::from(ImageFormat::Z_PIXMAP),
                false,
                shm_image.seg,
                0,
            )?;
            self.conn.flush()?;
            return Ok(());
        }

        self.upload_buffer.resize(frame_len, 0);
        fb.present_into(&mut self.upload_buffer, &self.lut);
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
            &self.upload_buffer,
        )?;
        self.conn.flush()?;
        Ok(())
    }

    pub(crate) fn resize(&mut self, width: usize, height: usize) -> Result<(), Box<dyn Error>> {
        self.conn.configure_window(
            self.window,
            &ConfigureWindowAux::new()
                .width(width as u32)
                .height(height as u32),
        )?;
        self.resize_backing(width, height)
    }

    /// Recreates the SHM backing buffer to match a size the window already
    /// has (e.g. reported by a `ConfigureNotify` from the WM), without
    /// requesting a new geometry from the server.
    pub(crate) fn resize_backing(
        &mut self,
        width: usize,
        height: usize,
    ) -> Result<(), Box<dyn Error>> {
        if let Some(shm_image) = self.shm_image.take() {
            self.conn.shm_detach(shm_image.seg)?;
        }
        self.shm_image = Self::open_shm_image(&self.conn, width, height).ok();
        self.conn.flush()?;
        Ok(())
    }

    pub(crate) fn draw_rect(&mut self, fb: &Framebuffer, rect: Rect) -> Result<(), Box<dyn Error>> {
        if rect.x == 0 && rect.y == 0 && rect.w == fb.width && rect.h == fb.height {
            return self.draw(fb);
        }
        self.update_shape(fb)?;

        let byte_len = rect.w * rect.h * Framebuffer::BYTES_PER_PIXEL;
        if let Some(shm_image) = &mut self.shm_image {
            fb.present_rect_into(rect, &mut shm_image.mmap[..byte_len], &self.lut);
            self.conn.shm_put_image(
                self.window,
                self.gc,
                rect.w as u16,
                rect.h as u16,
                0,
                0,
                rect.w as u16,
                rect.h as u16,
                rect.x as i16,
                rect.y as i16,
                self.depth,
                u8::from(ImageFormat::Z_PIXMAP),
                false,
                shm_image.seg,
                0,
            )?;
            self.conn.flush()?;
            return Ok(());
        }

        self.upload_buffer.resize(byte_len, 0);
        fb.present_rect_into(rect, &mut self.upload_buffer, &self.lut);
        self.conn.put_image(
            ImageFormat::Z_PIXMAP,
            self.window,
            self.gc,
            rect.w as u16,
            rect.h as u16,
            rect.x as i16,
            rect.y as i16,
            0,
            self.depth,
            &self.upload_buffer,
        )?;
        self.conn.flush()?;
        Ok(())
    }

    pub(crate) fn set_clipboard_text(&mut self, text: String) -> Result<(), Box<dyn Error>> {
        self.clipboard_text = Some(text);
        self.conn.set_selection_owner(
            self.window,
            self.clipboard_atoms.clipboard,
            Time::CURRENT_TIME,
        )?;
        self.conn.flush()?;
        let owner = self
            .conn
            .get_selection_owner(self.clipboard_atoms.clipboard)?
            .reply()?
            .owner;
        if owner != self.window {
            self.clipboard_text = None;
            return Err("failed to take ownership of the X clipboard".into());
        }
        Ok(())
    }

    pub(crate) fn clipboard_text(&mut self) -> Result<Option<String>, Box<dyn Error>> {
        if self
            .conn
            .get_selection_owner(self.clipboard_atoms.clipboard)?
            .reply()?
            .owner
            == self.window
        {
            return Ok(self.clipboard_text.clone());
        }

        self.conn.convert_selection(
            self.window,
            self.clipboard_atoms.clipboard,
            self.clipboard_atoms.utf8_string,
            self.clipboard_atoms.cozy_clipboard,
            Time::CURRENT_TIME,
        )?;
        self.conn.flush()?;

        let timeout = Instant::now() + Duration::from_millis(500);
        while Instant::now() < timeout {
            if let Some(event) = self.conn.poll_for_event()? {
                match event {
                    XEvent::SelectionNotify(event) => {
                        return self.read_selection_notify(event);
                    }
                    XEvent::SelectionRequest(event) => self.handle_selection_request(event)?,
                    XEvent::SelectionClear(event) => self.handle_selection_clear(event),
                    _ => {}
                }
            } else {
                std::thread::sleep(Duration::from_millis(5));
            }
        }

        Ok(None)
    }

    #[allow(clippy::needless_pass_by_ref_mut)]
    pub(crate) fn handle_selection_request(
        &mut self,
        event: SelectionRequestEvent,
    ) -> Result<(), Box<dyn Error>> {
        let mut property = AtomEnum::NONE.into();
        if event.selection == self.clipboard_atoms.clipboard {
            property = selection_property(event);
            if event.target == self.clipboard_atoms.multiple {
                if self.handle_multiple_selection_request(event, property)? {
                    property = event.property;
                } else {
                    property = AtomEnum::NONE.into();
                }
            } else if !self.write_selection_target(event.requestor, event.target, property)? {
                property = AtomEnum::NONE.into();
            }
        }

        let notify = SelectionNotifyEvent {
            response_type: SELECTION_NOTIFY_EVENT,
            sequence: 0,
            time: event.time,
            requestor: event.requestor,
            selection: event.selection,
            target: event.target,
            property,
        };
        self.conn
            .send_event(false, event.requestor, EventMask::NO_EVENT, notify)?;
        self.conn.flush()?;
        Ok(())
    }

    pub(crate) fn handle_selection_clear(&mut self, event: SelectionClearEvent) {
        if event.selection == self.clipboard_atoms.clipboard {
            self.clipboard_text = None;
        }
    }

    fn read_selection_notify(
        &self,
        event: SelectionNotifyEvent,
    ) -> Result<Option<String>, Box<dyn Error>> {
        if event.property == u32::from(AtomEnum::NONE) {
            return Ok(None);
        }

        let reply = self
            .conn
            .get_property(
                true,
                self.window,
                event.property,
                AtomEnum::ANY,
                0,
                u32::MAX / 4,
            )?
            .reply()?;
        let Some(bytes) = reply.value8() else {
            return Ok(None);
        };
        Ok(String::from_utf8(bytes.collect()).ok())
    }

    fn supported_text_target(&self, target: Atom) -> bool {
        target == self.clipboard_atoms.utf8_string
            || target == self.clipboard_atoms.text
            || target == self.clipboard_atoms.text_plain
            || target == self.clipboard_atoms.text_plain_utf8
            || target == self.clipboard_atoms.compound_text
            || target == u32::from(AtomEnum::STRING)
    }

    fn supported_targets(&self) -> [Atom; 10] {
        [
            self.clipboard_atoms.targets,
            self.clipboard_atoms.multiple,
            self.clipboard_atoms.timestamp,
            self.clipboard_atoms.save_targets,
            self.clipboard_atoms.utf8_string,
            self.clipboard_atoms.text_plain_utf8,
            self.clipboard_atoms.text_plain,
            self.clipboard_atoms.text,
            self.clipboard_atoms.compound_text,
            AtomEnum::STRING.into(),
        ]
    }

    fn write_selection_target(
        &self,
        requestor: Window,
        target: Atom,
        property: Atom,
    ) -> Result<bool, Box<dyn Error>> {
        if property == u32::from(AtomEnum::NONE) {
            return Ok(false);
        }

        if target == self.clipboard_atoms.targets {
            self.conn.change_property32(
                PropMode::REPLACE,
                requestor,
                property,
                AtomEnum::ATOM,
                &self.supported_targets(),
            )?;
            return Ok(true);
        }

        if target == self.clipboard_atoms.timestamp {
            self.conn.change_property32(
                PropMode::REPLACE,
                requestor,
                property,
                AtomEnum::INTEGER,
                &[0],
            )?;
            return Ok(true);
        }

        if target == self.clipboard_atoms.save_targets {
            self.conn.change_property32(
                PropMode::REPLACE,
                requestor,
                property,
                AtomEnum::ATOM,
                &[],
            )?;
            return Ok(true);
        }

        let Some(text) = &self.clipboard_text else {
            return Ok(false);
        };
        if !self.supported_text_target(target) {
            return Ok(false);
        }

        self.conn.change_property8(
            PropMode::REPLACE,
            requestor,
            property,
            self.text_property_type(target),
            text.as_bytes(),
        )?;
        Ok(true)
    }

    fn handle_multiple_selection_request(
        &self,
        event: SelectionRequestEvent,
        property: Atom,
    ) -> Result<bool, Box<dyn Error>> {
        if event.property == u32::from(AtomEnum::NONE) {
            return Ok(false);
        }

        let reply = self
            .conn
            .get_property(
                false,
                event.requestor,
                property,
                AtomEnum::ATOM,
                0,
                u32::MAX / 4,
            )?
            .reply()?;
        let Some(values) = reply.value32() else {
            return Ok(false);
        };

        let mut pairs = values.collect::<Vec<_>>();
        if pairs.len() % 2 != 0 {
            return Ok(false);
        }

        for index in (0..pairs.len()).step_by(2) {
            let target = pairs[index];
            let property = pairs[index + 1];
            if !self.write_selection_target(event.requestor, target, property)? {
                pairs[index + 1] = u32::from(AtomEnum::NONE);
            }
        }

        self.conn.change_property32(
            PropMode::REPLACE,
            event.requestor,
            event.property,
            AtomEnum::ATOM,
            &pairs,
        )?;
        Ok(true)
    }

    fn text_property_type(&self, target: Atom) -> Atom {
        if target == self.clipboard_atoms.text || target == u32::from(AtomEnum::STRING) {
            AtomEnum::STRING.into()
        } else {
            target
        }
    }
}

impl Drop for XWindow {
    fn drop(&mut self) {
        if let Some(shm_image) = &self.shm_image {
            let _ = self.conn.shm_detach(shm_image.seg);
            let _ = self.conn.flush();
        }
    }
}

impl ClipboardAtoms {
    fn load(conn: &XCBConnection) -> Result<Self, Box<dyn Error>> {
        Ok(Self {
            clipboard: intern_atom(conn, b"CLIPBOARD")?,
            targets: intern_atom(conn, b"TARGETS")?,
            timestamp: intern_atom(conn, b"TIMESTAMP")?,
            save_targets: intern_atom(conn, b"SAVE_TARGETS")?,
            multiple: intern_atom(conn, b"MULTIPLE")?,
            utf8_string: intern_atom(conn, b"UTF8_STRING")?,
            text: intern_atom(conn, b"TEXT")?,
            text_plain: intern_atom(conn, b"text/plain")?,
            text_plain_utf8: intern_atom(conn, b"text/plain;charset=utf-8")?,
            compound_text: intern_atom(conn, b"COMPOUND_TEXT")?,
            cozy_clipboard: intern_atom(conn, b"COZYUI_CLIPBOARD")?,
        })
    }
}

fn intern_atom(conn: &XCBConnection, name: &[u8]) -> Result<Atom, Box<dyn Error>> {
    Ok(conn.intern_atom(false, name)?.reply()?.atom)
}

/// Cover every non-`TRANSPARENT` pixel with rectangles: horizontal runs per
/// row, with identical consecutive rows merged into taller bands.
fn opaque_rects(fb: &Framebuffer) -> Vec<Rectangle> {
    let mut rects = Vec::new();
    let mut band_runs: Vec<(usize, usize)> = Vec::new();
    let mut band_start = 0;
    for y in 0..=fb.height {
        let runs = if y < fb.height {
            row_runs(fb.row(y))
        } else {
            Vec::new()
        };
        if runs != band_runs {
            for &(x, w) in &band_runs {
                rects.push(Rectangle {
                    x: x as i16,
                    y: band_start as i16,
                    width: w as u16,
                    height: (y - band_start) as u16,
                });
            }
            band_runs = runs;
            band_start = y;
        }
    }
    rects
}

/// `Rectangle` doesn't derive `PartialEq`, so compare fields directly.
fn rects_equal(a: &[Rectangle], b: &[Rectangle]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b)
            .all(|(p, q)| p.x == q.x && p.y == q.y && p.width == q.width && p.height == q.height)
}

/// (x, width) spans of non-`TRANSPARENT` pixels in one row.
fn row_runs(row: &[u8]) -> Vec<(usize, usize)> {
    let mut runs = Vec::new();
    let mut start = None;
    for (x, &index) in row.iter().enumerate() {
        match (index != TRANSPARENT, start) {
            (true, None) => start = Some(x),
            (false, Some(s)) => {
                runs.push((s, x - s));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        runs.push((s, row.len() - s));
    }
    runs
}

fn selection_property(event: SelectionRequestEvent) -> Atom {
    if event.property == u32::from(AtomEnum::NONE) {
        event.target
    } else {
        event.property
    }
}
