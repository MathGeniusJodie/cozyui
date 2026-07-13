//! Seat input: keyboard (with client-side key repeat, which Wayland leaves
//! to clients), pointer, and the sprite-based hardware cursors.

use std::error::Error;
use std::time::{Duration, Instant};

use smithay_client_toolkit::seat::keyboard::{
    KeyEvent, KeyboardHandler, Keysym, Modifiers, RawModifiers, RepeatInfo,
};
use smithay_client_toolkit::seat::pointer::{PointerEvent, PointerEventKind, PointerHandler};
use smithay_client_toolkit::seat::{Capability, SeatHandler, SeatState};
use smithay_client_toolkit::shell::WaylandSurface as _;
use smithay_client_toolkit::shm::slot::Buffer;
use wayland_client::protocol::{wl_keyboard, wl_pointer, wl_seat, wl_shm, wl_surface};
use wayland_client::{Connection, QueueHandle};
use xkbcommon::xkb::keysyms;

use super::{State, WaylandWindow};
use crate::text::KeyInput;
use crate::window::UiEvent;
use crate::{CURSOR_KIND_COUNT, CursorKind, Palette, Sprite, TRANSPARENT, assets};

/// Left mouse button, from the kernel's input-event-codes.
const BTN_LEFT: u32 = 0x110;

/// One wheel detent expressed in continuous-scroll units, per the Wayland
/// convention used by libinput.
const SCROLL_UNITS_PER_STEP: f64 = 15.0;

/// Client-side key repeat: Wayland compositors send press/release plus a
/// rate/delay policy and expect the client to generate the repeats. The main
/// loop's `wait_for_event` clamps its sleep to `deadline` so repeats fire on
/// time, and `poll_event` drains the due ones via `emit_due_repeats`.
#[derive(Default)]
pub(super) struct KeyRepeat {
    info: Option<RepeatInfo>,
    active: Option<ActiveRepeat>,
}

struct ActiveRepeat {
    raw_code: u32,
    input: KeyInput,
    next: Instant,
    interval: Duration,
}

impl KeyRepeat {
    fn arm(&mut self, raw_code: u32, input: KeyInput) {
        let Some(RepeatInfo::Repeat { rate, delay }) = self.info else {
            return;
        };
        self.active = Some(ActiveRepeat {
            raw_code,
            input,
            next: Instant::now() + Duration::from_millis(u64::from(delay)),
            interval: Duration::from_secs(1) / rate.get(),
        });
    }

    fn disarm(&mut self, raw_code: u32) {
        if self
            .active
            .as_ref()
            .is_some_and(|active| active.raw_code == raw_code)
        {
            self.active = None;
        }
    }
}

/// Keys the X server marks non-repeating in every stock keymap: the
/// Shift_L..Hyper_R modifier block, Num_Lock, and ISO_Level3_Shift (AltGr).
/// Everything else repeats, matching what X delivered to the app.
fn is_modifier_sym(sym: Keysym) -> bool {
    (keysyms::KEY_Shift_L..=keysyms::KEY_Hyper_R).contains(&sym.raw())
        || sym.raw() == keysyms::KEY_Num_Lock
        || sym.raw() == keysyms::KEY_ISO_Level3_Shift
}

impl State {
    /// Push a `Key` event for every repeat whose deadline has passed.
    pub(super) fn emit_due_repeats(&mut self) {
        let now = Instant::now();
        while let Some(active) = &mut self.repeat.active {
            if now < active.next {
                break;
            }
            self.events.push_back(UiEvent::Key(active.input.clone()));
            active.next += active.interval;
            // After a stall (e.g. a long widget update), resume at the normal
            // cadence instead of burst-emitting the missed repeats.
            if active.next < now {
                active.next = now + active.interval;
            }
        }
    }

    /// The longest the main loop may sleep without missing a repeat.
    pub(super) fn clamp_to_repeat_deadline(&self, timeout: Duration) -> Duration {
        match &self.repeat.active {
            Some(active) => timeout.min(active.next.saturating_duration_since(Instant::now())),
            None => timeout,
        }
    }

    /// Re-issue the current cursor; Wayland resets it to the default on
    /// every pointer enter, and `set_cursor` is only valid with that enter's
    /// serial.
    fn apply_cursor(&self) {
        let (Some(cursors), Some(pointer), Some(serial), Some(kind)) = (
            &self.cursors,
            &self.pointer,
            self.pointer_enter_serial,
            self.current_cursor,
        ) else {
            return;
        };
        let sprite = &cursors.sprites[kind as usize];
        pointer.set_cursor(serial, Some(&sprite.surface), sprite.hot.0, sprite.hot.1);
    }

    fn build_cursor(
        &mut self,
        sprite: &Sprite,
        palette: &Palette,
        hot: (i32, i32),
    ) -> Result<CursorSprite, Box<dyn Error>> {
        let (w, h) = (sprite.width as i32, sprite.height as i32);
        let (buffer, canvas) = self
            .pool
            .create_buffer(w, h, w * 4, wl_shm::Format::Argb8888)?;
        for y in 0..sprite.height {
            for x in 0..sprite.width {
                let index = sprite.at(x, y);
                // Premultiplied alpha; with only fully opaque or fully
                // transparent pixels the colors pass through unchanged.
                let pixel = if index == TRANSPARENT {
                    [0, 0, 0, 0]
                } else {
                    let c = palette.color(index);
                    [c.b, c.g, c.r, 0xFF]
                };
                canvas[(y * sprite.width + x) * 4..][..4].copy_from_slice(&pixel);
            }
        }
        let surface = self.compositor.create_surface(&self.qh);
        buffer.attach_to(&surface)?;
        surface.damage_buffer(0, 0, w, h);
        surface.commit();
        Ok(CursorSprite {
            surface,
            _buffer: buffer,
            hot,
        })
    }
}

pub(super) struct CursorSprites {
    sprites: [CursorSprite; CURSOR_KIND_COUNT],
}

struct CursorSprite {
    surface: wl_surface::WlSurface,
    /// Keeps the shm slot alive for as long as the cursor exists.
    _buffer: Buffer,
    hot: (i32, i32),
}

impl WaylandWindow {
    /// Build the four cursors from the baked `cursor_*` sprites, each as its
    /// own tiny surface handed to `wl_pointer::set_cursor`.
    pub(crate) fn load_cursors(&mut self, palette: &Palette) -> Result<(), Box<dyn Error>> {
        // Hotspots: arrow tip, I-beam center, fingertip, circle center (keep
        // in sync with the X backend's table in x_window/cursor.rs).
        let sprites: [(Sprite, i32, i32); CURSOR_KIND_COUNT] = [
            (assets::cursor_pointer(), 4, 0),
            (assets::cursor_text(), 12, 12),
            (assets::cursor_hand(), 11, 0),
            (assets::cursor_disabled(), 12, 12),
        ];
        let mut built = Vec::with_capacity(CURSOR_KIND_COUNT);
        for (sprite, hot_x, hot_y) in &sprites {
            built.push(self.state.build_cursor(sprite, palette, (*hot_x, *hot_y))?);
        }
        let sprites = built
            .try_into()
            .map_err(|_| "cursor sprite count mismatch")?;
        self.state.cursors = Some(CursorSprites { sprites });
        self.set_cursor(CursorKind::Pointer)
    }

    /// Switch the pointer's cursor; no-ops when unchanged or unavailable.
    pub(crate) fn set_cursor(&mut self, kind: CursorKind) -> Result<(), Box<dyn Error>> {
        if self.state.current_cursor == Some(kind) {
            return Ok(());
        }
        self.state.current_cursor = Some(kind);
        self.state.apply_cursor();
        self.state.conn.flush()?;
        Ok(())
    }
}

impl SeatHandler for State {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, _conn: &Connection, qh: &QueueHandle<Self>, seat: wl_seat::WlSeat) {
        if self.data_device.is_none()
            && let Some(manager) = &self.data_device_manager
        {
            self.data_device = Some(manager.get_data_device(qh, &seat));
        }
    }

    fn new_capability(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard && self.keyboard.is_none() {
            match self.seat_state.get_keyboard(qh, &seat, None) {
                Ok(keyboard) => self.keyboard = Some(keyboard),
                Err(err) => eprintln!("wayland keyboard unavailable: {err}"),
            }
        }
        if capability == Capability::Pointer && self.pointer.is_none() {
            match self.seat_state.get_pointer(qh, &seat) {
                Ok(pointer) => self.pointer = Some(pointer),
                Err(err) => eprintln!("wayland pointer unavailable: {err}"),
            }
        }
    }

    fn remove_capability(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard
            && let Some(keyboard) = self.keyboard.take()
        {
            keyboard.release();
            self.repeat.active = None;
        }
        if capability == Capability::Pointer
            && let Some(pointer) = self.pointer.take()
        {
            pointer.release();
        }
    }

    fn remove_seat(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _seat: wl_seat::WlSeat) {
    }
}

impl KeyboardHandler for State {
    fn enter(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        _surface: &wl_surface::WlSurface,
        _serial: u32,
        _raw: &[u32],
        _keysyms: &[Keysym],
    ) {
    }

    fn leave(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        _surface: &wl_surface::WlSurface,
        _serial: u32,
    ) {
        self.repeat.active = None;
    }

    fn press_key(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        serial: u32,
        event: KeyEvent,
    ) {
        self.last_input_serial = Some(serial);
        let input = KeyInput::new(
            event.keysym,
            event.utf8.unwrap_or_default(),
            self.modifiers.ctrl,
            self.modifiers.shift,
        );
        if !is_modifier_sym(event.keysym) {
            self.repeat.arm(event.raw_code, input.clone());
        }
        self.events.push_back(UiEvent::Key(input));
    }

    /// Compositor-driven repeats (sent instead of `RepeatInfo` by some
    /// compositors); ours are generated in `emit_due_repeats`.
    fn repeat_key(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        _serial: u32,
        event: KeyEvent,
    ) {
        let input = KeyInput::new(
            event.keysym,
            event.utf8.unwrap_or_default(),
            self.modifiers.ctrl,
            self.modifiers.shift,
        );
        self.events.push_back(UiEvent::Key(input));
    }

    fn release_key(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        _serial: u32,
        event: KeyEvent,
    ) {
        self.repeat.disarm(event.raw_code);
    }

    fn update_modifiers(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        _serial: u32,
        modifiers: Modifiers,
        _raw_modifiers: RawModifiers,
        _layout: u32,
    ) {
        self.modifiers = modifiers;
    }

    fn update_repeat_info(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        info: RepeatInfo,
    ) {
        if matches!(info, RepeatInfo::Disable) {
            self.repeat.active = None;
        }
        self.repeat.info = Some(info);
    }
}

impl PointerHandler for State {
    fn pointer_frame(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _pointer: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        for event in events {
            // Cursor sprite surfaces never receive events, but be explicit.
            if event.surface != *self.layer.wl_surface() {
                continue;
            }
            let (x, y) = (event.position.0 as isize, event.position.1 as isize);
            match event.kind {
                PointerEventKind::Enter { serial } => {
                    self.pointer_enter_serial = Some(serial);
                    self.apply_cursor();
                    self.pointer_pos = (x, y);
                    // Surface the entry position so hover state updates.
                    self.events.push_back(UiEvent::Motion { x, y });
                }
                PointerEventKind::Leave { .. } => self.pointer_enter_serial = None,
                PointerEventKind::Motion { .. } => {
                    self.pointer_pos = (x, y);
                    self.events.push_back(UiEvent::Motion { x, y });
                }
                PointerEventKind::Press { button, serial, .. } => {
                    self.last_input_serial = Some(serial);
                    if button == BTN_LEFT {
                        self.events.push_back(UiEvent::Press {
                            x,
                            y,
                            shift: self.modifiers.shift,
                        });
                    }
                }
                PointerEventKind::Release { button, .. } => {
                    if button == BTN_LEFT {
                        self.events.push_back(UiEvent::Release { x, y });
                    }
                }
                PointerEventKind::Axis { vertical, .. } => {
                    // Prefer the modern high-resolution field, then legacy
                    // discrete steps, then continuous pixels; each sub-step
                    // remainder accumulates until it forms a whole detent.
                    let steps = if vertical.value120 != 0 {
                        self.scroll_v120 += vertical.value120;
                        let steps = self.scroll_v120 / 120;
                        self.scroll_v120 %= 120;
                        steps
                    } else if vertical.discrete != 0 {
                        vertical.discrete
                    } else {
                        self.scroll_px += vertical.absolute;
                        let steps = (self.scroll_px / SCROLL_UNITS_PER_STEP).trunc() as i32;
                        self.scroll_px -= f64::from(steps) * SCROLL_UNITS_PER_STEP;
                        steps
                    };
                    for _ in 0..steps.unsigned_abs() {
                        // Positive axis values scroll toward the bottom.
                        self.events.push_back(if steps > 0 {
                            UiEvent::ScrollDown { x, y }
                        } else {
                            UiEvent::ScrollUp { x, y }
                        });
                    }
                }
            }
        }
    }
}
