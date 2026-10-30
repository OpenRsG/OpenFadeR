//! The PREVIEW button (right strip) and the Preview column's strips ([`super::strip`]): decode the chosen track on a worker
//! (straight to a 16-bit track) and play it on the browser preview player ([`openfader::audio::preview`]: heard on the headphone feed, level =
//! Preferences > Audio > Preview Volume). A second press of the button stops it; the button lights while a decode is pending
//! or the preview plays. The button starts from the beginning, a click on a strip from the clicked point (or its cue).

use super::{View, strip};
use crate::app::{App, BrowserState};
use openfader::audio;
use slint::{ComponentHandle, Timer, TimerMode};
use std::{
    cell::RefCell,
    path::PathBuf,
    rc::Rc,
    sync::{Arc, mpsc::TryRecvError},
    time::Duration,
};

/// A decode in flight and the position (ms) the preview starts at once it is done.
type Pending = (audio::DecodeJob<(audio::Track16, String)>, u32);

/// Decodes `file`; the preview starts at `from_ms` when the audio is there.
fn start(app: &App, pending: &RefCell<Option<Pending>>, file: PathBuf, from_ms: u32) {
    audio::live_bus().iter().for_each(|b| b.preview.unload());
    *pending.borrow_mut() = Some((audio::decode_track16_async(file), from_ms));
    let state = app.global::<BrowserState>();
    state.set_preview_playing(true);
    state.set_status(tr!("preview: decoding…").into());
}

/// Connects the button and the strips; keep the returned timer alive as long as the window.
pub(super) fn wire(app: &App, view: &Rc<RefCell<View>>) -> Timer {
    let pending: Rc<RefCell<Option<Pending>>> = Rc::default();
    let (p, weak, v) = (pending.clone(), app.as_weak(), view.clone());
    app.global::<BrowserState>().on_preview_toggled(move || {
        let app = weak.unwrap();
        let state = app.global::<BrowserState>();
        if state.get_preview_playing() {
            audio::live_bus().iter().for_each(|b| b.preview.unload());
            *p.borrow_mut() = None;
            state.set_preview_playing(false);
            return;
        }
        match v.borrow().selected.as_ref().and_then(|t| t.file.clone()) {
            Some(file) => start(&app, &p, file, 0),
            None => state.set_status(tr!("select a track with a file to preview").into()),
        }
    });
    let (p, weak, v) = (pending.clone(), app.as_weak(), view.clone());
    app.global::<BrowserState>().on_preview_seek(move |row, x| {
        let app = weak.unwrap();
        let Some((t, from_ms)) = usize::try_from(row)
            .ok()
            .and_then(|row| strip::click(&v.borrow(), row, x))
        else {
            return;
        };
        match t.file {
            Some(file) => start(&app, &p, file, from_ms),
            None => app
                .global::<BrowserState>()
                .set_status(tr!("this track has no file to preview").into()),
        }
    });
    let weak = app.as_weak();
    let timer = Timer::default();
    // 250 ms: it only picks up a finished decode and the end of the playback, which no one times to a tenth of a second.
    timer.start(TimerMode::Repeated, Duration::from_millis(250), move || {
        let Some(app) = weak.upgrade() else { return };
        let state = app.global::<BrowserState>();
        let bus = audio::live_bus();
        let mut slot = pending.borrow_mut();
        if let Some((rx, from_ms)) = slot.as_ref() {
            match rx.try_recv() {
                Err(TryRecvError::Empty) => return,
                Ok(Ok((track, _))) => match &bus {
                    Some(b) => {
                        b.preview.load(Arc::new(track));
                        b.preview.seek_ms(*from_ms);
                        b.preview.play();
                        state.set_status(tr!("previewing on the headphones").into());
                    }
                    None => state.set_status(tr!("preview: no audio output").into()),
                },
                Ok(Err(e)) => state.set_status(tr!("preview: {}", e).into()),
                Err(TryRecvError::Disconnected) => {}
            }
            *slot = None;
        }
        if let Some(b) = &bus
            && !b.preview.is_playing()
        {
            b.preview.unload();
        }
        state.set_preview_playing(bus.is_some_and(|b| b.preview.is_playing()));
    });
    timer
}
