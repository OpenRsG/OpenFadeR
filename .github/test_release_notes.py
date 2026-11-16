"""Tests of release_notes.py: python .github/test_release_notes.py -v"""
import hashlib
import pathlib
import sys
import tempfile
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).parent))
import release_notes as rn

NOTES = """> A short summary.

## Fixes

- One fix.

## New

- One feature.
"""


def archives(root: pathlib.Path, version: str, skip: str = "") -> None:
    for _, _, name, _ in rn.PLATFORMS:
        name = name.format(v=version)
        if name != skip:
            (root / name).write_bytes(name.encode() * 1000)


class ReleaseNotes(unittest.TestCase):
    def build(self, notes=NOTES, skip="", extra=""):
        with tempfile.TemporaryDirectory() as d:
            root = pathlib.Path(d)
            archives(root, "1.2.3", skip)
            if extra:
                (root / extra).write_bytes(b"x")
            return rn.build("1.2.3", root, notes, "o/r", "v1", "t1", "m1")

    def test_sections_follow_the_fixed_order_whatever_the_file_order(self):
        body = self.build()
        self.assertLess(body.index("## ✨ New"), body.index("## 🐛 Fixes"))
        self.assertLess(body.index("## 🐛 Fixes"), body.index("## ⬇️ Download"))

    def test_table_has_the_real_size_and_checksum_of_each_archive(self):
        body = self.build()
        name = "openfader-1.2.3-linux-x86_64.tar.gz"
        digest = hashlib.sha256(name.encode() * 1000).hexdigest()
        self.assertIn(f"{digest}  {name}", body)
        self.assertIn(f"`{digest[:12]}…`", body)
        self.assertIn("https://github.com/o/r/releases/download/v1.2.3/" + name, body)

    def test_a_missing_or_extra_archive_stops_the_release(self):
        with self.assertRaisesRegex(ValueError, "missing"):
            self.build(skip="openfader-1.2.3-windows-x86_64.zip")
        with self.assertRaisesRegex(ValueError, "unexpected"):
            self.build(extra="stray.zip")

    def test_notes_that_break_the_format_are_refused(self):
        for notes, why in [
            ("## Fixes\n\n- x\n", "summary"),
            ("> s\n\n## Bugs\n\n- x\n", "unknown section"),
            ("> s\n\n## Fixes\n\n- x\n\n## Fixes\n\n- y\n", "twice"),
            ("> s\n\n## Fixes\n", "empty section"),
            ("> s\n", "no section"),
            ("> s\n\nstray text\n\n## Fixes\n\n- x\n", "between"),
        ]:
            with self.subTest(why=why), self.assertRaisesRegex(ValueError, why):
                self.build(notes=notes)

    def test_bundled_companions_are_named(self):
        self.assertIn("`v1`", self.build())


if __name__ == "__main__":
    unittest.main()
