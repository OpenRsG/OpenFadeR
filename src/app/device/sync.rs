//! SYNC MANAGER tab: "Synchronize playlists with a device" and the playlists ticked for it (kept per device in the store,
//! [`openfader::store::Store::sync_playlists`]); SYNC NOW exports them in their latest state
//! ([`openfader::device::sync::sync`]), and so does the app when such a device is mounted for the first time in a run.

use super::{App, Ctx, DeviceState, record_export, report_text};
use openfader::{
    device::{Device, sync},
    plugin::device::Feature,
};
use slint::ComponentHandle;
use std::rc::Rc;

/// Shows the chosen device's ticks and switch.
pub(super) fn refresh(cx: &Rc<Ctx>) {
    let (Some(dev), Some(snap)) = (cx.current(), cx.snap.borrow().clone()) else {
        return;
    };
    let ticked = cx.store.sync_playlists(&dev.id).unwrap_or_default();
    cx.sync.set(&snap, &|id| ticked.iter().any(|t| t == id));
    let on = cx.store.device_sync_on(&dev.id).unwrap_or(false);
    cx.with_state(|s| s.set_sync_on(on));
}

pub(super) fn wire(cx: &Rc<Ctx>, app: &App) {
    let s = app.global::<DeviceState>();
    let c = cx.clone();
    s.on_sync_toggle(move |i| {
        let (Some(dev), Some((id, on))) = (c.current(), c.sync.toggle(i as usize)) else {
            return;
        };
        if let Err(e) = c.store.set_sync_playlist(&dev.id, &id, on) {
            c.say(&e, true);
        }
    });
    let c = cx.clone();
    s.on_sync_set_on(move |on| {
        let Some(dev) = c.current() else { return };
        match c.store.set_device_sync_on(&dev.id, on) {
            Ok(()) => {
                c.with_state(|s| s.set_sync_on(on));
            }
            Err(e) => c.say(&e, true),
        }
    });
    let c = cx.clone();
    s.on_sync_start(move || {
        if let Some(dev) = c.current() {
            run(&c, &dev, false);
        }
    });
}

/// Exports the ticked playlists to `dev`; `auto` = started by the app, so "nothing ticked" is not worth a message.
pub(super) fn run(cx: &Rc<Ctx>, dev: &Device, auto: bool) {
    let ticked = cx.store.sync_playlists(&dev.id).unwrap_or_default();
    if ticked.is_empty() {
        if !auto {
            cx.say(
                "No playlist is set to synchronize: tick some in the list",
                true,
            );
        }
        return;
    }
    if !cx.writable(dev) {
        return;
    }
    let Some(provider) = cx.provider_for(dev, Feature::Export) else {
        return;
    };
    let Some(snap) = cx.snapshot() else { return };
    let (o, store, dev) = (cx.options(Some(dev)), cx.store.clone(), dev.clone());
    cx.start(Some(dev.id.clone()), "Syncing", move |ctl| {
        let r = sync::sync(&provider, &snap, &ticked, &dev.mount, &o, &ctl)?;
        let mut text = format!("synced: {}", report_text(&r, &dev.label));
        record_export(&store, &dev.id, &r, &mut text);
        Ok(text)
    });
}
