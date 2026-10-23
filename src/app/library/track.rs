//! The Track menu on the track chosen in the browser: Import To Collection, Analyze Track, Analysis Lock,
//! Reload Tag, Show in file manager, Relocate, Auto Relocate, Remove from Collection.

use super::{Ask, Confirm, Ctx, missing, show_in_folder};
use crate::app::{analyze, browser};
use openfader::{
    library::{Track, folder},
    store,
};
use std::rc::Rc;

pub fn act(c: &Rc<Ctx>, id: &str) {
    let Some(t) = browser::context().track else {
        return c.say(tr!("choose a track first"));
    };
    match id {
        "track-edit" => crate::app::trackinfo::open_editor(&c.app(), &t),
        "track-import" => import(c, t),
        "track-analyze" => {
            if c.store.analysis_locked(&store::key(&t)).unwrap_or(false) {
                return c.say(tr!("this track has an Analysis Lock: unlock it first"));
            }
            c.say(&tr!(
                "analysing “{}”…{}",
                t.title,
                analyze::library_grid_notice(&t)
            ));
            c.analyze(vec![t], true);
        }
        "track-lock" => lock(c, &t),
        "track-reload-tag" => reload_tag(c, &t, true),
        "track-reload-tag-discard" => reload_tag(c, &t, false),
        "track-show" => match &t.file {
            Some(f) => show_in_folder(f),
            None => c.say(tr!("this track has no file")),
        },
        "track-relocate" => c.ask_path(Ask::Relocate(vec![t]), None),
        "track-auto-relocate" => missing::auto(c, vec![t]),
        "track-remove" => {
            if !c.protected(tr!("remove tracks")) {
                let text = tr!(
                    "Remove “{}” from the Collection? Its playlist places and edits are dropped; the file is not deleted.",
                    t.title
                );
                c.confirm(
                    Confirm::Remove(vec![t]),
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

fn lock(c: &Rc<Ctx>, t: &Track) {
    let key = store::key(t);
    let on = !c.store.analysis_locked(&key).unwrap_or(false);
    match c.store.set_analysis_lock(&key, on) {
        Ok(()) => c.say(&if on {
            tr!("Analysis Lock on for “{}”", t.title)
        } else {
            tr!("Analysis Lock off for “{}”", t.title)
        }),
        Err(e) => c.say(&tr!("store: {}", e)),
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
fn reload_tag(c: &Rc<Ctx>, t: &Track, keep_edits: bool) {
    let Some(file) = t.file.as_deref().filter(|f| f.is_file()) else {
        return c.say(tr!("the file is missing: relocate it first"));
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
    let label = if keep_edits {
        tr!("Reload Tag")
    } else {
        tr!("Reload Tag (discard edits)")
    };
    match c
        .store
        .action(label, || c.store.reload_tags(&key, &merged, keep_edits))
    {
        Ok(_) => {
            c.say(&if kept.is_empty() {
                tr!("tags of “{}” reloaded", t.title)
            } else {
                tr!("Reload Tag: kept your edits of {}", kept.join(", "))
            });
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
