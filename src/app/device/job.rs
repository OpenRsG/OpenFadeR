//! The background job: [`Ctx::start_then`] spawns it, the window timer's [`Ctx::poll_job`] shows its progress and ends it.
//! [`Ctx::write`] is the job every write to a device is: it finishes what an earlier cut-off change left, backs the library up,
//! writes ([`openfader::device::export::export`]) and says whether what was written reads back ([`checked`]).

use super::pace::{Pace, Reading};
use super::preview::{Build, Built, Target};
use super::{format_label, toast, Ctx, Job, BUSY};
use openfader::{
    device::{export::{export, Written}, plan::Speeds},
    library::Library,
    plugin::device::Control,
};
use slint::ComponentHandle;
use std::{
    rc::Rc,
    sync::{Arc, atomic::Ordering::Relaxed, mpsc},
    thread,
    time::Instant,
};

/// What to do when a write failed with EIO (`os error 5`): the kernel lost the device mid-write (a USB link that resets, a
/// stick that fails), which the one retry of the audio copy did not mend. A device behind a hub ([`openfader::device::Device::via_hub`]) is the
/// likely cause and is said first; the files already copied stay, so the same export again resumes.
fn usb_hint(e: &str, via_hub: bool) -> Option<String> {
    if !e.contains("(os error 5)") {
        return None;
    }
    let resume = tr!("Audio already copied stays on the device: export again and it continues with the rest.");
    Some(if via_hub {
        tr!("The device is plugged in through a USB hub, and the hub dropped its link while it was written (a USB input/output error, not a format problem): plug it straight into a port of the computer, then export again. {} If it keeps happening, the stick is failing.", resume)
    } else {
        tr!("The device stopped answering while it was written (a USB input/output error, not a format problem): plug it into another port of the computer, then export again. {} If it keeps happening, the stick is failing.", resume)
    })
}

/// `text` and under it `lines`, one per row.
fn with_lines(mut text: String, lines: impl IntoIterator<Item = String>) -> String {
    for l in lines {
        text.push('\n');
        text += &l;
    }
    text
}

/// Whether what the formats wrote reads back: the plugins check their files after writing (`ExportReport::verified`), and this
/// says "Checked: KEY holds 120 tracks in REKORDBOX + ONELIBRARY" when every format that wrote did, else which did not.
fn checked(t: &Target, done: &[Written]) -> Option<String> {
    let ok: Vec<_> = done
        .iter()
        .filter_map(|(n, r)| Some((n.as_str(), r.as_ref().ok()?)))
        .collect();
    let first = ok.first()?.0;
    let label = |n: &str| format_label(Some(&t.dev), n);
    let unchecked: Vec<String> = ok
        .iter()
        .filter(|(_, r)| r.verified != Some(true))
        .map(|(n, _)| label(n))
        .collect();
    if !unchecked.is_empty() {
        return Some(tr!("Not checked after writing: {}", unchecked.join(", ")));
    }
    // What the first format's library holds now (read back from the device), else what this job wrote.
    let held = openfader::plugin::library::open_with(first, &t.dest)
        .ok()
        .and_then(|l| l.tracks(None).ok())
        .map(|v| v.len())
        .unwrap_or_else(|| ok.iter().map(|(_, r)| r.tracks).max().unwrap_or(0));
    Some(tr_n!(
        "Checked: {} holds {n} track in {}",
        "Checked: {} holds {n} tracks in {}",
        held,
        t.name,
        ok.iter().map(|(n, _)| label(n)).collect::<Vec<_>>().join(" + ")
    ))
}

impl Ctx {
    /// [`Self::start_then`] with nothing to do when the job ends.
    pub(super) fn start(
        self: &Rc<Self>,
        device: Option<String>,
        phase: &str,
        work: impl FnOnce(Arc<Control>) -> Result<String, String> + Send + 'static,
    ) {
        self.start_then(device, phase, work, |_, _| {});
    }

    /// Runs `work` on a thread with a fresh [`Control`]; `then` sees its result on the UI thread. Refuses while another job runs.
    pub(super) fn start_then(
        self: &Rc<Self>,
        device: Option<String>,
        phase: &str,
        work: impl FnOnce(Arc<Control>) -> Result<String, String> + Send + 'static,
        then: impl FnOnce(&Rc<Ctx>, &Result<String, String>) + 'static,
    ) {
        if self.busy() {
            return self.say(crate::app::i18n::tr_dyn(BUSY), true);
        }
        let ctl = Arc::new(Control::default());
        ctl.set_phase(&format!("{phase}…"));
        let (worker, (tx, rx)) = (ctl.clone(), mpsc::channel());
        thread::spawn(move || drop(tx.send(work(worker))));
        let before = self
            .with_state(|s| (s.get_status().to_string(), s.get_failed()))
            .unwrap_or_default();
        *self.job.borrow_mut() = Some(Job {
            ctl,
            rx,
            device,
            before,
            pace: Pace::new(self.prep.take_expected()),
            auto: self.prep.automatic.get(),
            then: Box::new(then),
        });
        self.with_state(|s| {
            s.set_busy(true);
            s.set_progress(0.0);
            s.set_phase(phase.into());
            s.set_failed(false);
            s.set_status(format!("{phase}…").into());
        });
    }

    /// A write to `t` (the device or folder), the one job every export, sync and transfer is: before anything is written the
    /// interrupted change a pulled stick left is finished or undone and the library is backed up ([`super::safety`]); afterwards
    /// the plugins' read-back is reported. `build` makes what is written (on the worker), `finish` the result text from what the
    /// formats answered.
    pub(super) fn write(
        self: &Rc<Self>,
        t: Target,
        phase: &str,
        build: Build,
        finish: impl FnOnce(&Built, &[Written]) -> Result<String, String> + Send + 'static,
        then: impl FnOnce(&Rc<Ctx>, &Result<String, String>) + 'static,
    ) {
        let (safety, phase_text) = (self.safety(), format!("{phase}…"));
        self.start_then(
            Some(t.dev.id.clone()),
            phase,
            move |ctl| {
                let built = build(&ctl)?;
                if let Some(m) = &built.idle {
                    return Ok(m.clone());
                }
                // A plain folder is recovered, not backed up (its backups would list under no device).
                let notes: Vec<String> = if t.dev.is_folder() {
                    safety.recover(&t.dest, &t.name)?.into_iter().collect()
                } else {
                    safety.before_write(&t.dev, &ctl)?
                };
                ctl.set_phase(&phase_text);
                // A job that fails may have been cut off mid-switch: what a change left is finished or undone again, and the
                // error says so. A plain cancel stays as it is: the status line tells "cancelled" apart.
                let failed = |e: String| {
                    let fixed = if t.dest.is_dir() {
                        safety.recover(&t.dest, &t.name).unwrap_or_else(Some)
                    } else {
                        None
                    };
                    match (e == "cancelled", fixed) {
                        (true, None) => e,
                        (true, Some(f)) => f,
                        (false, f) => {
                            let hint = usb_hint(&e, t.dev.via_hub);
                            with_lines(e, hint.into_iter().chain(notes.iter().cloned()).chain(f))
                        }
                    }
                };
                let done = export(&t.writers, &t.dest, &built.snap, &t.o, &ctl).map_err(&failed)?;
                let text = finish(&built, &done).map_err(&failed)?;
                Ok(with_lines(text, notes.into_iter().chain(checked(&t, &done))))
            },
            then,
        );
    }

    /// What the status line says when `job` ends with `text`: an automatic job (the cue sync after an export) goes under the
    /// report it follows when that is still on the line, so the export's own report ("Checked: ...") is not lost.
    fn report(&self, job: &Job, text: &str) -> String {
        let mut last = self.prep.report.borrow_mut();
        let under = job.auto && !job.before.1 && !job.before.0.is_empty() && *last == job.before.0;
        *last = if under {
            format!("{}\n{text}", job.before.0)
        } else {
            text.to_owned()
        };
        last.clone()
    }

    /// The timer: the running job's progress or end.
    pub(super) fn poll_job(self: &Rc<Self>) {
        let outcome = {
            let mut slot = self.job.borrow_mut();
            let Some(job) = slot.as_mut() else { return };
            match job.rx.try_recv() {
                Err(mpsc::TryRecvError::Empty) => return self.show_progress(job),
                Err(mpsc::TryRecvError::Disconnected) => Err(tr!("the worker stopped").to_owned()),
                Ok(r) => r,
            }
        };
        let Some(job) = self.job.take() else { return };
        self.with_state(|s| {
            s.set_busy(false);
            s.set_progress(0.0);
        });
        if let Some(id) = job.device.as_deref() {
            if outcome.is_ok() {
                Speeds::remember(&self.store, id, job.pace.speeds());
            }
            if let Some(d) = self.device(id) {
                openfader::device::forget(&d.mount);
            }
        }
        // An empty message is "nothing to report" (an automatic job with nothing to do); a leading \u{1} is a question for `then`.
        let text = match &outcome {
            Ok(m) if m.is_empty() || m.starts_with('\u{1}') => None,
            Ok(m) => Some((m.as_str(), false)),
            Err(e) if e == "cancelled" => {
                Some((tr!("Cancelled: the device was left as it was"), true))
            }
            Err(e) => Some((e.as_str(), true)),
        };
        if let Some((text, failed)) = text {
            self.say(&self.report(&job, text), failed);
            if let Some(app) = self.app() {
                toast::show(&app, text.lines().next().unwrap_or(text));
            }
        } else {
            self.say(&job.before.0, job.before.1); // nothing to report: what the window said before stays
        }
        (job.then)(self, &outcome);
        self.refresh_devices();
    }

    /// The status line with speed and time left, and what makes a slow stick slow.
    fn show_progress(&self, job: &mut Job) {
        let ctl = &job.ctl;
        let (done, total) = (ctl.done.load(Relaxed), ctl.total.load(Relaxed));
        let phase = ctl.phase();
        let r = Reading {
            phase: &phase,
            done,
            total,
            bytes_done: ctl.bytes_done.load(Relaxed),
            bytes_total: ctl.bytes_total.load(Relaxed),
        };
        let now = Instant::now();
        job.pace.sample(now, &r);
        let (status, why) = job.pace.status(now, &r);
        let fraction = if total == 0 {
            0.0
        } else {
            done as f32 / total as f32
        };
        // A transfer from the browser has no window open: its progress shows in the status bar too.
        if self.mirror.get()
            && let Some(app) = self.app()
        {
            let s = app.global::<crate::app::BrowserState>();
            let line = why.as_ref().map_or_else(|| status.clone(), |w| format!("{status} - {w}"));
            s.set_load_text(line.into());
            s.set_load_progress(fraction);
        }
        self.with_state(|s| {
            s.set_progress(fraction);
            s.set_phase(phase.clone().into());
            s.set_status(why.map_or(status.clone(), |w| format!("{status}\n{w}")).into());
        });
    }
}
