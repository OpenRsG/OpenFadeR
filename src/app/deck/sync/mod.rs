//! SYNC, latched: a synced deck
//! follows the master deck's effective tempo continuously and keeps its beat phase.
//!
//! * MASTER is the tempo source (any of the four decks). Auto: the first playing deck, handed over when it
//!   stops ([`master::after_play_change`]); the MASTER button pins one. SYNC follows the master; pressing SYNC
//!   on the master (or with none) hands the master to [`master::leader_of`]'s pick, the first other deck that
//!   can be followed. A deck that becomes master stops following.
//! * Tempo: the follower plays at `master BPM × k`, k ∈ {1, 2, ½} whichever needs the rate closest
//!   to 1 (87 BPM follows 174 BPM at 1×, not 2×); its own tempo fader is ignored while it can follow ([`target`], [`Deck::tracking`]).
//! * Phase: the grids' fractional beat numbers are compared (`k` beats of the follower per master
//!   beat). Drift above 1 ms is pulled in with a tiny `Player::nudge`, never a seek; an
//!   error above 25 ms (SYNC pressed while playing, a hot-cue jump) re-seeks once to the aligned spot, in phase with what is
//!   heard ([`correct`]). Nothing is measured while a seek or start is still on its way to the speakers, or while either deck brakes
//!   or spins up. No correction while scratching, bending, looping or reversing (the master's scratch and reverse count), or when the
//!   master's loop cannot preserve the follower's beat phase. Tempo following continues through those loops.
//! * PLAY on a paused synced deck waits for the master's next beat and starts one output latency ahead, so it is heard on the beat,
//!   unless its loop cannot preserve that phase. A master that cannot be followed any more (unloaded, out of range) starts it at once.
//!
//! Base: [`Link`] and [`State`] (the master and each deck's SYNC state), the nudge plumbing. Children:
//! [`phase`] (tempo and phase maths, pure), [`master`] (hand-over rules, SYNC / MASTER buttons), [`follow`] (the
//! per-tick follower).
//!
//! ponytail: the grids are read as constant-tempo from the first beat (a variable grid would need the
//! tempo at the playhead); beat phase only, not bar phase; a master tempo outside 0.5 to 2× of the
//! follower's grid leaves the follower at its last rate without a message, and its tempo fader free.

mod follow;
mod master;
mod phase;

use super::{DECKS, Deck, grid_bpm};
use crate::app::prefs::controller::{Controller, SyncType};
use openfader::analysis::Beat;
pub use phase::target;
use phase::{Fix, align, correct, error_ms, start_ms, wait_phase};
use std::{
    cell::{Cell, OnceCell},
    rc::{Rc, Weak},
};

/// Master state every deck shares, and the decks themselves (so any deck can reach the master).
#[derive(Default)]
pub struct Link {
    master: Cell<Option<usize>>,
    /// The MASTER button pinned it: no automatic hand-over.
    manual: Cell<bool>,
    decks: OnceCell<[Weak<Deck>; DECKS]>,
}

impl Link {
    fn deck(&self, i: usize) -> Option<Rc<Deck>> {
        self.decks.get()?.get(i)?.upgrade()
    }
}

/// A deck's SYNC state.
pub struct State {
    link: Rc<Link>,
    synced: Cell<bool>,
    was_playing: Cell<bool>,
    /// Quantized start armed: the master's beat phase at the last tick.
    waiting: Cell<Option<f64>>,
    /// Jog pitch bend and the phase controller's nudge; their sum goes to the player.
    bend: Cell<f32>,
    nudge: Cell<f32>,
    pub(super) scratching: Cell<bool>,
    /// SYNC TYPE: BEAT SYNC (tempo and beat phase) or BPM SYNC (tempo only), and whether half / double tempo may be followed.
    beat: Cell<bool>,
    half_double: Cell<bool>,
}

impl State {
    pub fn new(link: Rc<Link>, c: &Controller) -> Self {
        Self {
            link,
            synced: Cell::new(false),
            was_playing: Cell::new(false),
            waiting: Cell::new(None),
            bend: Cell::new(0.0),
            nudge: Cell::new(0.0),
            scratching: Cell::new(false),
            beat: Cell::new(c.sync_type == SyncType::Beat),
            half_double: Cell::new(c.sync_half_double),
        }
    }

    /// The SYNC settings changed.
    pub fn refresh(&self, c: &Controller) {
        self.beat.set(c.sync_type == SyncType::Beat);
        self.half_double.set(c.sync_half_double);
    }
}

/// Lets every deck see the others.
pub fn connect(decks: &[Rc<Deck>; DECKS]) {
    let _ = decks[0]
        .sync
        .link
        .decks
        .set(std::array::from_fn(|i| Rc::downgrade(&decks[i])));
}

impl Deck {
    /// Deck `i` of the layout (every deck sees the others).
    pub(super) fn deck_at(&self, i: usize) -> Option<Rc<Deck>> {
        self.sync.link.deck(i)
    }

    /// The master deck, if there is one.
    pub(super) fn master_deck(&self) -> Option<Rc<Deck>> {
        self.sync.link.deck(self.sync.link.master.get()?)
    }

    /// The deck this one's phase meter is drawn against: the master while this deck follows it, else (this deck
    /// is the master) the first deck that follows it.
    pub(super) fn reference(&self) -> Option<Rc<Deck>> {
        if self.following() {
            return self.master_deck();
        }
        let l = &self.sync.link;
        l.master.get().filter(|&m| m == self.index)?;
        (0..DECKS)
            .filter(|&i| i != self.index)
            .filter_map(|i| l.deck(i))
            .find(|d| d.following())
    }

    pub(in crate::app) fn is_synced(&self) -> bool {
        self.sync.synced.get()
    }

    pub(in crate::app) fn is_master(&self) -> bool {
        self.sync.link.master.get() == Some(self.index)
    }

    /// MIX POINT LINK "Automatically changes the BEAT/BPM SYNC setting": this deck follows the tempo only, until
    /// [`State::refresh`] puts the PREFS setting back.
    pub(super) fn force_bpm_sync(&self) {
        self.sync.beat.set(false);
    }

    /// PLAY was pressed on a synced deck and it waits for the master's next beat (the controller's SYNC light blinks).
    pub(super) fn start_armed(&self) -> bool {
        self.sync.waiting.get().is_some()
    }

    /// This deck left the layout (2-deck mode): it stops, follows nobody and its waveforms stop rendering; if it
    /// was the master, the first playing deck of the `shown` ones takes over (else nobody leads).
    pub(super) fn park(&self, shown: usize) {
        self.with_player(|p| p.pause());
        self.sync.synced.set(false);
        self.sync.waiting.set(None);
        self.set_nudge(0.0);
        self.show(|v| v.synced = false);
        if self.is_master() {
            let l = &self.sync.link;
            self.set_master((0..shown).find(|&i| l.deck(i).is_some_and(|d| d.playing())));
        }
        self.detail_size.set((0.0, 0.0));
        self.overview_size.set((0.0, 0.0));
    }

    /// SYNC latched and the other deck is the master: this deck's tempo fader is ignored.
    pub(super) fn following(&self) -> bool {
        self.is_synced() && self.sync.link.master.get().is_some_and(|m| m != self.index)
    }

    /// Grid tempo, the rate and the multiple `k` of the master's tempo this deck (grid `own`) plays at to follow
    /// a peer (grid `theirs`, playing at `peer_rate`); `None` without grids or in range.
    fn tempo_target(
        &self,
        own: &[Beat],
        theirs: &[Beat],
        peer_rate: f64,
    ) -> Option<(f64, f64, f64)> {
        let (g, m) = (grid_bpm(own)?, grid_bpm(theirs)?);
        let (rate, k) = target(g, m * peer_rate, self.sync.half_double.get())?;
        Some((g, rate, k))
    }

    /// Jog pitch bend (±0.1 typical, 0 = none); adds to the phase controller's nudge.
    pub fn bend(&self, offset: f32) {
        self.sync.bend.set(offset);
        self.apply_nudge();
    }

    fn set_nudge(&self, d: f32) {
        if self.sync.nudge.replace(d) != d {
            self.apply_nudge();
        }
    }

    fn apply_nudge(&self) {
        let total = self.sync.bend.get() + self.sync.nudge.get();
        self.with_player(|p| p.nudge(total));
    }

    /// The player's position is what is being heard (see `Player::settled`); true without a player.
    fn settled(&self) -> bool {
        self.with_player(|p| p.settled()).unwrap_or(true)
    }

    /// No brake or spin-up ramp is changing the deck's speed (see `Player::steady`); true without a player.
    fn steady(&self) -> bool {
        self.with_player(|p| p.steady()).unwrap_or(true)
    }

    /// A LOAD starts clean: no bend, nudge or armed start.
    pub(super) fn sync_reset(&self) {
        self.sync.waiting.set(None);
        self.sync.bend.set(0.0);
        self.sync.nudge.set(0.0);
        self.apply_nudge();
    }

    /// A press of PLAY while an armed synced start waits cancels it.
    pub(super) fn cancel_start(&self) -> bool {
        let armed = self.sync.waiting.take().is_some();
        if armed {
            self.say(tr!("synced start cancelled"));
        }
        armed
    }
}
