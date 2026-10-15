//! The action table: [`Act`], the key chord [`Bind`], the live [`Row`] and the built-in default [`TABLE`].

use crate::app::deck::DECK_NAMES;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Act {
    PlayPause,
    Cue,
    Load,
    Sync,
    Master,
    /// Hot-cue pad; the key's offset in its range is the pad index.
    Pad,
    /// Auto loop of `CONTROLLER > auto beat loop` beats (`prefs.controller.loop_beats()`).
    AutoLoop,
    /// Export the deck's active loop as a WAV file (`SamplerState.export-loop`).
    ExportLoop,
    /// Jump by this many beats (negative = back).
    BeatJump(i32),
    LoopIn,
    LoopOut,
    Reloop,
    FocusSearch,
    /// Browse view on / off (controller VIEW), browser focus to the next area (controller Area).
    BrowseView,
    BrowseArea,
    /// Key sync to the other deck / key shift by this many semitones / key shift reset.
    KeySync,
    KeyStep(i32),
    KeyReset,
    Slip,
    Reverse,
    Brake,
    /// Cycle the pad mode (HOT CUE → BEAT JUMP → KEYBOARD → BEAT LOOP).
    PadMode,
    /// GRID edit row on / off, tap tempo, beat 1 at the playhead, shift the grid by this many ms.
    GridToggle,
    GridTap,
    GridBeat1,
    GridShift(i32),
    /// Add the browser's selected track to the tag list.
    TagTrack,
    /// FX beat fraction step (arrow keys) / FX on-off.
    FxBeat(i32),
    FxToggle,
    /// Sampler strip on / off (it replaces the Beat FX row), AUTOMIX on / off, AUTOMIX skip.
    SamplerToggle,
    AutomixToggle,
    AutomixSkip,
    /// Switch between the 2-deck and the 4-deck layout.
    DeckLayout,
    Help,
}

impl Act {
    /// The action's part of a row id (stable: it is stored in the settings file).
    pub(super) fn slug(self) -> &'static str {
        let neg = |n: i32, a: &'static str, b: &'static str| if n < 0 { a } else { b };
        match self {
            Act::PlayPause => "play_pause",
            Act::Cue => "cue",
            Act::Load => "load",
            Act::Sync => "sync",
            Act::Master => "master",
            Act::Pad => "pad",
            Act::AutoLoop => "auto_loop",
            Act::ExportLoop => "export_loop",
            Act::BeatJump(n) => neg(n, "jump_back", "jump_fwd"),
            Act::LoopIn => "loop_in",
            Act::LoopOut => "loop_out",
            Act::Reloop => "reloop",
            Act::FocusSearch => "focus_search",
            Act::BrowseView => "browse_view",
            Act::BrowseArea => "browse_area",
            Act::KeySync => "key_sync",
            Act::KeyStep(n) => neg(n, "key_down", "key_up"),
            Act::KeyReset => "key_reset",
            Act::Slip => "slip",
            Act::Reverse => "reverse",
            Act::Brake => "brake",
            Act::PadMode => "pad_mode",
            Act::GridToggle => "grid_toggle",
            Act::GridTap => "grid_tap",
            Act::GridBeat1 => "grid_beat1",
            Act::GridShift(n) => neg(n, "grid_back", "grid_fwd"),
            Act::TagTrack => "tag_track",
            Act::FxBeat(n) => neg(n, "fx_beat_down", "fx_beat_up"),
            Act::FxToggle => "fx_toggle",
            Act::SamplerToggle => "sampler_toggle",
            Act::AutomixToggle => "automix_toggle",
            Act::AutomixSkip => "automix_skip",
            Act::DeckLayout => "deck_layout",
            Act::Help => "help",
        }
    }

    /// How many consecutive keys the action's binding covers (the 8 hot-cue pads), else 1.
    pub(super) fn span(self) -> usize {
        if self == Act::Pad { 8 } else { 1 }
    }
}

/// A key chord. `keys` is a lowercase character, `space`, `enter`, `left`, `right`, or an inclusive range `a-b`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bind {
    pub keys: String,
    pub shift: bool,
    pub ctrl: bool,
}

impl Bind {
    pub(super) fn new(keys: &str, shift: bool, ctrl: bool) -> Bind {
        Bind {
            keys: keys.to_owned(),
            shift,
            ctrl,
        }
    }
}

/// The key characters of a binding as an inclusive range: `a-b`, or a single character as a range of one;
/// `None` for a named key. No allocation: it runs for every row on every key press.
pub(super) fn span(keys: &str) -> Option<(char, char)> {
    let mut cs = keys.chars();
    match (cs.next(), cs.next(), cs.next(), cs.next()) {
        (Some(a), Some('-'), Some(b), None) => Some((a, b)),
        (Some(c), None, ..) => Some((c, c)),
        _ => None,
    }
}

/// One action of the live table: its stable id, the current binding (`None` = cleared) and what it does.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    /// `deck.<action>.<A-D>` or `app.<action>`.
    pub id: String,
    pub bind: Option<Bind>,
    /// Target deck (0 = A); `None` for app-level actions.
    pub deck: Option<i32>,
    pub act: Act,
    pub what: &'static str,
}

/// The built-in default of one action.
pub(super) struct Def {
    pub(super) keys: &'static str,
    pub(super) shift: bool,
    pub(super) ctrl: bool,
    pub(super) deck: Option<i32>,
    pub(super) act: Act,
    pub(super) what: &'static str,
}

impl Def {
    pub(super) fn id(&self) -> String {
        match self.deck {
            Some(d) => format!("deck.{}.{}", self.act.slug(), DECK_NAMES[d as usize]),
            None => format!("app.{}", self.act.slug()),
        }
    }
}

const fn row(
    keys: &'static str,
    shift: bool,
    ctrl: bool,
    deck: Option<i32>,
    act: Act,
    what: &'static str,
) -> Def {
    Def {
        keys,
        shift,
        ctrl,
        deck,
        act,
        what,
    }
}

/// The DEFAULT preset (what the table was before it became editable).
#[rustfmt::skip]
pub(super) const TABLE: &[Def] = &[
    row("space", false, false, Some(0), Act::PlayPause, "play / pause"),
    row("space", true, false, Some(1), Act::PlayPause, "play / pause"),
    row("z", false, false, Some(0), Act::Cue, "cue (hold = preview)"),
    row("x", false, false, Some(1), Act::Cue, "cue (hold = preview)"),
    row("1-8", false, false, Some(0), Act::Pad, "performance pad (per pad mode)"),
    row("1-8", true, false, Some(1), Act::Pad, "performance pad (per pad mode)"),
    row("l", false, false, Some(0), Act::AutoLoop, "auto loop (length: Preferences > Controller)"),
    row("l", true, false, Some(1), Act::AutoLoop, "auto loop (length: Preferences > Controller)"),
    row("i", false, false, Some(0), Act::LoopIn, "loop in"),
    row("i", true, false, Some(1), Act::LoopIn, "loop in"),
    row("o", false, false, Some(0), Act::LoopOut, "loop out"),
    row("o", true, false, Some(1), Act::LoopOut, "loop out"),
    row("r", false, false, Some(0), Act::Reloop, "exit / re-enter loop"),
    row("r", true, false, Some(1), Act::Reloop, "exit / re-enter loop"),
    row("e", false, true, Some(0), Act::ExportLoop, "export the loop as WAV"),
    row("e", true, true, Some(1), Act::ExportLoop, "export the loop as WAV"),
    row("[", false, false, Some(0), Act::BeatJump(-4), "jump back 4 beats"),
    row("]", false, false, Some(0), Act::BeatJump(4), "jump forward 4 beats"),
    row("{", false, false, Some(1), Act::BeatJump(-4), "jump back 4 beats"),
    row("}", false, false, Some(1), Act::BeatJump(4), "jump forward 4 beats"),
    row("s", false, false, Some(0), Act::Sync, "sync (latch) to the other deck"),
    row("s", true, false, Some(1), Act::Sync, "sync (latch) to the other deck"),
    row("q", false, false, Some(0), Act::Master, "master (pin / unpin)"),
    row("q", true, false, Some(1), Act::Master, "master (pin / unpin)"),
    row("enter", false, false, Some(0), Act::Load, "load the selected track"),
    row("enter", true, false, Some(1), Act::Load, "load the selected track"),
    // Decks C and D (4-deck layout): the same actions under Ctrl (Ctrl+Shift = D) for the transport, sync and load.
    row("space", false, true, Some(2), Act::PlayPause, "play / pause"),
    row("space", true, true, Some(3), Act::PlayPause, "play / pause"),
    row("x", false, true, Some(2), Act::Cue, "cue (hold = preview)"),
    row("x", true, true, Some(3), Act::Cue, "cue (hold = preview)"),
    row("s", false, true, Some(2), Act::Sync, "sync (latch) to the master"),
    row("s", true, true, Some(3), Act::Sync, "sync (latch) to the master"),
    row("enter", false, true, Some(2), Act::Load, "load the selected track"),
    row("enter", true, true, Some(3), Act::Load, "load the selected track"),
    row("d", false, false, None, Act::DeckLayout, "2 / 4-deck layout"),
    row("k", false, false, Some(0), Act::KeySync, "key sync to the other deck"),
    row("k", true, false, Some(1), Act::KeySync, "key sync to the other deck"),
    row("-", false, false, Some(0), Act::KeyStep(-1), "key down 1 semitone"),
    row("=", false, false, Some(0), Act::KeyStep(1), "key up 1 semitone"),
    row("_", false, false, Some(1), Act::KeyStep(-1), "key down 1 semitone (Shift+-)"),
    row("+", false, false, Some(1), Act::KeyStep(1), "key up 1 semitone (Shift+=)"),
    row("0", false, false, Some(0), Act::KeyReset, "key shift reset"),
    row(")", false, false, Some(1), Act::KeyReset, "key shift reset (Shift+0)"),
    row("v", false, false, Some(0), Act::Slip, "slip on / off"),
    row("v", true, false, Some(1), Act::Slip, "slip on / off"),
    row("e", false, false, Some(0), Act::Reverse, "reverse on / off"),
    row("e", true, false, Some(1), Act::Reverse, "reverse on / off"),
    row("b", false, false, Some(0), Act::Brake, "brake (vinyl stop)"),
    row("b", true, false, Some(1), Act::Brake, "brake (vinyl stop)"),
    row("m", false, false, Some(0), Act::PadMode, "next pad mode"),
    row("m", true, false, Some(1), Act::PadMode, "next pad mode"),
    row("g", false, false, Some(0), Act::GridToggle, "GRID edit row on / off"),
    row("g", true, false, Some(1), Act::GridToggle, "GRID edit row on / off"),
    row("y", false, false, Some(0), Act::GridTap, "tap tempo (sets the grid)"),
    row("y", true, false, Some(1), Act::GridTap, "tap tempo (sets the grid)"),
    row("n", false, false, Some(0), Act::GridBeat1, "beat 1 here (grid)"),
    row("n", true, false, Some(1), Act::GridBeat1, "beat 1 here (grid)"),
    row(",", false, false, Some(0), Act::GridShift(-1), "shift grid −1 ms"),
    row(".", false, false, Some(0), Act::GridShift(1), "shift grid +1 ms"),
    row("<", false, false, Some(1), Act::GridShift(-1), "shift grid −1 ms (Shift+,)"),
    row(">", false, false, Some(1), Act::GridShift(1), "shift grid +1 ms (Shift+.)"),
    row("t", false, false, None, Act::TagTrack, "tag the selected track"),
    row("p", false, false, None, Act::SamplerToggle, "sampler strip on / off (replaces Beat FX)"),
    row("a", false, false, None, Act::AutomixToggle, "AUTOMIX on / off"),
    row("a", true, false, None, Act::AutomixSkip, "AUTOMIX skip to the next track"),
    row("left", false, false, None, Act::FxBeat(-1), "FX beat fraction −"),
    row("right", false, false, None, Act::FxBeat(1), "FX beat fraction +"),
    row("f", false, false, None, Act::FxToggle, "FX on / off"),
    row("w", false, false, None, Act::BrowseView, "browse view: waveforms + browser only"),
    row("w", true, false, None, Act::BrowseArea, "browser area: tree, Tag List, tracks, related"),
    row("f", false, true, None, Act::FocusSearch, "focus the search box"),
    row("?", false, false, None, Act::Help, "show / hide this help"),
];
