//! What a recording's sidecar text files say about its tracks: the cue sheet (`<recording>.cue`, our reading of the
//! public cue-sheet format: `FILE`, then per `TRACK` a `TITLE`, a `PERFORMER` and `INDEX 01 mm:ss:ff` with 75 frames
//! to the second) and the tracklist (`<recording>.tracklist.txt`).
//!
//! [`Tracklist`] follows the deck that leads the mix and notes each time another track takes over, on the recorder's
//! clock (ms of recorded audio). When the recording ends, [`write`] cuts that list per file: every file gets its own
//! sheet, with times relative to its first frame and without the tracks that led it for less than the setting's minimum.
//! ponytail: the sheets are written when the recording ends, so a crash mid-recording leaves the WAV without them
//! (the control log streams); write each finished file's sheet at its split if that ever matters.

use super::rec::clock;
use openfader::audio::Take;
use std::{
    io,
    path::{Path, PathBuf},
    time::Duration,
};

/// Frames to the second of a cue sheet's `INDEX` times.
const CUE_FPS: u64 = 75;

/// `mm:ss:ff` of a time in ms (frames of 1/75 s, rounded down); minutes run past 99 when they must.
pub fn cue_time(ms: u64) -> String {
    let frames = ms * CUE_FPS / 1000;
    format!(
        "{:02}:{:02}:{:02}",
        frames / (CUE_FPS * 60),
        frames / CUE_FPS % 60,
        frames % CUE_FPS
    )
}

/// One track that led the mix: when it took over (ms on the recording's clock) and what to call it.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub at_ms: u64,
    pub artist: String,
    pub title: String,
    /// Effective tempo when it took over, 0 = unknown.
    pub bpm: f64,
    pub key: String,
}

/// The tracks that led the mix during a recording, in order.
#[derive(Default)]
pub struct Tracklist {
    entries: Vec<Entry>,
    /// The (deck, load) that led last: the same one coming back after a pause is not a new track.
    current: Option<(usize, u32)>,
}

impl Tracklist {
    /// `lead`: the deck (index, load count) that is on air at `at_ms`; `None` (nothing audible) leaves the last one in charge.
    /// A different lead gets the entry `make` builds; returns it then.
    pub fn observe(
        &mut self,
        at_ms: u64,
        lead: Option<(usize, u32)>,
        make: impl FnOnce(u64) -> Entry,
    ) -> Option<&Entry> {
        let lead = lead.filter(|&l| Some(l) != self.current)?;
        self.current = Some(lead);
        self.entries.push(make(at_ms));
        self.entries.last()
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }
}

/// The tracks heard in `[from, to)` ms of the recording, their times relative to `from`. A track is heard from its entry
/// to the next one's (the last to `to`); the part inside the range must reach `min`, and be more than nothing.
fn within(entries: &[Entry], from: u64, to: u64, min: Duration) -> Vec<Entry> {
    let ends = entries.iter().skip(1).map(|e| e.at_ms).chain([to]);
    entries
        .iter()
        .zip(ends)
        .filter_map(|(e, end)| {
            let (start, end) = (e.at_ms.max(from), end.min(to));
            let heard = end.saturating_sub(start);
            (heard > 0 && u128::from(heard) >= min.as_millis()).then(|| Entry {
                at_ms: start - from,
                ..e.clone()
            })
        })
        .collect()
}

/// Text on one line.
fn one_line(s: &str) -> String {
    s.replace(['\r', '\n'], " ")
}

/// A cue sheet text value: it has no escape for a quote.
fn quoted(s: &str) -> String {
    format!("\"{}\"", one_line(&s.replace('"', "'")))
}

/// The cue sheet of the audio file `file` (a name next to the sheet) holding `entries`.
pub fn cue_sheet(file: &str, entries: &[Entry]) -> String {
    let mut s = format!("FILE {} WAVE\n", quoted(file));
    for (n, e) in entries.iter().enumerate() {
        s += &format!(
            "  TRACK {:02} AUDIO\n    TITLE {}\n    PERFORMER {}\n    INDEX 01 {}\n",
            n + 1,
            quoted(&e.title),
            quoted(&e.artist),
            cue_time(e.at_ms)
        );
    }
    s
}

/// One line per track: `M:SS  Artist - Title  (128.0 BPM, 8A)`.
pub fn tracklist(entries: &[Entry]) -> String {
    let mut s = String::new();
    for e in entries {
        let who = match e.artist.as_str() {
            "" => one_line(&e.title),
            a => format!("{} - {}", one_line(a), one_line(&e.title)),
        };
        let details: Vec<_> = (e.bpm > 0.0)
            .then(|| format!("{:.1} BPM", e.bpm))
            .into_iter()
            .chain((!e.key.is_empty()).then(|| e.key.clone()))
            .collect();
        s += &format!("{}  {who}", clock(Duration::from_millis(e.at_ms)));
        if !details.is_empty() {
            s += &format!("  ({})", details.join(", "));
        }
        s.push('\n');
    }
    s
}

/// `<recording>.<suffix>`: the recording's name without its extension, then the suffix, in its folder.
pub fn sidecar(recording: &Path, suffix: &str) -> PathBuf {
    let mut name = recording
        .file_stem()
        .unwrap_or(recording.as_os_str())
        .to_os_string();
    name.push(format!(".{suffix}"));
    recording.with_file_name(name)
}

/// Writes the cue sheet and / or the tracklist of every file of `take`, from the tracks that led its recording; a file
/// that none of the tracks reached gets no sheet. Returns the files written.
pub fn write(
    take: &Take,
    entries: &[Entry],
    min: Duration,
    cue: bool,
    list: bool,
) -> io::Result<Vec<PathBuf>> {
    let ms = |frames: u64| frames * 1000 / u64::from(take.rate.max(1));
    let mut written = Vec::new();
    for (i, file) in take.files.iter().enumerate() {
        let from = ms(take.starts.get(i).copied().unwrap_or(0));
        let to = take.starts.get(i + 1).map_or(ms(take.frames), |&f| ms(f));
        let tracks = within(entries, from, to, min);
        if tracks.is_empty() {
            continue;
        }
        let name = file.file_name().unwrap_or(file.as_os_str());
        let jobs = [
            (cue, "cue", cue_sheet(&name.to_string_lossy(), &tracks)),
            (list, "tracklist.txt", tracklist(&tracks)),
        ];
        for (_, suffix, text) in jobs.into_iter().filter(|j| j.0) {
            let path = sidecar(file, suffix);
            std::fs::write(&path, text)?;
            written.push(path);
        }
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(at_ms: u64, artist: &str, title: &str) -> Entry {
        Entry {
            at_ms,
            artist: artist.into(),
            title: title.into(),
            bpm: 0.0,
            key: String::new(),
        }
    }

    #[test]
    fn cue_times_count_75_frames_to_the_second() {
        assert_eq!(cue_time(0), "00:00:00");
        assert_eq!(cue_time(13), "00:00:00");
        assert_eq!(cue_time(14), "00:00:01");
        assert_eq!(cue_time(999), "00:00:74");
        assert_eq!(cue_time(1000), "00:01:00");
        assert_eq!(cue_time(61_500), "01:01:37");
        assert_eq!(cue_time(100 * 60_000), "100:00:00");
    }

    #[test]
    fn a_sequence_of_leads_becomes_one_entry_per_track() {
        let mut t = Tracklist::default();
        let mut at = |ms, lead| {
            t.observe(ms, lead, |at| entry(at, "", &format!("t{at}")))
                .is_some()
        };
        assert!(!at(0, None), "nothing on air");
        assert!(at(1_000, Some((0, 1))), "deck A leads");
        assert!(!at(2_000, Some((0, 1))), "still the same load");
        assert!(!at(3_000, None), "paused: the track stays current");
        assert!(!at(4_000, Some((0, 1))), "and resuming it adds nothing");
        assert!(at(60_000, Some((1, 1))), "B takes over");
        assert!(at(120_000, Some((0, 2))), "A with a new load");
        assert!(
            at(130_000, Some((0, 1))),
            "the first load of A, played again"
        );
        let at_ms: Vec<_> = t.entries().iter().map(|e| e.at_ms).collect();
        assert_eq!(at_ms, [1_000, 60_000, 120_000, 130_000]);
    }

    #[test]
    fn short_tracks_are_left_out_and_times_are_relative_to_the_file() {
        let e = [
            entry(0, "A", "one"),
            entry(20_000, "B", "blip"),
            entry(25_000, "C", "two"),
        ];
        let heard = |from, to, min| -> Vec<(u64, String)> {
            within(&e, from, to, Duration::from_secs(min))
                .into_iter()
                .map(|e| (e.at_ms, e.title))
                .collect()
        };
        assert_eq!(
            heard(0, 40_000, 10),
            [(0, "one".into()), (25_000, "two".into())]
        );
        // from 18 s on, "one" no longer reaches the minimum inside the range
        assert_eq!(heard(18_000, 40_000, 10), [(7_000, "two".into())]);
        assert_eq!(heard(0, 40_000, 0).len(), 3);
        assert!(
            heard(30_000, 30_000, 0).is_empty(),
            "an empty range hears nothing"
        );
    }

    #[test]
    fn cue_sheet_and_tracklist_name_the_tracks() {
        let mut e = entry(61_500, "Some \"Band\"", "Song\nTwo");
        e.bpm = 128.0;
        e.key = "8A".into();
        let tracks = [entry(0, "A", "One"), e];
        assert_eq!(
            cue_sheet("rec-1.wav", &tracks),
            "FILE \"rec-1.wav\" WAVE\n  TRACK 01 AUDIO\n    TITLE \"One\"\n    PERFORMER \"A\"\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    TITLE \"Song Two\"\n    PERFORMER \"Some 'Band'\"\n    INDEX 01 01:01:37\n"
        );
        assert_eq!(
            tracklist(&tracks),
            "0:00  A - One\n1:01  Some \"Band\" - Song Two  (128.0 BPM, 8A)\n"
        );
    }

    #[test]
    fn a_split_recording_gets_a_sheet_per_file_with_relative_times() {
        let dir = std::env::temp_dir().join(format!("of-sheet-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // 100 Hz clock: the second file starts at 60 s and the recording ends at 90 s
        let take = Take {
            frames: 9_000,
            rate: 100,
            dropped: 0,
            files: vec![dir.join("rec-1.wav"), dir.join("rec-2.wav")],
            starts: vec![0, 6_000],
        };
        let entries = [
            entry(0, "A", "one"),
            entry(40_000, "B", "two"),
            entry(62_500, "C", "three"),
        ];
        let made = write(&take, &entries, Duration::from_secs(10), true, true).unwrap();
        assert_eq!(made.len(), 4);
        let read = |n: &str| std::fs::read_to_string(dir.join(n)).unwrap();
        let first = read("rec-1.cue");
        assert!(first.starts_with("FILE \"rec-1.wav\" WAVE\n"));
        assert!(first.contains("TITLE \"one\"") && first.contains("TITLE \"two\""));
        assert!(first.contains("INDEX 01 00:00:00") && first.contains("INDEX 01 00:40:00"));
        assert!(!first.contains("three"), "starts in file 2");
        let second = read("rec-2.cue");
        assert!(second.starts_with("FILE \"rec-2.wav\" WAVE\n"));
        assert!(!second.contains("one"), "ended in file 1");
        assert!(
            !second.contains("two"),
            "2.5 s of it reach file 2: under the minimum"
        );
        assert!(second.contains("TITLE \"three\"") && second.contains("INDEX 01 00:02:37"));
        assert_eq!(read("rec-2.tracklist.txt"), "0:02  C - three\n");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
