#!/usr/bin/env python3
"""Reads the stick `cargo run --example fixture` wrote with readers that are not ours and fails on any difference from the
manifest it wrote next to it (titles, counts, cues, playlists):

* `export.pdb` (Device Library): a small DeviceSQL reader below, written from the public crate-digger layout, and, when
  `--rekordcrate PATH` is given, the `rekordcrate` command line (`list-playlists`, `dump-pdb`);
* the analysis files (`ANLZ0000.DAT`, `.EXT`, `.2EX`) and `exportLibrary.db` (OneLibrary) with pyrekordbox.

Usage: check_stick.py <folder holding stick/ and manifest.json> [--rekordcrate PATH]
"""
import argparse
import json
import pathlib
import struct
import subprocess
import sys

HEAP = 0x28  # page header size; row offsets count from its end


def u16(d, o):
    return struct.unpack_from("<H", d, o)[0]


def u32(d, o):
    return struct.unpack_from("<I", d, o)[0]


def string_at(d, p):
    """DeviceSQL string: short ASCII (odd length byte), 0x40 long ASCII, 0x90 UTF-16LE."""
    kind = d[p]
    if kind in (0x40, 0x90):
        n = u16(d, p + 1) - 4
        body = d[p + 4 : p + 4 + n]
        return body.decode("utf-16-le" if kind == 0x90 else "ascii")
    assert kind & 1, f"unknown string kind {kind:#x} at {p}"
    return d[p + 1 : p + (kind >> 1)].decode("ascii")


def rows(d, ty):
    """Offsets of the rows of the first table of type `ty`, page by page."""
    page_len, tables = u32(d, 4), u32(d, 8)
    assert len(d) % page_len == 0, "export.pdb is not a whole number of pages"
    out = []
    for t in range(tables):
        e = 28 + 16 * t
        if u32(d, e) != ty:
            continue
        page, last = u32(d, e + 8), u32(d, e + 12)
        for _ in range(len(d) // page_len + 1):
            start = page * page_len
            pg = d[start : start + page_len]
            assert len(pg) == page_len, "page out of file"
            if u32(pg, 8) != ty:
                break
            if pg[0x1B] & 0x40 == 0:  # data page (index pages carry 0x40)
                n = u32(pg, 0x18) & 0x1FFF
                for i in range(n):
                    base = page_len - (i // 16) * 0x24
                    if u16(pg, base - 4) >> (i % 16) & 1:
                        out.append(start + HEAP + u16(pg, base - 6 - 2 * (i % 16)))
            if page == last:
                break
            page = u32(pg, 0x0C)
    return out


def read_pdb(path):
    d = path.read_bytes()
    tracks = []
    for r in rows(d, 0):
        s = lambda i: string_at(d, r + u16(d, r + 0x5E + 2 * i))
        tracks.append(
            dict(
                id=u32(d, r + 0x48),
                title=s(17),
                analyze=s(14),
                filename=s(19),
                path=s(20),
                size=u32(d, r + 0x10),
            )
        )
    nodes = {}
    for r in rows(d, 7):
        nodes[u32(d, r + 12)] = dict(
            id=u32(d, r + 12),
            parent=u32(d, r),
            order=u32(d, r + 8),
            folder=u32(d, r + 16) != 0,
            name=string_at(d, r + 20),
            entries=[],
        )
    for r in rows(d, 8):
        nodes[u32(d, r + 8)]["entries"].append((u32(d, r), u32(d, r + 4)))
    return tracks, nodes


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("folder", type=pathlib.Path)
    ap.add_argument("--rekordcrate", help="path of the rekordcrate command line, to dump export.pdb with it too")
    args = ap.parse_args()
    stick = args.folder / "stick"
    manifest = json.loads((args.folder / "manifest.json").read_text())
    problems = []

    def check(ok, what):
        if not ok:
            problems.append(what)

    want = manifest["tracks"]
    want_titles = [t["title"] for t in want]

    # --- export.pdb, read by the reader above -------------------------------------------------------------------------
    tracks, nodes = read_pdb(stick / "PIONEER/rekordbox/export.pdb")
    check(sorted(t["title"] for t in tracks) == sorted(want_titles), f"pdb titles {sorted(t['title'] for t in tracks)} != {sorted(want_titles)}")
    by_title = {t["title"]: t for t in tracks}
    by_id = {t["id"]: t for t in tracks}
    for t in tracks:
        f = stick / t["path"].lstrip("/")
        check(f.is_file() and f.stat().st_size == t["size"], f"audio of {t['title']!r}: {f} missing or not {t['size']} bytes")
        check((stick / t["analyze"].lstrip("/")).is_file(), f"analysis file of {t['title']!r} missing")
        check(len(t["path"]) <= 255, f"path of {t['title']!r} is longer than a row holds")
    names = {n["name"]: n for n in nodes.values()}
    check(len(nodes) == len(manifest["playlists"]), f"pdb has {len(nodes)} playlists, manifest {len(manifest['playlists'])}")
    for p in manifest["playlists"]:
        n = names.get(p["name"])
        if n is None:
            problems.append(f"pdb has no playlist {p['name']!r}")
            continue
        check(n["folder"] == p["folder"], f"{p['name']!r}: folder flag")
        parent = nodes[n["parent"]]["name"] if n["parent"] else None
        check(parent == p["parent"], f"{p['name']!r}: parent {parent!r} != {p['parent']!r}")
        got = [by_id[t]["title"] for _, t in sorted(n["entries"])]
        check(got == p["tracks"], f"{p['name']!r}: tracks {got} != {p['tracks']}")

    # --- the analysis files, read by pyrekordbox ------------------------------------------------------------------------
    from pyrekordbox.anlz import AnlzFile

    for w in want:
        t = by_title.get(w["title"])
        if t is None:
            continue
        dat = stick / t["analyze"].lstrip("/")
        files = {ext: AnlzFile.parse_file(dat.with_suffix("." + ext)) for ext in ("DAT", "EXT", "2EX")}
        check(files["DAT"].get("PPTH") == t["path"], f"{w['title']!r}: PPTH {files['DAT'].get('PPTH')!r} != {t['path']!r}")
        in_dat = sum(len(tag.content.entries) for tag in files["DAT"].getall_tags("PCOB"))
        in_ext = sum(len(tag.content.entries) for tag in files["EXT"].getall_tags("PCO2"))
        slots = w["cue_slots"]
        in_dat_want = sum(1 for s in slots if s <= 3)  # the .DAT holds hot cues A to C and the memory cues
        check(in_dat == in_dat_want, f"{w['title']!r}: {in_dat} cues in the .DAT, manifest {in_dat_want}")
        check(in_ext == len(slots), f"{w['title']!r}: {in_ext} cues in the .EXT, manifest {len(slots)}")
        beats = len(files["DAT"].get("PQTZ")[0])
        check(beats == w["beats"], f"{w['title']!r}: {beats} beats, manifest {w['beats']}")

    # --- exportLibrary.db, read by pyrekordbox ------------------------------------------------------------------------
    from pyrekordbox.onelibrary import OneLibrary

    db = OneLibrary(stick / "PIONEER/rekordbox/exportLibrary.db")
    content = db.get_content().all()
    check(sorted(c.title for c in content) == sorted(want_titles), "OneLibrary titles differ from the manifest")
    for c in content:
        t = by_title.get(c.title)
        check(t is not None and c.path == t["path"], f"OneLibrary path of {c.title!r}: {c.path!r}")
        check(t is not None and c.analysisDataFilePath == t["analyze"], f"OneLibrary analysis path of {c.title!r}")
    lists = db.get_playlist().all()
    ids = {p.playlist_id: p for p in lists}
    check(len(lists) == len(manifest["playlists"]), f"OneLibrary has {len(lists)} playlists")
    entries = {}
    for e in db.get_playlist_content().all():
        entries.setdefault(e.playlist_id, []).append((e.sequenceNo, e.content_id))
    title_of = {c.content_id: c.title for c in content}
    for p in manifest["playlists"]:
        found = [x for x in lists if x.name == p["name"]]
        if len(found) != 1:
            problems.append(f"OneLibrary has {len(found)} playlists called {p['name']!r}")
            continue
        x = found[0]
        check((x.attribute == 1) == p["folder"], f"OneLibrary {p['name']!r}: folder flag")
        parent = ids[x.playlist_id_parent].name if x.playlist_id_parent else None
        check(parent == p["parent"], f"OneLibrary {p['name']!r}: parent {parent!r}")
        got = [title_of[c] for _, c in sorted(entries.get(x.playlist_id, []))]
        check(got == p["tracks"], f"OneLibrary {p['name']!r}: tracks {got}")
    prop = db.get_property().all()[0]
    check(prop.numberOfContents == len(want), f"OneLibrary numberOfContents {prop.numberOfContents}")

    # --- export.pdb, read by rekordcrate ------------------------------------------------------------------------------
    if args.rekordcrate:
        pdb = str(stick / "PIONEER/rekordbox/export.pdb")
        run = lambda *a: subprocess.run([args.rekordcrate, *a], capture_output=True, text=True)
        tree, dump = run("list-playlists", pdb), run("dump-pdb", pdb)
        check(tree.returncode == 0, f"rekordcrate list-playlists failed: {tree.stderr[:300]}")
        check(dump.returncode == 0, f"rekordcrate dump-pdb failed: {dump.stderr[:300]}")
        for p in manifest["playlists"]:
            check(p["name"] in tree.stdout, f"rekordcrate does not list playlist {p['name']!r}")
        for title in want_titles:
            check(f'DeviceSQLString("{title}")' in dump.stdout, f"rekordcrate does not dump track {title!r}")

    if problems:
        print(f"{len(problems)} difference(s) between the stick and the manifest:")
        for p in problems:
            print(" -", p)
        return 1
    print(
        f"ok: {len(want)} tracks, {len(manifest['playlists'])} playlists, {sum(len(w['cue_slots']) for w in want)} cues agree in"
        f" export.pdb{' (and rekordcrate)' if args.rekordcrate else ''}, the analysis files and exportLibrary.db"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
