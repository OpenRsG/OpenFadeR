# Controller mappings

A mapping is one XML file (`<OpenFadeRMapping>`) that tells the app which MIDI message of a controller is which control, and which
message lights which light. The format is documented at the top of `src/app/midi/mapfile.rs`; the files themselves live in the
[OpenFadeR-Map](https://github.com/OpenRsG/OpenFadeR-Map) repository. The app reads them from, in order, `$OPENFADER_MAPPINGS`
(`:`-separated folders), `~/.config/openfader/mappings/` (`$XDG_CONFIG_HOME/openfader/mappings`) and the bundled `mappings/` next to
the executable; a file in an earlier folder replaces one of the same name in a later one. When a controller connects, the file whose
`<port>` is the longest one found inside a MIDI input port name (case-insensitive) is loaded.

Input callbacks queue messages in order and wake the UI event loop once per pending batch. Each UI turn dispatches at most 256 messages, then yields without dropping or merging controls. The 5 ms timer remains for jog decay and session housekeeping, not as the normal input-delivery clock. Audio buffers and the output device add separate control-to-sound latency.

A controller whose tempo fader moves the tempo the wrong way (the app takes 0 at the fader's top as the + end; some report 1 there) is corrected with PREFERENCES > CONTROLLER > Deck > **Invert hardware tempo fader**. It is independent of PREFERENCES > VIEW > **Tempo fader: + at the bottom**, which only reverses the on-screen fader.

**Soft takeover** (PREFERENCES > CONTROLLER > Others > MIDI CONTROLS > **Soft takeover**, on by default): a knob or fader that sends an absolute value (CC 0..127 or 14-bit) is ignored while the value on screen is somewhere else, for example after a deck layer switch, a STEM ISO / EQ switch, an edit with the mouse, a load, a remap, or SYNC letting go of the tempo. It takes over once it crosses the on-screen value or comes within about two steps of it, and then controls it directly until the value on screen changes by itself again. While a control waits and is moved, a notification says which way to turn it ("eq-low A: turn up ▲ to pick up"). It covers the mixer knobs, channel and crossfader faders, STEM ISOLATOR knobs, MASTER / HP knobs, tempo faders, Beat FX levels and the MERGE FX knobs; the tempo fader of a deck that follows a master through SYNC is ignored as before. Relative encoders, buttons, the jog wheels and the controller's state report on connect are not affected; with the setting off an absolute control jumps to its position at once.

## Install from a repository

PREFERENCES > CONTROLLER > **GET MAPPINGS** lists the mapping files of your GitHub repositories, with the same repository list as GET
PLUGINS and GET THEMES (`~/.config/openfader/repositories.json`; `OpenRsG/OpenFadeR-Map` is in it on first run): type `owner/repo` and
**ADD**, **x** removes one, **REFRESH** downloads them again. A row shows the controller, its port name and the repository, with
**INSTALL**, **UPDATE** (the file of that name that the app reads differs from the repository's) or **INSTALLED**; the search box filters
by controller, port or file name. Installing copies the file into `~/.config/openfader/mappings/` after the mapping parser accepted it,
and if a matching controller is connected the session is opened again with it, as after a plug.

A repository's mapping files are the `*.xml` files at its top or at most two folders deep whose root element is `<OpenFadeRMapping`;
files the parser refuses are skipped and counted as "unreadable" on the repository's line.

**No search needed**: when a MIDI input port is connected that no mapping file matches, the app looks in the downloaded repositories (the
first time it downloads them) for a file whose `<port>` fits the port's name by the same rule, and shows
`Mapping for <controller> available: INSTALL` at the top right of the window. One click installs it and loads it.
