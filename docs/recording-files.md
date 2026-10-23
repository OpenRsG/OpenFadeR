# Files written next to a recording

REC (the button in the mixer's master section) writes the mix to `rec-YYYYmmdd-HHMMSS.wav` in the recordings folder
(PREFERENCES > ADVANCED > RECORDINGS > LOCATION OF FILES). With "separate files when silence continues" one recording
makes several WAV files. Next to the files REC can write three more, each named after its recording: `rec-….cue`,
`rec-….tracklist.txt` and `rec-….controls.jsonl`. They are switched in the same RECORDINGS list:

| Setting | Default | File |
|---|---|---|
| Write a cue sheet next to each recording | on | `<recording>.cue`, one per WAV file |
| Write a tracklist next to each recording | on | `<recording>.tracklist.txt`, one per WAV file |
| Write a control log next to each recording | off | `<recording>.controls.jsonl`, one per recording |
| SHORTEST TRACK LISTED | 30 s | tracks that led the mix for less are left out of the cue sheet and tracklist |

Nothing is written for a file that none of the tracks reached, and the sheets are written when the recording ends. The control
log is written while it runs and may be switched on or off in the middle of a recording (it then covers the time it was on).
None of this touches the audio thread: the sheets are a few lines of text when REC stops, and the control log is collected on the
UI thread and written by a thread of its own.

## Which track is on the mix

The same rule as the session history's "on air": a deck is on air while it plays and its channel fader times its crossfader gain
(the A / THRU / B assignment and the curve count) is at least -20 dB. The track that leads the mix is the master deck while it is
on air, else the first deck on air (A to D). Each time another track (another deck, or another load on the same deck) takes
over, it gets a line, at the moment it took over, on the recorder's clock: ms of audio written so far, so it is the time in the WAV.
A deck that pauses keeps its place until another track takes over, so a pause and resume adds nothing. The time is measured to
about 35 ms (one mixer tick).

## Cue sheet

A standard cue sheet (UTF-8, one `FILE` per sheet). The `FILE` is the WAV, by name, so keep the two together. Every track is a
`TRACK` with its `TITLE` and `PERFORMER` (the deck's title and artist) and `INDEX 01 mm:ss:ff` (frames of 1/75 s). A recording
split into files gets one sheet per file, with times counted from that file's first frame; a track that plays through a split is in
both sheets, and only the part inside a file counts towards the shortest track.

```
FILE "rec-20261023-201500.wav" WAVE
  TRACK 01 AUDIO
    TITLE "Night Drive"
    PERFORMER "Some Artist"
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    TITLE "Low Tide"
    PERFORMER "Another Artist"
    INDEX 01 03:41:37
```

## Tracklist

The same tracks as plain text, one per line: time (`M:SS`), artist and title, then the tempo the deck showed when the track
took over and its key when it has them.

```
0:00  Some Artist - Night Drive  (126.0 BPM, 8A)
3:41  Another Artist - Low Tide  (124.0 BPM, 9A)
```

## Control log

`rec-….controls.jsonl` has one JSON object per line:

```json
{"t":12840,"control":"ch1.fader","value":0.742}
```

- `t`: milliseconds on the wall clock since REC was pressed (an armed recording counts from the button, not from the first audio).
- `control`: what moved, below.
- `value`: its new value, a number, `true` / `false` or a string.

The first lines hold the state of every control when the log began, so the log can be replayed from nothing. After that a line is
written when a control's value changed, and one control is written at most 20 times a second (50 ms apart); a fast move is
thinned out and its last position is always written. A hot cue pad pressed writes its own line each time.

`recording.file` lines (`"value": "rec-….wav"`) say when each WAV file of the recording was opened. A file's time zero is its
line's `t`; for a silence-started recording the first file opens a moment after `t` 0. Use them to line the log up with the audio.

| Control | Value |
|---|---|
| `deck1.load` … `deck4.load` | Artist - Title of the loaded track, empty = no track |
| `deckN.play` | playing |
| `deckN.cue` | the CUE button is lit: the playhead is on the cue point or a preview plays |
| `deckN.cues` | memory cues, hot cues and loops as `tag@time`, e.g. `A@0:12.3 B@0:45.0` (`M` = memory cue); a change is a cue set or deleted |
| `deckN.hotcue` | the letter of a hot cue pad that was pressed (`A` to `H`) |
| `deckN.loop`, `deckN.loop_beats` | a loop is active, and its length in whole beats (0 = none or not whole) |
| `deckN.sync`, `deckN.master` | SYNC latched, this deck is the master |
| `deckN.tempo` | the tempo fader position, 0 to 1, 0.5 = 0 % |
| `deckN.bpm` | the effective tempo in BPM, 0 = unknown (it follows the master while synced) |
| `chN.fader` | channel fader, 0 to 1 |
| `chN.trim`, `chN.eq.hi`, `chN.eq.mid`, `chN.eq.low`, `chN.cfx` | knob positions, 0 to 1, 0.5 = centre |
| `chN.xf` | crossfader assignment: 0 = A side, 1 = THRU, 2 = B side |
| `chN.cue`, `chN.send` | headphone CUE button, SEND FX amount (0 to 1) |
| `mixer.crossfader` | crossfader position, 0 = A, 1 = B |
| `mixer.master` | MASTER knob position, 0 to 1 |
| `fxU.assign`, `fxU.release`, `fxU.part` | Beat FX unit U (1, 2): assignment (0 to 3 = channel A to D, 4 = MASTER), RELEASE held, stem part |
| `fxU.S.on`, `fxU.S.effect`, `fxU.S.beat`, `fxU.S.level` | slot S (1 to 3) of the unit: ON, effect name, beat fraction, LEVEL / DEPTH (0 to 1) |
| `mergeU.on`, `mergeU.preset`, `mergeU.knob`, `mergeU.volume` | MERGE FX side U: held ON, preset name, build-up knob and noise volume |
| `send.*`, `lever.*`, `mic.*` | the plugin racks: `on`, `effect` (name), `level`, `param` (0 to 1); `lever.position` is the lever |

Decks and channels C and D are logged only in the 4-deck layout. Controls that a mapping, the keyboard, the mouse or a plugin
moves are all seen alike: the log reads the controls' state, not who moved them.

Reading it is one `jq` away: `jq -c 'select(.control | startswith("deck2."))' rec-….controls.jsonl`.

## For plugins

`recording_track` (during REC) tells a plugin when another track took over the mix, with the recorder's clock; `recording_stopped`
carries `sidecars`, the paths of the files written next to the recording. See [plugins.md](plugins.md#events).
