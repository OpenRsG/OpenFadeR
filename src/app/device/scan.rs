//! The live device list: the scan of the mounted devices (when [`device::watch`] reports a change, or every few seconds), the toasts for those that come and go, and the automatic work
//! queued for a device that appears. A stick plugged in that nothing mounted (no desktop automounter) is mounted here first
//! ([`device::plug`]), with the step in the status bar's loading line. Only devices a library plugin handles are listed.

use super::{ActionItem, Auto, Ctx, DeviceItem, toast};
use crate::app::{browser::loading, plugins::suggest};
use openfader::{
    device::{self, Device, plug},
    plugin::{self, device::Feature},
    store::device_settings,
};
use slint::SharedString;
use std::{
    cell::RefCell,
    path::PathBuf,
    rc::Rc,
    sync::{Arc, LazyLock, mpsc},
};

/// Started by the app's device timer, even while its pane stays closed.
static WATCH: LazyLock<device::watch::Watch> = LazyLock::new(device::watch::Watch::start);

#[derive(Default)]
pub(crate) struct Snapshot {
    pub mounted: Vec<Device>,
    pub plugins: Vec<plugin::library::Source>,
    pub explorer: Option<PathBuf>,
}

impl Snapshot {
    /// Filesystem and plugin discovery stay on the device worker.
    fn discover(mounted: Vec<Device>) -> Self {
        let home = openfader::paths::home();
        let music = std::env::var_os("XDG_MUSIC_DIR")
            .map(PathBuf::from)
            .or_else(|| home.as_ref().map(|h| h.join("Music")))
            .filter(|p| p.is_dir());
        Self {
            mounted,
            plugins: plugin::library::sources(),
            explorer: music.or(home).filter(|p| p.is_dir()),
        }
    }
}

thread_local! {
    static MOUNTED: RefCell<Snapshot> = RefCell::new(Snapshot::default());
}

pub(crate) fn mounted() -> Vec<Device> {
    with_snapshot(|s| s.mounted.clone())
}

pub(crate) fn with_snapshot<R>(f: impl FnOnce(&Snapshot) -> R) -> R {
    MOUNTED.with_borrow(f)
}

fn publish(snapshot: Snapshot) -> bool {
    MOUNTED.with_borrow_mut(|held| {
        let changed = held.mounted != snapshot.mounted;
        *held = snapshot;
        changed
    })
}

pub(super) fn reset() {
    MOUNTED.with_borrow_mut(|s| *s = Snapshot::default());
}

#[derive(Default)]
pub(super) struct State {
    rx: Option<mpsc::Receiver<Event>>,
    again: bool,
    select_source: Option<String>,
    plugins: Option<Arc<plugin::Registry>>,
    config: u64,
}

impl State {
    fn event(&self) -> Option<Result<Event, mpsc::TryRecvError>> {
        self.rx.as_ref().map(|rx| rx.try_recv())
    }

    fn configuration_changed(&mut self, plugins: Arc<plugin::Registry>, config: u64) -> bool {
        let changed = self.config != config
            || !self
                .plugins
                .as_ref()
                .is_some_and(|old| Arc::ptr_eq(old, &plugins));
        self.plugins = Some(plugins);
        self.config = config;
        changed
    }
}

enum Event {
    Loading(String),
    Notice(String),
    Done(Snapshot, Vec<String>, Vec<suggest::Detected>),
}

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
    /// Drain scan results without waiting for filesystem, mount or plugin calls.
    pub(super) fn poll_devices(self: &Rc<Self>) {
        loop {
            let event = self.scan.borrow().event();
            match event {
                Some(Ok(Event::Loading(text))) => {
                    if let Some(app) = self.app() {
                        loading(&app, &text, (!text.is_empty()).then_some(0.02));
                    }
                }
                Some(Ok(Event::Notice(text))) => {
                    eprintln!("{text}");
                    if let Some(app) = self.app() {
                        toast::show(&app, &text);
                    }
                }
                Some(Ok(Event::Done(all, tried, suggestions))) => {
                    let again = {
                        let mut scan = self.scan.borrow_mut();
                        scan.rx = None;
                        std::mem::take(&mut scan.again)
                    };
                    *self.tried.borrow_mut() = tried;
                    self.scanned(all, suggestions);
                    let source = self.scan.borrow_mut().select_source.take();
                    if let Some(source) = source {
                        if let Some(d) = self.devices.borrow().iter().find(|d| d.source == source)
                            && let Some(app) = self.app()
                        {
                            super::show_panel(&app, &d.id);
                        }
                    }
                    if again {
                        self.refresh_devices();
                    }
                    break;
                }
                Some(Err(mpsc::TryRecvError::Disconnected)) => {
                    self.scan.borrow_mut().rx = None;
                    if let Some(app) = self.app() {
                        loading(&app, "", None);
                    }
                    self.say(tr!("the worker stopped"), true);
                    break;
                }
                _ => break,
            }
        }
        let changed = self
            .scan
            .borrow_mut()
            .configuration_changed(plugin::current(), plugin::config::revision());
        if WATCH.take() || changed {
            self.refresh_devices();
        }
    }

    pub(super) fn select_after_scan(self: &Rc<Self>, source: String) {
        self.scan.borrow_mut().select_source = Some(source);
        self.refresh_devices();
    }

    /// Only one scan runs at once; repeated requests coalesce into one follow-up.
    pub(super) fn refresh_devices(self: &Rc<Self>) {
        let mut scan = self.scan.borrow_mut();
        if scan.rx.is_some() {
            scan.again = true;
            return;
        }
        let (tx, rx) = mpsc::sync_channel(1);
        scan.rx = Some(rx);
        let mut tried = self.tried.borrow().clone();
        let known: Vec<String> = self.devices.borrow().iter().map(|d| d.id.clone()).collect();
        let store = self.store.clone();
        let looked = suggest::looked();
        std::thread::spawn(move || {
            tried.retain(|p| plug::plugged(p));
            for u in plug::unmounted() {
                if tried.contains(&u.part) {
                    continue;
                }
                tried.push(u.part.clone());
                if tx
                    .send(Event::Loading(tr!(
                        "{}: plugged in, mounting /dev/{}…",
                        u.label,
                        u.part
                    )))
                    .is_err()
                {
                    return;
                }
                let text = match plug::mount(&u.part) {
                    Ok(at) => format!("{}: mounted at {}", u.label, at.display()),
                    Err(e) => tr!(
                        "{}: cannot mount /dev/{}: {}. Mount it from your file manager, then it shows up here.",
                        u.label,
                        u.part,
                        e
                    ),
                };
                if tx.send(Event::Loading(String::new())).is_err() {
                    return;
                }
                if tx.send(Event::Notice(text)).is_err() {
                    return;
                }
            }
            let all = device::scan();
            for d in all
                .iter()
                .filter(|d| d.provider.is_some() && !known.contains(&d.id))
            {
                if let Some(text) = super::check::recover_mounted(d)
                    && tx.send(Event::Notice(text)).is_err()
                {
                    return;
                }
                let _ = store.device_seen(&d.id, &d.label, chrono::Local::now().timestamp());
                if d.can(Feature::Settings) {
                    match crate::app::prefs::dj_system::sync_from_device(&store, d) {
                        Ok(n @ 1..) => {
                            if tx
                                .send(Event::Notice(tr!(
                                    "{}: {} setting(s) changed on the player were taken over",
                                    d.label,
                                    n
                                )))
                                .is_err()
                            {
                                return;
                            }
                        }
                        Err(e) => {
                            if tx.send(Event::Notice(format!("{}: {e}", d.label))).is_err() {
                                return;
                            }
                        }
                        _ => {}
                    }
                }
            }
            let snapshot = Snapshot::discover(all);
            let suggestions = suggest::discover(&snapshot.mounted, &looked);
            let _ = tx.send(Event::Done(snapshot, tried, suggestions));
        });
    }

    fn scanned(self: &Rc<Self>, snapshot: Snapshot, suggestions: Vec<suggest::Detected>) {
        let all = snapshot.mounted.clone();
        let changed = publish(snapshot);
        if let Some(app) = self.app() {
            suggest::devices(&app, &suggestions);
        }
        let now: Vec<Device> = all.into_iter().filter(|d| d.provider.is_some()).collect();
        if *self.devices.borrow() == now {
            if changed {
                crate::app::prefs::refresh();
            }
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
            self.recheck(&d.id);
            say(&tr!("{} connected ({})", d.label, d.summary()));
            if let Some(w) = d.support.message() {
                say(w);
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
        // Something wrote to a stick (its free space or what its plugin finds changed): the verdict is looked at again.
        for d in now
            .iter()
            .filter(|d| before.iter().any(|b| b.id == d.id && b != *d))
        {
            self.recheck(&d.id);
        }
        for d in before.iter().filter(|d| !now.iter().any(|n| n.id == d.id)) {
            say(&tr!("{} removed", d.label));
            self.links.borrow_mut().remove(&d.id);
            self.pushes.borrow_mut().remove(&d.id);
            self.forget_check(&d.id);
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
        self.show_check();
        crate::app::prefs::refresh();
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn snapshot(path: &str) -> Snapshot {
        Snapshot {
            plugins: vec![plugin::library::Source {
                plugin: "test".into(),
                label: path.into(),
                path: path.into(),
            }],
            explorer: Some(path.into()),
            ..Snapshot::default()
        }
    }

    #[test]
    fn startup_and_configuration_changes_request_discovery_once() {
        let mut state = State::default();
        let plugins = Arc::new(plugin::Registry::default());
        assert!(state.configuration_changed(plugins.clone(), 0), "startup");
        assert!(!state.configuration_changed(plugins.clone(), 0));
        assert!(
            state.configuration_changed(plugins.clone(), 1),
            "settings command"
        );
        assert!(!state.configuration_changed(plugins, 1));
        assert!(
            state.configuration_changed(Arc::new(plugin::Registry::default()), 1),
            "reload"
        );
    }

    #[test]
    fn pending_worker_leaves_snapshot_available_until_publication() {
        reset();
        assert!(!publish(snapshot("/old")));
        let (release, gate) = mpsc::sync_channel::<()>(0);
        let (tx, rx) = mpsc::sync_channel(1);
        let worker = std::thread::spawn(move || {
            gate.recv_timeout(Duration::from_secs(5))
                .expect("poll must not wait for discovery");
            assert!(
                tx.send(Event::Done(snapshot("/new"), Vec::new(), Vec::new()))
                    .is_ok()
            );
        });
        let state = State {
            rx: Some(rx),
            ..State::default()
        };
        assert!(matches!(
            state.event(),
            Some(Err(mpsc::TryRecvError::Empty))
        ));
        with_snapshot(|s| {
            assert_eq!(s.plugins[0].path, PathBuf::from("/old"));
            assert_eq!(s.explorer.as_deref(), Some(std::path::Path::new("/old")));
        });
        release.send(()).unwrap();
        worker.join().unwrap();
        let Some(Ok(Event::Done(snapshot, _, _))) = state.event() else {
            panic!("completed discovery must be ready");
        };
        assert!(
            !publish(snapshot),
            "source changes publish even when devices are unchanged"
        );
        with_snapshot(|s| assert_eq!(s.plugins[0].path, PathBuf::from("/new")));
        assert!(mounted().is_empty());
        reset();
        with_snapshot(|s| assert!(s.plugins.is_empty() && s.explorer.is_none()));
    }
}
