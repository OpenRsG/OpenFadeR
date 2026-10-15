//! File drops winit reports itself (`HoveredFile` / `DroppedFile` / `HoveredFileCancelled`: X11, Windows and macOS; not
//! Wayland, see [`super::wayland`]). winit sends one event per file; the files of one drop arrive together and are handed
//! on as one batch once the event loop is idle again.

use super::{dropped, hovering};
use crate::app::App;
use slint::ComponentHandle;
use slint::winit_030::{EventResult, WinitWindowAccessor, winit::event::WindowEvent};
use std::{cell::RefCell, path::PathBuf, rc::Rc};

pub(super) fn wire(app: &App) {
    let batch: Rc<RefCell<Vec<PathBuf>>> = Rc::default();
    let weak = app.as_weak();
    app.window().on_winit_window_event(move |_, event| {
        match event {
            WindowEvent::HoveredFile(_) => {
                if let Some(app) = weak.upgrade() {
                    hovering(&app, true);
                }
            }
            WindowEvent::HoveredFileCancelled => {
                if let Some(app) = weak.upgrade() {
                    hovering(&app, false);
                }
            }
            WindowEvent::DroppedFile(path) => {
                let first = batch.borrow().is_empty();
                batch.borrow_mut().push(path.clone());
                if first {
                    let (batch, weak) = (batch.clone(), weak.clone());
                    slint::Timer::single_shot(std::time::Duration::ZERO, move || {
                        let paths = std::mem::take(&mut *batch.borrow_mut());
                        if let Some(app) = weak.upgrade() {
                            dropped(&app, paths);
                        }
                    });
                }
            }
            _ => {}
        }
        EventResult::Propagate
    });
}
