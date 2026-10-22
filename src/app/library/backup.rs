//! File > Library > Backup Library / Restore Library: "Do you want to back up music files as well?",
//! a folder, "Collection was backed up."; Restore asks for the zip and replaces our data ([`openfader::store`]'s backup
//! format; the settings file inside is applied at once, music files are copied back where they are missing).

use super::{Confirm, Ctx, prefs};
use openfader::store::{Backup, Restored};
use std::{path::PathBuf, rc::Rc};

/// Backup into `dir`, with the music files of the Collection when `music`.
pub fn run(c: &Rc<Ctx>, dir: PathBuf, music: bool) {
    let mut files: Vec<PathBuf> = Vec::new();
    if music {
        files = c.known().into_iter().filter(|f| f.is_file()).collect();
        files.sort();
    }
    let (store, settings) = (c.store.clone(), prefs::path());
    let stamp = chrono::Local::now().format("%Y-%m-%d_%H-%M-%S").to_string();
    c.say(if music {
        tr!("backing up the library and its music…")
    } else {
        tr!("backing up the library…")
    });
    c.spawn(
        move || store.backup(&dir, settings.as_deref(), &files, &stamp),
        |c, r| match r {
            Ok(Backup {
                zip,
                music,
                music_failed,
            }) => {
                let files = if music + music_failed > 0 {
                    tr!(
                        " · {} music files copied{}",
                        music,
                        if music_failed > 0 {
                            tr!(", {} failed", music_failed)
                        } else {
                            String::new()
                        }
                    )
                } else {
                    String::new()
                };
                c.say(&tr!("Collection was backed up: {}{}", zip.display(), files));
            }
            Err(e) => c.say(&tr!("backup failed: {}", e)),
        },
    );
}

/// The zip was chosen: confirm before replacing anything.
pub fn confirm_restore(c: &Rc<Ctx>, zip: PathBuf) {
    if !zip.is_file() {
        return c.say(&tr!("{}: no such file", zip.display()));
    }
    let text = tr!(
        "Restore {}? The Collection, playlists, cues, grids, ratings, tags, history and settings are replaced by the backup's.",
        zip.display()
    );
    c.confirm(
        Confirm::Restore(zip),
        tr!("RESTORE LIBRARY"),
        &text,
        tr!("RESTORE"),
        "",
    );
}

pub fn restore(c: &Rc<Ctx>, zip: PathBuf) {
    if c.protected(tr!("restore a library")) {
        return;
    }
    let store = c.store.clone();
    c.say(tr!("restoring the library…"));
    c.spawn(
        move || store.restore(&zip),
        |c, r| match r {
            Ok(Restored {
                settings,
                music,
                music_present,
                music_lost,
            }) => {
                if let Some(text) = settings.map(|s| String::from_utf8_lossy(&s).into_owned()) {
                    let (restored, _) = prefs::Prefs::parse(&text);
                    prefs::update(|p| *p = restored);
                }
                let files = if music + music_present + music_lost > 0 {
                    tr!(
                        " · music: {} copied back, {} already there, {} not in the backup",
                        music,
                        music_present,
                        music_lost
                    )
                } else {
                    String::new()
                };
                c.say(&tr!("Collection was restored{}", files));
                c.reload(None);
            }
            Err(e) => c.say(&tr!("restore failed: {}", e)),
        },
    );
}
