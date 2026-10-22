//! Association: tracks tied to the anchor by the same label, composer, artist or album, and by sitting together in
//! playlists and play histories ([`Links`]). Weights are ours: label 0.3, composer 0.3, artist 0.2, album 0.1, and up to 0.4
//! for playlist / history co-occurrence (a pair weight of 1 or more fills it).

use super::{Item, Links, Verdict, same};

/// A track shares an artist when any of the ", "-joined names of one is a name of the other.
fn shares_artist(a: &str, c: &str) -> bool {
    a.split(',').any(|x| c.split(',').any(|y| same(x, y)))
}

pub(super) fn score(a: &Item, c: &Item, links: &Links) -> Verdict {
    let mut score = 0.0;
    let mut why: Vec<String> = Vec::new();
    for (hit, weight, text) in [
        (same(a.label(), c.label()), 0.3, tr!("label {}", c.label())),
        (
            same(a.composer(), c.composer()),
            0.3,
            tr!("composer {}", c.composer()),
        ),
        (
            shares_artist(&a.track.artist, &c.track.artist),
            0.2,
            tr!("same artist").to_owned(),
        ),
        (
            same(&a.track.album, &c.track.album),
            0.1,
            tr!("same album").to_owned(),
        ),
    ] {
        if hit {
            score += weight;
            why.push(text);
        }
    }
    let w = links.weight(c.key);
    if w > 0.0 {
        score += 0.4 * w.min(1.0);
        why.push(if w >= 1.0 {
            tr!("often together in lists").into()
        } else {
            tr!("together in a list").into()
        });
    }
    if score == 0.0 {
        return Verdict::Out;
    }
    Verdict::Score(score, why.join(" · "))
}

#[cfg(test)]
mod tests {
    use super::super::{Item, Links, Mode, max_list, rank, testing::*};

    fn keys(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn links_weigh_short_lists_and_neighbours_more() {
        // the anchor sits beside "next" in a 4-track list, and far from "far" in the same list
        let l = Links::from_lists("a", &[keys(&["a", "next", "x", "far"])], 100);
        assert!((l.weight("next") - 2.0 / 2.0).abs() < 1e-6);
        assert!((l.weight("far") - 1.0 / 2.0).abs() < 1e-6);
        assert_eq!(l.weight("a"), 0.0);
        // lists without the anchor add nothing; a second list adds
        let l = Links::from_lists(
            "a",
            &[
                keys(&["b", "c"]),
                keys(&["a", "b"]),
                keys(&["b", "a", "c", "d", "e", "f", "g", "h", "i"]),
            ],
            100,
        );
        assert!(l.weight("b") > l.weight("c") && l.weight("c") > 0.0);
        assert_eq!(l.weight("zzz"), 0.0);
        // a list longer than the limit (a playlist of the whole collection) says nothing
        let dump: Vec<String> = (0..30).map(|i| format!("t{i}")).collect();
        assert_eq!(
            Links::from_lists("t0", &[dump.clone()], 29).weight("t1"),
            0.0
        );
        assert!(Links::from_lists("t0", &[dump], 30).weight("t1") > 0.0);
        assert_eq!((max_list(5), max_list(60), max_list(5000)), (10, 30, 100));
    }

    #[test]
    fn same_label_composer_and_artist_and_playlist_company_rank_a_track() {
        let mut tracks = vec![
            track("anchor", 0.0, ""),
            track("same-label", 0.0, ""),
            track("same-artist", 0.0, ""),
            track("playlist-mate", 0.0, ""),
            track("stranger", 0.0, ""),
            track("label-and-artist", 0.0, ""),
        ];
        tracks[0].label = "Drumcode".into();
        tracks[0].artist = "Adam Beyer, Ida Engberg".into();
        tracks[1].label = "drumcode".into();
        tracks[2].artist = "ida engberg".into();
        tracks[5].label = "Drumcode".into();
        tracks[5].artist = "Adam Beyer".into();
        let links = Links::from_lists("anchor", &[keys(&["anchor", "playlist-mate"])], 100);
        let profiles: Vec<_> = tracks.iter().map(|_| profile(None, None)).collect();
        let pool: Vec<Item> = tracks
            .iter()
            .zip(&profiles)
            .map(|(t, p)| Item::new(t, Some(p), &t.title))
            .collect();
        let o = rank(
            Mode::Association,
            &pool[0],
            &pool,
            &Default::default(),
            &links,
        );
        assert_eq!(
            titles(&o, &pool),
            [
                "label-and-artist",
                "playlist-mate",
                "same-label",
                "same-artist"
            ]
        );
        assert!(
            o.picks[0].note.contains("label Drumcode") && o.picks[0].note.contains("same artist")
        );
        assert!(o.picks[1].note.contains("together"));
    }
}
