#!/usr/bin/env python3
"""Fills a release folder with what the app finds next to its executable (src/paths.rs search_path): the plugins
(extension/plugin: each plugin folder as the app reads it, with the library built for this OS, no sources), the themes
(extension/skin) and the controller mappings (mappings). Usage: bundle.py <folder>  (run from the core checkout with the
companion repositories in place and the plugins built by extension/plugin/build.sh)."""
import json
import pathlib
import shutil
import sys

dest = pathlib.Path(sys.argv[1])
root = pathlib.Path(".")
SKIP = {"src", "tests", "examples", "target", "dist", ".git", ".github", "Cargo.toml", "Cargo.lock"}

# The adapters of compatibility/ are compiled into the app: an older companion checkout may still hold their folders
# (and libraries), which the app ignores by name, so they are not shipped.
builtin = {json.loads(f.read_text())["name"] for f in (root / "compatibility").glob("**/plugin.json")}
plugins = 0
for manifest in sorted((root / "extension/plugin").glob("*/plugin.json")):
    src = manifest.parent
    if json.loads(manifest.read_text()).get("name") in builtin:
        continue
    shutil.copytree(src, dest / "extension/plugin" / src.name, ignore=lambda _, names: [n for n in names if n in SKIP])
    plugins += 1

(dest / "extension/skin").mkdir(parents=True, exist_ok=True)
themes = sorted((root / "extension/skin").glob("*.json"))
for f in themes:
    shutil.copy(f, dest / "extension/skin" / f.name)

(dest / "mappings").mkdir(parents=True, exist_ok=True)
maps = [f for f in sorted((root / "mappings").iterdir()) if f.suffix in (".xml", ".json")]
for f in maps:
    shutil.copy(f, dest / "mappings" / f.name)

print(f"bundled {plugins} plugins, {len(themes)} themes, {len(maps)} mappings into {dest}")
if not (plugins and themes and maps):
    sys.exit("a companion repository is missing or empty")
