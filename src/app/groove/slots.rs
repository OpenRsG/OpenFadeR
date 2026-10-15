//! The actions on a slot: loading a track or a preset, PLAY, DRUM CAPTURE and Drum Presets.

use super::{
    Entry, Groove, GrooveState, Loop, Pending, SAVE_AS, SLOTS, State, WAVE_H, WAVE_W, cap, capture,
    groove, preset, slot_name, source, wave,
};
use crate::app::{
    deck::fmt_ms,
    prefs::{self, extensions::DrumSource},
};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use std::{
    path::PathBuf,
    sync::mpsc::{TryRecvError, channel},
};

impl Groove {
    /// Puts `drum` into slot `i` of deck `d` with what its tile shows.
    fn put(&self, d: usize, i: usize, drum: groove::Drum, entry: Option<Entry>, title: String) {
        let info = format!("{:.1} BPM · {} BEATS", drum.bpm, drum.beats);
        let l = Loop {
            title: title.into(),
            info: info.into(),
            wave: wave::image(&drum, WAVE_W, WAVE_H),
            beats: drum.beats as i32,
            entry,
        };
        if self.with(d, |c| c.load(i, Some(drum))).is_some() {
            self.cfg.borrow_mut()[d].loops[i] = Some(l);
        }
    }

    pub(super) fn clear(&self, d: usize, i: usize) {
        if self.locked(d, i) {
            return;
        }
        if self.with(d, |c| c.load(i, None)).is_some() {
            self.cfg.borrow_mut()[d].loops[i] = None;
        }
    }

    /// Decodes `jobs` (slot, entry) for deck `d` on a worker thread; [`poll`](Self::poll) takes the results.
    fn spawn(&self, d: usize, jobs: Vec<(usize, Entry)>) {
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            for (slot, e) in jobs {
                let r = source::load(&e.file, e.bpm);
                if tx.send((slot, e, r)).is_err() {
                    return; // the panel is gone
                }
            }
        });
        self.pending.borrow_mut().push(Pending { deck: d, rx });
    }

    /// ⤓ on a tile: the browser's selected track into the slot.
    pub(super) fn load_track(&self, d: usize, i: usize) {
        if self.locked(d, i) {
            return;
        }
        let Some(t) = self.track.borrow().clone() else {
            return self.say("select a track in the library first");
        };
        let Some(file) = t.file.clone() else {
            return self.say("this track has no audio file");
        };
        self.say(format!("slot {}: decoding {}…", slot_name(i), t.title));
        let entry = Entry {
            file,
            bpm: t.bpm_x100 as f32 / 100.0,
            title: t.title,
        };
        self.spawn(d, vec![(i, entry)]);
    }

    /// Takes the finished loads; drops workers that are done.
    pub(super) fn poll(&self) {
        let mut got = Vec::new();
        self.pending.borrow_mut().retain(|p| {
            loop {
                match p.rx.try_recv() {
                    Ok(r) => got.push((p.deck, r)),
                    Err(TryRecvError::Empty) => break true,
                    Err(TryRecvError::Disconnected) => break false,
                }
            }
        });
        for (d, (slot, mut entry, r)) in got {
            match r {
                Ok(drum) => {
                    entry.bpm = drum.bpm;
                    let title = entry.title.clone();
                    self.say(format!(
                        "slot {}: {title} · {:.1} BPM · {} beats",
                        slot_name(slot),
                        drum.bpm,
                        drum.beats
                    ));
                    self.put(d, slot, drum, Some(entry), title);
                }
                Err(e) => self.say(format!("slot {}: {} — {e}", slot_name(slot), entry.title)),
            }
        }
    }

    /// The tile of slot `i` of deck `d` was clicked: DRUM CAPTURE lands here, else PLAY / stop.
    pub(super) fn pressed(&self, d: usize, i: usize) {
        if let Some((from, beats)) = self.armed() {
            return self.capture_into(from, beats, d, i);
        }
        let Some((state, has, multi, stems)) = self.decks[d].with_player(|p| {
            let c = p.groove();
            (c.state(i), c.drum(i).is_some(), c.multi(), p.has_stems())
        }) else {
            return;
        };
        if !has {
            return self.say(format!(
                "slot {} is empty: ⤓ loads the selected track, PRESET loads eight",
                slot_name(i)
            ));
        }
        if state == State::Idle {
            if !self.decks[d].playing() {
                return self.say(format!(
                    "deck {} is not playing: DRUM SWAP plays in time with it",
                    d + 1
                ));
            }
            if self.decks[d].with_analysis(|a, _| a.beats.len() < 2) {
                return self.say("this track has no beat grid: DRUM SWAP cannot keep time");
            }
            if !multi && !stems {
                self.say("this deck has no stems: the loop plays over its drums");
            }
        }
        self.with(d, |c| c.press(i));
    }

    /// DRUM CAPTURE armed on `from` is dropped on slot `i` of deck `d`.
    fn capture_into(&self, from: usize, beats: u32, d: usize, i: usize) {
        if self.locked(d, i) {
            return;
        }
        let original = prefs::with(|p| p.extensions.drum_source == DrumSource::Original);
        let drum = match capture::take(&self.decks[from], beats, original) {
            Ok(drum) => drum,
            Err(e) => return self.say(format!("DRUM CAPTURE: {e}")),
        };
        let title = self.decks[from]
            .track()
            .map_or_else(|| format!("deck {}", from + 1), |(_, t)| t.title);
        let saved = capture::save(&drum, &title);
        let entry = saved.as_ref().ok().map(|p| Entry {
            file: p.clone(),
            bpm: drum.bpm,
            title: title.clone(),
        });
        let what = if original {
            "the track"
        } else {
            "the DRUMS stem"
        };
        self.say(match &saved {
            Ok(p) => format!(
                "DRUM CAPTURE: {beats} beats of {what} of deck {} into slot {} · {}",
                from + 1,
                slot_name(i),
                p.display()
            ),
            Err(e) => format!(
                "DRUM CAPTURE: {beats} beats of {what} into slot {} (not saved: {e})",
                slot_name(i)
            ),
        });
        self.put(d, i, drum, entry, title);
        self.cfg.borrow_mut()[from].cap = None;
    }

    pub(super) fn capture_clicked(&self, d: usize) {
        let beats = {
            let mut cfg = self.cfg.borrow_mut();
            if cfg.iter().any(|c| c.cap.is_some()) {
                cfg.iter_mut().for_each(|c| c.cap = None);
                drop(cfg);
                return self.say("DRUM CAPTURE off");
            }
            cfg[d].cap_beats
        };
        match self.decks[d].capture_range(beats) {
            Ok((from, _)) => {
                self.cfg.borrow_mut()[d].cap = Some(beats);
                self.say(format!(
                    "DRUM CAPTURE: {beats} beats from {} of deck {}: pick a slot",
                    fmt_ms(from as u32),
                    d + 1
                ));
            }
            Err(e) => self.say(format!("DRUM CAPTURE: {e}")),
        }
    }

    pub(super) fn range_clicked(&self, d: usize) {
        let beats = {
            let mut cfg = self.cfg.borrow_mut();
            let c = &mut cfg[d];
            c.cap_beats = cap::next_range(c.cap_beats);
            if c.cap.is_some() {
                c.cap = Some(c.cap_beats);
            }
            c.cap_beats
        };
        self.say(format!("DRUM CAPTURE range: {beats} beats"));
    }

    /// Reads the factory presets of the PREFS pack and the user's presets into the PRESET list.
    pub(super) fn refresh_presets(&self) {
        let pack = prefs::with(|p| p.extensions.drum_pack.clone());
        let mut p = if pack.is_empty() {
            Vec::new()
        } else {
            preset::factory(&PathBuf::from(&pack))
        };
        p.extend(preset::user());
        *self.pack.borrow_mut() = pack;
        let mut names: Vec<SharedString> = p.iter().map(|p| p.name.as_str().into()).collect();
        names.push(SAVE_AS.into());
        *self.presets.borrow_mut() = p;
        if let Some(a) = self.state() {
            a.global::<GrooveState>()
                .set_presets(ModelRc::new(VecModel::from(names)));
        }
    }

    /// Entry `n` of the PRESET list: its eight loops into the slots, or (the last entry) the save dialog.
    pub(super) fn preset_chosen(&self, d: usize, n: usize) {
        let Some(p) = self.presets.borrow().get(n).cloned() else {
            if let Some(a) = self.state() {
                a.global::<GrooveState>().set_saving(d as i32);
            }
            return;
        };
        let mut jobs = Vec::new();
        for (i, s) in p.slots.iter().enumerate().take(SLOTS) {
            match s {
                Some(e) if !self.locked(d, i) => jobs.push((i, e.clone())),
                None => self.clear(d, i),
                _ => {}
            }
        }
        self.say(format!(
            "Drum Preset {}: loading {} loops…",
            p.name,
            jobs.len()
        ));
        self.spawn(d, jobs);
    }

    pub(super) fn preset_saved(&self, d: usize, name: &str) {
        let slots = self.cfg.borrow()[d]
            .loops
            .iter()
            .map(|l| l.as_ref().and_then(|l| l.entry.clone()))
            .collect();
        match preset::save(name, slots) {
            Ok(p) => {
                self.say(format!("Drum Preset {} saved", p.name));
                if let Some(a) = self.state() {
                    a.global::<GrooveState>().set_saving(-1);
                }
                self.refresh_presets();
            }
            Err(e) => self.say(e),
        }
    }
}
