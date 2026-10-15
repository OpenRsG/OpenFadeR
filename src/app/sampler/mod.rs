//! Sampler panel ↔ ui/sampler.slint (global `SamplerState`); the audio is in `src/audio/sampler/`.
//!
//! The pads (4, 8 or 16 in use: CONTROLLER > Sampler > SLOT) fill from the playhead of any deck A to D (`N beats` at the deck's tempo, copied from the
//! deck's own audio with [`audio::Player::capture`]) or from the browser's selected track (decoded on a
//! worker thread, first [`MAX_SECS`] kept). A slice remembers its tempo (the deck's grid tempo, effective
//! BPM / playback rate, or the library BPM) so BPM SYNC can follow the master deck (the one `fx` follows).
//! Per-slot settings live here (`Cfg`) and are mirrored into the audio [`Controls`](audio::sampler::Controls);
//! a 50 ms timer lights the playing pads, hands over the finished track decode and pushes the master tempo.
//! Every action that cannot work says why in the status text.
//!
//! CONTROLLER > Sampler: the SLOT count limits the usable pads (and what the panel shows), "do not overwrite" refuses a LOAD / capture
//! into a filled slot ("slot n is loaded"), every deck capture is also written as a WAV file ([`capture`]), a pad pressed on an EMPTY slot while the
//! source deck loops captures that loop, Sequence Load makes a TRK load loop over the whole slice and start, and Auto Panel Mode lets a controller's
//! SAMPLER button open the panel ([`controller_button`]). Export Loop As Wav (ADVANCED) writes a deck's active loop, else the selected slot.
//! VIEW > Color > Sampler paints each slot by the slice's dominant frequency band, or all purple.
//!
//! Children: [`load`] (filling slots, Sequence Load, Export Loop As Wav), [`controls`] (the `SamplerState` callbacks), [`seq`] (the SEQUENCER),
//! [`capture`] (loop span, WAV file, frequency band).
//! ponytail: a TRK load decodes the whole file to keep 30 s of it, and only the newest TRK load is awaited; Sequence Load loops those 30 s.

mod capture;
mod controls;
mod load;
mod seq;
pub use controls::wire;
pub use load::load_bank;
pub use seq::{call, call_stop, pad_state};

use super::{
    App, DeckState, SamplerState, SlotView,
    deck::{DECK_NAMES, DECKS, Decks},
    gain,
    prefs::{self, view::SamplerColor},
};
use openfader::audio::{
    self, Bus, Pcm,
    sampler::{MAX_SECS, SLOTS},
};
use openfader::library::Track;
use slint::{ComponentHandle, Model, VecModel};
use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    path::PathBuf,
    rc::{Rc, Weak},
    sync::Arc,
    sync::mpsc::{Receiver, TryRecvError},
};

/// Beat counts of a deck capture and the one selected at start.
const BEATS: [u32; 6] = [1, 2, 4, 8, 16, 32];
const DEFAULT_BEATS: usize = 2;

fn deck_source(source: i32) -> Option<usize> {
    usize::try_from(source).ok().filter(|&d| d < DECKS)
}

/// May slot `i` be loaded now: it is one of the `slots` in use, and not a filled slot while "do not overwrite" is on.
fn usable(i: usize, slots: usize, keep_loaded: bool, loaded: bool) -> Result<(), String> {
    if i >= slots {
        Err(format!(
            "slot {} is not in use ({slots} slots: PREFS > Controller > Sampler)",
            i + 1
        ))
    } else if keep_loaded && loaded {
        Err(format!("slot {} is loaded", i + 1))
    } else {
        Ok(())
    }
}

/// One slot as the user set it (the slider positions are 0..1, 0.5 = unity gain).
#[derive(Clone)]
struct Cfg {
    loaded: bool,
    looped: bool,
    gate: bool,
    sync: bool,
    volume: f32,
    /// Tempo of the slice, 0 = unknown.
    bpm: f32,
    /// Dominant frequency band of the slice (0 low, 1 mid, 2 high), the slot's colour.
    band: i32,
    /// What the slice is, for the status line.
    info: String,
}

impl Default for Cfg {
    fn default() -> Cfg {
        Cfg {
            loaded: false,
            looped: false,
            gate: false,
            sync: false,
            volume: 0.5,
            bpm: 0.0,
            band: 0,
            info: String::new(),
        }
    }
}

/// A TRK load being decoded.
struct Pending {
    slot: usize,
    bpm: f32,
    rx: Receiver<Result<Pcm, String>>,
}
/// Slots to fill from a sample pack ([`load_bank`]): (slot, file, BPM), decoded one after the other.
type Bank = VecDeque<(usize, PathBuf, f32)>;

thread_local! {
    /// The panel's sampler, for [`load_bank`].
    static ME: RefCell<Weak<Sampler>> = RefCell::new(Weak::new());
}

struct Sampler {
    app: slint::Weak<App>,
    /// None when no deck has audio: the panel works but drives nothing.
    bus: Option<Arc<Bus>>,
    decks: Decks,
    track: Rc<RefCell<Option<Track>>>,
    model: Rc<VecModel<SlotView>>,
    cfg: RefCell<[Cfg; SLOTS]>,
    /// Slots in use (CONTROLLER > Sampler > SLOT), kept here so the 50 ms refresh does not read the settings.
    used: Cell<usize>,
    beats: Cell<usize>,
    pending: RefCell<Option<Pending>>,
    bank: RefCell<Bank>,
    timer: slint::Timer,
    seq: seq::Ui,
}

impl Sampler {
    fn state(&self) -> Option<(App, usize)> {
        let app = self.app.upgrade()?;
        let sel = app
            .global::<SamplerState>()
            .get_selected()
            .clamp(0, SLOTS as i32 - 1) as usize;
        Some((app, sel))
    }

    fn say(&self, text: impl Into<slint::SharedString>) {
        if let Some(app) = self.app.upgrade() {
            app.global::<SamplerState>().set_status(text.into());
        }
    }

    /// The settings the panel shows: slots in use, the colour mode; slots that are no longer in use stop.
    fn show_prefs(&self, p: &prefs::Prefs) {
        let n = p.controller.slots();
        self.used.set(n);
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let s = app.global::<SamplerState>();
        s.set_slot_count(n as i32);
        s.set_purple(p.view.sampler_color == SamplerColor::Purple);
        if n <= 8 && s.get_bank() == 1 {
            s.set_bank(0);
        }
        if s.get_selected() >= n as i32 {
            self.select(0);
        }
        if let Some(bus) = &self.bus {
            (n..SLOTS).for_each(|i| bus.sampler.stop(i));
        }
        self.refresh();
        self.decks.iter().for_each(|d| d.refresh_pads());
    }

    /// May slot `i` be loaded (see [`usable`])?
    fn check(&self, i: usize) -> Result<(), String> {
        usable(
            i,
            self.used.get(),
            prefs::with(|p| p.controller.sampler_keep_loaded),
            self.cfg.borrow()[i].loaded,
        )
    }

    /// Mirrors slot `i`'s settings into the audio side.
    fn apply(&self, i: usize) {
        if let Some(bus) = &self.bus {
            let c = &self.cfg.borrow()[i];
            let s = &bus.sampler;
            s.set_loop(i, c.looped);
            s.set_gate(i, c.gate);
            s.set_sync(i, c.sync);
            s.set_volume(i, gain(c.volume));
        }
        self.refresh();
        self.decks.iter().for_each(|d| d.refresh_pads()); // S.SCRATCH pads light per filled slot
    }

    /// Pads' loaded / playing / loop flags (a slot past the ones in use shows as empty).
    fn refresh(&self) {
        let cfg = self.cfg.borrow();
        for (i, c) in cfg.iter().enumerate() {
            let v = SlotView {
                loaded: c.loaded && i < self.used.get(),
                playing: self.bus.as_ref().is_some_and(|b| b.sampler.playing(i)),
                looped: c.looped,
                band: c.band,
                muted: self.bus.as_ref().is_some_and(|b| b.sampler.seq_muted(i)),
            };
            if self.model.row_data(i).as_ref() != Some(&v) {
                self.model.set_row_data(i, v);
            }
        }
    }

    /// Shows slot `i`'s gate / sync / volume on the controls.
    fn mirror(&self, i: usize) {
        if let Some(app) = self.app.upgrade() {
            let (s, c) = (app.global::<SamplerState>(), &self.cfg.borrow()[i]);
            s.set_gate(c.gate);
            s.set_sync(c.sync);
            s.set_volume(c.volume);
        }
    }

    /// Makes `i` the selected slot: the controls show its settings.
    fn select(&self, i: usize) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let s = app.global::<SamplerState>();
        s.set_selected(i as i32);
        self.mirror(i);
        let c = &self.cfg.borrow()[i];
        s.set_status(if c.loaded {
            c.info.as_str().into()
        } else {
            format!("slot {} is empty: pick A–D / TRK, then LOAD", i + 1).into()
        });
    }

    /// Edits the selected slot's settings, mirrors them to the audio side and the controls.
    fn edit(&self, f: impl FnOnce(&mut Cfg)) {
        if let Some((_, i)) = self.state() {
            f(&mut self.cfg.borrow_mut()[i]);
            self.apply(i);
            self.mirror(i);
        }
    }

    /// Puts `pcm` (tempo `bpm`, 0 = unknown) into slot `i`.
    fn give(&self, i: usize, pcm: Pcm, bpm: f32) -> Result<(), String> {
        let bus = self.bus.as_ref().ok_or("audio disabled")?;
        if pcm.samples.is_empty() {
            return Err("nothing to capture here (playhead at the end?)".into());
        }
        let secs = pcm.samples.len() as f64 / 2.0 / pcm.rate as f64;
        let info = if bpm > 0.0 {
            format!(
                "slot {}: {:.1} beats · {bpm:.1} BPM",
                i + 1,
                secs * bpm as f64 / 60.0
            )
        } else {
            format!("slot {}: {secs:.1} s · no tempo, SYNC does nothing", i + 1)
        };
        let band = capture::band(&pcm);
        bus.sampler.load(i, pcm, bpm);
        {
            let c = &mut self.cfg.borrow_mut()[i];
            (c.loaded, c.bpm, c.band, c.info) = (true, bpm, band, info.clone());
        }
        self.apply(i);
        self.say(info);
        Ok(())
    }

    /// Starts decoding `file` into slot `slot` (a TRK load or the next pack file); the timer takes the result.
    fn decode(&self, slot: usize, file: PathBuf, bpm: f32) {
        *self.pending.borrow_mut() = Some(Pending {
            slot,
            bpm,
            rx: audio::decode_async(file),
        });
        self.say(format!("slot {}: decoding…", slot + 1));
    }

    /// The 50 ms timer: the finished decode, the next pack file, the pads, the sequencer and the master tempo.
    fn tick(&self) {
        let done = self.pending.borrow().as_ref().map(|p| p.rx.try_recv());
        match done {
            Some(Err(TryRecvError::Empty)) | None => {}
            Some(r) => {
                let p = self.pending.borrow_mut().take().unwrap();
                let r = r
                    .map_err(|_| "decoder stopped".to_owned())
                    .and_then(|r| r)
                    .and_then(|mut pcm| {
                        self.check(p.slot)?; // the slot may have been filled / dropped from use while the track decoded
                        pcm.samples
                            .truncate(2 * (MAX_SECS * pcm.rate as f64) as usize);
                        self.give(p.slot, pcm, p.bpm)?;
                        self.sequence(p.slot);
                        Ok(())
                    });
                if let Err(e) = r {
                    self.say(e);
                }
            }
        }
        if self.pending.borrow().is_none() {
            let next = self.bank.borrow_mut().pop_front();
            if let Some((slot, file, bpm)) = next {
                self.decode(slot, file, bpm);
            }
        }
        self.refresh();
        self.seq_tick();
        let Some(app) = self.app.upgrade() else {
            return;
        };
        if let Some(bus) = &self.bus {
            let decks = app.global::<DeckState>();
            let row = |i| {
                decks
                    .get_decks()
                    .row_data(i)
                    .map_or((false, 0.0), |v| (v.playing, v.bpm))
            };
            let rows: [(bool, f32); DECKS] = std::array::from_fn(row);
            let playing = super::fx::tempo(decks.get_master(), rows);
            bus.sampler.set_bpm(playing.unwrap_or(0.0));
            // the sequencer also runs on the master's loaded tempo while no deck plays (a pattern can be recorded before the mix starts)
            let seq = playing
                .or_else(|| super::fx::tempo(decks.get_master(), rows.map(|(_, bpm)| (true, bpm))));
            self.seq.bpm.set(seq.unwrap_or(0.0));
            bus.sampler.set_seq_bpm(seq.unwrap_or(0.0));
        }
    }
}

/// A controller's SAMPLER button: opens the sampler panel when CONTROLLER > Sampler > Auto Panel Mode is on.
pub fn controller_button(app: &App) {
    if prefs::with(|p| p.controller.sampler_auto_panel) {
        app.global::<SamplerState>().set_open(true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_deck_is_a_source_and_trk_follows_them() {
        assert_eq!(
            (0..DECKS as i32).map(deck_source).collect::<Vec<_>>(),
            (0..DECKS).map(Some).collect::<Vec<_>>()
        );
        assert_eq!((deck_source(DECKS as i32), deck_source(-1)), (None, None)); // TRK = the chip after the last deck (ui/sampler.slint)
    }

    #[test]
    fn only_the_first_n_slots_are_usable() {
        let ok = |slots| {
            (0..SLOTS)
                .filter(|&i| usable(i, slots, false, false).is_ok())
                .count()
        };
        assert_eq!([4, 8, 16].map(ok), [4, 8, 16]);
        assert!(
            usable(4, 4, false, false)
                .unwrap_err()
                .contains("slot 5 is not in use")
        );
    }

    #[test]
    fn do_not_overwrite_refuses_only_a_filled_slot() {
        assert_eq!(usable(2, 16, true, true).unwrap_err(), "slot 3 is loaded");
        assert!(
            usable(2, 16, true, false).is_ok() && usable(2, 16, false, true).is_ok(),
            "an empty slot loads; a filled one too when the setting is off"
        );
    }
}
