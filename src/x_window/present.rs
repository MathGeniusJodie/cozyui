//! Presenting a `Framebuffer` to the X window: the SHM double-buffer (falling
//! back to `put_image` when SHM is unavailable), the always-current
//! background pixmap that lets the server repaint exposes on its own, and the
//! SHAPE-extension bounding-region computation that backs click-through
//! transparency.

use std::error::Error;
use std::fs::File;
#[cfg(unix)]
use std::os::fd::OwnedFd;

use memmap2::MmapMut;
use x11rb::connection::Connection;
use x11rb::protocol::Event as XEvent;
use x11rb::protocol::shape::{self, ConnectionExt as ShapeConnectionExt};
use x11rb::protocol::shm::{ConnectionExt as ShmConnectionExt, Seg};
use x11rb::protocol::xproto::ConnectionExt as XprotoConnectionExt;
use x11rb::protocol::xproto::{
    ChangeWindowAttributesAux, ClipOrdering, ConfigureWindowAux, Gcontext, ImageFormat, Pixmap,
    Rectangle, Window,
};
use x11rb::xcb_ffi::XCBConnection;

use super::XWindow;
use crate::{Framebuffer, Palette, Rect, TRANSPARENT};

pub(super) struct ShmImage {
    pub(super) seg: Seg,
    mmap: MmapMut,
    /// A `shm_put_image` from this segment is still in flight; cleared by its
    /// completion event (or a full sync).
    busy: bool,
}

/// The SHM double-buffer is always either entirely absent or exactly two
/// segments by construction (`open_shm_images_logged` never produces any
/// other shape); this says so in the type instead of leaving `Vec<ShmImage>`
/// to informally hold that invariant.
pub(super) enum ShmBacking {
    Unavailable,
    Double([ShmImage; 2]),
}

impl ShmBacking {
    pub(super) fn as_slice(&self) -> &[ShmImage] {
        match self {
            Self::Unavailable => &[],
            Self::Double(images) => images,
        }
    }

    fn as_mut_slice(&mut self) -> &mut [ShmImage] {
        match self {
            Self::Unavailable => &mut [],
            Self::Double(images) => images,
        }
    }
}

impl XWindow {
    /// Refresh the index->BGRA present table from the active palette.
    pub(crate) fn set_palette(&mut self, palette: &Palette) {
        self.lut = palette.present_lut();
    }

    /// The server finished reading `seg`; its segment can be reused.
    pub(super) fn shm_completed(&mut self, seg: Seg) {
        for image in self.shm_backing.as_mut_slice() {
            if image.seg == seg {
                image.busy = false;
            }
        }
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
        let mmap = match unsafe { MmapMut::map_mut(&file) } {
            Ok(mmap) => mmap,
            Err(err) => {
                let _ = conn.shm_detach(seg);
                return Err(err.into());
            }
        };
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

    #[cfg(not(unix))]
    fn open_shm_image(
        _conn: &XCBConnection,
        _width: usize,
        _height: usize,
    ) -> Result<ShmImage, Box<dyn Error>> {
        Err("MIT-SHM fd passing requires Unix".into())
    }

    /// Open the two alternating SHM segments, degrading to `Unavailable` (the
    /// slow `put_image` path) with a diagnostic instead of failing.
    pub(super) fn open_shm_images_logged(
        conn: &XCBConnection,
        width: usize,
        height: usize,
    ) -> ShmBacking {
        let first = match Self::open_shm_image(conn, width, height) {
            Ok(image) => image,
            Err(err) => {
                eprintln!("MIT-SHM unavailable, falling back to put_image: {err}");
                return ShmBacking::Unavailable;
            }
        };
        let second = match Self::open_shm_image(conn, width, height) {
            Ok(image) => image,
            Err(err) => {
                eprintln!("MIT-SHM unavailable, falling back to put_image: {err}");
                let _ = conn.shm_detach(first.seg);
                return ShmBacking::Unavailable;
            }
        };
        ShmBacking::Double([first, second])
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
            row_runs_into(fb.row(y as isize), &mut self.row_scratch);
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
    pub(super) fn create_back_pixmap(
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

    /// Drains every currently-queued event, applying completions (freeing
    /// their segment) and buffering anything else in `pending_events` for the
    /// normal event loop. Shared by `sync_shm` and `free_shm_index`, which
    /// only differ in what they do before/after this drain.
    fn drain_shm_completions(&mut self) -> Result<(), Box<dyn Error>> {
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

    /// Full barrier: round-trip to the server, after which every outstanding
    /// `shm_put_image` has been executed and its completion event is queued.
    /// Draining the queue (rather than force-clearing `busy`) consumes those
    /// completions now; a blind clear would leave them behind to wrongly
    /// free a segment re-used by a later put while the server still reads
    /// it. Only the fallback when both segments are still busy.
    fn sync_shm(&mut self) -> Result<(), Box<dyn Error>> {
        self.conn.get_input_focus()?.reply()?;
        self.drain_shm_completions()
    }

    /// A segment index that is free to write into. Drains any pending
    /// completion events first (buffering unrelated events); if both segments
    /// are somehow still in flight, falls back to a full sync.
    fn free_shm_index(&mut self) -> Result<Option<usize>, Box<dyn Error>> {
        if self.shm_backing.as_slice().is_empty() {
            return Ok(None);
        }
        if !self.shm_backing.as_slice().iter().any(|image| image.busy) {
            return Ok(Some(0));
        }
        self.drain_shm_completions()?;
        if let Some(index) = self
            .shm_backing
            .as_slice()
            .iter()
            .position(|image| !image.busy)
        {
            return Ok(Some(index));
        }
        self.sync_shm()?;
        // The barrier consumed every completion, so a segment must be free
        // now. Re-check instead of assuming: if the ordering guarantee this
        // relies on ever breaks, skip the frame rather than put_image into a
        // segment the server may still be reading.
        Ok(self
            .shm_backing
            .as_slice()
            .iter()
            .position(|image| !image.busy))
    }

    pub(crate) fn draw(&mut self, fb: &Framebuffer) -> Result<(), Box<dyn Error>> {
        self.update_shape_rows(fb, 0, fb.height)?;
        let Some(rect) = clip_to_fb(Rect::new(0, 0, fb.width, fb.height), fb) else {
            return Ok(());
        };
        self.blit_rect(fb, rect)
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
        if self.shm_backing.as_slice().iter().any(|image| image.busy) {
            self.sync_shm()?;
        }
        for shm_image in self.shm_backing.as_slice() {
            self.conn.shm_detach(shm_image.seg)?;
        }
        self.shm_backing = Self::open_shm_images_logged(&self.conn, width, height);
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
        let Some(clipped) = clip_to_fb(rect, fb) else {
            return Ok(());
        };
        let rect = clipped.rect();
        if rect.x == 0 && rect.y == 0 && rect.w == fb.width && rect.h == fb.height {
            return self.draw(fb);
        }
        let y0 = rect.y as usize;
        self.update_shape_rows(fb, y0, y0 + rect.h)?;
        self.blit_rect(fb, clipped)
    }

    /// Present `rect` of the framebuffer, via SHM when available, else a
    /// `put_image` upload. Pixels land in `back_pix` (the window's
    /// background), and a `clear_area` tells the server to show that region
    /// — the copy the server keeps is what it also repaints exposes from.
    /// Taking a `ClippedRect` (producible only via `clip_to_fb`) instead of a
    /// plain `Rect` means the bounds invariant is enforced by the type, not a
    /// runtime re-check here.
    fn blit_rect(&mut self, fb: &Framebuffer, rect: ClippedRect) -> Result<(), Box<dyn Error>> {
        let rect = rect.rect();
        let byte_len = rect.w * rect.h * Framebuffer::BYTES_PER_PIXEL;
        if let Some(index) = self.free_shm_index()? {
            let shm_image = &mut self.shm_backing.as_mut_slice()[index];
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

use clipped_rect::{ClippedRect, clip_to_fb};

/// A `Rect` guaranteed to lie fully within some framebuffer's bounds. The
/// inner `Rect` is private to this module, so the only way to build one is
/// `clip_to_fb` — callers that receive a `ClippedRect` don't need to
/// re-check it themselves.
mod clipped_rect {
    use crate::{Framebuffer, Rect};

    #[derive(Clone, Copy)]
    pub(super) struct ClippedRect(Rect);

    impl ClippedRect {
        pub(super) const fn rect(self) -> Rect {
            self.0
        }
    }

    /// Intersect `rect` with the framebuffer bounds; `None` if nothing remains
    /// (including when `rect`'s origin is negative — mid-relayout widget
    /// rects can momentarily disagree with the framebuffer size).
    pub(super) fn clip_to_fb(rect: Rect, fb: &Framebuffer) -> Option<ClippedRect> {
        if rect.x < 0 || rect.y < 0 {
            return None;
        }
        let (x, y) = (rect.x as usize, rect.y as usize);
        if x >= fb.width || y >= fb.height {
            return None;
        }
        let w = rect.w.min(fb.width - x);
        let h = rect.h.min(fb.height - y);
        if w == 0 || h == 0 {
            return None;
        }
        Some(ClippedRect(Rect::new(rect.x, rect.y, w, h)))
    }
}
