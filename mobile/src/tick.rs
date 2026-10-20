//! UI refresh and completion polling; the audio callback stays in the shared core.
use super::*;

impl Session {
    pub(super) fn tick(&mut self, app: &App) {
        #[cfg(target_os = "android")]
        {
            if platform::take_interrupted() {
                for d in &mut self.decks {
                    if let Some(p) = &mut d.player {
                        p.pause();
                    }
                }
                say(app, "Playback paused by Android; press Play to resume");
            }
            for event in platform::drain_imports() {
                match event {
                    platform::ImportEvent::File => {}
                    platform::ImportEvent::Error(e) => {
                        say(app, &e);
                        self.import_error = Some(e);
                    }
                    platform::ImportEvent::Finished => {
                        app.global::<LibraryState>().set_busy(false);
                        self.scanning = Some(library::scan(self.music.clone()));
                    }
                }
            }
        }
        if let Some(rx) = &self.scanning {
            match rx.try_recv() {
                Ok(r) => {
                    self.scanning = None;
                    match r {
                        Ok(tracks) => {
                            self.tracks = tracks;
                            self.search(app, &app.global::<LibraryState>().get_query());
                            if let Some(e) = &self.import_error {
                                say(app, &format!("{} tracks · {e}", self.tracks.len()));
                            } else {
                                say(
                                    app,
                                    &format!("{} tracks · Load A or B to start", self.tracks.len()),
                                );
                            }
                        }
                        Err(e) => say(app, &format!("Library: {e}. Try importing again.")),
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.scanning = None;
                    say(app, "Library scan stopped; import music to retry");
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        for i in 0..2 {
            let loaded = self.decks[i].poll();
            let d = &mut self.decks[i];
            let mut v = self.views.row_data(i).unwrap();
            v.loading = d.pending.is_some();
            if let Some(r) = loaded {
                match r {
                    Ok(image) => {
                        let t = d.track.as_ref().unwrap();
                        v.title = t.title.as_str().into();
                        v.artist = t.artist.as_str().into();
                        v.key = d.analysis.key.as_deref().unwrap_or(&t.key).into();
                        v.ready = true;
                        v.wave = image;
                        v.key_lock = true;
                        say(
                            app,
                            &format!("Deck {} loaded", if i == 0 { "A" } else { "B" }),
                        );
                    }
                    Err(e) => say(app, &format!("Load failed: {e}")),
                }
            }
            if let Some(p) = d.player.as_ref() {
                v.playing = p.is_playing();
                v.looping = p.looping().is_some();
                v.duration = p.duration_ms() as f32;
                let pos = p.position_ms();
                v.position = if v.duration > 0.0 {
                    (pos as f32 / v.duration).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                v.level = p.level();
                v.tempo = ((p.rate() - 1.0) * 100.0) as f32;
                let secs = (pos / 1000.0) as u32;
                let total = p.duration_ms() / 1000;
                if secs != d.last_second {
                    d.last_second = secs;
                    v.time = format!(
                        "{}:{:02} / {}:{:02}",
                        secs / 60,
                        secs % 60,
                        total / 60,
                        total % 60
                    )
                    .into();
                }
                let bpm = d.bpm() * p.rate();
                if bpm != d.last_bpm {
                    d.last_bpm = bpm;
                    v.bpm = if bpm > 0.0 {
                        format!("{bpm:.1}").into()
                    } else {
                        "—".into()
                    };
                }
                p.cfx().set_bpm(bpm as f32);
            }
            let cues: [bool; 4] =
                std::array::from_fn(|k| d.analysis.cues.iter().any(|c| c.hot_cue == k as u16 + 1));
            if (0..4).any(|k| v.cues.row_data(k) != Some(cues[k])) {
                v.cues = ModelRc::new(VecModel::from(cues.to_vec()));
            }
            self.views.set_row_data(i, v);
        }
        self.gains(app);
        if let Err(e) = self.focus() {
            say(app, &e);
        }
    }
}
