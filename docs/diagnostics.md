# Logs, crash reports and problem reports

## The log file

Everything OpenFadeR prints to stdout and stderr (also what the sound, MIDI and GPU libraries print) is copied to `logs/openfader.log` in the data folder (`~/.local/share/openfader/logs/`; `~/Library/Application Support/openfader/logs/` on macOS; `%LOCALAPPDATA%\openfader\logs\` on Windows), one line each:

```
2026-10-23 12:00:00.123 [err] audio: the stream stopped
```

`[out]` is stdout, `[err]` stderr, `[diag]` a note of the diagnostics themselves. The terminal output is unchanged. At 1 MiB the file becomes `openfader.log.1` (older ones move up to `.3`, the oldest is deleted), so the logs stay under 4 MiB. The copy is made by a thread that reads a pipe: the audio thread never writes a file.

The log file is raw: it holds the paths of your tracks. The report (below) takes them out; look at the file before you attach it somewhere.

## Crash reports

A panic writes `logs/crash-<YYYYmmdd-HHMMSS>.md` (a [problem report](#the-report-format) with the panic in it). The next start shows **"OpenFadeR closed unexpectedly. Report it?"** with the report, the buttons below and a close button; once closed the file is renamed `crash-….seen.md` and not offered again. The newest ten are kept. A panic on a worker thread that the app survives is also written, and marked seen when the app ends normally.

`OPENFADER_DEBUG_PANIC=1` makes the app panic three seconds after the window opens, to see all this without a bug at hand.

## Help > Report a Problem

Shows the report of the running session. Touch layouts have it among the commands.

- **Copy report**: the whole report goes to the clipboard.
- **Open logs folder**: the folder with `openfader.log` and the crash reports.
- **Open GitHub issue**: copies the report and opens the bug form of `OpenRsG/OpenFadeR` with the title, the labels (`bug`, plus `crash` after a crash), the version and the environment block filled in. The address is kept under 6000 characters (browsers and GitHub refuse longer ones): when the environment block would not fit, its last lines are left out. Paste the report into the form's "Problem report" box.

Paths and passwords are taken out before the report is shown (see below), but read it before you send it.

## The report format

Markdown with fixed headings in this order; every block is fenced as `text`:

| Heading | Content |
| --- | --- |
| `# OpenFadeR problem report` | the title |
| `## Environment` | `key: value` lines |
| `## Error` | `key: value`: `message`, `location`, `thread`; a report of a running session says `message: none` |
| `## Backtrace` | the panic's backtrace, `none` otherwise; paths cut to `src/…`, `library/std/…` or `crate-1.2.3/…` |
| `## Plugins` | `name: version (on)` or `(off)` per plugin found |
| `## Settings` | `key: value` of the settings that differ from the defaults; a path becomes `<path>`, a secret `<redacted>` |
| `## Log` | the last 300 log lines |

Keys of the environment block, in this order (a value is one line; `none` / `unknown` when there is nothing):

```text
report_format: 1                 raised when a heading or a key changes meaning
kind: crash                      crash | manual
area: app/deck                   the first source folder of ours on the panic location or backtrace (src/app/deck/…); "unknown" for a manual report
version: 0.2.5
commit: 4f3be40
build: release                   release | debug
os: Debian GNU/Linux 12, kernel 6.1.0
arch: x86_64
cpu: Intel(R) Core(TM) i5, 8 threads
ram: 15.5 GiB
renderer: OpenGL (femtovg)       or "software (…)" when there is no GPU path
gpu: Mesa Intel(R) UHD (OpenGL 4.6 Mesa 23.2)
controller_ports: Port A; Port B  the MIDI input ports
controller_mapping: Test Controller (258 bindings)   or none
audio_device: …                  the output device in use
audio_source: settings           OPENFADER_OUTPUT_DEVICE | controller | settings | system default
sample_rate: 48000
buffer: 512 frames               or automatic
channels: 2
audio_notes: …                   why a setting was not honoured
time: 2026-10-23T12:00:00+02:00
uptime: 342 s
```

A program reads a report by finding `## <heading>` and the fenced block under it: `openfader::diag::block(report, "Environment")` does that and returns the pairs. To find the failing area start with `area`, then `location` and `Backtrace`, and read the end of `Log`.

`ram` uses native physical capacity capped by the current Linux cgroup v2 limit, when present. It is the effective
device budget, not the RAM currently used by the process; the top-right RAM readout shows process resident usage.

### What is taken out

A word that starts with `/`, `~/`, `X:\` or `\\` is a path and becomes `<path>` (a quoted one up to its closing quote; an unquoted one with spaces keeps the part after the first space). The value of a `key=value` whose key names a token, secret, password or API key, the word after `Bearer` / `token` / `password` / `secret`, the password of a `scheme://user:pass@host` address, words that start like a GitHub, OpenAI, Slack or AWS key, and any 32+ character word of letters and digits (a full hash too) become `<redacted>`. Your home folder reads `~`.

## Controller problems

The issue form **Controller problem** asks for the controller, the OS, the mapping file, what fails and the MIDI monitor log: start the app from a terminal with `OPENFADER_MIDI_LOG=1` and every incoming message is printed with the action it hit (or `unmapped`); those lines are also in the log file.

## macOS menu

The window's menu bar becomes the native one (Slint builds it with `muda`). Slint already adds the application menu with About, Services, Hide, Hide Others, Show All and Quit (About shows the standard panel with the name, icon and version from the app bundle's `Info.plist`). It has no Settings entry and cannot be extended from the app, so **File > Preferences…** carries the shortcut Cmd+, (the shortcut is written as `Control + ","`, which Slint turns into Cmd on macOS and Ctrl elsewhere).
