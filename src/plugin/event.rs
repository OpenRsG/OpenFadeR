//! Plugin event hooks: the app tells every plugin that lists an event in its `events` (`plugin.json`) when it happens.
//! One `{"op": "event", "event": <name>, "data": {…}}` request per plugin and event, each on its own thread, so a slow
//! plugin delays nothing and two events may overlap. The reply is ignored; a failure goes to stderr.
//!
//! Events (deck numbers start at 1; `track` is a [`super::source::track_json`]):
//! - `app_started` (`{}`), `app_quit` (`{}`, the app waits for nobody);
//! - `track_loaded` (`{deck, track}`: a track is on a deck, downloaded if it came from a plugin source), `track_unloaded`
//!   (`{deck}`), `track_played` (`{track}`: it stayed on air as long as the play history needs);
//! - `deck_play`, `deck_pause` (`{deck, track, position_ms}`), `track_ended` (`{deck, track}`), `master_changed` (`{deck}`;
//!   0 = none);
//! - `recording_started` (`{}`), `recording_stopped` (`{files}`);
//! - `timer` (`{}`), every `timer` seconds of the manifest.

use super::{
    Plugin, Registry,
    action::{self, Command},
    current,
};
use serde_json::{Value, json};
use std::{sync::OnceLock, thread::JoinHandle, time::Duration};

const TIMEOUT: Duration = Duration::from_secs(30);

/// Tells every plugin of the [`current`] registry that lists `event`.
pub fn fire(event: &str, data: Value) {
    fire_in(&current(), event, data);
}

/// [`fire`] for a given registry; the handles end when the plugins have answered.
pub fn fire_in(registry: &Registry, event: &str, data: Value) -> Vec<JoinHandle<()>> {
    registry
        .plugins
        .iter()
        .filter(|p| p.manifest.events.iter().any(|e| e == event))
        .map(|plugin| fire_one(plugin, event, &data))
        .collect()
}

/// Where the commands of event replies go: the app runs them (`app/plugins`). Set once at start-up (runtime input, hence
/// `OnceLock`); without it they are dropped.
static SINK: OnceLock<Sink> = OnceLock::new();

/// Runs `(plugin name, commands)` of an event reply, called on the event's thread.
pub type Sink = Box<dyn Fn(String, Vec<Result<Command, String>>) + Send + Sync>;

/// Makes `sink` receive the commands event replies carry.
pub fn set_sink(sink: Sink) {
    let _ = SINK.set(sink);
}

/// Tells `plugin` about `event`, on a thread of its own; `commands` in its reply go to the [`set_sink`] sink.
pub fn fire_one(plugin: &Plugin, event: &str, data: &Value) -> JoinHandle<()> {
    let plugin = plugin.clone();
    let request = json!({"op": "event", "event": event, "data": data});
    std::thread::spawn(move || {
        let name = plugin.manifest.name.clone();
        match plugin
            .call(&request, TIMEOUT)
            .and_then(|reply| action::commands(&name, &reply))
        {
            Err(e) => eprintln!("plugin event {}: {e}", request["event"]),
            Ok(c) if c.is_empty() => {}
            Ok(c) => deliver(name, c),
        }
    })
}

/// Hands `plugin`'s `commands` to the [`set_sink`] sink (an event reply's, or a persistent plugin's own).
pub(super) fn deliver(plugin: String, commands: Vec<Result<Command, String>>) {
    if let Some(sink) = SINK.get() {
        sink(plugin, commands);
    }
}

/// The plugins whose `timer` event is due at `now`: every plugin listing `timer` with a period, whose last one (`last`,
/// by plugin name; updated here) is at least that long ago. A plugin new to `last` is due at once.
pub fn due_timers(registry: &Registry, last: &mut Vec<(String, f64)>, now: f64) -> Vec<Plugin> {
    let mut due = Vec::new();
    for p in registry
        .plugins
        .iter()
        .filter(|p| p.manifest.timer > 0.0 && p.manifest.events.iter().any(|e| e == "timer"))
    {
        let name = &p.manifest.name;
        match last.iter_mut().find(|(n, _)| n == name) {
            Some((_, t)) if now - *t < p.manifest.timer => {}
            Some((_, t)) => {
                *t = now;
                due.push(p.clone());
            }
            None => {
                last.push((name.clone(), now));
                due.push(p.clone());
            }
        }
    }
    due
}

#[cfg(test)]
mod tests;
