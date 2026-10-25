//! The playlist lists with ticks of the EXPORT and SYNC MANAGER tabs.

use super::PickRow;
use openfader::library::snapshot::Snapshot;
use slint::{Model, VecModel};
use std::{cell::RefCell, rc::Rc};

/// A list of playlists with ticks: the snapshot's playlist ids, row by row, and the model behind the list.
#[derive(Default)]
pub(super) struct Pick {
    ids: RefCell<Vec<String>>,
    pub(super) rows: Rc<VecModel<PickRow>>,
}

impl Pick {
    pub(super) fn set(&self, snap: &Snapshot, ticked: &dyn Fn(&str) -> bool) {
        let (ids, rows): (Vec<_>, Vec<_>) = snap
            .playlists()
            .map(|(p, _)| {
                (
                    p.id.clone(),
                    PickRow {
                        name: p.name.clone().into(),
                        depth: p.depth as i32,
                        folder: p.is_folder,
                        ticked: ticked(&p.id),
                    },
                )
            })
            .unzip();
        *self.ids.borrow_mut() = ids;
        self.rows.set_vec(rows);
    }

    /// A flat list of `names` (their own ids), ticked where `ticked` says so: the booth's player models.
    pub(super) fn set_names(&self, names: &[String], ticked: &dyn Fn(&str) -> bool) {
        *self.ids.borrow_mut() = names.to_vec();
        self.rows.set_vec(
            names
                .iter()
                .map(|n| PickRow {
                    name: n.as_str().into(),
                    depth: 0,
                    folder: false,
                    ticked: ticked(n),
                })
                .collect::<Vec<_>>(),
        );
    }

    /// Flips row `i`; returns its playlist id and the new tick.
    pub(super) fn toggle(&self, i: usize) -> Option<(String, bool)> {
        let mut r = self.rows.row_data(i)?;
        r.ticked = !r.ticked;
        let on = r.ticked;
        self.rows.set_row_data(i, r);
        Some((self.ids.borrow().get(i)?.clone(), on))
    }

    pub(super) fn clear(&self) {
        let rows: Vec<_> = self
            .rows
            .iter()
            .map(|r| PickRow { ticked: false, ..r })
            .collect();
        self.rows.set_vec(rows);
    }

    pub(super) fn ticked(&self) -> Vec<String> {
        self.ids
            .borrow()
            .iter()
            .zip(self.rows.iter())
            .filter(|(_, r)| r.ticked)
            .map(|(id, _)| id.clone())
            .collect()
    }
}
