# Plugins

A plugin adds to OpenFadeR without touching its code:

| Feature | How | Where it shows |
|---|---|---|
| **Track sources** (YouTube, SoundCloud, a web radio, your NAS catalogue, …) | a script (Lua, run inside the app, or Python) or any program answering JSON requests | one folder per source in the browser tree (Library), with playlists and a search entry |
| **Actions** | the same script | the **Plugins** menu of the menu bar |
| **Event hooks** (features: logs, overlays, automations) | the same script | nothing, or whatever the reply's commands do: the plugin is told when a track loads, a deck plays or pauses, the master changes, a recording starts or stops, every few seconds … |
| **Effects** for six places in the mixer: Beat FX, COLOR FX, MIC FX, SEND FX, LEVER FX, channel fader curves | a chain of the host's DSP blocks written in JSON (no code, no compiling), or a native library (`.so`) | each place's selector; see [Effects](#effects) and [`sdk/README.md`](../sdk/README.md) |
| **COLOR FX knob modes** (e.g. Center Lock) | two lines of JSON, no code | PREFERENCES > CONTROLLER > Mixer > COLOR FX KNOB |
| **Pad modes** (e.g. Gate Cue) | a list of deck steps in JSON, no code | a mode button of the performance pads |
| **Cover art** (online lookup: MusicBrainz, iTunes, …) | `"artwork": true` and a script answering `artwork` requests | Track > Find Artwork…, Fetch Missing Artwork, the track info; see [Artwork](#artwork) |
| **Track library formats** (a collection file, a media player's library) | a native library (`.so`) built against the `library` interface of `sdk/` | File > Import / Export, the Media Browser, and whatever path the app opens; see [Library plugins](#library-plugins) |
| **Controller drivers** (a controller's own mapping format: Mixxx `.midi.xml` + JS, a manufacturer SDK, …) | a native library (`.so`) built against the `controller` interface of `sdk/` | claims a MIDI port instead of our mapping files; PREFERENCES > CONTROLLER > CONTROLLER PLUGINS; see [Controller plugins](#controller-plugins) |
| **Settings** of a plugin | a list in JSON | the plugin's card in PREFERENCES > EXTENSIONS; every request carries their values |
| **Themes** (not a plugin: one JSON file) | see [`docs/skins.md`](skins.md) | PREFERENCES > THEMES |

A plugin can have sources, actions and events together (they share one program), and effects, knob modes and pad
modes on top.

Working examples in the [plugins repository](https://github.com/OpenRsG/OpenFadeR-Plugins) (checked out as `extension/plugin/`):
[`yt-dlp`](https://github.com/OpenRsG/OpenFadeR-Plugins/tree/main/yt-dlp) (YouTube and SoundCloud search, playlists, downloads;
Python and the `yt-dlp` program), [`now-playing`](https://github.com/OpenRsG/OpenFadeR-Plugins/tree/main/now-playing) (events,
actions and settings; Python), and the DJ effects listed under [Effects](#effects), all JSON
([`tutorial-fx`](https://github.com/OpenRsG/OpenFadeR-Plugins/tree/main/tutorial-fx) is the smallest). In this repository:
[`sdk/examples/`](../sdk/examples) (effects as native libraries, in Rust and C).

## Install and manage

Drop a plugin on the window: its folder, a `.zip` of it (the files at the root of the archive or in one top folder) or its
`plugin.json`. Dragging works from any file manager (Wayland, X11, Windows, macOS); PREFERENCES > EXTENSIONS also has
**INSTALL FILE…** and **INSTALL FOLDER…** (a file chooser: `zenity` or `kdialog`). The plugin is copied into
`~/.config/openfader/extension/plugin/<name>/` (`$XDG_CONFIG_HOME/openfader/extension/plugin`), replacing an older copy of the
same plugin, after it was checked; a broken one changes nothing and says why. The same drop installs a theme (a theme `.json`)
or imports music (audio files and folders).

The app reads plugins from

1. every folder listed in `$OPENFADER_PLUGINS` (`:`-separated),
2. `~/.config/openfader/extension/plugin/`, and
3. the bundled `extension/plugin/`: next to the executable or in a folder above it (a build in the core checkout's `target/`
   finds the plugins checkout `start.sh` cloned there).

An entry of that search path is either one plugin folder or a folder of plugin folders; the first plugin of a name wins.

PREFERENCES > EXTENSIONS lists every plugin as a card. The rekordbox extensions `rekordbox-device`, `rekordbox-xml` and `rekordbox-db`
come first, marked **BUILT IN**: they are compiled into the app ([The built-in rekordbox extensions](#the-built-in-rekordbox-extensions)),
always on, with no switch, folder or uninstall, and their settings work as any plugin's. The other cards have a switch to turn the plugin on or off (a plugin starts off and installing one switches it on; the exception is a fresh
profile with no `plugins.json`, where the reader `engine-dj` and the effect packs `beat-fx`,
`send-fx`, `lever-fx`, `mic-fx`, `soft-mix` and `cross-pass` start on, so a plugged-in Engine DJ stick is read at once;
once the file exists it is your choice and nothing is switched on behind it), what it runs on (LUA, PYTHON, PROGRAM or
JSON), what it brings, its settings (**SETTINGS**), **OPEN FOLDER**, **WEB PAGE** (`homepage`), then whatever went wrong while
reading the plugins. The card's **⋮** menu opens its folder or **uninstalls** it: an installed plugin's folder is deleted, and a
bundled one is hidden (listed under `removed`) until you install it again, from GET PLUGINS or a drop. **RELOAD** reads the folders
again (after you edit a plugin). Actions, events and settings apply at once; effects, sources, knob and pad modes are part of the
mixer and the browser from start-up, so a change to those shows a **RESTART NOW** banner. The switches, the uninstalled plugins and
the settings are kept in `~/.config/openfader/plugins.json`.

A plugin folder named like a built-in extension (an old copy in the plugin folders, or one from GET PLUGINS) is not read: the card
list shows the folder with a note to delete it, and installing a plugin of that name is refused. Built-ins are not offered or updated
by GET PLUGINS either; they change with the app.

When a stick is plugged in whose library only an extension that is off can read (`Engine Library/`:
`engine-dj`, `_Serato_/`: `serato`, `VirtualDJ/`: `virtualdj`, `Traktor/`: `traktor`), the welcome window opens with just that extension
and SWITCH ON turns it on (its library loads at the next start: RESTART NOW). Each extension is offered once per device per run.

### Install from a repository

PREFERENCES > EXTENSIONS > **GET PLUGINS** installs plugins in one click from GitHub repositories. The repository list is one list for
plugins and themes (`~/.config/openfader/repositories.json`, a plain list of `owner/repo`); `OpenRsG/OpenFadeR-Plugins` and
`OpenRsG/OpenFadeR-Themes` are in it on first run and can be removed with their **x**. Type `owner/repo` (or the GitHub address, with
or without `.git`) and **ADD**; **REFRESH** downloads the repositories again (the zips are cached in `~/.cache/openfader/repos/`,
so the page opens without the network). Every plugin found shows as a card (name, version, description, repository) with one button:
**INSTALL**, **UPDATE** (the repository's `version` differs from the installed one) or **INSTALLED**; **INSTALL ALL** does the rest. It
goes through the same checks and the same copy as a drop, then shows the manager's **RESTART NOW** banner when the plugin needs one.

**Plugin updates are announced.** The app's update check (README, Updates: at start, once a day, PREFERENCES > ADVANCED > Check for
updates; also Help > Check for Updates) reads these repositories again and counts the installed plugins whose repository `version`
differs, as the **UPDATE** button does. A notice and the update window name them; GET PLUGINS installs them. So a plugin author only
has to raise `version` in `plugin.json`. Themes and mappings carry no version and are not counted.

What a repository must look like: its default branch holds the plugin folders, each with a `plugin.json`, at the top or at most two
folders deep (`<repo>/<plugin>/plugin.json`, `<repo>/<group>/<plugin>/plugin.json`). The app downloads
`https://github.com/<owner>/<repo>/archive/HEAD.zip`; entries that leave the plugin folder (`..`, absolute paths) or are symlinks
make the install fail.

A plugin with a **native library** (`effects` naming a library, `library.lib` for this OS) is installed as is when the repository holds
the library file. Otherwise publish a release of the repository and attach, per platform, an asset named

```
<plugin name>-<os>-<arch>.zip      os = std::env::consts::OS (linux, windows, macos), arch = std::env::consts::ARCH (x86_64, aarch64, …)
e.g. my-fx-linux-x86_64.zip
```

fetched from `https://github.com/<owner>/<repo>/releases/latest/download/<asset>`. The zip's **root** holds the library file(s),
named and placed as the plugin's `plugin.json` says (`libmy_fx.so` beside `plugin.json`, or `lib/x.so` for `"lib/x.so"`). Without
that asset for the user's platform nothing is installed and the page says `no prebuilt <plugin> for <os>-<arch> yet`.

## plugin.json

```json
{
  "name": "yt-dlp",
  "version": "1.1.0",
  "author": "OpenFadeR",
  "homepage": "https://github.com/yt-dlp/yt-dlp",
  "description": "YouTube and SoundCloud through the yt-dlp program",
  "script": "main.py",
  "settings": [
    {"id": "results", "label": "Results per search", "type": "number", "default": 25}
  ],
  "sources": [
    {"id": "youtube", "name": "YouTube", "search": true}
  ],
  "actions": [
    {"id": "download", "label": "Download from URL…", "input": "Address of a track or playlist"}
  ],
  "events": ["track_loaded", "track_played", "timer"],
  "timer": 10,
  "effects": [{"name": "SQUASH", "slot": "mic", "chain": [{"block": "saturate", "drive": 4}, {"block": "mix"}]}],
  "knob_modes": [{"name": "CENTER LOCK", "stop": 0.5}],
  "pad_modes": [{"name": "GATE CUE", "press": ["jump", "play"], "release": ["pause", "jump"]}]
}
```

| Key | Meaning |
|---|---|
| `name` | Required. Unique, no `:`. Shown in the Plugins menu, PREFS and error messages. |
| `version`, `description`, `author`, `homepage` | Shown on the plugin's card. |
| `suggest` | The software the plugin makes OpenFadeR compatible with (its name). The welcome window, at the first start and from Help > Welcome, offers to switch on every plugin found with one and names that software. |
| `script` | The file serving sources, actions and events, in the plugin folder: `*.lua` runs inside the app ([Lua plugins](#lua-plugins)), `*.py` with `python3` (on Windows the `py` launcher, else `python`), anything else is run as a program. |
| `command` | Instead of `script`: the program and its arguments, started once per request. A relative program or argument that names a file in the plugin folder becomes that file's path (`["node", "main.js"]` runs the plugin's `main.js`). |
| `persistent` | `true`: one process (or one Lua state) serves every request instead of a new one each: no start-up cost per request, and the plugin keeps its state. See [Persistent plugins](#persistent-plugins). |
| `sources` | Track sources. `id` (no `:`, unique in the plugin), `name` (the folder's label), `search` (the source has a search entry). |
| `actions` | Menu entries: `id`, `label`, and `input`, the prompt of a text field asked for before the action runs (leave it out to run at once). |
| `events` | Names of the events the plugin wants to hear (see [Events](#events)). |
| `timer` | Seconds between two `timer` events (with `timer` in `events`). |
| `settings` | What the user can set on the plugin's card (see [Settings](#settings)). |
| `effects` | Effects: a list of effects declared in JSON (`name`, `slot`, `chain` of blocks, `lfo`s) or the path of a native effects library, relative to the plugin folder (see [Effects](#effects)). |
| `library` | A native library plugin for a format of track libraries (see [Library plugins](#library-plugins)). |
| `knob_modes` | COLOR FX knob behaviours (see [Knob modes](#knob-modes)). |
| `pad_modes` | Performance-pad modes (see [Pad modes](#pad-modes)). |
| `automix` | Declarative beat-timed transition styles (see [Automix styles](#automix-styles)). |

### Languages

The app shows a plugin's texts in its UI language (PREFERENCES > VIEW > LANGUAGE) when the plugin folder holds
`locales/<lang>.json` for it (`fr`, `es`, `de`, `pt`, `it`, `nl`): one object, English text → translation.

```json
{
  "YouTube and SoundCloud: search, playlists and downloads through the yt-dlp program": "YouTube et SoundCloud : recherche, playlists et téléchargements avec le programme yt-dlp",
  "Download from URL…": "Télécharger depuis une URL…",
  "{} tracks downloaded": "{} pistes téléchargées"
}
```

* Translated where they are shown: `description`, the settings' `label`, `help` and `choices` (the stored value stays the
  English one), `sources` names, `actions` `label` and `input`, `library.browser.label`, `collection_writer`, the `"error"` of
  any reply, the `text` of a `toast`, the `kind` of a library, and the texts of the device replies (`title`, `about`, `label`,
  `help`, `hint`, `message`, `text`, `yes`, `warnings`, `notes`, `apply_label`, `read_label`).
* A key with `{}` matches a text built at run time; each `{}` stands for at least one character, and the translation takes
  the values in order, or by position with `{0}`, `{1}` (`"{} of {} done": "{1} : {0} terminés"`). The most specific key wins.
* `name`, effect, knob-mode and pad-mode names are not translated (like the app's own DJ labels).
* A text the file lacks stays English; a broken file is reported on stderr and ignored.
* Every request also carries the language as `"lang"` (`"en"`, `"fr"` …), for a plugin that writes its texts itself.

## The protocol

A plugin's script answers **requests**: one JSON object in, one JSON object out. A Python script or program (`script: "main.py"`
or `command`) is started as a new process in the plugin folder per request, reads the request on stdin (then closed) and writes
its reply on stdout; whatever it writes to stderr appears on the app's stderr: print debug output there. A Lua script runs
inside the app and answers through a function instead ([Lua plugins](#lua-plugins)); the requests and replies are the same.

* The request always has `"op"`, the kind of request, and `"settings"`, the plugin's [settings](#settings) (`{}` without any).
* A reply `{"error": "text"}` is a failure: the app shows the text (a toast; the plugin's name in front) and goes on. Use it for
  everything that can go wrong at run time (no network, bad address, tool missing). An exit code other than 0, output that is not
  one JSON object, or no reply in time (the process is killed, a Lua script stopped) fail the same way.
* Unknown keys in a reply are ignored, so a newer plugin works with an older app.
* Requests never wait for each other and the app never waits for a request: a slow plugin makes its folder fill in late, nothing
  else. Several requests of one plugin may run at the same time, so do not keep state in files without care.
* Timeouts: playlist lists, track lists and searches 2 minutes, downloads and actions 15 minutes, events 30 seconds.

### Persistent plugins

With `"persistent": true` the process starts at the plugin's first request and stays: a request costs a line written and a line
read, well under a millisecond, instead of a program start. It reads one request per line on stdin and writes one reply per line
on stdout (one JSON object per line, no line breaks inside).

* Every request carries an `"id"` number; the reply must carry the same `id`. Replies may come in any order, and requests may
  overlap: answer them as they come or one by one, as you like.
* A line without `id` is the plugin's own message at any time: its `commands` (as an action's reply) run at once. This is how a
  plugin reacts to something it watches (a network peer, a file, a timer of its own) without waiting for a request.
* **Exit when stdin closes** (end of file). That is how the app quits the plugin, and on macOS the only way the plugin learns
  that the app has ended. At quit, or when the plugin is switched off, a plugin still running a second after its stdin closed is
  killed.
* A request without a reply in time kills the process; the next request starts a new one. So does a change of the plugin's
  folder, `command` or `version`.
* The app ends its plugin processes however it ends: on Linux the kernel kills them when the app dies (even on a crash), on
  Windows they live in a job the system closes with the app. A normal quit or a switch-off also ends what the plugin started
  (its process group, its job). After a crash on Linux or macOS, children of the plugin's own must end themselves.

```python
import json, sys
count = 0
for line in sys.stdin:  # ends when the app closes stdin
    req = json.loads(line)
    count += 1
    print(json.dumps({"id": req["id"], "commands": [{"cmd": "toast", "text": f"request {count}"}]}), flush=True)
```

A persistent Lua plugin keeps one Lua state: globals survive between requests (the script is loaded again when its file or the
plugin's `version` changes), and its requests take turns instead of running at the same time.

You can test a Python plugin without the app:

```sh
echo '{"op":"search","source":"youtube","query":"daft punk","settings":{}}' | python3 extension/plugin/yt-dlp/main.py
```

### Lua plugins

`"script": "main.lua"` runs the script inside the app (Lua 5.4, nothing to install), a fresh Lua state per request (one kept
state with [`persistent`](#persistent-plugins)). The script
defines `handle(request)`, as a global or in the table it returns, which gets the request as a Lua table and returns the reply
table (`nil` = `{}`); `error("…")` fails the request like an `error` reply. `require("util")` loads `util.lua` of the plugin folder.
The standard library is Lua's safe one (no `debug`, no C modules); the `rk` table adds:

| `rk.` | Does |
|---|---|
| `name`, `dir`, `data_dir`, `os` | The plugin's name, its folder, `<data dir>/plugins/<name>` (created for you), `"linux"` / `"windows"` / `"macos"`. |
| `log(…)` | Writes to the app's stderr with the plugin's name in front; `print` does the same. |
| `json.encode(value)`, `json.decode(text)` | JSON text ↔ Lua values. |
| `http.get(url [, headers])`, `http.post(url, body [, headers])` | `body, status`; an HTTP error status is returned, not raised. `headers` is `{["Name"] = "value"}`. |
| `http.download(url, path)` | Writes the response body to `path` as it is (audio for a source's `fetch`); returns the status. |
| `run(program, {args…})` | Runs a program in the plugin folder: `stdout, exit code`. |
| `fs.read(path)`, `fs.write(path, text)`, `fs.exists(path)`, `fs.list(dir)`, `fs.mkdir(path)` | Files; a relative path is in the plugin folder. `read` gives `nil` for a missing file. |
| `sleep(ms)` | Waits (at most a minute). |

```lua
-- plugin.json: {"name": "hello", "script": "main.lua", "actions": [{"id": "hi", "label": "Say hi"}],
--               "settings": [{"id": "who", "label": "Name", "default": "DJ"}]}
local M = {}
function M.handle(req)
  if req.op == "action" then
    return {commands = {{cmd = "toast", text = "Hello " .. req.settings.who}}}
  end
  return {}
end
return M
```

The plugins repository has no Lua example any more (the mix log is built into History: SAVE SET LIST); the protocol above is all of it.

### Settings

`"settings"` lists what the user sets on the plugin's card in PREFERENCES > EXTENSIONS; every request carries the values as
`"settings": {"id": value}` (the default until the user changes one), and the app keeps them in `~/.config/openfader/plugins.json`.

```json
"settings": [
  {"id": "folder", "label": "Folder", "type": "text", "default": "", "help": "Where the files go"},
  {"id": "results", "label": "Results", "type": "number", "default": 25},
  {"id": "format", "label": "Format", "type": "choice", "choices": ["csv", "txt"]},
  {"id": "announce", "label": "Toast each track", "type": "bool", "default": false}
]
```

`type` is `text` (the default), `secret` (text that is shown masked: keys and passwords; kept in `plugins.json` as plain text, so
it is as private as that file), `number`, `bool` or `choice` (`choices` required; the default is the first). `help` is a line under
the setting. A plugin may change its own settings with the `setting` command.

### Sources

A source is a folder in the tree under Library (read-only), before the Histories folder. Its entries, top to bottom: the search
entry (when `search` is true), then the playlists the plugin lists. Opening an entry asks the plugin; the first time the list is
empty, then it fills in (the app keeps what the plugin answered until restart; a failed request is asked again the next time the
list is opened, except the list of playlists).

**`playlists`**: what to list under the source.

```json
{"op": "playlists", "source": "youtube"}
→ {"playlists": [
     {"id": "mixes", "name": "Mixes", "folder": true},
     {"id": "https://…/playlist?list=PL1", "name": "Warm up", "parent": "mixes"}
   ]}
```

`id` and `name` are required; `folder: true` makes a folder (it lists no tracks); `parent` is the `id` of a folder listed in the
same reply (without it, or with an unknown one, the entry is at the top). Folders nest up to 8 levels. A source with nothing to
list (search only) answers `{"playlists": []}`.

**`tracks`**: the tracks of one playlist.

```json
{"op": "tracks", "source": "youtube", "playlist": "https://…/playlist?list=PL1"}
→ {"tracks": [{"id": "K0HSD_i2DvA", "title": "Around the World", "artist": "Daft Punk", "length": 242}]}
```

**`search`**: the search entry; it is asked when the user presses Enter in the browser's search box (the box then searches
online and does not filter the rows).

```json
{"op": "search", "source": "youtube", "query": "daft punk around the world"}
→ {"tracks": [ … ]}
```

**Track objects** (in `tracks` and `search` replies). A track needs an `id` (to download it later) or a `file` (it is local), or both.

| Key | Meaning |
|---|---|
| `id` | The plugin's own id of the track, a string or a number. Handed back in `fetch`. |
| `title`, `artist`, `album`, `genre`, `key` | Text. `key` is a musical key as you like to show it ("8A", "Am"). `title` defaults to the `id`. |
| `bpm` | Number. |
| `length` | Seconds, a number. |
| `url` | Web page of the track; shown in the Comment column. |
| `file` | Path of an audio file that exists on this machine (relative paths start in the plugin folder). The track then loads without a download. |
| `artwork` | Path of a cover image on this machine. |

Rows with neither `id` nor `file` are dropped. The tracks are the browser's usual rows: sort, FILTER, add to the Tag List, AUTOMIX.
They are not in the Collection and have no file until they are loaded (Track > Import To Collection says "this track has no file");
an action can add a file with the `import` command.

**`fetch`**: the audio of a track that has no `file`. The app asks when the user loads the track on a deck (double click, LOAD,
AUTOMIX, Continuous Playing): the deck shows "downloading…", then the track loads like any other.

```json
{"op": "fetch", "source": "youtube", "id": "K0HSD_i2DvA",
 "dir": "/home/me/.local/share/openfader/plugins/yt-dlp/youtube", "name": "K0HSD_i2DvA"}
→ {"file": "K0HSD_i2DvA.m4a"}
```

Write the audio into `dir` as `<name>.<extension>` and answer its path (relative to `dir`, or absolute). Rules:

* The audio must be a format the decks play: **mp3, AAC (m4a), FLAC or WAV**. Convert what you download (ffmpeg) and fail with a
  clear `error` when you cannot.
* `name` is the track's id made safe for a file name; keep it as the file's name without extension. The app looks for
  `<name>.*` in `dir` before asking, so a track that was fetched once is **never fetched again**, also in the next run. A file
  with the extension `part`, `tmp` or `ytdl`, or an empty one, does not count.
* Make the file appear complete: download into a temporary folder and rename it into `dir` at the end.
* `dir` is `<data dir>/plugins/<plugin>/<source>/`; the data dir is `$XDG_DATA_HOME/openfader` (else `~/.local/share/openfader`).
  Deleting files there is safe, they are fetched again.

### Actions

Every action of every plugin is an entry of the **Plugins** menu ("plugin: label"; the menu is missing when no plugin has an
action). An action with `input` first asks for a line of text (the `input` value is the prompt).

```json
{"op": "action", "action": "download", "input": "https://soundcloud.com/…",
 "context": {
   "decks": [{"deck": 1, "playing": true, "track": {"title": "…", "artist": "…", "file": "/m/a.mp3", "bpm": 128, "key": "8A"}},
             {"deck": 2, "playing": false, "track": null}],
   "selected": {"title": "…"},
   "dir": "/home/me/.local/share/openfader/plugins/yt-dlp"
 }}
→ {"commands": [{"cmd": "toast", "text": "Done"}]}
```

`input` is `null` for an action without a prompt. The **context** is what the app knows at that moment:

* `decks`: the decks in use (2 or 4), deck numbers from 1. `track` is `null` for an empty deck; a loaded track has `title`, `artist`,
  `album`, `genre`, `key`, `bpm`, `length` (seconds), `file` (`null` when there is none) and `source` (`{plugin, source, id}` for a
  plugin track, else `null`).
* `selected`: the track chosen in the browser, same fields, or `null`.
* `dir`: a folder of your own to write to (create it yourself): `<data dir>/plugins/<plugin>/`.

`commands` is the list of things the app does when the action has finished (leave it out to do nothing):

| Command | Does |
|---|---|
| `{"cmd": "toast", "text": "…"}` | Shows a notification and the text in the status line. |
| `{"cmd": "load", "deck": 2, "file": "/m/a.mp3"}` | Loads an audio file on deck 2 (numbers from 1; an eject-locked deck refuses). |
| `{"cmd": "import", "files": ["/m/a.mp3", "/m/folder"]}` | Adds files (or whole folders) to the Collection, like File > Import (Auto Analysis applies). |
| `{"cmd": "play", "deck": 1}`, `{"cmd": "pause", "deck": 1}` | Plays or pauses the deck (nothing when it already does). |
| `{"cmd": "seek", "deck": 1, "ms": 30000}` | Moves the playhead (NEEDLE LOCK applies). |
| `{"cmd": "tempo", "deck": 1, "percent": -2.5}` | Sets the deck's tempo change. |
| `{"cmd": "open", "url": "https://…"}` | Opens a web page, file or folder in the system's program for it. |
| `{"cmd": "setting", "id": "count", "value": 3}` | Stores one of the plugin's own settings. |
| `{"cmd": "key", "action": "deck.play_pause.A"}` | Runs a row of the keyboard shortcut table as if its key was pressed. |

A command the app cannot read (unknown `cmd`, a missing field) is reported with a toast and skipped; the others still run.
Ids of `key`: `app.<name>` or `deck.<name>.<A-D>`. Names: deck actions `play_pause`, `cue`, `load`, `sync`, `master`, `pad`
(the first hot-cue pad), `auto_loop`, `export_loop`, `jump_back`, `jump_fwd`, `loop_in`, `loop_out`, `reloop`, `key_sync`,
`key_down`, `key_up`, `key_reset`, `slip`, `reverse`, `brake`, `pad_mode`, `grid_toggle`, `grid_tap`, `grid_beat1`, `grid_back`,
`grid_fwd`; app actions `deck_layout`, `tag_track`, `sampler_toggle`, `automix_toggle`, `automix_skip`, `fx_beat_down`,
`fx_beat_up`, `fx_toggle`, `browse_view`, `browse_area`, `undo`, `redo`, `redo_alt`, `focus_search`, `help`. Only rows the shortcut table has exist (the `?`
overlay lists them; `deck.cue.A` and `deck.cue.B` do, `deck.cue.C` too); an id that matches no row is reported. Which key a row
is bound to does not matter, a cleared binding still runs.

### Artwork

A plugin with `"artwork": true` (and a `script` or `command`) is asked for cover pictures. The app asks every enabled artwork
plugin, one request at a time, and at least a second apart for one plugin.

```json
{"op": "artwork", "artist": "Daft Punk", "album": "Discovery", "title": "One More Time", "duration": 320, "settings": {}}
→ {"candidates": [{"url": "https://…/front.jpg", "thumb": "https://…/front-250.jpg", "size": 1000, "source": "MusicBrainz"}]}
```

| Key | Meaning |
|---|---|
| `artist`, `album`, `title` | The track's tags (empty when it has none). |
| `duration` | Whole seconds, 0 when unknown. |
| `url` | An `http` or `https` address of the picture. |
| `data` | Instead of `url`: the picture itself, base64 (PNG, JPEG, GIF, WebP, BMP). |
| `thumb` | Optional address of a small picture for the picker. |
| `size` | Optional pixels on the longer side, shown in the picker. |
| `source` | Optional label shown under the picture; the plugin's name when left out. |

Candidates without a usable `url` or `data` are dropped and at most eight are taken from one plugin, best first. A plugin that
fails (no network) only reports its error; a lookup that every plugin answered with nothing is remembered for 30 days.

**Find Artwork…** (Track menu, right-click menu, FIND ARTWORK in the track info) shows the candidates with thumbnails; a click
keeps one, for the track's whole album (one undo step). **Fetch Missing Artwork** does that for every album of the selected
tracks (of the list on screen when at most one track is selected) that has no cover, taking the first candidate. A folder image beside the file (`cover.jpg`, `folder.jpg`, `front.png`, …) counts
without any plugin. With PREFERENCES > ADVANCED > BROWSE > Write fetched artwork into the file's tags the picture is also
written into the tags of the album's tracks (MP3, WAV, AIFF with an ID3v2 `APIC` frame, FLAC with a `PICTURE` block; off by default). The pictures are shown in the browser's
Cover column, on the decks and in the track info.

[`cover-art`](https://github.com/OpenRsG/OpenFadeR-Plugins/tree/main/cover-art) (Python) answers from MusicBrainz with the Cover
Art Archive and from the iTunes Search API; with a Discogs personal access token in its settings it asks Discogs too.

### Events

A plugin that lists an event in `events` is told when it happens. Every event is one request of its own, so a slow plugin delays
nothing and two events may overlap. The reply may carry `commands` (as an action's), which the app runs: a plugin can react to the
mix (load the next track when one ends, pause a deck, change tempo, …). A failure goes to stderr.

```json
{"op": "event", "event": "track_loaded", "data": {"deck": 1, "track": {"title": "…", "artist": "…", "file": "/m/a.mp3"}}, "settings": {}}
→ {}
```

| Event | When | `data` |
|---|---|---|
| `app_started`, `app_quit` | The window opened / closed (the app waits for nobody on quit) | `{}` |
| `track_loaded` | A track is on a deck (a plugin track once its download is done) | `{"deck": 1, "track": {…}}` |
| `track_unloaded` | A deck was emptied | `{"deck": 1}` |
| `deck_play`, `deck_pause` | A deck starts or stops playing | `{"deck": 1, "track": {…}, "position_ms": 61234.5}` |
| `track_ended` | A deck stopped at the end of its track | `{"deck": 1, "track": {…}}` |
| `track_played` | A track stayed on air for the time the play history needs (the entry in History > today) | `{"track": {…}}` |
| `master_changed` | Another deck became the tempo master (0 = none) | `{"deck": 2}` |
| `recording_started`, `recording_stopped` | REC started / stopped (the files it wrote, and the cue sheets, tracklists and control log written next to them) | `{}` / `{"files": ["/m/rec.wav"], "sidecars": ["/m/rec.cue", "/m/rec.tracklist.txt"]}` |
| `recording_track` | During REC, another deck took over the mix (the master while it is on air, else the first deck on air); the moment is the recorder's clock, the same time the cue sheet gets | `{"deck": 2, "position_ms": 61234, "track": {…}}` |
| `recording_started`, `recording_stopped` | REC started / stopped (the files it wrote) | `{}` / `{"files": ["/m/rec.wav"]}` |
| `inbox_added` | New music from a watched folder joined INBOX (Preferences > Advanced > Database > Inbox): a plugin can tag it, convert it, tell a phone … | `{"files": ["/m/Downloads/a.mp3"]}` |
| `inbox_moved` | Tracks moved to another stage (INBOX → REVIEW → COLLECTION, by drag or MORE); `files` are where the files are now | `{"stage": "REVIEW", "files": ["/m/Review/a.mp3"]}` |
| `timer` | Every `timer` seconds of the manifest | `{}` |

`track` has the fields listed under Actions.

## Effects

An effect joins one **slot** of the mixer. There are two ways to bring one: **declare it in JSON**, as a chain of the host's own
DSP blocks (delay, filters, reverb, pitch shifter, loops …) steered by small formulas, with no code and no compiling; or build
a **native library** (see [Native libraries](#native-libraries)). Every effect names its slot:

| Slot | Where you pick it | What the effect gets and gives |
|---|---|---|
| `beat` | FX panel, BEAT tab, after the built-in effects | a channel (CH 1 to 4, or the AUX input's channel, see the README) or the master; LEVEL, beat, ON |
| `color` | COLOR FX type chips, after FILTER … CRUSH | a channel after its EQ; the COLOR knob as -1 to 1 |
| `mic` | FX panel, MIC tab | the microphone; LEVEL, PARAM, ON |
| `send` | FX panel, SEND tab | the sum of each channel's SEND knob; gives the return, added to the master |
| `lever` | FX panel, LEVER tab | the master; the lever, 0 at rest |
| `fader` | PREFERENCES > CONTROLLER > Mixer > CHANNEL FADER CURVE | a channel before its fader; the fader position; it applies the fader gain |

The exact contract (inputs, outputs, the `knob` and `level` of each slot, trails, the engage ramp) is the `Slot` documentation
in [`sdk/src/lib.rs`](../sdk/src/lib.rs). A name is 1 to 16 characters of ASCII letters, digits, space, `-` and `_`, and unique
within its slot (the slot's built-in names included, ignoring case), because choices are saved by name.

### Effects in JSON

`"effects"` is a list; each entry has a `name`, a `slot`, a `chain` of blocks and optionally `lfo`s:

```json
{
  "name": "my-fx",
  "effects": [
    {
      "name": "WOBBLE ECHO",
      "slot": "beat",
      "lfo": [{"name": "wob", "period": "period * 2", "shape": "sine"}],
      "chain": [
        {"block": "echo", "time": "period + 2ms * wob", "feedback": 0.55, "damp": 3500}
      ]
    },
    {
      "name": "SQUASH",
      "slot": "mic",
      "chain": [
        {"block": "saturate", "drive": "1 + 9 * knob"},
        {"block": "gain", "gain": "1 / sqrt(1 + 9 * knob)"},
        {"block": "mix"}
      ]
    }
  ]
}
```

The signal enters the first block as the slot's input frame (stereo) and each block hands its output to the next; the last
block's output is the effect's output. A block that makes only the *wet* part (a `delay` returns just the delayed signal)
is followed by `mix` to put it against the dry signal, which is the effect's own input whatever happens in between.
`echo` and `reverb` do that for you.

Everything is checked when the plugin loads: an unknown block, a misspelt parameter or a bad formula is reported in
PREFERENCES > EXTENSIONS as `plugin: beat effect "NAME": chain[2] (delay): "time": …`, and only that effect is skipped. Nothing
is parsed, allocated or locked on the audio thread; a chain whose output ever turns non-finite is cleared and silent for that
frame.

Conventions of the built-in effects, which the bundled ones follow:

* **Tails.** Feed delays and reverbs through `{"block": "gain", "gain": "g"}` (`echo` and `reverb` do) so that switching the
  effect OFF stops feeding them but lets the tail ring out.
* **Beat, mic, lever, color:** the effect ends in `mix` (`blend`: dry moves to the effect by LEVEL × ON, which is exactly the
  input while off) or `mix` with `"mode": "add"` (the repeats are added to the dry signal).
* **Send:** only the return comes out, no dry signal: end in `{"block": "gain", "gain": "level"}` (`echo` / `reverb` with
  `"dry": false`).
* **Lever:** `g` is 1 while the lever is off its rest and 0 at rest, where the effect must return the input exactly: a `mix` does.
* **Fader:** the effect applies the fader gain itself (`"gain": "clamp(knob, 0, 1)"`); `mix` hands the input back untouched while
  another curve is chosen (`g` 0).

#### Variables and formulas

A parameter written as a number is a constant; written as a string it is a formula over the values of the current frame:

| Variable | Meaning |
|---|---|
| `knob` | the slot's own control: COLOR knob -1 to 1, mic and send parameter 0 to 1, lever 0 (rest) to 1, fader 0 to 1; 0.5 for Beat |
| `level` | the LEVEL knob, 0 to 1 (1 for color, lever and fader) |
| `g` | the engage ramp, 0 (off) to 1 (on), faded over 5 ms |
| `beats` | the selected beat fraction in beats (1/8 to 16; 1 where the slot has no beat selector) |
| `period` | seconds of the selected beat fraction, tempo-slewed: a delay of `"period"` is on the beat |
| `period_ms` | the same in milliseconds |
| `bpm` | the tempo |
| `rate` | the sample rate in Hz |
| *an LFO's name* | the LFO's value (see below) |

Syntax: numbers (`0.5`, `1e-3`) with the units `ms` (divides by 1000) and `k` (multiplies by 1000): `40ms`, `18k`; the constants `pi`
and `tau`; `+ - * / %`; `^` (power, right to left); the comparisons `< > <= >= == !=` (1 or 0), `&&`, `||` and `!`; parentheses.
Functions: `sin cos tanh abs sqrt floor round exp2 log2`, `semi(x)` (the frequency ratio of x semitones), `min(a, b)`, `max(a, b)`,
`pow(a, b)`, `clamp(x, lo, hi)`, `lerp(a, b, t)`, `expo(a, b, t)` (a × (b / a)^t: a sweep from a to b on a pitch scale),
`if(cond, a, b)`. Times are in **seconds**, frequencies in **Hz**. A result that is not finite counts as 0. A formula without
variables is folded to a constant when the plugin loads.

Some parameters are plain numbers (marked *number* below): they are fixed at load and take a JSON number or a constant string
such as `"5ms"`; a formula there is an error.

#### LFOs

`"lfo": [{"name": "m", "period": "period * 2 / 3", "shape": "sweep"}]` makes the variable `m` for the effect's blocks, at most
four per effect. Give the `period` in seconds or `"hz"` (each a number or a formula over the variables above, not over other
LFOs); the phase restarts when the effect is switched ON. `shape` is `sine` (-1 to 1, starts at 0), `sweep` (0 to 1 to 0, a
cosine, starts at 0), `tri` (0 to 1 to 0, linear), `saw` (the phase, 0 to 1) or `square` (1 for the first half, then 0); default `sine`.

#### Blocks

Defaults are in parentheses; *f* = formula, *n* = number. Blocks that hold others take a list of blocks.

| Block | Parameters | What it does |
|---|---|---|
| `gain` | `gain` f (1), `smooth` n seconds (0) | multiplies the signal; `smooth` slews the gain (a declicked gate) |
| `route` | `matrix` `[[ll, lr], [rl, rr]]`, cells f (identity) | left out = ll × left + lr × right, right out = rl × left + rr × right: swap, mono, mono to one side |
| `pan` | `pan` f (0) | balance, -1 left only to 1 right only; the louder side keeps its level |
| `width` | `width` f (1) | stereo width: 0 mono, 1 unchanged, 2 twice the side signal |
| `mix` | `amount` f (`level * g`), `mode` `blend` or `add` (`blend`) | against the effect's input: `blend` moves from it to this signal by `amount`, `add` adds `amount` × this signal to it |
| `saturate` | `drive` f (1), `shape` `tanh` or `clip` (`tanh`) | `tanh(x × drive)`, or the signal times drive clipped to ±1 |
| `bitcrush` | `bits` f (8, 1 to 24), `hz` f (0 = off) | quantises to `bits` bits and holds each sample for `rate / hz` frames |
| `delay` | `time` f seconds (0.25), `max` n seconds (4, 0.01 to 30), `feedback` f (0), `loop` blocks | the signal `time` late (only the delayed signal comes out). A time that does not fit in `max` is halved until it does, which keeps a beat delay on the beat. With `feedback` (limited to ±0.99) the delayed signal goes through `loop` (a damping filter, saturation), times `feedback`, back into the line: repeats. Modulate `time` with an LFO for chorus, flanger, tape wobble |
| `allpass` | `time` n seconds (0.005), `gain` n (0.5), `spread` n (0) | a unit-gain Schroeder allpass (safe inside feedback loops); `spread` makes the right channel's longer by that share |
| `phaser` | `hz` f (1000), `stages` n (4, 1 to 12), `feedback` n (0, ±0.95) | the mean of the signal and `stages` first-order allpasses at `hz`: notches that move with `hz` |
| `freeverb` | `size` f (0.5, 0 to 1) | Freeverb's tail only, no dry signal, at Freeverb's wet gain; `size` sets the comb feedback 0.7 + 0.28 × size |
| `pitch` | `semitones` f (0), `window` n seconds (0.05, 0.005 to 2), `dead` n (0.01) | a two-tap delay-line pitch shifter; within `dead` semitones of 0 the input passes (the taps would otherwise comb it). Windows of 0.03 to 0.06 suit voices, 0.5 or more gives a tape-slowing sound |
| `filter` | `type` `lp` `hp` `bp` `notch` (`lp`), `hz` f (1000), `q` f (0.7071), `order` n 1 or 2 (2) | order 2: a resonant state-variable filter (cutoff and Q may change every frame); order 1: a 6 dB per octave one-pole `lp` or `hp` (no `q`) |
| `shepard` | `period` f (`period`), `low` n Hz (40), `q` n (3), `dry` n (0.33), `gain` n (1.5), `bands` n (7, 2 to 12) | `bands` band-passes one octave apart ride up the spectrum, one octave per `period`, fading in at the bottom and out at the top: an endless rising sweep. Restarts on ON |
| `feedback` | `chain` blocks (required), `loop` blocks, `gain` f (0.5) | `chain` with a loop around it, for loops that are not a plain delay: the output is what `chain` makes of (input + loop signal); the loop signal is the output through `loop` times `gain` (limited to ±0.99), one frame late |
| `parallel` | `branches` list of block lists (required) | every branch gets the same input; the output is the sum (an empty branch passes the input) |
| `crossover` | `freq` f Hz (150), `q` n (0.7071), `low` blocks, `high` blocks | splits at `freq` into lows and highs (the input minus the lows, so they sum back exactly), runs them through `low` and `high` and adds them |
| `loop` | `time` f seconds (`period`), `max` n (4), `climb` n semitones (0), `steps` n (12) | records the next `time` seconds when the effect goes ON (still heard) and then plays them over and over; each pass can play `climb` semitones faster than the last (a tape-speed stair), starting over after `steps` passes |
| `slice` | `time` f (`period`), `count` f (8), `stride` n (3), `max` n (2) | captures the next `time` seconds (still heard), cuts them into `count` slices (read at every slice start) and plays every `stride`-th slice in turn: a juggle |
| `reverse` | `time` f (`period`), `max` n (2) | the previous `time` seconds played backwards, chunk after chunk; the first chunk is heard forwards |
| `tape` | `speed` f (1), `max` n seconds (2) | varispeed playback of the last `max` seconds: `speed` times normal; below 1 it drops in pitch and falls behind live, above 1 it catches up; every pull starts from live |
| `echo` | `time` f (`period`), `feedback` f (0.5), `damp` f Hz (none), `loop` blocks, `pingpong` (false), `dry` (true), `mix` f (`level`), `max` n (4) | `gain g`, a `delay` with that `feedback` whose loop is the `damp` low-pass then `loop`, then `mix` (`dry`: the repeats × `mix` are added to the input; otherwise only the repeats × `mix` come out, for a send). `pingpong` puts the mono sum on the right and swaps the repeats' sides |
| `reverb` | `size` f (0.5), `mix` f (`level`), `dry` (true) | `gain g`, `freeverb`, then `mix` as in `echo` |

`loop`, `slice`, `reverse` and `tape` restart on ON and hand the input back while they record, so a `mix` after them fades
the effect in by LEVEL. Delay lines, filters and reverbs keep their tails when the effect is switched on again.

A few recipes (the bundled plugins are the full-size ones):

* flanger: `{"block": "delay", "time": "1ms + 4ms * sweep", "feedback": 0.6, "max": 0.05}` then `{"block": "mix", "mode": "add"}`
  with `"lfo": [{"name": "sweep", "period": "period", "shape": "sweep"}]`;
* trance gate: `{"block": "gain", "gain": "chop", "smooth": 0.003}` with a `square` LFO `chop`, then `mix`;
* a filter sweep that restarts on the beat: `{"block": "filter", "hz": "expo(18k, 150, m)", "q": 2}` with a `sweep` LFO `m`;
* a shimmer reverb (a `feedback` around `freeverb` with an octave-up `pitch` in the loop, its gain tied to the room size so the
  loop stays stable): see SHIMMER in [`send-fx`](https://github.com/OpenRsG/OpenFadeR-Plugins/blob/main/send-fx/plugin.json).

### Native libraries

`"effects": "libmyfx.so"` (a path relative to the plugin folder) loads a native library built against [`sdk/`](../sdk), in Rust or
C, that runs on the audio thread: for what the blocks cannot do. [`sdk/README.md`](../sdk/README.md) shows how to write and build one.

### The bundled effects

The DJ effects of the plugins repository are JSON-only plugins, one per hardware family:

| Plugin | Slot | Effects |
|---|---|---|
| `beat-fx` | beat | PING PONG, SPIRAL, TRIPLET ROLL, TRIPLET LPF, TRIPLET HPF, HELIX, MOBIUS |
| `mic-fx` | mic | ECHO, PITCH, MEGAPHONE, REVERB |
| `send-fx` | send | SHORT DELAY, REVERB, SHIMMER, TAPE ECHO, PING PONG, ECHO-VERB |
| `lever-fx` | lever | ECHO, REVERB, JUGGLE, REVERSE, SOLO, STRETCH |
| `cross-pass` | color | CROSS-PASS |
| `soft-mix` | fader | SOFT |
| `tutorial-fx` | beat, send | JET SWEEP, DEEP PHASER, TRANCE GATE, LO-FI, DUB SIREN: small examples of the format (listed after the others in their slots) |

The hardware's own algorithms are not published; each effect is a classic design, built from the blocks above (read its
`plugin.json`).

## Library plugins

A library plugin teaches the app a **format of track libraries**: a file or folder it can open as a library (playlists and tracks,
and optionally the beat grids, cues and waveforms the format holds), and optionally write the whole Collection as a file in
that format. It is a native shared library (Rust or C) built against the library interface of [`sdk/`](../sdk), declared in
`plugin.json`:

```json
{
  "name": "my-format",
  "library": {
    "lib": {"linux": "libmyformat.so", "windows": "myformat.dll", "macos": "libmyformat.dylib"},
    "formats": ["xml"],
    "collection_reader": "Import from my format…",
    "collection_writer": "Export Collection in my format",
    "browser": {"label": "My format", "path_setting": "file"}
  },
  "settings": [
    {"id": "file", "label": "Library file", "type": "text", "help": "listed in the Media Browser"}
  ]
}
```

| Key of `library` | Meaning |
|---|---|
| `lib` | Required. The library file in the plugin folder: one name, or one per OS (`linux`, `windows`, `macos`). |
| `formats` | What the plugin reads: lower-case file extensions without the dot, and `dir` for folders. The app only asks the plugin about paths that match (empty: every path). |
| `collection_reader` | Label of a **File > Import** entry: the user names a file, the plugin reads it, its tracks and playlists join the Collection. |
| `collection_writer` | Label of a **File > Export** entry (and the browser's EXPORT button): the plugin writes the Collection as a new file, in the output folder, named `<plugin>-<date>-<time>.<first of formats>`. The first of `formats` must be an extension. |
| `browser` | A Media Browser source: `label` is its name in the tree, `path_setting` the id of a `text` setting that holds the library file. Set the file in the plugin's settings and the source appears. |
| `device` | `true`: the plugin also works with removable devices, see [Devices](#devices). `formats` must then list `dir`. |
| `legacy_analysis_refs` | `true`: the plugin also answers `analysis_ref` for references that carry no plugin name (stored by older versions). |
| `detect` | Where the app this format belongs to keeps its library by default: a list of path patterns, or one list per OS (`linux`, `windows`, `macos`). `~`, `$HOME`, `%USERPROFILE%`, `%APPDATA%` and `%LOCALAPPDATA%` expand and `*` matches within one path part. A match that is not connected yet is offered in PREFERENCES > LIBRARIES. |
| `samples` | `true`: the plugin's libraries are sample collections: after File > Import the first list's files fill the sampler's empty slots. |

**How a path is opened.** `library::open` asks every enabled library plugin that may read the path (by `formats`) to `probe` it and
opens it with the highest score above 0; a folder no plugin claims is read as a plain folder of music, a file no plugin claims is an
error ("no library plugin reads it"). Libraries are loaded when the app starts (a library that does not load, has no entry point or
was built for another ABI is reported in PREFERENCES > EXTENSIONS and skipped); switching a plugin on or off applies after a restart.

**Settings.** The plugin's `settings` (see [Settings](#settings)) go with every `open` and `write_collection` request as
`"settings": {"id": value}`. Keys of encrypted libraries, path maps and export options are settings, not environment variables.

### The interface

The library exports one C function, `openfader_library_v2`, returning a static table `{abi, call, free}`. `call` takes one JSON
request and a pointer to the host's callback table (`{call, free}`, valid until `call` returns) and returns one JSON answer in a
buffer the library owns until it is handed to `free`; the host checks `abi` (2) before anything else. A request is an object with
an `"op"`; an answer is an object, or `{"error": "text"}`. An op the library does not know is an error, so later versions can add
ops.

| `op` | Fields | Answer |
|---|---|---|
| `probe` | `path` | `{"score": 0 to 100, "kind": "name"}`: does this plugin read that file or folder? Cheap: the name and the first bytes. |
| `open` | `path`, `settings` | `{"handle": n, "kind": "name"}`; `kind` is shown in the window title. |
| `playlists` | `handle` | `{"playlists": [{"id", "name", "depth", "is_folder"}]}` depth-first, folders before their lists. |
| `tracks` | `handle`, `playlist` (an id, or `null`: all tracks) | `{"tracks": [Track]}` |
| `search` | `handle`, `playlist`, `query` | `{"started": bool}`: runs the query in a search entry the library listed. |
| `analysis` | `handle`, `track` | `{"analysis": Analysis or null}`: the beat grid, cues, waveforms, phrases and key the format holds for that track. |
| `close` | `handle` | `{}` |
| `analysis_ref` | `reference`, `settings` | `{"analysis": Analysis or null}`: the analysis behind a track's `analysis_ref`, without an open library. |
| `write_collection` | `path`, `snapshot`, `settings` | `{"count": tracks written}`: writes a new file at `path`. |
| `capabilities` | `settings` | What the format holds: `hot_cues` and `memory_cues` (`null` = no limit), `loops`, `cue_colors` (RGB palette; empty = any), `rating_max` (0 = none), `tempo_changes`, `mp3_offset_ms`, `write_back` (can rewrite an existing library file). The app fits the Collection to it before `write_collection` or `export` and reports what was not kept. A plugin without this op holds everything. |
| device ops | | see [Devices](#devices). |

A `Track` has `title`, `artist`, `album`, `key`, `genre`, `label`, `bpm_x100`, `length_s`, `file` (the audio file path),
`analysis_ref`, `artwork`, `rating` (0 to 5), `comment`, `date_added`, `bitrate`, `sample_rate`; every field may be left out.
`id` is the app's own track id in snapshots it sends; a plugin leaves it out of what it returns.
`analysis_ref` is opaque to the app: it hands the whole track back in `analysis`, so a path or an id that finds the analysis
again goes there. An `Analysis` holds `beats` (`bar_pos`, `bpm_x100`, `time_ms`), `cues` (`hot_cue` 0 = memory, 1.. = hot cue,
`time_ms`, `loop_end_ms`, `color`, `comment`), the waveforms (`preview`, `detail`, `color_preview`, `color_detail`,
`three_band_preview`, `three_band_detail`), `phrases`, `key` and `vocal`. The `snapshot` of `write_collection` has the unique
`tracks` with their analysis, the `playlists` tree with each list's `members` (indexes into `tracks`), the track `colors`, and the
`mytags` with their assignments. The exact types are in `sdk/src/library/model.rs`; the Rust and C interface is described in
[`sdk/README.md`](../sdk/README.md).

### Writing one in Rust

Depend on `openfader-plugin` with the `library` feature, build a `cdylib` and keep `panic = "unwind"` (the SDK turns a panic
into an `{"error"}` answer):

```rust
use openfader_plugin::library::{LibraryPlugin, Playlist, Probe, Source, Track, export_library};

struct MyFormat;
struct Opened(Vec<Track>);

impl Source for Opened {
    fn kind(&self) -> String { "my format".into() }
    fn playlists(&self) -> Result<Vec<Playlist>, String> { Ok(Vec::new()) }
    fn tracks(&self, _playlist: Option<&str>) -> Result<Vec<Track>, String> { Ok(self.0.clone()) }
}

impl LibraryPlugin for MyFormat {
    fn probe(path: &std::path::Path) -> Option<Probe> {
        path.extension().is_some_and(|e| e == "myf").then(|| Probe { score: 90, kind: "my format".into() })
    }
    fn open(path: &std::path::Path, _settings: &serde_json::Value) -> Result<Box<dyn Source>, String> {
        let _ = path;
        Ok(Box::new(Opened(Vec::new())))
    }
}

export_library!(MyFormat);
```

`openfader_plugin::library` also has `PlaylistRow` and `playlist_tree`, which turn the flat folder rows most formats store into the
depth-first `playlists` answer. `cargo build --release`, copy the library next to `plugin.json`.

### Devices

A library plugin with `"device": true` (and `"dir"` among its `formats`) also works with **removable devices**: the USB sticks and SD
cards the app finds mounted. The app knows only what a mounted file system is (its label, kind, free space, eject); everything
about what a device holds is the plugin's. The device ops are requests like the others (`root` is the mount folder, `settings` the
plugin's own settings; `config` the device's settings, key → text):

| `op` | Fields | Answer |
|---|---|---|
| `device_probe` | `root` | `{"libraries": ["name", …], "can_write": bool, "features": […], "actions": [{"id", "label", "help", "enabled", "writes"}], "prefetch": [file…], "backup": [folder…], "modified": unix seconds or null, "title", "about", "options": [field…]}`. `libraries` names what the plugin found on the device (empty: nothing yet, it may still `can_write`). `features`: `export`, `update`, `remove`, `histories`, `backup`, `settings`, `plan` (`export` honours `dry_run`), `check` (`device_check`, `device_fix`); the app offers only those. `prefetch`: files (relative to `root`) read once with a progress bar before the library is opened; `backup`: folders a backup copies. `title`: the format's name in the device window (empty: the plugin name); `about`: one line on the software and players, with versions, that read it; `options`: settings fields (`bool` or `choice` only) shown under the format and kept per device, passed in `config`. |
| `export` | `root`, `snapshot`, `options` (`free`, `config`, `banks`, `dry_run`, `max_file`) | `{"tracks", "added", "updated", "playlists", "warnings": […], "notes": […], "baselines": [[key, n]…], "fingerprint", "plan", "verified"}`: writes the collection (or the playlists the user ticked) to the device: a new library when it has none, else it is extended. Audio and artwork are copied by the plugin. `max_file`: the largest file the file system holds (FAT: 4 GiB - 1); a bigger track is left out with a warning, never copied in part. With `dry_run` (only sent to a plugin listing `plan`) nothing at all is written and `plan` answers what would be: `{"add": [{"key", "title", "artist", "bytes"}…], "update": […], "analyse", "playlists", "copy_bytes", "write_bytes", "files", "blocked": [text…]}`. `verified: true`: the library files were read back and held what was written. |
| `update` | `root`, `snapshot`, `baselines` | `{"matched", "unmatched", "edits": [{"key", "rating", "comment", "color", "cues", "grid"}], "baselines": […]}`: "Update Collection": what was edited on the player since `baselines`. The app applies `edits` to its store. |
| `remove` | `root`, `target`, `delete_tracks`, `config` | `{"tracks", "playlists", "histories", "warnings"}`; `target` is `{"kind": "playlist", "path": [names]}`, `{"kind": "histories", "names": […]}` or `{"kind": "track", "file": path}`. |
| `device_action` | `root`, `action`, `confirmed`, `config` | `{"message"}` or `{"confirm": {"text", "yes"}}`: one of the buttons of `actions`. A `confirm` answer is asked of the user; on "yes" the request is repeated with `confirmed: true`. |
| `settings_schema` | | the settings pages, see below |
| `settings_read` | `root` | `{"values": {key: text}, "fingerprint"}`: what the device holds now. |
| `settings_write` | `root`, `values` | `{"fingerprint"}`: writes the settings to the device. |
| `backup_tracks` | `dir` | `{"tracks": [{"title", "name", "size", "file"}]}`: the tracks the library inside a backup folder lists. |
| `device_check` | `root`, `config` | `{"libraries": [{"name", "present", "readable", "modified", "tracks", "playlists", "cues"}…], "problems": [{"id", "severity", "library", "cause", "consequence", "action", "fix", "files"}…]}`: the state of the plugin's libraries, read-only and quick (it runs on every plug-in). `severity`: `info`, `warning`, `error` (a player would show an incomplete or empty device), `unreadable`. `cause` / `consequence` / `action` are three sentences for the user: what is wrong, what it means at a gig, what to do. `fix`: the `device_fix` id that repairs it, or null; `files`: the audio files (relative to `root`) it is about, which the app can copy again from the collection. |
| `device_fix` | `root`, `fix`, `config` | `{"message"}`: runs a fix that `device_check` named (a job with progress). |

**Several formats on one device.** Every device plugin is probed for every device. Those that hold a library there or `can_write`
are its formats; the one holding a library with the most `features` reads it (browse, update, histories). The DEVICES tab of the
device window lists them with their `title`, `about` and `options`: UNIVERSAL switches every format on, DEDICATED keeps the ticked
ones (the EXPORT and SYNC MANAGER tabs show the same as chips). By default these are the ones already on the device, or all of them
on an empty device, and the choice is kept per device. Each connected device also has a USB icon on the browser's left strip that
opens this tab on it. The formats are written one after the other. The first copies the audio. Each
later one gets the snapshot's tracks pointed at the files the first one's library lists, so every format shares one copy. A plugin
that writes audio should place it with the SDK's `place_audio` (`Contents/<artist>/<album>/<file>`); then the formats agree even
when the first one reads nothing back.

**Cue sync.** A track that is in the collection and on a device, same file name and size, has its cues, hot cues, loops, beat grid
and phrases kept the same on both copies (PREFERENCES > DJ SYSTEM > DEVICE > CUE SYNC, on by default). This needs a device whose
plugin offers `update`, so that it reads back. When a device is plugged in, each copy that lacks one of these parts is filled from the
other. When a track is loaded on a deck, the copy loaded wins. Edits of a loaded track reach its device copies a few seconds after the
last one. A device copy is written with an `export` of just those tracks, in the device's picked formats.

**Transfer.** Tracks go to a device straight from the browser: dragged onto the device's row in the tree, onto one of its playlists,
or sent with the track menu's "Transfer to <device>" (one item per mounted writable device); a My Playlists playlist or folder dragged
onto the device row goes as a Sync Manager sync of that one playlist does. Tracks without a beat grid or a key are analysed first (the
analysis pool, the Analysis Lock respected). The write is an `export` of a snapshot of just those tracks, in the device's picked
formats, with the analysis the collection holds (your cue, grid and key edits on top). Onto the device itself the snapshot has no
playlist. Onto a device playlist the snapshot holds one playlist with the device playlist's folder path and name, which the plugin
merges by (it replaces that playlist's content), so the snapshot lists the playlist's current members first (a member the collection
has too is written with the collection's analysis, its empty parts filled from the device copy; any other member with the analysis
read from the device) and then the new tracks; a track the playlist holds already is skipped and counted.

**Export target and formats.** Right-click a collection playlist → **Export to a device** to open EXPORT with it ticked.
The format chips apply to both a mounted device and a typed folder. A non-empty folder takes precedence; the page and
confirmation card name that target explicitly. Blank targets default to all available formats; existing libraries default to
their formats plus rekordbox. Each format switch is remembered separately for each target, so choosing another target
does not carry the previous target's switches over, and newly installed formats follow the defaults. EXPORT keeps its
playlist ticks when you change tabs. A playlist sent before the collection has loaded waits for its list to arrive.


**Before a write.** EXPORT, SYNC NOW, the sync of a device that was just plugged in, and Transfer show a card first (Preferences > DJ
SYSTEM > DEVICE, "Show what an export will write before it starts"): the tracks to copy and the ones refreshed, the analyses run
first, the time, what is left out and why, then START or CANCEL. A format whose plugin lists the `plan` feature answers the same
`export` request with `options.dry_run` set (nothing is written; the answer's `plan` is the card); for any other format the app
estimates from the audio files. The device holds one copy of each track, so its bytes count once whatever formats are written. The time
uses the write speed measured on earlier jobs on that device (8 MB/s and 20 ms per small file until there is one). Every write job
then finishes or undoes a change a pulled stick left (`commit::recover`), backs the library up (kept as an automatic backup, the last
five per device) unless Preferences say otherwise, writes, and reports the plugins' read-back (`verified`). Its status line shows the
speed and the time left, and names what makes a slow stick slow.

**Stick check.** The host judges every device ([`src/device/check.rs`](../src/device/check.rs); the DEVICES tab and `openfader verify`).
A plugin listing `check` answers `device_check` (`libraries`: every library it can hold with `present`, `readable`, `modified`,
`tracks`, `playlists`, `cues`; `problems`: `id` prefixed with the plugin name, `severity` `info` / `warning` / `error` /
`unreadable`, `library`, `cause` / `consequence` / `action` as texts for the user, `fix` the id `device_fix` repairs it with, `files`
the audio files it is about). A plugin without `check` is judged from the library the host opens through it: its track and playlist
counts, and every audio file the library lists must be on the device (else a problem whose `files` the host copies again from the
collection). The host adds what no plugin can know: a change a pulled stick cut off (the journal of `commit`), the file system and
the partition table. Player models come from `players.json` next to `plugin.json` (`players`: `model`, `reads` the library names it
reads, `file_systems`, `partition_tables`, `status` `confirmed` or `to_measure`, `note`; `presets`: named lists of models for the
booth profile); the models of all plugins are merged. A model is OK when the file system and the partition table are in its lists
(a list that is empty, or a table not known, is not held against it) and one library it reads is present, readable and without an
`error` problem; `to_measure` is never a plain OK. FIX THE STICK runs the problems' `fix` ids in severity order through `device_fix`.

A job (`export`, `update`, `remove`, `device_action`, `device_fix`) runs inside its call on a thread of the app. Meanwhile the plugin
calls back through the `host` table every request receives (`openfader_plugin::library::host` in Rust): `analysis` (the beat grid,
cues, waveforms of a track: `computed: false` what the track's own library holds, `true` the app's analysis of the audio, cached),
`audio_info` (sample rate of a file) and `progress` (`phase`, `done`, `total`, and `bytes_done` / `bytes_total` when the plugin counts
bytes, so the app shows the speed and the time left; the answer says whether the user cancelled, and the plugin then stops with an
error and leaves the device as it was). In Rust, `place_audio` counts the audio it copies; add other bytes with `host::add_bytes`
after `host::set_bytes_total`.

**All or nothing.** A stick can be pulled out, and a computer can crash, at any moment. A plugin writes its library files through
`openfader_plugin::library::commit::Commit`: each new file is staged next to the one it replaces (`<name>.ofnew`, flushed to the
device), read back and checked, then `commit()` switches them all. The journal `/.openfader/journal.json` records where a change
stands. While staging, an interruption leaves the old library in use, and the app deletes the staged files. While switching, every
staged file is whole and checked, so the app finishes the renames. Either way the device holds one whole generation of its
libraries, never a new one next to an old one. The app recovers a device when it shows up and before any job writes to it.

**Players.** A device plugin may ship `players.json` next to its `plugin.json` (a built-in one embeds it): `{"version", "players": [{"model", "reads":
[library names from device_check], "file_systems": ["vfat", "exfat", "hfsplus"], "partition_tables": ["dos", "gpt"], "status":
"confirmed" | "to_measure", "firmware", "note"}…], "presets": [{"id", "label", "models"}…]}`. The stick checker judges a device for
each player of the booth profile (PREFERENCES > DJ SYSTEM > DEVICE and the DEVICES tab). A model marked `to_measure` is never shown
as working. A new model is a line in that file, shipped with the plugin, with no new app.

**Settings pages.** `settings_schema` answers `{"tabs": [{"label", "scope", "groups": [{"label", "fields": […]}], "apply_label",
"read_label", "auto_key"}]}`. A field is `{"key", "label", "hint", "default", "scope", "type": …}` with `type` one of `choice`
(`choices` of `{value, label}`; `from_list` takes the active entries of a `list` field as the choices), `text`, `bool`, `list`
(entries move between an inactive and an active list and are ordered in the active one; stored as the active values joined by `,`),
`file` (a file the user imports; the app stores a copy and the value is its path) and `info`. `scope` is `global` (PREFERENCES >
DJ SYSTEM only), `device` (a device's own page) or `both`: a device without a value of its own follows the defaults. The app draws
the pages, stores the values per device and for all devices, and hands them to the plugin as `config` (`export`, `remove`,
`device_action`) and `settings_write`. A tab's `apply_label` adds a button that writes the defaults of the tab to every connected
device, `read_label` one that takes what was changed on the player over (`settings_read`), `auto_key` names a `bool` key that makes
edits of the tab reach the connected device at once.

**Analysis refs.** A track's `analysis_ref` is opaque to the plugin's host; the app stores it as `<plugin>:<ref>` and, when it needs the
analysis of a stored track without the library being open, asks that plugin with `analysis_ref` (`{"reference"}` → `{"analysis"}`). A
plugin that declares `"legacy_analysis_refs": true` also answers references stored before they carried a plugin name (bare paths):
the app prefixes them with its name at start-up.

### The built-in rekordbox extensions

`crates/` of the core repository holds the rekordbox extensions as crates of the app's Cargo workspace: `rekordbox-device` (the device
libraries on USB sticks and SD cards, Device Library `export.pdb` and OneLibrary `exportLibrary.db`: reads them, exports to them,
updates the collection from them, backs them up, converts them and edits their settings), `rekordbox-xml` (reads and writes a
rekordbox XML collection file), `rekordbox-db` (the desktop collection `master.db`, a key as a `secret` setting), and the crates
`rekordbox-common` and `rekordbox-export` they share (`itunes` and `sample-pack` of the plugins repository use `rekordbox-common` too).

They are the same library plugins as the ones below, with one difference: they are linked into the app, so they cannot go missing or be
switched off. A crate does not export `openfader_library_v2`; it implements `LibraryPlugin` and the host (`src/plugin/builtin.rs`)
holds a table per extension: its `plugin.json` (without `lib`), its `locales/`, its `players.json`, and the static
`openfader_plugin::library::api::<Plugin>()`. Requests go through that table's JSON `call` exactly as through a loaded library: there
is one protocol. The registry lists the built-ins first and on, whatever `plugins.json` says; their settings are stored there under
their names as before. A plugin folder of the same name is skipped with a note in the plugin list. They are behind the `rekordbox` feature of the `openfader` crate, which the `app` feature turns on (so
Pi builds with `--no-default-features --features app` have them). It also swaps the bundled SQLite for SQLCipher with its own
OpenSSL; a build without it, like the mobile crate, has no built-in plugins, plain SQLite and no OpenSSL. Their tests run with the app's:
`cargo test --release --workspace`.

What `rekordbox-device` guarantees when it writes a stick, and how that is checked:

- **All or nothing.** One user action (export, sync, remove, player settings) is one transaction. New audio and analysis files are
  written and fsynced first, the changed library files are staged beside their targets, then switched in one journalled step. Pulled
  out at any point, the stick holds the old libraries or the new ones, never a mix; the next write or `device_check` finishes or rolls
  back an interrupted switch first. A cancel or an error leaves the folder as it was.
- **Read back before it counts.** Before the switch the staged Device Library and OneLibrary are read again and compared with what
  was meant to be written (titles, counts, playlists and their entries, cue counts in the analysis files). A difference stops the
  export with a plain sentence and nothing changed; a clean export reports `verified`.
- **Dry run.** An export with `dry_run` writes nothing and returns the plan: tracks to add or update, bytes to copy and write, files,
  and what is blocked (no room, a file over the file system's limit, a name the stick cannot hold, too many playlists).
- **Names the file system accepts.** Long, accented, emoji and reserved (`CON`, `NUL`, ...) names are cleaned; a device path never
  exceeds the 255 characters a Device Library row holds.
- **Check and fix.** `device_check` reads what is on a stick without writing and lists problems (missing or unreadable library, the two
  libraries disagreeing, missing audio or analysis files, leftovers of an interrupted write) with cause, consequence and action.
  `device_fix` repairs one of them: rebuild one library from the other, rewrite the analysis files, clean leftovers.
- **Players.** `crates/rekordbox-device/players.json` lists the players the checker knows, what each reads and what is only inferred.
  `crates/rekordbox-device/COMPATIBILITY.md` says which rows are confirmed on hardware (none yet) and which are not; the test
  `compatibility_lists_every_player_and_library` keeps the two together.

Readers that are not ours check the output in CI (job `readers` of `ci.yml`): `crates/rekordbox-export/examples/fixture.rs` writes a
small stick (unicode, emoji and long names, cues, loops, a playlist folder), and `.github/check_stick.py` reads it with pyrekordbox
(the analysis files and `exportLibrary.db`), with a small DeviceSQL reader and with `rekordcrate` (`export.pdb`), and fails on any
difference from the manifest. Locally: `cargo run --release -p rekordbox-export --example fixture -- /tmp/fx && pip install
pyrekordbox==0.5.0 && python .github/check_stick.py /tmp/fx`. rekordcrate 0.3.0 cannot parse cue entries of type 1, so it is used for
`export.pdb` only.

### The bundled library plugins

Rust crates of one Cargo workspace, the plugins repository's `Cargo.toml` (`extension/plugin/Cargo.toml` in a core checkout), are library plugins: `itunes` (reads a media player's
library file), four device plugins that write the libraries of other players and apps beside the built-in rekordbox one on the same stick and also read those apps' desktop
libraries (with `detect` patterns, so PREFERENCES > LIBRARIES offers to connect them), and one for sampler packs (a zip of audio and a list file; the
samples are extracted to the plugin's cache folder and served as a library; with `"samples": true` the first list also fills the
sampler after File > Import). They need building once: `extension/plugin/build.sh` builds
the workspace in release mode and copies each library next to its `plugin.json` (`start.sh` runs it first); the workspace builds against
`sdk/` and `crates/rekordbox-common` of the core checkout. Their settings are on their
cards in PREFERENCES > EXTENSIONS; keys of encrypted libraries are `secret` settings (kept in `plugins.json`, shown masked).

## Controller plugins

A controller plugin **drives a MIDI port** instead of our mapping files: it reads the controller's messages, works the app's
controls and lights the controller's lights. It is for what a mapping file cannot say (a script, a manufacturer's protocol) and
for the mapping formats of other software. The bundled `mixxx` plugin runs Mixxx's mapping files ([below](#the-mixxx-plugin)).

```json
{"name": "mixxx", "controller": {"lib": {"linux": "libmixxx.so", "windows": "mixxx.dll", "macos": "libmixxx.dylib"}}}
```

PREFERENCES > CONTROLLER > **CONTROLLER PLUGINS** has one row per MIDI input port: **AUTO** (every enabled controller plugin is asked
how sure it is about the port, `probe`; the best one drives it, a port nobody knows keeps our mapping files), **OPENFADER MAPPING**
(the plugins are skipped) or one plugin by name (it drives the port even when it would not have recognised it). The choice is kept
per port name in `controller_plugins` of `settings.conf`, without the ALSA numbers. A claimed port **bypasses our XML mapping**: its
messages, whole, go to the plugin only. A plugin whose `open` fails says why in a notification and the port keeps our mapping. The
plugin's soft takeover has a table of its own per session.

### The interface

The library exports `openfader_controller_v1`, returning a static table (`ControllerApi`, `CONTROLLER_ABI` = 1) of `probe`, `open`,
`midi`, `changed`, `timer` and `close`. Every call carries the host's callback table (`Host`), valid until the call returns:
`get` / `set` a control, `midi_out`, `timer` / `cancel`, `watch` a control (its changes come back as `changed`), `takeover`,
`label`, `log` and `settings` (the plugin's settings of `plugin.json` as one JSON object). The host makes every call of a
session from one thread, one at a time, **never from the audio thread** (it is the UI thread: return quickly); the callbacks
never re-enter the plugin, a control it sets is reported back by a later `changed`. Messages arrive complete (sysex as
`F0 … F7`; clock and active sensing are dropped), and MIDI out is paced by the host.

In Rust depend on `openfader-plugin`, build a `cdylib` (keep `panic = "unwind"`), implement `ControllerPlugin` and `Controller` and
call `openfader_plugin::export_controller!(MyDriver)`; `openfader_plugin::controller::host` has the callbacks as functions. A
panic in a call is logged and the session lives on.

### The controls

A control is a key `name[/col[/arg]]`. `name` is one of the actions of the mapping files (`docs/mappings.md`) or a value below;
`col` counts from 1 and is the deck (the Beat FX unit or MERGE side for the FX actions); `crossfader`, `master` and `browse` take
none; `arg` is the constant of the action's mapping row (`hw-fx-on/1/2`, `hw-pad-delete/2/0`). A key that does not exist, or whose
deck is not in the layout, answers NaN and sets nothing.

| Setting a control | Value |
|---|---|
| a button (`play`, `cue`, `sync`, `pad1`…, `hw-*`) | 0.5 and up presses, below releases (a hot cue pad releases to the slip playhead, `hw-fx-hold` lets go); `play` and the like toggle on a press |
| a knob or fader (`fader`, `trim`, `eq-hi`, `eq-mid`, `eq-low`, `filter`, `crossfader`, `master`, `hp-mix`, `hp-level`, `tempo`, `fx-level`, `merge-knob`…) | 0 to 1; through soft takeover when the plugin asked for it with `takeover` and PREFERENCES > CONTROLLER > Others > Soft takeover is on |
| an encoder or jog (`jog`, `jog-bend`, `jog-search`, `jog-cut`, `browse`, `fx-beat-rel`, `zoom-rot`) | signed ticks (a jog turns the platter as its mapping row would) |
| `jog-touch` | 0.5 and up: the platter is touched |
| `position`, `position-ms` | the playhead as 0 to 1 of the track, or in ms (seeks) |

| Reading a control | Value |
|---|---|
| `loaded`, `playing`, `at-cue`, `synced`, `deck-master`, `loop-active`, `key-lock`, `quantize`, `slip`, `reverse`, `cue-monitor`, `vinyl`, `jog-cutter` | 0 or 1 |
| `pad1` … `pad8` | 1 when that hot cue is set |
| `fx-on/<unit>/<slot>` | 1 when the FX slot (0 to 2) is on |
| `fx-assign`, `loop-beats`, `pad-mode` | the number the panel shows |
| `bpm`, `level`, `position`, `position-ms`, `duration-ms`, `rate` | the number (`level` 0 to 1) |
| any knob or fader | its position on screen, 0 to 1 |

The action names are the ones of `docs/mappings.md`; the layer keys of a controller (`hw-deck-select`, `hw-dual`) cannot be set: a
plugin addresses decks directly. `watch` reports a change at most every 10 ms (and once when it starts, so the lights begin right).

### The `mixxx` plugin

It runs **Mixxx controller mappings** as Mixxx's manual documents them: a `*.midi.xml` file and the JavaScript files it lists. No
mapping file is bundled; they are read from your own Mixxx folders: the plugin's **Mapping folder** setting first, then
`~/.mixxx/controllers`, the Flatpak and macOS user folders, `%LOCALAPPDATA%\Mixxx\controllers`, and the install folders
(`/usr/share/mixxx/controllers`, `/usr/local/share/mixxx/controllers`, `C:\Program Files\Mixxx\controllers`, the macOS app bundle).
A mapping's scripts are looked up beside its XML, then in the other folders.

The mapping for a port is the one whose controller name (`<controller id>`, `<info><name>`) or file name is most like the port's
name (all of its words in the port's name, or its model number and half of its words; the plugin's **Mapping** setting names one,
for a port you assigned to the plugin by hand). `probe` reports 60 to 100.

**Controls and outputs.** `<options>` understood: `script-binding` (the key `Prefix.func` is called as
`func(channel, control, value, status, group)`, a sysex one as `func(data, length)`), `normal`, `invert`, `diff`, `selectknob`,
`hercjog` (two's complement steps), `spread64`, `rot64`, `rot64inv` (centred on 64), `button`, `switch`, `soft-takeover`, and
`fourteen-bit-msb` / `fourteen-bit-lsb` (the two halves of one control). A note-off a mapping did not list goes to its note-on with 0.
An `<output>` sends `status midino on|off` when its control changes (on from `minimum` to `maximum`; with no `minimum` above 0).
Scripts are loaded in the order listed, then every `functionprefix.init(controllerId, debugging)` runs; `functionprefix.shutdown()`
runs when the port is released.

**Script API** (Boa, a pure-Rust JavaScript engine, ES2023): `engine.getValue`, `setValue`, `getParameter`, `setParameter`,
`getParameterForValue`, `makeConnection` (and `makeUnbufferedConnection`, `connectControl`; the connection has `disconnect`,
`trigger`), `trigger`, `beginTimer` (at least 20 ms, `once` as Mixxx's `oneShot`), `stopTimer`, `softTakeover`,
`scratchEnable` / `scratchTick` / `scratchDisable` / `isScratching` (touch and ticks of the jog), `brake`, `spinback`, `softStart`,
`log`; `midi.sendShortMsg`, `midi.sendSysexMsg`; `print`, `console.*`; `script.deckFromGroup`, `midiDebug`, `toggleControl`,
`triggerControl`, `absoluteLin`, `absoluteLinInverse`, `absoluteNonLin`, `absoluteNonLinInverse`, `midiPitch`, `spinback`, `brake`,
`softStart`, the `*RegEx` patterns; `bpm.tapButton`. The plugin's own versions are loaded before the mapping, so a mapping that lists
Mixxx's own `common-controller-scripts.js` or `midi-components-0.0.js` (the library is in your Mixxx folder) runs on them.

**Mixxx controls** are translated to ours (a value in Mixxx's range, `setParameter` / `getParameter` the 0 to 1 knob position); one
not listed has no counterpart: reading it answers 0, setting it does nothing, the log says so once.

| Mixxx `[group]` `item` | OpenFadeR control |
|---|---|
| `[ChannelN]` `play` (toggle), `play_indicator` | `play/N`, `playing/N` |
| `cue_default`, `cue_gotoandplay`, `cue_gotoandstop`, `cue_preview`, `cue_indicator` | `cue/N`, `at-cue/N` |
| `sync_enabled`, `beatsync`, `keylock`, `quantize`, `slip_enabled`, `reverse`, `pfl` | `sync/N`, `key-lock/N`, `quantize/N`, `slip/N`, `reverse/N`, `cue-monitor/N` |
| `loop_enabled`, `reloop_toggle`, `loop_in`, `loop_out`, `loop_halve`, `loop_double` | `loop-active/N`, `reloop/N`, `loop-in/N`, `loop-out/N`, `loop-halve/N`, `loop-double/N` |
| `beatloop_{1,2,4,8,16}_toggle` (and `_activate`), `beatjump_{1,4}_forward` / `_backward` | `loop-X/N`, `jump-fwd-X/N`, `jump-back-X/N` |
| `hotcue_{1..8}_activate` (`gotoandplay`, `gotoandstop`), `_enabled`, `_clear` | `padX/N`, `padX/N` (read), `hw-pad-delete/N/(X-1)` |
| `LoadSelectedTrack`, `eject`, `track_loaded`, `bpm`, `bpm_tap` | `load/N`, `hw-pad-unload/N`, `loaded/N`, `bpm/N`, `grid-tap/N` |
| `volume`, `pregain` (0 to 4), `filterHigh` / `filterMid` / `filterLow` (0 to 4) | `fader/N`, `trim/N`, `eq-hi/N`, `eq-mid/N`, `eq-low/N` |
| `rate` (-1 to 1) | `tempo/N` (the same 0 to 1 of the knob: 0 is the + end) |
| `playposition` | `position/N` |
| `jog`, `wheel`, `scratch2` | `jog-bend/N`, `jog-bend/N`, `jog/N` (ticks) |
| `[EqualizerRack1_[ChannelN]_Effect1]` `parameter1` / `2` / `3` | `eq-low/N`, `eq-mid/N`, `eq-hi/N` |
| `[QuickEffectRack1_[ChannelN]]` `super1` | `filter/N` |
| `[EffectRack1_EffectUnitU]` `mix`, `group_[ChannelN]_enable` | `fx-level/U`, `hw-fx-assign/U/(N-1)` |
| `[EffectRack1_EffectUnitU_EffectV]` `enabled` | `fx-on/U/(V-1)` (read), `hw-fx-on/U/(V-1)` |
| `[Master]` `crossfader` (-1 to 1), `gain`, `headGain`, `headMix` (-1 to 1) | `crossfader`, `master`, `hp-level`, `hp-mix` |
| `[Library]` `MoveVertical`, `ScrollVertical`, `GoToItem`, `MoveFocusForward`, `MoveFocusBackward` | `browse`, `browse-push`, `browse-back` |

Limits: no samplers, vinyl-control, `engine.softTakeoverIgnoreNextValue` (nothing to do), `script.crossfaderCurve`, HID or
bulk controllers (only MIDI ports), and no mapping settings (`<settings>`); `engine.brake` / `spinback` run our BRAKE (its length is
ours). A pitch bend arrives as control 0 with its 14-bit value (inferred from the manual); a script that never returns stalls the UI
thread, as in a call that is slow.

## Knob modes

`"knob_modes": [{"name": "CENTER LOCK", "stop": 0.5}]` adds CENTER LOCK to PREFERENCES > CONTROLLER > Mixer > COLOR FX KNOB. `stop`
(0 to 1 of the knob's travel, 0.5 = the centre) is a position one drag cannot pass: a turn that started on one side halts
there, and the next drag may leave it either way. With the mode chosen, every COLOR FX knob behaves so on screen; a MIDI
controller's knob sends absolute positions and is not held. Example: `center-lock` in the plugins repository.

## Pad modes

A pad mode is a mode button after the built-in ones (HOT CUE … STEMS). Its pads show the deck's hot cues. Hitting pad n runs
the `press` steps on that deck, letting it go runs the `release` steps:

| Step | Does |
|---|---|
| `"jump"` | to pad n's hot cue, playing or paused as before; nothing on a pad without a hot cue |
| `"play"`, `"pause"` | play, pause |
| `{"beat_jump": 4}` | jump by beats (negative = back) |
| `{"loop": 1}` | a loop of that many beats from the playhead |
| `"loop_off"` | leave the loop |

GATE CUE (`gate-cue` in the plugins repository) is `"press": ["jump", "play"], "release": ["pause", "jump"]`: the deck plays
from the cue while the pad is held and is back on it, paused, when you let go. An unknown step makes the whole `plugin.json`
invalid (PREFS lists why).

## Automix styles

`automix` adds styles to the AUTOMIX panel's STYLE selector and RANDOM pool. No script, model, network or GPU is needed.
Restart after installing or editing a style, as with knob and pad modes. Built-ins use the same recipe engine.

```json
{
  "name": "transition-recipes",
  "automix": [{
    "name": "FILTER THROW",
    "steps": [
      {"at": 0, "over": "75%", "deck": "out", "filter": 1},
      {"at": 0, "over": "100%", "crossfader": 1},
      {"at": 0, "deck": "in", "low": 0},
      {"at": "50%", "deck": "in", "low": 0.5},
      {"at": -4, "deck": "out", "fx": {"effect": "echo", "beat": 0.5, "level": 0.6}},
      {"at": "100%", "deck": "out", "fx": {"on": false}}
    ]
  }]
}
```

Names are unique (including the built-ins), 1–16 characters, without commas. Optional `bars` (1–64) fixes the style's length;
otherwise LENGTH supplies it. The usual planner may shorten a transition when less track remains. Steps outside its length
are not played. `at` is beats from the transition's start, negative beats from its end, or a percentage string such as `"50%"`.
`over` (default zero) ramps continuous controls over that many beats or that percentage of the transition. A ramp begins at
the preceding step's current value; simultaneous steps are applied in JSON order. One-shot commands ignore `over`.

| Step field | Values and meaning |
|---|---|
| `deck` | `"out"` or `"in"`; required for everything except `crossfader` |
| `crossfader` | 0–1 travel from outgoing to incoming side, constant-power curve |
| `fader` | 0–1 times the DJ's channel fader |
| `low`, `mid`, `high` | EQ knob positions: 0 minimum, 0.5 flat, 1 maximum; EQ/ISOLATOR setting still applies |
| `filter` | −1 closed low-pass, 0 off, +1 closed high-pass; borrows FILTER COLOR FX |
| `fx` | FX2 slot 1 on the step's channel: `effect` (Beat FX selector name, any case), `beat` (nearest supported beat fraction), `level` (0–1), `on` (default true); omitted parameters keep their current values |
| `loop` | 1/8–32 beats from the current beat; 0 exits the loop |
| `brake` | Vinyl stop over more than 0 and at most 16 beats |

The controls and FX panel show the moves. Finishing, SKIP during a transition, or stopping restores the borrowed knobs,
faders, COLOR FX type and FX2 slot. The crossfader stays on the incoming deck; the outgoing deck pauses. A style-created loop
is cleared. Do not operate borrowed controls during a transition: automation owns them until release. Aim to finish with the
incoming EQ flat, its filter off and its fader multiplier at 1; the saved DJ values are restored afterwards.
Without grids or enough room, the existing end-of-track cut rule takes priority; late grid analysis replans the same chosen
style. RANDOM draws only from the checked styles and excludes the preceding pick; the panel keeps at least two enabled.

## In the studio theme

A theme that selects the studio workspace (`layout-profile` 2, [docs/skins.md](skins.md); the bundled Studio theme) shows every one of these
extension points without a line of theme code: the effects of plugins are in the effect drop-downs of the deck panels' three FX
slots, the pad modes are in the MORE menu of the pad tabs, the COLOR FX effects and knob modes in the SETTINGS tab of the mixer, a
plugin's source in the sidebar's tree, and its actions in the Plugins menu of the window's menu bar. The drop-downs read the same
lists as the desktop panels (`FxState.effects`, `DeckState.pad-mode-names`, `MixerState.cfx-names`), so a plugin installed or
removed changes all layouts alike.

## A first plugin

A folder `hello/` with `plugin.json`:

```json
{"name": "hello", "script": "main.py",
 "actions": [{"id": "hi", "label": "Say hello", "input": "Your name"}]}
```

and `main.py`:

```python
import json, sys
req = json.load(sys.stdin)
if req["op"] == "action":
    json.dump({"commands": [{"cmd": "toast", "text": "Hello " + req["input"]}]}, sys.stdout)
else:
    json.dump({"error": "unsupported: " + req["op"]}, sys.stdout)
```

Drop the folder on the window: **Plugins > hello: Say hello**. In Lua the same is a `main.lua` with `function handle(req) … end`
([Lua plugins](#lua-plugins)) and `"script": "main.lua"`. Any other language works through `command` as long as it reads stdin and
writes stdout. A program that is slow to start (a JVM, a Python with big imports) makes every request slow: keep start-up small,
make it [persistent](#persistent-plugins), or use Lua, which starts in microseconds.

## Limits

* Plugins run with your rights. Install only what you trust.
* Effects, libraries, sources, knob and pad modes are read at start-up; a change to them needs a restart (the plugin manager
  offers it). A source's lists are cached until then.
* Tracks of a source are not in the Collection. Once a track is loaded, its downloaded file is a file like any other: edits
  (cues, grid, rating) are kept by file name and size.
* All tracks, Related and SUGGEST never include plugin tracks, and DUPLICATE TO MY PLAYLISTS refuses a plugin list.
