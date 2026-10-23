# gamefix

**Makes games that won't start on Linux start.** Point it at a game you downloaded (from
itch.io, a zip, an installer, a disc image) or at a Steam game that misbehaves. gamefix works
out why it won't run, tells you in plain words, and sets it up once. After that you start the
game from your app menu/start menu or from Steam, like any other game.

gamefix isn't a launcher: you never start games through it. What it sets up keeps working
even if you uninstall it.

## What it fixes

A few examples of what it finds and fixes:

- **Missing old libraries.** Older native games need libraries that newer systems dropped
  (libssl 1.0, GTK 2, SDL 1.2, Allegro 5.0 ...). gamefix takes them from Steam's runtime for
  older games, or downloads them from Fedora's, Debian's or Ubuntu's packages, just for that
  game.
- **Games that crash or have no sound because of a library clash.** Examples: Unity games
  whose FMOD sound fails, programs with their own copy of a library the graphics driver also
  uses, and pygame bundles that can't play MP3.
- **Libraries newer glibc refuses** ("cannot enable executable stack").
- **Old Unity games:** blank menus with no text (no Arial font), quitting at once while Steam
  is open, and a one-colour screen on start.
- **Games another program runs**, set up with the right version of that program:
  - LÖVE games on the LÖVE version they were made for (0.8, 0.9, 0.10 or 11)
  - Java games on a Java version that still runs them
  - Python games in a Python of their own, with the packages they need
  - .NET games on Microsoft's .NET
  - DOS games in DOSBox Staging, GOG's DOS releases too (instead of the Windows DOSBox they bring)
  - Flash games in Ruffle
  - RPG Maker 2000/2003 games in EasyRPG Player
  - Game Boy games in mGBA
  - TIC-80 cartridges in TIC-80
- **Windows games:** it adds them to your app menu (started with Proton through
  [umu-launcher](https://github.com/Open-Wine-Components/umu-launcher), in a Wine prefix of
  the game's own) or to Steam with Proton. For Steam games, it picks the Proton
  version that ProtonDB players' reports say works. For GOG and Epic games that
  [protonfixes](https://github.com/Open-Wine-Components/umu-protonfixes) has a fix for, it
  picks GE-Proton and tells protonfixes which game it is. GOG games start the way GOG Galaxy
  starts them, and GOG's first-run helper is run once. It runs installers and installs from disc
  images once, then adds the installed game to Steam. Games packed in an Unreal Development
  Kit installer (`UDKInstall-<name>.exe`) are unpacked next to it instead. It can also add Windows parts an old game needs
  (MFC, Visual Basic 6, XNA ...) to its Proton setup.

When something can't be fixed, gamefix says so and why. Examples: anti-cheat that doesn't run
under Proton, a download that's missing files, a web game, source code that has to be built
first, an Android app, a program for another processor.

## How to use it

The easiest way is Dolphin's right-click menu (KDE). Turn it on once:

```sh
gamefix right-click on
```

Then right-click a game's folder, its program, an installer or a disc image. A **gamefix**
submenu has these items:

- **Check what's wrong**
- **Fix and add to the app menu**
- **Fix and add to Steam** (or both)
- **Undo gamefix's changes**
- **Report a problem…**

The same from a terminal:

```sh
gamefix check <game folder or file>
gamefix setup <game folder or file> [--menu] [--steam]   # --menu if neither is given
gamefix undo <game folder or file>
gamefix feedback [<game folder or file>]
```

Steam has to be closed while gamefix changes Steam's settings, because Steam rewrites them
when it quits. From the right-click menu, gamefix offers to close Steam for you.

Windows games installed from an installer or a disc go into `~/Games`. To use another folder
(it must be on a Linux drive, not NTFS or FAT):

```sh
gamefix games-folder <folder>
```

## What it changes

gamefix prefers changes that work from any shortcut, made where they belong:

- **In the game's folder,** only small fixes, each one reversible:
  - marking the program executable
  - rewriting a launcher script saved with Windows line endings (the original is kept)
- **In the shortcut,** settings and libraries for that game:
  - an app-menu entry in `~/.local/share/applications`
  - a Steam shortcut, or the Steam game's own launch options (fixes for a game's Linux
    version turn themselves off if you later switch it to Proton in Steam)
- **In gamefix's own folders** (`~/.local/share/gamefix`): libraries, runtimes, fonts, and
  Wine prefixes for games that need one of their own (undo keeps those: your saves are in them).

Every change is recorded, and `gamefix undo` takes it back. Shared downloads stay after undo
because other games can use them: LÖVE builds, .NET and Python runtimes, GE-Proton, and
Flathub apps.

gamefix never runs a game's files to find out what's wrong; it only reads them. It downloads
only what a fix needs: distribution packages, LÖVE's own builds, Microsoft's .NET, Python
builds, GE-Proton and RPG Maker's runtime package. Before installing an app from Flathub, it
asks.

## Reporting problems

Use **Report a problem…** in the right-click menu, or `gamefix feedback <game>`. You write what
went wrong. gamefix adds its report on the game, what it last did and what it changed, plus one
line about your system: the distribution, your graphics card's maker, and whether Steam's
runtime for older games is there.

Your home folder's path, your user name and your Steam account number are taken out first.
Then you see the whole text and decide whether to send it. Reports go to gamefix's own server
on Cloudflare; what it keeps is at <https://gamefix.petru101.workers.dev/privacy>. Issues here
on GitHub are welcome too.

## Status

gamefix is young. It's built and tested on Bazzite (Fedora) with KDE Plasma, against several
hundred itch.io games from 2014 to 2025 run in a virtual machine, and the author's own Steam
library. It's written for any distribution, but others are untested. The right-click menu
needs Dolphin. On other desktops, use the commands; results show in a notification.

## Building

gamefix is Rust with only the standard library:

```sh
cargo build --release
```

That gives `target/release/gamefix`, which uses your system's glibc. `run/build.sh` makes the
static build, which runs on any distribution. It uses the GNOME 50 SDK and its Rust extension
through flatpak, and is written for the author's distrobox (`host-spawn`).

gamefix runs on the host, not in a sandbox. It has to see the system's libraries, and the
fixes it writes start games with them.

`server/` is the Cloudflare Worker that receives reports (Workers + D1).

## Third-party data

`data/protondb-proton.tsv` and `data/protondb-options.tsv` are derived from ProtonDB's reports
export ([bdefore/protondb-data](https://github.com/bdefore/protondb-data)) under the
[Open Database License](https://opendatacommons.org/licenses/odbl/1-0/). They're made by
`tools/protondb-tables.py`, and are under the same licence.

`data/umu-fixes.tsv` is derived from [umu-database](https://github.com/Open-Wine-Components/umu-database)
(GPL-3.0) and the list of fixes in [umu-protonfixes](https://github.com/Open-Wine-Components/umu-protonfixes)
(BSD-2-Clause). It's made by `tools/umu-table.py`.

## Made with AI

Most of gamefix's code and this README were written by Claude (Anthropic's AI). Its author
designed it, directed every change and tests it on real games.

## License

GPL-3.0-or-later (`LICENSE`).
