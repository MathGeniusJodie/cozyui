//! ICCCM clipboard/selection handling: owning the `CLIPBOARD` selection,
//! answering other clients' `SelectionRequest`s (including `MULTIPLE` and
//! `INCR` for large transfers), and requesting/reading a paste from whoever
//! else owns it.

use std::error::Error;
use std::time::{Duration, Instant};

use x11rb::connection::Connection;
use x11rb::protocol::Event as XEvent;
use x11rb::protocol::xproto::ConnectionExt as XprotoConnectionExt;
use x11rb::protocol::xproto::{
    Atom, AtomEnum, EventMask, PropMode, Property, SELECTION_NOTIFY_EVENT, SelectionClearEvent,
    SelectionNotifyEvent, SelectionRequestEvent, Window,
};
use x11rb::wrapper::ConnectionExt as _;
use x11rb::xcb_ffi::XCBConnection;

use super::XWindow;

/// Cap on total paste size (both the single-property and INCR paths) so a
/// misbehaving (or malicious) selection owner can't force an unbounded
/// allocation or starve the INCR deadline check.
const MAX_PASTE_BYTES: usize = 16 * 1024 * 1024;

pub(super) struct ClipboardAtoms {
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

/// Result of one callback invocation inside `XWindow::wait_for_selection_event`.
enum EventOutcome<T> {
    /// The event was consumed but the wait isn't finished; keep polling.
    Consumed,
    /// The event finished the wait with this value.
    Done(T),
    /// Not the callback's concern; requeue it in `pending_events`.
    NotMine(XEvent),
}

impl ClipboardAtoms {
    pub(super) fn load(conn: &XCBConnection) -> Result<Self, Box<dyn Error>> {
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

fn selection_property(event: SelectionRequestEvent) -> Atom {
    if event.property == u32::from(AtomEnum::NONE) {
        event.target
    } else {
        event.property
    }
}

impl XWindow {
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
        self.wait_for_selection_event(
            deadline,
            "clipboard paste timed out waiting for the selection owner",
            |xwin, event| match event {
                XEvent::SelectionNotify(event) => {
                    Ok(EventOutcome::Done(xwin.read_selection_notify(event)?))
                }
                other => Ok(EventOutcome::NotMine(other)),
            },
        )
    }

    /// Shared skeleton behind `clipboard_text` and `read_incr_chunks`: polls
    /// for X events until `on_event` reports it's done, requeuing (in
    /// `pending_events`) anything it doesn't recognize as its own and always
    /// handling `SelectionRequest`/`SelectionClear` inline first, so a
    /// concurrent clipboard request from another client is never starved by
    /// our own wait. Gives up at `deadline`, logging `timeout_msg` and
    /// returning `T::default()`.
    fn wait_for_selection_event<T: Default>(
        &mut self,
        deadline: Instant,
        timeout_msg: &str,
        mut on_event: impl FnMut(&mut Self, XEvent) -> Result<EventOutcome<T>, Box<dyn Error>>,
    ) -> Result<T, Box<dyn Error>> {
        loop {
            while let Some(event) = self.conn.poll_for_event()? {
                match event {
                    XEvent::SelectionRequest(event) => self.handle_selection_request(event)?,
                    XEvent::SelectionClear(event) => self.handle_selection_clear(event),
                    other => match on_event(self, other)? {
                        EventOutcome::Done(value) => return Ok(value),
                        EventOutcome::Consumed => {}
                        EventOutcome::NotMine(event) => self.pending_events.push_back(event),
                    },
                }
            }
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                eprintln!("{timeout_msg}");
                return Ok(T::default());
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
        let timeout_msg = "clipboard paste timed out mid-INCR transfer";
        self.wait_for_selection_event(deadline, timeout_msg, |xwin, event| {
            let XEvent::PropertyNotify(notify) = event else {
                return Ok(EventOutcome::NotMine(event));
            };
            if notify.window != xwin.window
                || notify.atom != property
                || notify.state != Property::NEW_VALUE
            {
                return Ok(EventOutcome::NotMine(XEvent::PropertyNotify(notify)));
            }

            let reply = xwin
                .conn
                .get_property(
                    true,
                    xwin.window,
                    property,
                    AtomEnum::ANY,
                    0,
                    // Same cap as the non-INCR path (see its comment):
                    // without it, a single misbehaving chunk could force an
                    // arbitrarily large allocation here before the
                    // `data.len() > MAX_PASTE_BYTES` check below ever runs.
                    (MAX_PASTE_BYTES / 4) as u32,
                )?
                .reply()?;
            xwin.conn.flush()?;
            let Some(bytes) = reply.value8() else {
                return Ok(EventOutcome::Done(None));
            };
            let before = data.len();
            data.extend(bytes);
            if data.len() == before {
                return Ok(EventOutcome::Done(
                    String::from_utf8(std::mem::take(&mut data)).ok(),
                ));
            }
            if data.len() > MAX_PASTE_BYTES {
                eprintln!("clipboard paste exceeded {MAX_PASTE_BYTES} bytes, aborting");
                return Ok(EventOutcome::Done(None));
            }
            // A continuous flood of chunks could otherwise keep this inner
            // callback busy indefinitely without ever hitting the outer
            // wait's own deadline check (which only runs once the event
            // queue drains empty).
            if Instant::now() >= deadline {
                eprintln!("{timeout_msg}");
                return Ok(EventOutcome::Done(None));
            }
            Ok(EventOutcome::Consumed)
        })
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
                // Same cap as the paste paths, in 32-bit units: without it a
                // misbehaving (or malicious) requestor could force an
                // arbitrarily large allocation via its own MULTIPLE property.
                (MAX_PASTE_BYTES / 4) as u32,
            )?
            .reply()?;
        if reply.bytes_after > 0 {
            eprintln!("clipboard MULTIPLE request exceeded {MAX_PASTE_BYTES} bytes, ignoring");
            return Ok(false);
        }
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
