//! Fetch Missing Artwork: every album of the list on screen that shows no picture gets the first one the plugins offer
//! ([`artwork::Fetch::missing`]). One run at a time; the status line counts the lookups while it goes.

use super::{changed, plugins, say, Ctx};
use crate::app::{browser, library::settings};
use openfader::artwork::{self, Fetch, Gate};
use slint::ComponentHandle;
use std::{
    rc::Rc,
    sync::atomic::{AtomicBool, Ordering},
};

static RUNNING: AtomicBool = AtomicBool::new(false);

pub fn fetch_missing(c: &Rc<Ctx>) {
    let tracks = browser::context().list;
    let plugins = plugins();
    if plugins.is_empty() {
        return c.say(tr!(
            "no artwork plugin is switched on (Preferences > Plugins: cover-art)"
        ));
    }
    let Some(dir) = artwork::cache_dir() else {
        return c.say(tr!("no data folder"));
    };
    if RUNNING.swap(true, Ordering::SeqCst) {
        return c.say(tr!("artwork is being fetched already"));
    }
    let (store, weak) = (c.store.clone(), c.app().as_weak());
    let embed = settings::get().artwork_embed && !settings::get().protect;
    c.spawn(
        move || {
            let run = Fetch {
                store: &store,
                plugins,
                gate: Gate::polite(),
                dir: &dir,
            };
            let mut progress = |done: usize, total: usize| {
                let text = tr!("fetching artwork {}/{}", done + 1, total);
                let _ = weak.upgrade_in_event_loop(move |app| {
                    browser::loading(&app, &text, Some(done as f32 / total as f32));
                });
                true
            };
            let mut write = |t: &openfader::library::Track, bytes: &[u8]| {
                if let Some(f) = t.file.as_deref().filter(|_| embed) {
                    let _ = artwork::embed(f, bytes);
                }
            };
            let out = run.missing(&tracks, &mut progress, &mut write);
            let _ = weak.upgrade_in_event_loop(|app| browser::loading(&app, "", None));
            out
        },
        |c, out| {
            RUNNING.store(false, Ordering::SeqCst);
            changed(c);
            let mut msg = tr!(
                "artwork: {} found, {} not found, {} already had one, {} failed",
                out.found,
                out.missed,
                out.skipped,
                out.failed
            );
            if let Some(e) = out.error {
                msg = format!("{msg} ({e})");
            }
            say(&c.app(), &msg);
        },
    );
}
