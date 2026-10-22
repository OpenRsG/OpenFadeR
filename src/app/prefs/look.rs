//! Pushes the VIEW settings that restyle the whole window into the Slint side: the `Theme` global
//! (ui/theme.slint: the skin ([`crate::app::skin`]), tooltips on / off, the browse lists' character size, weight and row height),
//! `PrefsState.wave-order` (which deck each enlarged waveform shows) and the layout selector's choices
//! (`PrefsState.vertical`, `PrefsState.export-layout`). Consumers bind to those, never to the settings.

use super::{FontSize, LineSpace, Prefs, view, view::DeckLayout};
use crate::app::{App, DeckState, PrefsState, Theme, skin};
use slint::{ComponentHandle, ModelRc, VecModel};

/// Character size of the browse lists in px (VIEW > Browse > FontSize; values ours).
fn font_px(f: FontSize) -> f32 {
    match f {
        FontSize::Small => 10.0,
        FontSize::Normal => 11.0,
        FontSize::Large => 13.0,
    }
}

/// Row height of the browse lists in px (VIEW > Browse > Line Space; values ours). The 2-deck layout at 1100x960
/// keeps at least 8 rows in the browser even with the largest spacing.
fn row_px(l: LineSpace) -> f32 {
    match l {
        LineSpace::Small => 18.0,
        LineSpace::Medium => 22.0,
        LineSpace::Large => 28.0,
    }
}

/// The deck in each enlarged-waveform slot, top to bottom: VIEW's order restricted to the decks in use.
fn wave_slots(p: &Prefs) -> Vec<i32> {
    p.view
        .wave_order()
        .into_iter()
        .filter(|&d| d < p.decks)
        .map(|d| d as i32)
        .collect()
}

/// The skin and list settings in `window`'s own `Theme` (every window root has its own globals: the split-screen browser too).
pub fn apply_theme<'a, C: ComponentHandle>(window: &'a C, p: &Prefs)
where
    Theme<'a>: slint::Global<'a, C>,
    crate::app::Palette<'a>: slint::Global<'a, C>,
{
    skin::apply(window, view::skin_name(p));
    let t = window.global::<Theme>();
    t.set_tooltips(p.view.tooltips);
    t.set_tempo_decimals(p.view.tempo_decimals.digits() as i32);
    t.set_list_font(font_px(p.view.browse_font));
    t.set_list_weight(if p.view.browse_bold { 700 } else { 400 });
    t.set_h_row(row_px(p.view.line_space));
}

/// Pushes everything [`apply_theme`] covers plus the wave order and layout choices into the main window.
pub fn apply(app: &App, p: &Prefs) {
    apply_theme(app, p);
    let s = app.global::<PrefsState>();
    s.set_wave_order(ModelRc::new(VecModel::from(wave_slots(p))));
    s.set_vertical(p.view.deck_layout == DeckLayout::Vertical);
    s.set_export_layout(p.view.export_layout as i32);
    s.set_show_mixer(p.view.show_mixer);
    app.global::<DeckState>().set_tempo_reverse(p.view.tempo_reverse);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enlarged_waveform_order_keeps_only_the_decks_in_use() {
        let mut p = Prefs::default();
        p.view.wave_order = 3142;
        p.decks = 4;
        assert_eq!(wave_slots(&p), [2, 0, 3, 1]);
        p.decks = 2;
        assert_eq!(wave_slots(&p), [0, 1]);
    }
}
