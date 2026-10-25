# Player compatibility of the rekordbox device plugin

**No player has been tested with OpenFadeR's exports yet.** Every row below says *Not tested* or *Known problem*; none says *Tested*.
What the rows do say about which library and file systems a model reads comes from the owner's study of the makers' documentation
and of player reports, not from a run of OpenFadeR's output on the hardware. A row turns to *Tested* only when an export of this
plugin was played on that model, and the test is written next to it.

The facts are kept in [`rekordbox-device/players.json`](rekordbox-device/players.json); the stick checker of the app reads that
file (`status` `confirmed` is a fact from the study, `to_measure` is not known yet and the checker says so instead of a tick).
A new model is a new line there and a new row here: the test `compatibility_lists_every_player_and_library` keeps the two
together and checks that every library named is one `device_check` reports (`Device Library`, `OneLibrary`).

| Model | Status with OpenFadeR exports | Reads | File systems | Firmware | Fact recorded | Source |
|---|---|---|---|---|---|---|
| CDJ-2000NXS2 | Not tested | Device Library | FAT32 (vfat), HFS+; partition table MBR (dos); not exFAT | not stated | 2026-10-24 | the owner's study |
| CDJ-3000 | Not tested | Device Library, OneLibrary | FAT32 (vfat), exFAT, HFS+ | 3.30 (withdrawn 4 Nov 2025) needed a OneLibrary; later firmware reads the Device Library again | 2026-10-24 | the owner's study |
| CDJ-3000X | Not tested | OneLibrary | FAT32 (vfat), exFAT | not stated | 2026-10-24 | the owner's study |
| OPUS-QUAD | Not tested | OneLibrary | FAT32 (vfat), exFAT | not stated | 2026-10-24 | the owner's study |
| OMNIS-DUO | Not tested | OneLibrary | FAT32 (vfat), exFAT | not stated | 2026-10-24 | the owner's study |
| XDJ-AZ | Not tested | OneLibrary | FAT32 (vfat), exFAT | not stated | 2026-10-24 | the owner's study |
| XDJ-XZ | Not tested | Device Library (which library it reads is not documented yet: *to measure*) | FAT32 (vfat), exFAT | not stated | 2026-10-24 | the owner's study |
| XDJ-RX3 | Not tested | Device Library (which library it reads is not documented yet: *to measure*) | FAT32 (vfat), exFAT | not stated | 2026-10-24 | the owner's study |
| CDJ-2000 Nexus | Known problem | Device Library (*to measure*) | FAT32 (vfat) | not stated | 2026-10-24 | the owner's study |
| XDJ-1000MK2 | Known problem | Device Library (*to measure*) | FAT32 (vfat) | not stated | 2026-10-24 | the owner's study |

*Known problem*: players of that model were reported stuck on exports from other software. It says nothing yet about OpenFadeR's
exports; test before a gig.

"Fact recorded" is the day the study's facts were entered here, not the day a unit was measured. The model names are the makers'
names for the units, used only to say what reads what.

## Booth profiles

Preferences > DJ SYSTEM > DEVICE lets you name the players of your booth; the checker then judges a stick against those models.

| Profile | Models |
|---|---|
| Older players | CDJ-2000NXS2, CDJ-2000 Nexus, XDJ-1000MK2 |
| Mixed booth | CDJ-2000NXS2, CDJ-3000, CDJ-3000X |
| Newest only | CDJ-3000X, OPUS-QUAD, OMNIS-DUO, XDJ-AZ |
