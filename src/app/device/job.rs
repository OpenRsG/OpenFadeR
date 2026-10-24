//! The background job: [`Ctx::start_then`] spawns it, the window timer's [`Ctx::poll_job`] shows its progress and ends it.

use super::{BUSY, Ctx, Job, toast};
use openfader::{device, plugin::device::Control};
use slint::ComponentHandle;
use std::{
    rc::Rc,
    sync::{Arc, atomic::Ordering::Relaxed, mpsc},
    thread,
};

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

    /// The timer: the running job's progress or end.
    pub(super) fn poll_job(self: &Rc<Self>) {
        let outcome = {
            let slot = self.job.borrow();
            let Some(job) = slot.as_ref() else { return };
            match job.rx.try_recv() {
                Err(mpsc::TryRecvError::Empty) => return self.show_progress(&job.ctl),
                Err(mpsc::TryRecvError::Disconnected) => Err(tr!("the worker stopped").to_owned()),
                Ok(r) => r,
            }
        };
        let Some(job) = self.job.take() else { return };
        self.with_state(|s| {
            s.set_busy(false);
            s.set_progress(0.0);
        });
        if let Some(d) = job.device.as_deref().and_then(|id| self.device(id)) {
            device::forget(&d.mount);
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
            self.say(text, failed);
            if let Some(app) = self.app() {
                toast::show(&app, text.lines().next().unwrap_or(text));
            }
        } else {
            self.say(&job.before.0, job.before.1); // nothing to report: what the window said before stays
        }
        (job.then)(self, &outcome);
        self.refresh_devices();
    }

    fn show_progress(&self, ctl: &Control) {
        let (done, total) = (ctl.done.load(Relaxed), ctl.total.load(Relaxed));
        let phase = ctl.phase();
        let fraction = if total == 0 {
            0.0
        } else {
            done as f32 / total as f32
        };
        let status = if total == 0 {
            phase.clone()
        } else {
            format!("{phase} {done}/{total}")
        };
        // A transfer from the browser has no window open: its progress shows in the status bar too.
        if self.mirror.get()
            && let Some(app) = self.app()
        {
            let s = app.global::<crate::app::BrowserState>();
            s.set_load_text(status.as_str().into());
            s.set_load_progress(fraction);
        }
        self.with_state(|s| {
            s.set_progress(fraction);
            s.set_phase(phase.into());
            s.set_status(status.into());
        });
    }
}
