//! Keeping the card model in step with the machine: a worker thread builds the entries (enumerating ALSA devices opens every PCM,
//! about 0.25 s here), the UI thread patches the model; a 250 ms timer watches the page (a fresh scan when it opens, then
//! every [`PERIOD`] to pick up a hot-plugged card or a volume changed elsewhere; not right after the user's own volume edit).

use super::{EDITED, Entry, OPEN, SHOWN, app, build, slint_card};
use crate::app::{AudioCard, PrefsState};
use openfader::audio::{self, Status, device};
use slint::{ComponentHandle, Model, ModelRc, Timer, TimerMode, VecModel};
use std::{
    cell::{Cell, RefCell},
    collections::HashSet,
    sync::mpsc,
    time::{Duration, Instant},
};

/// How often the open page re-reads devices and volumes.
const PERIOD: Duration = Duration::from_secs(4);
/// After the user's own volume edit the page keeps what it shows for this long.
pub(super) const SETTLE: Duration = Duration::from_millis(1500);

/// What the background build needs: plain data. The stream's own state is read on the UI thread (it lives there).
pub(super) struct Job {
    pub(super) saved: String,
    pub(super) all: bool,
    pub(super) st: Status,
    pub(super) controller: Option<String>,
    pub(super) frames: u32,
    pub(super) open: HashSet<String>,
    /// The page just opened: forget the cached device lists first and rebuild the dialog's rows after.
    pub(super) fresh: bool,
}

thread_local! {
    static WORKER: RefCell<Option<mpsc::Sender<Job>>> = const { RefCell::new(None) };
    static WAS_OPEN: Cell<bool> = const { Cell::new(false) };
    static LAST_SYNC: Cell<Option<Instant>> = const { Cell::new(None) };
    static POLL: Timer = Timer::default();
}

/// Asks for a new model after a change. [`apply`] patches the model on the UI thread when the worker is done.
pub(in crate::app::prefs::audio) fn sync() {
    request(false);
}

fn request(fresh: bool) {
    let Some(app) = app() else { return };
    let (saved, all) =
        crate::app::prefs::with(|p| (p.audio.output_device.clone(), p.audio.output_show_all));
    app.global::<PrefsState>().set_audio_all(all);
    let st = audio::status();
    let job = Job {
        saved,
        all,
        frames: crate::app::prefs::with(|p| super::super::buffer_in_use(p, &st)),
        controller: audio::controller(),
        open: OPEN.with_borrow(Clone::clone),
        st,
        fresh,
    };
    WORKER.with_borrow_mut(|w| {
        let tx = w.get_or_insert_with(|| spawn(app.as_weak()));
        let _ = tx.send(job);
    });
}

fn spawn(app: slint::Weak<crate::app::App>) -> mpsc::Sender<Job> {
    let (tx, rx) = mpsc::channel::<Job>();
    std::thread::spawn(move || {
        while let Ok(mut job) = rx.recv() {
            while let Ok(newer) = rx.try_recv() {
                // only the newest request is worth building; a "fresh" one is not forgotten
                let fresh = job.fresh || newer.fresh;
                job = Job { fresh, ..newer };
            }
            if job.fresh {
                device::refresh();
            }
            let (entries, fresh) = (build(&job), job.fresh);
            if app
                .upgrade_in_event_loop(move |app| apply(&app, entries, fresh))
                .is_err()
            {
                break; // the window is gone
            }
        }
    });
    tx
}

/// Brings the model up to date: rows that changed are patched in place (an open drop-down or a held slider stays), a changed
/// card list replaces the model. Right after the user's own volume edit the levels already shown are kept.
fn apply(app: &crate::app::App, mut now: Vec<Entry>, fresh: bool) {
    let s = app.global::<PrefsState>();
    let model = s.get_audio_cards();
    let rows = model.as_any().downcast_ref::<VecModel<AudioCard>>();
    SHOWN.with_borrow_mut(|shown| {
        if EDITED.get().is_some_and(|t| t.elapsed() < SETTLE) {
            for e in &mut now {
                if let Some(old) = shown.iter().find(|o| o.key == e.key) {
                    e.level = old.level.clone();
                }
            }
        }
        let same_cards =
            shown.len() == now.len() && shown.iter().zip(&now).all(|(a, b)| a.key == b.key);
        match rows {
            Some(rows) if same_cards => {
                for (i, (old, new)) in shown.iter().zip(&now).enumerate() {
                    if old != new {
                        rows.set_row_data(i, slint_card(new));
                    }
                }
            }
            _ => s.set_audio_cards(ModelRc::new(VecModel::from(
                now.iter().map(slint_card).collect::<Vec<_>>(),
            ))),
        }
        *shown = now;
    });
    if fresh {
        crate::app::prefs::refresh(); // the rows (rates, channels) follow the fresh device lists
    }
}

/// Starts the page watcher.
pub(super) fn watch() {
    let Some(app) = app() else { return };
    let weak = app.as_weak();
    POLL.with(|t| {
        t.start(TimerMode::Repeated, Duration::from_millis(250), move || {
            let Some(app) = weak.upgrade() else { return };
            let s = app.global::<PrefsState>();
            let open = s.get_visible() && s.get_category() == 1;
            let was = WAS_OPEN.replace(open);
            if !open {
                return;
            }
            let due = LAST_SYNC.get().is_none_or(|t| t.elapsed() >= PERIOD)
                && EDITED.get().is_none_or(|t| t.elapsed() >= SETTLE);
            if !was || due {
                LAST_SYNC.set(Some(Instant::now()));
                request(!was); // a page that just opened sees what was plugged since
            }
        });
    });
}
