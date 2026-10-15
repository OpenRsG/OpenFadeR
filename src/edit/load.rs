//! Opens a track for editing: the decoded audio, its beat grid and a 3-band detail waveform
//! ([`analysis::DETAIL_ENTRIES_PER_SEC`] entries/s). The library's grid and waveform win; whichever is
//! missing comes from analysing the audio ([`analysis::analyze`], CPU heavy: call off the UI thread).

use crate::{analysis::{self, Analysis, Beat}, audio::{self, Pcm}};
use std::{path::Path, sync::Arc};

pub struct Loaded {
    pub pcm: Arc<Pcm>,
    pub beats: Vec<Beat>,
    pub detail: Vec<[u8; 3]>,
}

pub fn load(file: &Path, lib: Option<&Analysis>) -> Result<Loaded, String> {
    let pcm = audio::decode(file)?;
    let (mut beats, mut detail) = lib.map(|a| (a.beats.clone(), a.three_band_detail.clone())).unwrap_or_default();
    if beats.is_empty() || detail.is_empty() {
        let a = analysis::analyze(&pcm.samples, pcm.rate);
        if beats.is_empty() {
            beats = a.beats;
        }
        if detail.is_empty() {
            detail = a.three_band_detail;
        }
    }
    Ok(Loaded { pcm: Arc::new(pcm), beats, detail })
}
