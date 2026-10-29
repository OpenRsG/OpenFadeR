//! Review before an import, the way back, and the cues that differ (ui/device-review.slint, `DeviceState.review-*`).
//!
//! With Preferences > DJ SYSTEM > DEVICE > "Show what a device would change in the collection before importing it" on (the default),
//! UPDATE COLLECTION and IMPORT HISTORIES first read what the device holds and open a list ([`Review`]): one row per track (or
//! history) with what would change and a tick; APPLY writes the ticked rows, CANCEL writes nothing. With it off they apply at once.
//! Either way every import keeps its way back ([`openfader::device::undo`]): the bar at the bottom of the window offers UNDO after an
//! import, the DEVICES tab's UNDO… lists the last five.
//!
//! The same dialog lists the cue sync conflicts of the chosen device ([`Review::Differ`]): tracks whose cues, grid or phrases are not
//! the same in the collection and on the device. Nothing moves for them until the user chooses a side, per track or for all.

use super::{Ctx, DeviceState};
use crate::app::{toast, CompareRow, ReviewRow};
use openfader::{
    analysis::{Beat, Cue},
    device::{
        Device, history,
        link::{self, Conflict, Marks},
        undo::{self, Entry},
        update::{self, Diff},
    },
    store::{device_settings::Behaviour, TRACK_COLORS},
};
use slint::{ComponentHandle, ModelRc};
use std::rc::Rc;

/// What a job that reads a device (and, with review off, applies what it read) hands to the UI thread.
pub(super) enum Planned<P> {
    /// The review is on: show this and wait for a go.
    Review(P),
    /// Applied; the kept way back, if anything changed.
    Done(Option<i64>),
}

/// What the review dialog shows now.
pub(super) enum Review {
    Update {
        device: Device,
        plan: update::Plan,
        ticked: Vec<bool>,
        /// An import from the device ([`super::import`]): its tracks the collection already has with other values.
        import: bool,
    },
    Histories {
        device: Device,
        plan: history::Plan,
        ticked: Vec<bool>,
        /// Item indexes of `plan` shown as rows (the ones not imported before).
        shown: Vec<usize>,
        delete_after: bool,
    },
    /// The cue sync conflicts of this device id.
    Differ(String),
    /// The kept imports, newest first.
    Recent(Vec<i64>),
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// "Title — Artist" (or what there is of it).
fn title_of(title: &str, artist: &str) -> String {
    match (title.is_empty(), artist.is_empty()) {
        (_, true) => title.to_owned(),
        (true, false) => artist.to_owned(),
        (false, false) => format!("{title} — {artist}"),
    }
}

/// At most 40 characters of a comment.
fn clip(s: &str) -> String {
    let one: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() > 40 {
        format!("{}…", one.chars().take(40).collect::<String>())
    } else {
        one
    }
}

/// `m:ss.mmm` of a position.
fn time(ms: u32) -> String {
    format!("{}:{:02}.{:03}", ms / 60_000, ms / 1000 % 60, ms % 1000)
}

fn colour(id: Option<u8>) -> String {
    match id.and_then(|i| TRACK_COLORS.get(usize::from(i).checked_sub(1)?)) {
        Some((name, _)) => crate::app::i18n::tr_dyn(name).to_owned(),
        None => tr!("none").to_owned(),
    }
}

/// What one [`Diff`] of a review row says.
pub(super) fn diff_text(d: &Diff) -> String {
    match d {
        Diff::Rating { from, to } => tr!("rating {} → {}", from, to),
        Diff::Comment { from, to } if from.is_empty() => tr!("comment: {}", clip(to)),
        Diff::Comment { to, .. } if to.is_empty() => tr!("comment cleared").to_owned(),
        Diff::Comment { to, .. } => tr!("comment changed: {}", clip(to)),
        Diff::Color { from, to } => tr!("colour {} → {}", colour(*from), colour(Some(*to))),
        Diff::Cues {
            added,
            removed,
            moved,
        } => {
            let mut parts = Vec::new();
            if *added > 0 {
                parts.push(format!("+{added}"));
            }
            if *removed > 0 {
                parts.push(format!("−{removed}"));
            }
            if *moved > 0 {
                parts.push(tr!("{} moved", moved));
            }
            if parts.is_empty() {
                tr!("cue details changed").to_owned()
            } else {
                tr!("cues {}", parts.join(" / "))
            }
        }
        Diff::Grid { moved_ms, bpm } => {
            let (m, bpm) = (*moved_ms, *bpm);
            let (shift, tempo) = (
                format!("{m:+}"),
                bpm.map(|(a, b)| {
                    (
                        format!("{:.2}", f32::from(a) / 100.0),
                        format!("{:.2}", f32::from(b) / 100.0),
                    )
                }),
            );
            match (m, tempo) {
                (0, None) => tr!("grid changed").to_owned(),
                (_, None) => tr!("grid moved {} ms", shift),
                (0, Some((a, b))) => tr!("BPM {} → {}", a, b),
                (_, Some((a, b))) => tr!("grid moved {} ms, BPM {} → {}", shift, a, b),
            }
        }
        Diff::Key { from, to } if from.is_empty() => tr!("key {}", to),
        Diff::Key { from, to } => tr!("key {} → {}", from, to),
    }
}

/// The detail line of an UPDATE COLLECTION row.
fn row_text(r: &update::Row) -> String {
    r.diff.iter().map(diff_text).collect::<Vec<_>>().join(" · ")
}

/// "3 hot, 2 memory, 1 loop" of a cue list.
fn cue_counts(c: &[Cue]) -> String {
    let loops = c.iter().filter(|c| c.loop_end_ms.is_some()).count();
    let hot = c
        .iter()
        .filter(|c| c.hot_cue > 0 && c.loop_end_ms.is_none())
        .count();
    let memory = c.len() - loops - hot;
    if c.is_empty() {
        return tr!("none").to_owned();
    }
    tr!("{} hot, {} memory, {} loops", hot, memory, loops)
}

/// Where the cues sit: "A 0:32.500, B 1:05.000, memory 2:00.100" (the first eight).
fn cue_times(c: &[Cue]) -> String {
    let mut v: Vec<&Cue> = c.iter().collect();
    v.sort_by_key(|c| (c.time_ms, c.hot_cue));
    let mut out: Vec<String> = v
        .iter()
        .take(8)
        .map(|c| match c.hot_cue {
            0 => format!("M {}", time(c.time_ms)),
            n => format!(
                "{} {}",
                char::from(b'A' + ((n - 1) % 26) as u8),
                time(c.time_ms)
            ),
        })
        .collect();
    if v.len() > 8 {
        out.push("…".into());
    }
    out.join(", ")
}

/// "starts 0:00.320 at 128.00 BPM, 640 beats".
fn grid_text(b: &[Beat]) -> String {
    match b.first() {
        None => tr!("none").to_owned(),
        Some(f) => tr!(
            "starts {} at {} BPM, {} beats",
            time(f.time_ms),
            format!("{:.2}", f32::from(f.bpm_x100) / 100.0),
            b.len()
        ),
    }
}

fn phrase_text(m: &Marks) -> String {
    match &m.phrases {
        None => tr!("none").to_owned(),
        Some(p) => tr!("{} phrases", p.list.len()),
    }
}

/// The compare view of one conflict: your collection's copy against the device's.
fn compare_rows(c: &Conflict) -> Vec<CompareRow> {
    let row = |what: &str, mine: String, stick: String| CompareRow {
        what: what.into(),
        mine: mine.into(),
        stick: stick.into(),
    };
    vec![
        row(
            tr!("CUES"),
            cue_counts(&c.collection.cues),
            cue_counts(&c.device.cues),
        ),
        row(
            tr!("CUE TIMES"),
            cue_times(&c.collection.cues),
            cue_times(&c.device.cues),
        ),
        row(
            tr!("GRID"),
            grid_text(&c.collection.beats),
            grid_text(&c.device.beats),
        ),
        row(
            tr!("PHRASES"),
            phrase_text(&c.collection),
            phrase_text(&c.device),
        ),
    ]
}

/// What differs, in one line, for the conflict list.
fn differ_text(c: &Conflict) -> String {
    let mut parts = Vec::new();
    if c.differ.cues {
        parts.push(tr!(
            "cues: {} in your collection, {} on the stick",
            c.collection.cues.len(),
            c.device.cues.len()
        ));
    }
    if c.differ.beats {
        let bpm = |b: &[Beat]| b.first().map_or(0.0, |f| f32::from(f.bpm_x100) / 100.0);
        parts.push(tr!(
            "grid: {} BPM, on the stick {} BPM",
            format!("{:.2}", bpm(&c.collection.beats)),
            format!("{:.2}", bpm(&c.device.beats))
        ));
    }
    if c.differ.phrases {
        parts.push(tr!("phrases differ").to_owned());
    }
    parts.join(" · ")
}

/// The result line of UPDATE COLLECTION.
pub(super) fn updated_text(u: &update::Updated) -> String {
    let unmatched = if u.unmatched > 0 {
        tr!(
            "; {} track(s) of the device are not in the collection and were not updated",
            u.unmatched
        )
    } else {
        String::new()
    };
    tr!(
        "Update Collection: {} of {} track(s) changed ({} ratings, {} comments, {} colours, {} cue lists, {} grids){}",
        u.changed, u.matched, u.ratings, u.comments, u.colors, u.cues, u.grids, unmatched
    )
}

/// The result line of the review of an import from the device.
fn took_text(u: &update::Updated, label: &str) -> String {
    tr!(
        "Took the values of {} on {} track(s) ({} ratings, {} comments, {} cue lists, {} grids, {} keys); the others keep yours",
        label, u.changed, u.ratings, u.comments, u.cues, u.grids, u.keys
    )
}

/// The result line of IMPORT HISTORIES.
pub(super) fn imported_text(r: &history::Imported, label: &str) -> String {
    tr!(
        "Imported {} history(ies) of {}: {} plays, {} of unknown tracks skipped, {} imported before",
        r.histories, label, r.plays, r.skipped, r.already
    )
}

impl Ctx {
    /// A review of what UPDATE COLLECTION (`import`: an import from the device) would change on `d`: every row ticked.
    pub(super) fn open_update_review(&self, d: &Device, plan: update::Plan, import: bool) {
        let ticked = vec![true; plan.rows.len()];
        *self.review.borrow_mut() = Some(Review::Update {
            device: d.clone(),
            plan,
            ticked,
            import,
        });
        self.show_review();
    }

    /// A review of the histories `d` holds that were not imported yet: every row ticked.
    pub(super) fn open_history_review(&self, d: &Device, plan: history::Plan) {
        let shown = plan
            .items
            .iter()
            .enumerate()
            .filter(|(_, h)| !h.already)
            .map(|(i, _)| i)
            .collect();
        let ticked = vec![true; plan.items.len()];
        let delete_after = Behaviour::load(&self.store).delete_history_after_import;
        *self.review.borrow_mut() = Some(Review::Histories {
            device: d.clone(),
            plan,
            ticked,
            shown,
            delete_after,
        });
        self.show_review();
    }

    /// Opens the list of the tracks of the chosen device whose cues differ.
    fn open_differ(&self) {
        let Some(d) = self.current() else { return };
        *self.review.borrow_mut() = Some(Review::Differ(d.id));
        self.show_review();
    }

    fn open_recent(&self) {
        let ids = self
            .store
            .undo_entries()
            .unwrap_or_default()
            .into_iter()
            .map(|e| e.id)
            .collect();
        *self.review.borrow_mut() = Some(Review::Recent(ids));
        self.show_review();
    }

    /// Fills the dialog from [`Ctx::review`] (and closes it when nothing is left to show).
    pub(super) fn show_review(&self) {
        let (mode, title, text, rows, ticked, why, can_write) = match self.review.borrow().as_ref() {
            None => {
                self.with_state(|s| s.set_review_mode(0));
                return;
            }
            Some(Review::Update { device, plan, ticked, import }) => (
                1,
                if *import {
                    tr!("ALREADY IN YOUR COLLECTION: {}", device.label)
                } else {
                    tr!("UPDATE COLLECTION FROM {}", device.label)
                },
                if *import {
                    tr!(
                        "{} track(s) you imported from {} are already in your collection, with other cues, grid, key, rating or comment than on the device. Tick the tracks whose values you want to take from the device: each tick replaces yours. Unticked tracks, or CANCEL, keep yours. Nothing changes until you press APPLY, and UNDO afterwards restores exactly these fields.",
                        plan.rows.len(), device.label
                    )
                } else {
                    tr!(
                        "The player changed {} track(s) of your collection since the last export ({} more on the device are not in your collection and stay as they are). Tick what to bring into your collection: each tick replaces your value of that track with the player's. Nothing changes until you press APPLY, and UNDO afterwards restores exactly these fields.",
                        plan.rows.len(), plan.unmatched
                    )
                },
                plan.rows
                    .iter()
                    .zip(ticked)
                    .map(|(r, &t)| ReviewRow {
                        title: title_of(&r.title, &r.artist).into(),
                        detail: row_text(r).into(),
                        note: "".into(),
                        ticked: t,
                        enabled: true,
                    })
                    .collect::<Vec<_>>(),
                ticked.iter().filter(|&&t| t).count(),
                String::new(),
                false,
            ),
            Some(Review::Histories { device, plan, ticked, shown, delete_after }) => (
                2,
                tr!("IMPORT HISTORIES FROM {}", device.label),
                {
                    let base = tr!(
                        "{} history(ies) on the device are not in your play history yet. Importing adds their plays (tracks that are not in your collection are skipped); nothing changes until you press APPLY, and UNDO takes exactly those plays out again.",
                        shown.len()
                    );
                    if *delete_after {
                        format!("{base} {}", tr!("Preferences ask for the imported histories to be deleted from {} afterwards: UNDO does not bring them back to the device.", device.label))
                    } else {
                        base
                    }
                },
                shown
                    .iter()
                    .map(|&i| {
                        let h = &plan.items[i];
                        ReviewRow {
                            title: format!("{} · {}", h.day, h.name).into(),
                            detail: tr!("{} tracks, {} of them not in your collection", h.tracks, h.unknown).into(),
                            note: "".into(),
                            ticked: ticked[i],
                            enabled: true,
                        }
                    })
                    .collect(),
                shown.iter().filter(|&&i| ticked[i]).count(),
                String::new(),
                false,
            ),
            Some(Review::Differ(id)) => {
                let label = self.device(id).map_or_else(|| id.clone(), |d| d.label.clone());
                let list = self.conflicts.borrow();
                let list = list.get(id).map_or(&[][..], Vec::as_slice);
                let dev = self.device(id);
                let can_write = dev.as_ref().is_some_and(Device::writable);
                let why = match &dev {
                    Some(d) if !d.writable() => tr!("{} is read-only: KEEP MINE needs a device that can be written. TAKE THE STICK'S still works.", d.label),
                    None => tr!("That device is not mounted any more").to_owned(),
                    _ => String::new(),
                };
                (
                    3,
                    tr!("CUES THAT DIFFER ON {}", label),
                    tr_n!(
                        "{n} track has different cues or grid on {} than in your collection. Nothing was changed: neither copy wins by itself. KEEP MINE writes your collection's copy to the device, TAKE THE STICK'S copies the device's into your collection; UNDO stays available for both.",
                        "{n} tracks have different cues or grid on {} than in your collection. Nothing was changed: neither copy wins by itself. KEEP MINE writes your collection's copy to the device, TAKE THE STICK'S copies the device's into your collection; UNDO stays available for both.",
                        list.len(),
                        label
                    ),
                    list.iter()
                        .map(|c| ReviewRow {
                            title: title_of(&c.title, &c.artist).into(),
                            detail: differ_text(c).into(),
                            note: "".into(),
                            ticked: false,
                            enabled: true,
                        })
                        .collect(),
                    0,
                    why,
                    can_write,
                )
            }
            Some(Review::Recent(ids)) => {
                let entries = self.store.undo_entries().unwrap_or_default();
                (
                    4,
                    tr!("RECENT IMPORTS").to_owned(),
                    tr!("Each import and cue sync keeps the previous values of exactly the fields it changed. UNDO puts them back, except fields you edited since.").to_owned(),
                    ids.iter()
                        .filter_map(|id| entries.iter().find(|e| e.id == *id))
                        .map(|e| {
                            let n: Entry = serde_json::from_str(&e.data).unwrap_or_default();
                            let when = chrono::TimeZone::timestamp_opt(&chrono::Local, e.at, 0)
                                .earliest()
                                .map_or_else(String::new, |t| t.format("%Y-%m-%d %H:%M").to_string());
                            ReviewRow {
                                title: e.label.clone().into(),
                                detail: tr!("{}: {} field(s), {} play(s), {} device copy(ies)", when, n.changes.len(), n.plays.len(), n.restore.len()).into(),
                                note: "".into(),
                                ticked: false,
                                enabled: true,
                            }
                        })
                        .collect(),
                    0,
                    String::new(),
                    false,
                )
            }
        };
        self.models.review.set_vec(rows);
        self.with_state(|s| {
            s.set_review_title(title.as_str().into());
            s.set_review_text(text.as_str().into());
            s.set_review_ticked(ticked as i32);
            s.set_review_why(why.as_str().into());
            s.set_review_can_write(can_write && !s.get_busy());
            s.set_review_rows(ModelRc::from(self.models.review.clone()));
            s.set_review_mode(mode);
            s.set_visible(true);
        });
    }

    fn close_review(&self) {
        *self.review.borrow_mut() = None;
        self.with_state(|s| s.set_review_mode(0));
    }

    /// Row `i` of the ticked lists flips.
    fn toggle_row(&self, i: usize) {
        match self.review.borrow_mut().as_mut() {
            Some(Review::Update { ticked, .. }) => {
                if let Some(t) = ticked.get_mut(i) {
                    *t = !*t;
                }
            }
            Some(Review::Histories { ticked, shown, .. }) => {
                if let Some(t) = shown.get(i).and_then(|&j| ticked.get_mut(j)) {
                    *t = !*t;
                }
            }
            _ => return,
        }
        self.show_review();
    }

    fn tick_all(&self, on: bool) {
        match self.review.borrow_mut().as_mut() {
            Some(Review::Update { ticked, .. }) => ticked.iter_mut().for_each(|t| *t = on),
            Some(Review::Histories { ticked, shown, .. }) => {
                shown.iter().for_each(|&j| ticked[j] = on)
            }
            _ => return,
        }
        self.show_review();
    }

    /// APPLY: writes the ticked rows as a job, then offers UNDO.
    fn apply_review(self: &Rc<Self>) {
        let Some(review) = self.review.borrow_mut().take() else {
            return;
        };
        self.with_state(|s| s.set_review_mode(0));
        let (store, slot) = (
            self.store.clone(),
            std::sync::Arc::new(parking_lot::Mutex::new(None::<i64>)),
        );
        let done = slot.clone();
        match review {
            Review::Update {
                device,
                plan,
                ticked,
                import,
            } => {
                let id = device.id.clone();
                self.start_then(
                    false,
                    Some(id),
                    tr!("Updating the collection"),
                    move |_| {
                        let label = if import {
                            tr!("Import from {}", device.label)
                        } else {
                            tr!("Update Collection of {}", device.label)
                        };
                        let a = update::apply(&plan, &ticked, &device.id, &label, &store, now())?;
                        *done.lock() = a.undo;
                        Ok(if import { took_text(&a.updated, &device.label) } else { updated_text(&a.updated) })
                    },
                    move |cx, r| {
                        if let Ok(text) = r {
                            cx.offer_undo(slot.lock().take(), text);
                        }
                    },
                );
            }
            Review::Histories {
                device,
                plan,
                ticked,
                delete_after,
                ..
            } => {
                let (id, provider) = (device.id.clone(), device.provider());
                let config = openfader::store::device_settings::config(&self.store, &device.id);
                self.start_then(
                    true,
                    Some(id),
                    tr!("Importing histories"),
                    move |ctl| {
                        let label = tr!("Histories of {}", device.label);
                        let r = history::apply(&plan, &ticked, &device.id, &label, &store, now())?;
                        *done.lock() = r.undo;
                        let mut text = imported_text(&r, &device.label);
                        if delete_after && !r.done.is_empty() {
                            let deleted = provider
                                .ok_or_else(|| tr!("its plugin is not available").to_owned())
                                .and_then(|p| {
                                    history::delete(&p, &device.mount, &r.done, &config, &ctl)
                                });
                            match deleted {
                                Ok(n) => text.push_str(&tr!("; deleted {} from the device", n)),
                                Err(e) => {
                                    text.push_str(&tr!("; not deleted from the device: {}", e))
                                }
                            }
                        }
                        Ok(text)
                    },
                    move |cx, r| {
                        if let Ok(text) = r {
                            cx.offer_undo(slot.lock().take(), text);
                        }
                    },
                );
            }
            other => *self.review.borrow_mut() = Some(other),
        }
    }

    /// The track list shows the store's ratings, comments and cues again (an import or its undo changed them).
    pub(super) fn reload_browser(&self) {
        if let Some(app) = self.app() {
            app.global::<crate::app::BrowserState>().invoke_reload(-1);
        }
    }

    /// The bar with UNDO for the import `id` (nothing to offer when it changed nothing).
    pub(super) fn offer_undo(&self, id: Option<i64>, text: &str) {
        self.refresh_counts();
        let Some(id) = id else { return };
        self.reload_browser();
        self.undo_bar.set(id);
        self.with_state(|s| {
            s.set_undo_text(text.lines().next().unwrap_or(text).into());
            s.set_undo_shown(true);
        });
    }

    /// Keeps `entry` as the way back of the cue sync step `label` and offers UNDO for it.
    pub(super) fn keep_undo(&self, entry: &Entry, label: &str, device: &str) {
        match undo::keep(&self.store, label, device, entry, now()) {
            Ok(id) => self.offer_undo(id, label),
            Err(e) => eprintln!("cue sync: way back not kept: {e}"),
        }
    }

    /// Takes the import `id` back; says what came back and what was kept because it was edited since.
    fn undo_entry(self: &Rc<Self>, id: i64) {
        match undo::run(&self.store, id) {
            Ok(u) => {
                let mut text = tr!("Undone: {} field(s) put back", u.restored);
                if u.kept > 0 {
                    text.push_str(&tr!(
                        "; {} field(s) edited since were left as they are",
                        u.kept
                    ));
                }
                if u.plays > 0 {
                    text.push_str(&tr!("; {} play(s) removed from the play history", u.plays));
                }
                let mut waiting = 0;
                for r in u.restore {
                    if self
                        .links
                        .borrow()
                        .get(&r.device)
                        .is_some_and(|l| l.contains_key(&r.key))
                        && self.device(&r.device).is_some_and(|d| d.writable())
                    {
                        self.restores
                            .borrow_mut()
                            .insert((r.device.clone(), r.key.clone()), r.marks);
                        self.queue_push(&r.device, &r.key, None);
                        waiting += 1;
                    }
                }
                if waiting > 0 {
                    text.push_str(&tr!(
                        "; {} device copy(ies) are written back in a moment",
                        waiting
                    ));
                }
                self.say(&text, false);
                if let Some(app) = self.app() {
                    toast::show(&app, &text);
                }
            }
            Err(e) => self.say(&e, true),
        }
        if self.undo_bar.get() == id {
            self.undo_bar.set(0);
            self.with_state(|s| s.set_undo_shown(false));
        }
        self.refresh_counts();
        self.reload_browser();
        if matches!(self.review.borrow().as_ref(), Some(Review::Recent(_))) {
            self.open_recent();
        }
    }

    /// The counts the DEVICES tab shows for the chosen device: tracks whose cues differ, imports kept.
    pub(super) fn refresh_counts(&self) {
        let differ = self
            .current
            .borrow()
            .as_ref()
            .and_then(|id| self.conflicts.borrow().get(id).map(Vec::len))
            .unwrap_or(0);
        let kept = self.store.undo_entries().map_or(0, |e| e.len());
        self.with_state(|s| {
            s.set_differ_count(differ as i32);
            s.set_undo_count(kept as i32);
        });
    }

    /// The tracks of device `id` whose copies differ, as the last matching found them.
    pub(super) fn set_conflicts(&self, id: &str, list: Vec<Conflict>) {
        self.conflicts.borrow_mut().insert(id.to_owned(), list);
        self.refresh_counts();
        if matches!(self.review.borrow().as_ref(), Some(Review::Differ(d)) if d == id) {
            self.show_review();
        }
    }

    /// A loaded track's copies were settled: it is no conflict any more.
    pub(super) fn drop_conflict(&self, id: &str, key: &str) {
        if let Some(l) = self.conflicts.borrow_mut().get_mut(id) {
            l.retain(|c| c.key != key);
        }
        self.refresh_counts();
    }

    /// The user chose a side for the conflicts `keys` of device `id`: `keep_mine` writes the collection's copy to the device, else
    /// the device's marks are copied into the collection. The marks that are replaced are kept for UNDO.
    fn resolve(self: &Rc<Self>, id: &str, keys: &[String], keep_mine: bool) {
        let Some(d) = self.device_or_say(id) else {
            return;
        };
        if keep_mine && !self.writable(&d) {
            return;
        }
        let chosen: Vec<Conflict> = self
            .conflicts
            .borrow()
            .get(id)
            .map(|l| {
                l.iter()
                    .filter(|c| keys.contains(&c.key))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        let winner = if keep_mine {
            link::Winner::Collection
        } else {
            link::Winner::Device
        };
        let mut entry = Entry::default();
        for c in &chosen {
            let m = link::settle(&c.device, &c.collection, winner);
            if let Err(e) = link::pull_recorded(
                &self.store,
                &c.key,
                &m.to_collection,
                &c.collection.beats,
                &mut entry,
            ) {
                return self.say(&e, true);
            }
            if !m.to_device.is_empty() {
                entry
                    .restore
                    .extend(link::restore_of(id, &c.key, &m.lost_device));
                self.queue_push(id, &c.key, None);
            }
        }
        if let Some(l) = self.conflicts.borrow_mut().get_mut(id) {
            l.retain(|c| !keys.contains(&c.key));
        }
        let n = chosen.len();
        let text = if keep_mine {
            tr!(
                "Kept your cues and grid of {} track(s); {} gets them in a moment",
                n,
                d.label
            )
        } else {
            tr!(
                "Took the cues and grid of {} track(s) from {} into your collection",
                n,
                d.label
            )
        };
        self.keep_undo(&entry, &text, id);
        self.say(&text, false);
        self.show_review();
    }

    fn show_compare(&self, c: &Conflict, device: &str) {
        self.models.compare.set_vec(compare_rows(c));
        self.with_state(|s| {
            s.set_compare_title(
                tr!("COMPARE: {} ({})", title_of(&c.title, &c.artist), device)
                    .as_str()
                    .into(),
            );
            s.set_compare_rows(ModelRc::from(self.models.compare.clone()));
            s.set_compare_open(true);
        });
    }

    /// Row `row`'s button `act` (0 KEEP MINE / UNDO, 1 TAKE THE STICK'S, 2 COMPARE).
    fn row_action(self: &Rc<Self>, row: usize, act: i32) {
        enum Next {
            Resolve(String, String),
            Compare(Conflict, String),
            Undo(i64),
        }
        let next = match self.review.borrow().as_ref() {
            Some(Review::Differ(id)) => self
                .conflicts
                .borrow()
                .get(id)
                .and_then(|l| l.get(row))
                .map(|c| {
                    if act == 2 {
                        Next::Compare(
                            c.clone(),
                            self.device(id)
                                .map_or_else(|| id.clone(), |d| d.label.clone()),
                        )
                    } else {
                        Next::Resolve(id.clone(), c.key.clone())
                    }
                }),
            Some(Review::Recent(ids)) => ids.get(row).map(|&i| Next::Undo(i)),
            _ => None,
        };
        match next {
            Some(Next::Resolve(id, key)) => self.resolve(&id, &[key], act == 0),
            Some(Next::Compare(c, label)) => self.show_compare(&c, &label),
            Some(Next::Undo(id)) => self.undo_entry(id),
            None => {}
        }
    }

    fn bulk(self: &Rc<Self>, act: i32) {
        let Some(id) = (match self.review.borrow().as_ref() {
            Some(Review::Differ(id)) => Some(id.clone()),
            _ => None,
        }) else {
            return;
        };
        let keys: Vec<String> = self
            .conflicts
            .borrow()
            .get(&id)
            .map(|l| l.iter().map(|c| c.key.clone()).collect())
            .unwrap_or_default();
        if !keys.is_empty() {
            self.resolve(&id, &keys, act == 0);
        }
    }
}

pub(super) fn wire(cx: &Rc<Ctx>, app: &crate::app::App) {
    let s = app.global::<DeviceState>();
    s.set_review_rows(ModelRc::from(cx.models.review.clone()));
    s.set_compare_rows(ModelRc::from(cx.models.compare.clone()));
    let c = cx.clone();
    s.on_review_toggle(move |i| c.toggle_row(i.max(0) as usize));
    let c = cx.clone();
    s.on_review_all(move |on| c.tick_all(on));
    let c = cx.clone();
    s.on_review_apply(move || c.apply_review());
    let c = cx.clone();
    s.on_review_close(move || c.close_review());
    let c = cx.clone();
    s.on_review_row(move |i, act| c.row_action(i.max(0) as usize, act));
    let c = cx.clone();
    s.on_review_bulk(move |act| c.bulk(act));
    let c = cx.clone();
    s.on_compare_close(move || {
        c.with_state(|s| s.set_compare_open(false))
            .unwrap_or_default()
    });
    let c = cx.clone();
    s.on_undo_now(move || {
        let id = c.undo_bar.get();
        if id > 0 {
            c.undo_entry(id);
        }
    });
    let c = cx.clone();
    s.on_undo_dismiss(move || {
        c.undo_bar.set(0);
        c.with_state(|s| s.set_undo_shown(false));
    });
    let c = cx.clone();
    s.on_open_differ(move || c.open_differ());
    let c = cx.clone();
    s.on_open_recent(move || c.open_recent());
    cx.refresh_counts();
}

#[cfg(test)]
mod tests {
    use super::*;
    use openfader::analysis::Cue;

    fn cue(hot: u16, ms: u32) -> Cue {
        Cue {
            hot_cue: hot,
            time_ms: ms,
            loop_end_ms: None,
            color: None,
            comment: None,
        }
    }

    #[test]
    fn a_row_says_what_changes_in_plain_words() {
        let say = |d| diff_text(&d);
        assert_eq!(say(Diff::Rating { from: 3, to: 5 }), "rating 3 → 5");
        assert_eq!(
            say(Diff::Comment {
                from: String::new(),
                to: "warm up".into()
            }),
            "comment: warm up"
        );
        assert_eq!(
            say(Diff::Comment {
                from: "a".into(),
                to: String::new()
            }),
            "comment cleared"
        );
        assert_eq!(say(Diff::Color { from: None, to: 2 }), "colour none → Red");
        assert_eq!(
            say(Diff::Cues {
                added: 2,
                removed: 1,
                moved: 0
            }),
            "cues +2 / −1"
        );
        assert_eq!(
            say(Diff::Cues {
                added: 0,
                removed: 0,
                moved: 3
            }),
            "cues 3 moved"
        );
        assert_eq!(
            say(Diff::Cues {
                added: 0,
                removed: 0,
                moved: 0
            }),
            "cue details changed"
        );
        assert_eq!(
            say(Diff::Grid {
                moved_ms: 12,
                bpm: None
            }),
            "grid moved +12 ms"
        );
        assert_eq!(
            say(Diff::Grid {
                moved_ms: 0,
                bpm: Some((12000, 12400))
            }),
            "BPM 120.00 → 124.00"
        );
        assert_eq!(
            say(Diff::Grid {
                moved_ms: -5,
                bpm: Some((12000, 12400))
            }),
            "grid moved -5 ms, BPM 120.00 → 124.00"
        );
        assert_eq!(clip(&"word ".repeat(20)).chars().count(), 41);
    }

    #[test]
    fn the_compare_view_counts_and_times_the_cues_and_the_grid() {
        let mine = Marks {
            cues: vec![cue(1, 32_500), cue(0, 120_100)],
            beats: vec![Beat {
                bar_pos: 1,
                bpm_x100: 12800,
                time_ms: 320,
            }],
            phrases: None,
        };
        let stick = Marks {
            cues: vec![cue(2, 65_000)],
            ..Marks::default()
        };
        let c = Conflict {
            key: "t1".into(),
            title: "T".into(),
            artist: "A".into(),
            device: stick,
            collection: mine,
            differ: link::Differ {
                cues: true,
                beats: false,
                phrases: false,
            },
        };
        let rows = compare_rows(&c);
        let at = |i: usize| (rows[i].mine.to_string(), rows[i].stick.to_string());
        assert_eq!(
            at(0),
            (
                "1 hot, 1 memory, 0 loops".into(),
                "1 hot, 0 memory, 0 loops".into()
            )
        );
        assert_eq!(
            at(1),
            ("A 0:32.500, M 2:00.100".into(), "B 1:05.000".into())
        );
        assert_eq!(
            at(2),
            (
                "starts 0:00.320 at 128.00 BPM, 1 beats".into(),
                "none".into()
            )
        );
        assert_eq!(
            differ_text(&c),
            "cues: 2 in your collection, 1 on the stick"
        );
    }
}
