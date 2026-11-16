#!/usr/bin/env python3
"""The body of every GitHub release, in one format.

    release_notes.py VERSION ARCHIVE_DIR NOTES.md [--repo OWNER/NAME] [--plugins REF] [--themes REF] [--mappings REF]

NOTES.md is the hand-written part (docs/releases/VERSION.md, see docs/releases/README.md). It is checked here, so a release
cannot go out without it or with a section the format does not know. The five archives must all be in ARCHIVE_DIR; their
size and SHA-256 are written into the download table. Prints the Markdown body; the release workflow appends GitHub's
generated "What's Changed" list.
"""
import argparse
import hashlib
import pathlib
import re
import sys

# Sections of the hand-written notes, in the order they are shown.
SECTIONS = [
    ("New", "✨"),
    ("Fixes", "🐛"),
    ("Changes", "🔧"),
    ("Docs", "📖"),
    ("Build", "⚡"),
    ("Known issues", "⚠️"),
    ("Please test", "🧪"),
]

# (label, detail, archive name, how to start it); the order of the download table.
PLATFORMS = [
    (
        "🪟 Windows",
        "x86_64",
        "openfader-{v}-windows-x86_64.zip",
        'Unzip and run `openfader.exe`. The app is not code-signed, so SmartScreen may warn: "More info" > "Run anyway".',
    ),
    (
        "🍎 macOS",
        "Apple silicon",
        "OpenFadeR-{v}-macos-arm64.zip",
        "The zip holds `OpenFadeR.app`. It is ad-hoc signed, not developer-signed or notarized: move it to Applications, then "
        "right-click > Open (once) and confirm. If macOS says it is damaged, run "
        "`xattr -dr com.apple.quarantine /Applications/OpenFadeR.app`. Native tests and smoke run on Intel macOS; the ARM "
        "archive is cross-built and checked for ARM libraries and signatures, without an ARM runtime smoke test.",
    ),
    (
        "🐧 Linux",
        "x86_64",
        "openfader-{v}-linux-x86_64.tar.gz",
        "Unpack and run `./openfader`. Needs the usual desktop libraries (ALSA, X11 or Wayland, fontconfig). Install "
        "`zenity` or `kdialog` for the file chooser.",
    ),
    (
        "🍓 Raspberry Pi 3, 4, 5, CM4",
        "aarch64, Debian 13",
        "openfader-{v}-linux-aarch64-pi.tar.gz",
        "64-bit Raspberry Pi OS based on Debian 13. Unpack and run `./openfader-{v}-linux-aarch64-pi/openfader`. "
        "See [docs/raspberry-pi.md](https://github.com/{repo}/blob/v{v}/docs/raspberry-pi.md).",
    ),
    (
        "🍓 Raspberry Pi 4, CM4, tuned",
        "aarch64, Debian 13",
        "openfader-{v}-linux-aarch64-pi4.tar.gz",
        "Cortex-A72 code generation and whole-program LTO. Its updater stays on this variant; it is not for Pi 3.",
    ),
]

OPTIONAL = (
    "Optional: `ffmpeg` on the PATH for video; `python` (3) for the Python plugins. Volume sliders of the audio page exist "
    "on Linux only: on Windows and macOS the volume is the system's."
)


def check_notes(text: str) -> tuple[str, dict[str, str]]:
    """The one-line summary and the body of each section, or ValueError."""
    lines = text.strip().splitlines()
    if not lines or not lines[0].startswith("> "):
        raise ValueError("the first line must be the summary, as a quote: '> One sentence.'")
    summary = lines[0][2:].strip()
    known = [name for name, _ in SECTIONS]
    sections: dict[str, list[str]] = {}
    current = None
    for line in lines[1:]:
        m = re.match(r"##\s+(.*?)\s*$", line)
        if m:
            current = m.group(1)
            if current not in known:
                raise ValueError(f"unknown section '## {current}'; use one of: {', '.join(known)}")
            if current in sections:
                raise ValueError(f"section '## {current}' appears twice")
            sections[current] = []
        elif current is None:
            if line.strip():
                raise ValueError("text between the summary and the first '## ' section")
        else:
            sections[current].append(line)
    bodies = {k: "\n".join(v).strip() for k, v in sections.items()}
    empty = [k for k, v in bodies.items() if not v]
    if empty:
        raise ValueError(f"empty section: {', '.join(empty)}")
    if not bodies:
        raise ValueError("no section: add at least one of " + ", ".join(known))
    return summary, bodies


def sha256(path: pathlib.Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def build(version: str, archives: pathlib.Path, notes: str, repo: str, plugins: str, themes: str, mappings: str) -> str:
    summary, bodies = check_notes(notes)
    expected = {p[2].format(v=version) for p in PLATFORMS}
    actual = {p.name for p in archives.iterdir() if p.name.endswith((".zip", ".tar.gz"))}
    if actual != expected:
        raise ValueError(f"missing: {sorted(expected - actual)}; unexpected: {sorted(actual - expected)}")

    base = f"https://github.com/{repo}/releases/download/v{version}"
    rows, sums, start = [], [], []
    for label, detail, name_t, how_t in PLATFORMS:
        name = name_t.format(v=version)
        digest = sha256(archives / name)
        mib = (archives / name).stat().st_size / 1048576
        rows.append(f"| {label}<br><sub>{detail}</sub> | [`{name}`]({base}/{name}) | {mib:.0f} MiB | `{digest[:12]}…` |")
        sums.append(f"{digest}  {name}")
        start.append(f"- **{label.split(' ', 1)[1]}**: " + how_t.format(v=version, repo=repo))

    out = [f"> {summary}", ""]
    for name, icon in SECTIONS:
        if name in bodies:
            out += [f"## {icon} {name}", "", bodies[name], ""]
    out += [
        "## ⬇️ Download",
        "",
        "Each archive holds the app with its themes, controller mappings and plugins, ready to run. The built-in "
        "compatibility adapters are part of the app itself.",
        "",
        "| Platform | Archive | Size | SHA-256 |",
        "| --- | --- | ---: | --- |",
        *rows,
        "",
        "<details><summary>Full SHA-256 checksums</summary>",
        "",
        "```",
        *sums,
        "```",
        "",
        "</details>",
        "",
        "## 🚀 Getting started",
        "",
        *start,
        "",
        OPTIONAL,
        "",
        "## 📦 Bundled",
        "",
        f"[Plugins](https://github.com/OpenRsG/OpenFadeR-Plugins) `{plugins}` · "
        f"[Themes](https://github.com/OpenRsG/OpenFadeR-Themes) `{themes}` · "
        f"[Mappings](https://github.com/OpenRsG/OpenFadeR-Map) `{mappings}`",
        "",
    ]
    return "\n".join(out)


def main() -> int:
    if len(sys.argv) == 3 and sys.argv[1] == "--check":  # the notes alone, before there are archives
        try:
            check_notes(pathlib.Path(sys.argv[2]).read_text(encoding="utf-8"))
        except (OSError, ValueError) as e:
            print(f"error: {sys.argv[2]}: {e}", file=sys.stderr)
            return 1
        return 0
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("version")
    ap.add_argument("archives", type=pathlib.Path)
    ap.add_argument("notes", type=pathlib.Path)
    ap.add_argument("--repo", default="OpenRsG/OpenFadeR")
    ap.add_argument("--plugins", default="?")
    ap.add_argument("--themes", default="?")
    ap.add_argument("--mappings", default="?")
    a = ap.parse_args()
    if not a.notes.is_file():
        print(f"error: {a.notes} is missing: write the release notes first (docs/releases/README.md)", file=sys.stderr)
        return 1
    try:
        print(build(a.version, a.archives, a.notes.read_text(encoding="utf-8"), a.repo, a.plugins, a.themes, a.mappings))
    except ValueError as e:
        print(f"error: {a.notes}: {e}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
