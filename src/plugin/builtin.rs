//! Built-in compatibility adapters (`compatibility/`) use the same JSON [`LibraryApi`] as folder plugins,
//! without a dynamic library or plugin folder. A [`Builtin`] embeds its manifest, locales and player metadata.
//! Compiled availability is separate from activation: mandatory adapters stay on; optional ones use the saved
//! plugin choices. Old folders with the same name are ignored by [`super::discover`].

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
    pub(super) always_on: bool,
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
    ($dir:literal, $plugin:ty, $players:expr, $always_on:expr) => {{
        static API: LibraryApi = api::<$plugin>();
        Builtin {
            manifest: include_str!(concat!("../../compatibility/", $dir, "/plugin.json")),
            locales: locales!($dir),
            players: $players,
            always_on: $always_on,
            api: &API,
        }
    }};
}

#[cfg(feature = "compatibility")]
static ALL: [Builtin; 4] = [
    builtin!(
        "rekordbox/device",
        compat_rekordbox_device::Device,
        Some(include_str!(
            "../../compatibility/rekordbox/device/players.json"
        )),
        true
    ),
    builtin!(
        "rekordbox/xml",
        compat_rekordbox_xml::RekordboxXmlPlugin,
        None,
        true
    ),
    builtin!("rekordbox/db", compat_rekordbox_db::MasterDb, None, true),
    builtin!("traktor", compat_traktor::Plugin, None, false),
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
