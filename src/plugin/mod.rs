//! Plugins (base): folders holding a `plugin.json` [`Manifest`], found through [`crate::paths::search_path`]
//! (`$OPENFADER_PLUGINS`, then `~/.config/openfader/extension/plugin/`, then the bundled `extension/plugin/`). A
//! search-path entry is either one plugin folder or a folder of them. The first plugin of a name wins; later ones are
//! reported in [`Registry::errors`].
//!
//! A plugin brings any mix of:
//! - sources, actions and event hooks, served by its `script` or `command`: a `*.lua` script runs inside the app ([`lua`]),
//!   anything else is a child process started per request, one JSON object on stdin, one on stdout ([`Plugin::call`];
//!   protocol in `docs/plugins.md`). Children: [`source`] (track lists, search and download as a
//!   [`crate::library::Library`] decorator), [`action`] (menu commands and the app commands they answer with), [`event`]
//!   (fire-and-forget notifications).
//! - effects, each for one [`effect::Slot`] of the mixer (Beat FX, COLOR FX, mic, send, lever, channel fader), declared in
//!   JSON as chains of host DSP blocks or brought by a native library: [`effect`].
//! - declarative controls, no code: COLOR FX knob behaviours ([`KnobMode`]) and performance-pad modes ([`PadModeDecl`]).
//! - track libraries in a native library: a file or folder format the app reads, and writes as the collection export
//!   ([`library`]); a library that declares `device` also serves removable devices ([`device`]).
//! - settings the user sets in the plugin manager ([`SettingDecl`]); their values come with every request ([`config`]).
//!
//! [`registry`] reads everything on first use, which the app makes happen before the audio engine starts (effects are
//! part of every FX unit from then on). [`reload`] reads the folders again (after [`install`], or a plugin switched on or
//! off) into [`current`]: actions, events and settings follow at once; effects, libraries, sources, knob and pad modes stay
//! those of the first read until a restart ([`Registry::restart_needed`]).

pub mod action;
pub mod config;
pub mod device;
pub mod effect;
pub mod event;
pub mod install;
pub mod library;
mod lua;
pub mod source;

use parking_lot::RwLock;
use serde::Deserialize;
use serde_json::Value;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

/// `plugin.json`.
#[derive(Deserialize, Clone, Debug, Default)]
pub struct Manifest {
    /// Unique, shown in menus and PREFS; no `:` (it prefixes the ids the plugin hands out).
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub author: String,
    /// Web page of the plugin, shown in the plugin manager.
    #[serde(default)]
    pub homepage: String,
    /// The software the plugin makes the app compatible with (its name, e.g. a DJ program's). The welcome window
    /// offers to switch on every plugin found with one; empty: not suggested.
    #[serde(default)]
    pub suggest: String,
    /// Script serving sources, actions and events, relative to the plugin folder: `*.lua` runs inside the app ([`lua`]),
    /// `*.py` with `python3`, anything else as a program. Shorthand for `command`, which wins when both are given.
    #[serde(default)]
    pub script: Option<String>,
    /// Program and arguments of the process serving sources, actions and events; a relative program or
    /// argument that names an existing file in the plugin folder is made absolute. Empty: no process.
    #[serde(default)]
    pub command: Vec<String>,
    #[serde(default)]
    pub sources: Vec<SourceDecl>,
    #[serde(default)]
    pub actions: Vec<ActionDecl>,
    /// Event names the process is told about (see [`event`]).
    #[serde(default)]
    pub events: Vec<String>,
    /// Seconds between two `timer` events (when `events` lists `timer`).
    #[serde(default)]
    pub timer: f64,
    /// Settings shown in the plugin manager.
    #[serde(default)]
    pub settings: Vec<SettingDecl>,
    /// Effects: the path of a native library (relative to the plugin folder), or a list of effects declared in JSON
    /// ([`effect::chain`]).
    #[serde(default)]
    pub effects: Option<effect::Decls>,
    /// A track library in a native library ([`library`]).
    #[serde(default)]
    pub library: Option<library::LibraryDecl>,
    #[serde(default)]
    pub knob_modes: Vec<KnobMode>,
    #[serde(default)]
    pub pad_modes: Vec<PadModeDecl>,
}

/// A COLOR FX knob behaviour, offered beside NORMAL by PREFS > CONTROLLER > Mixer > COLOR FX KNOB. `stop` (0 to 1 of the
/// knob's travel, 0.5 = centre) is a position one drag cannot pass: a turn that started on one side halts there, the next
/// drag may leave it either way (a turn starting on it is free).
#[derive(Deserialize, Clone, Debug)]
pub struct KnobMode {
    pub name: String,
    pub stop: f32,
}

/// A performance-pad mode, a mode button after the built-in ones. Its pads show the deck's hot cues (as HOT CUE does);
/// hitting pad n runs `press`, letting it go runs `release`, each step on the pad's deck in order.
#[derive(Deserialize, Clone, Debug)]
pub struct PadModeDecl {
    pub name: String,
    #[serde(default)]
    pub press: Vec<PadStep>,
    #[serde(default)]
    pub release: Vec<PadStep>,
}

/// One step of a [`PadModeDecl`]; in JSON a string (`"jump"`) or a one-key object (`{"beat_jump": -4}`).
#[derive(Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum PadStep {
    /// To the pad's hot cue, playing or paused as before (nothing on a pad without one).
    Jump,
    Play,
    Pause,
    /// Beats, negative = back.
    BeatJump(f64),
    /// A loop of that many beats from the playhead.
    Loop(f64),
    LoopOff,
}

/// A track source the plugin serves: a folder in the browser tree.
#[derive(Deserialize, Clone, Debug)]
pub struct SourceDecl {
    pub id: String,
    pub name: String,
    /// The source answers `search` (the tree gets a search entry fed by the browser's search box).
    #[serde(default)]
    pub search: bool,
}

/// A command the plugin adds to the Plugins menu.
#[derive(Deserialize, Clone, Debug)]
pub struct ActionDecl {
    pub id: String,
    pub label: String,
    /// Prompt of a text field asked for before the action runs (e.g. "URL"); `None`: runs at once.
    #[serde(default)]
    pub input: Option<String>,
}

/// A setting of a plugin, edited in the plugin manager and stored by the app ([`config`]). Every request carries the
/// plugin's values as `"settings": {id: value}` (the default until the user changes it).
#[derive(Deserialize, Clone, Debug)]
pub struct SettingDecl {
    pub id: String,
    pub label: String,
    #[serde(rename = "type", default)]
    pub kind: SettingKind,
    /// The value before the user sets one: `false`, `""`, `0` or the first choice when left out.
    #[serde(default)]
    pub default: Value,
    /// The options of a `choice`.
    #[serde(default)]
    pub choices: Vec<String>,
    /// One line under the setting.
    #[serde(default)]
    pub help: String,
}

#[derive(Deserialize, Clone, Copy, Debug, Default, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum SettingKind {
    Bool,
    #[default]
    Text,
    /// Like `Text`, shown masked in the plugin manager; stored in `plugins.json` as plain text.
    Secret,
    Number,
    Choice,
}

impl SettingDecl {
    /// The default as the value type of the setting.
    pub fn default_value(&self) -> Value {
        match (self.kind, &self.default) {
            (SettingKind::Bool, Value::Bool(_))
            | (SettingKind::Number, Value::Number(_))
            | (SettingKind::Text | SettingKind::Secret | SettingKind::Choice, Value::String(_)) => {
                self.default.clone()
            }
            (SettingKind::Bool, _) => Value::Bool(false),
            (SettingKind::Number, _) => 0.into(),
            (SettingKind::Text | SettingKind::Secret, _) => "".into(),
            (SettingKind::Choice, _) => self.choices.first().cloned().unwrap_or_default().into(),
        }
    }
}

#[derive(Clone)]
pub struct Plugin {
    pub dir: PathBuf,
    pub manifest: Manifest,
}

/// Every plugin found, the effects their libraries add, and what went wrong on the way (bad manifests, duplicate
/// names, libraries that would not load).
#[derive(Default)]
pub struct Registry {
    pub plugins: Vec<Plugin>,
    /// Plugins switched off in the plugin manager ([`config`]): found, not used.
    pub disabled: Vec<Plugin>,
    /// Grouped by slot ([`effect::load`]); [`Registry::slot`] gives one slot's.
    pub effects: Vec<effect::Effect>,
    /// The loaded library plugins ([`library::load`]).
    pub libraries: Vec<library::Loaded>,
    pub errors: Vec<String>,
}

impl Registry {
    pub fn get(&self, name: &str) -> Option<&Plugin> {
        self.plugins.iter().find(|p| p.manifest.name == name)
    }

    /// The effects of `slot`, in selector order (after the slot's built-ins).
    pub fn slot(&self, slot: effect::Slot) -> &[effect::Effect] {
        let at = |s: u32| self.effects.partition_point(|e| (e.slot as u32) < s);
        &self.effects[at(slot as u32)..at(slot as u32 + 1)]
    }

    /// Every plugin's knob modes, in plugin order.
    pub fn knob_modes(&self) -> impl Iterator<Item = &KnobMode> {
        self.plugins.iter().flat_map(|p| &p.manifest.knob_modes)
    }

    /// Every plugin's pad modes, in plugin order.
    pub fn pad_modes(&self) -> impl Iterator<Item = &PadModeDecl> {
        self.plugins.iter().flat_map(|p| &p.manifest.pad_modes)
    }

    /// Names of the plugins whose effects, sources, knob or pad modes differ from what the app started with (installed,
    /// removed, switched on or off, or edited since): those parts apply after a restart.
    pub fn restart_needed(&self) -> Vec<String> {
        fn sig(m: &Manifest) -> Option<String> {
            let parts = (
                &m.effects,
                &m.library,
                &m.sources,
                &m.knob_modes,
                &m.pad_modes,
            );
            let empty = format!(
                "{:?}",
                (
                    &Manifest::default().effects,
                    &Manifest::default().library,
                    &[(); 0],
                    &[(); 0],
                    &[(); 0]
                )
            );
            Some(format!("{parts:?}")).filter(|s| *s != empty)
        }
        let set = |r: &Registry| -> Vec<(String, String)> {
            r.plugins
                .iter()
                .filter_map(|p| Some((p.manifest.name.clone(), sig(&p.manifest)?)))
                .collect()
        };
        let (first, now) = (set(&FIRST), set(self));
        let mut out: Vec<String> = first
            .iter()
            .filter(|f| !now.contains(f))
            .chain(now.iter().filter(|n| !first.contains(n)))
            .map(|(n, _)| n.clone())
            .collect();
        out.sort();
        out.dedup();
        out
    }
}

/// The plugin folders read, the uninstalled plugins left out and the switched-off ones set apart.
fn read_all() -> Registry {
    let mut r = discover(&crate::paths::search_path(
        "OPENFADER_PLUGINS",
        "extension/plugin",
    ));
    let mut cfg = config::get();
    let found: Vec<&str> = r.plugins.iter().map(|p| p.manifest.name.as_str()).collect();
    if cfg.adopt(&found) {
        if let Err(e) = config::update(|c| *c = cfg.clone()) {
            eprintln!("plugin: {e}");
        }
    }
    r.plugins
        .retain(|p| !cfg.removed.contains(&p.manifest.name));
    (r.plugins, r.disabled) = r
        .plugins
        .into_iter()
        .partition(|p| cfg.enabled(&p.manifest.name));
    r
}

/// The registry the app started with: its effects, sources, knob and pad modes are the app's for the rest of the process.
static FIRST: LazyLock<Registry> = LazyLock::new(|| {
    let mut r = read_all();
    r.effects = effect::load(&r.plugins, &mut r.errors);
    r.libraries = library::load(&r.plugins, &mut r.errors);
    if let Err(e) = library::migrate_refs(&r.libraries, &crate::store::global()) {
        eprintln!("plugin: analysis refs: {e}");
    }
    for e in &r.errors {
        eprintln!("plugin: {e}");
    }
    r
});

/// The plugins as of the last [`reload`] (the first read until then).
static NOW: LazyLock<RwLock<Arc<Registry>>> = LazyLock::new(|| {
    let f = &*FIRST;
    RwLock::new(Arc::new(Registry {
        plugins: f.plugins.clone(),
        disabled: f.disabled.clone(),
        effects: f.effects.clone(),
        libraries: f.libraries.clone(),
        errors: f.errors.clone(),
    }))
});

/// The plugins the app started with, effects loaded (on the first call). Effects, sources, knob and pad modes come from here.
pub fn registry() -> &'static Registry {
    &FIRST
}

/// The plugins as of the last [`reload`]: actions, events, settings and the plugin manager use these.
pub fn current() -> Arc<Registry> {
    NOW.read().clone()
}

/// Reads the plugin folders again and makes the result [`current`]. The effects stay those of the first read.
pub fn reload() -> Arc<Registry> {
    let mut r = read_all();
    (r.effects, r.libraries) = (FIRST.effects.clone(), FIRST.libraries.clone());
    let r = Arc::new(r);
    *NOW.write() = r.clone();
    r
}

/// Every plugin in `dirs` (see the module doc), without loading anything.
pub fn discover(dirs: &[PathBuf]) -> Registry {
    let mut r = Registry::default();
    for dir in dirs {
        let candidates: Vec<PathBuf> = if dir.join("plugin.json").is_file() {
            vec![dir.clone()]
        } else {
            let Ok(entries) = std::fs::read_dir(dir) else {
                continue;
            };
            let mut v: Vec<PathBuf> = entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.join("plugin.json").is_file())
                .collect();
            v.sort();
            v
        };
        for dir in candidates {
            match read_manifest(&dir) {
                Ok(m) if r.get(&m.name).is_some() => r.errors.push(format!(
                    "{}: a plugin named {:?} is already loaded",
                    dir.display(),
                    m.name
                )),
                Ok(manifest) => r.plugins.push(Plugin { dir, manifest }),
                Err(e) => r.errors.push(e),
            }
        }
    }
    r
}

fn read_manifest(dir: &Path) -> Result<Manifest, String> {
    let file = dir.join("plugin.json");
    let m: Manifest = crate::paths::at(
        serde_json::from_str(&crate::paths::at(std::fs::read_to_string(&file), &file)?),
        &file,
    )?;
    if m.name.is_empty() || m.name.contains(':') {
        return Err(format!(
            "{}: `name` must be non-empty and hold no `:`",
            file.display()
        ));
    }
    if let Some(e) = &m.effects {
        effect::check(e).map_err(|e| format!("{}: {e}", file.display()))?;
    }
    if let Some(l) = &m.library {
        library::check(l, &m.settings).map_err(|e| format!("{}: {e}", file.display()))?;
    }
    if let Some(s) = m
        .sources
        .iter()
        .find(|s| s.id.is_empty() || s.id.contains(':'))
    {
        return Err(format!(
            "{}: source id {:?} must be non-empty and hold no `:`",
            file.display(),
            s.id
        ));
    }
    if m.command.is_empty()
        && m.script.is_none()
        && !(m.sources.is_empty() && m.actions.is_empty() && m.events.is_empty())
    {
        return Err(format!(
            "{}: sources, actions and events need a `script` or a `command`",
            file.display()
        ));
    }
    if let Some(s) = m.script.as_ref().filter(|s| !dir.join(s).is_file()) {
        return Err(format!(
            "{}: script {s:?} is not in the plugin folder",
            file.display()
        ));
    }
    for (i, s) in m.settings.iter().enumerate() {
        if s.id.is_empty() || m.settings[..i].iter().any(|o| o.id == s.id) {
            return Err(format!(
                "{}: setting ids must be non-empty and unique ({:?})",
                file.display(),
                s.id
            ));
        }
        if s.kind == SettingKind::Choice && s.choices.is_empty() {
            return Err(format!(
                "{}: setting {:?} is a `choice` without `choices`",
                file.display(),
                s.id
            ));
        }
    }
    let label = |n: &str| !n.is_empty() && n.len() <= 16;
    if let Some(k) = m
        .knob_modes
        .iter()
        .find(|k| !label(&k.name) || !(0.0..=1.0).contains(&k.stop))
    {
        return Err(format!(
            "{}: knob mode {:?} needs a name of 1 to 16 characters and a `stop` from 0 to 1",
            file.display(),
            k.name
        ));
    }
    if let Some(p) = m
        .pad_modes
        .iter()
        .find(|p| !label(&p.name) || p.press.is_empty() && p.release.is_empty())
    {
        return Err(format!(
            "{}: pad mode {:?} needs a name of 1 to 16 characters and a `press` or `release` step",
            file.display(),
            p.name
        ));
    }
    Ok(m)
}

impl Plugin {
    /// Sends `request` (`{"op": …}`, given the plugin's `settings`, see [`config`]) to the plugin and returns its reply: a
    /// Lua `script` runs here ([`lua`]), else a fresh run of its process (`command`, or the `script` with `python3` /
    /// as a program). The process gets the plugin folder as working directory; whatever it prints to stderr is passed on to
    /// ours. A reply with an `"error"` string, a non-zero exit, output that is not one JSON object, or no reply within
    /// `timeout` (the process is killed, the script stopped) is an `Err`.
    pub fn call(&self, request: &Value, timeout: Duration) -> Result<Value, String> {
        let name = &self.manifest.name;
        let mut request = request.clone();
        if let Some(o) = request.as_object_mut() {
            o.insert("settings".into(), config::get().values(&self.manifest));
        }
        let command: Vec<String> = match (&self.manifest.command[..], &self.manifest.script) {
            ([], Some(s)) if s.ends_with(".lua") => return lua::call(self, s, &request, timeout),
            ([], Some(s)) if s.ends_with(".py") => vec![python().into(), s.clone()],
            ([], Some(s)) => vec![s.clone()],
            (c, _) => c.to_vec(),
        };
        let (program, args) = command
            .split_first()
            .ok_or_else(|| format!("{name}: no command"))?;
        let mut child = Command::new(self.resolve(program))
            .args(args.iter().map(|a| self.resolve(a)))
            .current_dir(&self.dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| format!("{name}: cannot start {program:?}: {e}"))?;
        let body = request.to_string();
        let mut stdin = child.stdin.take().expect("piped");
        // A writer thread: a reply larger than the pipe buffer must not deadlock against our write.
        let writer = std::thread::spawn(move || stdin.write_all(body.as_bytes()));
        let mut stdout = child.stdout.take().expect("piped");
        let reader = std::thread::spawn(move || {
            let mut s = String::new();
            stdout.read_to_string(&mut s).map(|_| s)
        });
        let deadline = Instant::now() + timeout;
        let status = loop {
            if let Some(s) = child.try_wait().map_err(|e| format!("{name}: {e}"))? {
                break s;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{name}: no reply within {} s", timeout.as_secs()));
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        let _ = writer.join(); // a process that exits without reading its request is allowed to
        let out = reader
            .join()
            .map_err(|_| format!("{name}: reader panicked"))?
            .map_err(|e| format!("{name}: {e}"))?;
        let reply: Value = serde_json::from_str(&out).map_err(|e| {
            if status.success() {
                format!("{name}: reply is not JSON ({e})")
            } else {
                format!("{name}: exited with {status}")
            }
        })?;
        if let Some(e) = reply.get("error").and_then(Value::as_str) {
            return Err(format!("{name}: {e}"));
        }
        if !status.success() {
            return Err(format!("{name}: exited with {status}"));
        }
        Ok(reply)
    }

    /// `arg` as a path in the plugin folder if such a file exists there, else unchanged (`python3`, `--flag`).
    fn resolve(&self, arg: &str) -> std::ffi::OsString {
        let p = self.dir.join(arg);
        if Path::new(arg).is_relative() && p.exists() {
            p.into_os_string()
        } else {
            arg.into()
        }
    }
}

/// The Python interpreter for `*.py` scripts: `python` on Windows, where `python3` is often missing.
fn python() -> &'static str {
    if cfg!(windows) {
        "python"
    } else {
        "python3"
    }
}

#[cfg(test)]
mod tests;
