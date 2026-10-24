//! The Track menu on the track chosen in the browser: Import To Collection, Analyze Track, Analysis Lock,
//! Reload Tag, Show in file manager, Relocate, Auto Relocate, Remove from Collection.

use super::{autocue, missing, show_in_folder, Ask, Confirm, Ctx};
use crate::app::{analyze, browser};
use openfader::{
    library::{Track, folder},
    store,
};
use std::rc::Rc;

pub fn act(c: &Rc<Ctx>, id: &str) {
    let ctx = browser::context();
    let Some(t) = ctx.track else {
        return c.say(tr!("choose a track first"));
    };
    let tracks = ctx.tracks;
    match id {
        "track-edit" => crate::app::trackinfo::open_editor(&c.app(), &t),
        "track-import" => import(c, t),
        "track-analyze" => analyze_tracks(c, tracks),
        "track-autocue" => autocue::start(c, tracks),
        "track-lock" => lock(c, &tracks),
        "track-reload-tag" => reload_tag(c, &tracks, true),
        "track-reload-tag-discard" => reload_tag(c, &tracks, false),
        "track-show" => match &t.file {
            Some(f) => show_in_folder(f),
            None => c.say(tr!("this track has no file")),
        },
        "track-relocate" => c.ask_path(Ask::Relocate(vec![t]), None),
        "track-auto-relocate" => missing::auto(c, tracks),
        "track-remove" => {
            if !c.protected(tr!("remove tracks")) {
                let text = match tracks.as_slice() {
                    [t] => tr!(
                        "Remove “{}” from the Collection? Its playlist places and edits are dropped; the file is not deleted.",
                        t.title
                    ),
                    many => tr!(
                        "Remove {} tracks from the Collection? Their playlist places and edits are dropped; the files are not deleted.",
                        many.len()
                    ),
                };
                c.confirm(
                    Confirm::Remove(tracks),
                    tr!("REMOVE FROM COLLECTION"),
                    &text,
                    tr!("OK"),
                    "",
                );
            }
        }
        _ => c.say(&tr!("unknown track action {}", id)),
    }
}

/// Analyze Track on the selection: tracks with an Analysis Lock are skipped, and the status says how many.
fn analyze_tracks(c: &Rc<Ctx>, tracks: Vec<Track>) {
    let (run, locked): (Vec<Track>, Vec<Track>) = tracks
        .into_iter()
        .partition(|t| !c.store.analysis_locked(&store::key(t)).unwrap_or(false));
    match (run.as_slice(), locked.len()) {
        ([], 1) => c.say(tr!("this track has an Analysis Lock: unlock it first")),
        ([t], 0) => c.say(&tr!(
            "analysing “{}”…{}",
            t.title,
            analyze::library_grid_notice(t)
        )),
        (run, skipped) => {
            let mut msg = if run.is_empty() {
                String::new()
            } else {
                tr!("analysing {} tracks…", run.len())
            };
            if skipped > 0 {
                if !msg.is_empty() {
                    msg += " · ";
                }
                msg += &tr!("{} skipped (Analysis Lock)", skipped);
            }
            c.say(&msg);
        }
    }
    if !run.is_empty() {
        c.analyze(run, true);
    }
}

/// Import To Collection: a track of a device or another library joins ours.
fn import(c: &Rc<Ctx>, t: Track) {
    let Some(file) = t.file.clone() else {
        return c.say(tr!("this track has no file"));
    };
    if c.known().contains(&file) {
        return c.say(tr!("already in the Collection"));
    }
    match c.store.collection_put(std::slice::from_ref(&t), false) {
        Ok(_) => {
            c.say(&tr!("“{}” imported to the Collection", t.title));
            c.reload(None);
            c.analyze(vec![t], false);
        }
        Err(e) => c.say(&tr!("store: {}", e)),
    }
}

/// Analysis Lock: every selected track gets the same new state (all locked, unless they all are already).
fn lock(c: &Rc<Ctx>, tracks: &[Track]) {
    let keys: Vec<String> = tracks.iter().map(store::key).collect();
    let on = keys
        .iter()
        .any(|k| !c.store.analysis_locked(k).unwrap_or(false));
    let done = keys
        .iter()
        .try_for_each(|k| c.store.set_analysis_lock(k, on));
    match (done, tracks) {
        (Ok(()), [t]) => c.say(&if on {
            tr!("Analysis Lock on for “{}”", t.title)
        } else {
            tr!("Analysis Lock off for “{}”", t.title)
        }),
        (Ok(()), _) if on => c.say(&tr!("Analysis Lock on for {} tracks", tracks.len())),
        (Ok(()), _) => c.say(&tr!("Analysis Lock off for {} tracks", tracks.len())),
        (Err(e), _) => c.say(&tr!("store: {}", e)),
    }
}

/// What a re-read of the file's tags changes on `t`: tag fields come from the file, BPM and key only when the file has them.
fn retagged(t: &Track, fresh: Track) -> Track {
    fn or<T: Default + PartialEq>(fresh: T, old: T) -> T {
        if fresh == T::default() { old } else { fresh }
    }
    Track {
        title: fresh.title,
        artist: fresh.artist,
        album: fresh.album,
        genre: fresh.genre,
        comment: fresh.comment,
        key: if fresh.key.is_empty() {
            t.key.clone()
        } else {
            fresh.key
        },
        bpm_x100: or(fresh.bpm_x100, t.bpm_x100),
        length_s: or(fresh.length_s, t.length_s),
        bitrate: or(fresh.bitrate, t.bitrate),
        sample_rate: or(fresh.sample_rate, t.sample_rate),
        ..t.clone()
    }
}

/// Reload Tag: the file's tags replace what the track shows (a Collection row shadows the source's, which stays untouched).
/// `keep_edits`: the text the user edited stays over the file's, and the status names the kept edits that differ from the file.
/// Tracks whose file is gone are left alone and counted.
fn reload_tag(c: &Rc<Ctx>, ts: &[Track], keep_edits: bool) {
    let mut jobs = Vec::new();
    for t in ts {
        let Some(file) = t.file.as_deref().filter(|f| f.is_file()) else {
            continue;
        };
        // The row's own values are the base: what the user edited (the overrides) is not the file's.
        let row =
            t.id.and_then(|id| c.store.collection_track(id).ok().flatten());
        let merged = retagged(row.as_ref().unwrap_or(t), folder::read_track(file));
        let key = c.store.key_of(t);
        let kept = if keep_edits {
            let e = c.store.edits(&key).unwrap_or_default();
            [
                (tr!("Title"), &e.title, &merged.title),
                (tr!("Artist"), &e.artist, &merged.artist),
                (tr!("Album"), &e.album, &merged.album),
                (tr!("Genre"), &e.genre, &merged.genre),
                (tr!("Label"), &e.label, &merged.label),
                (tr!("Comment"), &e.comment, &merged.comment),
            ]
            .into_iter()
            .filter(|(_, edit, file)| edit.as_ref().is_some_and(|v| v != *file))
            .map(|(name, ..)| name)
            .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        jobs.push((key, merged, kept));
    }
    if jobs.is_empty() {
        return c.say(tr!("the file is missing: relocate it first"));
    }
    let label = if keep_edits {
        tr!("Reload Tag")
    } else {
        tr!("Reload Tag (discard edits)")
    };
    let done = c.store.action(label, || {
        jobs.iter()
            .try_for_each(|(key, merged, _)| c.store.reload_tags(key, merged, keep_edits))
    });
    match done {
        Ok(()) => {
            let mut msg = match (ts, jobs.as_slice()) {
                ([t], [(.., kept)]) if kept.is_empty() => tr!("tags of “{}” reloaded", t.title),
                ([_], [(.., kept)]) => tr!("Reload Tag: kept your edits of {}", kept.join(", ")),
                _ => tr!("tags of {} tracks reloaded", jobs.len()),
            };
            if ts.len() > jobs.len() && ts.len() > 1 {
                msg += &format!(" · {}", tr!("{} files are missing", ts.len() - jobs.len()));
            }
            c.say(&msg);
            c.reload(None);
        }
        Err(e) => c.say(&tr!("store: {}", e)),
    }
}

/// Remove from Collection confirmed.
pub fn remove(c: &Rc<Ctx>, tracks: Vec<Track>) {
    c.remove_and_say(&tracks);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reloaded_tags_replace_text_but_keep_analysis_values_the_file_lacks() {
        let old = Track {
            title: "Old".into(),
            artist: "A".into(),
            key: "8A".into(),
            bpm_x100: 12800,
            length_s: 200,
            rating: 3,
            file: Some("/m/a.mp3".into()),
            ..Track::default()
        };
        let fresh = Track {
            title: "New".into(),
            key: String::new(),
            bpm_x100: 0,
            length_s: 201,
            ..Track::default()
        };
        let t = retagged(&old, fresh);
        assert_eq!(
            (
                t.title.as_str(),
                t.artist.as_str(),
                t.key.as_str(),
                t.bpm_x100,
                t.length_s,
                t.rating
            ),
            ("New", "", "8A", 12800, 201, 3)
        );
        assert_eq!(t.file, old.file);
    }
}
