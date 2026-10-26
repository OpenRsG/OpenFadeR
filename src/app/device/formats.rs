//! The library formats of a device ↔ the FORMATS list of the DEVICES tab and the chips of EXPORT / SYNC MANAGER: which formats
//! an export writes (UNIVERSAL = all of them), what reads each (the plugin's `title` / `about`) and its device options, stored
//! per device ([`device_settings::formats`] holds the user's switches, the options are device settings under the plugin's keys).
//! On the EXPORT tab with a folder typed, the chips are the folder's ([`Ctx::format_target`]).

use super::{Ctx, FormatItem, FormatOption};
use openfader::{
    device::{self, Device},
    plugin::device::{DeviceInfo, Feature, Provider},
    store::device_settings,
};
use openfader_plugin::library::{Field, FieldKind};
use slint::{ModelRc, VecModel};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    rc::Rc,
    sync::mpsc,
    time::{Duration, Instant},
};

/// The formats of `d` an export can write, in the device's order (the one handling it first).
fn writable(d: &Device) -> impl Iterator<Item = &(String, DeviceInfo)> {
    d.formats
        .iter()
        .filter(|(_, i)| i.features.contains(&Feature::Export))
}

/// The options of `info` the window draws: switches and choices.
fn options(info: &DeviceInfo) -> impl Iterator<Item = &Field> {
    info.options
        .iter()
        .filter(|f| matches!(f.kind, FieldKind::Bool | FieldKind::Choice { .. }))
}

#[derive(Default)]
pub(super) struct Folder {
    path: Option<PathBuf>,
    generation: u64,
    ready: Option<(Instant, Result<Device, String>)>,
    task: Option<(u64, mpsc::Receiver<Result<Device, String>>)>,
}

impl Folder {
    fn invalidate(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.ready = None;
    }

    fn request(&mut self, path: Option<PathBuf>) -> Option<Device> {
        if self.path != path {
            self.path = path;
            self.invalidate();
        }
        self.start();
        self.ready.as_ref()?.1.as_ref().ok().cloned()
    }

    fn start(&mut self) {
        if self.task.is_some()
            || self
                .ready
                .as_ref()
                .is_some_and(|r| r.0.elapsed() < Duration::from_secs(5))
        {
            return;
        }
        let Some(path) = self.path.clone() else {
            return;
        };
        let (tx, rx) = mpsc::channel();
        self.task = Some((self.generation, rx));
        std::thread::spawn(move || {
            let result = if path.is_dir() {
                Ok(device::folder(&path))
            } else {
                Err(tr!("No such folder: {}", path.display()))
            };
            let _ = tx.send(result);
        });
    }

    fn poll(&mut self) -> bool {
        let result = self.task.as_ref().map(|(_, rx)| rx.try_recv());
        let result = match result {
            Some(Ok(r)) => r,
            Some(Err(mpsc::TryRecvError::Disconnected)) => {
                Err(tr!("the worker stopped").to_owned())
            }
            _ => return false,
        };
        let (generation, _) = self.task.take().unwrap();
        let current = generation == self.generation;
        if current {
            self.ready = Some((Instant::now(), result));
        }
        self.start();
        current
    }
}

impl Ctx {
    /// What the FORMATS chips are about: on the EXPORT tab with a folder typed, that folder; else the chosen device.
    pub(super) fn format_target(&self) -> Option<Device> {
        let folder = self
            .with_state(|s| (s.get_tab() == 1).then(|| s.get_folder().trim().to_owned()))
            .flatten()
            .filter(|f| !f.is_empty());
        let target = self.folder.borrow_mut().request(folder.map(PathBuf::from));
        if self.folder.borrow().path.is_some() {
            target
        } else {
            self.current()
        }
    }

    pub(super) fn poll_formats(&self) {
        let changed = self.folder.borrow_mut().poll();
        if changed {
            self.show_formats();
        }
    }

    pub(super) fn refresh_folder(&self) {
        self.folder.borrow_mut().invalidate();
        self.show_formats();
    }

    pub(super) fn folder_error(&self) -> Option<String> {
        self.folder
            .borrow()
            .ready
            .as_ref()
            .and_then(|(_, r)| r.as_ref().err())
            .cloned()
    }

    /// The user's format switches for `d` (plugin name → on), stored as `+name` / `-name`. A list of plain names (what older
    /// versions stored: the whole pick) switches those on and the other formats writable there off.
    fn switches(&self, d: &Device) -> BTreeMap<String, bool> {
        let Some(list) = device_settings::formats(&self.store, &d.id) else {
            return BTreeMap::new();
        };
        if list.iter().any(|n| n.starts_with(['+', '-'])) {
            list.iter()
                .filter_map(|n| Some((n.get(1..)?.to_owned(), n.starts_with('+'))))
                .collect()
        } else {
            writable(d).map(|(n, _)| (n.clone(), list.contains(n))).collect()
        }
    }

    /// The formats picked for `d`, in the device's order: the user's switches, [`Device::default_formats`] for the formats never
    /// switched (so a format a plugin offers later follows the defaults instead of a list frozen at the first click).
    pub(super) fn picked(&self, d: &Device) -> Vec<String> {
        let (switched, default) = (self.switches(d), d.default_formats());
        d.formats
            .iter()
            .map(|(n, _)| n)
            .filter(|n| {
                switched
                    .get(*n)
                    .copied()
                    .unwrap_or_else(|| default.contains(n))
            })
            .cloned()
            .collect()
    }

    /// The plugins an export to `d` writes with: the picked formats that can export, the one handling the device first; says
    /// so when none is on.
    pub(super) fn writers(&self, d: &Device) -> Option<Vec<Provider>> {
        let picked = self.picked(d);
        let w: Vec<Provider> = writable(d)
            .filter(|(n, _)| picked.contains(n))
            .filter_map(|(n, _)| openfader::plugin::device::provider(n))
            .collect();
        if w.is_empty() {
            self.say(
                &tr!("{}: switch on at least one library format", d.label),
                true,
            );
            return None;
        }
        Some(w)
    }

    /// The FORMATS list of the chosen device, and whether it is universal.
    pub(super) fn show_formats(&self) {
        let (items, universal) = self.format_target().map_or((Vec::new(), false), |d| {
            let picked = self.picked(&d);
            let config = device_settings::config(&self.store, &d.id);
            let items: Vec<FormatItem> = writable(&d)
                .map(|(n, i)| FormatItem {
                    title: d.format_title(n).into(),
                    about: i.about.as_str().into(),
                    held: i.libraries.join(" + ").into(),
                    on: picked.contains(n),
                    options: ModelRc::from(Rc::new(VecModel::from(
                        options(i)
                            .map(|f| {
                                let v = config.get(&f.key).unwrap_or(&f.default);
                                let (choices, current) = match &f.kind {
                                    FieldKind::Choice { choices, .. } => (
                                        choices.iter().map(|c| c.label.as_str().into()).collect(),
                                        choices.iter().position(|c| &c.value == v).unwrap_or(0),
                                    ),
                                    _ => (Vec::new(), 0),
                                };
                                FormatOption {
                                    label: f.label.as_str().into(),
                                    hint: f.hint.as_str().into(),
                                    on: v == "1",
                                    choices: ModelRc::from(Rc::new(VecModel::from(choices))),
                                    current: current as i32,
                                }
                            })
                            .collect::<Vec<_>>(),
                    ))),
                })
                .collect();
            let universal = !items.is_empty() && items.iter().all(|f| f.on);
            (items, universal)
        });
        self.models.formats.set_vec(items);
        self.with_state(|s| s.set_universal(universal));
    }

    /// Switches `names` on or off for `d` (the other switches stay) and redraws the list.
    fn switch(&self, d: &Device, names: &[String], on: bool) {
        let mut switched = self.switches(d);
        for n in names {
            switched.insert(n.clone(), on);
        }
        let list: Vec<String> = switched
            .iter()
            .map(|(n, on)| format!("{}{n}", if *on { '+' } else { '-' }))
            .collect();
        if let Err(e) = device_settings::set_formats(&self.store, &d.id, &list) {
            self.say(&e, true);
        }
        self.show_formats();
    }

    /// Switches the `i`-th format of [`Self::show_formats`] for the chips' target.
    pub(super) fn toggle_format(&self, i: usize) {
        let Some(d) = self.format_target() else {
            return;
        };
        let Some((name, _)) = writable(&d).nth(i) else {
            return;
        };
        let on = !self.picked(&d).contains(name);
        self.switch(&d, std::slice::from_ref(name), on);
    }

    /// UNIVERSAL (`true`): every format of the chips' target on. DEDICATED: when it was universal, only the format handling
    /// it stays on (the user ticks the others); otherwise nothing changes.
    pub(super) fn set_universal(&self, on: bool) {
        let Some(d) = self.format_target() else {
            return;
        };
        let all: Vec<String> = writable(&d).map(|(n, _)| n.clone()).collect();
        let picked = self.picked(&d);
        if on {
            self.switch(&d, &all, true);
        } else if all.iter().all(|n| picked.contains(n)) {
            self.switch(&d, &all[all.len().min(1)..], false);
        }
    }

    /// Sets option `k` of format `i` of the chosen device: 1 / 0 for a switch, the entry index for a choice.
    pub(super) fn set_option(&self, i: usize, k: usize, value: usize) {
        let Some(d) = self.format_target() else {
            return;
        };
        let Some(f) = writable(&d)
            .nth(i)
            .and_then(|(_, info)| options(info).nth(k))
        else {
            return;
        };
        let v = match &f.kind {
            FieldKind::Choice { choices, .. } => match choices.get(value) {
                Some(c) => c.value.clone(),
                None => return,
            },
            _ => if value == 0 { "0" } else { "1" }.to_owned(),
        };
        if let Err(e) = self.store.set_device_setting(&d.id, &f.key, &v) {
            self.say(&e.to_string(), true);
        }
        self.show_formats();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changing_or_clearing_a_folder_discards_the_blocked_old_probe() {
        let dir =
            std::env::temp_dir().join(format!("openfader-folder-probe-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (tx, rx) = mpsc::channel();
        let mut f = Folder {
            path: Some(dir.join("missing")),
            task: Some((0, rx)),
            ..Default::default()
        };
        assert!(f.request(Some(dir.clone())).is_none());
        tx.send(Err("old folder failed".into())).unwrap();
        assert!(!f.poll());
        let end = Instant::now() + Duration::from_secs(10);
        while f.ready.is_none() {
            f.poll();
            assert!(Instant::now() < end, "latest folder probe did not finish");
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(f.request(Some(dir.clone())).unwrap().mount, dir);

        let (tx, rx) = mpsc::channel();
        f.invalidate();
        f.task = Some((f.generation, rx));
        f.request(None);
        tx.send(Err("cleared folder failed".into())).unwrap();
        assert!(!f.poll());
        assert!(f.ready.is_none());
        assert!(f.path.is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
