mod clipboard;
mod cursor;
mod present;

use std::collections::VecDeque;
use std::error::Error;
#[cfg(unix)]
use std::os::fd::AsRawFd as _;
use std::time::Duration;

use x11rb::connection::{Connection, RequestConnection};
use x11rb::protocol::Event as XEvent;
use x11rb::protocol::shape;
use x11rb::protocol::shm::{ConnectionExt as ShmConnectionExt, Seg};
use x11rb::protocol::xproto::ConnectionExt as XprotoConnectionExt;
use x11rb::protocol::xproto::{
    AtomEnum, BackingStore, ChangeWindowAttributesAux, CreateGCAux, CreateWindowAux, Cursor,
    EventMask, Gcontext, Gravity, Pixmap, PropMode, Rectangle, Timestamp, Window, WindowClass,
};
use x11rb::wrapper::ConnectionExt as _;
use x11rb::xcb_ffi::XCBConnection;

use clipboard::ClipboardAtoms;
use present::ShmBacking;

use crate::text::input as text_input;
use crate::window::UiEvent;
use crate::{CURSOR_KIND_COUNT, CursorKind};
use pixel_graphics::PresentLut;

/// X core button numbers for the scroll wheel.
const WHEEL_UP: u8 = 4;
const WHEEL_DOWN: u8 = 5;
/// The Shift bit in a core event's `state` field.
const SHIFT_MASK: u16 = 1;
/// Left mouse button (`ButtonIndex::M1`).
const BUTTON_LEFT: u8 = 1;

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
    /// round trip. `Unavailable` when SHM couldn't be set up, falling back
    /// to the slower `put_image` path.
    shm_backing: ShmBacking,
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
        let shm_backing = Self::open_shm_images_logged(&conn, width, height);

        Ok(Self {
            conn,
            window,
            gc,
            depth,
            back_pix,
            keyboard,
            upload_buffer: Vec::new(),
            shm_backing,
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
    /// clipboard wait before polling the connection, records input timestamps
    /// for ICCCM-correct selection requests, and translates the X vocabulary
    /// into backend-neutral `UiEvent`s. Protocol-internal events (SHM
    /// completions, selection traffic, xkb release bookkeeping) are consumed
    /// here without surfacing.
    pub(crate) fn poll_event(&mut self) -> Result<Option<UiEvent>, Box<dyn Error>> {
        loop {
            let event = match self.pending_events.pop_front() {
                Some(event) => Some(event),
                None => self.conn.poll_for_event()?,
            };
            let Some(event) = event else {
                return Ok(None);
            };
            self.note_event_time(&event);
            match event {
                // SHM completions are internal bookkeeping, not app events.
                XEvent::ShmCompletion(completion) => {
                    let seg: Seg = completion.shmseg;
                    self.shm_completed(seg);
                }
                XEvent::KeyPress(e) => {
                    let input = self.keyboard.press(e.detail, e.state.into());
                    return Ok(Some(UiEvent::Key(input)));
                }
                XEvent::KeyRelease(e) => self.keyboard.release(e.detail),
                XEvent::ButtonPress(e) => {
                    let (x, y) = (isize::from(e.event_x), isize::from(e.event_y));
                    match e.detail {
                        WHEEL_UP => return Ok(Some(UiEvent::ScrollUp { x, y })),
                        WHEEL_DOWN => return Ok(Some(UiEvent::ScrollDown { x, y })),
                        BUTTON_LEFT => {
                            let shift = u16::from(e.state) & SHIFT_MASK != 0;
                            return Ok(Some(UiEvent::Press { x, y, shift }));
                        }
                        _ => {}
                    }
                }
                XEvent::ButtonRelease(e) if e.detail == BUTTON_LEFT => {
                    return Ok(Some(UiEvent::Release {
                        x: isize::from(e.event_x),
                        y: isize::from(e.event_y),
                    }));
                }
                XEvent::MotionNotify(e) => {
                    return Ok(Some(UiEvent::Motion {
                        x: isize::from(e.event_x),
                        y: isize::from(e.event_y),
                    }));
                }
                XEvent::SelectionRequest(e) => self.handle_selection_request(e)?,
                XEvent::SelectionClear(e) => self.handle_selection_clear(e),
                XEvent::ConfigureNotify(e) => {
                    return Ok(Some(UiEvent::Resized {
                        width: e.width as usize,
                        height: e.height as usize,
                    }));
                }
                XEvent::DestroyNotify(_) => return Ok(Some(UiEvent::Closed)),
                _ => {}
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
}

impl Drop for XWindow {
    fn drop(&mut self) {
        for shm_image in self.shm_backing.as_slice() {
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
