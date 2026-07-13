//! The pty/alacritty backend: owns the `Term`, forwards key and mouse input
//! to the shell running inside it, and reports what changed since the last
//! frame. Screen-content rendering (turning cells into pixels) lives in
//! `chrome`; this module only knows about terminal state.

use std::borrow::Cow;
use std::error::Error;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, EventLoopSender, Msg, State};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::tty;

use super::keys::{is_copy_shortcut, key_bytes, key_scroll};
use super::{
    GLYPH_H, GLYPH_W, PressState, SCREEN_H, SCREEN_SOURCE_X, SCREEN_SOURCE_Y, SCREEN_W,
    art_x, art_y,
};
use crate::text::KeyInput;

struct TermSize {
    columns: usize,
    lines: usize,
}

impl Dimensions for TermSize {
    fn total_lines(&self) -> usize {
        self.lines
    }

    fn screen_lines(&self) -> usize {
        self.lines
    }

    fn columns(&self) -> usize {
        self.columns
    }
}

#[derive(Clone)]
pub(super) struct UiEventProxy(Sender<Event>);

impl EventListener for UiEventProxy {
    fn send_event(&self, event: Event) {
        let _ = self.0.send(event);
    }
}

type TerminalEventLoop = EventLoop<tty::Pty, UiEventProxy>;
type TerminalThread = JoinHandle<(TerminalEventLoop, State)>;

pub(super) struct Terminal {
    rx: Receiver<Event>,
    tx: EventLoopSender,
    term: Arc<FairMutex<Term<UiEventProxy>>>,
    window_size: WindowSize,
    event_thread: Option<TerminalThread>,
    clipboard: FairMutex<String>,
    /// Set after the first failed pty write so we only diagnose it once.
    pty_send_failed: crate::util::FailureLog,
}

impl Terminal {
    pub(super) fn open(window_id: u64) -> Result<Self, Box<dyn Error>> {
        // One column is kept clear at the screen's right edge so bold/glow
        // overdraw (glyphs redrawn at x+1) stays inside the CRT screen art.
        const COLUMN_MARGIN: usize = 1;
        let size = TermSize {
            columns: SCREEN_W / GLYPH_W - COLUMN_MARGIN,
            lines: SCREEN_H / GLYPH_H,
        };
        let window_size = WindowSize {
            num_lines: size.lines as u16,
            num_cols: size.columns as u16,
            cell_width: GLYPH_W as u16,
            cell_height: GLYPH_H as u16,
        };

        let (ui_tx, rx) = mpsc::channel();
        let proxy = UiEventProxy(ui_tx);
        let config = Config {
            scrolling_history: 10_000,
            ..Config::default()
        };
        let term = Arc::new(FairMutex::new(Term::new(config, &size, proxy.clone())));
        // Run the shell inside a persistent abduco session so the terminal
        // survives cozyui restarts; -A reattaches if "puter" already exists.
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
        let options = tty::Options {
            shell: Some(tty::Shell::new(
                "abduco".to_string(),
                vec!["-A".to_string(), "puter".to_string(), shell],
            )),
            ..tty::Options::default()
        };
        let pty = tty::new(&options, window_size, window_id)?;
        let event_loop = EventLoop::new(term.clone(), proxy, pty, true, false)?;
        let tx = event_loop.channel();
        let event_thread = Some(event_loop.spawn());

        Ok(Self {
            rx,
            tx,
            pty_send_failed: crate::util::FailureLog::new(),
            term,
            window_size,
            event_thread,
            clipboard: FairMutex::new(String::new()),
        })
    }

    pub(super) const fn term(&self) -> &Arc<FairMutex<Term<UiEventProxy>>> {
        &self.term
    }

    /// Send a message to the pty event loop, diagnosing (once) if it fails.
    fn send_pty(&self, msg: Msg) {
        if self.tx.send(msg).is_err() {
            self.pty_send_failed.record_err(|| {
                "puter: failed to write to pty (further failures will be suppressed)".to_string()
            });
        }
    }

    pub(super) fn drain_events(&self) -> super::TerminalEvents {
        let mut running = true;
        let mut dirty = false;
        while let Ok(event) = self.rx.try_recv() {
            dirty = true;
            match event {
                Event::Exit | Event::ChildExit(_) => running = false,
                Event::PtyWrite(text) => {
                    self.send_pty(Msg::Input(Cow::Owned(text.into_bytes())));
                }
                Event::TextAreaSizeRequest(formatter) => {
                    let text = formatter(self.window_size);
                    self.send_pty(Msg::Input(Cow::Owned(text.into_bytes())));
                }
                _ => {}
            }
        }
        super::TerminalEvents { running, dirty }
    }

    pub(super) fn handle_key_press(
        &self,
        input: &KeyInput,
        clipboard_text: Option<&str>,
    ) -> Option<String> {
        if let Some(scroll) = key_scroll(input) {
            self.scroll(scroll);
            None
        } else if is_copy_shortcut(input) {
            self.selection_to_clipboard()
        } else if input.is_paste_shortcut() {
            // Read the mode before locking `clipboard` so we never hold both
            // locks at once (avoids any chance of lock-order deadlock with
            // other code that takes `term` and `clipboard` separately).
            let bracketed = self.term.lock().mode().contains(TermMode::BRACKETED_PASTE);
            let fallback = self.clipboard.lock();
            let text =
                clipboard_text.or_else(|| (!fallback.is_empty()).then_some(fallback.as_str()))?;
            self.scroll(Scroll::Bottom);
            self.send_pty(Msg::Input(Cow::Owned(paste_bytes(bracketed, text))));
            None
        } else if let Some(bytes) = key_bytes(input) {
            self.scroll(Scroll::Bottom);
            self.send_pty(Msg::Input(Cow::Owned(bytes.into_bytes())));
            None
        } else {
            None
        }
    }

    /// Whether the mouse-down started a text selection or was forwarded as a
    /// mouse-down escape to the pty's SGR mouse mode. The two are mutually
    /// exclusive: a forwarded mouse-down never starts a selection.
    #[allow(clippy::significant_drop_tightening)]
    pub(super) fn mouse_press(&self, x: isize, y: isize, shift: bool) -> PressState {
        let Some(point) = screen_point(x, y, &self.window_size) else {
            return PressState::None;
        };

        let mouse_mode = self.term.lock().mode().intersects(TermMode::MOUSE_MODE);
        if mouse_mode && !shift {
            self.send_mouse(point, 0, true);
            return PressState::ForwardedMouse;
        }

        self.scroll(Scroll::Bottom);
        let mut term = self.term.lock();
        term.selection = Some(Selection::new(SelectionType::Simple, point, Side::Left));
        PressState::Selection(point)
    }

    pub(super) fn mouse_motion(&self, point: Point) -> bool {
        let mut term = self.term.lock();
        term.selection.as_mut().is_some_and(|selection| {
            selection.update(point, Side::Right);
            true
        })
    }

    /// Like `screen_point`, but clamps out-of-grid coordinates instead of
    /// rejecting them; see the free function of the same name for why.
    pub(super) fn clamped_screen_point(&self, x: isize, y: isize) -> Point {
        clamped_screen_point(x, y, &self.window_size)
    }

    pub(super) fn mouse_release(&self, x: isize, y: isize) {
        // Clamped, not rejected: the caller only forwards a release here to
        // balance a mouse-down it already forwarded, so this must always
        // send the matching mouse-up even if the drag ended outside the grid.
        let point = clamped_screen_point(x, y, &self.window_size);

        if self.term.lock().mode().intersects(TermMode::MOUSE_MODE) {
            self.send_mouse(point, 0, false);
        }
    }

    fn send_mouse(&self, point: Point, button: usize, pressed: bool) {
        let suffix = if pressed { 'M' } else { 'm' };
        let text = format!(
            "\x1b[<{};{};{}{}",
            button,
            point.column.0 + 1,
            point.line.0 + 1,
            suffix
        );
        self.send_pty(Msg::Input(Cow::Owned(text.into_bytes())));
    }

    fn copy_selection(&self) -> Option<String> {
        self.term.lock().selection_to_string()
    }

    pub(super) fn selection_to_clipboard(&self) -> Option<String> {
        let fallback = self.clipboard.lock();
        let text = self
            .copy_selection()
            .or_else(|| (!fallback.is_empty()).then_some(fallback.clone()))?;
        drop(fallback);
        self.clipboard.lock().clone_from(&text);
        Some(text)
    }

    pub(super) fn scroll(&self, scroll: Scroll) {
        self.term.lock().scroll_display(scroll);
    }

    pub(super) fn shutdown(mut self) {
        self.send_pty(Msg::Shutdown);
        if let Some(event_thread) = self.event_thread.take() {
            let _ = event_thread.join();
        }
    }
}

/// What to actually write to the pty for a paste, given whether the
/// application enabled bracketed-paste mode (`DECSET 2004`).
///
/// Bracketed paste tells the shell/editor "this text arrived from a paste,
/// not typing", which lets it, e.g., disable auto-indent in vim or avoid
/// treating pasted newlines as "run this line now" in a shell; we honor it
/// by wrapping the text in `ESC[200~`/`ESC[201~`. ESC and ETX are filtered
/// out of the pasted text first so a hostile/corrupted clipboard can't
/// smuggle its own bytes past the bracket (e.g. close it early with `ESC[201~`
/// and then inject arbitrary escape sequences or a Ctrl-C).
///
/// If the application never asked for bracketed paste, we fall back to what
/// terminals did before that mode existed: pasted newlines become carriage
/// returns (so a pasted multi-line snippet is fed to the shell a line at a
/// time, as if typed), and other C0 control bytes -- ESC in particular -- are
/// stripped so they can't be misread as an escape sequence.
fn paste_bytes(bracketed: bool, text: &str) -> Vec<u8> {
    if bracketed {
        let filtered: String = text.chars().filter(|&c| c != '\x1b' && c != '\x03').collect();
        let mut bytes = Vec::with_capacity(filtered.len() + 12);
        bytes.extend_from_slice(b"\x1b[200~");
        bytes.extend_from_slice(filtered.as_bytes());
        bytes.extend_from_slice(b"\x1b[201~");
        bytes
    } else {
        text.replace("\r\n", "\n")
            .replace('\n', "\r")
            .chars()
            .filter(|&c| c == '\r' || c == '\t' || !c.is_control())
            .collect::<String>()
            .into_bytes()
    }
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn screen_point(x: isize, y: isize, size: &WindowSize) -> Option<Point> {
    if x < 0 || y < 0 {
        return None;
    }

    let x = x as usize;
    let y = y as usize;
    let screen_x = art_x(SCREEN_SOURCE_X);
    let screen_y = art_y(SCREEN_SOURCE_Y);
    if x < screen_x || y < screen_y || x >= screen_x + SCREEN_W || y >= screen_y + SCREEN_H {
        return None;
    }

    Some(cell_at(x, y, screen_x, screen_y, size))
}

/// Like `screen_point`, but clamps out-of-grid coordinates to the nearest
/// cell instead of rejecting them. Used for mouse-release: a drag can start
/// inside the grid (forwarding a mouse-down escape) and end past its edge,
/// and dropping that release (as `screen_point` would) leaves the remote pty
/// app thinking the button is still held, since the matching mouse-up escape
/// never arrives.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn clamped_screen_point(x: isize, y: isize, size: &WindowSize) -> Point {
    let screen_x = art_x(SCREEN_SOURCE_X);
    let screen_y = art_y(SCREEN_SOURCE_Y);
    let x = (x.max(0) as usize).clamp(screen_x, screen_x + SCREEN_W - 1);
    let y = (y.max(0) as usize).clamp(screen_y, screen_y + SCREEN_H - 1);
    cell_at(x, y, screen_x, screen_y, size)
}

/// Column/line math shared by `screen_point` and `clamped_screen_point`;
/// `x`/`y` must already be within the screen rect.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn cell_at(x: usize, y: usize, screen_x: usize, screen_y: usize, size: &WindowSize) -> Point {
    let column =
        ((x - screen_x) / size.cell_width as usize).min((size.num_cols as usize).saturating_sub(1));
    let line = ((y - screen_y) / size.cell_height as usize)
        .min((size.num_lines as usize).saturating_sub(1));
    Point::new(Line(line as i32), Column(column))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bracketed_paste_wraps_text_in_bracket_markers() {
        let bytes = paste_bytes(true, "echo hi");
        assert_eq!(bytes, b"\x1b[200~echo hi\x1b[201~");
    }

    #[test]
    fn bracketed_paste_filters_esc_and_etx_so_clipboard_cant_escape_bracket() {
        let bytes = paste_bytes(true, "a\x1bb\x03c");
        assert_eq!(bytes, b"\x1b[200~abc\x1b[201~");
    }

    #[test]
    fn unbracketed_paste_converts_newlines_to_carriage_returns_and_strips_esc() {
        let bytes = paste_bytes(false, "line1\r\nline2\nline3\x1b[31m");
        assert_eq!(bytes, b"line1\rline2\rline3[31m");
    }

    #[test]
    fn unbracketed_paste_keeps_tabs() {
        let bytes = paste_bytes(false, "a\tb");
        assert_eq!(bytes, b"a\tb");
    }
}
