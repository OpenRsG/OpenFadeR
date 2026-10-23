//! Cover art ↔ ui/artwork.slint (global `ArtworkState`), the Track menu, the browser's Cover column, the decks' jacket and the
//! track info. The finding, keeping and resolving of pictures is [`openfader::artwork`]; this is what the user sees of it.
//!
//! Children: [`picker`] (Find Artwork: the plugins' candidates with thumbnails, a click keeps one) and [`bulk`] (Fetch Missing
//! Artwork: every album of the list on screen that has none gets the first picture the plugins offer).
//!
//! Pictures are shown at their own size's share only: [`image`] loads a file and shrinks anything larger than the place it is
//! shown (a 1200 px cover in a 20 px row would cost four megabytes of texture per row).

mod bulk;
mod picker;

pub use bulk::fetch_missing;
pub use picker::{find, wire};

use super::{library::Ctx, toast, App, BrowserState};
use openfader::{artwork, library::Track, plugin, store::Store};
use slint::{ComponentHandle, Image, Rgba8Pixel, SharedPixelBuffer};
use std::{path::Path, rc::Rc};

/// The file at `path` as a picture at most `max` pixels on its longer side; `None` when it cannot be read as one.
pub fn image(path: &Path, max: u32) -> Option<Image> {
    let img = Image::load_from_path(path).ok()?;
    let size = img.size();
    if size.width.max(size.height) <= max {
        return Some(img);
    }
    let Some(px) = img.to_rgba8() else {
        return Some(img);
    };
    let (w, h, out) = shrink(
        px.as_bytes(),
        size.width as usize,
        size.height as usize,
        max as usize,
    );
    let mut buf = SharedPixelBuffer::<Rgba8Pixel>::new(w as u32, h as u32);
    buf.make_mut_bytes().copy_from_slice(&out);
    Some(Image::from_rgba8(buf))
}

/// `rgba` (`w` × `h`, 4 bytes a pixel) averaged down by a whole factor so that neither side is over `max`: width, height and
/// the new pixels. A picture that already fits is returned as it is.
fn shrink(rgba: &[u8], w: usize, h: usize, max: usize) -> (usize, usize, Vec<u8>) {
    let f = w.max(h).div_ceil(max.max(1)).max(1);
    let (nw, nh) = ((w / f).max(1), (h / f).max(1));
    let mut out = Vec::with_capacity(nw * nh * 4);
    for y in 0..nh {
        for x in 0..nw {
            let mut sum = [0u32; 4];
            let mut n = 0;
            for yy in y * f..((y + 1) * f).min(h) {
                for xx in x * f..((x + 1) * f).min(w) {
                    for (s, v) in sum.iter_mut().zip(&rgba[(yy * w + xx) * 4..][..4]) {
                        *s += u32::from(*v);
                    }
                    n += 1;
                }
            }
            out.extend(sum.map(|s| (s / n.max(1)) as u8));
        }
    }
    (nw, nh, out)
}

/// The picture `t` shows ([`artwork::resolve`]) at most `max` pixels across.
pub fn cover(store: &Store, t: &Track, max: u32) -> Option<Image> {
    image(&artwork::resolve(store, t)?, max)
}

/// The enabled artwork plugins now.
fn plugins() -> Vec<plugin::Plugin> {
    plugin::current().artwork().cloned().collect()
}

/// Status line and toast.
fn say(app: &App, msg: &str) {
    app.global::<BrowserState>().set_status(msg.into());
    toast::show(app, msg);
}

/// A cover was chosen, fetched or undone: the browser, the track info and the loaded decks read it again.
pub(in crate::app) fn changed(c: &Rc<Ctx>) {
    super::browser::covers_changed(&c.app());
    c.reload(None);
    for d in c.decks.iter() {
        d.refresh_cover();
    }
    super::trackinfo::reshow(&c.app());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pictures_shrink_by_a_whole_factor_to_the_average_of_each_block() {
        // 4×2: the left 2×2 block is black and white pixels, the right one is red.
        let (b, w) = ([0, 0, 0, 255], [255, 255, 255, 255]);
        let r = [200, 0, 0, 255];
        let src: Vec<u8> = [b, w, r, r, w, b, r, r].concat();
        let (nw, nh, out) = shrink(&src, 4, 2, 2);
        assert_eq!((nw, nh), (2, 1));
        assert_eq!(out, [127, 127, 127, 255, 200, 0, 0, 255]);
        // Already small enough: one to one.
        let (nw, nh, out) = shrink(&src, 4, 2, 4);
        assert_eq!((nw, nh, out.len()), (4, 2, src.len()));
        assert_eq!(out, src);
        // Never over the limit, whatever the size.
        let big = vec![9u8; 601 * 300 * 4];
        let (nw, nh, _) = shrink(&big, 601, 300, 40);
        assert!(nw <= 40 && nh <= 40, "{nw}×{nh}");
    }
}
