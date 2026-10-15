//! The plugin manager (PREFS > EXTENSIONS, `PluginState.plugins` in ui/plugins.slint): every plugin found, on or off,
//! what it brings and what it runs on, its settings, and buttons to install (file chooser; dropping on the window goes
//! through `app/drop`), remove (installed ones), open its folder, and restart when a change needs one
//! ([`openfader::plugin::Registry::restart_needed`]). Each change is written ([`openfader::plugin::config`]), the
//! folders read again ([`openfader::plugin::reload`]) and the Plugins menu refreshed.

use super::{refresh_menu, say};
use crate::app::{App, PluginSettingView, PluginState, PluginView, library};
use openfader::plugin::{self, Plugin, SettingKind, config, current, install, registry};
use serde_json::Value;
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use std::path::Path;

/// What a plugin runs on, for its badge.
fn kind(p: &Plugin) -> &'static str {
    let m = &p.manifest;
    match (m.command.is_empty(), m.script.as_deref()) {
        (true, Some(s)) if s.ends_with(".lua") => "LUA",
        (true, Some(s)) if s.ends_with(".py") => "PYTHON",
        (true, None) if m.library.is_some() => "NATIVE",
        (true, None) => "JSON",
        _ => "PROGRAM",
    }
}

/// "2 sources · 1 action · 7 beat effects" (the effects the app has loaded for it).
fn provides(p: &Plugin) -> String {
    let m = &p.manifest;
    let name = &m.name;
    let mut parts: Vec<String> = Vec::new();
    let mut count = |n: usize, what: &str| {
        if n > 0 {
            parts.push(format!("{n} {what}{}", crate::app::plural(n)));
        }
    };
    count(usize::from(m.library.is_some()), "track library");
    count(m.sources.len(), "source");
    count(m.actions.len(), "action");
    count(m.events.len(), "event hook");
    count(m.knob_modes.len(), "knob mode");
    count(m.pad_modes.len(), "pad mode");
    count(m.settings.len(), "setting");
    let mut slots: Vec<(String, usize)> = Vec::new();
    for e in registry().effects.iter().filter(|e| &e.plugin == name) {
        let slot = format!("{} effect", e.slot.name().to_lowercase());
        match slots.iter_mut().find(|(s, _)| *s == slot) {
            Some((_, n)) => *n += 1,
            None => slots.push((slot, 1)),
        }
    }
    for (slot, n) in slots {
        count(n, &slot);
    }
    if parts.is_empty() {
        "nothing yet".into()
    } else {
        parts.join(" · ")
    }
}

fn setting_views(p: &Plugin) -> Vec<PluginSettingView> {
    let values = config::get().values(&p.manifest);
    p.manifest
        .settings
        .iter()
        .map(|s| {
            let v = &values[&s.id];
            PluginSettingView {
                label: s.label.as_str().into(),
                help: s.help.as_str().into(),
                kind: match s.kind {
                    SettingKind::Bool => 0,
                    SettingKind::Text => 1,
                    SettingKind::Number => 2,
                    SettingKind::Choice => 3,
                    SettingKind::Secret => 4,
                },
                on: v.as_bool().unwrap_or(false),
                text: match v {
                    Value::String(t) => t.as_str().into(),
                    Value::Number(n) => n.to_string().into(),
                    _ => SharedString::default(),
                },
                choices: ModelRc::new(VecModel::from(
                    s.choices
                        .iter()
                        .map(|c| SharedString::from(c.as_str()))
                        .collect::<Vec<_>>(),
                )),
                choice: s
                    .choices
                    .iter()
                    .position(|c| Some(c.as_str()) == v.as_str())
                    .map_or(0, |i| i as i32),
            }
        })
        .collect()
}

/// The plugins in manager order: by name, on and off together.
fn listed() -> Vec<(Plugin, bool)> {
    let r = current();
    let mut all: Vec<(Plugin, bool)> = r
        .plugins
        .iter()
        .map(|p| (p.clone(), true))
        .chain(r.disabled.iter().map(|p| (p.clone(), false)))
        .collect();
    all.sort_by(|a, b| {
        a.0.manifest
            .name
            .to_lowercase()
            .cmp(&b.0.manifest.name.to_lowercase())
    });
    all
}

/// Fills `PluginState` from [`current`] (keeps the open cards open).
pub(super) fn refresh(app: &App) {
    let s = app.global::<PluginState>();
    let open: Vec<SharedString> = s
        .get_plugins()
        .iter()
        .filter(|v| v.open)
        .map(|v| v.name)
        .collect();
    let restart = current().restart_needed();
    let views: Vec<PluginView> = listed()
        .iter()
        .map(|(p, on)| {
            let m = &p.manifest;
            PluginView {
                name: m.name.as_str().into(),
                version: m.version.as_str().into(),
                author: m.author.as_str().into(),
                description: m.description.as_str().into(),
                homepage: m.homepage.as_str().into(),
                kind: kind(p).into(),
                provides: provides(p).into(),
                folder: p.dir.to_string_lossy().as_ref().into(),
                enabled: *on,
                removable: install::removable(p),
                restart: restart.contains(&m.name),
                open: open.iter().any(|n| n == m.name.as_str()),
                settings: ModelRc::new(VecModel::from(setting_views(p))),
            }
        })
        .collect();
    s.set_plugins(ModelRc::new(VecModel::from(views)));
    let errors: Vec<SharedString> = current().errors.iter().map(|e| e.as_str().into()).collect();
    s.set_errors(ModelRc::new(VecModel::from(errors)));
    s.set_restart_text(if restart.is_empty() {
        SharedString::default()
    } else {
        format!("Restart to finish: {}", restart.join(", ")).into()
    });
    s.set_folder(
        install::user_dir()
            .map(|d| d.to_string_lossy().into_owned())
            .unwrap_or_default()
            .into(),
    );
}

/// Reads the plugin folders again and shows the result everywhere.
fn reloaded(app: &App) {
    plugin::reload();
    refresh(app);
    refresh_menu(app);
}

/// Installs the plugin at `path` (folder, `.zip` or `plugin.json`) and shows it; the message says what happened.
pub fn install(app: &App, path: &Path) -> Result<String, String> {
    let name = install::install(path)?;
    reloaded(app);
    let restart = current().restart_needed().contains(&name);
    Ok(format!(
        "plugin {name} installed{}",
        if restart {
            "; restart to load its effects or sources"
        } else {
            ""
        }
    ))
}

/// The plugin of card `i`.
fn nth(i: i32) -> Option<(Plugin, bool)> {
    listed().into_iter().nth(usize::try_from(i).ok()?)
}

/// Asks the file chooser for a plugin (`folder` or a file: .zip / plugin.json) on a thread and installs it.
fn choose(app: &App, folder: bool) {
    let weak = app.as_weak();
    std::thread::spawn(move || {
        let pick = if folder {
            library::Pick::Folder
        } else {
            library::Pick::File
        };
        let Some(path) = library::browse(pick, "") else {
            return;
        };
        let _ = weak.upgrade_in_event_loop(move |app| {
            let text = install(&app, Path::new(&path)).unwrap_or_else(|e| e);
            app.global::<PluginState>()
                .set_message(text.as_str().into());
            say(&app, &text);
        });
    });
}

fn changed(app: &App, r: Result<(), String>) {
    if let Err(e) = r {
        say(app, &e);
    }
    reloaded(app);
}

/// A setting of card `i` changed to `value`.
fn set(app: &App, i: i32, k: i32, value: impl FnOnce(&plugin::SettingDecl) -> Option<Value>) {
    let Some((p, _)) = nth(i) else { return };
    let Some(s) = usize::try_from(k)
        .ok()
        .and_then(|k| p.manifest.settings.get(k))
    else {
        return;
    };
    match value(s) {
        Some(v) => changed(app, config::update(|c| c.set(&p.manifest.name, &s.id, v))),
        None => say(app, &format!("{}: not a number", s.label)),
    }
}

pub(super) fn wire(app: &App) {
    let s = app.global::<PluginState>();
    let w = app.as_weak();
    s.on_toggle(move |i| {
        let (Some(app), Some((p, on))) = (w.upgrade(), nth(i)) else {
            return;
        };
        changed(
            &app,
            config::update(|c| c.set_enabled(&p.manifest.name, !on)),
        );
    });
    let w = app.as_weak();
    s.on_remove(move |i| {
        let (Some(app), Some((p, _))) = (w.upgrade(), nth(i)) else {
            return;
        };
        let r = install::uninstall(&p);
        let text = match &r {
            Ok(()) => format!("plugin {} removed", p.manifest.name),
            Err(e) => e.clone(),
        };
        changed(&app, r);
        app.global::<PluginState>()
            .set_message(text.as_str().into());
    });
    let w = app.as_weak();
    s.on_expand(move |i| {
        let Some(app) = w.upgrade() else { return };
        let model = app.global::<PluginState>().get_plugins();
        if let Some(mut v) = model.row_data(i as usize) {
            v.open = !v.open;
            model.set_row_data(i as usize, v);
        }
    });
    let w = app.as_weak();
    s.on_setting_bool(move |i, k, on| {
        if let Some(app) = w.upgrade() {
            set(&app, i, k, |_| Some(Value::Bool(on)));
        }
    });
    let w = app.as_weak();
    s.on_setting_text(move |i, k, text| {
        if let Some(app) = w.upgrade() {
            set(&app, i, k, |s| match s.kind {
                SettingKind::Number => text
                    .trim()
                    .parse::<f64>()
                    .ok()
                    .and_then(serde_json::Number::from_f64)
                    .map(Value::Number),
                _ => Some(Value::String(text.to_string())),
            });
        }
    });
    let w = app.as_weak();
    s.on_setting_choice(move |i, k, c| {
        if let Some(app) = w.upgrade() {
            set(&app, i, k, |s| {
                s.choices
                    .get(usize::try_from(c).ok()?)
                    .cloned()
                    .map(Value::String)
            });
        }
    });
    let w = app.as_weak();
    s.on_install(move |folder| {
        if let Some(app) = w.upgrade() {
            choose(&app, folder);
        }
    });
    s.on_open_folder(move |i| {
        let dir = match nth(i) {
            Some((p, _)) => p.dir,
            None => match install::user_dir() {
                Some(d) => d,
                None => return,
            },
        };
        let _ = std::fs::create_dir_all(&dir);
        let _ = library::open_external(&dir.to_string_lossy());
    });
    s.on_homepage(move |i| {
        if let Some((p, _)) = nth(i).filter(|(p, _)| !p.manifest.homepage.is_empty()) {
            let _ = library::open_external(&p.manifest.homepage);
        }
    });
    let w = app.as_weak();
    s.on_reload(move || {
        if let Some(app) = w.upgrade() {
            reloaded(&app);
            say(&app, "plugin folders read again");
        }
    });
    s.on_restart(crate::app::restart);
    refresh(app);
}
