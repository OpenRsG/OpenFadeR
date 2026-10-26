//! Waveform rendering (base), 1:1 with the screen: every buffer is drawn at the *physical* pixel
//! size of its Slint `Image` and shown unscaled, so no pixel is stretched or filtered.
//!
//! Shared here, used by the children [`overview`] (whole track), [`detail`] (window around the
//! playhead) and [`phrase`] (the phrase band and its colours):
//! - [`Axis`]: the one time→x mapping. Beat lines, cues, the playhead *and* the waveform columns
//!   all go through it, so they cannot drift apart. Pixel column `x` covers `[ms(x), ms(x+1))`;
//!   a line for time `t` sits on column `floor(x(t))`, the column that contains `t`.
//! - [`Wave`]: entries (any rate) → one value per pixel column. Entry `i` covers
//!   `[i, i+1)/per_ms` ms. A column spanning ≥1 entry takes the max height and the height-weighted
//!   mean colour (downsampling); a column inside one entry interpolates linearly between the
//!   neighbouring entry centres (upsampling), so outlines are smooth, not stairs.
//! - [`Canvas`]: RGBA buffer; waveform edges are anti-aliased (fractional row coverage blended
//!   against what is below, black at the bottom layer).
//! - Layers: a column is three height/colour layers drawn back to front. 3-band data uses
//!   all three (low = dark blue, mid = amber, high = white, as in the 3-band view); mono/colour data uses one.

pub mod backend;
#[cfg(test)]
mod composition_tests;
pub mod detail;
mod font;
pub mod overview;
pub mod phrase;

use super::prefs::{HotCueColors, WaveColors};
use openfader::analysis::Cue;
use slint::{Image, Rgba8Pixel, SharedPixelBuffer};
use std::{
    ops::Range,
    sync::atomic::{AtomicU32, Ordering::Relaxed},
};

pub type Rgb = [u8; 3];

/// The colours of the active skin that Rust draws itself: `Theme` tokens `wave-bg`, `wave-low`, `wave-mid`, `wave-high`, `cue`,
/// `memory-cue`, `wave-style` with `deck1`..`deck4` and their `-hi` twins, and `wave-fade`.
/// [`crate::app::skin`] stores them with [`set_palette`] whenever a skin is applied; the waveforms are rebuilt on every settings
/// change ([`crate::app::deck`] `rewave`), so they follow at once.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    pub bg: Rgb,
    /// Low, mid and high band.
    pub band: [Rgb; 3],
    pub cue: Rgb,
    /// Memory cue marker (Theme.memory-cue).
    pub memory: Rgb,
    /// Low and mid band of decks 1 to 4 (`deckN`, `deckN-hi`), used instead of `band` when `per_deck`.
    pub decks: [[Rgb; 2]; 4],
    /// `wave-style` 1: every deck draws its own low and mid band ([`Palette::for_deck`]).
    pub per_deck: bool,
    /// `wave-fade`: how much a band darkens towards its outer edge, 0..=1.
    pub fade: f32,
}

/// Low, mid and high band colours (blue, amber, white) whose overlaps are fixed mixes ([`three_band`]).
const CLASSIC_BANDS: [Rgb; 3] = [[0, 0x55, 0xe1], [0xff, 0xa6, 0], [255, 255, 255]];

/// The mono (BLUE) waveform's colour before whiteness is mixed in.
const MONO: Rgb = [0, 104, 216];

/// [`Palette`] as packed words: bg, low, mid, high, cue, memory, the four decks' low and mid, `per_deck`, `fade`.
const WORDS: usize = 16;

impl Palette {
    /// The built-in skins' values (ui/theme.slint, Catppuccin Mocha).
    pub const DEFAULT: Palette = Palette {
        bg: [0x11, 0x11, 0x1b],
        band: [[0x89, 0xb4, 0xfa], [0xfa, 0xb3, 0x87], [0xcd, 0xd6, 0xf4]],
        cue: [0xfa, 0xb3, 0x87],
        memory: [0xf5, 0xc2, 0xe7],
        decks: [
            [[0x89, 0xb4, 0xfa], [0xb4, 0xbe, 0xfe]],
            [[0xfa, 0xb3, 0x87], [0xf9, 0xe2, 0xaf]],
            [[0xcb, 0xa6, 0xf7], [0xf5, 0xc2, 0xe7]],
            [[0x94, 0xe2, 0xd5], [0xa6, 0xe3, 0xa1]],
        ],
        per_deck: false,
        fade: 0.0,
    };

    /// This palette for the waveforms of deck `deck` (0-based, the deck itself, not the slot that shows it): with `per_deck` its
    /// low and mid band are the deck's colours, else the palette is unchanged.
    pub fn for_deck(&self, deck: usize) -> Palette {
        let mut p = *self;
        if let Some([lo, mid]) = self.decks.get(deck).filter(|_| self.per_deck) {
            (p.band[0], p.band[1]) = (*lo, *mid);
        }
        p
    }

    /// A colour of the RGB waveform (R, G, B = how much low, mid, high) in the bands of a per-deck palette; unchanged otherwise.
    fn tint(&self, rgb: [f32; 3]) -> [f32; 3] {
        if !self.per_deck {
            return rgb;
        }
        let (w, top) = (
            rgb.map(|c| c / 255.0),
            rgb.iter().fold(0f32, |m, c| m.max(*c)),
        );
        let sum = (w[0] + w[1] + w[2]).max(f32::EPSILON);
        let mix = |k: usize| {
            self.band
                .iter()
                .zip(w)
                .map(|(b, w)| f32::from(b[k]) * w)
                .sum::<f32>()
                / sum
        };
        [mix(0), mix(1), mix(2)].map(|c| c * top / 255.0)
    }

    const fn words(&self) -> [u32; WORDS] {
        let mut w = [0; WORDS];
        w[0] = pack(self.bg);
        let mut i = 0;
        while i < 3 {
            w[1 + i] = pack(self.band[i]);
            i += 1;
        }
        w[4] = pack(self.cue);
        w[5] = pack(self.memory);
        i = 0;
        while i < 4 {
            w[6 + 2 * i] = pack(self.decks[i][0]);
            w[7 + 2 * i] = pack(self.decks[i][1]);
            i += 1;
        }
        w[14] = self.per_deck as u32;
        w[15] = self.fade.to_bits();
        w
    }

    fn from_words(w: [u32; WORDS]) -> Palette {
        Palette {
            bg: unpack(w[0]),
            band: [unpack(w[1]), unpack(w[2]), unpack(w[3])],
            cue: unpack(w[4]),
            memory: unpack(w[5]),
            decks: std::array::from_fn(|i| [unpack(w[6 + 2 * i]), unpack(w[7 + 2 * i])]),
            per_deck: w[14] != 0,
            fade: f32::from_bits(w[15]),
        }
    }
}

const fn pack(c: Rgb) -> u32 {
    (c[0] as u32) << 16 | (c[1] as u32) << 8 | c[2] as u32
}

fn unpack(v: u32) -> Rgb {
    [(v >> 16) as u8, (v >> 8) as u8, v as u8]
}

/// [`Palette`] as words ([`Palette::words`]). Atomics, so a frame reads them without a lock or allocation.
static PALETTE: [AtomicU32; WORDS] = {
    let w = Palette::DEFAULT.words();
    let mut a = [const { AtomicU32::new(0) }; WORDS];
    let mut i = 0;
    while i < WORDS {
        a[i] = AtomicU32::new(w[i]);
        i += 1;
    }
    a
};

/// Makes `p` the colours the renderers draw with.
pub fn set_palette(p: Palette) {
    for (slot, v) in PALETTE.iter().zip(p.words()) {
        slot.store(v, Relaxed);
    }
}

/// The shared colours: the bands of `wave-low`, `wave-mid`, `wave-high`.
pub fn palette() -> Palette {
    Palette::from_words(std::array::from_fn(|i| PALETTE[i].load(Relaxed)))
}

/// The colours deck `deck`'s waveforms are drawn with ([`Palette::for_deck`] of the skin's).
pub fn deck_palette(deck: usize) -> Palette {
    palette().for_deck(deck)
}

/// `wave-fade` (read per waveform drawn, not per column).
fn fade() -> f32 {
    f32::from_bits(PALETTE[15].load(Relaxed))
}

/// Time↔pixel mapping of one view. `x(ms)` is the (fractional) left edge of `ms` in pixels.
#[derive(Clone, Copy, Debug)]
pub struct Axis {
    pub start_ms: f64,
    pub px_per_ms: f64,
}

impl Axis {
    pub fn x(&self, ms: f64) -> f64 {
        (ms - self.start_ms) * self.px_per_ms
    }
    pub fn ms(&self, x: f64) -> f64 {
        x / self.px_per_ms + self.start_ms
    }
    /// Pixel column containing `ms` (negative / ≥ width when outside the view).
    pub fn col(&self, ms: f64) -> i64 {
        self.x(ms).floor() as i64
    }
}

#[derive(Clone)]
pub struct Canvas {
    pub w: usize,
    pub h: usize,
    buf: SharedPixelBuffer<Rgba8Pixel>,
}

/// `v` rounded half away from zero as a byte (saturating): the exact twin of `v.round() as u8`. Baseline x86-64 has no
/// SSE4.1, so `round`, `floor`, `ceil`, `trunc` and `fract` are each a libm call (measured: the per-column span and
/// per-pixel blend arithmetic was dominated by them). `v - t` is exact for |v| < 2²³.
fn round_u8(v: f32) -> u8 {
    let t = v as i32;
    (t + (v - t as f32 >= 0.5) as i32).clamp(0, 255) as u8
}

/// Blends `c` into `p` with coverage `cov` (0..=1). Keep the rounding of the sum, not of a pre-incremented one: adding 0.5
/// first can round a float just below a half up to the next integer before truncation.
fn blend(p: &mut Rgba8Pixel, c: [f32; 3], cov: f32) {
    let mix = |old: u8, new: f32| round_u8(old as f32 + (new - old as f32) * cov);
    *p = Rgba8Pixel::new(mix(p.r, c[0]), mix(p.g, c[1]), mix(p.b, c[2]), 255);
}

/// One anti-aliased layer in one pixel column, prepared once: the pixel `rows` it touches and the `full` rows it covers
/// entirely, which take a precomputed colour, except near a half-integer where float cancellation in the original blend
/// could change which byte it rounds to (`sensitive`: those spans blend every row).
struct Span {
    y0: f32,
    y1: f32,
    c: [f32; 3],
    rows: Range<usize>,
    full: Range<usize>,
    solid: Rgba8Pixel,
    sensitive: bool,
    /// `wave-fade`: (the row the layer is brightest at, its distance from there to its edge, the fade): a row is darker by
    /// the fade times its distance over the edge distance.
    fade: Option<(f32, f32, f32)>,
}

impl Span {
    /// Rows of `[y0, y1)` clipped to a canvas `h` rows tall. floor/ceil by integer cast: the clipped bounds are ≥ 0.
    fn new(h: usize, y0: f32, y1: f32, c: [f32; 3]) -> Self {
        let (y0, y1) = (y0.max(0.0), y1.min(h as f32));
        let (f0, f1) = (y0 as usize, y1.max(0.0) as usize);
        let (c0, c1) = (
            f0 + (y0 > f0 as f32) as usize,
            f1 + (y1 > f1 as f32) as usize,
        );
        // RGB is 0..255. Two floating-point operations can move a value across a half-integer rounding boundary; retain
        // the original arithmetic there. The conservative error bound is computed once per span, not per pixel.
        let sensitive = c
            .iter()
            .any(|v| (v - (*v as i32) as f32 - 0.5).abs() <= 2.0 * f32::EPSILON * 255.0);
        Self {
            y0,
            y1,
            c,
            rows: f0..c1.min(h),
            full: c0..f1.min(h),
            solid: Rgba8Pixel::new(round_u8(c[0]), round_u8(c[1]), round_u8(c[2]), 255),
            sensitive,
            fade: None,
        }
    }

    /// This span with `wave-fade` `k` (0..=1), for a layer brightest at row `mid` and `half` rows from there to its edge.
    fn faded(mut self, mid: f32, half: f32, k: f32) -> Self {
        self.fade = (k > 0.0 && half > 0.0).then_some((mid, half.recip(), k));
        self
    }
    /// Draws the rows `ys` (within `self.rows`) of column `x` into `px` (`w` pixels per row). The full rows are stored
    /// strided with no per-row branch; the partly covered edge rows blend with coverage.
    fn paint(&self, px: &mut [Rgba8Pixel], w: usize, x: usize, ys: Range<usize>) {
        if let Some((mid, inv_half, k)) = self.fade {
            for y in ys {
                let cov = (self.y1.min(y as f32 + 1.0) - self.y0.max(y as f32)).clamp(0.0, 1.0);
                let dim = 1.0 - k * ((y as f32 + 0.5 - mid).abs() * inv_half).min(1.0);
                blend(&mut px[y * w + x], self.c.map(|v| v * dim), cov);
            }
            return;
        }
        let edge = |px: &mut [Rgba8Pixel], rows: Range<usize>| {
            for y in rows {
                let cov = (self.y1.min(y as f32 + 1.0) - self.y0.max(y as f32)).clamp(0.0, 1.0);
                blend(&mut px[y * w + x], self.c, cov);
            }
        };
        let (a, b) = if self.sensitive || self.full.is_empty() {
            (ys.end, ys.end)
        } else {
            (
                self.full.start.clamp(ys.start, ys.end),
                self.full.end.clamp(ys.start, ys.end),
            )
        };
        edge(px, ys.start..a);
        if b > a {
            for p in px[a * w + x..].iter_mut().step_by(w).take(b - a) {
                *p = self.solid;
            }
        }
        edge(px, b..ys.end);
    }
}

/// A canvas filled black, whatever the skin's background is (the tests count on exact pixel values).
#[cfg(test)]
fn black_canvas(w: usize, h: usize) -> Canvas {
    let mut c = Canvas::new(w, h);
    c.buf.make_mut_slice().fill(Rgba8Pixel::new(0, 0, 0, 255));
    c
}

impl Canvas {
    /// Opaque skin background ([`Palette::bg`], Catppuccin Mocha's base in the built-in skin).
    pub fn new(w: usize, h: usize) -> Self {
        let mut buf = SharedPixelBuffer::<Rgba8Pixel>::new(w as u32, h as u32);
        let [r, g, b] = unpack(PALETTE[0].load(Relaxed));
        buf.make_mut_slice().fill(Rgba8Pixel::new(r, g, b, 255));
        Self { w, h, buf }
    }
    /// Solid 1-px vertical line over rows `ys` at column `x`; off-canvas columns are skipped. A whole-byte colour at coverage 1
    /// is exactly `c` (the blend's arithmetic is exact on integers), so it is stored, not blended.
    pub fn vline(&mut self, x: i64, ys: impl Iterator<Item = usize>, c: Rgb) {
        if (0..self.w as i64).contains(&x) {
            let (c, w, h) = (Rgba8Pixel::new(c[0], c[1], c[2], 255), self.w, self.h);
            let px = self.buf.make_mut_slice();
            for y in ys.filter(|&y| y < h) {
                px[y * w + x as usize] = c;
            }
        }
    }
    /// Enlarged waveform's bold grid: a contrasting outline and wider downbeats, not colour alone.
    pub fn bold_grid(&mut self, x: i64, downbeat: bool) {
        let p = palette();
        let (left, right, rgb) = if downbeat {
            (x - 1, x + 3, p.cue)
        } else {
            (x, x + 2, p.band[2])
        };
        self.shade(left - 1, right + 1, 0..self.h, p.bg, 1.0);
        self.shade(left, right, 0..self.h, rgb, 1.0);
    }
    /// Memory cue: a solid `width`-px bar centred on `x`, with an "M" flag at `flag_y` when there is room. Drawn after the grid, it
    /// is wider than a beat line (the callers widen it over the bold grid and with large markers).
    pub fn memory_cue(
        &mut self,
        x: i64,
        width: usize,
        rgb: Rgb,
        flag_y: Option<usize>,
        large: bool,
    ) {
        let left = x - ((width - 1) / 2) as i64;
        self.shade(left, left + width as i64, 0..self.h, rgb, 1.0);
        if let Some(y) = flag_y {
            self.cue_box(left, y, 'M', rgb, large);
        }
    }
    /// Translucent colour `c` (coverage `cov`) over pixel `rows` of columns `x0..x1`, clipped to the canvas. Coverage 1 fills
    /// (see [`Canvas::vline`]); below 1, [`blend`]'s result per old byte is tabulated once (3×256 entries) instead of
    /// computed per pixel, which is the same value: the phrase band, a loop and the vocal band are tens of thousands of pixels.
    pub fn shade(&mut self, x0: i64, x1: i64, rows: Range<usize>, c: Rgb, cov: f32) {
        let (w, h) = (self.w, self.h);
        let end = x1.clamp(0, w as i64) as usize;
        let xs = (x0.max(0) as usize).min(end)..end;
        let rows = rows.start..rows.end.min(h);
        if xs.is_empty() || rows.is_empty() {
            return;
        }
        let px = self.buf.make_mut_slice();
        if cov >= 1.0 {
            let solid = Rgba8Pixel::new(c[0], c[1], c[2], 255);
            for y in rows {
                px[y * w..][xs.clone()].fill(solid);
            }
            return;
        }
        let lut = c.map(|n| {
            std::array::from_fn::<u8, 256, _>(|old| {
                round_u8(old as f32 + (f32::from(n) - old as f32) * cov)
            })
        });
        for y in rows {
            for p in &mut px[y * w..][xs.clone()] {
                *p = Rgba8Pixel::new(
                    lut[0][p.r as usize],
                    lut[1][p.g as usize],
                    lut[2][p.b as usize],
                    255,
                );
            }
        }
    }
    /// `s` in the 5×7 [`font`] at integer `scale` with its top-left at (`x`, `y`); columns at or beyond `end` are not drawn.
    pub fn text(&mut self, mut x: i64, y: usize, s: &str, scale: usize, c: Rgb, end: i64) {
        let (c, w, h) = (Rgba8Pixel::new(c[0], c[1], c[2], 255), self.w, self.h);
        let end = end.min(w as i64);
        if y >= h || scale == 0 || x >= end || end <= 0 || s.is_empty() {
            return;
        }
        let px = self.buf.make_mut_slice();
        for ch in s.chars() {
            if x >= end {
                break;
            }
            for (r, row) in font::glyph(ch).iter().flatten().enumerate() {
                for k in (0..font::W).filter(|k| row >> (font::W - 1 - k) & 1 == 1) {
                    for (dx, dy) in (0..scale).flat_map(|dx| (0..scale).map(move |dy| (dx, dy))) {
                        let (cx, cy) = (x + (k * scale + dx) as i64, y + r * scale + dy);
                        if (0..end).contains(&cx) && cy < h {
                            px[cy * w + cx as usize] = c;
                        }
                    }
                }
            }
            x += ((font::W + 1) * scale) as i64;
        }
    }
    /// Anti-aliased vertical span `[y0, y1)` at column `x`; see [`Span`]. The tests' reference painter ([`Wave::draw`]
    /// paints spans itself).
    #[cfg(test)]
    fn span(&mut self, x: usize, y0: f32, y1: f32, c: [f32; 3]) {
        let s = Span::new(self.h, y0, y1, c);
        let rows = s.rows.clone();
        s.paint(self.buf.make_mut_slice(), self.w, x, rows);
    }
    pub fn image(&self) -> Image {
        Image::from_rgba8(self.buf.clone())
    }
    /// A copy with `extra` rows of the skin background appended below (the overview's phrase strip sits there).
    pub fn extended(&self, extra: usize) -> Canvas {
        let mut c = Canvas::new(self.w, self.h + extra);
        c.buf.make_mut_slice()[..self.w * self.h].copy_from_slice(self.buf.as_slice());
        c
    }
    /// Cue letter box anchored at (`x`, `y`): 8×9 px, or 16×18 with a double-size letter.
    pub fn cue_box(&mut self, x: i64, y: usize, letter: char, rgb: Rgb, large: bool) {
        let scale = if large { 2 } else { 1 };
        self.shade(x, x + (8 * scale) as i64, y..y + 9 * scale, rgb, 1.0);
        self.text(
            x + (2 * scale) as i64,
            y + scale,
            letter.encode_utf8(&mut [0; 4]),
            scale,
            [0; 3],
            x + (8 * scale) as i64,
        );
    }
}

/// One waveform layer in a column: height 0..=1 of the view, colour 0..=255 per channel.
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct Col {
    pub h: f32,
    pub rgb: [f32; 3],
}
/// Back to front.
pub type Layers = [Col; 3];

fn lerp(a: &Layers, b: &Layers, t: f32) -> Layers {
    let l = |a: f32, b: f32| a + (b - a) * t;
    std::array::from_fn(|i| Col {
        h: l(a[i].h, b[i].h),
        rgb: std::array::from_fn(|k| l(a[i].rgb[k], b[i].rgb[k])),
    })
}

/// Entries at `per_ms` entries per millisecond.
pub struct Wave {
    pub layers: Vec<Layers>,
    pub per_ms: f64,
}

impl Wave {
    /// Value of the column covering entry coordinates `[e0, e1)`; None when it lies outside the data.
    pub fn sample(&self, e0: f64, e1: f64) -> Option<Layers> {
        let n = self.layers.len() as f64;
        if e1 <= 0.0 || e0 >= n {
            return None;
        }
        if e1 - e0 < 1.0 {
            // Entry i is centred at i + 0.5: interpolate between the two centres around the column centre.
            let c = (e0 + e1) / 2.0 - 0.5;
            let left = c.floor();
            let at = |i: f64| self.layers[i.clamp(0.0, n - 1.0) as usize];
            return Some(lerp(&at(left), &at(left + 1.0), (c - left) as f32));
        }
        let (i0, i1) = (e0.floor().max(0.0) as usize, (e1.ceil().min(n)) as usize);
        let entries = &self.layers[i0..i1];
        Some(std::array::from_fn(|l| {
            let (mut h, mut sum, mut weighted) = (0f32, 0f32, [0f32; 3]);
            for e in entries {
                h = h.max(e[l].h);
                sum += e[l].h;
                for (w, c) in weighted.iter_mut().zip(e[l].rgb) {
                    *w += e[l].h * c;
                }
            }
            Col {
                h,
                rgb: if sum > 0.0 {
                    weighted.map(|w| w / sum)
                } else {
                    entries[0][l].rgb
                },
            }
        }))
    }

    /// Draw every pixel column of `c` through `axis`. `centered`: symmetric around the middle row
    /// (detail); otherwise anchored to the bottom (overview). Layers go back to front; a row a later layer covers fully
    /// (and so overwrites without reading) is not painted by the layers below it. Pixel-identical to painting every layer
    /// whole (`draw_matches_layer_by_layer`); measured −25…−45 % of the draw at 1562×56…2136×220 with the rounding changes.
    pub fn draw(&self, c: &mut Canvas, axis: &Axis, centered: bool) {
        let fade = fade();
        let (w, ch) = (c.w, c.h);
        let h = ch as f32;
        let px = c.buf.make_mut_slice();
        let mut e0 = axis.ms(0.0) * self.per_ms;
        for x in 0..w {
            let e1 = axis.ms(x as f64 + 1.0) * self.per_ms;
            let sampled = self.sample(e0, e1);
            e0 = e1;
            let Some(layers) = sampled else {
                continue;
            };
            let mut spans: [Option<Span>; 3] = [None, None, None];
            for (slot, l) in spans.iter_mut().zip(layers.iter().filter(|l| l.h > 0.0)) {
                let (y0, y1) = if centered {
                    (h / 2.0 * (1.0 - l.h), h / 2.0 * (1.0 + l.h))
                } else {
                    (h * (1.0 - l.h), h)
                };
                let (mid, half) = if centered {
                    (h / 2.0, h / 2.0 * l.h)
                } else {
                    (h, h * l.h)
                };
                *slot = Some(Span::new(ch, y0, y1, l.rgb).faded(mid, half, fade));
            }
            for (i, s) in spans.iter().enumerate() {
                let Some(s) = s else { break };
                let hidden = spans[i + 1..]
                    .iter()
                    .flatten()
                    .filter(|t| !t.sensitive)
                    .map(|t| &t.full)
                    .filter(|f| !f.is_empty())
                    .max_by_key(|f| f.len());
                let (a, b) = hidden.map_or((s.rows.end, s.rows.end), |f| {
                    (
                        f.start.clamp(s.rows.start, s.rows.end),
                        f.end.clamp(s.rows.start, s.rows.end),
                    )
                });
                s.paint(px, w, x, s.rows.start..a);
                s.paint(px, w, x, b..s.rows.end);
            }
        }
    }
}

const NONE: Col = Col {
    h: 0.0,
    rgb: [0.0; 3],
};

/// One layer: height `h` (0..=1) in colour `rgb`.
pub fn single(rgb: Rgb, h: f32) -> Layers {
    [
        Col {
            h,
            rgb: rgb.map(f32::from),
        },
        NONE,
        NONE,
    ]
}

/// Mono waveform byte: height 0..=31 in the low 5 bits, whiteness 0..=7 above.
/// Blue mixed towards white by whiteness.
pub fn mono_rgbh(v: u8) -> (Rgb, u8) {
    mono_rgbh_in(MONO, v)
}

fn mono_rgbh_in(base: Rgb, v: u8) -> (Rgb, u8) {
    let white = (v >> 5) as u32;
    let mix = |base: u8| (base as u32 + (255 - base as u32) * white / 7) as u8;
    (base.map(mix), v & 31)
}

/// The mono (BLUE) waveform layer: blue, or the low band of a per-deck palette ([`Palette::for_deck`]).
pub fn mono_layers(p: &Palette, v: u8) -> Layers {
    let (rgb, h) = mono_rgbh_in(if p.per_deck { p.band[0] } else { MONO }, v);
    single(rgb, h as f32 / 31.0)
}

/// The colour (RGB) waveform layer of `rgb` (a byte per channel) and height `h`: as stored, or mapped onto the bands of a
/// per-deck palette ([`Palette::for_deck`]).
pub fn color_layer(p: &Palette, rgb: [f32; 3], h: f32) -> Col {
    Col {
        h,
        rgb: p.tint(rgb),
    }
}

/// Stored waveform families a view can draw: 3-band, colour, mono blue.
#[derive(Clone, Copy)]
pub enum Src {
    ThreeBand,
    Color,
    Mono,
}

/// The families in the order the PREFS waveform mode wants them; a track without data for the first
/// uses the next, so every mode still draws something.
pub fn sources(mode: WaveColors) -> [Src; 3] {
    match mode {
        WaveColors::ThreeBand => [Src::ThreeBand, Src::Color, Src::Mono],
        WaveColors::Rgb => [Src::Color, Src::ThreeBand, Src::Mono],
        WaveColors::Blue => [Src::Mono, Src::ThreeBand, Src::Color],
    }
}

/// 3-band entries (`[low, mid, high]`) → layers, tallest band first, in the band colours of `p`.
/// Colour model:
/// byte0 → blue (0,85,225), byte1 → amber (255,166,0), byte2 → white; each band is scaled against a fixed
/// 128, low/mid linearly, high through (1 − cos(π·v/128))/2. Where bands overlap in a column
/// the overlap span is blue∩amber (180,105,10), blue∩white (210,220,250), amber∩white (255,240,215) and the
/// core where all three overlap (245,235,215). Per-band gains are 1. The [`CLASSIC_BANDS`] have fixed overlaps; a skin that
/// changes a band gets the mean of the bands that overlap.
pub fn three_band(p: &Palette, src: &[[u8; 3]]) -> Vec<Layers> {
    if src.is_empty() {
        return Vec::new();
    }
    // The byte's nonlinear height is palette-independent; evaluate its 256 values once, not once per track entry.
    static HIGH: std::sync::LazyLock<[f32; 256]> = std::sync::LazyLock::new(|| {
        std::array::from_fn(|hi| {
            ((1.0 - (std::f32::consts::PI * hi as f32 / 128.0).cos()) / 2.0).min(1.0)
        })
    });
    let high = &*HIGH;
    fn mean(cs: &[Rgb]) -> Rgb {
        std::array::from_fn(|i| {
            (cs.iter().map(|c| c[i] as u32).sum::<u32>() / cs.len() as u32) as u8
        })
    }
    let band = p.band;
    let (pair, all): ([Rgb; 3], Rgb) = if band == CLASSIC_BANDS {
        // indexed by the band left out: low, mid, high → mid∩high, low∩high, low∩mid
        (
            [[255, 240, 215], [210, 220, 250], [180, 105, 10]],
            [245, 235, 215],
        )
    } else {
        (
            [
                mean(&band[1..]),
                mean(&[band[0], band[2]]),
                mean(&band[..2]),
            ],
            mean(&band),
        )
    };
    let col = |h: f32, rgb: Rgb| Col {
        h,
        rgb: rgb.map(f32::from),
    };
    src.iter()
        .map(|&[lo, mid, hi]| {
            let h = [lo as f32 / 128.0, mid as f32 / 128.0, high[hi as usize]].map(|h| h.min(1.0));
            let mut o = [0, 1, 2];
            o.sort_by(|&a, &b| h[b].total_cmp(&h[a]));
            // tallest alone, then the two tallest overlapped, then all three
            [
                col(h[o[0]], band[o[0]]),
                col(h[o[1]], pair[o[2]]),
                col(h[o[2]], all),
            ]
        })
        .collect()
}

/// Colour of a cue marker / pad in the VIEW > HOT CUE COLOR mode ([`hot_cue_rgb`]).
pub fn cue_color(c: &Cue) -> Rgb {
    hot_cue_rgb(super::prefs::with(|p| p.view.hot_cue_colors), c)
}

/// The letter on a hot cue's box: hot cue 1 is A, wrapping after Z.
pub fn cue_letter(hot_cue: u8) -> char {
    (b'A' + (hot_cue - 1) % 26) as char
}

/// Memory cues keep their own colour (else Theme.memory-cue) in every mode. Hot cues: COLORFUL = the cue's own colour
/// (else green); CLASSIC = hot cues green, hot loops amber; COLD1 = hot cues blue, hot loops green; COLD2 = blue
/// gradation from light (A) to dark (H); PADS = red, orange, blue, dark blue, green, magenta, cyan, purple by pad.
/// The RGB values are ours.
pub fn hot_cue_rgb(mode: HotCueColors, c: &Cue) -> Rgb {
    let own = c.color.filter(|&rgb| rgb != [0, 0, 0]);
    if c.hot_cue == 0 {
        return own.unwrap_or(palette().memory);
    }
    let looped = c.loop_end_ms.is_some();
    match mode {
        HotCueColors::Colorful => own.unwrap_or([40, 220, 80]),
        HotCueColors::Classic if looped => [255, 166, 0],
        HotCueColors::Classic => [40, 220, 80],
        HotCueColors::Cold1 if looped => [40, 220, 80],
        HotCueColors::Cold1 => [20, 115, 235],
        HotCueColors::Cold2 => {
            let t = (c.hot_cue - 1).min(7) as f32 / 7.0;
            let mix = |a: f32, b: f32| (a + (b - a) * t).round() as u8;
            [mix(140.0, 0.0), mix(196.0, 60.0), mix(255.0, 190.0)]
        }
        HotCueColors::Pads => {
            const PADS: [Rgb; 8] = [
                [191, 39, 36],
                [246, 131, 25],
                [18, 133, 196],
                [22, 60, 161],
                [30, 175, 36],
                [205, 55, 155],
                [35, 187, 188],
                [103, 35, 182],
            ];
            PADS[usize::from(c.hot_cue - 1).min(7)]
        }
    }
}

/// 2-px playhead centred on `x` (columns `x-1` and `x`), full height.
pub fn playhead(c: &mut Canvas, x: i64) {
    for x in x - 1..=x {
        c.vline(x, 0..c.h, [255, 255, 255]);
    }
}

/// Slip ghost line: the shadow playhead, 1-px dashed cyan at column `x` (our choice).
#[cfg(test)]
pub fn slip_ghost(c: &mut Canvas, x: i64) {
    c.vline(x, (0..c.h).step_by(2), [60, 220, 255]);
}

/// player cue point (the session cue, src/app/deck/cue.rs): a solid orange line with a downward triangle
/// of `size` px hanging from the top edge. Hot / memory cues are dashed lines in their own colours.
pub fn cue_marker(c: &mut Canvas, x: i64, size: usize) {
    let cue = palette().cue; // Theme.cue
    for r in 0..size {
        let half = (size - 1 - r) as i64;
        for dx in -half..=half {
            c.vline(x + dx, r..r + 1, cue);
        }
    }
    c.vline(x, size..c.h, cue);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hot_cue_colour_modes_recolour_hot_cues_but_not_memory_cues() {
        let cue = |hot_cue, loop_end_ms, color| Cue {
            hot_cue,
            time_ms: 0,
            loop_end_ms,
            color,
            comment: None,
        };
        let (hot, looped, memory) = (
            cue(1, None, Some([9, 9, 9])),
            cue(2, Some(5), None),
            cue(0, None, None),
        );
        assert_eq!(hot_cue_rgb(HotCueColors::Colorful, &hot), [9, 9, 9]);
        assert_eq!(hot_cue_rgb(HotCueColors::Classic, &hot), [40, 220, 80]);
        assert_eq!(hot_cue_rgb(HotCueColors::Classic, &looped), [255, 166, 0]);
        assert_eq!(hot_cue_rgb(HotCueColors::Cold1, &hot), [20, 115, 235]);
        assert_ne!(
            hot_cue_rgb(HotCueColors::Cold2, &hot),
            hot_cue_rgb(HotCueColors::Cold2, &cue(8, None, None))
        );
        assert_eq!(hot_cue_rgb(HotCueColors::Cold1, &memory), palette().memory);
    }

    #[test]
    fn waveform_spans_preserve_solid_rows_and_antialiased_edges_when_clipped() {
        let below_half = f32::from_bits(64.5f32.to_bits() - 1);
        for (old, rgb) in [
            (Rgba8Pixel::new(17, 83, 211, 255), [64.25, 127.0, 200.75]),
            (Rgba8Pixel::new(211, 211, 211, 255), [below_half; 3]),
            (Rgba8Pixel::new(0, 83, 211, 255), [127.5, 64.5, 0.5]),
        ] {
            for (y0, y1) in [
                (-2.5f32, 9.25f32),
                (1.25, 6.75),
                (3.25, 3.75),
                (2.0, 6.0),
                (4.0, 4.0),
                (7.5, 10.5),
                (9.0, 11.0),
            ] {
                let mut c = Canvas::new(3, 8);
                c.buf.make_mut_slice().fill(old);
                c.span(1, y0, y1, rgb);
                for y in 0..8 {
                    let coverage = (y1.min(8.0).min(y as f32 + 1.0) - y0.max(0.0).max(y as f32))
                        .clamp(0.0, 1.0);
                    let mix = |a: u8, b: f32| (a as f32 + (b - a as f32) * coverage).round() as u8;
                    let want = Rgba8Pixel::new(
                        mix(old.r, rgb[0]),
                        mix(old.g, rgb[1]),
                        mix(old.b, rgb[2]),
                        255,
                    );
                    let row = &c.buf.as_slice()[y * 3..][..3];
                    assert_eq!(
                        row,
                        &[old, want, old],
                        "span {y0}..{y1}, row {y}, rgb {rgb:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_per_deck_palette_colours_each_decks_waves_and_a_shared_one_does_not() {
        let mut p = Palette::DEFAULT;
        p.decks[0] = [[0, 255, 0], [1, 2, 3]];
        p.decks[1] = [[200, 0, 255], [4, 5, 6]];
        assert_eq!(
            Palette::from_words(p.words()),
            p,
            "the atomics keep every field"
        );
        let low = |p: &Palette| three_band(p, &[[100, 0, 0]])[0][0].rgb;
        assert_eq!(
            low(&p.for_deck(0)),
            low(&p.for_deck(1)),
            "wave-style 0: every deck the same"
        );
        assert_eq!(low(&p.for_deck(0)), Palette::DEFAULT.band[0].map(f32::from));
        p.per_deck = true;
        assert_eq!(low(&p.for_deck(0)), [0.0, 255.0, 0.0]);
        assert_eq!(low(&p.for_deck(1)), [200.0, 0.0, 255.0]);
        let mid = three_band(&p.for_deck(1), &[[0, 100, 0]])[0][0].rgb;
        assert_eq!(mid, [4.0, 5.0, 6.0], "mid is deckN-hi");
        let high = |d| three_band(&p.for_deck(d), &[[0, 0, 100]])[0][0].rgb;
        assert_eq!(high(0), high(1), "high is shared");
        // BLUE scheme: the deck's colour, whiter with the stored whiteness; the RGB scheme: pure R is the low band
        assert_eq!(mono_layers(&p.for_deck(1), 31)[0].rgb, [200.0, 0.0, 255.0]);
        assert_eq!(
            mono_layers(&Palette::DEFAULT, 31)[0].rgb,
            [0.0, 104.0, 216.0]
        );
        assert_eq!(
            color_layer(&p.for_deck(0), [255.0, 0.0, 0.0], 1.0).rgb,
            [0.0, 255.0, 0.0]
        );
        assert_eq!(
            color_layer(&p.for_deck(0), [0.0, 0.0, 0.0], 1.0).rgb,
            [0.0; 3],
            "silence stays dark"
        );
        assert_eq!(
            color_layer(&Palette::DEFAULT, [9.0, 8.0, 7.0], 1.0).rgb,
            [9.0, 8.0, 7.0]
        );
        assert_eq!(
            p.for_deck(7),
            p,
            "a deck without colours draws the shared ones"
        );
    }

    #[test]
    fn wave_fade_darkens_a_band_towards_its_edge_and_zero_is_flat() {
        let rows = |k: f32| {
            let mut c = black_canvas(1, 21);
            Span::new(21, 0.0, 21.0, [200.0; 3])
                .faded(10.5, 10.5, k)
                .paint(c.buf.make_mut_slice(), 1, 0, 0..21);
            c.buf.as_slice().iter().map(|p| p.r).collect::<Vec<_>>()
        };
        let flat = rows(0.0);
        assert!(flat.iter().all(|&r| r == 200));
        let faded = rows(0.5);
        assert_eq!(faded[10], 200, "the middle row keeps the colour");
        assert_eq!(faded[0], faded[20], "symmetric");
        assert!(faded[0] < faded[5] && faded[5] < faded[10], "{faded:?}");
        assert_eq!(
            faded[0], 105,
            "half the fade at the edge: 200 * (1 - 0.5 * 0.95)"
        );
    }

    fn wave(h: &[f32]) -> Wave {
        Wave {
            layers: h.iter().map(|&h| single([10, 20, 30], h)).collect(),
            per_ms: 0.15,
        }
    }

    #[test]
    fn axis_maps_time_and_columns_consistently() {
        // 5 px per 150-Hz entry
        let a = Axis {
            start_ms: 1000.0,
            px_per_ms: 0.75,
        };
        assert_eq!(a.x(1000.0), 0.0);
        assert_eq!(a.col(1000.0), 0);
        assert_eq!(a.col(999.999), -1);
        // column 5 starts exactly at entry 151 (151/150 s) and spans a fifth of it
        let (e0, e1) = (a.ms(5.0) * 0.15, a.ms(6.0) * 0.15);
        assert!(
            (e0 - 151.0).abs() < 1e-9 && (e1 - 151.2).abs() < 1e-9,
            "{e0} {e1}"
        );
        // ms(x(t)) round-trips
        for t in [0.0, 1.0, 333.3, 8000.0] {
            assert!((a.ms(a.x(t)) - t).abs() < 1e-9);
        }
        // a time just inside a column's span lands on that column
        assert_eq!(a.col(a.ms(7.0) + 1e-6), 7);
        assert_eq!(a.col(a.ms(8.0) - 1e-6), 7);
    }

    #[test]
    fn wave_downsamples_by_max_and_upsamples_by_interpolation() {
        let w = wave(&[0.0, 1.0, 0.0, 0.0]);
        // 4 entries in one column: max
        assert_eq!(w.sample(0.0, 4.0).unwrap()[0].h, 1.0);
        // upsampling: column centres between entry centres 0.5 (h=0) and 1.5 (h=1)
        let h = |e0: f64| w.sample(e0, e0 + 0.25).unwrap()[0].h;
        assert!((h(0.5 - 0.125) - 0.0).abs() < 1e-6);
        assert!((h(1.0 - 0.125) - 0.5).abs() < 1e-6);
        assert!((h(1.5 - 0.125) - 1.0).abs() < 1e-6);
        // outside the data
        assert!(w.sample(-2.0, -1.0).is_none());
        assert!(w.sample(4.0, 5.0).is_none());
        // downsampled colour is height-weighted
        let mut w = wave(&[1.0, 0.0]);
        w.layers[1][0].rgb = [200.0; 3];
        assert_eq!(w.sample(0.0, 2.0).unwrap()[0].rgb, [10.0, 20.0, 30.0]);
    }

    /// The drawing before the overdraw skip: every layer of every column painted whole through [`Canvas::span`].
    fn draw_every_layer(w: &Wave, c: &mut Canvas, axis: &Axis, centered: bool) {
        let h = c.h as f32;
        for x in 0..c.w {
            let (e0, e1) = (
                axis.ms(x as f64) * w.per_ms,
                axis.ms(x as f64 + 1.0) * w.per_ms,
            );
            let Some(layers) = w.sample(e0, e1) else {
                continue;
            };
            for l in layers.iter().filter(|l| l.h > 0.0) {
                let (y0, y1) = if centered {
                    (h / 2.0 * (1.0 - l.h), h / 2.0 * (1.0 + l.h))
                } else {
                    (h * (1.0 - l.h), h)
                };
                c.span(x, y0, y1, l.rgb);
            }
        }
    }

    #[test]
    fn draw_matches_layer_by_layer() {
        // Fixed-seed xorshift: heights in every nesting order (and absent layers), colours whole, fractional and at half-bytes.
        let mut s = 0x2545_f491_4f6c_dd1du64;
        let mut next = move || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            (s >> 40) as f32 / (1u64 << 24) as f32
        };
        let channels = [0.0, 64.5, 127.25, 200.0, 255.0];
        let layers: Vec<Layers> = (0..400)
            .map(|_| {
                std::array::from_fn(|_| Col {
                    h: if next() < 0.15 { 0.0 } else { next() },
                    rgb: std::array::from_fn(|_| {
                        if next() < 0.5 {
                            channels[(next() * 5.0) as usize % 5]
                        } else {
                            next() * 255.0
                        }
                    }),
                })
            })
            .collect();
        let w = Wave {
            layers,
            per_ms: 0.15,
        };
        // 0.75 px/ms upsamples (5 px per entry), 0.05 downsamples (3 entries per px).
        for (px_per_ms, h, centered) in [
            (0.75, 37, true),
            (0.05, 23, false),
            (0.75, 8, true),
            (0.15 - 1e-9, 37, true),
            (0.15, 37, true),
            (0.15 + 1e-9, 37, true),
        ] {
            for start_ms in [-2000.0, -6.6666666667, 0.0, 123.4, 2666.0] {
                let axis = Axis {
                    start_ms,
                    px_per_ms,
                };
                let (mut a, mut b) = (Canvas::new(300, h), Canvas::new(300, h));
                draw_every_layer(&w, &mut a, &axis, centered);
                w.draw(&mut b, &axis, centered);
                assert!(
                    a.buf.as_slice() == b.buf.as_slice(),
                    "{px_per_ms} px/ms, {h} rows, start {start_ms}"
                );
            }
        }
    }

    #[test]
    fn large_cue_letters_scale_with_the_flag_and_clip_at_the_canvas_edge() {
        let mut normal = black_canvas(8, 9);
        normal.cue_box(0, 0, 'A', [255, 180, 0], false);
        assert_eq!(
            normal.buf.as_slice()[8 + 3],
            Rgba8Pixel::new(0, 0, 0, 255),
            "A has a visible top"
        );
        assert_eq!(
            normal.buf.as_slice()[4 * 8 + 4],
            Rgba8Pixel::new(0, 0, 0, 255),
            "A has a crossbar"
        );
        let mut large = black_canvas(16, 18);
        large.cue_box(0, 0, 'A', [255, 180, 0], true);
        for y in 0..18 {
            for x in 0..16 {
                assert_eq!(
                    large.buf.as_slice()[y * 16 + x],
                    normal.buf.as_slice()[(y / 2) * 8 + x / 2]
                );
            }
        }
        let mut clipped = black_canvas(12, 13);
        clipped.cue_box(-4, 0, 'A', [255, 180, 0], true);
        for y in 0..13 {
            for x in 0..12 {
                assert_eq!(
                    clipped.buf.as_slice()[y * 12 + x],
                    large.buf.as_slice()[y * 16 + x + 4]
                );
            }
        }
        normal.cue_box(0, 0, 'F', [255, 180, 0], false);
        assert_eq!(
            normal.buf.as_slice()[8 + 6],
            Rgba8Pixel::new(0, 0, 0, 255),
            "F has a top bar"
        );
        assert_eq!(
            normal.buf.as_slice()[7 * 8 + 2],
            Rgba8Pixel::new(0, 0, 0, 255),
            "F has a stem"
        );
        assert_eq!(
            normal.buf.as_slice()[7 * 8 + 6],
            Rgba8Pixel::new(255, 180, 0, 255),
            "F has no bottom bar"
        );
    }

    #[test]
    fn edge_is_antialiased() {
        let mut c = black_canvas(1, 4);
        c.span(0, 1.5, 4.0, [200.0; 3]);
        let px = c.buf.as_slice();
        assert_eq!([px[0].r, px[1].r, px[2].r, px[3].r], [0, 100, 200, 200]);
    }

    #[test]
    fn shade_and_rounding_equal_the_per_pixel_blend() {
        for v in (0..=255 * 64).map(|i| i as f32 / 64.0 - 1.0) {
            assert_eq!(round_u8(v), v.round() as u8, "{v}");
        }
        let mut base = Canvas::new(256, 2);
        for (i, p) in base.buf.make_mut_slice().iter_mut().enumerate() {
            *p = Rgba8Pixel::new(i as u8, 255 - i as u8, (i * 7) as u8, 255);
        }
        for (rgb, cov) in [
            ([20, 115, 235], 0.35),
            ([2, 218, 12], 0.8),
            ([255, 153, 0], 0.3),
            ([255; 3], 1.0),
        ] {
            let mut c = base.clone();
            c.shade(0, 256, 0..2, rgb, cov);
            for (got, old) in c.buf.as_slice().iter().zip(base.buf.as_slice()) {
                let mut want = *old;
                blend(&mut want, rgb.map(f32::from), cov);
                assert_eq!(*got, want, "{rgb:?} at {cov}");
            }
        }
    }

    #[test]
    fn shade_tints_only_the_clipped_columns() {
        let mut c = black_canvas(4, 1);
        c.shade(-3, 2, 0..1, [200, 0, 0], 0.5);
        let px = c.buf.as_slice();
        assert_eq!([px[0].r, px[1].r, px[2].r, px[3].r], [100, 100, 0, 0]);
    }

    #[test]
    fn invisible_shades_and_text_leave_shared_pixels_untouched() {
        let base = Canvas::new(4, 3);
        for (x0, x1, rows) in [
            (-4, 0, 0..3),
            (4, 8, 0..3),
            (3, 1, 0..3),
            (0, 4, 3..8),
            (0, 4, 2..1),
        ] {
            let mut c = base.clone();
            c.shade(x0, x1, rows, [200; 3], 0.3);
            assert_eq!(c.buf.as_slice().as_ptr(), base.buf.as_slice().as_ptr());
        }
        for (x, y, end) in [(4, 0, 4), (0, 3, 4), (-8, 0, 0)] {
            let mut c = base.clone();
            c.text(x, y, "INTRO 1", 1, [255; 3], end);
            assert_eq!(c.buf.as_slice().as_ptr(), base.buf.as_slice().as_ptr());
        }
    }

    #[test]
    fn extended_keeps_rows_and_appends_the_background() {
        let mut c = Canvas::new(2, 1);
        c.shade(0, 2, 0..1, [200, 0, 0], 1.0);
        let e = c.extended(2);
        let px = e.buf.as_slice();
        assert_eq!((e.w, e.h, px.len()), (2, 3, 6));
        let bg = Palette::DEFAULT.bg[0];
        assert_eq!((px[0].r, px[2].r, px[5].r), (200, bg, bg));
    }

    /// The palette of the Rek theme: the bands that have fixed overlaps.
    fn classic() -> Palette {
        Palette {
            band: CLASSIC_BANDS,
            ..Palette::DEFAULT
        }
    }

    #[test]
    fn three_band_lookup_preserves_every_byte_height_and_layer_order() {
        let src: Vec<_> = (0..=255u8)
            .map(|v| [v, v.wrapping_add(91), v.wrapping_add(173)])
            .collect();
        let actual = three_band(&classic(), &src);
        for (&[lo, mid, hi], layers) in src.iter().zip(actual) {
            let mut heights = [
                lo as f32 / 128.0,
                mid as f32 / 128.0,
                (1.0 - (std::f32::consts::PI * hi as f32 / 128.0).cos()) / 2.0,
            ]
            .map(|h| h.min(1.0));
            heights.sort_by(|a, b| b.total_cmp(a));
            assert_eq!(
                layers.map(|l| l.h.to_bits()),
                heights.map(f32::to_bits),
                "{lo}/{mid}/{hi}"
            );
        }
    }

    #[test]
    fn three_band_order_scale_and_overlap() {
        let px = |e: [u8; 3]| three_band(&classic(), &[e])[0];
        // only byte0 (low): one blue layer, linear against 128
        let l = px([128, 0, 0]);
        assert_eq!((l[0].rgb, l[0].h, l[1].h), ([0.0, 85.0, 225.0], 1.0, 0.0));
        assert_eq!(px([64, 0, 0])[0].h, 0.5);
        // byte1 amber, byte2 white through the cosine curve ((1 − cos(π/2))/2 = 0.5 at v=64)
        assert_eq!(px([0, 128, 0])[0].rgb, [255.0, 166.0, 0.0]);
        let h = px([0, 0, 128])[0];
        assert_eq!((h.rgb, h.h), ([255.0; 3], 1.0));
        assert!((px([0, 0, 64])[0].h - 0.5).abs() < 1e-6);
        // blue 100 over amber 50: blue alone, then blue∩amber
        let o = px([100, 50, 0]);
        assert_eq!(
            (o[0].rgb, o[1].rgb, o[1].h),
            ([0.0, 85.0, 225.0], [180.0, 105.0, 10.0], 50.0 / 128.0)
        );
        // all three: core colour
        assert_eq!(px([128, 128, 128])[2].rgb, [245.0, 235.0, 215.0]);
    }

    #[test]
    fn a_skin_palette_recolours_the_bands_and_their_overlaps() {
        let skin = Palette {
            band: [[200, 0, 0], [0, 200, 0], [0, 0, 200]],
            ..Palette::DEFAULT
        };
        let px = |e: [u8; 3]| three_band(&skin, &[e])[0];
        assert_eq!(px([128, 0, 0])[0].rgb, [200.0, 0.0, 0.0]);
        // low 100 over mid 50: low alone, then the mean of the two, then (with high absent from the column) the three-way mean
        let o = px([100, 50, 0]);
        assert_eq!(
            (o[1].rgb, o[2].rgb),
            ([100.0, 100.0, 0.0], [66.0, 66.0, 66.0])
        );
        // the classic bands keep their fixed overlaps
        assert_eq!(
            three_band(&classic(), &[[100, 50, 0]])[0][1].rgb,
            [180.0, 105.0, 10.0]
        );
    }
}
