//! Tests that need window roots (a component's layout, its globals) but no display: [`init`] makes every window a software-rendered
//! one that draws nowhere.

use slint::platform::{
    software_renderer::{MinimalSoftwareWindow, RepaintBufferType},
    Platform, WindowAdapter,
};
use std::{
    cell::{Cell, RefCell},
    rc::{Rc, Weak},
};

struct Headless;

thread_local! {
    static LAST_WINDOW: RefCell<Weak<MinimalSoftwareWindow>> = RefCell::default();
    static REUSED: Cell<bool> = const { Cell::new(false) };
}

impl Platform for Headless {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        let window = MinimalSoftwareWindow::new(if REUSED.get() {
            RepaintBufferType::ReusedBuffer
        } else {
            RepaintBufferType::NewBuffer
        });
        LAST_WINDOW.with(|last| *last.borrow_mut() = Rc::downgrade(&window));
        Ok(window)
    }
}

/// Installs the platform (once per process; later calls are no-ops).
pub fn init() {
    slint::platform::set_platform(Box::new(Headless)).ok();
}

/// Windows created on this thread from now on repaint only what changed into the buffer they were last drawn to (what a
/// real software window does); the default repaints everything into a new buffer. For timing benches.
pub fn repaint_changes_only() {
    REUSED.set(true);
}

/// The most recently created window on this test thread, for pixel-level checks of the actual UI.
pub fn last_window() -> Rc<MinimalSoftwareWindow> {
    LAST_WINDOW.with(|last| {
        last.borrow()
            .upgrade()
            .expect("create a window before rendering")
    })
}
