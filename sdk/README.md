# openfader-plugin

SDK for native plugins of OpenFadeR: effects (a native shared library (`.so`) that shows up in a slot of the mixer: the Beat FX
selector (the default: runs in every FX unit, CH1 to CH4, MASTER, stem parts), the Sound Color FX knob, the Mic FX, a Send /
return, the Lever or the channel fader) and track libraries (see [Library plugins](#library-plugins) below). The effects part has
no dependencies. Write it in Rust with the safe `Effect` trait, or in C/C++/anything with the header `openfader_plugin.h`.

Most effects need no compiling at all: `plugin.json` can declare an effect as a chain of the host's DSP blocks (delay, filters,
reverb, pitch shifter, loops …) with small formulas, see the Effects section of `docs/plugins.md`. Write a native library when
the blocks cannot say what you want. Plugins that add track sources, menu commands or event hooks are separate (a process
speaking JSON, same document); one plugin folder can have all of them.

## Install

A plugin is a folder with a `plugin.json` that names the library:

```json
{ "name": "bitcrush", "version": "0.1.0", "effects": "libbitcrush.so" }
```

Put the folder in `~/.config/openfader/extension/plugin/` (or in a folder listed in `$OPENFADER_PLUGINS`) and restart the app.
Plugins are read once at start; effects appear after the slot's built-ins (Beat FX: the nine, Sound Color FX: the six,
fader: its three curves), in plugin order. A library that does not load (missing file, no entry point, wrong ABI, bad
effect table) is reported on stderr as `plugin: <name>: <reason>` and skipped. Effect names must be unique within
their slot, also against the built-ins, ignoring case. The saved choice is stored by name, so adding or removing
plugins does not shift it.

## Rust

`Cargo.toml` of the plugin:

```toml
[lib]
crate-type = ["cdylib"]

[dependencies]
openfader-plugin = { path = "/path/to/OpenFadeR/sdk" }
```

`src/lib.rs`:

```rust
use openfader_plugin::{Ctx, Effect, Frame, export_effects};
use std::ffi::CStr;

struct Wobble { phase: f32 }

impl Effect for Wobble {
    const NAME: &'static CStr = c"WOBBLE";                 // selector label, 1 to 16 characters
    fn new(_rate: u32) -> Wobble { Wobble { phase: 0.0 } } // allocate here
    fn tick(&mut self, x: Frame, c: &Ctx) -> Frame {       // once per stereo frame
        self.phase = (self.phase + 1.0 / c.period.max(1.0)).fract();
        let g = 0.5 + 0.5 * (self.phase * std::f32::consts::TAU).sin();
        c.mix(x, [x[0] * g, x[1] * g])                     // LEVEL and the on/off ramp applied; exactly `x` while off
    }
    fn reset(&mut self) { self.phase = 0.0 }
    fn engage(&mut self) { self.phase = 0.0 }              // optional: the unit was switched ON
}

export_effects!(Wobble);                                   // several: export_effects!(A, B, C)
```

`cargo build --release`, copy `target/release/libyourplugin.so` next to `plugin.json`. A complete example:
`examples/bitcrush/` (`cargo build --example bitcrush` in `sdk/`, then copy `libbitcrush.so` from the target
folder's `examples/` next to `examples/bitcrush/plugin.json`).

Keep the default `panic = "unwind"`: `export_effects!` catches a panic in `tick`, passes the input through from then on
and revives the effect at the next `reset`. (`panic = "abort"` would take the app down.)

## Slots

`const SLOT: Slot = Slot::Send;` in the `Effect` impl (C: `RkEffect.slot = RK_SLOT_SEND`) picks the slot; the default
is `Slot::Beat`. The table in the `Slot` documentation (`src/lib.rs`) says per slot what the input frame is, what the
output replaces, and what `level`, `g` and `knob` mean:

| Slot | Runs on | `knob` |
|---|---|---|
| `Beat` | channel or master (Beat FX unit) | unused (0.5) |
| `Color` | channel after the EQ (COLOR FX) | the knob, -1 to 1 |
| `Mic` | microphone after its level | MIC FX parameter, 0 to 1 |
| `Send` | sum of the channel sends; outputs the return only | parameter, 0 to 1 |
| `Lever` | master | lever, 0 (rest) to 1 |
| `Fader` | channel before its fader; applies the gain itself | fader, 0 to 1 |

## DSP toolbox

`openfader_plugin::dsp` is the host's own building blocks, so a plugin sounds like the built-ins: `Line` (delay line
with interpolated reads), `Lfo`, `sweep`, `fold`, `ramp`, `OnePole` (low / high pass), `Svf` (resonant state-variable
filter: low, band, high), `Shifter` (two-tap pitch shifter), `Allpass` and `Reverb` (Freeverb). All allocate in their
constructors only.

## Bundled plugins

The effects the built-ins lack live in `extension/plugin/` as JSON-only plugins (`beat-fx`, `mic-fx`, `send-fx`, `lever-fx`,
`cross-pass`, `soft-mix`, and the small examples of `tutorial-fx`): one `plugin.json` each, built from the host blocks of
`docs/plugins.md`, nothing to compile. The host blocks are made of the `dsp` toolbox above, so a native effect and a JSON one
sound alike. `extension/plugin/` loads with no setting.

## C

```c
#include "openfader_plugin.h"
/* create / tick / destroy are required, reset / engage may be NULL */
static const RkEffect EFFECTS[] = {{"MYFX", create, tick, NULL, NULL, destroy}};
static const RkPluginInfo INFO = {RK_ABI, 1, EFFECTS};
const RkPluginInfo *openfader_plugin_v1(void) { return &INFO; }
```

```sh
cc -O2 -shared -fPIC -I /path/to/OpenFadeR/sdk myfx.c -o libmyfx.so -lm
```

A complete example: `examples/ringmod/` (`ringmod.c`, `plugin.json`; `build.sh` builds `libringmod.so` in place).

## The ABI (version 1)

`openfader_plugin_v1() -> *const PluginInfo` is the only exported symbol. The version is part of the name, so a
host that does not know it finds no symbol; `PluginInfo.abi` is checked too, before anything else is read. Layouts are
`#[repr(C)]` in `src/lib.rs` and mirrored in `openfader_plugin.h`; the host uses the same `Ctx` type as the SDK.

| `EffectVTable` field | Call |
|---|---|
| `name` | NUL-terminated label, 1 to 16 characters of ASCII letters, digits, space, `-`, `_` |
| `create(rate) -> inst` | new instance at `rate` Hz, NULL on failure (the host calls it once at load as a check, then once per FX unit) |
| `tick(inst, frame, ctx)` | replace `frame[0..2]` (left, right) with the output |
| `reset(inst)` | forget everything: the effect was selected, or woke from idle |
| `engage(inst)` | the unit was switched ON: restart phases or captures |
| `destroy(inst)` | free the instance |
| `slot` | `Slot` as its number, 0 (a zeroed field) = Beat; last field |

`Ctx` per frame: `rate` (Hz), `period` (samples per selected beat fraction, tempo-slewed), `beats` (the fraction, 1/8
to 16), `level` (LEVEL knob 0..1), `g` (engage ramp 0..1, 5 ms), `knob` (the slot's own control, see Slots). Build the
wet signal, then blend it with
`level * g` (`Ctx::mix` in Rust): while off the output is then bit-exact input and the host skips the unit once your
tail is quiet. An effect with a tail (delay, reverb) should keep sounding after `g` falls to 0; the host keeps calling
`tick` until the output matches the input within -120 dBFS for a period.

Rules:
- `tick`, `reset` and `engage` run on the audio thread: **no allocation, locks, I/O or blocking**. Do all of that in `create`.
- One instance is never called from two threads at once, but may move between threads (`Effect: Send`).
- `tick` must not unwind or longjmp out; non-finite output is dropped by the host (the effect is reset).
- There is no phase lock to the deck's beat grid: the unit knows the tempo, not the beat position. Keep your own phase
  and restart it in `engage` (the DJ presses ON on the beat).
- The library is never unloaded while the app runs.

## Library plugins

The same crate, with the `library` feature, is the SDK of **library plugins**: a native library that reads a format of track
libraries (a file or a folder) for the host, hands over beat grids, cues and waveforms, and may write the host's collection in
its format. The types (`Track`, `Playlist`, `Analysis`, `Snapshot`) are the ones the host itself uses; the ops and their JSON are
in the Library plugins section of `docs/plugins.md`.

```toml
[lib]
crate-type = ["cdylib"]

[dependencies]
openfader-plugin = { path = "/path/to/OpenFadeR/sdk", features = ["library"] }
```

In Rust implement [`LibraryPlugin`] (`probe`, `open`, optionally `write_collection`) and [`Source`] (the opened library:
`playlists`, `tracks`, optionally `search` and `analysis`) and call `export_library!(MyFormat)`: the macro defines the
`openfader_library_v1` entry point and keeps panics from crossing the C boundary (an `{"error"}` answer instead; keep the default
`panic = "unwind"`). `playlist_tree` turns flat folder rows into the depth-first list `playlists` answers with. Examples: the
library plugins of `extension/plugin/` (one Cargo workspace; `build.sh` builds them and copies each library next to its
`plugin.json`).

### The library ABI (version 1)

`openfader_library_v1() -> *const LibraryApi` is the only exported symbol; `LibraryApi.abi` is checked before anything else.

| `LibraryApi` field | Call |
|---|---|
| `abi` | `LIBRARY_ABI` (1) |
| `call(req, len) -> Buf` | one JSON request in (an object with an `"op"`), one JSON answer out: `Buf { ptr, len }` is owned by the library until given to `free`; an `{"error": "text"}` answer is a failure, an unknown `op` is one too; never unwinds |
| `free(buf)` | frees an answer of `call`, once |

From C, build the requests and parse the answers with any JSON library; `openfader_plugin.h` has the table layout (`RkLibraryApi`,
`RkBuf`). One library may export both entry points (effects and a track library). The library is never unloaded while the app runs.
