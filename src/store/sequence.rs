//! Saved sampler sequences (the sequencer panel keeps up to 8; SEQ. CALL plays them from the pads). A
//! slot holds the pattern's length in bars, the sampler bank it was played on and its events packed by
//! [`crate::audio::sampler::pack`] (slot and tick), stored as space-separated numbers.

use super::{R, Store, query_all};
use rusqlite::params;

/// Sequences that can be saved.
pub const MAX_SEQUENCES: usize = 8;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sequence {
    pub bars: u8,
    /// Sampler bank (0 or 1) of the slots it plays.
    pub bank: u8,
    pub events: Vec<u32>,
}

impl Store {
    /// The saved sequences, index = save slot.
    pub fn sequences(&self) -> R<[Option<Sequence>; MAX_SEQUENCES]> {
        let rows: Vec<(usize, Sequence)> = query_all(
            &self.db(),
            "SELECT slot, bars, bank, events FROM sequence",
            [],
            |r| {
                let events: String = r.get(3)?;
                Ok((
                    r.get::<_, i64>(0)? as usize,
                    Sequence {
                        bars: r.get(1)?,
                        bank: r.get(2)?,
                        events: events
                            .split_whitespace()
                            .filter_map(|e| e.parse().ok())
                            .collect(),
                    },
                ))
            },
        )?;
        let mut out: [Option<Sequence>; MAX_SEQUENCES] = Default::default();
        for (slot, s) in rows {
            if let Some(cell) = out.get_mut(slot) {
                *cell = Some(s);
            }
        }
        Ok(out)
    }

    /// Saves `s` in slot `slot` (0..[`MAX_SEQUENCES`]), replacing what was there.
    pub fn save_sequence(&self, slot: usize, s: &Sequence) -> R<()> {
        if slot >= MAX_SEQUENCES {
            return Err(format!("sequence slot {} does not exist", slot + 1));
        }
        let events = s
            .events
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(" ");
        self.run(
            "INSERT OR REPLACE INTO sequence(slot, bars, bank, events) VALUES (?1, ?2, ?3, ?4)",
            params![slot as i64, s.bars, s.bank, events],
        )
    }

    pub fn delete_sequence(&self, slot: usize) -> R<()> {
        self.run("DELETE FROM sequence WHERE slot = ?1", [slot as i64])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sequence_round_trips_per_slot_and_can_be_deleted() {
        let st = Store::in_memory().unwrap();
        assert!(st.sequences().unwrap().iter().all(Option::is_none));
        let a = Sequence {
            bars: 2,
            bank: 1,
            events: vec![1 << 31, 1 << 31 | 5 << 16 | 96],
        };
        st.save_sequence(3, &a).unwrap();
        st.save_sequence(
            3,
            &Sequence {
                bars: 4,
                ..a.clone()
            },
        )
        .unwrap(); // replaces
        let got = st.sequences().unwrap();
        assert_eq!(got[3], Some(Sequence { bars: 4, ..a }));
        assert!(got.iter().enumerate().all(|(i, s)| (i == 3) == s.is_some()));
        assert!(st.save_sequence(8, &got[3].clone().unwrap()).is_err());
        st.delete_sequence(3).unwrap();
        assert!(st.sequences().unwrap()[3].is_none());
    }
}
