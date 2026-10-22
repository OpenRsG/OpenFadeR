//! The library formats of a device ↔ the FORMATS list of the DEVICES tab and the chips of EXPORT / SYNC MANAGER: which formats
//! an export writes (UNIVERSAL = all of them), what reads each (the plugin's `title` / `about`) and its device options, stored
//! per device ([`device_settings::formats`], the options as device settings under the plugin's keys).

use super::{Ctx, FormatItem, FormatOption};
use openfader::{
    device::Device,
    plugin::device::{DeviceInfo, Feature, Provider},
    store::device_settings,
};
use openfader_plugin::library::{Field, FieldKind};
use slint::{ModelRc, VecModel};
use std::rc::Rc;

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

impl Ctx {
    /// The formats picked for `d` (its [`Device::default_formats`] until the user picks), in the device's order.
    pub(super) fn picked(&self, d: &Device) -> Vec<String> {
        device_settings::formats(&self.store, &d.id).unwrap_or_else(|| d.default_formats())
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
        let (items, universal) = self.current().map_or((Vec::new(), false), |d| {
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

    /// Stores `picked` as the formats of `d` and redraws the list.
    fn pick(&self, d: &Device, picked: &[String]) {
        if let Err(e) = device_settings::set_formats(&self.store, &d.id, picked) {
            self.say(&e, true);
        }
        self.show_formats();
    }

    /// Switches the `i`-th format of [`Self::show_formats`] for the chosen device.
    pub(super) fn toggle_format(&self, i: usize) {
        let Some(d) = self.current() else { return };
        let Some((name, _)) = writable(&d).nth(i) else {
            return;
        };
        let mut picked = self.picked(&d);
        match picked.iter().position(|p| p == name) {
            Some(at) => drop(picked.remove(at)),
            None => picked.push(name.clone()),
        }
        self.pick(&d, &picked);
    }

    /// UNIVERSAL (`true`): every format of the chosen device on. DEDICATED: when it was universal, only the format handling
    /// the device stays on (the user ticks the others); otherwise nothing changes.
    pub(super) fn set_universal(&self, on: bool) {
        let Some(d) = self.current() else { return };
        let all: Vec<String> = writable(&d).map(|(n, _)| n.clone()).collect();
        let picked = self.picked(&d);
        if on {
            self.pick(&d, &all);
        } else if all.iter().all(|n| picked.contains(n)) {
            self.pick(&d, &all[..all.len().min(1)]);
        }
    }

    /// Sets option `k` of format `i` of the chosen device: 1 / 0 for a switch, the entry index for a choice.
    pub(super) fn set_option(&self, i: usize, k: usize, value: usize) {
        let Some(d) = self.current() else { return };
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
