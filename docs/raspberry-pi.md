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

With two decks playing, the software renderer drew 62 frames per second using 16 % of one x86_64 core. The `Dockerfile.pi`
build, run under qemu-user emulation in a container limited to 4 cores and 1 GB, loaded, analysed and played the same two
tracks at 31 to 34 frames per second; the tracks added 135 MB over idle, as on x86_64 (qemu's own translator adds about
130 MB, so its totals are not the Pi's). A Pi core is several times slower than an x86_64 one: on a Pi 3, or at 1920×1080,
set PREFS > VIEW > WAVEFORM > DRAWING RATE to 30, which halves the drawing work. A track plays as soon as it is decoded; its
analysis finishes afterwards without holding up the window.
