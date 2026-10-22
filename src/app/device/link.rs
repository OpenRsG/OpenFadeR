//! Cue sync ([`openfader::device::link`]) in the app (Preferences > DJ SYSTEM > DEVICE > CUE SYNC, on by default):
//! - a device that appears is matched against the collection on a worker ([`Ctx::link`]): the links are kept for the run, and
//!   each copy that lacks cues, a grid or phrases is filled from the other;
//! - a track loaded on a deck ([`track_loading`], called by the deck before it reads the analysis) settles with its other copy:
//!   the copy loaded wins;
//! - an edit of a loaded track's cues, grid or phrases ([`track_edited`]) is written to its device copies a few seconds after
//!   the last one ([`Ctx::run_pushes`]), in every format picked for the device.
//!
//! What the collection takes is written to the store at once; what a device takes is an export of just those tracks, run as the
//! window's job when no other job runs.

use super::{Auto, Ctx, record_all};
use openfader::{
    device::{self, Device, link},
    library::{self, Track, snapshot::Snapshot},
    plugin::device::Feature,
    store::{self, device_settings::Behaviour},
};
use parking_lot::Mutex;
use std::{
    cell::RefCell,
    rc::{Rc, Weak},
    sync::Arc,
    time::{Duration, Instant},
};

/// How long an edit waits for the next one before the device is written (a cue dragged around writes once).
const SETTLE: Duration = Duration::from_secs(3);

thread_local! {
    /// The window logic, for the deck's hooks.
    static CTX: RefCell<Weak<Ctx>> = RefCell::default();
}

/// Lets [`track_loading`] and [`track_edited`] reach `cx`.
pub(super) fn register(cx: &Rc<Ctx>) {
    CTX.with(|c| *c.borrow_mut() = Rc::downgrade(cx));
}

/// Runs `f` while the window logic exists and cue sync is on.
fn with_ctx(f: impl FnOnce(&Rc<Ctx>)) {
    if let Some(cx) = CTX.with(|c| c.borrow().upgrade())
        && Behaviour::load(&cx.store).sync_cues
    {
        f(&cx);
    }
}

/// `t` is about to be loaded on a deck: its copies settle, the loaded one winning, before the deck reads its analysis.
pub fn track_loading(t: &Track) {
    with_ctx(|cx| cx.settle_load(t));
}

/// The cues, grid or phrases of `t` were edited (and stored): its device copies are written soon.
pub fn track_edited(t: &Track) {
    with_ctx(|cx| {
        let key = store::key(t);
        let ids: Vec<String> = cx
            .links
            .borrow()
            .iter()
            .filter(|(_, l)| l.contains_key(&key))
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            cx.queue_push(&id, &key);
        }
    });
}

impl Ctx {
    /// Matches the collection against the library of `d` on a worker and fills the copies that lack a part; the links replace
    /// the device's. Only a device whose plugin reads its edits back (Update Collection) takes part.
    pub(super) fn link(self: &Rc<Self>, d: &Device) {
        if !d.has_library() || !d.can(Feature::Update) || !Behaviour::load(&self.store).sync_cues {
            return;
        }
        let (collection, Some(reader)) = (self.lib.tracks(None), self.lib.analysis_reader()) else {
            return;
        };
        let collection = match collection {
            Ok(t) => t,
            Err(e) => return eprintln!("cue sync: {e}"),
        };
        let writers = if d.writable() {
            self.writers(d).unwrap_or_default()
        } else {
            Vec::new()
        };
        let (o, store, dev) = (self.options(Some(d)), self.store.clone(), d.clone());
        let out: Arc<Mutex<Option<link::Links>>> = Arc::default();
        let (found, id) = (out.clone(), d.id.clone());
        self.start_then(
            Some(d.id.clone()),
            tr!("Matching tracks"),
            move |ctl| {
                let lib = device::open_library(&dev)?;
                let links = link::links(&*lib, &dev.mount, collection)?;
                let (pulled, mut push) = link::fill(&links, &*lib, &reader, &store)?;
                *found.lock() = Some(links);
                if writers.is_empty() {
                    push.clear();
                }
                let pushed = push.len();
                if pushed > 0 {
                    let done = device::export::export(
                        &writers,
                        &dev.mount,
                        &Snapshot::of_tracks(push),
                        &o,
                        &ctl,
                    )?;
                    record_all(&store, Some(&dev), &done, &dev.label)?;
                }
                Ok(if pulled + pushed > 0 {
                    tr!(
                        "{}: cue sync filled {} track(s) here and {} on the device",
                        dev.label,
                        pulled,
                        pushed
                    )
                } else {
                    String::new()
                })
            },
            move |cx, _| {
                if let Some(links) = out.lock().take() {
                    cx.links.borrow_mut().insert(id, Rc::new(links));
                }
            },
        );
    }

    /// Matches device `id` again when no job runs (an export just added or changed tracks on it).
    pub(super) fn relink(&self, id: &str) {
        let mut autos = self.autos.borrow_mut();
        if !autos
            .iter()
            .any(|(i, a)| i == id && matches!(a, Auto::Link))
        {
            autos.push_back((id.to_owned(), Auto::Link));
        }
    }

    /// Settles every linked copy of `t` before it loads: the copy loaded wins (an empty part is filled instead).
    fn settle_load(&self, t: &Track) {
        let Some(file) = t.file.as_deref() else {
            return;
        };
        let key = store::key(t);
        let origin = self
            .devices
            .borrow()
            .iter()
            .find(|d| file.starts_with(&d.mount))
            .map(|d| d.id.clone());
        let pairs: Vec<(String, link::Pair)> = self
            .links
            .borrow()
            .iter()
            .filter_map(|(id, l)| l.get(&key).map(|p| (id.clone(), p.clone())))
            .collect();
        for (id, p) in pairs {
            let winner = match &origin {
                Some(o) if *o == id => link::Winner::Device,
                Some(_) => continue, // loaded from another device: this one waits for its own turn
                None => link::Winner::Collection,
            };
            let dev = library::load_ref(&p.device).unwrap_or_default();
            let col = self.lib.analysis(&p.collection).unwrap_or_default();
            let m = link::settle(&link::Marks::from(&dev), &link::Marks::from(&col), winner);
            if let Err(e) = link::pull(&self.store, &key, &m.to_collection, &col.beats) {
                eprintln!("cue sync: {e}");
            }
            if !m.to_device.is_empty() {
                self.queue_push(&id, &key);
            }
        }
    }

    /// Writes `key`'s device copy on device `id` once the edits settle.
    fn queue_push(&self, id: &str, key: &str) {
        let mut pushes = self.pushes.borrow_mut();
        let (at, keys) = pushes.entry(id.to_owned()).or_default();
        *at = Some(Instant::now());
        keys.insert(key.to_owned());
    }

    /// Starts writing the queued tracks of one device whose last edit settled, when no job runs (the window's timer).
    pub(super) fn run_pushes(self: &Rc<Self>) {
        if self.busy() {
            return;
        }
        let due = self
            .pushes
            .borrow()
            .iter()
            .find(|(_, (at, _))| at.is_some_and(|a| a.elapsed() >= SETTLE))
            .map(|(id, _)| id.clone());
        let Some(id) = due else { return };
        let Some((_, keys)) = self.pushes.borrow_mut().remove(&id) else {
            return;
        };
        let links = self.links.borrow().get(&id).cloned();
        let (Some(d), Some(links)) = (self.device(&id), links) else {
            return;
        };
        if !d.writable() {
            return;
        }
        let tracks: Vec<_> = keys
            .iter()
            .filter_map(|k| links.get(k))
            .map(|p| (p.collection.clone(), self.lib.analysis(&p.collection)))
            .collect();
        let Some(writers) = self.writers(&d) else {
            return;
        };
        let (o, store, n) = (self.options(Some(&d)), self.store.clone(), tracks.len());
        self.start(Some(id), tr!("Writing cues"), move |ctl| {
            let done =
                device::export::export(&writers, &d.mount, &Snapshot::of_tracks(tracks), &o, &ctl)?;
            record_all(&store, Some(&d), &done, &d.label)?;
            Ok(tr!("cues of {} track(s) written to {}", n, d.label))
        });
    }
}
