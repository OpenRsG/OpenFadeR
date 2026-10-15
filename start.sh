#!/usr/bin/env bash
# Start OpenFadeR. Usage: ./start.sh [path/to/a library | device root | folder of audio]
# With no argument: the last library opened, else ~/Music/OpenFadeR.
# The plugins, themes and controller mappings are their own repositories; the first run clones each into its place here
# (extension/plugin, extension/skin, mappings: ignored by this repository's git), where the app finds them next to its binary.
# Add your own via OPENFADER_PLUGINS / OPENFADER_SKINS / OPENFADER_MAPPINGS. The native plugins are built first.
set -euo pipefail
root="$(dirname "$(realpath "${BASH_SOURCE[0]}")")"
for pair in extension/plugin:OpenFadeR-Plugins extension/skin:OpenFadeR-Themes mappings:OpenFadeR-Map; do
	dir="$root/${pair%%:*}"
	[ -e "$dir" ] || git clone --quiet "https://github.com/OpenRsG/${pair#*:}.git" "$dir"
done
"$root/extension/plugin/build.sh" --quiet
exec cargo run --release --quiet --manifest-path "$root/Cargo.toml" -- "$@"
