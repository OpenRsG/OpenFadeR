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
        let mut p = Prefs::default();
        read(&app, &mut p.view);
        assert_eq!(
            (
                p.view.pane_stage,
                p.view.pane_tree,
                p.view.pane_info,
                p.view.col_artist
            ),
            (120, -30, -60, 44)
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
                s.get_col_artist_d()
            ),
            (120.0, -30.0, -60.0, 44.0)
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
