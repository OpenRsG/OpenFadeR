# OpenFadeR library format

Two things are documented here, so other tools can read and write a collection without reverse engineering:

1. the **open export** (File > Export Collection): a JSON file, a CSV table and an M3U8 playlist;
2. the **library database** `store.db`, the SQLite file that holds the Collection itself.

The export is the stable interface. The database is documented as it is today (schema version 17); it can only grow:
migrations append columns and tables, they never rename or reuse one.

## 1. Open export

Files are created in the export folder (`OPENFADER_EXPORT_DIR`, else the music folder's `OpenFadeR` folder) as
`openfader-YYYYmmdd-HHMMSS.<ext>`. An existing file is never overwritten.

### JSON (`.json`)

One UTF-8 object, written compact (pipe it through `jq` to read it).

```json
{
  "format": "openfader-library",
  "version": 1,
  "tracks": [ { "id": "t12", "title": "…" } ],
  "playlists": [ { "name": "Sets", "folder": true, "tracks": [], "children": [ … ] } ]
}
```

`format` is always `"openfader-library"`. `version` is bumped when a field changes meaning or goes away; new optional
fields do not bump it, so readers ignore unknown fields and treat missing ones as empty / 0.

Track (`tracks[]`):

| field | type | meaning |
|---|---|---|
| `id` | string | unique in the file; what playlists list. `t<n>` is Collection row `n` (see the database below). A track that is not a Collection row has a `name|size` or `meta|title|artist` string; a repeated id gets `#<index>` appended |
| `file` | string or null | absolute audio path |
| `title`, `artist`, `album`, `genre`, `label`, `comment`, `date_added` | string | `date_added` is `YYYY-MM-DD` or empty |
| `key` | string | the analysed key in Camelot notation (`8A`), else the key as the source named it |
| `key_id` | 0..=23 or null | `(camelot number - 1) * 2`, plus 1 for B; null when `key` is not Camelot notation |
| `bpm_x100` | integer | tempo times 100 (`12850` = 128.50) |
| `length_s` | integer | seconds |
| `rating` | 0..=5 | stars |
| `bitrate`, `sample_rate` | integer | kbps and Hz, 0 when unknown |
| `color` | string or null | track colour: Pink, Red, Orange, Yellow, Green, Aqua, Blue, Purple |
| `tags` | `[[category, tag], …]` | My Tag assignments |
| `cues` | array | `{"hot_cue": n, "time_ms": ms, "loop_end_ms": ms or null, "color": [r,g,b] or null, "comment": string or null}`; `hot_cue` 0 is a memory cue, 1 and up are hot cues A, B, … |
| `grid` | `[[time_ms, bpm_x100, bar_pos], …]` | one row per beat; `bar_pos` is 1..=4 |
| `phrases` | object or null | `{"mood": "High" \| "Mid" \| "Low", "list": [{"beat", "end_beat", "kind", "variant", "fill"}]}`; beats are 1-based grid indexes, `kind` is Intro, Verse, Chorus, Bridge, Outro, Up or Down, `fill` is a beat or null |

Playlist (`playlists[]`, the tree; folders nest through `children`):

| field | type | meaning |
|---|---|---|
| `name` | string | |
| `folder` | bool | a folder holds `children`, a playlist holds `tracks` |
| `tracks` | `[id, …]` | track ids in playlist order |
| `children` | array | playlists and folders below |

History playlists and smart playlists (their criteria belong to the browser) are not exported. Waveforms are not exported.

### CSV (`.csv`)

RFC 4180: UTF-8, `\r\n` line ends, a field with a comma, quote or line break is quoted and quotes are doubled. One header
row, then one row per track:

`id,title,artist,album,genre,label,bpm,key,key_id,length_s,rating,color,date_added,bitrate,sample_rate,comment,tags,file`

`bpm` is text with two decimals (`128.50`); `tags` is `category: tag` joined by `; `. The other columns are the JSON
fields of the same name. Cues, grids and playlists are only in the JSON.

### M3U8 (`.m3u8`)

`#EXTM3U`, then the absolute path of every track that has a file.

## 2. The library database

`store.db` lives in the per-user data folder `…/openfader/` (`$XDG_DATA_HOME`, else `~/.local/share` on Linux,
`~/Library/Application Support` on macOS, `%LOCALAPPDATA%` on Windows). It is plain SQLite; `PRAGMA user_version` is the
schema version. Open it read-only while the app runs, or work on a copy (a library backup is a zip holding a consistent
copy as `store.db`). Edit it only with the app closed.

### Track identity

Every track of the Collection is a row of `collection`, and its key is **`t<id>`** (`collection.id`). Tables that say
"key" below hold that string. Edits made before a track had an id (and tracks of a device that are not in the Collection)
use a legacy key: `<file name>|<size>` or `meta|<title>|<artist>`.
Descriptive edits require an id or file: changing the title or artist of a `meta|title|artist` track would change its key.

### Collection

`collection`: one row per track.

| column | meaning |
|---|---|
| `id` | integer primary key, the `t<id>` key |
| `path` | audio file, unique |
| `title`, `artist`, `album`, `genre`, `label`, `comment` | text |
| `tkey` | the key as the source named it |
| `key_id` | 0..=23, as in the export; null when unknown |
| `bpm_x100`, `length_s`, `bitrate`, `sample_rate` | integers |
| `analysis_ref` | opaque reference of the library plugin the track came from |
| `artwork` | cover image path or null |
| `date_added` | `YYYY-MM-DD` text; `added_at` the same moment as unix seconds |
| `size`, `mtime` | the file's size and modification time (s) when last seen |
| `fingerprint` | hash of the decoded audio, used to find a moved or renamed file; null until computed |

Also: `removed(id)` (tracks of a source the user removed), `relocated(orig, path)` (files moved), `analysis_lock(key)`
(tracks whose analysis the user locked), `taglist(pos, key)` (the Tag List).

### Edits by track key (table `track` and children)

`track(key, rating, comment, color, cues_set, grid, grid_orig, title, artist, album, genre, label)`: a null column means no
edit, the source's value stands. An empty text value explicitly clears that field. The title, artist, album, genre, label
and comment overrides are saved together as one atomic edit and one undo step; they do not alter the `collection` row or file tags.
The library overlay applies these fields along with rating and comment, so exports use the edited values. Source libraries
are unchanged unless an existing two-way connection writes back the edits.
`rating` 0..=5. `color` 1..=8 (1 Pink, 2 Red, 3 Orange, 4 Yellow, 5 Green, 6 Aqua, 7 Blue, 8 Purple). `cues_set` is 1 when
the `cue` rows are the track's complete cue list (it may be empty). `grid` and `grid_orig` (the grid before the first edit)
are blobs of 8 bytes per beat, little-endian: `time_ms u32, bpm_x100 u16, bar_pos u16`.

`cue(key, pos, hot, time_ms, loop_end_ms, color, comment)`: `pos` orders the list; `hot` 0 is a memory cue, 1 and up are hot
cues A, B, …; `color` is `r << 16 | g << 8 | b` or null.

`phrase_edit(key, mood, list)`: manual phrase edits; `mood` 1 high, 2 mid, 3 low; `list` is 14 bytes per phrase, little-endian:
`beat u32, end_beat u32, kind u8 (0 Intro, 1 Verse, 2 Chorus, 3 Bridge, 4 Outro, 5 Up, 6 Down), variant u8, fill u32` (0 = no
fill-in). `gain(key, analysed_db, adjust_db)`: auto gain.

### Playlists and tags

`node(id, parent, name, kind, pos, smart)`: the playlist tree; `parent` is null at the top; `kind` 0 folder, 1 playlist,
2 smart playlist (`smart` holds its criteria as text); `pos` orders siblings. `entry(node, pos, key)`: the tracks of a playlist
in order. `mytag_category(id, name, pos)`, `mytag(id, category, name, pos)`, `track_mytag(key, tag)`: My Tags.

### History and performance

`history(id, day, key, played_at)`: one row per played track, `day` is `YYYY-MM-DD`, `played_at` unix seconds.
`hotcue_bank` / `hotcue_bank_cue`: Cue Banks. `matching(a, b)`: MATCHING pairs. `sequence(slot, bars, bank, events)`: saved
sampler sequences.

### Connections

A connection links the Collection to a library of another app (or a plain folder) through a library plugin.

`connection(id, plugin, path, label, mode, wins, last_sync, stamp, last_seq)`: `plugin` is the plugin name (empty for a plain
folder), `path` the source; `mode` 0 import only, 1 two-way; `wins` decides a conflict: 0 the source, 1 ours, 2 the newest;
`last_sync` unix seconds; `stamp` the source file's state at the last sync; `last_seq` the last `journal.seq` written back.
`source_map(conn, foreign_id, track, sig)`: the source's track id to `collection.id`, `sig` a hash of what the source last said.
`source_node(conn, foreign_id, node)`: the same for playlists and folders (`node.id`).

### Change journal

`journal_action(id, at, label, undone)`: one row per user action (`at` unix seconds, `undone` 1 after undo).
`journal(seq, action, tbl, op, old, new)`: one row per changed table row; `op` 0 insert, 1 update, 2 delete; `old` and `new` are
the row as a JSON object (null for the missing side). Undo applies `old`, redo applies `new`; write-back to a connection sends
what changed after `connection.last_seq`.

### Devices

`device`, `sync_playlist`, `device_backup`, `history_import`, `device_setting`, `device_sig`: known players, the playlists kept
in sync with them, their backups, imported play histories, their settings and the baseline of what a player held after our last
export. They describe players, not music; a reader of the Collection can ignore them.
