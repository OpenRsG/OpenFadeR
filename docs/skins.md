# Themes (skins)

A theme is one JSON file that restyles the whole UI (colours, fonts, sizes, spacing, animation times) and may ask for a layout, while
the app runs. Pick one in PREFERENCES > THEMES (a gallery: click a card), or in PREFERENCES > VIEW > Color > SKIN; it applies at
once, in the main window and in the split-screen window, and the choice is kept by the theme's name. A theme file that is missing at
the next start falls back to Catppuccin Mocha (a line on stderr says so). "Skin" and "theme" are the same thing: the settings key, the folder and
the code say `skin`.

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

Except the two built-ins, they are the files of the [OpenFadeR-Themes](https://github.com/OpenRsG/OpenFadeR-Themes) repository:
copy them into `~/.config/openfader/extension/skin/`, or point `OPENFADER_SKINS` at a checkout of it.

## Choosing, customizing and sharing (PREFERENCES > THEMES)

* **Gallery**: a card per theme, painted in the theme's own colours (window, deck, waveform, buttons, track list); the applied one
  has a blue frame and a tick. Click a card to apply it: colours, fonts, sizes and, when the theme has `layout` hints, the layout
  change at once.
* **CUSTOMIZE** opens the editor on the applied theme. Every token is there, in the groups SURFACES, TEXT, ACCENTS, WAVEFORM, DECKS
  AND METERS, TYPOGRAPHY, SIZES and MOTION, with the doc line of ui/theme.slint as its hint. Every change shows in the whole app at
  once (the app is the preview).
  * a colour: click its swatch for the picker (hue, saturation, value and alpha sliders, the hex code, and the theme's own colours to
    pick from), or type `#rrggbb` / `#rrggbbaa` (the `#` is optional) and press Enter;
  * a size, weight, time or number: `−` and `+` step it (1 px, a weight by 100, 10 ms, 0.05) or type the value (`12`, `12px`, `80ms`,
    `0.2s`);
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
| `base` | `"dark"` (default) or `"light"`: the built-in values every token you do not list keeps. It also picks the light or dark scheme of the menu bar and its menus, which Slint draws itself and which no token reaches. |
| `layout` | VIEW settings the theme sets when it is chosen (optional), see below. |
| `tokens` | Token name to value. Anything you leave out is the base's value. Files written by the editor list every token. |

A theme sets *every* token (yours, else the base's), so switching from one theme to another, or back to a built-in one, never leaves
anything of the previous theme behind. Files from before `author`, `description` and `layout` existed load unchanged.

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

### Value forms

| Type | Written as |
|---|---|
| color | `"#rrggbb"` or `"#rrggbbaa"` (alpha last) |
| length | a number (logical px) or `"12px"` |
| string | text, e.g. a font family name |
| int | a whole number, e.g. a font weight (`400`, `700`) or `pad-style` |
| float | a number |
| duration | a number of milliseconds, `"80ms"` or `"0.2s"` |

### Errors

A token that is unknown, of the wrong type or has a bad value (`"red"` is not a colour) is reported on stderr as
`skin <file>: token "accent": bad colour "red", use #rrggbb or #rrggbbaa` and left at the base's value; the rest of the theme still
applies. A file that is not valid JSON, or has no JSON object at its top, holds no theme and is skipped. IMPORT and `install` refuse a
file with no valid token at all.

## Tokens

The tokens are the `in-out` properties of the `Theme` global in `ui/theme.slint`, which stays the one source of truth: `ui/build.rs`
reads them (name, type, the `// @group` they sit under and their doc comment) and the skin code and the editor use them, so a token
added there is themable, editable and listed in the editor without any Rust change. To list them:
`grep 'in-out property' ui/theme.slint`. The doc comment above each property says where it is used. The tables below are that list
at the time of writing (a test fails when a token is missing from them). The `in` properties (`light`, `tooltips`, `h-row`,
`list-font`, `list-weight`) are set by the VIEW preferences and cannot be themed.

Two kinds of token are used by Rust code that draws images instead of Slint elements: `wave-bg`, `wave-low`, `wave-mid`,
`wave-high` (the deck waveforms and overviews, the mix point lanes, the EDIT and browser strips) and `cue` (the cue point marker).
The waveform bands overlap in fixed mixes of the three colours; a theme that changes a band gets the mean of the
overlapping bands instead.

### Surfaces (backgrounds, lines, selection, overlays and the toast)

| Token | Type | Default (dark / light if they differ) |
|---|---|---|
| `bg` | color | `#11111b` / `#dce0e8` |
| `bg-deep` | color | `#181825` / `#eff1f5` |
| `panel` | color | `#1e1e2e` / `#e6e9ef` |
| `panel-raised` | color | `#313244` / `#ccd0da` |
| `panel-high` | color | `#45475a` / `#bcc0cc` |
| `deck-bg` | color | `#1e1e2e` / `#e6e9ef` |
| `row-alt` | color | `#1b1b2a` / `#eaecf1` |
| `list-head` | color | `#313244` / `#ccd0da` |
| `line` | color | `#313244` / `#ccd0da` |
| `line-strong` | color | `#585b70` / `#9ca0b0` |
| `select` | color | `#425173` / `#b0c7f5` |
| `hover-row` | color | `#282e43` / `#d6e0f5` |
| `scrim` | color | `#11111bb0` / `#dce0e8b0` |
| `fader-cap` | color | `#a6adc8` / `#8c8fa1` |
| `deck-frame` | color | `#00000000` |
| `overlay-scrim` | color | `#11111bcc` / `#dce0e8cc` |
| `overlay-bg` | color | `#181825` / `#1c1c1c` |
| `overlay-border` | color | `#fab387` / `#fe640b` |
| `overlay-title` | color | `#fab387` / `#fe640b` |
| `overlay-text` | color | `#cdd6f4` / `#ffffff` |
| `overlay-soft` | color | `#bac2de` / `#cccccc` |
| `overlay-dim` | color | `#7f849c` / `#888888` |
| `overlay-faint` | color | `#6c7086` / `#666666` |
| `overlay-ok` | color | `#a6e3a1` / `#79bc6b` |
| `overlay-bound` | color | `#94e2d5` / `#40a02b` |
| `overlay-warn` | color | `#f9e2af` / `#fe640b` |
| `toast-bg` | color | `#313244f2` / `#2a2a2af2` |
| `toast-text` | color | `#cdd6f4` / `#eeeeee` |
| `toast-shadow` | color | `#11111baa` / `#000000aa` |

`deck-bg` is the deck panels' body, `list-head` the track list's column header strip, `deck-frame` the outline of a deck panel
(thickness: `deck-frame-w`, 0 = none).

### Text

| Token | Type | Default (dark / light if they differ) |
|---|---|---|
| `text-bright` | color | `#cdd6f4` / `#11111b` |
| `text` | color | `#bac2de` / `#4c4f69` |
| `text-dim` | color | `#9399b2` / `#6c6f85` |
| `text-faint` | color | `#7f849c` / `#8c8fa1` |
| `text-disabled` | color | `#585b70` / `#acb0be` |
| `row-dim` | color | `#9399b2` / `#6c6f85` |
| `row-loaded` | color | `#cdd6f4` / `#11111b` |

`text-bright` is also the label of a lit button, so on a light theme it must read on `accent`. `row-dim` is the track list's secondary
cells (artist, genre, comment, time); `row-loaded` the cells of a row whose track a deck has loaded.

### Accents

| Token | Type | Default |
|---|---|---|
| `accent` | color | `#59719e` / `#1e66f5` |
| `accent-hi` | color | `#89b4fa` / `#209fb5` |
| `play` | color | `#a6e3a1` / `#40a02b` |
| `cue` | color | `#fab387` / `#fe640b` |
| `sync` | color | `#59719e` / `#1e66f5` |
| `master` | color | `#59719e` / `#1e66f5` |
| `bpm-sync` | color | `#f9e2af` / `#df8e1d` |
| `danger` | color | `#f38ba8` / `#d20f39` |
| `rec` | color | `#c6738c` / `#e64553` |
| `memory-cue` | color | `#f5c2e7` / `#ea76cb` |
| `sampler` | color | `#cba6f7` / `#8839ef` |

### Waveform

| Token | Type | Default |
|---|---|---|
| `wave-bg` | color | `#11111b` / `#eff1f5` |
| `wave-low` | color | `#89b4fa` / `#1e66f5` |
| `wave-mid` | color | `#fab387` / `#fe640b` |
| `wave-high` | color | `#cdd6f4` / `#5c5f77` |
| `wave-playhead` | color | `#f5e0dc` / `#4c4f69` |
| `slip-ghost` | color | `#89dceb` / `#04a5e5` |

### Decks and meters

| Token | Type | Default (dark / light if they differ) |
|---|---|---|
| `stem-vocal` | color | `#a6e3a1` / `#40a02b` |
| `stem-inst` | color | `#f38ba8` / `#d20f39` |
| `stem-drums` | color | `#89b4fa` / `#1e66f5` |
| `stem-bass` | color | `#fab387` / `#fe640b` |
| `deck1` | color | `#89b4fa` / `#1e66f5` |
| `deck2` | color | `#fab387` / `#fe640b` |
| `deck3` | color | `#cba6f7` / `#8839ef` |
| `deck4` | color | `#94e2d5` / `#179299` |
| `jog-ring` | color | `#45475a` / `#bcc0cc` |
| `jog-face` | color | `#11111b` / `#eff1f5` |
| `jog-arc` | color | `#f38ba8` / `#d20f39` |
| `jog-bpm` | color | `#cdd6f4` / `#4c4f69` |
| `jog-text` | color | `#a6adc8` / `#6c6f85` |
| `jog-range` | color | `#a6e3a1` / `#40a02b` |
| `pad-style` | int | `0` |
| `key-sat` | float | `0.45` / `0.75` |
| `key-val` | float | `1.0` / `0.75` |
| `deck-title` | length | `14px` |
| `deck-sub` | length | `12px` |
| `deck-bpm` | length | `12px` |
| `meter-lo` | color | `#a6e3a1` / `#40a02b` |
| `meter-mid` | color | `#f9e2af` / `#df8e1d` |
| `meter-hi` | color | `#f38ba8` / `#d20f39` |

The jog is a ring (`jog-ring`) around a disc (`jog-face`) with the playhead arc (`jog-arc`), the BPM (`jog-bpm`), the tempo offset
(`jog-text`) and the range (`jog-range`). `pad-style` 0 is the hot cue list (2 columns of 4, a letter badge and the cue
time), 1 is a grid (4 columns of 2, each pad filled in its cue's colour). `key-sat` and `key-val` (0 to 1) are the saturation
and brightness of the key column's colours, whose hue is the key's.

`deck-title`, `deck-sub` and `deck-bpm` are the sizes of the deck header's title, the artist under it and the BPM beside the artist (a
`deck-bpm` above `deck-sub` makes it bold: a large BPM).

### Typography

| Token | Type | Default |
|---|---|---|
| `font` | string | `"Liberation Sans"` |
| `font-xs` | length | `9px` |
| `font-sm` | length | `10px` |
| `font-size` | length | `11px` |
| `font-md` | length | `12px` |
| `font-lg` | length | `14px` |
| `font-xl` | length | `18px` |
| `font-xxl` | length | `22px` |
| `weight` | int | `400` |
| `weight-bold` | int | `700` |
| `font-toast` | length | `13px` |
| `weight-semi` | int | `600` |

### Sizes (spacing, radii and the shared regions)

| Token | Type | Default |
|---|---|---|
| `gap` | length | `2px` |
| `space-xs` | length | `2px` |
| `space-sm` | length | `4px` |
| `space-md` | length | `6px` |
| `space-lg` | length | `8px` |
| `space-xl` | length | `12px` |
| `radius` | length | `2px` |
| `radius-lg` | length | `4px` |
| `radius-xl` | length | `6px` |
| `deck-frame-w` | length | `0px` |
| `h-bar` | length | `28px` |
| `h-status` | length | `22px` |
| `h-ctl` | length | `22px` |
| `h-ctl-sm` | length | `18px` |
| `h-header` | length | `20px` |
| `h-tab` | length | `22px` |
| `h-toolbar` | length | `28px` |
| `h-wave-2` | length | `56px` |
| `h-wave-4` | length | `44px` |
| `h-overview` | length | `36px` |
| `h-phrase` | length | `14px` |
| `h-deck-2` | length | `240px` |
| `h-deck-4` | length | `132px` |
| `h-fx` | length | `62px` |
| `h-mixpoint` | length | `96px` |
| `h-sampler` | length | `96px` |
| `w-channel` | length | `52px` |
| `jog` | length | `112px` |
| `ring` | length | `36px` |

### Motion

| Token | Type | Default |
|---|---|---|
| `t-fast` | duration | `80ms` |
| `t-fade` | duration | `180ms` |
| `tooltip-delay` | duration | `600ms` |

## Writing one

1. Easiest: PREFERENCES > THEMES, apply the theme closest to what you want, CUSTOMIZE, SAVE AS. By hand: copy
   `extension/skin/ocean.json` (coloured dark) or `extension/skin/rek-light.json` (light base) into
   `~/.config/openfader/extension/skin/` and change `name`.
2. List only what you change; start with the surfaces (`bg`, `panel*`, `text*`) and `accent`.
3. Keep text readable on `select`, `hover-row` and `accent` (`text-bright` sits on them); `extension/skin/high-contrast.json` shows a
   high contrast set.
4. The theme shows within a second of saving the file; stderr tells what was left out.
