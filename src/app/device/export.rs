//! EXPORT tab: the library, or the ticked playlists, onto the chosen device or a typed folder in every format picked for it
//! ([`openfader::device::export::export`]: a new library when there is none, else the existing one is extended; a folder is
//! taken as a device, [`openfader::device::folder`], and wins over the device list, which the status line says). The list is the collection's playlists as the export sees them (ids =
//! `Snapshot` playlist ids), ticking none exports everything. After a device export the baselines for Update Collection and the
//! settings fingerprint are kept ([`record_all`]).

use super::preview::{Built, Target};
use super::{record_all, App, Ctx, DeviceState};
use openfader::plugin::device as plugins;
use slint::ComponentHandle;
use std::{path::Path, rc::Rc, sync::Arc};

/// Reloads the playlist list when the window is open on a tab that needs the collection (EXPORT, SYNC MANAGER, BACKUPS).
/// `count` also shows the collection size in the status line (not after a job, which has just put its result there).
pub(super) fn refresh(cx: &Rc<Ctx>, count: bool) {
    let Some(app) = cx.app() else { return };
    let s = app.global::<DeviceState>();
    if !s.get_visible() {
        return;
    }
    cx.show_formats(); // the chips follow the tab: a typed folder's on EXPORT, the device's elsewhere
    if s.get_tab() == 0 {
        return;
    }
    let pre = std::mem::take(&mut *cx.preselect.borrow_mut());
    let Some(snap) = cx.pending() else {
        return wait_for(cx, pre);
    };
    let listed = |id: &String| snap.outline().playlists().any(|(p, _)| &p.id == id);
    if !pre.iter().all(listed) {
        return wait_for(cx, pre);
    }
    cx.preselect_tries.set(0);
    // A playlist sent from the browser replaces the ticks; otherwise the user's ticks survive tab switches and finished exports.
    let ticked = if pre.is_empty() {
        cx.export.ticked()
    } else {
        pre
    };
    cx.export
        .set(snap.outline(), &|id| ticked.iter().any(|p| p == id));
    if count && s.get_tab() == 1 && !cx.busy() {
        cx.say(
            &tr!(
                "{} tracks, {} playlists in the collection",
                snap.outline().len(),
                snap.outline().playlists().count()
            ),
            false,
        );
    }
    *cx.snap.borrow_mut() = Some(snap);
}

/// A playlist sent to EXPORT before the collection lists it (still loading): says so and looks again every half second, ticking
/// it once it is there; stops retrying after about 10 s but keeps that selection required.
fn wait_for(cx: &Rc<Ctx>, pre: Vec<String>) {
    let tries = cx.preselect_tries.get();
    if pre.is_empty() {
        return;
    }
    if tries >= 20 {
        *cx.preselect.borrow_mut() = pre;
        return cx.say(tr!("That playlist is not in the collection"), true);
    }
    if tries == 0 {
        cx.say(
            tr!("The collection is still loading: the playlist is ticked as soon as it is there"),
            false,
        );
    }
    cx.preselect_tries.set(tries + 1);
    *cx.preselect.borrow_mut() = pre;
    let c = cx.clone();
    slint::Timer::single_shot(std::time::Duration::from_millis(500), move || {
        refresh(&c, false)
    });
}

pub(super) fn wire(cx: &Rc<Ctx>, app: &App) {
    let s = app.global::<DeviceState>();
    let c = cx.clone();
    s.on_export_toggle(move |i| {
        c.preselect.borrow_mut().clear();
        c.preselect_tries.set(0);
        c.export.toggle(i as usize);
    });
    let c = cx.clone();
    s.on_export_clear(move || {
        c.preselect.borrow_mut().clear();
        c.preselect_tries.set(0);
        c.export.clear();
    });
    let c = cx.clone();
    s.on_folder_edited(move || {
        c.show_formats();
        say_target(&c);
    });
    let c = cx.clone();
    s.on_export_start(move || start(&c));
    let c = cx.clone();
    s.on_export_playlists(move |id| {
        *c.preselect.borrow_mut() = vec![id.to_string()];
        c.preselect_tries.set(0);
        c.with_state(|s| {
            s.set_tab(1);
            s.set_visible(true);
        });
        refresh(&c, true);
    });
}

/// What EXPORT writes to, said in the status line: the typed folder (it wins over the device list), else the chosen device.
pub(super) fn say_target(cx: &Ctx) {
    let folder = cx.with_state(|s| (s.get_tab() == 1).then(|| s.get_folder().trim().to_owned()));
    match (folder.flatten().filter(|f| !f.is_empty()), cx.current()) {
        (Some(f), Some(d)) => cx.say(
            &tr!("EXPORT writes to the folder {}, not to {}", f, d.label),
            false,
        ),
        (Some(f), None) => cx.say(&tr!("EXPORT writes to the folder {}", f), false),
        (None, Some(d)) => cx.say(&tr!("EXPORT writes to {}", d.label), false),
        (None, None) => {}
    }
}

fn start(cx: &Rc<Ctx>) {
    if !cx.preselect.borrow().is_empty() {
        if cx.preselect_tries.get() >= 20 {
            return cx.say(tr!("That playlist is not in the collection"), true);
        }
        return cx.say(
            tr!("The collection is still loading: the playlist is ticked as soon as it is there"),
            false,
        );
    }
    let Some(folder) = cx.with_state(|s| s.get_folder().trim().to_owned()) else {
        return;
    };
    if !folder.is_empty() && !Path::new(&folder).is_dir() {
        return cx.say(&tr!("No such folder: {}", folder), true);
    }
    let Some(dev) = cx.format_target() else {
        return cx.say(tr!("Choose a device or type a folder"), true);
    };
    if !cx.writable(&dev) {
        return;
    }
    if !dev
        .formats
        .iter()
        .any(|(_, i)| i.features.contains(&plugins::Feature::Export))
    {
        return cx.say(tr!("No device plugin can write a library there"), true);
    }
    let Some(writers) = cx.writers(&dev) else {
        return;
    };
    let Some(full) = cx.snap.borrow().clone() else {
        return cx.say(tr!("Nothing to export: reopen the window"), true);
    };
    let ticked = cx.export.ticked();
    let ids: Vec<&str> = ticked.iter().map(String::as_str).collect();
    let snap = if ticked.is_empty() {
        full
    } else {
        match full.narrowed(&ids) {
            Some(s) => s,
            None => return cx.say(tr!("None of the ticked playlists has tracks"), true),
        }
    };
    let snap = Arc::new(snap);
    let t = Target::new(cx, &dev, writers);
    let (store, target, name) = (cx.store.clone(), dev.clone(), t.name.clone());
    let (build, go_snap, go_t) = (snap.clone(), snap, t.clone());
    let (build_store, go_store) = (store.clone(), store.clone());
    cx.gate(
        &t,
        0,
        move || Box::new(move |_| Ok(Built::read(&build, &build_store))),
        move |cx| {
            cx.write(
                go_t,
                tr!("Exporting"),
                Box::new(move |_| Ok(Built::read(&go_snap, &go_store))),
                move |_, done| record_all(&store, Some(&target), done, &name),
                move |cx, _| {
                    refresh(cx, false);
                    if dev.is_folder() {
                        // The job forgets only mounted devices: the folder's chips must see the libraries now there.
                        openfader::device::forget(&dev.mount);
                        cx.show_formats();
                    } else {
                        cx.relink(&dev.id);
                    }
                },
            )
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use openfader::library::{Library, Playlist, Track};

    #[test]
    fn a_timed_out_preselection_cannot_export_an_unrelated_playlist() {
        struct Lists;
        impl Library for Lists {
            fn kind(&self) -> &str {
                "late export fixture"
            }
            fn playlists(&self) -> Result<Vec<Playlist>, String> {
                Ok(["intended", "unrelated"]
                    .into_iter()
                    .map(|id| Playlist {
                        id: id.into(),
                        name: id.into(),
                        depth: 0,
                        is_folder: false,
                    })
                    .collect())
            }
            fn tracks(&self, _: Option<&str>) -> Result<Vec<Track>, String> {
                Ok(Vec::new())
            }
        }
        crate::app::headless::init();
        let app = App::new().unwrap();
        let lib: Rc<dyn Library> = Rc::new(Lists);
        let decks = crate::app::deck::wire(&app, lib.clone(), Default::default());
        let guard = super::super::wire(&app, lib, decks);
        let cx = &guard._ctx;
        let state = app.global::<DeviceState>();
        state.set_visible(true);
        state.set_tab(1);
        cx.export.set_names(&["unrelated".into()], &|_| true);
        cx.preselect_tries.set(20);
        wait_for(cx, vec!["intended".into()]);
        refresh(cx, false);
        assert_eq!(cx.export.ticked(), ["intended"]);
        wait_for(cx, vec!["missing".into()]);
        state.invoke_export_clear();
        assert!(cx.preselect.borrow().is_empty());
        assert!(cx.export.ticked().is_empty());
    }
}
