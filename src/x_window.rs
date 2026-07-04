use std::collections::VecDeque;
use std::error::Error;
use std::fs::File;
#[cfg(unix)]
use std::os::fd::{AsRawFd, OwnedFd};
use std::time::{Duration, Instant};

use memmap2::MmapMut;
use x11rb::connection::{Connection, RequestConnection};
use x11rb::protocol::Event as XEvent;
use x11rb::protocol::render::{self, ConnectionExt as RenderConnectionExt, PictType};
use x11rb::protocol::shape::{self, ConnectionExt as ShapeConnectionExt};
use x11rb::protocol::shm::{ConnectionExt as ShmConnectionExt, Seg};
use x11rb::protocol::xproto::ConnectionExt as XprotoConnectionExt;
use x11rb::protocol::xproto::{
    Atom, AtomEnum, BackingStore, ChangeWindowAttributesAux, ClipOrdering, ConfigureWindowAux,
    CreateGCAux, CreateWindowAux, Cursor, EventMask, Gcontext, Gravity, ImageFormat, ImageOrder,
    Pixmap, PropMode, Property, Rectangle, SELECTION_NOTIFY_EVENT, SelectionClearEvent,
    SelectionNotifyEvent, SelectionRequestEvent, Timestamp, Window, WindowClass,
};
use x11rb::wrapper::ConnectionExt as _;
use x11rb::xcb_ffi::XCBConnection;

use crate::text::input as text_input;
use crate::{
    CURSOR_KIND_COUNT, CursorKind, Framebuffer, Palette, Rect, Sprite, TRANSPARENT, assets,
};
use pixel_graphics::PresentLut;

pub struct XWindow {
    pub(crate) conn: XCBConnection,
    pub(crate) window: Window,
    gc: Gcontext,
    depth: u8,
    /// Server-side copy of the last presented frame, installed as the
    /// window's `background_pixmap`: the server repaints exposed regions
    /// from it by itself, so a WM dragging the window around (the splitwm
    /// dock rides the canvas edge) costs us no exposure traffic, no
    /// re-render and no re-upload per move.
    back_pix: Pixmap,
    pub(crate) keyboard: text_input::Keyboard,
    upload_buffer: Vec<u8>,
    /// Two SHM segments used alternately: while the server is still reading
    /// one (busy until its completion event arrives), the next frame is
    /// written into the other, so presenting normally never blocks on a
    /// round trip.
    shm_images: Vec<ShmImage>,
    clipboard_atoms: ClipboardAtoms,
    clipboard_text: Option<String>,
    /// Index -> BGRA table applied when presenting; refreshed via `set_palette`.
    lut: Box<PresentLut>,
    /// SHAPE-based transparency: cut `TRANSPARENT` framebuffer pixels out of
    /// the window. `false` when disabled or the server lacks the extension.
    shaped: bool,
    /// Last bounding-shape rectangles sent, to skip redundant updates.
    shape_rects: Option<Vec<Rectangle>>,
    /// Per-row opaque runs backing `shape_rects`, so partial redraws only
    /// rescan the dirty rows instead of the whole framebuffer.
    shape_rows: Vec<Vec<(usize, usize)>>,
    /// Reusable scratch buffer for `row_runs`, so scanning a row that hasn't
    /// changed doesn't allocate a fresh `Vec` every partial redraw.
    row_scratch: Vec<(usize, usize)>,
    /// ARGB cursors baked from the `cursor_*` sprites, indexed by
    /// `CursorKind as usize`. `None` when the server lacks RENDER cursors.
    cursors: Option<[Cursor; CURSOR_KIND_COUNT]>,
    /// Last cursor set on the window, to skip redundant updates.
    current_cursor: Option<CursorKind>,
    /// Events pulled off the connection while blocked waiting for a selection
    /// reply (paste); replayed to the main loop by `poll_event` so no input or
    /// exposure is ever dropped.
    pending_events: VecDeque<XEvent>,
    /// Timestamp of the most recent input event, used where ICCCM forbids
    /// `CurrentTime` (selection ownership and conversion requests).
    last_event_time: Timestamp,
    /// When we took ownership of the clipboard (the TARGETS `TIMESTAMP` answer).
    selection_time: Timestamp,
}

/// Cap on total paste size (both the single-property and INCR paths) so a
/// misbehaving (or malicious) selection owner can't force an unbounded
/// allocation or starve the INCR deadline check.
const MAX_PASTE_BYTES: usize = 16 * 1024 * 1024;

struct ShmImage {
    seg: Seg,
    mmap: MmapMut,
    /// A `shm_put_image` from this segment is still in flight; cleared by its
    /// completion event (or a full sync).
    busy: bool,
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
    cozy_clipboard: Atom,
    incr: Atom,
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
        // No EXPOSURE: exposed regions repaint server-side from the
        // window's background pixmap (`back_pix`), so exposure events would
        // only be wakeups with nothing to do — at their worst during a WM
        // window drag, which exposes per pointer motion.
        let event_mask = EventMask::KEY_PRESS
            | EventMask::KEY_RELEASE
            | EventMask::BUTTON_PRESS
            | EventMask::BUTTON_RELEASE
            | EventMask::POINTER_MOTION
            | EventMask::STRUCTURE_NOTIFY
            // For the PropertyNotify chunks of INCR clipboard transfers.
            | EventMask::PROPERTY_CHANGE;

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
            // Backing store + bit gravity are best-effort hints (Xorg
            // ignores backing store by default); the background pixmap
            // below is what actually keeps moves/exposes flash-free. They
            // still cover the brief window between a WM-initiated resize
            // and `resize_backing` recreating the pixmap.
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
        // The GC's default foreground (0 = black) matches the undefined
        // pixels a fresh window shows anyway, so the pixmap starts out
        // indistinguishable from the pre-first-draw window today.
        let back_pix = Self::create_back_pixmap(&conn, window, gc, depth, width, height)?;

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
        let shm_images = Self::open_shm_images_logged(&conn, width, height);

        Ok(Self {
            conn,
            window,
            gc,
            depth,
            back_pix,
            keyboard,
            upload_buffer: Vec::new(),
            shm_images,
            clipboard_atoms,
            clipboard_text: None,
            lut: Box::new([[0, 0, 0, 0xFF]; 256]),
            shaped,
            shape_rects: None,
            shape_rows: Vec::new(),
            row_scratch: Vec::new(),
            cursors: None,
            current_cursor: None,
            pending_events: VecDeque::new(),
            last_event_time: x11rb::CURRENT_TIME,
            selection_time: x11rb::CURRENT_TIME,
        })
    }

    /// The main loop's event source: replays events buffered during a
    /// clipboard wait before polling the connection, and records input
    /// timestamps for ICCCM-correct selection requests.
    pub(crate) fn poll_event(&mut self) -> Result<Option<XEvent>, Box<dyn Error>> {
        loop {
            let event = match self.pending_events.pop_front() {
                Some(event) => Some(event),
                None => self.conn.poll_for_event()?,
            };
            // SHM completions are internal bookkeeping, not app events.
            if let Some(XEvent::ShmCompletion(completion)) = &event {
                let seg = completion.shmseg;
                self.shm_completed(seg);
                continue;
            }
            if let Some(event) = &event {
                self.note_event_time(event);
            }
            return Ok(event);
        }
    }

    /// The server finished reading `seg`; its segment can be reused.
    fn shm_completed(&mut self, seg: Seg) {
        for image in &mut self.shm_images {
            if image.seg == seg {
                image.busy = false;
            }
        }
    }

    fn note_event_time(&mut self, event: &XEvent) {
        match event {
            XEvent::KeyPress(e) | XEvent::KeyRelease(e) => self.last_event_time = e.time,
            XEvent::ButtonPress(e) | XEvent::ButtonRelease(e) => self.last_event_time = e.time,
            XEvent::MotionNotify(e) => self.last_event_time = e.time,
            _ => {}
        }
    }

    /// Refresh the index->BGRA present table from the active palette.
    pub(crate) fn set_palette(&mut self, palette: &Palette) {
        self.lut = palette.present_lut();
    }

    /// Build the four ARGB hardware cursors from the baked `cursor_*` sprites
    /// via the RENDER extension and show the pointer one. Leaves the default
    /// X cursor in place when the server can't do RENDER cursors (>= 0.5).
    pub(crate) fn load_cursors(&mut self, palette: &Palette) -> Result<(), Box<dyn Error>> {
        if self
            .conn
            .extension_information(render::X11_EXTENSION_NAME)?
            .is_none()
        {
            return Ok(());
        }
        let version = self.conn.render_query_version(0, 8)?.reply()?;
        if version.major_version == 0 && version.minor_version < 5 {
            return Ok(());
        }
        let formats = self.conn.render_query_pict_formats()?.reply()?;
        let Some(argb32) = formats.formats.iter().find(|f| {
            f.depth == 32
                && f.type_ == PictType::DIRECT
                && f.direct.alpha_mask == 0xFF
                && f.direct.alpha_shift == 24
                && f.direct.red_shift == 16
                && f.direct.green_shift == 8
                && f.direct.blue_shift == 0
        }) else {
            return Ok(());
        };

        // Hotspots: arrow tip, I-beam center, fingertip, circle center.
        let sprites: [(Sprite, i16, i16); CURSOR_KIND_COUNT] = [
            (assets::cursor_pointer(), 4, 0),
            (assets::cursor_text(), 12, 12),
            (assets::cursor_hand(), 11, 0),
            (assets::cursor_disabled(), 12, 12),
        ];
        let mut cursors = [0; CURSOR_KIND_COUNT];
        for (cursor, (sprite, hot_x, hot_y)) in cursors.iter_mut().zip(&sprites) {
            *cursor = self.create_cursor(sprite, palette, argb32.id, *hot_x, *hot_y)?;
        }
        self.cursors = Some(cursors);
        self.set_cursor(CursorKind::Pointer)
    }

    fn create_cursor(
        &self,
        sprite: &Sprite,
        palette: &Palette,
        format: render::Pictformat,
        hot_x: i16,
        hot_y: i16,
    ) -> Result<Cursor, Box<dyn Error>> {
        let mut data = Vec::with_capacity(sprite.width * sprite.height * 4);
        let msb_first = self.conn.setup().image_byte_order == ImageOrder::MSB_FIRST;
        for y in 0..sprite.height {
            for x in 0..sprite.width {
                let index = sprite.at(x, y);
                // RENDER wants premultiplied alpha; with only fully opaque or
                // fully transparent pixels the colors pass through unchanged.
                let pixel = if index == TRANSPARENT {
                    [0, 0, 0, 0]
                } else {
                    let c = palette.color(index);
                    if msb_first {
                        [0xFF, c.r, c.g, c.b]
                    } else {
                        [c.b, c.g, c.r, 0xFF]
                    }
                };
                data.extend_from_slice(&pixel);
            }
        }

        let pixmap = self.conn.generate_id()?;
        self.conn.create_pixmap(
            32,
            pixmap,
            self.window,
            sprite.width as u16,
            sprite.height as u16,
        )?;
        let gc = self.conn.generate_id()?;
        self.conn.create_gc(gc, pixmap, &CreateGCAux::new())?;
        self.conn.put_image(
            ImageFormat::Z_PIXMAP,
            pixmap,
            gc,
            sprite.width as u16,
            sprite.height as u16,
            0,
            0,
            0,
            32,
            &data,
        )?;
        let picture = self.conn.generate_id()?;
        self.conn.render_create_picture(
            picture,
            pixmap,
            format,
            &render::CreatePictureAux::new(),
        )?;
        let cursor = self.conn.generate_id()?;
        self.conn
            .render_create_cursor(cursor, picture, hot_x as u16, hot_y as u16)?;
        self.conn.render_free_picture(picture)?;
        self.conn.free_gc(gc)?;
        self.conn.free_pixmap(pixmap)?;
        self.conn.flush()?;
        Ok(cursor)
    }

    /// Switch the window's cursor; no-ops when unchanged or unavailable.
    pub(crate) fn set_cursor(&mut self, kind: CursorKind) -> Result<(), Box<dyn Error>> {
        let Some(cursors) = &self.cursors else {
            return Ok(());
        };
        if self.current_cursor == Some(kind) {
            return Ok(());
        }
        self.conn.change_window_attributes(
            self.window,
            &ChangeWindowAttributesAux::new().cursor(cursors[kind as usize]),
        )?;
        self.conn.flush()?;
        self.current_cursor = Some(kind);
        Ok(())
    }

    /// Blocks until the X socket becomes readable or `timeout` elapses, so the
    /// main loop can sleep instead of polling. Pending requests are flushed
    /// first; without that the server may never produce the reply or event the
    /// wait is for. Spurious early wake-ups (EINTR) are fine — the caller
    /// re-checks all its event sources every iteration. A genuine poll error
    /// (anything other than EINTR/EAGAIN) is propagated instead of silently
    /// swallowed.
    #[cfg(unix)]
    pub(crate) fn wait_for_event(&self, timeout: Duration) -> Result<(), Box<dyn Error>> {
        self.conn.flush()?;
        let mut pfd = libc::pollfd {
            fd: self.conn.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let millis = i32::try_from(timeout.as_millis()).unwrap_or(i32::MAX);
        let ret = unsafe { libc::poll(&raw mut pfd, 1, millis) };
        if ret < 0 {
            let err = std::io::Error::last_os_error();
            if !matches!(err.raw_os_error(), Some(libc::EINTR) | Some(libc::EAGAIN)) {
                return Err(err.into());
            }
        }
        Ok(())
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
        let size = width * height * Framebuffer::BYTES_PER_PIXEL;
        let reply = conn.shm_create_segment(seg, size as u32, false)?.reply()?;
        let fd: OwnedFd = reply.shm_fd;
        let file = File::from(fd);
        let mmap = unsafe { MmapMut::map_mut(&file)? };
        if mmap.len() < size {
            let _ = conn.shm_detach(seg);
            return Err(format!(
                "SHM segment is {} bytes, expected at least {size}",
                mmap.len()
            )
            .into());
        }
        Ok(ShmImage {
            seg,
            mmap,
            busy: false,
        })
    }

    /// Open the two alternating SHM segments, degrading to an empty list (the
    /// slow `put_image` path) with a diagnostic instead of failing.
    fn open_shm_images_logged(conn: &XCBConnection, width: usize, height: usize) -> Vec<ShmImage> {
        let mut images = Vec::new();
        for _ in 0..2 {
            match Self::open_shm_image(conn, width, height) {
                Ok(image) => images.push(image),
                Err(err) => {
                    eprintln!("MIT-SHM unavailable, falling back to put_image: {err}");
                    for image in &images {
                        let _ = conn.shm_detach(image.seg);
                    }
                    return Vec::new();
                }
            }
        }
        images
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
    /// pixels, if shaping is enabled and the mask actually changed. Only the
    /// rows in `y0..y1` are rescanned; the rest come from the per-row cache.
    fn update_shape_rows(
        &mut self,
        fb: &Framebuffer,
        y0: usize,
        y1: usize,
    ) -> Result<(), Box<dyn Error>> {
        if !self.shaped {
            return Ok(());
        }
        let full = self.shape_rows.len() != fb.height;
        let (y0, y1) = if full {
            self.shape_rows = vec![Vec::new(); fb.height];
            (0, fb.height)
        } else {
            (y0.min(fb.height), y1.min(fb.height))
        };
        let mut changed = full;
        for y in y0..y1 {
            row_runs_into(fb.row(y), &mut self.row_scratch);
            if self.row_scratch != self.shape_rows[y] {
                std::mem::swap(&mut self.row_scratch, &mut self.shape_rows[y]);
                changed = true;
            }
        }
        if !changed {
            return Ok(());
        }

        let rects = rects_from_rows(&self.shape_rows);
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

    /// Create a window-sized pixmap, black-filled so it never shows stale
    /// bits, and install it as the window's background. The server paints
    /// exposed regions from it without our involvement (see `back_pix`).
    fn create_back_pixmap(
        conn: &XCBConnection,
        window: Window,
        gc: Gcontext,
        depth: u8,
        width: usize,
        height: usize,
    ) -> Result<Pixmap, Box<dyn Error>> {
        let (w, h) = (width.max(1) as u16, height.max(1) as u16);
        let pixmap = conn.generate_id()?;
        conn.create_pixmap(depth, pixmap, window, w, h)?;
        conn.poly_fill_rectangle(
            pixmap,
            gc,
            &[Rectangle {
                x: 0,
                y: 0,
                width: w,
                height: h,
            }],
        )?;
        conn.change_window_attributes(
            window,
            &ChangeWindowAttributesAux::new().background_pixmap(pixmap),
        )?;
        Ok(pixmap)
    }

    /// Full barrier: round-trip to the server, after which every outstanding
    /// `shm_put_image` has been executed and its completion event is queued.
    /// Draining the queue (rather than force-clearing `busy`) consumes those
    /// completions now; a blind clear would leave them behind to wrongly
    /// free a segment re-used by a later put while the server still reads
    /// it. Only the fallback when both segments are still busy.
    fn sync_shm(&mut self) -> Result<(), Box<dyn Error>> {
        self.conn.get_input_focus()?.reply()?;
        while let Some(event) = self.conn.poll_for_event()? {
            if let XEvent::ShmCompletion(completion) = &event {
                let seg = completion.shmseg;
                self.shm_completed(seg);
            } else {
                self.pending_events.push_back(event);
            }
        }
        Ok(())
    }

    /// A segment index that is free to write into. Drains any pending
    /// completion events first (buffering unrelated events); if both segments
    /// are somehow still in flight, falls back to a full sync.
    fn free_shm_index(&mut self) -> Result<Option<usize>, Box<dyn Error>> {
        if self.shm_images.is_empty() {
            return Ok(None);
        }
        if !self.shm_images.iter().any(|image| image.busy) {
            return Ok(Some(0));
        }
        while let Some(event) = self.conn.poll_for_event()? {
            if let XEvent::ShmCompletion(completion) = &event {
                let seg = completion.shmseg;
                self.shm_completed(seg);
            } else {
                self.pending_events.push_back(event);
            }
        }
        if let Some(index) = self.shm_images.iter().position(|image| !image.busy) {
            return Ok(Some(index));
        }
        self.sync_shm()?;
        // The barrier consumed every completion, so a segment must be free
        // now. Re-check instead of assuming: if the ordering guarantee this
        // relies on ever breaks, skip the frame rather than put_image into a
        // segment the server may still be reading.
        Ok(self.shm_images.iter().position(|image| !image.busy))
    }

    pub(crate) fn draw(&mut self, fb: &Framebuffer) -> Result<(), Box<dyn Error>> {
        self.update_shape_rows(fb, 0, fb.height)?;
        self.blit_rect(fb, Rect::new(0, 0, fb.width, fb.height))
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

    /// Recreates the SHM backing buffer and background pixmap to match a
    /// size the window already has (e.g. reported by a `ConfigureNotify`
    /// from the WM), without requesting a new geometry from the server.
    pub(crate) fn resize_backing(
        &mut self,
        width: usize,
        height: usize,
    ) -> Result<(), Box<dyn Error>> {
        // Make sure no put is still reading the old segments before detaching.
        if self.shm_images.iter().any(|image| image.busy) {
            self.sync_shm()?;
        }
        for shm_image in self.shm_images.drain(..) {
            self.conn.shm_detach(shm_image.seg)?;
        }
        self.shm_images = Self::open_shm_images_logged(&self.conn, width, height);
        // A background pixmap smaller than the window would tile; replace it
        // before the caller's full-frame draw fills the new one.
        self.conn.free_pixmap(self.back_pix)?;
        self.back_pix =
            Self::create_back_pixmap(&self.conn, self.window, self.gc, self.depth, width, height)?;
        self.conn.flush()?;
        Ok(())
    }

    pub(crate) fn draw_rect(&mut self, fb: &Framebuffer, rect: Rect) -> Result<(), Box<dyn Error>> {
        // Widget rects can momentarily disagree with the framebuffer size
        // (mid-relayout); a clipped partial repaint beats an out-of-bounds
        // panic in present_rect_into.
        let Some(rect) = clip_to_fb(rect, fb) else {
            return Ok(());
        };
        if rect.x == 0 && rect.y == 0 && rect.w == fb.width && rect.h == fb.height {
            return self.draw(fb);
        }
        self.update_shape_rows(fb, rect.y, rect.y.saturating_add(rect.h))?;
        self.blit_rect(fb, rect)
    }

    /// Present `rect` of the framebuffer, via SHM when available, else a
    /// `put_image` upload. Pixels land in `back_pix` (the window's
    /// background), and a `clear_area` tells the server to show that region
    /// — the copy the server keeps is what it also repaints exposes from.
    /// Callers must pass a rect that lies within `fb`; `draw_rect` clips
    /// before getting here.
    fn blit_rect(&mut self, fb: &Framebuffer, rect: Rect) -> Result<(), Box<dyn Error>> {
        let byte_len = rect.w * rect.h * Framebuffer::BYTES_PER_PIXEL;
        if let Some(index) = self.free_shm_index()? {
            let shm_image = &mut self.shm_images[index];
            // This slice is only sound while fb and shm_images resize in
            // lockstep; guard against that invariant breaking silently.
            debug_assert!(
                byte_len <= shm_image.mmap.len(),
                "blit_rect: byte_len {byte_len} exceeds shm mmap len {}",
                shm_image.mmap.len()
            );
            if byte_len > shm_image.mmap.len() {
                eprintln!(
                    "blit_rect: byte_len {byte_len} exceeds shm mmap len {}, skipping blit",
                    shm_image.mmap.len()
                );
                return Ok(());
            }
            fb.present_rect_into(rect, &mut shm_image.mmap[..byte_len], &self.lut);
            self.conn.shm_put_image(
                self.back_pix,
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
                // Ask for a completion event: the segment stays busy until the
                // server has read it, instead of stalling here on a sync.
                true,
                shm_image.seg,
                0,
            )?;
            shm_image.busy = true;
            self.show_background(rect)?;
            return Ok(());
        }

        self.upload_buffer.resize(byte_len, 0);
        fb.present_rect_into(rect, &mut self.upload_buffer, &self.lut);
        self.conn.put_image(
            ImageFormat::Z_PIXMAP,
            self.back_pix,
            self.gc,
            rect.w as u16,
            rect.h as u16,
            rect.x as i16,
            rect.y as i16,
            0,
            self.depth,
            &self.upload_buffer,
        )?;
        self.show_background(rect)?;
        Ok(())
    }

    /// Repaint `rect` of the window from its background pixmap (a no-expose
    /// `ClearArea`) — how a freshly blitted region becomes visible.
    fn show_background(&self, rect: Rect) -> Result<(), Box<dyn Error>> {
        self.conn.clear_area(
            false,
            self.window,
            rect.x as i16,
            rect.y as i16,
            rect.w as u16,
            rect.h as u16,
        )?;
        self.conn.flush()?;
        Ok(())
    }

    pub(crate) fn set_clipboard_text(&mut self, text: String) -> Result<(), Box<dyn Error>> {
        self.clipboard_text = Some(text);
        // ICCCM: ownership must be claimed with the timestamp of the event
        // that triggered the copy, never CurrentTime.
        self.conn.set_selection_owner(
            self.window,
            self.clipboard_atoms.clipboard,
            self.last_event_time,
        )?;
        self.conn.flush()?;
        let owner = self
            .conn
            .get_selection_owner(self.clipboard_atoms.clipboard)?
            .reply()?
            .owner;
        if owner != self.window {
            // Losing the ownership race (e.g. to a clipboard manager) only
            // means this one copy didn't stick; it must not abort the app.
            self.clipboard_text = None;
            eprintln!("clipboard copy failed: another client owns the selection");
            return Ok(());
        }
        self.selection_time = self.last_event_time;
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
            self.last_event_time,
        )?;
        self.conn.flush()?;

        // Wait for the owner's SelectionNotify by sleeping on the X socket
        // (no busy-wait); 500ms bounds the stall when the owner is dead.
        // Unrelated events arriving meanwhile are buffered for `poll_event`,
        // not dropped.
        let deadline = Instant::now() + Duration::from_millis(500);
        loop {
            while let Some(event) = self.conn.poll_for_event()? {
                match event {
                    XEvent::SelectionNotify(event) => {
                        return self.read_selection_notify(event);
                    }
                    XEvent::SelectionRequest(event) => self.handle_selection_request(event)?,
                    XEvent::SelectionClear(event) => self.handle_selection_clear(event),
                    other => self.pending_events.push_back(other),
                }
            }
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                eprintln!("clipboard paste timed out waiting for the selection owner");
                return Ok(None);
            };
            self.wait_for_event(remaining)?;
        }
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
        &mut self,
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
                // Same cap as the INCR path, in 32-bit units: without it a
                // hostile selection owner could force an arbitrarily large
                // allocation through a single non-INCR property.
                (MAX_PASTE_BYTES / 4) as u32,
            )?
            .reply()?;
        if reply.type_ == self.clipboard_atoms.incr {
            // Deleting the INCR property above told the owner to start
            // sending; the chunks arrive as PropertyNotify events.
            return self.read_incr_chunks(event.property);
        }
        if reply.bytes_after > 0 {
            eprintln!("clipboard paste exceeded {MAX_PASTE_BYTES} bytes, ignoring");
            return Ok(None);
        }
        let Some(bytes) = reply.value8() else {
            return Ok(None);
        };
        Ok(String::from_utf8(bytes.collect()).ok())
    }

    /// Receive a large selection via the INCR protocol: each NewValue
    /// PropertyNotify carries one chunk (read-and-delete to request the next),
    /// and a zero-length chunk ends the transfer.
    fn read_incr_chunks(&mut self, property: Atom) -> Result<Option<String>, Box<dyn Error>> {
        self.conn.flush()?;
        let mut data = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            while let Some(event) = self.conn.poll_for_event()? {
                match event {
                    XEvent::PropertyNotify(event)
                        if event.window == self.window
                            && event.atom == property
                            && event.state == Property::NEW_VALUE =>
                    {
                        let reply = self
                            .conn
                            .get_property(
                                true,
                                self.window,
                                property,
                                AtomEnum::ANY,
                                0,
                                u32::MAX / 4,
                            )?
                            .reply()?;
                        self.conn.flush()?;
                        let Some(bytes) = reply.value8() else {
                            return Ok(None);
                        };
                        let before = data.len();
                        data.extend(bytes);
                        if data.len() == before {
                            return Ok(String::from_utf8(data).ok());
                        }
                        if data.len() > MAX_PASTE_BYTES {
                            eprintln!("clipboard paste exceeded {MAX_PASTE_BYTES} bytes, aborting");
                            return Ok(None);
                        }
                        if Instant::now() >= deadline {
                            eprintln!("clipboard paste timed out mid-INCR transfer");
                            return Ok(None);
                        }
                    }
                    XEvent::SelectionRequest(event) => self.handle_selection_request(event)?,
                    XEvent::SelectionClear(event) => self.handle_selection_clear(event),
                    other => self.pending_events.push_back(other),
                }
            }
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                eprintln!("clipboard paste timed out mid-INCR transfer");
                return Ok(None);
            };
            self.wait_for_event(remaining)?;
        }
    }

    fn supported_text_target(&self, target: Atom) -> bool {
        target == self.clipboard_atoms.utf8_string
            || target == self.clipboard_atoms.text
            || target == self.clipboard_atoms.text_plain
            || target == self.clipboard_atoms.text_plain_utf8
            || target == u32::from(AtomEnum::STRING)
    }

    // COMPOUND_TEXT is deliberately not offered: it's an ISO-2022 encoding,
    // and serving UTF-8 bytes under that label renders as mojibake in
    // ICCCM-strict clients. Modern requestors negotiate UTF8_STRING instead.
    fn supported_targets(&self) -> [Atom; 9] {
        [
            self.clipboard_atoms.targets,
            self.clipboard_atoms.multiple,
            self.clipboard_atoms.timestamp,
            self.clipboard_atoms.save_targets,
            self.clipboard_atoms.utf8_string,
            self.clipboard_atoms.text_plain_utf8,
            self.clipboard_atoms.text_plain,
            self.clipboard_atoms.text,
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
                &[self.selection_time],
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
        for shm_image in &self.shm_images {
            let _ = self.conn.shm_detach(shm_image.seg);
        }
        let _ = self.conn.free_pixmap(self.back_pix);
        let _ = self.conn.free_gc(self.gc);
        if let Some(cursors) = self.cursors {
            for cursor in cursors {
                let _ = self.conn.free_cursor(cursor);
            }
        }
        let _ = self.conn.flush();
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
            cozy_clipboard: intern_atom(conn, b"COZYUI_CLIPBOARD")?,
            incr: intern_atom(conn, b"INCR")?,
        })
    }
}

fn intern_atom(conn: &XCBConnection, name: &[u8]) -> Result<Atom, Box<dyn Error>> {
    Ok(conn.intern_atom(false, name)?.reply()?.atom)
}

/// Cover every non-`TRANSPARENT` pixel with rectangles from the cached
/// per-row runs, with identical consecutive rows merged into taller bands.
fn rects_from_rows(rows: &[Vec<(usize, usize)>]) -> Vec<Rectangle> {
    let empty: Vec<(usize, usize)> = Vec::new();
    let mut rects = Vec::new();
    let mut band_runs = &empty;
    let mut band_start = 0;
    for y in 0..=rows.len() {
        let runs = rows.get(y).unwrap_or(&empty);
        if runs != band_runs {
            for &(x, w) in band_runs {
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

/// (x, width) spans of non-`TRANSPARENT` pixels in one row, written into
/// `out` (which is cleared first). Reusing a scratch buffer across calls
/// avoids allocating a fresh `Vec` for every row scanned on every partial
/// redraw.
fn row_runs_into(row: &[u8], out: &mut Vec<(usize, usize)>) {
    out.clear();
    let mut start = None;
    for (x, &index) in row.iter().enumerate() {
        match (index != TRANSPARENT, start) {
            (true, None) => start = Some(x),
            (false, Some(s)) => {
                out.push((s, x - s));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        out.push((s, row.len() - s));
    }
}

fn selection_property(event: SelectionRequestEvent) -> Atom {
    if event.property == u32::from(AtomEnum::NONE) {
        event.target
    } else {
        event.property
    }
}

/// Intersect `rect` with the framebuffer bounds; `None` if nothing remains.
fn clip_to_fb(rect: Rect, fb: &Framebuffer) -> Option<Rect> {
    if rect.x >= fb.width || rect.y >= fb.height {
        return None;
    }
    let w = rect.w.min(fb.width - rect.x);
    let h = rect.h.min(fb.height - rect.y);
    if w == 0 || h == 0 {
        return None;
    }
    Some(Rect::new(rect.x, rect.y, w, h))
}
