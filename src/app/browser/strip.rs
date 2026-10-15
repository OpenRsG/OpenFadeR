//! The Preview column of the track table: one waveform strip per row, drawn when the row first shows (the table calls
//! `BrowserState.preview-wanted`). With Preferences > View > Layout > Browser panel > **Display Cue Markers on Preview**
//! the strip also shows the track's memory and hot cues (the library's, the store's edits on top), and a click near a cue
//! starts the preview from that cue instead of the clicked position.
//!
//! The waveform comes from the track's own analysis (analysis files), else the analysis cache; a track with neither shows a blank
//! strip (cues still drawn). Drawn strips are kept per track file ([`Strips::image`], at most [`CACHE_MAX`]) and reused while
//! the track's store edits (every cue edit writes there) and the detected-analysis version are unchanged, so sorting,
//! filtering and scrolling read no analysis again. ponytail: a track's library cues or analysis files changing under a
//! running app are not noticed (sources are read-only); the cache is cleared whole when full, add LRU if that shows.

use super::View;
use crate::app::{
    App, BrowserState, analyze, prefs,
    render::{Axis, Canvas, cue_color, cue_letter, overview},
};
use openfader::{
    analysis::{self, Analysis},
    library::{Library, Track},
    store::{self, Edits},
};
use slint::{ComponentHandle, Image, Model, VecModel};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    path::PathBuf,
    rc::Rc,
};

/// Logical size of a strip; ui/browser-table.slint draws it 1:1.
const W: usize = 110;
const H: usize = 20;
/// A click this close (logical px) to a cue marker counts as a click on the cue.
const HIT_PX: f64 = 6.0;

/// Most strips kept between row creations (≈9 KB each at scale 1); a full cache is cleared.
const CACHE_MAX: usize = 512;

/// A drawn strip and what it was drawn from.
struct Drawn {
    /// The store's edits of the track then (cues and grid are what the strip shows).
    edits: Edits,
    /// [`analyze::Detected::version`] then: bulk analysis may have added a waveform.
    detected: u64,
    image: Image,
}

/// The strips shown now, one image per visible row (`BrowserState.previews`), and the strips drawn lately by track file,
/// strip scale and the cue-markers setting, so a sort, filter or scroll does not read the analysis again.
pub(super) struct Strips {
    model: Rc<VecModel<Image>>,
    cache: RefCell<HashMap<(PathBuf, usize, bool), Drawn>>,
}

impl Strips {
    pub fn new(app: &App) -> Self {
        let model = Rc::new(VecModel::default());
        app.global::<BrowserState>()
            .set_previews(model.clone().into());
        Self {
            model,
            cache: RefCell::default(),
        }
    }

    /// The rows changed (new list, sort, filter): every strip is blank until its row asks for it again.
    pub fn reset(&self, rows: usize) {
        self.model.set_vec(vec![Image::default(); rows]);
    }

    /// `t`'s strip: the cached drawing while the store's edits of the track and the detected analysis are the ones it was
    /// drawn from (so a cue just set on a deck shows at once), else drawn from the analysis (and cached).
    fn image(&self, lib: &dyn Library, t: &Track, scale: usize) -> Image {
        let cues = prefs::with(|p| p.view.panel_preview_cues);
        let Some(file) = &t.file else {
            let a = analysis_of(lib, t);
            return draw(&a, duration_ms(t, &a), cues, scale);
        };
        let edits = store::global().edits(&store::key(t)).unwrap_or_default();
        let detected = analyze::detected().version();
        let key = (file.clone(), scale, cues);
        let mut cache = self.cache.borrow_mut();
        if let Some(d) = cache
            .get(&key)
            .filter(|d| d.edits == edits && d.detected == detected)
        {
            return d.image.clone();
        }
        let a = analysis_of(lib, t);
        let image = draw(&a, duration_ms(t, &a), cues, scale);
        if cache.len() >= CACHE_MAX {
            cache.clear(); // ponytail: no LRU; the visible rows redraw once after a full cache, add LRU if that shows
        }
        cache.insert(
            key,
            Drawn {
                edits,
                detected,
                image: image.clone(),
            },
        );
        image
    }
}

/// The analysis a strip draws: the track's own (waveform and cues), else the cache's waveform under the track's own cues.
fn analysis_of(lib: &dyn Library, t: &Track) -> Analysis {
    let own = lib.analysis(t);
    match own {
        Some(a) if overview::wave(&a, 1).is_some() => a,
        own => {
            let cached = t
                .file
                .as_deref()
                .and_then(|f| analysis::cache::load(f).ok().flatten());
            let mut a = cached.unwrap_or_default();
            a.cues = own.map(|o| o.cues).unwrap_or_default();
            a
        }
    }
}

/// Track length in ms: the library's, else the last beat of the grid.
fn duration_ms(t: &Track, a: &Analysis) -> u32 {
    if t.length_s > 0 {
        t.length_s as u32 * 1000
    } else {
        a.beats.last().map_or(0, |b| b.time_ms)
    }
}

/// The strip at `scale` physical pixels per logical one (the window's scale factor), so Slint shows it unscaled.
fn draw(a: &Analysis, duration_ms: u32, cues: bool, scale: usize) -> Image {
    let (w, h) = (W * scale, H * scale);
    let mut c = Canvas::new(w, h);
    if duration_ms > 0 {
        let axis = Axis {
            start_ms: 0.0,
            px_per_ms: w as f64 / f64::from(duration_ms),
        };
        if let Some(wave) = overview::wave(a, duration_ms) {
            wave.draw(&mut c, &axis, true);
        }
        for cue in a.cues.iter().filter(|_| cues) {
            let (x, rgb) = (axis.col(f64::from(cue.time_ms)), cue_color(cue));
            c.vline(x, 0..h, rgb);
            if cue.hot_cue > 0 {
                c.cue_box(x, 0, cue_letter(cue.hot_cue as u8), rgb);
            }
        }
    }
    c.image()
}

/// Where a click at `x` (0..=1 across the strip) starts the preview: the cue (ms in `cues`) nearest to the click when one
/// is within [`HIT_PX`], else the clicked position.
fn start_ms(cues: &[u32], duration_ms: u32, x: f32) -> u32 {
    let x = f64::from(x).clamp(0.0, 1.0);
    let px_per_ms = W as f64 / f64::from(duration_ms.max(1));
    let miss = |c: &u32| (f64::from(*c) * px_per_ms - x * W as f64).abs();
    cues.iter()
        .min_by(|a, b| miss(a).total_cmp(&miss(b)))
        .filter(|c| miss(c) <= HIT_PX)
        .copied()
        .unwrap_or((x * f64::from(duration_ms)) as u32)
}

/// Preview start (ms) for a click at `x` on row `row`'s strip, with the track; `None` when the row is gone.
pub(super) fn click(v: &View, row: usize, x: f32) -> Option<(Track, u32)> {
    let t = v.list.get(*v.shown.get(row)?)?;
    let a = analysis_of(&*v.lib, t);
    let on = prefs::with(|p| p.view.panel_preview_cues);
    let cues: Vec<u32> = a.cues.iter().filter(|_| on).map(|c| c.time_ms).collect();
    Some((t.clone(), start_ms(&cues, duration_ms(t, &a), x)))
}

pub(super) fn wire(app: &App, view: &Rc<RefCell<View>>) {
    let (v, weak) = (view.clone(), app.as_weak());
    app.global::<BrowserState>().on_preview_wanted(move |row| {
        let (Ok(v), Ok(row), Some(app)) = (v.try_borrow(), usize::try_from(row), weak.upgrade())
        else {
            return;
        };
        let Some(t) = v.shown.get(row).and_then(|&k| v.list.get(k)) else {
            return;
        };
        let scale = app.window().scale_factor().round().max(1.0) as usize;
        let image = v.strips.image(&*v.lib, t, scale);
        if v.strips.model.row_count() != v.shown.len() {
            v.strips.reset(v.shown.len());
        }
        v.strips.model.set_row_data(row, image);
    });
    // The setting changed: the rows are made again, so every visible strip asks for its drawing again.
    let (v, weak) = (view.clone(), app.as_weak());
    let cues = Cell::new(prefs::with(|p| p.view.panel_preview_cues));
    prefs::on_change(move |p| {
        if cues.replace(p.view.panel_preview_cues) != p.view.panel_preview_cues
            && let (Ok(mut v), Some(app)) = (v.try_borrow_mut(), weak.upgrade())
        {
            v.refresh(&app);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 100 s track on a 110 px strip: 1 px = 0.909 s.
    const LEN: u32 = 100_000;

    #[test]
    fn a_click_near_a_cue_starts_from_the_cue() {
        let cues = [10_000, 50_000];
        // 50 % of the strip is 55 px; the cue at 50 s sits at 55 px: dead centre and 5 px off both hit it.
        assert_eq!(start_ms(&cues, LEN, 0.5), 50_000);
        assert_eq!(start_ms(&cues, LEN, 0.5 + 5.0 / W as f32), 50_000);
        // the nearer of two cues wins
        assert_eq!(
            start_ms(&[50_000, 52_000], LEN, 0.5 + 2.0 / W as f32),
            52_000
        );
    }

    #[test]
    fn a_click_away_from_cues_starts_where_it_landed() {
        let cues = [10_000, 50_000];
        // 7 px away from the nearest cue: outside HIT_PX
        assert_eq!(
            start_ms(&cues, LEN, 0.5 + 7.0 / W as f32),
            ((0.5 + 7.0 / W as f64) * f64::from(LEN)) as u32
        );
        assert_eq!(start_ms(&[], LEN, 0.25), 25_000);
        // the ends clamp
        assert_eq!(start_ms(&[], LEN, -1.0), 0);
        assert_eq!(start_ms(&[], LEN, 2.0), LEN);
        // a track of unknown length starts at 0 whatever was clicked
        assert_eq!(start_ms(&[], 0, 0.5), 0);
    }

    #[test]
    fn cue_markers_are_drawn_only_when_asked_for() {
        use openfader::analysis::Cue;
        let a = Analysis {
            cues: vec![Cue {
                hot_cue: 1,
                time_ms: 50_000,
                loop_end_ms: None,
                color: Some([255, 0, 0]),
                comment: None,
            }],
            ..Analysis::default()
        };
        let buf = |cues| draw(&a, LEN, cues, 1).to_rgba8().unwrap();
        let (with, without) = (buf(true), buf(false));
        // a red pixel in a lower row, below the letter box: only the cue line can put it there
        let red = |b: &slint::SharedPixelBuffer<slint::Rgba8Pixel>| {
            (0..W).any(|x| {
                let p = b.as_slice()[15 * W + x];
                (p.r, p.g, p.b) == (255, 0, 0)
            })
        };
        assert!(red(&with) && !red(&without));
    }
}
