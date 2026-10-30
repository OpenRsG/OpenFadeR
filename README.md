<p align="center">
  <img src="assets/openfader.png" width="112" alt="OpenFadeR logo">
</p>

# OpenFadeR

DJ software written in Rust with a Slint interface. Mix with two or four decks on Linux, Windows and macOS, using a mouse, keyboard or MIDI controller.

[Download](https://github.com/OpenRsG/OpenFadeR/releases/latest) · [Quick start](#quick-start) · [Feature tour](#feature-tour) · [Documentation](#documentation) · [Report a bug](https://github.com/OpenRsG/OpenFadeR/issues)

![OpenFadeR with two decks and the track browser](docs/media/hero.png)

[Watch the demo (MP4)](docs/media/demo.mp4)

## Download and run

Choose an archive from the [releases page](https://github.com/OpenRsG/OpenFadeR/releases/latest), extract it and run OpenFadeR from the extracted folder. Keep the companion folders with the executable.

| System | Build |
| --- | --- |
| Linux PC | x86_64 |
| Windows PC | x86_64 |
| macOS | Apple Silicon (arm64) |
| Raspberry Pi / ARM Linux | aarch64 |
| Raspberry Pi 4 / CM4 | Cortex-A72 build |

The Pi builds need a 64-bit OS based on Debian 13 (trixie). See the [Raspberry Pi guide](docs/raspberry-pi.md) for dependencies and setup. Prebuilt archives avoid compiling Rust on the Pi.

Windows builds are unsigned. macOS builds are ad-hoc signed, not Developer ID signed or notarized, so the OS may require you to approve them before launch.

Use Help > Check for updates to check for a newer release. Release notes describe changes and any upgrade requirements.

## Quick start

1. Open Preferences > Audio and select your audio output.
2. Open a folder of music, or add a library source in Preferences > Libraries.
3. Select a track in the browser. Enter loads deck A; Shift+Enter loads deck B.
4. Space starts or pauses deck A; Shift+Space controls deck B. Use the channel faders to set their levels.

Press `?` for the shortcut list. Change bindings in Preferences > Keyboard.

For a MIDI controller, OpenFadeR loads a matching installed mapping when it connects. Preferences > Controller > Get mappings installs additional mappings. See the [controller guide](docs/mappings.md) for setup and soft takeover.

### Crossfader control

Source builds include Preferences > Controller > Mixer > Enable crossfader. Clear it to ignore manual crossfader input and leave both mix sides open. Channel faders still work, and Automix keeps its automatic transitions. Re-enabling applies the crossfader position again.

The crossfader-disable setting is unreleased; the 0.2.8 downloads do not include it.

## Feature tour

Screenshots show several layouts and themes. Controls can differ between releases.

### Two or four decks

Mix with scrolling waveforms, hot cues, loops, slip mode and sync. Switch to four decks when you need more tracks in the mix.

![Four-deck performance layout with stacked waveforms, transport controls and the track browser](docs/media/four-decks.png)

### Effects and sampling

Channel EQ, Color FX and Beat FX sit alongside the decks. The sampler and sequencer add sample playback and saved patterns.

![Effects panels open beneath the two decks](docs/media/fx.png)

### STEMS

Separate a track into parts for mixing. The first use downloads a model, and separation needs additional CPU or GPU memory. ARM Linux builds use the CPU provider; see the [Pi guide](docs/raspberry-pi.md) before enabling it on a small board.

![STEMS controls and separated waveform parts on two decks](docs/media/stems.png)

### Track preparation

Inspect waveforms and adjust beat grids before mixing. Keep cues, loops and track metadata in the library, with Beat Lab available alongside the decks.

![Beat Lab waveform editor open above the track list](docs/media/beatlab.png)

### EDIT workspace

Arrange edits on the timeline, use snapping and export the result as WAV. The edit-decision list works over the source audio.

![EDIT workspace with a waveform timeline and a selected region](docs/media/edit.png)

### USB library preparation

Use the EXPORT workspace to prepare tracks and cue points for supported DJ players. Library adapters handle each format's device files.

![EXPORT workspace with track-preparation players, cue controls and the library browser](docs/media/export.png)

Back up a USB device before exporting to it, and test the exported library on your player before a gig. Format support varies by player and firmware; check the [Rekordbox device compatibility table](compatibility/rekordbox/device/COMPATIBILITY.md) and the [adapter guide](docs/plugins.md#compatibility-layer).

### Themes and layouts

Choose a theme in Preferences or install one from a repository. Desktop, Studio, Stage and Pocket layouts share the same library and playback state. The [skin guide](docs/skins.md) covers customization.

![A selection of OpenFadeR themes with different colors and deck layouts](docs/media/themes-grid.jpg)

<details>
<summary>More screenshots: sampler, track suggestions, lighting and theme browser</summary>

#### Sampler and sequencer

Trigger samples and work with sequences from the performance workspace.

![Sampler panel open above the library browser](docs/media/sampler.png)

#### Track suggestions

Browse the SUGGEST view while keeping the decks visible.

![SUGGEST track list beneath the playing decks](docs/media/suggest.png)

#### Lighting

The LIGHTING workspace has fixture and DMX scene controls. Lighting tools are beta. Video playback has its own workspace.

![LIGHTING workspace showing fixtures and scene controls](docs/media/lighting.png)

#### Theme browser

Preview and select themes in Preferences.

![Theme browser showing the installed theme previews](docs/media/themes-gallery.png)

</details>

Automix and mix recording are also available from the main workspace. See the [recording guide](docs/recording-files.md) for recorded files.

## Library and controller support

OpenFadeR keeps its Collection in its own database. Adapters read external libraries and perform the operations supported by each format.

| Integration | Delivery |
| --- | --- |
| Rekordbox device libraries, XML and desktop collection | Built-in adapters |
| Traktor collections | Optional built-in adapter |
| Engine DJ, Serato, VirtualDJ and iTunes / Music | Companion library plugins |
| Sampler packs | Companion `sample-pack` plugin |
| Mixxx controller mappings and scripts | Companion controller plugin, not a library reader |

The [compatibility-layer guide](docs/plugins.md#compatibility-layer) lists formats and supported read/write operations. [OpenFadeR-Plugins](https://github.com/OpenRsG/OpenFadeR-Plugins) contains the companion integrations.

MIDI support does not mean that every controller's audio interface is configured automatically. Select the audio output separately in Preferences > Audio when needed.

## Build from source

Install Git, Python 3, stable Rust and your platform's native build tools. Linux also needs pkg-config and development packages for ALSA, fontconfig, XKB, Wayland and udev; the [CI workflow](.github/workflows/ci.yml) lists the Ubuntu packages.

```sh
git clone https://github.com/OpenRsG/OpenFadeR.git
cd OpenFadeR
./start.sh ~/Music
```

`start.sh` fetches the companion repositories on the first run, builds native plugins and starts a release build. It uses plugin tag `v0.2.11` by default. Existing companion checkouts are left as they are; update them deliberately when changing core versions.

On machines with little RAM, set `CARGO_BUILD_JOBS=1` to limit compiler parallelism. The [Pi guide](docs/raspberry-pi.md) also covers cross-compilation. Android has a separate [build guide](docs/android.md).

On Linux, `./install.sh` builds and installs for the current user. `./install.sh --uninstall` removes that installation. See [CONTRIBUTING.md](CONTRIBUTING.md) for development and test instructions.

## Plugins, themes and controllers

Companion repositories are checked out into these folders when running from source:

| Repository | Folder | Documentation |
| --- | --- | --- |
| [OpenFadeR-Plugins](https://github.com/OpenRsG/OpenFadeR-Plugins) | `extension/plugin` | [Plugin guide](docs/plugins.md) |
| [OpenFadeR-Themes](https://github.com/OpenRsG/OpenFadeR-Themes) | `extension/skin` | [Skin guide](docs/skins.md) |
| [OpenFadeR-Map](https://github.com/OpenRsG/OpenFadeR-Map) | `mappings` | [Controller mapping guide](docs/mappings.md) |

Use the [SDK](sdk/) to write a native plugin. Use a controller mapping compatible with your hardware; the [mapping guide](docs/mappings.md) describes installation and input behavior.

## Backups and bug reports

File > Library > Backup library saves OpenFadeR's library metadata. It does not back up your music files. Keep a separate backup of the audio and make a library backup before major upgrades.

For a bug report, use Help > Report a problem and include the steps to reproduce it. The [diagnostics guide](docs/diagnostics.md) explains logs, crash reports and what the report contains. Check reports for private information before posting them publicly.

## Documentation

- [Library format and import/export](docs/library-format.md)
- [Recording files](docs/recording-files.md)
- [Key detection](docs/key-detector.md)
- [Code structure](docs/code-structure.md)
- [Contributing](CONTRIBUTING.md)
- [Security policy](SECURITY.md)
- [Code of conduct](CODE_OF_CONDUCT.md)

## License

[AGPL-3.0](LICENSE). Maintained by [OpenRsG](https://github.com/OpenRsG).
