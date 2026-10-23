//! Track filter and related-track ranking: pure functions over
//! [`Track`]s and the master deck's [`Reference`].

use super::super::camelot::{self, related as key_related};
use super::table::{bpm_close, cell_of, Facts, Look, Reference, BPM, KEY, RATING, TIME};
use super::{contains_folded, fold};
use openfader::{library::Track, store};
use std::cmp::Ordering;

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

/// Whether every word of the search `q` (lowercase; empty = everything) is in `t`'s title, artist, genre, comment or key (the
/// key as named and as shown in the chosen notation), in any of them: "daft around" finds artist Daft Punk's "Around the World".
/// Case, accents and "ß" do not matter ([`super::fold`]).
fn matches(t: &Track, x: &Facts, words: &[String], look: &Look) -> bool {
    let shown_key = camelot::display(&x.key, look.notation);
    words.iter().all(|w| {
        [
            &t.title, &t.artist, &t.genre, &x.key, &t.comment, &shown_key,
        ]
        .iter()
        .any(|f| contains_folded(f, w))
    })
}

/// The indices of `list` (whose [`Facts`] are `facts`) that match the search `q` and pass the filter bar and `tags` (the tag
/// filter), in list order.
pub fn shown(
    list: &[Track],
    facts: &[Facts],
    q: &str,
    f: &Filter,
    r: &Reference,
    look: &Look,
    tags: impl Fn(&Track) -> bool,
) -> Vec<usize> {
    let words: Vec<String> = fold(q).split_whitespace().map(str::to_owned).collect();
    (0..list.len())
        .filter(|&i| {
            matches(&list[i], &facts[i], &words, look)
                && passes(&list[i], &facts[i], f, r, look)
                && tags(&list[i])
        })
        .collect()
}

/// Orders `shown` (indices of `list`) by column `col`, descending unless `asc`; rows that tie keep their list order either
/// way. A key sorts on the Camelot wheel (1A, 1B, 2A … 12B, keys that are none last), text without regard to case and accents.
pub fn sort(
    shown: &mut [usize],
    list: &[Track],
    facts: &[Facts],
    col: usize,
    asc: bool,
    look: &Look,
) {
    let by = |o: Ordering| if asc { o } else { o.reverse() };
    match col {
        BPM => shown.sort_by(|&a, &b| by(facts[a].bpm_x100.cmp(&facts[b].bpm_x100))),
        TIME => shown.sort_by(|&a, &b| by(list[a].length_s.cmp(&list[b].length_s))),
        RATING => shown.sort_by(|&a, &b| by(list[a].rating.cmp(&list[b].rating))),
        KEY => shown.sort_by(|&a, &b| match (facts[a].wheel, facts[b].wheel) {
            (Some(x), Some(y)) => by(x.cmp(&y)),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        }),
        _ => {
            let mut keyed: Vec<(String, usize)> = shown
                .iter()
                .map(|&i| (fold(&cell_of(&list[i], &facts[i], col, look)), i))
                .collect();
            keyed.sort_by(|a, b| by(a.0.cmp(&b.0)));
            shown.iter_mut().zip(keyed).for_each(|(s, (_, i))| *s = i);
        }
    }
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
            key: key.into(),
            bpm_x100: (bpm * 100.0) as i64,
            rating,
            ..Track::default()
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

    fn listed(list: &[Track], q: &str, f: &Filter) -> Vec<String> {
        let facts: Vec<Facts> = list.iter().map(Facts::of).collect();
        shown(
            list,
            &facts,
            q,
            f,
            &Reference::default(),
            &Look::now(),
            |_| true,
        )
        .into_iter()
        .map(|i| list[i].title.clone())
        .collect()
    }

    #[test]
    fn search_matches_any_text_field_and_the_key_and_combines_with_the_filter() {
        let mut a = track("Blue Monday", "8A", 130.0, 5);
        a.artist = "New Order".into();
        let mut b = track("Strobe", "9B", 128.0, 1);
        b.comment = "peak time BLUE".into();
        let list = [a, b, track("Other", "", 0.0, 0)];
        assert_eq!(listed(&list, "", &Filter::default()).len(), 3);
        assert_eq!(
            listed(&list, "blue", &Filter::default()),
            ["Blue Monday", "Strobe"]
        );
        assert_eq!(listed(&list, "order", &Filter::default()), ["Blue Monday"]);
        assert_eq!(listed(&list, "9b", &Filter::default()), ["Strobe"]);
        let rated = Filter {
            min_rating: 3,
            ..Default::default()
        };
        assert_eq!(listed(&list, "blue", &rated), ["Blue Monday"]);
    }

    #[test]
    fn sort_orders_numbers_by_value_text_case_insensitively_and_reverses() {
        let list = [
            track("b", "10A", 90.0, 0),
            track("A", "2A", 130.0, 0),
            track("c", "1B", 100.0, 0),
        ];
        let facts: Vec<Facts> = list.iter().map(Facts::of).collect();
        let order = |col, asc| {
            let mut s = vec![0, 1, 2];
            sort(&mut s, &list, &facts, col, asc, &Look::now());
            s
        };
        assert_eq!(order(0, true), [1, 0, 2]);
        assert_eq!(order(0, false), [2, 0, 1]);
        assert_eq!(order(BPM, true), [0, 2, 1]);
        assert_eq!(
            order(super::super::table::KEY, true),
            [2, 1, 0],
            "a key sorts on the Camelot wheel: 1B, 2A, 10A"
        );
    }

    #[test]
    fn sort_puts_keys_in_wheel_order_accents_with_their_letter_and_ties_in_list_order() {
        use super::super::table::{KEY, RATING};
        let mut list: Vec<Track> = ["9A", "10A", "2B", "2A", "", "12B", "1A"]
            .iter()
            .map(|k| track(k, k, 120.0, 3))
            .collect();
        list[4].key = String::new();
        let facts: Vec<Facts> = list.iter().map(Facts::of).collect();
        let order = |col, asc| {
            let mut s: Vec<usize> = (0..list.len()).collect();
            sort(&mut s, &list, &facts, col, asc, &Look::now());
            s.into_iter()
                .map(|i| list[i].title.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(order(KEY, true), ["1A", "2A", "2B", "9A", "10A", "12B", ""]);
        assert_eq!(
            order(KEY, false),
            ["12B", "10A", "9A", "2B", "2A", "1A", ""]
        );
        assert_eq!(
            order(RATING, false),
            ["9A", "10A", "2B", "2A", "", "12B", "1A"]
        );
        let names = ["Zed", "Étienne", "Adele", "Özdemir", "Bob"].map(|n| track(n, "", 0.0, 0));
        let facts: Vec<Facts> = names.iter().map(Facts::of).collect();
        let mut s: Vec<usize> = (0..names.len()).collect();
        sort(&mut s, &names, &facts, 0, true, &Look::now());
        let sorted: Vec<&str> = s.iter().map(|&i| names[i].title.as_str()).collect();
        assert_eq!(sorted, ["Adele", "Bob", "Étienne", "Özdemir", "Zed"]);
    }

    #[test]
    fn search_needs_every_word_in_some_field_and_ignores_case_accents_and_sharp_s() {
        let mut daft = track("Around the World", "8A", 120.0, 0);
        daft.artist = "Daft Punk".into();
        let mut bjork = track("Jóga", "", 0.0, 0);
        bjork.artist = "Bj\u{f6}rk".into(); // precomposed ö
        let mut cafe = track("Cafe\u{301} del Mar", "", 0.0, 0); // decomposed é
        cafe.comment = "Straße".into();
        let turkish = track("İstanbul", "", 0.0, 0);
        let list = [daft, bjork, cafe, turkish];
        let f = Filter::default();
        assert_eq!(listed(&list, "daft punk around", &f), ["Around the World"]);
        assert_eq!(listed(&list, "around 8a", &f), ["Around the World"]);
        assert!(listed(&list, "daft jóga", &f).is_empty());
        assert_eq!(listed(&list, "bjork", &f), ["Jóga"]);
        assert_eq!(listed(&list, "joga bjork", &f), ["Jóga"]);
        assert_eq!(listed(&list, "café", &f), ["Cafe\u{301} del Mar"]);
        assert_eq!(listed(&list, "cafe\u{301}", &f), ["Cafe\u{301} del Mar"]);
        assert_eq!(listed(&list, "strasse", &f), ["Cafe\u{301} del Mar"]);
        assert_eq!(listed(&list, "straße", &f), ["Cafe\u{301} del Mar"]);
        assert_eq!(listed(&list, "istanbul", &f), ["İstanbul"]);
        assert_eq!(listed(&list, "ıstanbul", &f), ["İstanbul"]);
    }

    /// `cargo test --release --lib bench_search -- --ignored --nocapture`: the passes `View::refresh` runs per keystroke.
    #[test]
    #[ignore = "bench: 100k tracks"]
    fn bench_search_filter_and_sort_over_100k_tracks() {
        use std::time::Instant;
        const N: usize = 100_000;
        let list: Vec<Track> = (0..N)
            .map(|i| {
                let mut t = track(
                    &format!("Track number {i}"),
                    &format!("{}{}", i % 12 + 1, ["A", "B"][i % 2]),
                    90.0 + (i % 90) as f64,
                    (i % 6) as u8,
                );
                t.artist = format!("Artist {}", i % 700);
                t.genre = ["House", "Techno", "Trance", "Drum & Bass"][i % 4].into();
                t.length_s = 120 + (i % 300) as i64;
                t
            })
            .collect();
        let look = Look::now();
        let run = |name: &str, f: &mut dyn FnMut()| {
            let t = Instant::now();
            f();
            eprintln!("{name}: {:?}", t.elapsed());
            t.elapsed()
        };
        let mut facts = Vec::new();
        run("facts of 100k", &mut || {
            facts = list.iter().map(Facts::of).collect();
        });
        let r = Reference::default();
        let mut hits = 0;
        let search = run("search \"techno\"", &mut || {
            hits = shown(
                &list,
                &facts,
                "techno",
                &Filter::default(),
                &r,
                &look,
                |_| true,
            )
            .len();
        });
        assert_eq!(hits, N / 4);
        let miss = run("search \"zzzz\" (no hit)", &mut || {
            hits = shown(&list, &facts, "zzzz", &Filter::default(), &r, &look, |_| {
                true
            })
            .len();
        });
        assert_eq!(hits, 0);
        let bar = Filter {
            bpm_min: 120.0,
            min_rating: 3,
            ..Default::default()
        };
        run("filter bar bpm >= 120, 3+ stars", &mut || {
            hits = shown(&list, &facts, "", &bar, &r, &look, |_| true).len();
        });
        let mut all = shown(&list, &facts, "", &Filter::default(), &r, &look, |_| true);
        let sort_title = run("sort by title", &mut || {
            sort(&mut all, &list, &facts, 0, true, &look)
        });
        let sort_bpm = run("sort by BPM", &mut || {
            sort(&mut all, &list, &facts, BPM, false, &look)
        });
        let mut rows = 0;
        let reference = Reference::default();
        run("table rows of 100k", &mut || {
            rows = all
                .iter()
                .map(|&i| super::super::table::row(&list[i], &facts[i], &reference, false, &look))
                .count();
        });
        assert_eq!(rows, N);
        assert!(
            search.as_millis() < 10 && miss.as_millis() < 10,
            "search should take < 10 ms: {search:?}, {miss:?}"
        );
        eprintln!("sorts: {sort_title:?}, {sort_bpm:?}");
    }
}
