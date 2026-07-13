//! Backend-neutral windowing: the input/lifecycle events `main` consumes and
//! the `Window` enum dispatching to the platform implementation. Everything
//! X- or Wayland-specific (selection protocol round-trips, xkb bookkeeping,
//! SHM completions) stays inside the respective backend's `poll_event`.

use std::error::Error;
use std::time::Duration;

use crate::text::KeyInput;
use crate::wayland_window::WaylandWindow;
use crate::x_window::XWindow;
use crate::{CursorKind, Framebuffer, Palette, Rect, TRANSPARENT};

/// One user-visible event, already translated out of the platform's
/// vocabulary. Pointer coordinates are window-local pixels.
pub(crate) enum UiEvent {
    /// A key press, fully resolved through xkb (sym, text, modifiers).
    Key(KeyInput),
    /// Left-button press. `shift` feeds the terminal's mouse-mode override.
    Press { x: isize, y: isize, shift: bool },
    /// Left-button release.
    Release { x: isize, y: isize },
    Motion { x: isize, y: isize },
    ScrollUp { x: isize, y: isize },
    ScrollDown { x: isize, y: isize },
    /// The window's actual size changed (compositor/WM-initiated); the
    /// backing buffers are NOT resized yet — the caller decides the new
    /// layout and calls `resize_backing`.
    Resized { width: usize, height: usize },
    /// The window is gone or the compositor asked us to stop.
    Closed,
}

/// (x, width) spans of non-`TRANSPARENT` pixels in one row, written into
/// `out` (which is cleared first). Reusing a scratch buffer across calls
/// avoids allocating a fresh `Vec` for every row scanned on every partial
/// redraw. Both backends build their click-through geometry from this: X for
/// the SHAPE bounding region, Wayland for the surface's input region.
pub(crate) fn row_runs_into(row: &[u8], out: &mut Vec<(usize, usize)>) {
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

/// Cover every opaque run with `(x, y, w, h)` rectangles, with identical
/// consecutive rows merged into taller bands.
pub(crate) fn opaque_bands(rows: &[Vec<(usize, usize)>]) -> Vec<(usize, usize, usize, usize)> {
    let empty: Vec<(usize, usize)> = Vec::new();
    let mut bands = Vec::new();
    let mut band_runs = &empty;
    let mut band_start = 0;
    for y in 0..=rows.len() {
        let runs = rows.get(y).unwrap_or(&empty);
        if runs != band_runs {
            for &(x, w) in band_runs {
                bands.push((x, band_start, w, y - band_start));
            }
            band_runs = runs;
            band_start = y;
        }
    }
    bands
}

// Boxed: the two backends differ by ~1KB in size, and exactly one Window
// exists for the whole program, so the indirection is free and silences
// clippy::large_enum_variant.
pub(crate) enum Window {
    X(Box<XWindow>),
    Wayland(Box<WaylandWindow>),
}

impl Window {
    /// Connect to the session's display server: Wayland when
    /// `WAYLAND_DISPLAY` is set, X11 otherwise.
    pub(crate) fn open(
        width: usize,
        height: usize,
        transparent: bool,
    ) -> Result<Self, Box<dyn Error>> {
        if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            Ok(Self::Wayland(Box::new(WaylandWindow::open(
                width,
                height,
                transparent,
            )?)))
        } else {
            Ok(Self::X(Box::new(XWindow::open(width, height, transparent)?)))
        }
    }

    /// The X window id for the terminal's `WINDOWID`; 0 on Wayland, which
    /// has no numeric window handles (matching what terminals do there).
    pub(crate) fn terminal_window_id(&self) -> u64 {
        match self {
            Self::X(x) => u64::from(x.window),
            Self::Wayland(_) => 0,
        }
    }

    pub(crate) fn set_palette(&mut self, palette: &Palette) {
        match self {
            Self::X(x) => x.set_palette(palette),
            Self::Wayland(w) => w.set_palette(palette),
        }
    }

    pub(crate) fn load_cursors(&mut self, palette: &Palette) -> Result<(), Box<dyn Error>> {
        match self {
            Self::X(x) => x.load_cursors(palette),
            Self::Wayland(w) => w.load_cursors(palette),
        }
    }

    pub(crate) fn set_cursor(&mut self, kind: CursorKind) -> Result<(), Box<dyn Error>> {
        match self {
            Self::X(x) => x.set_cursor(kind),
            Self::Wayland(w) => w.set_cursor(kind),
        }
    }

    pub(crate) fn draw(&mut self, fb: &Framebuffer) -> Result<(), Box<dyn Error>> {
        match self {
            Self::X(x) => x.draw(fb),
            Self::Wayland(w) => w.draw(fb),
        }
    }

    pub(crate) fn draw_rect(&mut self, fb: &Framebuffer, rect: Rect) -> Result<(), Box<dyn Error>> {
        match self {
            Self::X(x) => x.draw_rect(fb, rect),
            Self::Wayland(w) => w.draw_rect(fb, rect),
        }
    }

    /// App-initiated resize: request the new geometry and rebuild backings.
    pub(crate) fn resize(&mut self, width: usize, height: usize) -> Result<(), Box<dyn Error>> {
        match self {
            Self::X(x) => x.resize(width, height),
            Self::Wayland(w) => w.resize(width, height),
        }
    }

    /// Rebuild backings for a size the window already has (after `Resized`).
    pub(crate) fn resize_backing(
        &mut self,
        width: usize,
        height: usize,
    ) -> Result<(), Box<dyn Error>> {
        match self {
            Self::X(x) => x.resize_backing(width, height),
            Self::Wayland(w) => w.resize_backing(width, height),
        }
    }

    pub(crate) fn poll_event(&mut self) -> Result<Option<UiEvent>, Box<dyn Error>> {
        match self {
            Self::X(x) => x.poll_event(),
            Self::Wayland(w) => w.poll_event(),
        }
    }

    pub(crate) fn wait_for_event(&mut self, timeout: Duration) -> Result<(), Box<dyn Error>> {
        match self {
            Self::X(x) => x.wait_for_event(timeout),
            Self::Wayland(w) => w.wait_for_event(timeout),
        }
    }

    pub(crate) fn clipboard_text(&mut self) -> Result<Option<String>, Box<dyn Error>> {
        match self {
            Self::X(x) => x.clipboard_text(),
            Self::Wayland(w) => w.clipboard_text(),
        }
    }

    pub(crate) fn set_clipboard_text(&mut self, text: String) -> Result<(), Box<dyn Error>> {
        match self {
            Self::X(x) => x.set_clipboard_text(text),
            Self::Wayland(w) => w.set_clipboard_text(text),
        }
    }
}
