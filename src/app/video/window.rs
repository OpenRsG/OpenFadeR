//! The separate output window.

use super::{Video, VideoWindow};
use slint::ComponentHandle;

impl Video {
    pub(super) fn window_open(&self) -> bool {
        self.window
            .borrow()
            .as_ref()
            .is_some_and(|w| w.window().is_visible())
    }

    /// The output window, created on first use.
    fn open_window(&self) {
        let mut slot = self.window.borrow_mut();
        let w = slot.get_or_insert_with(|| {
            let w = VideoWindow::new().expect("video output window");
            let weak = w.as_weak();
            w.on_toggle_full_screen(move || {
                if let Some(w) = weak.upgrade() {
                    w.window().set_fullscreen(!w.window().is_fullscreen());
                }
            });
            w
        });
        let _ = w.show();
    }

    pub(super) fn toggle_window(&self) {
        if self.window_open() {
            if let Some(w) = self.window.borrow().as_ref() {
                let _ = w.hide();
            }
        } else {
            self.open_window();
        }
    }

    /// FULL SCREEN: the output window, opened if need be, toggled to full screen.
    pub(super) fn full_screen(&self) {
        self.open_window();
        if let Some(w) = self.window.borrow().as_ref() {
            w.window().set_fullscreen(!w.window().is_fullscreen());
        }
    }
}
