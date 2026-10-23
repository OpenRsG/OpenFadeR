//! The NEURAL key detector's model on the app side: choosing NEURAL (or starting with it chosen) while the model is missing
//! downloads it at once ([`neural::fetch`], size and sha256 checked) with its progress on the track info panel's status line, then
//! sets the analysis options again so the detector runs from the next analysis on. Offline or a wrong file: a toast says the
//! analysis stays CLASSIC, and a run started meanwhile says so too ([`note`]). The preference is not changed.

use super::{set_options, App};
use crate::app::{prefs, toast, trackinfo};
use slint::{ComponentHandle, Timer, TimerMode};
use std::{cell::Cell, time::Duration};

#[cfg(feature = "stems")]
use openfader::analysis::key::neural;
use openfader::analysis::key::neural_ready;

thread_local! {
    static POLL: Timer = Timer::default();
    static RUNNING: Cell<bool> = const { Cell::new(false) };
}

fn chosen() -> bool {
    use crate::app::prefs::analysis::KeyDetector;
    prefs::with(|p| p.analysis.part_key && p.analysis.key_detector == KeyDetector::Neural)
}

/// Starts the download unless the model is there, NEURAL is not chosen or a download of ours runs.
#[cfg(feature = "stems")]
fn start(app: &App) {
    if !chosen() || neural_ready() || RUNNING.replace(true) {
        return;
    }
    let weak = app.as_weak();
    POLL.with(|t| {
        t.start(TimerMode::Repeated, Duration::from_millis(250), move || {
            let (Some(app), Some(got)) = (weak.upgrade(), neural::downloading()) else {
                return;
            };
            let percent = got * 100 / neural::SIZE;
            trackinfo::set_progress(
                &app,
                &tr!("KEY DETECTOR: downloading the NEURAL model {}%", percent),
            );
        })
    });
    let weak = app.as_weak();
    std::thread::spawn(move || {
        let r = neural::fetch(&|_| {});
        let _ = weak.upgrade_in_event_loop(move |app| {
            RUNNING.set(false);
            POLL.with(Timer::stop);
            match r {
                Ok(()) => {
                    set_options(&prefs::get());
                    trackinfo::set_progress(&app, "");
                    toast::show(&app, tr!("NEURAL key model ready"));
                }
                Err(e) => {
                    eprintln!("analyze: {e}");
                    trackinfo::set_progress(&app, &e);
                    toast::show(
                        &app,
                        tr!("NEURAL key model unavailable: analysing with CLASSIC"),
                    );
                }
            }
        });
    });
}

#[cfg(not(feature = "stems"))]
fn start(_: &App) {}

/// Called as a run starts: says so when NEURAL is chosen but this run uses CLASSIC.
pub fn note(app: &App) {
    if chosen() && !neural_ready() {
        toast::show(
            app,
            if RUNNING.get() {
                tr!("NEURAL key model is still downloading: this run uses CLASSIC")
            } else {
                tr!("NEURAL key model unavailable: this run uses CLASSIC")
            },
        );
    }
}

/// Fetches the model when NEURAL is chosen, now and whenever the setting changes to it.
pub fn wire(app: &App) {
    let was = Cell::new(chosen());
    let weak = app.as_weak();
    prefs::on_change(move |_| {
        // prefs change on every window resize: act on the switch only
        let now = chosen();
        if was.replace(now) != now && now {
            if let Some(app) = weak.upgrade() {
                start(&app);
            }
        }
    });
    start(app);
}
