//! The Cover column of the track table: one small picture per row, set when the row first shows (the table calls
//! `BrowserState.cover-wanted`). The picture is the track's [`openfader::artwork::resolve`] (a kept cover, the library's, a folder
//! image). Decoded pictures are kept by file ([`Covers::cache`]): a new cover is a new file, so an entry never goes stale, and the
//! tracks of one album share one. ponytail: the cache is cleared whole when full, add LRU if that shows.

use super::{strip::StripRows, App, BrowserState, View};
use openfader::{artwork, store};
use slint::{ComponentHandle, Image, Model};
use std::{cell::RefCell, collections::HashMap, path::PathBuf, rc::Rc};

/// Side of the stored pictures, in pixels (the column shows 18 logical px).
const SIDE: u32 = 48;
/// Most pictures kept between row creations.
const CACHE_MAX: usize = 256;

pub(super) struct Covers {
    model: Rc<StripRows>,
    cache: RefCell<HashMap<PathBuf, Option<Image>>>,
}

impl Covers {
    pub fn new(app: &App) -> Self {
        let model = Rc::new(StripRows::default());
        app.global::<BrowserState>()
            .set_covers(model.clone().into());
        Self {
            model,
            cache: RefCell::default(),
        }
    }

    /// The rows or the covers changed: every cover is blank until its row asks for it again.
    pub fn reset(&self, app: &App, rows: usize) {
        let generation = self.model.reset(rows);
        app.global::<BrowserState>()
            .set_cover_generation(generation);
    }

    fn image(&self, path: PathBuf) -> Option<Image> {
        let mut cache = self.cache.borrow_mut();
        if cache.len() >= CACHE_MAX {
            cache.clear();
        }
        cache
            .entry(path)
            .or_insert_with_key(|p| crate::app::artwork::image(p, SIDE))
            .clone()
    }
}

pub(super) fn wire(app: &App, view: &Rc<RefCell<View>>) {
    let (v, weak) = (view.clone(), app.as_weak());
    app.global::<BrowserState>().on_cover_wanted(move |row| {
        let (Ok(v), Ok(row), Some(app)) = (v.try_borrow(), usize::try_from(row), weak.upgrade())
        else {
            return;
        };
        let Some(t) = v.shown.get(row).and_then(|&k| v.list.get(k)) else {
            return;
        };
        if v.covers.model.row_count() != v.shown.len() {
            v.covers.reset(&app, v.shown.len());
        }
        if let Some(img) = artwork::resolve(&store::global(), t).and_then(|p| v.covers.image(p)) {
            v.covers.model.set_row_data(row, img);
        }
    });
}
