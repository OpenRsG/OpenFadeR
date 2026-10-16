//! The deck's cue / loop list (`DeckView.cues`, drawn by ui/pads.slint): every cue of the loaded
//! analysis (memory cues, hot cues and loops, a loop being a cue with a loop end) in track order. Clicking a
//! row jumps to it; a loop row also activates the loop. SAVE LOOP stores the active loop as a memory
//! loop; right-click a row to pick its colour and comment. Every change is saved to the store
//! ([`openfader::store`]) and comes back on the next LOAD; the library's files are never written.
//!
//! Format (see `analysis::cues`): a loop is a cue whose `loop_end_ms` is set; `hot_cue` 0 is
//! a memory cue.

use super::{Deck, Loaded, pad_label, quantize::Call};
use crate::app::{CueRowView, prefs, render::cue_color};
use openfader::analysis::{Analysis, Cue, picker};
use openfader::store;
use slint::Color;

/// What a click on a list row does.
#[derive(Debug, PartialEq)]
enum Jump {
    /// Move the playhead.
    Seek(u32),
    /// Activate the loop and move the playhead to its start.
    Loop(u32, u32),
}

/// A cue's loop span; a loop end not after the start is ignored (plain cue).
fn span(c: &Cue) -> Option<(u32, u32)> {
    c.loop_end_ms
        .filter(|&e| e > c.time_ms)
        .map(|e| (c.time_ms, e))
}

/// Indices into `cues` in list order: by time, memory before hot, then pad number (stable).
fn order(cues: &[Cue]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..cues.len()).collect();
    order.sort_by_key(|&i| (cues[i].time_ms, cues[i].hot_cue));
    order
}

fn nth_index(cues: &[Cue], row: usize) -> Option<usize> {
    order(cues).get(row).copied()
}

/// The cue shown in list row `row`, mutable.
fn nth_mut(cues: &mut [Cue], row: usize) -> Option<&mut Cue> {
    nth_index(cues, row).and_then(|i| cues.get_mut(i))
}

pub fn views(a: &Analysis) -> Vec<CueRowView> {
    order(&a.cues)
        .into_iter()
        .map(|i| {
            let c = &a.cues[i];
            let tag = match c.hot_cue {
                0 => "M".to_owned(),
                n @ 1..=8 => pad_label(n as usize - 1).to_string(),
                n => n.to_string(),
            };
            let [r, g, b] = cue_color(c);
            CueRowView {
                time: super::fmt_ms(c.time_ms).into(),
                tag: tag.into(),
                kind: if span(c).is_some() { "loop" } else { "cue" }.into(),
                len: span(c)
                    .map_or(String::new(), |(s, e)| {
                        format!("{:.2}s", (e - s) as f64 / 1000.0)
                    })
                    .into(),
                color: Color::from_rgb_u8(r, g, b),
                comment: c.comment.clone().unwrap_or_default().into(),
            }
        })
        .collect()
}

fn jump(a: &Analysis, row: usize) -> Option<Jump> {
    let c = &a.cues[nth_index(&a.cues, row)?];
    Some(span(c).map_or(Jump::Seek(c.time_ms), |(s, e)| Jump::Loop(s, e)))
}

/// Stores `(start, end)` as a memory loop; `false` if that exact loop is already listed.
fn add_loop(cues: &mut Vec<Cue>, (s, e): (u32, u32)) -> bool {
    if cues.iter().any(|c| span(c) == Some((s, e))) {
        return false;
    }
    cues.push(Cue {
        hot_cue: 0,
        time_ms: s,
        loop_end_ms: Some(e),
        color: None,
        comment: None,
    });
    true
}

/// A playhead this close to a memory cue is "on" it: the call moves on to the next one.
const CALL_MS: f64 = 50.0;

/// The memory cue (plain or loop, not a hot cue) nearest the playhead before (`dir < 0`) or after it.
fn neighbour(a: &Analysis, pos_ms: f64, dir: i32) -> Option<u32> {
    let mems = a.cues.iter().filter(|c| c.hot_cue == 0).map(|c| c.time_ms);
    if dir < 0 {
        mems.filter(|&t| (t as f64) < pos_ms - CALL_MS).max()
    } else {
        mems.filter(|&t| t as f64 > pos_ms + CALL_MS).min()
    }
}

/// Removes the memory cue (plain or loop) within [`CALL_MS`] of the playhead; `false` when there is none.
pub(super) fn delete_memory(cues: &mut Vec<Cue>, pos_ms: f64) -> bool {
    let n = cues.len();
    cues.retain(|c| c.hot_cue != 0 || (c.time_ms as f64 - pos_ms).abs() > CALL_MS);
    cues.len() != n
}

impl Deck {
    /// List row clicked: seek to the cue, or activate the loop (from its start).
    pub fn cue_jump(&self, row: usize) {
        if !self.ready() {
            return;
        }
        let l = self.loaded.borrow_mut();
        match jump(&l.analysis, row) {
            Some(Jump::Seek(ms)) => {
                drop(l);
                self.with_player(|p| p.seek(ms));
            }
            Some(Jump::Loop(s, e)) => {
                drop(l);
                self.call(Call::Loop(s, e)); // kept on the beat while playing with Q on (QUANTIZE LOOP)
            }
            None => {}
        }
    }

    /// Memory cue call « / »: `dir < 0` jumps to the previous memory cue, else the next. Refused while playing with
    /// MEMORY CUE CALL LOCK on.
    pub fn cue_call(&self, dir: i32) {
        if !self.ready() {
            return;
        }
        if prefs::with(|p| p.controller.memory_cue_lock) && self.playing() {
            return self.say("memory cue lock: no memory cue call while playing");
        }
        let (pos, _) = self.clock(&self.loaded.borrow());
        let target = neighbour(&self.loaded.borrow().analysis, pos, dir);
        match target {
            Some(ms) => {
                self.with_player(|p| p.seek(ms));
            }
            None => self.say("no memory cue that way"),
        }
    }

    /// SAVE LOOP: the active loop becomes a memory loop in the list.
    pub fn save_loop(&self) {
        if !self.ready() {
            return;
        }
        let mut l = self.loaded.borrow_mut();
        let Some(lp) = l.loop_ else {
            return self.say("no active loop to save");
        };
        if add_loop(&mut l.analysis.cues, lp) {
            self.cues_changed(
                l,
                format!(
                    "loop {} – {} saved to the list",
                    super::fmt_ms(lp.0),
                    super::fmt_ms(lp.1)
                ),
            );
        } else {
            drop(l);
            self.say("that loop is already in the list");
        }
    }

    /// Writes the cue list to the store (every cue edit goes through [`Deck::cues_changed`], which calls this), and soon to the
    /// track's device copies (cue sync). Nothing to write for a track without a library identity (sampler slice).
    pub(super) fn persist_cues(&self, l: &Loaded) -> Result<(), String> {
        l.track.as_ref().map_or(Ok(()), |t| {
            store::global().set_cues(&store::key(t), Some(&l.analysis.cues))?;
            l.auto_cues.set(false);
            crate::app::device::track_edited(t);
            Ok(())
        })
    }

    /// Cue list row `row`: colour `palette` = index into [`picker`], anything else = back to the default.
    pub fn cue_color(&self, row: usize, palette: usize) {
        self.edit_cue(row, "cue colour set", |c| c.color = picker().nth(palette));
    }

    /// Cue list row `row`: comment (blank = none).
    pub fn cue_comment(&self, row: usize, text: String) {
        let text = text.trim().to_owned();
        self.edit_cue(row, "cue comment saved", |c| {
            c.comment = Some(text).filter(|t| !t.is_empty())
        });
    }

    fn edit_cue(&self, row: usize, status: &str, edit: impl FnOnce(&mut Cue)) {
        if !self.ready() {
            return;
        }
        let mut l = self.loaded.borrow_mut();
        if let Some(c) = nth_mut(&mut l.analysis.cues, row) {
            edit(c);
            self.cues_changed(l, status.into());
        }
    }

    /// × of the EXPORT mode MEMORY / HOT CUE panels: deletes the cue or loop shown in list row `row`.
    pub fn cue_remove(&self, row: usize) {
        if !self.ready() {
            return;
        }
        let mut l = self.loaded.borrow_mut();
        if let Some(i) = nth_index(&l.analysis.cues, row) {
            l.analysis.cues.remove(i);
            self.cues_changed(l, "cue deleted".into());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cue(hot: u16, ms: u32, end: Option<u32>) -> Cue {
        Cue {
            hot_cue: hot,
            time_ms: ms,
            loop_end_ms: end,
            color: None,
            comment: None,
        }
    }

    fn analysis() -> Analysis {
        Analysis {
            cues: vec![
                cue(2, 3000, None),
                cue(0, 3000, None),
                cue(0, 1000, Some(5000)),
                cue(1, 500, None),
            ],
            ..Default::default()
        }
    }

    #[test]
    fn rows_are_in_track_order_memory_before_hot_at_the_same_time() {
        let rows = views(&analysis());
        let got: Vec<_> = rows
            .iter()
            .map(|r| {
                (
                    r.time.to_string(),
                    r.tag.to_string(),
                    r.kind.to_string(),
                    r.len.to_string(),
                )
            })
            .collect();
        let s = |a: &str, b: &str, c: &str, d: &str| {
            (a.to_owned(), b.to_owned(), c.to_owned(), d.to_owned())
        };
        assert_eq!(
            got,
            [
                s("0:00.5", "A", "cue", ""),
                s("0:01.0", "M", "loop", "4.00s"),
                s("0:03.0", "M", "cue", ""),
                s("0:03.0", "B", "cue", "")
            ]
        );
    }

    #[test]
    fn clicking_a_cue_seeks_and_a_loop_activates_it() {
        let a = analysis();
        assert_eq!(
            [jump(&a, 0), jump(&a, 1), jump(&a, 3)],
            [
                Some(Jump::Seek(500)),
                Some(Jump::Loop(1000, 5000)),
                Some(Jump::Seek(3000))
            ]
        );
        assert_eq!(jump(&a, 4), None);
    }

    #[test]
    fn a_loop_end_not_after_the_start_is_a_plain_cue() {
        let a = Analysis {
            cues: vec![cue(0, 1000, Some(1000)), cue(0, 2000, Some(10))],
            ..Default::default()
        };
        assert_eq!(
            [jump(&a, 0), jump(&a, 1)],
            [Some(Jump::Seek(1000)), Some(Jump::Seek(2000))]
        );
        assert!(views(&a).iter().all(|r| r.kind.as_str() == "cue"));
    }

    #[test]
    fn a_saved_loop_joins_the_list_once() {
        let mut a = Analysis::default();
        assert!(add_loop(&mut a.cues, (2000, 4000)));
        assert!(!add_loop(&mut a.cues, (2000, 4000)));
        assert!(add_loop(&mut a.cues, (2000, 3000))); // same start, other length: another loop
        let rows = views(&a);
        assert_eq!(
            rows.iter().map(|r| r.len.to_string()).collect::<Vec<_>>(),
            ["2.00s", "1.00s"]
        ); // equal sort keys keep insertion order
        assert_eq!(jump(&a, 0), Some(Jump::Loop(2000, 4000)));
    }

    #[test]
    fn memory_cue_call_steps_to_the_neighbour_and_skips_the_one_underfoot() {
        let a = Analysis {
            cues: vec![
                cue(0, 1000, None),
                cue(0, 4000, None),
                cue(2, 2500, None),
                cue(0, 7000, Some(8000)),
            ],
            ..Default::default()
        };
        assert_eq!(
            [neighbour(&a, 4000.0, -1), neighbour(&a, 4010.0, 1)],
            [Some(1000), Some(7000)]
        );
        assert_eq!(
            [neighbour(&a, 500.0, -1), neighbour(&a, 7000.0, 1)],
            [None, None]
        );
        assert_eq!(
            neighbour(&a, 3000.0, 1),
            Some(4000),
            "hot cues are not memory cues"
        );
    }

    #[test]
    fn a_row_maps_to_its_cue_through_the_list_order() {
        let mut a = analysis();
        nth_mut(&mut a.cues, 1).unwrap().comment = Some("drop".into()); // row 1 = the memory loop at 1:00
        assert_eq!(a.cues[2].comment.as_deref(), Some("drop"));
        assert_eq!(views(&a)[1].comment.as_str(), "drop");
        assert!(nth_mut(&mut a.cues, 4).is_none());
    }
}
