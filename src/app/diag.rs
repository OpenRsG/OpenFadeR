//! Help > Report a Problem ↔ ui/report.slint (`ReportState`), the offer at start after a crash, and what only the app can tell
//! the report (`openfader::diag`, docs/diagnostics.md): the renderer and GPU ([`graphics`], from the window's first frame), the
//! audio output ([`wire`]'s timer), the controller ports and mapping ([`controller`], from the MIDI session), the plugins and the
//! settings that differ from the defaults.

use super::{App, ReportState, library, prefs, toast};
use openfader::diag;
use slint::{ComponentHandle, Timer, TimerMode};
use std::ffi::{CStr, c_char, c_void};
use std::{cell::RefCell, path::PathBuf, time::Duration};

/// How often the audio facts are read again (the output moves when a controller connects or the settings change).
const AUDIO_EVERY: Duration = Duration::from_secs(3);

thread_local! {
    /// The crash report the dialog shows, until it is closed (then it is marked seen).
    static OFFERED: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

/// What the report knows of the renderer: the OpenGL driver's own words, or the name of the API.
pub fn graphics(api: &slint::GraphicsAPI) {
    let (renderer, gpu) = match api {
        slint::GraphicsAPI::NativeOpenGL { get_proc_address } => {
            ("OpenGL (femtovg)".to_owned(), gl_driver(get_proc_address))
        }
        other => (format!("{other:?}"), "unknown".to_owned()),
    };
    diag::set("renderer", renderer);
    diag::set("gpu", gpu);
}

/// The window has no rendering notifier: Slint draws with the CPU.
pub fn software(why: &str) {
    diag::set("renderer", format!("software ({why})"));
    diag::set("gpu", "none");
}

/// `GL_RENDERER` and `GL_VERSION` through the driver's `glGetString`.
fn gl_driver(get: &dyn Fn(&CStr) -> *const c_void) -> String {
    const GL_RENDERER: u32 = 0x1F01;
    const GL_VERSION: u32 = 0x1F02;
    let f = get(c"glGetString");
    if f.is_null() {
        return "unknown".into();
    }
    // SAFETY: glGetString has this signature in every OpenGL and OpenGL ES; the context is current while the window sets up.
    let gl: unsafe extern "system" fn(u32) -> *const c_char = unsafe { std::mem::transmute(f) };
    let text = |name| {
        let p = unsafe { gl(name) };
        // SAFETY: a non-null result is the driver's NUL-terminated string.
        (!p.is_null()).then(|| unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned())
    };
    format!(
        "{} (OpenGL {})",
        text(GL_RENDERER).unwrap_or_else(|| "unknown".into()),
        text(GL_VERSION).unwrap_or_else(|| "unknown".into())
    )
}

/// The MIDI input ports and the mapping the session loaded (`None` = no mapping file for them).
pub fn controller(ports: &[String], mapping: Option<&str>, bindings: usize) {
    diag::set("controller_ports", if ports.is_empty() { "none".to_owned() } else { ports.join("; ") });
    diag::set("controller_mapping", mapping.map_or("none".to_owned(), |m| format!("{m} ({bindings} bindings)")));
}

fn audio() {
    let s = openfader::audio::status();
    let note = s.notes.iter().chain(&s.error).cloned().collect::<Vec<_>>().join("; ");
    diag::set("audio_device", if s.device.is_empty() { "none" } else { &s.device });
    diag::set("audio_source", if s.source.is_empty() { "none" } else { s.source });
    diag::set("sample_rate", s.rate);
    diag::set("buffer", s.buffer.map_or("automatic".to_owned(), |b| format!("{b} frames")));
    diag::set("channels", s.channels);
    diag::set("audio_notes", note);
}

fn settings(p: &prefs::Prefs) {
    diag::set_settings(diag::settings_diff(&p.to_text(), &prefs::Prefs::default().to_text()));
}

fn plugins() {
    let r = openfader::plugin::registry();
    let line = |p: &openfader::plugin::Plugin, on: &str| {
        let v = &p.manifest.version;
        format!("{}: {} ({on})", p.manifest.name, if v.is_empty() { "no version" } else { v })
    };
    let on = |p: &openfader::plugin::Plugin| if p.builtin.is_some() { "built in" } else { "on" };
    let all = r.plugins.iter().map(|p| line(p, on(p))).chain(r.disabled.iter().map(|p| line(p, "off")));
    diag::set_plugins(all.collect::<Vec<_>>().join("\n"));
}

/// Shows the dialog with `report`; a crash report from an earlier run is remembered until it is closed.
fn open(app: &App, report: String, offered: Option<PathBuf>) {
    let s = app.global::<ReportState>();
    s.set_crashed(offered.is_some());
    OFFERED.set(offered);
    s.set_report(report.into());
    s.set_visible(true);
}

/// Wires the dialog and the session facts; offers the crash report of an earlier run. Keep the timer.
pub fn wire(app: &App) -> Timer {
    // The order of the first `set` calls is the order in the report.
    diag::set("renderer", "not started");
    diag::set("gpu", "unknown");
    controller(&[], None, 0);
    audio();
    plugins();
    prefs::with(settings);
    prefs::on_change(settings);
    let timer = Timer::default();
    timer.start(TimerMode::Repeated, AUDIO_EVERY, audio);
    // To see the crash report and the offer without a bug at hand (docs/diagnostics.md).
    if std::env::var_os("OPENFADER_DEBUG_PANIC").is_some() {
        Timer::single_shot(Duration::from_secs(3), || panic!("forced by OPENFADER_DEBUG_PANIC"));
    }

    let s = app.global::<ReportState>();
    let weak = app.as_weak();
    s.on_show(move || {
        if let Some(app) = weak.upgrade() {
            open(&app, diag::report(), None);
        }
    });
    let weak = app.as_weak();
    s.on_copied(move || {
        if let Some(app) = weak.upgrade() {
            toast::show(&app, tr!("report copied to the clipboard"));
        }
    });
    s.on_open_logs(|| {
        if let Some(dir) = diag::dir() {
            let _ = std::fs::create_dir_all(&dir);
            library::show_in_folder(&dir);
        }
    });
    let weak = app.as_weak();
    s.on_open_issue(move || {
        let Some(app) = weak.upgrade() else { return };
        let url = diag::issue_url(&app.global::<ReportState>().get_report());
        match library::open_external(&url) {
            Ok(()) => toast::show(&app, tr!("report copied: paste it into the issue")),
            Err(e) => toast::show(&app, &e),
        }
    });
    let weak = app.as_weak();
    s.on_dismissed(move || {
        if let Some(path) = OFFERED.take() {
            diag::mark_seen(&path);
        }
        if let Some(app) = weak.upgrade() {
            app.global::<ReportState>().set_visible(false);
        }
    });

    if let Some(crash) = diag::pending_crash() {
        open(app, crash.text, Some(crash.path));
    }
    timer
}
