//! Transfer: tracks or a playlist sent to a device from the browser, in every format picked for the device ([`openfader::device::
//! transfer`]): dropped on the device's row (the device itself: the tracks only, no playlist) or on one of its playlists (the
//! playlist ends up with what it held, then the new tracks), or sent from the track menu's "Transfer to <device>" (which lists the
//! mounted writable devices, [`Ctx::show_targets`]). A My Playlists playlist or folder dropped on a device row goes as a Sync
//! of that one playlist does.
//!
//! Analysis first: a track without a beat grid or a key is analysed by the app's analysis pool before anything is written (a job
//! of its own, "Analysing {} of {}…", the Analysis Lock respected), so the cues, grid, waveform and key reach the device; then
//! the export job ("Transferring…") builds its snapshot from what the collection holds by then. Both show in the status bar.

use super::preview::{Build, Built, Target};
use super::{record_all, toast, Ctx, DeviceState, BUSY};
use crate::app::{analyze, browser, BrowserState};
use openfader::{
    analysis::{Analysis, cache},
    device::{
        self, link,
        plan::Speeds,
        transfer::{self, Plan},
        Device,
    },
    library::{snapshot::Snapshot, Track},
    plugin::device::Feature,
    store,
};
use parking_lot::Mutex;
use slint::{ComponentHandle, SharedString};
use std::{
    cell::RefCell,
    collections::HashMap,
    rc::{Rc, Weak},
    sync::Arc,
    time::Instant,
};

/// A playlist of a device as the browser's tree shows it: its id in the device's library and its names from the top level down.
#[derive(Clone, Debug, PartialEq)]
pub struct List {
    pub id: String,
    pub path: Vec<String>,
}

/// Where a transfer lands: a device (by id), and one of its playlists (`None`: the device itself).
#[derive(Clone, Debug, PartialEq)]
pub struct Dest {
    pub device: String,
    pub list: Option<List>,
}

/// What a transfer moves.
#[derive(Clone)]
enum Pack {
    Tracks(Vec<Track>),
    /// A playlist or folder of the export snapshot, by its id (`store:<node>` for My Playlists).
    Playlist(String),
}

/// [`Pack`] read from the library: what the export job writes, with the analysis the library has now. Cheap to clone: the
/// preview and the write each build from it.
#[derive(Clone)]
enum Gathered {
    Tracks(Arc<Vec<(Track, Option<Analysis>)>>),
    Playlist(Arc<Snapshot>),
}

impl Gathered {
    /// The tracks that are analysed before they are written ([`transfer::needs_analysis`]).
    fn todo(&self) -> Vec<Track> {
        let tracks = match self {
            Self::Tracks(v) => v.as_slice(),
            Self::Playlist(s) => s.tracks(),
        };
        tracks
            .iter()
            .filter(|(t, a)| transfer::needs_analysis(t, a.as_ref(), cache::glance(t).as_ref()))
            .map(|(t, _)| t.clone())
            .collect()
    }
}

thread_local! {
    /// The window logic, for the browser's drops.
    static CTX: RefCell<Weak<Ctx>> = RefCell::default();
}

/// `tracks` were dropped on `to`.
pub fn drop_tracks(to: Dest, tracks: Vec<Track>) {
    with_ctx(|cx| cx.go(&to, Pack::Tracks(tracks)));
}

/// The playlist or folder `id` (a [`Snapshot`] playlist id) was dropped on the device `to`.
pub fn drop_playlist(to: Dest, id: String) {
    with_ctx(|cx| cx.go(&to, Pack::Playlist(id)));
}

pub(super) fn with_ctx(f: impl FnOnce(&Rc<Ctx>)) {
    if let Some(cx) = CTX.with(|c| c.borrow().upgrade()) {
        f(&cx);
    }
}

/// Connects the track menu's "Transfer to".
pub(super) fn wire(cx: &Rc<Ctx>, app: &crate::app::App) {
    CTX.with(|c| *c.borrow_mut() = Rc::downgrade(cx));
    let b = app.global::<BrowserState>();
    b.set_transfer_targets(slint::ModelRc::from(cx.models.targets.clone()));
    let c = cx.clone();
    b.on_transfer_to(move |i| {
        let tracks = browser::context().tracks;
        let Some(d) = usize::try_from(i).ok().and_then(|i| c.targets().into_iter().nth(i)) else {
            return;
        };
        c.go(
            &Dest {
                device: d.id,
                list: None,
            },
            Pack::Tracks(tracks),
        );
    });
}

impl Ctx {
    /// The devices the track menu lists: mounted, writable, with a plugin that exports.
    fn targets(&self) -> Vec<Device> {
        self.devices
            .borrow()
            .iter()
            .filter(|d| d.writable() && d.can(Feature::Export))
            .cloned()
            .collect()
    }

    pub(super) fn show_targets(&self) {
        let labels: Vec<SharedString> = self
            .targets()
            .iter()
            .map(|d| d.label.as_str().into())
            .collect();
        self.models.targets.set_vec(labels);
    }

    /// Starts a transfer; a refusal reaches the user as a toast (the window may be closed).
    fn go(self: &Rc<Self>, to: &Dest, pack: Pack) {
        if self.transfer(to, pack, false).is_none()
            && let Some(app) = self.app()
        {
            let text = app.global::<DeviceState>().get_status();
            toast::show(&app, &text);
        }
    }

    /// Checks the device and what is sent; then, unless it was analysed and confirmed before (`analysed`), the preview card, the
    /// analysis of what needs it and the write. `None`: refused, and said so in the window's status line.
    fn transfer(self: &Rc<Self>, to: &Dest, pack: Pack, analysed: bool) -> Option<()> {
        if self.busy() {
            self.say(crate::app::i18n::tr_dyn(BUSY), true);
            return None;
        }
        let d = self.device_or_say(&to.device)?;
        if !self.writable(&d) {
            return None;
        }
        let writers = self.writers(&d)?;
        let g = self.gather(&pack)?;
        let t = Target::new(self, &d, writers);
        if analysed {
            self.write_transfer(d, t, to, g);
            return Some(());
        }
        let batch = analyze::batch(&g.todo());
        let analyse = batch.as_ref().map_or(0, |b| b.len());
        let (bd, bto, bg) = (d.clone(), to.clone(), g.clone());
        let (go_t, dest) = (t.clone(), to.clone());
        self.gate(
            &t,
            analyse,
            || self.transfer_build(&bd, &bto, bg),
            move |cx| match batch {
                Some(batch) => cx.analyse(&d, batch, dest, pack),
                None => cx.write_transfer(d, go_t, &dest, g),
            },
        );
        Some(())
    }

    /// What `pack` is, read from the library now.
    fn gather(&self, pack: &Pack) -> Option<Gathered> {
        match pack {
            Pack::Tracks(tracks) if tracks.is_empty() => {
                self.say(tr!("Nothing to transfer: choose some tracks first"), true);
                None
            }
            Pack::Tracks(tracks) => {
                let picks = tracks
                    .iter()
                    .map(|t| (t.clone(), self.lib.analysis(t)))
                    .collect();
                let mut s = Snapshot::of_tracks(picks);
                for (t, beats, cues) in self.decks.iter().filter_map(|d| d.session()) {
                    s.override_session(&t, beats, cues);
                }
                Some(Gathered::Tracks(Arc::new(s.into_tracks())))
            }
            Pack::Playlist(id) => {
                let narrowed = self.snapshot()?.narrowed(&[id.as_str()]);
                if narrowed.is_none() {
                    self.say(tr!("That playlist has no tracks to transfer"), true);
                }
                narrowed.map(|s| Gathered::Playlist(Arc::new(s)))
            }
        }
    }

    /// Job 1: the analysis of `batch`; the transfer goes on when it ends.
    fn analyse(self: &Rc<Self>, d: &Device, batch: analyze::Batch, to: Dest, pack: Pack) {
        let found = Arc::new(Mutex::new(Vec::new()));
        let (out, store, n, id) = (found.clone(), self.store.clone(), batch.len(), d.id.clone());
        self.mirror.set(true);
        self.start_then(
            Some(d.id.clone()),
            tr!("Analysing"),
            move |ctl| {
                let say = |done: usize| ctl.set_phase(&tr!("Analysing {} of {}…", done, n));
                say(0);
                let started = Instant::now();
                let r = batch.run(store.clone(), &|| ctl.cancelled(), &say);
                if ctl.cancelled() {
                    return Err("cancelled".into());
                }
                if n > 0 {
                    let analyse_s = Some(started.elapsed().as_secs_f64() / n as f64);
                    Speeds::remember(&store, &id, Speeds { analyse_s, ..Speeds::default() });
                }
                *out.lock() = r;
                Ok(String::new())
            },
            move |cx, r| {
                analyze::publish(std::mem::take(&mut *found.lock()));
                cx.mirror.set(false);
                cx.clear_loading();
                if r.is_ok() {
                    cx.go_on(&to, pack);
                }
            },
        );
    }

    /// The transfer after its analysis: no second analysis, a refusal is toasted as in [`Self::go`].
    fn go_on(self: &Rc<Self>, to: &Dest, pack: Pack) {
        if self.transfer(to, pack, true).is_none()
            && let Some(app) = self.app()
        {
            let text = app.global::<DeviceState>().get_status();
            toast::show(&app, &text);
        }
    }

    fn clear_loading(&self) {
        if let Some(app) = self.app() {
            browser::loading(&app, "", None);
        }
    }

    /// What a transfer writes, built on the worker: a playlist target is the playlist's current members, then the new tracks.
    /// Used by the preview and by the write, which both start from the collection as it is then.
    fn transfer_build(&self, d: &Device, to: &Dest, g: Gathered) -> Build {
        let list = to.list.clone();
        let collection = list.as_ref().and_then(|_| self.lib.tracks_all().ok());
        let reader = self
            .lib
            .analysis_reader()
            .unwrap_or_else(|| Arc::new(|_: &Track| None));
        let (store, d) = (self.store.clone(), d.clone());
        Box::new(move |_| {
            let picked = matches!(g, Gathered::Tracks(_));
            let mut plan = match (g, list) {
                (Gathered::Playlist(snapshot), _) => Plan {
                    added: snapshot.len(),
                    skipped: 0,
                    snapshot: (*snapshot).clone(),
                },
                (Gathered::Tracks(picks), None) => Plan {
                    added: picks.len(),
                    skipped: 0,
                    snapshot: Snapshot::of_tracks((*picks).clone()),
                },
                (Gathered::Tracks(picks), Some(list)) => {
                    let lib = device::open_library(&d)?;
                    let pls = lib.playlists()?;
                    let at = transfer::find(&pls, &list.id, &list.path)
                        .ok_or(tr!("That playlist is not on the device any more"))?;
                    let all = collection.unwrap_or_default();
                    let own: HashMap<String, &Track> =
                        all.iter().map(|t| (store::key(t), t)).collect();
                    let current = transfer::members(&*lib, &at.id, &own, &|t| {
                        Some(link::collection_analysis(&reader, &store, t))
                    })?;
                    let (name, folders) = list.path.split_last().ok_or("a playlist without a name")?;
                    transfer::append((*picks).clone(), current, folders, name)
                }
            };
            let idle = (plan.added == 0).then(|| {
                tr_n!(
                    "Nothing transferred: {n} track is in {} already",
                    "Nothing transferred: {n} tracks are in {} already",
                    plan.skipped,
                    d.label
                )
            });
            // Colours and tags of picked tracks go along (a playlist snapshot has its own); a track the collection does not
            // know has none.
            if picked && idle.is_none() && let Err(e) = plan.snapshot.add_store_marks(&store) {
                eprintln!("transfer: {e}");
            }
            Ok(Built {
                snap: Arc::new(plan.snapshot),
                added: plan.added,
                skipped: plan.skipped,
                idle,
            })
        })
    }

    /// Job 2: the write (backup first, then the export).
    fn write_transfer(self: &Rc<Self>, d: Device, t: Target, to: &Dest, g: Gathered) {
        let build = self.transfer_build(&d, to, g);
        let (store, id) = (self.store.clone(), d.id.clone());
        self.mirror.set(true);
        self.write(
            t,
            tr!("Transferring"),
            build,
            move |b, done| {
                let detail = record_all(&store, Some(&d), done, &d.label)?;
                let mut text = tr_n!(
                    "{n} track transferred to {}",
                    "{n} tracks transferred to {}",
                    b.added,
                    d.label
                );
                if b.skipped > 0 {
                    text += &tr!(" ({} already there)", b.skipped);
                }
                let noisy = done
                    .iter()
                    .any(|(_, r)| r.as_ref().map_or(true, |r| !r.warnings.is_empty()));
                if noisy {
                    text.push('\n');
                    text.push_str(&detail);
                }
                Ok(text)
            },
            move |cx, r| {
                cx.mirror.set(false);
                cx.clear_loading();
                if r.is_ok() {
                    cx.relink(&id);
                    if let Some(app) = cx.app() {
                        browser::device_written(&app, &id);
                    }
                }
            },
        );
    }
}
