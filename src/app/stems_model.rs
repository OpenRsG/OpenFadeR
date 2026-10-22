//! The STEMS model download ↔ ui/stems-model.slint (`StemsModelState`): switching STEMS on while the model is missing
//! downloads it at once (not at the first track loaded), with a progress card until it is in place. A failure stays on
//! the card with RETRY. A separation that needs the model meanwhile waits for this download ([`model::fetch`]).

use super::{prefs, toast, App, StemsModelState};
use openfader::stems::model;
use slint::{ComponentHandle, Timer, TimerMode};
use std::{cell::Cell, time::Duration};

thread_local! {
    static POLL: Timer = Timer::default();
    static RUNNING: Cell<bool> = const { Cell::new(false) };
}

const MB: u64 = 1_000_000;

/// Starts the download unless the model is there or a download of ours runs.
pub fn start(app: &App) {
    if model::usable() || RUNNING.replace(true) {
        return;
    }
    let s = app.global::<StemsModelState>();
    s.set_failed(false);
    s.set_progress(0.0);
    s.set_text(tr!("0 of {} MB", model::SIZE / MB).into());
    s.set_shown(true);
    let weak = app.as_weak();
    POLL.with(|t| {
        t.start(TimerMode::Repeated, Duration::from_millis(250), move || {
            let (Some(app), Some(got)) = (weak.upgrade(), model::downloading()) else {
                return;
            };
            let s = app.global::<StemsModelState>();
            s.set_progress(got as f32 / model::SIZE as f32);
            s.set_text(tr!("{} of {} MB", got / MB, model::SIZE / MB).into());
        })
    });
    let weak = app.as_weak();
    std::thread::spawn(move || {
        let r = model::fetch(&|_| {});
        let _ = weak.upgrade_in_event_loop(move |app| {
            RUNNING.set(false);
            POLL.with(Timer::stop);
            let s = app.global::<StemsModelState>();
            match r {
                Ok(()) => {
                    s.set_shown(false);
                    toast::show(&app, tr!("STEMS model ready"));
                }
                Err(e) => {
                    s.set_failed(true);
                    s.set_text(e.into());
                }
            }
        });
    });
}

pub fn wire(app: &App) {
    let s = app.global::<StemsModelState>();
    let w = app.as_weak();
    s.on_retry(move || {
        if let Some(app) = w.upgrade() {
            start(&app);
        }
    });
    let w = app.as_weak();
    s.on_close(move || {
        if let Some(app) = w.upgrade() {
            app.global::<StemsModelState>().set_shown(false);
        }
    });
    // Only when STEMS turns on: prefs change on every window resize, and a failed download waits for RETRY.
    let on = prefs::with(|p| p.stems);
    let was = Cell::new(on);
    let w = app.as_weak();
    prefs::on_change(move |p| {
        if p.stems && !was.replace(p.stems) {
            if let Some(app) = w.upgrade() {
                start(&app);
            }
        }
        was.set(p.stems);
    });
    if on {
        start(app); // STEMS was left on without the model (an earlier download failed or was cut off)
    }
}
