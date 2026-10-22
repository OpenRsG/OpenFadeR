//! The library's video tracks and the per-deck LOAD button.

use super::{Video, VideoItem, VideoState};
use openfader::video;
use slint::{ComponentHandle, ModelRc, VecModel};

impl Video {
    pub(super) fn refresh_list(&self) {
        let tracks = match self.lib.tracks(None) {
            Ok(t) => t
                .into_iter()
                .filter(|t| t.file.as_deref().is_some_and(video::is_video))
                .collect::<Vec<_>>(),
            Err(e) => {
                *self.note.borrow_mut() = tr!("library: {}", e);
                return;
            }
        };
        let items: Vec<_> = tracks
            .iter()
            .map(|t| VideoItem {
                title: t.title.as_str().into(),
                info: if t.artist.is_empty() {
                    t.length_text()
                } else {
                    format!("{} · {}", t.artist, t.length_text())
                }
                .into(),
            })
            .collect();
        if let Some(app) = self.app.upgrade() {
            app.global::<VideoState>()
                .set_list(ModelRc::new(VecModel::from(items)));
        }
        *self.list.borrow_mut() = tracks;
    }

    pub(super) fn load(&self, row: usize, deck: usize) {
        let list = self.list.borrow();
        if let (Some(t), Some(d)) = (list.get(row), self.decks.get(deck)) {
            d.load(t, &*self.lib);
        }
    }
}
