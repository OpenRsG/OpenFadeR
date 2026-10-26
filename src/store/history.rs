//! Play history: which track was played when, grouped by local day (`YYYY-MM-DD`), one
//! history playlist per session.

use super::{R, Sql, Store, column, query_all};
use rusqlite::params;

const APPEND: &str = "INSERT INTO history(day, key, played_at) VALUES (?1, ?2, ?3)";

impl Store {
    /// Plays are not user edits, so never an undo step (undoing a track's removal brings its plays back, see [`Store::system`]).
    pub fn history_append(&self, day: &str, key: &str, played_at: i64) -> R<()> {
        self.system(|| self.run(APPEND, params![day, key, played_at]))
    }

    /// [`Store::history_append`] for every `(key, played_at)` of `plays` on `day`, in one transaction (one commit instead
    /// of one per play).
    pub fn history_append_many(&self, day: &str, plays: &[(String, i64)]) -> R<()> {
        self.system(|| {
            self.write(|tx| {
                let mut ins = tx.prepare_cached(APPEND).s()?;
                for (key, played_at) in plays {
                    ins.execute(params![day, key, played_at]).s()?;
                }
                Ok(())
            })
        })
    }

    /// Days with plays, newest first.
    pub fn history_days(&self) -> R<Vec<String>> {
        column(
            &self.db(),
            "SELECT DISTINCT day FROM history ORDER BY day DESC",
            [],
        )
    }

    /// [`Store::history_days`] with the number of plays of each, in one query.
    pub fn history_day_counts(&self) -> R<Vec<(String, usize)>> {
        query_all(
            &self.db(),
            "SELECT day, COUNT(*) FROM history GROUP BY day ORDER BY day DESC",
            [],
            |r| Ok((r.get(0)?, r.get::<_, i64>(1)? as usize)),
        )
    }

    /// Track keys played on `day`, in play order.
    pub fn history_day(&self, day: &str) -> R<Vec<String>> {
        column(
            &self.db(),
            "SELECT key FROM history WHERE day = ?1 ORDER BY played_at, id",
            [day],
        )
    }

    /// [`Store::history_day`] with each play's time (Unix seconds).
    pub fn history_day_plays(&self, day: &str) -> R<Vec<(String, i64)>> {
        query_all(
            &self.db(),
            "SELECT key, played_at FROM history WHERE day = ?1 ORDER BY played_at, id",
            [day],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_batch_orders_with_single_appends_by_play_time() {
        let s = Store::in_memory().unwrap();
        s.history_append("2026-10-13", "a|1", 5).unwrap();
        s.history_append_many("2026-10-13", &[("c|3".into(), 30), ("b|2".into(), 20)])
            .unwrap();
        assert_eq!(s.history_day("2026-10-13").unwrap(), ["a|1", "b|2", "c|3"]);
        s.history_append("2026-10-14", "a|1", 50).unwrap();
        assert_eq!(
            s.history_day_counts().unwrap(),
            [("2026-10-14".to_owned(), 1), ("2026-10-13".to_owned(), 3)]
        );
        assert_eq!(
            s.history_day_plays("2026-10-13").unwrap()[1],
            ("b|2".to_owned(), 20)
        );
    }
}
