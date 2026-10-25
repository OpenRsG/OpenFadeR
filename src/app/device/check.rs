//! The stick checker in the DEVICES tab ([`openfader::device::check`]): a verdict for each mounted device, found in the background
//! when it is mounted, when something wrote to it and when the tab opens; the card at the top of the tab (READY / NEEDS FIXING /
//! UNREADABLE / EMPTY), DETAILS (libraries, what is wrong, the players, the booth profile), FIX THE STICK, the check before an
//! eject, and what is said when a device that a cut-off export left half-written is mounted.
//!
//! FIX THE STICK is one job: a backup of the library first (Preferences > DEVICE), finish or undo the interrupted change, each
//! plugin's own repair, the libraries written again from the collection when none can be repaired (after a question), the audio
//! files a library lists but the stick lacks copied again from the collection; then the stick is checked again.
//!
//! The words of every verdict are here ([`words`], [`why`], [`line`]) so the window, the toasts and `openfader verify` say the
//! same, translated.

use super::{App, CheckLib, CheckPlayer, CheckPreset, CheckProblem, Ctx, DeviceState, Pick, record_all, toast};
use openfader::{
    device::{
        self, Device,
        check::{self, Facts, Fit, Overall, Step, Verdict, Why},
        export::export,
    },
    plugin::device::{self as plugins, Control, ExportOptions, Problem, Provider, Severity, commit},
    store::{Store, device_settings::{self, Behaviour}},
};
use slint::{ComponentHandle, Model, ModelRc, VecModel};
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    path::{Path, PathBuf},
    rc::Rc,
    sync::{Arc, atomic::Ordering::Relaxed, mpsc},
};

/// What the checks of the window keep: the facts read from each device (a verdict is judged from them, so a change of the booth
/// profile needs no new look at the stick), the devices still to look at, and what an eject or a question is waiting for.
pub(super) struct State {
    tx: mpsc::Sender<(String, Facts)>,
    rx: mpsc::Receiver<(String, Facts)>,
    facts: RefCell<HashMap<String, Facts>>,
    running: RefCell<HashSet<String>>,
    /// Device ids to look at as soon as no job writes to them and no look is running.
    stale: RefCell<BTreeSet<String>>,
    /// The verdict last said out loud per device, so a toast comes when it changes, not every time it is looked at.
    told: RefCell<HashMap<String, Overall>>,
    /// The device an eject waits for the check of, and whether the user agreed to leave the loaded library.
    eject: RefCell<Option<(String, bool)>>,
    /// What the second answer of the open question does.
    alt: RefCell<Option<Box<dyn FnOnce(&Rc<Ctx>)>>>,
    /// The player models of the booth picker, ticked where the booth profile holds them.
    booth: Pick,
}

impl Default for State {
    fn default() -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            tx,
            rx,
            facts: RefCell::default(),
            running: RefCell::default(),
            stale: RefCell::default(),
            told: RefCell::default(),
            eject: RefCell::default(),
            alt: RefCell::default(),
            booth: Pick::default(),
        }
    }
}

/// "12 Oct 2026 21:03".
fn when(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0)
        .map(|t| t.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_default()
}

/// "FAT32", "exFAT", "HFS+", "NTFS": a kernel file system name as people say it.
fn fs_name(fs: &str) -> String {
    match fs {
        "vfat" | "msdos" | "fat" | "fat32" => "FAT32".into(),
        "exfat" => "exFAT".into(),
        "hfsplus" | "hfs" => "HFS+".into(),
        "ntfs" | "ntfs3" | "fuseblk" => "NTFS".into(),
        other => other.to_uppercase(),
    }
}

/// What a player cannot do with the stick, as one fragment that follows the model name ("cannot read exFAT").
pub(super) fn why(w: &Why) -> String {
    match w {
        Why::FileSystem(fs) => tr!("cannot read {}", fs_name(fs)),
        Why::Table(t) => tr!(
            "cannot read a {} partition table",
            if t == "dos" { "MBR".to_owned() } else { t.to_uppercase() }
        ),
        Why::NoLibrary(reads) => tr!("needs {}, this stick has none", reads.join(&format!(" {} ", tr!("or")))),
        Why::Unreadable(n) => tr!("cannot open the {} of this stick", n),
        Why::Damaged(n) => tr!("finds the {} of this stick damaged (see WHAT IS WRONG)", n),
    }
}

/// The line under a player's name: why not, or what the plugin knows of the model.
pub(super) fn reason(p: &check::PlayerFit) -> String {
    match &p.fit {
        Fit::No(w) => why(w),
        Fit::Unconfirmed if p.note.is_empty() => tr!("Not confirmed yet").to_owned(),
        Fit::Unconfirmed => tr!("Not confirmed: {}", p.note),
        Fit::Unknown => tr!("no extension knows this model").to_owned(),
        Fit::Ok => p.note.clone(),
    }
}

/// `(cause, consequence, action)` of a problem: a plugin's are its own (already translated); the host's are worded here.
pub(super) fn words(p: &Problem) -> (String, String, String) {
    let lib = if p.library.is_empty() {
        tr!("library").to_owned()
    } else {
        p.library.clone()
    };
    match p.id.as_str() {
        check::INTERRUPTED => (
            tr!("An export to this stick was cut off (it was pulled out or lost power while the library was written).").into(),
            tr!("A player could show an old library next to a new one.").into(),
            tr!("FIX THE STICK finishes or undoes it; your music is not touched.").into(),
        ),
        check::MISSING_AUDIO => (
            tr_n!(
                "{n} audio file that the {} lists is missing from this stick.",
                "{n} audio files that the {} lists are missing from this stick.",
                p.files.len(),
                lib
            ),
            tr!("Those tracks would not load on a player.").into(),
            tr!("FIX THE STICK copies them again from your collection.").into(),
        ),
        check::UNREADABLE => (
            tr!("The {} of this stick cannot be read ({}).", lib, p.cause),
            tr!("A player would show an empty stick.").into(),
            tr!("FIX THE STICK writes the library again from your collection, or restore a backup in BACKUPS. Your music is not touched.").into(),
        ),
        check::CHECK_FAILED => (
            tr!("Part of the check of this stick failed: {}", p.cause),
            tr!("Some problems may not have been found.").into(),
            tr!("Press CHECK AGAIN. If it keeps failing, update the extension.").into(),
        ),
        _ => (p.cause.clone(), p.consequence.clone(), p.action.clone()),
    }
}

pub(super) fn title(o: Overall) -> &'static str {
    match o {
        Overall::Ready => tr!("READY"),
        Overall::NeedsFixing => tr!("NEEDS FIXING"),
        Overall::Unreadable => tr!("UNREADABLE"),
        Overall::Empty => tr!("EMPTY"),
    }
}

/// The card's number for a verdict (`DeviceState.check-state`).
fn state(o: Overall) -> i32 {
    match o {
        Overall::Ready => 0,
        Overall::NeedsFixing => 1,
        Overall::Unreadable => 2,
        Overall::Empty => 3,
    }
}

/// What is wrong first, in a sentence: the worst problem's cause, else the first booth player that refuses the stick.
pub(super) fn first_cause(v: &Verdict) -> String {
    match (v.problems.first(), v.refused().next()) {
        (Some(p), _) if p.severity >= Severity::Error => words(p).0,
        (_, Some(r)) => match &r.fit {
            Fit::No(w) => tr!("{} {}.", r.model, why(w)),
            _ => String::new(),
        },
        (Some(p), None) => words(p).0,
        (None, None) => String::new(),
    }
}

/// The line under the big word of the card.
pub(super) fn line(v: &Verdict) -> String {
    let join = |a: String, b: String| if b.is_empty() { a } else { format!("{a} {b}") };
    match v.overall {
        Overall::Ready => {
            let (n, ok) = (v.players.len(), v.players.iter().filter(|p| p.fit == Fit::Ok).count());
            let players = match n {
                0 => String::new(),
                _ if ok == n => tr_n!("{n} player reads it.", "{n} players read it.", n),
                _ => tr!("{} of {} players read it.", ok, n),
            };
            let warn = v
                .problems
                .iter()
                .find(|p| p.severity >= Severity::Warning)
                .map(|p| words(p).0)
                .unwrap_or_default();
            join(join(tr!("Ready for a set.").into(), players), warn)
        }
        Overall::NeedsFixing | Overall::Unreadable => {
            let action = v.problems.first().map(|p| words(p).2).unwrap_or_default();
            let cause = first_cause(v);
            if cause.is_empty() {
                tr!("No library of this stick can be read.").into()
            } else if v.problems.first().is_some_and(|p| p.severity >= Severity::Error) {
                join(cause, action)
            } else {
                cause
            }
        }
        Overall::Empty => tr!("No library on this stick yet. Export to it to use it.").into(),
    }
}

/// What is said when a device that a cut-off export left half-written is mounted, and what was done about it (`None`: nothing to
/// say).
pub(super) fn recovery_text(label: &str, r: &Result<Option<commit::Recovered>, String>) -> Option<String> {
    match r {
        Ok(None) => None,
        Ok(Some(commit::Recovered::Finished { .. })) => Some(tr!(
            "The last export to {} was cut off. It was finished from what was already on the stick: the new library is complete.",
            label
        )),
        Ok(Some(commit::Recovered::RolledBack { .. })) => Some(tr!(
            "The last export to {} was cut off. Its previous library is in use again: export once more to bring it up to date.",
            label
        )),
        Err(e) => Some(tr!(
            "The last export to {} was cut off and could not be put right ({}). Open DEVICES and press FIX THE STICK.",
            label,
            e
        )),
    }
}

fn lib_row(l: &check::Lib) -> CheckLib {
    let n = |x: Option<usize>| x.map_or_else(|| "–".to_owned(), |n| n.to_string());
    let (written, state) = match (l.present, l.readable) {
        (false, _) => (tr!("not on this stick").to_owned(), 1),
        (true, false) => (tr!("cannot be read").to_owned(), 2),
        (true, true) => (l.modified.map_or_else(|| "–".to_owned(), when), 0),
    };
    CheckLib {
        name: l.name.as_str().into(),
        written: written.into(),
        tracks: n(l.tracks).into(),
        playlists: n(l.playlists).into(),
        cues: n(l.cues).into(),
        state,
    }
}

fn models<T: Clone + 'static>(v: Vec<T>) -> ModelRc<T> {
    ModelRc::from(Rc::new(VecModel::from(v)))
}

/// Model names as a set, for comparing booth profiles.
fn set(names: &[String]) -> BTreeSet<String> {
    names.iter().map(|n| n.to_lowercase()).collect()
}

impl Ctx {
    /// Marks device `id` to be looked at again as soon as nothing writes to it.
    pub(super) fn recheck(&self, id: &str) {
        self.check.stale.borrow_mut().insert(id.to_owned());
    }

    /// Whether device `id` has been looked at since it was mounted.
    pub(super) fn seen(&self, id: &str) -> bool {
        self.check.facts.borrow().contains_key(id)
    }

    /// A device was unplugged: what was known of it goes.
    pub(super) fn forget_check(&self, id: &str) {
        self.check.facts.borrow_mut().remove(id);
        self.check.told.borrow_mut().remove(id);
        self.check.stale.borrow_mut().remove(id);
    }

    /// The verdicts of the mounted devices, in list order, for the booth profile in Preferences.
    fn verdicts(&self) -> Vec<Option<Verdict>> {
        let booth = Behaviour::load(&self.store).booth_models();
        let facts = self.check.facts.borrow();
        self.devices
            .borrow()
            .iter()
            .map(|d| facts.get(&d.id).map(|f| check::judge(f, &booth)))
            .collect()
    }

    /// The verdict of device `id` now.
    fn verdict_of(&self, id: &str) -> Option<Verdict> {
        let booth = Behaviour::load(&self.store).booth_models();
        self.check.facts.borrow().get(id).map(|f| check::judge(f, &booth))
    }

    /// Puts the verdicts into the window: the chip of each device row, and the card and details of the chosen one.
    pub(super) fn show_check(&self) {
        let verdicts = self.verdicts();
        let at = self
            .current
            .borrow()
            .as_ref()
            .and_then(|id| self.devices.borrow().iter().position(|d| &d.id == id));
        let (chips, labels): (Vec<i32>, Vec<slint::SharedString>) = verdicts
            .iter()
            .map(|v| match v {
                Some(v) => (state(v.overall), title(v.overall).into()),
                None => (-1, tr!("CHECKING…").into()),
            })
            .unzip();
        let current = at.and_then(|i| verdicts.get(i)).cloned().flatten();
        let writable = at.is_some_and(|i| self.devices.borrow()[i].writable());
        let booth = Behaviour::load(&self.store).booth_models();
        let known = current.as_ref().map(|v| v.known.clone()).unwrap_or_default();
        let ticked = self.check.booth.ticked();
        if set(&ticked) != set(&booth) || self.check.booth.rows.row_count() != known.len() {
            let on = set(&booth);
            self.check.booth.set_names(&known, &|n| on.contains(&n.to_lowercase()));
        }
        self.with_state(|s| {
            s.set_device_checks(models(chips));
            s.set_device_check_labels(models(labels));
            s.set_booth_all(booth.is_empty());
            let Some(v) = &current else {
                s.set_check_state(-1);
                s.set_check_title(tr!("CHECKING…").into());
                s.set_check_line(tr!("Looking at the stick…").into());
                s.set_check_libs(models(Vec::new()));
                s.set_check_problems(models(Vec::new()));
                s.set_check_players(models(Vec::new()));
                s.set_check_presets(models(Vec::new()));
                s.set_fixable(false);
                return;
            };
            s.set_check_state(state(v.overall));
            s.set_check_title(title(v.overall).into());
            s.set_check_line(line(v).into());
            s.set_check_libs(models(v.libs.iter().map(lib_row).collect()));
            s.set_check_problems(models(
                v.problems
                    .iter()
                    .map(|p| {
                        let (cause, consequence, action) = words(p);
                        CheckProblem {
                            severity: p.severity as i32,
                            cause: cause.into(),
                            consequence: consequence.into(),
                            action: action.into(),
                        }
                    })
                    .collect(),
            ));
            s.set_check_players(models(
                v.players
                    .iter()
                    .map(|p| CheckPlayer {
                        model: p.model.as_str().into(),
                        fit: match p.fit {
                            Fit::Ok => 0,
                            Fit::Unconfirmed => 1,
                            Fit::No(_) => 2,
                            Fit::Unknown => 3,
                        },
                        reason: reason(p).into(),
                    })
                    .collect(),
            ));
            let on = set(&booth);
            s.set_check_presets(models(
                v.presets
                    .iter()
                    .map(|b| CheckPreset {
                        id: b.id.as_str().into(),
                        label: b.label.as_str().into(),
                        on: !booth.is_empty() && set(&b.models) == on,
                    })
                    .collect(),
            ));
            s.set_fixable(writable && !v.steps().is_empty());
        });
    }

    /// The timer: takes the finished looks, starts the ones that are due.
    pub(super) fn run_checks(self: &Rc<Self>) {
        while let Ok((id, facts)) = self.check.rx.try_recv() {
            self.check.running.borrow_mut().remove(&id);
            self.check.facts.borrow_mut().insert(id.clone(), facts);
            self.checked(&id);
        }
        let due: Vec<String> = self.check.stale.borrow().iter().cloned().collect();
        for id in due {
            let Some(dev) = self.device(&id) else {
                self.check.stale.borrow_mut().remove(&id);
                continue;
            };
            let writing = self
                .job
                .borrow()
                .as_ref()
                .is_some_and(|j| j.device.as_deref() == Some(id.as_str()));
            if writing || self.check.running.borrow().contains(&id) {
                continue;
            }
            self.check.stale.borrow_mut().remove(&id);
            self.check.running.borrow_mut().insert(id.clone());
            let (tx, config) = (
                self.check.tx.clone(),
                device_settings::config(&self.store, &id),
            );
            std::thread::spawn(move || drop(tx.send((id, check::gather(&dev, &config)))));
        }
    }

    /// A look at device `id` finished: show it, say it when the stick needs attention, go on with an eject that waited for it.
    fn checked(self: &Rc<Self>, id: &str) {
        self.show_check();
        let (Some(d), Some(v)) = (self.device(id), self.verdict_of(id)) else {
            return;
        };
        let before = self.check.told.borrow_mut().insert(id.to_owned(), v.overall);
        if before != Some(v.overall)
            && matches!(v.overall, Overall::NeedsFixing | Overall::Unreadable)
            && let Some(app) = self.app()
        {
            toast::show(
                &app,
                &tr!("{} needs fixing: {} Open DEVICES to fix it.", d.label, first_cause(&v)),
            );
        }
        let waiting = self.check.eject.borrow().clone();
        if let Some((w, confirmed)) = waiting
            && w == id
        {
            self.check.eject.borrow_mut().take();
            if matches!(v.overall, Overall::Ready | Overall::Empty) {
                return self.eject_from(&d, confirmed, true);
            }
            let cause = first_cause(&v);
            let text = tr!(
                "{} needs fixing: {} Eject anyway?",
                d.label,
                if cause.is_empty() { title(v.overall).to_owned() } else { cause }
            );
            let (anyway, fixing) = (d.clone(), d.clone());
            self.confirm(&text, tr!("EJECT ANYWAY"), move |c| {
                c.eject_from(&anyway, confirmed, true)
            });
            self.confirm_alt(tr!("FIX THE STICK"), move |c| c.fix(&fixing, false));
        }
    }

    /// Adds a second answer to the question just asked with [`Ctx::confirm`].
    fn confirm_alt(&self, label: &str, then: impl FnOnce(&Rc<Ctx>) + 'static) {
        *self.check.alt.borrow_mut() = Some(Box::new(then));
        self.with_state(|s| s.set_confirm_alt(label.into()));
    }

    /// The question was closed: forget its second answer.
    pub(super) fn drop_alt(&self) {
        self.check.alt.borrow_mut().take();
        self.with_state(|s| s.set_confirm_alt("".into()));
    }

    /// Looks at `dev` again from scratch (CHECK AGAIN, the tab opening, an eject).
    pub(super) fn check_now(self: &Rc<Self>, dev: &Device) {
        device::forget(&dev.mount);
        self.check.facts.borrow_mut().remove(&dev.id);
        self.refresh_devices();
        self.recheck(&dev.id);
        self.show_check();
    }

    /// Ejects only after a look at the stick: with the check on, a device that needs fixing asks first.
    pub(super) fn check_before_eject(self: &Rc<Self>, dev: &Device, confirmed: bool) {
        *self.check.eject.borrow_mut() = Some((dev.id.clone(), confirmed));
        self.say(&tr!("Checking {} before it is ejected…", dev.label), false);
        self.check_now(dev);
    }

    /// FIX THE STICK on `dev` (`confirmed`: the user agreed to rebuild a library from the collection).
    pub(super) fn fix(self: &Rc<Self>, dev: &Device, confirmed: bool) {
        if self.busy() {
            return self.say(crate::app::i18n::tr_dyn(super::BUSY), true);
        }
        let Some(v) = self.verdict_of(&dev.id) else {
            return self.say(tr!("Wait for the check to finish, then try again."), true);
        };
        let steps = v.steps();
        if steps.is_empty() {
            return self.say(&tr!("Nothing on {} that this app can repair.", dev.label), true);
        }
        if !self.writable(dev) {
            return;
        }
        let rebuild = steps.contains(&Step::Rebuild);
        let ticked = self.store.sync_playlists(&dev.id).unwrap_or_default();
        if rebuild && !confirmed {
            let text = if ticked.is_empty() {
                tr!("Rebuild the library on {} from your collection: the whole collection?", dev.label)
            } else {
                tr!(
                    "Rebuild the library on {} from your collection: the playlists ticked in SYNC MANAGER?",
                    dev.label
                )
            };
            let d = dev.clone();
            return self.confirm(&text, tr!("REBUILD"), move |c| c.fix(&d, true));
        }
        let needs_collection = steps.iter().any(|s| matches!(s, Step::Recopy(_) | Step::Rebuild));
        let snap = if needs_collection {
            match self.snapshot() {
                Some(s) => Some(s),
                None => return,
            }
        } else {
            None
        };
        let writers = if rebuild {
            match self.writers(dev) {
                Some(w) => w,
                None => return,
            }
        } else {
            Vec::new()
        };
        let job = Fix {
            dev: dev.clone(),
            steps,
            snap,
            writers,
            options: self.options(Some(dev)),
            config: device_settings::config(&self.store, &dev.id),
            ticked,
            safety: self.safety(),
            store: self.store.clone(),
        };
        let id = dev.id.clone();
        self.start_then(
            Some(dev.id.clone()),
            tr!("Fixing the stick"),
            move |ctl| job.run(&ctl),
            move |cx, _| {
                cx.check.facts.borrow_mut().remove(&id);
                cx.recheck(&id);
                cx.relink(&id);
            },
        );
    }

    /// A device appeared: finishes or undoes an export a pulled stick cut off, before anything else touches the device, says what
    /// happened, and queues the first look.
    pub(super) fn on_mounted(&self, d: &Device) {
        if commit::pending(&d.mount).is_some() || d.mount.join(commit::JOURNAL).is_file() {
            let r = if d.writable() {
                commit::recover(&d.mount)
            } else {
                Err(tr!("it is mounted read-only").to_owned())
            };
            if let (Some(text), Some(app)) = (recovery_text(&d.label, &r), self.app()) {
                toast::show(&app, &text);
            }
            device::forget(&d.mount);
        }
        self.recheck(&d.id);
    }

    /// The callbacks of the card and the booth picker.
    pub(super) fn wire_check(cx: &Rc<Self>, app: &App) {
        let s = app.global::<DeviceState>();
        s.set_booth_rows(ModelRc::from(cx.check.booth.rows.clone()));
        let c = cx.clone();
        s.on_check_again(move || c.on_current(|c, d| c.check_now(d)));
        let c = cx.clone();
        s.on_fix_stick(move || c.on_current(|c, d| c.fix(d, false)));
        let c = cx.clone();
        s.on_booth_toggle(move |i| {
            if c.check.booth.toggle(i.max(0) as usize).is_some() {
                c.save_booth(c.check.booth.ticked());
            }
        });
        let c = cx.clone();
        s.on_booth_everything(move || c.save_booth(Vec::new()));
        let c = cx.clone();
        s.on_booth_preset(move |id| {
            let models = c
                .current()
                .and_then(|d| c.verdict_of(&d.id))
                .and_then(|v| v.presets.into_iter().find(|p| p.id == id.as_str()))
                .map(|p| p.models)
                .unwrap_or_default();
            c.save_booth(models);
        });
        let c = cx.clone();
        s.on_confirmed_alt(move || {
            let then = c.check.alt.borrow_mut().take();
            c.with_state(|s| {
                s.set_confirm_open(false);
                s.set_confirm_alt("".into());
            });
            c.ask.borrow_mut().take();
            if let Some(then) = then {
                then(&c);
            }
        });
    }

    fn save_booth(&self, models: Vec<String>) {
        if let Err(e) = Behaviour::set(&self.store, "booth", &models.join(",")) {
            return self.say(&e, true);
        }
        self.show_check();
    }
}

/// Everything FIX THE STICK needs, moved to its worker.
struct Fix {
    dev: Device,
    steps: Vec<Step>,
    snap: Option<openfader::library::snapshot::Snapshot>,
    writers: Vec<Provider>,
    options: ExportOptions,
    config: BTreeMap<String, String>,
    /// Playlists ticked in SYNC MANAGER: a rebuild writes these, else the whole collection.
    ticked: Vec<String>,
    safety: super::safety::Safety,
    store: Arc<Store>,
}

/// `dev` as it is now: the plugins asked again.
fn fresh(dev: &Device) -> Device {
    device::forget(&dev.mount);
    device::scan()
        .into_iter()
        .find(|d| d.id == dev.id)
        .unwrap_or_else(|| dev.clone())
}

impl Fix {
    fn run(self, ctl: &Arc<Control>) -> Result<String, String> {
        let dev = &self.dev;
        let mut said: Vec<String> = Vec::new();
        if let Some(l) = self.safety.backup_first(dev, ctl)? {
            said.push(l);
        }
        ctl.set_phase(tr!("Finishing what was cut off"));
        match commit::recover(&dev.mount) {
            Ok(r) => said.extend(recovery_text(&dev.label, &Ok(r))),
            Err(e) => return Err(tr!("{}: the cut-off export could not be put right ({}).", dev.label, e)),
        }
        for s in &self.steps {
            let Step::Plugin { plugin, fix } = s else {
                continue;
            };
            let p = plugins::provider(plugin)
                .ok_or_else(|| tr!("{}: the extension {} is switched off. Switch it on in PREFERENCES > EXTENSIONS.", dev.label, plugin))?;
            ctl.set_phase(&tr!("Repairing ({})", plugin));
            let a = p.fix(&dev.mount, fix, &self.config, ctl)?;
            said.extend(Some(a.message).filter(|m| !m.is_empty()));
        }
        let mut left = check::check(&fresh(dev), &self.config, &[]).steps();
        if left.contains(&Step::Rebuild) && self.steps.contains(&Step::Rebuild) {
            said.push(self.rebuild(ctl)?);
            left = check::check(&fresh(dev), &self.config, &[]).steps();
        }
        if let Some(Step::Recopy(files)) = left.iter().find(|s| matches!(s, Step::Recopy(_))) {
            said.push(self.recopy(files, ctl)?);
        }
        let v = check::check(&fresh(dev), &self.config, &[]);
        let details = said.join("\n");
        if matches!(v.overall, Overall::Ready | Overall::Empty) {
            Ok(format!("{}\n{details}", tr!("{} is ready.", dev.label)).trim_end().to_owned())
        } else {
            Err(format!(
                "{}\n{details}",
                tr!("{} still needs attention: {}", dev.label, first_cause(&v))
            )
            .trim_end()
            .to_owned())
        }
    }

    /// The libraries written again from the collection (the ticked playlists, else all of it), in every format picked.
    fn rebuild(&self, ctl: &Arc<Control>) -> Result<String, String> {
        let snap = self.snap.as_ref().ok_or("no collection")?;
        let ids: Vec<&str> = self.ticked.iter().map(String::as_str).collect();
        let narrowed = (!ids.is_empty()).then(|| snap.narrowed(&ids)).flatten();
        ctl.set_phase(tr!("Writing the library again"));
        let done = export(
            &self.writers,
            &self.dev.mount,
            narrowed.as_ref().unwrap_or(snap),
            &self.options,
            ctl,
        )?;
        record_all(&self.store, Some(&self.dev), &done, &self.dev.label)
    }

    /// Copies the audio files `files` (relative to the root) from the collection: the file of the same name, as the stick's library
    /// lists it. Files the collection does not have are named in the answer.
    fn recopy(&self, files: &[String], ctl: &Arc<Control>) -> Result<String, String> {
        let snap = self.snap.as_ref().ok_or("no collection")?;
        let mut by_name: HashMap<String, PathBuf> = HashMap::new();
        for (t, _) in snap.tracks() {
            let Some(f) = t.file.as_ref().filter(|f| !f.starts_with(&self.dev.mount) && f.is_file()) else {
                continue;
            };
            let name = f.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
            let clean = openfader_plugin::library::clean_name(&name, "track");
            by_name.entry(name).or_insert_with(|| f.clone());
            by_name.entry(clean).or_insert_with(|| f.clone());
        }
        let found: Vec<(&String, &PathBuf)> = files
            .iter()
            .filter_map(|rel| {
                let name = rel.rsplit('/').next().unwrap_or(rel).to_lowercase();
                Some((rel, by_name.get(&name)?))
            })
            .collect();
        ctl.set_phase(tr!("Copying missing music"));
        ctl.start(found.len());
        ctl.bytes_total.store(
            found.iter().filter_map(|(_, s)| std::fs::metadata(s).ok()).map(|m| m.len()).sum(),
            Relaxed,
        );
        for (rel, src) in &found {
            copy_audio(src, &self.dev.mount.join(rel), ctl)?;
            ctl.done.fetch_add(1, Relaxed);
        }
        let lost = files.len() - found.len();
        Ok(if lost == 0 {
            tr_n!("Copied {n} missing audio file again from your collection.", "Copied {n} missing audio files again from your collection.", found.len())
        } else {
            tr!(
                "Copied {} of {} missing audio files again. {} are not in your collection: copy them to the stick by hand or take them off its playlists.",
                found.len(),
                files.len(),
                lost
            )
        })
    }
}

/// Copies `src` to `to`, which does not exist: into a `.part` file that is flushed to the stick, checked against the source's
/// size and renamed, so a copy that is cut short never looks whole. The bytes count in the job's progress.
fn copy_audio(src: &Path, to: &Path, ctl: &Control) -> Result<(), String> {
    use std::io::{Read, Write};
    let err = |p: &Path| {
        let p = p.display().to_string();
        move |e: std::io::Error| format!("{p}: {e}")
    };
    if let Some(dir) = to.parent() {
        std::fs::create_dir_all(dir).map_err(err(dir))?;
    }
    let tmp = to.with_extension("part");
    let copy = || -> Result<(), String> {
        let mut from = std::fs::File::open(src).map_err(err(src))?;
        let mut out = std::fs::File::create(&tmp).map_err(err(&tmp))?;
        let mut buf = vec![0; 1 << 20];
        let mut total = 0u64;
        loop {
            let n = from.read(&mut buf).map_err(err(src))?;
            if n == 0 {
                break;
            }
            out.write_all(&buf[..n]).map_err(err(&tmp))?;
            total += n as u64;
            ctl.bytes_done.fetch_add(n as u64, Relaxed);
            if ctl.cancelled() {
                return Err("cancelled".into());
            }
        }
        out.sync_all().map_err(err(&tmp))?;
        if std::fs::metadata(&tmp).map_err(err(&tmp))?.len() != total {
            return Err(tr!("{}: the copy is shorter than the original", to.display()));
        }
        std::fs::rename(&tmp, to).map_err(err(to))
    };
    copy().inspect_err(|_| drop(std::fs::remove_file(&tmp)))?;
    if let Some(dir) = to.parent() {
        commit::sync_dir(dir);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use openfader::plugin::device::{BoothPreset, Player};

    fn verdict(f: &Facts, booth: &[&str]) -> Verdict {
        check::judge(f, &booth.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    fn healthy() -> Facts {
        Facts {
            fs: "vfat".into(),
            table: "dos".into(),
            libs: vec![check::Lib {
                name: "OneLibrary".into(),
                present: true,
                readable: true,
                tracks: Some(5),
                ..Default::default()
            }],
            players: vec![
                Player {
                    model: "CDJ-3000X".into(),
                    reads: vec!["OneLibrary".into()],
                    file_systems: vec!["vfat".into()],
                    status: "confirmed".into(),
                    ..Default::default()
                },
                Player {
                    model: "CDJ-2000NXS2".into(),
                    reads: vec!["Device Library".into()],
                    file_systems: vec!["vfat".into(), "hfsplus".into()],
                    status: "confirmed".into(),
                    ..Default::default()
                },
            ],
            presets: vec![BoothPreset {
                id: "club".into(),
                label: "Club".into(),
                models: vec!["CDJ-3000X".into()],
            }],
            ..Default::default()
        }
    }

    #[test]
    fn every_no_has_a_sentence_that_names_the_cause() {
        assert_eq!(why(&Why::FileSystem("exfat".into())), "cannot read exFAT");
        assert_eq!(why(&Why::Table("dos".into())), "cannot read a MBR partition table");
        assert_eq!(
            why(&Why::NoLibrary(vec!["OneLibrary".into()])),
            "needs OneLibrary, this stick has none"
        );
        assert_eq!(
            why(&Why::NoLibrary(vec!["OneLibrary".into(), "Device Library".into()])),
            "needs OneLibrary or Device Library, this stick has none"
        );
        let v = verdict(&healthy(), &[]);
        let r: Vec<String> = v.players.iter().map(reason).collect();
        assert_eq!(r[0], "");
        assert_eq!(r[1], "needs Device Library, this stick has none");
        let mut f = healthy();
        f.players[0].status = "to_measure".into();
        f.players[0].note = "firmware 1.2".into();
        assert_eq!(reason(&verdict(&f, &[]).players[0]), "Not confirmed: firmware 1.2");
    }

    #[test]
    fn the_card_says_what_matters_first() {
        let mut f = healthy();
        assert_eq!(line(&verdict(&f, &["CDJ-3000X"])), "Ready for a set. 1 player reads it.");
        assert_eq!(line(&verdict(&f, &[])), "Ready for a set. 1 of 2 players read it.");
        // A booth player that cannot read it is the first thing said.
        assert_eq!(line(&verdict(&f, &["CDJ-2000NXS2"])), "CDJ-2000NXS2 needs Device Library, this stick has none.");
        f.problems.push(Problem {
            id: check::INTERRUPTED.into(),
            severity: Severity::Error,
            ..Default::default()
        });
        let v = verdict(&f, &[]);
        assert_eq!(v.overall, Overall::NeedsFixing);
        assert!(line(&v).starts_with("An export to this stick was cut off"), "{}", line(&v));
        assert!(line(&v).ends_with("your music is not touched."));
        f.libs.clear();
        f.problems.clear();
        assert_eq!(line(&verdict(&f, &[])), "No library on this stick yet. Export to it to use it.");
    }

    #[test]
    fn host_problems_are_worded_and_a_plugins_are_left_alone() {
        let gone = Problem {
            id: check::MISSING_AUDIO.into(),
            library: "OneLibrary".into(),
            files: vec!["a.mp3".into(), "b.mp3".into()],
            ..Default::default()
        };
        assert_eq!(words(&gone).0, "2 audio files that the OneLibrary lists are missing from this stick.");
        let theirs = Problem {
            id: "p:x".into(),
            cause: "c".into(),
            consequence: "q".into(),
            action: "a".into(),
            ..Default::default()
        };
        assert_eq!(words(&theirs), ("c".into(), "q".into(), "a".into()));
    }

    #[test]
    fn a_cut_off_export_is_told_by_what_recovery_did() {
        let done = |r| recovery_text("STICK", &r);
        assert_eq!(done(Ok(None)), None);
        assert!(done(Ok(Some(commit::Recovered::Finished { owner: "p".into() })))
            .unwrap()
            .contains("was finished"));
        assert!(done(Ok(Some(commit::Recovered::RolledBack { owner: "p".into() })))
            .unwrap()
            .contains("previous library is in use again"));
        assert!(done(Err("read-only".into())).unwrap().contains("press FIX THE STICK"));
    }

    #[test]
    fn a_missing_audio_file_is_copied_whole_or_not_at_all() {
        let dir = std::env::temp_dir().join(format!("of-fix-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/a.mp3"), vec![7u8; 3_000_000]).unwrap();
        let ctl = Control::default();
        copy_audio(&dir.join("src/a.mp3"), &dir.join("stick/Contents/A/B/a.mp3"), &ctl).unwrap();
        assert_eq!(std::fs::read(dir.join("stick/Contents/A/B/a.mp3")).unwrap().len(), 3_000_000);
        assert_eq!(ctl.bytes_done.load(Relaxed), 3_000_000);
        assert!(!dir.join("stick/Contents/A/B/a.part").exists());
        // A cancel leaves no part.
        ctl.cancel();
        assert_eq!(
            copy_audio(&dir.join("src/a.mp3"), &dir.join("stick/b.mp3"), &ctl),
            Err("cancelled".into())
        );
        assert!(!dir.join("stick/b.mp3").exists() && !dir.join("stick/b.part").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
