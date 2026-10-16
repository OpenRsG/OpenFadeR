//! Tags: categories with tags (store `mytag_categories`, `mytags`), tags assigned to tracks (`track_mytag`).
//! Managed in the sidebar's Tags section (+ NEW, RENAME, DELETE there; choosing a tag lists its tracks), assigned
//! from the browser's TAGS panel (toggles on the chosen track) and used as a filter by the TAG FILTER panel (a track
//! must carry every tag ticked there; combines with the search box and the Track Filter).
//!
//! The default categories are Genre, Components and Situation ([`DEFAULT_CATEGORIES`]); we create them when missing and keep them from being renamed or deleted. They start without tags.

use super::{playlists::Panel, App, BrowserState, Home, View};
use crate::app::trackinfo;
use openfader::{
    library::Track,
    store::{self, Store, Tag, TagGroup},
};
use slint::{ComponentHandle, Model};
use std::collections::HashMap;

pub const DEFAULT_CATEGORIES: [&str; 3] = ["Genre", "Components", "Situation"];

pub fn is_default(name: &str) -> bool {
    DEFAULT_CATEGORIES.contains(&name)
}

/// Adds the [`DEFAULT_CATEGORIES`] the store lacks (by name); set-up, not an undo step.
pub fn ensure_defaults(store: &Store) {
    let have = store.mytag_categories().unwrap_or_default();
    store.system(|| {
        for name in DEFAULT_CATEGORIES
            .iter()
            .filter(|n| !have.iter().any(|c| c.name == **n))
        {
            if let Err(e) = store.add_mytag_category(name) {
                eprintln!("mytag: {e}");
            }
        }
    })
}

/// Whether a track carrying `tags` carries every tag in `wanted` (an empty `wanted` passes everything).
pub fn has_all(tags: &[i64], wanted: &[i64]) -> bool {
    wanted.iter().all(|w| tags.contains(w))
}

/// The TAG FILTER test for `t`: its tags are looked up by store key, so nothing is read when no filter is set.
pub fn passes(tag_map: &HashMap<String, Vec<i64>>, wanted: &[i64], t: &Track) -> bool {
    wanted.is_empty()
        || has_all(
            tag_map.get(&store::key(t)).map_or(&[], Vec::as_slice),
            wanted,
        )
}

/// Panel rows: a heading per category, then its tags as `[x] name` / `[ ] name`; each row's tag id (`None` for a heading).
pub fn panel_rows(
    cats: &[TagGroup],
    tags: &[Tag],
    on: impl Fn(i64) -> bool,
) -> (Vec<String>, Vec<Option<i64>>) {
    let mut rows = Vec::new();
    for c in cats {
        rows.push((format!("— {} —", c.name), None));
        rows.extend(tags.iter().filter(|t| t.category == c.id).map(|t| {
            (
                format!("{} {}", if on(t.id) { "[x]" } else { "[ ]" }, t.name),
                Some(t.id),
            )
        }));
    }
    rows.into_iter().unzip()
}

/// `comment` with its trailing `/*…*/` tag block replaced by one listing `names` (none: the block is just removed).
/// Format: tag names in a comment block after the text.
pub fn with_tags(comment: &str, names: &[&str]) -> String {
    let text = comment.trim_end();
    let text = match text
        .strip_suffix("*/")
        .and_then(|t| t.rfind("/*").map(|i| &t[..i]))
    {
        Some(before) => before.trim_end(),
        None => text,
    };
    match (text.is_empty(), names.is_empty()) {
        (_, true) => text.to_owned(),
        (true, false) => format!("/*{}*/", names.join(" ")),
        (false, false) => format!("{text} /*{}*/", names.join(" ")),
    }
}

impl View {
    /// Add Tags to the Comments (Preferences > Advanced > Browse): the chosen track's comment lists its tags.
    fn tags_into_comment(&mut self, app: &App) {
        let Some(mut t) = self
            .selected
            .clone()
            .filter(|_| crate::app::library::settings::get().mytag_comment)
        else {
            return;
        };
        let on = self.selected_tags();
        let names: Vec<&str> = self
            .mytags
            .iter()
            .filter(|m| on.contains(&m.id))
            .map(|m| m.name.as_str())
            .collect();
        t.comment = with_tags(&t.comment, &names);
        if let Err(e) = self.store.set_comment(&store::key(&t), Some(&t.comment)) {
            return self.say(app, format!("store: {e}"));
        }
        self.track_edited(app, &t);
        trackinfo::show(app, &t);
    }

    pub(super) fn reload_tag_map(&mut self, app: &App) {
        self.tag_map = self.ok(app, self.store.all_track_mytags(), HashMap::new());
    }

    /// Tag ids of the chosen track.
    fn selected_tags(&self) -> Vec<i64> {
        self.selected
            .as_ref()
            .and_then(|t| self.tag_map.get(&store::key(t)))
            .cloned()
            .unwrap_or_default()
    }

    pub(super) fn mytag_list(&mut self, tag: i64) -> Result<Vec<Track>, String> {
        let cache = self.cache()?;
        Ok(cache
            .tracks
            .iter()
            .zip(&cache.keys)
            .filter(|(_, k)| self.tag_map.get(*k).is_some_and(|v| v.contains(&tag)))
            .map(|(t, _)| t.clone())
            .collect())
    }

    /// TAGS (`assign`: tags of the chosen track) or TAG FILTER pressed.
    pub(super) fn open_tag_panel(&mut self, app: &App, assign: bool) {
        if assign && self.selected.is_none() {
            return self.say(app, "select a track to tag");
        }
        if self.mytags.is_empty() {
            return self.say(
                app,
                "no tags yet: choose a category under Tags in the tree and + NEW > New tag",
            );
        }
        let on = if assign {
            self.selected_tags()
        } else {
            self.tag_filter.clone()
        };
        let (rows, ids) = panel_rows(&self.cats, &self.mytags, |t| on.contains(&t));
        if assign {
            self.show_panel(app, Panel::Assign(ids), "TAGS OF THE CHOSEN TRACK", rows);
        } else {
            self.show_panel(
                app,
                Panel::Filter(ids),
                "TAG FILTER (tracks need every tag ticked)",
                rows,
            );
        }
    }

    /// Row `i` (tag `id`) of the tag panel was clicked: toggles it on the chosen track / in the filter.
    pub(super) fn tag_row_clicked(&mut self, app: &App, i: usize, id: i64) {
        let assign = matches!(self.panel, Panel::Assign(_));
        let was = if assign {
            self.selected_tags().contains(&id)
        } else {
            self.tag_filter.contains(&id)
        };
        if assign {
            let Some(t) = self.selected.clone() else {
                return;
            };
            if let Err(e) = self.store.set_track_mytag(&store::key(&t), id, !was) {
                return self.say(app, format!("store: {e}"));
            }
            self.reload_tag_map(app);
        } else if was {
            self.tag_filter.retain(|t| *t != id);
        } else {
            self.tag_filter.push(id);
        }
        let name = self
            .mytags
            .iter()
            .find(|t| t.id == id)
            .map(|t| t.name.clone())
            .unwrap_or_default();
        self.panel_rows.set_row_data(
            i,
            slint::StandardListViewItem::from(
                format!("{} {name}", if was { "[ ]" } else { "[x]" }).as_str(),
            ),
        );
        // The list on screen may show other tracks now.
        if assign && matches!(self.home, Home::Tag(_) | Home::Smart(_)) {
            self.load(app);
        } else if !assign {
            self.refresh(app);
        }
        if assign {
            self.say(
                app,
                format!("{} “{name}”", if was { "removed" } else { "added" }),
            );
            self.tags_into_comment(app);
        }
        app.global::<BrowserState>()
            .set_tag_filter_count(self.tag_filter.len() as i32);
    }

    /// `t` gets tag `id` (a drop on the tag's tree row).
    pub(super) fn tag_track(&mut self, app: &App, t: &Track, id: i64) {
        let key = store::key(t);
        if let Err(e) = self.store.set_track_mytag(&key, id, true) {
            return self.say(app, format!("store: {e}"));
        }
        self.reload_tag_map(app);
        if matches!(self.home, Home::Tag(_) | Home::Smart(_)) {
            self.load(app);
        }
        let name = self
            .mytags
            .iter()
            .find(|m| m.id == id)
            .map(|m| m.name.as_str());
        self.say(
            app,
            format!("added “{}” to tag “{}”", t.title, name.unwrap_or_default()),
        );
        if self.selected.as_ref().is_some_and(|s| store::key(s) == key) {
            self.tags_into_comment(app);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_comment_block_lists_the_tags_and_is_replaced_not_stacked() {
        assert_eq!(
            with_tags("great opener", &["Warm", "Vocal"]),
            "great opener /*Warm Vocal*/"
        );
        assert_eq!(
            with_tags("great opener /*Warm Vocal*/", &["Warm"]),
            "great opener /*Warm*/"
        );
        assert_eq!(with_tags("great opener /*Warm*/", &[]), "great opener");
        assert_eq!(with_tags("", &["A"]), "/*A*/");
        assert_eq!(with_tags("/*A*/", &[]), "");
        assert_eq!(with_tags("a */ b", &["X"]), "a */ b /*X*/"); // a stray closer is ordinary text
    }
    #[test]
    fn a_track_must_carry_every_wanted_tag() {
        assert!(has_all(&[1, 2, 3], &[2, 3]));
        assert!(!has_all(&[1, 2], &[2, 3]));
        assert!(!has_all(&[], &[1]));
        assert!(
            has_all(&[], &[]) && has_all(&[5], &[]),
            "no wanted tag filters nothing"
        );
    }

    #[test]
    fn the_filter_looks_tracks_up_by_store_key() {
        let t = Track {
            title: "A".into(),
            ..Track::default()
        };
        let u = Track {
            title: "B".into(),
            ..Track::default()
        };
        let map = HashMap::from([(store::key(&t), vec![7, 8])]);
        assert!(passes(&map, &[7], &t) && passes(&map, &[7, 8], &t) && !passes(&map, &[9], &t));
        assert!(!passes(&map, &[7], &u), "untagged track");
        assert!(passes(&map, &[], &u), "no filter");
    }

    #[test]
    fn panel_rows_group_tags_under_their_category() {
        let cats = [
            TagGroup {
                id: 1,
                name: "Genre".into(),
                pos: 0,
            },
            TagGroup {
                id: 2,
                name: "Situation".into(),
                pos: 1,
            },
        ];
        let tags = [
            Tag {
                id: 10,
                category: 1,
                name: "Deep".into(),
                pos: 0,
            },
            Tag {
                id: 11,
                category: 2,
                name: "Peak".into(),
                pos: 0,
            },
            Tag {
                id: 12,
                category: 1,
                name: "Tech".into(),
                pos: 1,
            },
        ];
        let (rows, ids) = panel_rows(&cats, &tags, |t| t == 12);
        assert_eq!(
            rows,
            [
                "— Genre —",
                "[ ] Deep",
                "[x] Tech",
                "— Situation —",
                "[ ] Peak"
            ]
        );
        assert_eq!(ids, [None, Some(10), Some(12), None, Some(11)]);
    }

    #[test]
    fn default_categories_are_created_once() {
        let store = Store::in_memory().unwrap();
        ensure_defaults(&store);
        ensure_defaults(&store);
        let names: Vec<String> = store
            .mytag_categories()
            .unwrap()
            .into_iter()
            .map(|c| c.name)
            .collect();
        assert_eq!(names, DEFAULT_CATEGORIES);
        store.add_mytag_category("Mine").unwrap();
        ensure_defaults(&store);
        assert_eq!(store.mytag_categories().unwrap().len(), 4);
        assert!(is_default("Genre") && !is_default("Mine"));
    }

    #[test]
    fn assigned_tags_round_trip_through_the_store_map() {
        let store = Store::in_memory().unwrap();
        let cat = store.add_mytag_category("Genre").unwrap();
        let tag = store.add_mytag(cat, "Deep").unwrap();
        let t = Track {
            title: "A".into(),
            ..Track::default()
        };
        store.set_track_mytag(&store::key(&t), tag, true).unwrap();
        let map = store.all_track_mytags().unwrap();
        assert!(passes(&map, &[tag], &t));
        store.set_track_mytag(&store::key(&t), tag, false).unwrap();
        assert!(!passes(&store.all_track_mytags().unwrap(), &[tag], &t));
    }
}
