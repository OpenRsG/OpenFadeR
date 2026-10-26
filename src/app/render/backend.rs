//! Waveform image composition uses Slint's renderer, never a second graphics context.
//! AUTO rejects software GL implementations; GPU explicitly allows those for diagnosis.
//! A default-renderer startup failure requests one clean restart with Slint's native software renderer.
use crate::app::{
    App, DeckState,
    prefs::{self, view::WaveRenderer},
};
use slint::{ComponentHandle, GraphicsAPI};
use std::{cell::Cell, time::Duration};

thread_local! {
    static AVAILABLE: Cell<(bool, bool)> = const { Cell::new((false, false)) };
    static ENABLED: Cell<bool> = const { Cell::new(false) };
    static FIRST_FRAME: Cell<bool> = const { Cell::new(false) };
    static WINIT: Cell<bool> = const { Cell::new(false) };
    static WINDOW_FAILED: Cell<bool> = const { Cell::new(false) };
}

pub fn select() -> Result<(), slint::PlatformError> {
    slint::BackendSelector::new()
        .with_winit_window_attributes_hook(|attributes| {
            WINIT.set(true);
            attributes
        })
        .select()
}

pub fn watch(app: &App) {
    use slint::winit_030::WinitWindowAccessor;
    if !WINIT.get() {
        return;
    }
    let weak = app.as_weak();
    // shortcut: windowless startup is limited to 2 s; remove when Slint propagates deferred creation failures.
    slint::Timer::single_shot(Duration::from_secs(2), move || {
        if let Some(app) = weak.upgrade()
            && app.window().is_visible()
            && !app.window().has_winit_window()
        {
            WINDOW_FAILED.set(true);
            let _ = slint::quit_event_loop();
        }
    });
}

pub fn check_startup() -> Result<(), slint::PlatformError> {
    if WINDOW_FAILED.get() {
        Err("Graphics backend did not create a native window within 2 seconds".into())
    } else {
        Ok(())
    }
}

fn software(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    ["llvmpipe", "softpipe", "software", "swrast", "swiftshader", "gdi generic"]
        .iter().any(|s| name.contains(s))
}

fn selected(mode: WaveRenderer, available: bool, hardware: bool) -> bool {
    available && (mode == WaveRenderer::Gpu || (mode == WaveRenderer::Auto && hardware))
}

pub fn gpu() -> bool {
    ENABLED.get()
}

/// AfterRendering precedes presentation; leave startup retry available until the frame returns to the event loop.
pub fn first_frame() {
    slint::Timer::single_shot(Duration::ZERO, || FIRST_FRAME.set(true));
}

fn retry_allowed(backend: Option<&std::ffi::OsStr>, first_frame: bool) -> bool {
    !first_frame && backend.is_none_or(|name| name.is_empty())
}

/// Slint cannot replace its platform after selection; retry through the app's clean process restart.
pub fn retry_software(error: &slint::PlatformError) -> bool {
    if !retry_allowed(
        std::env::var_os("SLINT_BACKEND").as_deref(),
        FIRST_FRAME.get(),
    ) {
        return false;
    }
    eprintln!("Slint startup failed: {error}; restarting once with SLINT_BACKEND=winit-software");
    true
}

pub fn apply(app: &App) {
    let (available, hardware) = AVAILABLE.get();
    let mode = prefs::with(|p| p.view.waveform_renderer);
    let enabled = selected(mode, available, hardware);
    if ENABLED.replace(enabled) != enabled {
        eprintln!("waveform renderer: {} (requested {mode:?})", if enabled { "GPU image compositor" } else { "CPU" });
    }
    app.global::<DeckState>().set_waveform_gpu(enabled);
}

/// Called only with the renderer's context current. No GL objects or function pointers outlive it.
pub fn setup(api: &GraphicsAPI, app: &App) {
    let hardware = match api {
        GraphicsAPI::NativeOpenGL { get_proc_address } => {
            let p = get_proc_address(c"glGetString");
            if p.is_null() {
                eprintln!("Slint renderer: femtovg ({api:?}), GL_RENDERER unavailable");
                false
            } else {
                // GL_RENDERER is a context-owned NUL-terminated string (OpenGL 2 / GLES 2 and newer).
                let name = unsafe {
                    let get: unsafe extern "system" fn(u32) -> *const std::ffi::c_char = std::mem::transmute(p);
                    let name = get(0x1f01);
                    if name.is_null() { None } else { Some(std::ffi::CStr::from_ptr(name).to_string_lossy().into_owned()) }
                };
                eprintln!("Slint renderer: femtovg ({api:?}), GL_RENDERER={}", name.as_deref().unwrap_or("unknown"));
                name.is_some_and(|name| !software(&name))
            }
        }
        _ => {
            eprintln!("Slint renderer: {api:?}; hardware capability unknown");
            false
        }
    };
    AVAILABLE.set((true, hardware));
    apply(app);
    if !gpu() { eprintln!("waveform renderer: CPU (no hardware compositor selected)"); }
}

pub fn teardown() {
    // The window is already mutably borrowed here; never touch Slint properties during destruction.
    AVAILABLE.set((false, false));
    ENABLED.set(false);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn startup_retry_remains_available_until_the_rendered_frame_returns() {
        crate::app::headless::init();
        FIRST_FRAME.set(false);
        first_frame();
        assert!(retry_allowed(None, FIRST_FRAME.get()));
        slint::platform::update_timers_and_animations();
        assert!(!retry_allowed(None, FIRST_FRAME.get()));
        FIRST_FRAME.set(false);
    }

    #[test]
    fn startup_retry_preserves_explicit_selection_and_never_retries_after_a_frame() {
        use std::ffi::OsStr;
        assert!(retry_allowed(None, false));
        assert!(retry_allowed(Some(OsStr::new("")), false));
        for name in [
            "winit",
            "femtovg",
            "winit-femtovg",
            "software",
            "winit-software",
            "invalid",
        ] {
            assert!(!retry_allowed(Some(OsStr::new(name)), false), "{name}");
        }
        assert!(!retry_allowed(None, true));
        assert!(!retry_allowed(Some(OsStr::new("")), true));
    }
    #[test]
    fn teardown_discards_capabilities_without_accessing_a_window() {
        AVAILABLE.set((true, true));
        ENABLED.set(true);
        teardown();
        assert_eq!(AVAILABLE.get(), (false, false));
        assert!(!gpu());
    }
    #[test]
    fn renderer_setting_parses_and_rejects_unknown_modes() {
        use crate::app::prefs::Section;
        let mut v = prefs::view::View::default();
        assert_eq!(v.waveform_renderer, WaveRenderer::Auto);
        for (text, mode) in [("gpu", WaveRenderer::Gpu), ("cpu", WaveRenderer::Cpu), ("auto", WaveRenderer::Auto)] {
            assert_eq!(v.parse("waveform_renderer", text), Some(true));
            assert_eq!(v.waveform_renderer, mode);
        }
        assert_eq!(v.parse("waveform_renderer", "unknown"), Some(false));
        assert_eq!(v.waveform_renderer, WaveRenderer::Auto);
    }
    #[test]
    fn auto_requires_hardware_and_gpu_falls_back_without_a_graphics_api() {
        for mode in [WaveRenderer::Auto, WaveRenderer::Gpu, WaveRenderer::Cpu] {
            assert!(!selected(mode, false, false));
            assert!(!selected(mode, false, true));
        }
        assert!(!selected(WaveRenderer::Auto, true, false));
        assert!(selected(WaveRenderer::Auto, true, true));
        assert!(selected(WaveRenderer::Gpu, true, false));
        assert!(!selected(WaveRenderer::Cpu, true, true));
        for name in ["llvmpipe (LLVM 20)", "softpipe", "Google SwiftShader", "GDI Generic"] {
            assert!(software(name));
        }
        for name in ["V3D 7.1", "NVIDIA GeForce RTX 3090", "Apple M2"] { assert!(!software(name)); }
    }
}
