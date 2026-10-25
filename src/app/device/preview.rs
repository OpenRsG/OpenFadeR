//! The preview card every write to a device goes through (EXPORT, SYNC NOW, the sync a mounted device asks for, Transfer) and the
//! question a plugged-in device with synced playlists asks. Preferences > DJ SYSTEM > DEVICE: "Show what will be written first"
//! (`preview`) and "Ask before syncing a plugged-in device" (`sync_ask`).
//!
//! [`Ctx::gate`]: preview off → the write starts at once. On → a worker asks the formats what they would do
//! ([`openfader::device::plan`]: dry runs, or the host's estimate for a plugin without them; nothing is written), the card shows
//! it with START / CANCEL, and START runs what the caller handed over. The write job rebuilds everything itself from the
//! collection as it is then: the card is only a look.

use super::pace::approx;
use super::{format_label, Ctx, DeviceState, PreviewRow};
use openfader::{
    device::{
        self,
        plan::{self, Preview, Speeds},
        Device,
    },
    library::snapshot::{Pending as Collection, Snapshot},
    plugin::device::{Control, ExportOptions, Feature, PlanItem, Provider},
    store::{device_settings::Behaviour, Store},
};
use parking_lot::Mutex;
use slint::{ComponentHandle, ModelRc, VecModel};
use std::{
    cell::{Cell, RefCell},
    path::PathBuf,
    rc::Rc,
    sync::Arc,
};

/// Rows of a track list the card draws; the rest is "… and N more" (the list is virtual, this keeps building it cheap).
const ROWS: usize = 2000;
/// Lines of one kind of trouble the card words out.
const LINES: usize = 4;

/// What the write job writes: built on the worker from the collection (and, for a Transfer to a playlist, from what the device
/// holds).
pub(super) struct Built {
    pub snap: Arc<Snapshot>,
    /// Transfer: tracks that go / were left out as already there.
    pub added: usize,
    pub skipped: usize,
    /// Something to say instead of writing ("Nothing transferred: ...").
    pub idle: Option<String>,
}

impl Built {
    /// The collection `pending` with its analyses read now ([`Collection::read`]): call it on the worker.
    pub fn read(pending: &Collection, store: &Store) -> Self {
        Self {
            snap: Arc::new(pending.read(store)),
            added: 0,
            skipped: 0,
            idle: None,
        }
    }
}

pub(super) type Build = Box<dyn FnOnce(&Control) -> Result<Built, String> + Send>;
pub(super) type Go = Box<dyn FnOnce(&Rc<Ctx>)>;

/// Where a write goes and with which formats.
#[derive(Clone)]
pub(super) struct Target {
    /// The device, or a plain folder taken as one ([`device::folder`]).
    pub dev: Device,
    pub dest: PathBuf,
    /// The device's label, else the folder.
    pub name: String,
    pub writers: Vec<Provider>,
    /// Plugin names that answer a dry run.
    pub plans: Vec<String>,
    pub o: ExportOptions,
}

impl Target {
    /// `dev` (a device or a folder) written by `writers`.
    pub fn new(cx: &Ctx, dev: &Device, writers: Vec<Provider>) -> Self {
        let plans = dev
            .formats
            .iter()
            .filter(|(_, i)| i.features.contains(&Feature::Plan))
            .map(|(n, _)| n.clone())
            .collect();
        Self {
            name: dev.label.clone(),
            o: cx.options(Some(dev)),
            dev: dev.clone(),
            dest: dev.mount.clone(),
            writers,
            plans,
        }
    }
}

/// What START of the open card does, and the files the job will write (to learn the time per small file from).
struct Pending {
    go: Go,
    files: usize,
}

/// The state of the card and the question.
#[derive(Default)]
pub(super) struct Prep {
    pending: RefCell<Option<Pending>>,
    asked: RefCell<Option<Go>>,
    /// Files the next job is expected to write: set by START, taken by the job.
    expect: Cell<usize>,
    /// The job being started is one of the app's own ([`super::Auto`]).
    pub automatic: Cell<bool>,
    /// What the status line last said about a job's end ([`Ctx::report`]).
    pub report: RefCell<String>,
    add: Rc<VecModel<PreviewRow>>,
    update: Rc<VecModel<PreviewRow>>,
}

impl Prep {
    /// The files the job about to start will write.
    pub fn take_expected(&self) -> usize {
        self.expect.take()
    }
}

pub(super) fn wire(cx: &Rc<Ctx>, app: &super::App) {
    let s = app.global::<DeviceState>();
    s.set_preview_add_rows(ModelRc::from(cx.prep.add.clone()));
    s.set_preview_update_rows(ModelRc::from(cx.prep.update.clone()));
    let c = cx.clone();
    s.on_preview_answer(move |start| {
        c.with_state(|s| s.set_preview_open(false));
        c.prep.add.set_vec(Vec::new());
        c.prep.update.set_vec(Vec::new());
        let p = c.prep.pending.borrow_mut().take();
        if let (true, Some(p)) = (start, p) {
            c.prep.expect.set(p.files);
            (p.go)(&c);
            c.prep.expect.set(0);
        }
    });
    let c = cx.clone();
    s.on_sync_ask_answer(move |yes| {
        c.with_state(|s| s.set_sync_ask_open(false));
        if let (true, Some(go)) = (yes, c.prep.asked.borrow_mut().take()) {
            go(&c);
        }
    });
}

/// "Artist - Title" of a plan item.
fn name(i: &PlanItem) -> String {
    match (i.artist.is_empty(), i.title.is_empty()) {
        (false, false) => format!("{} - {}", i.artist, i.title),
        (true, false) => i.title.clone(),
        _ => i.key.clone(),
    }
}

fn rows(items: &[PlanItem]) -> Vec<PreviewRow> {
    let mut v: Vec<PreviewRow> = items
        .iter()
        .take(ROWS)
        .map(|i| PreviewRow {
            title: name(i).into(),
            detail: if i.bytes > 0 {
                device::size(i.bytes)
            } else {
                String::new()
            }
            .into(),
        })
        .collect();
    if items.len() > ROWS {
        v.push(PreviewRow {
            title: tr!("… and {} more", items.len() - ROWS).into(),
            detail: Default::default(),
        });
    }
    v
}

/// The lines of trouble: what the plugins say, what a failed dry run says, what the estimate found; `short` = the room needed
/// and the room there is, when it is not enough.
fn trouble(
    p: &Preview,
    dev: Option<&Device>,
    max_file: Option<u64>,
    short: Option<(u64, u64)>,
) -> Vec<String> {
    let mut v = p.blocked.clone();
    v.extend(p.failed.iter().map(|(f, e)| {
        if p.formats.len() > 1 {
            format!("{}: {e}", format_label(dev, f))
        } else {
            e.clone()
        }
    }));
    if let Some((need, free)) = short {
        v.push(tr!(
            "Not enough room: this needs {} and the stick has {} free. Leave some playlists out or free some space.",
            device::size(need),
            device::size(free)
        ));
    }
    let limit = max_file.map_or_else(String::new, device::size);
    v.extend(p.too_big.iter().take(LINES).map(|i| {
        tr!(
            "{} is {}: this stick holds files up to {}. It would be left out; convert it to FLAC or use an exFAT stick.",
            name(i),
            device::size(i.bytes),
            limit
        )
    }));
    if p.too_big.len() > LINES {
        v.push(tr!(
            "… and {} more tracks are too big for this stick",
            p.too_big.len() - LINES
        ));
    }
    if !p.no_file.is_empty() {
        v.push(tr_n!(
            "{n} track has no audio file and would be left out: {}",
            "{n} tracks have no audio file and would be left out: {}",
            p.no_file.len(),
            p.no_file
                .iter()
                .take(LINES)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    v
}

impl Ctx {
    /// Runs the dry runs / estimates for `build`'s snapshot on a worker (`analyse` tracks are analysed first); `then` gets the
    /// answer on the UI thread. A `build` with nothing to write ends the job with its message instead.
    pub(super) fn plan_then(
        self: &Rc<Self>,
        t: &Target,
        analyse: usize,
        build: Build,
        then: impl FnOnce(&Rc<Ctx>, Preview) + 'static,
    ) {
        let (slot, t) = (Arc::new(Mutex::new(None)), t.clone());
        let put = slot.clone();
        let id = Some(t.dev.id.clone());
        self.start_then(
            id,
            tr!("Preparing the preview"),
            move |ctl| {
                let built = build(&ctl)?;
                if let Some(m) = built.idle {
                    return Ok(m);
                }
                let can = |n: &str| t.plans.iter().any(|p| p == n);
                *put.lock() = Some(plan::build(
                    &t.writers,
                    &can,
                    &t.dest,
                    &built.snap,
                    &t.o,
                    analyse,
                    &ctl,
                )?);
                Ok(String::new())
            },
            move |cx, _| {
                if let Some(p) = slot.lock().take() {
                    then(cx, p);
                }
            },
        );
    }

    /// The gate of every write: preview off → `go` at once; on → the card first, `go` on START. `build` makes what the write
    /// would write (only called when the card is wanted).
    pub(super) fn gate(
        self: &Rc<Self>,
        t: &Target,
        analyse: usize,
        build: impl FnOnce() -> Build,
        go: impl FnOnce(&Rc<Ctx>) + 'static,
    ) {
        if !Behaviour::load(&self.store).preview {
            return go(self);
        }
        let t2 = t.clone();
        self.plan_then(t, analyse, build(), move |cx, p| {
            cx.show_preview(&t2, p, Box::new(go))
        });
    }

    /// Fills the card and opens it; START runs `go`.
    pub(super) fn show_preview(self: &Rc<Self>, t: &Target, p: Preview, go: Go) {
        let speeds = Speeds::load(&self.store, &t.dev.id);
        let behaviour = Behaviour::load(&self.store);
        let short =
            t.o.free
                .filter(|free| p.needed() > *free)
                .map(|f| (p.needed(), f));
        let mut lines = Vec::new();
        if p.analyse > 0 {
            lines.push(tr_n!(
                "{n} track is analysed first (beat grid and key): {}",
                "{n} tracks are analysed first (beat grid and key): {}",
                p.analyse,
                approx(p.analyse as f64 * speeds.analyse())
            ));
        }
        if p.playlists > 0 {
            lines.push(tr_n!(
                "{n} playlist written",
                "{n} playlists written",
                p.playlists
            ));
        }
        let secs = approx(p.seconds(&speeds));
        lines.push(if speeds.measured() && !p.estimated {
            tr!("Takes {} (from earlier jobs on this stick)", secs)
        } else {
            tr!("Takes {} (estimate)", secs)
        });
        if behaviour.backup_before_write
            && !t.dev.is_folder()
            && t.dev.has_library()
            && t.dev.can(Feature::Backup)
        {
            lines.push(tr!("The current library is backed up first").to_owned());
        }
        lines.push(tr!("Your music on this computer is not changed").to_owned());
        let formats = p
            .formats
            .iter()
            .map(|f| format_label(Some(&t.dev), f))
            .collect::<Vec<_>>()
            .join(" + ");
        let trouble = trouble(&p, Some(&t.dev), t.o.max_file, short);
        self.prep.add.set_vec(rows(&p.add));
        self.prep.update.set_vec(rows(&p.update));
        *self.prep.pending.borrow_mut() = Some(Pending { go, files: p.files });
        self.with_state(|s| {
            s.set_preview_title(tr!("Write to {}", t.name).into());
            s.set_preview_device(
                if t.dev.is_folder() {
                    format!("{} · {}", tr!("folder {}", t.dest.display()), formats)
                } else {
                    format!("{} · {}", t.dev.summary(), formats)
                }
                .into(),
            );
            s.set_preview_add(
                if p.add.is_empty() {
                    tr!("No new track to copy").to_owned()
                } else {
                    tr_n!(
                        "{n} new track to copy ({})",
                        "{n} new tracks to copy ({})",
                        p.add.len(),
                        device::size(p.copy_bytes)
                    )
                }
                .into(),
            );
            s.set_preview_update(
                if p.update.is_empty() {
                    String::new()
                } else {
                    tr_n!(
                        "{n} track is there already: its marks are refreshed",
                        "{n} tracks are there already: their marks are refreshed",
                        p.update.len()
                    )
                }
                .into(),
            );
            s.set_preview_lines(ModelRc::new(VecModel::from(
                lines
                    .into_iter()
                    .map(Into::into)
                    .collect::<Vec<slint::SharedString>>(),
            )));
            s.set_preview_blocked(ModelRc::new(VecModel::from(
                trouble
                    .into_iter()
                    .map(Into::into)
                    .collect::<Vec<slint::SharedString>>(),
            )));
            s.set_preview_can_start(p.can_start() && short.is_none());
            s.set_preview_open(true);
        });
    }

    /// Asks `text` on a small card (SYNC / NOT NOW); SYNC runs `yes`.
    pub(super) fn ask_sync(self: &Rc<Self>, text: String, yes: impl FnOnce(&Rc<Ctx>) + 'static) {
        *self.prep.asked.borrow_mut() = Some(Box::new(yes));
        self.with_state(|s| {
            s.set_sync_ask_text(text.into());
            s.set_sync_ask_open(true);
        });
    }
}
