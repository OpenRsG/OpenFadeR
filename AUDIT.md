# Codebase Audit — OpenFadeR

Rust/Slint native DJ application · sampled core/UI/SDK/compatibility scope: 150,244 Rust code lines and 17,052 Slint code lines · 145 commits, 3 contributors · audited 2026-10-27.

Archetype: long-lived interactive product with a real-time audio engine.

## Verdict

**Prioritize event-loop blocking and memory ownership, not a waveform rewrite.**

What this repo does: OpenFadeR loads and analyzes music, plays/mixes two or four decks, draws scrolling waveforms, and integrates controllers and external libraries. The assumed load is two/four decks during library browsing and background analysis, including 100k-track libraries and memory-constrained devices.

The existing compact PCM, shared audio ownership, waveform strips, lazy browser rows, bounded workers, and generation-checked publication are valuable. The clearest remaining opportunities are redundant device discovery on the UI thread, synchronous track-selection cache reads, repeated library sorting/index reconstruction, unbudgeted result publication, and stem-result buffers outside the inference admission budget. A fresh-process probe also confirmed a one-time resampler allocation that should be prepared before the audio callback can reach it.

“Footprint” here means CPU use, RAM, VRAM, installed bytes, and disk cache. It does not mean privacy fingerprinting.

The baseline audit changed no application behavior and installed nothing. Its temporary benchmark sources were removed. Findings below separate measured results, observed source behavior, and inferred impact; source line numbers describe that baseline.

## Implementation record — first approved batch

Completed 2026-10-27: P0-1/F-01, P0-2/F-02 and P0-3/F-09. The next implementation record identifies subsequent P1 work.

| Item | Implemented change | Verification |
|---|---|---|
| Shared discovery | Existing coalesced device worker publishes devices, configured plugin sources, Explorer root and suggestion detections. Browser collection consumes this snapshot; configuration revision and registry replacement invalidate it. | Startup/configuration and held-worker regressions; native synthetic USB sidebar icons changed 1 → 2 → 1 with the device pane closed. |
| Selected-track analysis | Validated summary-prefix reads and deferred library phrase reads run off the event loop. One active read plus one replaceable pending request; bounded memo includes source analysis reference, library scope and store generation. Publication updates analysis labels without replacing drafts. | Stale source/store responses, same-file/different-reference, miss-then-analysis, explicit phrase deletion and pinned-target regressions. Native playback continued through 80 alternating selections; selected Audit 2 changed from no BPM to 120 BPM / 7A after real background analysis without reselection, while its unsaved comment remained intact. |
| Cold audio preparation | Mixer construction initializes the existing sinc table before callback publication; no DSP algorithm or position mapping changed. | Fresh-process real-mixer regression failed before with one callback allocation and passed after with zero; it also checks audible output. |

Integration gates: 1,868 workspace tests passed, mobile host compile passed, 14 vendored CPAL checks passed, and existing lazy-browser/paced-waveform benchmarks passed. Workspace totals exclude the nested child-process result to avoid double-counting it. The paced two/four-deck 60/120Hz cases had zero CPU-budget misses; pure filtering/sorting was not optimized in this batch and no benchmark speedup is claimed.

Native NVIDIA verification used private Xvfb at 125% with synthetic six-minute tracks and a private hardware-disabled audio server. All 60 repeated CUE release captures matched the primary cue triangle exactly at the 55ms check; actual held-preview movement was verified first. Automatic hot cue A overlapped the old memory-M reference, so this run captured the primary CUE marker instead. This is not an input-to-photon measurement.

Evidence and installed-artifact identity: `.local/performance-p0/results.json`, regression/benchmark logs and native screenshots. No owner collection/settings, physical USB, MIDI or audio hardware was modified. Full 100k-library native scrolling, inference-memory changes and physical-controller latency were not part of this batch.

## Implementation record — second approved batch

Completed 2026-10-27: P1-1/F-03, P1-2/F-04 and the shared-list index-reuse item P1-3/F-06.

- Analysis submission retains shared track snapshots and selected row indices. The Track menu dispatches analysis before building the general browser context, avoiding its selected-track and whole-visible-list string clones. Integer selection capture remains linear, not constant-time.
- Existing workers register identities in chunks of 64, preserving admission order and the per-key preparation/commit FIFO. Materialized pending subscribers are capped at 64. Publication yields while request identities are registering rather than dropping valid unrelated results; a short publication guard fences each accepted chunk against concurrent admissions.
- UI delivery consumes at most 128 results and 128 notices per turn and coalesces selected-track/progress refreshes. Full backlogs schedule later-turn continuations instead of waiting 500ms per chunk. Registration records and completed-result queues can still occupy O(N) memory; this is not a constant-memory pipeline.
- Browser rows retain one stable full-list sort permutation. Queries/tags/reference filters select from it; manual/ranked lists bypass it. New list Arcs invalidate ordering; detected-only changes invalidate BPM/key ordering without rebuilding unrelated title ordering. Empty queries avoid displayed-key formatting, which is otherwise lazy once per track.
- Library key/index data is retained when the library returns the identical shared list Arc after an unrelated store write. Metadata and Collection changes still replace the cached payload. This does not implement the separate external-source metadata-restoration lookup optimization mentioned in the broader F-06 discussion.

Matched production-shaped benchmark: both paths use cached facts and lazy visible rows; only sort caching differs.

| Workload | Repeated uncached sorting | Cached permutation | Retained permutation |
|---|---:|---:|---:|
| 20k tracks, ten title/BPM search refreshes | 14.927 ms | 8.675 ms | 160,000 bytes |
| 50k tracks, ten title/BPM search refreshes | 38.152 ms | 25.053 ms | 400,000 bytes |

Each cached run built two orders instead of ten. This trades one retained `usize` per track for fewer temporary folded-key allocations/sorts; at 100k tracks that vector would occupy 800,000 bytes on this host. The first full-list sort may cost more than sorting a tiny filtered subset; no universal per-query or whole-application speedup is claimed.

Key/index-only benchmark, five unrelated generations: rebuilding 100k indexes took 100.006ms; shared-index reuse took 3.26µs after initial construction. This excludes the library query/store-generation lock and is not an end-to-end UI latency figure.

The real shared-pool warm-cache benchmark processed 100k cached tracks: index-vector capture 137.141µs, admission 5.39µs with an already-running pool, producer completion 411.312ms, and 782 consumer chunks totaling 40.719ms of consumer work. Longest measured consumer chunk was 6.318ms. Its reported 456.975ms end-to-end span excludes fixture creation and the initial capture, and the benchmark drains chunks synchronously rather than measuring actual event-loop scheduling. The separate 10k run paid initial worker startup; these are single warm-cache samples, not physical storage guarantees.

Verification: 1,882 workspace tests passed (nested cold-process output counted once), mobile host check and 14 CPAL checks passed, and all paced two/four-deck 60/120Hz cases had zero isolated waveform CPU-budget misses. A final release build removed the only compiler warning.

Native NVIDIA/Xvfb verification used 129 synthetic tracks while a six-minute deck played: title ascending/descending, multiword substring search, BPM sorting after all 129 analysis results arrived, metadata edit and undo under the cached title order. All 129 results completed and set cue lists; twelve half-second waveform samples during the run were distinct. Query `track 12` correctly matched eleven rows, including `Track 012` and `Track 112`, preserving substring semantics. Forty subsequent CUE release samples matched the primary marker exactly at the 55ms check after the resting reference was ready.

Evidence, limitations and installed-artifact identity: `.local/performance-p1/results.json`, workload logs and native screenshots. No owner settings/collection or physical hardware was modified. Stem admission, row retention and shared separation were outside this batch; the next record covers them.

## Implementation record — remaining P1 footprint work

Completed implementation: F-07 output ownership, F-08 compatible in-flight sharing and F-05 formatted-row retention.

- Output admission precedes allocation/cache reads and embedded-part decoding. A shared RAII lease accounts for four stereo i16 arrays and segment bookkeeping until the last `Stems` consumer drops. Lease release is atomic, with no lock or I/O on the audio thread. The output budget is one eighth of effective RAM; unknown capacity falls back to 256MiB. Too-large outputs fail explicitly, while occupied capacity exposes a cancellable memory-wait state and leaves original audio available.
  Admission uses owned reservations rather than repeatedly polling cgroup limit-minus-current: that counter includes reclaimable file cache and can defer otherwise affordable work. This also removes repeated `/proc` reads from memory-wait loops.
- Cache chunks and directory-lock waits are cancellable. A cancelled write removes its partial file without replacing an existing complete entry. Embedded-part decoding limits appends to the mix length before allocation can grow beyond the reservation.
- Decks subscribe to one compatible native producer through a weak registry. Worker-prepared identities include source version and exact decoded-PCM digest; actual stem files additionally fingerprint their encoded parts. Quality, rate/length, threading and stem-file settings remain distinct. Completion/error delivery is independent per subscriber, SPEED focus includes all live consumers, and only the final subscription cancels the native job. Identity reads/hashing stay on the existing two preparation workers.
- Browser rows keep 64 strong hot entries plus weak references to models still owned by Slint. Destruction notices retire dead bookkeeping without scanning all visited rows per lookup. Live evicted handles retain identity and receive reference, played/reset, detected, preference and metadata updates; removed indices reset their old handles. Externally held handles are intentionally not bounded.

The 100k-track scrolling benchmark used a 48-row viewport: final retained models 64, sampled peak 67, weak-map capacity 224 and retirement-queue capacity 4. Traversal took 50.879ms. Baseline/final RSS were 126,824/127,400KiB; process high-water RSS was 129,604KiB. These are one workload's absolute measurements, not a before/after whole-app RAM reduction.

Real core jobs in 2GiB cgroups retained four independently allocated six-minute inputs. Each trial admitted one output while three reported memory wait:

| Trial | Maximum reported process high-water RSS | Cancellation/release |
|---|---:|---:|
| Cached outputs, including held-consumer blocking and resume | 498.0 MiB | 50 ms |
| Actual CPU model opening and first completed segment | 1,556.7 MiB | 2,499 ms |
| Full CPU separation and cache publication: 62 segments, 106.7s | 1,965.1 MiB | 50 ms |

The cached trial used valid synthetic zero-valued cache payloads and a missing model path, proving cache hits need no inference. The first CPU trial stopped after a segment to exercise active cancellation; the final trial ran the real original model through the whole six-minute track and cache publication. Observed swap peak and OOM counts were zero. The first CPU trial recorded 291 cgroup limit-pressure events; the full run recorded none, with cgroup peak charge 2,140,397,568 bytes. Its process high-water RSS was close to the 2GiB limit: this is not proof that a full application with extra inputs/UI will fit that limit.

Verification gates: 1,902 workspace tests passed (nested subprocess output counted once), mobile host check and 7 vendored CPAL unit tests passed (`cargo test --release --manifest-path vendor/cpal/Cargo.toml --lib`). The real cached subscription smoke proved one native producer/output, two independently decoded consumers, exact cached samples, independent outcomes and release after the final consumer. All four paced two/four-deck 60/120Hz waveform cases had zero isolated CPU-budget misses.

Native NVIDIA/Xvfb: ordinary loads of the same six-minute song showed shared progress; replacing the first subscriber at 35–66% did not stop the second, which completed. The log contains one 48-segment/98.0s separation for that song and a separate three-segment/10.6s replacement. Playback continued across full 129-row scrolling, descending filtering, metadata edit/undo and 40 exact resting CUE captures after 55ms release waits. These are not physical input-to-photon measurements.

An additional real software-rendered French UI in a 2GiB cgroup showed one completed six-minute output and the other deck waiting; replacing the owning track with a short one let the waiting deck reach 100%. The initial long wait label was ellipsized; the final `ATT. RAM` caption visibly fits without widening the layout. The exact final NVIDIA release also repeated cached playback, 20 exact CUE returns and clean exit. Evidence: `.local/performance-p2/results.json`, native screenshots, core-job logs and workload logs.

Output reservations do not budget decoded inputs, inference workspaces or external UI handles. Native scrolling used 129 tracks; 100k retention coverage is the isolated model benchmark. Physical controllers/audio, arm64/Wayland execution and long-session residency remain unmeasured. F-06 metadata restoration and P2 recommendations were still open at this point; the next record closes the scoped restoration/timer work.

## Implementation record — source restoration and inactive visual timers

F-06 metadata restoration now keeps one lazy snapshot/index per source reached during a reload. It filters source rows by retained
paths before computing identity keys, borrows source tracks and paths, and discards the lookup after restoration. An explicit
Collection ID still wins; otherwise the active source precedes other sources, with first-match duplicate semantics and Collection
fallback preserved. Unneeded sources are not queried. One-shot track-details refresh keeps its existing scan rather than building
a source-sized index. Source opening remains asynchronous and does not acquire new restoration behavior.

Suggest stops its timer when inactive, including Related/preference transitions. Palette follows its enabled/open state. Activation
uses existing immediate refresh paths; analysis workers and completion delivery are unchanged. Mixed-purpose session/preview,
deck, groove, MIDI, audio, lighting and theme-file watcher lifecycles were not changed.

| Matched workload | Before | After |
|---|---:|---:|
| Restore 1,000 retained tracks against 100,000 cached source rows | 1,413.96 ms | 6.54 ms |
| Source reads in that restoration | 1,000 | 1 |
| Suggest callbacks per closed 2-second phase | 6 | 0 |
| Palette callbacks per closed 2-second phase | 6 | 0 |
| Either timer's callbacks per open/reopened 2-second phase | 6 | 6 |

The metadata workload compares production one-shot scans with the production restoration pass, using the same cached Arc provider
and missing file paths; it checks exact Track outputs. This is not whole-window reload latency or a real external-provider timing.
Short matched native idle samples did not demonstrate a process-wide CPU reduction. Callback dispatches are not OS wakeup counts.
Palette kept logically open across a workspace switch still refreshes; workspace visibility was not added as a second lifecycle.

Verification: 1,909 workspace tests passed, 32 ignored, zero failures; mobile host check and 7 vendored CPAL unit tests passed.
Source regressions cover precedence, unnecessary-read avoidance, failed/unopened/replaced sources, fileless collisions and real
overlay edit/undo with empty values. Four paced two/four-deck 60/120Hz waveform cases had zero isolated CPU-budget misses.
Native NVIDIA/EGL at 125% exercised Suggest through Tracks/Related and completed candidate analysis, Palette close/tag/reopen,
Collection metadata edit/undo without resetting playback, 20 exact resting CUE returns and clean File > Exit. The installed SHA
matches this native-tested release and its CLI ran. Native external-source opening, Studio/touch and Wayland were not exercised;
source restoration itself ran through the production-path regressions and benchmark.
Evidence: `.local/performance-p3/results.json`, workload/gate logs and native screenshots.

## Implementation record: non-stretched stem FX

F-10 is completed. `Rest::frame_with_part` returns the rest and selected part from one mixed read and one selected-part read.
The voice uses this pair only without stretching and keeps the subtraction-then-FX-addition order. Key lock and pitch shift
retain independent stretchers. Rest's existing unit-step interpolation is preserved; step-aware anti-aliasing remains a separate
DSP concern. No callback allocation, lock, dependency or setting was added.

The isolated source probe used the real PCM interpolator, Track16 and stem implementations. Its 56,320 comparisons checked
float bits directly across all masks, varied gains, empty/end/negative/fractional positions, signed read steps and partial/complete
publication states. All matched. Full callback output digests also matched before/after across all four cases below.

| Real voice callback fixture | Before, ns/stereo output frame | After, ns/stereo output frame |
|---|---:|---:|
| Rate 1.0, drums FX, no key lock | 46.049 | 42.184 |
| Rate 1.037, drums FX, no key lock | 117.832 | 111.041 |
| Rate 2.25, all-stem FX, no key lock | 107.865 | 97.884 |
| Rate 1.037, drums FX, key-lock control | 331.193 | 329.070 |

These are wall-time medians from five alternating before/after runs, each containing seven 131,072-frame trials, at 44.1kHz
with 512-frame buffers. The synthetic two-tone track, gain ramps and built-in Trans effect are the same in both builds.
The key-lock path is unchanged; its timing difference is not an optimization claim. This measures the voice callback, not
whole-process CPU, RAM, UI responsiveness or physical audio latency.

Verification: 1,910 workspace tests passed, 33 ignored including documentation examples, zero failures; mobile host check
and 7 vendored CPAL tests passed. A new callback regression covers both stereo channels while stems are pending and exact
termination at the track end. Release test executables ran in a disposable sandbox with private HOME and ALSA null output;
all four paced waveform cases had zero isolated CPU-budget misses. The installed binary matches the built release and its
CLI ran. Native GPU, physical audio/controllers, ARM and Wayland were not rechecked.
Evidence: `.local/performance-p4/results.json`, callback comparison data and sandbox gate logs.

## Implementation record: shared XML scanner

F-11 is completed. The two scanner bodies and tests were identical apart from their module comment. One implementation now
lives in `openfader_plugin::markup`; both former copies and the common-package re-exports were removed. Controller maps,
Rekordbox XML, database smart lists, iTunes and sample-pack readers import the SDK directly. The scanner body is unchanged.
The affected Rust files contain 154 fewer source lines. No package, feature, dependency or parser behavior was added.

A standalone SDK build with no optional features matched the original scanner on 379 inputs, including 176 real controller
mappings, malformed attributes, entities, Unicode, text and declarations. Tags, attribute values, edges, following text and
errors matched. This is a source-maintenance improvement; no application CPU, RAM or disk saving is claimed.

Verification: 1,909 core-workspace tests passed, 33 ignored including documentation examples; 124 companion-workspace tests
passed; the SDK with optional features disabled passed 9 tests with 1 ignored. Core without default features, mobile host,
and independent Rekordbox XML/database checks passed. Release test executables ran in a private-HOME sandbox with ALSA null
output and freshly built companion libraries. The installed executable and all six compiled companions match their built
hashes; the installed CLI ran. Installation used each workspace's normal target directory so plugin copies were current.

Rust consumers need the new SDK import path; native entry points and ABI versions are unchanged. Publish the core SDK before
a companion release using it. Traktor/VirtualDJ parsers, protocol colors, DSP and native GPU/physical-device checks were unchanged.
The separately reported coding-agent memory belonged to its owned rust-analyzer, not an OpenFadeR process; that indexer was stopped.
Evidence: `.local/performance-p5/results.json`, `agent-memory.json` and sandbox verification logs.

## Implementation record: Linux bundle footprint

There was no existing dist executable to compare. The local release ELF retains a symbol table used by the crash-report
backtrace path. Its bytes and existing dist/Pi profiles were preserved; no fresh LTO build or dist performance result is claimed.
Inspection instead found six stale companion libraries at the installed root, in addition to the current plugin-folder copies.
The executable's runtime dependency is WebGPU Dawn; plugin loading resolves each library inside its own plugin directory.

`install.sh` and the Linux release step now copy only the ONNX Runtime/WebGPU shared-library families beside the executable.
Versioned ONNX files and provider libraries remain eligible; CPU-only builds need no optional shared runtime. Copy errors for
existing files are no longer hidden. Native companions remain bundled by the existing manifest-folder bundler.

| Installed logical file bytes | Before | After |
|---|---:|---:|
| Whole installed tree | 235,380,773 | 209,056,965 |
| Executable | 165,800,440 | 165,800,440 |

The reduction is 26,323,808 bytes (25.10MiB), solely the six stale root libraries. The installed executable SHA and all six
current companion SHAs are unchanged. This saves disk space; it is not a resident-RAM or UI-performance measurement.

Two installer regressions failed before the fix and pass after it: native fixture executables load required runtime/provider
and plugin symbols, a versioned runtime alias loads, stale/unrelated root artifacts are absent, and existing settings survive.
The tests also cover CPU-only installation and run in Linux CI. The actual Linux release Package run block was exercised with
the current release ELF staged as its input, then the archive was unpacked and its CLI ran with six bundled companions present.
The real local installation was updated and its CLI ran. Shell syntax and both workflow YAML files parsed successfully.

Validation used one CPU and a 512MiB aggregate sandbox memory cap, with zero OOM events; the peak reached that cap including
file cache. No Rust indexer or subagents were started. Automatically started shell/YAML tooling was stopped after editing.
Windows/macOS packaging, native UI, physical audio/controllers, ARM and fresh dist/LTO execution were not rechecked.
Evidence: `.local/performance-p6/results.json`, before/after regression logs and archive/installed CLI logs.

## Experiment record: EXPORT panel timer and build memory

The 100ms timer in `src/app/exportmode/mod.rs` refreshes the visual player panels. The separate 250ms timer in
`src/app/device/mod.rs` also delivers background export-job completion and must keep running when EXPORT is hidden.
An instrumented baseline dispatched 20 panel callbacks in each two-second sample, including hidden samples.
Its panel rows remained stale on entry until the next poll. These are callback/state observations; no process CPU saving
or kernel wakeup reduction was measured.

A visibility-bound Slint timer with immediate entry refresh was tried, then reverted before installation. Fresh UI
compilation hit the hard 6GiB/no-swap cap with one Cargo job: workspace build, UI-only 64-codegen-unit build, and
metadata-only UI check all ended in cgroup OOM kills. The generated UI source was 61,098,771 bytes. Full Slint
inlining was already disabled by default. More LLVM codegen units did not solve this limit.

The baseline harness reused compiled UI artifacts; its successful bounded build does not establish fresh UI compiler
memory requirements. Four headless waveform frame cases passed without budget misses before the experiment.
Build profiles, accessibility and the installed executable were preserved. No native candidate UI or runtime-RAM
improvement was verified. Reduce generated UI/compiler footprint before revisiting this low-priority timer change.
Evidence: `.local/performance-p7/results.json`, build logs and `sandbox-evidence/`.

## Implementation record: visibility-bound EXPORT panels

After approval to raise the maximum to 12GiB, the panel timer change was restored and verified. The window owns one
declarative timer bound to EXPORT visibility and requests a refresh on mode entry. The Rust callback keeps its visibility
guard and existing row-change suppression. The obsolete Rust timer guard and its caller binding were removed.
The separate device/job timer, audio, MIDI, lighting and video lifetimes are unchanged.

| Panel callbacks per two-second sample | Before | After |
|---|---:|---:|
| EXPORT hidden | 20 | 0 |
| EXPORT visible | 20 | 20 |

The hidden/visible sequence was repeated after closing and reopening the mode. Temporary counters were removed;
the permanent regression checks actual panel state on entry, during visible changes, while hidden and after re-entry.
It failed before the change and passes in the final uninstrumented build.

All 17 root-package test executables passed: 1700 tests, 27 ignored. Physical-device tests retain their no-device skips.
Four headless waveform frame controls had zero budget misses. Private X11/NVIDIA verification at 125% scale exercised
Rek, Pocket, Studio and Neon Circuit mode selectors and re-entry. Replacing a six-minute track while hidden showed the
new 16-second track on return; its grid panel reset from FROM HERE to WHOLE. Native windows exited normally.

Fresh UI compilation completed under 12GiB/no swap with one Cargo job; systemd reported an 11.9G peak.
The cached final regression/release build peaked at 2,313,945,088 bytes with zero OOM events. Release optimization,
accessibility and diagnostic symbols were preserved. These are build/callback observations; no application RAM,
process CPU or input-to-photon improvement is claimed.
Evidence: `.local/performance-p7-resume/results.json`, build logs, isolated consumer logs and native captures.





The local installation matches the native-tested release ELF (`52112e475052…`). Its `verify --help` command ran from an
isolated copy. Installation stayed under the same 12GiB cap, with one Cargo job and zero OOM events.

## Implementation record: unchanged playlist palette reads

Open Playlist Palette previously fetched all three assigned playlists from SQLite every 300ms before comparing its snapshot.
The existing snapshot now includes assignments and read success. A matching store generation, BPM precision, assignment,
current tree names and Tag List returns before those queries. Failed reads remain retryable; row-change suppression remains.
No second playlist-key cache, dependency or new timer was added.

| Synthetic steady-state workload: 300 polls, three 10,000-track playlists | Elapsed |
|---|---:|
| Before | 714 ms |
| After | <1 ms |

This is one warm matched microbenchmark, not a process CPU or application RAM comparison. Temporary benchmark code was removed.
The final application suite passed 777 tests with 14 ignored. A consumer-state regression covers same-name assignment changes,
membership replacement, delayed tree rename publication, same-length Tag List replacement and playlist deletion.
Private native X11/NVIDIA at 125% verified populated palettes, live rename, removing a track and closing/reopening the panel.
The original snapshot's failed-read retry behaviour is preserved. Compilation stayed under the 12GiB/no-swap cap with one job.
Evidence: `.local/performance-p8/results.json`, matched logs and native captures.

The local installation matches the native-tested executable (`b652da14632e…`); its CLI help smoke passed in isolation.

## Implementation record: indexed playlist menu paths

`playlist_labels` (Palette chooser and ADD TO) and `more_rows` repeatedly scanned the entire node slice while constructing
paths and testing ancestry. Both now build one temporary index of borrowed nodes per operation and share indexed helpers.
Outputs still follow the original slice order; duplicate IDs keep their first entry, missing parents stop traversal, and
the existing 64-hop limits remain. Empty output cases avoid constructing an unused index. No retained hierarchy cache was added.

| Synthetic 10,000-node hierarchy, ten constructions | Before | After |
|---|---:|---:|
| Playlist labels | 124.14 ms | 4.39 ms |
| Move menu | 124.69 ms | 4.41 ms |

These are matched menu-construction samples, including index creation, not application CPU or resident-RAM measurements.
The temporary benchmark was removed. The final application suite passed 778 tests with 14 ignored, retaining existing
move restrictions and adding malformed hierarchy/path boundary coverage. Private X11/NVIDIA at 125% showed nested labels
and verified that selecting "My sets › Saturday" assigned the correct playlist and Metadata B. Native shutdown was normal.
Builds retain release settings and the 12GiB/no-swap cap with one Cargo job. Evidence: `.local/performance-p9/results.json`.

The local installation matches the native-tested release ELF (`90d99f98cb9b…`); its CLI smoke passed in isolation.

## Implementation record: indexed sidebar icon publication

Sidebar labels already used a linear depth pass, but publishing each visible My Playlists row called `View::node`,
which linearly searched the complete node list for its icon. Rebuilds now reuse the operation-local borrowed-node
index helper. The index is built only when visible user rows remain after folding, and is released after the rebuild.
Other icon branches, row ordering, indentation, selected-row publication and automatic section opening are unchanged.

| Synthetic expanded sidebar: 10,000 nodes, ten full rebuilds | Elapsed |
|---|---:|
| Before | 157.49 ms |
| After | 37.74 ms |

The matched workload includes store reads, labels and model publication. It is not an application CPU, input-latency
or resident-RAM measurement. Temporary benchmark code was removed. The final application suite passed 779 tests with
14 ignored, including actual sidebar icon/depth/selection/folding transitions. Private X11/NVIDIA at 125% verified folder,
playlist and smart icons, selecting Friday's Metadata A, keyboard parent navigation and Left/Right folding/reopening.
Native shutdown was normal; compilation retained the 12GiB/no-swap cap and one Cargo job.
Evidence: `.local/performance-p10/results.json`, matched rebuild logs and native captures.

The local installation matches the native-tested release ELF (`4a3acd0a7ccc…`); its isolated CLI smoke passed.

## Implementation record: functional UI interaction audit

This pass followed press, move, release, cancellation and delayed-result ownership through the three track tables,
deck controls, keyboard/MIDI dispatch, sampler, path dialogs, theme deletion and device jobs. It preserves the skins
and their optional-column breakpoints; it is not a claim that every possible interaction has been exercised.

| Path | Confirmed defect | Repair |
|---|---|---|
| Desktop column dividers | Flexible TITLE absorbed other columns' width changes, moving the wrong edge | Independent saved widths; bounded left-button gestures; horizontal overflow |
| Studio/Stage tables | Minimum-width columns could extend beyond the visible table | Scrollable content with clipped, synchronized headings |
| Browser clicks and loaded badges | Row indices and editable titles were mistaken for media identity | Stable local/remote identity; reject stale second clicks and release-time selection collapse |
| Held controls | Release used the current deck, pad mode, mapping or sampler bank | Snapshot press recipients and action context; consume each owned release once, including Learn/disconnect |
| Performance pads | A stale release could end a newer pad; mode exit left momentary state behind | Pad/mode ownership, full mode-exit cleanup and immediate pending-jump cancellation on pause |
| Pad/hot-cue feedback | Replacing a data-driven repeater destroyed held controls; in-place nested updates left native labels stale | Keep the eight slot components stable, replace changed feedback data and skip unchanged snapshots |
| Sampler CLEAR | A pending decode could repopulate a cleared slot | Cancel only that slot's pending load before clearing |
| Path dialogs | Hidden persistent fields did not refocus; empty acceptance hid the dialog; late chooser results could overwrite a later request | Create the field on opening, retain empty requests and assign process-wide request identities |
| Theme DELETE | Immediate file removal, with no confirmation | Confirm the captured theme; retain unrelated current themes/drafts |
| Device progress | X was offered for irreversible work; cancellation implied rollback and an I/O failure | Worker-owned capability, persistent CANCELLING status and neutral partial-change warning |

These changes do not establish a measured process-RAM, CPU or input-to-photon improvement. Native checks rejected
in-place nested-model updates: Rust and published values changed, but rendered pad labels did not. Forcing the parent
row to publish again did not repair that mismatch. Fixed slot identity separates the control lifecycle from its data.

The final application suite passed 804 tests with 14 ignored. Its pad regression paints between pointer down and up,
checks the visible feedback colour, and verifies that rendering does not release the hold. Translation completeness
and placeholder checks pass for all six translated languages.

Private X11/NVIDIA at 125% exercised Rek, Studio in French, Neon Circuit/Stage, Pocket and single-player EXPORT:
independent saved widths, overflow/header sorting, duplicate-title media, changing pad labels/colours, FX and hot-cue
release, pointer cancellation and path-dialog focus. Theme duplicate/cancel/confirmed deletion was also exercised;
the final pad-only correction did not change that workflow. The installed ELF matches the final native-tested
executable (`d0a5c579091b…`), and its installed CLI smoke passed in the isolated runtime.

Each build used one Cargo job in a 12GiB/no-swap process group; the highest observed group peak was 11.65GiB with
zero OOM events. This is not an aggregate agent-memory cap: final inspection found a restarted 7.37GiB Rust indexer
outside those groups. It was stopped again and the tool lifecycle issue reported. No physical controller/audio-device,
destructive disk operation, native Wayland or non-Linux desktop verification is claimed.
Evidence: `.local/ux-audit/results.json`, build/suite logs, diagnostics and native captures.

## Scorecard

No overall numeric health grade is assigned. This is a performance/footprint audit, not a complete security, dependency-cycle, or architecture certification; a weighted total would conceal those unmeasured dimensions.

| Dimension | Assessment | Evidence available |
|---|---|---|
| Coupling & cycles | Partially reviewed; no exhaustive module-cycle result | Source call chains; project graph unavailable |
| Hotspot concentration | 16 files in both top deciles | 676 files ranked; inline tests affect scores |
| Change locality & cohesion | Shared UI callbacks are useful; device discovery has duplicated ownership | Browser/device call chains and co-change pairs |
| Cognitive load | Ownership and asynchronous publication matter more than file length | Audio, browser, analysis and rendering paths sampled |
| Test design | Relevant behavior and workload checks exist | Three existing benchmarks exercised; cold probe added temporarily |
| Boundary correctness | Preserve cancellation, generation and resource-lifetime guards | Source review; earlier ALSA/CUE regressions remain relevant |
| Duplication class | 95 exact and 22 structural-clone candidate groups | Candidates, not 22 confirmed drift bugs; Slint excluded by scanner |
| AI-risk trend | Not graded | No reliable longitudinal authorship/duplication trend measured |
| Supply chain | Not graded | Dependency duplicates inspected; no advisory database scan |
| Docs & decisions | Useful local lessons and explicit performance shortcuts | MAP, README and implementation comments reviewed |

## What was measured

### Actual native application

Installed release binary SHA-256: `462e82ee9cb264ea5b628ce2b9a01890f9e99980afe8a9ece6415075a4edc837`.

Private Xvfb display, 1601×1000 physical window, 125% scale, Rek skin, private PipeWire null output, hardware monitors disabled, no MIDI controller. Two synthetic six-minute, 44.1kHz stereo WAVs; stems disabled. GPU mode was confirmed by the application's NVIDIA RTX 3090 renderer log. These profiles used the isolated default plugin set, not the owner's full plugin/library workload.

CPU percentages below are **of one CPU core**, not of the whole 16-thread machine. Each phase is one approximately five-second sample, not a distribution or confidence interval. RSS is resident mapped memory; PSS apportions shared mappings and is more useful when comparing process ownership.

| Native phase | CPU, one core | RSS MiB | PSS MiB | Anonymous MiB |
|---|---:|---:|---:|---:|
| GPU idle, two library entries, no deck loaded | 1.40% | 421.16 | 242.61 | 113.78 |
| GPU, one six-minute deck paused | 1.60% | 487.96 | 309.59 | 178.95 |
| GPU, one six-minute deck playing | 30.12% | 487.92 | 309.60 | 178.95 |
| GPU, two six-minute decks paused | 1.20% | 557.00 | 378.68 | 244.02 |
| GPU, two six-minute decks playing, 60Hz setting | 34.71% | 557.01 | 378.69 | 244.02 |
| GPU, two six-minute decks playing, fresh 30Hz profile | 38.51% | 500.77 | 331.05 | 193.48 |
| Software idle | 1.60% | 128.65 | 107.63 | 44.57 |
| Software, two six-minute decks paused | 2.00% | 258.72 | 237.66 | 174.06 |
| Software, two six-minute decks playing, 60Hz setting | 33.94% | 258.78 | 237.72 | 174.06 |

Exact-PID NVML graphics memory was 50.88 MiB in the two-deck GPU/60Hz sample and 50.25 MiB in GPU/30Hz. Software rendering had no graphics-process entry in that query; absence is not a claim about every system allocation.

**Interpretation:** compact PCM scales reasonably with loaded duration. The lower software-renderer memory in this fixture is real, but not a recommendation to change the default: the GPU path includes NVIDIA mappings and VirtualGL transport, and software rendering moves raster work to the CPU. The 30Hz run did **not** demonstrate a CPU improvement. Fresh-process allocation history and background work differ; do not claim a saving from that comparison. None of these measurements is physical scanout latency or a Wayland responsiveness measurement.

### Existing workload benchmarks

Commands used `--bin openfader`: the browser code is part of `src/main.rs`, not the library test target. Existing benchmark comments suggesting `--lib` would select the wrong target.

| Operation | Measured result |
|---|---:|
| Build facts for 100k tracks | 3.36 ms |
| Search “techno” over 100k tracks | 6.06 ms |
| No-hit search over 100k tracks | 6.63 ms |
| Filter BPM/rating over 100k tracks | 3.99 ms |
| Sort 100k tracks by title | 14.46 ms |
| Sort 100k tracks by BPM | 1.56 ms |
| Eagerly format 100k rows, diagnostic comparison | 50.54 ms |
| Five search/sort refreshes, 20k tracks, existing lazy model | 7.14 ms total |
| Five search/sort refreshes, 50k tracks, existing lazy model | 16.26 ms total |

Do not sum these into a measured keystroke latency: inputs/result sizes differ, facts are cached, and production already uses lazy rows. The title sort alone uses most of a 60Hz frame interval, however, so avoid repeating it when ordering inputs have not changed.

Paced software-frame benchmark, 1920×1080, reused framebuffer:

| Decks | Target | Thread-CPU p50 | Thread-CPU p99 | Maximum | Over-budget frames |
|---:|---:|---:|---:|---:|---:|
| 2 | 60Hz | 1.797 ms | 3.638 ms | 4.783 ms | 0/600 |
| 2 | 120Hz | 1.476 ms | 2.961 ms | 4.458 ms | 0/1200 |
| 4 | 60Hz | 3.221 ms | 4.460 ms | 6.380 ms | 0/600 |
| 4 | 120Hz | 2.845 ms | 3.749 ms | 6.906 ms | 0/1200 |

This benchmark includes the prepared UI workload, not native GPU completion or the entire application under analysis. Strip redraws were two/four per case and overview redraws zero: caching is working in this workload. Frame-update p99 stayed below 0.1 ms.

### Cold resampler and disk footprint

Six fresh processes called the actual public `Track16::frame_at_step`, with an allocation-counting global allocator enabled only around each read:

| Read | Allocations | Allocated bytes | Observed time range |
|---|---:|---:|---:|
| First integer-position read | 0 | 0 | 0.08–0.30 µs |
| First fractional-position read | 1 | 16,392 | 153.97–227.03 µs |
| Subsequent fractional read | 0 | 0 | 0.11–0.13 µs |

Single-call warm timings include timer/instrumentation overhead; these are not throughput numbers. This proves lazy initialization in the source reader, **not** that every application session performs it on the callback: another analysis path can initialize it first.

Read-only disk measurement of the current installation/cache:

| Area | Bytes |
|---|---:|
| Installed bundle | 208,919,261 |
| Total application cache | 4,851,236,959 |
| Stem cache | 4,158,542,224 |
| Model files | 661,586,258 |
| Analysis cache | 20,553,439 |
| Repository archive cache | 10,405,199 |

The stem cache is below its existing 4GiB cap. Models are outside that cap. This is primarily retained useful data, not proof of a disk leak. No user cache was deleted.

### Structural measurements

- `scc`: 652 Rust and 108 Slint files in the sampled directories; counts include tests. Auxiliary formats are retained in the raw output but excluded from the headline code total.
- Hotspot scanner: 676 files ranked, 16 top-decile intersections. Its indentation score is a screening proxy, not cyclomatic complexity or runtime cost. It does not support `.slint`.
- Clone scanner: 744 files, 95 exact and 22 structural candidate groups, including companion plugin code. These are not an exhaustive clone census; Slint is not scanned and structural resemblance is not proof of harmful drift.
- `cargo tree --offline --duplicates`: duplicate dependency versions exist; for example ALSA 0.9 through CPAL and ALSA 0.10 through MIDI. The raw output also repeats host/build contexts: its entry count is not a count of independently removable runtime dependencies.
- Git author dates span October 15–26, 2026. The toolcheck helper reported an incorrect first-date range; corrected from raw `git log` and reported to tooling QA.

Raw evidence: `.local/performance-audit/` contains `results.json`, `native-metrics.json`, screenshots, benchmark logs, `cold-resampler.json`, `hotspots.json`, `duplication.json`, dependency output and source-size output.

## Hotspots

| Rank | File | Commits | Significant LOC | Indentation proxy | Authors | Score |
|---:|---|---:|---:|---:|---:|---:|
| 1 | `src/app/deck/load.rs` | 15 | 1159 | 186.9 | 1 | 2803.5 |
| 2 | `src/app/deck/frame.rs` | 8 | 815 | 257.2 | 1 | 2057.6 |
| 3 | `src/app/analyze/job.rs` | 10 | 1692 | 189.8 | 1 | 1898.0 |
| 4 | `src/app/analyze/mod.rs` | 13 | 661 | 139.1 | 1 | 1808.3 |
| 5 | `src/app/device/mod.rs` | 16 | 439 | 98.4 | 1 | 1574.4 |
| 6 | `src/app/device/scan.rs` | 8 | 311 | 185.6 | 1 | 1484.8 |
| 7 | `src/app/deck/mod.rs` | 16 | 367 | 89.7 | 1 | 1435.2 |
| 8 | `src/app/browser/wire.rs` | 11 | 551 | 130.4 | 1 | 1434.4 |
| 9 | `src/app/browser/sources.rs` | 9 | 465 | 156.4 | 1 | 1407.6 |
| 10 | `src/app/midi/mod.rs` | 13 | 447 | 105.6 | 2 | 1372.8 |

Deck loading, analysis, devices and browser wiring are sensible priorities. The large inline regression suites contribute to these scores; file length alone does not justify splitting them. History does not include all current uncommitted fixes.

## Findings

### F-01 — Share the worker-owned device snapshot instead of rescanning on the UI thread

**Severity:** high · **Evidence:** [strong] for shared ownership/locality; latency impact is source-derived, not timed with a slow device.

**Where:** `src/app/browser/session.rs:123-145`, `src/app/browser/sources.rs:120-145,414-431`, `src/app/device/scan.rs:22-28,82-133,142-233`.

**Observed:** every fifth 200ms browser tick calls source collection, which directly invokes `device::scan()`. The device subsystem already runs discovery on a worker and publishes `MOUNTED`. Cached probes reduce frequency but do not bound a cache-miss filesystem/plugin call on the event loop.

**Problem:** slow mounts or plugin probes can delay all input and drawing. Discovery also has two owners that must agree about current devices.

**Smallest fix:** have the browser consume the existing mounted snapshot. Preserve initial scan delivery, hotplug notifications, plugin enable/disable and Explorer-source changes; do not merely read an initially empty snapshot forever. Rebuild sidebar sources when their input generation changes, not by repeating physical discovery.

**If skipped:** occasional device-dependent UI stalls remain even when waveform drawing is fast. This is the strongest DRY opportunity because it removes duplicated work, not just text.

### F-02 — Track selection reads more analysis than the panel needs

**Severity:** high · **Evidence:** [moderate]; synchronous I/O observed, storage-dependent latency unmeasured.

**Where:** `src/app/trackinfo.rs:132-135,265-288,313-329`, `src/app/analyze/detected.rs:54-61`, `src/analysis/cache.rs:234-300,375-392`.

**Observed:** selection calls `Detected::lookup`, which loads full analysis and retains only BPM/key. The existing `cache::glance` prefix reader is suitable for that summary after validation. A separate first-time phrase lookup still requests whole library analysis. Successful summaries are memoized; misses are not.

**Smallest fix:** reuse `glance` for summary data. Read first-time phrases off the UI thread, keyed by selection identity/generation, so an old response cannot overwrite a newer selection. Avoid endless repeated missing-cache reads while retaining invalidation when analysis appears.

**If skipped:** arrow-key browsing pays file-size-dependent allocation and I/O on the UI thread. Fixing the summary alone does not remove phrase-read blocking.

### F-03 — Analysis result delivery needs a UI work budget

**Severity:** high · **Evidence:** [moderate]; unbounded publication observed, worst-case latency not measured.

**Where:** `src/app/analyze/mod.rs:517-599`, especially `rx.try_iter()` at `:550`; `src/app/analyze/job.rs` submission/pending pool; `src/app/browser/table.rs:265-279`.

**Observed:** analysis workers are bounded, but runs use completion channels drained without a count/time budget. Whole-library submission also eagerly creates jobs. Warm-cache batches can finish much faster than full audio analysis, making publication bursty. A detected-version change can then trigger another library-wide facts pass.

**Smallest fix:** cap result publication per event-loop turn and coalesce progress/selected-panel updates per batch. Stage very large submissions instead of holding shared admission locks while enqueueing every track. Retain ticket/generation checks, request-specific settings, and cancellation identity.

**If skipped:** fast worker throughput can still produce UI freezes. Do not fix this with a blocking bounded-channel send under the pool lock, or by dropping valid results.

### F-04 — Cache ordering; do not sort the library again for every query

**Severity:** medium · **Evidence:** [moderate]; sorting cost measured, invalidation policy observed.

**Where:** `src/app/browser/filter.rs:58-87,92-119`, `src/app/browser/list.rs:81-107`, shared search callbacks in `src/app/browser/wire.rs`.

**Measured:** title sorting 100k tracks took 14.46ms. Search itself took approximately 6ms in the tested ASCII cases. Those timings are separate workloads, not a measured combined keystroke time.

**Observed:** every query rebuilds visible indices and can sort again. `matches` also formats the displayed key before checking whether there are any query words. Text sorting creates folded-string keys again.

**Smallest fix:** skip key formatting for an empty query; defer it until a word needs that field. Reuse a sorted index permutation while sort column/direction, metadata and display-dependent keys are unchanged, then filter that order. Coalesce superseded text edits without adding arbitrary delay to header clicks or transport input.

**If skipped:** title-sort browsing consumes a large share of the UI frame interval. Preserve stable tie ordering, accent folding, key notation, and master-relative filtering when changing the order of operations.

### F-05 — Lazy row construction still retains every visited row

**Severity:** medium · **Evidence:** [moderate]; retention policy observed, whole-library scroll RSS not measured.

**Where:** `src/app/browser/table.rs:290-351,355-376`.

**Observed:** `row_data` inserts formatted row models into `cells`, with no capacity bound. Changing the visible index list does not evict them. Reference/played updates walk cached rows. Existing lazy benchmarks visit only the first 32 rows per query.

**Smallest fix:** bound retained formatted rows using a viewport/handle-aware policy. Do not blindly evict a model still held by the visible Slint table: existing handles must continue receiving updates. Prototype this before choosing a cache size.

**If skipped:** memory and reference-update work grow with tracks visited, eventually the whole library. This is library-bounded retention, not an infinite leak. Keep the existing lazy construction; it is already much cheaper than eager formatting.

### F-06 — Store invalidation rebuilds keys even when the track list is unchanged

**Severity:** medium · **Evidence:** [moderate]; O(N) rebuild observed, savings unmeasured.

**Where:** `src/app/browser/mod.rs:326-347`, `src/app/browser/health.rs`, `src/store/overlay.rs:104-127`.

**Observed:** any store-generation change can rebuild the browser's key vector and key-to-index map. The lower Collection cache can preserve the same track `Arc` for unrelated writes, but the browser does not use that identity to retain its index.

**Smallest fix:** still ask for the current tracks after invalidation, but reuse keys/index when the list `Arc` is unchanged. Keep metadata and external-source invalidation correct. During metadata restoration, build a source identity lookup once per reload rather than scanning source lists for each retained track.

**Status:** completed in the source-restoration implementation record above, including the separate per-reload source lookup.

**If skipped:** cue/history/other updates can allocate library-sized indexes unnecessarily. Do not replace all generation checking with a single Collection counter; external sources and overrides also matter.

### F-07 — Stem output buffers sit outside memory admission

**Severity:** high for constrained devices · **Evidence:** [moderate]; allocation order and payload formula observed; no fresh OOM/residency trial.

**Where:** `src/stems/mod.rs:139-174,242-266`, `src/audio/stems.rs:89-115`, `src/stems/model.rs:407-411`.

**Observed:** each job allocates four full-length stereo i16 stem buffers before cache lookup and before `Batch::start` admission. Serializing model execution therefore does not serialize or budget result-buffer ownership.

For six minutes at 44.1kHz, source-derived payload sizes are **60.56MiB PCM + 242.25MiB stems per track**. Four fully resident track/stem pairs require about **1,211MiB**, before the application, models, inference workspace and waveforms. This is a payload calculation, not measured RSS: zero-filled pages can remain uncommitted until touched.

**Smallest fix:** admission must account for output buffers before allocating/filling them, including cache hits. Account for completed attached stems until their last consumer releases them. Do not acquire/open model sessions for cache hits. Waiting or rejected work needs an explicit, cancellable state rather than silently losing requested stems.

**If skipped:** low-memory model policy can still be defeated by long-track output/cache buffers. Add cancellation checks between cache I/O chunks as part of this ownership boundary, preserving locking and atomic rename.

### F-08 — Share in-flight same-song separation, not only completed doubles

**Severity:** medium · **Evidence:** [moderate]; independent jobs observed, duplicate segment count not measured.

**Where:** `src/app/deck/stems/job.rs:202-255` and completed-result sharing nearby; `src/stems/mod.rs:139-174,254-266`.

**Observed:** two ordinary deck loads of the same uncached song can each create their own stem job, miss the disk cache, allocate outputs and process segments. Session mutexes serialize access but do not deduplicate computation. Instant doubles already have a narrower completed-result sharing path.

**Smallest fix:** extend sharing to compatible in-flight requests, keyed by identity/rate/length/quality. Each deck is a subscriber: unloading one must not cancel work still needed by the other; SPEED-mode focus must represent all consumers. Reuse existing ownership patterns rather than introducing a generic task framework.

**If skipped:** overlapping duplicate loads waste separation work and output storage. This is higher-risk than the UI quick wins because cancellation and focus semantics must remain exact.

### F-09 — Initialize the existing sinc table before the callback can reach it

**Severity:** medium, real-time hygiene · **Evidence:** [moderate]; directly measured in six fresh processes.

**Where:** `src/audio/mod.rs:303-325,335-370`, `src/audio/track16.rs:63-72`, callback consumer in `src/audio/player/voice.rs`.

**Measured:** the first fractional read allocated 16,392 bytes and took 0.154–0.227ms; integer reads did not initialize the table. Warming one integer-speed playback path is not sufficient proof that the table is ready.

**Smallest fix:** explicitly initialize the existing kernel during stream preparation before publishing the callback. Keep one interpolation implementation and the exact downsampling behavior. A worker may already warm it in some sessions; do not claim a dropout was reproduced.

**If skipped:** the first fractional callback read can perform allocation, sinc/Bessel calculation, or wait for another initializer. This is a small, low-risk tail-latency fix, not an explanation for sustained 30% CPU.

### F-10 — Non-stretched stem FX evaluates the selected part twice

**Severity:** low · **Evidence:** [moderate]; duplicate execution observed, CPU gain unmeasured.

**Where:** `src/audio/player/voice.rs:374-400`, `src/audio/stems.rs:340-367`.

**Observed:** the split branch reads Rest and Part; Rest itself evaluates Mixed and Part. The non-stretched selected part is therefore read twice at the same source position.

**Smallest fix:** compute mixed/selected once and form the same rest-plus-effect result, preserving floating-point operation order where practical. Restrict the optimization to the non-stretched branch: independent Part/Rest stretchers may choose different positions.

**Status:** completed in the non-stretched stem-FX implementation record above. The separate step-aware DSP concern is unchanged.

**If skipped:** bounded repeated atomic/interpolation work remains. Before touching this, separately check step-aware anti-aliasing: Rest does not override `frame_at_step` and its Mixed read goes through the unit-step path. Treat that source-contract concern as a DSP correctness investigation, not a hidden behavior change in a DRY patch.

### F-11 — Consolidate XML parsing knowledge only after its contract is confirmed

**Severity:** low, maintainability · **Evidence:** [contested] for clone removal as a general rule; exact clone observed.

**Where:** current owner `sdk/src/markup.rs`; former copies were `src/library/markup.rs` and `compatibility/rekordbox/common/src/markup.rs`.

**Measured:** 143 matching lines detected across the parser/test blocks. This duplicates XML escaping/tag rules, not a proven runtime bottleneck.

**Smallest fix:** if both consumers are intended to keep the same XML subset, move one implementation to an existing suitable shared package and migrate all callers. Preserve compatibility-disabled/mobile builds and independently built adapters. Otherwise document independent contracts and leave stable copies alone.

**Status:** completed in the shared-scanner implementation record above, including all five consumers and feature-disabled/independent builds.

**If skipped:** parser fixes must be applied twice. No measurable CPU/RAM saving is promised, and the adapter's protocol cue colors should not be merged with UI colors merely because their current tables look alike.

## Conflicts and judgment calls

- **DRY versus independent adapters:** share device discovery now because it repeats expensive work and ownership. XML consolidation is lower priority; protocol/color tables and independently evolving adapters are not generic utility cleanup targets.
- **Lower RAM versus rendering CPU:** software rendering used less process memory in this fixture, but changes where raster work runs. Keep both backends and measure the actual target hardware rather than forcing software globally.
- **Lower disk use versus repeated inference:** the stem cache is already capped and useful. A smaller budget is a deliberate cache-hit/compute tradeoff, not a leak repair. Do not delete either model file until CPU/GPU fallback requirements are verified.
- **Small code versus correct ownership:** in-flight stem sharing and row eviction must preserve cancellation/subscriber/model-handle lifetimes. A tiny implementation that cancels another deck or leaves visible rows stale is not an optimization.
- **Framerate versus reactivity:** 30Hz did not produce a measured native CPU win here. Do not reduce input responsiveness or delay CUE to make rendering statistics look better.

## What not to change

| Area | Why to preserve it |
|---|---|
| `src/app/deck/frame.rs` and shared waveform crop | Cached strips perform well in the measured workload. Keep identical CPU/GPU physical-column mapping and pre-scene frame preparation. |
| Seek generations, audible clock and deferred anchors | Protect rapid CUE against old callback publication. Do not simplify them to “latest position” atomics. |
| `Track16` and existing shared Arcs | Normal playback already avoids whole-track f32 duplication. Do not claim all deck loads clone PCM. |
| WSOLA correlation/search and independent stretchers | Existing stereo, pitch and phase behavior has regression history; smaller searches are not free wins. |
| Worker-prepared effects and bounded audio mailboxes | Keep construction, destruction and blocking coordination off the callback. |
| Analysis tickets, cue revisions and cancellation subscriptions | Required to prevent stale results/data loss when optimizing batching or sharing. |
| Lazy rows and Collection Arc caching | Already save real work; bound/invalidate them carefully instead of replacing them with eager rebuilding. |
| Video/lighting visibility guards | Hidden video releases decode streams; lighting may legitimately continue driving hardware while its pane is hidden. Never stop it solely because the pane is closed. |
| Renderer fallback and vendored ALSA shutdown/recovery | Preserve recent crash fixes. Dependency-version unification is not justification to discard the CPAL patch. |
| Existing dist/Pi build profiles | LTO and stripping already exist for shipping profiles. Do not present enabling them as a new optimization. |

Idle timers still wake for unused modes and all deck slots (`src/app/deck/mod.rs:365-371`, video 33ms, lighting 25ms), but heavy hidden work is substantially gated. The measured idle CPU is low; further on-demand timer lifetimes are a P2 energy experiment, not a claimed major CPU saving. EXPORT's visual timer now stops while hidden.

## Change plan

Original prioritization follows. The implementation record above identifies completed P0 items; the remaining items are not yet implemented. Order balances evidence, impact and correctness risk.

### P0 — do first

#### P0-1 Remove duplicate UI-thread device discovery — F-01
- **Why:** delete expensive repeated work using an existing owner [strong].
- **Where:** browser source collection and device snapshot publication.
- **How:** consume the worker snapshot and configuration generations; retain startup/hotplug behavior.
- **Effort:** S. **Risk:** low–medium: first snapshot and external-source changes must still reach the browser.
- **Verify:** controlled slow probe with an event-loop heartbeat; connect/disconnect/configuration tests; native browsing during playback.
- **Rollback:** one focused change; leave the worker and API intact.

#### P0-2 Use prefix summaries and asynchronous selection reads — F-02
- **Why:** no full-cache read is needed for two displayed values [moderate].
- **Where:** Detected lookup and first-time phrase lookup.
- **How:** reuse `glance`; selection-keyed asynchronous phrase publication; invalidate negative results correctly.
- **Effort:** S/M. **Risk:** medium: stale selection responses and edits must not overwrite current data.
- **Verify:** rapid selection over cold/missing caches and slow source reads; late-response ordering test; native playback while browsing.
- **Rollback:** separate summary and phrase changes so they can be reverted independently.

#### P0-3 Warm the resampler off the callback — F-09
- **Why:** removes the measured first-read allocation without replacing DSP [moderate].
- **Where:** existing kernel and stream preparation.
- **How:** one explicit internal initialization before callback publication.
- **Effort:** S. **Risk:** low: initialization moves, interpolation does not change.
- **Verify:** fresh-process fractional-first callback allocation check; concurrent initialization case; existing resampling/cue tests.
- **Rollback:** remove the preparation call; retain the lazy table for safety.

### P1 — do next

| Order | Finding | Concrete change | Risk / acceptance |
|---:|---|---|---|
| 1 | F-03 | **Completed:** budget UI result delivery; stage large submissions | Verified ordering, cancellation, no-loss delivery and native bulk analysis |
| 2 | F-04 | **Completed:** empty-query fast path, cached sort permutation | Verified stable ties, Unicode/key notation, metadata and reference transitions |
| 3 | F-06 | **Completed:** reuse unchanged-Arc indexes; lazily index metadata-restoration sources once per reload | Verified store writes, source precedence/replacement/failures, inherited metadata undo and native Collection undo |
| 4 | F-07 | **Completed:** reserve pending/retained outputs; cancellable cache chunks | Verified lease lifetime, real cache hit/miss under 2GiB and on-screen waiting/resume |
| 5 | F-05 | **Completed:** bounded hot rows with live-handle tracking | Verified 100k traversal, bounded dead bookkeeping, held-handle updates and native scrolling |
| 6 | F-08 | **Completed:** share compatible in-flight separation | Verified independent outcomes, last-subscriber cancellation, focus union and native replacement |

For each, capture the before/after workload, change one concern, run relevant regressions, then native smoke. Do not attach a percentage saving until measured.

### P2 — do when touching the area

- F-10: **Completed:** reuse the non-stretched selected-part read; verified exact source-pair output, matching callback output digests and matched callback timings. Independent stretchers and the separate step-aware DSP concern are unchanged.
- F-11: **Completed:** one standard-library SDK scanner, all callers migrated, old aliases removed; verified unchanged behavior, independent companions and feature-disabled builds.
- **Scoped work completed:** Suggest inactive/Related/preference transitions and Palette closed/disabled states stop their visual timers; opening requests a refresh. Background completion and device/audio/MIDI/lighting work are preserved. Callback benchmarks confirm dispatch removal, but short idle profiling did not show a process-wide CPU reduction. Palette workspace visibility and other timers are unchanged.
- **Packaging footprint completed:** Linux install/release packaging excludes stale companion/unrelated root libraries, saving 25.10MiB on this install while retaining its executable/symbols. Existing dist/Pi profiles are unchanged; a fresh dist/LTO performance comparison remains unmeasured.

Lean: meaningful work removal is discovery, redundant sorting, unnecessary full-cache reads and duplicate separation. The XML consolidation removes 154 affected Rust source lines; it has no measured runtime saving. No dependency removal is justified by this audit.

## Guardrails to install

Reuse existing functional regressions for CUE pixels, command epochs, ALSA teardown, cancellation and generation publication. Add behavior checks only for the new boundaries: bounded completion turns, selection response identity, same-song subscription cancellation, resident row handles, memory admission, and fractional-first callback allocation.

For an already provisioned Linux CI job, publish performance logs as artifacts rather than adding tighter machine-dependent pass/fail timing thresholds:

```yaml
- name: Capture browser and waveform workload baselines
  shell: bash
  run: |
    set -o pipefail
    mkdir -p perf-results
    cargo test --release --bin openfader bench_search_filter_and_sort_over_100k_tracks -- --ignored --nocapture 2>&1 | tee perf-results/search.log
    cargo test --release --bin openfader benchmark_lazy_browser_search_sort -- --ignored --nocapture 2>&1 | tee perf-results/lazy-rows.log
    cargo test --release --bin openfader bench_waveform_ui_frames -- --ignored --nocapture 2>&1 | tee perf-results/waveforms.log
- uses: actions/upload-artifact@v4
  if: always()
  with:
    name: performance-workloads
    path: perf-results/
```

Prefer a manual/scheduled or dedicated stable runner for trend comparison. The existing search benchmark already contains a hard <10ms assertion; do not tighten it or treat a shared-runner miss as proof of a functional regression. Native cold-cache browsing, full-list scrolling and inference peak memory need separate fixtures; the existing first-32-row microbenchmark does not cover them.

## Open questions

No missing information blocks the first three changes. Before choosing deployment defaults, the product tradeoff is which target to optimize first: this desktop GPU, CPU/software low-memory devices, or battery-powered hardware. The same renderer/cache defaults need not be best for all three.

## Methodology and caveats

Read-only source analysis plus executed native workloads, existing benchmarks, structural scripts and one disposable cold-reader probe. Two independent bounded source audits covered audio/model lifetimes and library/background work; the main audit covered rendering, runtime measurements and cross-cutting findings.

- The OpenFadeR project was not indexed by codebase-memory; source searches/reads were used instead. No exhaustive dependency-cycle, dead-code or complete-callsite claim is made.
- `perf`, heaptrack and flamegraph were unavailable. No sampled CPU stack or allocator call-tree profile was captured, so process CPU is not attributed to individual functions.
- No new advisory/security scan, Clippy run or full correctness-suite run was performed for this read-only audit. Earlier fix verification is separate; the three named workload benchmarks were executed here and passed.
- Source counts include tests. Clone/hotspot tools have language/scope limits. Structural clone candidates are not confirmed drift defects.
- Native profiles are short single samples on a shared machine, with warmed/cold allocation histories differing. Do not infer statistically significant renderer/rate wins or private-heap size from RSS alone.
- The GPU used real NVIDIA rendering through VirtualGL/Xvfb. Physical display refresh, Wayland input-to-photon, physical controller latency and real output hardware were not measured.
- Actual inference peak/OOM, four-deck native memory, full 100k-row scrolling retention, slow-device stalls and concurrent large-library analysis were not exercised. Their findings are source-backed candidates with explicit verification plans, not claimed reproduced runtime failures.
- The owner’s collection/settings were not modified. User cache sizes were read only. No cache cleanup, application optimization or local reinstall was performed.

Evidence tiers describe recommendation support, not a fabricated performance confidence interval: **strong** = replicated architectural principle; **moderate** = supported mechanism/limited empirical measurements; **emerging** = recent unreplicated evidence; **expert opinion** = reasoned practice with little measurement; **contested** = conflicting evidence. The per-finding observed/measured/inferred statements are the actual code-specific confidence record.
