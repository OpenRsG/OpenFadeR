//! The pane sizes the user drags with the `Splitter` handles (ui/panes.slint `PaneState`) ↔ the VIEW settings (`View::pane_*`,
//! `View::col_*`, px from each region's default size). [`apply`] restores them at start-up, [`save`] stores what was dragged (the
//! settings' 0.5 s watcher calls it). What a window can actually show of a stored size is `PaneState`'s clamping, not this file's.

use super::view::View;
use crate::app::{App, PaneState};
use slint::ComponentHandle;

/// Largest delta read from the file; keeps a damaged file from handing the UI absurd numbers.
const LIMIT: i32 = 4096;

/// One `View` field ↔ one `PaneState` property, listed once so that a pane cannot be restored but not saved.
macro_rules! panes {
    ($($field:ident = $get:ident / $set:ident),* $(,)?) => {
        /// Restores the dragged pane sizes from the settings (the split-screen window mirrors them).
        pub fn apply(app: &App, v: &View) {
            let s = app.global::<PaneState>();
            $(s.$set(v.$field.clamp(-LIMIT, LIMIT) as f32);)*
        }

        /// Copies the window's pane sizes into the settings.
        pub fn read(app: &App, v: &mut View) {
            let s = app.global::<PaneState>();
            $(v.$field = s.$get().round() as i32;)*
        }
    };
}
panes!(
    pane_stage = get_stage_d / set_stage_d,
    pane_player = get_player_d / set_player_d,
    pane_tree = get_tree_d / set_tree_d,
    pane_info = get_info_d / set_info_d,
    pane_palette = get_palette_d / set_palette_d,
    col_title = get_col_title_d / set_col_title_d,
    col_artist = get_col_artist_d / set_col_artist_d,
    col_genre = get_col_genre_d / set_col_genre_d,
    col_comment = get_col_comment_d / set_col_comment_d,
    col_bpm = get_col_bpm_d / set_col_bpm_d,
    col_rating = get_col_rating_d / set_col_rating_d,
    col_time = get_col_time_d / set_col_time_d,
    col_key = get_col_key_d / set_col_key_d,
);

/// Stores the pane sizes the user dragged; nothing is written when no size changed.
pub fn save(app: &App) {
    super::update(|p| read(app, &mut p.view));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::prefs::file::{load_from, save_to};
    use crate::app::{DeckState, headless, prefs::Prefs};
    use slint::LogicalSize;

    fn window(w: f32, h: f32) -> App {
        headless::init();
        let app = App::new().unwrap();
        app.window().set_size(LogicalSize::new(w, h));
        app
    }

    /// A dragged size goes into the settings file and comes back into a fresh window; a pane nobody dragged stays at its default.
    #[test]
    fn a_dragged_size_survives_the_settings_file() {
        let app = window(1100.0, 960.0);
        let s = app.global::<PaneState>();
        s.set_stage_d(120.0);
        s.set_tree_d(-30.0);
        s.set_info_d(-60.0);
        s.set_col_artist_d(44.0);
        s.set_col_title_d(27.0);
        let mut p = Prefs::default();
        read(&app, &mut p.view);
        assert_eq!(
            (
                p.view.pane_stage,
                p.view.pane_tree,
                p.view.pane_info,
                p.view.col_title,
                p.view.col_artist
            ),
            (120, -30, -60, 27, 44)
        );

        let dir = std::env::temp_dir().join(format!("openfader-panes-{}", std::process::id()));
        let file = dir.join("settings.conf");
        save_to(&file, &p).unwrap();
        let (back, notes) = load_from(&file);
        assert!(notes.is_empty(), "{notes:?}");
        std::fs::remove_dir_all(&dir).unwrap();

        let again = window(1100.0, 960.0);
        apply(&again, &back.view);
        let s = again.global::<PaneState>();
        assert_eq!(
            (
                s.get_stage_d(),
                s.get_tree_d(),
                s.get_info_d(),
                s.get_col_title_d(),
                s.get_col_artist_d()
            ),
            (120.0, -30.0, -60.0, 27.0, 44.0)
        );
        assert_eq!(
            (s.get_palette_d(), s.get_col_key_d(), s.get_player_d()),
            (0.0, 0.0, 0.0)
        );
    }

    /// A damaged file cannot push a pane out of the UI's reach.
    #[test]
    fn absurd_stored_sizes_are_limited() {
        let app = window(1100.0, 960.0);
        let mut v = View::default();
        v.pane_tree = i32::MAX;
        v.col_key = i32::MIN;
        apply(&app, &v);
        let s = app.global::<PaneState>();
        assert_eq!(
            (s.get_tree_d(), s.get_col_key_d()),
            (LIMIT as f32, -LIMIT as f32)
        );
    }

    /// The clamp rules: at 1100x960 and at the default size the browser shows at least 8 track rows with two decks and 6 with four;
    /// dragging the border as far as it goes down stops exactly there, dragging it up grows the browser; the tree column, the track
    /// info and the palette stay inside their limits whatever was dragged.
    #[test]
    fn the_browser_keeps_its_rows_and_the_panes_their_limits() {
        let app = window(1100.0, 960.0);
        let s = app.global::<PaneState>();
        for (decks, rows) in [(2, 8), (4, 6)] {
            app.global::<DeckState>().set_count(decks);
            s.set_stage_d(0.0);
            assert!(
                app.get_browser_rows() >= rows,
                "{decks} decks at the default size: {} rows",
                app.get_browser_rows()
            );
            let at_default = app.get_browser_rows();
            s.set_stage_d(5000.0);
            assert_eq!(
                app.get_browser_rows(),
                rows,
                "{decks} decks dragged all the way down"
            );
            s.set_stage_d(-5000.0);
            assert!(
                app.get_browser_rows() > at_default,
                "{decks} decks dragged all the way up"
            );
        }

        s.set_tree_d(5000.0);
        assert_eq!(s.invoke_tree_w(1100.0), 420.0);
        s.set_tree_d(-5000.0);
        assert_eq!(s.invoke_tree_w(1100.0), 120.0);
        s.set_tree_d(5000.0);
        assert!(
            s.invoke_tree_w(760.0) <= 760.0 - 422.0,
            "the list keeps its room in the narrowest window"
        );
        s.set_info_d(5000.0);
        assert_eq!(
            s.invoke_info_h(500.0),
            430.0,
            "the tree keeps 70 px over the track info"
        );
        s.set_info_d(-5000.0);
        assert_eq!(s.invoke_info_h(500.0), 80.0);
        s.set_palette_d(5000.0);
        assert_eq!(s.invoke_palette_h(500.0), 500.0 - s.invoke_browser_h(4));
        s.set_palette_d(-5000.0);
        assert_eq!(s.invoke_palette_h(500.0), 60.0);
    }
}

#[cfg(test)]
mod gestures {
    use slint::platform::{PointerEventButton as Button, WindowEvent as Event};
    use slint::{
        ComponentHandle, LogicalPosition, Model, ModelRc, PhysicalSize, StandardListViewItem,
        VecModel,
    };
    use std::{cell::Cell, rc::Rc};

    slint::slint! {
        import { TrackTable } from "../../../ui/browser-table.slint";
        import { StudioTable } from "../../../ui/studio/table.slint";
        import { StageTable } from "../../../ui/stage/table.slint";
        import { Splitter } from "../../../ui/widgets.slint";
        export { PaneState } from "../../../ui/panes.slint";
        export { BrowserState } from "../../../ui/browser-state.slint";
        export { Theme } from "../../../ui/theme.slint";
        export component Tables inherits Window {
            min-width: 0px;
            min-height: 0px;
            in property <int> variant;
            callback sorted(int);
            if root.variant == 0: TrackTable { width: root.width; height: root.height; sort-clicked(c) => { root.sorted(c); } }
            if root.variant == 1: StudioTable { width: root.width; height: root.height; }
            if root.variant == 2: StageTable { width: root.width; height: root.height; }
        }
        export component BoundedSplitter inherits Window {
            min-width: 0px;
            min-height: 0px;
            in-out property <length> value: 30px;
            Splitter { x: 50px; width: 8px; height: 80px; lo: 10px; hi: 100px; value <=> root.value; }
        }
    }

    fn point(x: f32, y: f32) -> LogicalPosition {
        LogicalPosition::new(x, y)
    }
    fn drag(window: &slint::Window, from: f32, to: f32, y: f32, button: Button) {
        window.dispatch_event(Event::PointerPressed {
            position: point(from, y),
            button,
        });
        window.dispatch_event(Event::PointerMoved {
            position: point(to, y),
        });
        window.dispatch_event(Event::PointerReleased {
            position: point(to, y),
            button,
        });
    }
    fn rows() -> Rc<VecModel<ModelRc<StandardListViewItem>>> {
        Rc::new(VecModel::from(
            ["first", "second"]
                .map(|id| {
                    let mut cells = vec![StandardListViewItem::default(); 15];
                    cells[0].text = id.into();
                    cells[14].text = id.into();
                    ModelRc::new(VecModel::from(cells))
                })
                .to_vec(),
        ))
    }

    #[test]
    fn narrow_tables_keep_the_rightmost_header_reachable_and_aligned() {
        crate::app::headless::init();
        for variant in 0..3 {
            let ui = Tables::new().unwrap();
            ui.set_variant(variant);
            ui.show().unwrap();
            crate::app::headless::last_window().set_size(PhysicalSize::new(480, 300));
            ui.global::<BrowserState>().set_tracks(rows().into());
            let sorted = Rc::new(Cell::new(-1));
            let seen = sorted.clone();
            ui.on_sorted(move |col| seen.set(col));
            let seen = sorted.clone();
            ui.global::<BrowserState>()
                .on_sort(move |col, _| seen.set(col));
            ui.window().dispatch_event(Event::PointerScrolled {
                position: point(400.0, 80.0),
                delta_x: -10000.0,
                delta_y: 0.0,
            });
            let y = ui.global::<Theme>().get_h_header() / 2.0 + ui.global::<Theme>().get_space_sm();
            drag(ui.window(), 430.0, 430.0, y, Button::Left);
            assert_eq!(
                sorted.get(),
                if variant == 2 { 6 } else { 3 },
                "variant {variant}"
            );
        }
    }

    #[test]
    fn resizing_changes_only_the_grabbed_column_and_keeps_overflow_reachable() {
        crate::app::headless::init();
        let ui = Tables::new().unwrap();
        ui.show().unwrap();
        crate::app::headless::last_window().set_size(PhysicalSize::new(1100, 300));
        ui.global::<BrowserState>().set_tracks(rows().into());
        let state = ui.global::<PaneState>();
        let y = ui.global::<Theme>().get_h_header() / 2.0;
        drag(ui.window(), 626.0, 666.0, y, Button::Left);
        assert_eq!(
            (
                state.get_col_artist_d(),
                state.get_col_title_d(),
                state.get_col_genre_d()
            ),
            (40.0, 0.0, 0.0)
        );
        drag(ui.window(), 756.0, 786.0, y, Button::Left);
        assert_eq!(
            (
                state.get_col_genre_d(),
                state.get_col_artist_d(),
                state.get_col_title_d()
            ),
            (30.0, 40.0, 0.0)
        );
        drag(ui.window(), 496.0, 516.0, y, Button::Left);
        assert_eq!(
            (state.get_col_title_d(), state.get_col_artist_d()),
            (20.0, 40.0)
        );
        ui.window().dispatch_event(Event::PointerPressed {
            position: point(686.0, y),
            button: Button::Left,
        });
        ui.window().dispatch_event(Event::PointerMoved {
            position: point(696.0, y),
        });
        ui.window().dispatch_event(Event::PointerExited);
        ui.window().dispatch_event(Event::PointerMoved {
            position: point(726.0, y),
        });
        ui.window().dispatch_event(Event::PointerReleased {
            position: point(726.0, y),
            button: Button::Left,
        });
        assert_eq!(state.get_col_artist_d(), 50.0, "cancel ends the owned drag");
        drag(ui.window(), 516.0, 536.0, y, Button::Right);
        assert_eq!(state.get_col_title_d(), 20.0);
        ui.window().dispatch_event(Event::PointerScrolled {
            position: point(900.0, 80.0),
            delta_x: -10000.0,
            delta_y: 0.0,
        });
        drag(ui.window(), 1086.0, 1094.0, y, Button::Left);
        assert_eq!(
            state.get_col_key_d(),
            8.0,
            "the last column stays resizable after horizontal scrolling"
        );
        assert_eq!(state.get_col_genre_d(), 30.0);
    }

    #[test]
    fn a_replaced_pressed_row_does_not_collapse_the_selection_onto_its_replacement() {
        crate::app::headless::init();
        for variant in 0..3 {
            let ui = Tables::new().unwrap();
            ui.set_variant(variant);
            ui.show().unwrap();
            crate::app::headless::last_window().set_size(PhysicalSize::new(1100, 300));
            let state = ui.global::<BrowserState>();
            let rows = rows();
            state.set_tracks(rows.clone().into());
            state.set_selected_rows(Rc::new(VecModel::from(vec![true, true])).into());
            state.set_selected_count(2);
            let selected = Rc::new(Cell::new(0));
            let seen = selected.clone();
            state.on_track_selected(move |_| seen.set(seen.get() + 1));
            let position = point(
                450.0,
                ui.global::<Theme>().get_h_header() + ui.global::<Theme>().get_space_sm() + 5.0,
            );
            ui.window().dispatch_event(Event::PointerPressed {
                position,
                button: Button::Left,
            });
            rows.set_row_data(0, rows.row_data(1).unwrap());
            ui.window().dispatch_event(Event::PointerReleased {
                position,
                button: Button::Left,
            });
            assert_eq!(
                selected.get(),
                0,
                "variant {variant}: old gesture cannot select a new track"
            );
            ui.window().dispatch_event(Event::PointerPressed {
                position,
                button: Button::Left,
            });
            ui.window().dispatch_event(Event::PointerReleased {
                position,
                button: Button::Left,
            });
            assert_eq!(
                selected.get(),
                1,
                "variant {variant}: a fresh click still collapses selection"
            );
        }
    }

    #[test]
    fn splitter_reset_obeys_bounds_and_right_clicks_do_not_reset() {
        crate::app::headless::init();
        let ui = BoundedSplitter::new().unwrap();
        ui.show().unwrap();
        crate::app::headless::last_window().set_size(PhysicalSize::new(200, 80));
        for _ in 0..2 {
            drag(ui.window(), 54.0, 54.0, 20.0, Button::Left);
        }
        assert_eq!(ui.get_value(), 10.0);
        ui.set_value(40.0);
        for _ in 0..2 {
            drag(ui.window(), 54.0, 54.0, 20.0, Button::Right);
        }
        assert_eq!(ui.get_value(), 40.0);
    }
}
