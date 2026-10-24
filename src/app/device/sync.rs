//! SYNC MANAGER tab: "Synchronize playlists with a device" and the playlists ticked for it (kept per device in the store,
//! [`openfader::store::Store::sync_playlists`]); SYNC NOW exports them in their latest state, in every format picked for the
//! device ([`openfader::device::sync::narrow`] the collection, then [`Ctx::write`]), and so does the app when such a device is
//! mounted for the first time in a run: after asking, when Preferences ask ("Ask before syncing a plugged-in device").

use super::preview::{Build, Built, Target};
use super::{record_all, App, Ctx, DeviceState};
use openfader::{
    device::{plan::Preview, size, sync, Device},
    library::snapshot::Snapshot,
    store::device_settings::Behaviour,
};
use slint::ComponentHandle;
use std::{rc::Rc, sync::Arc};

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

/// What a plugged-in device is asked: "KEY is plugged in: sync 3 playlists (12 new tracks, 480 MB)?"
fn question(label: &str, playlists: usize, p: &Preview) -> String {
    let what = if p.add.is_empty() {
        tr!("no new tracks").to_owned()
    } else {
        tr_n!(
            "{n} new track, {}",
            "{n} new tracks, {}",
            p.add.len(),
            size(p.copy_bytes)
        )
    };
    tr!(
        "{} is plugged in: sync {} ({})?",
        label,
        tr_n!("{n} playlist", "{n} playlists", playlists),
        what
    )
}

/// Exports the ticked playlists to `dev` (after the preview card when Preferences ask for it); `auto` = started by the app
/// because the device was just mounted: then "nothing ticked" is not worth a message and, when Preferences ask, the user is
/// asked first ("KEY is plugged in: sync ...?") with the numbers the preview found.
pub(super) fn run(cx: &Rc<Ctx>, dev: &Device, auto: bool) {
    let ticked = cx.store.sync_playlists(&dev.id).unwrap_or_default();
    if ticked.is_empty() {
        if !auto {
            cx.say(
                tr!("No playlist is set to synchronize: tick some in the list"),
                true,
            );
        }
        return;
    }
    if !cx.writable(dev) {
        return;
    }
    let Some(writers) = cx.writers(dev) else {
        return;
    };
    let Some(full) = cx.snapshot() else { return };
    let snap = match sync::narrow(&full, &ticked) {
        Ok(s) => Arc::new(s),
        Err(e) => return cx.say(&e, true),
    };
    let t = Target::new(cx, Some(dev), dev.mount.clone(), writers, false);
    let build = |snap: &Arc<Snapshot>| {
        let snap = snap.clone();
        move || -> Build { Box::new(move |_| Ok(Built::of(snap))) }
    };
    let (store, id) = (cx.store.clone(), dev.id.clone());
    let (go_t, go_snap) = (t.clone(), snap.clone());
    let go = move |cx: &Rc<Ctx>| {
        // The device may be gone when the question was answered.
        let Some(d) = cx.device(&id) else {
            return cx.say(tr!("That device is not mounted any more"), true);
        };
        cx.write(
            go_t,
            tr!("Syncing"),
            Box::new(move |_| Ok(Built::of(go_snap))),
            {
                let (store, d) = (store, d);
                move |_, done| {
                    record_all(&store, Some(&d), done, &d.label).map(|t| tr!("synced: {}", t))
                }
            },
            move |cx, _| cx.relink(&id),
        );
    };
    let behaviour = Behaviour::load(&cx.store);
    if !(auto && behaviour.sync_ask) {
        return cx.gate(&t, 0, build(&snap), go);
    }
    let (label, n, preview, t2) = (
        dev.label.clone(),
        ticked.len(),
        behaviour.preview,
        t.clone(),
    );
    cx.plan_then(&t, 0, build(&snap)(), move |cx, p| {
        let text = question(&label, n, &p);
        cx.ask_sync(text, move |cx| {
            if preview {
                cx.show_preview(&t2, p, Box::new(go));
            } else {
                go(cx);
            }
        });
    });
}
