//! CONTROLLER PLUGINS rows: which driver a MIDI input port gets: AUTO (the controller plugins are asked, the best one that
//! recognises the port drives it, else our mapping files), OPENFADER MAPPING (the plugins are skipped) or one plugin by name. The
//! choice is stored per port in `controller_plugins` as `port=plugin;…` (`-` = OpenFadeR mapping, AUTO has no entry); the port
//! is named without the ids ALSA adds to it ([`port_key`]), which change from one plug to the next.

use super::{Controller, Prefs, Row};
use crate::app::midi;
use openfader::plugin::controller;

/// What drives a port.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Driver {
    Auto,
    Mapping,
    Plugin(String),
}

/// `name` without the client and port numbers ALSA appends ("Pad MIDI 1 20:0" is "Pad MIDI 1"): the name of a port that stays
/// the same when it is plugged in again.
pub fn port_key(name: &str) -> &str {
    name.rsplit_once(' ')
        .filter(|(_, ids)| {
            ids.split_once(':').is_some_and(|(a, b)| {
                [a, b]
                    .iter()
                    .all(|n| !n.is_empty() && n.bytes().all(|c| c.is_ascii_digit()))
            })
        })
        .map_or(name, |(key, _)| key)
}

impl Controller {
    fn entries(&self) -> impl Iterator<Item = (&str, &str)> {
        self.controller_plugins
            .split(';')
            .filter_map(|e| e.rsplit_once('='))
    }

    /// The driver the user chose for the input port `port`.
    pub fn driver(&self, port: &str) -> Driver {
        let key = port_key(port);
        match self.entries().find(|(k, _)| *k == key) {
            None => Driver::Auto,
            Some((_, "-")) => Driver::Mapping,
            Some((_, plugin)) => Driver::Plugin(plugin.to_owned()),
        }
    }

    pub fn set_driver(&mut self, port: &str, d: Driver) {
        let key = port_key(port);
        let mut keep: Vec<String> = self
            .entries()
            .filter(|(k, _)| *k != key)
            .map(|(k, v)| format!("{k}={v}"))
            .collect();
        match d {
            Driver::Auto => {}
            Driver::Mapping => keep.push(format!("{key}=-")),
            Driver::Plugin(p) => keep.push(format!("{key}={p}")),
        }
        self.controller_plugins = keep.join(";");
    }
}

/// The controller plugins that are switched on, by name.
fn plugins() -> Vec<String> {
    controller::enabled().map(|l| l.name().to_owned()).collect()
}

pub(super) fn rows(c: &Controller) -> Vec<Row> {
    let mut rows = vec![Row::header(tr!("CONTROLLER PLUGINS"))];
    let names = plugins();
    if names.is_empty() {
        rows.push(Row::info(
            "",
            tr!("no controller plugin is switched on (PREFS > EXTENSIONS)"),
        ));
        return rows;
    }
    let ports = midi::ports();
    if ports.is_empty() {
        rows.push(Row::info("", tr!("no MIDI input ports")));
    }
    for (n, p) in ports.iter().enumerate() {
        let mut options = vec![tr!("AUTO").to_owned(), tr!("OPENFADER MAPPING").to_owned()];
        options.extend(names.iter().cloned());
        let current = match c.driver(&p.name) {
            Driver::Auto => 0,
            Driver::Mapping => 1,
            Driver::Plugin(d) => names.iter().position(|n| *n == d).map_or(0, |i| i + 2),
        };
        let row = Row::dropdown_of(&format!("ctl_driver:{n}"), &p.name, options, current);
        rows.push(match &p.driver {
            Some(d) => row.hint(&tr!("driven by {}", d)),
            None => row,
        });
    }
    rows.push(Row::info(
        "",
        tr!("AUTO: the plugin that recognises the port drives it; a port nobody claims uses the mapping files"),
    ));
    rows
}

/// A click on a driver row: `key` is `ctl_driver:<port number>`, `i` the option.
pub(super) fn choose(p: &mut Prefs, key: &str, i: usize) -> bool {
    let Some(n) = key
        .strip_prefix("ctl_driver:")
        .and_then(|n| n.parse::<usize>().ok())
    else {
        return false;
    };
    let Some(port) = midi::ports().into_iter().nth(n) else {
        return false;
    };
    let driver = match i {
        0 => Driver::Auto,
        1 => Driver::Mapping,
        _ => match plugins().into_iter().nth(i - 2) {
            Some(name) => Driver::Plugin(name),
            None => return false,
        },
    };
    p.controller.set_driver(&port.name, driver);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_port_is_known_by_its_name_without_the_alsa_ids() {
        assert_eq!(port_key("Pad MIDI 1 20:0"), "Pad MIDI 1");
        assert_eq!(port_key("Pad 3:1x"), "Pad 3:1x", "not ids");
        assert_eq!(port_key("USB MIDI"), "USB MIDI");
    }

    #[test]
    fn the_chosen_driver_is_stored_per_port_and_survives_a_replug() {
        let mut c = Controller::default();
        assert_eq!(c.driver("Pad 20:0"), Driver::Auto);
        c.set_driver("Pad 20:0", Driver::Plugin("mixxx".into()));
        c.set_driver("Other 21:0", Driver::Mapping);
        assert_eq!(c.driver("Pad 28:1"), Driver::Plugin("mixxx".into()));
        assert_eq!(c.driver("Other 21:0"), Driver::Mapping);
        c.set_driver("Pad 20:0", Driver::Auto);
        assert_eq!(c.controller_plugins, "Other=-");
        assert_eq!(c.driver("Pad 20:0"), Driver::Auto);
    }
}
