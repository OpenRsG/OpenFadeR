//! The welcome window ↔ ui/welcome.slint (`WelcomeState`): at the first start (prefs key `welcomed`) and from Help > Welcome.
//! It offers what is off of the suggested set: every plugin found whose manifest names software it makes the app
//! compatible with (`suggest`, [`openfader::plugin::Manifest::suggest`]), and STEMS. ACCEPT switches on what the window shows
//! (plugins with effects or libraries load at the next start: RESTART NOW), SKIP leaves everything as it is.
//!
//! [`offer`] opens the same window with only the extensions that read what a plugged-in device holds (found by
//! [`plugins::suggest`]).
use super::{plugins, prefs, App, SuggestView, WelcomeState};
use openfader::plugin::{config, current, Manifest, Plugin};
use openfader::stems::model;
use slint::{ComponentHandle, Model, ModelRc, VecModel};

const SEEN: &str = "welcomed";
const STEMS: &str = "STEMS";

/// The suggested plugins that are off.
fn offered() -> Vec<Plugin> {
    let r = current();
    r.disabled
        .iter()
        .filter(|p| !p.manifest.suggest.is_empty())
        .cloned()
        .collect()
}

/// What a plugin brings, for its badge.
fn kind(m: &Manifest) -> &'static str {
    match &m.library {
        Some(l) if l.device => tr!("LIBRARY + DEVICES"),
        Some(_) => tr!("LIBRARY"),
        None if m.controller.is_some() => tr!("CONTROLLER"),
        None if m.effects.is_some() => tr!("EFFECTS"),
        None if !m.pad_modes.is_empty() => tr!("PAD MODE"),
        None if !m.knob_modes.is_empty() => tr!("KNOB MODE"),
        None => tr!("PLUGIN"),
    }
}

fn view(p: &Plugin) -> SuggestView {
    SuggestView {
        name: p.manifest.name.as_str().into(),
        kind: kind(&p.manifest).into(),
        description: p.tr(&p.manifest.description).into(),
    }
}

/// Shows the window with `views` under `intro`.
fn open(app: &App, views: Vec<SuggestView>, intro: &str) {
    let s = app.global::<WelcomeState>();
    s.set_intro(intro.into());
    s.set_plugins(ModelRc::new(VecModel::from(views)));
    s.set_done("".into());
    s.set_restart(false);
    s.set_visible(true);
}

/// Whether the window is up (the first start, Help > Welcome, or an [`offer`]).
pub fn visible(app: &App) -> bool {
    app.global::<WelcomeState>().get_visible()
}

/// Opens the window with just `plugins`, the extensions that read what the device `device` holds; ACCEPT switches them on.
pub fn offer(app: &App, device: &str, plugins: &[Plugin]) {
    let intro = tr_n!(
        "{} holds a library written by other DJ software. This extension reads it; it stays off until you switch it on.",
        "{} holds libraries written by other DJ software. These extensions read them; they stay off until you switch them on.",
        plugins.len(),
        device
    );
    open(app, plugins.iter().map(view).collect(), &intro);
}

/// Fills and shows the window.
pub fn show(app: &App) {
    let off = offered();
    let mut targets: Vec<&str> = off.iter().map(|p| p.manifest.suggest.as_str()).collect();
    targets.sort_unstable();
    targets.dedup();
    let mut views: Vec<SuggestView> = off.iter().map(view).collect();
    if !prefs::with(|p| p.stems) {
        views.push(SuggestView {
            name: STEMS.into(),
            kind: tr!("FEATURE").into(),
            description: tr!(
                "Splits a track into vocals, instruments and drums. Switching it on downloads a {} MB model once.",
                model::SIZE / 1_000_000
            )
            .into(),
        });
    }
    let intro = if targets.is_empty() {
        tr!("Switch these on to get the most out of OpenFadeR.").to_owned()
    } else {
        tr!(
            "These make OpenFadeR fully compatible with {}: its libraries, devices, effects and pad modes.",
            targets.join(&tr!(" and "))
        )
    };
    open(app, views, &tr!("{} They stay off until you switch them on.", intro));
}

/// Remembers that the window was seen, so it does not open at the next start.
fn seen() {
    if !prefs::with(|p| p.extra_bool(SEEN, false)) {
        prefs::update(|p| p.set_extra(SEEN, true));
    }
}

fn close(app: &App) {
    app.global::<WelcomeState>().set_visible(false);
    seen();
}

fn accept(app: &App) {
    let s = app.global::<WelcomeState>();
    let shown: Vec<String> = s.get_plugins().iter().map(|v| v.name.to_string()).collect();
    let stems = shown.iter().any(|n| n == STEMS);
    let names: Vec<String> = shown.into_iter().filter(|n| n != STEMS).collect();
    let mut done = Vec::new();
    if !names.is_empty() {
        match config::update(|c| names.iter().for_each(|n| c.set_enabled(n, true))) {
            Ok(()) => {
                plugins::installed(app, &names);
                done.push(tr!("{} extensions switched on", names.len()));
            }
            Err(e) => done.push(e),
        }
    }
    if stems {
        prefs::update(|p| p.stems = true); // starts the model download (`stems_model`)
        done.push(tr!("STEMS on: its model is downloading").into());
    }
    let needed = current().restart_needed();
    let restart = names.iter().any(|n| needed.contains(n));
    if restart {
        done.push(tr!("restart OpenFadeR to load their effects and libraries").into());
    }
    s.set_done(tr!("{}.", done.join("; ")).into());
    s.set_restart(restart);
    seen();
}

pub fn wire(app: &App) {
    let s = app.global::<WelcomeState>();
    let w = app.as_weak();
    s.on_show(move || {
        if let Some(app) = w.upgrade() {
            show(&app);
        }
    });
    let w = app.as_weak();
    s.on_accept(move || {
        if let Some(app) = w.upgrade() {
            accept(&app);
        }
    });
    let w = app.as_weak();
    s.on_skip(move || {
        if let Some(app) = w.upgrade() {
            close(&app);
        }
    });
    s.on_restart_now(super::restart);
    if !prefs::with(|p| p.extra_bool(SEEN, false)) {
        show(app);
    }
}
