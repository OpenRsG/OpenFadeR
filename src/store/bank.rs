//! Cue Banks (Media Browser > Cue Banks): a named list of up to
//! [`MAX_BANKS`] cues or loops of any tracks, letters A to H in order. Each cue keeps the track's store key (to find it
//! in the library again) and its audio file (what the device export matches on), plus the cue itself, so the list
//! still holds when the track's own cues are edited later.

use super::{R, Store, cue_at, exec, query_all, rgb, scalar};
use crate::{
    analysis::Cue,
    plugin::device::{Bank, BankCue, MAX_BANKS},
};
use rusqlite::params;
use std::{collections::HashMap, path::PathBuf};

/// One bank of a list: letter = index in [`HotCueBank::slots`].
#[derive(Clone, Debug, PartialEq)]
pub struct BankSlot {
    pub key: String,
    pub file: PathBuf,
    pub cue: Cue,
}

#[derive(Clone, Debug, PartialEq)]
pub struct HotCueBank {
    pub id: i64,
    pub name: String,
    pub slots: Vec<BankSlot>,
}

/// The collection's Cue Banks that hold at least one cue, as a device export takes them ([`Store::banks`]).
pub fn collect_banks(store: &Store) -> R<Vec<Bank>> {
    Ok(store
        .banks()?
        .into_iter()
        .filter(|b| !b.slots.is_empty())
        .map(|b| Bank {
            name: b.name,
            cues: b
                .slots
                .into_iter()
                .map(|s| BankCue {
                    file: s.file,
                    cue: s.cue,
                })
                .collect(),
        })
        .collect())
}

impl Store {
    /// Every list in order, with its banks.
    pub fn banks(&self) -> R<Vec<HotCueBank>> {
        let db = self.db();
        let mut lists: Vec<HotCueBank> = query_all(
            &db,
            "SELECT id, name FROM hotcue_bank ORDER BY pos, id",
            [],
            |r| {
                Ok(HotCueBank {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    slots: Vec::new(),
                })
            },
        )?;
        let at: HashMap<i64, usize> = lists.iter().enumerate().map(|(i, l)| (l.id, i)).collect();
        let slots: Vec<(i64, BankSlot)> = query_all(
            &db,
            "SELECT bank, key, file, hot, time_ms, loop_end_ms, color, comment FROM hotcue_bank_cue ORDER BY bank, pos",
            [],
            |r| {
                Ok((
                    r.get(0)?,
                    BankSlot {
                        key: r.get(1)?,
                        file: PathBuf::from(r.get::<_, String>(2)?),
                        cue: cue_at(r, 3)?,
                    },
                ))
            },
        )?;
        for (bank, slot) in slots {
            if let Some(&i) = at.get(&bank) {
                lists[i].slots.push(slot);
            }
        }
        Ok(lists)
    }

    pub fn add_bank(&self, name: &str) -> R<i64> {
        self.insert(
            "INSERT INTO hotcue_bank(name, pos) VALUES (?1, (SELECT COALESCE(MAX(pos) + 1, 0) FROM hotcue_bank))",
            [name],
        )
    }

    pub fn rename_bank(&self, id: i64, name: &str) -> R<()> {
        self.run(
            "UPDATE hotcue_bank SET name = ?2 WHERE id = ?1",
            params![id, name],
        )
    }

    /// Deletes the list with its banks.
    pub fn delete_bank(&self, id: i64) -> R<()> {
        self.run("DELETE FROM hotcue_bank WHERE id = ?1", [id])
    }

    /// Puts `slot` in the next free letter of list `id`; the letter's index, or an error when A to H are all taken.
    pub fn bank_add_cue(&self, id: i64, slot: &BankSlot) -> R<usize> {
        let db = self.db();
        let pos: i64 = scalar(
            &db,
            "SELECT COUNT(*) FROM hotcue_bank_cue WHERE bank = ?1",
            [id],
        )?
        .unwrap_or(0);
        if pos as usize >= MAX_BANKS {
            return Err(format!("the list is full: A–H hold {MAX_BANKS} cues"));
        }
        let c = &slot.cue;
        exec(
            &db,
            "INSERT INTO hotcue_bank_cue(bank, pos, key, file, hot, time_ms, loop_end_ms, color, comment) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![
                id,
                pos,
                slot.key,
                slot.file.to_string_lossy(),
                c.hot_cue,
                c.time_ms,
                c.loop_end_ms,
                c.color.map(rgb),
                c.comment
            ],
        )?;
        Ok(pos as usize)
    }

    /// Takes bank `pos` out of list `id`; the later ones move up a letter.
    pub fn bank_remove_cue(&self, id: i64, pos: usize) -> R<()> {
        self.write(|tx| {
            let p = [id, pos as i64];
            exec(
                tx,
                "DELETE FROM hotcue_bank_cue WHERE bank = ?1 AND pos = ?2",
                p,
            )?;
            exec(
                tx,
                "UPDATE hotcue_bank_cue SET pos = pos - 1 WHERE bank = ?1 AND pos > ?2",
                p,
            )
        })
    }

    /// Swaps the letters of banks `a` and `b` of list `id`.
    pub fn bank_swap(&self, id: i64, a: usize, b: usize) -> R<()> {
        self.write(|tx| {
            // Through -1: (bank, pos) is the key, two rows cannot hold one place even for an instant.
            for (from, to) in [(a as i64, -1), (b as i64, a as i64), (-1, b as i64)] {
                exec(
                    tx,
                    "UPDATE hotcue_bank_cue SET pos = ?3 WHERE bank = ?1 AND pos = ?2",
                    params![id, from, to],
                )?;
            }
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slot(name: &str, ms: u32, loop_end: Option<u32>) -> BankSlot {
        BankSlot {
            key: format!("{name}|1"),
            file: format!("/m/{name}").into(),
            cue: Cue {
                hot_cue: 2,
                time_ms: ms,
                loop_end_ms: loop_end,
                color: Some([1, 2, 3]),
                comment: Some("drop".into()),
            },
        }
    }

    #[test]
    fn lists_keep_their_banks_in_letter_order_and_close_gaps() {
        let s = Store::in_memory().unwrap();
        let (a, b) = (s.add_bank("Opening").unwrap(), s.add_bank("Peak").unwrap());
        for (i, n) in ["x", "y", "z"].into_iter().enumerate() {
            assert_eq!(
                s.bank_add_cue(a, &slot(n, 100 * i as u32, None)).unwrap(),
                i
            );
        }
        s.bank_add_cue(b, &slot("w", 7, Some(900))).unwrap();
        s.bank_remove_cue(a, 0).unwrap();
        s.bank_swap(a, 0, 1).unwrap();
        s.bank_swap(a, 1, 0).unwrap();
        s.rename_bank(b, "Peak time").unwrap();
        let got = s.banks().unwrap();
        assert_eq!(
            got.iter().map(|l| l.name.as_str()).collect::<Vec<_>>(),
            ["Opening", "Peak time"]
        );
        assert_eq!(
            got[0].slots,
            [slot("y", 100, None), slot("z", 200, None)],
            "y moved up to A, and two swaps put the letters back"
        );
        assert_eq!(
            got[1].slots,
            [slot("w", 7, Some(900))],
            "loop, colour and comment survive"
        );
        s.delete_bank(a).unwrap();
        assert_eq!(s.banks().unwrap().len(), 1);
        let n: i64 = s
            .db()
            .query_row(
                "SELECT COUNT(*) FROM hotcue_bank_cue WHERE bank = ?1",
                [a],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0, "deleting a list deletes its banks");
    }

    #[test]
    fn a_ninth_cue_is_refused() {
        let s = Store::in_memory().unwrap();
        let id = s.add_bank("Full").unwrap();
        for i in 0..MAX_BANKS {
            s.bank_add_cue(id, &slot("t", i as u32, None)).unwrap();
        }
        assert!(
            s.bank_add_cue(id, &slot("t", 99, None))
                .unwrap_err()
                .contains("full")
        );
        assert_eq!(s.banks().unwrap()[0].slots.len(), MAX_BANKS);
    }
}
