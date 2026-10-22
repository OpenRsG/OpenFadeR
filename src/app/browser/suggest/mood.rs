//! Mood: tracks with the same vibe: the phrase analysis' mood (the track's stored phrase mood, else our phrases, else the energy; High / Mid /
//! Low), energy and timbre, with a nudge for the same genre and a near tempo. Weights are ours: 45 % mood (same 1, one step
//! apart 0.5), 35 % sound ([`Features::similarity`]), 10 % genre, 10 % tempo.

use super::{Item, Verdict, closeness, same};
use openfader::analysis::Mood;

fn level(m: Mood) -> i32 {
    match m {
        Mood::Low => 0,
        Mood::Mid => 1,
        Mood::High => 2,
    }
}

pub(super) fn word(m: Mood) -> &'static str {
    match m {
        Mood::High => tr!("high"),
        Mood::Mid => tr!("mid"),
        Mood::Low => tr!("low"),
    }
}

pub(super) fn score(a: &Item, c: &Item) -> Verdict {
    let (Some(fa), Some(fc)) = (a.features(), c.features()) else {
        return Verdict::NoData;
    };
    let steps = (level(fa.mood()) - level(fc.mood())).abs();
    let genre = same(&a.track.genre, &c.track.genre);
    let tempo = if a.bpm > 0.0 && c.bpm > 0.0 {
        closeness((c.bpm - a.bpm).abs() / a.bpm, 0.2)
    } else {
        0.0
    };
    let sound = fa.similarity(fc);
    let mood = [1.0, 0.5, 0.0][steps as usize];
    let score = 0.45 * mood + 0.35 * sound + 0.10 * f32::from(u8::from(genre)) + 0.10 * tempo;
    if steps == 2 && !genre {
        return Verdict::Out; // High against Low with nothing else in common
    }
    Verdict::Score(
        score,
        tr!(
            "{}% · {} mood{}{}",
            format!("{:.0}", score * 100.0),
            word(fc.mood()),
            if steps == 0 {
                ""
            } else {
                tr!(" (one step off)")
            },
            if genre { tr!(" · same genre") } else { "" }
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::super::{Item, Links, Mode, rank, testing::*};
    use openfader::analysis::Mood;
    use openfader::analysis::similar::Features;

    fn with_mood(mut f: Features, m: Mood) -> Features {
        f.phrase_mood = Some(m);
        f
    }

    #[test]
    fn same_mood_ranks_above_a_neighbour_mood_and_the_far_mood_drops_out() {
        let sound = [0.4, 0.35, 0.25];
        let tracks = [
            track("anchor", 124.0, ""),
            track("mood-twin", 124.0, ""),
            track("mid", 124.0, ""),
            track("low", 124.0, ""),
            track("unanalysed", 124.0, ""),
        ];
        let profiles = [
            profile(Some(with_mood(feat(sound, 0.6), Mood::High)), None),
            profile(Some(with_mood(feat(sound, 0.58), Mood::High)), None),
            profile(Some(with_mood(feat(sound, 0.4), Mood::Mid)), None),
            profile(Some(with_mood(feat(sound, 0.2), Mood::Low)), None),
            profile(None, None),
        ];
        let pool: Vec<Item> = tracks
            .iter()
            .zip(&profiles)
            .map(|(t, p)| Item::new(t, Some(p), &t.title))
            .collect();
        let o = rank(
            Mode::Mood,
            &pool[0],
            &pool,
            &Default::default(),
            &Links::default(),
        );
        assert_eq!(titles(&o, &pool), ["mood-twin", "mid"]);
        assert_eq!(o.no_data, 1);
        assert!(o.picks[1].note.contains("one step off"));
    }

    #[test]
    fn without_phrases_the_energy_decides_the_mood() {
        let tracks = [
            track("anchor", 0.0, ""),
            track("loud", 0.0, ""),
            track("quiet", 0.0, ""),
        ];
        let profiles = [
            profile(Some(feat([0.4, 0.3, 0.3], 0.7)), None),
            profile(Some(feat([0.4, 0.3, 0.3], 0.65)), None),
            profile(Some(feat([0.4, 0.3, 0.3], 0.15)), None),
        ];
        let pool: Vec<Item> = tracks
            .iter()
            .zip(&profiles)
            .map(|(t, p)| Item::new(t, Some(p), &t.title))
            .collect();
        let o = rank(
            Mode::Mood,
            &pool[0],
            &pool,
            &Default::default(),
            &Links::default(),
        );
        assert_eq!(titles(&o, &pool), ["loud", "quiet"]);
        assert!(o.picks[0].score > o.picks[1].score);
    }
}
