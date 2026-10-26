#!/bin/bash
# Native Intel validation only; Apple-silicon packages use the vendor's GPU-enabled runtime.
set -euo pipefail
version=1.31.0 # Matches Cargo.toml's ONNX Runtime api-31.
out="$GITHUB_WORKSPACE/.local/onnxruntime-intel"
if [ ! -f "$out/libonnxruntime.$version.dylib" ]; then
    source_dir=$(mktemp -d "$RUNNER_TEMP/onnxruntime-XXXXXX")
    trap 'rm -rf "$source_dir"' EXIT
    git clone --depth 1 --branch "v$version" --recurse-submodules --shallow-submodules \
        https://github.com/microsoft/onnxruntime.git "$source_dir"
    (
        cd "$source_dir"
        ./build.sh --config Release --update --build --build_shared_lib --parallel 2 \
            --compile_no_warning_as_error --cmake_extra_defines CMAKE_OSX_ARCHITECTURES=x86_64
    )
    mkdir -p "$out"
    cp -L "$source_dir/build/MacOS/Release/"*.dylib "$out/"
fi
lipo -verify_arch x86_64 "$out/libonnxruntime.$version.dylib"
printf 'ORT_LIB_LOCATION=%s\nORT_PREFER_DYNAMIC_LINK=1\n' "$out" >> "$GITHUB_ENV"
