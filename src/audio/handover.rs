//! UI to audio-callback hand-over of one shared item (a sampler slice, a drum loop).
//!
//! The mailbox mutex is held for a pointer move only: the callback `try_lock`s it and swaps boxes, so it neither allocates, frees nor
//! blocks. The item it replaces goes back through the mailbox and is dropped by the UI thread on the next [`Handover::load`].

use parking_lot::Mutex;
use std::sync::Arc;
use std::sync::atomic::{
    AtomicBool,
    Ordering::{Acquire, Release},
};

pub(crate) struct Handover<T> {
    mailbox: Mutex<Option<Arc<T>>>,
    /// The UI's own handle on what was loaded last.
    kept: Mutex<Option<Arc<T>>>,
    /// The mailbox holds something the callback has not taken yet (`None` there empties the callback's slot).
    fresh: AtomicBool,
}

impl<T> Default for Handover<T> {
    fn default() -> Self {
        Handover {
            mailbox: Mutex::new(None),
            kept: Mutex::new(None),
            fresh: AtomicBool::new(false),
        }
    }
}

impl<T> Handover<T> {
    /// UI thread: hands `item` over (`None` empties the callback's slot).
    pub(crate) fn load(&self, item: Option<Arc<T>>) {
        *self.kept.lock() = item.clone();
        let old = std::mem::replace(&mut *self.mailbox.lock(), item);
        self.fresh.store(true, Release);
        drop(old); // the previous hand-over, if the callback never took it
    }

    /// What was loaded last (shared, not copied).
    pub(crate) fn kept(&self) -> Option<Arc<T>> {
        self.kept.lock().clone()
    }

    /// Callback: swaps a pending item into `cur`; true when it did (the old one waits in the mailbox for the UI to drop).
    pub(crate) fn take(&self, cur: &mut Option<Arc<T>>) -> bool {
        if self.fresh.load(Acquire)
            && let Some(mut mail) = self.mailbox.try_lock()
        {
            std::mem::swap(&mut *mail, cur);
            self.fresh.store(false, Release); // inside the mailbox guard: a later load() sets it again after
            return true;
        }
        false
    }
}
