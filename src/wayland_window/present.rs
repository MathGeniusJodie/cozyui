//! Presenting a `Framebuffer` to the layer surface: LUT translation into the
//! full-surface staging canvas, `wl_shm` buffer upload, and the input-region
//! computation that gives transparent pixels X-SHAPE-style click-through.

use std::error::Error;

use smithay_client_toolkit::shell::WaylandSurface as _;
use wayland_client::protocol::wl_shm;

use super::{State, WaylandWindow, exclusive_zone};
use crate::window::{opaque_bands, row_runs_into};
use crate::{Framebuffer, Palette, Rect, TRANSPARENT};

impl WaylandWindow {
    /// Refresh the index->BGRA present table from the active palette. Unlike
    /// X (where SHAPE cuts holes instead), the transparent index really
    /// carries alpha 0 — all zeros, since `wl_shm` ARGB is premultiplied.
    pub(crate) fn set_palette(&mut self, palette: &Palette) {
        let mut lut = palette.present_lut();
        if self.state.transparent {
            lut[TRANSPARENT as usize] = [0, 0, 0, 0];
        }
        self.state.lut = lut;
    }

    pub(crate) fn draw(&mut self, fb: &Framebuffer) -> Result<(), Box<dyn Error>> {
        self.draw_rect(fb, Rect::new(0, 0, fb.width, fb.height))
    }

    pub(crate) fn draw_rect(&mut self, fb: &Framebuffer, rect: Rect) -> Result<(), Box<dyn Error>> {
        self.state.blit_to_staging(fb, rect);
        self.state.present()?;
        self.state.conn.flush()?;
        Ok(())
    }

    /// App-initiated resize. Only the width is ours to choose — the surface
    /// is anchored to both vertical edges, so the compositor keeps deciding
    /// the height and answers with a configure (surfacing as `Resized`).
    pub(crate) fn resize(&mut self, width: usize, _height: usize) -> Result<(), Box<dyn Error>> {
        let state = &mut self.state;
        state.requested_width = width;
        state.layer.set_size(width as u32, 0);
        state.layer.set_exclusive_zone(exclusive_zone(width));
        state.layer.commit();
        state.conn.flush()?;
        Ok(())
    }

    /// The staging canvas tracks the compositor-configured size, which this
    /// call doesn't change, and pool buffers are sized per present; nothing
    /// to rebuild.
    pub(crate) fn resize_backing(
        &mut self,
        _width: usize,
        _height: usize,
    ) -> Result<(), Box<dyn Error>> {
        Ok(())
    }
}

impl State {
    /// Translate `rect` of the framebuffer into the staging canvas through
    /// the LUT, clipped to both the framebuffer and the surface (the two
    /// disagree mid-relayout and while a configure is in flight). Dirty rows
    /// have their input-region runs rescanned as they are touched.
    fn blit_to_staging(&mut self, fb: &Framebuffer, rect: Rect) {
        if rect.x < 0 || rect.y < 0 {
            return;
        }
        let (x0, y0) = (rect.x as usize, rect.y as usize);
        let max_w = fb.width.min(self.width);
        let max_h = fb.height.min(self.height);
        if x0 >= max_w || y0 >= max_h {
            return;
        }
        let w = rect.w.min(max_w - x0);
        let h = rect.h.min(max_h - y0);

        let full = self.transparent && self.input_rows.len() != self.height;
        if full {
            self.input_rows = vec![Vec::new(); self.height];
            self.input_region_stale = true;
        }
        for y in y0..y0 + h {
            let row = &fb.row(y as isize)[x0..x0 + w];
            let out = &mut self.staging[(y * self.width + x0) * 4..][..w * 4];
            for (pixel, &index) in out.chunks_exact_mut(4).zip(row) {
                pixel.copy_from_slice(&self.lut[index as usize]);
            }
            if self.transparent {
                // The run scan covers the whole framebuffer row, not just the
                // blitted span: runs are per-row state and must stay whole.
                let fb_row = &fb.row(y as isize)[..max_w];
                row_runs_into(fb_row, &mut self.row_scratch);
                if self.row_scratch != self.input_rows[y] {
                    std::mem::swap(&mut self.row_scratch, &mut self.input_rows[y]);
                    self.input_region_stale = true;
                }
            }
        }
    }

    /// Upload the staging canvas and commit. The whole canvas is copied and
    /// the whole surface damaged every time: pool buffers rotate through
    /// slots whose previous contents are several frames old, so per-rect
    /// damage against them would resurrect stale pixels.
    fn present(&mut self) -> Result<(), Box<dyn Error>> {
        if !self.configured || self.width == 0 || self.height == 0 {
            return Ok(());
        }
        let (w, h) = (self.width as i32, self.height as i32);
        let (buffer, canvas) = self
            .pool
            .create_buffer(w, h, w * 4, wl_shm::Format::Argb8888)?;
        canvas[..self.staging.len()].copy_from_slice(&self.staging);

        self.update_input_region();
        let surface = self.layer.wl_surface();
        surface.damage_buffer(0, 0, w, h);
        buffer.attach_to(surface)?;
        self.layer.commit();
        Ok(())
    }

    /// Sync the surface's input region to the cached opaque runs, if they
    /// changed since the last commit. Pixels outside every band (transparent
    /// desk holes, the strip below the framebuffer) pass pointer events
    /// through to whatever is behind the surface.
    fn update_input_region(&mut self) {
        if !self.transparent || !self.input_region_stale {
            return;
        }
        let region = self.compositor.wl_compositor().create_region(&self.qh, ());
        for (x, y, w, h) in opaque_bands(&self.input_rows) {
            region.add(x as i32, y as i32, w as i32, h as i32);
        }
        self.layer.wl_surface().set_input_region(Some(&region));
        // The compositor keeps its own copy from the next commit; the object
        // itself is no longer needed.
        region.destroy();
        self.input_region_stale = false;
    }
}
