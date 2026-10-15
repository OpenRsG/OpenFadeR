//! Plugins in the window ↔ ui/plugins.slint (global `PluginState`): the Plugins menu (one entry per plugin action), the
//! prompt of an action that asks for text, the commands actions and event replies answer with, the `timer` and
//! `app_started` events, and the [`notify`] callback of the plugin track sources ([`openfader::plugin::source`]), which
//! makes the browser re-read the library. Child: [`manager`] (PREFS > EXTENSIONS: install, switch on / off, settings,
//! remove, restart).
//!
//! An action runs on a worker thread ([`openfader::plugin::action::run`]); its commands run here, on the UI thread,
//! when it is done. Nothing in this section waits for a plugin. The menu follows [`openfader::plugin::current`]
//! ([`refresh`] after a reload).

mod manager;
pub use manager::install;

use super::{
    App, BrowserState, PluginState, browser, deck::Decks, deck::active, keys, library, toast,
};
use openfader::{
    library::{Library, folder::read_track},
    plugin::{
        ActionDecl, Plugin, action,
        action::Command,
        config, current, event,
        source::{Notify, track_json},
    },
};
use serde_json::{Value, json};
use slint::{ComponentHandle, ModelRc, SharedString, Timer, TimerMode, VecModel, Weak};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};

struct Ctx {
    app: Weak<App>,
    lib: Rc<dyn Library>,
    decks: Decks,
    /// The menu entries: every action of every plugin.
    actions: RefCell<Vec<(Plugin, ActionDecl)>>,
    /// The action whose prompt is open.
    asking: Cell<Option<usize>>,
    /// Last `timer` event per plugin (seconds since start-up).
    timers: RefCell<Vec<(String, f64)>>,
    started: Instant,
    _timer: Timer,
}

thread_local! {
    /// The context, for work coming back from a thread.
    static CTX: RefCell<Option<Rc<Ctx>>> = const { RefCell::new(None) };
}

/// Message in the status line and as a toast.
fn say(app: &App, text: &str) {
    app.global::<BrowserState>().set_status(text.into());
    toast::show(app, text);
}

/// What the plugin track sources call (from a worker thread) when an answer is cached or a request failed: the browser
/// reads the library again, or the error is shown.
pub fn notify(app: &App) -> Notify {
    let weak = app.as_weak();
    Arc::new(move |r| {
        let _ = weak.upgrade_in_event_loop(move |app| match r {
            Ok(()) => app.global::<BrowserState>().invoke_reload(-1),
            Err(e) => say(&app, &e),
        });
    })
}

/// What a plugin knows when its action runs: the decks in use with their tracks, the chosen track, and `dir`, a folder of
/// the plugin's own to write to.
fn context(c: &Ctx, plugin: &Plugin) -> Value {
    let dir = openfader::paths::data_dir().map(|d| d.join("plugins").join(&plugin.manifest.name));
    json!({
        "decks": c.decks.iter().take(active()).enumerate().map(|(i, d)| json!({
            "deck": i + 1,
            "playing": d.playing(),
            "track": d.track().map(|(_, t)| track_json(&t)),
        })).collect::<Vec<_>>(),
        "selected": browser::context().track.as_ref().map(track_json),
        "dir": dir.map(|d| d.to_string_lossy().into_owned()),
    })
}

impl Ctx {
    fn app(&self) -> App {
        self.app.unwrap()
    }

    /// Runs action `i` of the menu on a worker thread; the commands come back through [`Ctx::finished`].
    fn run(&self, i: usize, input: Option<String>) {
        let Some((plugin, decl)) = self.actions.borrow().get(i).cloned() else {
            return;
        };
        say(&self.app(), &format!("{}…", decl.label));
        let context = context(self, &plugin);
        std::thread::spawn(move || {
            let result = action::run(&plugin, &decl.id, input.as_deref(), context);
            let name = plugin.manifest.name;
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(c) = CTX.with(|c| c.borrow().clone()) {
                    c.finished(&name, result);
                }
            });
        });
    }

    fn finished(&self, plugin: &str, result: Result<Vec<Result<Command, String>>, String>) {
        let app = self.app();
        match result {
            Err(e) => say(&app, &e),
            Ok(commands) => {
                for c in commands {
                    match c {
                        Ok(c) => self.apply(&app, plugin, c),
                        Err(e) => say(&app, &e),
                    }
                }
            }
        }
    }

    fn apply(&self, app: &App, plugin: &str, command: Command) {
        let deck = |n: usize| {
            let d = self.decks.iter().take(active()).nth(n - 1);
            if d.is_none() {
                say(app, &format!("{plugin}: no deck {n}"));
            }
            d
        };
        match command {
            Command::Toast(text) => say(app, &text),
            Command::Load { deck: n, file } => match deck(n) {
                None => {}
                Some(_) if !file.is_file() => {
                    say(app, &format!("{}: no such file", file.display()))
                }
                Some(d) => d.load(&read_track(&file), &*self.lib),
            },
            Command::Import(files) => library::import_files(files),
            Command::Key(id) => {
                if !keys::run(app, &id) {
                    say(app, &format!("no shortcut {id}"));
                }
            }
            Command::Play { deck: n, on } => {
                if let Some(d) = deck(n).filter(|d| d.playing() != on) {
                    d.play_pause();
                }
            }
            Command::Seek { deck: n, ms } => {
                if let Some(d) = deck(n) {
                    d.seek_ms(ms);
                }
            }
            Command::Tempo { deck: n, percent } => {
                if let Some(d) = deck(n) {
                    d.set_rate(1.0 + percent / 100.0);
                }
            }
            Command::Open(url) => {
                if let Err(e) = library::open_external(&url) {
                    say(app, &format!("{plugin}: {e}"));
                }
            }
            Command::Setting { id, value } => {
                if let Err(e) = config::update(|c| c.set(plugin, &id, value)) {
                    say(app, &format!("{plugin}: {e}"));
                }
                manager::refresh(app);
            }
        }
    }

    /// The `timer` events that are due.
    fn tick(&self) {
        let now = self.started.elapsed().as_secs_f64();
        for p in event::due_timers(&current(), &mut self.timers.borrow_mut(), now) {
            event::fire_one(&p, "timer", &json!({}));
        }
    }
}

/// Fills the Plugins menu from [`current`] (at start-up and after a reload).
pub fn refresh_menu(app: &App) {
    let Some(c) = CTX.with(|c| c.borrow().clone()) else {
        return;
    };
    let actions: Vec<(Plugin, ActionDecl)> = current()
        .plugins
        .iter()
        .flat_map(|p| {
            p.manifest
                .actions
                .iter()
                .map(move |a| (p.clone(), a.clone()))
        })
        .collect();
    app.global::<PluginState>()
        .set_actions(ModelRc::new(VecModel::from(
            actions
                .iter()
                .map(|(p, a)| SharedString::from(format!("{}: {}", p.manifest.name, a.label)))
                .collect::<Vec<_>>(),
        )));
    *c.actions.borrow_mut() = actions;
}

/// Fills the Plugins menu and the plugin manager, connects the menu, the prompt and the manager, routes the commands of
/// event replies here and starts the `timer` events (once, UI thread).
pub fn wire(app: &App, lib: Rc<dyn Library>, decks: Decks) {
    let state = app.global::<PluginState>();
    let c = Rc::new(Ctx {
        app: app.as_weak(),
        lib,
        decks,
        actions: RefCell::default(),
        asking: Cell::new(None),
        timers: RefCell::default(),
        started: Instant::now(),
        _timer: Timer::default(),
    });
    CTX.with(|x| *x.borrow_mut() = Some(c.clone()));
    refresh_menu(app);
    let k = c.clone();
    state.on_run(move |i| {
        let Some((_, decl)) = usize::try_from(i)
            .ok()
            .and_then(|i| k.actions.borrow().get(i).cloned())
        else {
            return;
        };
        match &decl.input {
            None => k.run(i as usize, None),
            Some(label) => {
                let app = k.app();
                let s = app.global::<PluginState>();
                s.set_prompt_title(decl.label.as_str().into());
                s.set_prompt_label(label.as_str().into());
                s.set_prompt_text("".into());
                s.set_prompt_open(true);
                k.asking.set(Some(i as usize));
            }
        }
    });
    let k = c.clone();
    state.on_prompt_done(move |ok| {
        let app = k.app();
        let s = app.global::<PluginState>();
        s.set_prompt_open(false);
        let (Some(i), true) = (k.asking.take(), ok) else {
            return;
        };
        let text = s.get_prompt_text().trim().to_owned();
        if text.is_empty() {
            say(&k.app(), "nothing was typed");
        } else {
            k.run(i, Some(text));
        }
    });
    event::set_sink(Box::new(|plugin, commands| {
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(c) = CTX.with(|c| c.borrow().clone()) {
                c.finished(&plugin, Ok(commands));
            }
        });
    }));
    let weak = Rc::downgrade(&c);
    c._timer
        .start(TimerMode::Repeated, Duration::from_secs(1), move || {
            if let Some(c) = weak.upgrade() {
                c.tick();
            }
        });
    manager::wire(app);
    event::fire("app_started", json!({}));
}
