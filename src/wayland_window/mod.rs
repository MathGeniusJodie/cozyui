//! Native Wayland backend: a wlr-layer-shell surface pinned full-height to
//! the screen's right edge on the bottom layer (above the wallpaper, below
//! normal windows). The exclusive zone is `EXCLUSIVE_INSET` narrower than the
//! surface, so tiled windows overlap its left strip while the rest stays
//! clear. Frames are software-rendered into `wl_shm` buffers through the
//! same palette LUT as the X backend, with real per-pixel alpha standing in
//! for X's SHAPE holes and the surface input region providing the matching
//! click-through.

mod clipboard;
mod input;
mod present;

use std::collections::VecDeque;
use std::error::Error;
use std::os::fd::AsRawFd as _;
use std::time::Duration;

use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState};
use smithay_client_toolkit::data_device_manager::DataDeviceManagerState;
use smithay_client_toolkit::data_device_manager::data_device::DataDevice;
use smithay_client_toolkit::data_device_manager::data_source::CopyPasteSource;
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::registry_handlers;
use smithay_client_toolkit::seat::SeatState;
use smithay_client_toolkit::seat::keyboard::Modifiers;
use smithay_client_toolkit::shell::WaylandSurface as _;
use smithay_client_toolkit::shell::wlr_layer::{
    Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
    LayerSurfaceConfigure,
};
use smithay_client_toolkit::shm::slot::SlotPool;
use smithay_client_toolkit::shm::{Shm, ShmHandler};
use smithay_client_toolkit::{
    delegate_compositor, delegate_data_device, delegate_keyboard, delegate_layer, delegate_output,
    delegate_pointer, delegate_registry, delegate_seat, delegate_shm,
};
use wayland_client::globals::registry_queue_init;
use wayland_client::protocol::{wl_keyboard, wl_output, wl_pointer, wl_region, wl_surface};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle};

use crate::window::UiEvent;
use crate::CursorKind;
use input::{CursorSprites, KeyRepeat};
use pixel_graphics::PresentLut;

/// How much narrower the reserved (exclusive) strip is than the surface:
/// tiled and maximized windows stop this many pixels into the window, so its
/// left edge slides underneath them while the rest stays uncovered. This is
/// the single knob for the dock overlap — splitwm derives it as the panel's
/// width minus this exclusive zone and adds nothing of its own.
const EXCLUSIVE_INSET: i32 = 310;

fn exclusive_zone(width: usize) -> i32 {
    (width as i32 - EXCLUSIVE_INSET).max(0)
}

pub(crate) struct WaylandWindow {
    event_queue: EventQueue<State>,
    state: State,
}

pub(super) struct State {
    conn: Connection,
    qh: QueueHandle<State>,
    registry_state: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,
    compositor: CompositorState,
    shm: Shm,
    layer: LayerSurface,
    pool: SlotPool,

    transparent: bool,
    /// Compositor-configured surface size; buffers always match it exactly,
    /// with the framebuffer blitted into the top-left and anything beyond it
    /// left fully transparent.
    width: usize,
    height: usize,
    /// The width last requested via `set_size`; configure events answer 0
    /// for dimensions the client chose, so this fills that gap.
    requested_width: usize,
    configured: bool,

    /// Full-surface BGRA copy of the last presented content. Pool buffers
    /// rotate through slots with stale content, so every present copies this
    /// canvas in whole; partial draws only re-translate their dirty rect.
    staging: Vec<u8>,
    /// Index -> BGRA table applied when presenting; refreshed via
    /// `set_palette`, with the `TRANSPARENT` entry cleared to alpha 0.
    lut: Box<PresentLut>,
    /// Per-row opaque runs backing the surface input region, so partial
    /// redraws only rescan the dirty rows (mirrors the X SHAPE row cache).
    input_rows: Vec<Vec<(usize, usize)>>,
    row_scratch: Vec<(usize, usize)>,
    input_region_stale: bool,

    events: VecDeque<UiEvent>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    pointer: Option<wl_pointer::WlPointer>,
    modifiers: Modifiers,
    pointer_pos: (isize, isize),
    /// Serial of the last pointer enter; `set_cursor` is only valid against
    /// it, and it resets the cursor to default on every enter.
    pointer_enter_serial: Option<u32>,
    /// Most recent key/button serial, needed to take the clipboard selection
    /// (compositors reject `set_selection` without a recent input serial).
    last_input_serial: Option<u32>,
    repeat: KeyRepeat,
    /// Sub-step scroll accumulators: value120 counts (preferred) and raw
    /// pixels (continuous sources), drained into whole wheel steps.
    scroll_v120: i32,
    scroll_px: f64,

    data_device_manager: Option<DataDeviceManagerState>,
    data_device: Option<DataDevice>,
    /// Present while we own the clipboard selection; pastes short-circuit to
    /// `clipboard_text` instead of round-tripping through our own pipe.
    copy_source: Option<CopyPasteSource>,
    clipboard_text: Option<String>,

    cursors: Option<CursorSprites>,
    current_cursor: Option<CursorKind>,
    closed: bool,
}

impl WaylandWindow {
    pub(crate) fn open(
        width: usize,
        height: usize,
        transparent: bool,
    ) -> Result<Self, Box<dyn Error>> {
        let conn = Connection::connect_to_env()?;
        let (globals, mut event_queue) = registry_queue_init(&conn)?;
        let qh = event_queue.handle();
        let compositor = CompositorState::bind(&globals, &qh)
            .map_err(|err| format!("wl_compositor unavailable: {err}"))?;
        let layer_shell = LayerShell::bind(&globals, &qh).map_err(|err| {
            format!("wlr-layer-shell unavailable (needs a wlroots-style compositor): {err}")
        })?;
        let shm = Shm::bind(&globals, &qh).map_err(|err| format!("wl_shm unavailable: {err}"))?;
        // Clipboard is best-effort: without a data device manager copy/paste
        // is inert rather than fatal.
        let data_device_manager = DataDeviceManagerState::bind(&globals, &qh).ok();

        let surface = compositor.create_surface(&qh);
        let layer =
            layer_shell.create_layer_surface(&qh, surface, Layer::Bottom, Some("cozyui"), None);
        // Anchoring both top and bottom stretches the surface to full screen
        // height; the 0 height in set_size defers that dimension to the
        // compositor, which reports the real value in the first configure.
        layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::RIGHT);
        layer.set_keyboard_interactivity(KeyboardInteractivity::OnDemand);
        layer.set_size(width as u32, 0);
        layer.set_exclusive_zone(exclusive_zone(width));
        // The initial buffer-less commit requests the first configure; a
        // buffer may only be attached after it arrives.
        layer.commit();

        let pool = SlotPool::new((width * height * 4).max(4096), &shm)?;
        let mut state = State {
            conn: conn.clone(),
            qh: qh.clone(),
            registry_state: RegistryState::new(&globals),
            seat_state: SeatState::new(&globals, &qh),
            output_state: OutputState::new(&globals, &qh),
            compositor,
            shm,
            layer,
            pool,
            transparent,
            width,
            height,
            requested_width: width,
            configured: false,
            staging: vec![0; width * height * 4],
            lut: Box::new([[0, 0, 0, 0xFF]; 256]),
            input_rows: Vec::new(),
            row_scratch: Vec::new(),
            input_region_stale: transparent,
            events: VecDeque::new(),
            keyboard: None,
            pointer: None,
            modifiers: Modifiers::default(),
            pointer_pos: (0, 0),
            pointer_enter_serial: None,
            last_input_serial: None,
            repeat: KeyRepeat::default(),
            scroll_v120: 0,
            scroll_px: 0.0,
            data_device_manager,
            data_device: None,
            copy_source: None,
            clipboard_text: None,
            cursors: None,
            current_cursor: None,
            closed: false,
        };
        while !state.configured && !state.closed {
            event_queue.blocking_dispatch(&mut state)?;
        }
        if state.closed {
            return Err("layer surface closed before the first configure".into());
        }
        Ok(Self { event_queue, state })
    }

    /// The main loop's event source: dispatches whatever the socket has,
    /// emits due key repeats, and hands out the next translated event.
    pub(crate) fn poll_event(&mut self) -> Result<Option<UiEvent>, Box<dyn Error>> {
        loop {
            self.event_queue.dispatch_pending(&mut self.state)?;
            self.state.emit_due_repeats();
            if let Some(event) = self.state.events.pop_front() {
                return Ok(Some(event));
            }
            if !self.read_socket(Duration::ZERO)? {
                return Ok(None);
            }
        }
    }

    /// Blocks until the Wayland socket becomes readable or `timeout` elapses
    /// (clamped to the next key-repeat deadline so repeats fire on time).
    /// Spurious early wake-ups are fine — the caller re-checks all its event
    /// sources every iteration.
    pub(crate) fn wait_for_event(&mut self, timeout: Duration) -> Result<(), Box<dyn Error>> {
        if !self.state.events.is_empty() {
            return Ok(());
        }
        let timeout = self.state.clamp_to_repeat_deadline(timeout);
        self.read_socket(timeout)?;
        Ok(())
    }

    /// Flush pending requests, then wait up to `timeout` for the socket and
    /// read it. Returns whether new events landed in the queue. EINTR/EAGAIN
    /// wake-ups return `false` instead of erroring, like the X backend.
    fn read_socket(&mut self, timeout: Duration) -> Result<bool, Box<dyn Error>> {
        self.state.conn.flush()?;
        let Some(guard) = self.event_queue.prepare_read() else {
            // Another queue already read the socket; events are pending.
            return Ok(true);
        };
        let mut pfd = libc::pollfd {
            fd: guard.connection_fd().as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let millis = i32::try_from(timeout.as_millis()).unwrap_or(i32::MAX);
        let ret = unsafe { libc::poll(&raw mut pfd, 1, millis) };
        if ret < 0 {
            let err = std::io::Error::last_os_error();
            if matches!(err.raw_os_error(), Some(libc::EINTR) | Some(libc::EAGAIN)) {
                return Ok(false);
            }
            return Err(err.into());
        }
        if ret == 0 || pfd.revents & libc::POLLIN == 0 {
            return Ok(false);
        }
        match guard.read() {
            Ok(count) => Ok(count > 0),
            // The socket was drained by a competing thread between poll and
            // read; nothing was lost.
            Err(wayland_client::backend::WaylandError::Io(err))
                if err.kind() == std::io::ErrorKind::WouldBlock =>
            {
                Ok(false)
            }
            Err(err) => Err(err.into()),
        }
    }
}

impl CompositorHandler for State {
    fn scale_factor_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _new_factor: i32,
    ) {
        // Pixel-art app: the buffer stays 1:1 and the compositor scales it.
    }

    fn transform_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _new_transform: wl_output::Transform,
    ) {
    }

    fn frame(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _time: u32,
    ) {
        // The main loop self-paces (~60fps while animating); frame callbacks
        // are never requested.
    }

    fn surface_enter(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {
    }
}

impl LayerShellHandler for State {
    fn closed(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _layer: &LayerSurface) {
        self.closed = true;
        self.events.push_back(UiEvent::Closed);
    }

    fn configure(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _serial: u32,
    ) {
        let (w, h) = configure.new_size;
        // 0 means "you decide": keep the requested width / current height.
        let width = if w == 0 { self.requested_width } else { w as usize };
        let height = if h == 0 { self.height } else { h as usize };
        self.configured = true;
        if width == self.width && height == self.height {
            return;
        }
        self.width = width;
        self.height = height;
        // All-transparent until the caller redraws in response to `Resized`.
        self.staging = vec![0; width * height * 4];
        self.input_rows.clear();
        self.input_region_stale = self.transparent;
        self.events.push_back(UiEvent::Resized { width, height });
    }
}

impl OutputHandler for State {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
    }

    fn update_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
    }

    fn output_destroyed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
    }
}

impl ShmHandler for State {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for State {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}

/// Input regions are built from `wl_region` objects, which SCTK doesn't
/// wrap; wl_region has no events, so the handler is trivially empty.
impl Dispatch<wl_region::WlRegion, ()> for State {
    fn event(
        _state: &mut Self,
        _proxy: &wl_region::WlRegion,
        event: wl_region::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        let _ = event;
        unreachable!("wl_region has no events");
    }
}

delegate_compositor!(State);
delegate_output!(State);
delegate_shm!(State);
delegate_seat!(State);
delegate_keyboard!(State);
delegate_pointer!(State);
delegate_layer!(State);
delegate_registry!(State);
delegate_data_device!(State);
