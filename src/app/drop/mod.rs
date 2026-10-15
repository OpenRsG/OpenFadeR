//! Files dragged onto the window from the desktop ↔ `FileDropState` (ui/dnd.slint: the "drop to install" overlay while a
//! drag is over the window). Each dropped path goes where it belongs ([`dropped`]): a plugin (folder, `.zip` or
//! `plugin.json`) is installed (`app/plugins`), a theme (`.json`) is installed and listed (`app/skin`), anything else
//! (music files and folders) is imported into the Collection.
//!
//! Children: [`x11`] (winit's own file drops: X11, Windows, macOS) and [`wayland`] (winit 0.30 has none under Wayland:
//! a data device of our own on winit's connection).

#[cfg(target_os = "linux")]
mod wayland;
mod x11;

use super::{App, FileDropState, library, plugins, skin, toast};
use openfader::plugin::install::is_plugin;
use slint::ComponentHandle;
use std::path::{Path, PathBuf};

/// Hooks both drop paths to the window.
pub fn wire(app: &App) {
    x11::wire(app);
    #[cfg(target_os = "linux")]
    wayland::wire(app);
}

/// A drag with files is over the window (`true`) or left it.
fn hovering(app: &App, on: bool) {
    app.global::<FileDropState>().set_hovering(on);
}

/// What a dropped `.json` is: a plugin's `plugin.json`, else a theme.
fn is_theme(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("json"))
        && !is_plugin(path)
}

/// Installs or imports every dropped path and says what happened.
pub fn dropped(app: &App, paths: Vec<PathBuf>) {
    hovering(app, false);
    let mut said: Vec<String> = Vec::new();
    let mut music = Vec::new();
    for p in paths {
        if is_plugin(&p) {
            said.push(plugins::install(app, &p).unwrap_or_else(|e| e));
        } else if is_theme(&p) {
            said.push(match skin::install_and_choose(&p) {
                Ok(name) => format!("theme {name} installed and applied"),
                Err(e) => e,
            });
        } else {
            music.push(p);
        }
    }
    if !music.is_empty() {
        said.push(format!(
            "importing {} item{}…",
            music.len(),
            super::plural(music.len())
        ));
        library::import_files(music);
    }
    if !said.is_empty() {
        toast::show(app, &said.join(" · "));
    }
}
