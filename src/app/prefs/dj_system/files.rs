//! File fields (a picture the plugin writes to the device): an import row (the path of a file, which the host copies to
//! `<data dir>/device-files/<scope>/<key>.<ext>`, see `store::device_settings::import_file`), delete, and on a device "copy
//! from Preferences". The setting's value is the path of the copy.

use super::{Ctx, Done, Row, find};
use openfader::store::device_settings;
use openfader_plugin::library::{Field, FieldKind, Tab};
use std::path::Path;

pub fn rows(c: &Ctx, f: &Field) -> Vec<Row> {
    let FieldKind::File { extensions, .. } = &f.kind else {
        return Vec::new();
    };
    let v = c.value(f);
    let status = std::fs::metadata(&v)
        .ok()
        .filter(|_| !v.is_empty())
        .map_or("none imported".to_owned(), |m| {
            format!("{} KB", m.len().div_ceil(1000))
        });
    let mut rows = vec![
        Row::info(&f.label, &status),
        Row::text(
            &format!("djs.f.{}", f.key),
            &format!("Import file ({})", extensions.join(", ")),
            "",
        ),
        Row::chips(
            &format!("djs.del.{}", f.key),
            "Delete file",
            &["DELETE"],
            usize::MAX,
        )
        .enabled(c.has_own(&f.key)),
    ];
    if c.is_device() {
        rows.push(Row::chips(
            &format!("djs.copy.{}", f.key),
            "Copy from Preferences",
            &["COPY"],
            usize::MAX,
        ));
    }
    rows
}

/// Copies `src` in as the scope's file of `f` and makes it the value.
fn put_copy(c: &mut Ctx, f: &Field, src: &Path) -> Result<(), String> {
    let FieldKind::File { extensions, .. } = &f.kind else {
        return Err(format!("{}: not a file field", f.key));
    };
    let root = c.files.as_deref().ok_or("no data directory")?;
    let to = device_settings::import_file(root, c.scope, &f.key, src, extensions)?;
    c.put(f, &to.to_string_lossy())
}

/// A path was entered: import that file and write the keys the field says an import sets (an empty entry does nothing).
pub fn import(c: &mut Ctx, tab: &Tab, f: &Field, path: &str) -> Done {
    if path.is_empty() {
        return Ok(String::new());
    }
    put_copy(c, f, Path::new(path))?;
    if let FieldKind::File { sets, .. } = &f.kind {
        for (key, value) in sets {
            c.put(
                find(tab, key).ok_or_else(|| format!("{key}: no such field"))?,
                value,
            )?;
        }
    }
    Ok("file imported".into())
}

/// "Copy from Preferences": the device gets its own copy of the file the defaults hold.
pub fn copy(c: &mut Ctx, f: &Field) -> Done {
    let global = c.base(f);
    if global.is_empty() {
        return Err("no file in the Preferences (import one there first)".into());
    }
    put_copy(c, f, Path::new(&global)).map(|()| "file copied".into())
}

/// Forgets the scope's file (a device then follows the Preferences again).
pub fn delete(c: &mut Ctx, f: &Field) -> Done {
    c.reset([f]).map(|()| "file deleted".into())
}
