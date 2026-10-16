#!/usr/bin/env bash
# Builds OpenFadeR from this checkout and installs it for the current user (Linux):
#   ~/.local/lib/openfader/   the program with its bundled plugins, themes and controller mappings (as in a release archive)
#   ~/.local/bin/openfader    the command
#   ~/.local/share/applications/openfader.desktop   the menu entry
# and adds the default plugin, theme and mapping repositories to PREFERENCES (GET PLUGINS / GET THEMES / GET MAPPINGS)
# unless you already have a repository list. Run it again to update. Remove with: ./install.sh --uninstall
set -euo pipefail
root="$(dirname "$(realpath "${BASH_SOURCE[0]}")")"
lib="${OPENFADER_PREFIX:-$HOME/.local}/lib/openfader"
bin="${OPENFADER_PREFIX:-$HOME/.local}/bin"
apps="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
config="${XDG_CONFIG_HOME:-$HOME/.config}/openfader"
repos=(OpenRsG/OpenFadeR-Plugins OpenRsG/OpenFadeR-Themes OpenRsG/OpenFadeR-Map) # = src/catalog/list.rs DEFAULTS

if [ "${1:-}" = --uninstall ]; then
	rm -f "$bin/openfader" "$apps/openfader.desktop"
	[ -d "$lib" ] && rm -r "$lib"
	echo "removed (your settings, library edits and installed add-ons in $config stay)"
	exit
fi

cd "$root"
for pair in extension/plugin:OpenFadeR-Plugins extension/skin:OpenFadeR-Themes mappings:OpenFadeR-Map; do
	[ -e "${pair%%:*}" ] || git clone --quiet "https://github.com/OpenRsG/${pair#*:}.git" "${pair%%:*}"
done
extension/plugin/build.sh --quiet
cargo build --release --quiet

[ -d "$lib" ] && rm -r "$lib"
mkdir -p "$lib" "$bin" "$apps"
cp target/release/openfader "$lib/"
cp -L target/release/*.so* "$lib/" 2>/dev/null || true # ONNX Runtime / WebGPU for STEMS, found next to the program
python3 .github/bundle.py "$lib"
ln -sf "$lib/openfader" "$bin/openfader"
cat >"$apps/openfader.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=OpenFadeR
Comment=DJ software
Exec=$lib/openfader %F
Icon=multimedia-audio-player
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
