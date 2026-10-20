#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
: "${ANDROID_HOME:?Set ANDROID_HOME to your Android SDK}"
: "${ANDROID_NDK_HOME:=$ANDROID_HOME/ndk/28.2.13676358}"
export ANDROID_NDK_HOME
: "${ANDROID_JAR:=$ANDROID_HOME/platforms/android-35/android.jar}"
export ANDROID_JAR
command -v cargo >/dev/null
command -v javac >/dev/null
cargo ndk --version >/dev/null || { echo 'Install cargo-ndk: cargo install cargo-ndk --locked' >&2; exit 1; }
[[ -d "$ANDROID_NDK_HOME" ]] || { echo "Missing NDK: $ANDROID_NDK_HOME" >&2; exit 1; }
ABIS="${OPENFADER_ANDROID_ABIS:-arm64-v8a}"
args=()
for abi in $ABIS; do
    case "$abi" in
        arm64-v8a) rustup target add aarch64-linux-android ;;
        x86_64) rustup target add x86_64-linux-android ;;
        *) echo "Unsupported ABI: $abi" >&2; exit 1 ;;
    esac
    args+=(-t "$abi")
done
cargo ndk "${args[@]}" --platform 26 --link-libcxx-shared -o "$ROOT/android/app/src/main/jniLibs" build --manifest-path "$ROOT/Cargo.toml" --lib --release --locked
GRADLE="${GRADLE:-gradle}"
"$GRADLE" --project-dir "$ROOT/android" --no-daemon "-PopenfaderAbis=$ABIS" assembleDebug
printf '\nAPK: %s\n' "$ROOT/android/app/build/outputs/apk/debug/app-debug.apk"
