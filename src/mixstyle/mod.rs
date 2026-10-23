//! Automix transition styles. A style is a [`Recipe`]: timed steps of mixer moves at beat offsets from the start of a
//! transition. The built-in styles ([`builtin`], `builtin.json`) and the styles a plugin declares (`"automix"` in its
//! plugin.json) are the same type, so one engine runs them all: [`Recipe::timeline`] resolves the offsets for a transition
//! of a given length, [`Timeline::at`] gives every control's position at a beat, [`Timeline::events`] the one-shot moves
//! (effect, loop, brake). Pure data, no audio and no UI: `app/automix/` applies the result to the mixer, the decks and the
//! effect unit. [`random`] picks the style of a RANDOM transition.
//!
//! # Recipe (JSON)
//! ```json
//! {"name": "SLAM", "bars": 4, "steps": [
//!   {"at": 0, "over": "50%", "crossfader": 1},
//!   {"at": 0, "deck": "in", "low": 0},
//!   {"at": -4, "deck": "out", "fx": {"effect": "echo", "level": 0.6}}]}
//! ```
//! A position (`at`, `over`) is beats from the start of the transition; a negative `at` counts from its end; `"NN%"` is a
//! share of its length. `over` (default 0) makes the step a ramp from the control's previous target. Our own format.

mod random;
#[cfg(test)]
mod tests;

pub use random::{pick, random_u64};
use serde::Deserialize;
use std::sync::LazyLock;

/// Which deck of the transition a step moves.
#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    /// The track that plays out.
    Out,
    /// The track that comes in.
    In,
}

/// The continuous controls of one deck, in the order of [`State::deck`]; a step's value is their position.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ctl {
    /// Times the channel fader as the DJ left it, 0..1.
    Fader,
    /// EQ bands as knob positions, 0..1: 0.5 flat, 0 killed, 1 boosted.
    Low,
    Mid,
    High,
    /// COLOR FX knob on FILTER, -1 (low-pass closed) .. 0 (off) .. 1 (high-pass closed).
    Filter,
}

impl Ctl {
    pub const ALL: [Ctl; 5] = [Ctl::Fader, Ctl::Low, Ctl::Mid, Ctl::High, Ctl::Filter];
    /// Where a control sits before its first step and where a recipe should leave it.
    pub const NEUTRAL: [f32; 5] = [1.0, 0.5, 0.5, 0.5, 0.0];
    /// Allowed range of the step values.
    fn range(self) -> (f32, f32) {
        match self {
            Ctl::Filter => (-1.0, 1.0),
            _ => (0.0, 1.0),
        }
    }
    fn name(self) -> &'static str {
        ["fader", "low", "mid", "high", "filter"][self as usize]
    }
}

/// A position in a step: a number of beats, or `"NN%"` of the transition.
#[derive(Deserialize, Clone, Debug, PartialEq)]
#[serde(untagged)]
pub enum At {
    Beats(f64),
    Text(String),
}

impl At {
    /// As beats in a transition `len` beats long; `None` for text that is not a percentage.
    fn beats(&self, len: f64) -> Option<f64> {
        match self {
            At::Beats(b) => Some(*b),
            At::Text(t) => {
                let p: f64 = t.trim().strip_suffix('%')?.trim().parse().ok()?;
                Some(len * p / 100.0)
            }
        }
        .filter(|b| b.is_finite())
    }
}

/// One step: at `at`, set the controls it names (ramping over `over`). `deck` says whose controls; `crossfader` needs none.
#[derive(Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Step {
    pub at: At,
    #[serde(default)]
    pub over: Option<At>,
    #[serde(default)]
    pub deck: Option<Side>,
    /// Travel from the outgoing deck (0) to the incoming one (1), constant power.
    #[serde(default)]
    pub crossfader: Option<f32>,
    #[serde(default)]
    pub fader: Option<f32>,
    #[serde(default)]
    pub low: Option<f32>,
    #[serde(default)]
    pub mid: Option<f32>,
    #[serde(default)]
    pub high: Option<f32>,
    #[serde(default)]
    pub filter: Option<f32>,
    #[serde(default)]
    pub fx: Option<FxStep>,
    /// A loop of that many beats on the deck (0 = loop off).
    #[serde(default, rename = "loop")]
    pub looped: Option<f64>,
    /// A vinyl stop over that many beats.
    #[serde(default)]
    pub brake: Option<f64>,
}

/// The Beat FX unit AUTOMIX borrows for a transition, on the step's deck. Every field left out keeps its value.
#[derive(Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FxStep {
    /// A selector label of the Beat FX (`ECHO`, `REVERB`, a plugin's effect), any case.
    #[serde(default)]
    pub effect: Option<String>,
    /// Switches the effect on (default) or off; an off effect lets its tail ring out.
    #[serde(default = "yes")]
    pub on: bool,
    /// LEVEL / DEPTH, 0..1.
    #[serde(default)]
    pub level: Option<f32>,
    /// The beat fraction of the effect (1/16 to 32 beats; the nearest step of the panel's list).
    #[serde(default)]
    pub beat: Option<f32>,
}

fn yes() -> bool {
    true
}

/// A transition style.
#[derive(Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Recipe {
    /// Label in the selector, 1 to 16 characters, no comma.
    pub name: String,
    /// The transition's length in bars; left out, the LENGTH the DJ chose.
    #[serde(default)]
    pub bars: Option<u32>,
    pub steps: Vec<Step>,
}

impl Recipe {
    /// Whether the recipe can run: what a plugin.json must satisfy to be read. The message names the step.
    pub fn validate(&self) -> Result<(), String> {
        let n = self.name.chars().count();
        if n == 0 || n > 16 || self.name.contains(',') {
            return Err(format!(
                "automix style {:?} needs a name of 1 to 16 characters without a comma",
                self.name
            ));
        }
        let err =
            |i: usize, why: &str| format!("automix style {:?}, step {}: {why}", self.name, i + 1);
        if self.bars.is_some_and(|b| !(1..=64).contains(&b)) {
            return Err(format!("automix style {:?}: `bars` is 1 to 64", self.name));
        }
        if self.steps.is_empty() {
            return Err(format!("automix style {:?} has no steps", self.name));
        }
        for (i, s) in self.steps.iter().enumerate() {
            if s.at.beats(16.0).is_none()
                || s.over
                    .as_ref()
                    .is_some_and(|o| o.beats(16.0).is_none_or(|o| o < 0.0))
            {
                return Err(err(
                    i,
                    "`at` and `over` are beats or \"NN%\" (`over` not negative)",
                ));
            }
            let values = [s.fader, s.low, s.mid, s.high, s.filter];
            for (c, v) in Ctl::ALL.iter().zip(values) {
                let (lo, hi) = c.range();
                if v.is_some_and(|v| !(lo..=hi).contains(&v)) {
                    return Err(err(i, &format!("`{}` is {lo} to {hi}", c.name())));
                }
            }
            if s.crossfader.is_some_and(|v| !(0.0..=1.0).contains(&v)) {
                return Err(err(i, "`crossfader` is 0 to 1"));
            }
            if s.fx
                .as_ref()
                .and_then(|f| f.level)
                .is_some_and(|v| !(0.0..=1.0).contains(&v))
            {
                return Err(err(i, "fx `level` is 0 to 1"));
            }
            if s.looped.is_some_and(|b| !(0.0..=32.0).contains(&b))
                || s.brake.is_some_and(|b| b <= 0.0 || b > 16.0)
            {
                return Err(err(i, "`loop` is 0 to 32 beats, `brake` over 0 up to 16"));
            }
            let deck_move = values.iter().any(Option::is_some)
                || s.fx.is_some()
                || s.looped.is_some()
                || s.brake.is_some();
            if !deck_move && s.crossfader.is_none() {
                return Err(err(i, "moves nothing"));
            }
            if deck_move && s.deck.is_none() {
                return Err(err(i, "needs `deck`: \"out\" or \"in\""));
            }
        }
        Ok(())
    }

    /// The recipe resolved for a transition of `len` beats (steps that [`Recipe::validate`] rejects are dropped).
    pub fn timeline(&self, len: f64) -> Timeline {
        let mut t = Timeline {
            len,
            crossfader: Vec::new(),
            deck: Default::default(),
            events: Vec::new(),
        };
        for s in &self.steps {
            let Some(at) = s.at.beats(len) else { continue };
            let at = if at < 0.0 { len + at } else { at }.max(0.0);
            let over = s
                .over
                .as_ref()
                .and_then(|o| o.beats(len))
                .unwrap_or(0.0)
                .max(0.0);
            let ramp = |to: f32| Ramp { at, over, to };
            if let Some(v) = s.crossfader {
                t.crossfader.push(ramp(v));
            }
            let Some(side) = s.deck else { continue };
            let values = [s.fader, s.low, s.mid, s.high, s.filter];
            for (c, v) in Ctl::ALL.iter().zip(values) {
                if let Some(v) = v {
                    t.deck[side as usize][*c as usize].push(ramp(v));
                }
            }
            let one_shots = [
                s.fx.clone().map(Event::Fx),
                s.looped.map(Event::Loop),
                s.brake.map(Event::Brake),
            ];
            t.events
                .extend(one_shots.into_iter().flatten().map(|e| (at, side, e)));
        }
        let by_at = |a: &Ramp, b: &Ramp| a.at.total_cmp(&b.at);
        t.crossfader.sort_by(by_at);
        t.deck.iter_mut().flatten().for_each(|r| r.sort_by(by_at));
        t.events.sort_by(|a, b| a.0.total_cmp(&b.0));
        t
    }
}

/// A one-shot move of a step.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    Fx(FxStep),
    /// Beats; 0 = loop off.
    Loop(f64),
    /// Beats the vinyl stop lasts.
    Brake(f64),
}

#[derive(Clone, Copy, Debug)]
struct Ramp {
    at: f64,
    over: f64,
    to: f32,
}

/// The value of the control that `ramps` (sorted by `at`) move, at `beat`: each step ramps from where the one before it
/// stood when it began, to its target; a control before its first step is at `neutral`.
fn value(ramps: &[Ramp], neutral: f32, beat: f64) -> f32 {
    let Some(i) = ramps.iter().rposition(|r| r.at <= beat) else {
        return neutral;
    };
    let r = ramps[i];
    if r.over <= 0.0 || beat >= r.at + r.over {
        return r.to;
    }
    let from = value(&ramps[..i], neutral, r.at);
    from + (r.to - from) * ((beat - r.at) / r.over) as f32
}

/// Where every control stands at one beat of a transition. `None`: the recipe never moves that control, so the DJ's own
/// setting stays.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct State {
    /// 0 = the outgoing deck's side, 1 = the incoming one's.
    pub crossfader: f32,
    /// `[Side as usize][Ctl as usize]`.
    pub deck: [[Option<f32>; 5]; 2],
}

impl State {
    pub fn get(&self, side: Side, c: Ctl) -> Option<f32> {
        self.deck[side as usize][c as usize]
    }
}

/// A [`Recipe`] for a transition of known length, offsets in beats from its start.
#[derive(Clone, Debug)]
pub struct Timeline {
    len: f64,
    crossfader: Vec<Ramp>,
    deck: [[Vec<Ramp>; 5]; 2],
    events: Vec<(f64, Side, Event)>,
}

impl Timeline {
    /// The transition's length in beats.
    pub fn len(&self) -> f64 {
        self.len
    }

    /// The controls at `beat`.
    pub fn at(&self, beat: f64) -> State {
        let mut deck = [[None; 5]; 2];
        for (d, ramps) in deck.iter_mut().zip(&self.deck) {
            for ((v, r), n) in d.iter_mut().zip(ramps).zip(Ctl::NEUTRAL) {
                *v = (!r.is_empty()).then(|| value(r, n, beat));
            }
        }
        State {
            crossfader: value(&self.crossfader, 0.0, beat),
            deck,
        }
    }

    /// The one-shot moves as `(beat, deck, move)`, in time order.
    pub fn events(&self) -> &[(f64, Side, Event)] {
        &self.events
    }
}

/// The built-in styles, in selector order (`builtin.json`, the format a plugin uses).
pub fn builtin() -> &'static [Recipe] {
    static ALL: LazyLock<Vec<Recipe>> =
        LazyLock::new(|| serde_json::from_str(include_str!("builtin.json")).expect("builtin.json"));
    &ALL
}

/// What a plugin's `"automix"` list must satisfy: every recipe valid, names unique among themselves and the built-ins.
pub fn validate_all(plugin: &[Recipe]) -> Result<(), String> {
    for (i, r) in plugin.iter().enumerate() {
        r.validate()?;
        let taken = builtin()
            .iter()
            .chain(&plugin[..i])
            .any(|o| o.name.eq_ignore_ascii_case(&r.name));
        if taken {
            return Err(format!("automix style {:?}: the name is taken", r.name));
        }
    }
    Ok(())
}
