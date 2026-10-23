//! Beat FX: one effect unit synced to a tempo, inserted on one of the four channels or the master sum
//! (each effect is a
//! classic published design, noted in its child). Base: what the children share ([`Line`] delay line,
//! [`Lfo`], [`Ctx`], [`Stage`], [`fold`], [`sweep`], [`ramp`], [`moved`], [`finite`], [`Bpm`]) and the beat table.
//! Children: [`controls`] ([`Effect`], [`Assign`], [`Controls`]: the lock-free knobs the UI sets and the
//! audio thread reads), [`unit`] ([`Unit`]: engage ramp, trails, tempo slew, bypass) and one [`Stage`] each:
//! [`echo`] (ECHO, DELAY), [`reverb`] (Freeverb), [`flanger`], [`phaser`], [`roll`] (captured slice loop),
//! [`filter`] (LPF / HPF sweep), [`trans`] (gate). Plugin libraries add more [`Stage`]s after these nine
//! ([`crate::plugin::effect`]): [`Effect::Plugin`] selects them. Each [`Unit`] holds only its selected stage (and, for 5 ms
//! after a switch while its tail rang, the stage it replaces, faded out); a shared worker prepares replacements and destroys
//! retired stages outside the callback.
//!
//! Timing: every effect's period is `beats × 60 / bpm` seconds, where `bpm` is the master deck's
//! effective BPM, pushed by the UI via [`Controls::set_bpm`]. LFOs and the gate are phase-restarted when
//! the unit is switched ON (the DJ presses it on the beat) but are not phase-locked to the deck's grid
//! afterwards. ponytail: the audio thread has no beat position; add a phase input if drift is audible.
//!
//! ON / OFF: a 5 ms ramp ([`Unit`]'s engage gain `g`) fades the effect in and out. ECHO, DELAY and REVERB
//! stop feeding their buffers at OFF but keep sounding (trails) until their output is below -120 dBFS for
//! longer than one period; the other effects stop at once. Once off and silent the unit is idle: the
//! signal is not touched at all (bit-exact pass-through, no CPU).
//!
//! The bus owns the Beat FX panel's [`Controls`] and one more per deck: that deck's PAD FX unit, which its
//! performance pads switch on while held (same effects, fixed on the deck's own channel, run after the Beat FX unit).
//!
//! Stem FX: with [`Controls::set_part`] a Beat FX unit assigned to CH1..CH4 acts on chosen stems of that channel only. The engine's
//! [`Unit::apply`] then skips it; the channel's voice owns one more [`Unit`] (one per channel: if FX1 and FX2 both target parts of
//! the same channel, FX1 wins) and splits its read into `rest` and `part`, runs `out = rest + unit(part)` before the voice's EQ,
//! COLOR FX and fader (so those and the FX tail affect both alike). With key lock on the two parts have independent stretchers and
//! may differ in phase by less than the stretcher's tolerance. A track without stems gets no effect.

mod controls;
mod echo;
mod filter;
mod flanger;
mod phaser;
mod prepare;
mod reverb;
mod roll;
mod trans;
mod unit;

pub(crate) use controls::Settings;
pub use controls::{Assign, Bpm, Controls, Effect};
/// What a stage needs for one frame; the plugin SDK owns the type so host and plugin libraries cannot drift apart.
pub use openfader_plugin::Ctx;
/// The shared DSP helpers live in the SDK so plugins use the very same ones.
pub use openfader_plugin::dsp::{Lfo, Line, Reverb, fold, ramp, sweep};
pub use unit::Unit;

/// One stereo frame.
pub type Frame = [f32; 2];

/// Longest delay / roll slice kept. A 16-beat echo at 60 BPM would need 16 s (6 MB per effect at 48 kHz);
/// longer periods are halved until they fit ([`fold`]), which keeps them on the beat.
/// ponytail: 4 s caps ECHO / DELAY / ROLL at 16 beats from 240 BPM up; raise it if longer ones matter.
pub const MAX_DELAY_S: f32 = 4.0;

/// Beat fractions the BEAT selector steps through and their labels: 1/16 to 32 beats with 3/4
/// (11 steps; the BEAT ◁ / ▷ handlers clamp to them).
pub const BEATS: [f32; 11] = [
    0.0625, 0.125, 0.25, 0.5, 0.75, 1.0, 2.0, 4.0, 8.0, 16.0, 32.0,
];
pub const BEAT_TEXT: [&str; 11] = [
    "1/16", "1/8", "1/4", "1/2", "3/4", "1", "2", "4", "8", "16", "32",
];
/// BEAT selector position at start (1 beat).
pub const DEFAULT_BEAT: usize = 5;

/// BPM the unit falls back to when the UI has none to give (also COLOR FX and MERGE FX, which build on this base).
pub const FALLBACK_BPM: f32 = 120.0;

/// Output within this of the input counts as silent (-120 dBFS).
const QUIET: f32 = 1e-6;

/// Whether `y` differs from `x` by more than [`QUIET`] on either channel.
pub fn moved(x: Frame, y: Frame) -> bool {
    (y[0] - x[0]).abs().max((y[1] - x[1]).abs()) > QUIET
}

/// Whether both channels of `y` are finite; an effect that blows up is dropped, not played.
pub fn finite(y: Frame) -> bool {
    y[0].is_finite() && y[1].is_finite()
}

/// One effect.
pub trait Stage: Send {
    /// Output frame for input `x`.
    fn tick(&mut self, x: Frame, c: &Ctx) -> Frame;
    /// Forget everything (the effect was selected, or woke from idle).
    fn reset(&mut self);
    /// The unit was switched ON: restart the LFO / gate phase or the roll capture.
    fn engage(&mut self) {}
}

#[cfg(test)]
mod tests;
