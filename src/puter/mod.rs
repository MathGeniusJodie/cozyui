use std::error::Error;

use alacritty_terminal::grid::Scroll;
use alacritty_terminal::index::Point;
use alacritty_terminal::tty;

use crate::text::KeyInput;
use crate::{CursorKind, Framebuffer, Index, Palette, Sprite, TRANSPARENT};

mod chrome;
mod keys;
mod terminal;

use chrome::{
    BUTTON_TARGETS, ButtonKind, DisplaySettings, GlyphAtlas, IconAction, ModeImages, button_at,
};
use terminal::Terminal;

const GLYPH_W: usize = 6;
const GLYPH_H: usize = 12;

const SCREEN_SOURCE_X: usize = 49;
const SCREEN_SOURCE_Y: usize = 49;
const SCREEN_W: usize = 205;
const SCREEN_H: usize = 158;

const ART_CROP_X: usize = 19;
const ART_CROP_Y: usize = 17;

const BUTTON_W: usize = 19;
const BUTTON_H: usize = 16;
const BUTTON_SPRITE_OFFSET_X: usize = 3;
const BUTTON_SPRITE_STRIDE: usize = 19;

const BUTTON_HIT_OFFSET_X: isize = -2;
const SCROLL_LINES: i32 = 3;

const fn art_x(x: usize) -> usize {
    x.saturating_sub(ART_CROP_X)
}

const fn art_y(y: usize) -> usize {
    y.saturating_sub(ART_CROP_Y)
}

pub struct TerminalEvents {
    pub(crate) running: bool,
    pub(crate) dirty: bool,
}

/// Mutually exclusive states for an in-progress mouse press. Replaces three
/// separately nullable fields (`active_button`, `selection_point`,
/// `mouse_down_forwarded`) that could otherwise disagree with each other.
#[derive(Clone, Copy)]
enum PressState {
    /// Nothing pressed.
    None,
    /// Holding down the front-panel button at this index into `BUTTON_TARGETS`.
    Chrome(usize),
    /// Dragging out a terminal text selection anchored at this point.
    Selection(Point),
    /// A mouse-down escape was forwarded to the pty's SGR mouse mode; the
    /// matching mouse-up is still owed.
    ForwardedMouse,
}

pub struct Puter {
    mode_images: ModeImages,
    button_sprites: Sprite,
    button_pressed_sprites: Sprite,
    power_button: Sprite,
    lock_button: Sprite,
    atlas: GlyphAtlas,
    terminal: Option<Terminal>,
    settings: DisplaySettings,
    /// What an in-progress mouse press is doing: nothing, holding a chrome
    /// button, dragging out a terminal text selection, or having forwarded a
    /// mouse-down escape to the pty (awaiting the matching mouse-up). These
    /// were three separately nullable fields that could disagree with each
    /// other; now there's exactly one state to keep in sync.
    press_state: PressState,
    /// `render_chrome`'s output, cached because it's static apart from the
    /// `DisplaySettings` it was last drawn with. Rebuilt (via interior
    /// mutability, since `render` only takes `&self`) whenever `settings`
    /// changes; blitted as-is otherwise.
    chrome_cache: std::cell::RefCell<Option<(DisplaySettings, Framebuffer)>>,
}

impl Puter {
    pub(crate) fn load(_palette: &Palette) -> Result<Self, Box<dyn Error>> {
        Ok(Self {
            mode_images: ModeImages::load(),
            button_sprites: crate::assets::buttons(),
            button_pressed_sprites: crate::assets::buttons_pressed(),
            power_button: crate::assets::puter_power(),
            lock_button: crate::assets::puter_lock(),
            atlas: GlyphAtlas::load(),
            terminal: None,
            settings: DisplaySettings::new(),
            press_state: PressState::None,
            chrome_cache: std::cell::RefCell::new(None),
        })
    }

    pub(crate) fn press_button(&mut self, x: isize, y: isize, shift: bool) {
        self.press_state = match button_at(x, y) {
            Some(index) => PressState::Chrome(index),
            None => self
                .terminal()
                .map_or(PressState::None, |term| term.mouse_press(x, y, shift)),
        };
    }

    /// Returns true when the power button was clicked and the app should quit.
    pub(crate) fn release_button(&mut self, x: isize, y: isize) -> bool {
        let mut quit = false;
        let released_button = button_at(x, y);
        if let (PressState::Chrome(pressed), Some(released)) = (self.press_state, released_button)
            && pressed == released
        {
            match BUTTON_TARGETS[pressed].kind {
                ButtonKind::Icon(IconAction::Power) => quit = true,
                ButtonKind::Icon(IconAction::Lock) => {
                    if let Err(err) =
                        crate::util::spawn_and_reap(&mut std::process::Command::new("xflock4"))
                    {
                        eprintln!("puter: failed to spawn xflock4: {err}");
                    }
                }
                ButtonKind::Setting { action, .. } => self.settings.toggle(action),
            }
        }
        if let Some(term) = self.terminal() {
            match self.press_state {
                PressState::Selection(_) => {
                    term.selection_to_clipboard();
                }
                PressState::ForwardedMouse => {
                    // Only balance the pty's mouse-down with a mouse-up if we
                    // actually forwarded a mouse-down in the first place (e.g.
                    // not when the press landed on a chrome button).
                    term.mouse_release(x, y);
                }
                PressState::None | PressState::Chrome(_) => {}
            }
        }
        self.press_state = PressState::None;
        quit
    }

    pub(crate) fn start_terminal(&mut self, window_id: u64) -> Result<(), Box<dyn Error>> {
        tty::setup_env();
        self.terminal = Some(Terminal::open(window_id)?);
        Ok(())
    }

    pub(crate) fn drain_terminal_events(&self) -> TerminalEvents {
        self.terminal().map_or(
            TerminalEvents {
                running: true,
                dirty: false,
            },
            Terminal::drain_events,
        )
    }

    pub(crate) fn handle_key_press(
        &self,
        input: &KeyInput,
        clipboard_text: Option<&str>,
    ) -> Option<String> {
        self.terminal()?.handle_key_press(input, clipboard_text)
    }

    pub(crate) fn scroll_up(&self) {
        if let Some(term) = self.terminal() {
            term.scroll(Scroll::Delta(SCROLL_LINES));
        }
    }

    pub(crate) fn scroll_down(&self) {
        if let Some(term) = self.terminal() {
            term.scroll(Scroll::Delta(-SCROLL_LINES));
        }
    }

    pub(crate) fn shutdown_terminal(&mut self) {
        if let Some(terminal) = self.terminal.take() {
            terminal.shutdown();
        }
    }

    /// The active terminal, or `None` if it hasn't been started yet. Call
    /// sites degrade gracefully rather than panic.
    const fn terminal(&self) -> Option<&Terminal> {
        self.terminal.as_ref()
    }
}

impl crate::widget::Widget for Puter {
    fn width(&self) -> usize {
        self.mode_images.green_low_contrast.width
    }

    fn height(&self) -> usize {
        self.mode_images.green_low_contrast.height
    }

    fn fill_color(&self, _palette: &Palette) -> Index {
        TRANSPARENT
    }

    fn render(&mut self, fb: &mut Framebuffer, palette: &Palette) {
        Self::render(self, fb, palette);
    }

    fn click(
        &mut self,
        x: isize,
        y: isize,
        shift: bool,
    ) -> Result<crate::widget::ClickOutcome, Box<dyn Error>> {
        self.press_button(x, y, shift);
        Ok(crate::widget::ClickOutcome::default())
    }

    fn motion(&mut self, x: isize, y: isize) -> bool {
        let PressState::Selection(current) = self.press_state else {
            return false;
        };

        // Clamped, not rejected: a selection drag can leave the grid before
        // the button is released, and the selection should keep tracking the
        // nearest cell instead of freezing until the pointer re-enters it.
        let Some(point) = self.terminal().map(|term| term.clamped_screen_point(x, y)) else {
            return false;
        };
        if current == point {
            return false;
        }

        self.press_state = PressState::Selection(point);
        self.terminal().is_some_and(|term| term.mouse_motion(point))
    }

    fn scroll(&mut self, _x: isize, _y: isize, direction: crate::widget::ScrollDirection) -> bool {
        match direction {
            crate::widget::ScrollDirection::Up => self.scroll_up(),
            crate::widget::ScrollDirection::Down => self.scroll_down(),
        }
        true
    }

    /// Hand over the front-panel buttons, text over the terminal screen; the
    /// case around them is inert.
    fn hit_test(&self, x: isize, y: isize) -> Option<CursorKind> {
        if button_at(x, y).is_some() {
            return Some(CursorKind::Hand);
        }
        if x >= 0 && y >= 0 {
            let x = x as usize;
            let y = y as usize;
            let screen_x = art_x(SCREEN_SOURCE_X);
            let screen_y = art_y(SCREEN_SOURCE_Y);
            if x >= screen_x && x < screen_x + SCREEN_W && y >= screen_y && y < screen_y + SCREEN_H
            {
                return Some(CursorKind::Text);
            }
        }
        None
    }

    fn handle_key_press(
        &mut self,
        input: &KeyInput,
        clipboard_text: Option<&str>,
    ) -> Result<Option<String>, Box<dyn Error>> {
        Ok(Self::handle_key_press(self, input, clipboard_text))
    }

    fn wants_clipboard(&self, input: &KeyInput) -> bool {
        input.is_paste_shortcut()
    }
}

impl Drop for Puter {
    fn drop(&mut self) {
        self.shutdown_terminal();
    }
}
