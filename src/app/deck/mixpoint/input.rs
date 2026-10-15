//! What the user does in the panel and on the enlarged waveforms: the callbacks of `MixPointState`.

use super::{CUE_HIT, LANE_OUT, Mix, Mode, cue_times, plan, probe, snap};
use crate::app::prefs;

impl Mix {
    pub(super) fn set_mode(&self, n: i32) {
        self.s.borrow_mut().mode = if n == 1 { Mode::Out } else { Mode::In };
        self.render();
    }

    pub(super) fn select_deck(&self, d: usize) {
        let (mode, has_point) = {
            let s = self.s.borrow();
            (s.mode, s.point(d, s.mode).is_some())
        };
        if has_point {
            self.s.borrow_mut().clear(d, mode);
            self.restore_sync(d);
        } else if let Err(why) = self.eligible(mode, d) {
            self.say(why);
        } else {
            self.s.borrow_mut().select(d);
        }
        self.render();
    }

    /// MIX POINT SELECT: the selected deck's next Hot Cue / memory cue, else its current cue point.
    pub(super) fn pick_cue(&self) {
        let (mode, sel, auto) = {
            let s = self.s.borrow();
            (s.mode, s.selected(), self.opts.get().auto_switch)
        };
        let Some(d) = sel else {
            return self
                .say("MIX POINT SELECT: pick a deck first (its button, or click its waveform)");
        };
        let ms = {
            let l = self.decks[d].loaded.borrow();
            plan::next_cue(cue_times(&l), self.s.borrow().point(d, mode), l.cue_ms)
        };
        self.s.borrow_mut().set_point(d, ms, auto);
        self.render();
    }

    pub(super) fn silent_toggled(&self) {
        let mut s = self.s.borrow_mut();
        match s.inn {
            Some(i) => s.silent[i] = !s.silent[i],
            None => {
                drop(s);
                return self.say("SILENT: pick the MIX IN deck first");
            }
        }
        drop(s);
        self.render();
    }

    pub(super) fn silent_edit_toggled(&self) {
        let mut s = self.s.borrow_mut();
        s.silent_edit = !s.silent_edit;
        drop(s);
        self.render();
    }

    /// LINK: arms the pair; a MIX IN deck that is already running is put back in step with the MIX OUT deck.
    pub(super) fn link(&self) {
        let Some((o, i)) = self.s.borrow_mut().press_link() else {
            return self
                .say("MIX POINT LINK: set a MIX POINT on the MIX OUT deck and on the MIX IN deck");
        };
        let (po, pi) = (probe(&self.decks[o]), probe(&self.decks[i]));
        let (out_ms, in_ms) = {
            let s = self.s.borrow();
            (s.out_ms[o], s.in_ms[i])
        };
        if let (true, true, Some(out_ms), Some(in_ms)) = (po.playing, pi.playing, out_ms, in_ms) {
            let at = plan::in_position(
                po.pos,
                f64::from(out_ms),
                po.rate,
                f64::from(in_ms),
                pi.rate,
            );
            if (0.0..pi.duration).contains(&at) {
                self.decks[i].with_player(|p| p.seek(at.round() as u32));
            }
        }
        self.render();
    }

    /// The X of a MIX POINT readout: 0 MIX IN, 1 MIX OUT.
    pub(super) fn clear(&self, which: i32) {
        let (mode, deck) = {
            let s = self.s.borrow();
            if which == 1 {
                (Mode::Out, s.out)
            } else {
                (Mode::In, s.inn)
            }
        };
        if let Some(d) = deck {
            self.s.borrow_mut().clear(d, mode);
            self.restore_sync(d);
        }
        self.render();
    }

    /// A click on a deck's enlarged waveform while the panel is on screen: SILENT EDIT moves the silent end of the MIX IN deck,
    /// otherwise the deck becomes the MIX OUT / MIX IN deck of the mode and its MIX POINT goes where the click was (on a cue
    /// marker: the cue; else the nearest SNAP beat while Q is on).
    pub(super) fn wave_click(&self, deck: usize, frac: f32) {
        let p = probe(&self.decks[deck]);
        if !p.ready {
            return;
        }
        let ms = {
            let l = self.decks[deck].loaded.borrow();
            let at = (p.pos + (f64::from(frac) - 0.5) * p.span).clamp(0.0, p.duration);
            let plain = at as u32;
            let hit = plan::snap_to_cue(cue_times(&l), plain, p.span * CUE_HIT);
            if hit != plain {
                hit
            } else if p.quantize {
                snap(
                    &l.analysis.beats,
                    at,
                    prefs::with(|p| p.controller.snap_length.beats()),
                )
            } else {
                plain
            }
        };
        let (mode, auto, editing) = {
            let s = self.s.borrow();
            let editing = s.silent_edit && s.inn == Some(deck) && s.in_ms[deck].is_some();
            (s.mode, self.opts.get().auto_switch, editing)
        };
        if editing {
            let mut s = self.s.borrow_mut();
            let end = s.in_ms[deck].map_or(ms, |p| ms.min(p));
            s.silent_end[deck] = Some(end);
        } else {
            let selected = self.s.borrow().selected() == Some(deck);
            let eligible = if selected {
                Ok(())
            } else {
                self.eligible(mode, deck)
            };
            if let Err(why) = eligible {
                return self.say(why);
            }
            self.s.borrow_mut().set_point(deck, ms, auto);
        }
        self.render();
    }

    pub(super) fn resized(&self, which: usize, w: f32, h: f32) {
        let mut sizes = self.sizes.get();
        if let Some(s) = sizes.get_mut(which) {
            *s = (w, h);
            self.sizes.set(sizes);
        }
    }

    /// Dragging a lane moves what the panel shows with the hand.
    pub(super) fn lane_drag(&self, dx: f32) {
        let w = f64::from(self.sizes.get()[LANE_OUT].0).max(1.0);
        let mut v = self.view.get();
        v.move_to(v.centre - f64::from(dx) / w * v.span, self.ext.get());
        self.view.set(v);
        self.render();
    }

    /// The overview's white frame was dragged to `frac` of the overview.
    pub(super) fn overview_move(&self, frac: f32) {
        let ext = self.ext.get();
        let mut v = self.view.get();
        v.move_to(
            ext.0 + f64::from(frac.clamp(0.0, 1.0)) * (ext.1 - ext.0),
            ext,
        );
        self.view.set(v);
        self.render();
    }

    pub(super) fn zoom(&self, steps: f32) {
        let mut v = self.view.get();
        v.zoom(f64::from(steps), self.ext.get(), self.opts.get().zoom_limit);
        self.view.set(v);
        self.render();
    }
}
