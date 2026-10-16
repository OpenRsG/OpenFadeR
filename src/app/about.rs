//! Help > About ↔ ui/about.slint (`AboutState`): the version, and the project links opened in the browser.

use super::{library, AboutState, App};
use slint::ComponentHandle;

pub fn wire(app: &App) {
    let s = app.global::<AboutState>();
    s.set_version(env!("CARGO_PKG_VERSION").into());
    s.on_open(|url| {
        let _ = library::open_external(&url);
    });
}
