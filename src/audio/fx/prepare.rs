use super::Stage;
use crate::plugin::effect::Effect;
use std::sync::atomic::{
    AtomicBool, AtomicUsize,
    Ordering::{Acquire, Release},
};
use std::sync::{Arc, LazyLock, Mutex, Weak};
use std::thread::{self, JoinHandle};

pub(super) struct Prepared {
    pub index: usize,
    pub stage: Box<dyn Stage>,
}

// At most one replacement and one retired stage per unit; only the worker drops either.
#[derive(Default)]
pub(super) struct Handoff {
    pub ready: Option<Prepared>,
    pub retired: Option<Prepared>,
    closing: Option<Prepared>,
}

pub(super) struct Exchange {
    pub desired: AtomicUsize,
    pub active: AtomicUsize,
    pub handoff: Mutex<Handoff>,
    alive: AtomicBool,
    rate: u32,
    beat: bool,
    effects: Vec<Effect>,
}

impl Exchange {
    pub fn new(rate: u32, beat: bool, effects: &[Effect]) -> Arc<Self> {
        let exchange = Arc::new(Self {
            desired: AtomicUsize::new(0),
            active: AtomicUsize::new(0),
            handoff: Mutex::new(Handoff::default()),
            alive: AtomicBool::new(true),
            rate,
            beat,
            effects: effects.to_vec(),
        });
        // Darwin lazily allocates the native mutex on its first lock, never in the audio callback.
        drop(exchange.handoff.lock().unwrap());
        exchange
    }

    fn prepare(&self) {
        let desired = self.desired.load(Acquire);
        let mut h = self.handoff.lock().unwrap();
        drop(h.retired.take());
        if h.ready.as_ref().is_some_and(|p| p.index != desired) {
            drop(h.ready.take());
        }
        if self.active.load(Acquire) == desired || h.ready.is_some() {
            return;
        }
        drop(h);
        let stage = if self.beat {
            super::unit::stage(
                super::Effect::from_index_in(desired, self.effects.len()),
                self.rate,
                &self.effects,
            )
        } else {
            self.effects[desired].stage(self.rate)
        };
        let mut h = self.handoff.lock().unwrap();
        if self.desired.load(Acquire) == desired && self.active.load(Acquire) != desired {
            h.ready = Some(Prepared {
                index: desired,
                stage,
            });
        }
    }
}
struct State {
    exchanges: Mutex<Vec<Arc<Exchange>>>,
    stop: AtomicBool,
}
pub(super) struct Worker {
    state: Arc<State>,
    thread: Option<JoinHandle<()>>,
}

impl Worker {
    pub fn shared() -> Arc<Self> {
        static WORKER: LazyLock<Mutex<Weak<Worker>>> = LazyLock::new(Mutex::default);
        let mut current = WORKER.lock().unwrap();
        if let Some(worker) = current.upgrade() {
            return worker;
        }
        let state = Arc::new(State {
            exchanges: Mutex::new(Vec::new()),
            stop: AtomicBool::new(false),
        });
        let s = state.clone();
        let thread = thread::Builder::new()
            .name("fx-prepare".into())
            .spawn(move || loop {
                thread::park();
                let mut exchanges = s.exchanges.lock().unwrap();
                exchanges.retain(|e| {
                    if e.alive.load(Acquire) {
                        return true;
                    }
                    let mut h = e.handoff.lock().unwrap();
                    drop(h.ready.take());
                    drop(h.retired.take());
                    drop(h.closing.take());
                    false
                });
                if s.stop.load(Acquire) {
                    exchanges.clear();
                    break;
                }
                for exchange in exchanges.iter() {
                    exchange.prepare();
                }
            })
            .expect("FX preparation thread");
        let worker = Arc::new(Self {
            state,
            thread: Some(thread),
        });
        *current = Arc::downgrade(&worker);
        worker
    }

    pub fn register(&self, exchange: Arc<Exchange>) {
        self.state.exchanges.lock().unwrap().push(exchange);
    }

    pub fn wake(&self) {
        self.thread.as_ref().unwrap().thread().unpark();
    }

    pub fn retire(&self, exchange: &Exchange, current: Option<Prepared>) {
        exchange.handoff.lock().unwrap().closing = current;
        exchange.alive.store(false, Release);
        self.wake();
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.state.stop.store(true, Release);
        self.wake();
        // The worker owns the exchange list until it has destroyed all retired stages.
        let _ = self.thread.take().unwrap().join();
    }
}
