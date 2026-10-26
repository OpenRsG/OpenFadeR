//! Process memory sampling stays off the UI, rendering and audio threads.

mod sample;

use super::{App, ResourceState};
use parking_lot::Mutex;
use sample::{Reader, Sample};
use slint::{ComponentHandle, Timer, TimerMode};
use std::sync::{Arc, mpsc};
use std::time::Duration;

pub(super) struct Watch {
    _timer: Timer,
    // Dropping the sender wakes the worker; never join a potentially blocked driver call on the UI thread.
    _cancel: mpsc::SyncSender<()>,
}

pub(super) fn wire(app: &App) -> Watch {
    let latest = Arc::new(Mutex::new(None));
    let (cancel, cancelled) = mpsc::sync_channel::<()>(0);
    let output = latest.clone();
    if let Err(error) = std::thread::Builder::new()
        .name("resource-stats".into())
        .spawn(move || {
            let mut reader = Reader::new();
            loop {
                let sample = reader.sample();
                *output.lock() = Some(sample);
                if cancelled.recv_timeout(Duration::from_secs(1))
                    != Err(mpsc::RecvTimeoutError::Timeout)
                {
                    break;
                }
            }
        })
    {
        eprintln!("resource stats: {error}");
        *latest.lock() = Some(Sample::default());
    }

    let weak = app.as_weak();
    let mut shown: Option<Sample> = None;
    let timer = Timer::default();
    timer.start(TimerMode::Repeated, Duration::from_millis(250), move || {
        let Some(app) = weak.upgrade() else { return };
        let Some(sample) = latest.try_lock().and_then(|slot| *slot) else { return };
        let state = app.global::<ResourceState>();
        if shown.map(|old| old.ram) != Some(sample.ram) {
            state.set_ram_text(memory(sample.ram).into());
        }
        if shown.map(|old| old.vram) != Some(sample.vram) {
            state.set_vram_text(memory(sample.vram).into());
        }
        shown = Some(sample);
        let ram_detail = if sample.ram.is_some() {
            tr!("Resident RAM used by OpenFadeR, including shared pages.")
        } else {
            tr!("Process RAM usage is unavailable on this system.")
        };
        let vram_detail = if sample.vram.is_some() {
            tr!("GPU memory used by OpenFadeR, as reported by the driver. Shared allocations follow driver accounting.")
        } else {
            tr!("Per-process VRAM usage is unavailable on this GPU or driver.")
        };
        if state.get_ram_detail() != ram_detail {
            state.set_ram_detail(ram_detail.into());
        }
        if state.get_vram_detail() != vram_detail {
            state.set_vram_detail(vram_detail.into());
        }
    });
    Watch {
        _timer: timer,
        _cancel: cancel,
    }
}

fn memory(bytes: Option<u64>) -> String {
    const MIB: u64 = 1 << 20;
    const GIB: u64 = 1 << 30;
    match bytes {
        None => "—".into(),
        Some(bytes) if bytes >= GIB => format!("{:.2}GiB", bytes as f64 / GIB as f64),
        Some(bytes) => format!("{:.1}MiB", bytes as f64 / MIB as f64),
    }
}

#[cfg(test)]
mod tests {
    use super::memory;

    #[test]
    fn unavailable_is_not_zero_and_binary_units_keep_their_scale() {
        assert_eq!(memory(None), "—");
        assert_eq!(memory(Some(0)), "0.0MiB");
        assert_eq!(memory(Some(1536 * 1024)), "1.5MiB");
        assert_eq!(memory(Some((1 << 30) - (1 << 20))), "1023.0MiB");
        assert_eq!(memory(Some(1 << 30)), "1.00GiB");
        assert_eq!(memory(Some(5 << 29)), "2.50GiB");
    }
}
