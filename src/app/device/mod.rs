//! Devices ↔ ui/device*.slint (global `DeviceState`) (base): the window with DEVICES / EXPORT / SYNC MANAGER / BACKUPS, the live
//! device list, and the one background job that works on a device with its progress and cancel.
//!
//! One timer ticks every 250 ms: it shows the running job's progress, and every fourth tick (1 s) looks at the mounted devices
//! ([`openfader::device::scan`]); a device that appears is announced (with the file-system warning), remembered in the store, and
//! gets its automatic work (Preferences > DEVICE: import the play history, SYNC MANAGER auto-sync).
//!
//! What a device can do and how it is called comes from the library plugin that handles it ([`Device::info`]): the window lists
//! the devices that have a plugin, shows the buttons the plugin lists for the chosen one and only what its
//! [`Feature`]s offer. The work itself is the plugin's ([`openfader::plugin::device::Provider`]).
//!
//! Children: [`job`] (the background job), [`scan`] (the live device list), [`pick`] (the tick lists of EXPORT and SYNC MANAGER),
//! then one per tab / family: [`export`] (EXPORT tab, also called from the browser), [`sync`] (SYNC MANAGER), [`backup`]
//! (BACKUPS, and the eject that backs up first), [`actions`] (the plugin's buttons, Update Collection, histories, delete / import
//! from the Devices tree), [`link`] (cue sync between the collection and the device copies of a track), [`formats`] (which
//! library formats a device gets: UNIVERSAL or the ticked ones, and their options).
//!
//! `Rc<dyn Library>` cannot leave the UI thread, so a press snapshots the cheap part there ([`Ctx::snapshot`]: the overlaid
//! tracks, the store's colours / tags / user playlists, the decks' session edits) and the work runs on a worker with a
//! [`Control`] the timer polls and the cancel button sets. Only one job runs at a time.
//!
//! The browser's Devices tree (src/app/browser) shows each device's playlists read-only and calls [`show_panel`] when a device
//! row is chosen; its toolbar buttons call the `DeviceState` callbacks `import-playlist`, `delete-playlist`, `import-history`,
//! `delete-track` and `export-playlists` that [`actions`] / [`export`] answer. [`transfer`] sends tracks and playlists from the
//! browser (a drop on a device row, the track menu's "Transfer to <device>") to a device.

mod actions;
mod backup;
mod export;
mod formats;
mod job;
mod link;
mod pick;
mod scan;
mod sync;
mod transfer;

pub use link::{track_edited, track_loading};
pub use transfer::{drop_playlist, drop_tracks, Dest, List};

use super::{
    deck::Decks, toast, ActionItem, App, BackupItem, DeviceItem, DeviceState, FormatItem,
    FormatOption, PickRow,
};
use openfader::{
    device::{self, export::Written, Device},
    library::{snapshot::Snapshot, Library},
    plugin::device::{Control, ExportOptions, ExportReport, Feature, Provider},
    store::{self, device_settings, Store},
};
use pick::Pick;
use slint::{ComponentHandle, Model, ModelRc, SharedString, Timer, TimerMode, VecModel};
use std::{
    cell::{Cell, RefCell},
    collections::{BTreeSet, HashMap, VecDeque},
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};

const BUSY: &str = n_!("Another job is running: wait for it or cancel it (X)");

/// What runs when a job ends, with the worker's result.
type Then = Box<dyn FnOnce(&Rc<Ctx>, &Result<String, String>)>;

struct Job {
    ctl: Arc<Control>,
    rx: std::sync::mpsc::Receiver<Result<String, String>>,
    /// The device it writes to (eject and delete are refused meanwhile).
    device: Option<String>,
    /// The status line and its error colour when the job started: shown again when the job has nothing to report.
    before: (String, bool),
    then: Then,
}

/// Work queued for a device that just appeared, run when no job is.
enum Auto {
    History,
    Sync,
    /// Match the collection against the device, filling the copies that lack cues ([`link`]).
    Link,
}

pub(super) struct Ctx {
    app: slint::Weak<App>,
    lib: Rc<dyn Library>,
    decks: Decks,
    store: Arc<Store>,
    /// The mounted devices as of the last scan; `DeviceState.devices` shows the same list.
    devices: RefCell<Vec<Device>>,
    current: RefCell<Option<String>>,
    job: RefCell<Option<Job>>,
    autos: RefCell<VecDeque<(String, Auto)>>,
    /// Devices already given their automatic work in this run.
    greeted: RefCell<Vec<String>>,
    /// Partitions (`sda1`) already given their automatic mount while plugged in ([`scan`]).
    tried: RefCell<Vec<String>>,
    /// What the open question does on "yes".
    ask: RefCell<Option<Box<dyn FnOnce(&Rc<Ctx>)>>>,
    export: Pick,
    /// The collection as listed in the EXPORT / SYNC MANAGER tabs (loaded when the window opens or a tab changes).
    snap: RefCell<Option<Snapshot>>,
    /// Playlist ids to tick the next time the EXPORT list loads (the browser's "export to device").
    preselect: RefCell<Vec<String>>,
    /// The backups of the BACKUPS tab, row by row.
    backups: RefCell<Vec<store::DeviceBackup>>,
    sync: Pick,
    ticks: Cell<u32>,
    /// The running job's progress also shows in the browser's status bar (a transfer started from the browser).
    mirror: Cell<bool>,
    models: Models,
    /// Cue sync: the tracks each device shares with the collection, by device id ([`link`]).
    links: RefCell<HashMap<String, Rc<openfader::device::link::Links>>>,
    /// Cue sync: per device id, the keys of the tracks whose device copy is to be written, and the time of the last edit.
    pushes: RefCell<HashMap<String, (Option<Instant>, BTreeSet<String>)>>,
}

struct Models {
    devices: Rc<VecModel<DeviceItem>>,
    names: Rc<VecModel<SharedString>>,
    actions: Rc<VecModel<ActionItem>>,
    backups: Rc<VecModel<BackupItem>>,
    formats: Rc<VecModel<FormatItem>>,
    /// The labels of the devices the track menu's "Transfer to" lists ([`transfer`]).
    targets: Rc<VecModel<SharedString>>,
}

/// Keeps the timer (and with it the whole window logic) alive for as long as the window runs.
pub struct Guard {
    _timer: Timer,
    _ctx: Rc<Ctx>,
}

/// "exported 5 tracks (2 new, 3 updated) and 3 playlists to KEY", with what the user should know: the plugin's notes and the first
/// warning.
fn report_text(r: &ExportReport, name: &str) -> String {
    let notes = if r.notes.is_empty() {
        String::new()
    } else {
        format!(" ({})", r.notes.join("; "))
    };
    let warn = r.warnings.first().map_or_else(String::new, |w| {
        tr!(" — {} warning(s), first: {}", r.warnings.len(), w)
    });
    tr!(
        "exported {} tracks ({} new, {} updated) and {} playlists to {}{}{}",
        r.tracks,
        r.added,
        r.updated,
        r.playlists,
        name,
        notes,
        warn
    )
}

/// After an export or sync wrote to device `id`: keeps the baselines of the tracks written (what Update Collection compares
/// against) and the fingerprint of the settings files. A baseline that fails is appended to `text`.
fn record_export(store: &Store, id: &str, r: &ExportReport, text: &mut String) {
    if let Some(fp) = &r.fingerprint {
        let _ = device_settings::record_fingerprint(store, id, fp);
    }
    if let Err(e) = store.set_device_sigs(id, &r.baselines) {
        text.push_str(&tr!(" — Update Collection baseline not recorded: {}", e));
    }
}

/// The name of format `plugin` in reports: its title on device `dev`, else the plugin name in capitals.
fn format_label(dev: Option<&Device>, plugin: &str) -> String {
    dev.map_or_else(
        || plugin.replace(['-', '_'], " ").to_uppercase(),
        |d| d.format_title(plugin),
    )
}

/// What a multi-format export did, one part per format (labelled when there are several), keeping each written format's
/// baselines and settings fingerprint for device `dev`; `Err` when every format failed.
fn record_all(
    store: &Store,
    dev: Option<&Device>,
    done: &[Written],
    name: &str,
) -> Result<String, String> {
    let parts: Vec<String> = done
        .iter()
        .map(|(plugin, r)| {
            let mut text = match r {
                Ok(r) => report_text(r, name),
                Err(e) => tr!("failed: {}", e),
            };
            if let (Ok(r), Some(d)) = (r, dev) {
                record_export(store, &d.id, r, &mut text);
            }
            if done.len() > 1 {
                format!("{}: {text}", format_label(dev, plugin))
            } else {
                text
            }
        })
        .collect();
    if done.iter().all(|(_, r)| r.is_err()) {
        Err(parts.join("\n"))
    } else {
        Ok(parts.join("\n"))
    }
}

impl Ctx {
    fn app(&self) -> Option<App> {
        self.app.upgrade()
    }

    /// Runs `f` on `DeviceState` while the window exists.
    fn with_state<R>(&self, f: impl FnOnce(DeviceState<'_>) -> R) -> Option<R> {
        self.app().map(|app| f(app.global::<DeviceState>()))
    }

    /// Status line of the window (and `failed` colours it as an error).
    fn say(&self, msg: &str, failed: bool) {
        self.with_state(|s| {
            s.set_status(msg.into());
            s.set_failed(failed);
        });
    }

    fn busy(&self) -> bool {
        self.job.borrow().is_some()
    }

    /// The device chosen in the window.
    fn current(&self) -> Option<Device> {
        self.device(self.current.borrow().as_deref()?)
    }

    fn device(&self, id: &str) -> Option<Device> {
        self.devices.borrow().iter().find(|d| d.id == id).cloned()
    }

    /// [`Self::device`], saying so when the device was unplugged since the browser's tree was drawn.
    fn device_or_say(&self, id: &str) -> Option<Device> {
        let d = self.device(id);
        if d.is_none() {
            self.say(tr!("That device is not mounted any more"), true);
        }
        d
    }

    /// Runs `f` on the device chosen in the window, or asks to choose one.
    fn on_current(self: &Rc<Self>, f: impl FnOnce(&Rc<Self>, &Device)) {
        match self.current() {
            Some(d) => f(self, &d),
            None => self.say(tr!("Choose a device first"), true),
        }
    }

    /// Whether an export may write to `d`; says why not.
    fn writable(&self, d: &Device) -> bool {
        let ok = d.writable();
        if !ok {
            self.say(
                &format!(
                    "{}: {}",
                    d.label,
                    d.support.message().unwrap_or(tr!("mounted read-only"))
                ),
                true,
            );
        }
        ok
    }

    fn read_collection(&self, f: impl FnOnce() -> Result<Snapshot, String>) -> Option<Snapshot> {
        f().map_err(|e| self.say(&tr!("cannot read the collection: {}", e), true))
            .ok()
    }

    /// The collection as it is (overlaid tracks and their analysis), for the jobs that compare or look tracks up.
    fn collection(&self) -> Option<Snapshot> {
        self.read_collection(|| Snapshot::take(&*self.lib))
    }

    /// The collection as the export writes it: the overlaid tracks, the store's colours / tags / user playlists, the decks'
    /// session edits (grid, cues of a loaded track).
    fn snapshot(&self) -> Option<Snapshot> {
        self.read_collection(|| {
            let mut snap = Snapshot::take(&*self.lib)?;
            snap.add_store(&self.store)?;
            for (t, beats, cues) in self.decks.iter().filter_map(|d| d.session()) {
                snap.override_session(&t, beats, cues);
            }
            Ok(snap)
        })
    }

    /// What an export to `dev` (or a plain folder) is configured with: the free space, the device's settings, the Cue Banks.
    fn options(&self, dev: Option<&Device>) -> ExportOptions {
        ExportOptions {
            free: dev.and_then(|d| device::space(&d.mount)).map(|s| s.1),
            config: device_settings::config(&self.store, dev.map_or("", |d| d.id.as_str())),
            banks: store::collect_banks(&self.store).unwrap_or_else(|e| {
                eprintln!("cue banks: {e}");
                Vec::new()
            }),
        }
    }

    /// The plugin of `d` when it offers `f` for it; says so otherwise.
    fn provider_for(&self, d: &Device, f: Feature) -> Option<Provider> {
        let p = d.provider().filter(|_| d.can(f));
        if p.is_none() {
            self.say(&tr!("{}: its plugin does not offer that", d.label), true);
        }
        p
    }

    /// Asks `text`; "yes" (button `yes`) runs `then`.
    fn confirm(&self, text: &str, yes: &str, then: impl FnOnce(&Rc<Ctx>) + 'static) {
        *self.ask.borrow_mut() = Some(Box::new(then));
        self.with_state(|s| {
            s.set_confirm_text(text.into());
            s.set_confirm_yes(yes.into());
            s.set_confirm_open(true);
            s.set_visible(true);
        });
    }

    /// Starts the next queued automatic job, if no job runs and the device is still there.
    fn run_autos(self: &Rc<Self>) {
        while !self.busy() {
            let Some((id, what)) = self.autos.borrow_mut().pop_front() else {
                return;
            };
            let Some(dev) = self.device(&id) else {
                continue;
            };
            match what {
                Auto::History => actions::import_histories(self, &dev, true),
                Auto::Sync => sync::run(self, &dev, true),
                Auto::Link => self.link(&dev),
            }
        }
    }
}

/// Opens the window on the device `id` (the browser's Devices tree calls this when a device row is chosen).
pub fn show_panel(app: &App, id: &str) {
    let s = app.global::<DeviceState>();
    if let Some(i) = s.get_devices().iter().position(|d| d.id == id) {
        s.set_current(i as i32);
    }
    s.set_tab(0);
    s.set_visible(true);
    s.invoke_select(s.get_current());
    s.invoke_opened();
}

/// Connects `DeviceState`; the returned guard keeps the timer alive.
pub fn wire(app: &App, lib: Rc<dyn Library>, decks: Decks) -> Guard {
    let cx = Rc::new(Ctx {
        app: app.as_weak(),
        lib,
        decks,
        store: store::global(),
        devices: RefCell::default(),
        current: RefCell::default(),
        job: RefCell::default(),
        autos: RefCell::default(),
        greeted: RefCell::default(),
        tried: RefCell::default(),
        ask: RefCell::default(),
        export: Pick::default(),
        snap: RefCell::default(),
        preselect: RefCell::default(),
        backups: RefCell::default(),
        sync: Pick::default(),
        ticks: Cell::new(0),
        mirror: Cell::new(false),
        models: Models {
            devices: Rc::default(),
            names: Rc::default(),
            actions: Rc::default(),
            backups: Rc::default(),
            formats: Rc::default(),
            targets: Rc::default(),
        },
        links: RefCell::default(),
        pushes: RefCell::default(),
    });
    let s = app.global::<DeviceState>();
    s.set_devices(ModelRc::from(cx.models.devices.clone()));
    s.set_has_plugin(!openfader::plugin::device::providers().is_empty());
    s.set_device_names(ModelRc::from(cx.models.names.clone()));
    s.set_actions(ModelRc::from(cx.models.actions.clone()));
    s.set_backups(ModelRc::from(cx.models.backups.clone()));
    s.set_formats(ModelRc::from(cx.models.formats.clone()));
    let c = cx.clone();
    s.on_format_toggle(move |i| c.toggle_format(i.max(0) as usize));
    let c = cx.clone();
    s.on_format_universal(move |on| c.set_universal(on));
    let c = cx.clone();
    s.on_format_option(move |i, k, v| {
        c.set_option(i.max(0) as usize, k.max(0) as usize, v.max(0) as usize)
    });
    s.set_export_rows(ModelRc::from(cx.export.rows.clone()));
    s.set_sync_rows(ModelRc::from(cx.sync.rows.clone()));
    s.set_backup_dir(
        device::backup::default_dir()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default()
            .into(),
    );

    let c = cx.clone();
    s.on_select(move |i| {
        let id = c
            .devices
            .borrow()
            .get(i.max(0) as usize)
            .map(|d| d.id.clone());
        *c.current.borrow_mut() = id;
        c.show_current();
        sync::refresh(&c);
    });
    let c = cx.clone();
    s.on_opened(move || {
        c.refresh_devices();
        export::refresh(&c, true);
        sync::refresh(&c);
        backup::refresh(&c);
    });
    let c = cx.clone();
    s.on_cancel(move || {
        if let Some(j) = c.job.borrow().as_ref() {
            j.ctl.cancel();
            c.say(tr!("Cancelling…"), false);
        }
    });
    let c = cx.clone();
    s.on_confirmed(move |yes| {
        let then = c.ask.borrow_mut().take();
        c.with_state(|s| s.set_confirm_open(false));
        if let (true, Some(then)) = (yes, then) {
            then(&c);
        }
    });
    export::wire(&cx, app);
    sync::wire(&cx, app);
    backup::wire(&cx, app);
    actions::wire(&cx, app);
    link::register(&cx);
    transfer::wire(&cx, app);

    let c = cx.clone();
    let timer = Timer::default();
    timer.start(TimerMode::Repeated, Duration::from_millis(250), move || {
        c.poll_job();
        c.ticks.set(c.ticks.get() + 1);
        if c.ticks.get() % 4 == 1 {
            c.refresh_devices();
        }
        c.run_autos();
        c.run_pushes();
    });
    Guard {
        _timer: timer,
        _ctx: cx,
    }
}
