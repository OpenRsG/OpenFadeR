//! The option chips under a deck's pads (`PadsState.options`, drawn by `PadOptions` in ui/pads.slint) for the pad modes that have some:
//! SLICER / SLICE LOOP: ◀ ▶ (move the range), LENGTH, ROLL, LOOP (the other slicer mode); SAMPLER: BANK, REC, PLAY / STOP, MUTE, ERASE (the sampler
//! panel's sequencer); SEQ. CALL: STOP. Every mode else shows none.

use super::{Deck, Decks, pads::PadMode};
use crate::app::{App, PadOpt, PadsState, SamplerState};
use slint::{ComponentHandle, Model, ModelRc, VecModel};

fn opt(text: impl Into<String>, on: bool, tip: &str) -> PadOpt {
    PadOpt {
        text: text.into().into(),
        on,
        tip: tip.into(),
    }
}

/// One row (of `decks`) of `PadsState.options` per deck.
pub(super) fn wire(app: &App, decks: &Decks) {
    let st = app.global::<PadsState>();
    st.set_options(ModelRc::new(VecModel::from(
        decks.iter().map(|_| ModelRc::default()).collect::<Vec<_>>(),
    )));
    let decks = decks.clone();
    st.on_option(move |d, i| {
        if let Some(d) = decks.get(d as usize) {
            d.pad_option(i.max(0) as usize);
        }
    });
}

impl Deck {
    /// The chips of the current mode.
    fn pad_options(&self) -> Vec<PadOpt> {
        let mode = self.pad_mode();
        match mode {
            PadMode::Slicer | PadMode::SlicerLoop => {
                let (len, roll) = self.slicer_text();
                vec![
                    opt("◀", false, tr!("Move the slicer range back by its length")),
                    opt("▶", false, tr!("Move the slicer range on by its length")),
                    opt(len, false, tr!("LENGTH of the range cut into 8 slices")),
                    opt(
                        roll,
                        false,
                        tr!("ROLL: how much of the slice loops while the pad is held"),
                    ),
                    opt(
                        tr!("LOOP"),
                        mode == PadMode::SlicerLoop,
                        tr!("SLICE LOOP: the range loops"),
                    ),
                ]
            }
            PadMode::Sampler => {
                let Some(app) = self.app.upgrade() else {
                    return vec![];
                };
                let s = app.global::<SamplerState>();
                let running = matches!(s.get_seq_mode(), 1 | 3);
                vec![
                    opt(tr!("BANK {}", s.get_bank() + 1), false, tr!("Sampler bank")),
                    opt(
                        tr!("REC"),
                        s.get_seq_mode() >= 2,
                        tr!("Sequence REC: the first pad hit starts it"),
                    ),
                    opt(
                        if running { tr!("STOP") } else { tr!("PLAY") },
                        running,
                        tr!("Play / stop the sequence"),
                    ),
                    opt(
                        tr!("MUTE"),
                        s.get_seq_mute(),
                        tr!("Then press a pad: mute its slot in the sequence"),
                    ),
                    opt(
                        tr!("ERASE"),
                        s.get_seq_erase(),
                        tr!("Then press a pad: erase its slot from the sequence"),
                    ),
                ]
            }
            PadMode::SeqCall => vec![opt(tr!("STOP"), false, tr!("Pause the sequence"))],
            _ => vec![],
        }
    }

    /// Pushes the chips of the current mode to this deck's row.
    pub(super) fn refresh_options(&self) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let row = ModelRc::new(VecModel::from(self.pad_options()));
        app.global::<PadsState>()
            .get_options()
            .set_row_data(self.index, row);
    }

    /// Chip `i` clicked.
    fn pad_option(&self, i: usize) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        match self.pad_mode() {
            m @ (PadMode::Slicer | PadMode::SlicerLoop) => match i {
                0 => self.slicer_shift(-1),
                1 => self.slicer_shift(1),
                2 => self.slicer_cycle_length(),
                3 => self.slicer_cycle_roll(),
                4 => self.set_pad_mode(if m == PadMode::Slicer {
                    PadMode::SlicerLoop
                } else {
                    PadMode::Slicer
                } as i32),
                _ => {}
            },
            PadMode::Sampler => {
                let s = app.global::<SamplerState>();
                match i {
                    0 => s.invoke_bank_toggled(),
                    1 => s.invoke_seq_rec(),
                    2 => s.invoke_seq_play(),
                    3 => s.invoke_seq_mute_toggled(),
                    _ => s.invoke_seq_erase_toggled(),
                }
            }
            PadMode::SeqCall => crate::app::sampler::call_stop(),
            _ => {}
        }
    }
}
