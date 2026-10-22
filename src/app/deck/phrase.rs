//! The PHRASE EDIT row of EXPORT mode's GRID panel on a deck: CUT, the type chip, dragging a
//! divider and CLEAR act on the loaded track's phrases ([`phrase_edit`], pure), save the whole list to the store
//! (`Store::set_phrase_edit`; every later load and the device export read it through the library overlay) and redraw the phrase
//! strip and the enlarged waveform's phrase band. Positions come as fractions of the overview, whose width is the track's duration.
//! The row is refused while the track has an Analysis Lock, as the grid edits are, and without
//! phrases or a beat grid to start from; a drag previews in memory and is saved when released.

use super::Deck;
use crate::app::render::phrase::phrase_color;
use openfader::{
    analysis::{self, phrase_edit, Beat, Mood, Phrases},
    store,
};

/// What the PHRASE EDIT row shows of a deck.
pub struct PhraseInfo {
    /// A track is loaded, unlocked, and has phrases or a grid to start them from.
    pub enabled: bool,
    /// Start and end of every phrase as fractions of the track.
    pub segs: Vec<(f32, f32)>,
    /// Name and colour of the selected phrase.
    pub selected: Option<(String, [u8; 3])>,
}

/// What a click on the phrase strip landed on.
pub enum Hit {
    /// The divider between phrase `i` and `i + 1`.
    Divider(usize),
    Phrase(usize),
    Nothing,
}

impl Deck {
    /// Start and end of every phrase as fractions of the track (the overview's x).
    pub(in crate::app) fn phrase_segs(&self) -> Vec<(f32, f32)> {
        let l = self.loaded.borrow();
        let dur = f64::from(l.duration_ms.max(1));
        let frac = |ms: f64| (ms / dur).clamp(0.0, 1.0) as f32;
        l.analysis
            .phrases
            .iter()
            .flat_map(|p| &p.list)
            .map(|ph| {
                let (a, b) = ph.span_ms(&l.analysis.beats);
                (frac(a), frac(b))
            })
            .collect()
    }

    /// `locked`: the deck's Analysis Lock ([`Deck::grid_info`] has read it already); `sel`: the selected phrase.
    pub(in crate::app) fn phrase_info(&self, sel: Option<usize>, locked: bool) -> PhraseInfo {
        let segs = self.phrase_segs();
        let l = self.loaded.borrow();
        let p = l.analysis.phrases.as_ref();
        PhraseInfo {
            enabled: self.ready()
                && l.track.is_some()
                && !locked
                && (p.is_some() || !l.analysis.beats.is_empty()),
            segs,
            selected: sel.and_then(|i| {
                let (p, ph) = (p?, p?.list.get(i)?);
                Some((ph.label(), phrase_color(p.mood, ph.kind, ph.variant)))
            }),
        }
    }

    /// The divider within `tol` (a fraction) of `frac`, else the phrase under it.
    pub(in crate::app) fn phrase_hit(&self, frac: f32, tol: f32) -> Hit {
        let segs = self.phrase_segs();
        if let Some(i) = segs.iter().skip(1).position(|s| (s.0 - frac).abs() <= tol) {
            Hit::Divider(i)
        } else if let Some(i) = segs.iter().position(|s| s.0 <= frac && frac < s.1) {
            Hit::Phrase(i)
        } else {
            Hit::Nothing
        }
    }

    fn phrase_refused(&self) -> bool {
        let locked = self.grid_locked();
        if locked {
            self.say(tr!(
                "Analysis Lock is on: unlock the track to edit its phrases"
            ));
        }
        locked
    }

    /// Runs one edit `f` (phrases, grid, duration in ms) on a copy of the deck's phrases (none yet = an empty list); on `Ok(status)`
    /// the result becomes the deck's phrases and the strips redraw, with `save` it is also stored. `false` when refused.
    fn phrase_change(
        &self,
        save: bool,
        f: impl FnOnce(&mut Phrases, &[Beat], f64) -> Result<String, &'static str>,
    ) -> bool {
        if !self.ready() || self.phrase_refused() {
            return false;
        }
        let mut l = self.loaded.borrow_mut();
        let mut p = l.analysis.phrases.clone().unwrap_or(Phrases {
            mood: Mood::Mid,
            list: Vec::new(),
        });
        let status = match f(&mut p, &l.analysis.beats, f64::from(l.duration_ms)) {
            Ok(s) => s,
            Err(e) => {
                drop(l);
                self.say(tr!("phrases: {}", e));
                return false;
            }
        };
        let saved = match (&l.track, save) {
            (Some(t), true) => store::global()
                .set_phrase_edit(&store::key(t), &p)
                .inspect(|()| crate::app::device::track_edited(t)),
            _ => Ok(()),
        };
        l.analysis.phrases = Some(p);
        l.invalidate();
        drop(l);
        match saved {
            Err(e) => self.say(tr!("{} · NOT saved: {}", status, e)),
            Ok(()) if !status.is_empty() => self.say(status),
            Ok(()) => {}
        }
        true
    }

    /// CUT: divides the phrase at `frac` of the track at the nearest bar line (an empty list starts from one phrase over the grid).
    pub(in crate::app) fn phrase_cut(&self, frac: f32) -> bool {
        self.phrase_change(true, |p, beats, dur| {
            if p.list.is_empty() {
                p.list = phrase_edit::blank(beats, p.mood)
                    .ok_or(tr!("a beat grid with a whole bar is needed"))?
                    .list;
            }
            let n = phrase_edit::bar_at(beats, f64::from(frac) * dur).ok_or(tr!("no beat grid"))?;
            phrase_edit::cut(p, n)
                .then(|| tr!("phrase divided at beat {}", n))
                .ok_or(tr!("click inside a phrase, away from its start"))
        })
    }

    /// The type icon of phrase `i`: its next name of the mood's legend.
    pub(in crate::app) fn phrase_next_kind(&self, i: usize) -> bool {
        self.phrase_change(true, |p, _, _| {
            phrase_edit::next_kind(p, i)
                .then(|| tr!("phrase {} is now {}", i + 1, p.list[i].label()))
                .ok_or(tr!("select a phrase first"))
        })
    }

    /// Dragging the divider after phrase `i` to `frac` of the track (snapped to a bar line); `save` on release.
    pub(in crate::app) fn phrase_move(&self, i: usize, frac: f32, save: bool) -> bool {
        self.phrase_change(save, |p, beats, dur| {
            let n = phrase_edit::bar_at(beats, f64::from(frac) * dur).ok_or(tr!("no beat grid"))?;
            phrase_edit::move_boundary(p, i, n, beats)
                .then(|| {
                    if save {
                        tr!("phrase boundary at beat {}", p.list[i].end_beat)
                    } else {
                        String::new()
                    }
                })
                .ok_or(tr!("there is no divider there"))
        })
    }

    /// CLEAR: every phrase deleted.
    pub(in crate::app) fn phrase_clear(&self) -> bool {
        self.phrase_change(true, |p, _, _| {
            phrase_edit::clear(p);
            Ok(tr!("all phrases deleted").into())
        })
    }

    /// Drops the stored edit: the track's analysed phrases (its stored ones, else ours) show again.
    pub(in crate::app) fn phrase_reset(&self) {
        if !self.ready() || self.phrase_refused() {
            return;
        }
        let mut l = self.loaded.borrow_mut();
        let Some(key) = l.track.as_ref().map(store::key) else {
            return;
        };
        let saved = store::global().clear_phrase_edit(&key);
        let phrases = self.unedited_phrases(&l);
        l.analysis.phrases = phrases;
        l.invalidate();
        drop(l);
        self.say(match saved {
            Ok(()) => tr!("phrases back to the analysis").to_owned(),
            Err(e) => tr!("phrases: {}", e),
        });
    }

    /// The loaded track's phrases without the stored edit: the library's, else ours from the grid.
    pub(super) fn unedited_phrases(&self, l: &super::Loaded) -> Option<Phrases> {
        l.track
            .as_ref()
            .and_then(|t| self.lib.analysis(t))
            .and_then(|a| a.phrases)
            .or_else(|| {
                analysis::options::get()
                    .parts
                    .phrase
                    .then(|| {
                        analysis::auto::phrases(&l.analysis.beats, &l.analysis.three_band_detail)
                    })
                    .flatten()
            })
    }
}
