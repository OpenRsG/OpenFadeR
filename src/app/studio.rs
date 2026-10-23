//! The studio workspace's top-bar clock ↔ ui/studio-state.slint (global `StudioState`): the local time as `HH:MM`, refreshed while the
//! app runs. The rest of the workspace (ui/studio/) reads the shared globals of the decks, mixer, FX, browser and AUTOMIX.

use super::{App, StudioState};
use chrono::Timelike;
use slint::{ComponentHandle, Timer, TimerMode};
use std::time::Duration;

/// Often enough that the minute changes within a few seconds of the wall clock.
const REFRESH: Duration = Duration::from_secs(5);

/// `HH:MM`, 24 hours, zero-padded.
fn clock_text(t: impl Timelike) -> String {
    format!("{:02}:{:02}", t.hour(), t.minute())
}

/// Shows the time now and keeps it current; the returned timer lives until the window closes.
pub fn wire(app: &App) -> Timer {
    let weak = app.as_weak();
    let show = move || {
        if let Some(app) = weak.upgrade() {
            app.global::<StudioState>()
                .set_clock(clock_text(chrono::Local::now()).into());
        }
    };
    show();
    let timer = Timer::default();
    timer.start(TimerMode::Repeated, REFRESH, show);
    timer
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveTime;

    #[test]
    fn the_clock_is_twenty_four_hours_with_padding() {
        let at = |h, m| clock_text(NaiveTime::from_hms_opt(h, m, 59).unwrap());
        assert_eq!(at(7, 5), "07:05");
        assert_eq!(at(0, 0), "00:00");
        assert_eq!(at(23, 59), "23:59");
    }
}
