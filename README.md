<p align="center">
  <img src="assets/openfader.png" width="112" alt="OpenFadeR logo">
</p>

# OpenFadeR

DJ software written in Rust with a Slint interface. Mix with two or four decks on Linux, Windows and macOS, using a mouse, keyboard or MIDI controller.

[Download the latest release](https://github.com/OpenRsG/OpenFadeR/releases/latest) · [Watch a demo](docs/media/demo.mp4) · [Report a bug](https://github.com/OpenRsG/OpenFadeR/issues)

![OpenFadeR with two decks and the track browser](docs/media/hero.png)

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

## Start mixing

Open a folder of music or add a source in Preferences > Libraries. Select a track in the browser and press Enter to load deck A, or Shift+Enter to load deck B. Space starts or pauses deck A; Shift+Space controls deck B.

Select your output in Preferences > Audio before mixing. MIDI mappings live in Preferences > Controller, and keyboard shortcuts can be changed in Preferences > Keyboard. Press `?` for the shortcut list.

### Disable the crossfader

In source builds, clear Enable crossfader under Preferences > Controller > Mixer. Manual crossfader input is ignored and both sides stay open. Channel faders still control their channels, and Automix keeps its automatic transitions. Re-enabling applies the crossfader position again.

This setting is unreleased; the 0.2.8 downloads do not include it.

## Mixing and library tools

- Two- and four-deck layouts, scrolling waveforms, hot cues, loops and slip mode.
- BPM and key analysis, editable beat grids, sync and key lock.
- Channel EQ, filters, beat effects, a sampler and sequencer.
- Library search, playlists, tags, cue editing and mix recording.
- Automix with selectable transition styles.
- USB library export for supported DJ players.
- Optional STEMS separation. The first use downloads a model; separation needs additional CPU or GPU memory.
- An EDIT view for arrangements, video playback and lighting tools. The lighting tools are beta.

Rekordbox and Traktor readers are built in. Plugins add other library sources and effects. The [plugin repository](https://github.com/OpenRsG/OpenFadeR-Plugins) lists its integrations and their limits.

Back up a USB device before exporting to it, and check the exported library on your player before relying on it at a gig. Format support varies by player and firmware.

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

Use the [SDK](sdk/) to write a native plugin. Use a controller mapping compatible with your hardware; MIDI support does not imply that every controller's audio interface is configured automatically.

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
