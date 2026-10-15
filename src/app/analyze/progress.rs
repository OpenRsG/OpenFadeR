//! Counters of one bulk run and the progress text shown in the track info panel.

use super::Outcome;
use openfader::analysis::cache::Source;
use std::fmt::Write;

#[derive(Default)]
pub struct Progress {
    pub total: usize,
    pub done: usize,
    pub failed: usize,
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

    pub fn text(&self) -> String {
        if self.done < self.total {
            return format!("analyzing {}/{}", self.done, self.total);
        }
        let mut s = format!("analyzed {}", self.done - self.failed);
        for (n, what) in [
            (self.cached, "from cache"),
            (self.failed, "failed"),
            (self.recomputed, "stale/corrupt cache files recomputed"),
            (self.cues, "cue lists set"),
            (self.tags, "key tags written"),
            (self.tag_failed, "key tags not written"),
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
    fn progress_text_reports_cache_failures_cues_and_tags() {
        let mut p = Progress {
            total: 4,
            done: 1,
            ..Default::default()
        };
        assert_eq!(p.text(), "analyzing 1/4");
        (p.done, p.cached, p.failed, p.recomputed) = (4, 2, 1, 1);
        assert_eq!(
            p.text(),
            "analyzed 3 · 2 from cache · 1 failed · 1 stale/corrupt cache files recomputed"
        );
        (p.cues, p.tags, p.tag_failed) = (3, 2, 1);
        assert!(
            p.text()
                .ends_with("· 3 cue lists set · 2 key tags written · 1 key tags not written")
        );
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
        };
        q.record(&out(Some(Ok(true))));
        q.record(&out(Some(Ok(false))));
        assert_eq!((q.cached, q.cues, q.tags, q.tag_failed), (2, 2, 1, 0));
    }
}
