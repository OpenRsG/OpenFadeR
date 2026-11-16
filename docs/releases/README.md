# Release notes

Every release has a file here named after its version (`0.2.13.md`) with the hand-written part of the notes. The release
workflow refuses to build a release without it, then adds the download table, checksums, getting-started steps, bundled
companions and GitHub's "What's Changed" list. Every release therefore has the same layout.

```markdown
> One sentence that says what this release is about.

## Fixes

- What was wrong and what happens now, with the issue link.
```

Sections (use only these; they are shown in this order whatever the order in the file; none may be empty):
`## New`, `## Fixes`, `## Changes`, `## Docs`, `## Build`, `## Known issues`, `## Please test`.

Check a file before you tag, with the archives of an earlier release in a folder:
`python .github/release_notes.py 0.2.13 <folder> docs/releases/0.2.13.md`. The tests: `python .github/test_release_notes.py -v`.
