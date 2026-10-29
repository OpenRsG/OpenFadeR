//! BACKUPS tab: library backups of this computer ([`openfader::device::backup`]; index in the store, listed with date and
//! time, computer, device and file system), BACKUP NOW, RESTORE TO DEVICE (asks before it replaces a
//! library, and again when the collection lacks tracks the backup lists: "The following tracks cannot be found. Continue to
//! restore?") and DELETE. [`now`] is also what "Automatically backup the library upon removal" runs before an eject, so
//! [`Ctx::eject`] lives here.

use super::{App, BUSY, BackupItem, Ctx, DeviceState};
use openfader::{
    device::{self, Device, backup},
    plugin::device::Feature,
    store::{self, DeviceBackup, NewBackup, device_settings},
};
use slint::{ComponentHandle, Model};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    rc::Rc,
};

/// Marks a finished restore that stopped for missing tracks (the titles follow, one per line).
const MISSING: &str = "\u{1}missing\n";

/// How many missing titles the question lists.
const MISSING_SHOWN: usize = 8;

fn when(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0)
        .map(|t| {
            t.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_default()
}

/// Reloads the list from the store.
pub(super) fn refresh(cx: &Rc<Ctx>) {
    let all = cx.store.backups().unwrap_or_default();
    cx.models.backups.set_vec(
        all.iter()
            .map(|b| BackupItem {
                when: when(b.taken_at).into(),
                computer: b.computer.clone().into(),
                device: if b.auto {
                    format!("{} · {}", b.name, tr!("automatic, before a write"))
                } else {
                    b.name.clone()
                }
                .into(),
                fs: b.fs.clone().into(),
                selected: false,
            })
            .collect::<Vec<_>>(),
    );
    *cx.backups.borrow_mut() = all;
}

fn selected(cx: &Ctx) -> Vec<DeviceBackup> {
    let all = cx.backups.borrow();
    cx.models
        .backups
        .iter()
        .zip(all.iter())
        .filter(|(r, _)| r.selected)
        .map(|(_, b)| b.clone())
        .collect()
}

pub(super) fn wire(cx: &Rc<Ctx>, app: &App) {
    let s = app.global::<DeviceState>();
    let c = cx.clone();
    s.on_backup_toggle(move |i| {
        if let Some(mut r) = c.models.backups.row_data(i as usize) {
            r.selected = !r.selected;
            c.models.backups.set_row_data(i as usize, r);
        }
    });
    let c = cx.clone();
    s.on_backup_now(move || c.on_current(|c, d| now(c, d, |_, _| {})));
    let c = cx.clone();
    s.on_restore(move || {
        let sel = selected(&c);
        let (Some(dev), [b]) = (c.current(), sel.as_slice()) else {
            return c.say(tr!("Choose a device and select one backup"), true);
        };
        if !c.writable(&dev) {
            return;
        }
        let text = tr!(
            "Restore the backup of {} ({}) onto {}? The library on the device is replaced.",
            b.name,
            when(b.taken_at),
            dev.label
        );
        let b = b.clone();
        c.confirm(&text, tr!("RESTORE"), move |c| restore(c, &dev, &b, false));
    });
    let c = cx.clone();
    s.on_backup_delete(move || {
        let sel = selected(&c);
        if sel.is_empty() {
            return c.say(tr!("Select the backups to delete"), true);
        }
        c.confirm(
            &tr!("Delete {} backup(s) from this computer?", sel.len()),
            tr!("DELETE"),
            move |c| {
                for b in &sel {
                    let path = Path::new(&b.path);
                    match backup::delete(path) {
                        Ok(()) => drop(c.store.delete_backup(b.id)),
                        // A folder that is gone already is a backup that is gone: forget it.
                        Err(_) if !path.exists() => drop(c.store.delete_backup(b.id)),
                        Err(e) => return c.say(&e, true),
                    }
                }
                refresh(c);
                c.say(tr!("Backups deleted"), false);
            },
        );
    });
    let c = cx.clone();
    s.on_manage_backups(move || {
        c.with_state(|s| {
            s.set_tab(3);
            s.set_visible(true);
        });
        refresh(&c);
    });
}

/// Backs up the library of `dev` into the chosen folder; `then` sees the outcome (the eject after an automatic backup).
pub(super) fn now(
    cx: &Rc<Ctx>,
    dev: &Device,
    then: impl FnOnce(&Rc<Ctx>, &Result<String, String>) + 'static,
) {
    if !dev.can(Feature::Backup) || !dev.has_library() {
        return cx.say(&tr!("{}: there is no library on it to back up. Export to it first.", dev.label), true);
    }
    let typed = cx
        .with_state(|s| s.get_backup_dir().trim().to_owned())
        .filter(|d| !d.is_empty());
    let Some(dir) = typed.map(PathBuf::from).or_else(backup::default_dir) else {
        return cx.say(tr!("No backup folder: type one"), true);
    };
    let (store, dev) = (cx.store.clone(), dev.clone());
    let computer = device_settings::Behaviour::load(&cx.store).computer();
    let bg = device_settings::config(&cx.store, &dev.id)
        .get("background")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    cx.start_then(
        true,
        Some(dev.id.clone()),
        tr!("Backing up"),
        move |ctl| {
            let made = backup::create(&dev, &dir, &computer, &ctl)?;
            store.add_backup(&NewBackup {
                device: &dev.id,
                taken_at: chrono::Local::now().timestamp(),
                computer: &computer,
                name: &dev.label,
                path: &made.dir.to_string_lossy(),
                fs: &dev.fs,
                bg,
                auto: false,
            })?;
            Ok(tr!(
                "Backed up the library of {} ({} files) to {}",
                dev.label,
                made.files,
                made.dir.display()
            ))
        },
        move |cx, r| {
            refresh(cx);
            then(cx, r);
        },
    );
}

/// Restores `b` onto `dev`; with missing tracks it asks before it goes on (`go_on` = it was asked).
fn restore(cx: &Rc<Ctx>, dev: &Device, b: &DeviceBackup, go_on: bool) {
    let Some(provider) = cx.provider_for(dev, Feature::Backup) else {
        return;
    };
    let Some(snap) = cx.snapshot() else { return };
    let (dev, b) = (dev.clone(), b.clone());
    let (asked_dev, asked_b) = (dev.clone(), b.clone());
    cx.start_then(
        true,
        Some(dev.id.clone()),
        tr!("Restoring"),
        move |ctl| {
            // The collection's files by "file name|size", the way the store keys a track.
            let files: HashMap<String, PathBuf> = snap
                .tracks()
                .iter()
                .filter_map(|(t, _)| t.file.clone().map(|f| (store::key(t), f)))
                .collect();
            let find = |name: &str, size: u64| files.get(&format!("{name}|{size}")).cloned();
            match backup::restore(
                &provider,
                Path::new(&b.path),
                &dev.mount,
                &find,
                go_on,
                &ctl,
            )? {
                backup::Restore::Missing(titles) => Ok(format!("{MISSING}{}", titles.join("\n"))),
                backup::Restore::Done { tracks, missing } => Ok(tr!(
                    "Restored the library onto {} and {} track(s){}",
                    dev.label,
                    tracks,
                    if missing.is_empty() {
                        String::new()
                    } else {
                        tr!("; {} track(s) were not in the collection", missing.len())
                    }
                )),
            }
        },
        move |cx, r| {
            let Ok(text) = r else { return };
            let Some(titles) = text.strip_prefix(MISSING) else {
                return;
            };
            let total = titles.lines().count();
            let mut list = titles
                .lines()
                .take(MISSING_SHOWN)
                .collect::<Vec<_>>()
                .join("\n");
            if total > MISSING_SHOWN {
                list.push('\n');
                list.push_str(&tr!("… and {} more", total - MISSING_SHOWN));
            }
            cx.confirm(
                &tr!(
                    "The following tracks cannot be found. Continue to restore?\n{}",
                    list
                ),
                tr!("CONTINUE"),
                move |c| restore(c, &asked_dev, &asked_b, true),
            );
        },
    );
}

impl Ctx {
    /// How many tracks of the opened library (and so its decks' sources) play from files on `dev`.
    fn loaded_from(&self, dev: &Device) -> usize {
        self.lib.tracks(None).map_or(0, |v| {
            v.iter()
                .filter(|t| t.file.as_deref().is_some_and(|f| f.starts_with(&dev.mount)))
                .count()
        })
    }

    /// Ejects `dev` (a stick or card): after a check of the stick when Preferences ask for one, and after the automatic backup
    /// when they ask for that, refused while a job writes to it. `confirmed` = the user already agreed to eject the device the
    /// library is loaded from.
    pub(super) fn eject(self: &Rc<Self>, dev: &Device, confirmed: bool) {
        self.eject_from(dev, confirmed, false);
    }

    /// [`Self::eject`]; `checked`: the stick was looked at for this eject already (or the user said to go on).
    pub(super) fn eject_from(self: &Rc<Self>, dev: &Device, confirmed: bool, checked: bool) {
        let loaded = self.loaded_from(dev);
        if loaded > 0 && !confirmed {
            let d = dev.clone();
            return self.confirm(&tr!("The library is loaded from {}: {} track(s) play from it and stop working when it is removed. Eject anyway?", dev.label, loaded), tr!("EJECT"), move |c| c.eject_from(&d, true, checked));
        }
        let refusal = self.job.borrow().as_ref().map(|j| {
            if j.device.as_deref() == Some(dev.id.as_str()) {
                tr!(
                    "A job on {} is running: wait for it to finish before ejecting",
                    dev.label
                )
            } else {
                crate::app::i18n::tr_dyn(BUSY).to_owned()
            }
        });
        if let Some(msg) = refusal {
            return self.say(&msg, true);
        }
        if !checked && device_settings::Behaviour::load(&self.store).check_on_eject {
            return self.check_before_eject(dev, confirmed);
        }
        let go = |cx: &Rc<Ctx>, dev: &Device| {
            let d = dev.clone();
            cx.start(false, Some(dev.id.clone()), tr!("Ejecting"), move |_| {
                device::eject::eject(&d)
            });
        };
        if dev.can(Feature::Backup)
            && dev.has_library()
            && device_settings::Behaviour::load(&self.store).auto_backup
        {
            let d = dev.clone();
            now(self, dev, move |cx, r| {
                match r {
                Ok(_) => go(cx, &d),
                Err(e) => cx.say(&tr!("Not ejected: the backup failed ({}). Fix that, or switch the backup off in PREFERENCES > DJ SYSTEM, then eject again.", e), true),
            }
            });
        } else {
            go(self, dev);
        }
    }
}
