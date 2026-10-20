# Android development handoff

The Android app belongs to this OpenFadeR checkout. `mobile/` is the Rust/Slint application; this directory is its Android packaging and Java platform bridge, not a second implementation. Do not create a separate OpenFaderMobile project or move the Rust/UI sources into Gradle.

The complete build, backup, feature and verification contract is in [docs/android.md](../../docs/android.md). The desktop app and the concurrent Linux-phone responsive layout must remain intact.

## Current implementation

- Standalone two-deck DJ app; no video, lighting or Beat Lab. Desktop-only features are excluded through the mobile core dependency's disabled default features.
- Landscape-only Android console in either landscape orientation, using the desktop Theme, glyphs, faders and meters. Twin playhead platters, waveform/jog seeking, pitch, sync/loops, transport, mixer/crossfader, headphone cue and hot-cue pads are wired to the shared engine. Jog seeking is not scratching; sync is one-shot, not continuously engaged.
- More opens Library/import, mixer/output, deck controls/FX, help and a full pad view. Short screens move beat controls into More instead of shrinking touch targets.
- Native multi-file document import copies audio into private storage. Collection, analysis and saved cues persist. The app requests no broad storage or network permission.
- Playback is foreground-only. Backgrounding, audio-focus loss or headphone disconnection pauses it; returning does not resume automatically.

## Source ownership

| Path relative to `mobile/` | Responsibility |
| --- | --- |
| `src/lib.rs`, `src/tick.rs` | Session, callback wiring, completion polling and UI updates |
| `src/deck.rs`, `src/library.rs` | Shared-engine decks, loading, waveforms, sync and Collection indexing |
| `src/platform.rs` | JNI activity reference, process directories and import/interruption queues |
| `ui/state.slint`, `ui/app.slint` | Section globals, navigation and platform-specific layout selection |
| `ui/deck.slint`, `ui/platter.slint`, `ui/mixer.slint` | Indexed decks, playhead platters and mixer/pads |
| `ui/panels.slint`, `ui/widgets.slint` | More destinations and touch-sized shared controls |
| `android/app/src/main/AndroidManifest.xml` | Activity, orientation and Android application declaration |
| `android/app/src/main/java/org/openfader/mobile/OpenFaderActivity.java` | Picker, private file copying, audio focus, immersive bars and backgrounding |
| `build-android.sh`, `android/app/build.gradle` | Native compilation, shared C++ runtime and APK packaging |

## Rebuild and update

Run from the OpenFadeR repository root, with the toolchain installed as described in the build guide:

```sh
export ANDROID_HOME="$HOME/Android/Sdk"
OPENFADER_ANDROID_ABIS='arm64-v8a x86_64' ./mobile/build-android.sh
adb install -r mobile/android/app/build/outputs/apk/debug/app-debug.apk
adb shell am start -n org.openfader.mobile/.OpenFaderActivity
```

The default ABI is ARM64; include x86-64 for the emulator. Compilation uses SDK/target 35, deployment/minimum 26, NDK `28.2.13676358`, build-tools `35.0.0`, JDK 17 and Gradle `8.11.1`. The verified builder used Rust `1.99.1` and cargo-ndk `4.1.2`. Gradle currently declares version `0.2.1`, code `201`.

Keep `ANDROID_JAR` on API 35 even though cargo-ndk deploys to API 26: Slint's Java helper needs the newer compile SDK. The script sets this and packages `libc++_shared.so` with `--link-libcxx-shared`.

Reuse the private `$HOME/.android/debug.keystore` in isolated builders; never commit it. A different signing certificate prevents an in-place update. Do not uninstall to work around that without first backing up private music and cues; the build guide contains the exercised backup/UID-normalized restore procedure.

## Verified artifact, 2026-10-19

- APK: `app/build/outputs/apk/debug/app-debug.apk`, ARM64 plus x86-64, debug signed.
- SHA-256: `f0b26f722a9fb0b7cb567e82106510485dcce7fa8edafc4ec37cb33c923813f8`.
- Public signer certificate SHA-256: `58e75e1400ee97c57031306eea90b1c29e41aa676d7065291996a89ff6a6a3ab`.
- All four packaged native libraries have at least 16 KiB LOAD alignment; no requested Android permissions. Same-key `adb install -r` succeeded without uninstalling.
- Actual playing-surface capture: `app/build/outputs/apk/debug/landscape.png`.

These are generated, ignored outputs, not source-controlled deliverables. Rebuilding changes the artifact identity; the hashes describe this tested snapshot only.

API 35 emulator checks exercised both loaded decks and playback, sync/loops, waveform/jog seeking, memory/hot-cue persistence, search, gain/EQ/master/tempo faders and same-process Activity recreation. The final APK separately passed every More destination's Back path, menu Close/scrim/Back, background/reopen with loaded decks retained, playback controls and four physical rotations with a 1280×640 landscape surface. Fresh logs contained no matching ANR, fatal exception, Rust panic or binding-loop entries.

Phone/tablet layouts were inspected after fresh launches at 568×320, 640×360, roughly 853×427 and 1280×800 logical sizes. API 26 separately passed landscape startup, native multi-file import and loading both demo tracks. An additional concurrent legacy playback run timed out under resource pressure; it is not a latest-APK playback pass. Previously passing shared-core tests were not rerun for these UI changes.

## Fixes to preserve

- **Back and conditional focus:** Slint 1.18 maps Android Back to `Key.Back`, not Escape or a window-close request. Its API 34+ helper finishes the Activity if the event is unhandled. The ancestor FocusScope must accept Back and regain focus when navigation removes a focused menu button. More closes first, secondary views return to decks, then root Back posts `moveTaskToBack(true)`. Do not add competing Java Back hooks.
- **Activity recreation:** NativeActivity can re-enter Rust in the same process. Replace the JNI activity GlobalRef each time, but initialize HOME/XDG directories only once.
- **Lifecycle deadlock:** Android pause waits for Rust acknowledgement. Rust must not synchronously wait for Java UI work during that path. Post UI-only work; use thread-safe audio-focus APIs directly.
- **Touch input:** A sibling FocusScope over a Fader intercepts pointer events. Keep the Fader inside the focus scope and check an actual changed value. Saved and empty cue pads need equal stretch despite different icon content.
- **Slint layout/rendering:** Latch responsive choices from window dimensions; child-size-dependent visibility caused intrinsic layout cycles. Slint 1.18 does not support Rectangle `rotation-angle`; the platter marker uses trigonometric coordinates. Do not replace the Linux-phone adaptive branches with Android-only assumptions.
- **Emulator resources:** Keep one emulator active while compiling in an 8 GB sandbox. Two emulators plus compilation killed the VM and left stale lock files. Remove only owned stale locks after confirming no live QEMU or lock holders. Stop/relaunch the Activity after resolution/density changes: a live non-resizable Activity otherwise exercises Android size-compat letterboxing, not a fresh device class.

## Remaining verification before shipping

1. Run the current APK on a physical ARM64 Android device; check audible stereo output, wired split cue/master routing, interruptions, sustained mixing and memory use with real-length tracks.
2. Re-run the full current-APK playback/control/lifecycle pass on API 26 without simultaneous compilation or a second emulator.
3. Check Bluetooth latency/route changes, end-to-end touch-to-audio latency and TalkBack. None was verified by the remote emulator; its remote view streams video/input, not audio.
4. Use a separately managed release signing key for distribution. This snapshot is a development APK, not a store-ready release.
