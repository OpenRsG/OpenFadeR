//! Cover art by album: which cached image ([`crate::artwork`]) a track shows, and which lookups found nothing.
//!
//! One row per [`artwork_key`]: the album (`album:<artist>\u{1f}<album>`, lower case) of every track that has both tags, else
//! the track's own [`super::key`]. So a cover chosen for one track shows on every track of that album, and a track without
//! an album keeps its own. `path` is the image file in the artwork cache; an empty `path` is a **miss**: a lookup found nothing
//! at `at` (unix s), and bulk fetching does not ask again until [`MISS_TTL`] has passed. The store wins over the library's
//! own artwork; [`crate::artwork::resolve`] reads it.

use super::{scalar, Store, R};
use crate::library::Track;
use rusqlite::params;
use std::{
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

/// How long a lookup that found nothing is remembered (s): 30 days.
pub const MISS_TTL: i64 = 30 * 24 * 3600;

/// `album:<artist>\u{1f}<album>` of a track with both tags (case and surrounding space do not matter).
pub fn album_key(t: &Track) -> Option<String> {
    let (artist, album) = (t.artist.trim(), t.album.trim());
    (!artist.is_empty() && !album.is_empty()).then(|| {
        format!(
            "album:{}\u{1f}{}",
            artist.to_lowercase(),
            album.to_lowercase()
        )
    })
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

impl Store {
    /// The key `t`'s cover is stored under: its album when artist and album are both known, else the track.
    pub fn artwork_key(&self, t: &Track) -> String {
        album_key(t).unwrap_or_else(|| self.key_of(t))
    }

    /// The cover stored under `key`.
    pub fn artwork(&self, key: &str) -> R<Option<PathBuf>> {
        Ok(scalar::<String>(
            &self.db(),
            "SELECT path FROM artwork WHERE key = ?1 AND path <> ''",
            [key],
        )?
        .map(PathBuf::from))
    }

    /// Stores `path` as the cover under `key` (an earlier cover or miss is replaced); one undo step.
    pub fn set_artwork(&self, key: &str, path: &std::path::Path) -> R<()> {
        self.run(
            "INSERT INTO artwork(key, path, at) VALUES (?1, ?2, ?3)
             ON CONFLICT(key) DO UPDATE SET path = ?2, at = ?3",
            params![key, path.to_string_lossy(), now()],
        )
    }

    /// Forgets the cover (or miss) of `key`.
    pub fn clear_artwork(&self, key: &str) -> R<()> {
        self.run("DELETE FROM artwork WHERE key = ?1", [key])
    }

    /// Remembers that a lookup for `key` found nothing, unless it has a cover. Not an undo step.
    pub fn set_artwork_miss(&self, key: &str) -> R<()> {
        self.system(|| {
            self.run(
                "INSERT INTO artwork(key, path, at) VALUES (?1, '', ?2)
                 ON CONFLICT(key) DO UPDATE SET at = ?2 WHERE path = ''",
                params![key, now()],
            )
        })
    }

    /// Whether a lookup for `key` found nothing less than [`MISS_TTL`] ago.
    pub fn artwork_missed(&self, key: &str) -> R<bool> {
        Ok(scalar::<i64>(
            &self.db(),
            "SELECT at FROM artwork WHERE key = ?1 AND path = ''",
            [key],
        )?
        .is_some_and(|at| now() - at < MISS_TTL))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(artist: &str, album: &str) -> Track {
        Track {
            id: Some(7),
            artist: artist.into(),
            album: album.into(),
            ..Track::default()
        }
    }

    #[test]
    fn a_cover_belongs_to_the_album_and_a_track_without_one_has_its_own() {
        let s = Store::in_memory().unwrap();
        let key = |a, b| s.artwork_key(&track(a, b));
        assert_eq!(
            key(" Daft Punk ", "Discovery"),
            key("daft punk", "DISCOVERY")
        );
        assert_ne!(key("Daft Punk", "Discovery"), key("Daft Punk", "Homework"));
        assert_eq!(key("Daft Punk", ""), "t7");
        assert_eq!(key("", "Discovery"), "t7");
    }

    #[test]
    fn misses_expire_and_a_cover_replaces_them() {
        let s = Store::in_memory().unwrap();
        assert!(!s.artwork_missed("k").unwrap());
        s.set_artwork_miss("k").unwrap();
        assert!(s.artwork_missed("k").unwrap());
        assert_eq!(s.artwork("k").unwrap(), None, "a miss is not a cover");
        s.run("UPDATE artwork SET at = ?1", [now() - MISS_TTL - 1])
            .unwrap();
        assert!(
            !s.artwork_missed("k").unwrap(),
            "old misses are asked again"
        );
        s.set_artwork("k", std::path::Path::new("/c/1.jpg"))
            .unwrap();
        assert!(!s.artwork_missed("k").unwrap());
        s.set_artwork_miss("k").unwrap();
        assert_eq!(
            s.artwork("k").unwrap(),
            Some("/c/1.jpg".into()),
            "a later miss never replaces a cover"
        );
        s.clear_artwork("k").unwrap();
        assert_eq!(s.artwork("k").unwrap(), None);
    }
}
