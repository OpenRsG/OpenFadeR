//! The track table's multi-selection: which rows of `View::list` are selected and where a Shift range starts.
//!
//! It holds indices into `list` (so a re-sort or a search never changes what is selected) and is kept in step with the visible
//! rows by [`Selection::retain`]; a rebuilt list maps it over by track identity ([`Selection::remap`]). The chosen row
//! (`View::selected`) is always one of the selected rows: `View::refresh` puts it back.

use openfader::library::Track;
use std::{
    collections::{HashMap, HashSet},
    path::Path,
};

#[derive(Default)]
pub(super) struct Selection {
    rows: HashSet<usize>,
    /// The row a Shift click or Shift+arrow extends from (the last row picked without Shift).
    anchor: Option<usize>,
}

impl Selection {
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn contains(&self, i: usize) -> bool {
        self.rows.contains(&i)
    }

    pub fn clear(&mut self) {
        self.rows.clear();
        self.anchor = None;
    }

    /// A plain click: only `i` is selected, and the next Shift range starts here.
    pub fn pick(&mut self, i: usize) {
        self.rows.clear();
        self.rows.insert(i);
        self.anchor = Some(i);
    }

    /// Ctrl+click: `i` joins or leaves the selection.
    pub fn toggle(&mut self, i: usize) {
        if !self.rows.remove(&i) {
            self.rows.insert(i);
        }
        self.anchor = Some(i);
    }

    /// Adds `i` without moving the anchor (the chosen row after a list rebuild).
    pub fn add(&mut self, i: usize) {
        self.rows.insert(i);
        self.anchor.get_or_insert(i);
    }

    /// Shift+click: the rows of `shown` between the anchor and `to` (both are indices into `list`) are the selection. A missing
    /// or filtered-out anchor starts the range at `to`.
    pub fn range(&mut self, shown: &[usize], to: usize) {
        let at = |i: usize| shown.iter().position(|&s| s == i);
        let Some(end) = at(to) else { return };
        let start = self.anchor.and_then(at).unwrap_or(end);
        self.anchor = Some(self.anchor.filter(|&a| at(a).is_some()).unwrap_or(to));
        self.rows = shown[start.min(end)..=start.max(end)]
            .iter()
            .copied()
            .collect();
    }

    /// Ctrl+A: every shown row.
    pub fn all(&mut self, shown: &[usize]) {
        self.rows = shown.iter().copied().collect();
        self.anchor = self.anchor.or_else(|| shown.first().copied());
    }

    /// What a drag that starts on shown row `row` carries (indices into `list`): the whole selection when the row is one of
    /// several selected, else the row alone.
    pub fn carried(&self, shown: &[usize], row: usize) -> Vec<usize> {
        let Some(&at) = shown.get(row) else {
            return Vec::new();
        };
        if self.rows.len() > 1 && self.rows.contains(&at) {
            self.ordered(shown)
        } else {
            vec![at]
        }
    }

    /// Drops the rows `shown` no longer lists (a filter hid them).
    pub fn retain(&mut self, shown: &[usize]) {
        if self.rows.is_empty() {
            return;
        }
        let visible: HashSet<usize> = shown.iter().copied().collect();
        self.rows.retain(|i| visible.contains(i));
        self.anchor = self.anchor.filter(|a| visible.contains(a));
    }

    /// The selected rows of `shown` (indices into `list`), in row order.
    pub fn ordered(&self, shown: &[usize]) -> Vec<usize> {
        shown
            .iter()
            .copied()
            .filter(|i| self.rows.contains(i))
            .collect()
    }

    /// One flag per shown row: whether it is selected.
    pub fn marks(&self, shown: &[usize]) -> Vec<bool> {
        shown.iter().map(|i| self.rows.contains(i)).collect()
    }

    /// `list` was replaced by `new`: the selected tracks keep their selection wherever `new` lists them (a track is the same by
    /// id and file); the rest drops. A single row is not mapped: the chosen row is put back by `View::refresh`.
    pub fn remap(&mut self, old: &[Track], new: &[Track]) {
        if self.rows.len() < 2 {
            return self.clear();
        }
        fn id(t: &Track) -> (Option<i64>, Option<&Path>) {
            (t.id, t.file.as_deref())
        }
        let mut by_id: HashMap<(Option<i64>, Option<&Path>), Vec<usize>> = HashMap::new();
        for (i, t) in new.iter().enumerate() {
            by_id.entry(id(t)).or_default().push(i);
        }
        let anchor = self.anchor.and_then(|a| old.get(a));
        let find = |t: &Track| -> Vec<usize> {
            if id(t) == (None, None) {
                // no identity but its text: compare whole tracks
                return (0..new.len()).filter(|&i| new[i] == *t).collect();
            }
            by_id.get(&id(t)).cloned().unwrap_or_default()
        };
        self.rows = self
            .rows
            .iter()
            .filter_map(|&i| old.get(i))
            .flat_map(find)
            .collect();
        self.anchor = anchor.and_then(|t| find(t).first().copied());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(s: &Selection, shown: &[usize]) -> Vec<usize> {
        s.ordered(shown)
    }

    fn track(id: i64) -> Track {
        Track {
            id: Some(id),
            title: format!("t{id}"),
            ..Track::default()
        }
    }

    #[test]
    fn click_selects_one_ctrl_toggles_and_shift_extends_from_the_anchor() {
        let shown: Vec<usize> = (0..10).collect();
        let mut s = Selection::default();
        s.pick(2);
        s.toggle(5);
        s.toggle(7);
        assert_eq!(rows(&s, &shown), [2, 5, 7]);
        s.toggle(5);
        assert_eq!(rows(&s, &shown), [2, 7]);
        s.range(&shown, 4); // the anchor is 5 (the last toggle)
        assert_eq!(rows(&s, &shown), [4, 5]);
        s.range(&shown, 8);
        assert_eq!(rows(&s, &shown), [5, 6, 7, 8]); // the anchor stays put
        s.pick(9);
        assert_eq!(rows(&s, &shown), [9]);
    }

    #[test]
    fn a_range_follows_the_rows_as_shown_not_the_list_order() {
        let shown = [4, 2, 0, 1, 3]; // sorted: list index 4 is the first row
        let mut s = Selection::default();
        s.pick(2);
        s.range(&shown, 1);
        assert_eq!(rows(&s, &shown), [2, 0, 1]);
        assert_eq!(s.marks(&shown), [false, true, true, true, false]);
    }

    #[test]
    fn the_selection_survives_a_re_sort_and_drops_rows_a_filter_hides() {
        let mut s = Selection::default();
        s.all(&[0, 1, 2, 3]);
        s.toggle(1);
        assert_eq!(rows(&s, &[3, 2, 1, 0]), [3, 2, 0]); // re-sorted: same tracks, new row order
        s.retain(&[3, 1, 0]); // a search hides list row 2
        assert_eq!(rows(&s, &[3, 1, 0]), [3, 0]);
        assert!(!s.contains(2));
        s.retain(&[0, 1, 2, 3]); // the search clears: hidden rows do not come back
        assert_eq!(rows(&s, &[0, 1, 2, 3]), [0, 3]);
    }

    #[test]
    fn a_range_with_a_hidden_anchor_starts_at_the_clicked_row() {
        let mut s = Selection::default();
        s.pick(9);
        s.retain(&[0, 1, 2]);
        s.range(&[0, 1, 2], 1);
        assert_eq!(rows(&s, &[0, 1, 2]), [1]);
    }

    #[test]
    fn select_all_covers_the_shown_rows_only() {
        let mut s = Selection::default();
        s.all(&[5, 3, 8]);
        assert_eq!(s.len(), 3);
        assert!(!s.contains(0));
    }

    #[test]
    fn a_rebuilt_list_keeps_the_selected_tracks_by_identity() {
        let old: Vec<Track> = (0..5).map(track).collect();
        let new = vec![track(4), track(9), track(1), track(2)];
        let mut s = Selection::default();
        s.pick(1);
        s.toggle(2);
        s.toggle(4);
        s.remap(&old, &new);
        assert_eq!(rows(&s, &[0, 1, 2, 3]), [0, 2, 3]);
        let mut one = Selection::default();
        one.pick(1);
        one.remap(&old, &new);
        assert_eq!(one.len(), 0); // a lone chosen row is restored by the refresh
    }
}

#[cfg(test)]
mod drag_tests {
    use super::*;

    #[test]
    fn a_drag_from_a_selected_row_carries_the_selection_in_row_order() {
        let shown = [3, 1, 0, 2];
        let mut s = Selection::default();
        s.pick(0);
        s.toggle(1);
        s.toggle(2);
        assert_eq!(s.carried(&shown, 1), [1, 0, 2]); // row 1 is list row 1: selected
        assert_eq!(s.carried(&shown, 0), [3]); // list row 3 is not selected: it goes alone
        assert_eq!(s.carried(&shown, 9), Vec::<usize>::new());
        s.pick(1);
        assert_eq!(s.carried(&shown, 1), [1]); // one selected row is a plain drag
    }
}
