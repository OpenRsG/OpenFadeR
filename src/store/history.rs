//! Play history: which track was played when, grouped by local day (`YYYY-MM-DD`), one
//! history playlist per session.

use super::{R, Sql, Store, column};
use rusqlite::params;

const APPEND: &str = "INSERT INTO history(day, key, played_at) VALUES (?1, ?2, ?3)";

impl Store {
    pub fn history_append(&self, day: &str, key: &str, played_at: i64) -> R<()> {
        self.run(APPEND, params![day, key, played_at])
    }

    /// [`Store::history_append`] for every `(key, played_at)` of `plays` on `day`, in one transaction (one commit instead
    /// of one per play).
    pub fn history_append_many(&self, day: &str, plays: &[(String, i64)]) -> R<()> {
        self.write(|tx| {
            let mut ins = tx.prepare_cached(APPEND).s()?;
            for (key, played_at) in plays {
                ins.execute(params![day, key, played_at]).s()?;
            }
            Ok(())
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

    /// Track keys played on `day`, in play order.
    pub fn history_day(&self, day: &str) -> R<Vec<String>> {
        column(
            &self.db(),
            "SELECT key FROM history WHERE day = ?1 ORDER BY played_at, id",
            [day],
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
    }
}
