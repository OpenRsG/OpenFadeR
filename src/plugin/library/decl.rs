//! `"library"` of `plugin.json` ([`LibraryDecl`]) and the manifest rules it must keep ([`check`]).

use serde::Deserialize;
use std::collections::BTreeMap;

/// `"library"` of `plugin.json`.
#[derive(Deserialize, Clone, Debug)]
pub struct LibraryDecl {
    /// The native library, relative to the plugin folder: a file name, or one per OS (`linux`, `windows`, `macos`).
    pub lib: LibFile,
    /// What the plugin reads: lower-case file extensions without the dot, and `dir` for folders. Empty: probed for every path.
    #[serde(default)]
    pub formats: Vec<String>,
    /// Menu label of File > Export: the plugin writes the Collection as a file (named with the first of `formats`).
    #[serde(default)]
    pub collection_writer: Option<String>,
    /// Menu label of File > Import: the plugin reads a collection file the user names.
    #[serde(default)]
    pub collection_reader: Option<String>,
    /// A Media Browser source: the library file is the plugin's setting `path_setting`.
    #[serde(default)]
    pub browser: Option<BrowserDecl>,
    /// The plugin answers the device ops (`device_probe`, `export`, `update`, …, see [`super::device`]); it reads folders.
    #[serde(default)]
    pub device: bool,
    /// The plugin also answers the analysis refs stored before refs carried a plugin name: bare absolute paths.
    #[serde(default)]
    pub legacy_analysis_refs: bool,
    /// The plugin reads sample packs: importing one also fills the sampler's empty slots with its first playlist.
    #[serde(default)]
    pub samples: bool,
    /// Where the app this plugin reads usually keeps its library: glob patterns per OS (`linux`, `windows`, `macos`), or one
    /// list for every OS. `~`, `$HOME`, `%APPDATA%` and `%USERPROFILE%` expand; `*` matches within one path part.
    /// Found libraries are offered by Preferences > Libraries.
    #[serde(default)]
    pub detect: Detect,
}

/// A Media Browser source of a library plugin.
#[derive(Deserialize, Clone, Debug)]
pub struct BrowserDecl {
    /// Name of the source in the browser tree.
    pub label: String,
    /// Id of the plugin's `text` setting that holds the path of the library; no path, no source.
    pub path_setting: String,
}

#[derive(Deserialize, Clone, Debug)]
#[serde(untagged)]
pub enum LibFile {
    One(String),
    PerOs(BTreeMap<String, String>),
}

impl LibFile {
    /// The file for this OS (`linux`, `windows`, `macos`).
    pub fn file(&self) -> Option<&str> {
        match self {
            LibFile::One(f) => Some(f),
            LibFile::PerOs(m) => m.get(std::env::consts::OS).map(String::as_str),
        }
    }
}

/// `"detect"` of [`LibraryDecl`]: default library locations, one list or one per OS.
#[derive(Deserialize, Clone, Debug)]
#[serde(untagged)]
pub enum Detect {
    All(Vec<String>),
    PerOs(BTreeMap<String, Vec<String>>),
}

impl Default for Detect {
    fn default() -> Self {
        Detect::All(Vec::new())
    }
}

impl Detect {
    /// The patterns for this OS (`linux`, `windows`, `macos`).
    pub fn patterns(&self) -> &[String] {
        match self {
            Detect::All(v) => v,
            Detect::PerOs(m) => m
                .get(std::env::consts::OS)
                .map_or(&[][..], Vec::as_slice),
        }
    }
}

/// Manifest rules of [`LibraryDecl`]; `settings` are the plugin's setting ids with their kinds.
pub fn check(d: &LibraryDecl, settings: &[crate::plugin::SettingDecl]) -> Result<(), String> {
    if d.lib.file().is_none_or(str::is_empty) {
        return Err("library: `lib` must name a file for this OS".into());
    }
    if d.formats
        .iter()
        .any(|f| f.is_empty() || *f != f.to_lowercase() || f.contains('.'))
    {
        return Err("library: `formats` are lower-case extensions without a dot, or `dir`".into());
    }
    if d.collection_writer.is_some() && d.formats.first().is_none_or(|f| f == "dir") {
        return Err(
            "library: a `collection_writer` needs a file extension as the first of `formats`"
                .into(),
        );
    }
    if d.device && !d.formats.iter().any(|f| f == "dir") {
        return Err("library: a `device` plugin reads folders: `formats` must list `dir`".into());
    }
    for label in [&d.collection_writer, &d.collection_reader]
        .into_iter()
        .flatten()
    {
        if label.is_empty() {
            return Err("library: menu labels must not be empty".into());
        }
    }
    if let Some(b) = &d.browser
        && (b.label.is_empty()
            || !settings
                .iter()
                .any(|s| s.id == b.path_setting && s.kind == crate::plugin::SettingKind::Text))
    {
        return Err(
            "library: `browser` needs a label and a `path_setting` naming a text setting".into(),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn decl(v: serde_json::Value) -> LibraryDecl {
        let mut m = json!({"lib": "x.so"});
        m.as_object_mut()
            .unwrap()
            .extend(v.as_object().unwrap().clone());
        serde_json::from_value(m).unwrap()
    }

    #[test]
    fn a_device_plugin_reads_folders() {
        assert!(check(&decl(json!({"device": true, "formats": ["dir"]})), &[]).is_ok());
        assert!(check(&decl(json!({"device": true, "formats": ["db"]})), &[]).is_err());
        assert!(check(&decl(json!({"device": true})), &[]).is_err());
        let d = decl(json!({"legacy_analysis_refs": true, "samples": true}));
        assert!(d.legacy_analysis_refs && d.samples && !d.device);
    }
}
