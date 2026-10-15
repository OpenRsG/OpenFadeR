//! Camelot wheel helpers shared by the browser (harmonic hints) and the decks (key shift / key sync).
//! A key is `(number 1 to 12, major)`: "A" = minor = `false`, "B" = major = `true`.
//! [`display`] writes a key in the notation the user picked (PREFS).

use super::prefs::{KeyNotation, view::TrafficLight};

pub type Key = (u8, bool);

/// Camelot position of a key given either as Camelot ("10A") or as a musical name ("F#m", "Bb",
/// "Am", "C#min", "Dmaj"). Musical names map by the
/// standard Camelot wheel (Am = 8A, C = 8B; a fifth up = +1), computed from the tonic's pitch class.
pub fn camelot(key: &str) -> Option<Key> {
    let k = key.trim();
    if let Some(l @ ('A' | 'B' | 'a' | 'b')) = k.chars().last() {
        if let Ok(n @ 1..=12) = k[..k.len() - 1].parse::<u8>() {
            return Some((n, l.eq_ignore_ascii_case(&'B')));
        }
    }
    let mut it = k.chars();
    let mut pc = match it.next()?.to_ascii_uppercase() {
        'C' => 0,
        'D' => 2,
        'E' => 4,
        'F' => 5,
        'G' => 7,
        'A' => 9,
        'B' => 11,
        _ => return None,
    } as i32;
    let rest = it.as_str();
    let (acc, rest) = match rest.chars().next() {
        Some('#' | '♯') => (1, &rest[rest.chars().next()?.len_utf8()..]),
        Some('♭') => (-1, &rest['♭'.len_utf8()..]),
        Some('b') => (-1, &rest[1..]),
        _ => (0, rest),
    };
    pc += acc;
    let minor = match rest.to_lowercase().as_str() {
        "" | "maj" | "major" => false,
        "m" | "min" | "minor" => true,
        _ => return None,
    };
    let steps = (pc - if minor { 9 } else { 0 }).rem_euclid(12) * 7 % 12;
    Some((((steps + 7) % 12 + 1) as u8, !minor))
}

/// Keys that count as matching `a` at traffic-light range `level` (VIEW > Traffic Light): `Same` the key itself;
/// `Related1` also the relative key (same number, other letter); `Related2` also ±1 on the wheel with the same letter
/// (2A: 2A/2B/1A/3A); `Related3` also ±1 with the other letter (2A/2B/1A/3A/1B/3B).
/// Level 1's membership is our own choice.
pub fn related(a: Key, b: Key, level: TrafficLight) -> bool {
    let d = (a.0 as i32 - b.0 as i32).rem_euclid(12);
    let (same_letter, near) = (a.1 == b.1, d == 1 || d == 11);
    match level {
        TrafficLight::Same => same_letter && d == 0,
        TrafficLight::Related1 => d == 0,
        TrafficLight::Related2 => d == 0 || (same_letter && near),
        TrafficLight::Related3 => d == 0 || near,
    }
}

/// [`related`] at the range chosen in PREFS; what the browser's key hints and the related-tracks list use.
pub fn compatible(a: Key, b: Key) -> bool {
    related(a, b, super::prefs::with(|p| p.view.traffic_light))
}

/// `key` raised `semitones` (negative = lowered): +1 semitone is +7 on the wheel (a fifth), mod 12;
/// the letter stays.
pub fn shift((n, major): Key, semitones: i32) -> Key {
    (
        ((n as i32 - 1 + 7 * semitones).rem_euclid(12) + 1) as u8,
        major,
    )
}

/// "8A" / "8B".
pub fn name((n, major): Key) -> String {
    format!("{n}{}", if major { 'B' } else { 'A' })
}

/// Classic spelling per Camelot number 1..=12. Each key has a Camelot number, a Classic name (first spelling = the one used here,
/// second = enharmonic alias) and an Open Key number.
const CLASSIC_MINOR: [&str; 12] = [
    "Abm", "Ebm", "Bbm", "Fm", "Cm", "Gm", "Dm", "Am", "Em", "Bm", "F#m", "Dbm",
];
const CLASSIC_MAJOR: [&str; 12] = [
    "B", "F#", "Db", "Ab", "Eb", "Bb", "F", "C", "G", "D", "A", "E",
];

/// `key` in `notation`: Camelot "8A", Classic "Am"/"C", Open Key "1m"/"1d". Open Key is the
/// Beatport notation (d = dur/major, m = moll/minor; C major = 1d, A minor = 1m, +1 per fifth);
/// the number is `(camelot + 4) % 12 + 1`.
pub fn format((n, major): Key, notation: KeyNotation) -> String {
    let i = (n as usize + 11) % 12;
    match notation {
        KeyNotation::Camelot => name((n, major)),
        KeyNotation::Classic => (if major { CLASSIC_MAJOR } else { CLASSIC_MINOR })[i].to_owned(),
        KeyNotation::OpenKey => format!(
            "{}{}",
            (n as usize + 4) % 12 + 1,
            if major { 'd' } else { 'm' }
        ),
        KeyNotation::Database => name((n, major)), // callers holding the raw text use `display`
    }
}

/// A library key string (Camelot or a musical name) in `notation`; text that is no key (empty,
/// "?") comes back unchanged, and so does every key in the Database notation (as recorded).
pub fn display(key: &str, notation: KeyNotation) -> String {
    match notation {
        KeyNotation::Database => key.to_owned(),
        _ => camelot(key).map_or_else(|| key.to_owned(), |k| format(k, notation)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn camelot_and_musical_names_agree() {
        assert_eq!(camelot("10A"), Some((10, false)));
        assert_eq!(camelot("1b"), Some((1, true)));
        for (n, c) in [
            ("Am", "8A"),
            ("C", "8B"),
            ("Abm", "1A"),
            ("G#m", "1A"),
            ("B", "1B"),
            ("F#", "2B"),
            ("Gb", "2B"),
            ("Bbm", "3A"),
            ("Dbm", "12A"),
            ("C#min", "12A"),
            ("E", "12B"),
            ("Dmaj", "10B"),
            ("Bm", "10A"),
            ("Em", "9A"),
        ] {
            assert_eq!(camelot(n), camelot(c), "{n} = {c}");
        }
        assert_eq!(camelot(""), None);
        assert_eq!(camelot("13A"), None);
        assert_eq!(camelot("H"), None);
    }

    #[test]
    fn compatible_neighbours_wrap_the_wheel() {
        let k = |s| camelot(s).unwrap();
        assert!(compatible(k("12A"), k("12A")));
        assert!(compatible(k("12A"), k("1A")) && compatible(k("1A"), k("12A")));
        assert!(compatible(k("8A"), k("9A")) && compatible(k("8A"), k("7A")));
        assert!(compatible(k("8A"), k("8B")));
        assert!(!compatible(k("8A"), k("9B")) && !compatible(k("8A"), k("10A")));
    }

    #[test]
    fn traffic_light_ranges_grow() {
        use TrafficLight::*;
        let k = |s| camelot(s).unwrap();
        let keys: Vec<Key> = ["2A", "2B", "1A", "3A", "1B", "3B", "4A", "8A"]
            .map(k)
            .to_vec();
        let hits = |l| keys.iter().filter(|&&b| related(k("2A"), b, l)).count();
        assert_eq!(
            (hits(Same), hits(Related1), hits(Related2), hits(Related3)),
            (1, 2, 4, 6)
        );
        assert!(related(k("1A"), k("12A"), Related2), "the wheel wraps");
    }

    #[test]
    fn a_semitone_is_seven_wheel_steps() {
        let k = |s| camelot(s).unwrap();
        assert_eq!(shift(k("8A"), 1), k("Bbm")); // Am → A#m = 3A
        assert_eq!(shift(k("8B"), 1), k("Db")); // C → C#
        assert_eq!(shift(k("Am"), 2), k("Bm")); // 8A → 10A
        assert_eq!(shift(k("12A"), 1), k("Dm")); // C#m → Dm = 7A
        assert_eq!(shift(k("8A"), -1), k("G#m"));
        assert_eq!(shift(k("8A"), 12), k("8A"));
        assert_eq!(shift(k("5B"), 5), k("Ab")); // Eb + 5 semitones
        assert_eq!(name(k("Bbm")), "3A");
    }

    #[test]
    fn notations_match_the_reference_key_table() {
        // Camelot → (Classic, Open Key)
        for (cam, classic, open) in [
            ("1A", "Abm", "6m"),
            ("1B", "B", "6d"),
            ("2A", "Ebm", "7m"),
            ("3B", "Db", "8d"),
            ("4A", "Fm", "9m"),
            ("5B", "Eb", "10d"),
            ("7B", "F", "12d"),
            ("8A", "Am", "1m"),
            ("8B", "C", "1d"),
            ("9A", "Em", "2m"),
            ("10B", "D", "3d"),
            ("11A", "F#m", "4m"),
            ("12A", "Dbm", "5m"),
            ("12B", "E", "5d"),
        ] {
            assert_eq!(display(cam, KeyNotation::Classic), classic);
            assert_eq!(display(cam, KeyNotation::OpenKey), open);
            assert_eq!(display(cam, KeyNotation::Camelot), cam);
            // musical-name input gives the same keys
            assert_eq!(display(classic, KeyNotation::Camelot), cam);
        }
        // every Camelot key round-trips through Classic and Open Key spelling
        for n in 1..=12 {
            for major in [false, true] {
                let k = (n, major);
                assert_eq!(camelot(&format(k, KeyNotation::Classic)), Some(k));
            }
        }
        assert_eq!(display("", KeyNotation::Classic), "");
        assert_eq!(display("?", KeyNotation::OpenKey), "?");
    }
}
