//! Era: tracks released in a year close to the anchor's (year tag of the audio file). Same year scores 1, falling to 0 at
//! [`SPAN`] years apart; ties lean to the same genre, then the closer tempo. The window and tie weights are ours.

use super::{Item, Verdict, closeness, same};

/// Years apart at which a track stops being "of the era".
const SPAN: f64 = 30.0;

pub(super) fn score(a: &Item, c: &Item) -> Verdict {
    let (Some(ya), Some(yc)) = (a.year(), c.year()) else {
        return Verdict::NoData;
    };
    let d = i32::from(yc) - i32::from(ya);
    let era = closeness(f64::from(d.abs()), SPAN);
    if era == 0.0 {
        return Verdict::Out;
    }
    let genre = same(&a.track.genre, &c.track.genre);
    let tempo = if a.bpm > 0.0 && c.bpm > 0.0 {
        closeness((c.bpm - a.bpm).abs() / a.bpm, 0.5)
    } else {
        0.0
    };
    let score = era + 0.02 * f32::from(u8::from(genre)) + 0.01 * tempo;
    let when = match d {
        0 => "same year".to_owned(),
        d if d > 0 => format!("{} year{} later", d, if d == 1 { "" } else { "s" }),
        d => format!("{} year{} earlier", -d, if d == -1 { "" } else { "s" }),
    };
    Verdict::Score(
        score,
        format!("{yc} · {when}{}", if genre { " · same genre" } else { "" }),
    )
}

#[cfg(test)]
mod tests {
    use super::super::{Item, Links, Mode, rank, testing::*};

    #[test]
    fn nearest_year_first_and_ties_lean_to_the_same_genre() {
        let mut tracks = vec![
            track("anchor", 125.0, ""),
            track("same-year-other-genre", 125.0, ""),
            track("same-year-same-genre", 125.0, ""),
            track("minus-two", 125.0, ""),
            track("plus-five", 125.0, ""),
            track("ancient", 125.0, ""),
            track("no-tag", 125.0, ""),
        ];
        tracks[0].genre = "House".into();
        tracks[2].genre = "house".into();
        let years = [
            Some(2015),
            Some(2015),
            Some(2015),
            Some(2013),
            Some(2020),
            Some(1970),
            None,
        ];
        let profiles: Vec<_> = years.iter().map(|&y| profile(None, y)).collect();
        let pool: Vec<Item> = tracks
            .iter()
            .zip(&profiles)
            .map(|(t, p)| Item::new(t, Some(p), &t.title))
            .collect();
        let o = rank(
            Mode::Era,
            &pool[0],
            &pool,
            &Default::default(),
            &Links::default(),
        );
        assert_eq!(
            titles(&o, &pool),
            [
                "same-year-same-genre",
                "same-year-other-genre",
                "minus-two",
                "plus-five"
            ]
        );
        assert_eq!(o.no_data, 1);
        assert!(
            o.picks[2].note.contains("2 years earlier")
                && o.picks[3].note.contains("5 years later")
        );
    }

    #[test]
    fn an_anchor_without_a_year_blocks_era() {
        let t = track("a", 120.0, "");
        let p = profile(None, None);
        let pool = [Item::new(&t, Some(&p), "a")];
        let o = rank(
            Mode::Era,
            &pool[0],
            &pool,
            &Default::default(),
            &Links::default(),
        );
        assert_eq!(o.blocked, Some("the track has no year tag"));
    }
}
