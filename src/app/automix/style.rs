//! Catalogue, saved choice, RANDOM pool and the transition plan shown in the panel.
use super::{Automix, Run, Stage};
use crate::app::{prefs, App, AutomixState};
use openfader::{
    mixstyle::{self, Recipe},
    plugin,
};
use slint::{ComponentHandle, Model, ModelRc, VecModel};
use std::{rc::Rc, sync::LazyLock};

pub(super) fn all() -> &'static [Recipe] {
    static ALL: LazyLock<Vec<Recipe>> = LazyLock::new(|| {
        let mut all = mixstyle::builtin().to_vec();
        for r in plugin::registry().automix_styles() {
            if !all.iter().any(|o| o.name.eq_ignore_ascii_case(&r.name)) {
                all.push(r.clone());
            }
        }
        all
    });
    &ALL
}

pub(super) fn chosen(run: &Run) -> usize {
    run.styles
        .get(run.next.saturating_sub(1))
        .copied()
        .unwrap_or(0)
}

pub(super) fn roll(app: &App, styles: &mut Vec<usize>, len: usize, from: usize) {
    let s = app.global::<AutomixState>();
    let enabled: Vec<_> = (0..all().len())
        .filter(|&i| s.get_pool().row_data(i + 1).unwrap_or(true))
        .collect();
    styles.truncate(from.min(styles.len()));
    while styles.len() < len {
        let i = if s.get_style() > 0 {
            s.get_style() as usize - 1
        } else {
            mixstyle::pick(&enabled, styles.last().copied(), mixstyle::random_u64()).unwrap_or(0)
        };
        styles.push(i.min(all().len() - 1));
    }
}

pub(super) fn show(app: &App, run: &mut Run) {
    let rows: Vec<slint::SharedString> = run
        .queue
        .iter()
        .enumerate()
        .skip(run.next.saturating_sub(1))
        .map(|(i, t)| {
            format!(
                "{}. {} · {}",
                i + 1,
                t.title,
                all()[run.styles.get(i).copied().unwrap_or(0)].name
            )
            .into()
        })
        .collect();
    app.global::<AutomixState>()
        .set_plan(ModelRc::new(VecModel::from(rows)));
    run.plan_at = Some(run.next);
}

pub(super) fn wire(app: &App, am: &Rc<Automix>) {
    let s = app.global::<AutomixState>();
    let names: Vec<slint::SharedString> = std::iter::once(tr!("RANDOM").into())
        .chain(all().iter().map(|r| {
            plugin::registry()
                .plugins
                .iter()
                .find(|p| p.manifest.automix.iter().any(|s| s.name == r.name))
                .map_or_else(|| r.name.clone(), |p| p.tr(&r.name))
                .into()
        }))
        .collect();
    let (selected, disabled) = prefs::with(|p| {
        (
            p.extra("automix_style").unwrap_or("SMOOTH").to_owned(),
            p.extra("automix_disabled").unwrap_or("").to_owned(),
        )
    });
    s.set_styles(ModelRc::new(VecModel::from(names)));
    s.set_style(
        all()
            .iter()
            .position(|r| r.name == selected)
            .map_or(0, |i| i as i32 + 1),
    );
    let mut pool: Vec<_> = std::iter::once(false)
        .chain(
            all()
                .iter()
                .map(|r| !disabled.split(',').any(|n| n == r.name)),
        )
        .collect();
    if pool.iter().filter(|&&v| v).count() < 2 {
        pool.iter_mut().skip(1).for_each(|v| *v = true);
    }
    s.set_pool(ModelRc::new(VecModel::from(pool)));
    let a = am.clone();
    let weak = app.as_weak();
    s.on_style_chose(move |i| {
        let Some(app) = weak.upgrade() else { return };
        if !(0..=all().len() as i32).contains(&i) {
            return;
        }
        app.global::<AutomixState>().set_style(i);
        prefs::update(|p| {
            p.set_extra(
                "automix_style",
                if i == 0 {
                    "RANDOM"
                } else {
                    &all()[i as usize - 1].name
                },
            )
        });
        a.restyle(&app);
    });
    let a = am.clone();
    let weak = app.as_weak();
    s.on_pool_toggled(move |i| {
        let Some(app) = weak.upgrade() else { return };
        if i <= 0 || i as usize > all().len() {
            return;
        }
        let pool = app.global::<AutomixState>().get_pool();
        let was = pool.row_data(i as usize).unwrap_or(false);
        // RANDOM must have an alternative to the previous style.
        if was && pool.iter().filter(|v| *v).count() <= 2 {
            return;
        }
        pool.set_row_data(i as usize, !was);
        let disabled = all()
            .iter()
            .enumerate()
            .filter(|(i, _)| !pool.row_data(i + 1).unwrap_or(true))
            .map(|(_, r)| r.name.as_str())
            .collect::<Vec<_>>()
            .join(",");
        prefs::update(|p| p.set_extra("automix_disabled", disabled));
        a.restyle(&app);
    });
}

impl Automix {
    fn restyle(&self, app: &App) {
        let mut guard = self.run.borrow_mut();
        let Some(run) = guard.as_mut() else { return };
        let from = match run.stage {
            Stage::Armed { .. } | Stage::Loading { .. } => run.next.saturating_sub(1),
            _ => run.next,
        };
        roll(app, &mut run.styles, run.queue.len(), from);
        if let Stage::Armed {
            cur, inc, serial, ..
        } = run.stage
        {
            run.stage = self.arm(run, app, cur, inc, serial, run.rushed);
        }
        show(app, run);
    }
}
