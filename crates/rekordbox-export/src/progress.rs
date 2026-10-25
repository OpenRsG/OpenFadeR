//! Progress and cancel of a running job towards the host (`host::progress`): a phase text and `done` of `total` steps. The host
//! learns a step at most every [`STEP`] (a call crosses the ABI), and always at a phase change and at the end of one.

use openfader_plugin::library::host;
use std::time::{Duration, Instant};

const STEP: Duration = Duration::from_millis(50);

pub struct Progress {
    phase: &'static str,
    total: usize,
    last: Option<Instant>,
}

impl Progress {
    pub fn new() -> Self {
        Self {
            phase: "",
            total: 0,
            last: None,
        }
    }

    /// Starts a phase of `total` steps and reports it at once; `Err("cancelled")` when the user cancelled the job.
    pub fn phase(&mut self, phase: &'static str, total: usize) -> Result<(), String> {
        (self.phase, self.total) = (phase, total);
        self.report(0)
    }

    /// `done` steps of the phase are done; `Err("cancelled")` when the user cancelled the job.
    pub fn tick(&mut self, done: usize) -> Result<(), String> {
        if done < self.total && self.last.is_some_and(|t| t.elapsed() < STEP) {
            return Ok(());
        }
        self.report(done)
    }

    fn report(&mut self, done: usize) -> Result<(), String> {
        self.last = Some(Instant::now());
        if host::progress(self.phase, done, self.total) {
            Err("cancelled".into())
        } else {
            Ok(())
        }
    }
}
