//! What the panel shows and the audio side is told on a timer: DRUM RELEASE FX, the 50 ms refresh, PREFS changes.

use super::{
    DECKS, DrumSlotView, Groove, GrooveDeckView, GrooveState, RELEASE_FX, SLOTS, State, gain_text,
    prefs,
};
use openfader::audio::{
    fx::{BEAT_TEXT, Effect},
    groove::RELEASE_FX_NAMES,
};
use slint::{ComponentHandle, Model, ModelRc};

impl Groove {
    /// A DRUM RELEASE FX / ROLL / TRANS button down or up: `kind` 0 = the selected release effect, 1 = ROLL, 2 = TRANS at beat index `beat`.
    pub(super) fn fx_held(&self, d: usize, kind: i32, beat: usize, on: bool) {
        let (fx, sel) = {
            let c = &self.cfg.borrow()[d];
            (c.fx, c.beat)
        };
        let (effect, beat, level) = match kind {
            1 => (Effect::Roll, beat, 1.0),
            2 => (Effect::Trans, beat, 1.0),
            _ => (RELEASE_FX[fx], sel, 0.5),
        };
        let bpm = self.decks[d].effective_bpm() as f32;
        self.with(d, |c| {
            c.release.set_effect(effect);
            c.release.set_beat(beat);
            c.release.set_level(level);
            c.release.set_bpm(bpm);
            c.release.set_on(on);
        });
        self.cfg.borrow_mut()[d].held = on.then_some(kind as u8);
    }

    /// The 50 ms timer: finished loads, the release tempo, and what the audio thread reports.
    pub(super) fn tick(&self) {
        self.poll();
        let titles = prefs::with(|p| p.extensions.drum_titles);
        let target = self.armed().is_some();
        for d in 0..DECKS {
            let Some((slots, multi, swapped)) = self.with(d, |c| {
                let bpm = self.decks[d].effective_bpm();
                if bpm > 0.0 {
                    c.release.set_bpm(bpm as f32);
                }
                (
                    std::array::from_fn::<(State, f32, f32), SLOTS, _>(|i| {
                        (c.state(i), c.position(i), c.gain(i))
                    }),
                    c.multi(),
                    c.swapping(),
                )
            }) else {
                continue;
            };
            let mut guard = self.cfg.borrow_mut();
            let cfg = &mut guard[d];
            for (i, (state, pos, g)) in slots.into_iter().enumerate() {
                let l = cfg.loops[i].as_ref();
                let v = DrumSlotView {
                    loaded: l.is_some(),
                    state: state as i32,
                    title: l
                        .filter(|_| titles)
                        .map(|l| l.title.clone())
                        .unwrap_or_default(),
                    info: l
                        .map(|l| match gain_text(g) {
                            t if t.is_empty() => l.info.clone(),
                            t => format!("{}{t}", l.info).into(),
                        })
                        .unwrap_or_default(),
                    wave: l.map(|l| l.wave.clone()).unwrap_or_default(),
                    beats: l.map_or(0, |l| l.beats),
                    pos: (pos * 128.0).round() / 128.0,
                    target,
                };
                if cfg.slots.row_data(i).as_ref() != Some(&v) {
                    cfg.slots.set_row_data(i, v);
                }
            }
            let shown = (
                cfg.bank,
                multi,
                cfg.knob.to_bits(),
                cfg.held.is_some(),
                cfg.fx,
                cfg.beat,
                swapped,
                cfg.cap.unwrap_or(0),
            );
            if cfg.shown != Some(shown) {
                cfg.shown = Some(shown);
                self.model.set_row_data(
                    d,
                    GrooveDeckView {
                        bank: cfg.bank as i32,
                        multi,
                        master: cfg.knob,
                        slots: ModelRc::from(cfg.slots.clone()),
                        release_on: cfg.held.is_some(),
                        release_fx: RELEASE_FX_NAMES[cfg.fx].into(),
                        release_beat: BEAT_TEXT[cfg.beat].into(),
                        swapped,
                        capture_on: cfg.cap.is_some(),
                        capture_beats: cfg.cap.unwrap_or(cfg.cap_beats).to_string().into(),
                    },
                );
            }
            if cfg.swapped != swapped {
                cfg.swapped = swapped;
                self.decks[d].show_stems(); // the DRUMS button shows off while a loop has the drums
                self.decks[d].refresh_pads();
            }
        }
    }

    /// PREFS changed: the panel's switch, the factory presets' pack, and a disabled function stops its loops.
    pub(super) fn prefs_changed(&self) {
        let (on, pack_changed) = prefs::with(|p| {
            (
                p.extensions.groove_on(p.stems),
                *self.pack.borrow() != p.extensions.drum_pack,
            )
        });
        if let Some(a) = self.state() {
            let st = a.global::<GrooveState>();
            if st.get_enabled() != on {
                st.set_enabled(on);
                if !on {
                    for d in 0..DECKS {
                        self.with(d, |c| c.stop_all());
                    }
                }
            }
        }
        if pack_changed {
            self.refresh_presets();
        }
    }
}
