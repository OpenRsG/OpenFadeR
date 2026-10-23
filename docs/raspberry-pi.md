# Raspberry Pi 3, 4, 5 and CM4

OpenFadeR runs on the 64-bit Raspberry Pi OS based on Debian 13 ("trixie"). Each release has a Pi archive,
`openfader-<version>-linux-aarch64-pi.tar.gz`. It runs on the Pi 3, 4, 5 and CM4. To build that archive yourself, cross-compile
it in Docker on any x86_64 machine, then copy it to the Pi:

```sh
docker buildx build -f Dockerfile.pi -o dist/pi .
scp dist/pi/openfader-*-linux-aarch64-pi.tar.gz pi@raspberrypi.local:
# on the Pi
tar xzf openfader-*-linux-aarch64-pi.tar.gz && ./openfader-*-linux-aarch64-pi/openfader
```

The archive holds the same plugins, themes and controller mappings as a release archive. The build takes the companion
repositories from GitHub (`--build-arg PLUGINS_REF=…`, `THEMES_REF`, `MAPPINGS_REF` pick a branch or tag); checkouts already
present in `extension/plugin`, `extension/skin` or `mappings` are used as they are. The cargo registry and the target folders stay in
BuildKit cache mounts, so the next build only recompiles what changed.

To build on the Pi itself, with the menu entry and the `openfader` command, run `./install.sh` from a checkout. On ARM Linux it
builds without the GPU STEMS engine; a plain `cargo build` needs `--no-default-features --features app` there.

The Pi archive updates itself too: Help > Check for Updates (and the daily check at start) offers a newer release's
`openfader-<version>-linux-aarch64-pi.tar.gz`, checks its SHA-256 and swaps it in next to the old version (kept in
`<folder>.prev`). Untick PREFERENCES > ADVANCED > Check for updates to stop the daily request; the update needs no GPU and no
extra software.

## What the build changes

- Profile `pi` (`Cargo.toml`): the `dist` profile with whole-program LTO and no symbol table (an 82 MB program instead of 104 MB).
- `-C target-cpu=cortex-a53`: the Pi 3's core. The code also runs on the Pi 4 / CM4 (Cortex-A72) and the Pi 5 (Cortex-A76). To build for one of those only, pass `--build-arg TARGET_CPU=cortex-a72` or `cortex-a76`.
- `--no-default-features --features app`: STEMS separates on the CPU. ONNX Runtime has no WebGPU build for ARM Linux; its
  ARM build is also why the program needs Debian 13 (glibc 2.38, GCC 14's C++ library).

## Memory

The program keeps each loaded track once, as 16-bit samples (10.6 MB per minute of stereo at 44.1 kHz), and decodes to that
directly; glibc's allocator is set to give freed blocks back. Measured on x86_64 with two 6-minute tracks loaded and playing
(Xvfb at 1280×800):

| Renderer | Idle | Both playing | Peak while loading |
|---|---|---|---|
| Software (`SLINT_BACKEND=winit-software`) | 73 MB | 202 MB | 259 MB |
| OpenGL through Mesa's llvmpipe | 210 MB | 359 MB | 406 MB |

The OpenGL figures are for llvmpipe, the CPU emulation of a GPU, which compiles shaders with LLVM and runs 16 threads; the Pi's
V3D (Pi 4 / CM4) and VC4 (Pi 3) drivers do neither, so they are not a measure of the Pi. On a 1 GB Pi 3, or wherever memory is
tight, start it with the software renderer:

```sh
SLINT_BACKEND=winit-software ./openfader
```

STEMS stays off unless you switch it on: its model is 316 MB and separating takes about 0.8 GB more, so it is for a 4 GB Pi or
larger.

## Speed

PREFS > VIEW > WAVEFORM > RENDERER selects AUTO (default), GPU or CPU without restarting playback.
AUTO uses hardware image composition when the window exposes an accelerated graphics API. The startup log reports
the API and OpenGL `GL_RENDERER`: V3D is hardware, while `llvmpipe` / `softpipe` are CPU emulation and AUTO keeps
the CPU waveform fallback. GPU explicitly permits emulated GL for diagnosis; it still falls back to CPU when
the window has no graphics API (for example `winit-software`). This setting does not replace the window's renderer.
If the log says llvmpipe on a Pi, fix the graphics driver or use `SLINT_BACKEND=winit-software`; forcing GPU does
not turn an emulated renderer into hardware.

The image-composition path keeps the enlarged strip in a texture and changes its source rectangle, rather than
moving a three-view-wide item. It also keeps each overview and its cue marker in a texture: the playhead and dashed
slip ghost are tiny separate textures. The CPU prepares waveform pixels only when the cached strip or overview
changes (load, zoom, size, palette, grid, cues, loops or stem data), not on each playhead step. The existing detail
strip cache is retained; no shader duplicates the CPU painter's palette, grid or cue rules.

Measured on x86_64 with an NVIDIA RTX 3090, a private headless Wayland compositor and its Xwayland display,
at 1500×950, 60 fps selected, with the same two generated four-minute WAVs playing (30-second samples):

| Path | Idle loaded | Both playing |
|---|---:|---:|
| Before: cached detail, CPU overview overlays | 1.4% | 31.2% |
| After: GPU source clipping and cached overview composition | 1.0% | 26.6% |

Percentages are process CPU, where 100% is one core, not GPU usage. The total reduction here was about 15%;
it is not a Pi measurement or a promise of the same reduction there. The detail strip was already cached before
this change, so it was not rasterized every frame. A release microbenchmark at 680×50 measured 10.756 µs per CPU
overview-overlay update versus 0.002 µs for cached marker placement; the latter excludes Slint composition and GPU
time. CPU fallback with the software renderer remained usable in the two-deck smoke test. Native Wayland and X11
use the same images and painter; no GL-only waveform shader is required.

For the same scene under Xvfb with `SLINT_BACKEND=winit-software`, CPU fallback measured 29.8% before and 20.2%
after (idle loaded: 0.9% and 0.6%). These are single-run whole-process samples, including the rest of the UI and
scheduling variation; they do not isolate the GPU optimization. Forcing GPU on that software renderer correctly
logged a CPU fallback and kept both decks playing.

An earlier software-renderer measurement with two decks playing drew 62 frames per second using 16 % of one x86_64 core. The `Dockerfile.pi`
build, run under qemu-user emulation in a container limited to 4 cores and 1 GB, loaded, analysed and played the same two
tracks at 31 to 34 frames per second; the tracks added 135 MB over idle, as on x86_64 (qemu's own translator adds about
130 MB, so its totals are not the Pi's). A Pi core is several times slower than an x86_64 one: on a Pi 3, or at 1920×1080,
set PREFS > VIEW > WAVEFORM > DRAWING RATE to 30, which halves the drawing work. A track plays as soon as it is decoded; its
analysis finishes afterwards without holding up the window.
