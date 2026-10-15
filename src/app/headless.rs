//! Tests that need window roots (a component's layout, its globals) but no display: [`init`] makes every window a software-rendered
//! one that draws nowhere.

use slint::platform::{
    Platform, WindowAdapter,
    software_renderer::{MinimalSoftwareWindow, RepaintBufferType},
};
use std::rc::Rc;

struct Headless;

impl Platform for Headless {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer))
    }
}

/// Installs the platform (once per process; later calls are no-ops).
pub fn init() {
    slint::platform::set_platform(Box::new(Headless)).ok();
}
