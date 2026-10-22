//! VIEW > Browse > "Coloring of played tracks": a track that stayed on air for the stored history's threshold
//! ([`history::MIN_PLAYED`](super::history::MIN_PLAYED)) shows dimmed in every list until RESET NOW. The set lives in the
//! store (settings scope [`SCOPE`], key = [`store::key`]), so it survives a restart; with "reset the colour when quitting" on
//! it is emptied at the next start (we have no quit hook, and nothing can look at the colours in between).

use super::{App, BrowserState, View};
use crate::app::prefs;
use openfader::{
    library::Track,
    store::{self, Store},
};
use slint::ComponentHandle;
use std::collections::HashSet;

const SCOPE: &str = "played";

/// The stored set; emptied first when the setting says the colour resets on quit.
pub(super) fn load(store: &Store) -> HashSet<String> {
    if prefs::with(|p| p.view.played_reset_on_quit) {
        let _ = store.reset_device_settings(SCOPE, "");
    }
    store
        .device_settings(SCOPE)
        .map(|m| m.into_keys().collect())
        .unwrap_or_default()
}

impl View {
    pub(super) fn mark_played(&mut self, t: &Track) {
        let key = store::key(t);
        if self.rows.mark_played(&key) {
            let _ = self.store.set_device_setting(SCOPE, &key, "1");
        }
    }

    /// RESET NOW.
    pub(super) fn reset_played(&mut self, app: &App) {
        self.rows.reset_played();
        let msg = match self.store.reset_device_settings(SCOPE, "") {
            Ok(()) => tr!("the colour of played tracks was reset").to_owned(),
            Err(e) => tr!("store: {}", e),
        };
        self.refresh(app);
        app.global::<BrowserState>().set_status(msg.into());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_played_set_round_trips_through_the_store() {
        let store = Store::in_memory().unwrap();
        let t = Track {
            title: "One".into(),
            ..Track::default()
        };
        store
            .set_device_setting(SCOPE, &store::key(&t), "1")
            .unwrap();
        let set = load(&store);
        assert!(set.contains(&store::key(&t)) && set.len() == 1);
        store.reset_device_settings(SCOPE, "").unwrap();
        assert!(load(&store).is_empty());
    }
}
