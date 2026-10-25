# OpenFadeR code structure

## Layout: one folder per section, base + children
```
src/
  main.rs            thin: CLI args → app::run
  lib.rs             core sections only (no Slint)
  library/           mod.rs = base: Track, Playlist, trait Library, open(), mini XML reader, path remap; children = built-in sources (folder, collection, smart, playlist files); device/third-party library formats live in library plugins (sdk/src/library, host in plugin/library.rs), never in core
  analysis/          mod.rs = base: Analysis, load_track, analyze; children = one per analysis family (beatgrid, cues, waveform, phrase) + key, cache (on-disk), detect/ (analysis from PCM; grid child), auto/ (phrase analysis + auto cues)
  store/             our SQLite collection edits (cues, grid, rating, comment, colour, playlists, MyTag, history) + Library overlay; library sources stay read-only
  mixstyle/          core Automix recipe types and timeline evaluation; builtin.json is the built-in catalogue, random.rs chooses non-repeating styles; plugin manifests add recipes in `automix`, app/automix/{style,fade,rig}.rs selects, clocks and applies them to the live controls
  export/            device export writer: format-neutral plan and per-track writes; the on-device formats are written by plugins
  stems/             STEMS separation (Demucs ONNX job, model fetch, stems cache); playback mixing lives in audio/stems.rs; a stem file's own parts (audio/stemfile.rs) take the place of the separation in `Job::from_stem_file`
  inbox/             INBOX → REVIEW → COLLECTION: mod.rs = base (Config); children = scan (watched folders, zips, duplicates), rules (where a file goes: stage folder + pattern), moves (a batch of moves, undo of a batch); the stages and batches live in store/inbox.rs, the undo of a move's files in the journal replay
  stems/             STEMS separation (Demucs ONNX job, model fetch, stems cache); playback mixing lives in audio/stems.rs
  edit/              EDIT mode: edit-decision list over source PCM, snapping, WAV export
  lighting/          LIGHTING mode: fixtures, DMX, phrase scenes, Art-Net/sACN output
  video/             VIDEO: ffmpeg pipe decoder, playhead→frame clock, crossfader mix
  audio/             mod.rs = base: Pcm, Loop/advance, render(), SPSC Ring, output-device lookup; children = decode, device/ (OS devices: capabilities, cards with friendly names/ports, card volume via ALSA mixer or PipeWire), player/ (one shared output stream), transport, bus (master/headphone routing), cue (2nd device), rec (WAV), input (REC input pair, plus `Feed` / `Keeper` for the mixer inputs), mic (microphone + talkover), auxin (AUX / LINE channel), duck (ducker shared by mic and AUX), stretch; dsp/ (eq, filter); cfx/ (COLOR FX); fx/ (Beat FX, one file per effect); merge/ (MERGE FX); sampler/
  paths.rs           shared output folders + timestamped names, `at` (path errors), `on_device`, `file_stem`
  zip.rs             PKWARE zip reader/writer shared by sample packs and store backups
  diag/              mod.rs = base: init (log tee + panic hook), the session facts the app sets, report(), pending_crash(); children = log (rotating log file, stdout/stderr tee), crash (panic hook, crash-*.md), report (the markdown format, issue URL), redact, sys (OS/CPU/RAM); docs/diagnostics.md
  update/            mod.rs = base: GitHub release lookup (REST, `OPENFADER_UPDATE_API`; Stable = `releases/latest`, Beta = the `releases` list, `pick`), semver compare, asset per platform, roll back (puts the pre-update library back with `store::backup::roll_back_store` when the new version migrated the store past the old one); children = install (download with SHA-256 check, unpack, swap with `<install>.prev`, roll back, which installs the app may replace), state (`<config>/update.json`: last check, skipped version, version just installed with the store schema it replaces and its automatic backup)
  plugin/            mod.rs = base: Manifest (plugin.json, incl. declarative knob_modes / pad_modes / settings), discovery via paths::search_path, registry() (the start-up read: effects, sources, knob and pad modes; loaded before the audio engine; effects grouped by Slot), current() / reload() (the live read: actions, events, settings, the plugin manager), Plugin::call (a Lua script in-process, else one process per JSON request); children = config (plugins.json: switched-on plugins, setting values; DEFAULT_ON plugins are on while the file does not exist), install (folder / zip / plugin.json into the user plugin folder, uninstall), lua (the in-process Lua runtime and its `rk` API), source/ (Library decorator: never blocks, worker threads + notify; reply, fetch), action (commands), event (events, timers, the command sink), effect/ (per Slot: beat, color, mic, send, lever, fader; mod.rs = base: Effect, load, name rules; children = native (C ABI of sdk/, which also owns the dsp helpers the host re-exports) and chain/ (JSON effects: mod.rs = Block trait + parser, expr = formulas, one file per block family: basic, delay, filter, container, capture, sugar))
  (repo) extension/  bundled add-ons found via paths::search_path: plugin/ (example plugins; the effect ones are JSON only, one plugin.json per hardware family, no compiling) and skin/ (example skins)
  app/               Slint binary side (not in lib.rs)
    mod.rs           base: run(), window construction, connects children
    keys.rs          the one shortcut table (dispatch + ui/help.slint overlay)
    diag.rs          Help > Report a Problem ↔ ui/report.slint (ReportState), the crash offer at start, the renderer/audio/controller/plugin/settings facts for the report
    camelot.rs       shared key maths + key notation (used by deck, browser, trackinfo)
    prefs/           settings file + PREFS dialog ↔ ui/prefs.slint; others read prefs::with(|p| …) (borrow) or prefs::get() (owned copy) and on_change
    browser/         mod.rs = base; children = filter, history, health (the Library health node: the scan of the Collection runs on a worker, `library/health.rs` holds the checks) ↔ ui/browser.slint (BrowserState, AutomixState)
    trackinfo.rs     selected-track panel ↔ ui/trackinfo.slint (global TrackInfoState)
    mixer/           mod.rs = base; children = map (knob→dB), vu, curve, auxin (the AUX strip) ↔ ui/mixer.slint (global MixerState)
    deck/            mod.rs = base: Deck (one type, instantiated per deck), snap/beat helpers, controller API; children = load, tick, frame (what moves every frame: the detail strip, platter, phase meter, paced by the window's rendering), transport, cue, sync, loops, slip, pads, cuelist, key, grid
                     ↔ ui/deck.slint + ui/pads.slint (global DeckState holding `[DeckView]`, callbacks take the deck index)
    fx/, sampler/, groove/, export.rs, analyze/, automix/, midi/   one feature each ↔ own .slint + global; callbacks wired with app::on! (one macro in app/mod.rs)
                     midi/ reads our controller mapping files (`mappings/*.xml`, the OpenFadeR-Map repository; found through paths::search_path with OPENFADER_MAPPINGS)
    plugins/         mod.rs = base: Plugins menu, action prompt, commands of actions and event replies, timer events ↔ ui/plugins.slint (PluginState); child manager (the plugin manager in PREFS > EXTENSIONS)
    update.rs        the update check at start, Help > Check for Updates and UPDATE NOW ↔ ui/update.slint (global UpdateState); the plugin-update count reuses `catalog`
    drop/            files dragged in from the desktop ↔ FileDropState (ui/dnd.slint): plugins, themes, music; children = x11 (winit's file drops), wayland (our own wl_data_device on winit's connection)
    skin/            mod.rs = base: Skin, skin list, rescan, apply (restyles every window from skin files); children = file (parse / write), store (the user's skin folder), draft (theme editor's working copy), page (PREFS > THEMES ↔ ui/themes.slint, global ThemeState)
    render/          mod.rs = base: Axis, Canvas, Wave, colour helpers; children = overview, detail, phrase, font, backend (live waveform compositor choice and graphics API detection)
ui/                  its own crate (`openfader-ui`, Cargo.toml + build.rs + lib.rs here): the Slint compile and the code generated
                     from the globals (split-window mirror, skin tokens); app/ re-exports it. Kept apart so a Rust edit in app/
                     does not recompile the generated UI (tens of MB of Rust), and a .slint edit does not recompile the core lib
  app.slint          window layout only; re-exports each section's global
  <section>.slint    one component + one `export global <Section>State` per section
  theme.slint        `Theme` global: every colour, font size, spacing and region height; section files use tokens, no raw hex
  widgets.slint      small shared widgets reused by several sections (+ gauges.slint, icons.slint, dialog.slint frame)
tests/
  common/mod.rs      shared test helpers (device root, keys); tests/<topic>.rs use `mod common;`
```
- **Base (`mod.rs`)** owns the section's shared types, traits and helpers, plus the section doc comment listing its children. It holds no format- or variant-specific logic.
- **Child** = one variant/format/stage. It depends on its base (`use super::…`), never on a sibling. Shared code two children need moves up into the base.
- New section → new folder with `mod.rs`. New variant → new child file. A file over ~400 lines is split into children.
- `app/` (and the `ui/` crate it re-exports) is the only place that imports `slint`. Core sections never depend on `app/`.
- Slint inlines every use of a component whose root binds `visible`, `opacity`, a drop shadow or an animated public property
  (e.g. `animate background`), and every use given children, `clip` or layout padding. For widgets used many times, put such
  bindings on a child element (`face := Rectangle { … animate background … }`), never on the root: each inlined use is a full
  copy in the generated code.
- Large views and overlay groups use permanent `if true` item trees to create separate heap-allocation boundaries. Named Slint components alone remain embedded by value. Keep both individual constructors and nested initialization within the 1 MiB stack budget exercised by `.github/smoke.sh` on Linux; dialog visibility and state retain their existing lifetimes.
- Each Slint section talks to Rust only through its `<Section>State` global. Rust sets it via `app.global::<…State>()`. Do not add root `App` properties for a section.
- Multi-instance sections (decks) use ONE global holding an indexed model plus index-taking callbacks. Never write per-instance A/B copies.

## DRY
- Before writing a helper, grep the base modules (`library/mod.rs`, `analysis/mod.rs`, `audio/mod.rs`, `app/render/mod.rs`, `tests/common/mod.rs`). Reuse or extend; never re-implement a sibling's code.
- Duplicate logic in two children (SQL open/key, playlist-tree build, byte readers, colour mixing, ms↔pixel mapping) → move it to the base on the second occurrence.
- Constants (sample rates, entries/s, sizes) are defined once, in the module that owns the concept, and are imported elsewhere.
- Hot paths (audio callback, per-frame render, timers, per-row browser code) never clone `Prefs` or `Track`: use `prefs::with` and `Deck::serial()`/`with_track`. The audio callback allocates nothing (`player/mix.rs` test).
- Moving deck readouts update in the rendering notifier when available; the deck timer retains paused updates and renderer fallback. Beat-count lookups binary-search the ordered beat grid rather than scanning it per frame.
- Tests share fixtures through `tests/common/` or `examples/make_fixture.rs`, not through copied helpers.

## Boundaries & evidence
- Library sources return `library::Track`/`Playlist` through `trait Library`. The UI never matches on a concrete source.
- Machine-local paths, keys and user data are never in tracked files. USB tests read `OPENFADER_USB` and skip when it is unset. Keys come from env vars or the git-ignored `.local/` files.
- Format claims are labelled inferred or ours in the doc comment of the child that implements them. The core names no third-party software; only plugins and mapping files may.
- Repositories: core (`OpenFadeR`), `OpenFadeR-Plugins` (`extension/plugin/`), `OpenFadeR-Map` (`mappings/`), `OpenFadeR-Themes` (`extension/skin/`).

## Translations
- Every text a user sees is marked: `@tr("…")` in `.slint` files, `tr!` / `tr_n!` in `src/app` (`n_!` in a table, translated where it is shown with `i18n::tr_dyn`). Ids, setting values, compared strings and log output are never marked. See `src/app/i18n.rs`.
- Placeholders follow Slint: `{}`, `{0}`, `{n}` (the count of a plural). Numbers are formatted before they are passed.
- After adding or changing a text, run `python3 ui/translations/extract.py`: it updates `ui/translations/openfader-ui.pot` and adds the new texts to each language's `.po` file, where they show in English until translated. The test `every_language_translates_every_text_with_its_placeholders` lists the missing ones.
- A text an extension gives (its `plugin.json`, a reply, a theme's description) is never marked: show it through `Plugin::tr` (or `Registry::tr` by plugin name), device replies through `Loaded::call_shown`, theme descriptions through `Skin::shown_description`. They read the extension's own `locales/` (`src/locale.rs`, `docs/plugins.md` Languages).
- Texts drawn on canvas pixels (`app/render/font.rs`, A–Z only) stay English.

## Performance contracts

- Delay-line reset invalidates history in constant time. Interpolated reads must treat both unwritten taps as silence; resetting an effect must not clear its full buffer inside the callback.
- `audio/fx/prepare.rs` owns replacement construction and retirement. Each unit keeps its selected stage and bounded handoff slots. Initialize the handoff mutex during construction because Darwin allocates its native mutex on first lock. Initial stages are built during stream setup; callbacks only try a handoff when selection changes. Pending selections pass dry audio, and the latest selection wins.
- Deck loading sends playable `Arc<Track16>` after worker-side gain and auto-cue preparation, before optional analysis. Late results merge into the current load without overwriting edits or disturbing transport.
- A deck track, a browser preview track and a bulk-analysis track never exist as a full-length f32 buffer: `audio::decode_track16_cancellable` quantises each packet into one `Track16` buffer (reserved from the header's frame count) and hashes the audio fingerprint from the decoder's f32 frames (`analysis::fingerprint::Window`, so the fingerprint is the one `fingerprint::of` gives the f32 decode). Gain, auto cue, the fallback waveform and the analysis read that same `Arc<Track16>` through `audio::Frames`, which f32 slices implement too. Sampler, edit and groove stay on f32 `Pcm`.
- Bulk runs share `app/analyze/job.rs`'s process-wide budget. Compatible queued or running requests share computation; cue guides, tag writes and progress remain subscriber-specific. Bulk detectors run sequentially per track rather than nesting detector threads inside track workers.
- Cancellation is checked between decode packets, during detector scans and after acquiring inference locks. An active ORT inference cannot be interrupted. Video decode cancellation kills and reaps its ffmpeg child; dropping a result receiver alone is insufficient.
- Stem jobs allocate off the UI thread. Engine availability is established before claiming a segment. Completed instant doubles share validated audio/stem Arcs instead of decoding or reading the stem cache again.
- Separation consumes the borrowed ORT output and deposits resampled frames directly. Preserve interpolation, overlap weighting and quantization; compare against an independent scalar reference.
- Waveform peaks and grid hop energies share one band-filter traversal. Stem-wave normalization is cached per loaded PCM; cancelled waveform workers retain their slot until they exit, and stale results cannot install.
- Tempo search retains every fine candidate, resolution and strict tie order. Conservative bounds may reject losing candidates; worst-case search remains quadratic. Tests retain exhaustive scoring as the reference.
- Browser facts are cached and cell models are constructed on demand. Invalidate metadata, detected analysis, missing-file markers and display preferences explicitly. Preview generations refresh reused delegates after sorting or filtering.

Run optimized timing checks with `cargo test --release --lib benchmark_ -- --ignored --nocapture --test-threads=1`.
Browser and stem-wave comparisons use the binary filters `benchmark_lazy_browser_search_sort` and `bench_cached_stemwave_normalization`.
`examples/stems_bench.rs` reports first-usable stems, whole-track time and Linux peak RSS; use a fresh cache for cold runs and compare identical profiles and inputs.
Allocated buffer payload, anonymous resident memory and total RSS are different measurements. Kernel timings do not establish controller-to-audio latency.
