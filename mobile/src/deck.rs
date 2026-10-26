//! Mobile deck state; decoding/analysis runs off-thread and transport uses the shared audio engine.
use openfader::{analysis::{self, Analysis, Beat, Cue}, audio::{self, Pcm, Player}, library::Track, store};
use slint::{Image, Rgba8Pixel, SharedPixelBuffer};
use std::sync::mpsc::{self, Receiver};

pub struct Loaded {
    pub track: Track,
    pub pcm: Pcm,
    pub analysis: Analysis,
}

#[derive(Default)]
pub struct Deck {
    pub player: Option<Player>,
    pub track: Option<Track>,
    pub analysis: Analysis,
    pub pending: Option<Receiver<Result<Loaded, String>>>,
    pub gain: f32,
    pub last_second: u32,
    pub last_bpm: f64,
}

impl Deck {
    pub fn new() -> Self { Self { gain: 1.0, last_second: u32::MAX, last_bpm: f64::NAN, ..Self::default() } }

    pub fn start_load(&mut self, track: Track) -> Result<(), String> {
        if self.pending.is_some() { return Err("Wait for this deck to finish loading".into()); }
        if self.player.as_ref().is_some_and(Player::is_playing) {
            return Err("Pause the deck before replacing its track".into());
        }
        let (tx, rx) = mpsc::channel();
        self.pending = Some(rx);
        std::thread::spawn(move || {
            let result = (|| {
                let path = track.file.as_ref().ok_or("Track has no audio file")?;
                let pcm = audio::decode(path)?;
                let (mut a, _) = analysis::cache::analyze_cached(&track, &pcm.samples, pcm.rate);
                store::apply(&mut a, &store::global().edits(&store::key(&track))?);
                Ok(Loaded { track, pcm, analysis: a })
            })();
            let _ = tx.send(result);
        });
        Ok(())
    }

    pub fn poll(&mut self) -> Option<Result<Image, String>> {
        let result = match self.pending.as_ref()?.try_recv() {
            Ok(r) => r,
            Err(mpsc::TryRecvError::Empty) => return None,
            Err(mpsc::TryRecvError::Disconnected) => {
                Err("Track worker stopped; load the track again".into())
            }
        };
        self.pending = None;
        Some(result.and_then(|loaded| {
            if self.player.is_none() { self.player = Some(Player::new()?); }
            let player = self.player.as_mut().unwrap();
            player.load(loaded.pcm);
            player.set_key_lock(true);
            self.last_second = u32::MAX;
            self.last_bpm = f64::NAN;
            self.track = Some(loaded.track);
            self.analysis = loaded.analysis;
            Ok(waveform(&self.analysis.three_band_preview))
        }))
    }

    pub fn bpm(&self) -> f64 {
        let pos = self.player.as_ref().map_or(0.0, Player::position_ms);
        let beats = &self.analysis.beats;
        let i = beats.partition_point(|b| f64::from(b.time_ms) <= pos).saturating_sub(1);
        beats.get(i).map(|b| f64::from(b.bpm_x100) / 100.0)
            .filter(|b| *b > 0.0)
            .or_else(|| {
                self.track
                    .as_ref()
                    .map(|t| t.bpm_x100 as f64 / 100.0)
                    .filter(|b| *b > 0.0)
            })
            .unwrap_or(0.0)
    }

    pub fn cue(&mut self, hot: u16, mode: i32) -> Result<(), String> {
        let Some(p) = self.player.as_mut() else {
            return Ok(());
        };
        let Some(track) = self.track.as_ref() else {
            return Ok(());
        };
        let existing = self
            .analysis
            .cues
            .iter()
            .find(|c| c.hot_cue == hot)
            .cloned();
        if mode == 0
            && let Some(c) = existing
        {
            if hot == 0 {
                p.pause();
            }
            p.set_loop(c.loop_end_ms.map(|e| (f64::from(c.time_ms), f64::from(e))));
            p.seek(c.time_ms);
            return Ok(());
        }
        if mode == 0 && hot == 0 {
            p.pause(); p.set_loop(None); p.seek(0); return Ok(());
        }
        let mut cues = self.analysis.cues.clone();
        cues.retain(|c| c.hot_cue != hot);
        if mode != 2 {
            cues.push(Cue { hot_cue: hot, time_ms: p.position_ms() as u32,
                loop_end_ms: None, color: None, comment: None });
        }
        store::global().set_cues(&store::key(track), Some(&cues))?;
        self.analysis.cues = cues;
        Ok(())
    }

    pub fn toggle_loop(&mut self, index: i32) -> Result<(), String> {
        let bpm = self.bpm();
        let Some(p) = self.player.as_mut() else {
            return Ok(());
        };
        if p.looping().is_some() {
            p.set_loop(None);
            return Ok(());
        }
        if bpm <= 0.0 {
            return Err(
                "No beat grid found for this track; load a track with a detected BPM".into(),
            );
        }
        let pos = p.position_ms();
        let beats = &self.analysis.beats;
        let i = beats
            .partition_point(|b| f64::from(b.time_ms) <= pos)
            .saturating_sub(1);
        let start = beats.get(i).map_or(pos, |b| f64::from(b.time_ms));
        let count = 1u32 << index.clamp(0, 4);
        let end = start + 60000.0 / bpm * f64::from(count);
        if end > f64::from(p.duration_ms()) {
            return Err("Not enough audio remains for this loop; choose a shorter loop".into());
        }
        p.set_loop(Some((start, end)));
        Ok(())
    }
}

/// Target tempo and phase of a one-shot sync. Reject tempos outside the mobile fader range.
pub fn sync_target(own: &[Beat], pos: f64, bpm: f64, master: &[Beat], master_pos: f64,
    master_bpm: f64, master_rate: f64) -> Result<(f64, u32), String> {
    if bpm <= 0.0 || master_bpm <= 0.0 || own.is_empty() || master.is_empty() {
        return Err("Load and analyze both decks before syncing".into());
    }
    let rate = master_bpm * master_rate / bpm;
    if !(0.84..=1.16).contains(&rate) {
        return Err("Tempos are outside the ±16% range; choose closer BPMs".into());
    }
    let a = own.partition_point(|b| f64::from(b.time_ms) <= pos).saturating_sub(1);
    let b = master.partition_point(|b| f64::from(b.time_ms) <= master_pos).saturating_sub(1);
    let phase = ((master_pos - f64::from(master[b].time_ms)) / (60000.0 / master_bpm)).rem_euclid(1.0);
    Ok((rate, (f64::from(own[a].time_ms) + phase * 60000.0 / bpm).round() as u32))
}

fn waveform(bands: &[[u8; 3]]) -> Image {
    let mut pixels = SharedPixelBuffer::<Rgba8Pixel>::new(640, 88);
    for x in 0..640usize {
        let Some(b) = bands.get(x * bands.len() / 640) else {
            continue;
        };
        let height = usize::from(*b.iter().max().unwrap()).min(127) * 42 / 127;
        let color = Rgba8Pixel::new(b[0].saturating_add(48), b[1].saturating_add(64), b[2].saturating_add(80), 255);
        for y in 44 - height..44 + height + 1 {
            pixels.make_mut_slice()[y * 640 + x] = color;
        }
    }
    Image::from_rgba8(pixels)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sync_matches_audible_tempo_and_phase_and_rejects_out_of_range() {
        let grid = |bpm: u16, step: u32| {
            (0..12)
                .map(|i| Beat {
                    time_ms: i * step,
                    bpm_x100: bpm,
                    bar_pos: 1,
                })
                .collect::<Vec<_>>()
        };
        let a = grid(12000, 500);
        let b = grid(12500, 480);
        let (rate, seek) = sync_target(&a, 1100.0, 120.0, &b, 1320.0, 125.0, 1.02).unwrap();
        assert!((120.0 * rate - 127.5).abs() < 1e-9);
        assert_eq!(seek, 1375);
        assert!(sync_target(&a, 0.0, 120.0, &b, 0.0, 160.0, 1.0).is_err());
        assert!(sync_target(&[], 0.0, 0.0, &b, 0.0, 125.0, 1.0).is_err());
    }
}
