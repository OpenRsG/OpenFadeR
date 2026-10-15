//! Image overlay controls; the UI draws the overlay on top of the mixed picture.

use super::{Video, VideoState};
use slint::{ComponentHandle, Image, SharedString};
use std::path::Path;

impl Video {
    pub(super) fn choose_image(&self, path: &str) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let state = app.global::<VideoState>();
        match Image::load_from_path(Path::new(path.trim())) {
            Ok(img) => {
                state.set_overlay_image(img);
                state.set_overlay_on(true);
                self.note.borrow_mut().clear();
            }
            Err(e) => *self.note.borrow_mut() = format!("cannot read an image from {path}: {e:?}"),
        }
    }

    pub(super) fn clear_overlay(&self) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let state = app.global::<VideoState>();
        state.set_overlay_on(false);
        state.set_text(SharedString::new());
        state.set_image_path(SharedString::new());
        state.set_overlay_image(Image::default());
        self.note.borrow_mut().clear();
    }
}
