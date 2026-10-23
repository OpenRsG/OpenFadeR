//! Smart playlists the user builds in the app: a list of conditions joined by ALL (and) or ANY (or), evaluated over
//! the whole library whenever the playlist is shown. Imported smart lists are read by
//! `library::smart`; ours live in the store as text ([`Smart::to_text`]).
//!
//! The conditions are the six the editor offers:
//! BPM range, key, minimum rating, genre contains, Tags, date added. A smart list with no condition matches nothing.

use super::super::camelot::camelot;
use super::contains_ignore_case;
use chrono::NaiveDate;
use openfader::library::Track;

#[derive(Clone, Debug, PartialEq)]
pub enum Cond {
    /// BPM bounds, inclusive; 0 = open. A track without a BPM fails any bound.
    Bpm { min: f64, max: f64 },
    /// Same key as this one, whatever notation either side is written in (compared on the Camelot wheel).
    Key(String),
    /// At least this many stars.
    Rating(u8),
    /// Genre contains this text, ignoring case.
    Genre(String),
    /// Carries this tag (store id).
    Tag(i64),
    /// Added on or after `from` and on or before `to` (`YYYY-MM-DD`, empty = open).
    Added { from: String, to: String },
}

/// Kind numbers (`Cond::fields`, the editor's kind chip): 0 BPM, 1 Key, 2 Rating, 3 Genre, 4 Tags, 5 Date added.
impl Cond {
    /// `bpm_x100` and `key` are the effective values (library, else detected); `tags` the track's tag ids.
    pub fn matches(&self, t: &Track, bpm_x100: i64, key: &str, tags: &[i64]) -> bool {
        match self {
            Cond::Bpm { min, max } => {
                let bpm = bpm_x100 as f64 / 100.0;
                bpm > 0.0 && bpm >= *min && (*max <= 0.0 || bpm <= *max)
            }
            Cond::Key(k) => camelot(k).is_some_and(|k| camelot(key) == Some(k)),
            Cond::Rating(n) => t.rating >= *n,
            Cond::Genre(g) => contains_ignore_case(&t.genre, g),
            Cond::Tag(id) => tags.contains(id),
            Cond::Added { from, to } => {
                let d = t.date_added.get(..10).unwrap_or(&t.date_added);
                !d.is_empty() && d >= from.as_str() && (to.is_empty() || d <= to.as_str())
            }
        }
    }

    /// (kind number, first box, second box) for the editor. `tag_name` shows a tag id.
    pub fn fields(&self, tag_name: impl Fn(i64) -> String) -> (i32, String, String) {
        let num = |v: f64| {
            if v > 0.0 {
                v.to_string()
            } else {
                String::new()
            }
        };
        match self {
            Cond::Bpm { min, max } => (0, num(*min), num(*max)),
            Cond::Key(k) => (1, k.clone(), String::new()),
            Cond::Rating(n) => (2, n.to_string(), String::new()),
            Cond::Genre(g) => (3, g.clone(), String::new()),
            Cond::Tag(id) => (4, tag_name(*id), String::new()),
            Cond::Added { from, to } => (5, from.clone(), to.clone()),
        }
    }

    /// Inverse of [`Cond::fields`]; the error says what to fix. `tag_id` resolves a tag name.
    pub fn from_fields(
        kind: i32,
        a: &str,
        b: &str,
        tag_id: impl Fn(&str) -> Option<i64>,
    ) -> Result<Cond, String> {
        let (a, b) = (a.trim(), b.trim());
        let bound = |s: &str| {
            if s.is_empty() {
                Ok(0.0)
            } else {
                s.replace(',', ".")
                    .parse::<f64>()
                    .ok()
                    .filter(|v| *v >= 0.0)
                    .ok_or_else(|| tr!("“{}” is not a BPM", s))
            }
        };
        let date = |s: &str| {
            if s.is_empty() {
                return Ok(String::new());
            }
            // stored as YYYY-MM-DD whatever was typed (2025-3-9): the match compares the text with the track's date
            NaiveDate::parse_from_str(s, "%Y-%m-%d")
                .map(|d| d.format("%Y-%m-%d").to_string())
                .map_err(|_| tr!("“{}” is not a date (YYYY-MM-DD)", s))
        };
        match kind {
            0 => match (bound(a)?, bound(b)?) {
                (0.0, 0.0) => Err(tr!("BPM needs a min or a max").into()),
                (min, max) => Ok(Cond::Bpm { min, max }),
            },
            1 => camelot(a)
                .map(|_| Cond::Key(a.to_owned()))
                .ok_or_else(|| tr!("“{}” is not a key", a)),
            2 => a
                .parse()
                .ok()
                .filter(|n| (1..=5).contains(n))
                .map(Cond::Rating)
                .ok_or_else(|| tr!("rating is 1 to 5 stars").to_owned()),
            3 if !a.is_empty() => Ok(Cond::Genre(a.to_owned())),
            3 => Err(tr!("genre needs some text").into()),
            4 => tag_id(a)
                .map(Cond::Tag)
                .ok_or_else(|| tr!("no tag called “{}”", a)),
            5 if a.is_empty() && b.is_empty() => {
                Err(tr!("date added needs a from or a to date").into())
            }
            5 => Ok(Cond::Added {
                from: date(a)?,
                to: date(b)?,
            }),
            _ => Err(tr!("pick a condition").into()),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Smart {
    /// ANY condition (or) instead of ALL (and).
    pub any: bool,
    pub conds: Vec<Cond>,
}

impl Smart {
    pub fn matches(&self, t: &Track, bpm_x100: i64, key: &str, tags: &[i64]) -> bool {
        let hit = |c: &Cond| c.matches(t, bpm_x100, key, tags);
        !self.conds.is_empty()
            && if self.any {
                self.conds.iter().any(hit)
            } else {
                self.conds.iter().all(hit)
            }
    }

    /// First line `all` / `any`, then one tab-separated line per condition (`bpm`, `key`, `rating`, `genre`, `mytag`, `added`).
    /// Tabs and newlines inside a value are flattened to spaces.
    pub fn to_text(&self) -> String {
        let flat = |s: &str| s.replace(['\t', '\n', '\r'], " ");
        let mut out = String::from(if self.any { "any\n" } else { "all\n" });
        for c in &self.conds {
            out += &match c {
                Cond::Bpm { min, max } => format!("bpm\t{min}\t{max}"),
                Cond::Key(k) => format!("key\t{}", flat(k)),
                Cond::Rating(n) => format!("rating\t{n}"),
                Cond::Genre(g) => format!("genre\t{}", flat(g)),
                Cond::Tag(id) => format!("mytag\t{id}"),
                Cond::Added { from, to } => format!("added\t{from}\t{to}"),
            };
            out.push('\n');
        }
        out
    }

    /// Inverse of [`Smart::to_text`]; any malformed line is an error (the text is ours, so that is corruption).
    pub fn parse(text: &str) -> Result<Smart, String> {
        let mut lines = text.lines();
        let any = match lines.next() {
            Some("all") => false,
            Some("any") => true,
            _ => return Err(tr!("smart playlist: missing all/any line").into()),
        };
        let conds = lines
            .map(|l| {
                let f: Vec<&str> = l.split('\t').collect();
                let bad = || tr!("smart playlist: bad line “{}”", l);
                let num = |s: &str| s.parse::<f64>().map_err(|_| bad());
                Ok(match f[..] {
                    ["bpm", a, b] => Cond::Bpm {
                        min: num(a)?,
                        max: num(b)?,
                    },
                    ["key", k] => Cond::Key(k.into()),
                    ["rating", n] => Cond::Rating(n.parse().map_err(|_| bad())?),
                    ["genre", g] => Cond::Genre(g.into()),
                    ["mytag", id] => Cond::Tag(id.parse().map_err(|_| bad())?),
                    ["added", a, b] => Cond::Added {
                        from: a.into(),
                        to: b.into(),
                    },
                    _ => return Err(bad()),
                })
            })
            .collect::<Result<_, String>>()?;
        Ok(Smart { any, conds })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(genre: &str, rating: u8, added: &str) -> Track {
        Track {
            genre: genre.into(),
            rating,
            date_added: added.into(),
            ..Track::default()
        }
    }

    #[test]
    fn each_condition_matches_what_it_says() {
        let t = track("Deep House", 4, "2025-03-09");
        let m = |c: Cond, bpm, key: &str, tags: &[i64]| c.matches(&t, bpm, key, tags);
        assert!(m(
            Cond::Bpm {
                min: 120.0,
                max: 130.0
            },
            12800,
            "",
            &[]
        ));
        assert!(
            m(
                Cond::Bpm {
                    min: 128.0,
                    max: 128.0
                },
                12800,
                "",
                &[]
            ),
            "bounds are inclusive"
        );
        assert!(!m(
            Cond::Bpm {
                min: 120.0,
                max: 126.0
            },
            12800,
            "",
            &[]
        ));
        assert!(
            m(
                Cond::Bpm {
                    min: 120.0,
                    max: 0.0
                },
                17400,
                "",
                &[]
            ),
            "max 0 = open"
        );
        assert!(
            !m(
                Cond::Bpm {
                    min: 0.0,
                    max: 200.0
                },
                0,
                "",
                &[]
            ),
            "no BPM fails any bound"
        );
        assert!(
            m(Cond::Key("Am".into()), 0, "8A", &[]),
            "notations compare on the wheel"
        );
        assert!(!m(Cond::Key("8B".into()), 0, "8A", &[]));
        assert!(!m(Cond::Key("8A".into()), 0, "", &[]));
        assert!(m(Cond::Rating(4), 0, "", &[]) && !m(Cond::Rating(5), 0, "", &[]));
        assert!(
            m(Cond::Genre("house".into()), 0, "", &[])
                && !m(Cond::Genre("techno".into()), 0, "", &[])
        );
        assert!(m(Cond::Tag(7), 0, "", &[3, 7]) && !m(Cond::Tag(7), 0, "", &[3]));
        let added = |f: &str, to: &str| Cond::Added {
            from: f.into(),
            to: to.into(),
        };
        assert!(m(added("2025-03-09", "2025-03-09"), 0, "", &[]));
        assert!(m(added("2025-01-01", ""), 0, "", &[]) && m(added("", "2025-12-31"), 0, "", &[]));
        assert!(!m(added("2025-03-10", ""), 0, "", &[]) && !m(added("", "2025-03-08"), 0, "", &[]));
        assert!(
            Cond::Added {
                from: "2025-03-09".into(),
                to: String::new()
            }
            .matches(&track("", 0, "2025-03-09 12:00:00"), 0, "", &[]),
            "a time after the date is ignored"
        );
        assert!(
            !added("", "2025-12-31").matches(&track("", 0, ""), 0, "", &[]),
            "no date fails"
        );
    }

    #[test]
    fn all_needs_every_condition_any_needs_one_and_empty_matches_nothing() {
        let t = track("House", 3, "");
        let conds = vec![Cond::Genre("house".into()), Cond::Rating(5)];
        assert!(
            !Smart {
                any: false,
                conds: conds.clone()
            }
            .matches(&t, 0, "", &[])
        );
        assert!(Smart { any: true, conds }.matches(&t, 0, "", &[]));
        assert!(
            !Smart::default().matches(&t, 0, "", &[])
                && !Smart {
                    any: true,
                    conds: vec![]
                }
                .matches(&t, 0, "", &[])
        );
    }

    #[test]
    fn text_round_trips_and_rejects_corruption() {
        let s = Smart {
            any: true,
            conds: vec![
                Cond::Bpm {
                    min: 120.5,
                    max: 0.0,
                },
                Cond::Key("8A".into()),
                Cond::Rating(3),
                Cond::Genre("drum | bass".into()),
                Cond::Tag(12),
                Cond::Added {
                    from: "2025-01-01".into(),
                    to: String::new(),
                },
            ],
        };
        assert_eq!(Smart::parse(&s.to_text()), Ok(s));
        assert!(
            Smart::parse("").is_err()
                && Smart::parse("all\nbpm\tx\t1").is_err()
                && Smart::parse("all\nwhat").is_err()
        );
        assert_eq!(Smart::parse("all\n"), Ok(Smart::default()));
        assert_eq!(
            Smart {
                any: false,
                conds: vec![Cond::Genre("a\tb\nc".into())]
            }
            .to_text(),
            "all\ngenre\ta b c\n"
        );
    }

    #[test]
    fn editor_fields_round_trip_and_validate() {
        let tag = |s: &str| (s == "Peak").then_some(9);
        let name = |id: i64| {
            if id == 9 {
                "Peak".to_owned()
            } else {
                "?".into()
            }
        };
        for c in [
            Cond::Bpm {
                min: 0.0,
                max: 140.0,
            },
            Cond::Key("8A".into()),
            Cond::Rating(2),
            Cond::Genre("x".into()),
            Cond::Tag(9),
            Cond::Added {
                from: String::new(),
                to: "2025-01-02".into(),
            },
        ] {
            let (k, a, b) = c.fields(name);
            assert_eq!(Cond::from_fields(k, &a, &b, tag), Ok(c));
        }
        let bad = |k, a: &str, b: &str| Cond::from_fields(k, a, b, tag).is_err();
        assert!(bad(0, "", "") && bad(0, "fast", "") && bad(0, "-1", ""));
        assert!(
            bad(1, "zz", "")
                && bad(2, "6", "")
                && bad(2, "0", "")
                && bad(3, " ", "")
                && bad(4, "Nope", "")
        );
        assert!(
            bad(5, "", "") && bad(5, "2025-13-01", "") && bad(5, "yesterday", "") && bad(9, "", "")
        );
    }

    #[test]
    fn editor_takes_a_decimal_comma_and_stores_dates_zero_padded() {
        let none = |_: &str| None;
        assert_eq!(
            Cond::from_fields(0, "125,5", "", none),
            Ok(Cond::Bpm {
                min: 125.5,
                max: 0.0
            })
        );
        let from = Cond::from_fields(5, "2025-3-9", "", none).unwrap();
        let to = Cond::from_fields(5, "", "2025-3-9", none).unwrap();
        assert_eq!(from.fields(|_| String::new()).1, "2025-03-09");
        let on = |d: &str| track("", 0, d);
        assert!(
            from.matches(&on("2025-03-09"), 0, "", &[])
                && from.matches(&on("2025-10-01"), 0, "", &[])
        );
        assert!(!from.matches(&on("2025-03-08"), 0, "", &[]));
        assert!(
            to.matches(&on("2025-03-09"), 0, "", &[]) && !to.matches(&on("2025-12-31"), 0, "", &[])
        );
    }
}
