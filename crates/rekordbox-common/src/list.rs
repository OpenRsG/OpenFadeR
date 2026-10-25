//! What the device and database library readers share: where a track's files are, and the "Histories" playlist folder.

use openfader_plugin::library::PlaylistRow;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// `rel` (as the libraries store it, leading `/` allowed) under `root`; empty/absent → `None`.
pub fn under(root: &Path, rel: Option<&str>) -> Option<PathBuf> {
    rel.filter(|p| !p.is_empty())
        .map(|p| crate::on_device(root, p))
}

/// Like [`under`], but only if the file exists.
pub fn existing(root: &Path, rel: Option<&str>) -> Option<PathBuf> {
    under(root, rel).filter(|p| p.is_file())
}

/// [`under`] as a track's `analysis_ref`: the absolute path of the track's analysis file.
pub fn analysis_ref(root: &Path, rel: Option<&str>) -> Option<String> {
    under(root, rel).map(|p| p.to_string_lossy().into_owned())
}

/// Id of the synthetic "Histories" folder; each history playlist's id is this followed by the
/// source's own id (the SDK's convention for history playlists).
pub const HISTORY: &str = "history:";

/// A history playlist's id as its source stores it; `None` for any other playlist id.
pub fn history_raw(id: &str) -> Option<&str> {
    id.strip_prefix(HISTORY)
}

/// Appends the source's history rows (raw ids; `parent` a raw id of a history folder or `None`)
/// under a top-level "Histories" folder that sorts after every other top-level entry. Adds
/// nothing when the source has no histories.
pub fn add_histories(rows: &mut Vec<PlaylistRow>, hist: Vec<PlaylistRow>) {
    if hist.is_empty() {
        return;
    }
    let folders: HashSet<String> = hist
        .iter()
        .filter(|h| h.is_folder)
        .map(|h| h.id.clone())
        .collect();
    rows.push(PlaylistRow {
        id: HISTORY.into(),
        name: "Histories".into(),
        is_folder: true,
        parent: None,
        order: i64::MAX,
    });
    rows.extend(hist.into_iter().map(|h| {
        PlaylistRow {
            id: format!("{HISTORY}{}", h.id),
            parent: Some(
                h.parent
                    .filter(|p| folders.contains(p))
                    .map_or_else(|| HISTORY.into(), |p| format!("{HISTORY}{p}")),
            ),
            ..h
        }
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, folder: bool, parent: Option<&str>) -> PlaylistRow {
        PlaylistRow {
            id: id.into(),
            name: id.into(),
            is_folder: folder,
            parent: parent.map(Into::into),
            order: 0,
        }
    }

    #[test]
    fn histories_go_under_their_own_folder() {
        let mut all = vec![row("1", false, None)];
        add_histories(&mut all, Vec::new());
        assert_eq!(all.len(), 1); // none: no folder
        add_histories(
            &mut all,
            vec![
                row("1", true, None),
                row("2", false, Some("1")),
                row("3", false, Some("9")),
            ],
        );
        let shape: Vec<_> = all
            .iter()
            .map(|r| (r.id.as_str(), r.parent.as_deref()))
            .collect();
        assert_eq!(
            shape,
            [
                ("1", None),
                ("history:", None),
                ("history:1", Some("history:")),
                ("history:2", Some("history:1")),
                ("history:3", Some("history:")),
            ]
        );
        assert_eq!(history_raw("history:3"), Some("3"));
        assert_eq!(history_raw("3"), None);
    }

    #[test]
    fn paths_under_root() {
        let root = Path::new("/r");
        assert_eq!(
            under(root, Some("/A/b.mp3")),
            Some(PathBuf::from("/r/A/b.mp3"))
        );
        assert_eq!(under(root, Some("")), None);
        assert_eq!(existing(root, Some("/nope")), None);
        assert_eq!(analysis_ref(root, None), None);
    }
}
