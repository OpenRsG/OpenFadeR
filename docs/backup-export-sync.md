# Backup, export and sync

Three different things. A backup is a safety copy of your library, an export writes music and playlists for a player or
another program, and a sync keeps a device you already exported to up to date.

| What | Where | What it holds | What it is for |
| --- | --- | --- | --- |
| Library backup | File > Library > Backup Library… / Restore Library… | A zip with the Collection database (`store.db`: tracks, playlists, cues, grids, ratings, tags, history, devices) and the settings. Answer yes to "back up music files as well?" and the audio is copied into a `music-backup` folder beside the zip. | A safety copy, and moving to a new installation that uses the same folders. |
| Export Collection | File > Export Collection | Text files: JSON, CSV, M3U8 ([format](library-format.md#1-open-export)). No audio. | Other programs. Restore Library does not read these files. |
| Device export | DEVICES window > EXPORT, or right-click a playlist > Export to a device | The ticked playlists, their audio and their analysis, written to a USB stick or folder in the ticked player formats. | Playing on a player, or on another computer that runs OpenFadeR. |
| Sync Manager | DEVICES window > SYNC MANAGER | The playlists you chose to keep on a device. SYNC NOW, or plugging the device in, writes what changed since the last time. | A stick you reuse. |
| Cue sync | Automatic (Preferences > Player settings > DEVICE > CUE SYNC) | Cues, hot cues, loops, beat grid and phrases of a track that is both in the Collection and on a device (same file name and size), kept the same on both copies. | Edits made on either side. |
| Update Collection, Import To Collection, Import Histories | DEVICES window, and the track menu on a device's tracks | Brings back what changed on the device: ratings, colours and comments, tracks and playlists copied into the Collection, what the players played. | Coming home with a stick. |
| Device backup | DEVICES window > BACKUPS | The library of a device (databases, analysis files, artwork, settings), not its music. | Before a risky write to a stick. |

Restore Library replaces the library of the installation it runs on. It copies a music file back only when nothing is at
the file's original path.

## Two computers

Every installation has its own Collection. Nothing merges two Collections automatically.

**A stick between them.** Keep the Collection on one computer and export to a stick (EXPORT, then Sync Manager for the
next times). On the other computer the stick shows under Devices and its tracks load without an import. Cue, loop and grid
edits made there are saved on the stick. Back on the first computer, cue sync brings them into the Collection when the stick
is plugged in, and UPDATE COLLECTION brings ratings, colours and comments. To also keep the tracks on the second computer,
use Import To Collection on the stick's tracks or on one of its playlists.

**Moving the whole library.** Make a library backup with the music, copy the zip and the `music-backup` folder, and use
Restore Library on the other computer. The Collection stores the full path of every file, so this fits two computers with
the same folders (the same system and the same user name). Between Windows and Linux the paths differ: the tracks are
listed as missing until File > Display All Missing Files relocates them.

**The database on a stick.** OpenFadeR keeps `store.db` in the per-user data folder. Setting the environment variable
`XDG_DATA_HOME` moves it on any system: the database is then `<XDG_DATA_HOME>/openfader/store.db`. Because paths are
stored in full, one database shared between Windows (`E:\Music\…`) and Linux (`/media/…/Music/…`) lists every track as
missing on the other system, and relocating them changes the paths for the first one. There is no portable mode with
paths relative to the database yet. Never open one database from two computers at once.
