//! File drops under Wayland, which winit 0.30 does not report: a `wl_data_device` of our own on winit's connection (the
//! foreign `wl_display` winit opened, so the compositor sees one client), on an event queue of our own that a 30 ms
//! timer dispatches (winit's loop reads the socket; libwayland sorts our events into our queue).
//!
//! The protocol, as a drop target: the compositor announces a `wl_data_offer` and its MIME types, then `enter` with it;
//! we accept `text/uri-list` (copy). On `drop` the offer's data is read through a pipe (on a thread: the source writes
//! it at its own pace), the `file://` URIs become paths for [`super::dropped`], and the offer is finished.

use super::{dropped, hovering};
use crate::app::App;
use slint::winit_030::{
    WinitWindowAccessor,
    winit::raw_window_handle::{HasDisplayHandle, RawDisplayHandle},
};
use slint::{ComponentHandle, Timer, TimerMode, Weak};
use std::{cell::RefCell, io::Read, path::PathBuf, time::Duration};
use wayland_client::{
    Connection, Dispatch, EventQueue, Proxy, QueueHandle,
    backend::Backend,
    event_created_child,
    protocol::{
        wl_data_device::{self, WlDataDevice},
        wl_data_device_manager::{DndAction, WlDataDeviceManager},
        wl_data_offer::{self, WlDataOffer},
        wl_registry::{self, WlRegistry},
        wl_seat::WlSeat,
    },
};

const URIS: &str = "text/uri-list";

thread_local! {
    /// The timer dispatching our queue; it lives as long as the app.
    static LISTENER: RefCell<Option<Timer>> = const { RefCell::new(None) };
}

/// What the event handlers keep between events.
struct State {
    app: Weak<App>,
    conn: Connection,
    manager: Option<WlDataDeviceManager>,
    seat: Option<WlSeat>,
    device: Option<WlDataDevice>,
    /// Offers announced and the MIME types each carries.
    offers: Vec<(WlDataOffer, Vec<String>)>,
    /// The offer of the drag over the window, and whether it carries files.
    current: Option<(WlDataOffer, bool)>,
}

/// Starts listening once the window exists (a no-op off Wayland).
pub(super) fn wire(app: &App) {
    let weak = app.as_weak();
    let _ = slint::spawn_local(async move {
        let Some(app) = weak.upgrade() else { return };
        let Ok(window) = app.window().winit_window().await else {
            return;
        };
        let Ok(handle) = window.display_handle() else {
            return;
        };
        let RawDisplayHandle::Wayland(h) = handle.as_raw() else {
            return;
        };
        // SAFETY: the display stays open as long as winit's event loop, which outlives the window and this listener.
        let backend = unsafe { Backend::from_foreign_display(h.display.as_ptr().cast()) };
        let conn = Connection::from_backend(backend);
        let mut queue: EventQueue<State> = conn.new_event_queue();
        let _registry = conn.display().get_registry(&queue.handle(), ());
        let mut state = State {
            app: weak.clone(),
            conn,
            manager: None,
            seat: None,
            device: None,
            offers: vec![],
            current: None,
        };
        let _ = state.conn.flush();
        let timer = Timer::default();
        timer.start(TimerMode::Repeated, Duration::from_millis(30), move || {
            if state.device.is_none()
                && let (Some(m), Some(seat)) = (&state.manager, &state.seat)
            {
                state.device = Some(m.get_data_device(seat, &queue.handle(), ()));
            }
            if let Err(e) = queue.dispatch_pending(&mut state) {
                eprintln!("drag and drop (Wayland): {e}");
                // Stops this timer (it is not running its own callback after this one).
                slint::Timer::single_shot(Duration::ZERO, || {
                    LISTENER.with_borrow_mut(|l| *l = None)
                });
                return;
            }
            let _ = state.conn.flush();
        });
        LISTENER.with_borrow_mut(|l| *l = Some(timer));
    });
}

impl Dispatch<WlRegistry, ()> for State {
    fn event(
        s: &mut Self,
        registry: &WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            match interface.as_str() {
                "wl_data_device_manager" if s.manager.is_none() => {
                    s.manager = Some(registry.bind(name, version.min(3), qh, ()));
                }
                "wl_seat" if s.seat.is_none() => {
                    s.seat = Some(registry.bind(name, version.min(5), qh, ()))
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<WlSeat, ()> for State {
    fn event(
        _: &mut Self,
        _: &WlSeat,
        _: <WlSeat as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlDataDeviceManager, ()> for State {
    fn event(
        _: &mut Self,
        _: &WlDataDeviceManager,
        _: <WlDataDeviceManager as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlDataOffer, ()> for State {
    fn event(
        s: &mut Self,
        offer: &WlDataOffer,
        event: wl_data_offer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_data_offer::Event::Offer { mime_type } = event
            && let Some((_, types)) = s.offers.iter_mut().find(|(o, _)| o == offer)
        {
            types.push(mime_type);
        }
    }
}

impl Dispatch<WlDataDevice, ()> for State {
    fn event(
        s: &mut Self,
        _: &WlDataDevice,
        event: wl_data_device::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_data_device::Event::DataOffer { id } => s.offers.push((id, Vec::new())),
            wl_data_device::Event::Enter {
                serial,
                id: Some(offer),
                ..
            } => {
                let files = s
                    .offers
                    .iter()
                    .any(|(o, t)| *o == offer && t.iter().any(|t| t == URIS));
                if files {
                    offer.accept(serial, Some(URIS.into()));
                    if offer.version() >= 3 {
                        offer.set_actions(DndAction::Copy, DndAction::Copy);
                    }
                } else {
                    offer.accept(serial, None);
                }
                s.current = Some((offer, files));
                if let Some(app) = s.app.upgrade() {
                    hovering(&app, files);
                }
            }
            wl_data_device::Event::Leave => {
                s.forget_current();
                if let Some(app) = s.app.upgrade() {
                    hovering(&app, false);
                }
            }
            wl_data_device::Event::Drop => s.take_drop(),
            wl_data_device::Event::Selection { id: Some(o) } => {
                // Clipboard offers are not ours to keep (the clipboard has its own data device).
                s.offers.retain(|(x, _)| *x != o);
                o.destroy();
            }
            _ => {}
        }
    }

    event_created_child!(State, WlDataDevice, [wl_data_device::EVT_DATA_OFFER_OPCODE => (WlDataOffer, ())]);
}

impl State {
    fn forget_current(&mut self) {
        if let Some((o, _)) = self.current.take() {
            self.offers.retain(|(x, _)| *x != o);
            o.destroy();
        }
    }

    /// Reads the dropped URIs on a thread; the paths go to [`dropped`] and the offer is finished on the UI thread.
    fn take_drop(&mut self) {
        let Some((offer, files)) = self.current.take() else {
            return;
        };
        self.offers.retain(|(x, _)| *x != offer);
        let Some((mut reader, writer)) = files.then(std::io::pipe).and_then(Result::ok) else {
            offer.destroy();
            return;
        };
        offer.receive(URIS.into(), std::os::fd::AsFd::as_fd(&writer));
        let _ = self.conn.flush();
        drop(writer);
        let weak = self.app.clone();
        std::thread::spawn(move || {
            let mut text = String::new();
            let _ = reader.read_to_string(&mut text);
            let _ = weak.upgrade_in_event_loop(move |app| {
                if offer.version() >= 3 {
                    offer.finish();
                }
                offer.destroy();
                dropped(&app, uri_paths(&text));
            });
        });
    }
}

/// The local paths of a `text/uri-list` (`file://` URIs, percent-decoded; comments and other schemes skipped).
fn uri_paths(text: &str) -> Vec<PathBuf> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.strip_prefix("file://"))
        // `file://host/path`: the path starts at the first `/` after the (usually empty) host.
        .filter_map(|l| l.find('/').map(|i| &l[i..]))
        .map(|p| PathBuf::from(percent_decode(p)))
        .collect()
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |c: u8| (c as char).to_digit(16);
        match (
            b[i],
            b.get(i + 1).copied().and_then(hex),
            b.get(i + 2).copied().and_then(hex),
        ) {
            (b'%', Some(h), Some(l)) => {
                out.push((h * 16 + l) as u8);
                i += 3;
            }
            (c, ..) => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_lists_become_local_paths() {
        let list = "# comment\r\nfile:///home/me/My%20Plugin\r\nfile://host/tmp/a%C3%A9.zip\nhttps://example.com/x\n\n";
        assert_eq!(
            uri_paths(list),
            [
                PathBuf::from("/home/me/My Plugin"),
                PathBuf::from("/tmp/aé.zip")
            ]
        );
    }
}
