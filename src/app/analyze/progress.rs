//! Counters of one bulk run and the progress text shown in the track info panel.

use super::Outcome;
use openfader::analysis::cache::Source;
use std::fmt::Write;

#[derive(Default)]
pub struct Progress {
    pub total: usize,
    pub done: usize,
    pub failed: usize,
    pub skipped: usize,
    pub cancelled: usize,
    cached: usize,
    recomputed: usize,
    cues: usize,
    tags: usize,
    tag_failed: usize,
}

impl Progress {
    pub fn new(total: usize) -> Progress {
        Progress {
            total,
            ..Default::default()
        }
    }

    pub fn include(&mut self, other: &Self) {
        self.total += other.total;
        self.done += other.done;
        self.failed += other.failed;
        self.skipped += other.skipped;
        self.cancelled += other.cancelled;
        self.cached += other.cached;
        self.recomputed += other.recomputed;
        self.cues += other.cues;
        self.tags += other.tags;
        self.tag_failed += other.tag_failed;
    }

    pub fn text(&self) -> String {
        if self.done < self.total {
            return tr!("analyzing {}/{}", self.done, self.total);
        }
        let mut s = tr!(
            "analyzed {}",
            self.done - self.failed - self.skipped - self.cancelled
        );
        for (n, what) in [
            (self.cached, tr!("from cache")),
            (self.failed, tr!("failed")),
            (self.skipped, tr!("skipped (Analysis Lock)")),
            (self.cancelled, tr!("cancelled")),
            (self.recomputed, tr!("stale/corrupt cache files recomputed")),
            (self.cues, tr!("cue lists set")),
            (self.tags, tr!("key tags written")),
            (self.tag_failed, tr!("key tags not written")),
        ] {
            if n > 0 {
                let _ = write!(s, " · {n} {what}");
            }
        }
        s
    }

    pub fn record(&mut self, o: &Outcome) {
        match &o.src {
            Source::Cache => self.cached += 1,
            Source::Computed { ignored } => self.recomputed += ignored.is_some() as usize,
        }
        self.cues += o.cues as usize;
        match &o.tag {
            Some(Ok(true)) => self.tags += 1,
            Some(Err(e)) => {
                self.tag_failed += 1;
                eprintln!("analyze: {e}");
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unchanged_key_tags_are_not_counted_as_writes() {
        let mut q = Progress {
            total: 2,
            done: 2,
            ..Default::default()
        };
        let out = |tag| Outcome {
            found: (0, String::new()),
            src: Source::Cache,
            cues: true,
            tag,
            generation: 0,
            skipped: false,
            cancelled: false,
            ticket: None,
        };
        q.record(&out(Some(Ok(true))));
        q.record(&out(Some(Ok(false))));
        assert_eq!((q.cached, q.cues, q.tags, q.tag_failed), (2, 2, 1, 0));
    }
}
