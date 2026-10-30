#!/usr/bin/env bash
# Builds OpenFadeR from this checkout and installs it for the current user (Linux):
#   ~/.local/lib/openfader/   the program with its bundled plugins, themes and controller mappings (as in a release archive)
#   ~/.local/bin/openfader    the command
#   ~/.local/share/applications/openfader.desktop   the menu entry, with its icon in ~/.local/share/icons
# and adds the default plugin, theme and mapping repositories to PREFERENCES (GET PLUGINS / GET THEMES / GET MAPPINGS)
# unless you already have a repository list. Run it again to update. Remove with: ./install.sh --uninstall
# (An in-app update, Help > Check for Updates, swaps $lib for the new version and keeps the old one in $lib.prev; both modes of this script remove it.)
set -euo pipefail
root="$(dirname "$(realpath "${BASH_SOURCE[0]}")")"
lib="${OPENFADER_PREFIX:-$HOME/.local}/lib/openfader"
bin="${OPENFADER_PREFIX:-$HOME/.local}/bin"
apps="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
icons="${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor/256x256/apps"
config="${XDG_CONFIG_HOME:-$HOME/.config}/openfader"
repos=(OpenRsG/OpenFadeR-Plugins OpenRsG/OpenFadeR-Themes OpenRsG/OpenFadeR-Map) # = src/catalog/list.rs DEFAULTS

if [ "${1:-}" = --uninstall ]; then
	rm -f "$bin/openfader" "$apps/openfader.desktop" "$icons/openfader.png"
	[ -d "$lib" ] && rm -r "$lib"
	rm -rf "$lib.prev" "$lib.new" "$lib.swap"
	exit
fi

cd "$root"
[ -e extension/plugin ] || git clone --quiet --branch "${PLUGINS_REF:-v0.2.12}" \
	https://github.com/OpenRsG/OpenFadeR-Plugins.git extension/plugin
for pair in extension/skin:OpenFadeR-Themes mappings:OpenFadeR-Map; do
	[ -e "${pair%%:*}" ] || git clone --quiet "https://github.com/OpenRsG/${pair#*:}.git" "${pair%%:*}"
done
extension/plugin/build.sh --quiet
# ONNX Runtime ships no WebGPU build for ARM Linux (Raspberry Pi): STEMS separates on the CPU there.
features=()
[ "$(uname -s)-$(uname -m)" = Linux-aarch64 ] && features=(--no-default-features --features app)
cargo build --release --quiet "${features[@]}"

[ -d "$lib" ] && rm -r "$lib"
rm -rf "$lib.prev" "$lib.new" "$lib.swap" # a build installed here is newer than what an in-app update kept
mkdir -p "$lib" "$bin" "$apps" "$icons"
cp target/release/openfader "$lib/"
# Plugins are bundled in their own folders, not beside the executable.
for runtime in target/release/libonnxruntime*.so* target/release/libwebgpu_dawn.so*; do
	[ -f "$runtime" ] || continue
	cp -L "$runtime" "$lib/"
done
python3 .github/bundle.py "$lib"
ln -sf "$lib/openfader" "$bin/openfader"
cp assets/icon.png "$icons/openfader.png"
# A stale icon-theme.cache hides icons added after it was written: refresh it when there is one.
theme="${icons%/256x256/apps}"
if [ -e "$theme/icon-theme.cache" ] && command -v gtk-update-icon-cache >/dev/null; then
	gtk-update-icon-cache -q -f -t "$theme" || true
fi
cat >"$apps/openfader.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=OpenFadeR
Comment=DJ software
Exec=$lib/openfader %F
Icon=openfader
StartupWMClass=openfader
Terminal=false
Categories=AudioVideo;Audio;Player;
EOF

if [ ! -e "$config/repositories.json" ]; then
	mkdir -p "$config"
	printf '[\n  "%s",\n  "%s",\n  "%s"\n]\n' "${repos[@]}" >"$config/repositories.json"
	echo "repositories: ${repos[*]}"
fi
echo "installed: run 'openfader' or pick OpenFadeR in the application menu"
case ":$PATH:" in *":$bin:"*) ;; *) echo "note: $bin is not on your PATH" ;; esac
