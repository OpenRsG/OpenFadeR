//! Effects from plugins (base): what `plugin.json`'s `effects` brings, for each [`Slot`] of the mixer. [`load`] reads
//! every plugin's declaration and makes an [`Effect`] of each; [`Effect::stage`] makes one [`Stage`] per FX unit, rack,
//! COLOR FX or fader that runs it.
//!
//! `effects` is either the path of a native library or a list of effects declared in JSON, which need no compiling.
//! Children: [`native`] (the C ABI of the `openfader-plugin` crate in `sdk/`, which owns [`openfader_plugin::Ctx`],
//! [`Slot`] and the tables) and [`chain`] (an effect as a chain of the host's DSP blocks and formulas over the context).
//!
//! Each effect joins the selector of its slot after that slot's built-ins (Beat FX: [`crate::audio::fx::Effect::Plugin`],
//! in plugin order). A name is 1 to 16 characters of ASCII letters, digits, space, `-` and `_`, unlike every other name in
//! its slot (built-in names included, compared without regard to case), because choices are stored by name in the
//! settings file.

pub mod chain;
mod native;

use super::Plugin;
use crate::audio::{
    cfx,
    fx::{self, Ctx, Frame, Stage},
};
pub use openfader_plugin::Slot;
use serde::{Deserialize, Deserializer, de::Error as _};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;

/// Most plugin effects one slot's selector can hold ([`fx::Effect::Plugin`] is a `u8`).
const MAX: usize = u8::MAX as usize + 1;
const NAME_LEN: usize = 16;
/// Rate of the instance [`load`] creates once to see that an effect works.
const PROBE_RATE: u32 = 48_000;
/// CHANNEL FADER CURVE's built-in choices (`app::prefs::controller::FaderCurve`), which fader plugins sit beside.
const FADER_CURVES: [&str; 3] = ["STEEP", "LINEAR", "GRADUAL"];

/// What a manifest's `effects` holds: a native library, or effects declared in JSON.
#[derive(Clone, Debug)]
pub enum Decls {
    /// Path of a library, relative to the plugin folder.
    Library(PathBuf),
    Chains(Vec<ChainDecl>),
}

/// One effect declared in JSON (see [`chain`]).
#[derive(Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct ChainDecl {
    pub name: String,
    /// `beat`, `color`, `mic`, `send`, `lever` or `fader`.
    pub slot: String,
    #[serde(default)]
    pub lfo: Vec<LfoDecl>,
    /// Blocks, in order.
    pub chain: Vec<Value>,
}

/// An LFO of a [`ChainDecl`]: a variable the blocks' formulas read. Give the `period` in seconds or the rate in `hz`
/// (each a number or a formula).
#[derive(Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct LfoDecl {
    pub name: String,
    #[serde(default)]
    pub period: Option<Value>,
    #[serde(default)]
    pub hz: Option<Value>,
    #[serde(default = "sine")]
    pub shape: String,
}

fn sine() -> String {
    "sine".into()
}

impl<'de> Deserialize<'de> for Decls {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Decls, D::Error> {
        match Value::deserialize(d)? {
            Value::String(s) => Ok(Decls::Library(s.into())),
            Value::Array(list) => list
                .into_iter()
                .enumerate()
                .map(|(i, e)| {
                    serde_json::from_value(e)
                        .map_err(|e| D::Error::custom(format!("effects[{i}]: {e}")))
                })
                .collect::<Result<_, _>>()
                .map(Decls::Chains),
            _ => Err(D::Error::custom(
                "`effects` is the path of a library or a list of effects",
            )),
        }
    }
}

/// Whether `n` is a good effect name.
fn valid_name(n: &str) -> bool {
    !n.is_empty()
        && n.len() <= NAME_LEN
        && n.chars()
            .all(|c| c.is_ascii_alphanumeric() || " -_".contains(c))
}

fn slot_of(d: &ChainDecl) -> Result<Slot, String> {
    Slot::ALL
        .into_iter()
        .find(|s| s.name() == d.slot)
        .ok_or_else(|| {
            format!(
                "effect {:?}: unknown slot {:?}, use beat, color, mic, send, lever or fader",
                d.name, d.slot
            )
        })
}

/// The part of `effects` that is checked when the manifest is read: names, slots and a non-empty chain. The blocks and
/// formulas are checked by [`load`], which skips just the effect that has a mistake.
pub fn check(d: &Decls) -> Result<(), String> {
    let Decls::Chains(list) = d else {
        return Ok(());
    };
    for (i, e) in list.iter().enumerate() {
        if !valid_name(&e.name) {
            return Err(format!(
                "effects[{i}]: name {:?} must be 1 to {NAME_LEN} characters of ASCII letters, digits, space, - or _",
                e.name
            ));
        }
        slot_of(e)?;
        if e.chain.is_empty() {
            return Err(format!("effect {:?} has an empty `chain`", e.name));
        }
    }
    Ok(())
}

/// Names the host's own choices of `slot` already use.
pub fn builtin_names(slot: Slot) -> &'static [&'static str] {
    match slot {
        Slot::Beat => &fx::Effect::NAMES[..],
        Slot::Color => &cfx::Kind::NAMES[..],
        Slot::Fader => &FADER_CURVES[..],
        Slot::Mic | Slot::Send | Slot::Lever => &[],
    }
}

/// How an effect runs.
#[derive(Clone, Debug)]
enum Imp {
    Native(native::Fns),
    Chain(Arc<chain::Def>),
}

/// One effect a plugin brings.
#[derive(Clone, Debug)]
pub struct Effect {
    /// Selector label, e.g. "BITCRUSH".
    pub name: String,
    /// Name of the plugin it came from.
    pub plugin: String,
    pub slot: Slot,
    imp: Imp,
}

impl Effect {
    /// A new instance for one FX unit. If a native library's `create` fails now (it worked at load) the stage passes
    /// the signal through.
    pub fn stage(&self, rate: u32) -> Box<dyn Stage> {
        match &self.imp {
            Imp::Native(f) => f.stage(rate),
            Imp::Chain(d) => d.stage(rate),
        }
    }
}

struct Bypass;

impl Stage for Bypass {
    fn tick(&mut self, x: Frame, _: &Ctx) -> Frame {
        x
    }
    fn reset(&mut self) {}
}

/// Loads the `effects` of every plugin that has some; failures go to `errors`, and the other plugins and effects still
/// load. The result is grouped by slot (stable: plugin order within a slot), as [`super::Registry::slot`] wants it.
pub fn load(plugins: &[Plugin], errors: &mut Vec<String>) -> Vec<Effect> {
    let mut out: Vec<Effect> = Vec::new();
    for p in plugins {
        let Some(decls) = &p.manifest.effects else {
            continue;
        };
        let name = &p.manifest.name;
        let found: Vec<Result<Effect, String>> = match decls {
            Decls::Library(rel) => {
                let path = std::path::absolute(p.dir.join(rel)).unwrap_or_else(|_| p.dir.join(rel));
                match native::open(&path, name) {
                    Err(e) => {
                        errors.push(format!("{name}: {e}"));
                        continue;
                    }
                    Ok(found) => found.into_iter().map(Ok).collect(),
                }
            }
            Decls::Chains(list) => list.iter().map(|d| declared(name, d)).collect(),
        };
        for e in found {
            match e.and_then(|e| admit(&out, &e).map(|()| e)) {
                Ok(e) => out.push(e),
                Err(why) => errors.push(format!("{name}: {why}")),
            }
        }
    }
    out.sort_by_key(|e| e.slot as u32);
    out
}

/// The effect `d` of `plugin`, parsed and checked.
fn declared(plugin: &str, d: &ChainDecl) -> Result<Effect, String> {
    let slot = slot_of(d)?;
    let def = chain::compile(d).map_err(|e| format!("{} effect {:?}: {e}", slot.name(), d.name))?;
    Ok(Effect {
        name: d.name.clone(),
        plugin: plugin.into(),
        slot,
        imp: Imp::Chain(Arc::new(def)),
    })
}

/// Whether `e` may join `loaded`: its name is free in its slot and the slot has room.
fn admit(loaded: &[Effect], e: &Effect) -> Result<(), String> {
    let same: Vec<&str> = loaded
        .iter()
        .filter(|l| l.slot == e.slot)
        .map(|l| l.name.as_str())
        .collect();
    let (name, slot) = (&e.name, e.slot.name());
    let mut taken = builtin_names(e.slot)
        .iter()
        .copied()
        .chain(same.iter().copied());
    if taken.any(|n| n.eq_ignore_ascii_case(name)) {
        return Err(format!("{slot} effect {name:?}: the name is already taken"));
    }
    if same.len() >= MAX {
        return Err(format!(
            "{slot} effect {name:?}: at most {MAX} plugin effects per slot"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod bundled;
#[cfg(test)]
pub(crate) mod kit;
#[cfg(test)]
mod tests;
/// The SDK test effects (GAIN doubles the signal, BOOM panics) for other modules' tests.
#[cfg(test)]
pub(crate) use tests::fake_effects;
