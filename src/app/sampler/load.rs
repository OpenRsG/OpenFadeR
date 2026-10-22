//! Filling slots: from a deck's playhead or loop, from the browser's track or a sample pack; Sequence Load and Export Loop As Wav.

use super::{
    App, BEATS, Bank, DECK_NAMES, DeckState, MAX_SECS, ME, Sampler, SamplerState, capture,
    deck_source, prefs,
};
use openfader::audio::Player;
use slint::{ComponentHandle, Model};
use std::path::PathBuf;

impl Sampler {
    /// Copies the span `span(player, grid tempo)` = `(from_ms, secs)` of deck `d`'s audio into slot `i` and writes it as a WAV file next to it.
    /// The grid tempo is the effective BPM over the playback rate, 0 when the deck has none.
    fn from_deck_span(
        &self,
        d: usize,
        i: usize,
        span: impl FnOnce(&Player, f64) -> Result<(f64, f64), String>,
    ) -> Result<(), String> {
        let name = DECK_NAMES[d];
        self.check(i)?;
        let row = self
            .app
            .upgrade()
            .and_then(|a| a.global::<DeckState>().get_decks().row_data(d))
            .ok_or(tr!("no deck"))?;
        if !row.loaded {
            return Err(tr!("deck {} has no track", name));
        }
        let (pcm, bpm) = self.decks[d]
            .with_player(|p| {
                // the grid tempo, whatever the tempo fader says
                let native = if row.bpm > 0.0 {
                    row.bpm as f64 / p.rate()
                } else {
                    0.0
                };
                span(p, native).map(|(from, secs)| (p.capture(from, secs), native as f32))
            })
            .ok_or(tr!("deck {} has no audio", name))??;
        let saved = (!pcm.samples.is_empty()).then(|| capture::save(&pcm, "sampler"));
        self.give(i, pcm, bpm)?;
        if let Some(saved) = saved {
            let info = self.cfg.borrow()[i].info.clone();
            self.say(match saved {
                Ok(p) => tr!("{} · saved {}", info, p.display()),
                Err(e) => tr!("{} · WAV not saved: {}", info, e),
            });
        }
        Ok(())
    }

    /// `N beats` from deck `d`'s playhead.
    fn from_deck(&self, d: usize, i: usize) -> Result<(), String> {
        let (name, beats) = (DECK_NAMES[d], BEATS[self.beats.get()] as f64);
        self.from_deck_span(d, i, |p, native| {
            if native <= 0.0 {
                return Err(tr!("deck {} has no tempo to count beats with", name));
            }
            Ok((p.position_ms(), (beats * 60.0 / native).min(MAX_SECS)))
        })
    }

    /// Deck `d`'s active loop (at most [`MAX_SECS`] of it).
    fn from_loop(&self, d: usize, i: usize) -> Result<(), String> {
        let name = DECK_NAMES[d];
        self.from_deck_span(d, i, |p, _| {
            p.looping()
                .map(|lp| capture::loop_region(lp, MAX_SECS))
                .ok_or(tr!("deck {} has no loop", name))
        })
    }

    /// The browser's selected track, decoded in the background.
    fn from_track(&self, i: usize) -> Result<(), String> {
        self.check(i)?;
        let t = self
            .track
            .borrow()
            .clone()
            .ok_or(tr!("select a track in the browser first"))?;
        let file = t.file.ok_or(tr!("that track has no audio file"))?;
        self.decode(i, file, t.bpm_x100 as f32 / 100.0);
        Ok(())
    }

    /// LOAD: the selected source (a deck or TRK) into the selected slot.
    pub(super) fn load(&self) {
        let Some((app, i)) = self.state() else { return };
        let r = match deck_source(app.global::<SamplerState>().get_source()) {
            Some(d) => self.from_deck(d, i),
            None => self.from_track(i),
        };
        if let Err(e) = r {
            self.say(e);
        }
    }

    /// Sequence Load: a track loaded into a slot loops over its whole slice and starts playing (the pad is pressed).
    pub(super) fn sequence(&self, i: usize) {
        let Some(bus) = self
            .bus
            .as_ref()
            .filter(|_| prefs::with(|p| p.controller.sequence_load))
        else {
            return;
        };
        {
            let c = &mut self.cfg.borrow_mut()[i];
            (c.looped, c.gate) = (true, false);
        }
        self.apply(i);
        self.select(i);
        bus.sampler.press(i);
        bus.sampler.release(i);
    }

    /// Deck `d`'s active loop `(start_ms, end_ms)`, if any.
    fn loop_of(&self, d: usize) -> Option<(u32, u32)> {
        self.decks[d].with_player(|p| p.looping()).flatten()
    }

    /// "Capture the loop in an empty slot": pressing the pad of an EMPTY slot while the source deck (the one FROM names, else the master)
    /// loops captures that loop into it. True when it did (the pad then does not play).
    pub(super) fn capture_loop(&self, i: usize) -> bool {
        if !prefs::with(|p| p.controller.capture_loop_empty) || self.cfg.borrow()[i].loaded {
            return false;
        }
        let Some(app) = self.app.upgrade() else {
            return false;
        };
        let d = deck_source(app.global::<SamplerState>().get_source())
            .or_else(|| deck_source(app.global::<DeckState>().get_master()));
        let Some(d) = d.filter(|&d| self.loop_of(d).is_some()) else {
            return false;
        };
        if let Err(e) = self.from_loop(d, i) {
            self.say(e);
        }
        true
    }

    /// Export Loop As Wav: deck `d`'s active loop, else (no such deck / no loop) the selected slot's slice, as a WAV file at the ADVANCED frequency.
    pub(super) fn export_loop(&self, d: i32) {
        let r = || -> Result<PathBuf, String> {
            let lp = deck_source(d).and_then(|d| self.loop_of(d).map(|lp| (d, lp)));
            if let Some((d, lp)) = lp {
                let (from, secs) = capture::loop_region(lp, f64::INFINITY);
                let pcm = self.decks[d]
                    .with_player(|p| p.capture(from, secs))
                    .ok_or(tr!("deck {} has no audio", DECK_NAMES[d]))?;
                return capture::save(&pcm, "loop");
            }
            let (_, i) = self.state().ok_or(tr!("no panel"))?;
            let slice = self
                .bus
                .as_ref()
                .and_then(|b| b.sampler.slice(i))
                .ok_or(tr!("no loop on that deck and slot {} is empty", i + 1))?;
            capture::save(&slice, "loop")
        };
        self.say(match r() {
            Ok(p) => tr!("exported {}", p.display()),
            Err(e) => e,
        });
    }
}

/// A sample pack was imported (File > Additional contents): its `files` (with BPM) go into the empty slots in use, in order, and
/// the panel opens. Returns how many fit.
pub fn load_bank(app: &App, files: Vec<(PathBuf, f32)>) -> usize {
    let Some(s) = ME.with(|m| m.borrow().upgrade()) else {
        return 0;
    };
    let free = (0..s.used.get()).filter(|&i| {
        !s.cfg.borrow()[i].loaded && s.pending.borrow().as_ref().is_none_or(|p| p.slot != i)
    });
    let queue: Bank = free.zip(files).map(|(i, (f, b))| (i, f, b)).collect();
    let n = queue.len();
    s.bank.borrow_mut().extend(queue);
    if n > 0 {
        app.global::<SamplerState>().set_open(true);
    }
    n
}
