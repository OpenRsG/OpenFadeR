//! Built-in compatibility adapters (`compatibility/`). They are library plugins like any other
//! and are called through the same JSON `call` of a [`LibraryApi`] ([`super::library`]); they only have no folder, no
//! `libloading` and no switch. A [`Builtin`] holds what a folder would: the `plugin.json` text, the `locales/`, the
//! `players.json`. The registry lists them first and always on, whatever `plugins.json` says; a folder of the same name
//! (an old install, a GET PLUGINS copy) is not read ([`super::discover`]).

use super::{Manifest, Plugin};
use openfader_plugin::library::LibraryApi;
#[cfg(feature = "compatibility")]
use openfader_plugin::library::api;
use std::sync::LazyLock;

pub struct Builtin {
    /// `plugin.json`, without the `lib` entry: there is no library file.
    manifest: &'static str,
    /// `(language code, locales/<code>.json)`.
    locales: &'static [(&'static str, &'static str)],
    /// `players.json`, for a device plugin that has one.
    players: Option<&'static str>,
    pub(super) api: &'static LibraryApi,
}

#[cfg(feature = "compatibility")]
macro_rules! locales {
    ($dir:literal) => {
        &[
            (
                "de",
                include_str!(concat!("../../compatibility/", $dir, "/locales/de.json")),
            ),
            (
                "es",
                include_str!(concat!("../../compatibility/", $dir, "/locales/es.json")),
            ),
            (
                "fr",
                include_str!(concat!("../../compatibility/", $dir, "/locales/fr.json")),
            ),
            (
                "it",
                include_str!(concat!("../../compatibility/", $dir, "/locales/it.json")),
            ),
            (
                "nl",
                include_str!(concat!("../../compatibility/", $dir, "/locales/nl.json")),
            ),
            (
                "pt",
                include_str!(concat!("../../compatibility/", $dir, "/locales/pt.json")),
            ),
        ]
    };
}

#[cfg(feature = "compatibility")]
macro_rules! builtin {
    ($dir:literal, $plugin:ty, $players:expr) => {{
        static API: LibraryApi = api::<$plugin>();
        Builtin {
            manifest: include_str!(concat!("../../compatibility/", $dir, "/plugin.json")),
            locales: locales!($dir),
            players: $players,
            api: &API,
        }
    }};
}

#[cfg(feature = "compatibility")]
static ALL: [Builtin; 3] = [
    builtin!(
        "rekordbox/device",
        compat_rekordbox_device::Device,
        Some(include_str!(
            "../../compatibility/rekordbox/device/players.json"
        ))
    ),
    builtin!(
        "rekordbox/xml",
        compat_rekordbox_xml::RekordboxXmlPlugin,
        None
    ),
    builtin!("rekordbox/db", compat_rekordbox_db::MasterDb, None),
];

/// A build without the `compatibility` feature (the mobile crate) has no built-in plugins, and so no SQLCipher.
#[cfg(not(feature = "compatibility"))]
static ALL: [Builtin; 0] = [];

impl Builtin {
    /// The player models of a device plugin (its `players.json`).
    pub fn players(&self) -> Option<&'static str> {
        self.players
    }

    /// `text` of this plugin in the language set ([`crate::locale::tr_embedded`]).
    pub fn tr(&self, name: &str, text: &str) -> String {
        crate::locale::tr_embedded(name, self.locales, text)
    }
}

static PLUGINS: LazyLock<Vec<Plugin>> = LazyLock::new(|| {
    ALL.iter()
        .map(|b| {
            let manifest: Manifest =
                serde_json::from_str(b.manifest).expect("a built-in plugin.json parses");
            super::validate(&manifest, "built-in", None).expect("a built-in plugin.json is valid");
            Plugin {
                dir: Default::default(),
                manifest,
                builtin: Some(b),
            }
        })
        .collect()
});

/// The built-in plugins in listing order, each with an empty folder.
pub fn plugins() -> &'static [Plugin] {
    &PLUGINS
}

/// Whether a plugin of this name is compiled in (a folder of that name is not read).
pub fn is(name: &str) -> bool {
    PLUGINS.iter().any(|p| p.manifest.name == name)
}

#[cfg(all(test, not(feature = "compatibility")))]
mod tests_without {
    #[test]
    fn there_are_none() {
        assert!(super::plugins().is_empty());
    }
}

#[cfg(all(test, feature = "compatibility"))]
mod tests {
    use super::*;

    #[test]
    fn every_built_in_is_a_valid_library_that_answers_through_the_json_call() {
        let all = plugins();
        assert_eq!(all.len(), ALL.len());
        for p in all {
            let b = p.builtin.expect("built in");
            let lib = p.manifest.library.as_ref().expect("a library plugin");
            assert!(
                lib.lib.file().is_none(),
                "{}: no library file",
                p.manifest.name
            );
            let req = br#"{"op": "probe", "path": "/nowhere"}"#;
            let buf = unsafe { (b.api.call)(req.as_ptr(), req.len(), std::ptr::null()) };
            let v: serde_json::Value =
                serde_json::from_slice(unsafe { std::slice::from_raw_parts(buf.ptr, buf.len) })
                    .unwrap();
            unsafe { (b.api.free)(buf) };
            assert_eq!(v["score"], 0, "{}: {v}", p.manifest.name);
        }
        let names: Vec<&str> = all.iter().map(|p| p.manifest.name.as_str()).collect();
        assert_eq!(names, ["rekordbox-device", "rekordbox-xml", "rekordbox-db"]);
    }

    #[test]
    fn the_players_and_the_translations_are_embedded() {
        let device = &plugins()[0];
        let players: openfader_plugin::library::Players =
            serde_json::from_str(device.builtin.unwrap().players().unwrap()).unwrap();
        assert!(!players.players.is_empty());
        for p in plugins() {
            for (code, json) in p.builtin.unwrap().locales {
                let map: std::collections::HashMap<String, String> = serde_json::from_str(json)
                    .unwrap_or_else(|e| panic!("{} {code}: {e}", p.manifest.name));
                assert!(!map.is_empty(), "{} {code}", p.manifest.name);
            }
        }
    }
}
