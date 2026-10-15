//! Track filter and related-track ranking: pure functions over
//! [`Track`]s and the master deck's [`Reference`].

use super::super::camelot::related as key_related;
use super::table::{Facts, Look, Reference, bpm_close};
use openfader::{library::Track, store};

/// The filter bar's state. Combines (AND) with the search box.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Filter {
    /// BPM bounds; 0 = open. A track without a BPM fails any bound.
    pub bpm_min: f64,
    pub bpm_max: f64,
    /// Only tracks within ±6 % of the master deck's BPM.
    pub around_master: bool,
    /// Only tracks harmonically compatible with the master deck's effective key.
    pub key_match: bool,
    /// Stars, 0 = any.
    pub min_rating: u8,
}

impl Filter {
    /// Whether the result changes when the master deck's key or tempo does.
    pub fn uses_reference(&self) -> bool {
        self.around_master || self.key_match
    }

    /// A master-relative toggle is on but the reference has nothing to compare with, so it filters nothing.
    pub fn ignored(&self, r: &Reference) -> bool {
        (self.around_master && r.bpm <= 0.0) || (self.key_match && r.key.is_none())
    }
}

/// Whether `t` (whose BPM and key resolve to `x`) passes the filter. Master-relative toggles pass everything while the
/// reference is empty.
pub fn passes(t: &Track, x: &Facts, f: &Filter, r: &Reference, look: &Look) -> bool {
    if t.rating < f.min_rating {
        return false;
    }
    if (f.bpm_min > 0.0 || f.bpm_max > 0.0)
        && (x.bpm <= 0.0 || x.bpm < f.bpm_min || (f.bpm_max > 0.0 && x.bpm > f.bpm_max))
    {
        return false;
    }
    if f.around_master && r.bpm > 0.0 && !bpm_close(x.bpm, r.bpm) {
        return false;
    }
    !(f.key_match
        && r.key
            .is_some_and(|k| !x.wheel.is_some_and(|c| key_related(k, c, look.level))))
}

/// Tracks of `all` (except `anchor`) with a key compatible with `anchor`'s and a BPM within ±6 %,
/// same key first, then by BPM distance. Empty when the anchor has no readable key or no BPM.
/// Library values, else what bulk analysis detected.
pub fn related<'a>(anchor: &Track, all: &'a [Track], look: &Look) -> Vec<&'a Track> {
    let a = Facts::of(anchor);
    let Some(key) = a.wheel else {
        return Vec::new();
    };
    let mut hits: Vec<(bool, f64, &Track)> = all
        .iter()
        .filter(|t| *t != anchor)
        .filter_map(|t| {
            let x = Facts::of(t);
            let c = x.wheel?;
            (key_related(key, c, look.level) && bpm_close(x.bpm, a.bpm))
                .then(|| (c != key, (x.bpm - a.bpm).abs(), t))
        })
        .collect();
    hits.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)));
    hits.into_iter().map(|h| h.2).collect()
}

/// [`related`] with the tracks the user registered as partners of `anchor` (EXPORT mode's Matching, `partners` = their store
/// keys) first, in registration order, whatever their key and BPM; then the key / BPM matches without them.
pub fn related_with<'a>(
    anchor: &Track,
    all: &'a [Track],
    partners: &[String],
    look: &Look,
) -> Vec<&'a Track> {
    if partners.is_empty() {
        return related(anchor, all, look);
    }
    let keys: Vec<String> = all.iter().map(store::key).collect();
    let mut out: Vec<&Track> = partners
        .iter()
        .filter_map(|p| keys.iter().position(|k| k == p))
        .map(|i| &all[i])
        .filter(|t| *t != anchor)
        .collect();
    let rest: Vec<&Track> = related(anchor, all, look)
        .into_iter()
        .filter(|t| !out.contains(t))
        .collect();
    out.extend(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::camelot::camelot;

    fn passes(t: &Track, f: &Filter, r: &Reference) -> bool {
        super::passes(t, &Facts::of(t), f, r, &Look::now())
    }

    fn related<'a>(anchor: &Track, all: &'a [Track]) -> Vec<&'a Track> {
        super::related(anchor, all, &Look::now())
    }

    fn related_with<'a>(a: &Track, all: &'a [Track], partners: &[String]) -> Vec<&'a Track> {
        super::related_with(a, all, partners, &Look::now())
    }

    fn track(title: &str, key: &str, bpm: f64, rating: u8) -> Track {
        Track {
            title: title.into(),
            artist: String::new(),
            album: String::new(),
            key: key.into(),
            genre: String::new(),
            label: String::new(),
            bpm_x100: (bpm * 100.0) as i64,
            length_s: 0,
            file: None,
            analysis_ref: None,
            artwork: None,
            rating,
            comment: String::new(),
            date_added: String::new(),
            bitrate: 0,
            sample_rate: 0,
            remote: None,
        }
    }

    fn master(key: &str, bpm: f64) -> Reference {
        Reference {
            key: camelot(key),
            bpm,
        }
    }

    fn titles(v: &[&Track]) -> Vec<String> {
        v.iter().map(|t| t.title.clone()).collect()
    }

    #[test]
    fn bpm_range_and_rating_bound_the_list() {
        let (a, b, none) = (
            track("a", "8A", 120.0, 3),
            track("b", "8A", 130.0, 5),
            track("none", "8A", 0.0, 5),
        );
        let f = Filter {
            bpm_min: 125.0,
            ..Default::default()
        };
        let r = Reference::default();
        assert!(!passes(&a, &f, &r) && passes(&b, &f, &r) && !passes(&none, &f, &r));
        let f = Filter {
            bpm_max: 125.0,
            ..Default::default()
        };
        assert!(passes(&a, &f, &r) && !passes(&b, &f, &r) && !passes(&none, &f, &r));
        let f = Filter {
            bpm_min: 120.0,
            bpm_max: 130.0,
            min_rating: 4,
            ..Default::default()
        };
        assert!(!passes(&a, &f, &r) && passes(&b, &f, &r));
        assert!(
            passes(&none, &Filter::default(), &r),
            "no bound keeps unanalysed tracks"
        );
    }

    #[test]
    fn master_toggles_follow_the_reference_and_are_inert_without_one() {
        let (near, far, other_key) = (
            track("near", "9A", 126.0, 0),
            track("far", "9A", 140.0, 0),
            track("key", "3B", 125.0, 0),
        );
        let f = Filter {
            around_master: true,
            key_match: true,
            ..Default::default()
        };
        let r = master("8A", 125.0);
        assert!(passes(&near, &f, &r) && !passes(&far, &f, &r) && !passes(&other_key, &f, &r));
        assert!(!passes(&track("nokey", "", 125.0, 0), &f, &r));
        assert!(!f.ignored(&r));
        assert!(f.ignored(&Reference::default()) && passes(&far, &f, &Reference::default()));
    }

    #[test]
    fn related_lists_compatible_keys_within_six_percent_nearest_first() {
        let anchor = track("anchor", "8A", 125.0, 0);
        let all = [
            anchor.clone(),
            track("relative", "8B", 125.5, 0),
            track("same far", "8A", 130.0, 0),
            track("same near", "8A", 125.2, 0),
            track("up", "9A", 124.0, 0),
            track("clash", "3B", 125.0, 0),
            track("fast", "8A", 140.0, 0),
            track("no key", "", 125.0, 0),
        ];
        assert_eq!(
            titles(&related(&anchor, &all)),
            ["same near", "same far", "relative", "up"]
        );
        assert!(related(&track("x", "", 125.0, 0), &all).is_empty());
        assert!(related(&track("x", "8A", 0.0, 0), &all).is_empty());
    }

    #[test]
    fn registered_partners_come_first_whatever_their_key_and_tempo() {
        let anchor = track("anchor", "8A", 125.0, 0);
        let all = [
            anchor.clone(),
            track("near", "8A", 126.0, 0),
            track("partner-far", "3B", 90.0, 0),
            track("partner-near", "9A", 124.0, 0),
        ];
        let keys = |names: &[&str]| -> Vec<String> {
            names
                .iter()
                .map(|n| store::key(all.iter().find(|t| t.title == *n).unwrap()))
                .collect()
        };
        let partners = keys(&["partner-far", "partner-near", "anchor"]);
        assert_eq!(
            titles(&related_with(&anchor, &all, &partners)),
            ["partner-far", "partner-near", "near"]
        );
        assert_eq!(
            titles(&related_with(&anchor, &all, &[])),
            titles(&related(&anchor, &all))
        );
        // a partner needs no key or BPM on the anchor
        let bare = track("x", "", 0.0, 0);
        assert_eq!(
            titles(&related_with(&bare, &all, &keys(&["near"]))),
            ["near"]
        );
    }
}
