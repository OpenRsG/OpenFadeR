#!/usr/bin/env python3
"""Collects the texts marked for translation into openfader-ui.pot and brings each language's .po file up to date.

Marked texts: `@tr("…")` and `@tr("…" | "…" % n)` in ui/**/*.slint; `tr!("…"`, `tr_n!("…", "…"` and `n_!("…")` in
src/app/**/*.rs; and the values of the `choice!` settings in src/app/prefs (shown upper-case by `Row::of`). A .po file keeps
the translations of the texts still used, gets the new texts with an empty translation (shown in English) and loses the
texts no longer used. Run from anywhere: python3 ui/translations/extract.py
"""
import re
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
DOMAIN = "openfader-ui"
LANGS = {
    "fr": ("French", "nplurals=2; plural=(n > 1);"),
    "es": ("Spanish", "nplurals=2; plural=(n != 1);"),
    "de": ("German", "nplurals=2; plural=(n != 1);"),
    "pt": ("Portuguese", "nplurals=2; plural=(n > 1);"),
    "it": ("Italian", "nplurals=2; plural=(n != 1);"),
    "nl": ("Dutch", "nplurals=2; plural=(n != 1);"),
}


def read_string(src, i):
    """The string literal starting at src[i] == '"' (Rust or Slint escapes decoded) and the index after it."""
    assert src[i] == '"'
    out, i = [], i + 1
    while src[i] != '"':
        c = src[i]
        if c == "\\":
            e = src[i + 1]
            if e == "u" and src[i + 2] == "{":
                end = src.index("}", i)
                out.append(chr(int(src[i + 3 : end], 16)))
                i = end + 1
                continue
            if e == "\n":  # Rust line continuation: the newline and the next line's indent are dropped
                i += 2
                while src[i] in " \t\n":
                    i += 1
                continue
            out.append({"n": "\n", "t": "\t", "0": "\0"}.get(e, e))
            i += 2
            continue
        out.append(c)
        i += 1
    return "".join(out), i + 1


def skip_ws(src, i):
    while src[i] in " \t\r\n":
        i += 1
    return i


def line_of(src, i):
    return src.count("\n", 0, i) + 1


def collect():
    """msgid -> [plural or None, references], in order of first use."""
    found = {}

    def add(one, other, path, src, at):
        if not one.strip() or "//" in src[src.rfind("\n", 0, at) + 1 : at]:  # empty, or an example in a comment
            return
        e = found.setdefault(one, [None, []])
        e[0] = e[0] or other
        e[1].append(f"{path.relative_to(ROOT)}:{line_of(src, at)}")

    for path in sorted((ROOT / "ui").rglob("*.slint")):
        src = path.read_text(encoding="utf-8")
        for m in re.finditer(r"@tr\(", src):
            i = skip_ws(src, m.end())
            if src[i] != '"':
                continue
            one, i = read_string(src, i)
            i = skip_ws(src, i)
            if src.startswith("=>", i):  # a context: the text follows
                i = skip_ws(src, i + 2)
                one, i = read_string(src, i)
                i = skip_ws(src, i)
            other = None
            if src[i] == "|":
                other, i = read_string(src, skip_ws(src, i + 1))
            add(one, other, path, src, m.start())
    for path in sorted((ROOT / "src" / "app").rglob("*.rs")):
        src = path.read_text(encoding="utf-8")
        for m in re.finditer(r"\b(tr|tr_n|n_)!\(", src):
            i = skip_ws(src, m.end())
            if src[i] != '"':
                continue
            one, i = read_string(src, i)
            other = None
            if m.group(1) == "tr_n":
                i = skip_ws(src, i)
                if src[i] == ",":
                    other, i = read_string(src, skip_ws(src, i + 1))
            add(one, other, path, src, m.start())
        if "prefs" in path.parts:
            for block in re.finditer(r"choice!\(([\s\S]*?)\n\);", src):
                for v in re.finditer(r'\w+\s*=\s*"([^"]*)"', block.group(1)):
                    add(v.group(1).upper(), None, path, src, block.start() + v.start())
    return found


def quote(s):
    s = s.replace("\\", "\\\\").replace('"', '\\"').replace("\t", "\\t")
    if "\n" in s:
        parts = s.split("\n")
        lines = [p + "\\n" for p in parts[:-1]] + ([parts[-1]] if parts[-1] else [])
        return '""\n' + "\n".join(f'"{l}"' for l in lines)
    return f'"{s}"'


def unquote(q):
    return re.sub(r"\\(.)", lambda m: {"n": "\n", "t": "\t"}.get(m.group(1), m.group(1)), q.strip()[1:-1])


def read_po(path):
    """msgid -> list of msgstr (one, or the plural forms), and the header."""
    if not path.exists():
        return {}, None
    entries, header = {}, None
    cur, field = None, None
    for raw in path.read_text(encoding="utf-8").splitlines() + [""]:
        line = raw.strip()
        if line.startswith("msgid "):
            cur, field = {"id": unquote(line[6:]), "str": []}, "id"
        elif line.startswith("msgid_plural"):
            field = None
        elif line.startswith("msgstr"):
            cur["str"].append(unquote(line[line.index('"'):]))
            field = "str"
        elif line.startswith('"') and cur is not None:
            if field == "id":
                cur["id"] += unquote(line)
            elif field == "str":
                cur["str"][-1] += unquote(line)
        elif not line and cur is not None:
            if cur["id"]:
                entries[cur["id"]] = cur["str"]
            else:
                header = cur["str"][0]
            cur, field = None, None
    return entries, header


def entry(msgid, plural, refs, strs):
    out = [f"#: {r}" for r in refs[:3]]
    out.append(f"msgid {quote(msgid)}")
    if plural:
        out.append(f"msgid_plural {quote(plural)}")
        for i in range(2):
            out.append(f"msgstr[{i}] {quote(strs[i] if i < len(strs) else '')}")
    else:
        out.append(f"msgstr {quote(strs[0] if strs else '')}")
    return "\n".join(out)


def header(lang, name, forms):
    return (
        f"Project-Id-Version: OpenFadeR\\n\nLanguage: {lang}\\n\nMIME-Version: 1.0\\n\n"
        f"Content-Type: text/plain; charset=UTF-8\\n\nContent-Transfer-Encoding: 8bit\\n\nPlural-Forms: {forms}\\n\n"
    )


def write(path, head, body):
    hdr = 'msgid ""\nmsgstr ""\n' + "\n".join(f'"{l}"' for l in head.split("\n") if l) + "\n"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(hdr + "\n" + "\n\n".join(body) + "\n", encoding="utf-8")


def main():
    found = collect()
    write(
        HERE / f"{DOMAIN}.pot",
        header("", "", "nplurals=2; plural=(n != 1);"),
        [entry(k, p, refs, []) for k, (p, refs) in found.items()],
    )
    for lang, (name, forms) in LANGS.items():
        path = HERE / lang / "LC_MESSAGES" / f"{DOMAIN}.po"
        old, _ = read_po(path)
        body = [entry(k, p, refs, old.get(k, [])) for k, (p, refs) in found.items()]
        write(path, header(lang, name, forms), body)
        done = sum(1 for k in found if all(old.get(k, [""])) and old.get(k))
        print(f"{lang}: {done} / {len(found)} translated", file=sys.stderr)


if __name__ == "__main__":
    main()
