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
//! [`about`] is Help > About; [`diag`] Help > Report a Problem (with the crash offer at start); [`welcome`] the first-start window (also Help > Welcome); [`stems_model`] the STEMS model download;
//! [`update`] the update check and one-click update (Help > Check for Updates).

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

#[macro_use]
pub mod i18n;
mod about;
mod analyze;
mod artwork;
mod automix;
mod browser;
mod camelot;
mod catalog;
mod connect;
mod deck;
mod device;
mod diag;
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
mod resources;
mod sampler;
mod skin;
mod split;
mod stems_model;
mod studio;
mod toast;
mod trackinfo;
mod update;
mod video;
mod welcome;

/// `openfader verify` (src/app/device/verify.rs): the stick checker without a window.
pub use device::verify;

use openfader::library::Library;
use std::sync::atomic::{AtomicBool, Ordering};
use std::{cell::RefCell, path::PathBuf, rc::Rc};

/// Media identity for loaded-row indicators, independent of titles and cached remote downloads.
fn track_identity(track: &openfader::library::Track) -> String {
    let encoded = if let Some(remote) = &track.remote {
        serde_json::to_string(remote)
    } else if let Some(file) = &track.file {
        serde_json::to_string(file.as_os_str().as_encoded_bytes())
    } else {
        return String::new();
    };
    encoded.expect("media identity contains only strings or bytes")
}

/// Replace changed row data; fixed-slot repeaters keep their controls while the model changes.
fn update_rows<T: Clone + PartialEq + 'static>(
    current: &slint::ModelRc<T>,
    rows: Vec<T>,
) -> Option<slint::ModelRc<T>> {
    use slint::Model;
    if current.row_count() == rows.len()
        && rows
            .iter()
            .enumerate()
            .all(|(i, row)| current.row_data(i).as_ref() == Some(row))
    {
        return None;
    }
    Some(slint::ModelRc::new(slint::VecModel::from(rows)))
}

/// Set by [`restart`]: `main` starts the app again once the window has closed.
static RESTART: AtomicBool = AtomicBool::new(false);
static RESTART_EXE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
static RESTART_SOFTWARE: AtomicBool = AtomicBool::new(false);

/// Closes the window and has `main` start the app again (plugin manager: RESTART NOW).
pub fn restart() {
    RESTART.store(true, Ordering::Relaxed);
    let _ = slint::quit_event_loop();
}

/// [`restart`] into `exe` rather than the running executable: an update moved the install, so what the OS reports as the
/// running executable is its `.prev` copy.
pub fn restart_as(exe: PathBuf) {
    let _ = RESTART_EXE.set(exe);
    restart();
}

/// The executable [`restart_as`] chose.
pub fn restart_exe() -> Option<&'static PathBuf> {
    RESTART_EXE.get()
}

/// Whether the app asked to be started again ([`restart`]).
pub fn restart_requested() -> bool {
    RESTART.load(Ordering::Relaxed)
}

/// Whether this restart needs Slint's software renderer; set only after default-renderer startup failed.
pub fn restart_software_requested() -> bool {
    RESTART_SOFTWARE.load(Ordering::Relaxed)
}

fn startup_error(error: slint::PlatformError) -> Result<(), Box<dyn std::error::Error>> {
    if !render::backend::retry_software(&error) {
        return Err(error.into());
    }
    RESTART_SOFTWARE.store(true, Ordering::Relaxed);
    restart();
    Ok(())
}

/// When the process started (set first thing in `main`), for the start-up line [`log_first_frame`] writes.
static STARTED: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

/// Records the start of the process.
pub fn started() {
    STARTED.get_or_init(std::time::Instant::now);
}

/// Writes `startup: window shown after <ms> ms (<n> tracks)` once. The decks' rendering notifier calls it after each frame
/// (a window has one notifier: a second one would replace theirs and freeze the waveforms); without a notifier (software
/// renderer) a timer calls it at the first turn of the event loop, right after the window is shown. `.github/smoke.sh` reads it.
pub(crate) fn log_first_frame(app: &App) {
    static DONE: AtomicBool = AtomicBool::new(false);
    if DONE.swap(true, Ordering::Relaxed) {
        return;
    }
    render::backend::first_frame();
    let ms = STARTED.get().map_or(0, |t| t.elapsed().as_millis());
    let n = slint::Model::row_count(&app.global::<BrowserState>().get_tracks());
    eprintln!("startup: window shown after {ms} ms ({n} tracks)");
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
    if let Err(error) = render::backend::select() {
        return startup_error(error);
    }
    let app = match App::new() {
        Ok(app) => app,
        Err(error) => return startup_error(error),
    };
    // Wayland / X11 match the window to the menu entry `openfader.desktop` (its icon) by this id; set before the window shows.
    let _ = slint::set_xdg_app_id("openfader");
    about::wire(&app);
    let _prefs = prefs::wire(&app); // before everything that reads the settings
    // the UI language before the texts the views below write
    let lang = prefs::with(|p| p.view.language);
    i18n::apply(prefs::Choice::name(lang));
    let shown = std::cell::Cell::new(lang);
    let w = app.as_weak();
    prefs::on_change(move |p| {
        if p.view.language != shown.replace(p.view.language) {
            i18n::apply(prefs::Choice::name(p.view.language));
            // what Rust wrote from the extensions' and the menus' texts
            if let Some(app) = w.upgrade() {
                plugins::retranslate(&app);
                library::menu_labels(&app);
                skin::refresh();
                app.global::<BrowserState>().invoke_reload(-1);
            }
        }
    });
    stems_model::wire(&app);
    update::wire(&app); // the update check (a setting, read after prefs::wire) and Help > Check for Updates
    let _skin = skin::wire(&app); // PREFS > THEMES and the skin folder watcher; the guard holds the watcher's timer
    let _studio = studio::wire(&app); // the studio workspace's clock; the timer lives until the window closes
    catalog::wire(&app); // GET PLUGINS / THEMES / MAPPINGS; before the controller session, which asks it about a controller with no mapping
                         // plugin effects join every FX unit, so they load before the engine makes its units; the "found" count (folders only) is what the smoke test reads
    let r = openfader::plugin::registry();
    let (on, off) = (
        r.plugins.iter().filter(|p| p.builtin.is_none()).count(),
        r.disabled.len(),
    );
    eprintln!("plugins: {} found, {on} on", on + off);
    eprintln!("plugins: {} built in", openfader::plugin::builtin::plugins().len());
    prefs::audio::wire(); // hands the AUDIO settings to the engine before the decks open the output
    let _diag = diag::wire(&app); // Help > Report a Problem and the crash offer; the guard holds the timer that reads the audio facts
    let _resources = resources::wire(&app);
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
    exportmode::wire(&app, decks.clone());
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
    render::backend::watch(&app);
    if let Err(error) = app.run().and_then(|()| render::backend::check_startup()) {
        openfader::plugin::process::stop_all();
        return startup_error(error);
    }
    update::quit(); // a clean exit shows that the version just installed starts
    openfader::plugin::event::fire("app_quit", serde_json::json!({}));
    openfader::plugin::process::stop_all(); // closes their stdin, kills what has not exited within a second
    Ok(())
}

#[cfg(test)]
mod identity_tests {
    use super::*;
    use openfader::library::{Remote, Track};
    use slint::{ComponentHandle, ModelRc, VecModel};

    #[test]
    fn loaded_rows_use_media_identity_across_metadata_edits_and_all_four_decks() {
        let mut first = Track {
            file: Some("/one/track.wav".into()),
            title: "Same title".into(),
            ..Default::default()
        };
        let other = Track {
            file: Some("/two/track.wav".into()),
            ..first.clone()
        };
        let identity = track_identity(&first);
        assert_ne!(identity, track_identity(&other));
        first.title = "Renamed".into();
        first.artist = "Edited".into();
        assert_eq!(identity, track_identity(&first));
        let mut remote = Track {
            remote: Some(Remote {
                plugin: "reader".into(),
                source: "library".into(),
                id: "one".into(),
            }),
            ..Default::default()
        };
        let remote_id = track_identity(&remote);
        remote.file = Some("/cache/download.wav".into());
        assert_eq!(remote_id, track_identity(&remote));
        remote.remote.as_mut().unwrap().id = "two".into();
        assert_ne!(remote_id, track_identity(&remote));
        headless::init();
        let app = App::new().unwrap();
        let state = app.global::<DeckState>();
        let mut decks = vec![DeckView::default(); 4];
        decks[3] = DeckView {
            title: first.title.into(),
            track_id: identity.clone().into(),
            loaded: true,
            ..Default::default()
        };
        state.set_decks(ModelRc::new(VecModel::from(decks.clone())));
        assert_eq!(state.invoke_loaded_deck(identity.clone().into()), 3);
        assert_eq!(state.invoke_loaded_deck(track_identity(&other).into()), -1);
        assert_eq!(state.invoke_loaded_deck("".into()), -1);
        decks[3].loaded = false;
        decks[3].loading = true;
        state.set_decks(ModelRc::new(VecModel::from(decks.clone())));
        assert_eq!(state.invoke_loaded_deck(identity.clone().into()), 3);
        decks[3].loading = false;
        state.set_decks(ModelRc::new(VecModel::from(decks)));
        assert_eq!(state.invoke_loaded_deck(identity.into()), -1);
    }
}
