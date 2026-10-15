//! Patch rows: one `FixtureView` per fixture of the rig.

use super::{FixtureView, Lighting};

impl Lighting {
    pub(super) fn patch_changed(&self) {
        self.changed();
        self.show_patch();
    }

    /// Rebuilds the patch rows (colours follow from the next tick).
    pub(super) fn show_patch(&self) {
        let cfg = self.cfg.borrow();
        let conflicts = cfg.rig.conflicts();
        let count = |k| {
            cfg.rig
                .fixtures
                .iter()
                .filter(|f| f.profile.kind == k)
                .count() as i32
        };
        let rows: Vec<_> = cfg
            .rig
            .fixtures
            .iter()
            .enumerate()
            .map(|(i, f)| FixtureView {
                name: f.name.clone().into(),
                profile: f.profile.name.clone().into(),
                kind: f.profile.kind as i32,
                slot: cfg.rig.fixtures[..i]
                    .iter()
                    .filter(|g| g.profile.kind == f.profile.kind)
                    .count() as i32,
                count: count(f.profile.kind),
                colour: slint::Color::from_rgb_u8(0, 0, 0),
                universe: f.universe.into(),
                address: f.address.into(),
                conflict: conflicts[i],
            })
            .collect();
        self.model.set_vec(rows);
    }
}
