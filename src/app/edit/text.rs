//! The texts, flags and status line of `EditState`.

use super::State;
use crate::app::EditState;
use openfader::edit::snap::{Line, Snap};
use slint::ComponentHandle;

/// `m:ss.mmm` of a frame.
fn clock(frame: usize, rate: u32) -> String {
    let ms = frame as u64 * 1000 / rate.max(1) as u64;
    format!("{}:{:02}.{:03}", ms / 60_000, ms / 1000 % 60, ms % 1000)
}

/// Bar and beat (1-based) the position is in; `None` before the first beat of the grid.
fn bar_beat(lines: &[Line], frame: usize) -> Option<(usize, usize)> {
    let upto = &lines[..lines.partition_point(|l| l.frame <= frame)];
    upto.last()?;
    let bars = upto.iter().filter(|l| l.bar).count().max(1);
    Some((bars, upto.iter().rev().take_while(|l| !l.bar).count() + 1))
}

impl State {
    pub(super) fn push(&self) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let g = app.global::<EditState>();
        let open = self.open.borrow();
        let (title, pos, sel, len) = match open.as_ref() {
            None => (
                "no track open".to_string(),
                "—".to_string(),
                String::new(),
                String::new(),
            ),
            Some(o) => {
                let rate = o.edit.rate();
                let c = self.cursor.get();
                let pos = format!(
                    "{}{}",
                    clock(c, rate),
                    bar_beat(&o.lines, c)
                        .map_or(String::new(), |(b, n)| format!("   bar {b} beat {n}"))
                );
                let sel =
                    self.sel()
                        .map_or("no selection: drag on the waveform".to_string(), |r| {
                            let beats = o.lines.iter().filter(|l| r.contains(&l.frame)).count();
                            format!(
                                "SELECTION {} → {}   {:.2} s   {beats} beats",
                                clock(r.start, rate),
                                clock(r.end, rate),
                                r.len() as f64 / rate as f64
                            )
                        });
                let src = o.edit.pcm().samples.len() / 2;
                let len = format!(
                    "LENGTH {} (source {}){}   CLIPBOARD {:.2} s",
                    clock(o.edit.len(), rate),
                    clock(src, rate),
                    if o.edit.is_edited() { "   EDITED" } else { "" },
                    o.edit.clip_len() as f64 / rate as f64,
                );
                let t = &o.track;
                (
                    if t.artist.is_empty() {
                        t.title.clone()
                    } else {
                        format!("{} — {}", t.artist, t.title)
                    },
                    pos,
                    sel,
                    len,
                )
            }
        };
        g.set_title(title.into());
        g.set_pos_text(pos.into());
        g.set_sel_text(sel.into());
        g.set_len_text(len.into());
        g.set_status(self.status.borrow().as_str().into());
        g.set_loaded(open.is_some());
        g.set_busy(self.loading.borrow().is_some());
        g.set_playing(self.play.borrow().is_some());
        g.set_has_sel(self.sel.borrow().is_some());
        g.set_has_clip(open.as_ref().is_some_and(|o| o.edit.clip_len() > 0));
        g.set_can_undo(open.as_ref().is_some_and(|o| o.edit.can_undo()));
        g.set_can_redo(open.as_ref().is_some_and(|o| o.edit.can_redo()));
        g.set_edited(open.as_ref().is_some_and(|o| o.edit.is_edited()));
        g.set_snap(match self.snap.get() {
            Snap::Off => 0,
            Snap::Beat => 1,
            Snap::Bar => 2,
        });
        g.set_deck_count(crate::app::deck::active() as i32);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(frame: usize, bar: bool) -> Line {
        Line { frame, bar }
    }

    #[test]
    fn bar_and_beat_of_a_position() {
        let l = [
            line(100, true),
            line(200, false),
            line(300, false),
            line(400, false),
            line(500, true),
            line(600, false),
        ];
        assert_eq!(bar_beat(&l, 50), None, "before the first beat");
        assert_eq!(bar_beat(&l, 100), Some((1, 1)));
        assert_eq!(bar_beat(&l, 399), Some((1, 3)));
        assert_eq!(bar_beat(&l, 400), Some((1, 4)));
        assert_eq!(bar_beat(&l, 550), Some((2, 1)));
        assert_eq!(bar_beat(&l, 9999), Some((2, 2)));
    }

    #[test]
    fn clock_text() {
        assert_eq!(clock(44100 * 61 + 441, 44100), "1:01.010");
    }
}
