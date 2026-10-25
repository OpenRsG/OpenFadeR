//! FORMAT… on the DEVICES tab ([`openfader::device::format`]): reading the disk the chosen device is on (a job: Windows asks
//! PowerShell), the dialog (ui/device-format.slint: what will be erased, file system, label, backup first, the typed word),
//! then the format as a job, and the new device selected when it appears. The safeguards are the library's ([`format::check`],
//! run again by [`format::format`]); the dialog shows what they refuse and nothing is erased until they pass. This file only
//! turns their answers into text: cause, consequence, action.

use super::{backup, show_panel, App, Ctx, DeviceState, BUSY};
use openfader::{
    device::{
        self,
        format::{self, Disk, Done, Error, Fs, Request, Spec, Step, Volume},
        Device, Kind,
    },
    plugin::device::Feature,
};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use std::{
    rc::Rc,
    sync::{Arc, Mutex},
};

/// The marker a finished job puts first in its text when the text is data for the next step, not for the status line.
const DATA: char = '\u{1}';

pub(super) fn wire(cx: &Rc<Ctx>, app: &App) {
    let s = app.global::<DeviceState>();
    let c = cx.clone();
    s.on_format_ask(move || c.on_current(ask));
    let c = cx.clone();
    s.on_format_go(move || {
        let then = c.ask.borrow_mut().take();
        if let Some(then) = then {
            then(&c);
        }
    });
    let c = cx.clone();
    s.on_format_cancel(move || {
        c.ask.borrow_mut().take();
        c.with_state(|s| s.set_format_open(false));
    });
}

/// What the progress line says during step `s`.
fn phase(s: Step) -> &'static str {
    match s {
        Step::Read => tr!("Reading the disk"),
        Step::Unmount => tr!("Unmounting"),
        Step::Table => tr!("Erasing the disk"),
        Step::Partition => tr!("Formatting"),
        Step::Mount => tr!("Mounting"),
    }
}

fn fs_name(fs: Fs) -> &'static str {
    match fs {
        Fs::Fat32 => "FAT32",
        Fs::ExFat => "exFAT",
    }
}

/// The text for what stopped a format of the device called `label`: what happened, what it means for the data, what to do.
fn explain(e: &Error, label: &str) -> String {
    match e {
        Error::NoDisk => tr!("{} is not on a disk this app can format. Nothing was erased. Choose a USB stick or an SD card.", label),
        Error::System => tr!("{} is on the disk this computer runs from. Nothing was erased. Only USB sticks and SD cards can be formatted here.", label),
        Error::NotRemovable => tr!("{} is on a disk that is not removable. Nothing was erased. Only USB sticks and SD cards can be formatted here.", label),
        Error::ReadOnly => tr!("{} is write-protected or mounted read-only. Nothing was erased. Slide the lock switch of the card to unlocked or plug it in again, then try again.", label),
        Error::JobBusy => tr!("A job is writing to {}. Nothing was erased. Wait for it or cancel it (X), then format.", label),
        Error::Unseen(v) => tr!("The disk holds a volume ({}) that was not listed when you confirmed. Nothing was erased. Press FORMAT… again to see everything on the disk.", v),
        Error::Confirm(word) => tr!("The confirmation does not match. Nothing was erased. Type {} exactly as shown.", word),
        Error::ExFatOnSd => tr!("Players refuse exFAT SD cards. Nothing was erased. Choose FAT32 for the card.").to_owned(),
        Error::TooBig => tr!("This disk is larger than 2 TB, the most an MBR partition table holds. Nothing was erased. Use a smaller stick, or format it with the disk tool of your system.").to_owned(),
        Error::Label(fs) => tr!("The label is not valid for {}: use up to 11 letters or digits, no punctuation. Nothing was erased. Type a simpler label.", fs_name(*fs)),
        Error::Denied { erased: false } => tr!("The system did not allow formatting. Nothing was erased. Try again and enter your password when asked.").to_owned(),
        Error::Denied { erased: true } => tr!("The system stopped allowing the format after the disk was erased, so it has no usable volume yet. Press FORMAT… again and enter your password when asked.").to_owned(),
        Error::InUse(_) => tr!("{} is in use by another program. Nothing was erased. Close what uses it, then try again.", label),
        Error::Missing(tool) => tr!("The tool {} is not available. Nothing was erased. Install it (on Linux: udisks2) and try again.", tool),
        Error::Failed { step, detail, erased } => {
            let step = match step {
                Step::Read => tr!("reading the disk"),
                Step::Unmount => tr!("unmounting it"),
                Step::Table => tr!("writing the partition table"),
                Step::Partition => tr!("making the file system"),
                Step::Mount => tr!("mounting it"),
            };
            if *erased {
                tr!("Formatting {} stopped while {}: {}. The disk was erased and has no usable volume yet. Press FORMAT… again to finish.", label, step, detail)
            } else {
                tr!("Formatting {} stopped while {}: {}. Nothing was erased. Try again.", label, step, detail)
            }
        }
    }
}

/// "FAT32" for `vfat` and the like.
fn kind_of(fs: &str) -> String {
    match fs {
        "" => tr!("no file system").to_owned(),
        "vfat" | "msdos" | "fat" => "FAT".into(),
        "exfat" => "exFAT".into(),
        other => other.to_uppercase(),
    }
}

/// One line of the confirmation: "MY KEY · FAT32 · 14.9 GB · /run/media/dj/MY KEY".
fn volume_line(v: &Volume) -> String {
    let label = if v.label.is_empty() {
        tr!("no label")
    } else {
        v.label.as_str()
    };
    let mut line = format!("{label} · {} · {}", kind_of(&v.fs), device::size(v.size));
    if let Some(m) = &v.mount {
        line += &format!(" · {}", m.display());
    }
    line
}

/// FORMAT… pressed on `dev`: reads its disk (in a job), then opens the dialog.
fn ask(cx: &Rc<Ctx>, dev: &Device) {
    if cx.busy() {
        return cx.say(crate::app::i18n::tr_dyn(BUSY), true);
    }
    let d = dev.clone();
    let shown = dev.clone();
    cx.start_then(
        Some(dev.id.clone()),
        phase(Step::Read),
        move |_| {
            let disk = format::describe(&d).map_err(|e| explain(&e, &d.label))?;
            serde_json::to_string(&disk)
                .map(|j| format!("{DATA}{j}"))
                .map_err(|e| e.to_string())
        },
        move |cx, r| {
            if let Some(disk) = r
                .as_ref()
                .ok()
                .and_then(|j| serde_json::from_str(j.trim_start_matches(DATA)).ok())
            {
                open(cx, shown, disk);
            }
        },
    );
}

/// Shows the dialog for `dev` on `disk`; its answer comes back through [`go`].
fn open(cx: &Rc<Ctx>, dev: Device, disk: Disk) {
    let word = disk.word(&dev);
    let label = Some(word.as_str())
        .filter(|w| *w != "ERASE")
        .map(|w| Spec {
            fs: Fs::Fat32,
            label: w.to_owned(),
        })
        .and_then(|s| s.label().ok())
        .unwrap_or_else(|| "DJ USB".into());
    let model = if disk.model.is_empty() {
        tr!("Disk").to_owned()
    } else {
        disk.model.clone()
    };
    let lines: Vec<SharedString> = disk.volumes.iter().map(|v| volume_line(v).into()).collect();
    let can_backup = dev.can(Feature::Backup) && dev.has_library();
    cx.with_state(|s| {
        s.set_format_warning(tr!("Everything on this disk will be erased: the music, the libraries and every volume listed here. This cannot be undone.").into());
        s.set_format_disk(format!("{model} · {} · {}", device::size(disk.size), disk.id).into());
        s.set_format_volumes(ModelRc::from(Rc::new(VecModel::from(lines))));
        s.set_format_word(word.as_str().into());
        s.set_format_typed("".into());
        s.set_format_label(label.as_str().into());
        s.set_format_fs(0);
        s.set_format_exfat(dev.kind != Kind::Sd);
        s.set_format_can_backup(can_backup);
        s.set_format_backup(can_backup);
        s.set_format_problem("".into());
        s.set_format_open(true);
        s.set_visible(true);
    });
    install(cx, dev, disk);
}

/// The dialog's "ERASE AND FORMAT" runs [`go`] on `dev` / `disk`.
fn install(cx: &Rc<Ctx>, dev: Device, disk: Disk) {
    *cx.ask.borrow_mut() = Some(Box::new(move |cx| go(cx, dev, disk)));
}

/// The dialog's button: the safeguards on what the user chose; on a pass the dialog closes and the format runs (after the backup, when
/// asked for).
fn go(cx: &Rc<Ctx>, dev: Device, disk: Disk) {
    let Some((fs, label, typed, backup_first)) = cx.with_state(|s| {
        let fs = if s.get_format_fs() == 1 {
            Fs::ExFat
        } else {
            Fs::Fat32
        };
        (
            fs,
            s.get_format_label().to_string(),
            s.get_format_typed().to_string(),
            s.get_format_backup(),
        )
    }) else {
        return;
    };
    let seen: Vec<String> = disk.volumes.iter().map(|v| v.name.clone()).collect();
    let spec = Spec { fs, label };
    let asked = Request {
        spec: spec.clone(),
        typed: &typed,
        seen: &seen,
        busy: cx.busy(),
    };
    if let Err(e) = format::check(&dev, &disk, &asked) {
        cx.with_state(|s| s.set_format_problem(explain(&e, &dev.label).into()));
        return install(cx, dev, disk);
    }
    cx.with_state(|s| s.set_format_open(false));
    let shown = dev.clone();
    let run = move |cx: &Rc<Ctx>| start(cx, dev, spec, typed, seen);
    if backup_first && shown.has_library() {
        backup::now(cx, &shown, move |cx, r| {
            match r {
            Ok(_) => run(cx),
            Err(e) => cx.say(&tr!("Not formatted: the library backup failed ({}). The disk was not touched. Fix that or untick the backup, then try again.", e), true),
        }
        });
    } else {
        run(cx);
    }
}

/// Runs the format as the window's job; the device it makes is chosen when it shows up.
fn start(cx: &Rc<Ctx>, dev: Device, spec: Spec, typed: String, seen: Vec<String>) {
    let made: Arc<Mutex<Option<Done>>> = Arc::default();
    let (job, kept) = (made.clone(), made.clone());
    let id = dev.id.clone();
    cx.start_then(
        Some(id),
        phase(Step::Unmount),
        move |ctl| {
            let req = Request {
                spec,
                typed: &typed,
                seen: &seen,
                busy: false,
            };
            let done = format::format(&dev, &req, &|s| ctl.set_phase(phase(s)))
                .map_err(|e| explain(&e, &dev.label))?;
            let name = Some(done.label.as_str()).filter(|l| !l.is_empty()).unwrap_or(&dev.label);
            let text = match &done.short {
                None => tr!(
                    "{} formatted: {} on one MBR partition. Choose EXPORT to put music on it.",
                    name,
                    fs_name(done.fs)
                ),
                Some(v) => tr!(
                    "{} formatted as {} instead of FAT32: the system makes FAT32 on a card this small only with an extra permission. DJ players read it all the same. Press FORMAT… again and enter your password when asked if you want FAT32.",
                    name,
                    v
                ),
            };
            *job.lock().unwrap() = Some(done);
            Ok(text)
        },
        move |cx, r| {
            let Some(done) = kept.lock().unwrap().take().filter(|_| r.is_ok()) else {
                return;
            };
            // The new volume is mounted by now: look again and show it.
            cx.refresh_devices();
            let id = cx
                .devices
                .borrow()
                .iter()
                .find(|d| d.source == done.source)
                .map(|d| d.id.clone());
            if let (Some(id), Some(app)) = (id, cx.app()) {
                show_panel(&app, &id);
            }
        },
    );
}
