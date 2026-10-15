//! The data a library plugin hands the host (and takes for a collection export): plain serde types, the same ones the
//! host's own library code uses, so nothing is converted on either side of the ABI.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Detail waveform columns per second (`Analysis::detail`, `color_detail`, `three_band_detail`). The detail waveform has
/// one column per `1 / DETAIL_ENTRIES_PER_SEC` s of the track.
pub const DETAIL_ENTRIES_PER_SEC: u32 = 150;
/// Overview columns (`Analysis::three_band_preview`, `color_preview`); the entries span the whole track.
pub const PREVIEW_ENTRIES: usize = 1200;

/// Where a track without a local file comes from: a plugin track source. LOAD fetches the audio first.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Remote {
    pub plugin: String,
    pub source: String,
    /// The track's id as the plugin names it.
    pub id: String,
}

/// One track row. Every field but `file` may be empty / zero.
#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Track {
    pub title: String,
    pub artist: String,
    pub album: String,
    /// Musical key as the source names it.
    pub key: String,
    pub genre: String,
    pub label: String,
    /// BPM × 100.
    pub bpm_x100: i64,
    pub length_s: i64,
    /// Audio file.
    pub file: Option<PathBuf>,
    /// Opaque to the host: what the plugin needs to find this track's analysis again (a path, an id). The host hands the
    /// whole [`Track`] back in the `analysis` op and never reads this.
    pub analysis_ref: Option<String>,
    /// Cover image: absolute path of an existing file (a missing file reads as no artwork).
    pub artwork: Option<PathBuf>,
    /// Stars, 0 to 5.
    pub rating: u8,
    pub comment: String,
    /// Date the track was added, as the source's own string (usually `YYYY-MM-DD`); may be empty.
    pub date_added: String,
    /// kbps and Hz of the audio file, 0 if unknown.
    pub bitrate: u32,
    pub sample_rate: u32,
    pub remote: Option<Remote>,
}

impl Track {
    /// "128.00".
    pub fn bpm_text(&self) -> String {
        format!("{:.2}", self.bpm_x100 as f64 / 100.0)
    }

    /// "m:ss".
    pub fn length_text(&self) -> String {
        format!("{}:{:02}", self.length_s / 60, self.length_s % 60)
    }
}

/// A folder or playlist row of the tree, in depth-first order.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Playlist {
    pub id: String,
    pub name: String,
    pub depth: usize,
    pub is_folder: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Beat {
    /// Position in the bar, 1..=4.
    pub bar_pos: u16,
    pub bpm_x100: u16,
    pub time_ms: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Cue {
    /// 0 = memory cue, 1.. = hot cue A..
    pub hot_cue: u16,
    pub time_ms: u32,
    /// Loop end for loop cues.
    pub loop_end_ms: Option<u32>,
    /// RGB display colour, when the cue carries one.
    pub color: Option<[u8; 3]>,
    /// DJ-assigned comment, when non-empty.
    pub comment: Option<String>,
}

/// A region where a voice is likely.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VocalRegion {
    pub start_ms: u32,
    pub end_ms: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Mood {
    High,
    Mid,
    Low,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    Intro,
    Verse,
    Chorus,
    Bridge,
    Outro,
    Up,
    Down,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Phrase {
    /// First beat (1-based grid index).
    pub beat: u32,
    /// Beat the phrase ends at: the next phrase's start, or the structure's end beat for the last one.
    pub end_beat: u32,
    pub kind: Kind,
    /// Style number shown after the name ("Verse 2", "Up 3"); 0 when the label has none.
    pub variant: u8,
    /// Beat at which the fill-in at the end of the phrase starts.
    pub fill: Option<u32>,
}

/// The song structure of a track.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Phrases {
    pub mood: Mood,
    pub list: Vec<Phrase>,
}

impl Phrase {
    /// The label, e.g. "Intro 1", "Verse 3", "Down".
    pub fn label(&self) -> String {
        match self.variant {
            0 => self.kind.name().to_owned(),
            v => format!("{} {v}", self.kind.name()),
        }
    }

    /// `(start_ms, end_ms)` on the beat grid. Beats past the grid (`end_beat` may exceed it by a few) extend it at the
    /// last beat's tempo.
    pub fn span_ms(&self, beats: &[Beat]) -> (f64, f64) {
        (beat_ms(beats, self.beat), beat_ms(beats, self.end_beat))
    }

    /// Start time of the fill-in at the end of the phrase, when it has one.
    pub fn fill_ms(&self, beats: &[Beat]) -> Option<f64> {
        self.fill.map(|f| beat_ms(beats, f))
    }
}

impl Kind {
    const fn name(self) -> &'static str {
        match self {
            Kind::Intro => "Intro",
            Kind::Verse => "Verse",
            Kind::Chorus => "Chorus",
            Kind::Bridge => "Bridge",
            Kind::Outro => "Outro",
            Kind::Up => "Up",
            Kind::Down => "Down",
        }
    }
}

impl Mood {
    /// The phrase names of this mood in the order of the legend and colour tables. One entry per distinct
    /// [`Phrase::label`] the mood can have.
    pub const fn legend(self) -> &'static [&'static str] {
        match self {
            Mood::High => &[
                "Intro 1", "Intro 2", "Up 1", "Up 2", "Up 3", "Chorus 1", "Chorus 2", "Down",
                "Outro 1", "Outro 2",
            ],
            Mood::Mid => &[
                "Intro", "Verse 1", "Verse 2", "Verse 3", "Verse 4", "Verse 5", "Verse 6",
                "Chorus", "Bridge", "Outro",
            ],
            Mood::Low => &["Intro", "Verse 1", "Verse 2", "Chorus", "Bridge", "Outro"],
        }
    }

    /// Index into [`Self::legend`] of a phrase of `kind` and style number `variant` (0 reads as 1 where the mood numbers its
    /// styles, as our own phrase analysis leaves it); `None` when the mood has no such phrase.
    pub fn slot(self, kind: Kind, variant: u8) -> Option<usize> {
        self.legend().iter().position(|n| match n.split_once(' ') {
            Some((name, num)) => name == kind.name() && num.parse() == Ok(variant.max(1)),
            None => *n == kind.name(),
        })
    }
}

/// Time of 1-based beat number `n`; past the grid, extrapolated at the last beat's tempo.
fn beat_ms(beats: &[Beat], n: u32) -> f64 {
    let i = n.saturating_sub(1) as usize;
    match (beats.get(i), beats.last()) {
        (Some(b), _) => b.time_ms as f64,
        (None, Some(l)) => {
            l.time_ms as f64 + (i + 1 - beats.len()) as f64 * 6_000_000.0 / l.bpm_x100.max(1) as f64
        }
        (None, None) => 0.0,
    }
}

/// Beat grid, cues, waveforms of every kind the app draws, key, vocal regions and phrases of one track. Every part may
/// be empty.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Analysis {
    /// Audio path the analysis was made for.
    pub path: Option<String>,
    pub beats: Vec<Beat>,
    /// 400-column overview. Low 5 bits height (0..=31), high 3 bits whiteness.
    pub preview: Vec<u8>,
    /// Memory and hot cues, loops.
    pub cues: Vec<Cue>,
    /// Monochrome scrolling detail, 1 byte per entry ([`DETAIL_ENTRIES_PER_SEC`]), low 5 bits height, high 3 bits whiteness.
    pub detail: Vec<u8>,
    /// Colour overview, 6 raw bytes per entry ([`PREVIEW_ENTRIES`] entries).
    pub color_preview: Vec<[u8; 6]>,
    /// Colour detail, one u16 per column: bits 15..13 red, 12..10 green, 9..7 blue, 6..2 height.
    pub color_detail: Vec<u16>,
    /// 3-band overview as `[low, mid, high]` heights ([`PREVIEW_ENTRIES`] entries).
    pub three_band_preview: Vec<[u8; 3]>,
    /// 3-band detail as `[low, mid, high]` heights ([`DETAIL_ENTRIES_PER_SEC`] per second).
    pub three_band_detail: Vec<[u8; 3]>,
    /// Vocal-detection thresholds `[low, mid, high]`.
    pub vocal_thresholds: Option<[u16; 3]>,
    /// Musical key in Camelot notation (e.g. "8A").
    pub key: Option<String>,
    /// Regions where a voice is likely.
    pub vocal: Vec<VocalRegion>,
    /// Song structure; `None` when the source has none.
    pub phrases: Option<Phrases>,
}

/// Everything a collection export reads, owned: the unique tracks with their analysis, the playlist tree with its
/// members, and what only the host's store has (track colours, tags, user playlists, already merged into the tree).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Snapshot {
    /// Unique tracks and their analysis (`None`: the source has none).
    pub tracks: Vec<(Track, Option<Analysis>)>,
    /// Playlists and folders in tree order; history playlists are not in it.
    pub playlists: Vec<Playlist>,
    /// `members[i]`: 0-based indexes into `tracks` of `playlists[i]` (empty for folders).
    pub members: Vec<Vec<usize>>,
    /// `(track index, colour)`: track colour 1..=8 (pink, red, orange, yellow, green, aqua, blue, purple).
    pub colors: Vec<(usize, u8)>,
    /// tag categories with their tag names.
    pub mytags: Vec<(String, Vec<String>)>,
    /// `(track index, category index, tag index)` tag assignments.
    pub tagged: Vec<(usize, usize, usize)>,
}
