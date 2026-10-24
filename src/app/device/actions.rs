//! Device actions: the plugin's buttons, Update Collection, import the play histories, and what the browser's Devices tree asks
//! for: import a playlist into My Playlists, delete a playlist / history / track from the device, the per-device settings.
//! Eject is `Ctx::eject` in the `backup` child (it backs up first).

use super::{App, Ctx, DeviceState};
use openfader::{
    device::{self, Device, history, update},
    library::{Library, Playlist, is_history},
    plugin::device::{Feature, RemoveTarget},
    store::{
        self,
        device_settings::{self, Behaviour},
        NodeKind, Store,
    },
};
use slint::ComponentHandle;
use std::{collections::HashSet, path::PathBuf, rc::Rc};

/// Marks a finished action job that stopped for a question (the "yes" label, a line break and the question follow).
const ASK: &str = "\u{1}ask\n";

pub(super) fn wire(cx: &Rc<Ctx>, app: &App) {
    let s = app.global::<DeviceState>();
    let c = cx.clone();
    s.on_eject(move || c.on_current(|c, d| c.eject(d, false)));
    let c = cx.clone();
    s.on_run_action(move |id| c.on_current(|c, d| run_action(c, d, &id, false)));
    let c = cx.clone();
    s.on_update_collection(move || c.on_current(update_collection));
    let c = cx.clone();
    s.on_import_histories(move || c.on_current(|c, d| import_histories(c, d, false)));
    let c = cx.clone();
    s.on_edit_settings(move || {
        if let (Some(d), Some(app)) = (c.current(), c.app()) {
            crate::app::prefs::dj_system::open_scope(&app, &d.id);
        }
    });
    let c = cx.clone();
    s.on_import_playlist(move |dev, id| import_playlist(&c, &dev, &id));
    let c = cx.clone();
    s.on_delete_playlist(move |dev, id| delete_playlist(&c, &dev, &id));
    let c = cx.clone();
    s.on_import_history(move |dev, id| {
        if let Some(d) = c.device(&dev) {
            import_histories_of(&c, &d, Some(id.to_string()), false);
        }
    });
    let c = cx.clone();
    s.on_delete_track(move |dev, file| delete_track(&c, &dev, &file));
}

/// Runs the plugin's action `id` on `d` (`confirmed`: the user answered its question with "yes"); a question of the plugin is asked
/// here and the action repeated.
fn run_action(cx: &Rc<Ctx>, d: &Device, id: &str, confirmed: bool) {
    let Some(a) = d.info.actions.iter().find(|a| a.id == id) else {
        return cx.say(
            &tr!("{}: its extension no longer offers that action. Open DEVICES and press CHECK AGAIN.", d.label),
            true,
        );
    };
    if a.writes && !cx.writable(d) {
        return;
    }
    let Some(provider) = d.provider() else {
        return cx.say(&tr!("{}: its extension is switched off. Switch it on in PREFERENCES > EXTENSIONS, then restart.", d.label), true);
    };
    let config = device_settings::config(&cx.store, &d.id);
    let (dev, id) = (d.clone(), id.to_owned());
    let again = (dev.clone(), id.clone());
    cx.start_then(
        Some(d.id.clone()),
        &a.label,
        move |ctl| {
            let r = provider.action(&dev.mount, &id, confirmed, &config, &ctl)?;
            Ok(match r.confirm {
                Some(q) => format!("{ASK}{}\n{}", q.yes, q.text),
                None => r.message,
            })
        },
        move |cx, r| {
            let Some((yes, text)) = r
                .as_ref()
                .ok()
                .and_then(|t| t.strip_prefix(ASK)?.split_once('\n'))
            else {
                return;
            };
            let (d, id) = again;
            cx.confirm(text, yes, move |c| run_action(c, &d, &id, true));
        },
    );
}

fn update_collection(cx: &Rc<Ctx>, d: &Device) {
    if !d.has_library() {
        return cx.say(
            &tr!(
                "{} has no library to read from. Export to it first.",
                d.label
            ),
            true,
        );
    }
    let Some(provider) = cx.provider_for(d, Feature::Update) else {
        return;
    };
    let Some(snap) = cx.collection() else { return };
    let (store, d) = (cx.store.clone(), d.clone());
    cx.start(Some(d.id.clone()), tr!("Updating the collection"), move |ctl| {
        let u = update::update_collection(&provider, &d.mount, &d.id, &snap, &store, &ctl)?;
        let unmatched = if u.unmatched > 0 { tr!("; {} track(s) of the device are not in the collection and were not updated", u.unmatched) } else { String::new() };
        Ok(tr!(
            "Update Collection: {} of {} track(s) changed ({} ratings, {} comments, {} colours, {} cue lists, {} grids){}",
            u.changed, u.matched, u.ratings, u.comments, u.colors, u.cues, u.grids, unmatched
        ))
    });
}

/// Imports every history of `d` (`auto`: it was the app's own doing, so nothing is said when there is nothing to do).
pub(super) fn import_histories(cx: &Rc<Ctx>, d: &Device, auto: bool) {
    import_histories_of(cx, d, None, auto);
}

fn import_histories_of(cx: &Rc<Ctx>, d: &Device, which: Option<String>, auto: bool) {
    if !d.has_library() || !d.can(Feature::Histories) {
        return;
    }
    let Some(snap) = cx.collection() else { return };
    let (store, d, provider) = (cx.store.clone(), d.clone(), d.provider());
    let config = device_settings::config(&cx.store, &d.id);
    let delete_after = Behaviour::load(&cx.store).delete_history_after_import;
    cx.start(Some(d.id.clone()), tr!("Importing histories"), move |ctl| {
        let lib = device::open_library(&d)?;
        let keys: HashSet<String> = snap.tracks().iter().map(|(t, _)| store::key(t)).collect();
        let which = which.map(|w| vec![w]);
        let r = history::import(&d.info, &d.id, &*lib, &keys, &store, which.as_deref())?;
        let mut text = tr!("Imported {} history(ies) of {}: {} plays, {} of unknown tracks skipped, {} imported before", r.histories, d.label, r.plays, r.skipped, r.already);
        if delete_after && !r.done.is_empty() {
            match provider.ok_or(tr!("its plugin is not available").to_owned()).and_then(|p| history::delete(&p, &d.mount, &r.done, &config, &ctl)) {
                Ok(n) => text.push_str(&tr!("; deleted {} from the device", n)),
                Err(e) => text.push_str(&tr!("; not deleted from the device: {}", e)),
            }
        }
        if auto && r.histories == 0 && r.already == 0 && r.skipped == 0 {
            return Ok(String::new());
        }
        Ok(text)
    });
}

/// Tracks of device playlist `at` (and below it) become a My Playlists node; tracks the collection lacks are added to it.
fn import_node(
    store: &Store,
    lib: &dyn Library,
    pls: &[Playlist],
    at: usize,
    parent: Option<i64>,
    known: &mut HashSet<String>,
    stats: &mut (usize, usize),
) -> Result<(), String> {
    let p = &pls[at];
    if p.is_folder {
        let node = store.add_node(parent, &p.name, NodeKind::Folder)?;
        for j in (at + 1..pls.len())
            .take_while(|&j| pls[j].depth > p.depth)
            .filter(|&j| pls[j].depth == p.depth + 1)
        {
            import_node(store, lib, pls, j, Some(node), known, stats)?;
        }
        return Ok(());
    }
    let tracks = lib.tracks(Some(&p.id))?;
    let fresh: Vec<_> = tracks
        .iter()
        .filter(|t| t.file.as_deref().is_some_and(|f| f.is_file()) && known.insert(store::key(t)))
        .cloned()
        .collect();
    store.collection_put(&fresh, false)?;
    let keys: Vec<String> = tracks.iter().map(store::key).collect();
    let node = store.add_node(parent, &p.name, NodeKind::Playlist)?;
    store.set_entries(node, &keys)?;
    stats.0 += keys.len();
    stats.1 += fresh.len();
    Ok(())
}

fn import_playlist(cx: &Rc<Ctx>, dev: &str, id: &str) {
    let Some(d) = cx.device_or_say(dev) else {
        return;
    };
    let run = || -> Result<String, String> {
        let lib = device::open_library(&d)?;
        let pls = lib.playlists()?;
        let at = pls
            .iter()
            .position(|p| p.id == id)
            .ok_or(tr!("that playlist is not on the device any more"))?;
        let mut known: HashSet<String> = cx.lib.tracks(None)?.iter().map(store::key).collect();
        let mut stats = (0, 0);
        import_node(&cx.store, &*lib, &pls, at, None, &mut known, &mut stats)?;
        Ok(tr!(
            "Imported {} into My Playlists: {} track(s), {} added to the collection",
            pls[at].name, stats.0, stats.1
        ))
    };
    match run() {
        Ok(m) => cx.say(&m, false),
        Err(e) => cx.say(&e, true),
    }
}

fn delete_playlist(cx: &Rc<Ctx>, dev: &str, id: &str) {
    let Some(d) = cx.device_or_say(dev) else {
        return;
    };
    let log = is_history(id);
    let Some(provider) = cx.provider_for(
        &d,
        if log {
            Feature::Histories
        } else {
            Feature::Remove
        },
    ) else {
        return;
    };
    if !cx.writable(&d) {
        return;
    }
    let pls = match device::open_library(&d).and_then(|l| l.playlists()) {
        Ok(p) => p,
        Err(e) => return cx.say(&e, true),
    };
    let (Some(path), Some(name)) = (
        device::playlist_path(&pls, id),
        pls.iter().find(|p| p.id == id).map(|p| p.name.clone()),
    ) else {
        return cx.say(&tr!("That playlist is not on the device any more"), true);
    };
    let delete_tracks = Behaviour::load(&cx.store).delete_tracks;
    let text = if log {
        tr!("Delete the history {} from {}?", name, d.label)
    } else if delete_tracks {
        tr!(
            "Delete {} from {}? Tracks that no other playlist lists are deleted too.",
            name,
            d.label
        )
    } else {
        tr!(
            "Delete {} from {}? Its tracks stay on the device.",
            name,
            d.label
        )
    };
    cx.confirm(&text, tr!("DELETE"), move |c| {
        let config = device_settings::config(&c.store, &d.id);
        c.start(Some(d.id.clone()), tr!("Deleting"), move |ctl| {
            if log {
                history::delete(&provider, &d.mount, &[name.clone()], &config, &ctl)?;
                return Ok(tr!("Deleted the history {} from {}", name, d.label));
            }
            let target = RemoveTarget::Playlist { path };
            let r = provider.remove(&d.mount, &target, delete_tracks, &config, &ctl)?;
            Ok(tr!(
                "Deleted {} ({} list(s), {} track(s)) from {}{}",
                name,
                r.playlists,
                r.tracks,
                d.label,
                r.warnings.first().map_or(String::new(), |w| tr!(
                    " — {} file(s) could not be deleted, first: {}",
                    r.warnings.len(),
                    w
                ))
            ))
        });
    });
}

fn delete_track(cx: &Rc<Ctx>, dev: &str, file: &str) {
    let Some(d) = cx.device_or_say(dev) else {
        return;
    };
    let Some(provider) = cx.provider_for(&d, Feature::Remove) else {
        return;
    };
    if !cx.writable(&d) {
        return;
    }
    let file = PathBuf::from(file);
    let name = file
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    cx.confirm(
        &tr!(
            "Delete {} from {}? The file and its analysis are removed.",
            name,
            d.label
        ),
        tr!("DELETE"),
        move |c| {
            let config = device_settings::config(&c.store, &d.id);
            c.start(Some(d.id.clone()), tr!("Deleting"), move |ctl| {
                let r = provider.remove(
                    &d.mount,
                    &RemoveTarget::Track { file },
                    false,
                    &config,
                    &ctl,
                )?;
                Ok(tr!("Deleted {} track(s) from {}", r.tracks, d.label))
            });
        },
    );
}
