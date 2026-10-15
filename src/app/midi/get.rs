//! Mapping files from the repositories (`app/catalog`, GET MAPPINGS and the offer for a controller with no mapping): reading a file as
//! [`mapfile`] does ([`entry`]), whether it is installed ([`action`]), installing it into `<config>/mappings/` ([`install`]: the
//! folder the mapping search path reads before the bundled one, so a file there replaces one of the same name further down) and
//! making the connected controller use it at once ([`reload_if_connected`], what a hot-plug does). [`entry`], [`install`] and [`best`]
//! touch no Slint state, so the catalog calls them from its worker threads.

use super::{mapfile, Midi};
use openfader::catalog::mappings::File;
use std::{
    cell::RefCell,
    collections::HashMap,
    path::{Path, PathBuf},
    rc::{Rc, Weak},
};

/// A mapping file a repository holds, read.
#[derive(Clone, Debug)]
pub struct Entry {
    /// The file name, `.xml` included: what it is installed as.
    pub name: String,
    pub controller: String,
    /// Its `<port>`: picked when an input port name contains it.
    pub port: String,
    pub text: String,
}

/// What an [`Entry`]'s button does.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Action {
    Install,
    /// A file of that name is there and differs.
    Update,
    Installed,
}

/// The mapping in `f`; the error says why [`mapfile::parse`] refused it.
pub fn entry(f: &File) -> Result<Entry, String> {
    let (m, port) = mapfile::parse(&f.text).map_err(|e| format!("{}: {e}", f.name))?;
    Ok(Entry {
        name: f.name.clone(),
        controller: m.controller,
        port,
        text: f.text.clone(),
    })
}

/// The mapping files the app would read now, by file name (the first folder of the search path wins): the text of each.
pub fn installed() -> HashMap<String, String> {
    mapfile::files().into_iter().collect()
}

/// Whether `e` is installed among `installed` ([`installed`]).
pub fn action(e: &Entry, installed: &HashMap<String, String>) -> Action {
    match installed.get(&e.name) {
        None => Action::Install,
        Some(t) if t.trim() == e.text.trim() => Action::Installed,
        Some(_) => Action::Update,
    }
}

/// The entry whose `<port>` fits the connected input `ports` best (the longest, as [`mapfile::load`] picks), if any.
pub fn best<'a>(ports: &[String], entries: &'a [Entry]) -> Option<&'a Entry> {
    entries
        .iter()
        .filter_map(|e| Some((mapfile::match_len(ports, &e.port)?, e)))
        .max_by_key(|(len, _)| *len)
        .map(|(_, e)| e)
}

fn user_dir() -> Result<PathBuf, String> {
    openfader::paths::config_dir()
        .map(|d| d.join("mappings"))
        .ok_or_else(|| "no config folder (set HOME)".into())
}

/// Writes `e` into the user's mapping folder (through a `.part` file), after checking that it is a plain `*.xml` file name and that
/// the parser still takes the text.
pub fn install(e: &Entry) -> Result<(), String> {
    install_in(&user_dir()?, e)
}

fn install_in(dir: &Path, e: &Entry) -> Result<(), String> {
    let plain = Path::new(&e.name)
        .file_name()
        .is_some_and(|n| n == e.name.as_str());
    if !plain || !e.name.ends_with(".xml") {
        return Err(format!("{:?} is not a mapping file name", e.name));
    }
    mapfile::parse(&e.text).map_err(|why| format!("{}: {why}", e.name))?;
    openfader::paths::at(std::fs::create_dir_all(dir), dir)?;
    let (to, part) = (dir.join(&e.name), dir.join(format!("{}.part", e.name)));
    openfader::paths::at(std::fs::write(&part, &e.text), &part)?;
    openfader::paths::at(std::fs::rename(&part, &to), &to)
}

thread_local! {
    /// The session, for [`reload_if_connected`]; set by `wire`.
    static MIDI: RefCell<Weak<Midi>> = RefCell::new(Weak::new());
}

pub(super) fn register(midi: &Rc<Midi>) {
    MIDI.with_borrow_mut(|m| *m = Rc::downgrade(midi));
}

/// When an input port matching `e` is connected, opens the session again (the mapping is read from the folders, so the controller
/// is loaded with the file just installed, lights and all, as after a plug). UI thread only.
pub fn reload_if_connected(e: &Entry) {
    let Some(midi) = MIDI.with_borrow(Weak::upgrade) else {
        return;
    };
    let (Some(app), names) = (midi.app.upgrade(), midi.names.borrow().clone()) else {
        return;
    };
    if mapfile::match_len(&names, &e.port).is_some() {
        midi.reconnect(&app, names);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str, controller: &str, port: &str) -> File {
        File {
            name: name.into(),
            text: format!(
                "<OpenFadeRMapping><info><name>{controller}</name><port>{port}</port></info><controls>\
                 <control status=\"0x90\" midino=\"0x0B\" action=\"play\" deck=\"0\"/></controls></OpenFadeRMapping>"
            ),
        }
    }

    #[test]
    fn the_longest_port_picks_and_the_installed_copy_decides_the_button() {
        let all: Vec<Entry> = [
            file("a.xml", "Test", "Test"),
            file("b.xml", "Test Pro", "Test Pro"),
            file("c.xml", "Other", "Other"),
        ]
        .iter()
        .map(|f| entry(f).unwrap())
        .collect();
        let ports = ["Test Pro:Test Pro MIDI 1 20:0".to_owned()];
        assert_eq!(best(&ports, &all).unwrap().controller, "Test Pro");
        assert!(best(&["Midi Through:Midi Through Port-0 14:0".to_owned()], &all).is_none());
        let mut have = HashMap::new();
        assert_eq!(action(&all[0], &have), Action::Install);
        have.insert("a.xml".to_owned(), format!("{}\n", all[0].text));
        assert_eq!(action(&all[0], &have), Action::Installed);
        have.insert("a.xml".to_owned(), "old".to_owned());
        assert_eq!(action(&all[0], &have), Action::Update);
        assert!(
            entry(&File {
                name: "x.xml".into(),
                text: "<OpenFadeRMapping><info/></OpenFadeRMapping>".into()
            })
            .is_err(),
            "no <name>"
        );
    }

    #[test]
    fn install_writes_a_valid_plain_file_name_only() {
        let dir = std::env::temp_dir().join(format!("rk-mapget-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let e = entry(&file("a.xml", "Test", "Test")).unwrap();
        install_in(&dir, &e).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("a.xml")).unwrap(), e.text);
        assert!(!dir.join("a.xml.part").exists());
        for bad in ["../a.xml", "sub/a.xml", "a.txt", ""] {
            let evil = Entry {
                name: bad.into(),
                ..e.clone()
            };
            assert!(install_in(&dir, &evil).is_err(), "{bad:?}");
        }
        let broken = Entry {
            text: "<OpenFadeRMapping/>".into(),
            ..e
        };
        assert!(install_in(&dir, &broken).is_err());
        std::fs::remove_dir_all(dir).ok();
    }
}
