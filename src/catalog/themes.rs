//! Theme files of a repository: every `*.json` up to [`super::DEPTH`] folders below the top folder that is not part of a plugin.
//! Whether one is a theme is up to the skin parser of the app (a file with no theme token is none); this only reads them.

use super::{below_top, plugins};
use crate::zip::Zip;
use std::path::Path;

/// A candidate theme file.
#[derive(Clone, Debug)]
pub struct File {
    /// The file name without `.json`.
    pub stem: String,
    pub text: String,
}

/// The biggest theme file read.
const MAX_FILE: u64 = 1 << 20;

/// The `*.json` files of `z` outside the folders of `plugins`, in archive order.
pub fn scan(z: &mut Zip, plugins: &[plugins::Entry]) -> Vec<File> {
    let names: Vec<String> = z
        .names()
        .filter(|n| {
            below_top(n).is_some_and(|(_, rest)| {
                rest.ends_with(".json")
                    && rest.matches('/').count() <= super::DEPTH
                    && !rest.ends_with("/plugin.json")
                    && rest != "plugin.json"
            }) && !plugins.iter().any(|p| n.starts_with(&p.dir))
        })
        .map(str::to_owned)
        .collect();
    names
        .into_iter()
        .filter_map(|n| {
            if z.size(&n)? > MAX_FILE {
                return None;
            }
            let text = String::from_utf8(z.read(&n).ok()?).ok()?;
            let stem = Path::new(&n).file_stem()?.to_string_lossy().into_owned();
            Some(File { stem, text })
        })
        .collect()
}
