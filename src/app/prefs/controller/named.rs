//! CONTROLLER > Mixer choices that list the host's own entries first and the plugins' after them: CHANNEL FADER CURVE
//! ([`Fader`], the built-in [`FaderCurve`]s then the FADER slot effects) and COLOR FX KNOB ([`KnobMode`], NORMAL then the
//! plugins' knob modes). Both are stored by name (compared without regard to case; the built-ins keep their old file names),
//! so adding or removing a plugin does not move a saved choice. A name no list has reads as the default.

use super::FaderCurve;
use crate::app::prefs::{Choice, Setting};
use openfader::plugin::{effect::Slot, registry};

/// Position of `v` in `names`, ignoring case.
fn find(names: &[String], v: &str) -> Option<usize> {
    names.iter().position(|n| n.eq_ignore_ascii_case(v))
}

/// File text of entry `i`; an index past the list reads as entry `default`.
fn text(names: &[String], i: usize, default: usize) -> String {
    names.get(i).unwrap_or(&names[default]).to_lowercase()
}

/// A click on option `i` of the row: ignored past the list.
fn pick(at: &mut usize, names: &[String], i: usize) {
    if i < names.len() {
        *at = i;
    }
}

/// CHANNEL FADER CURVE: index into [`Fader::names`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fader(pub usize);

impl Fader {
    /// The default, [`FaderCurve::Linear`] (its place in [`Choice::ALL`]).
    pub const LINEAR: Fader = Fader(1);

    /// The built-in curves' names, then the FADER plugin effects'.
    pub fn names() -> Vec<String> {
        let plugins = registry().slot(Slot::Fader).iter().map(|e| e.name.clone());
        FaderCurve::ALL
            .iter()
            .map(|c| c.name().to_owned())
            .chain(plugins)
            .collect()
    }

    /// The built-in curve, or `None` for a plugin's.
    pub fn curve(self) -> Option<FaderCurve> {
        FaderCurve::ALL.get(self.0).copied()
    }

    /// The effect's position among the FADER plugin effects, or `None` for a built-in curve.
    pub fn plugin(self) -> Option<usize> {
        self.0.checked_sub(FaderCurve::ALL.len())
    }

    fn parse_in(names: &[String], v: &str) -> Option<Fader> {
        find(names, v).map(Fader)
    }
}

impl Setting for Fader {
    fn parse(v: &str) -> Option<Fader> {
        Fader::parse_in(&Fader::names(), v)
    }
    fn text(&self) -> String {
        text(&Fader::names(), self.0, Fader::LINEAR.0)
    }
    fn pick(&mut self, i: usize) {
        pick(&mut self.0, &Fader::names(), i);
    }
}

/// COLOR FX KNOB: 0 = NORMAL, then the plugins' knob modes in plugin order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KnobMode(pub usize);

impl KnobMode {
    pub const NORMAL: KnobMode = KnobMode(0);

    /// NORMAL, then every plugin's knob mode.
    pub fn names() -> Vec<String> {
        std::iter::once("normal".to_owned())
            .chain(registry().knob_modes().map(|k| k.name.clone()))
            .collect()
    }

    /// Where the knob stops (0..=1, see `MixerState.cfx-stop`); `None` = NORMAL (or a mode that is gone).
    pub fn stop(self) -> Option<f32> {
        registry()
            .knob_modes()
            .nth(self.0.checked_sub(1)?)
            .map(|k| k.stop)
    }

    fn parse_in(names: &[String], v: &str) -> Option<KnobMode> {
        find(names, v).map(KnobMode)
    }
}

impl Setting for KnobMode {
    fn parse(v: &str) -> Option<KnobMode> {
        KnobMode::parse_in(&KnobMode::names(), v)
    }
    fn text(&self) -> String {
        text(&KnobMode::names(), self.0, 0)
    }
    fn pick(&mut self, i: usize) {
        pick(&mut self.0, &KnobMode::names(), i);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn names_are_found_without_regard_to_case_and_unknown_ones_are_not() {
        let list = names(&["steep", "linear", "gradual", "SOFT"]);
        assert_eq!(Fader::parse_in(&list, "Soft"), Some(Fader(3)));
        assert_eq!(Fader::parse_in(&list, "linear"), Some(Fader::LINEAR));
        assert_eq!(Fader::parse_in(&list, "gone"), None);
        assert_eq!(text(&list, 3, 1), "soft");
        assert_eq!(
            text(&list, 9, 1),
            "linear",
            "past the list reads as the default"
        );
        let mut at = 1;
        pick(&mut at, &list, 3);
        pick(&mut at, &list, 4);
        assert_eq!(at, 3);
    }

    #[test]
    fn the_built_in_curves_come_first_in_the_selector() {
        assert_eq!(FaderCurve::ALL[Fader::LINEAR.0], FaderCurve::Linear);
        assert_eq!(Fader(3).curve(), None);
        assert_eq!(Fader(3).plugin(), Some(0));
        assert_eq!(Fader(2).curve(), Some(FaderCurve::Gradual));
        assert_eq!(Fader(2).plugin(), None);
    }
}
