//! What the settings do with the connected devices: the CONNECTED DEVICES rows, the tab's `apply_label` button (write the
//! defaults to every device), its `read_label` button (take over what the player changed), its `auto_key` (write a device's
//! edit at once) and [`sync_from_device`] (a device that appears). The plugin reads and writes ([`Provider::settings_read`],
//! [`Provider::settings_write`]); the host keeps the fingerprint it reported so that it tells what the player changed.

use super::{Ctx, Done, Row, all_fields, find};
use openfader::device::Device;
use openfader::plugin::device::Provider;
use openfader::store::{Store, device_settings};
use openfader_plugin::library::{Feature, FieldKind, SettingsRead, SettingsSchema, Tab};
use std::collections::BTreeMap;

/// Whether `d` holds settings of `provider`'s kind.
fn has_settings(d: &Device, provider: Option<&str>) -> bool {
    d.provider.is_some()
        && provider.is_none_or(|p| d.provider.as_deref() == Some(p))
        && d.info.features.contains(&Feature::Settings)
}

/// "CONNECTED DEVICES": a button to the settings of each device that has some.
pub fn rows(devices: &[Device]) -> Vec<Row> {
    let mut rows = vec![Row::header(tr!("CONNECTED DEVICES"))];
    let before = rows.len();
    rows.extend(devices.iter().filter(|d| has_settings(d, None)).map(|d| {
        Row::chips(
            &format!("djs.dev.{}", d.id),
            &d.label,
            &[tr!("DEVICE SETTINGS")],
            usize::MAX,
        )
        .hint(&d.fs)
    }));
    if rows.len() == before {
        rows.push(Row::info(
            tr!("No device connected"),
            tr!("settings of one device are edited here once it is connected"),
        ));
    }
    rows
}

/// `settings_write` of the device's configuration, then remember what the device holds.
fn write(store: &Store, provider: &Provider, d: &Device) -> Result<(), String> {
    let w = provider.settings_write(&d.mount, &device_settings::config(store, &d.id))?;
    device_settings::record_fingerprint(store, &d.id, &w.fingerprint)?;
    Ok(())
}

/// "Apply": every writable connected device of the provider forgets its own values of the tab's keys and gets the defaults
/// written.
pub fn apply(c: &Ctx, provider: &Provider, tab: &Tab, devices: &[Device]) -> Done {
    let targets: Vec<&Device> = devices
        .iter()
        .filter(|d| d.writable() && has_settings(d, Some(provider.name())))
        .collect();
    if targets.is_empty() {
        return Err(tr!("no writable device is connected").into());
    }
    let fields: Vec<_> = all_fields(tab)
        .filter(|f| f.kind != FieldKind::Info)
        .collect();
    for d in &targets {
        Ctx::new(c.store, &d.id, c.files.clone()).reset(fields.iter().copied())?;
        write(c.store, provider, d)?;
    }
    Ok(tr!("applied to {} device(s)", targets.len()))
}

/// A device's setting was edited: when its tab has an `auto_key` that is on and the device is connected, it is written at
/// once (so that [`sync_from_device`] never mistakes an edit that was not yet exported for a change made on the player).
pub fn push(c: &Ctx, provider: &Provider, tab: &Tab, devices: &[Device]) -> Result<(), String> {
    let on = c.is_device()
        && tab
            .auto_key
            .as_deref()
            .and_then(|k| find(tab, k))
            .is_some_and(|f| c.value(f) == "1");
    match devices
        .iter()
        .find(|d| on && d.id == c.scope && d.writable() && has_settings(d, Some(provider.name())))
    {
        Some(d) => write(c.store, provider, d),
        None => Ok(()),
    }
}

/// Stores every value the device reported that differs from what `scope` has as an override, and forgets the overrides that
/// now equal what it would have anyway; returns how many values differ from what the scope had. A key the device did not
/// report (it has no file for it) is left alone.
pub(super) fn import(
    store: &Store,
    scope: &str,
    schema: &SettingsSchema,
    values: &BTreeMap<String, String>,
) -> Result<usize, String> {
    let mut c = Ctx::new(store, scope, None);
    let mut n = 0;
    for f in schema.tabs.iter().flat_map(|t| all_fields(t)) {
        if let Some(v) = values.get(&f.key).filter(|_| f.kind != FieldKind::Info) {
            n += usize::from(c.value(f) != *v);
            c.put(f, v)?;
        }
    }
    Ok(n)
}

/// Takes the settings the device holds over as its overrides, and remembers their fingerprint.
fn take(store: &Store, provider: &Provider, d: &Device, r: &SettingsRead) -> Result<usize, String> {
    let n = import(store, &d.id, &provider.schema()?, &r.values)?;
    device_settings::record_fingerprint(store, &d.id, &r.fingerprint)?;
    Ok(n)
}

/// "Read": the settings changed on the player become the device's own.
pub fn read(c: &Ctx, provider: &Provider, devices: &[Device]) -> Done {
    let d = devices
        .iter()
        .find(|d| d.id == c.scope)
        .ok_or(tr!("the device is not connected"))?;
    let r = provider.settings_read(&d.mount)?;
    let n = take(c.store, provider, d, &r)?;
    Ok(tr!("{} setting(s) changed on the player", n))
}

/// For the code that sees a device appear: takes over what the player changed since we last wrote its settings; 0 and nothing
/// touched when the device holds what we wrote (or its plugin has no settings).
pub fn sync_from_device(store: &Store, d: &Device) -> Result<usize, String> {
    let Some(provider) = d.provider().filter(|_| has_settings(d, None)) else {
        return Ok(0);
    };
    let r = provider.settings_read(&d.mount)?;
    if device_settings::player_changed(store, &d.id, &r.fingerprint) {
        take(store, &provider, d, &r)
    } else {
        Ok(0)
    }
}
