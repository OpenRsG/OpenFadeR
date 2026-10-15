//! Toast ↔ ui/toast.slint (global `ToastState`): `show(&app, "text")` pops a short notification near the top centre
//! that hides itself after [`SHOW_FOR`]. A message that arrives while one is still up goes under it (three lines at
//! most, oldest dropped) and restarts the clock, so "controller connected" and "Audio output: controller" read together.

use super::{App, ToastState};
use slint::{ComponentHandle, Timer, TimerMode};
use std::time::Duration;

const SHOW_FOR: Duration = Duration::from_secs(3);
const LINES: usize = 3;

thread_local! {
    /// Restarted by every toast, so only the last one's three seconds count. UI thread only, like every Slint call.
    static HIDE: Timer = Timer::default();
}

/// `current` (what is up now, "" when nothing is) with `new` underneath, keeping the last [`LINES`] lines.
fn stacked(current: &str, new: &str) -> String {
    let lines: Vec<&str> = current
        .lines()
        .filter(|l| !l.is_empty())
        .chain(std::iter::once(new))
        .collect();
    lines[lines.len().saturating_sub(LINES)..].join("\n")
}

/// Shows `text` (UI thread only).
pub fn show(app: &App, text: &str) {
    let s = app.global::<ToastState>();
    let current = if s.get_shown() {
        s.get_text()
    } else {
        "".into()
    };
    s.set_text(stacked(&current, text).into());
    s.set_shown(true);
    let weak = app.as_weak();
    HIDE.with(|t| {
        t.start(TimerMode::SingleShot, SHOW_FOR, move || {
            if let Some(app) = weak.upgrade() {
                app.global::<ToastState>().set_shown(false);
            }
        })
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_stack_and_keep_the_newest_three_lines() {
        assert_eq!(stacked("", "controller connected"), "controller connected");
        assert_eq!(
            stacked("controller connected", "Audio output: controller"),
            "controller connected\nAudio output: controller"
        );
        assert_eq!(stacked("a\nb\nc", "d"), "b\nc\nd");
    }
}
