//! Output reservations outlive the job: attached audio and waveform consumers retain the same lease.

use crate::audio::{Grid, STEM_COUNT};
use std::mem::size_of;
use std::sync::atomic::{
    AtomicBool, AtomicUsize,
    Ordering::{AcqRel, Acquire, Relaxed},
};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

pub(super) static OUTPUT: LazyLock<Arc<Budget>> = LazyLock::new(|| {
    // Leave seven eighths for decoded tracks, inference workspaces and the UI, including inside cgroups.
    Arc::new(Budget::new(
        (crate::memory::total_bytes().unwrap_or(2 << 30) / 8).min(usize::MAX as u64) as usize,
    ))
});

pub(crate) struct Lease {
    budget: Arc<Budget>,
    bytes: usize,
}

pub(super) struct Budget {
    limit: usize,
    used: AtomicUsize,
}

impl Budget {
    fn new(limit: usize) -> Self {
        Self {
            limit,
            used: AtomicUsize::new(0),
        }
    }

    fn try_reserve(self: &Arc<Self>, bytes: usize) -> Option<Lease> {
        self.used
            .try_update(AcqRel, Acquire, |used| {
                used.checked_add(bytes).filter(|&total| total <= self.limit)
            })
            .ok()
            .map(|_| Lease {
                budget: self.clone(),
                bytes,
            })
    }

    fn acquire(
        self: &Arc<Self>,
        bytes: usize,
        cancel: &AtomicBool,
        waiting: impl Fn(),
    ) -> Result<Lease, String> {
        if bytes > self.limit {
            return Err(format!(
                "STEMS output needs {} MiB, memory budget is {} MiB; use a shorter track or a device with more memory",
                bytes.div_ceil(1 << 20),
                self.limit >> 20
            ));
        }
        loop {
            if cancel.load(Relaxed) {
                return Err("cancelled".into());
            }
            // Cgroup headroom includes reclaimable file cache; admit against owned outputs, not that counter.
            if let Some(lease) = self.try_reserve(bytes) {
                return Ok(lease);
            }
            waiting();
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        // The last audio consumer may release this: never take a lock or perform I/O here.
        self.budget.used.fetch_sub(self.bytes, AcqRel);
    }
}

pub(super) fn reserve(
    frames: usize,
    grid: Grid,
    cancel: &AtomicBool,
    waiting: impl Fn(),
) -> Result<Lease, String> {
    OUTPUT.acquire(bytes(frames, grid)?, cancel, waiting)
}

fn bytes(frames: usize, grid: Grid) -> Result<usize, String> {
    let samples = frames.checked_mul(2 * STEM_COUNT * size_of::<i16>());
    let flags = grid
        .count(frames)
        .checked_mul(size_of::<AtomicBool>() + size_of::<parking_lot::Mutex<bool>>());
    samples
        .zip(flags)
        .and_then(|(samples, flags)| samples.checked_add(flags))
        .ok_or_else(|| "STEMS output size overflow".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::Stems;
    use std::sync::mpsc;

    #[test]
    fn shared_audio_consumers_keep_the_reservation_until_the_last_drop() {
        let grid = Grid {
            len: 64,
            overlap: 8,
        };
        let amount = bytes(128, grid).unwrap();
        let budget = Arc::new(Budget::new(amount));
        let lease = budget.try_reserve(amount).unwrap();
        let stems = Arc::new(Stems::new(128, grid).with_memory(lease));
        let other_deck = stems.clone();
        assert!(budget.try_reserve(amount).is_none());
        drop(stems);
        assert!(
            budget.try_reserve(amount).is_none(),
            "the other deck still owns the output"
        );
        drop(other_deck);
        assert_eq!(budget.used.load(Acquire), 0);
        assert!(budget.try_reserve(amount).is_some());
    }

    #[test]
    fn waiting_is_cancellable_and_release_admits_the_next_output() {
        let budget = Arc::new(Budget::new(64));
        let held = budget.try_reserve(64).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::sync_channel(1);
        let worker = {
            let (budget, cancel) = (budget.clone(), cancel.clone());
            std::thread::spawn(move || {
                budget.acquire(32, &cancel, || {
                    let _ = tx.try_send(());
                })
            })
        };
        rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(budget.used.load(Acquire), 64);
        cancel.store(true, Relaxed);
        assert!(worker.join().unwrap().is_err());
        assert_eq!(budget.used.load(Acquire), 64);
        drop(held);
        let admitted = budget
            .acquire(64, &AtomicBool::new(false), || panic!("budget released"))
            .unwrap();
        assert_eq!(budget.used.load(Acquire), 64);
        drop(admitted);
    }

    #[test]
    fn oversized_outputs_fail_without_reserving_and_size_arithmetic_is_checked() {
        let budget = Arc::new(Budget::new(64));
        assert!(
            budget
                .acquire(65, &AtomicBool::new(false), || panic!(
                    "oversized must not wait"
                ))
                .is_err()
        );
        assert_eq!(budget.used.load(Acquire), 0);
        assert!(
            bytes(
                usize::MAX,
                Grid {
                    len: 64,
                    overlap: 8
                }
            )
            .is_err()
        );
    }
}
