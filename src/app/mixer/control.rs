//! The control log of a recording (`<recording>.controls.jsonl`, schema in docs/recording-files.md): one JSON object per
//! line, `{"t": ms, "control": "deck1.play", "value": true}`, for what the DJ did while REC ran.
//!
//! Nothing here touches the audio thread, and the UI thread does no file I/O: every mixer tick (while recording)
//! [`sample`] reads the controls from the UI models (so the mouse, the keyboard, MIDI and plugins are all seen the
//! same way), [`Coalesce`] keeps what changed and lets one control through at most once per [`MIN_GAP_MS`] (its
//! latest value follows on a later tick, so the last position of a moved fader is never lost), and the lines go through
//! a channel to a writer thread ([`Log`]). Presses a model does not show (a hot cue pad) arrive through [`note`].
//! `t` is wall-clock ms since REC was pressed; the `recording.file` lines say when each WAV opened, which ties it to the audio.

use super::sheet::sidecar;
use crate::app::{
    deck::{active, Decks},
    App, DeckState, FxRackState, FxState, MixerState,
};
use serde_json::Value;
use slint::{ComponentHandle, Model};
use std::{
    cell::RefCell,
    collections::HashMap,
    fs::File,
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender},
    thread::JoinHandle,
    time::{Duration, Instant},
};

/// A control is written at most this often: 20 lines a second.
pub const MIN_GAP_MS: u64 = 50;

/// One line of the log.
fn line(t_ms: u64, control: &str, value: &Value) -> String {
    format!(
        "{{\"t\":{t_ms},\"control\":{},\"value\":{value}}}",
        Value::from(control)
    )
}

/// Keeps what each control last wrote and lets only a changed one through, at most once per [`MIN_GAP_MS`].
#[derive(Default)]
pub struct Coalesce {
    sent: HashMap<String, (u64, Value)>,
}

impl Coalesce {
    /// The lines for the controls of `state` that differ from what was last written (every one the first time), at `t_ms`.
    pub fn feed(&mut self, t_ms: u64, state: Vec<(String, Value)>) -> Vec<String> {
        let mut lines = Vec::new();
        for (name, value) in state {
            let due = self
                .sent
                .get(&name)
                .is_none_or(|(at, last)| *last != value && t_ms >= at + MIN_GAP_MS);
            if due {
                lines.push(line(t_ms, &name, &value));
                self.sent.insert(name, (t_ms, value));
            }
        }
        lines
    }
}

thread_local! {
    /// Presses noted by the UI thread's controls since the last tick; `None` while nothing records a log.
    static NOTES: RefCell<Option<Vec<(String, Value)>>> = const { RefCell::new(None) };
}

/// A press the models do not show (a hot cue pad): kept for the log while one is being written, else dropped.
pub fn note(control: String, value: impl Into<Value>) {
    NOTES.with(|n| {
        if let Some(n) = n.borrow_mut().as_mut() {
            n.push((control, value.into()));
        }
    });
}

enum Msg {
    Open(PathBuf),
    Line(String),
}

/// The writer thread: keeps the lines until the file is named, then appends them, flushing every second.
fn write_log(rx: Receiver<Msg>) {
    let mut out: Option<BufWriter<File>> = None;
    let mut held = Vec::new();
    loop {
        let msg = rx.recv_timeout(Duration::from_secs(1));
        let res = match msg {
            Ok(Msg::Open(path)) => File::create(&path).and_then(|f| {
                let mut w = BufWriter::new(f);
                held.drain(..)
                    .try_for_each(|l: String| writeln!(w, "{l}"))?;
                out = Some(w);
                Ok(())
            }),
            Ok(Msg::Line(l)) => match &mut out {
                Some(w) => writeln!(w, "{l}"),
                None => {
                    held.push(l);
                    Ok(())
                }
            },
            Err(RecvTimeoutError::Timeout) => out.as_mut().map_or(Ok(()), |w| w.flush()),
            Err(RecvTimeoutError::Disconnected) => break,
        };
        if let Err(e) = res {
            eprintln!("rec: control log: {e}");
            return;
        }
    }
    if let Some(w) = &mut out {
        if let Err(e) = w.flush() {
            eprintln!("rec: control log: {e}");
        }
    }
}

/// What REC keeps while it logs controls.
pub struct Capture {
    tx: Option<Sender<Msg>>,
    thread: Option<JoinHandle<()>>,
    coalesce: Coalesce,
    started: Instant,
    /// The log file, once the first recording file has named it.
    path: Option<PathBuf>,
}

impl Capture {
    pub fn start() -> Self {
        let (tx, rx) = channel();
        NOTES.with(|n| *n.borrow_mut() = Some(Vec::new()));
        Capture {
            tx: Some(tx),
            thread: std::thread::Builder::new()
                .name("rec-controls".into())
                .spawn(move || write_log(rx))
                .map_err(|e| eprintln!("rec: control log: {e}"))
                .ok(),
            coalesce: Coalesce::default(),
            started: Instant::now(),
            path: None,
        }
    }

    /// Ends the log (the writer flushes and stops); the file it writes, if a recording file ever named it.
    pub fn finish(mut self) -> Option<PathBuf> {
        self.path.take()
    }

    fn send(&self, msg: Msg) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(msg); // the writer ended after an I/O error it reported
        }
    }

    /// The recorder opened `file`: the first one names the log, each is marked in it.
    pub fn file(&mut self, file: &Path) {
        if self.path.is_none() {
            let path = sidecar(file, "controls.jsonl");
            self.send(Msg::Open(path.clone()));
            self.path = Some(path);
        }
        let name = file.file_name().unwrap_or(file.as_os_str());
        let at = self.started.elapsed().as_millis() as u64;
        self.send(Msg::Line(line(
            at,
            "recording.file",
            &Value::from(name.to_string_lossy().as_ref()),
        )));
    }

    /// One mixer tick: what [`sample`] read, and the presses noted since the last one.
    pub fn tick(&mut self, state: Vec<(String, Value)>) {
        let at = self.started.elapsed().as_millis() as u64;
        let pressed = NOTES.with(|n| n.borrow_mut().as_mut().map(std::mem::take));
        let mut lines = self.coalesce.feed(at, state);
        lines.extend(
            pressed
                .into_iter()
                .flatten()
                .map(|(name, value)| line(at, &name, &value)),
        );
        lines.into_iter().for_each(|l| self.send(Msg::Line(l)));
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        NOTES.with(|n| *n.borrow_mut() = None);
        self.tx.take(); // the writer flushes and ends
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// A knob / fader position: three decimals are finer than any control moves.
fn pos(v: f32) -> Value {
    Value::from((f64::from(v) * 1000.0).round() / 1000.0)
}

/// Every control the log covers, read from the UI models.
pub fn sample(app: &App, decks: &Decks) -> Vec<(String, Value)> {
    let mut s: Vec<(String, Value)> = Vec::new();
    let mut put = |name: String, value: Value| s.push((name, value));

    let views = app.global::<DeckState>().get_decks();
    for d in 0..active().min(decks.len()) {
        let Some(v) = views.row_data(d) else { continue };
        let n = d + 1;
        let title = match (v.artist.is_empty(), v.title.is_empty()) {
            (_, true) => String::new(),
            (true, false) => v.title.to_string(),
            (false, false) => format!("{} - {}", v.artist, v.title),
        };
        let cues: Vec<String> = v
            .cues
            .iter()
            .map(|c| format!("{}@{}", c.tag, c.time))
            .collect();
        put(format!("deck{n}.load"), title.into());
        put(format!("deck{n}.play"), v.playing.into());
        put(format!("deck{n}.cue"), v.at_cue.into());
        put(format!("deck{n}.cues"), cues.join(" ").into());
        put(format!("deck{n}.loop"), v.loop_active.into());
        put(format!("deck{n}.loop_beats"), v.loop_beats.into());
        put(format!("deck{n}.sync"), v.synced.into());
        put(format!("deck{n}.master"), v.master.into());
        put(format!("deck{n}.tempo"), pos(v.tempo));
        put(format!("deck{n}.bpm"), pos(v.bpm));
    }

    let mixer = app.global::<MixerState>();
    put("mixer.crossfader".into(), pos(mixer.get_crossfader()));
    if let Some(m) = mixer.get_bus().knobs.row_data(0) {
        put("mixer.master".into(), pos(m));
    }
    let sends = app.global::<FxRackState>().get_sends();
    for (c, row) in mixer.get_channels().iter().enumerate().take(active()) {
        let n = c + 1;
        let knob = |k| pos(row.knobs.row_data(k).unwrap_or_default());
        put(format!("ch{n}.fader"), pos(row.fader));
        put(format!("ch{n}.trim"), knob(0));
        put(format!("ch{n}.eq.hi"), knob(1));
        put(format!("ch{n}.eq.mid"), knob(2));
        put(format!("ch{n}.eq.low"), knob(3));
        put(format!("ch{n}.cfx"), knob(4));
        put(format!("ch{n}.xf"), row.xf.into());
        put(format!("ch{n}.cue"), row.cue.into());
        put(
            format!("ch{n}.send"),
            pos(sends.row_data(c).unwrap_or_default()),
        );
    }

    let fx = app.global::<FxState>();
    let effects = fx.get_effects();
    let name_of = |i: i32| {
        usize::try_from(i)
            .ok()
            .and_then(|i| effects.row_data(i))
            .map_or(Value::Null, |n| n.to_string().into())
    };
    for (u, unit) in fx.get_units().iter().enumerate() {
        let n = u + 1;
        put(format!("fx{n}.assign"), unit.assign.into());
        put(format!("fx{n}.release"), unit.releasing.into());
        put(format!("fx{n}.part"), unit.part.into());
        for (i, slot) in unit.slots.iter().enumerate() {
            let k = i + 1;
            put(format!("fx{n}.{k}.on"), slot.on.into());
            put(format!("fx{n}.{k}.effect"), name_of(slot.effect));
            put(format!("fx{n}.{k}.beat"), slot.beat_text.to_string().into());
            put(format!("fx{n}.{k}.level"), pos(slot.level));
        }
        put(format!("merge{n}.on"), unit.merge_on.into());
        put(
            format!("merge{n}.preset"),
            unit.merge_name.to_string().into(),
        );
        put(format!("merge{n}.knob"), pos(unit.merge_knob));
        put(format!("merge{n}.volume"), pos(unit.merge_volume));
    }
    let racks = app.global::<FxRackState>();
    for (r, rack) in racks.get_racks().iter().enumerate() {
        let name = ["send", "lever", "mic"].get(r).copied().unwrap_or("rack");
        put(format!("{name}.on"), rack.on.into());
        put(
            format!("{name}.effect"),
            rack.names
                .row_data(usize::try_from(rack.effect).unwrap_or(0))
                .map_or(Value::Null, |n| n.to_string().into()),
        );
        put(format!("{name}.level"), pos(rack.level));
        put(format!("{name}.param"), pos(rack.param));
    }
    put("lever.position".into(), pos(racks.get_lever()));
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn lines(c: &mut Coalesce, t: u64, state: &[(&str, Value)]) -> Vec<Value> {
        c.feed(
            t,
            state
                .iter()
                .map(|(n, v)| (n.to_string(), v.clone()))
                .collect(),
        )
        .iter()
        .map(|l| serde_json::from_str(l).expect("a line is JSON"))
        .collect()
    }

    #[test]
    fn the_first_sample_writes_everything_and_a_still_control_nothing() {
        let mut c = Coalesce::default();
        let first = lines(
            &mut c,
            0,
            &[("deck1.play", json!(false)), ("ch1.fader", json!(0.2))],
        );
        assert_eq!(
            first[0],
            json!({"t": 0, "control": "deck1.play", "value": false})
        );
        assert_eq!(first.len(), 2);
        assert!(lines(
            &mut c,
            500,
            &[("deck1.play", json!(false)), ("ch1.fader", json!(0.2))]
        )
        .is_empty());
    }

    #[test]
    fn a_fader_swept_for_a_second_writes_at_most_20_lines_and_ends_on_its_last_value() {
        let mut c = Coalesce::default();
        let mut written = Vec::new();
        // a tick every 33 ms: the fader moves a little each time, then rests for a few ticks
        for tick in 0..40u64 {
            let t = tick * 33;
            let v = (tick.min(30)) as f64 / 30.0;
            written.extend(lines(&mut c, t, &[("ch1.fader", json!(v))]));
        }
        let during = written
            .iter()
            .filter(|l| l["t"].as_u64().unwrap() < 1000)
            .count();
        assert!(
            during <= 20 && during >= 10,
            "{during} lines in the first second"
        );
        assert_eq!(
            written.last().unwrap()["value"],
            json!(1.0),
            "the end of the sweep is never lost"
        );
        let times: Vec<u64> = written.iter().map(|l| l["t"].as_u64().unwrap()).collect();
        assert!(
            times.windows(2).all(|w| w[1] - w[0] >= MIN_GAP_MS),
            "{times:?}"
        );
    }

    #[test]
    fn controls_are_coalesced_one_by_one() {
        let mut c = Coalesce::default();
        lines(&mut c, 0, &[("a", json!(0)), ("b", json!(0))]);
        let got = lines(&mut c, 10, &[("a", json!(1)), ("b", json!(0))]);
        assert!(
            got.is_empty(),
            "a changed 10 ms after its last line: it waits"
        );
        let got = lines(&mut c, 60, &[("a", json!(1)), ("b", json!(1))]);
        assert_eq!(got.len(), 2);
    }

    #[test]
    fn the_writer_holds_lines_until_the_file_is_named_and_notes_reach_the_log() {
        let dir = std::env::temp_dir().join(format!("of-controls-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut cap = Capture::start();
        note("deck1.hotcue".into(), "B");
        cap.tick(vec![("deck1.play".into(), json!(true))]);
        cap.file(&dir.join("rec-1.wav"));
        drop(cap);
        let text = std::fs::read_to_string(dir.join("rec-1.controls.jsonl")).unwrap();
        let got: Vec<Value> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(got.len(), 3, "{text}");
        assert_eq!(got[0]["control"], "deck1.play");
        assert_eq!(
            got[1],
            json!({"t": got[1]["t"], "control": "deck1.hotcue", "value": "B"})
        );
        assert_eq!(got[2]["control"], "recording.file");
        assert_eq!(got[2]["value"], "rec-1.wav");
        note("deck1.hotcue".into(), "C"); // nothing logs now: the note is dropped
        NOTES.with(|n| assert!(n.borrow().is_none()));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
