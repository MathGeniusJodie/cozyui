//! Clipboard via the core data-device protocol: copies publish a data source
//! carrying the plain-text mime types, pastes receive the current selection
//! offer through a pipe. Both need recent input serials, which the input
//! handlers record. Drag-and-drop is not supported; its handler methods are
//! deliberate no-ops.

use std::error::Error;
use std::io::{Read as _, Write as _};
use std::os::fd::AsRawFd as _;
use std::time::{Duration, Instant};

use smithay_client_toolkit::data_device_manager::WritePipe;
use smithay_client_toolkit::data_device_manager::data_device::DataDeviceHandler;
use smithay_client_toolkit::data_device_manager::data_offer::{DataOfferHandler, DragOffer};
use smithay_client_toolkit::data_device_manager::data_source::DataSourceHandler;
use wayland_client::protocol::wl_data_device::WlDataDevice;
use wayland_client::protocol::wl_data_device_manager::DndAction;
use wayland_client::protocol::wl_data_source::WlDataSource;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, QueueHandle};

use super::WaylandWindow;

/// Offered when copying, and matched in this order when pasting.
const TEXT_MIMES: &[&str] = &[
    "text/plain;charset=utf-8",
    "UTF8_STRING",
    "text/plain",
    "STRING",
    "TEXT",
];

/// Ceiling on how long a paste may block waiting on the source client, so a
/// stalled or malicious clipboard owner can't wedge the UI forever.
const PASTE_TIMEOUT: Duration = Duration::from_secs(2);

impl WaylandWindow {
    /// Take the clipboard selection, keeping the text to serve from
    /// `send_request`. Requires some prior input (the serial); without one
    /// the compositor would reject the claim anyway.
    pub(crate) fn set_clipboard_text(&mut self, text: String) -> Result<(), Box<dyn Error>> {
        let state = &mut self.state;
        state.clipboard_text = Some(text);
        let (Some(manager), Some(device), Some(serial)) = (
            &state.data_device_manager,
            &state.data_device,
            state.last_input_serial,
        ) else {
            return Ok(());
        };
        let source = manager.create_copy_paste_source(&state.qh, TEXT_MIMES.iter().copied());
        source.set_selection(device, serial);
        state.copy_source = Some(source);
        state.conn.flush()?;
        Ok(())
    }

    /// The current selection as text, or `None` when there is none / it has
    /// no text form. When we own the selection this short-circuits to the
    /// stored text — receiving from ourselves would deadlock on our own pipe.
    pub(crate) fn clipboard_text(&mut self) -> Result<Option<String>, Box<dyn Error>> {
        let state = &mut self.state;
        if state.copy_source.is_some() {
            return Ok(state.clipboard_text.clone());
        }
        let Some(device) = &state.data_device else {
            return Ok(None);
        };
        let Some(offer) = device.data().selection_offer() else {
            return Ok(None);
        };
        let Some(mime) = offer.with_mime_types(|mimes| {
            TEXT_MIMES
                .iter()
                .find(|wanted| mimes.iter().any(|offered| offered == *wanted))
                .map(|mime| (*mime).to_string())
        }) else {
            return Ok(None);
        };

        let mut pipe = offer.receive(mime)?;
        // The receive request must reach the compositor before the source
        // client can start writing.
        state.conn.flush()?;

        let deadline = Instant::now() + PASTE_TIMEOUT;
        let mut data = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            match pipe.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => data.extend_from_slice(&buf[..n]),
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    if remaining.is_zero() {
                        return Err("clipboard source timed out".into());
                    }
                    let mut pfd = libc::pollfd {
                        fd: pipe.as_raw_fd(),
                        events: libc::POLLIN,
                        revents: 0,
                    };
                    let millis = i32::try_from(remaining.as_millis()).unwrap_or(i32::MAX);
                    unsafe { libc::poll(&raw mut pfd, 1, millis) };
                }
                Err(err) => return Err(err.into()),
            }
            if Instant::now() >= deadline {
                return Err("clipboard source timed out".into());
            }
        }
        Ok(Some(String::from_utf8_lossy(&data).into_owned()))
    }
}

impl DataDeviceHandler for super::State {
    fn enter(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _data_device: &WlDataDevice,
        _x: f64,
        _y: f64,
        _surface: &WlSurface,
    ) {
    }

    fn leave(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _data_device: &WlDataDevice) {}

    fn motion(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _data_device: &WlDataDevice,
        _x: f64,
        _y: f64,
    ) {
    }

    /// A new selection exists; it stays parked on the device's data until a
    /// paste actually asks for it.
    fn selection(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _device: &WlDataDevice) {}

    fn drop_performed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _data_device: &WlDataDevice,
    ) {
    }
}

impl DataSourceHandler for super::State {
    fn accept_mime(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _source: &WlDataSource,
        _mime: Option<String>,
    ) {
    }

    fn send_request(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _source: &WlDataSource,
        _mime: String,
        fd: WritePipe,
    ) {
        // Every advertised mime type is plain UTF-8 text. The pipe closes on
        // drop, which signals end-of-transfer to the receiver.
        if let Some(text) = &self.clipboard_text {
            let mut pipe = fd;
            let _ = pipe.write_all(text.as_bytes());
        }
    }

    /// Someone else took the selection; the text stays around only as our
    /// (now stale) last copy.
    fn cancelled(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, source: &WlDataSource) {
        if self
            .copy_source
            .as_ref()
            .is_some_and(|ours| ours.inner() == source)
        {
            self.copy_source = None;
        }
    }

    fn dnd_dropped(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _source: &WlDataSource) {
    }

    fn dnd_finished(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _source: &WlDataSource,
    ) {
    }

    fn action(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _source: &WlDataSource,
        _action: DndAction,
    ) {
    }
}

impl DataOfferHandler for super::State {
    fn source_actions(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _offer: &mut DragOffer,
        _actions: DndAction,
    ) {
    }

    fn selected_action(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _offer: &mut DragOffer,
        _actions: DndAction,
    ) {
    }
}
