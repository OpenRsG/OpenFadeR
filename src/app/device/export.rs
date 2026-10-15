//! EXPORT tab: the library, or the ticked playlists, onto the chosen device or any folder ([`openfader::plugin::device::Provider::export`]:
//! a new library when there is none, else the existing one is extended). The list is the collection's playlists as the export sees
//! them (ids = `Snapshot` playlist ids), ticking none exports everything. After a device export the baselines for Update
//! Collection and the settings fingerprint are kept ([`record_export`]).

use super::{App, Ctx, DeviceState, record_export, report_text};
use openfader::plugin::device::{self, Feature};
use slint::ComponentHandle;
use std::{path::PathBuf, rc::Rc};

/// Reloads the playlist list when the window is open on a tab that needs the collection (EXPORT, SYNC MANAGER, BACKUPS).
/// `count` also shows the collection size in the status line (not after a job, which has just put its result there).
pub(super) fn refresh(cx: &Rc<Ctx>, count: bool) {
    let Some(app) = cx.app() else { return };
    let s = app.global::<DeviceState>();
    if !s.get_visible() || s.get_tab() == 0 {
        return;
    }
    let Some(snap) = cx.snapshot() else { return };
    let pre = std::mem::take(&mut *cx.preselect.borrow_mut());
    cx.export.set(&snap, &|id| pre.iter().any(|p| p == id));
    if count && s.get_tab() == 1 && !cx.busy() {
        cx.say(
            &format!(
                "{} tracks, {} playlists in the collection",
                snap.len(),
                snap.playlists().count()
            ),
            false,
        );
    }
    *cx.snap.borrow_mut() = Some(snap);
}

pub(super) fn wire(cx: &Rc<Ctx>, app: &App) {
    let s = app.global::<DeviceState>();
    let c = cx.clone();
    s.on_export_toggle(move |i| {
        c.export.toggle(i as usize);
    });
    let c = cx.clone();
    s.on_export_clear(move || c.export.clear());
    let c = cx.clone();
    s.on_export_start(move || start(&c));
    let c = cx.clone();
    s.on_export_playlists(move |id| {
        *c.preselect.borrow_mut() = vec![id.to_string()];
        c.with_state(|s| {
            s.set_tab(1);
            s.set_visible(true);
        });
        refresh(&c, true);
    });
}

fn start(cx: &Rc<Ctx>) {
    let Some(folder) = cx.with_state(|s| s.get_folder().trim().to_owned()) else {
        return;
    };
    let dev = if folder.is_empty() {
        cx.current()
    } else {
        None
    };
    let dest = match (&dev, folder.is_empty()) {
        (_, false) => PathBuf::from(&folder),
        (Some(d), _) => d.mount.clone(),
        (None, true) => return cx.say("Choose a device or type a folder", true),
    };
    if dev.as_ref().is_some_and(|d| !cx.writable(d)) {
        return;
    }
    let provider = match &dev {
        Some(d) => cx.provider_for(d, Feature::Export),
        None => device::find(&dest).map(|f| f.0).or_else(|| {
            cx.say("No device plugin can write a library there", true);
            None
        }),
    };
    let Some(provider) = provider else { return };
    let Some(full) = cx.snap.borrow().clone() else {
        return cx.say("Nothing to export: reopen the window", true);
    };
    let ticked = cx.export.ticked();
    let ids: Vec<&str> = ticked.iter().map(String::as_str).collect();
    let snap = if ticked.is_empty() {
        full
    } else {
        match full.narrowed(&ids) {
            Some(s) => s,
            None => return cx.say("None of the ticked playlists has tracks", true),
        }
    };
    let (o, store) = (cx.options(dev.as_ref()), cx.store.clone());
    let dev_id = dev.as_ref().map(|d| d.id.clone());
    let name = dev
        .as_ref()
        .map_or_else(|| dest.display().to_string(), |d| d.label.clone());
    cx.start_then(
        dev_id.clone(),
        "Exporting",
        move |ctl| {
            let r = provider.export(&dest, &snap, &o, &ctl)?;
            let mut text = report_text(&r, &name);
            if let Some(id) = &dev_id {
                record_export(&store, id, &r, &mut text);
            }
            Ok(text)
        },
        |cx, _| refresh(cx, false),
    );
}
