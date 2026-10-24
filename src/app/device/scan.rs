//! The live device list: the 1 s scan of the mounted devices, the toasts for those that come and go, and the automatic work
//! queued for a device that appears. A stick plugged in that nothing mounted (no desktop automounter) is mounted here first
//! ([`device::plug`]), with the step in the status bar's loading line. Only devices a library plugin handles are listed.

use super::{ActionItem, Auto, Ctx, DeviceItem, toast};
use crate::app::browser::loading;
use openfader::{
    device::{self, Device, plug},
    plugin::device::Feature,
    store::device_settings,
};
use slint::SharedString;
use std::rc::Rc;

fn item(d: &Device) -> DeviceItem {
    let library = match d.held() {
        h if h.is_empty() => tr!("no library yet").to_owned(),
        h => h,
    };
    let warning = d
        .support
        .message()
        .map(str::to_owned)
        .or_else(|| {
            d.read_only
                .then(|| tr!("mounted read-only: nothing can be written").to_owned())
        })
        .unwrap_or_default();
    DeviceItem {
        id: d.id.clone().into(),
        name: d.label.clone().into(),
        summary: d.summary().into(),
        warning: warning.into(),
        library: library.into(),
        used: if d.total == 0 {
            0.0
        } else {
            1.0 - d.free as f32 / d.total as f32
        },
        writable: d.writable(),
        has_library: d.has_library(),
        can_export: d.can(Feature::Export),
        can_update: d.can(Feature::Update),
        can_histories: d.can(Feature::Histories),
        can_backup: d.can(Feature::Backup),
        can_settings: d.can(Feature::Settings),
    }
}

impl Ctx {
    /// Mounts each plugged-in file system nothing mounted, once per plug (one unmounted by hand stays unmounted); the next scan
    /// finds it mounted and announces it.
    fn automount(&self) {
        self.tried.borrow_mut().retain(|p| plug::plugged(p));
        for u in plug::unmounted() {
            if self.tried.borrow().contains(&u.part) {
                continue;
            }
            self.tried.borrow_mut().push(u.part.clone());
            let Some(app) = self.app() else { return };
            loading(
                &app,
                &tr!("{}: plugged in, mounting /dev/{}…", u.label, u.part),
                Some(0.02),
            );
            let weak = self.app.clone();
            std::thread::spawn(move || {
                let r = plug::mount(&u.part);
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(app) = weak.upgrade() else { return };
                    loading(&app, "", None);
                    match r {
                        Ok(at) => eprintln!("{}: mounted at {}", u.label, at.display()),
                        Err(e) => {
                            let text = tr!("{}: cannot mount /dev/{}: {}", u.label, u.part, e);
                            eprintln!("{text}");
                            toast::show(&app, &text);
                        }
                    }
                });
            });
        }
    }

    /// Looks at the mounted devices; updates the list, announces changes, queues the automatic work of new devices.
    pub(super) fn refresh_devices(self: &Rc<Self>) {
        self.automount();
        let now: Vec<Device> = device::scan()
            .into_iter()
            .filter(|d| d.provider.is_some())
            .collect();
        if *self.devices.borrow() == now {
            return;
        }
        let app = self.app();
        let say = |text: &str| {
            if let Some(app) = &app {
                toast::show(app, text);
            }
        };
        let before = self.devices.replace(now.clone());
        for d in now.iter().filter(|d| !before.iter().any(|b| b.id == d.id)) {
            say(&tr!("{} connected ({})", d.label, d.summary()));
            if let Some(w) = d.support.message() {
                say(w);
            }
            let _ = self
                .store
                .device_seen(&d.id, &d.label, chrono::Local::now().timestamp());
            if d.can(Feature::Settings) {
                // What the player changed in its settings since we wrote them becomes the device's settings here.
                if let Ok(n @ 1..) = crate::app::prefs::dj_system::sync_from_device(&self.store, d)
                {
                    say(&tr!(
                        "{}: {} setting(s) changed on the player were taken over",
                        d.label,
                        n
                    ));
                }
            }
            if self.greeted.borrow().contains(&d.id) {
                continue;
            }
            self.greeted.borrow_mut().push(d.id.clone());
            if d.has_library()
                && d.can(Feature::Histories)
                && device_settings::Behaviour::load(&self.store).import_history
            {
                self.autos
                    .borrow_mut()
                    .push_back((d.id.clone(), Auto::History));
            }
            if d.writable()
                && d.can(Feature::Export)
                && self.store.device_sync_on(&d.id).unwrap_or(false)
            {
                self.autos
                    .borrow_mut()
                    .push_back((d.id.clone(), Auto::Sync));
            }
            self.autos
                .borrow_mut()
                .push_back((d.id.clone(), Auto::Link));
        }
        for d in before.iter().filter(|d| !now.iter().any(|n| n.id == d.id)) {
            say(&tr!("{} removed", d.label));
            self.links.borrow_mut().remove(&d.id);
            self.pushes.borrow_mut().remove(&d.id);
        }
        self.models
            .devices
            .set_vec(now.iter().map(item).collect::<Vec<_>>());
        self.models.names.set_vec(
            now.iter()
                .map(|d| SharedString::from(d.label.as_str()))
                .collect::<Vec<_>>(),
        );
        self.show_targets();
        if self.current().is_none() {
            *self.current.borrow_mut() = now.first().map(|d| d.id.clone());
        }
        self.show_current();
    }

    /// Puts the chosen device's index and the buttons its plugin lists into the window.
    pub(super) fn show_current(&self) {
        let devices = self.devices.borrow();
        let at = self
            .current
            .borrow()
            .as_ref()
            .and_then(|id| devices.iter().position(|d| &d.id == id));
        let actions: Vec<ActionItem> = at.map_or_else(Vec::new, |i| {
            devices[i]
                .info
                .actions
                .iter()
                .map(|a| ActionItem {
                    id: a.id.as_str().into(),
                    label: a.label.as_str().into(),
                    help: a.help.as_str().into(),
                    enabled: a.enabled,
                    writes: a.writes,
                })
                .collect()
        });
        drop(devices);
        self.models.actions.set_vec(actions);
        self.with_state(|s| s.set_current(at.map_or(-1, |i| i as i32)));
        self.show_formats();
    }
}
