# Controller mappings

A mapping is one XML file (`<OpenFadeRMapping>`) that tells the app which MIDI message of a controller is which control, and which
message lights which light. The format is documented at the top of `src/app/midi/mapfile.rs`; the files themselves live in the
[OpenFadeR-Map](https://github.com/OpenRsG/OpenFadeR-Map) repository. The app reads them from, in order, `$OPENFADER_MAPPINGS`
(`:`-separated folders), `~/.config/openfader/mappings/` (`$XDG_CONFIG_HOME/openfader/mappings`) and the bundled `mappings/` next to
the executable; a file in an earlier folder replaces one of the same name in a later one. When a controller connects, the file whose
`<port>` is the longest one found inside a MIDI input port name (case-insensitive) is loaded.

Input callbacks queue messages in order and wake the UI event loop once per pending batch. Each UI turn dispatches at most 256 messages, then yields without dropping or merging controls. The 5 ms timer remains for jog decay and session housekeeping, not as the normal input-delivery clock. Audio buffers and the output device add separate control-to-sound latency.

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
