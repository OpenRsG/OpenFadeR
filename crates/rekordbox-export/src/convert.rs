//! "Convert from Device Library" (rekordbox: [Devices] > device > OneLibrary > Convert from Device Library): builds
//! `exportLibrary.db` from the `export.pdb` / `exportExt.pdb` the device already holds, same tracks, playlists, MyTag and
//! histories, same ids. An existing OneLibrary is replaced, so playlists or histories that only it held are lost (observed
//! strings: "OneLibrary already exists in the device. Do you want to overwrite it?"; the web FAQ warns about the loss).

use super::pdb::read;
use super::progress::Progress;
use super::settings::DeviceConfig;
use super::{carry, onelibrary, stage_onelibrary, verify, ONE, OWNER};
use openfader_plugin::library::{
    commit::{self, Commit},
    ActionAnswer, Confirm,
};
use rekordbox_common::{at, on_device};
use std::path::{Path, PathBuf};

/// Id of the device action.
pub const ID: &str = "convert";

/// The question asked before an existing OneLibrary is replaced.
const EXISTS: &str = "OneLibrary already exists in the device. Do you want to overwrite it?";

/// `device_action("convert")`: writes `<dest>/PIONEER/rekordbox/exportLibrary.db` (encrypted with `key`) from the Device Library.
/// An existing one is replaced only when `confirmed` (else the answer is a [`Confirm`]). Artwork gets its `b` copy where missing.
pub fn to_onelibrary(
    dest: &Path,
    key: &str,
    confirmed: bool,
    config: &DeviceConfig,
) -> Result<ActionAnswer, String> {
    let one = dest.join(ONE);
    if one.is_file() && !confirmed {
        return Ok(ActionAnswer {
            confirm: Some(Confirm {
                text: EXISTS.into(),
                yes: "Overwrite".into(),
            }),
            ..ActionAnswer::default()
        });
    }
    let (tracks, lists) = build(dest, key, config)?;
    Ok(ActionAnswer {
        message: format!("Converted {tracks} track(s) and {lists} playlist(s) to the OneLibrary"),
        confirm: None,
    })
}

/// Writes the OneLibrary of `dest` from its Device Library (replacing one that is there) through one commit; the number of
/// tracks and of playlists and folders it holds.
pub(crate) fn build(
    dest: &Path,
    key: &str,
    config: &DeviceConfig,
) -> Result<(usize, usize), String> {
    let one = dest.join(ONE);
    let mut c = Commit::new(dest, OWNER)?;
    let (db, tags) = read::open(dest, config)?;
    let mut progress = Progress::new();
    progress.phase("Writing the library", db.tracks.len())?;
    let mut made = Vec::new();
    let staged = (|| {
        for (n, t) in db.tracks.iter().enumerate() {
            progress.tick(n)?;
            copy_cover(dest, t.artwork.as_deref(), &mut made)?;
        }
        stage_onelibrary(&mut c, &one, key, &db, &tags, |tmp| {
            carry::histories(tmp, key, &db)
        })?;
        progress.phase("Checking the library", 1)?;
        verify::staged(&c, &db, &tags, key)
    })();
    if let Err(e) = staged.and_then(|()| c.commit()) {
        // The covers are only copies: the library that was in use does not name them. A change the commit began is finished by
        // the next recovery, and then names them, so they stay.
        if commit::pending(dest).is_none() {
            made.iter().for_each(|p| drop(std::fs::remove_file(p)));
        }
        return Err(e);
    }
    Ok((db.tracks.len(), db.nodes.len()))
}

/// `a<n>.jpg` → its OneLibrary twin `b<n>.jpg` (byte-identical, observed), when the cover exists and the twin does not; the
/// twin made is added to `made`.
fn copy_cover(dest: &Path, artwork: Option<&str>, made: &mut Vec<PathBuf>) -> Result<(), String> {
    let Some(artwork) = artwork else {
        return Ok(());
    };
    let a = on_device(dest, artwork);
    let b = on_device(dest, &onelibrary::image_variant(artwork));
    if a.is_file() && !b.exists() {
        commit::write_synced(&b, &at(std::fs::read(&a), &a)?)?;
        made.push(b);
    }
    Ok(())
}
