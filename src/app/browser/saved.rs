//! Saved searches: the search box and the filter bar (BPM bounds, ±6 % MASTER, KEY MATCH, RATING, TAG FILTER) kept under a
//! name in the [store](openfader::store) and recalled with one click from the SAVED panel (ui/browser-saved.slint).
//! Smart playlists ([`super::smart`]) are the lasting kind that lives in the tree; this is the quick one.

use super::{filter::Filter, playlists::Ask, App, BrowserState, View};
use serde::{Deserialize, Serialize};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use std::{cell::RefCell, rc::Rc};

/// What a saved search stores: JSON with only the parts that are set. A key missing from a row is its default and a key this
/// build does not know (written by a newer one) is ignored, so adding a filter never breaks the saved ones.
#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct Spec {
    #[serde(skip_serializing_if = "String::is_empty")]
    q: String,
    #[serde(skip_serializing_if = "is_zero")]
    bpm_min: f64,
    #[serde(skip_serializing_if = "is_zero")]
    bpm_max: f64,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    around: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    key: bool,
    #[serde(skip_serializing_if = "is_zero")]
    rating: u8,
    /// The tag ids the TAG FILTER requires.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tags: Vec<i64>,
}

fn is_zero<T: Default + PartialEq>(v: &T) -> bool {
    *v == T::default()
}

impl Spec {
    fn of(query: &str, f: &Filter, tags: &[i64]) -> Self {
        let bound = |b: f64| if b.is_finite() { b } else { 0.0 }; // JSON has no infinity
        Self {
            q: query.trim().to_owned(),
            bpm_min: bound(f.bpm_min),
            bpm_max: bound(f.bpm_max),
            around: f.around_master,
            key: f.key_match,
            rating: f.min_rating,
            tags: tags.to_vec(),
        }
    }

    fn filter(&self) -> Filter {
        Filter {
            bpm_min: self.bpm_min.max(0.0),
            bpm_max: self.bpm_max.max(0.0),
            around_master: self.around,
            key_match: self.key,
            min_rating: self.rating.min(5),
        }
    }

    fn encode(&self) -> String {
        serde_json::to_string(self).expect("a plain struct is JSON")
    }

    fn decode(text: &str) -> Option<Self> {
        serde_json::from_str(text).ok()
    }
}

impl View {
    /// Re-reads the saved searches from the store into the panel's list.
    pub(super) fn saved_reload(&mut self, app: &App) {
        self.saved = self.ok(app, self.store.saved_searches(), Vec::new());
        let names: Vec<SharedString> = self.saved.iter().map(|s| s.name.as_str().into()).collect();
        app.global::<BrowserState>()
            .set_saved_names(ModelRc::new(VecModel::from(names)));
    }

    /// The filter bar as the browser holds it now (the BPM boxes are parsed on every change into `self.filter`).
    fn current_spec(&self) -> Spec {
        Spec::of(&self.query, &self.filter, &self.tag_filter)
    }

    /// SAVE: asks for the name (the search text is the suggestion); nothing to keep is refused.
    fn saved_save(&mut self, app: &App) {
        if self.current_spec() == Spec::default() {
            return self.say(
                app,
                tr!("nothing to save: type a search or set a filter first"),
            );
        }
        let name = self.query.trim().to_owned();
        self.ask(app, Ask::SaveSearch, tr!("Save the search as:"), name);
    }

    /// The name bar answered [`Ask::SaveSearch`]: a saved search of that name is replaced, and the status line says so.
    pub(super) fn save_search(&mut self, app: &App, name: &str) {
        let spec = self.current_spec().encode();
        let Some(replaced) = self.try_store(app, self.store.save_search(name, &spec)) else {
            return;
        };
        self.saved_reload(app);
        self.say(
            app,
            if replaced {
                tr!("replaced the saved search “{}”", name)
            } else {
                tr!("saved the search “{}”", name)
            },
        );
    }

    /// The name bar answered [`Ask::RenameSearch`].
    pub(super) fn rename_search(&mut self, app: &App, id: i64, name: &str) {
        match self.try_store(app, self.store.rename_search(id, name)) {
            Some(true) => self.saved_reload(app),
            Some(false) => self.say(app, tr!("a saved search named “{}” exists", name)),
            None => {}
        }
    }

    /// One click on a saved search: the search box and the filters take its values as if typed, and the list is filtered.
    fn saved_recall(&mut self, app: &App, i: usize) {
        let Some(saved) = self.saved.get(i) else {
            return;
        };
        let Some(spec) = Spec::decode(&saved.spec) else {
            return self.say(app, tr!("the saved search “{}” cannot be read", saved.name));
        };
        let live: Vec<i64> = self.mytags.iter().map(|t| t.id).collect(); // a tag deleted since is dropped
        self.tag_filter = spec
            .tags
            .iter()
            .copied()
            .filter(|t| live.contains(t))
            .collect();
        self.filter = spec.filter();
        self.query = spec.q;
        let s = app.global::<BrowserState>();
        let bound = |b: f64| {
            if b > 0.0 {
                b.to_string()
            } else {
                String::new()
            }
        };
        s.set_search_text(self.query.as_str().into());
        s.set_bpm_min(bound(self.filter.bpm_min).into());
        s.set_bpm_max(bound(self.filter.bpm_max).into());
        s.set_bpm_around(self.filter.around_master);
        s.set_key_match(self.filter.key_match);
        s.set_min_rating(i32::from(self.filter.min_rating));
        s.set_saved_open(false);
        self.refresh(app);
    }
}

pub(super) fn wire(app: &App, view: &Rc<RefCell<View>>) {
    view.borrow_mut().saved_reload(app);
    let state = app.global::<BrowserState>();
    let (v, weak) = (view.clone(), app.as_weak());
    state.on_saved_save(move || v.borrow_mut().saved_save(&weak.unwrap()));
    let (v, weak) = (view.clone(), app.as_weak());
    state.on_saved_recall(move |i| {
        if let Ok(i) = usize::try_from(i) {
            v.borrow_mut().saved_recall(&weak.unwrap(), i);
        }
    });
    let (v, weak) = (view.clone(), app.as_weak());
    state.on_saved_rename(move |i| {
        let (app, mut v) = (weak.unwrap(), v.borrow_mut());
        if let Some(s) = usize::try_from(i)
            .ok()
            .and_then(|i| v.saved.get(i))
            .cloned()
        {
            v.ask(&app, Ask::RenameSearch(s.id), tr!("Rename to:"), s.name);
        }
    });
    let (v, weak) = (view.clone(), app.as_weak());
    state.on_saved_delete(move |i| {
        let (app, mut v) = (weak.unwrap(), v.borrow_mut());
        if let Some(s) = usize::try_from(i)
            .ok()
            .and_then(|i| v.saved.get(i))
            .cloned()
        {
            let done = v.store.delete_search(s.id);
            if v.try_store(&app, done).is_some() {
                v.saved_reload(&app);
                v.say(&app, tr!("deleted the saved search “{}”", s.name));
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_spec_round_trips_and_keeps_only_what_is_set() {
        let f = Filter {
            bpm_min: 120.0,
            bpm_max: 128.5,
            around_master: false,
            key_match: true,
            min_rating: 3,
        };
        let spec = Spec::of("  daft punk ", &f, &[4, 9]);
        let text = spec.encode();
        assert!(!text.contains("around"), "a default is not written: {text}");
        let back = Spec::decode(&text).unwrap();
        assert_eq!(back, spec);
        assert_eq!((back.q.as_str(), back.filter()), ("daft punk", f));
        assert_eq!(Spec::of("", &Filter::default(), &[]), Spec::default());
        assert_eq!(Spec::default().encode(), "{}");
    }

    #[test]
    fn unknown_keys_of_a_newer_build_are_ignored_and_missing_ones_default() {
        let old =
            Spec::decode(r#"{"q":"abc","key":true,"era":"1990s","mood":{"x":[1,2]}}"#).unwrap();
        assert_eq!(
            old,
            Spec {
                q: "abc".into(),
                key: true,
                ..Spec::default()
            }
        );
        assert!(Spec::decode("{}").unwrap() == Spec::default());
        assert!(Spec::decode("not json").is_none());
    }

    #[test]
    fn out_of_range_values_are_clamped_and_infinity_is_not_stored() {
        let spec = Spec::decode(r#"{"bpm_min":-5,"rating":300}"#);
        assert!(
            spec.is_none(),
            "a rating past u8 is an unreadable row, not a panic"
        );
        let spec = Spec::decode(r#"{"bpm_min":-5,"rating":9}"#)
            .unwrap()
            .filter();
        assert_eq!((spec.bpm_min, spec.min_rating), (0.0, 5));
        let f = Filter {
            bpm_max: f64::INFINITY,
            ..Filter::default()
        };
        assert_eq!(Spec::of("", &f, &[]).encode(), "{}");
    }
}
