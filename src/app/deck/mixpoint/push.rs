//! Slint side of the mix: pushes the session, the points on the enlarged waveforms and the panel's pictures to
//! `MixPointState` while the panel is on screen.

use super::{
    DECKS, KEY, LANE_IN, LANE_OUT, Lane, Mix, MixDeckView, MixPointState, Mode, OVERVIEW, Probe,
    fmt_ms, pad_label, plan, probe, view,
};
use slint::{ComponentHandle, Image, Model};

impl Mix {
    /// Pushes the session, the points on the enlarged waveforms and the pictures to Slint (while the panel is on screen).
    /// The pictures and the readouts are redone only when the redraw key (every input of them) changed.
    pub(super) fn render(&self) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let st = app.global::<MixPointState>();
        if !st.get_active() {
            return;
        }
        let o = self.opts.get();
        let s = self.s.borrow();
        let probes: [Probe; DECKS] = std::array::from_fn(|d| probe(&self.decks[d]));
        for (d, p) in probes.iter().enumerate() {
            let x = |ms: u32| (0.5 + (f64::from(ms) - p.pos) / p.span) as f32;
            let silent = s.silent_until(d);
            let row = MixDeckView {
                loaded: p.ready,
                state: s.deck_state(d) as i32,
                sel_out: s.out == Some(d),
                sel_in: s.inn == Some(d),
                out_set: s.out_ms[d].is_some(),
                out_x: s.out_ms[d].map_or(0.0, x),
                in_set: s.in_ms[d].is_some(),
                in_x: s.in_ms[d].map_or(0.0, x),
                silent_set: silent.is_some(),
                silent_x: silent.map_or(0.0, x),
                linked: s.linked(d),
            };
            if self.model.row_data(d).is_none_or(|r| r != row) {
                self.model.set_row_data(d, row);
            }
        }
        st.set_mode(i32::from(s.mode == Mode::Out));
        st.set_link_state(s.link_state() as i32);
        st.set_silent(s.inn.is_some_and(|i| s.silent[i]));
        st.set_silent_edit(s.silent_edit);
        // The lanes: a deck without a point sits on its playhead.
        let lane_of = |d: Option<usize>, mode: Mode| {
            d.map(|d| {
                let p = &probes[d];
                let point = s.point(d, mode).map_or(p.pos, f64::from);
                let lane = Lane {
                    point,
                    duration: p.duration,
                    rate: p.rate,
                };
                (d, lane)
            })
        };
        let (out, inn) = (lane_of(s.out, Mode::Out), lane_of(s.inn, Mode::In));
        let lanes = || {
            [out, inn]
                .into_iter()
                .flatten()
                .map(|(d, l)| (l, probes[d].pos))
        };
        let ext = plan::extent(lanes().map(|(l, _)| l));
        self.ext.set(ext);
        let mut v = self.view.get();
        v.clamp(ext);
        self.view.set(v);
        let scale = app.window().scale_factor();
        let phys =
            |(w, h): (f32, f32)| ((w * scale).round() as usize, (h * scale).round() as usize);
        let sizes = self.sizes.get().map(phys);
        let mut key = [0u64; KEY];
        let mut fields = key.iter_mut();
        let mut put = |x: u64| *fields.next().expect("KEY covers every field") = x;
        for lane in [out, inn] {
            let (d, l) = lane.unwrap_or((
                usize::MAX,
                Lane {
                    point: 0.0,
                    duration: 0.0,
                    rate: 1.0,
                },
            ));
            let p = probes.get(d);
            put(d as u64);
            put(l.point.to_bits());
            put(l.rate.to_bits());
            put(l.duration.to_bits());
            put(p.map_or(0, |p| p.pos.to_bits()));
            put(p.map_or(0, |p| u64::from(p.serial) << 1 | u64::from(p.ready)));
            put(p.map_or(0, |p| u64::from(p.cue_ms)));
            put(self
                .decks
                .get(d)
                .map_or(0, |dk| dk.loaded.borrow().analysis.cues.len() as u64));
        }
        put(v.centre.to_bits());
        put(v.span.to_bits());
        for (w, h) in sizes {
            put((w as u64) << 32 | h as u64);
        }
        put(u64::from(o.view.time)
            | u64::from(o.view.grids) << 1
            | u64::from(o.view.grids_enlarged_only) << 2);
        put(s
            .out_ms
            .iter()
            .chain(&s.in_ms)
            .fold(0u64, |a, m| a.rotate_left(7) ^ m.map_or(0, u64::from)));
        put(s
            .link
            .iter()
            .fold(0u64, |a, l| a << 3 | l.map_or(0, |d| d as u64 + 1)));
        put(s.silent_end.iter().enumerate().fold(0u64, |a, (d, m)| {
            a.rotate_left(5) ^ s.silent_until(d).map_or(0, u64::from) ^ m.map_or(0, u64::from)
        }));
        if key == self.drawn.get() {
            return;
        }
        self.drawn.set(key);
        let readout = |d: Option<usize>, mode: Mode| {
            d.and_then(|d| {
                s.point(d, mode)
                    .map(|ms| format!("{} {}", pad_label(d), fmt_ms(ms)))
            })
            .unwrap_or_default()
        };
        st.set_out_text(readout(s.out, Mode::Out).into());
        st.set_in_text(readout(s.inn, Mode::In).into());
        let lane_image = |lane: Option<(usize, Lane)>, mode: Mode, (w, h): (usize, usize)| {
            let Some((d, lane)) = lane.filter(|_| w > 0 && h > 0) else {
                return Image::default();
            };
            let l = self.decks[d].loaded.borrow();
            let src = view::Src {
                wave: l.detail.as_ref(),
                beats: &l.analysis.beats,
                cues: &l.analysis.cues,
                lane,
                pos: probes[d].pos,
            };
            let marks = view::Marks {
                point: s.point(d, mode).is_some(),
                linked: s.linked(d),
                silent_end: if mode == Mode::In {
                    s.silent_until(d)
                } else {
                    None
                },
            };
            view::lane(&src, v, o.view, marks, w, h)
        };
        st.set_lane_out(lane_image(out, Mode::Out, sizes[LANE_OUT]));
        st.set_lane_in(lane_image(inn, Mode::In, sizes[LANE_IN]));
        let (w, h) = sizes[OVERVIEW];
        let lanes: Vec<(Lane, f64)> = lanes().collect();
        st.set_overview(if w > 0 && h > 0 && !lanes.is_empty() {
            view::overview(&lanes, ext, v, w, h)
        } else {
            Image::default()
        });
    }
}
