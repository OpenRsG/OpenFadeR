//! Which playlist each of the three playlist palettes shows. Kept in the store's `device_setting` rows like the other
//! small browser settings ([`super::super::played`]): scope [`SCOPE`], key = the palette's number (1..=3), value = the
//! playlist's node id. A palette without a row is blank.

use openfader::store::Store;
use std::collections::BTreeMap;

/// Playlist palettes; the TAG LIST palette in front of them is not assigned.
pub const SLOTS: usize = 3;
const SCOPE: &str = "browser_palette";

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Assign([Option<i64>; SLOTS]);

impl Assign {
    fn parse(rows: &BTreeMap<String, String>) -> Self {
        let mut a = Self::default();
        for (slot, id) in (0..SLOTS).zip(1..) {
            a.0[slot] = rows.get(&id.to_string()).and_then(|v| v.parse().ok());
        }
        a
    }

    /// What the last run left; a blank palette when the store cannot be read.
    pub fn load(store: &Store) -> Self {
        Self::parse(&store.device_settings(SCOPE).unwrap_or_default())
    }

    /// The playlist (node id) of playlist palette `slot` (0-based).
    pub fn get(&self, slot: usize) -> Option<i64> {
        self.0.get(slot).copied().flatten()
    }

    /// Shows playlist `id` in `slot` (`None`: blank), overwriting what it showed, and keeps it.
    pub fn set(&mut self, store: &Store, slot: usize, id: Option<i64>) -> Result<(), String> {
        let key = (slot + 1).to_string();
        match id {
            Some(id) => store.set_device_setting(SCOPE, &key, &id.to_string())?,
            None => store.clear_device_setting(SCOPE, &key)?,
        }
        self.0[slot] = id;
        Ok(())
    }

    /// Blanks the palettes whose playlist `alive` no longer finds (deleted); returns whether any changed.
    pub fn prune(&mut self, store: &Store, alive: impl Fn(i64) -> bool) -> bool {
        let dead: Vec<usize> = (0..SLOTS)
            .filter(|&s| self.get(s).is_some_and(|id| !alive(id)))
            .collect();
        for &s in &dead {
            let _ = self.set(store, s, None);
        }
        !dead.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assignments_survive_a_restart_and_overwrite() {
        let store = Store::in_memory().unwrap();
        let mut a = Assign::load(&store);
        assert_eq!(a, Assign::default());
        a.set(&store, 0, Some(7)).unwrap();
        a.set(&store, 2, Some(9)).unwrap();
        // "restart": a new Assign reads the store
        let mut b = Assign::load(&store);
        assert_eq!((b.get(0), b.get(1), b.get(2)), (Some(7), None, Some(9)));
        // another playlist onto the same palette overwrites it, a blank one empties it
        b.set(&store, 0, Some(11)).unwrap();
        b.set(&store, 2, None).unwrap();
        let c = Assign::load(&store);
        assert_eq!((c.get(0), c.get(1), c.get(2)), (Some(11), None, None));
    }

    #[test]
    fn a_deleted_playlist_blanks_its_palette() {
        let store = Store::in_memory().unwrap();
        let mut a = Assign::default();
        a.set(&store, 0, Some(1)).unwrap();
        a.set(&store, 1, Some(2)).unwrap();
        assert!(!a.prune(&store, |_| true));
        assert!(a.prune(&store, |id| id != 2));
        assert_eq!(Assign::load(&store), a);
        assert_eq!((a.get(0), a.get(1)), (Some(1), None));
    }

    #[test]
    fn unreadable_rows_are_blank() {
        let rows = BTreeMap::from([
            ("1".to_owned(), "x".to_owned()),
            ("9".to_owned(), "3".to_owned()),
        ]);
        assert_eq!(Assign::parse(&rows), Assign::default());
    }
}
