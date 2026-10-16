//! Several library formats on one device: an export writes the formats picked for the device one after the other (each is a
//! device plugin, [`Provider::export`]). The first copies the audio; before the next ones run, the snapshot's tracks are pointed
//! at the copies the first one's library lists ([`Snapshot::retarget`]), so the device holds one copy of each track whatever
//! folders that library files them under. Each format gets the snapshot fitted to what it can hold
//! ([`translate::fit_tracks`]); what did not fit is added to the report's notes.

use crate::library::{Library, snapshot::Snapshot, translate};
use crate::plugin::device::{Control, ExportOptions, ExportReport, Provider};
use crate::plugin::library::capabilities_of;
use crate::store;
use std::borrow::Cow;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// What one format's export did: the plugin name and its report or error.
pub type Written = (String, Result<ExportReport, String>);

/// Writes `snap` to `dest` with each of `writers` in turn. A format that fails does not stop the others; a cancel does
/// (`Err("cancelled")`), as does a failure of the first one, which the others would build on.
pub fn export(
    writers: &[Provider],
    dest: &Path,
    snap: &Snapshot,
    o: &ExportOptions,
    ctl: &Arc<Control>,
) -> Result<Vec<Written>, String> {
    let mut snap = Cow::Borrowed(snap);
    let mut done: Vec<Written> = Vec::new();
    for (n, w) in writers.iter().enumerate() {
        if n == 1
            && let Some(copies) = copies(&writers[0], dest)
        {
            snap.to_mut().retarget(&copies);
        }
        let caps = capabilities_of(w.name());
        let mut fitted = Cow::Borrowed(&*snap);
        let mut loss = translate::Loss::default();
        if !translate::is_noop(&caps) {
            loss = translate::fit_tracks(fitted.to_mut().tracks_mut(), &caps);
        }
        let mut r = w.export(dest, &fitted, o, ctl);
        if let (Ok(report), false) = (&mut r, loss.is_empty()) {
            report.notes.push(format!("Not kept by this format: {loss}"));
        }
        if ctl.cancelled() {
            return Err("cancelled".into());
        }
        if let (0, Err(e)) = (n, &r) {
            return Err(e.clone());
        }
        done.push((w.name().to_owned(), r));
    }
    Ok(done)
}

/// The audio files the library of `p` lists on `dest`, by [`store::key`]; `None` when `p` reads no library there.
fn copies(p: &Provider, dest: &Path) -> Option<HashMap<String, PathBuf>> {
    let lib = crate::plugin::library::open_with(p.name(), dest).ok()?;
    let tracks = lib.tracks(None).ok()?;
    Some(
        tracks
            .into_iter()
            .filter_map(|t| Some((store::key(&t), t.file?)))
            .collect(),
    )
}
