# Themes (skins)

A theme is one JSON file that restyles the whole UI (colours, fonts, sizes, spacing, animation times) and may ask for a layout, while
the app runs. Pick one in Preferences > Themes (click a gallery card), or in Preferences > Appearance > Color > SKIN; it applies at
once, in the main window and in the split-screen window, and the choice is kept by the theme's name. A theme file that is missing at
the next start falls back to Catppuccin Mocha (a line on stderr says so). "Skin" and "theme" are the same thing: the settings key, the folder and
the code say `skin`.

A theme can also choose the **workspace**, the way the window is laid out: the desktop, touch or stage workspace (see
[Structural presentation](#structural-presentation)). The stage workspace is built from tokens, so one JSON file can change where the
waveforms sit, how a deck panel looks and what the mixer shows. [Writing one](#writing-one) has the minimal theme, the token groups and four recipes.

Preferences > Appearance > DISPLAY TYPE > TEMPO DECIMAL PLACES switches BPM readouts between 1 and 2 decimals (default: 2).
It applies immediately to deck headers and jogs, browser tables, track info and BEAT FX, and is saved independently of the theme.
Regular and Pocket layouts share this preference. Compact jogs still hide the fractional part; playback, SYNC, stored BPM and the
editable grid-tempo field keep their original precision.

## Themes that come with the app

| Theme | Base | What it is |
|---|---|---|
| Catppuccin Mocha | dark | built in, the default: the official palette mapped onto every surface, text, accent, waveform, meter, stem and deck token; no file |
| Catppuccin Latte | light | built in, the light flavour of the same palette; no file |
| Catppuccin Frappé, Macchiato | dark | the other two flavours (files) |
| Rek | dark | neutral greys, blue accents, green play, amber cues |
| Rek Light | light | the light counterpart of Rek |
| Dracula | dark | vivid purple, pink and green on dark blue-grey |
| Nord | dark | cold polar-night greys with frost blue |
| Gruvbox Dark | dark | warm retro browns with yellow, orange and aqua |
| Tokyo Night | dark | deep indigo with neon blue, magenta and orange |
| So Pro | dark | near-black greys, a bright blue accent, a white platter, eight coloured pads in a 4 × 2 grid, the vertical 2-deck layout |
| Ocean, High Contrast | dark | small examples that list only a few tokens |
| Pocket | dark | the full Linux app with a touch workspace, focused/paired decks, persistent transport and mixer controls, and all desktop modes and commands |

Except the two built-ins, they are the files of the [OpenFadeR-Themes](https://github.com/OpenRsG/OpenFadeR-Themes) repository:
copy them into `~/.config/openfader/extension/skin/`, or point `OPENFADER_SKINS` at a checkout of it.

## Choosing, customizing and sharing (PREFERENCES > THEMES)

* **Gallery**: a card per theme, painted in the theme's own colours (window, deck, waveform, buttons, track list); the applied one
  has a blue frame and a tick. Click a card to apply it: colours, fonts, sizes and, when the theme has `layout` hints, the layout
  change at once.
* **CUSTOMIZE** opens the editor on the applied theme. Every token is there, in the groups SURFACES, TEXT, ACCENTS, WAVEFORM, DECKS
  AND METERS, TYPOGRAPHY, SIZES, STAGE, STYLE, LIBRARY TREE AND TRACK MENU and MOTION, with the doc line of ui/theme.slint as its hint.
  Every change shows in the whole app at once (the app is the preview).
  * a colour: click its swatch for the picker (hue, saturation, value and alpha sliders, the hex code, and the theme's own colours to
    pick from), or type `#rrggbb` / `#rrggbbaa` (the `#` is optional) and press Enter;
  * a size, weight, time or number: `−` and `+` step it (1 px, a weight by 100, 10 ms, 0.05) or type the value (`12`, `12px`, `80ms`,
    `0.2s`);
  * a token with a fixed set of values (`layout-profile`, `stage-deck`, `button-style`, ...): a drop-down of the named values;
  * a token with a range (`glow`, `bevel`, `wave-fade`, ...): a slider and a field; a typed value outside the range is refused with
    a message naming the range;
  * a font: type the family or pick one of the common ones; a family that is not installed falls back to the system's sans-serif;
  * **RESET** (per token) and **RESET ALL** go back to the values the theme had when the editor opened; edited tokens are shown in blue
    and bold;
  * **BASE** (dark / light) sets the values the tokens you did not touch have, and the menu bar's colours;
  * **SAVE AS** writes `<name>.json` in your themes folder (`~/.config/openfader/extension/skin/`) with every token, applies it and
    closes the editor; **DISCARD** drops the changes. The name of a built-in or bundled theme cannot be used; saving under the name of
    one of your own replaces it.
* **DUPLICATE** copies the applied theme into your folder as `<name> copy`; **DELETE** removes the applied theme when it is one of
  yours (bundled ones are not yours to delete); **IMPORT…** adds a theme file (a file chooser: zenity or kdialog); **OPEN FOLDER**
  opens your themes folder; **RESCAN** reads the folders again.

Folders are re-read while the app runs: a file added, edited or deleted shows within a second (the applied theme restyles at once, so
editing a JSON file in a text editor is a live preview), and the gallery and the SKIN row follow. Installing from code is
`skin::install_and_choose(path)` (what a file dropped on the window uses): it validates a file, copies it into your folder, rescans, applies the theme and returns its name.

### Install from a repository

PREFERENCES > THEMES > **GET THEMES** lists the themes of your GitHub repositories, painted like the gallery, with one button each:
**INSTALL**, **UPDATE** (you have a theme of that name of your own and the repository's differs) or **INSTALLED**, then **APPLY**. The
repositories are one list shared with GET PLUGINS (`~/.config/openfader/repositories.json`; `OpenRsG/OpenFadeR-Themes` is in it on
first run): type `owner/repo` and **ADD**, **x** removes one, **REFRESH** downloads them again. A repository's theme files are the
`*.json` files at its top or at most two folders deep (outside plugin folders) that set at least one valid token, read from
`https://github.com/<owner>/<repo>/archive/HEAD.zip`. Installing writes through the same store as IMPORT, so a built-in or bundled
theme name is never replaced and the theme is in the gallery above at once.

## Install by hand

Put `*.json` files in any of these folders (the first folder that has a name wins):

1. every folder listed in `$OPENFADER_SKINS` (`:`-separated),
2. `$XDG_CONFIG_HOME/openfader/extension/skin` (else `~/.config/openfader/extension/skin`): your themes, where the editor saves,
3. the bundled `extension/skin/` next to the executable or in a folder above it.

## Format

```json
{
  "name": "Ocean",
  "author": "me",
  "description": "Cool blues on a near-black window",
  "base": "dark",
  "layout": { "deck_layout": "vertical", "line_space": "small" },
  "tokens": {
    "accent": "#00b4d8",
    "wave-bg": "#050d14",
    "font-size": 12,
    "font": "Inter",
    "t-fast": "120ms"
  }
}
```

| Key | Meaning |
|---|---|
| `name` | What the gallery lists and the settings file stores. Optional (default: the file name without `.json`). `Catppuccin Mocha` and `Catppuccin Latte` are the built-in themes' names and cannot be reused; a name already taken by an earlier theme is skipped. |
| `author`, `description` | Text for the gallery card (optional). |
| `locales` | Translations of the description, per language of the app (`fr`, `es`, `de`, `pt`, `it`, `nl`): `{"fr": {"Cool blues on a near-black window": "Bleus froids sur fenêtre presque noire"}}`. The gallery shows the description in the UI language when it is there, else as written. |
| `base` | `"dark"` (default) or `"light"`: the built-in values every token you do not list keeps. It also picks the light or dark scheme of the menu bar and its menus, which Slint draws itself and which no token reaches. |
| `layout` | VIEW settings the theme sets when it is chosen (optional), see below. |
| `tokens` | Token name to value. Anything you leave out is the base's value, so list only what you change. Files written by the editor list every token. |

A theme sets *every* token (yours, else the base's), so switching from one theme to another, or back to a built-in one, never leaves
anything of the previous theme behind. Files from before `author`, `description` and `layout` existed load unchanged.

### Older themes

The Studio workspace (`layout-profile` 2) was removed; Stage replaced it. A saved Studio selection reads as Catppuccin Mocha when the
theme file is gone (stderr names it) and keeps working when you have a copy of `studio.json` in your own themes folder, which is never
deleted: it loads as a Stage theme. Reading a file migrates it, nothing is rewritten until you save from the editor: `layout-profile` 3
(Stage's old number) is 2 now, and the six Studio sizes are the Stage sizes of the same name (`studio-radius` → `stage-radius`,
`studio-radius-ctl`, `studio-button`, `studio-gap`, `studio-cover`, `studio-row` likewise; a theme that sets both keeps the `stage-*` one).

### Tokens that follow another token

A token whose default is another token follows it: `panel-top` (the top of a Stage panel's gradient, the bottom is `panel`) is
`panel` unless the theme sets it, so a theme that sets `panel` and not `panel-top` gets flat panels in its own colour, on either base,
whatever theme was applied before. The Tokens tables say `follows panel` in the Default column. The theme editor does the same: moving
`panel` moves `panel-top` along while the two are equal; give `panel-top` its own colour for a gradient.

`font-display` is empty by default. UI code reads the computed `display-font` (`Theme.display-font`), which is `font-display`, or
`font` when that is empty. It is not a token: a theme cannot set it, it follows the two.

### Waveform colours per deck

`wave-style` 0 (the default) draws every deck's waveform with the shared bands `wave-low`, `wave-mid` and `wave-high`. `wave-style` 1
gives each deck its own: the low band is `deckN`, the mid band is `deckN-hi` and the high band stays `wave-high`, where N is the deck
that owns the waveform (the deck itself, not the panel that shows it: DECK 3 and DECK 4 of a 2-deck controller use `deck3` and `deck4`
in whichever panel they appear). It covers the enlarged waveform, the overview and the vertical waveform, in every workspace, and
follows the VIEW waveform colour setting: THREE BAND uses the three bands, RGB maps the red, green and blue of the stored waveform
onto the low, mid and high band, BLUE draws `deckN` (whiter where the stored waveform is whiter). The preview column of the track
list belongs to no deck and keeps the shared bands. The waveforms are rebuilt when the theme changes.

`wave-fade` (0 to 1) darkens each band towards its outer edge, so a waveform is bright in the middle and dim at the peaks; 0 is the
flat look. It applies to every waveform.

### Layout hints

`layout` maps VIEW settings (their names in the settings file, the same values PREFERENCES shows) to the value the theme wants.
They are applied once, when the theme is chosen (a click in the gallery or the SKIN row), not at every start, so the user stays free
to change the layout afterwards. A key that is not in this table, or a value the setting does not accept, is reported on stderr and
ignored.

| Key | Values |
|---|---|
| `deck_layout` | `horizontal`, `vertical` (the enlarged waveforms stacked above the decks, or side by side between them) |
| `decks` | `2`, `4` |
| `waveform_colors` | `threeband`, `rgb`, `blue` |
| `hot_cue_colors` | `colorful`, `classic`, `cold1`, `cold2`, `pads` (eight distinct pad colours, by pad) |
| `pad_fx_color` | `blue`, `green`, `orange`, `red`, `purple` |
| `sampler_color` | `frequency`, `purple` |
| `line_space` | `small`, `medium`, `large` (row height of the browse lists: 18, 22, 28 px) |
| `browse_font` | `small`, `normal`, `large` (10, 11, 13 px) |
| `browse_bold` | `true`, `false` |
| `overview` | `full`, `half` |
| `show_mixer` | `true`, `false` (the mixer column and strip; hidden, the vertical waveform columns sit together and grow) |

Sizes of the regions (deck height, waveform heights, the jog, the mixer channel) are tokens, not hints, and apply with the theme.

### Structural presentation

Themes can select a different presentation, not just recolor the desktop. The `layout-profile` token is `desktop` (0), `touch` (1) or `stage` (2). These use the same Linux application, engine, Collection, plugins and callbacks; `mobile/` is the separate Android frontend and is not the Linux phone application.

Pocket (`extension/skin/pocket.json`) selects the touch presentation. Its workspace selector exposes decks/export players, Library, mixer/output, effects, sampler, Mix Point Link, Beat Lab and all commands. EDIT, LIGHTING and VIDEO use the same mode selector as desktop; optional features retain their normal preference/extension switches. Two or four decks are available. Below `touch-breakpoint`, select a deck; wider windows show a pair. All eight pads and all desktop pad modes, cue/loop editing and grid tools remain available.

Play/pause, transport CUE, channel levels, headphone cues and the crossfader remain available while navigating. The Library separates tracks, sources/playlists, track info and actions, with explicit deck-load buttons. Preferences uses a category selector and stacked form labels. Wide editors and dense effect/sample panels retain their complete controls in a scrollable workspace; narrow tables can scroll their columns.

Library → Actions includes every browser sort column, direction and a headphone-preview waveform that seeks without loading a deck. Mix Point Link keeps a selected deck's editable waveform in the same workspace as MIX IN/OUT and SILENT, with separate touch buttons for deck and linked-lane zoom.

`touch-target` sizes primary touch controls; `icon-size` sizes their centered glyphs independently. Shared Slint controls use `Theme.target-size(base)` to retain their desktop size in profile `0` and enforce the touch target in profile `1`. The existing font, spacing and region tokens still apply. Switching to a desktop theme restores the desktop presentation without changing or copying library data. These tokens are editable and saved by the existing theme editor.

For the full Linux app's viewport check (Xvfb, Openbox, xdotool and ImageMagick required):

```sh
xvfb-run -a -s '-screen 0 1440x1400x24' sh -c \
  'openbox & python3 ui/check-touch.py target/release/openfader'
```

This runs the actual binary with disposable settings and checks compact portrait/landscape window sizes, saving screenshots and logs. It is not proof of physical audio output, controllers or device export.

### Stage workspace (`layout-profile` 2)

Stage is a two-deck performance workspace built in `ui/stage/*.slint` from the same globals as the desktop workspace (`DeckState`, `MixerState`,
`FxState`, `PadsState`, `BrowserState`, ...), so every control changes the real deck, mixer, effect, pad or library state, and a tab or
button with no backing feature does not appear. What differs is that its look is tokens: one theme file decides where the waveforms
sit, which deck panel is drawn, how pads, buttons and the jog look and what the mixer and the strip hold. A deck panel shows the deck
of its slot (`DeckState.slots`), so DECK 3 and DECK 4 of a 2-deck controller use the same two panels. The sketches below show the
arrangement only, not the proportions.

**Waveforms (`stage-wave`)**: 0 stacked across the top, 1 in a centre column between the decks, 2 inside each deck panel.

```text
stage-wave 0
+-----------------------------------------------+
| top bar                                       |
+-----------------------------------------------+
| wave 1  ~~~~~~~~~~~~~~~~~|~~~~~~~~~~~~~~~~~~~ |
| wave 2  ~~~~~~~~~~~~~~~~~|~~~~~~~~~~~~~~~~~~~ |
+----------------+---------------+--------------+
| deck 1         |     mixer     |        deck 2|
| panel          |               |         panel|
+----------------+---------------+--------------+
| strip (stage-strip 1), then the library       |
+-----------------------------------------------+

stage-wave 1
+-----------------------------------------------+
| top bar                                       |
+----------------+---------------+--------------+
| deck 1         | wave 1 ~~|~~  |        deck 2|
| panel          | wave 2 ~~|~~  |         panel|
|                +---------------+              |
|                |     mixer     |              |
+----------------+---------------+--------------+
| strip (stage-strip 1), then the library       |
+-----------------------------------------------+

stage-wave 2
+-----------------------------------------------+
| top bar                                       |
+----------------+---------------+--------------+
| wave ~~|~~     |               | wave ~~|~~   |
| overview ~~~   |     mixer     |  overview ~~~|
| controls       |               |      controls|
+----------------+---------------+--------------+
| strip (stage-strip 1), then the library       |
+-----------------------------------------------+
```

**Deck panel (`stage-deck`)**, drawn for deck 1; deck 2 is the mirror image, with the controls on the other side. The pads, the
mixer and the strip come from their own tokens; the sketches show the character of each style.

```text
stage-deck 0, performance
+-------------------------------+
| cover  title        BPM  key  |
|        artist       genre     |
| overview ~~~~~~~~~~~~~~~~~~~~ |
| [1][2][3][4]   LOOP  IN  OUT  |
| [5][6][7][8]        1/2       |
| BEAT FX  ECHO  (o) (o) (o)    |
| FILTER  (  jog ring: BPM,key )|
| (CUE) (PLAY)    SYNC  MASTER  |
+-------------------------------+

stage-deck 1, turntable
+-------------------------------+
| cover  title        BPM  key  |
| HOT CUE | BEAT LOOP | SLICER  |
|  .------.        A 0:32.1  x  |
| /  vinyl  \      B 1:04.5  x  |
| | platter |      C 1:36.9  x  |
|  \ cover  /      tempo  +-10  |
|   '------'       fader  [|]   |
| CUE   PLAY   SYNC             |
+-------------------------------+

stage-deck 2, controller
+-------------------------------+
| 1   title           BPM  key  |
| wave ~~~~|~~~~  (stage-wave 2)|
| LOW MID HI CUE LPF HPF LEVEL  |
| (o) (o) (o) (o) (o) (o) (o)   |
| [ CUE ]  [ PLAY ]  [ SYNC ]   |
| SLIP     VINYL     Q          |
+-------------------------------+

stage-deck 3, ring
+-------------------------------+
| title / artist                |
|       .---------.             |
|      /  128.0    \   tempo    |
|     |    0.0 %    |   fader   |
|      \           /    [|]     |
|       '---------'             |
| (PLAY) (CUE)   QUANTIZE (o)   |
|                KEY SYNC (o)   |
+-------------------------------+
```

What each token picks (values in the Tokens tables):

* `stage-pads`: the hot cue pads as a numbered grid 1-8 (outlined, the lit one in the deck colour), a cue list (rows A-F with the cue
  time and a delete cross) or a grid filled in each cue's colour.
* `stage-mixer`: two channels, or four (decks 3 and 4 follow the controller's layers). `stage-tabs`: none, MIXER / FX / REC or
  MIXER / VIDEO / LIGHTING over the mixer. `stage-mixer-w` is its width.
* `stage-strip` 1: a strip under the decks, EFFECTS (a BEAT FX drop-down, the effect, its knobs) on the left, SOUND COLOR FX and the
  SAMPLER pads on the right.
* `stage-mirror` 0: the right-hand deck has the left deck's arrangement; 1 (default): mirrored, its controls on the outer side.
* `stage-list-wave` 1: the track list gets a waveform preview column and a badge naming the deck a track is loaded on. `stage-sidebar`: library side
  bar with icon and label, or icons only. The list also shows ALBUM from 1000 px wide and DATE ADDED from 1180 px wide.
* `wave-ruler` 1: a row of bar numbers (counted from the first downbeat of the grid; 1, 9, 17 … at wide zoom, every bar when zoomed in)
  over the enlarged waveforms at the top or in the centre column.
* Sizes: `stage-wave-h` (each enlarged waveform), `stage-gap`, `stage-radius` (panels), `stage-radius-ctl` (buttons, pads, tabs,
  fields), `stage-cover`, `stage-row` (track list row), `stage-button` (big CUE / PLAY / SYNC), `stage-jog`.
* Looks (Style group): `button-style` filled, outlined (the lit one glows) or raised (bevelled caps); `jog-style` ring with the BPM
  inside, vinyl platter with the cover art, or a thin ring with beat ticks; `glow` (0 to 1) neon glow round lit controls, the jog ring
  and the playheads; `bevel` (0 to 1) raised and inset shading of buttons, knobs and panels; `panel-top`, `panel` and `panel-border`
  the vertical gradient and the outline of every panel; `deck-tint` 1 colours each deck's controls (jog arc, PLAY and CUE rims, lit
  pads, knob arcs, meters, the deck badge) with its `deckN`; `font-display` and `weight-display` the big figures.

The Stage workspace is the two-deck PERFORMANCE mode. The 4-deck layouts, EXPORT, EDIT, LIGHTING and VIDEO keep the desktop workspace, with its own global bar whose mode selector comes back to Stage. The window grows to the smallest size the tokens fit (at least 1100 × 720 px). Switching to any other theme restores that theme's workspace; the library, the decks, the mixer and the effects keep their values.

### Value forms

| Type | Written as |
|---|---|
| color | `"#rrggbb"` or `"#rrggbbaa"` (alpha last) |
| length | a number (logical px) or `"12px"` |
| string | text, e.g. a font family name |
| int | a whole number, e.g. a font weight (`400`, `700`). Some tokens take only a fixed set of values (`layout-profile`, `pad-style`, every `stage-*` choice, `button-style`, `jog-style`, `deck-tint`, `wave-style`): write the option's name (`"stage-deck": "turntable"`, any capitals) or its number; the Values column of the Tokens tables lists both. The editor writes names |
| float | a number. `glow`, `bevel`, `wave-fade`, `key-sat` and `key-val` take 0 to 1 |
| duration | a number of milliseconds, `"80ms"` or `"0.2s"` |

### Errors

A token that is unknown, of the wrong type or has a bad value (`"red"` is not a colour) is reported on stderr as
`skin <file>: token "accent": bad colour "red", use #rrggbb or #rrggbbaa` and left at the base's value; the rest of the theme still
applies. So is a value outside what the token allows: `skin <file>: token "stage-deck": 7 is not allowed, use 0 (performance), 1
(turntable), 2 (controller), 3 (ring)` (a name that is not one of them is reported the same way) or `token "glow": 1.5 is out of range, use 0 to 1`. A file that is not valid JSON, or has no JSON
object at its top, holds no theme and is skipped. IMPORT and `install` refuse a file with no valid token at all.

## Tokens

The tokens are the `in-out` properties of the `Theme` global in `ui/theme.slint`, which stays the one source of truth: `ui/build.rs`
reads them (name, type, the `// @group` they sit under, their doc comment, a `// @values a | b | c` or `// @range 0..1` line for the
values they accept, and a default of `root.other` for a token that follows another) and the skin code and the editor use them, so a
token added there is themable, editable and listed in the editor without any Rust change. The tables below list every token with its
type, its default on the dark and the light base, the values it accepts and its doc comment; the test `the_docs_list_every_token`
fails when a token is missing from them. The `in` properties (`light`, `tooltips`, `h-row`, `list-font`, `list-weight`) are set by the
VIEW preferences and cannot be themed.

Tokens that Rust code reads to draw images instead of Slint elements: `wave-bg`, `wave-low`, `wave-mid`, `wave-high`, `wave-fade`,
`wave-style` with `deck1` to `deck4` and their `-hi` twins (the deck waveforms and overviews, the mix point lanes, the EDIT and browser
strips) and `cue`, `memory-cue` (the cue markers). The waveform bands overlap in fixed mixes of the three colours; a theme that changes
a band gets the mean of the overlapping bands instead.

### Surfaces (backgrounds, lines, selection, overlays and the toast)

| Token | Type | Default (dark / light if they differ) | Values | Meaning |
|---|---|---|---|---|
| `bg` | color | `#11111b` / `#dce0e8` |  | Window and the 1-2 px gaps between panels. |
| `bg-deep` | color | `#181825` / `#eff1f5` |  | Waveform, track list and status bar background. |
| `panel` | color | `#1e1e2e` / `#e6e9ef` |  | Panel body (deck panel, mixer, browser toolbar, tree). |
| `panel-raised` | color | `#313244` / `#ccd0da` |  | Idle button / pad / input fill. |
| `panel-high` | color | `#45475a` / `#bcc0cc` |  | Hovered button, fader groove, thumb-less borders. |
| `deck-bg` | color | `#1e1e2e` / `#e6e9ef` |  | The deck panels' body (the header, overview, pads and jog sit on it). |
| `row-alt` | color | `#1b1b2a` / `#eaecf1` |  | Odd track-list row (even rows are `bg-deep`). |
| `list-head` | color | `#313244` / `#ccd0da` |  | Column header strip of the track list. |
| `line` | color | `#313244` / `#ccd0da` |  | Hairline between panels / table cells. |
| `line-strong` | color | `#585b70` / `#9ca0b0` |  | Checkbox / radio / input border. |
| `select` | color | `#425173` / `#b0c7f5` |  | Selected track-list row and tree entry. |
| `hover-row` | color | `#282e43` / `#d6e0f5` |  | Row under the pointer. |
| `scrim` | color | `#11111bb0` / `#dce0e8b0` |  | Dim overlay behind dialogs. |
| `fader-cap` | color | `#a6adc8` / `#8c8fa1` |  | Fader cap face. |
| `deck-frame` | color | `#00000000` |  | Outline of a deck panel (see `deck-frame-w`; transparent = none). A themed accent colour draws one around each deck. |
| `overlay-scrim` | color | `#11111bcc` / `#dce0e8cc` |  | Dim overlay behind the MIDI learn screen. |
| `overlay-bg` | color | `#181825` / `#1c1c1c` |  | Card of the MIDI learn overlay. |
| `overlay-border` | color | `#fab387` / `#fe640b` |  | Border of the learn overlay's card. |
| `overlay-title` | color | `#fab387` / `#fe640b` |  | Title of the learn overlay. |
| `overlay-text` | color | `#cdd6f4` / `#ffffff` |  | Main text of the learn overlay. |
| `overlay-soft` | color | `#bac2de` / `#cccccc` |  | Secondary text of the learn overlay. |
| `overlay-dim` | color | `#7f849c` / `#888888` |  | Dim text of the learn overlay. |
| `overlay-faint` | color | `#6c7086` / `#666666` |  | Faintest text of the learn overlay. |
| `overlay-ok` | color | `#a6e3a1` / `#79bc6b` |  | A mapped control in the learn overlay. |
| `overlay-bound` | color | `#94e2d5` / `#40a02b` |  | A control that is bound in the learn overlay. |
| `overlay-warn` | color | `#f9e2af` / `#fe640b` |  | A warning in the learn overlay. |
| `toast-bg` | color | `#313244f2` / `#2a2a2af2` |  | Background of the toast (the notice that fades in over the window). |
| `toast-text` | color | `#cdd6f4` / `#eeeeee` |  | Text of the toast. |
| `toast-shadow` | color | `#11111baa` / `#000000aa` |  | Shadow of the toast. |

`deck-bg` is the deck panels' body, `list-head` the track list's column header strip, `deck-frame` the outline of a deck panel
(thickness: `deck-frame-w`, 0 = none).

### Text

| Token | Type | Default (dark / light if they differ) | Values | Meaning |
|---|---|---|---|---|
| `text-bright` | color | `#cdd6f4` / `#11111b` |  | The strongest text: titles, the label of a lit button, the track title. Must read on `accent` and `select`. |
| `text` | color | `#bac2de` / `#4c4f69` |  | Default text of lists, inputs and values. |
| `text-dim` | color | `#9399b2` / `#6c6f85` |  | Labels (TRIM, HIGH, MID, LOW), artist, secondary values. |
| `text-faint` | color | `#7f849c` / `#8c8fa1` |  | Disabled-looking glyphs, unit text. |
| `text-disabled` | color | `#585b70` / `#acb0be` |  | Text of a disabled control. |
| `row-dim` | color | `#9399b2` / `#6c6f85` |  | Secondary cells of the track list (artist, genre, comment, time). |
| `row-loaded` | color | `#cdd6f4` / `#11111b` |  | The cells of a track list row whose track a deck has loaded. |

`text-bright` is also the label of a lit button, so on a light theme it must read on `accent`. `row-dim` is the track list's secondary
cells (artist, genre, comment, time); `row-loaded` the cells of a row whose track a deck has loaded.

### Accents

| Token | Type | Default (dark / light if they differ) | Values | Meaning |
|---|---|---|---|---|
| `accent` | color | `#59719e` / `#1e66f5` |  | ON state of nearly every toggle: SYNC, MASTER, Q, MT, AU, bank, panel toggles. |
| `accent-hi` | color | `#89b4fa` / `#209fb5` |  | A lighter accent: headings of the preferences, a synced BPM, glyphs on dark. |
| `play` | color | `#a6e3a1` / `#40a02b` |  | PLAY / PAUSE ring and glyph. The loaded track's title is also this green. |
| `cue` | color | `#fab387` / `#fe640b` |  | CUE ring and cue point marker. |
| `sync` | color | `#59719e` / `#1e66f5` |  | The SYNC button when it is on. |
| `master` | color | `#59719e` / `#1e66f5` |  | The MASTER button when it is on. |
| `bpm-sync` | color | `#f9e2af` / `#df8e1d` |  | BPM SYNC icon when on. |
| `danger` | color | `#f38ba8` / `#d20f39` |  | Alerts, SLIP lit, memory cue passed. |
| `rec` | color | `#c6738c` / `#e64553` |  | REC button and recording indicator. |
| `memory-cue` | color | `#f5c2e7` / `#ea76cb` |  | Memory cue not yet reached. |
| `sampler` | color | `#cba6f7` / `#8839ef` |  | Sampler slot play arrow. |

### Waveform

| Token | Type | Default (dark / light if they differ) | Values | Meaning |
|---|---|---|---|---|
| `wave-bg` | color | `#11111b` / `#eff1f5` |  | Background of the waveforms and overviews (drawn by Rust). |
| `wave-low` | color | `#89b4fa` / `#1e66f5` |  | Low (bass) band of the waveforms. |
| `wave-mid` | color | `#fab387` / `#fe640b` |  | Mid band of the waveforms. |
| `wave-high` | color | `#cdd6f4` / `#5c5f77` |  | High band of the waveforms. |
| `wave-playhead` | color | `#f5e0dc` / `#4c4f69` |  | Playhead of the enlarged waveform and the slip ghost (the shadow playhead while SLIP diverges). |
| `slip-ghost` | color | `#89dceb` / `#04a5e5` |  | The shadow playhead drawn while SLIP diverges. |
| `wave-fade` | float | `0.0` | `0 to 1` | How much each waveform band darkens towards its outer edge (0..1): 0 = one flat colour per band, 1 = black at the edge, so the bright core stands out. Applies to every waveform. |

### Decks and meters

| Token | Type | Default (dark / light if they differ) | Values | Meaning |
|---|---|---|---|---|
| `stem-vocal` | color | `#a6e3a1` / `#40a02b` |  | Stem accents: VOCAL green, INST red, DRUMS blue. |
| `stem-inst` | color | `#f38ba8` / `#d20f39` |  | INST stem. |
| `stem-drums` | color | `#89b4fa` / `#1e66f5` |  | DRUMS stem. |
| `stem-bass` | color | `#fab387` / `#fe640b` |  | BASS stem (4 stems). |
| `deck1` | color | `#89b4fa` / `#1e66f5` |  | Deck number badges and "loaded on deck n" markers only. |
| `deck2` | color | `#fab387` / `#fe640b` |  | Deck 2. |
| `deck3` | color | `#cba6f7` / `#8839ef` |  | Deck 3. |
| `deck4` | color | `#94e2d5` / `#179299` |  | Deck 4. |
| `jog-ring` | color | `#45475a` / `#bcc0cc` |  | The jog's outer ring and the disc inside it. |
| `jog-face` | color | `#11111b` / `#eff1f5` |  | The disc inside the jog's ring. |
| `jog-arc` | color | `#f38ba8` / `#d20f39` |  | The playhead arc on the jog's ring. |
| `jog-bpm` | color | `#cdd6f4` / `#4c4f69` |  | The big BPM figure in the jog. |
| `jog-text` | color | `#a6adc8` / `#6c6f85` |  | The tempo offset text in the jog. |
| `jog-range` | color | `#a6e3a1` / `#40a02b` |  | The tempo range text (±6 %) in the jog. |
| `pad-style` | int | `0` | `0` list, `1` grid | Hot cue pads: 0 = a list (2 columns of 4 rows, a letter badge and the cue time), 1 = a grid (4 columns of 2 rows, every pad filled in its cue's colour). |
| `key-sat` | float | `0.45` / `0.75` | `0 to 1` | Saturation and brightness (0..1) of the key column's Camelot colours (the hue is the key's). |
| `key-val` | float | `1.0` / `0.75` | `0 to 1` | Brightness (0..1) of the key column's colours. |
| `deck-title` | length | `14px` |  | Size of the deck header's title. |
| `deck-sub` | length | `12px` |  | Size of the artist line under the deck title. |
| `deck-bpm` | length | `12px` |  | Size of the BPM next to the artist (above `deck-sub` it is bold). |
| `meter-lo` | color | `#a6e3a1` / `#40a02b` |  | Level meters: the quiet part. |
| `meter-mid` | color | `#f9e2af` / `#df8e1d` |  | Level meters: the middle part. |
| `meter-hi` | color | `#f38ba8` / `#d20f39` |  | Level meters: the loud part. |

The jog is a ring (`jog-ring`) around a disc (`jog-face`) with the playhead arc (`jog-arc`), the BPM (`jog-bpm`), the tempo offset
(`jog-text`) and the range (`jog-range`). `pad-style` 0 is the hot cue list (2 columns of 4, a letter badge and the cue
time), 1 is a grid (4 columns of 2, each pad filled in its cue's colour). `key-sat` and `key-val` (0 to 1) are the saturation
and brightness of the key column's colours, whose hue is the key's.

`deck-title`, `deck-sub` and `deck-bpm` are the sizes of the deck header's title, the artist under it and the BPM beside the artist (a
`deck-bpm` above `deck-sub` makes it bold: a large BPM).

### Typography

| Token | Type | Default (dark / light if they differ) | Values | Meaning |
|---|---|---|---|---|
| `font` | string | `"Liberation Sans"` |  | Family of every text. A name that is not installed falls back to the system's default sans-serif. |
| `font-xs` | length | `9px` |  | Smallest text: badges and tiny labels. |
| `font-sm` | length | `10px` |  | Small text: labels, buttons, hints. |
| `font-size` | length | `11px` |  | Default text (track list, tree, inputs). |
| `font-md` | length | `12px` |  | Medium text: headings of the preferences, dialog titles. |
| `font-lg` | length | `14px` |  | Deck title, elapsed / remaining time. |
| `font-xl` | length | `18px` |  | Large text. |
| `font-xxl` | length | `22px` |  | Jog-centre BPM integer part. |
| `weight` | int | `400` |  | Normal font weight. |
| `weight-bold` | int | `700` |  | Bold font weight (buttons, labels). |
| `font-toast` | length | `13px` |  | Text size of the toast. |
| `weight-semi` | int | `600` |  | Semi-bold font weight. |

### Sizes (spacing, radii and the shared regions)

| Token | Type | Default (dark / light if they differ) | Values | Meaning |
|---|---|---|---|---|
| `layout-profile` | int | `0` | `0` desktop, `1` touch, `2` stage | Presentation profile: 0 desktop workspace, 1 touch workspace, 2 stage workspace (one window: waveforms, a deck panel per side, a central mixer, the library under them; its look is the Stage tokens below). Same application features in all three. |
| `touch-breakpoint` | length | `760px` |  | Width below which the touch workspace presents one selected deck instead of two columns. |
| `touch-target` | length | `44px` |  | Minimum primary control size in the touch presentation. |
| `icon-size` | length | `14px` |  | Glyph size inside shared buttons, independent of the control's hit area. |
| `gap` | length | `2px` |  | Gap between panels. |
| `space-xs` | length | `2px` |  | Spacing, smallest step. |
| `space-sm` | length | `4px` |  | Spacing, small step. |
| `space-md` | length | `6px` |  | Spacing, medium step. |
| `space-lg` | length | `8px` |  | Spacing, large step. |
| `space-xl` | length | `12px` |  | Spacing, largest step. |
| `radius` | length | `2px` |  | Corner radius of buttons and inputs. |
| `radius-lg` | length | `4px` |  | Corner radius of cards and dialogs. |
| `radius-xl` | length | `6px` |  | Corner radius, largest. |
| `deck-frame-w` | length | `0px` |  | Thickness of the `deck-frame` outline around each deck panel. |
| `h-bar` | length | `28px` |  | Height of the global bar (and dialog title bars). |
| `h-status` | length | `22px` |  | Height of the status bar. |
| `h-ctl` | length | `22px` |  | Height of buttons and inputs. |
| `h-ctl-sm` | length | `18px` |  | Height of small buttons. |
| `h-header` | length | `20px` |  | Height of panel and column headers. |
| `h-tab` | length | `22px` |  | Height of the browser tabs. |
| `h-toolbar` | length | `28px` |  | Height of the browser toolbar and the mixer strip. |
| `h-wave-2` | length | `56px` |  | Height of each enlarged waveform with 2 decks (horizontal layout). |
| `h-wave-4` | length | `44px` |  | Height of each enlarged waveform with 4 decks (horizontal layout). |
| `h-overview` | length | `36px` |  | Height of the deck's full-track waveform. |
| `h-phrase` | length | `14px` |  | Height of the phrase strip under it. |
| `h-deck-2` | length | `240px` |  | Height of a deck panel with 2 decks. |
| `h-deck-4` | length | `132px` |  | Height of a deck panel with 4 decks. |
| `h-fx` | length | `110px` |  | Optional panels (toggled from the global bar). |
| `h-mixpoint` | length | `96px` |  | Height of the MIX POINT LINK panel. |
| `h-sampler` | length | `96px` |  | Height of the sampler panel. |
| `w-channel` | length | `52px` |  | Width of one mixer channel. |
| `jog` | length | `112px` |  | Diameter of the jog. |
| `ring` | length | `36px` |  | Diameter of the CUE and PLAY buttons. |

### Stage (the stage workspace, `layout-profile` 2)

The tokens of the stage workspace, described under [Stage workspace](#stage-workspace-layout-profile-2). They have no effect in the other workspaces, except `wave-style` and `wave-fade` (Style), which also colour the desktop's waveforms.

| Token | Type | Default (dark / light if they differ) | Values | Meaning |
|---|---|---|---|---|
| `stage-wave` | int | `0` | `0` top, `1` centre, `2` decks | Stage workspace (layout-profile 2): where the enlarged waveforms sit. 0 = stacked across the top, 1 = in a centre column between the decks (above the mixer), 2 = inside each deck panel. |
| `stage-deck` | int | `0` | `0` performance, `1` turntable, `2` controller, `3` ring | Stage deck panel: 0 = performance (pad grid, LOOP, BEAT FX, jog ring), 1 = turntable (big platter with the cover art, hot cue list, tempo fader), 2 = controller (a row of EQ / filter knobs, big CUE / PLAY / SYNC buttons), 3 = ring (a large tempo ring with the BPM, round PLAY / CUE, tempo fader). |
| `stage-pads` | int | `0` | `0` numbered grid, `1` cue list, `2` coloured grid | Stage hot cue pads: 0 = numbered grid (1-8, outlined, the lit one in the deck colour), 1 = cue list (A-F rows with the cue time and a delete cross), 2 = grid filled in each cue's colour. |
| `stage-mixer` | int | `0` | `0` two channels, `1` four channels | Stage mixer: 0 = two channels, 1 = four channels (decks 3 and 4 follow the controller's layers). |
| `stage-tabs` | int | `0` | `0` none, `1` MIXER FX REC, `2` MIXER VIDEO LIGHTING | Tabs over the Stage mixer: 0 = none, 1 = MIXER / FX / REC, 2 = MIXER / VIDEO / LIGHTING. |
| `stage-strip` | int | `0` | `0` none, `1` effects and sampler | A strip under the Stage decks: 0 = none, 1 = EFFECTS (left), SOUND COLOR FX and SAMPLER (right). |
| `stage-list-wave` | int | `0` | `0` off, `1` on | Stage track list: 1 = a waveform preview column and a badge naming the deck a track is loaded on. |
| `stage-sidebar` | int | `0` | `0` labels, `1` icons only | Stage library side bar: 0 = icon and label per entry, 1 = icons only. |
| `stage-mirror` | int | `1` | `0` same, `1` mirrored | Stage right-hand deck: 0 = the same left-to-right arrangement as the left deck, 1 = mirrored (jog and buttons on the outer side). |
| `wave-ruler` | int | `0` | `0` off, `1` on | Stage workspace: 1 = a bar ruler (the bar numbers, stepping with the zoom) along the top of the enlarged waveforms stacked across the top or in the centre column; hidden while a track has no beat grid. |
| `stage-mixer-w` | length | `280px` |  | Width of the Stage mixer. |
| `stage-wave-h` | length | `64px` |  | Height of each enlarged waveform in the Stage workspace. |
| `stage-gap` | length | `8px` |  | Spacing between the Stage panels (and the window edge). |
| `stage-radius` | length | `10px` |  | Corner radius of the Stage panels. |
| `stage-radius-ctl` | length | `6px` |  | Corner radius of the Stage buttons, pads, tabs and fields. |
| `stage-cover` | length | `80px` |  | Side of the cover art in the Stage deck panels. |
| `stage-row` | length | `30px` |  | Row height of the Stage track list. |
| `stage-button` | length | `44px` |  | Height of the Stage big CUE / PLAY / SYNC buttons (diameter when round). |
| `stage-jog` | length | `180px` |  | Diameter of the Stage jog (ring or platter). |

### Style (looks shared by the stage workspace and the waveforms)

`glow`, `bevel`, `button-style`, `jog-style`, `panel-top`, `panel-border` and `deck-tint` are read by the stage workspace's controls. `wave-style` colours every deck waveform, in every workspace. `font-display` is the family of the big figures; UI code reads `display-font` (`Theme.display-font`), which is `font-display`, or `font` when `font-display` is empty (it is not a token).

| Token | Type | Default (dark / light if they differ) | Values | Meaning |
|---|---|---|---|---|
| `glow` | float | `0.0` | `0 to 1` | Neon glow (0..1) around lit controls, the jog ring and the playheads of the Stage workspace; 0 = none. |
| `bevel` | float | `0.0` | `0 to 1` | Raised / inset shading (0..1) of the Stage buttons, knobs and panels; 0 = flat. |
| `button-style` | int | `0` | `0` filled, `1` outlined, `2` raised | Stage buttons: 0 = filled, 1 = outlined (the lit one glows in its colour), 2 = raised (bevelled caps). |
| `jog-style` | int | `0` | `0` ring, `1` platter, `2` ticks | Stage jog: 0 = ring with the BPM inside, 1 = vinyl platter showing the cover art, 2 = thin ring with the beat ticks. |
| `panel-top` | color | follows `panel` |  | Top colour of the Stage panels' vertical gradient (the bottom is `panel`); `panel` = no gradient. |
| `panel-border` | color | `#00000000` |  | Outline of the Stage panels; transparent = none. |
| `deck-tint` | int | `0` | `0` off, `1` on | 1 = each deck's controls take its deck colour (jog arc, PLAY / CUE rims, lit pads, knob arcs, meters, deck badge); 0 = the shared accents. |
| `deck1-hi` | color | `#b4befe` / `#04a5e5` |  | Second colour of each deck: gradients and the mid band of its waveform when `wave-style` is 1. |
| `deck2-hi` | color | `#f9e2af` / `#df8e1d` |  | Deck 2's second colour. |
| `deck3-hi` | color | `#f5c2e7` / `#ea76cb` |  | Deck 3's second colour. |
| `deck4-hi` | color | `#a6e3a1` / `#40a02b` |  | Deck 4's second colour. |
| `wave-style` | int | `0` | `0` shared, `1` per deck | Waveform colours: 0 = the shared bands (`wave-low`, `wave-mid`, `wave-high`), 1 = per deck (low = `deckN`, mid = `deckN-hi`, high = `wave-high`). |
| `font-display` | string | `""` |  | Family of the big figures (BPM, times, deck numbers); empty = `font`. |
| `weight-display` | int | `600` |  | Weight of the big figures. |

### Library tree and track menu

The playlist tree's headings (My Playlists, History, Devices …) and the glyphs of its lists, and the track right-click menu
(six groups: PLAY, PLAYLISTS, USB STICK / SD CARD, ANALYSIS, TRACK INFO, FILE). `menu-focus` marks the item the controller's
browse knob or the arrow keys are on. Bigger orange headings, no glyphs on the lists and the menu in one column:

```json
{
  "name": "Booth Tree",
  "tokens": {
    "tree-heading": "#ff9e3b",
    "tree-heading-icon": "#ff9e3b",
    "tree-heading-grow": "4px",
    "tree-heading-extra": "16px",
    "tree-icons": 0,
    "tree-lines": 0,
    "menu-layout": 1,
    "menu-heading": "#ff9e3b",
    "menu-focus": "#2d6a4f"
  }
}
```

| Token | Type | Default (dark / light if they differ) | Values | Meaning |
|---|---|---|---|---|
| `tree-heading-grow` | length | `2px` |  | How much bigger than a list row the text of a tree heading (My Playlists, History, Devices …) is. |
| `tree-heading-extra` | length | `10px` |  | How much taller than a list row a tree heading is. |
| `tree-heading` | color | follows `text-bright` |  | Text of the tree headings. |
| `tree-heading-icon` | color | follows `accent-hi` |  | Glyph of the tree headings. |
| `tree-indent` | length | `14px` |  | Indent of each level of the tree. |
| `tree-icons` | int | `1` | `0` off, `1` on | The glyph before every list of the tree (playlist, folder, history day, stick …); headings keep theirs. |
| `tree-lines` | int | `1` | `0` off, `1` on | A hairline above each tree heading. |
| `menu-layout` | int | `0` | `0` two columns, `1` one column | Track right-click menu: its six groups side by side in two columns, or one under the other. |
| `menu-col-w` | length | `210px` |  | Width of a column of the track menu. |
| `menu-heading` | color | follows `accent-hi` |  | Group titles of the track menu (PLAY, PLAYLISTS, USB …) and their glyphs. |
| `menu-focus` | color | follows `accent` |  | The item the controller's browse knob (or the arrow keys) is on in an open menu. |
| `menu-lines` | int | `1` | `0` off, `1` on | A line between the groups of the track menu. |

### Motion

| Token | Type | Default (dark / light if they differ) | Values | Meaning |
|---|---|---|---|---|
| `t-fast` | duration | `80ms` |  | Hover and press fades of buttons. |
| `t-fade` | duration | `180ms` |  | Toast fade. |
| `tooltip-delay` | duration | `600ms` |  | Hover time before a tooltip shows. |

## Writing one

The smallest working theme is a name and the tokens that differ (the rest is the base's):

```json
{
  "name": "My blue",
  "base": "dark",
  "tokens": { "accent": "#00b4d8", "select": "#0b6a85" }
}
```

Where to start, by what you want to change (the Tokens tables list every token under these groups; the editor shows the same groups):

| You want | Tokens |
|---|---|
| Colours of windows, panels, lists | Surfaces: `bg`, `bg-deep`, `panel`, `panel-raised`, `panel-high`, `row-alt`, `line`, `select`, `hover-row` |
| Readable text | Text: `text-bright`, `text`, `text-dim`, `text-faint` (`text-bright` sits on `accent` and `select`) |
| Buttons that are on, PLAY, CUE, REC | Accents: `accent`, `accent-hi`, `play`, `cue`, `sync`, `master`, `danger`, `rec` |
| Waveforms | Waveform: `wave-bg`, `wave-low`, `wave-mid`, `wave-high`, `wave-fade`; per deck `wave-style`, `deck1` to `deck4` |
| Deck colours, jog, meters, pads | Decks and meters: `deck1` to `deck4`, `jog-*`, `meter-*`, `pad-style` |
| Fonts | Typography: `font`, `font-xs` to `font-xxl`, `weight`, `weight-bold` |
| Spacing, corners, heights | Sizes: `space-*`, `radius*`, `h-*`, `jog`, `ring`, and the touch tokens |
| A different window arrangement | `layout-profile` (`touch`, `stage`) and the Stage group |
| Glow, bevel, button and jog looks | Style: `glow`, `bevel`, `button-style`, `jog-style`, `panel-*`, `deck-tint` |
| Tree and track menu | Library tree and track menu |
| Speed of hover fades | Motion |

1. Easiest: PREFERENCES > THEMES, apply the theme closest to what you want, CUSTOMIZE, SAVE AS. By hand: copy
   `extension/skin/ocean.json` (coloured dark) or `extension/skin/rek-light.json` (light base) into
   `~/.config/openfader/extension/skin/` and change `name`.
2. List only what you change; start with the surfaces (`bg`, `panel*`, `text*`) and `accent`. A value equal to the base's adds
   nothing (the bundled themes leave it out; a file that lists it is still valid). A Stage theme spells out its arrangement (the
   `stage-*` choices) so the file reads as a recipe.
3. Write a choice by its name (`"stage-pads": "cue list"`), a length as a number (logical px) and a colour as `"#rrggbb"`. A token
   that follows another (`panel-top`, `menu-focus`, `tree-heading`, ...) needs no line unless you want it different.
4. Keep text readable on `select`, `hover-row` and `accent` (`text-bright` sits on them); `extension/skin/high-contrast.json` shows a
   high contrast set.
5. Preview: the theme shows within a second of saving the file (the app compares the folders once a second), the applied theme
   restyles at once, so keep the app open next to a text editor; stderr tells what was left out and why. A working folder of your own:
   `OPENFADER_SKINS=~/my-themes openfader`. In CUSTOMIZE every change shows in the whole app as you make it.
6. Share: a theme is one `.json` file, so send the file (IMPORT… or drop it on the window installs it), or put it in a GitHub
   repository (files at the top or at most two folders deep) that others add in GET THEMES. Fill `author` and `description` (and
   `locales` for translations) so the gallery card says what it is. Do not use the names or logos of other products in a theme, its
   description or its screenshots.

### Recipes

Neon dark with a colour per deck: black panels, deck 1 lime and deck 2 purple, glowing outlined controls, waveforms in the deck
colours fading towards their edges, four channels in the mixer. `panel-top` is not set, so it follows `panel` and the panels are flat.

```json
{
  "name": "Neon recipe",
  "base": "dark",
  "description": "Black panels, lime deck 1, purple deck 2, glowing controls",
  "tokens": {
    "layout-profile": "stage",
    "stage-wave": "top",
    "stage-deck": "performance",
    "stage-pads": "numbered grid",
    "stage-mixer": "four channels",
    "stage-list-wave": "on",
    "button-style": "outlined",
    "jog-style": "ring",
    "glow": 0.8,
    "deck-tint": "on",
    "wave-style": "per deck",
    "wave-fade": 0.6,
    "bg": "#050505",
    "panel": "#0b0b0d",
    "panel-border": "#26262c",
    "wave-bg": "#000000",
    "wave-high": "#ffffff",
    "deck1": "#b6ff1a",
    "deck1-hi": "#e4ff8a",
    "deck2": "#b44dff",
    "deck2-hi": "#dca8ff"
  }
}
```

Light bevelled: a light base, raised buttons, a gradient from a lighter top to the panel colour, a vinyl platter and a cue list.

```json
{
  "name": "Frost recipe",
  "base": "light",
  "description": "Light frosted grey, raised buttons, vinyl platter",
  "tokens": {
    "layout-profile": "stage",
    "stage-deck": "turntable",
    "stage-pads": "cue list",
    "stage-tabs": "MIXER FX REC",
    "bevel": 0.7,
    "button-style": "raised",
    "jog-style": "platter",
    "panel-top": "#f7f8fa",
    "panel": "#dfe2e8",
    "panel-border": "#b9bec9",
    "wave-style": "per deck",
    "deck1": "#2f7cf6",
    "deck2": "#ff6a3d"
  }
}
```

Centre waveform: both waveforms in a column between the decks, ring decks, filled pads, the effects strip, controls in the deck
colours.

```json
{
  "name": "Centre recipe",
  "description": "Waveforms between the decks, orange and blue",
  "tokens": {
    "layout-profile": "stage",
    "stage-wave": "centre",
    "stage-deck": "ring",
    "stage-pads": "coloured grid",
    "stage-strip": "effects and sampler",
    "deck-tint": "on",
    "jog-style": "ticks",
    "wave-style": "per deck",
    "wave-fade": 0.4,
    "deck1": "#ff8a1f",
    "deck1-hi": "#ffc27a",
    "deck2": "#2f8bff",
    "deck2-hi": "#8cc0ff"
  }
}
```

Per-deck waveforms on the desktop workspace (`layout-profile` stays `desktop`): only the waveform tokens.

```json
{
  "name": "Lime and purple recipe",
  "tokens": { "wave-style": "per deck", "wave-fade": 0.6, "deck1": "#a6e22e", "deck2": "#9b59ff" }
}
```
