//! Plugin actions: the entries a plugin adds to the Plugins menu. Running one sends `{"op": "action", "action": <id>,
//! "input": <text, when the action declares `input`>, "context": {…}}` and the plugin answers `{"commands": […]}`:
//! what the app should do now. The context is built by the app (the decks and the chosen track, see
//! `docs/plugins.md`). A command that cannot be read is reported and the others still run. An event reply may carry
//! `commands` too ([`super::event`]).
//!
//! Commands, `{"cmd": <name>, …}` (`deck` counts from 1): `toast` (`text`), `load` (`deck`, `file`), `import` (`files`:
//! added to the Collection), `key` (`action`: the id of a shortcut-table row, e.g. `deck.play_pause.A`, run as if its key
//! was hit), `play` / `pause` (`deck`), `seek` (`deck`, `ms`), `tempo` (`deck`, `percent`: the tempo change, e.g. `-2.5`),
//! `open` (`url`: a web page, file or folder in the system's program for it) and `setting` (`id`, `value`: one of the
//! plugin's own settings).

use super::Plugin;
use serde_json::{Value, json};
use std::{path::PathBuf, time::Duration};

/// An action may download and convert audio.
const TIMEOUT: Duration = Duration::from_secs(900);

#[derive(Debug, PartialEq)]
pub enum Command {
    Toast(String),
    /// Deck number from 1.
    Load {
        deck: usize,
        file: PathBuf,
    },
    Import(Vec<PathBuf>),
    /// Id of a shortcut-table row.
    Key(String),
    /// Play (`true`) or pause the deck.
    Play {
        deck: usize,
        on: bool,
    },
    Seek {
        deck: usize,
        ms: f64,
    },
    /// Tempo change in percent.
    Tempo {
        deck: usize,
        percent: f64,
    },
    Open(String),
    Setting {
        id: String,
        value: Value,
    },
}

/// Runs action `action` (with the text the user typed, if it asks for one) and reads its reply. Blocks until the plugin
/// is done: call it from a worker. An `Err` inside the list is a command that could not be read.
pub fn run(
    plugin: &Plugin,
    action: &str,
    input: Option<&str>,
    context: Value,
) -> Result<Vec<Result<Command, String>>, String> {
    let reply = plugin.call(
        &json!({"op": "action", "action": action, "input": input, "context": context}),
        TIMEOUT,
    )?;
    commands(&plugin.manifest.name, &reply)
}

/// The commands of a reply; no `commands` means the plugin has nothing to do.
pub fn commands(plugin: &str, reply: &Value) -> Result<Vec<Result<Command, String>>, String> {
    let Some(list) = reply.get("commands") else {
        return Ok(Vec::new());
    };
    let list = list
        .as_array()
        .ok_or_else(|| format!("{plugin}: `commands` must be a list"))?;
    Ok(list
        .iter()
        .map(|c| command(c).map_err(|e| format!("{plugin}: {e}")))
        .collect())
}

fn command(c: &Value) -> Result<Command, String> {
    let name = c
        .get("cmd")
        .and_then(Value::as_str)
        .ok_or("a command without `cmd`")?;
    let text = |key: &str| {
        c.get(key)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| format!("`{name}` needs a text `{key}`"))
    };
    let deck = || {
        c.get("deck")
            .and_then(Value::as_u64)
            .filter(|&d| d >= 1)
            .map(|d| d as usize)
            .ok_or_else(|| format!("`{name}` needs a `deck` number from 1"))
    };
    let number = |key: &str| {
        c.get(key)
            .and_then(Value::as_f64)
            .ok_or_else(|| format!("`{name}` needs a number `{key}`"))
    };
    match name {
        "toast" => Ok(Command::Toast(text("text")?)),
        "load" => Ok(Command::Load {
            deck: deck()?,
            file: text("file")?.into(),
        }),
        "import" => Ok(Command::Import(
            c.get("files")
                .and_then(Value::as_array)
                .ok_or("`import` needs a list `files`")?
                .iter()
                .filter_map(|f| f.as_str().map(PathBuf::from))
                .collect(),
        )),
        "key" => Ok(Command::Key(text("action")?)),
        "play" | "pause" => Ok(Command::Play {
            deck: deck()?,
            on: name == "play",
        }),
        "seek" => Ok(Command::Seek {
            deck: deck()?,
            ms: number("ms")?.max(0.0),
        }),
        "tempo" => Ok(Command::Tempo {
            deck: deck()?,
            percent: number("percent")?,
        }),
        "open" => Ok(Command::Open(text("url")?)),
        "setting" => Ok(Command::Setting {
            id: text("id")?,
            value: c.get("value").cloned().ok_or("`setting` needs a `value`")?,
        }),
        other => Err(format!("unknown command {other:?}")),
    }
}

#[cfg(test)]
mod tests;
