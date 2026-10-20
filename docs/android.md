# Android

`mobile/` is the standalone, touch-first Rust/Slint application for phones and tablets. It uses the same audio engine, decoder, analysis cache and SQLite Collection as the desktop application, with its own UI. It does not require the companion repositories.

Implementation status, the verified APK identity and continuation checks are recorded in [the Android handoff](../mobile/android/HANDOFF.md).

## Mixing

- Two decks, track overview waveforms, seek, play/pause, memory cue and four saved hot cues per track.
- ±16% tempo, key lock, beat loops of 1/2/4/8/16 beats and one-shot tempo/phase sync to the other deck. Sync refuses tracks with no detected grid or tempos outside the fader range; it is not a continuously engaged sync mode.
- Three-band isolator EQ, channel levels, constant-power crossfader and master level.
- Filter, Space, Dub Echo, Sweep, Noise and Crush using the shared COLOR FX engine.
- Headphone cue with a DJ splitter cable: left output is mono cue, right output is mono master. Leave split off for ordinary stereo output.

Android runs in landscape only, in either direction. Both decks remain visible around a central mixer, with waveform seeking, playhead platters, pitch faders, transport and beat controls. Drag a platter horizontally for relative jog seeking; it is not scratching. Mixer / Pads switches between channel faders/headphone cues and the eight hot cue pads. More opens Library/import, Mixer/output, Deck controls/FX, help and a dedicated pad view. On short landscape displays, sync, loop length, loop enable and key lock remain available under More → Deck controls & FX rather than squeezing the platters or touch targets. Pause a playing deck before replacing its track.

This app does not expose desktop video, lighting, Beat Lab, STEMS separation, MIDI, device export, external plugins, sampler or edit mode. The desktop application is unchanged.

## Music and lifecycle

**Import music** opens Android's document picker and accepts multiple MP3, AAC, M4A, FLAC, WAV or AIFF files. Files are copied into app-private storage; the originals are not changed. The app requests no broad storage permission or network permission. Files, analysis and cues remain after restart; uninstalling the app removes them.

The screen stays awake while playing. Switching away from the app, losing audio focus or disconnecting headphones pauses playback. Returning does not automatically resume it. There is no background playback service.

Android Back closes More first, returns secondary views to the decks, then backgrounds the performance screen. It does not tear down the native UI just to navigate. If Android recreates the activity, the JNI activity reference is replaced while the process's data directories are initialized only once.

Tracks are decoded in full, then retained by the audio engine as 16-bit stereo PCM. Long tracks use correspondingly more memory. Bluetooth output adds device-dependent latency; wired output is preferable for cueing.

## Build an APK

On Linux, install Rust 1.92 or newer, JDK 17, Gradle 8.11.1 and the Android SDK command-line tools. Install the pinned SDK/NDK packages and Rust packaging tool:

```sh
sdkmanager 'platforms;android-35' 'build-tools;35.0.0' 'platform-tools' 'ndk;28.2.13676358'
cargo install cargo-ndk --locked
export ANDROID_HOME="$HOME/Android/Sdk"
./mobile/build-android.sh
```

The script installs the selected Rust targets, builds the Rust library, packages the shared C++ runtime and builds a debug-signed APK. It sets `ANDROID_JAR` to API 35 explicitly: Slint's Java helper needs the compile SDK jar, not cargo-ndk's API 26 deployment setting. Override `ANDROID_NDK_HOME` or `GRADLE` when the tools are installed elsewhere.

The APK is `mobile/android/app/build/outputs/apk/debug/app-debug.apk`. It supports Android 8.0/API 26 and newer. The default ABI is ARM64. To include an x86-64 emulator:

```sh
OPENFADER_ANDROID_ABIS='arm64-v8a x86_64' ./mobile/build-android.sh
adb install -r mobile/android/app/build/outputs/apk/debug/app-debug.apk
adb shell am start -n org.openfader.mobile/.OpenFaderActivity
```

Debug signing is for sideloading and development, not store distribution. No release signing key is included.

### Updating debug builds

Keep `$HOME/.android/debug.keystore` private, outside the repository, and reuse it in isolated build environments. Updates signed with the same key can use `adb install -r` without removing app data. The first downloadable APK used a disposable signing key; the lifecycle-fixed APK uses a persisted development key and cannot update that first APK in place.

**Do not uninstall an existing copy before backing up its imported music and cues.** Uninstall deletes app-private storage. For these debuggable builds, with one authorized device connected, stop the app and save its files before changing signing keys:

```sh
adb shell am force-stop org.openfader.mobile
adb exec-out run-as org.openfader.mobile tar -cf - files > openfader-backup.tar
tar -tf openfader-backup.tar
```

Keep the archive private and verify that it includes `files/Music/` and `files/data/openfader/store.db` before uninstalling. Then uninstall the old debug build, install the new APK, and restore **before opening it**. Android assigns a new app UID after reinstall; Android 8's tar otherwise fails while trying to restore the old ownership. With Bash and Python 3, validate the archive's paths and normalize ownership to the new app UID:

```bash
set -euo pipefail
adb uninstall org.openfader.mobile
adb install mobile/android/app/build/outputs/apk/debug/app-debug.apk
uid=$(adb shell run-as org.openfader.mobile id -u | tr -d '\r')
python3 - "$uid" <<'PY' | adb shell -T run-as org.openfader.mobile tar -xf -
import pathlib, sys, tarfile
uid = int(sys.argv[1])
with tarfile.open("openfader-backup.tar") as source:
    entries = source.getmembers()
    for entry in entries:
        parts = pathlib.PurePosixPath(entry.name).parts
        if not parts or parts[0] != "files" or ".." in parts or not (entry.isfile() or entry.isdir()):
            raise ValueError("Unexpected backup entry: " + entry.name)
    with tarfile.open(fileobj=sys.stdout.buffer, mode="w|", format=tarfile.GNU_FORMAT) as target:
        for entry in entries:
            entry.uid = entry.gid = uid
            entry.uname = entry.gname = ""
            target.addfile(entry, source.extractfile(entry) if entry.isfile() else None)
PY
adb shell am start -n org.openfader.mobile/.OpenFaderActivity
```

Keep the backup until the Library and saved cues have been checked. This migration was exercised across different debug certificates on API 26; both imported WAVs remained byte-identical and the saved memory/hot cue rows survived. It is not a release-build backup mechanism.

## Desktop preview

The same mobile UI can be run under X11 for layout work.

On Debian/Ubuntu the preview needs the desktop runtime/development libraries: `libasound2-dev`, `libxkbcommon-dev`, `libxkbcommon-x11-0`, `libxcursor1`, `libxi6` and the X11 client libraries. These are not Android device dependencies.

```sh
cargo run --manifest-path mobile/Cargo.toml --features preview -- /path/to/music
cargo test --manifest-path mobile/Cargo.toml --lib
```

In the preview, Import music rescans the supplied folder; Android uses the native picker. Use a disposable HOME/XDG environment when testing to keep the preview's Collection and saved cues separate from the desktop app.

Linux phones should run the full desktop executable with the [Pocket touch theme](skins.md#structural-presentation), not this reduced Android preview. That preserves desktop modes, plugins and library/device tools while changing presentation. On a compositor that already scales XWayland, use `WINIT_X11_SCALE_FACTOR=1` to avoid applying DPI scaling twice.

The preview's legacy Linux layout follows the available window size, not a device model: two decks flank the mixer at 760 logical pixels, tall narrow windows stack decks, and short narrow windows select one deck. Android selects its landscape console instead. This is useful for Android frontend development, but it is not the full Linux phone interface.

For an isolated X11 viewport regression check, build the preview, install Xvfb, Openbox, xdotool and ImageMagick, then run:

```sh
xvfb-run -a -s '-screen 0 1280x1400x24' sh -c \
  'openbox & python3 mobile/check-layout.py mobile/target/release/openfader-mobile'
```

This checks preview window sizes from 320×568 through 1000×540 and saves screenshots/logs. Earlier X11 checks exercised Library loading and EQ. These checks prove neither physical audio/touch behavior nor Android lifecycle correctness. Use `ui/check-touch.py` for the full Linux application's Pocket theme.


## Verification

The ARM64 and x86-64 libraries and initial debug APK were built in a clean Linux container. The original API 30 smoke exercised multi-file import/cancellation, both decks playing, sync/loops, saved cues, EQ/effect controls and playing-deck load protection. Phone widths around 320/375/414 logical pixels, the tablet dual-deck layout and rotation were inspected; the desktop preview opened under private Xvfb.

Follow-up runs on API 26 (Android 8), API 30 (Android 11) and API 35 (Android 15), with SELinux enforcing, rechecked native document import, deck loading/playback, sync/loops, background pause, the headphone-disconnect broadcast and saved cues. The API 26/35 runs exposed a lifecycle deadlock missed by the original smoke: Java's NativeActivity pause callback awaited Rust while Rust awaited a Java UI-thread FutureTask. The bridge now posts UI-only work without waiting and calls the thread-safe audio-focus API directly. Four repeated Home/resume cycles per API 26/35 emulator completed without fresh ANR, fatal exception or Rust panic log entries; screenshots confirmed paused transport after interruption.

Before the landscape redesign, the lifecycle-only APK was rebuilt with all four native libraries byte-identical to the original; its signature was verified and the exported APK's digest matched. The original mobile sync boundary check and 569 shared-core unit tests passed. Those unchanged core suites were not rerun for the landscape UI work.

The landscape APK rebuild includes ARM64 and x86-64 native libraries, the same persisted debug signer, no requested permissions, and 16 KiB native LOAD alignment. API 35 emulator checks exercised both decks, playback focus, waveform/jog seeking, sync/loops, memory/hot-cue persistence, More destinations, search, Back navigation, background/reopen and same-process activity recreation. Actual fader touches changed channel gain, EQ, master level and tempo; this caught and fixed a focus scope that intercepted pointer input. Four physical device rotations kept a landscape app surface. Fresh launches at 568×320, 640×360, roughly 853×427 and 1280×800 logical sizes were visually inspected; live resolution changes on a non-resizable activity use Android size-compat scaling, so they are not equivalent to a fresh device-class launch.

API 26 separately exercised the immersive startup, native document picker and loading both imported demo tracks in the landscape console. The complete latest-APK interaction pass used API 35; an additional concurrent legacy playback run was interrupted by sandbox memory pressure and is not counted as a pass. The remote emulator view is video/input only, not an audio stream.

All Android runtime checks used x86-64 emulators. ARM64 execution, real-device output, Bluetooth behavior, end-to-end latency and TalkBack have not been verified.

## Layout

- `mobile/src/lib.rs`: window creation, shared session state and callbacks.
- `mobile/src/deck.rs`: deck loading, transport helpers, waveform image and sync boundary check.
- `mobile/src/tick.rs`: load/import completion polling and UI updates.
- `mobile/src/library.rs`: indexing imported files through the existing folder reader and Collection.
- `mobile/src/platform.rs`: JNI bridge, private directories and Android interruption/import queues.
- `mobile/ui/`: `state.slint` owns the section globals; `app.slint` owns navigation, `deck.slint`/`platter.slint` the decks, `mixer.slint` the central mixer/pads, `panels.slint` the More destinations and `widgets.slint` the touch-sized controls. All use the desktop `ui/theme.slint` tokens and shared glyphs/faders/meters.
- `mobile/android/`: NativeActivity, document copying, audio focus and Gradle packaging.

The core's `stems` feature owns the ONNX dependency and is enabled by the desktop `app` feature. Mobile uses `default-features = false`; the desktop-only removable-device module is not compiled for Android.
