//! Volume and mute from the cards: the sliders and mute buttons act on the device through [`volume`] and patch the card in
//! place (the page is not rebuilt, so a drag is not interrupted).

use super::{AudioCard, EDITED, SHOWN, app, slint_card};
use crate::app::PrefsState;
use openfader::audio::device::volume::{self, Level, Target};
use slint::{ComponentHandle, Model, Timer, TimerMode, VecModel};
use std::{
    cell::{Cell, RefCell},
    time::{Duration, Instant},
};

/// The sound server's volume is a process call: at most one per this, the last value always follows.
const GAP: Duration = Duration::from_millis(120);

thread_local! {
    /// The last sound-server call and the value waiting for the next one.
    static LAST_CALL: Cell<Option<Instant>> = const { Cell::new(None) };
    static PENDING: RefCell<Option<(String, u32)>> = const { RefCell::new(None) };
    static TIMER: Timer = Timer::default();
}

/// Replaces the shown level of card `key` (after a volume or mute change) and patches its row, and those of the cards that
/// share its control (System default and System sound server are one sink).
fn show(key: &str, level: Level) {
    let Some(app) = app() else { return };
    SHOWN.with_borrow_mut(|shown| {
        let Some(target) = shown
            .iter()
            .find(|e| e.key == key)
            .map(|e| e.target.clone())
        else {
            return;
        };
        let model = app.global::<PrefsState>().get_audio_cards();
        let rows = model.as_any().downcast_ref::<VecModel<AudioCard>>();
        for (i, e) in shown.iter_mut().enumerate() {
            if e.key == key || (target.is_some() && e.target == target) {
                e.level = Some(level.clone());
                if let Some(rows) = rows {
                    rows.set_row_data(i, slint_card(e));
                }
            }
        }
    });
}

/// The control of card `key` and its level as shown.
fn control(key: &str) -> Option<(Target, Level)> {
    SHOWN.with_borrow(|s| {
        let e = s.iter().find(|e| e.key == key)?;
        Some((e.target.clone()?, e.level.clone()?))
    })
}

fn send(key: &str, percent: u32) {
    LAST_CALL.set(Some(Instant::now()));
    if let Some((t, cur)) = control(key)
        && let Some(l) = volume::set_percent(&t, percent, &cur)
    {
        show(key, l);
    }
}

/// The slider moved. The sound server is a `pactl` process per call, so its calls are spaced by [`GAP`] and the last value is
/// sent when the gap ends; the label follows the pointer at once either way.
pub(super) fn set_volume(key: &str, percent: u32) {
    EDITED.set(Some(Instant::now()));
    let Some((t, cur)) = control(key) else { return };
    if t == Target::Sink && LAST_CALL.get().is_some_and(|c| c.elapsed() < GAP) {
        let first = PENDING.with_borrow_mut(|p| p.replace((key.to_owned(), percent)).is_none());
        show(
            key,
            Level {
                percent,
                db: (percent > 0).then(|| volume::db_of(percent as f32)),
                ..cur
            },
        );
        if first {
            TIMER.with(|t| {
                t.start(TimerMode::SingleShot, GAP, || {
                    if let Some((k, p)) = PENDING.with_borrow_mut(Option::take) {
                        send(&k, p);
                    }
                })
            });
        }
        return;
    }
    send(key, percent);
}

pub(super) fn set_mute(key: &str, muted: bool) {
    EDITED.set(Some(Instant::now()));
    if let Some((t, cur)) = control(key)
        && let Some(l) = volume::set_muted(&t, muted, &cur)
    {
        show(key, l);
    }
}
