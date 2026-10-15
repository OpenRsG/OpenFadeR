# OpenFadeR

An open-source DJ application written in Rust with [Slint](https://slint.dev). It plays and mixes
two or four decks, browses a music library, drives MIDI controllers, and exports to USB sticks and SD
cards through plugins.

## Features

- **Library**: browse a library through library plugins (see the plugins repository), or open any
  folder: audio files are scanned recursively, tags are read with symphonia and subfolders become
  playlists. Playlist tree, search, sortable columns, track info with cover art, track filter
  (BPM range, key match, rating), related tracks, Tag List, tags, smart playlists, history,
  duplicate finder, missing-file manager. Collection edits (cues, loops, grid, rating, comment, colour)
  are kept in our own store (`$XDG_DATA_HOME/openfader/store.db`); library sources stay read-only.
- **Analysis**: BPM, beat grid, downbeat, key, 3-band waveform, phrases and auto cues, computed
  in the background and cached in `$XDG_CACHE_HOME/openfader/analysis`.
- **Decks**: 2 or 4 decks, scrolling and overview waveforms, play/cue, sync, key lock and key shift,
  hot cues, memory cues, loops, beat jump, slip, reverse, scratch, pad modes (hot cue, loop, roll,
  slicer, sampler, sequencer call, key, stems, effects), quantize, grid editing.
- **Mixer**: trim, 3-band isolator EQ, COLOR FX, channel faders, crossfader, master and headphone
  levels, MERGE FX, recording to WAV, microphone with talkover, per-card sound device management.
- **Effects**: Beat FX with several units and slots, COLOR FX, MERGE FX; more through effect plugins.
- **Sampler** with sequencer, **BEAT LAB** (drum swap and capture), **STEMS** separation
  (ONNX model, runs on CPU or GPU), **AUTOMIX**, **MIX POINT LINK**, **Track Suggestion**.
- **Modes**: PERFORMANCE, EXPORT, EDIT, LIGHTING (DMX / Art-Net / sACN) and VIDEO.
- **Controllers**: MIDI mapping files, MIDI learn, LED feedback, hot-plug detection.
- **Preferences**, themes with an editor, an editable keyboard-shortcut table, a split-screen browser window.

## Build and run

```sh
./start.sh [path]          # first run clones the plugins, themes and mappings (below), builds and runs
cargo build --release      # the core alone; `cargo test --release` tests it (with the companions checked out, their tests too)
```

Without a path the app reopens the last library, else opens `$XDG_MUSIC_DIR` or `~/Music/OpenFadeR`
(created if missing) as a folder library. Settings live in `$XDG_CONFIG_HOME/openfader/` (default
`~/.config/openfader/`).

## Repositories

| Repository | Holds |
| --- | --- |
| [OpenRsG/OpenFadeR](https://github.com/OpenRsG/OpenFadeR) | this core application and the plugin SDK (`sdk/`) |
| [OpenRsG/OpenFadeR-Plugins](https://github.com/OpenRsG/OpenFadeR-Plugins) | library, device, effect and action plugins, checked out as `extension/plugin/` |
| [OpenRsG/OpenFadeR-Map](https://github.com/OpenRsG/OpenFadeR-Map) | controller mapping files, checked out as `mappings/` |
| [OpenRsG/OpenFadeR-Themes](https://github.com/OpenRsG/OpenFadeR-Themes) | themes, checked out as `extension/skin/` |

## Where plugins, themes and mappings are found

Each kind is looked up, in order, in an environment-variable folder list (`:`-separated), a folder in the
config directory, and the bundled folder next to the executable (or in a folder above it, so
`target/release/openfader` finds a checkout's):

| Kind | Environment variable | Config folder | Bundled folder |
| --- | --- | --- | --- |
| Plugins | `OPENFADER_PLUGINS` | `~/.config/openfader/extension/plugin/` | `extension/plugin/` |
| Themes | `OPENFADER_SKINS` | `~/.config/openfader/extension/skin/` | `extension/skin/` |
| Controller mappings | `OPENFADER_MAPPINGS` | `~/.config/openfader/mappings/` | `mappings/` |

`start.sh` clones the three companion repositories into those folders (git ignores them here); the native plugins build
with `extension/plugin/build.sh` against this checkout's `sdk/`. Or point the variables at other checkouts. Plugin
format and the SDK: [docs/plugins.md](docs/plugins.md). Theme files: [docs/skins.md](docs/skins.md).
The mapping format is documented at the top of `src/app/midi/mapfile.rs`.

## Environment variables

`OPENFADER_OUTPUT_DEVICE`, `OPENFADER_HP_CHANNELS`, `OPENFADER_CUE_DEVICE` (audio routing),
`OPENFADER_MIDI_PORT`, `OPENFADER_MIDI_LOG`, `OPENFADER_JOG_TICKS`, `OPENFADER_TEMPO_INVERT` (MIDI),
`OPENFADER_REC_DIR`, `OPENFADER_EXPORT_DIR` (output folders), `OPENFADER_PATH_MAP` (audio path remap,
`from=>to;…`), `OPENFADER_DEVICE_ROOTS` (extra folders treated as devices).

## Keyboard shortcuts

One table drives dispatch and the `?` help overlay (Shift = deck B):

| Key | Action | Key | Action |
| --- | --- | --- | --- |
| Space | play/pause | K | key sync |
| Z / X | CUE A / CUE B | `-` / `=` / 0 | key down / up / reset |
| 1–8 | pads (per pad mode) | M | next pad mode |
| L | 4-beat loop | V / E / B | slip / reverse / brake |
| I / O / R | loop in / out / reloop | T | add to Tag List |
| `[` / `]` | beat jump −4 / +4 | ← / → | FX beat fraction |
| Enter | LOAD | F | FX on/off |
| S | SYNC | Ctrl+F | focus search |
| Q | MASTER | W / Shift+W | VIEW / Area |
| Up / Down | browser selection | ? | help overlay |
| G / Y / N | grid row / tap / beat 1 here | `,` / `.` | grid shift ∓1 ms |
| P | sampler strip | A / Shift+A | AUTOMIX on/off / skip |

## Code layout

See `.omp/rules/code-structure.md`: one folder per section (`library/`, `analysis/`, `audio/`, `plugin/`,
`app/`, …), each with a base `mod.rs` and one child per variant; only `app/` and the `ui/` crate use Slint.
The plugin SDK is the `sdk/` crate.

## License

AGPL-3.0.
