//! Library health: the problems of the Collection, one [`Check`] each, and the scan that finds them (the browser's "Library
//! health" sidebar node lists the tracks of a check, with its fixes: `app::browser::health`).
//!
//! The scan is pure data in, data out, so it runs on a worker thread: [`Scanner::run`] takes the Collection as the browser shows it
//! (the store's edits laid over the rows), reads what the disk says about each track once ([`probe`]: the file, the cover,
//! the analysis cache's first bytes) and judges every track ([`judge`]) against the user's [`Limits`]. A [`Scanner`] remembers
//! the probes, so the next run only probes the tracks that changed and those whose probe could still improve (a file missing,
//! a track not analysed yet); a full run probes everything again. What a user ignored is not known here: the browser takes
//! it from the store ([`openfader::store::Store::health_ignores`]) when it shows a check, so ignoring needs no scan.
//!
//! Checks and what counts (the thresholds are Preferences > Advanced > Browse > Library health):
//! * **Missing file**: the file is not there (the fix is Relocate, [`super::missing`]).
//! * **Corrupt or undecodable**: the file is there but the last load or analysis failed ([`openfader::store::Store::health_note_failure`]).
//! * **Not analysed**: no up-to-date entry in the analysis cache and no analysis from the library the track came from.
//! * **Missing tags**: no title, artist, genre, key or BPM; the key and BPM our analysis found count as present.
//! * **No artwork**: no cover file.
//! * **No rating**: zero stars.
//! * **Very long**: at least the threshold in minutes (a mix or a podcast); 0 switches the check off.
//! * **Probably not music**: our own heuristic ([`spoken`]): a genre, a folder or a file type that says audiobook, podcast or
//!   spoken word; a title like "Chapter 3" or "Episode 12" of 5 minutes or more; or a track of 15 minutes or more whose analysis found
//!   no beat at all.
//! * **Duplicates**: the Duplicate tracks finder's groups ([`super::dupes::find`]).
//! * **Low bitrate**: a known bitrate below the threshold in kbps; 0 switches the check off.

use super::{dupes, missing, Ctx};
use crate::app::browser;
use openfader::{
    analysis::cache::{self, Glance},
    library::Track,
    store::Store,
};
use std::{
    collections::{HashMap, HashSet},
    path::Path,
    rc::Rc,
};

/// One kind of problem.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Check {
    Missing,
    Corrupt,
    NotAnalysed,
    Tags,
    Artwork,
    Rating,
    Long,
    NotMusic,
    Duplicate,
    LowBitrate,
}

impl Check {
    /// In sidebar order; a check's position here is its index in [`Report::found`].
    pub const ALL: [Check; 10] = [
        Check::Missing,
        Check::Corrupt,
        Check::NotAnalysed,
        Check::Tags,
        Check::Artwork,
        Check::Rating,
        Check::Long,
        Check::NotMusic,
        Check::Duplicate,
        Check::LowBitrate,
    ];

    pub fn index(self) -> usize {
        Check::ALL.iter().position(|&c| c == self).unwrap_or(0)
    }

    /// What the store keeps ignores under: never changes.
    pub fn id(self) -> &'static str {
        match self {
            Check::Missing => "missing",
            Check::Corrupt => "corrupt",
            Check::NotAnalysed => "not-analysed",
            Check::Tags => "tags",
            Check::Artwork => "artwork",
            Check::Rating => "rating",
            Check::Long => "long",
            Check::NotMusic => "not-music",
            Check::Duplicate => "duplicate",
            Check::LowBitrate => "low-bitrate",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Check::Missing => tr!("Missing file"),
            Check::Corrupt => tr!("Corrupt or undecodable"),
            Check::NotAnalysed => tr!("Not analysed"),
            Check::Tags => tr!("Missing tags"),
            Check::Artwork => tr!("No artwork"),
            Check::Rating => tr!("No rating"),
            Check::Long => tr!("Very long"),
            Check::NotMusic => tr!("Probably not music"),
            Check::Duplicate => tr!("Duplicates"),
            Check::LowBitrate => tr!("Low bitrate"),
        }
    }

    /// What the check found and what fixes it, for the status line.
    pub fn hint(self, lim: &Limits) -> String {
        match self {
            Check::Missing => tr!("The file is gone: RELOCATE picks the new file, AUTO RELOCATE ALL searches your music folders").into(),
            Check::Corrupt => tr!("The last load or analysis of the file failed: replace the file and RELOCATE, or REMOVE it").into(),
            Check::NotAnalysed => tr!("No beat grid or key stored yet: ANALYSE").into(),
            Check::Tags => tr!("No title, artist, genre, key or BPM: EDIT TAGS, or ANALYSE for the key and BPM").into(),
            Check::Artwork => tr!("No cover image").into(),
            Check::Rating => tr!("Zero stars: rate the track in the track panel").into(),
            Check::Long => tr!("Longer than {} min, likely a mix or a podcast: IGNORE it or REMOVE it", lim.long_min),
            Check::NotMusic => tr!("Looks like an audiobook, a podcast or speech: REMOVE it, or IGNORE it").into(),
            Check::Duplicate => tr!("Same title, artist and length as another track: Duplicated File Manager merges them").into(),
            Check::LowBitrate => tr!("Below {} kbps: look for a better file", lim.low_kbps),
        }
    }
}

/// The thresholds the user sets; 0 switches the check off.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// "Very long": minutes.
    pub long_min: u32,
    /// "Low bitrate": kbps.
    pub low_kbps: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            long_min: 20,
            low_kbps: 128,
        }
    }
}

/// What the disk says about one track.
#[derive(Clone, Debug, PartialEq)]
struct Facts {
    file: bool,
    artwork: bool,
    /// The analysis cache's entry for the file.
    analysis: Option<Glance>,
}

impl Facts {
    /// A probe that cannot get better by itself: the file is there and analysed. The others are probed again on every run.
    fn settled(&self) -> bool {
        self.file && self.analysis.is_some()
    }
}

/// Where a scan looks for the analysis of a track.
type Analysis = Box<dyn Fn(&Track) -> Option<Glance> + Send>;

fn probe(t: &Track, analysis: &Analysis) -> Facts {
    let file = t.file.as_deref().is_some_and(Path::is_file);
    Facts {
        file,
        artwork: t.artwork.as_deref().is_some_and(Path::is_file),
        analysis: if file { analysis(t) } else { None },
    }
}

/// Genre words of speech: matched in the lower-cased genre.
const SPOKEN_GENRES: [&str; 10] = [
    "audiobook",
    "audio book",
    "podcast",
    "spoken",
    "speech",
    "lecture",
    "sermon",
    "interview",
    "talk radio",
    "comedy",
];
/// Folder-name words: matched in each lower-cased folder of the path.
const SPOKEN_FOLDERS: [&str; 5] = [
    "audiobook",
    "audio book",
    "podcast",
    "spoken word",
    "lecture",
];
/// File types made for audiobooks (M4B, Audible's AAX and AA).
const SPOKEN_TYPES: [&str; 3] = ["m4b", "aax", "aa"];
/// A "Chapter 3" / "Episode 12" title says speech from this length (s) on.
const CHAPTER_MIN_S: i64 = 5 * 60;
/// A track this long (s) whose analysis found no beat at all is speech or ambient noise.
const BEATLESS_MIN_S: i64 = 15 * 60;

/// Whether `t` looks like an audiobook, a podcast or speech. Inferred: our own heuristic, see the module docs. `beatless`: the
/// track is analysed and the analysis found no beat.
fn spoken(t: &Track, beatless: bool) -> bool {
    let has = |text: &str, words: &[&str]| {
        let text = text.to_lowercase();
        words.iter().any(|w| text.contains(w))
    };
    let file = t.file.as_deref();
    let kind = file
        .and_then(Path::extension)
        .map(|e| e.to_string_lossy().to_lowercase());
    let title = t.title.trim().to_lowercase();
    let numbered = ["chapter", "episode", "ep.", "ep"]
        .iter()
        .filter_map(|w| title.strip_prefix(w))
        .any(|rest| rest.trim_start().starts_with(|c: char| c.is_ascii_digit()));
    has(&t.genre, &SPOKEN_GENRES)
        || file.and_then(Path::parent).is_some_and(|dir| {
            dir.components()
                .any(|c| has(&c.as_os_str().to_string_lossy(), &SPOKEN_FOLDERS))
        })
        || kind.is_some_and(|k| SPOKEN_TYPES.contains(&k.as_str()))
        || (numbered && t.length_s >= CHAPTER_MIN_S)
        || (beatless && t.length_s >= BEATLESS_MIN_S)
}

/// The checks `t` fails, as a bit set over [`Check::index`]; [`Check::Duplicate`] is not decided per track.
fn judge(t: &Track, f: &Facts, failed: bool, lim: &Limits) -> u16 {
    let found = f.analysis.as_ref();
    let (bpm, key) = (
        t.bpm_x100 > 0 || found.is_some_and(|a| a.bpm_x100 > 0),
        !t.key.trim().is_empty() || found.is_some_and(|a| a.key.is_some()),
    );
    let blank = |s: &str| s.trim().is_empty();
    let mut set = 0u16;
    let mut flag = |c: Check, on: bool| set |= u16::from(on) << c.index();
    flag(Check::Missing, !f.file);
    flag(Check::Corrupt, f.file && failed);
    flag(
        Check::NotAnalysed,
        f.file && found.is_none() && t.analysis_ref.is_none(),
    );
    flag(
        Check::Tags,
        blank(&t.title) || blank(&t.artist) || blank(&t.genre) || !key || !bpm,
    );
    flag(Check::Artwork, !f.artwork);
    flag(Check::Rating, t.rating == 0);
    flag(
        Check::Long,
        lim.long_min > 0 && t.length_s >= i64::from(lim.long_min) * 60,
    );
    flag(
        Check::NotMusic,
        spoken(t, found.is_some_and(|a| a.bpm_x100 == 0) && !bpm),
    );
    flag(
        Check::LowBitrate,
        lim.low_kbps > 0 && t.bitrate > 0 && t.bitrate < lim.low_kbps,
    );
    set
}

/// What a scan found: per check ([`Check::index`]) the Collection row ids it lists, in library order, ignores not applied.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Report {
    pub found: Vec<Vec<i64>>,
    /// Why a corrupt file failed, by row id.
    pub failures: HashMap<i64, String>,
}

impl Report {
    pub fn of(&self, c: Check) -> &[i64] {
        self.found.get(c.index()).map_or(&[], Vec::as_slice)
    }
}

/// The ids of `found` a check shows: those the user has not ignored, or with `ignored` on those they have.
pub fn shown(found: &[i64], ignores: Option<&HashSet<i64>>, ignored: bool) -> Vec<i64> {
    found
        .iter()
        .copied()
        .filter(|id| ignores.is_some_and(|s| s.contains(id)) == ignored)
        .collect()
}

/// The probes of the last run, kept so the next one reads less.
pub struct Scanner {
    analysis: Analysis,
    prev: HashMap<i64, (Track, Facts)>,
}

impl Scanner {
    /// A scanner reading analyses from the analysis cache on disk.
    pub fn disk() -> Scanner {
        Scanner::reading(Box::new(cache::glance))
    }

    fn reading(analysis: Analysis) -> Scanner {
        Scanner {
            analysis,
            prev: HashMap::new(),
        }
    }

    /// Scans `tracks` (Collection rows). `full`: probe every track again; else a track that is unchanged since the last run and
    /// settled ([`Facts::settled`]) keeps its probe.
    pub fn run(&mut self, tracks: &[Track], store: &Store, lim: &Limits, full: bool) -> Report {
        let failures = store.health_failures().unwrap_or_default();
        let mut twins = vec![false; tracks.len()];
        for i in dupes::find(tracks).into_iter().flatten() {
            twins[i] = true;
        }
        let mut found = vec![Vec::new(); Check::ALL.len()];
        let mut kept = HashMap::with_capacity(tracks.len());
        for (t, twin) in tracks.iter().zip(twins) {
            let Some(id) = t.id else { continue };
            let facts = match self.prev.remove(&id) {
                Some((was, f)) if !full && was == *t && f.settled() => f,
                _ => probe(t, &self.analysis),
            };
            let set = judge(t, &facts, failures.contains_key(&id), lim)
                | u16::from(twin) << Check::Duplicate.index();
            for c in Check::ALL {
                if set >> c.index() & 1 == 1 {
                    found[c.index()].push(id);
                }
            }
            kept.insert(id, (t.clone(), facts));
        }
        self.prev = kept;
        Report { found, failures }
    }
}

/// Acts of the health bar that need the library context (see `ui/browser.slint`): `health-analyze-all`, `health-relocate-all`.
pub fn act(c: &Rc<Ctx>, id: &str) {
    let list = browser::context().list;
    if list.is_empty() {
        return c.say(tr!("nothing is listed"));
    }
    match id {
        "health-analyze-all" => {
            c.say(&tr_n!(
                "analysing {n} track…",
                "analysing {n} tracks…",
                list.len()
            ));
            c.analyze(list, true);
        }
        "health-relocate-all" => missing::auto(c, list),
        _ => c.say(&tr!("unknown health action {}", id)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        path::PathBuf,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        },
    };

    /// A small Collection in a temp folder: every track starts clean and `put` changes one thing.
    struct Lib {
        dir: PathBuf,
        store: Store,
        analysed: Arc<std::sync::Mutex<HashMap<PathBuf, Glance>>>,
        probes: Arc<AtomicUsize>,
        scanner: Scanner,
    }

    fn glance() -> Glance {
        Glance {
            bpm_x100: 12800,
            key: Some("8A".into()),
        }
    }

    impl Lib {
        fn new(name: &str) -> Lib {
            let dir = std::env::temp_dir()
                .join(format!("openfader-health-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(dir.join("music")).unwrap();
            let analysed = Arc::new(std::sync::Mutex::new(HashMap::new()));
            let probes = Arc::new(AtomicUsize::new(0));
            let (a, p) = (analysed.clone(), probes.clone());
            let scanner = Scanner::reading(Box::new(move |t| {
                p.fetch_add(1, Ordering::Relaxed);
                a.lock().unwrap().get(t.file.as_ref()?).cloned()
            }));
            Lib {
                dir,
                store: Store::in_memory().unwrap(),
                analysed,
                probes,
                scanner,
            }
        }

        /// Adds a track that passes every check (named `name`, a real file, a cover, analysed, four stars), changed by `f`.
        /// Returns its row id.
        fn put(&self, name: &str, f: impl FnOnce(&mut Track)) -> i64 {
            let file = self.dir.join("music").join(format!("{name}.wav"));
            let cover = self.dir.join(format!("{name}.jpg"));
            std::fs::write(&file, name).unwrap();
            std::fs::write(&cover, b"jpg").unwrap();
            let mut t = Track {
                title: name.into(),
                artist: "Band".into(),
                genre: "House".into(),
                key: "8A".into(),
                bpm_x100: 12800,
                length_s: 200,
                bitrate: 320,
                rating: 4,
                artwork: Some(cover),
                file: Some(file.clone()),
                ..Track::default()
            };
            self.analysed.lock().unwrap().insert(file, glance());
            f(&mut t);
            // The rating is the store's edit laid over the row, not a column: the test lays it on in `tracks`.
            let rating = t.rating;
            let id = self.store.collection_upsert(&t, None).unwrap();
            self.store
                .set_rating(&format!("t{id}"), Some(rating))
                .unwrap();
            id
        }

        /// The Collection as the browser lists it (edits on).
        fn tracks(&self) -> Vec<Track> {
            let mut rows = self.store.collection_tracks().unwrap().to_vec();
            for t in &mut rows {
                if let Some(r) = self
                    .store
                    .edits(&format!("t{}", t.id.unwrap()))
                    .unwrap()
                    .rating
                {
                    t.rating = r;
                }
            }
            rows
        }

        fn scan(&mut self, full: bool) -> Report {
            let rows = self.tracks();
            self.scanner
                .run(&rows, &self.store, &Limits::default(), full)
        }

        /// The ids `check` lists after a full scan.
        fn listed(&mut self, check: Check) -> Vec<i64> {
            self.scan(true).of(check).to_vec()
        }
    }

    impl Drop for Lib {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn a_clean_track_fails_no_check() {
        let mut lib = Lib::new("clean");
        lib.put("clean", |_| {});
        let r = lib.scan(true);
        for c in Check::ALL {
            assert!(r.of(c).is_empty(), "{c:?} listed a clean track");
        }
    }

    #[test]
    fn missing_file_lists_the_track_whose_file_is_gone_and_nothing_else() {
        let mut lib = Lib::new("missing");
        let clean = lib.put("clean", |_| {});
        let gone = lib.put("gone", |_| {});
        std::fs::remove_file(lib.dir.join("music/gone.wav")).unwrap();
        assert_eq!(lib.listed(Check::Missing), [gone]);
        // A track with no file is not "not analysed" or "corrupt" on top: relocating it is the one fix.
        let r = lib.scan(true);
        assert!(r.of(Check::NotAnalysed).is_empty() && r.of(Check::Corrupt).is_empty());
        assert!(!r.of(Check::Missing).contains(&clean));
    }

    #[test]
    fn corrupt_lists_a_file_that_failed_to_load_until_it_loads_again() {
        let mut lib = Lib::new("corrupt");
        let clean = lib.put("clean", |_| {});
        let bad = lib.put("bad", |_| {});
        let t = lib
            .tracks()
            .into_iter()
            .find(|t| t.id == Some(bad))
            .unwrap();
        lib.store.health_note_failure(&t, "no decoder").unwrap();
        let r = lib.scan(true);
        assert_eq!(r.of(Check::Corrupt), [bad]);
        assert_eq!(r.failures[&bad], "no decoder");
        assert!(!r.of(Check::Corrupt).contains(&clean));
        lib.store.health_clear_failure(&t).unwrap();
        assert!(lib.listed(Check::Corrupt).is_empty());
    }

    #[test]
    fn not_analysed_lists_tracks_without_an_analysis_of_their_own_or_their_library() {
        let mut lib = Lib::new("analysed");
        lib.put("clean", |_| {});
        let bare = lib.put("bare", |_| {});
        lib.analysed
            .lock()
            .unwrap()
            .remove(&lib.dir.join("music/bare.wav"));
        let from_library = lib.put("imported", |t| t.analysis_ref = Some("plugin:42".into()));
        lib.analysed
            .lock()
            .unwrap()
            .remove(&lib.dir.join("music/imported.wav"));
        assert_eq!(lib.listed(Check::NotAnalysed), [bare]);
        assert!(!lib
            .scan(true)
            .of(Check::NotAnalysed)
            .contains(&from_library));
    }

    #[test]
    fn missing_tags_checks_each_field_and_takes_the_key_and_bpm_from_our_analysis() {
        let mut lib = Lib::new("tags");
        let clean = lib.put("clean", |_| {});
        let no_title = lib.put("a", |t| t.title = " ".into());
        let no_artist = lib.put("b", |t| t.artist.clear());
        let no_genre = lib.put("c", |t| t.genre.clear());
        // The row has no key and no BPM, the analysis has both: nothing is missing.
        let analysed = lib.put("d", |t| (t.key, t.bpm_x100) = (String::new(), 0));
        // The row has neither and the analysis found a BPM but no key.
        let no_key = lib.put("e", |t| (t.key, t.bpm_x100) = (String::new(), 0));
        lib.analysed.lock().unwrap().insert(
            lib.dir.join("music/e.wav"),
            Glance {
                bpm_x100: 12000,
                key: None,
            },
        );
        // Neither the row nor the analysis (not analysed) has a BPM.
        let no_bpm = lib.put("f", |t| t.bpm_x100 = 0);
        lib.analysed
            .lock()
            .unwrap()
            .remove(&lib.dir.join("music/f.wav"));
        let got = lib.listed(Check::Tags);
        for id in [no_title, no_artist, no_genre, no_key, no_bpm] {
            assert!(got.contains(&id), "row {id} lacks a tag");
        }
        assert!(!got.contains(&clean) && !got.contains(&analysed));
        assert_eq!(got.len(), 5);
    }

    #[test]
    fn no_artwork_lists_a_missing_cover_path_and_a_cover_file_that_is_gone() {
        let mut lib = Lib::new("art");
        let clean = lib.put("clean", |_| {});
        let none = lib.put("none", |t| t.artwork = None);
        let gone = lib.put("gone", |_| {});
        std::fs::remove_file(lib.dir.join("gone.jpg")).unwrap();
        let got = lib.listed(Check::Artwork);
        assert_eq!(got, [none, gone]);
        assert!(!got.contains(&clean));
    }

    #[test]
    fn no_rating_lists_zero_stars_only() {
        let mut lib = Lib::new("rating");
        let clean = lib.put("clean", |_| {});
        let one = lib.put("one", |t| t.rating = 1);
        let zero = lib.put("zero", |t| t.rating = 0);
        let got = lib.listed(Check::Rating);
        assert_eq!(got, [zero]);
        assert!(!got.contains(&clean) && !got.contains(&one));
    }

    #[test]
    fn very_long_follows_the_minutes_threshold_and_zero_switches_it_off() {
        let mut lib = Lib::new("long");
        let clean = lib.put("clean", |_| {});
        let under = lib.put("under", |t| t.length_s = 20 * 60 - 1);
        let exact = lib.put("exact", |t| t.length_s = 20 * 60);
        let mix = lib.put("mix", |t| t.length_s = 3 * 3600);
        let got = lib.listed(Check::Long);
        assert_eq!(got, [exact, mix]);
        assert!(!got.contains(&clean) && !got.contains(&under));
        let rows = lib.tracks();
        let mut at = |long_min| {
            lib.scanner
                .run(
                    &rows,
                    &lib.store,
                    &Limits {
                        long_min,
                        ..Limits::default()
                    },
                    true,
                )
                .of(Check::Long)
                .to_vec()
        };
        assert_eq!(at(10), [under, exact, mix], "a lower threshold lists more");
        assert!(at(0).is_empty(), "0 is off");
    }

    #[test]
    fn probably_not_music_reads_genre_folder_type_chapter_titles_and_beatless_length() {
        let mut lib = Lib::new("spoken");
        let clean = lib.put("clean", |_| {});
        let genre = lib.put("genre", |t| t.genre = "Podcast".into());
        let spoken_word = lib.put("spoken", |t| t.genre = "Spoken Word".into());
        let book = lib.put("book", |t| {
            t.file = Some(t.file.take().unwrap().with_extension("m4b"))
        });
        let folder = lib.put("folder", |_| {});
        let folder_file = lib.dir.join("Audiobooks").join("folder.wav");
        std::fs::create_dir_all(folder_file.parent().unwrap()).unwrap();
        std::fs::rename(lib.dir.join("music/folder.wav"), &folder_file).unwrap();
        lib.store
            .collection_upsert(
                &Track {
                    file: Some(folder_file),
                    ..lib
                        .tracks()
                        .into_iter()
                        .find(|t| t.id == Some(folder))
                        .unwrap()
                },
                Some(folder),
            )
            .unwrap();
        let chapter = lib.put("chapter", |t| {
            (t.title, t.length_s) = ("Chapter 3".into(), 600)
        });
        let episode = lib.put("episode", |t| {
            (t.title, t.length_s) = ("Episode 12 - Guests".into(), 2400)
        });
        let beatless = lib.put("beatless", |t| (t.bpm_x100, t.length_s) = (0, 1000));
        lib.analysed.lock().unwrap().insert(
            lib.dir.join("music/beatless.wav"),
            Glance {
                bpm_x100: 0,
                key: None,
            },
        );
        // Not speech: music genres, a short "Part 2", a short beatless clip, a long track that has a beat, an unanalysed long one.
        lib.put("hiphop", |t| t.genre = "Hip-Hop".into());
        lib.put("part", |t| (t.title, t.length_s) = ("Part 2".into(), 200));
        lib.put("clip", |t| (t.bpm_x100, t.length_s) = (0, 100));
        lib.analysed.lock().unwrap().insert(
            lib.dir.join("music/clip.wav"),
            Glance {
                bpm_x100: 0,
                key: None,
            },
        );
        lib.put("longbeat", |t| t.length_s = 3600);
        lib.put("longunknown", |t| (t.bpm_x100, t.length_s) = (0, 3600));
        lib.analysed
            .lock()
            .unwrap()
            .remove(&lib.dir.join("music/longunknown.wav"));
        let got = lib.listed(Check::NotMusic);
        for (name, id) in [
            ("genre", genre),
            ("spoken word", spoken_word),
            ("m4b", book),
            ("folder", folder),
            ("chapter", chapter),
            ("episode", episode),
            ("beatless", beatless),
        ] {
            assert!(got.contains(&id), "{name} should look spoken");
        }
        assert!(!got.contains(&clean));
        assert_eq!(got.len(), 7, "nothing else: {got:?}");
    }

    #[test]
    fn duplicates_reuse_the_duplicate_finder_and_list_every_member() {
        let mut lib = Lib::new("dupes");
        let clean = lib.put("clean", |_| {});
        let a = lib.put("twin-a", |t| t.title = "Twin".into());
        let b = lib.put("twin-b", |t| {
            (t.title, t.artist, t.length_s) = ("twin ".into(), " BAND".into(), 201)
        });
        let far = lib.put("twin-c", |t| (t.title, t.length_s) = ("Twin".into(), 260));
        let got = lib.listed(Check::Duplicate);
        assert_eq!(got, [a, b]);
        assert!(!got.contains(&clean) && !got.contains(&far));
    }

    #[test]
    fn low_bitrate_lists_known_rates_below_the_threshold_only() {
        let mut lib = Lib::new("kbps");
        let clean = lib.put("clean", |_| {});
        let low = lib.put("low", |t| t.bitrate = 96);
        let edge = lib.put("edge", |t| t.bitrate = 128);
        let unknown = lib.put("unknown", |t| t.bitrate = 0);
        let got = lib.listed(Check::LowBitrate);
        assert_eq!(got, [low]);
        assert!(![clean, edge, unknown].iter().any(|id| got.contains(id)));
        let rows = lib.tracks();
        let strict = lib.scanner.run(
            &rows,
            &lib.store,
            &Limits {
                low_kbps: 0,
                ..Limits::default()
            },
            true,
        );
        assert!(strict.of(Check::LowBitrate).is_empty(), "0 is off");
    }

    #[test]
    fn the_next_run_probes_only_what_changed_or_could_still_improve() {
        let mut lib = Lib::new("incremental");
        let settled = lib.put("settled", |_| {});
        let waiting = lib.put("waiting", |_| {});
        lib.analysed
            .lock()
            .unwrap()
            .remove(&lib.dir.join("music/waiting.wav"));
        let edited = lib.put("edited", |_| {});
        let first = lib.scan(true);
        assert_eq!(
            lib.probes.load(Ordering::Relaxed),
            3,
            "a full run reads every track"
        );
        assert_eq!(first.of(Check::NotAnalysed), [waiting]);

        lib.probes.store(0, Ordering::Relaxed);
        lib.scan(false);
        assert_eq!(
            lib.probes.load(Ordering::Relaxed),
            1,
            "only the unanalysed track is asked again"
        );

        // Analysis finished for one, a tag edit on another: both show without a full run.
        lib.analysed
            .lock()
            .unwrap()
            .insert(lib.dir.join("music/waiting.wav"), glance());
        lib.store
            .set_rating(&format!("t{edited}"), Some(0))
            .unwrap();
        lib.probes.store(0, Ordering::Relaxed);
        let next = lib.scan(false);
        assert!(next.of(Check::NotAnalysed).is_empty());
        assert_eq!(next.of(Check::Rating), [edited]);
        assert_eq!(
            lib.probes.load(Ordering::Relaxed),
            2,
            "the one that was waiting and the one that changed"
        );

        // A removed track drops out and the settled one was never read again.
        lib.store.collection_delete(waiting).unwrap();
        lib.probes.store(0, Ordering::Relaxed);
        let last = lib.scan(false);
        assert_eq!(lib.probes.load(Ordering::Relaxed), 0);
        assert!(last.of(Check::Rating) == [edited] && !last.of(Check::Rating).contains(&settled));
    }

    #[test]
    fn ignored_tracks_leave_the_list_and_come_back_with_unignore() {
        let mut lib = Lib::new("ignores");
        let a = lib.put("a", |t| t.rating = 0);
        let b = lib.put("b", |t| t.rating = 0);
        let found = lib.scan(true).of(Check::Rating).to_vec();
        assert_eq!(found, [a, b]);
        let ignores = |lib: &Lib| lib.store.health_ignores().unwrap();
        lib.store
            .health_ignore(Check::Rating.id(), &[a], true)
            .unwrap();
        let ign = ignores(&lib);
        let set = ign.get(Check::Rating.id());
        assert_eq!(shown(&found, set, false), [b]);
        assert_eq!(shown(&found, set, true), [a]);
        assert_eq!(
            shown(&found, ign.get(Check::Tags.id()), false),
            [a, b],
            "another check is not affected"
        );
        lib.store
            .health_ignore(Check::Rating.id(), &[a], false)
            .unwrap();
        let ign = ignores(&lib);
        assert_eq!(shown(&found, ign.get(Check::Rating.id()), false), [a, b]);
        assert!(shown(&found, ign.get(Check::Rating.id()), true).is_empty());
    }

    #[test]
    fn probe_reads_the_file_the_cover_and_the_analysis_only_of_a_present_file() {
        let lib = Lib::new("probe");
        let id = lib.put("p", |_| {});
        let t = lib.tracks().into_iter().find(|t| t.id == Some(id)).unwrap();
        let ask: Analysis = Box::new(|_| Some(glance()));
        assert_eq!(
            probe(&t, &ask),
            Facts {
                file: true,
                artwork: true,
                analysis: Some(glance())
            }
        );
        std::fs::remove_file(t.file.as_ref().unwrap()).unwrap();
        std::fs::remove_file(t.artwork.as_ref().unwrap()).unwrap();
        assert_eq!(
            probe(&t, &ask),
            Facts {
                file: false,
                artwork: false,
                analysis: None
            }
        );
    }
}
