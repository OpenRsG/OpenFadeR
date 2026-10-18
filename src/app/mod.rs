//! Slint application (base): builds the window and connects the children: [`prefs`] (settings and
//! their dialog, wired first), [`browser`] (library, with [`trackinfo`]) stores the selected track,
//! [`deck`] (two or four decks, the `decks` setting: playback + waveforms, drawn by [`render`]) loads it on LOAD, [`fx`] runs the
//! beat effects on the decks' bus, [`mixer`] sets the decks' gains, [`analyze`] fills in missing
//! analyses in the background, [`export`] writes the library (with the decks' session edits) as XML,
//! [`midi`] maps controllers, and [`keys`] maps keyboard shortcuts onto the callbacks; [`camelot`]
//! holds the key-wheel maths browser and decks share; [`sampler`] is the 16-pad sampler strip;
//! [`groove`] is the BEAT LAB panel (DRUM SWAP slots, DRUM CAPTURE, DRUM RELEASE FX over the decks' stems);
//! [`automix`] mixes the list over the decks; the global bar's mode selector (ui/app.slint) switches EXPORT ([`exportmode`]: the player
//! panels over decks A and B, layouts 1 PLAYER … FULL BROWSER) / PERFORMANCE (the layouts above: 2Deck / 4Deck Horizontal or Vertical,
//! Browse; the choice lives in the VIEW settings) / EDIT ([`edit`]) / LIGHTING ([`lighting`], DMX lights following the master deck) /
//! VIDEO ([`video`]) full-window views; [`split`] opens the browser in a window of its own (split screen, status bar / View menu);
//! [`skin`] restyles the `Theme` global of every window from skin files (VIEW > Color > Skin, applied by [`prefs`]);
//! [`plugins`] is the Plugins menu and what plugin actions answer with (the plugin track sources join the library in [`run`]);
//! [`about`] is Help > About; [`welcome`] the first-start window (also Help > Welcome); [`stems_model`] the STEMS model download.

/// Wires one callback of a Slint global: `on!(state, owner, on_x, |o, args..| body)` moves a clone of `owner` into the
/// closure and binds it as `o`. The first word of the owner picks a variant:
/// * `weak owner`: holds an `Rc` weakly and skips the callback once the owner is gone;
/// * `at decks`: the callback's first argument is a deck index (taken modulo the number of decks) and `d` is that deck;
/// * `deck owner`: the first argument is a deck index too; `|o, d, args..|` binds the owner and the index, and an index
///   outside the decks skips the callback.
macro_rules! on {
    ($st:expr, weak $me:expr, $cb:ident, |$s:ident $(, $a:ident)*| $body:expr) => {{
        let weak = ::std::rc::Rc::downgrade(&$me);
        $st.$cb(move |$($a),*| {
            if let Some($s) = weak.upgrade() {
                $body
            }
        });
    }};
    ($st:expr, at $decks:expr, $cb:ident, |$d:ident $(, $a:ident)*| $body:expr) => {{
        let decks = $decks.clone();
        $st.$cb(move |i $(, $a)*| {
            let $d = &decks[i as usize % decks.len()];
            $body
        });
    }};
    ($st:expr, deck $me:expr, $cb:ident, |$s:ident, $d:ident $(, $a:ident)*| $body:expr) => {{
        let me = $me.clone();
        $st.$cb(move |i $(, $a)*| {
            let $s = &me;
            if let Some($d) = usize::try_from(i).ok().filter(|&i| i < $crate::app::deck::DECKS) {
                $body
            }
        });
    }};
    ($st:expr, $me:expr, $cb:ident, |$s:ident $(, $a:ident)*| $body:expr) => {{
        let $s = $me.clone();
        $st.$cb(move |$($a),*| $body);
    }};
}

/// Linear gain of a sampler or drum-slot slider position: 0.5 is unity, the audio side allows up to 2.
fn gain(v: f32) -> f32 {
    v * 2.0
}

/// The "s" of "{n} track{}": empty for exactly one.
fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

mod about;
mod analyze;
mod automix;
mod browser;
mod camelot;
mod catalog;
mod connect;
mod deck;
mod device;
mod drop;
mod edit;
mod exportmode;
mod fx;
mod fxrack;
mod groove;
#[cfg(test)]
mod headless;
mod keys;
pub mod library;
mod lighting;
mod midi;
mod mixer;
mod plugins;
mod prefs;
mod render;
mod sampler;
mod skin;
mod split;
mod stems_model;
mod toast;
mod trackinfo;
mod video;
mod welcome;

use openfader::library::Library;
use std::sync::atomic::{AtomicBool, Ordering};
use std::{cell::RefCell, path::PathBuf, rc::Rc};

/// Set by [`restart`]: `main` starts the app again once the window has closed.
static RESTART: AtomicBool = AtomicBool::new(false);

/// Closes the window and has `main` start the app again (plugin manager: RESTART NOW).
pub fn restart() {
    RESTART.store(true, Ordering::Relaxed);
    let _ = slint::quit_event_loop();
}

/// Whether the app asked to be started again ([`restart`]).
pub fn restart_requested() -> bool {
    RESTART.load(Ordering::Relaxed)
}

pub use openfader_ui::*;

/// Opens the Collection. `legacy` (a path given at start, or the old Database setting) and `music` (the own music folder)
/// are connected libraries that sync into it ([`connect`]).
pub fn run(
    legacy: Option<PathBuf>,
    music: Option<PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    let store = openfader::store::global();
    connect::adopt_startup(&store, legacy, music);
    // the store's edits (cues, grid, rating, comment) win over the Collection's rows
    let inner = openfader::store::overlay(Box::new(openfader::library::own::Own::new(store)));
    let app = App::new()?;
    // Wayland / X11 match the window to the menu entry `openfader.desktop` (its icon) by this id; set before the window shows.
    let _ = slint::set_xdg_app_id("openfader");
    about::wire(&app);
    let _prefs = prefs::wire(&app); // before everything that reads the settings
    stems_model::wire(&app);
    let _skin = skin::wire(&app); // PREFS > THEMES and the skin folder watcher; the guard holds the watcher's timer
    catalog::wire(&app); // GET PLUGINS / THEMES / MAPPINGS; before the controller session, which asks it about a controller with no mapping
                         // plugin effects join every FX unit, so they load before the engine makes its units; the count is what the smoke test reads
    let r = openfader::plugin::registry();
    eprintln!(
        "plugins: {} found, {} on",
        r.plugins.len() + r.disabled.len(),
        r.plugins.len()
    );
    prefs::audio::wire(); // hands the AUDIO settings to the engine before the decks open the output
    let lib: Rc<dyn Library> = openfader::plugin::source::wrap(inner, plugins::notify(&app)).into(); // plugin sources join the tree, outside the store's overlay
    app.set_window_title("OpenFadeR".into());
    let selected = Rc::new(RefCell::new(None));
    let decks = deck::wire(&app, lib.clone(), selected.clone());
    fx::wire(&app, &decks);
    fxrack::wire(&app, &decks);
    sampler::wire(&app, &decks, selected.clone());
    groove::wire(&app, &decks, selected.clone());
    let _midi = midi::wire(&app, decks.clone()); // dropping the guard (end of run) turns the controller LEDs off
    mixer::wire(&app, decks.clone());
    let _export = exportmode::wire(&app, decks.clone()); // EXPORT mode: the player panels over decks A and B; the guard holds its timer
    let _video = video::wire(&app, lib.clone(), decks.clone()); // the guard holds the video workers until the window closes
    let _lighting = lighting::wire(&app, decks.clone()); // the timer lives in the guard: lights follow the decks until the window closes
    keys::wire(&app);
    let _device = device::wire(&app, lib.clone(), decks.clone()); // the guard holds the device timer until the window closes
    library::wire(&app, lib.clone(), decks.clone());
    let _split = split::wire(&app); // the split-screen browser window is made on first use
    edit::wire(&app, lib.clone(), decks.clone(), selected.clone());
    plugins::wire(&app, lib.clone(), decks.clone());
    welcome::wire(&app); // after the plugin manager, which ACCEPT refreshes
    drop::wire(&app);
    browser::wire(&app, lib.clone(), decks, move |t| {
        *selected.borrow_mut() = Some(t.clone())
    })?;
    let _analyzer = analyze::wire(&app, lib.clone()); // dropping it cancels the worker: lives until the window closes
    connect::wire(&app); // live sync of the connected libraries; after the browser, which it asks to reload
    app.run()?;
    openfader::plugin::event::fire("app_quit", serde_json::json!({}));
    openfader::plugin::process::stop_all(); // closes their stdin, kills what has not exited within a second
    Ok(())
}
