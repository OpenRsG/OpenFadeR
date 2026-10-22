//! Plugin sources in the browser: the search entry of a source ([`openfader::plugin::source`]). Enter in the search box
//! asks the library to search online ([`Library::search`]); the results arrive through the library's notify callback
//! (`BrowserState.reload`) and are listed without the box's text filtering them.

use super::{App, BrowserState, Home, View};
use openfader::plugin::source::is_search;
use slint::ComponentHandle;

impl View {
    /// The id of the plugin search entry on screen.
    pub(super) fn search_entry(&self) -> Option<&str> {
        match &self.home {
            Home::Playlist(Some(id)) if self.related.is_none() && is_search(id) => Some(id),
            _ => None,
        }
    }

    /// Enter in the search box: runs `text` as a search in the plugin search entry on screen, if that is what is on screen.
    pub(super) fn search_online(&mut self, app: &App, text: &str) {
        let Some(id) = self.search_entry().map(str::to_owned) else {
            return;
        };
        let state = app.global::<BrowserState>();
        if text.trim().is_empty() {
            return state.set_status(tr!("type what to search for, then press Enter").into());
        }
        if self.lib.search(&id, text) {
            self.load(app); // the old results go while the plugin works
            state.set_status(tr!("searching “{}”…", text.trim()).into());
        }
    }
}
