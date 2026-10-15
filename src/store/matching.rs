//! MATCHING (EXPORT mode, 2 PLAYER / DUAL PLAYER layouts): two tracks loaded in the
//! players registered as a good combination. The pair is stored once, whichever order it was made in; the browser's Related
//! Tracks list shows the partners of the chosen track first.

use super::{R, Store, column, exists};
use rusqlite::params;

/// `(a, b)` in the order the table keeps it.
fn ordered<'a>(a: &'a str, b: &'a str) -> (&'a str, &'a str) {
    if a <= b { (a, b) } else { (b, a) }
}

impl Store {
    /// Registers `a` and `b` as related; a track is not its own partner.
    pub fn add_match(&self, a: &str, b: &str) -> R<()> {
        if a == b {
            return Ok(());
        }
        let (a, b) = ordered(a, b);
        self.run(
            "INSERT OR IGNORE INTO matching(a, b) VALUES (?1, ?2)",
            params![a, b],
        )
    }

    pub fn remove_match(&self, a: &str, b: &str) -> R<()> {
        let (a, b) = ordered(a, b);
        self.run(
            "DELETE FROM matching WHERE a = ?1 AND b = ?2",
            params![a, b],
        )
    }

    pub fn is_matched(&self, a: &str, b: &str) -> R<bool> {
        let (a, b) = ordered(a, b);
        exists(
            &self.db(),
            "SELECT 1 FROM matching WHERE a = ?1 AND b = ?2",
            params![a, b],
        )
    }

    /// The keys registered as related to `key`.
    pub fn matches_of(&self, key: &str) -> R<Vec<String>> {
        column(
            &self.db(),
            "SELECT b FROM matching WHERE a = ?1 UNION SELECT a FROM matching WHERE b = ?1 ORDER BY 1",
            [key],
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pair_is_stored_once_and_found_from_both_sides() {
        let st = Store::in_memory().unwrap();
        st.add_match("b|2", "a|1").unwrap();
        st.add_match("a|1", "b|2").unwrap(); // the same pair
        st.add_match("a|1", "c|3").unwrap();
        st.add_match("a|1", "a|1").unwrap(); // never its own partner
        assert_eq!(st.matches_of("a|1").unwrap(), ["b|2", "c|3"]);
        assert_eq!(st.matches_of("b|2").unwrap(), ["a|1"]);
        assert!(st.is_matched("b|2", "a|1").unwrap() && !st.is_matched("b|2", "c|3").unwrap());
        st.remove_match("b|2", "a|1").unwrap();
        assert_eq!(st.matches_of("a|1").unwrap(), ["c|3"]);
    }
}
