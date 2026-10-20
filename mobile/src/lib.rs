//! Touch-first Slint application over OpenFadeR's shared Rust audio, analysis and Collection.
mod deck;
mod library;
#[cfg(target_os = "android")]
mod platform;
mod tick;

slint::include_modules!();
use openfader::{audio, library::Track};
use slint::{ComponentHandle, Model, ModelRc, Timer, TimerMode, VecModel};
use std::{
    cell::RefCell,
    path::PathBuf,
    rc::Rc,
    sync::{mpsc::Receiver, Arc},
    time::Duration,
};

struct Session {
    decks: [deck::Deck; 2],
    tracks: Arc<Vec<Track>>,
    scanning: Option<Receiver<Result<Arc<Vec<Track>>, String>>>,
    music: PathBuf,
    views: Rc<VecModel<DeckView>>,
    focused: bool,
    import_error: Option<String>,
}

impl Session {
    fn gains(&mut self, app: &App) {
        let mix = app.global::<MixerState>();
        let gains = audio::equal_power(mix.get_crossfade());
        for (i, d) in self.decks.iter_mut().enumerate() {
            if let Some(p) = d.player.as_mut() {
                p.set_volume(d.gain * gains[i]);
                p.bus().set_master(mix.get_master());
                p.bus().set_split(mix.get_split());
                p.set_cue_gain(if mix.get_headphones().row_data(i).unwrap_or(false) {
                    1.0
                } else {
                    0.0
                });
            }
        }
    }

    fn focus(&mut self) -> Result<(), String> {
        let playing = self
            .decks
            .iter()
            .any(|d| d.player.as_ref().is_some_and(audio::Player::is_playing));
        if playing != self.focused {
            #[cfg(target_os = "android")]
            if let Err(e) = platform::set_playing(playing) {
                for d in &mut self.decks {
                    if let Some(p) = &mut d.player {
                        p.pause();
                    }
                }
                return Err(e);
            }
            self.focused = playing;
        }
        Ok(())
    }

    fn search(&self, app: &App, query: &str) {
        let q = query.to_lowercase();
        let rows: Vec<TrackRow> = self
            .tracks
            .iter()
            .enumerate()
            .filter(|(_, t)| {
                q.is_empty()
                    || t.title.to_lowercase().contains(&q)
                    || t.artist.to_lowercase().contains(&q)
            })
            .map(|(i, t)| TrackRow {
                index: i as i32,
                title: t.title.as_str().into(),
                artist: t.artist.as_str().into(),
            })
            .collect();
        app.global::<LibraryState>()
            .set_tracks(ModelRc::new(VecModel::from(rows)));
    }
}

fn say(app: &App, message: &str) {
    app.global::<MobileState>().set_status(message.into());
}

macro_rules! wire {
    ($state:expr, $callback:ident, $session:expr, $app:expr, |$s:ident, $a:ident $(, $arg:ident)*| $body:block) => {{
        let weak = $app.as_weak(); let session = $session.clone();
        $state.$callback(move |$($arg),*| {
            if let Some($a) = weak.upgrade() {
                let $s = &mut *session.borrow_mut();
                $body
            }
        });
    }};
}

pub fn run(music: PathBuf) -> Result<(), String> {
    std::fs::create_dir_all(&music).map_err(|e| format!("Music folder: {e}"))?;
    let mut options = openfader::analysis::options::Options::default();
    options.parts.phrase = false;
    options.parts.vocal = false;
    openfader::analysis::options::set(options);
    let app = App::new().map_err(|e| e.to_string())?;
    app.global::<Palette>()
        .set_color_scheme(slint::language::ColorScheme::Dark);
    app.global::<MobileState>()
        .set_landscape_only(cfg!(target_os = "android"));
    #[cfg(target_os = "android")]
    {
        let weak = app.as_weak();
        app.global::<MobileState>().on_background(move || {
            if let Some(app) = weak.upgrade() {
                if let Err(e) = platform::background() {
                    say(&app, &e);
                }
            }
        });
    }
    let views = Rc::new(VecModel::from(
        (0..2)
            .map(|_| DeckView {
                title: "No track loaded".into(),
                gain: 1.0,
                loop_index: 2,
                cues: ModelRc::new(VecModel::from(vec![false; 4])),
                bpm: "—".into(),
                ..Default::default()
            })
            .collect::<Vec<_>>(),
    ));
    app.global::<DeckState>().set_decks(views.clone().into());
    let session = Rc::new(RefCell::new(Session {
        decks: std::array::from_fn(|_| deck::Deck::new()),
        tracks: Arc::default(),
        scanning: Some(library::scan(music.clone())),
        music,
        views,
        focused: false,
        import_error: None,
    }));
    let st = app.global::<DeckState>();
    wire!(st, on_play, session, app, |s, a, i| {
        if let Some(d) = usize::try_from(i).ok().and_then(|i| s.decks.get_mut(i)) {
            if let Some(p) = &mut d.player {
                if p.is_playing() {
                    p.pause()
                } else {
                    #[cfg(target_os = "android")]
                    if let Err(e) = platform::set_playing(true) {
                        say(&a, &e);
                        return;
                    }
                    p.play()
                }
            }
        }
        if let Err(e) = s.focus() {
            say(&a, &e);
        }
    });
    wire!(st, on_cue, session, app, |s, a, i, set| {
        if let Some(d) = usize::try_from(i).ok().and_then(|i| s.decks.get_mut(i)) {
            if let Err(e) = d.cue(0, if set { 1 } else { 0 }) {
                say(&a, &e);
            }
        }
    });
    wire!(st, on_hot_cue, session, app, |s, a, i, slot, mode| {
        if (0..4).contains(&slot) && let Some(d) = usize::try_from(i).ok().and_then(|i| s.decks.get_mut(i)) {
            if let Err(e) = d.cue(slot as u16 + 1, mode) { say(&a, &e); }
        }
    });
    wire!(st, on_seek, session, app, |s, _a, i, v| {
        if let Some(p) = usize::try_from(i)
            .ok()
            .and_then(|i| s.decks.get_mut(i))
            .and_then(|d| d.player.as_mut())
        {
            p.set_loop(None);
            p.seek((v.clamp(0.0, 1.0) * p.duration_ms() as f32) as u32);
        }
    });
    wire!(st, on_tempo, session, app, |s, _a, i, v| {
        if let Some(p) = usize::try_from(i)
            .ok()
            .and_then(|i| s.decks.get_mut(i))
            .and_then(|d| d.player.as_mut())
        {
            p.set_rate(1.0 + f64::from(v.clamp(-16.0, 16.0)) / 100.0);
        }
    });
    wire!(st, on_key_lock, session, app, |s, _a, i| {
        if let Some(p) = usize::try_from(i)
            .ok()
            .and_then(|i| s.decks.get_mut(i))
            .and_then(|d| d.player.as_mut())
        {
            p.set_key_lock(!p.key_lock());
            let mut v = s.views.row_data(i as usize).unwrap();
            v.key_lock = p.key_lock();
            s.views.set_row_data(i as usize, v);
        }
    });
    wire!(st, on_loop, session, app, |s, a, i, size| {
        if let Some(d) = usize::try_from(i).ok().and_then(|i| s.decks.get_mut(i)) {
            if let Err(e) = d.toggle_loop(size) {
                say(&a, &e);
            }
        }
    });
    wire!(st, on_sync, session, app, |s, a, i| {
        if let Ok(i @ 0..=1) = usize::try_from(i) {
            let other = 1 - i;
            let target = match (&s.decks[i].player, &s.decks[other].player) {
                (Some(p), Some(m)) => deck::sync_target(
                    &s.decks[i].analysis.beats,
                    p.position_ms(),
                    s.decks[i].bpm(),
                    &s.decks[other].analysis.beats,
                    m.position_ms(),
                    s.decks[other].bpm(),
                    m.rate(),
                ),
                _ => Err("Load both decks before syncing".into()),
            };
            match target {
                Ok((rate, pos)) => {
                    let p = s.decks[i].player.as_mut().unwrap();
                    p.set_loop(None);
                    p.set_rate(rate);
                    p.seek(pos);
                    say(&a, "Tempo and beat phase matched to the other deck");
                }
                Err(e) => say(&a, &e),
            }
        }
    });
    wire!(st, on_mix, session, app, |s, a, i, control, value| {
        if let Ok(i @ 0..=1) = usize::try_from(i) {
            let mut v = s.views.row_data(i).unwrap();
            match control {
                0 => {
                    v.gain = value;
                    s.decks[i].gain = value;
                }
                1 => v.low = value,
                2 => v.mid = value,
                3 => v.high = value,
                4 => v.amount = value,
                5 => {
                    v.low = 0.0;
                    v.mid = 0.0;
                    v.high = 0.0;
                    v.amount = 0.0;
                }
                _ => {}
            }
            if let Some(p) = &mut s.decks[i].player {
                p.set_eq(v.low, v.mid, v.high);
                p.cfx().set_knob(v.amount);
            }
            s.views.set_row_data(i, v);
            s.gains(&a);
        }
    });
    wire!(st, on_effect, session, app, |s, _a, i, effect| {
        if let Ok(i @ 0..=1) = usize::try_from(i) {
            if let Some(p) = &s.decks[i].player {
                p.cfx().set_index(effect.clamp(0, 5) as usize);
            }
            let mut v = s.views.row_data(i).unwrap();
            v.effect = effect;
            s.views.set_row_data(i, v);
        }
    });
    wire!(st, on_loop_size, session, app, |s, _a, i, size| {
        if let Ok(i @ 0..=1) = usize::try_from(i) {
            let mut v = s.views.row_data(i).unwrap();
            v.loop_index = size.clamp(0, 4);
            s.views.set_row_data(i, v);
        }
    });
    wire!(st, on_cue_mode, session, app, |s, _a, i, mode| {
        if let Ok(i @ 0..=1) = usize::try_from(i) {
            let mut v = s.views.row_data(i).unwrap();
            v.cue_mode = mode;
            s.views.set_row_data(i, v);
        }
    });
    let mixer = app.global::<MixerState>();
    wire!(mixer, on_change_crossfade, session, app, |s, a, v| {
        a.global::<MixerState>().set_crossfade(v);
        s.gains(&a);
    });
    wire!(mixer, on_change_master, session, app, |s, a, v| {
        a.global::<MixerState>().set_master(v);
        s.gains(&a);
    });
    wire!(mixer, on_change_split, session, app, |s, a, on| {
        a.global::<MixerState>().set_split(on);
        s.gains(&a);
    });
    wire!(mixer, on_change_headphones, session, app, |s, a, i, on| {
        let mut phones = [false; 2];
        for (k, p) in phones.iter_mut().enumerate() {
            *p = a
                .global::<MixerState>()
                .get_headphones()
                .row_data(k)
                .unwrap_or(false);
        }
        if let Ok(i @ 0..=1) = usize::try_from(i) {
            phones[i] = on;
        }
        a.global::<MixerState>()
            .set_headphones(ModelRc::new(VecModel::from(phones.to_vec())));
        s.gains(&a);
    });
    let lib = app.global::<LibraryState>();
    wire!(lib, on_search, session, app, |s, a, q| {
        s.search(&a, &q);
    });
    wire!(lib, on_import_music, session, app, |_s, a| {
        _s.import_error = None;
        #[cfg(target_os = "android")]
        match platform::import() {
            Ok(()) => {
                a.global::<LibraryState>().set_busy(true);
                say(&a, "Choose audio files to copy into OpenFadeR");
            }
            Err(e) => say(&a, &e),
        }
        #[cfg(not(target_os = "android"))]
        {
            _s.scanning = Some(library::scan(_s.music.clone()));
            say(&a, "Rescanning the preview music folder");
        }
    });
    wire!(lib, on_load, session, app, |s, a, i, row| {
        if let (Ok(i @ 0..=1), Some(track)) = (
            usize::try_from(i),
            usize::try_from(row)
                .ok()
                .and_then(|r| s.tracks.get(r))
                .cloned(),
        ) {
            if let Err(e) = s.decks[i].start_load(track) {
                say(&a, &e);
            } else {
                say(&a, "Loading audio and detecting its beat grid…");
            }
        }
    });
    let timer = Timer::default();
    let weak = app.as_weak();
    let ticking = session.clone();
    timer.start(TimerMode::Repeated, Duration::from_millis(50), move || {
        if let Some(app) = weak.upgrade() {
            ticking.borrow_mut().tick(&app);
        }
    });
    app.run().map_err(|e| e.to_string())?;
    #[cfg(target_os = "android")]
    platform::set_playing(false)?;
    Ok(())
}

#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
fn android_main(app: slint::android::AndroidApp) {
    let result = (|| -> Result<(), String> {
        let root = platform::init(&app)?;
        slint::android::init(app).map_err(|e| e.to_string())?;
        run(root.join("Music"))
    })();
    if let Err(e) = result {
        eprintln!("OpenFadeR startup: {e}");
    }
}
