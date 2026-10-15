//! Pointer and viewport: press / drag / release select and place the playhead, the wheel scrolls and zooms, the
//! playhead is followed while previewing.

use super::{CLICK_PX, State};
use openfader::edit::snap::{self, Snap};

impl State {
    /// While previewing, scroll so the playhead stays in the first 90 % of the picture.
    pub(super) fn follow(&self) {
        let open = self.open.borrow();
        let (Some(o), true) = (open.as_ref(), self.play.borrow().is_some()) else {
            return;
        };
        let Some(g) = self.geo(o) else { return };
        let (t, span) = (o.ms(self.cursor.get()), g.w as f64 / g.axis.px_per_ms);
        if t < g.axis.start_ms || t > g.axis.start_ms + 0.9 * span {
            self.start_ms.set(t - 0.1 * span);
            self.invalidate();
        }
    }

    /// Arrangement frame under logical x.
    fn frame_at(&self, x: f32) -> Option<usize> {
        let open = self.open.borrow();
        let o = open.as_ref()?;
        let g = self.geo(o)?;
        let ms = g.axis.ms(x as f64 * g.scale);
        Some(((ms * o.edit.rate() as f64 / 1000.0).round().max(0.0) as usize).min(o.edit.len()))
    }

    fn snap_to(&self, frame: usize) -> usize {
        self.open
            .borrow()
            .as_ref()
            .map_or(frame, |o| snap::snap(&o.lines, self.snap.get(), frame))
    }

    pub(super) fn press(&self, x: f32) {
        self.press.set(self.frame_at(x).map(|f| (f, x)));
        self.moved.set(false);
    }

    pub(super) fn drag(&self, x: f32) {
        let (Some((anchor, x0)), Some(now)) = (self.press.get(), self.frame_at(x)) else {
            return;
        };
        if !self.moved.get() && (x - x0).abs() < CLICK_PX {
            return;
        }
        self.moved.set(true);
        if let Some(o) = self.open.borrow().as_ref() {
            *self.sel.borrow_mut() = Some(snap::range(
                &o.lines,
                self.snap.get(),
                anchor,
                now,
                o.edit.len(),
            ));
        }
        self.invalidate();
    }

    /// A drag selects (and puts the playhead at its start); a click clears the selection and puts the playhead there.
    pub(super) fn release(&self, x: f32) {
        self.drag(x);
        let Some((anchor, _)) = self.press.take() else {
            return;
        };
        if self.moved.get() {
            if let Some(r) = self.sel() {
                self.cursor.set(r.start);
            }
        } else {
            let at = self.snap_to(anchor);
            self.cursor.set(at);
            *self.sel.borrow_mut() = None;
            if self.play.borrow().is_some() {
                self.play_from(at);
            }
        }
        self.invalidate();
    }

    /// Ctrl + wheel zooms around the pointer, the wheel scrolls.
    pub(super) fn scroll(&self, dx: f32, dy: f32, x: f32, ctrl: bool) {
        let Some(g) = self.open.borrow().as_ref().and_then(|o| self.geo(o)) else {
            return;
        };
        if ctrl {
            self.zoom_at(if dy > 0.0 { 1.25 } else { 0.8 }, x as f64 * g.scale);
        } else {
            self.start_ms
                .set(g.axis.start_ms - (dx + dy) as f64 * g.scale / g.axis.px_per_ms);
            self.invalidate();
        }
    }

    /// Zooms in by `factor` (< 1 out) keeping the time under pixel column `anchor` where it is.
    fn zoom_at(&self, factor: f64, anchor: f64) {
        let Some(g) = self.open.borrow().as_ref().and_then(|o| self.geo(o)) else {
            return;
        };
        let t = g.axis.ms(anchor);
        self.ppm.set(Some(g.axis.px_per_ms * factor));
        let ppm = self
            .open
            .borrow()
            .as_ref()
            .and_then(|o| self.geo(o))
            .map_or(g.axis.px_per_ms, |g| g.axis.px_per_ms);
        self.start_ms.set(t - anchor / ppm);
        self.invalidate();
    }

    /// ZOOM chips: +1 in, -1 out (around the centre), 0 the whole arrangement.
    pub(super) fn zoom(&self, dir: i32) {
        let Some(g) = self.open.borrow().as_ref().and_then(|o| self.geo(o)) else {
            return;
        };
        if dir == 0 {
            self.ppm.set(None);
            self.start_ms.set(0.0);
            self.invalidate();
        } else {
            self.zoom_at(if dir > 0 { 1.5 } else { 1.0 / 1.5 }, g.w as f64 / 2.0);
        }
    }

    pub(super) fn set_snap(&self, mode: i32) {
        self.snap.set(match mode {
            1 => Snap::Beat,
            2 => Snap::Bar,
            _ => Snap::Off,
        });
        self.invalidate();
    }
}
