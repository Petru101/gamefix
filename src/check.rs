//! Turns what the scan found into problems, each with what it means and how to fix it.

use crate::binary::{self, Binary, Elf, Symbols, EM_386, EM_X86_64};
use crate::scan::{self, Game};
use crate::system::System;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::ffi::OsStr;
use std::fmt::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

pub enum Problem {
    NoLinuxBuild,
    /// A Windows game's own copies of DLLs Wine has built in (names without ".dll"), next to
    /// its program: Windows would load them, Wine uses its own unless told otherwise.
    OwnWindowsDlls(Vec<String>),
    /// Windows parts the game's program needs that neither the game brings nor Proton's Wine
    /// has (MFC, the Visual Basic runtime, XNA, ...), and what provides each.
    MissingWindowsParts(Vec<(String, crate::proton::Provider)>),
    /// Nothing built, but a project's source files (the marker file and what it's for):
    /// A-Maze-Batz's download is its CMake project, dodge_the_creeps a Godot project.
    Source(PathBuf, &'static str),
    /// Nothing to run but an HTML5 export (index.html with the engine's scripts): a game for a
    /// browser (Ferryman, Construct 2: c2runtime.js, data.js).
    Web(PathBuf),
    /// A Unity Web Player build (`.unity3d` for the browser plugin), which nothing plays now.
    UnityWeb(PathBuf),
    /// Nothing to run, but an archive the game may still be packed in (C-Dogs SDL: a .tar.gz
    /// inside the .zip).
    Archive(PathBuf),
    /// The Linux program is there but damaged (went through a text conversion): no fixing it.
    Damaged(PathBuf),
    /// A Linux installer (MojoSetup) with the game packed inside, and no program besides.
    LinuxInstaller(PathBuf),
    /// Epic's UDK installer (UnSetup): the game is a zip after its program, unpacked rather
    /// than installed (Retro Bike Park's only download was added to Steam as the game).
    UdkInstaller(PathBuf),
    /// Unity 2018.2-2018.3 with its resolution window enabled starts at 1x1 pixels when the
    /// window can't open (ScreenSelector.so needs GTK 2): one colour fills the screen (Baldi's
    /// Basics, Brownie Cove, Precedence). Steam's runtime has GTK 2; its run.sh, if any.
    UnityResolution(Option<PathBuf>),
    /// A Unity program without its `<name>_Data` folder: the upload is incomplete (PicoPico's
    /// is the Linux program alone; ESCAPE's the Windows one; Car Crash's the .exe and a loose
    /// mainData), and Unity quits with "There is no data folder".
    UnityDataMissing(PathBuf),
    /// A Flash game (.swf); `ruffle` is how to start an installed Ruffle.
    Flash { file: PathBuf, ruffle: Option<Vec<String>> },
    /// A TIC-80 cartridge; `tic80` is how to start an installed TIC-80.
    Tic { file: PathBuf, tic80: Option<Vec<String>> },
    /// A .NET program built for Mono on Linux; `mono` is the installed Mono. `libs`: native
    /// libraries its .NET parts load by name that the system lacks (see `mono_libs`), and
    /// whether Steam's old runtime has each.
    Mono { file: PathBuf, mono: Option<PathBuf>, libs: Vec<(String, bool)>, standard: Vec<String> },
    /// A console cartridge (`console` names it); `mgba` is how to start an installed mGBA.
    Rom { file: PathBuf, console: &'static str, mgba: Option<Vec<String>> },
    /// A CD's files, with the installer among them.
    CdFiles { installer: PathBuf },
    /// Disc copy protection (SafeDisc, SecuROM) around the game's program, which Wine can't
    /// run; `others` are copies of the program with that name in the download without it.
    Protected { what: &'static str, file: PathBuf, others: Vec<PathBuf> },
    /// A disc image (.iso) and the installer on it, if one was found; None in `files` when it
    /// couldn't be read (no 7-Zip).
    Disc { file: PathBuf, files: Option<usize>, installer: Option<String> },
    /// A DOS game; `dosbox` is how to start an installed DOSBox.
    Dos { file: PathBuf, dosbox: Option<Vec<String>> },
    /// Only a .deb or .rpm package, which Bazzite can't simply install.
    Package(PathBuf, &'static str),
    /// A .love file: needs the LÖVE version it was made for; `love` is how to start an installed one.
    Love { file: PathBuf, version: Option<String>, love: Option<Vec<String>> },
    /// Python code: needs Python 3 (2 is gone from current systems) and what `plan` lists.
    Python { file: PathBuf, v2: bool, plan: Option<crate::python::Plan> },
    /// A .jar: needs Java; `java` is where one was found.
    Java { file: PathBuf, java: Option<PathBuf> },
    WrongMachine(u16),
    NotExecutable(PathBuf),
    /// The developer's launcher script saved with Windows line endings: Linux takes
    /// "/bin/bash\r" for the shell's name and won't start it (Curse of the Arrow's run.sh).
    WindowsLineEndings(PathBuf),
    /// Folders a config file beside the program names among others that exist, which the
    /// download lacks: (config file, folders), relative to the game's root.
    MissingFolders(PathBuf, Vec<PathBuf>),
    /// Library name → the game's files that need it: `libs` at start (the game can't start
    /// without them), `later` only by plugins loaded later, if at all. Which of all those Steam's
    /// runtime for older games has, and which the game ships in a folder ld.so doesn't look in.
    MissingLibs {
        bits: u8,
        libs: BTreeMap<String, Vec<PathBuf>>,
        later: BTreeMap<String, Vec<PathBuf>>,
        in_old_runtime: BTreeSet<String>,
        misplaced: BTreeMap<String, PathBuf>,
    },
    ExecStack(Vec<PathBuf>),
    /// Libraries that get another library's functions of the same name instead of the ones
    /// they were built against (Final Shot: Unity's built-in FMOD answering for the game's).
    SymbolClash(Vec<Clash>),
    /// Unity before 5.4: its single-instance check mistakes other programs for the game.
    UnitySingleInstance,
    /// A program that draws no window and drives the terminal itself (ncurses, or raw
    /// termios with escape codes): started from the app menu without one, it has nowhere to
    /// draw (Potatoed Encryption: "Error opening terminal: unknown."; Xonitix: std::length_error).
    /// `old_curses`: it uses ncurses 5, which can't read the terminal descriptions current
    /// systems keep for xterm-256color (ncurses 6's extended format: "Error opening terminal:
    /// xterm-256color"); plain xterm's are still in the old format.
    Terminal { old_curses: bool },
    /// H.264 videos in a Unity Linux build: its player only decodes VP8 there.
    UnityH264(Vec<PathBuf>),
    /// A Unity player that finds its built-in font (the GUI's, "Arial", which carries no font
    /// data) by name in /usr/share/fonts, and none of the names it tries is there: text drawn
    /// with it doesn't show (Zandvox, Sector Plural Zed: Unity 4.3, buttons without labels).
    /// The names it tries.
    UnityFont(Vec<&'static str>),
    /// A Unity 4/5.0 player compiles every shader variant through the graphics driver before it
    /// draws anything, and this game has that many (GLSL programs counted in its data files): the
    /// first start stays black for minutes (The Asylum: 4,928, 1½-3 minutes on an RTX 3090). The
    /// driver keeps what it compiled, so later starts are quick (10 s).
    UnityShaders(usize),
    /// The program has the C++ standard library built in and exports it; the system's copy,
    /// loaded later by the graphics driver, then gets the old functions and crashes (2:22AM).
    /// The system's libstdc++ to preload, if found.
    OwnLibstdcxx(Option<String>),
    /// A PyInstaller build carrying its own libstdc++.so.6, which goes first; Mesa's GL driver
    /// needs the system's newer one and isn't loaded: "Couldn't find matching GLX visual" from
    /// SDL (Skeletris, RESYNC). The system's copy to preload.
    BundledLibstdcxx(Option<String>),
    /// A PyInstaller pygame build whose SDL_mixer is older than 2.6 (MP3 only through a bundled
    /// mpg123, smpeg or libmad, and it has none) while the game carries .mp3 files: loading the
    /// music fails ("Unrecognized audio format": Flyre, pygame 2.0.1 with SDL_mixer 2.0.2).
    /// `sdl20`: its SDL is 2.0.x, and pygame 2.0-2.1 refuses any other minor version ("SDL
    /// compiled with version 2.0.14, linked to 2.32.72"), so a newer mixer needs an SDL 2.0 too.
    PygameMp3 { sdl20: bool },
    /// The game's own Allegro image add-on was built without WebP and the game has .webp images:
    /// Allegro picks the loader by the file's ending, gets nothing, and the game crashes
    /// (Non-Competitive Singing Potatoes: segfault on its logo, holypangolin.webp). The add-on.
    AllegroWebp(PathBuf),
    /// A start-up file exporting its own HarfBuzz: the system's pango (through GTK) then gets
    /// those old functions and crashes (JK Assassins, NW.js with Chromium 48). What to preload
    /// (LD_PRELOAD's value), and whether the program itself exports it: then preloading the
    /// system's HarfBuzz can't help (ld.so looks in the program first), so it's Steam's old
    /// runtime's pango, from before pango used HarfBuzz (Rough & Tumble, 32-bit NW.js).
    OwnHarfbuzz(PathBuf, Option<String>, bool),
    /// NW.js on Chromium 59-73 quits at start with "Could not find the default font" under
    /// current fontconfig rules (lilac & her light and others, 65; 38-58 and 74-78 are fine). The Chromium version.
    NwjsFonts(String),
    /// Native libraries FNA.dll.config maps to that are neither shipped nor installed
    /// (Shipwreck's download has empty lib/ and lib64/).
    FnaLibs(Vec<String>),
    /// MonoKickstart's config files the download left out (monoconfig, monomachineconfig).
    KickFiles(Vec<String>),
    /// SDL2 add-ons (mixer, image, ...) come from Steam's runtime, built for the real SDL2, while
    /// the system's SDL2 is sdl2-compat on top of SDL3: C-Dogs SDL crashed with that mix and
    /// runs with the runtime's own SDL2 preloaded. That library's path.
    Sdl2Mix(String),
    /// Sound only through ALSA in a 32-bit game, without a 32-bit plugin for ALSA's default
    /// device; the runtime's PulseAudio plugins (pcm, ctl) if there.
    Alsa32(Option<(PathBuf, PathBuf)>),
    /// A bundled libpulse older than 1.0, which can't find the sound server by itself; the
    /// server's address for PULSE_SERVER.
    OldPulse(String),
    /// The game brings its own Java, whose sound (javax.sound) opens the card itself and gets
    /// nothing while PipeWire holds it; PipeWire's ALSA plugin to send it there.
    JavaSound(PathBuf),
    /// The game's own copy of a library the system also has, loaded first, older than what a
    /// system library loaded with it expects: (the copy, the system library, a function the
    /// copy lacks, the system's copy to preload).
    Shadowed(PathBuf, String, String, Option<String>),
    /// .NET Core before 5 only looks for older ICU versions than current systems have, and
    /// quits at start (The Heart's Illumination, 2.1).
    DotnetIcu(String),
    /// An RPG Maker 2000/2003 game (the folder holding RPG_RT.ldb); `easyrpg` is how to start an
    /// installed EasyRPG Player; `rtp`: it uses RPG Maker's runtime package (not a full package).
    RpgMaker { dir: PathBuf, easyrpg: Option<Vec<String>>, rtp: bool },
    /// An RPG Maker game packed in a self-extracting package (see `rpgmaker::packed`).
    RpgMakerPacked { file: PathBuf, kind: &'static str, dest: String },
    /// An RPG Maker XP/VX/VX Ace game (Windows) needing that version's runtime package.
    RgssRtp(&'static str),
    /// A framework-dependent .NET program built for any system (`<name>.dll`, Linux libraries
    /// beside it: Anodyne Sharp, .NET 6); `dotnet` is the installed runtime's folder.
    Dotnet { file: PathBuf, version: String, dotnet: Option<PathBuf> },
    /// A framework-dependent .NET program (no libhostfxr.so beside it; its runtimeconfig.json
    /// names the framework) and no .NET runtime of that major version installed: Incremental
    /// Unlimited ("the required library libhostfxr.so could not be found", .NET Core 2.1).
    DotnetRuntime(String),
    /// A self-contained .NET program whose deps.json lists static libraries (.a) the download
    /// left out; the host refuses to start while any listed file is missing, though .a files
    /// are only for building and never loaded (Sokoban, 3.1.11: System.IO.Compression.Native.a
    /// and three more). Their names, in the program's folder.
    DotnetStatic(Vec<String>),
    /// A pygame 1 game packed by PyInstaller 3 whose code asks for system fonts (SysFont,
    /// get_default_font): for a font that isn't installed pygame falls back to its own default
    /// font, which it looks up through pkg_resources first; PyInstaller 3's pyi_rth_pkgres hook
    /// gives its importer pkg_resources' NullProvider, which raises NotImplementedError, and
    /// pygame 1 segfaults on that (Souls of the Tower). pygame 2 and PyInstaller 4's hook
    /// (PyiFrozenProvider) handle it. The program whose archive changes; where the default font
    /// (freesansbold.ttf) is missing too, if it is; the stand-in for it.
    PygameFont { program: PathBuf, missing: Option<PygameFont>, font: Option<PathBuf> },
}

pub enum PygameFont {
    /// Inside a one-file bundle (the program).
    Bundle,
    /// In the pygame folder of a folder build (relative to the game's folder).
    Folder(PathBuf),
}

pub struct Clash {
    /// The library that ends up calling the wrong functions.
    pub victim: PathBuf,
    /// The library it was built against.
    pub provider: PathBuf,
    /// The library loaded first whose same-named functions win.
    pub shadow: PathBuf,
    pub count: usize,
    pub example: String,
}

impl Problem {
    /// Libraries to put in LD_PRELOAD so the intended ones are found first: providers, then
    /// the victims themselves (the combination tested on Final Shot).
    pub fn preload(clashes: &[Clash]) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = Vec::new();
        for p in clashes.iter().map(|c| &c.provider).chain(clashes.iter().map(|c| &c.victim)) {
            if !out.contains(p) {
                out.push(p.clone());
            }
        }
        out
    }
}

pub struct Report {
    pub problems: Vec<Problem>,
    pub libs_checked: usize,
    pub windows: Vec<(PathBuf, u8)>,
    pub installers: Vec<(PathBuf, &'static str)>,
    pub glibc: Option<(u32, u32)>,
    /// The program is UPX-packed, so the libraries it needs couldn't be read.
    pub packed: bool,
    /// A PyInstaller bundle on a glibc that refuses executable stacks: its packed libraries
    /// can't be checked, so setup allows them in case.
    pub pyinstaller: bool,
    /// The game's own libraries it finds at start only through its launcher's LD_LIBRARY_PATH
    /// and the system lacks: (name, folder relative to the game), for when setup has to start
    /// it without that launcher.
    pub launcher_libs: Vec<(String, PathBuf)>,
    /// run.sh of Steam's runtime for older games, if installed.
    pub old_runtime: Option<PathBuf>,
    /// Missing libraries only optional plugins need: what is lost, and the libraries.
    pub harmless: Vec<(&'static str, Vec<String>)>,
    /// What's inside an AppImage, checked like a folder: its report text and the problems
    /// found there (the ones fixable at launch are applied to the AppImage).
    pub appimage: Option<(String, Vec<Problem>)>,
    /// This report is about an AppImage's insides: its library path is the AppImage's own
    /// business, so setup can't add libraries there.
    pub inside_appimage: bool,
}

/// Plugins the game starts without when they can't load, and what is lost then.
const OPTIONAL_PLUGINS: &[(&str, &str)] = &[
    ("ScreenSelector.so", "Unity's start-up resolution window"),
    ("libcoreclrtraceptprovider.so", ".NET's developer tracing"),
    ("yad", "GOG's error-message window"),
];

/// Whether two library files are the same library: their names up to ".so", without the
/// "-<8 hex>" that auditwheel and PyInstaller add to bundled copies.
/// Unpacks an AppImage's squashfs into the cache, checks it, and removes it again.
/// A Unity player whose `<name>_Data` folder isn't next to it. Unity 2017.2+ keeps the engine
/// in UnityPlayer.dll/.so beside a small program; older players carry it ("UnityPlayer/<version>").
/// Programs of an engine recognised otherwise, PyInstaller bundles, packed programs and
/// AppImages aren't read (reading every program whole made checking the VM's games ~40% slower).
fn unity_data_missing(game: &Game, program: &scan::Found) -> bool {
    let full = game.full(program);
    let dir = full.parent().unwrap_or(&game.root);
    let stem = full.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    if dir.join(format!("{stem}_Data")).is_dir() || game.engine.as_ref().is_some_and(|e| e.name != "Unity") {
        return false;
    }
    if dir.join("UnityPlayer.dll").is_file() || dir.join("UnityPlayer.so").is_file() {
        return true;
    }
    let plain = match &program.binary {
        Binary::Elf(e) => !e.pyinstaller && !e.packed && !e.mojosetup && binary::appimage_offset(&full).is_none(),
        _ => true,
    };
    plain && std::fs::metadata(&full).is_ok_and(|m| m.len() < 64 << 20) && binary::contains(&full, b"UnityPlayer/")
}

/// Unity versions whose player starts at 1x1 when its resolution window can't open: seen in
/// 2018.2.14 to 2018.3.0b12; 2018.2.1 and 2018.4 start at the desktop's size. The edges in
/// between are a guess.
fn unity_tiny_start(version: &str) -> bool {
    let mut parts = version.split('.');
    let (Some(major), Some(minor), Some(patch)) = (parts.next(), parts.next(), parts.next()) else { return false };
    let patch: u32 = patch.chars().take_while(char::is_ascii_digit).collect::<String>().parse().unwrap_or(0);
    major == "2018" && (minor == "3" || (minor == "2" && patch >= 2))
}

/// glibc's own libraries, always there.
fn glibc_part(n: &str) -> bool {
    ["libc.so", "libm.so", "libdl.so", "libpthread.so", "librt.so", "ld-linux"].iter().any(|g| n.starts_with(g))
}

fn appimage_inside(file: &Path, offset: u64, system: &System) -> Option<(String, Vec<Problem>)> {
    let cache = std::env::var_os("XDG_CACHE_HOME").map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    let dir = cache.join(format!("gamefix/appimage-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.parent()?).ok()?;
    // It may report errors it can't help as a normal user (ownership, attributes) and still
    // unpack everything, so what counts is what's there.
    let _ = std::process::Command::new("unsquashfs")
        .args(["-n", "-q", "-no-xattrs", "-o", &offset.to_string(), "-d"])
        .arg(&dir)
        .arg(file)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    let unpacked = std::fs::read_dir(&dir).is_ok_and(|mut d| d.next().is_some());
    let inside = unpacked.then(|| scan::scan(&dir)).flatten().map(|inner| {
        let mut report = check(&inner, system);
        report.inside_appimage = true;
        // Its own first line names the unpacked folder: leave that out.
        let text = report.render(&inner).lines().skip(1).collect::<Vec<_>>().join("\n");
        (text, report.problems)
    });
    let _ = std::fs::remove_dir_all(&dir);
    inside
}

fn same_library(a: &Path, b: &Path) -> bool {
    let base = |p: &Path| {
        let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let stem = name.split(".so").next().unwrap_or("").to_string();
        match stem.rsplit_once('-') {
            Some((head, hash)) if hash.len() == 8 && hash.chars().all(|c| c.is_ascii_hexdigit()) => head.to_string(),
            _ => stem,
        }
    };
    base(a) == base(b)
}

/// An HTML5 export at the top or one folder down: index.html next to JavaScript files or a
/// scripts/ folder.
fn web_export(root: &Path) -> Option<PathBuf> {
    let dirs = std::iter::once(root.to_path_buf()).chain(std::fs::read_dir(root).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.is_dir()));
    dirs.filter(|d| d.join("index.html").is_file())
        // Construct 2 keeps its scripts beside the page or in scripts/; others in js/ (Inferieur 8).
        .find(|d| d.join("scripts").is_dir() || d.join("js").is_dir() || std::fs::read_dir(d).into_iter().flatten().flatten().any(|e| e.path().extension().is_some_and(|x| x == "js")))
        .map(|d| d.join("index.html").strip_prefix(root).map(Path::to_path_buf).unwrap_or_default())
}

/// A Unity Web Player build at the top or one folder down: a `.unity3d` file of the plugin's
/// own format ("UnityWeb", or "UnityRaw" uncompressed), with the page that embedded it (Caged).
fn unity_web(root: &Path) -> Option<PathBuf> {
    let dirs = std::iter::once(root.to_path_buf()).chain(std::fs::read_dir(root).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.is_dir()));
    dirs.flat_map(|d| std::fs::read_dir(d).into_iter().flatten().flatten().map(|e| e.path()))
        .find(|p| p.extension().is_some_and(|x| x == "unity3d") && (binary::head_contains(p, b"UnityWeb", 8) || binary::head_contains(p, b"UnityRaw", 8)))
        .map(|p| p.strip_prefix(root).map(Path::to_path_buf).unwrap_or_default())
}

/// A project's build file at the top or one folder down, when there's nothing to run.
fn source_project(root: &Path) -> Option<(PathBuf, &'static str)> {
    const MARKERS: [(&str, &str); 6] = [("project.godot", "a Godot project"), ("CMakeLists.txt", "a CMake project"), ("Cargo.toml", "a Rust project"), ("meson.build", "a Meson project"), ("configure.ac", "an Autotools project"), ("Makefile", "a Makefile project")];
    let dirs = std::iter::once(root.to_path_buf()).chain(std::fs::read_dir(root).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.is_dir()));
    dirs.flat_map(|d| {
        // A Processing sketch is a folder holding <folder name>.pde (Shooting Gallery).
        let sketch = d.file_name().map(|n| d.join(format!("{}.pde", n.to_string_lossy())));
        MARKERS.iter().map(move |(m, kind)| (d.join(m), *kind)).chain(sketch.map(|s| (s, "a Processing sketch")))
    })
    .find(|(p, _)| p.is_file())
    .map(|(p, kind)| (p.strip_prefix(root).unwrap_or(&p).to_path_buf(), kind))
}

fn optional_plugin(path: &std::path::Path) -> Option<&'static str> {
    let name = path.file_name()?;
    OPTIONAL_PLUGINS.iter().find(|(n, _)| name == *n).map(|(_, what)| *what)
}

pub const PYGAME_FONT: &str = "freesansbold.ttf";

/// See `Problem::PygameFont`: Some(where the default font is missing too, if it is).
fn pygame_font(game: &Game, main: &scan::Found) -> Option<Option<PygameFont>> {
    let program = game.full(main);
    let names = binary::bundle_names(&program)?;
    if !names.iter().any(|n| n == "pyi_rth_pkgres") {
        return None;
    }
    // pygame packed (one-file), or beside the program (a folder build). pygame 1 has the
    // overlay module, which pygame 2 dropped.
    let folder = ["pygame", "_internal/pygame"].iter().map(|d| program.parent().unwrap_or(&game.root).join(d)).find(|d| d.is_dir());
    let overlay = |n: &str| n.starts_with("overlay.");
    let pygame1 = names.iter().any(|n| n.strip_prefix("pygame/").is_some_and(overlay)) || folder.as_ref().is_some_and(|d| std::fs::read_dir(d).into_iter().flatten().flatten().any(|e| overlay(&e.file_name().to_string_lossy())));
    if !pygame1 || !binary::bundle_asks_default_font(&program) {
        return None;
    }
    Some(if names.iter().any(|n| n.starts_with("pygame/")) {
        (!names.iter().any(|n| n == &format!("pygame/{PYGAME_FONT}"))).then_some(PygameFont::Bundle)
    } else {
        folder.filter(|d| !d.join(PYGAME_FONT).exists()).and_then(|d| d.strip_prefix(&game.root).ok().map(|d| PygameFont::Folder(d.to_path_buf())))
    })
}

/// See `Problem::PygameMp3`: Some(whether its SDL is 2.0.x) when its mixer can't play the .mp3
/// files it has. The libraries' names, packed (one-file) or beside the program (folder builds:
/// `_internal`, auditwheel's `pygame.libs`), carry their versions: libSDL2_mixer-2.0.so.0.2.2
/// is 2.0.2 (2.6 is .so.0.600.0), auditwheel's copies add "-<hash>" (libSDL2-2-d6813302.0.so.0.14.0).
fn pygame_without_mp3(game: &Game, main: &scan::Found) -> Option<bool> {
    let program = game.full(main);
    let dir = program.parent()?;
    let mut names = binary::bundle_names(&program).unwrap_or_default();
    for sub in ["", "_internal", "pygame.libs", "_internal/pygame.libs"] {
        names.extend(std::fs::read_dir(dir.join(sub)).into_iter().flatten().flatten().map(|e| e.file_name().to_string_lossy().into_owned()));
    }
    let minor = |prefix: &str| names.iter().filter(|n| n.starts_with(prefix)).find_map(|n| n.split(".so.").nth(1)?.split('.').nth(1)?.parse::<u32>().ok());
    let mixer = minor("libSDL2_mixer-2")?;
    let decoder = names.iter().any(|n| ["mpg123", "smpeg", "libmad"].iter().any(|d| n.contains(d)));
    let mp3 = names.iter().any(|n| n.to_lowercase().ends_with(".mp3")) || has_ending(dir, "mp3", 4);
    (mixer < 600 && !decoder && mp3).then(|| minor("libSDL2-2").is_some_and(|m| m < 100))
}

/// Whether a file with that ending (any case) is in the folder, at most `depth` folders down.
fn has_ending(dir: &Path, ending: &str, depth: usize) -> bool {
    std::fs::read_dir(dir).into_iter().flatten().flatten().any(|e| {
        let path = e.path();
        match e.file_type() {
            Ok(t) if t.is_dir() => depth > 0 && has_ending(&path, ending, depth - 1),
            Ok(t) if t.is_file() => path.extension().is_some_and(|x| x.eq_ignore_ascii_case(ending)),
            _ => false,
        }
    })
}

/// A font to stand in for pygame's FreeSans Bold: the closest the system has.
fn stand_in_font() -> Option<PathBuf> {
    let out = std::process::Command::new("fc-match").args(["-f", "%{file}", "FreeSans:bold"]).output().ok()?;
    let file = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    (file.is_file() && file.extension().is_some_and(|e| e.eq_ignore_ascii_case("ttf"))).then_some(file)
}

pub fn check(game: &Game, system: &System) -> Report {
    let mut windows = Vec::new();
    let mut installers = Vec::new();
    for f in &game.files {
        if let Binary::Pe(pe) = &f.binary {
            match pe.installer {
                Some(kind) => installers.push((f.path.clone(), kind)),
                // Ren'Py keeps its Windows Python and helpers in lib/windows-* (8: lib/py3-windows-*):
                // not a Windows build.
                None if f.path.components().any(|c| c.as_os_str().to_string_lossy().trim_start_matches("py3-").trim_start_matches("py2-").starts_with("windows-")) && game.main.is_some() => {}
                None if !scan::not_the_game(&f.path) && !scan::bundled_dosbox(&f.path) && !(pe.dotnet && (game.main.is_some() || game.portable.is_some())) => windows.push((f.path.clone(), pe.bits)),
                None => {}
            }
        }
    }
    let mut problems = Vec::new();
    let mut report = Report { problems: Vec::new(), libs_checked: 0, windows, installers, glibc: system.glibc, packed: false, pyinstaller: false, launcher_libs: Vec::new(), appimage: None, inside_appimage: false, old_runtime: system.old_runtime.as_ref().map(|r| r.run.clone()), harmless: Vec::new() };

    let Some((main, elf)) = game.main() else {
        match game.portable.map(|i| &game.files[i]) {
            Some(f) if matches!(f.binary, Binary::Love) => problems.push(Problem::Love { file: f.path.clone(), version: game.engine.as_ref().and_then(|e| e.version.clone()), love: system.love() }),
            Some(f) if matches!(f.binary, Binary::Jar) => {
                let needs = binary::jar_needs(&game.full(f));
                let java = if needs.nashorn {
                    system.java_up_to(14).or_else(|| system.java())
                } else if needs.old_lwjgl3 {
                    system.java_up_to(16).or_else(|| system.java())
                } else if needs.applet {
                    system.java_lts().or_else(|| system.java())
                } else {
                    system.java()
                };
                problems.push(Problem::Java { file: f.path.clone(), java });
            }
            Some(f) if matches!(f.binary, Binary::Script) => {
                let java = scan::processing_java(&game.root, &f.path).or_else(|| system.java_lts()).or_else(|| system.java());
                problems.push(Problem::Java { file: f.path.clone(), java });
            }
            Some(f) if matches!(f.binary, Binary::Flash) => problems.push(Problem::Flash { file: f.path.clone(), ruffle: system.ruffle() }),
            Some(f) if matches!(f.binary, Binary::Tic) => problems.push(Problem::Tic { file: f.path.clone(), tic80: system.tic80() }),
            Some(scan::Found { path, binary: Binary::Rom(console) }) => problems.push(Problem::Rom { file: path.clone(), console, mgba: system.mgba() }),
            Some(f) if matches!(f.binary, Binary::Dos) => problems.push(Problem::Dos { file: f.path.clone(), dosbox: system.dosbox() }),
            Some(f) if matches!(f.binary, Binary::RpgMaker) => {
                let dir = f.path.parent().unwrap_or(Path::new("")).to_path_buf();
                let rtp = !crate::rpgmaker::full_package(&game.root.join(&dir));
                problems.push(Problem::RpgMaker { dir, easyrpg: system.easyrpg(), rtp });
            }
            Some(f) if matches!(f.binary, Binary::Pe(_)) && f.path.extension().is_some_and(|e| e.eq_ignore_ascii_case("dll")) => {
                let version = crate::dotnet::framework(&game.root.join(f.path.with_extension("runtimeconfig.json"))).unwrap_or_default();
                let dotnet = crate::dotnet::installed(&version);
                problems.push(Problem::Dotnet { file: f.path.clone(), version, dotnet });
            }
            Some(f) if matches!(f.binary, Binary::Pe(_)) => {
                let mono = system.mono();
                let libs = mono_libs(game, f, mono.as_deref(), system).into_iter().map(|l| {
                    let runtime = system.old_runtime.as_ref().is_some_and(|r| r.has(&l, 64));
                    (l, runtime)
                });
                // Beside a MonoKickstart program: that one's Mono can't load these (scan::kick_exe).
                let dir = game.root.join(f.path.parent().unwrap_or(Path::new("")));
                let kick = game.files.iter().any(|k| k.path.parent() == f.path.parent() && k.path.to_string_lossy().contains(".bin.x86"));
                let standard = if kick { scan::net_standard(&dir) } else { Vec::new() };
                problems.push(Problem::Mono { file: f.path.clone(), libs: libs.collect(), mono, standard });
            }
            Some(f) => {
                let python = game.files.iter().filter_map(|f| match &f.binary {
                    Binary::Python(p) => Some(p),
                    _ => None,
                });
                let v2 = python.fold(false, |v2, p| v2 || p.v2);
                let plan = (!v2).then(|| crate::python::plan(&game.root, &game.files, &f.path));
                problems.push(Problem::Python { file: f.path.clone(), v2, plan });
            }
            None => {
                let damaged = game.files.iter().find(|f| matches!(f.binary, Binary::Damaged));
                let package = game.files.iter().find_map(|f| match f.binary {
                    Binary::Package(kind) => Some((f.path.clone(), kind)),
                    _ => None,
                });
                let udk = report.installers.iter().find(|(_, kind)| *kind == "UnSetup").map(|(p, _)| p.clone());
                let source = if game.windows.is_none() { source_project(&game.root) } else { None };
                let web = if game.windows.is_none() { web_export(&game.root) } else { None };
                let unity_web = if game.windows.is_none() { unity_web(&game.root) } else { None };
                problems.push(match (damaged, game.archive, package) {
                    (Some(f), _, _) => Problem::Damaged(f.path.clone()),
                    _ if game.packed.is_some() => {
                        let file = game.files[game.packed.unwrap()].path.clone();
                        let kind = crate::rpgmaker::packed(&game.root.join(&file)).unwrap_or("RPG Maker");
                        let dest = crate::rpgmaker::unpacked_dir(&game.root.join(&file)).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                        Problem::RpgMakerPacked { file, kind, dest }
                    }
                    _ if game.linux_installer.is_some() => Problem::LinuxInstaller(game.files[game.linux_installer.unwrap()].path.clone()),
                    _ if udk.is_some() && game.windows.is_none() => Problem::UdkInstaller(udk.unwrap()),
                    _ if game.cd_installer.is_some() => Problem::CdFiles { installer: game.files[game.cd_installer.unwrap()].path.clone() },
                    _ if game.disc.is_some() => {
                        let file = game.files[game.disc.unwrap()].path.clone();
                        let files = crate::disc::files(&game.root.join(&file));
                        let installer = files.as_ref().and_then(|f| crate::disc::installer(&game.root.join(&file), f, &game.name));
                        let files = files.map(|f| f.iter().filter(|f| f.to_lowercase().ends_with(".exe")).count());
                        Problem::Disc { file, files, installer }
                    }
                    _ if source.is_some() => {
                        let (file, kind) = source.unwrap();
                        Problem::Source(file, kind)
                    }
                    _ if web.is_some() => Problem::Web(web.unwrap()),
                    _ if unity_web.is_some() => Problem::UnityWeb(unity_web.unwrap()),
                    (None, Some(i), _) => Problem::Archive(game.files[i].path.clone()),
                    (None, None, Some((path, kind))) => Problem::Package(path, kind),
                    (None, None, None) => Problem::NoLinuxBuild,
                });
            }
        }
        // The game's program, run or installed from the CD, with copy protection Wine can't run.
        let protected = game.files.iter().find_map(|f| match &f.binary {
            Binary::Pe(pe) if pe.installer.is_none() && (game.windows.is_some_and(|i| game.files[i].path == f.path) || game.cd_installer.is_some()) => pe.protection.map(|what| (what, f)),
            _ => None,
        });
        if let Some((what, f)) = protected {
            let name = f.path.file_name().map(|n| n.to_string_lossy().to_lowercase());
            let others = game.files.iter().filter(|o| o.path != f.path && o.path.file_name().map(|n| n.to_string_lossy().to_lowercase()) == name && matches!(&o.binary, Binary::Pe(pe) if pe.protection.is_none())).map(|o| o.path.clone()).collect();
            problems.push(Problem::Protected { what, file: f.path.clone(), others });
        }
        if let Some(i) = game.windows.filter(|&i| unity_data_missing(game, &game.files[i])) {
            problems.retain(|p| !matches!(p, Problem::NoLinuxBuild));
            problems.push(Problem::UnityDataMissing(game.files[i].path.clone()));
            report.problems = problems;
            return report;
        }
        if let Some(i) = game.windows {
            let dlls = crate::proton::ignored_dlls(&game.full(&game.files[i]));
            if !dlls.is_empty() {
                problems.push(Problem::OwnWindowsDlls(dlls));
            }
            let ini = game.full(&game.files[i]).with_file_name("Game.ini");
            if let Some((kind, true)) = crate::rpgmaker::rgss(&ini) {
                problems.push(Problem::RgssRtp(kind));
            }
            let parts = crate::proton::missing_parts(&game.full(&game.files[i]));
            if !parts.is_empty() {
                problems.push(Problem::MissingWindowsParts(parts));
            }
        }
        report.problems = problems;
        return report;
    };
    if elf.machine != EM_X86_64 && elf.machine != EM_386 {
        // Nothing else matters: it can't run here, and its libraries (down to ld.so) are ARM's too.
        problems.push(Problem::WrongMachine(elf.machine));
        report.problems = problems;
        return report;
    }
    if unity_data_missing(game, main) {
        problems.push(Problem::UnityDataMissing(main.path.clone()));
        report.problems = problems;
        return report;
    }
    report.packed = elf.packed;
    report.pyinstaller = elf.pyinstaller && system.refuses_exec_stack();
    // Nearly every PyInstaller pygame build carries libstdc++, harmless while SDL draws without
    // OpenGL; ones drawing through PyOpenGL load the driver (Tempest Run's runs anyway: its
    // libstdc++ may be newer, which can't be read from the packed copy; the preload is harmless).
    if elf.pyinstaller {
        let dir = game.full(main).parent().map(Path::to_path_buf).unwrap_or_default();
        let beside = dir.join("libstdc++.so.6").is_file() || dir.join("_internal/libstdc++.so.6").is_file();
        let opengl = || dir.join("OpenGL").is_dir() || dir.join("_internal/OpenGL").is_dir() || binary::contains(&game.full(main), b"OpenGL.GL");
        if (beside || binary::bundle_names(&game.full(main)).is_some_and(|n| n.iter().any(|n| n == "libstdc++.so.6"))) && opengl() {
            problems.push(Problem::BundledLibstdcxx(system.path("libstdc++.so.6", elf.bits).map(String::from)));
        }
    }
    if elf.pyinstaller {
        if let Some(sdl20) = pygame_without_mp3(game, main) {
            problems.push(Problem::PygameMp3 { sdl20 });
        }
    }
    if let Some(image) = game.files.iter().find(|f| f.path.file_name().is_some_and(|n| n == "liballegro_image.so.5.2")) {
        if !binary::contains(&game.full(image), b"_al_load_webp") && has_ending(&game.root, "webp", 4) {
            problems.push(Problem::AllegroWebp(image.path.clone()));
        }
    }
    if elf.pyinstaller {
        if let Some(missing) = pygame_font(game, main) {
            let font = missing.as_ref().and_then(|_| stand_in_font());
            problems.push(Problem::PygameFont { program: main.path.clone(), missing, font });
        }
    }
    let mode = std::fs::metadata(game.full(main)).map(|m| m.permissions().mode()).unwrap_or(0);
    if mode & 0o100 == 0 {
        problems.push(Problem::NotExecutable(main.path.clone()));
    }
    if let Some((config, folders)) = missing_folders(&game.root, main.path.parent().unwrap_or(Path::new(""))) {
        problems.push(Problem::MissingFolders(config, folders));
    }
    if let Some(l) = game.launcher().filter(|l| binary::crlf(&game.full(l))) {
        problems.push(Problem::WindowsLineEndings(l.path.clone()));
    }

    // Everything built for the same machine as the program: its libraries and plugins.
    let same: Vec<(&PathBuf, &Elf)> = game
        .files
        .iter()
        .filter_map(|f| match &f.binary {
            Binary::Elf(e) if e.machine == elf.machine && e.bits == elf.bits => Some((&f.path, e)),
            _ => None,
        })
        .collect();
    let mut by_name: HashMap<&OsStr, (&PathBuf, &Elf)> = HashMap::new();
    for &(p, e) in &same {
        if let Some(n) = p.file_name() {
            by_name.entry(n).or_insert((p, e));
        }
    }
    // The program's DT_RPATH also applies to everything it loads that has no DT_RUNPATH.
    let main_dir = game.root.join(main.path.parent().unwrap_or(Path::new("")));
    // ld.so takes the program's $ORIGIN from /proc/self/exe, where links are resolved (an
    // AppImage's AppRun is often a link to usr/bin/<name>, whose RUNPATH is $ORIGIN/../lib);
    // a library's comes from the path it was found by.
    let main_origin = std::fs::canonicalize(game.full(main)).ok().and_then(|p| p.parent().map(Path::to_path_buf)).unwrap_or_else(|| main_dir.clone());
    let main_rpath = if elf.runpath.is_empty() { expand(&elf.rpath, &main_origin, &main_dir) } else { Vec::new() };
    let search = |path: &Path, e: &Elf| -> Vec<PathBuf> {
        let dir = if path == main.path { main_origin.clone() } else { game.root.join(path.parent().unwrap_or(Path::new(""))) };
        if e.runpath.is_empty() { expand(&e.rpath, &dir, &main_dir).into_iter().chain(main_rpath.iter().cloned()).collect() } else { expand(&e.runpath, &dir, &main_dir) }
    };
    // A launcher script next to the program or above it may point LD_LIBRARY_PATH anywhere in
    // the folder; it isn't evaluated.
    let launcher_path = game.files.iter().any(|f| {
        matches!(f.binary, Binary::Script) && f.path.parent().is_some_and(|d| main.path.starts_with(d)) && binary::sets_library_path(&game.full(f))
    });
    let reachable = |path: &Path, e: &Elf, lib: &str| system.has(lib, elf.bits) || search(path, e).iter().any(|d| d.join(lib).is_file());

    // A bundled library the system or Steam's runtime also has may be loaded from there instead
    // (Gravitus Minus' launcher puts its own folder last), so what its copy misses isn't certain.
    let replaceable = |p: &PathBuf| {
        p != &main.path
            && p.file_name().map(|n| n.to_string_lossy().into_owned()).is_some_and(|n| system.has(&n, elf.bits) || system.old_runtime.as_ref().is_some_and(|r| r.has(&n, elf.bits)))
    };
    // What ld.so loads at start: the program, then what each needs, found where it looks. Only
    // these have to be found that way; plugins loaded later may find theirs among what's
    // already loaded, or by a path the game passes, so any copy in the folder counts for them.
    let mut startup: HashSet<&PathBuf> = HashSet::from([&main.path]);
    let mut queue: VecDeque<(&PathBuf, &Elf)> = VecDeque::from([(&main.path, elf)]);
    while let Some((path, e)) = queue.pop_front() {
        for lib in &e.needed {
            let Some(&(p, dep)) = by_name.get(OsStr::new(lib)) else { continue };
            // ld.so gets the game's copy through the search path or a launcher's LD_LIBRARY_PATH,
            // or, when the system lacks it, through the LD_LIBRARY_PATH the fix sets for it
            // (so what that copy needs counts too: Liberation Circuit's Allegro needs libwrap).
            let loads_copy = launcher_path || search(path, e).iter().any(|d| d.join(lib).is_file()) || !system.has(lib, elf.bits);
            if loads_copy && !replaceable(p) && startup.insert(p) {
                queue.push_back((p, dep));
            }
        }
    }
    let mut missing: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
    let mut later: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
    let mut misplaced: BTreeMap<String, PathBuf> = BTreeMap::new();
    let mut optional: BTreeMap<&'static str, Vec<String>> = BTreeMap::new();
    for &(path, e) in &same {
        let at_start = startup.contains(path);
        for lib in &e.needed {
            let copy = by_name.get(OsStr::new(lib)).map(|(p, _)| *p);
            if reachable(path, e, lib) {
                continue;
            }
            if let Some(copy) = copy.filter(|_| !at_start || launcher_path) {
                if at_start && !system.has(lib, elf.bits) && !report.launcher_libs.iter().any(|(l, _)| l == lib) {
                    report.launcher_libs.push((lib.clone(), copy.parent().unwrap_or(Path::new("")).to_path_buf()));
                }
                continue;
            }
            if let Some(what) = optional_plugin(path).or_else(|| java_window_part(path, lib)) {
                optional.entry(what).or_default().push(lib.clone());
                continue;
            }
            if let Some(copy) = copy {
                misplaced.insert(lib.clone(), copy.parent().unwrap_or(Path::new("")).to_path_buf());
            }
            (if at_start { &mut missing } else { &mut later }).entry(lib.clone()).or_default().push(path.clone());
        }
    }
    // The window may come from any of the game's own files: OpenFL's lime.ndll and SDL2
    // builds load X11 themselves, and .NET, Haxe and Lisp runtimes use termios too.
    let window = |e: &Elf| e.needed.iter().any(|l| WINDOW_LIBS.iter().any(|w| l.starts_with(w)));
    // Text games are small, and bundled window libraries show in their names (libSDL2-2.0.so.0,
    // OpenFL's lime.ndll): both rule out reading big runtimes (.NET, OpenRA) for nothing.
    let bundled_window = game.files.iter().filter_map(|f| f.path.file_name()).map(|n| n.to_string_lossy()).any(|n| WINDOW_LIBS.iter().any(|w| n.starts_with(w)) || n.ends_with(".ndll"));
    let small = same.iter().map(|(p, _)| std::fs::metadata(game.root.join(p)).map_or(0, |m| m.len())).sum::<u64>() < 64 << 20;
    if game.engine.is_none() && small && !bundled_window && !elf.packed && !elf.pyinstaller && !elf.needed.is_empty() && !same.iter().any(|(_, e)| window(e)) {
        let needed: Vec<&String> = same.iter().filter(|(p, _)| startup.contains(p)).flat_map(|(_, e)| &e.needed).collect();
        let curses = needed.iter().any(|l| ["libncurses", "libtinfo", "libcurses"].iter().any(|c| l.starts_with(c)));
        // Names of window libraries its files may load later (dlopen), read only when it sets
        // the terminal up at all.
        let names: Vec<Vec<u8>> = WINDOW_LIBS.iter().flat_map(|w| [format!("{w}.so"), format!("{w}-")]).map(String::into_bytes).collect();
        let names: Vec<&[u8]> = names.iter().map(Vec::as_slice).collect();
        let termios = curses || binary::contains(&game.full(main), b"\0tcsetattr\0");
        if termios && !same.iter().any(|(p, _)| binary::find_groups(&game.root.join(p), &[&names])[0]) {
            let old_curses = needed.iter().any(|l| ["libncurses.so.5", "libncursesw.so.5", "libtinfo.so.5"].contains(&l.as_str()));
            problems.push(Problem::Terminal { old_curses });
        }
    }
    // A program needing only glibc's parts may load the rest itself, by names it keeps as text
    // (D's Derelict bindings: Blue Bug Bob quit with "libSDL2_image.so ... cannot open shared
    // object file"). It needs one of each list at start.
    let loads_itself = !elf.needed.is_empty() && elf.needed.iter().all(|l| glibc_part(l)) && !elf.pyinstaller && !elf.packed;
    if loads_itself && binary::appimage_offset(&game.full(main)).is_none() && std::fs::metadata(game.full(main)).is_ok_and(|m| m.len() < 64 << 20) {
        for names in binary::dlopen_lists(&game.full(main)) {
            if names.iter().any(|n| !n.contains('/') && system.has(n, elf.bits)) {
                continue;
            }
            // The versioned name, as packages and Steam's runtime carry it.
            if let Some(name) = names.iter().filter(|n| !n.contains('/')).max_by_key(|n| n.matches('.').count()) {
                missing.entry(name.clone()).or_default().push(main.path.clone());
            }
        }
    }
    // A LuaJIT game loads its libraries from Lua (`ffi.load "GLee"`), by the unversioned names
    // only development packages install (Phone Fighter: libGLee.so, libSDL.so). The game's own
    // ones are found through its launcher's LD_LIBRARY_PATH.
    let luajit = main.path.file_name().is_some_and(|n| n.to_string_lossy().starts_with("luajit")) || elf.needed.iter().any(|l| l.starts_with("libluajit"));
    if luajit {
        let own: HashSet<String> = game.files.iter().filter_map(|f| f.path.file_name()).map(|n| n.to_string_lossy().into_owned()).collect();
        for (lib, file) in lua_ffi_libs(&game.root) {
            let found = own.contains(&lib) || system.has(&lib, elf.bits) || system.old_runtime.as_ref().is_some_and(|r| r.has(&lib, elf.bits));
            if !found && !missing.contains_key(&lib) {
                // A versioned copy the game ships (libGLEW.so.1.11 for "GLEW"): what the name
                // pointed to on the developer's system.
                if let Some(copy) = game.files.iter().find(|f| f.path.file_name().is_some_and(|n| n.to_string_lossy().starts_with(&format!("{lib}.")))) {
                    misplaced.insert(lib.clone(), copy.path.parent().unwrap_or(Path::new("")).to_path_buf());
                }
                missing.entry(lib).or_default().push(file);
            }
        }
    }
    // .NET Core before 5 loads OpenSSL itself (dlopen, no DT_NEEDED) through its shim, and only
    // knows 1.0 and 1.1; without one it aborts at the first HTTPS request ("No usable version
    // of libssl was found": KlSS, .NET Core 2.2, fetching a level). 1.1 is what can be fetched.
    let shim_name = "System.Security.Cryptography.Native.OpenSsl.so";
    let shim_file = main.path.parent().unwrap_or(Path::new("")).join(shim_name);
    // A single-file bundle carries the shim inside the program.
    let shim = if dotnet_core(&main_dir, &main.path).is_some_and(|v| v < 5) && !["libssl.so.1.1", "libssl.so.1.0.2", "libssl.so.1.0.0", "libssl.so.10"].iter().any(|l| system.has(l, elf.bits)) {
        if game.root.join(&shim_file).is_file() {
            Some(shim_file)
        } else {
            binary::contains(&game.full(main), shim_name.as_bytes()).then(|| main.path.clone())
        }
    } else {
        None
    };
    if let Some(shim) = shim {
        for lib in ["libssl.so.1.1", "libcrypto.so.1.1"] {
            later.entry(lib.to_string()).or_default().push(shim.clone());
        }
    }
    // A library missing at start and later is a start problem.
    later.retain(|lib, _| !missing.contains_key(lib));
    let screen = "Unity's start-up resolution window";
    if optional.contains_key(screen) && game.engine.as_ref().and_then(|e| e.version.as_deref()).is_some_and(unity_tiny_start) {
        optional.remove(screen);
        problems.push(Problem::UnityResolution(system.old_runtime.as_ref().map(|r| r.run.clone())));
    }
    report.libs_checked = same.iter().filter(|(_, e)| !e.program).count();
    report.harmless = optional
        .into_iter()
        .map(|(what, mut libs)| {
            libs.sort();
            libs.dedup();
            (what, libs)
        })
        .collect();
    let sdl2_addons = ["libSDL2_mixer-2.0.so.0", "libSDL2_image-2.0.so.0", "libSDL2_ttf-2.0.so.0", "libSDL2_net-2.0.so.0"];
    let addon_from_runtime = missing.keys().chain(later.keys()).any(|l| sdl2_addons.contains(&l.as_str()) && system.old_runtime.as_ref().is_some_and(|r| r.has(l, elf.bits)));
    // sdl2-compat loads SDL3 itself (dlopen), so it's known by its own settings' names.
    let compat = system.path("libSDL2-2.0.so.0", elf.bits).is_some_and(|p| binary::contains(Path::new(p), b"SDL2COMPAT_"));
    let sdl2_mix = (addon_from_runtime && compat).then(|| system.old_runtime.as_ref().and_then(|r| r.path("libSDL2-2.0.so.0", elf.bits))).flatten();
    if !missing.is_empty() || !later.is_empty() {
        let in_old_runtime = missing.keys().chain(later.keys()).filter(|l| system.old_runtime.as_ref().is_some_and(|r| r.has(l, elf.bits))).cloned().collect();
        problems.push(Problem::MissingLibs { bits: elf.bits, libs: missing, later, in_old_runtime, misplaced });
    }
    if let Some(sdl2) = sdl2_mix {
        problems.push(Problem::Sdl2Mix(sdl2.display().to_string()));
    }

    // libpulse before 1.0, bundled, doesn't look for the server in XDG_RUNTIME_DIR, so it finds
    // none and the game gets no sound (Sascha's PyInstaller bundle, libpulse 0.9.22).
    let old_pulse = game.files.iter().any(|f| f.path.file_name().is_some_and(|n| n.to_string_lossy().starts_with("libpulsecommon-0.")))
        || (elf.pyinstaller && binary::contains(&game.full(main), b"libpulsecommon-0."));
    if old_pulse {
        problems.push(Problem::OldPulse(pulse_socket()));
    }

    if main.path.to_string_lossy().contains(".bin.x86") && binary::contains(&game.full(main), b"monomachineconfig") {
        let gone: Vec<String> = ["monoconfig", "monomachineconfig"].iter().filter(|f| !main_dir.join(f).exists()).map(|f| f.to_string()).collect();
        if !gone.is_empty() {
            problems.push(Problem::KickFiles(gone));
        }
    }

    // A bundled Java: its own sound opens the card directly (see System::pipewire_alsa).
    if let Some(plugin) = (game.files.iter().any(|f| f.path.file_name().is_some_and(|n| n == "libjsound.so")) && uses_java_sound(game)).then(|| system.pipewire_alsa(elf.bits)).flatten() {
        problems.push(Problem::JavaSound(plugin));
    }

    // A 32-bit game that can only play sound through ALSA (nothing in it knows PulseAudio or
    // PipeWire) finds no device where ALSA's default is the 64-bit-only PipeWire plugin.
    // RogueBox's bundled pygame (SDL 1.2) then quits loading its first sound.
    if elf.bits == 32 && system.alsa32_broken() {
        let (alsa, pulse) = same.iter().map(|(p, _)| binary::find_groups(&game.root.join(p), &[&[b"libasound"], &[b"libpulse", b"libpipewire"]])).fold((false, false), |(a, p), f| (a || f[0], p || f[1]));
        if alsa && !pulse {
            problems.push(Problem::Alsa32(system.old_runtime.as_ref().and_then(|r| r.alsa_pulse32())));
        }
    }

    // glibc only refuses to make the stack executable for libraries loaded later; if the program
    // already asks for one, these load fine too.
    let exec_stack: Vec<PathBuf> = same.iter().filter(|(p, e)| !e.program && e.exec_stack && !startup.contains(p)).map(|(p, _)| (*p).clone()).collect();
    if !exec_stack.is_empty() && !elf.exec_stack && system.refuses_exec_stack() {
        problems.push(Problem::ExecStack(exec_stack));
    }

    // A bundled library loaded first (the launcher's LD_LIBRARY_PATH, or the RPATH of what
    // needs it) is also what the system's libraries loaded alongside get. If one of those needs
    // a function only the system's newer copy has, it fails to load (Darkness Looming's old
    // libgmodule vs the system's libgio: g_module_open_full).
    let mut first: Vec<(&String, &PathBuf)> = by_name
        .iter()
        .filter_map(|(n, (p, _))| {
            let name = n.to_str()?;
            let needed_from = same.iter().any(|(q, e)| e.needed.iter().any(|l| l == name) && (launcher_path || search(q, e).iter().any(|d| d.join(name).is_file())));
            (system.has(name, elf.bits) && !glibc_part(name) && needed_from).then(|| (same.iter().find_map(|(_, e)| e.needed.iter().find(|l| *l == name)), *p))
        })
        .filter_map(|(n, p)| n.map(|n| (n, p)))
        .collect();
    // In name order: the report came out differently from run to run (Because).
    first.sort();
    if !first.is_empty() {
        let mut users: BTreeSet<String> = BTreeSet::new();
        for (_, e) in &same {
            users.extend(e.needed.iter().filter(|l| !by_name.contains_key(OsStr::new(l.as_str())) && system.has(l, elf.bits) && !glibc_part(l)).cloned());
        }
        // And system libraries the game loads by way of others, which the walk can't see: the
        // sound server's client loaded by the game's SDL or OpenAL brings libsndfile, which then
        // gets Star Turtle 64's bundled libmpg123 (run.sh puts it first) and fails on
        // mpg123_info2. Only for what PulseAudio, PipeWire and ALSA load: a check against
        // every system library that needs the name named copies the game never loads.
        let sound = ["libpulse.so.0", "libpulse-simple.so.0", "libasound.so.2", "libpipewire-0.3.so.0"];
        // Followed through RUNPATHs too: libpulse finds libpulsecommon in /usr/lib64/pulseaudio.
        let mut queue: Vec<(String, PathBuf)> = sound.iter().filter_map(|l| Some((l.to_string(), PathBuf::from(system.path(l, elf.bits)?)))).collect();
        let mut closure: BTreeSet<String> = BTreeSet::new();
        while let Some((lib, path)) = queue.pop() {
            if !closure.insert(lib) {
                continue;
            }
            if let Some(Binary::Elf(e)) = binary::identify(&path) {
                for l in e.needed.iter().filter(|l| !glibc_part(l)) {
                    let found = system.path(l, elf.bits).map(PathBuf::from).or_else(|| e.runpath.iter().map(|d| Path::new(d).join(l)).find(|p| p.is_file()));
                    if let Some(p) = found {
                        queue.push((l.clone(), p));
                    }
                }
            }
        }
        let loads_sound = same.iter().any(|(_, e)| e.needed.iter().any(|l| ["libSDL2", "libSDL-1.2", "libopenal", "libpulse", "libasound"].iter().any(|s| l.starts_with(s))));
        if loads_sound {
            users.extend(closure.into_iter().filter(|l| !by_name.contains_key(OsStr::new(l.as_str()))));
        }
        let mut seen: BTreeSet<&String> = BTreeSet::new();
        for user in &users {
            let Some(user_path) = system.path(user, elf.bits) else { continue };
            let Some(Binary::Elf(user_elf)) = binary::identify(Path::new(user_path)) else { continue };
            let Some(user_syms) = binary::symbols(Path::new(user_path)) else { continue };
            for (name, copy) in first.iter().filter(|(n, _)| user_elf.needed.contains(n)) {
                let (Some(sys), Some(own)) = (system.path(name, elf.bits).and_then(|p| binary::symbols(Path::new(p))), binary::symbols(&game.root.join(copy))) else { continue };
                let mut lacking: Vec<&String> = user_syms.undefined.iter().filter(|s| sys.defined.contains(*s) && !own.defined.contains(*s)).collect();
                lacking.sort();
                if let Some(example) = lacking.first() {
                    if seen.insert(name) {
                        problems.push(Problem::Shadowed((*copy).clone(), user.clone(), (*example).clone(), system.path(name, elf.bits).map(String::from)));
                    }
                }
            }
        }
    }

    // An AppImage: unpack its squashfs (with the system's unsquashfs, never running it) and
    // check what's inside like a folder.
    if let Some(offset) = binary::appimage_offset(&game.full(main)) {
        report.appimage = appimage_inside(&game.full(main), offset, system);
    }

    let clashes = symbol_clashes(game, &main.path, &same);
    if !clashes.is_empty() {
        problems.push(Problem::SymbolClash(clashes));
    }
    // Unity 4 players have it too, but run fine (Succulent, Forever, Hurt Me Plenty); Unity 5.0
    // crashes (2:22AM, Flail Rider).
    let unity4 = game.engine.as_ref().is_some_and(|e| e.name == "Unity" && e.version.as_deref().is_some_and(|v| v.starts_with("4.")));
    // Flash Player's projector has one too, and crashes with the system's preloaded ("free():
    // invalid size": Ending); without it, it runs.
    let projector = game.engine.as_ref().is_some_and(|e| e.name == "Flash Player projector");
    if !unity4 && !projector && !elf.needed.iter().any(|l| l == "libstdc++.so.6") && binary::symbols(&game.full(main)).is_some_and(|s| s.defined.iter().any(|d| d.starts_with("_ZNSt8ios_base4Init"))) {
        problems.push(Problem::OwnLibstdcxx(system.path("libstdc++.so.6", elf.bits).map(String::from)));
    }
    let mut startup_files: Vec<&PathBuf> = startup.iter().copied().filter(|p| !p.to_string_lossy().contains("libharfbuzz")).collect();
    startup_files.sort();
    // Only a problem when the system's HarfBuzz gets loaded too (JK Assassins: GTK 2 → pango);
    // Ren'Py 8's librenpython.so exports its own but loads neither, and runs.
    let system_text = same.iter().any(|(p, e)| startup.contains(p) && e.needed.iter().any(|l| ["libpango", "libgtk", "libharfbuzz"].iter().any(|s| l.starts_with(s))));
    if let Some(file) = startup_files.into_iter().filter(|_| system_text).find(|p| binary::symbols(&game.root.join(p)).is_some_and(|s| s.defined.contains("hb_face_reference_table"))) {
        let in_program = binary::symbols(&game.full(main)).is_some_and(|s| s.defined.contains("hb_face_create"));
        let preload = if in_program {
            let pango = ["libpango-1.0.so.0", "libpangoft2-1.0.so.0", "libpangocairo-1.0.so.0"].map(|l| system.old_runtime.as_ref().and_then(|r| r.path(l, elf.bits)));
            pango.iter().all(Option::is_some).then(|| pango.iter().flatten().map(|p| p.display().to_string()).collect::<Vec<_>>().join(":"))
        } else {
            system.path("libharfbuzz.so.0", elf.bits).map(String::from)
        };
        problems.push(Problem::OwnHarfbuzz(file.clone(), preload, in_program));
    }
    if let Some(version) = old_dotnet(&main_dir, &main.path) {
        problems.push(Problem::DotnetIcu(version));
    }
    if let Some(version) = dotnet_framework(&main_dir, &main.path).filter(|v| crate::dotnet::installed(v).is_none()) {
        problems.push(Problem::DotnetRuntime(version));
    }
    let statics = dotnet_missing_static(&main_dir, &main.path);
    if !statics.is_empty() {
        problems.push(Problem::DotnetStatic(statics));
    }
    if game.engine.as_ref().is_some_and(|e| e.name == "NW.js") {
        let lib = [main_dir.join("lib/libnw.so"), game.full(main)].into_iter().find(|p| p.is_file());
        if let Some(version) = lib.and_then(|l| chromium_version(&l)).filter(|v| v.split('.').next().and_then(|m| m.parse::<u32>().ok()).is_some_and(|m| (59..74).contains(&m))) {
            problems.push(Problem::NwjsFonts(version));
        }
    }
    let fna_missing = fna_libs(&main_dir, elf.bits).into_iter().filter(|l| !system.has(l, elf.bits)).collect::<Vec<_>>();
    if !fna_missing.is_empty() {
        problems.push(Problem::FnaLibs(fna_missing));
    }

    let old_unity = game.engine.as_ref().filter(|e| e.name == "Unity").and_then(|e| e.version.as_deref()).and_then(|v| {
        let mut parts = v.split('.');
        Some((parts.next()?.parse::<u32>().ok()?, parts.next()?.parse::<u32>().ok()?))
    });
    if old_unity.is_some_and(|v| v < (5, 4)) {
        problems.push(Problem::UnitySingleInstance);
    }
    // Unity 5.0.1 and newer fall back to fonts Linux systems have (Liberation Sans, then
    // Droid and Noto); 2017 and newer draw text here too (Karlson, Baldi's Basics), with the
    // names in UnityPlayer.so not readable as text, so only 4.x and 5.0 players are read.
    if old_unity.is_some_and(|v| v <= (5, 0)) {
        let player = [main_dir.join("UnityPlayer.so"), game.full(main)].into_iter().find(|p| p.is_file());
        if let Some(names) = player.and_then(|p| unity_missing_font(&p)) {
            problems.push(Problem::UnityFont(names));
        }
        let shaders = unity_shaders(&game.root, &main.path);
        if shaders >= UNITY_MANY_SHADERS {
            problems.push(Problem::UnityShaders(shaders));
        }
        let videos = h264_videos(&game.root, &main.path);
        if !videos.is_empty() {
            problems.push(Problem::UnityH264(videos));
        }
    }

    report.problems = problems;
    report
}

/// The Chromium version built into NW.js: the "65.0.3325.146" that appears most in the library.
fn chromium_version(path: &Path) -> Option<String> {
    let data = std::fs::read(path).ok()?;
    let mut counts: HashMap<&[u8], usize> = HashMap::new();
    for (i, w) in data.windows(4).enumerate() {
        // "NN.0." followed by a 4-digit build and a patch number.
        if w[2..] == *b".0" && w[0].is_ascii_digit() && w[1].is_ascii_digit() && (i == 0 || !data[i - 1].is_ascii_digit()) {
            let end = data[i..].iter().take(20).position(|c| !c.is_ascii_digit() && *c != b'.').map_or(i, |e| i + e);
            let s = &data[i..end];
            if s.iter().filter(|&&c| c == b'.').count() == 3 && s.split(|&c| c == b'.').nth(2).is_some_and(|b| b.len() == 4) {
                *counts.entry(s).or_default() += 1;
            }
        }
    }
    counts.into_iter().max_by_key(|&(_, n)| n).map(|(s, _)| String::from_utf8_lossy(s).into_owned())
}

/// Libraries a program opens a window or draws with. One that loads none at start and names
/// none for loading later, but uses ncurses or sets the terminal up itself, is a text game.
const WINDOW_LIBS: &[&str] = &[
    "libX11", "libxcb", "libGL", "libEGL", "libOpenGL", "libvulkan", "libwayland", "libSDL", "libglfw", "libgtk", "libgdk", "libQt", "libsfml-window",
    "libsfml-graphics", "liballegro", "libraylib", "libglut", "libnw", "libcef", "liblove", "libIrrlicht", "libOgre",
];

/// Libraries a LuaJIT game's Lua code loads (`ffi.load "name"`), as file names the way LuaJIT
/// makes them ("GLee" → libGLee.so), with the file whose code asks: loose .lua files and those
/// inside zips near the top (Phone Fighter keeps its code in rom.zip beside bin/luajit's folder). Only the last
/// quoted name in a call counts: `ffi.load((pandora and "SDL-1.2.so.0") or "SDL")`. Names with
/// a folder are the developer's own paths.
fn lua_ffi_libs(root: &Path) -> Vec<(String, PathBuf)> {
    let mut sources: Vec<(PathBuf, String)> = Vec::new();
    let mut todo = vec![(root.to_path_buf(), 0)];
    while let Some((d, depth)) = todo.pop() {
        for entry in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let path = entry.path();
            let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
            if path.is_dir() && depth < 3 {
                todo.push((path, depth + 1));
            } else if ext == "lua" {
                sources.push((path.clone(), scan::lua_without_comments(&std::fs::read_to_string(&path).unwrap_or_default())));
            } else if ext == "zip" && depth <= 1 {
                let out = std::process::Command::new("unzip").arg("-p").arg(&path).arg("*.lua").stderr(std::process::Stdio::null()).output();
                sources.push((path.clone(), scan::lua_without_comments(&out.map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default())));
            }
        }
    }
    let mut out: Vec<(String, PathBuf)> = Vec::new();
    for (path, code) in sources {
        for (at, _) in code.match_indices("ffi.load") {
            let rest = &code[at + 8..];
            // The call's text: a bare string (`ffi.load "x"`) or up to the closing parenthesis.
            let call = if rest.trim_start().starts_with('(') {
                let mut level = 0;
                let end = rest.char_indices().find(|&(_, c)| {
                    level += (c == '(') as i32 - (c == ')') as i32;
                    c == ')' && level == 0
                });
                &rest[..end.map_or(rest.len().min(400), |(i, _)| i)]
            } else {
                rest.lines().next().unwrap_or("")
            };
            let strings: Vec<&str> = call.split(['"', '\'']).skip(1).step_by(2).collect();
            let Some(name) = strings.last().filter(|n| !n.is_empty() && !n.contains('/') && !n.contains(char::is_whitespace)) else { continue };
            let mut lib = if name.contains('.') { name.to_string() } else { format!("{name}.so") };
            if !lib.starts_with("lib") {
                lib = format!("lib{lib}");
            }
            let rel = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
            if !out.iter().any(|(l, _)| l == &lib) {
                out.push((lib, rel));
            }
        }
    }
    out
}

/// Names Unity players look for in /usr/share/fonts for their built-in font: Arial, then
/// (4.6.8 and 5.0.1 on) Liberation Sans, later DejaVu, Droid and Noto CJK. Players before
/// those have only Arial and FreeSans, neither of which Fedora (Bazzite) installs.
const UNITY_FONTS: &[&str] = &["Arial", "Liberation Sans", "DejaVu Sans", "FreeSans", "Droid Sans", "Droid Sans Fallback", "Noto Sans CJK JP", "Noto Sans CJK SC"];

/// The names a Unity player tries for its built-in font, when it looks in /usr/share/fonts
/// and finds none of them there (its own list, as text in the player).
fn unity_missing_font(player: &Path) -> Option<Vec<&'static str>> {
    let data = std::fs::read(player).ok()?;
    let needles: Vec<(usize, &[u8])> = UNITY_FONTS.iter().map(|n| n.as_bytes()).chain([&b"/usr/share/fonts"[..]]).enumerate().collect();
    let found = binary::find_any(&data, &needles);
    if !found.contains(&UNITY_FONTS.len()) {
        return None;
    }
    let names: Vec<&str> = UNITY_FONTS.iter().enumerate().filter(|(i, _)| found.contains(i)).map(|(_, n)| *n).collect();
    let installed = system_fonts()?;
    (!names.iter().any(|n| installed.contains(*n))).then_some(names)
}

/// Family names of the fonts in /usr/share/fonts, from fontconfig (None without fc-list).
fn system_fonts() -> Option<&'static BTreeSet<String>> {
    static FONTS: std::sync::OnceLock<Option<BTreeSet<String>>> = std::sync::OnceLock::new();
    FONTS
        .get_or_init(|| {
            let out = std::process::Command::new("fc-list").args(["--format", "%{file}\t%{family}\n"]).output().ok().filter(|o| o.status.success())?;
            Some(
                String::from_utf8_lossy(&out.stdout)
                    .lines()
                    .filter_map(|l| l.split_once('\t'))
                    .filter(|(file, _)| file.starts_with("/usr/share/fonts/"))
                    .flat_map(|(_, families)| families.split(',').map(str::to_string).collect::<Vec<_>>())
                    .collect(),
            )
        })
        .as_ref()
}

/// Native libraries an FNA game maps to in FNA.dll.config (os="linux") that aren't in its lib64/
/// (64-bit) or lib/ folder.
fn fna_libs(dir: &Path, bits: u8) -> Vec<String> {
    let Ok(config) = std::fs::read_to_string(dir.join("FNA.dll.config")) else { return Vec::new() };
    let libdir = dir.join(if bits == 64 { "lib64" } else { "lib" });
    let mut out: Vec<String> = config
        .lines()
        .filter(|l| l.contains("os=\"linux\""))
        .filter_map(|l| l.split("target=\"").nth(1)?.split('"').next().map(String::from))
        .filter(|t| !libdir.join(t).exists())
        .collect();
    out.sort();
    out.dedup();
    out
}

/// The .NET Core version a self-contained .NET program next to `dir` ships, if older than 5 and
/// not already set to work without ICU.
/// Native libraries a Mono game's .NET parts load by name, as their `.dll.config` files map
/// them for Linux (`os="linux"` or `"!windows,..."`, 64-bit or any processor; versioned names),
/// that are neither beside it nor on the system. Mono loads each when first called, so a
/// missing one stops the game there: Glanduff's Tao.Sdl maps SDL.dll to libSDL-1.2.so.0,
/// called at start for the joystick. Some are never called (Tao.Sdl also maps smpeg and
/// SDL_gfx). Plus libgdiplus.so.0 when one of its files names System.Drawing (OpenTK sets the
/// window's icon with it: "DllNotFoundException: libgdiplus.so.0") and neither the system nor
/// Mono's own lib folder has it (Homebrew's mono doesn't).
fn mono_libs(game: &Game, exe: &scan::Found, mono: Option<&Path>, system: &System) -> Vec<String> {
    let dir = game.root.join(exe.path.parent().unwrap_or(Path::new("")));
    let files: Vec<PathBuf> = std::fs::read_dir(&dir).into_iter().flatten().flatten().map(|e| e.path()).collect();
    let attr = |line: &str, name: &str| line.split(&format!("{name}=\"")).nth(1).and_then(|v| v.split('"').next()).map(String::from);
    let mut out: Vec<String> = files
        .iter()
        .filter(|p| p.to_string_lossy().ends_with(".dll.config"))
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .flat_map(|text| text.lines().filter(|l| l.contains("<dllmap")).map(String::from).collect::<Vec<_>>())
        .filter(|l| attr(l, "os").is_none_or(|os| os == "linux" || (os.starts_with('!') && !os.contains("linux"))))
        .filter(|l| attr(l, "cpu").is_none_or(|c| c == "x86-64"))
        .filter_map(|l| attr(&l, "target"))
        .filter(|t| t.starts_with("lib") && t.contains(".so.") && !dir.join(t).exists() && !system.has(t, 64))
        .collect();
    let drawing = files.iter().filter(|p| p.extension().is_some_and(|x| x == "dll" || x == "exe")).any(|p| binary::contains(p, b"System.Drawing"));
    let in_mono = mono.and_then(|m| std::fs::canonicalize(m).ok()).and_then(|m| m.parent()?.parent().map(|p| p.join("lib/libgdiplus.so.0"))).is_some_and(|l| l.exists());
    if drawing && !in_mono && !system.has("libgdiplus.so.0", 64) {
        out.push("libgdiplus.so.0".into());
    }
    out.sort();
    out.dedup();
    out
}

/// Whether a Java game plays sound with Java's own (javax.sound: Processing's Minim, JSyn)
/// rather than OpenAL: LWJGL and libGDX carry OpenAL's native library in their jars
/// (libopenal64.so, OpenAL64.dll), and the jar's file names are stored as they are.
pub fn uses_java_sound(game: &Game) -> bool {
    let jars = || std::iter::once(game.root.clone()).chain(game.files.iter().filter_map(|f| f.path.parent().map(|d| game.root.join(d)))).collect::<BTreeSet<_>>();
    let openal = jars().iter().flat_map(|d| std::fs::read_dir(d).into_iter().flatten().flatten()).map(|e| e.path()).chain(game.files.iter().map(|f| game.full(f))).any(|p| {
        let name = p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
        name.starts_with("libopenal") || (name.ends_with(".jar") && binary::contains(&p, b"libopenal"))
    });
    !openal
}

/// The PulseAudio socket (PipeWire's pulse server), as PULSE_SERVER takes it.
fn pulse_socket() -> String {
    let runtime = std::env::var("XDG_RUNTIME_DIR").ok().filter(|d| !d.is_empty()).unwrap_or_else(|| {
        let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
        let uid = status.lines().find_map(|l| l.strip_prefix("Uid:")).and_then(|v| v.split_whitespace().next()).unwrap_or("1000").to_string();
        format!("/run/user/{uid}")
    });
    format!("unix:{runtime}/pulse/native")
}

/// A small text file in the program's folder that lists folders under one key, some there and
/// some not (labb's config.labb: `data editor_data`, `data data-hd`, `data data`; the download
/// has no editor_data, and the game stops when it lists it). `key value` or `key = value`
/// lines; a value counts as a folder name when it has no dot or slash. At least two of the
/// key's values must be folders that exist.
fn missing_folders(root: &Path, dir: &Path) -> Option<(PathBuf, Vec<PathBuf>)> {
    let here = root.join(dir);
    for entry in std::fs::read_dir(&here).ok()?.flatten() {
        let path = entry.path();
        if !path.is_file() || entry.metadata().map_or(true, |m| m.len() > 64 << 10) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        let mut keys: std::collections::BTreeMap<&str, Vec<&str>> = Default::default();
        for line in text.lines().map(str::trim).filter(|l| !l.starts_with('#') && !l.starts_with(';')) {
            let parts: Vec<&str> = line.split(|c: char| c.is_whitespace() || c == '=').filter(|p| !p.is_empty()).collect();
            if let [key, value] = parts.as_slice() {
                keys.entry(key).or_default().push(value);
            }
        }
        for values in keys.values() {
            let named = |v: &&str| !v.contains(['.', '/', '\\']) && v.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-');
            let there = values.iter().filter(|v| named(v) && here.join(v).is_dir()).count();
            let missing: Vec<PathBuf> = values.iter().filter(|v| named(v) && !here.join(v).exists()).map(|v| dir.join(v)).collect();
            if there >= 2 && !missing.is_empty() {
                return Some((path.strip_prefix(root).ok()?.to_path_buf(), missing));
            }
        }
    }
    None
}

/// LÖVE 0.x, which today's LÖVE 11 can't run.
pub fn old_love(version: Option<&str>) -> bool {
    version.is_some_and(|v| v.starts_with("0."))
}

fn old_dotnet(dir: &Path, main: &Path) -> Option<String> {
    let stem = main.file_name()?.to_string_lossy().into_owned();
    if dir.join("libhostfxr.so").is_file() {
        let config = std::fs::read_to_string(dir.join(format!("{stem}.runtimeconfig.json"))).unwrap_or_default();
        if config.contains("System.Globalization.Invariant\":true") || config.contains("System.Globalization.Invariant\": true") {
            return None;
        }
    } else if !binary::head_contains(&dir.join(&stem), b"libhostfxr.so", 2 << 20) || binary::contains_groups(&dir.join(&stem), &[&[b"System.Globalization.Invariant\": true", b"System.Globalization.Invariant\":true"]]) {
        return None;
    }
    let version = dotnet_core_version(dir, main)?;
    let major: u32 = version.split('.').next()?.parse().ok()?;
    (major < 5).then_some(version)
}

/// Static libraries (.a) a self-contained .NET program's deps.json lists that aren't beside it.
fn dotnet_missing_static(dir: &Path, main: &Path) -> Vec<String> {
    let Some(stem) = main.file_name().map(|n| n.to_string_lossy().into_owned()) else { return Vec::new() };
    if !dir.join("libhostfxr.so").is_file() {
        return Vec::new();
    }
    let deps = std::fs::read_to_string(dir.join(format!("{stem}.deps.json"))).unwrap_or_default();
    let mut missing: Vec<String> = deps
        .split('"')
        .filter(|s| s.ends_with(".a") && !s.contains(' '))
        .filter_map(|s| s.rsplit('/').next())
        .filter(|name| !dir.join(name).exists())
        .map(String::from)
        .collect();
    missing.sort();
    missing.dedup();
    missing
}

/// A self-contained .NET Core program's framework version, from its deps.json:
/// "runtimeTarget": {"name": ".NETCoreApp,Version=v2.1/linux-x64", ...}. A single-file
/// bundle (no libhostfxr.so beside it) carries deps.json inside the program (Asciipocalypse,
/// .NET Core 3.1: ICU abort like The Heart's Illumination).
fn dotnet_core_version(dir: &Path, main: &Path) -> Option<String> {
    let stem = main.file_name()?.to_string_lossy().into_owned();
    let deps = if dir.join("libhostfxr.so").is_file() {
        std::fs::read_to_string(dir.join(format!("{stem}.deps.json"))).ok()?
    } else {
        let program = dir.join(&stem);
        if !binary::head_contains(&program, b"libhostfxr.so", 2 << 20) || !binary::contains(&program, format!("{stem}.deps.json").as_bytes()) {
            return None;
        }
        format!(".NETCoreApp,Version=v{}", String::from_utf8_lossy(&binary::find_after(&program, b".NETCoreApp,Version=v", 8)?))
    };
    let at = deps.find(".NETCoreApp,Version=v")? + ".NETCoreApp,Version=v".len();
    Some(deps[at..].chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect())
}

/// The .NET version a framework-dependent program asks for: libhostfxr.so isn't beside it (a
/// self-contained one brings it; a single-file one has its own runtimeconfig inside).
fn dotnet_framework(dir: &Path, main: &Path) -> Option<String> {
    let stem = main.file_name()?.to_string_lossy().into_owned();
    if dir.join("libhostfxr.so").is_file() {
        return None;
    }
    crate::dotnet::framework(&dir.join(format!("{stem}.runtimeconfig.json")))
}

/// Its major version.
fn dotnet_core(dir: &Path, main: &Path) -> Option<u32> {
    dotnet_core_version(dir, main)?.split('.').next()?.parse().ok()
}

/// Shader programs from which a Unity 4/5.0 game's first start is noticeably slow: the VM's
/// Unity 4 games have up to 1,501 (For Science), The Asylum 4,928 (black for 1½-3 minutes).
const UNITY_MANY_SHADERS: usize = 1000;

/// GLSL programs in a Unity game's data files: Unity 4 keeps each as text with both halves in
/// it, `#ifdef VERTEX` starting the first. Textures and sounds (.resS, .resource) are skipped.
fn unity_shaders(root: &Path, main: &Path) -> usize {
    let stem = main.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let data = root.join(main.parent().unwrap_or(Path::new(""))).join(format!("{stem}_Data"));
    std::fs::read_dir(data)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter(|e| !e.path().extension().is_some_and(|x| x == "resS" || x == "resource"))
        .map(|e| binary::count(&e.path(), b"#ifdef VERTEX"))
        .sum()
}

/// The start of an MP4 file: its "ftyp" box with a common brand.
const MP4_BRANDS: &[&[u8]] = &[b"ftypisom", b"ftypmp42", b"ftypmp41", b"ftypiso2", b"ftypavc1", b"ftypM4V ", b"ftypqt  "];

/// Videos Unity's Linux player can't play: H.264 files in StreamingAssets, and H.264 clips
/// the build kept untranscoded in its .resource files (an MP4 header and H.264's "avcC" box).
fn h264_videos(root: &Path, main: &Path) -> Vec<PathBuf> {
    let stem = main.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let data = main.parent().unwrap_or(Path::new("")).join(format!("{stem}_Data"));
    let mut found = Vec::new();
    let mut dirs = vec![data.clone()];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(root.join(&dir)).into_iter().flatten().flatten() {
            let path = dir.join(entry.file_name());
            let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                if path.starts_with(data.join("StreamingAssets")) || dir == data {
                    dirs.push(path);
                }
            } else if (["mp4", "m4v", "mov"].contains(&ext.as_str()) && path.starts_with(data.join("StreamingAssets")))
                || (ext == "resource" && binary::contains_groups(&root.join(&path), &[MP4_BRANDS, &[b"avcC"]]))
            {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

/// Java runtimes bundled with games are often trimmed of AWT, the windowing toolkit games drawing
/// with OpenGL (LWJGL, libGDX) don't use; the AWT libraries left behind then miss libawt.so.
fn java_window_part(path: &Path, lib: &str) -> Option<&'static str> {
    let in_jre = path.components().any(|c| c.as_os_str() == "jre");
    (in_jre && lib.starts_with("libawt")).then_some("the bundled Java's windowing toolkit (AWT, unused by games that draw with OpenGL)")
}

/// A DT_RPATH or DT_RUNPATH list as folders, with $ORIGIN standing for the file's own folder.
/// Relative entries depend on where the game is started from: the program's folder (`workdir`)
/// for gamefix's menu entries and Steam shortcuts, and for a double-click in a file manager
/// (Shawy Adventures, Godot: RUNPATH `.:$ORIGIN/linux64`, libsteam_api.so next to it).
fn expand(dirs: &[String], origin: &Path, workdir: &Path) -> Vec<PathBuf> {
    let origin = origin.to_string_lossy();
    dirs.iter().map(|d| PathBuf::from(d.replace("${ORIGIN}", &origin).replace("$ORIGIN", &origin))).map(|d| if d.is_absolute() { d } else { workdir.join(d) }).collect()
}

/// glibc resolves a library's calls through the global scope first (the program and everything
/// loaded with it at start), and only then through the library's own dependencies. So when a
/// plugin loaded later expects functions from a bundled library, but a library loaded at start
/// defines functions with the same names, the plugin silently gets those instead.
fn symbol_clashes(game: &Game, main: &Path, same: &[(&PathBuf, &Elf)]) -> Vec<Clash> {
    let mut by_name: HashMap<&OsStr, (&PathBuf, &Elf)> = HashMap::new();
    for &(p, e) in same {
        if let Some(n) = p.file_name() {
            by_name.entry(n).or_insert((p, e));
        }
    }
    let Some(&(_, main_elf)) = same.iter().find(|(p, _)| p.as_path() == main) else { return Vec::new() };

    // The libraries loaded at start, in load order (breadth first, like ld.so).
    let mut startup: Vec<&PathBuf> = Vec::new();
    let mut queue: VecDeque<&Elf> = VecDeque::from([main_elf]);
    while let Some(e) = queue.pop_front() {
        for n in &e.needed {
            if let Some(&(p, dep)) = by_name.get(OsStr::new(n)) {
                if !startup.contains(&p) {
                    startup.push(p);
                    queue.push_back(dep);
                }
            }
        }
    }

    let mut cache: HashMap<&PathBuf, Option<Symbols>> = HashMap::new();
    let mut clashes = Vec::new();
    for &(victim, e) in same {
        if e.program || startup.contains(&victim) || optional_plugin(victim).is_some() {
            continue;
        }
        for n in &e.needed {
            let Some(&(provider, _)) = by_name.get(OsStr::new(n)) else { continue };
            if startup.contains(&provider) {
                continue;
            }
            for p in [victim, provider].into_iter().chain(startup.iter().copied()) {
                cache.entry(p).or_insert_with(|| binary::symbols(&game.root.join(p)));
            }
            let (Some(Some(v)), Some(Some(pr))) = (cache.get(victim), cache.get(provider)) else { continue };
            let mut per_shadow: BTreeMap<&PathBuf, Vec<&String>> = BTreeMap::new();
            for sym in v.undefined.iter().filter(|s| pr.defined.contains(*s)) {
                if let Some(shadow) = startup.iter().find(|g| cache.get(*g).and_then(|s| s.as_ref()).is_some_and(|s| s.defined.contains(sym))) {
                    per_shadow.entry(shadow).or_default().push(sym);
                }
            }
            for (shadow, mut syms) in per_shadow {
                // The same library under another name gives the same functions (a PyInstaller
                // bundle's renamed libz-a147dcb0.so.1.2.3 and the libz.so.1 it also packs).
                if same_library(provider, shadow) {
                    continue;
                }
                syms.sort();
                clashes.push(Clash { victim: victim.clone(), provider: provider.clone(), shadow: shadow.clone(), count: syms.len(), example: readable(syms[0]) });
            }
        }
    }
    clashes
}

/// "_ZN4FMOD6System10getVersionEPj" → "FMOD::System::getVersion"; C names stay as they are.
fn readable(sym: &str) -> String {
    let Some(mut rest) = sym.strip_prefix("_ZN") else { return sym.to_string() };
    let mut parts = Vec::new();
    while let Some(len_end) = rest.find(|c: char| !c.is_ascii_digit()).filter(|&i| i > 0) {
        let Ok(len) = rest[..len_end].parse::<usize>() else { break };
        let Some(part) = rest.get(len_end..len_end + len) else { break };
        parts.push(part);
        rest = &rest[len_end + len..];
    }
    if parts.is_empty() { sym.to_string() } else { parts.join("::") }
}

fn list(out: &mut String, paths: &[PathBuf]) {
    for p in paths {
        let _ = writeln!(out, "      {}", p.display());
    }
}

impl Report {
    pub fn render(&self, game: &Game) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "{}  ({})", game.name, game.root.display());
        if let Some((main, elf)) = game.main() {
            let engine = game
                .engine
                .as_ref()
                .map(|e| format!(", {}{}", e.name, e.version.as_ref().map(|v| format!(" {v}")).unwrap_or_default()))
                .unwrap_or_default();
            let _ = writeln!(out, "  Runs:  {} (Linux, {}-bit{engine})", main.path.display(), elf.bits);
            if let Some(l) = game.launcher() {
                let renpy = l.path.file_stem().is_some_and(|s| game.full(l).with_file_name(format!("{}.py", s.to_string_lossy())).is_file());
                let why = if renpy {
                    "it tells the program which game file to run"
                } else if binary::sets_library_path(&game.full(l)) {
                    "it sets where the game's libraries are"
                } else if l.path.parent() == main.path.parent() {
                    "it tells the program which game file to run"
                } else {
                    "it starts the program from the game's folder, with its settings"
                };
                let _ = writeln!(out, "  Start: {} (the developer's launcher: {why})", l.path.display());
            }
        } else if let Some(Binary::Pe(pe)) = game.windows.map(|i| &game.files[i].binary) {
            let engine = game.engine.as_ref().map(|e| format!(", {}{}", e.name, e.version.as_ref().map(|v| format!(" {v}")).unwrap_or_default())).unwrap_or_default();
            let _ = writeln!(out, "  Runs:  {} (Windows, {}-bit{engine})", game.files[game.windows.unwrap()].path.display(), pe.bits);
        }
        if let Some(f) = game.portable.filter(|_| game.main().is_none()).map(|i| &game.files[i]) {
            let what = match (&f.binary, &game.engine) {
                (Binary::Love, Some(e)) => format!("LÖVE game{}", e.version.as_ref().map(|v| format!(", made for LÖVE {v}")).unwrap_or_default()),
                (Binary::Python(p), _) if p.v2 => "Python 2 game".into(),
                (Binary::Jar, _) => "Java game".into(),
                (Binary::Flash, _) => "Flash game".into(),
                (Binary::Tic, _) => "TIC-80 cartridge".into(),
                (Binary::Rom(console), _) => format!("{console} cartridge"),
                (Binary::Dos, _) => "DOS game".into(),
                (Binary::Pe(_), _) if f.path.extension().is_some_and(|e| e.eq_ignore_ascii_case("dll")) => ".NET game for any system".into(),
                (Binary::Pe(_), _) => ".NET game for Mono on Linux".into(),
                (Binary::RpgMaker, _) => "RPG Maker 2000/2003 game".into(),
                (Binary::Script, _) if scan::install4j_script(&game.root, &f.path) => "Java game, install4j launcher".into(),
                (Binary::Script, _) => "Processing game, runs with Java".into(),
                _ => "Python game".into(),
            };
            let _ = writeln!(out, "  Runs:  {} ({what})", f.path.display());
        }
        if !self.windows.is_empty() && game.windows.is_none() && game.cd_installer.is_none() {
            let _ = writeln!(out, "  {}: {}", if game.main().is_some() { "Also has a Windows build" } else { "Windows build" }, self.windows.iter().map(|(p, bits)| format!("{} ({bits}-bit)", p.display())).collect::<Vec<_>>().join(", "));
        }
        for (p, kind) in &self.installers {
            let _ = writeln!(out, "  Installer ({kind}): {}", p.display());
        }
        let _ = writeln!(out);
        for (what, libs) in &self.harmless {
            let _ = writeln!(out, "• Harmless: {} missing, but only {what} needs it; the game starts without it.", libs.join(", "));
        }
        if !self.harmless.is_empty() {
            let _ = writeln!(out);
        }

        if self.pyinstaller {
            let _ = writeln!(out, "• It's a PyInstaller bundle (Python, with its libraries packed inside), so those libraries");
            let _ = writeln!(out, "  can't be checked. gamefix setup lets ones asking for an executable stack load, in case");
            let _ = writeln!(out, "  (old pygame's SDL 1.2 does: Sascha wouldn't start without it).");
            let _ = writeln!(out);
        }
        if self.packed {
            let _ = writeln!(out, "• Its program is packed (UPX), so the libraries it needs can't be read until it runs.");
            if self.old_runtime.is_some() {
                let _ = writeln!(out, "  Steam's runtime for older games has most libraries old games miss; gamefix setup");
                let _ = writeln!(out, "  starts it through that in case.");
            }
            let _ = writeln!(out);
        }
        if let Some((text, _)) = &self.appimage {
            let _ = writeln!(out, "Inside the AppImage (unpacked to look, not run):");
            for line in text.lines() {
                let _ = writeln!(out, "{}{line}", if line.is_empty() { "" } else { "  " });
            }
            let _ = writeln!(out);
        }
        // The AppImage file itself has nothing to say beyond what's inside.
        if self.problems.is_empty() && self.appimage.is_some() {
            return out;
        }
        if self.problems.is_empty() {
            let _ = writeln!(out, "No {}problems found in its files ({} libraries checked).", if self.packed || self.pyinstaller { "other " } else { "" }, self.libs_checked);
            let _ = writeln!(out, "If it still doesn't start, the next step is running it and reading what it prints.");
            return out;
        }
        for problem in &self.problems {
            match problem {
                Problem::Archive(path) if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("zip")) && !binary::zip_complete(&game.root.join(path)) => {
                    let _ = writeln!(out, "✗ There's no program to run here, and the archive the game is in is incomplete:");
                    let _ = writeln!(out, "      {}", path.display());
                    let _ = writeln!(out, "  It has no table of contents at its end, so it can't be unpacked: the download was cut off,");
                    let _ = writeln!(out, "  or the file on the game's page is damaged.");
                    let _ = writeln!(out, "    Fix: download it again; if it's the same, only the developer can fix it.");
                }
                Problem::Archive(path) => {
                    let _ = writeln!(out, "✗ There's no program to run here, but there's an archive the game may still be packed in:");
                    let _ = writeln!(out, "      {}", path.display());
                    let _ = writeln!(out, "    Fix: unpack it (right-click → Extract), then check the folder it makes.");
                }
                Problem::NwjsFonts(version) => {
                    let _ = writeln!(out, "✗ Its NW.js (Chromium {version}) may quit at start with \"Could not find the default font\":");
                    let _ = writeln!(out, "  that Chromium can't handle this system's font settings.");
                    let _ = writeln!(out, "    Fix (at launch): FONTCONFIG_FILE pointing to a plain font list (gamefix setup writes one).");
                }
                Problem::FnaLibs(libs) => {
                    let _ = writeln!(out, "✗ The game's own native libraries are missing (its download left them out): {}.", libs.join(", "));
                    let _ = writeln!(out, "  It quits at start without them.");
                    let _ = writeln!(out, "    Fix: FNA's \"fnalibs\" archive (fna.flibitijibibo.com) has them; put its lib64 files in the");
                    let _ = writeln!(out, "    game's lib64 folder. Or use the Windows download with Proton, if there is one.");
                }
                Problem::Source(file, kind) => {
                    let _ = writeln!(out, "✗ This is the game's source code ({kind}: {}), not a built game.", file.display());
                    let _ = writeln!(out, "    It has to be built first (its README usually says how), or use a download with a");
                    let _ = writeln!(out, "    ready-made program if there is one.");
                }
                Problem::MissingWindowsParts(parts) => {
                    use crate::proton::Provider;
                    let names = |want: fn(&Provider) -> bool| parts.iter().filter(|(_, p)| want(p)).map(|(d, _)| d.as_str()).collect::<Vec<_>>().join(", ");
                    let components: Vec<&str> = parts.iter().filter_map(|(_, p)| if let Provider::Component(c) = p { Some(*c) } else { None }).collect();
                    if !components.is_empty() {
                        let _ = writeln!(out, "✗ It needs parts of Windows that neither the game brings nor Proton has: {}.", names(|p| matches!(p, Provider::Component(_))));
                        let _ = writeln!(out, "    Fix: install {} into its Wine prefix (winetricks); gamefix setup does that (for a", components.join(" "));
                        let _ = writeln!(out, "    game in Steam, with --steam, once it has been started from Steam once).");
                    }
                    let debug = names(|p| *p == Provider::DebugBuild);
                    if !debug.is_empty() {
                        let _ = writeln!(out, "✗ It needs {debug}: Microsoft's debug C++ runtime, which only developers' machines have.");
                        let _ = writeln!(out, "  The developer shipped a debug build; only they can fix that (a release build).");
                    }
                    let unknown = names(|p| *p == Provider::Unknown);
                    if !unknown.is_empty() {
                        let _ = writeln!(out, "✗ It needs {unknown}, which neither the game brings nor Proton has, and no known");
                        let _ = writeln!(out, "  package provides: the download probably left it out.");
                    }
                }
                Problem::OwnWindowsDlls(dlls) => {
                    let files: Vec<String> = dlls.iter().map(|d| format!("{d}.dll")).collect();
                    let _ = writeln!(out, "✗ It brings its own {}, replacing a part of Windows. Windows would use the", files.join(", "));
                    let _ = writeln!(out, "  game's copy, but Wine uses its own unless told otherwise, so the game may not look or");
                    let _ = writeln!(out, "  sound as it should (Empire Earth flickers without its own ddraw.dll).");
                    let _ = writeln!(out, "    Fix (at launch): WINEDLLOVERRIDES={}; gamefix setup adds it.", crate::proton::overrides(dlls));
                }
                Problem::Web(file) => {
                    let _ = writeln!(out, "✗ This is a web (HTML5) game ({}): it runs in a browser, not as a program.", file.display());
                    let _ = writeln!(out, "  Opened straight from the disk it often stays blank: browsers stop a page opened from a");
                    let _ = writeln!(out, "  file from loading the rest of the game.");
                    let _ = writeln!(out, "    Fix: play it on its itch.io page, where it runs in the browser as made.");
                }
                Problem::UnityWeb(file) => {
                    let _ = writeln!(out, "✗ This is a Unity Web Player game ({}): it ran inside a browser plugin that Unity", file.display());
                    let _ = writeln!(out, "  and the browsers dropped in 2015-2017, and nothing else plays it.");
                    let _ = writeln!(out, "    Fix: none here; the game's page may have a download for Windows or Linux, or a WebGL");
                    let _ = writeln!(out, "    version that plays in the browser.");
                }
                Problem::NoLinuxBuild => {
                    let _ = writeln!(out, "✗ There is no Linux version of this game.");
                    let anticheat = crate::proton::anticheat(&game.root);
                    let steam_app = crate::steam::installed_app(&game.root);
                    let hint = steam_app.and_then(crate::proton::hint);
                    if let Some((what, _)) = anticheat.iter().find(|(_, works)| !works) {
                        let _ = writeln!(out, "✗ It uses {what}, an anti-cheat that doesn't run under Proton. Nothing on this");
                        let _ = writeln!(out, "  side gets past that.");
                    } else if let Some(hint) = hint.as_ref().filter(|h| h.advice == crate::proton::Advice::AntiCheat) {
                        let _ = writeln!(out, "✗ Its anti-cheat stops it under Proton: {}.", hint.reason());
                        let _ = writeln!(out, "  Nothing on this side gets past that.");
                    } else if let Some(i) = game.windows {
                        let options = steam_app.and_then(|id| crate::proton::options(id, &crate::proton::gpu_makers()));
                        match (steam_app, &hint, &options) {
                            (Some(_), None, None) => {
                                let _ = writeln!(out, "    Steam runs it with Proton. ProtonDB's reports don't name a Proton or launch options");
                                let _ = writeln!(out, "    that run it better than Steam's own setup.");
                            }
                            (Some(_), _, _) => {
                                if let Some(hint) = &hint {
                                    let _ = writeln!(out, "    On ProtonDB it worked with {} for {} players, where Steam's default Proton", hint.tool(), hint.reports);
                                    let _ = writeln!(out, "    failed for {}.", hint.of);
                                    let _ = writeln!(out, "    Fix: gamefix setup --steam sets it to {}.", hint.tool());
                                }
                                if let Some(o) = &options {
                                    let _ = writeln!(out, "    On ProtonDB these launch options fixed it for {} players, where Steam's default", o.reports);
                                    let _ = writeln!(out, "    Proton failed for {}: {}", o.of, o.text());
                                    let _ = writeln!(out, "    Fix: gamefix setup --steam adds them to its launch options.");
                                }
                            }
                            _ => {
                                let _ = writeln!(out, "    Fix: run {} with Proton: gamefix setup adds it to the app menu that way (--steam: to Steam).", game.files[i].path.display());
                                let gog = crate::gog::read(&game.root).filter(|g| g.program == game.full(&game.files[i]));
                                if let Some(helper) = gog.as_ref().and_then(|g| g.helper.as_ref()) {
                                    let _ = writeln!(out, "• GOG Galaxy runs a first-run helper once after installing it, which sets up what the");
                                    let _ = writeln!(out, "  game expects from Windows (registry keys, system DLLs): {}.", helper.file_name().unwrap_or_default().to_string_lossy());
                                    let _ = writeln!(out, "    gamefix setup runs it once, into a Wine prefix of the game's own.");
                                }
                                if let Some(umu) = crate::umu::find(&game.root, &game.full(&game.files[i])) {
                                    let _ = writeln!(out, "• protonfixes (part of GE-Proton and UMU-Proton) has a fix for this game: {}, {}.", umu.title, umu.id);
                                    let _ = writeln!(out, "    gamefix setup runs it with one of those and tells protonfixes which game it is.");
                                }
                                if let Some((target, _)) = crate::binary::unreal_bootstrap(&game.full(&game.files[i])) {
                                    let _ = writeln!(out, "• {} is Unreal's launcher: it checks for Microsoft's VC++ runtime, then starts", game.files[i].path.display());
                                    let _ = writeln!(out, "  the game's program, {}.", target.strip_prefix(&game.root).unwrap_or(&target).display());
                                    let _ = writeln!(out, "  Newer launchers refuse the runtime Proton brings (\"Microsoft Visual C++ 2015-2022");
                                    let _ = writeln!(out, "  Redistributable (x64)\" is required).");
                                    let _ = writeln!(out, "    gamefix setup starts the game's program directly; it runs on Proton's own runtime.");
                                }
                            }
                        }
                        for (what, _) in &anticheat {
                            let _ = writeln!(out, "• It uses {what}: that runs under Proton only if the game's developer turned it on.");
                            let _ = writeln!(out, "  If it won't start or can't go online, nothing on this side can change that.");
                        }
                    } else if !self.installers.is_empty() {
                        let _ = writeln!(out, "    It's an installer: gamefix setup runs it with Proton (you click through it");
                        let _ = writeln!(out, "    once) and adds the game it installs to the app menu (--steam: to Steam).");
                    }
                }
                Problem::Flash { file, ruffle: Some(ruffle) } => {
                    let _ = writeln!(out, "• This is a Flash game. Flash Player is gone, but Ruffle (installed) plays most Flash games:");
                    let _ = writeln!(out, "  {} \"{}\"", ruffle.join(" "), file.display());
                    let _ = writeln!(out, "  gamefix setup adds that to the app menu.");
                }
                Problem::Flash { file, ruffle: None } => {
                    let _ = writeln!(out, "✗ This is a Flash game ({}): Flash Player was retired in 2020.", file.display());
                    let _ = writeln!(out, "    Fix: Ruffle from Flathub (rs.ruffle.Ruffle) plays most Flash games: gamefix setup offers to");
                    let _ = writeln!(out, "    install it, then adds the game to the app menu or Steam.");
                }
                Problem::Tic { file, tic80: Some(tic80) } => {
                    let _ = writeln!(out, "• This is a TIC-80 game (a cartridge for that fantasy console); TIC-80 is installed:");
                    let _ = writeln!(out, "  {} \"{}\"", tic80.join(" "), file.display());
                    let _ = writeln!(out, "  gamefix setup adds that to the app menu.");
                }
                Problem::Tic { file, tic80: None } => {
                    let _ = writeln!(out, "✗ This is a TIC-80 game ({}): a cartridge for that fantasy console, not a program.", file.display());
                    let _ = writeln!(out, "    Fix: TIC-80 from Flathub (com.tic80.TIC_80) plays it: gamefix setup offers to install it,");
                    let _ = writeln!(out, "    then adds the game to the app menu or Steam.");
                }
                Problem::Mono { file, mono, libs, standard } => {
                    if !standard.is_empty() {
                        let _ = writeln!(out, "• Part of the game was built for .NET Standard, which the Mono it brings (MonoKickstart) can't");
                        let _ = writeln!(out, "  load, so its own program quits at start ({}); a current Mono has those parts.", standard.join(", "));
                    }
                    match mono {
                        Some(mono) => {
                            let _ = writeln!(out, "• This is a .NET game built to run with Mono on Linux; Mono is installed, so it runs with:");
                            let _ = writeln!(out, "  {} \"{}\"", mono.display(), file.display());
                            let _ = writeln!(out, "  gamefix setup adds that to the app menu.");
                        }
                        None => {
                            let _ = writeln!(out, "✗ This is a .NET game built to run with Mono on Linux ({}), and Mono isn't installed.", file.display());
                            let _ = writeln!(out, "    Fix: on Bazzite, run \"brew install mono\" in a terminal; then gamefix setup adds it to the app menu.");
                        }
                    }
                    if !libs.is_empty() {
                        let names: Vec<String> = libs.iter().map(|(l, runtime)| if *runtime { format!("{l} (Steam's runtime has it)") } else { l.clone() }).collect();
                        let _ = writeln!(out, "\n✗ Native libraries its .NET parts load by name aren't installed: {}.", names.join(", "));
                        let _ = writeln!(out, "  Mono loads each when it's first used, and the game quits there if it's missing (Glanduff:");
                        let _ = writeln!(out, "  libgdiplus.so.0, then libSDL-1.2.so.0); some may never be used.");
                        let _ = writeln!(out, "    Fix (at launch): gamefix setup starts it through Steam's runtime for the ones that has, and");
                        let _ = writeln!(out, "    fetches the others into ~/.local/share/gamefix/libs.");
                    }
                }
                Problem::Rom { file, console, mgba: Some(mgba) } => {
                    let _ = writeln!(out, "• This is a {console} game (a cartridge image); mGBA, which plays them, is installed:");
                    let _ = writeln!(out, "  {} \"{}\"", mgba.join(" "), file.display());
                    let _ = writeln!(out, "  gamefix setup adds that to the app menu.");
                }
                Problem::Rom { file, console, mgba: None } => {
                    let _ = writeln!(out, "✗ This is a {console} game ({}): a cartridge image for that console, not a program.", file.display());
                    let _ = writeln!(out, "    Fix: mGBA from Flathub (io.mgba.mGBA) plays it: gamefix setup offers to install it,");
                    let _ = writeln!(out, "    then adds the game to the app menu or Steam.");
                }
                Problem::CdFiles { installer } => {
                    let _ = writeln!(out, "• These are a Windows game CD's files; its installer is {}.", installer.display());
                    let _ = writeln!(out, "  gamefix setup installs it with Proton (you click through the installer once)");
                    let _ = writeln!(out, "  and adds the installed game to the app menu (--steam: to Steam).");
                }
                Problem::Protected { what, file, others } => {
                    let _ = writeln!(out, "✗ Its program ({}) is protected by {what}, whose driver Windows 10 and later", file.display());
                    let _ = writeln!(out, "  and Wine/Proton don't run: the game won't start, installed or not.");
                    if !others.is_empty() {
                        let list: Vec<String> = others.iter().map(|o| o.display().to_string()).collect();
                        let _ = writeln!(out, "  The download also has a copy of the program without it: {}.", list.join(", "));
                    }
                }
                Problem::Disc { file, installer: Some(installer), .. } => {
                    let _ = writeln!(out, "• This is a Windows game's disc image ({}); its installer is {installer}.", file.display());
                    let _ = writeln!(out, "  gamefix setup installs it with Proton (you click through the installer once)");
                    let _ = writeln!(out, "  and adds the installed game to the app menu (--steam: to Steam).");
                }
                Problem::Disc { file, files: None, .. } => {
                    let _ = writeln!(out, "✗ This is a disc image ({}), and 7-Zip (7z) is needed to look inside it.", file.display());
                }
                Problem::Disc { file, files: Some(0), .. } => {
                    let _ = writeln!(out, "✗ This disc image ({}) holds no Windows programs: it may be for another machine", file.display());
                    let _ = writeln!(out, "  (a console, or an old Japanese computer), which needs an emulator.");
                }
                Problem::Disc { file, .. } => {
                    let _ = writeln!(out, "✗ This disc image ({}) holds Windows programs, but none that looks like its installer.", file.display());
                }
                Problem::Dos { file, dosbox: Some(dosbox) } => {
                    let _ = writeln!(out, "• This is a DOS game. Proton can't run DOS programs, but DOSBox (installed) can:");
                    let _ = writeln!(out, "  {} \"{}\"", dosbox.join(" "), file.display());
                    let _ = writeln!(out, "  gamefix setup adds that to the app menu or Steam.");
                }
                Problem::Dos { file, dosbox: None } => {
                    let _ = writeln!(out, "✗ This is a DOS game ({}): Proton can't run DOS programs.", file.display());
                    let _ = writeln!(out, "    Fix: DOSBox Staging from Flathub (io.github.dosbox-staging) runs it: gamefix setup offers to");
                    let _ = writeln!(out, "    install it, then adds the game to the app menu or Steam.");
                }
                Problem::Package(file, "apk") => {
                    let _ = writeln!(out, "✗ This download is an Android app ({}), not a game for PCs.", file.display());
                    let _ = writeln!(out, "    Fix: get the Linux or Windows download from the game's page instead.");
                }
                Problem::Package(file, "deb") => {
                    let _ = writeln!(out, "✗ This download is a Debian/Ubuntu package ({}). Bazzite can't install those,", file.display());
                    let _ = writeln!(out, "  and the game inside expects to be installed into the system's folders.");
                    let _ = writeln!(out, "    Fix: look on the game's page for a .tar.gz, .zip, AppImage or Flatpak download instead.");
                }
                Problem::Package(file, "snap") => {
                    let _ = writeln!(out, "✗ This download is a Snap package ({}). Bazzite has no Snap support, and the", file.display());
                    let _ = writeln!(out, "  game inside expects Snap's own runtime around it.");
                    let _ = writeln!(out, "    Fix: look on the game's page for a .tar.gz, .zip, AppImage or Flatpak download instead.");
                }
                Problem::Package(file, _) => {
                    let _ = writeln!(out, "✗ This download is a Fedora-style package ({}). On Bazzite installing one means", file.display());
                    let _ = writeln!(out, "  layering it into the system (rpm-ostree install, then a restart).");
                    let _ = writeln!(out, "    Fix: a .tar.gz, .zip, AppImage or Flatpak download from the game's page is simpler.");
                }
                Problem::LinuxInstaller(file) => {
                    let _ = writeln!(out, "• {} is an installer (MojoSetup) with the game packed inside it.", file.display());
                    let _ = writeln!(out, "  gamefix setup unpacks the game into the folder \"{}\" next to it and sets that up.", crate::setup::unpacked_name(file));
                }
                Problem::UdkInstaller(file) => {
                    let _ = writeln!(out, "• {} is an installer (Epic's UDK installer) with the game packed inside it.", file.display());
                    let _ = writeln!(out, "  gamefix setup unpacks the game into the folder \"{}\" next to it, without running", crate::setup::unpacked_name(file));
                    let _ = writeln!(out, "  the installer, and sets that up.");
                }
                Problem::UnityResolution(run) => {
                    let _ = writeln!(out, "✗ When its start-up resolution window is on (most games), this Unity version (2018.2-2018.3)");
                    let _ = writeln!(out, "  starts at 1x1 pixels, one colour filling the screen, if the window can't open; it needs");
                    let _ = writeln!(out, "  GTK 2, which this system lacks (Baldi's Basics, Brownie Cove, Precedence). A game that");
                    let _ = writeln!(out, "  already started that way keeps 1x1 in its settings until the window replaces it.");
                    match run {
                        Some(run) => {
                            let _ = writeln!(out, "    Fix (at launch): start it through {} (Steam's runtime has GTK 2),", run.display());
                            let _ = writeln!(out, "    so the window opens, as on Windows. gamefix setup does that.");
                        }
                        None => {
                            let _ = writeln!(out, "    Fix: Steam's runtime for older games has GTK 2: install Steam, then set it up with gamefix.");
                        }
                    }
                }
                Problem::UnityDataMissing(file) => {
                    let stem = file.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                    let _ = writeln!(out, "✗ {} is a Unity game's program, but its data folder ({stem}_Data) isn't in", file.display());
                    let _ = writeln!(out, "  the download, so it can't start (\"There is no data folder\"). The upload is incomplete:");
                    let _ = writeln!(out, "  look for another download of the game on its page, or ask the developer.");
                }
                Problem::Damaged(file) => {
                    let _ = writeln!(out, "✗ The Linux program is damaged in the download itself: {}", file.display());
                    let _ = writeln!(out, "  It was saved as text somewhere along the way (every byte above 127 became the \"unknown");
                    let _ = writeln!(out, "  character\" mark), so it can't run. Downloading it again won't help; only the developer");
                    let _ = writeln!(out, "  can fix it by uploading it again.");
                    if let Some(i) = game.windows {
                        let _ = writeln!(out, "    Fix: run {} with Proton instead: gamefix setup adds it to the app menu that way (--steam: to Steam).", game.files[i].path.display());
                    }
                }
                Problem::Love { file, version, love } => {
                    // LÖVE 11 changed its API; 0.x games mostly fail on it (0.10.1: crashes loading sounds).
                    let old = old_love(version.as_deref());
                    if let (Some(love), false) = (love, old) {
                        let _ = writeln!(out, "• This is a LÖVE game; LÖVE is installed, so it runs with: {} \"{}\"", love.join(" "), file.display());
                        let _ = writeln!(out, "  gamefix setup adds that to the app menu.");
                        continue;
                    }
                    if love.is_some() {
                        let _ = writeln!(out, "✗ This is a LÖVE game ({}), made for LÖVE {}. The LÖVE installed here", file.display(), version.as_deref().unwrap_or_default());
                        let _ = writeln!(out, "  is 11 (the only one on Flathub), which can't run games made for 0.10 or older.");
                    } else {
                        let _ = writeln!(out, "✗ This is a LÖVE game: {} is run by the LÖVE program, which isn't part of the system.", file.display());
                    }
                    if let Some(why) = game.engine.as_ref().and_then(|e| e.guess.as_ref()) {
                        let _ = writeln!(out, "  (Its conf.lua names no LÖVE version; its code uses {why}.)");
                    }
                    if old {
                        if love.is_none() {
                            let _ = writeln!(out, "  It was made for LÖVE {}, and the LÖVE you can install (11, on Flathub) can't run", version.as_deref().unwrap_or_default());
                            let _ = writeln!(out, "  games made for 0.10 or older.");
                        }
                        match version.as_deref().and_then(crate::love::build_for) {
                            Some(build) if crate::love::installed(build) => {
                                let _ = writeln!(out, "    Fix: LÖVE {build} (LÖVE's own Linux build) is set up here; gamefix setup adds the game to the");
                                let _ = writeln!(out, "    app menu running on it.");
                            }
                            Some(build) => {
                                let _ = writeln!(out, "    Fix: LÖVE {build}, LÖVE's own Linux build: gamefix setup downloads it once (under 1 MB, plus");
                                let _ = writeln!(out, "    the libraries it needs that the system lacks) and adds the game to the app menu on it.");
                            }
                            None => {
                                let _ = writeln!(out, "    Fix: if the game has a Windows download, use that with Proton (it brings its own LÖVE).");
                            }
                        }
                    } else {
                        let _ = writeln!(out, "    Fix: LÖVE from Flathub (org.love2d.love2d): gamefix setup offers to install it, then adds the");
                        let _ = writeln!(out, "    game to the app menu or Steam.");
                    }
                }
                Problem::Python { file, v2: true, .. } => {
                    let _ = writeln!(out, "✗ This is a Python 2 game ({}). Python 2 was retired in 2020 and current", file.display());
                    let _ = writeln!(out, "  Linux systems don't include it anymore, so it can't run here.");
                    let _ = writeln!(out, "    Fix: if the game has a Windows download, use that with Proton.");
                }
                Problem::Python { file, plan: None, .. } => {
                    let _ = writeln!(out, "✗ This is a Python game ({}).", file.display());
                }
                Problem::Python { file, plan: Some(plan), .. } => {
                    let start = plan.launcher.as_ref().map(|l| format!(", started by {}", l.display())).unwrap_or_default();
                    let terminal = if plan.terminal { "\n  It's a text game: it runs in a terminal." } else { "" };
                    if plan.python.is_none() {
                        let _ = writeln!(out, "✗ This is a Python 3 game ({}{start}), and Python 3 isn't installed.", file.display());
                    } else if !plan.compile.is_empty() {
                        let _ = writeln!(out, "✗ This is a Python 3 game ({}{start}), but part of it ({}) is Cython source that has", file.display(), plan.compile.iter().map(|m| format!("{m}.pyx")).collect::<Vec<_>>().join(", "));
                        let _ = writeln!(out, "  to be compiled first, which needs a C compiler and its build tools: gamefix can't set it up.");
                        let _ = writeln!(out, "    Fix: if the game has a Windows download, use that with Proton.");
                    } else if plan.packages.is_empty() {
                        let _ = writeln!(out, "• This is a Python 3 game ({}{start}); Python 3 is part of the system.{terminal}", file.display());
                        let _ = writeln!(out, "  gamefix setup adds it to the app menu.");
                    } else {
                        let _ = writeln!(out, "✗ This is a Python 3 game ({}{start}).", file.display());
                        let _ = writeln!(out, "  Python 3 is part of the system, but it also needs {}, which {}.{terminal}", plan.packages.join(", "), if plan.packages.len() > 1 { "aren't" } else { "isn't" });
                        let _ = writeln!(out, "    Fix: gamefix setup installs that from PyPI (Python's package index) into a folder of the game's");
                        let _ = writeln!(out, "    own, ~/.local/share/gamefix/python/, and adds the game to the app menu.");
                    }
                    match (&plan.older, plan.system_minor) {
                        (Some((minor, what)), Some(system)) if crate::python::build_info(*minor).is_some() => {
                            let (version, size) = crate::python::build_info(*minor).unwrap();
                            let _ = writeln!(out, "✗ Its code uses {what}; this system has Python 3.{system}.");
                            let _ = writeln!(out, "    Fix: gamefix setup downloads Python {version} (python-build-standalone, {size} MB) once into");
                            let _ = writeln!(out, "    ~/.local/share/gamefix/python-builds/ for every game that needs it, and runs the game on it.");
                        }
                        (Some((minor, what)), _) => {
                            let _ = writeln!(out, "✗ Its code uses {what}: it needs Python 3.{minor} or older, and gamefix can get");
                            let _ = writeln!(out, "  3.8 at the oldest, so it can't set it up.");
                            let _ = writeln!(out, "    Fix: if the game has a Windows download, use that with Proton.");
                        }
                        _ => {}
                    }
                    if let Some((wanted, real)) = plan.case_links.first() {
                        let more = if plan.case_links.len() > 1 { format!(" and {} more", plan.case_links.len() - 1) } else { String::new() };
                        let _ = writeln!(out, "✗ Its code names files with other upper/lower case than they have ({} is {}{more}):", wanted.display(), real.file_name().unwrap_or_default().to_string_lossy());
                        let _ = writeln!(out, "  Windows doesn't mind, Linux does, so it stops when it loads one.");
                        let _ = writeln!(out, "    Fix: gamefix setup adds links under the names the code uses, beside the files.");
                    }
                }
                Problem::WrongMachine(m) => {
                    let cpu = match m {
                        183 => "64-bit ARM (aarch64: Raspberry Pi, ARM laptops, handhelds)".to_string(),
                        40 => "32-bit ARM (Raspberry Pi)".to_string(),
                        243 => "RISC-V".to_string(),
                        20 | 21 => "PowerPC".to_string(),
                        8 => "MIPS".to_string(),
                        _ => format!("another processor (ELF machine {m})"),
                    };
                    let _ = writeln!(out, "✗ The program is built for {cpu}, not a PC's x86 processor, so it can't run here.");
                    let _ = writeln!(out, "    Fix: get the game's download for x86_64 (\"Linux\", \"amd64\" or \"64-bit\"), or its Windows one with Proton.");
                }
                Problem::MissingFolders(config, folders) => {
                    let names: Vec<String> = folders.iter().map(|f| f.display().to_string()).collect();
                    let _ = writeln!(out, "✗ Its {} names folders the download doesn't have: {}. The game may stop", config.display(), names.join(", "));
                    let _ = writeln!(out, "  when it looks in one (labb: \"directory iterator cannot open directory\").");
                    let _ = writeln!(out, "    Fix: gamefix setup creates them, empty.");
                }
                Problem::WindowsLineEndings(p) => {
                    let _ = writeln!(out, "✗ {} was saved with Windows line endings, so Linux can't run it", p.display());
                    let _ = writeln!(out, "  (\"bad interpreter\", or \"No such file or directory\" for a file that's there).");
                    let _ = writeln!(out, "    Fix (once): convert it to Linux line endings (gamefix setup does that, keeping the original).");
                }
                Problem::NotExecutable(p) => {
                    let _ = writeln!(out, "✗ {} isn't marked as a program, so double-clicking it does nothing.", p.display());
                    let _ = writeln!(out, "    Usually lost when unpacking a zip. Fix (once): chmod +x \"{}\"", p.display());
                }
                Problem::MissingLibs { bits, libs, later, in_old_runtime, misplaced } => {
                    let mark = |lib: &String| match (in_old_runtime.contains(lib), misplaced.get(lib)) {
                        (true, Some(dir)) => format!(" (Steam's runtime has it; the game has it in {}/)", dir.display()),
                        (true, None) => " (Steam's runtime has it)".to_string(),
                        (false, Some(dir)) => format!(" (the game has it in {}/)", dir.display()),
                        (false, None) => String::new(),
                    };
                    let plural = |n: usize| if n == 1 { "y" } else { "ies" };
                    if !libs.is_empty() {
                        let _ = writeln!(out, "✗ {} {bits}-bit librar{} the game needs to start {} missing:", libs.len(), plural(libs.len()), if libs.len() == 1 { "is" } else { "are" });
                        for (lib, users) in libs {
                            let _ = writeln!(out, "    {lib}{}, needed by:", mark(lib));
                            list(&mut out, users);
                        }
                    }
                    if !later.is_empty() {
                        let _ = writeln!(out, "{} {} {bits}-bit librar{} only plugins need {} missing; they're loaded later, if at all, so", if libs.is_empty() { "•" } else { " " }, later.len(), plural(later.len()), if later.len() == 1 { "is" } else { "are" });
                        let _ = writeln!(out, "  the game may start without them (that feature won't work):");
                        for (lib, users) in later {
                            let _ = writeln!(out, "    {lib}{}, needed by:", mark(lib));
                            list(&mut out, users);
                        }
                    }
                    match &self.old_runtime {
                        Some(_) if !in_old_runtime.is_empty() && self.inside_appimage => {
                            let _ = writeln!(out, "  Steam's runtime for older games keeps libraries newer systems dropped{}, but the", if in_old_runtime.len() < libs.len() + later.len() { " (marked)" } else { ", all of these" });
                            let _ = writeln!(out, "  AppImage sets where it looks for libraries itself, so gamefix can't add them.");
                        }
                        Some(run) if !in_old_runtime.is_empty() => {
                            let _ = writeln!(out, "  Steam's runtime for older games keeps libraries newer systems dropped{}.", if in_old_runtime.len() < libs.len() + later.len() { " (marked)" } else { ", all of these" });
                            let _ = writeln!(out, "    Fix (at launch): start it through {}", run.display());
                            let _ = writeln!(out, "    Steam doesn't do that by itself; gamefix setup does it for the app-menu entry and the Steam shortcut.");
                        }
                        Some(_) => {}
                        None => {
                            let _ = writeln!(out, "  Steam's runtime for older games often has libraries like these, but Steam isn't installed.");
                        }
                    }
                    if !misplaced.is_empty() {
                        let _ = writeln!(out, "  The game ships some itself (marked), in a folder Linux doesn't look in for them.");
                        let _ = writeln!(out, "    Fix (at launch): LD_LIBRARY_PATH pointing to just those (gamefix setup links them into");
                        let _ = writeln!(out, "    a folder of their own, so the game's copies of what the system has aren't used).");
                    }
                    let unfixable: Vec<&str> = libs.keys().filter(|l| !in_old_runtime.contains(*l) && !misplaced.contains_key(*l)).map(String::as_str).collect();
                    if !unfixable.is_empty() {
                        let _ = writeln!(out, "  Neither the system{} nor the game has {}.", if self.old_runtime.is_some() { ", Steam's runtime" } else { "" }, unfixable.join(", "));
                        let _ = writeln!(out, "    Fix: gamefix setup looks for them in Fedora's packages (the release this system is built");
                        let _ = writeln!(out, "    on), then Debian's and older Ubuntu releases', and downloads them into ~/.local/share/gamefix/libs.");
                    }
                }
                Problem::Java { file, java: None } => {
                    let _ = writeln!(out, "✗ This is a Java game ({}): it runs with \"java -jar\", and Java isn't installed.", file.display());
                    let _ = writeln!(out, "    Fix: on Bazzite, run \"brew install openjdk\" in a terminal; then gamefix setup adds it to the app menu.");
                }
                Problem::Java { file, java: Some(java) } if file.extension().is_some_and(|e| e == "jar") => {
                    let _ = writeln!(out, "• This is a Java game; Java is installed, so it runs with: {} -jar \"{}\"", java.display(), file.display());
                    let _ = writeln!(out, "  gamefix setup adds that to the app menu.");
                    if binary::jar_in_jar(&game.root.join(file)).is_some() {
                        let _ = writeln!(out, "  Its libraries are jars packed inside it (Eclipse's jar-in-jar export), and the loader it");
                        let _ = writeln!(out, "  starts with doesn't find them on current Java (NoClassDefFoundError: Wobble, Art Treachery);");
                        let _ = writeln!(out, "  setup unpacks them into ~/.local/share/gamefix/jars and starts the game with them.");
                    }
                    let natives = binary::missing_natives(&game.root.join(file));
                    if !natives.is_empty() {
                        let names: Vec<&str> = natives.iter().filter_map(|n| n.rsplit('/').next()).collect();
                        let _ = writeln!(out, "\n✗ It carries only the Windows parts of its libraries (.dll, no .so), so on Linux it quits at");
                        let _ = writeln!(out, "  start (\"Couldn't load shared library 'libgdx64.so'\": Amphibian).");
                        let _ = writeln!(out, "    Fix: the Linux parts of the same versions from Maven Central, which gamefix setup downloads");
                        let _ = writeln!(out, "    into ~/.local/share/gamefix/jars and starts the game with: {}.", names.join(", "));
                    }
                    let needs = binary::jar_needs(&game.root.join(file));
                    let major = System::java_major(java);
                    if needs.nashorn && !major.is_some_and(|v| v <= 14) {
                        let _ = writeln!(out, "  It runs JavaScript through Java's own engine (Nashorn), which Java 15 removed: it needs");
                        let _ = writeln!(out, "  Java 14 or older (\"brew install openjdk@11\"; gamefix uses that one when it's there).");
                    } else if needs.old_lwjgl3 && !major.is_some_and(|v| v <= 16) {
                        let _ = writeln!(out, "  It brings LWJGL 3 from before 3.1 (2015-2016), which fails on current Java when it draws");
                        let _ = writeln!(out, "  (IndexOutOfBoundsException: Homorium); Java 11 runs it (\"brew install openjdk@11\"; gamefix");
                        let _ = writeln!(out, "  uses that one when it's there).");
                    } else if needs.applet && major.is_none() {
                        let _ = writeln!(out, "  It uses Java applets, which Java 26 removed: it needs Java 25 or older (\"brew install");
                        let _ = writeln!(out, "  openjdk@21\"; gamefix uses that one when it's there).");
                    }
                    if let Some(dir) = &needs.home_dir {
                        let home = std::env::var("HOME").unwrap_or_default();
                        if !Path::new(&home).join(dir).exists() {
                            let _ = writeln!(out, "• It keeps its files in ~/{dir}, which it may not make itself (Sheep Farm Simulator quits");
                            let _ = writeln!(out, "  on its log file without it): gamefix setup creates that folder.");
                        }
                    }
                    if let Some(shader) = &needs.shader {
                        let _ = writeln!(out, "✗ Its shader {shader} doesn't match the GLSL version it's compiled as: NVIDIA's driver lets");
                        let _ = writeln!(out, "  that pass, Mesa's (AMD and Intel graphics) stops it with \"Error compiling shader\".");
                        let _ = writeln!(out, "    Fix: gamefix setup makes Mesa read such shaders as GLSL 1.30 (force_glsl_version=130,");
                        let _ = writeln!(out, "    which other drivers ignore).");
                    }
                }
                Problem::Java { file, java: Some(java) } if scan::install4j_script(&game.root, file) => {
                    let _ = writeln!(out, "• This is a Java game; its launcher {} starts it with Java ({}).", file.display(), java.display());
                    let _ = writeln!(out, "  gamefix setup adds that to the app menu.");
                }
                Problem::Java { file, java: Some(java) } => {
                    let _ = writeln!(out, "• This is a Processing game; its script {} starts it with Java ({}).", file.display(), java.display());
                    let _ = writeln!(out, "  gamefix setup adds that to the app menu, with Java's own sound sent to PipeWire (it would");
                    let _ = writeln!(out, "  open the sound card directly, which PipeWire already holds, and get none).");
                    let (applet, only32, arm) = scan::processing_notes(&game.root, file);
                    let lts = java.to_string_lossy().contains("/openjdk@");
                    if applet && !lts {
                        let _ = writeln!(out, "  It's built on Processing 2 or older, a Java applet, and Java 26 removed those: it needs");
                        let _ = writeln!(out, "  Java 25 or older (\"brew install openjdk@21\"; gamefix uses that one when it's there).");
                    } else if !lts {
                        let _ = writeln!(out, "  Processing from before 2018 can't start on a Java whose version is a single number (\"27\",");
                        let _ = writeln!(out, "  until its first update): Dial didn't. Java 21 (\"brew install openjdk@21\") works, and");
                        let _ = writeln!(out, "  gamefix uses it when it's there.");
                    }
                    if only32 {
                        let _ = writeln!(out, "  It's a 32-bit export: if it draws with OpenGL, 64-bit Java can't load its 32-bit parts");
                        let _ = writeln!(out, "  (Rainbeats, Protection Recommended couldn't). gamefix setup downloads the 64-bit ones");
                        let _ = writeln!(out, "  for its JOGL version from Maven Central into its lib folder.");
                    }
                    if arm {
                        let _ = writeln!(out, "✗ But it's an export for ARM computers (Raspberry Pi): its OpenGL and sound parts can't");
                        let _ = writeln!(out, "  load on this PC (Fairy Piano's sound library didn't).");
                        let _ = writeln!(out, "    Fix: the game's Linux 64-bit or Windows download, if it has one.");
                    }
                }
                Problem::OwnHarfbuzz(file, preload, in_program) => {
                    let _ = writeln!(out, "✗ {} has its own old copy of HarfBuzz (text shaping) built in and", file.display());
                    let _ = writeln!(out, "  shared. When the system's text libraries load theirs, they get its old functions and the");
                    let _ = writeln!(out, "  game crashes at start (JK Assassins: in hb_face_reference_table).");
                    if *in_program {
                        let _ = writeln!(out, "  The program itself has it, so it comes first whatever is preloaded; an older pango");
                        let _ = writeln!(out, "  that doesn't use HarfBuzz avoids it (Steam's runtime for older games has one).");
                    }
                    match preload {
                        Some(p) => {
                            let _ = writeln!(out, "    Fix (at launch): LD_PRELOAD={p}");
                        }
                        None if *in_program => {
                            let _ = writeln!(out, "    Fix (at launch): preload Steam's runtime's pango (Steam's runtime wasn't found).");
                        }
                        None => {
                            let _ = writeln!(out, "    Fix (at launch): preload the system's libharfbuzz.so.0 (not found here).");
                        }
                    }
                }
                Problem::PygameMp3 { sdl20 } => {
                    let _ = writeln!(out, "✗ Its music is MP3, and the SDL_mixer its pygame brings (older than 2.6) plays MP3 only with a");
                    let _ = writeln!(out, "  decoder library the developer's build left out: \"Unrecognized audio format\" when it loads it.");
                    let _ = writeln!(out, "    Fix (at launch): preload a newer SDL_mixer (Fedora's, which has MP3 built in); gamefix setup");
                    let _ = writeln!(out, "    fetches it into ~/.local/share/gamefix/libs.");
                    if *sdl20 {
                        let _ = writeln!(out, "    Its pygame accepts only an SDL 2.0, so Debian 11's SDL 2.0.14 goes with it.");
                    }
                }
                Problem::AllegroWebp(image) => {
                    let _ = writeln!(out, "✗ Its own Allegro image add-on was built without WebP, and the game has .webp images: Allegro");
                    let _ = writeln!(out, "  finds no loader for them and the game crashes when it draws one ({}).", image.display());
                    let _ = writeln!(out, "    Fix (at launch): preload Fedora's Allegro image add-on, which reads WebP; gamefix setup");
                    let _ = writeln!(out, "    fetches it into ~/.local/share/gamefix/libs.");
                }
                Problem::BundledLibstdcxx(preload) => {
                    let _ = writeln!(out, "✗ It draws with OpenGL from Python, and its bundle brings its own copy of the C++ standard");
                    let _ = writeln!(out, "  library, which goes first. If that's older than what the graphics driver (Mesa: AMD,");
                    let _ = writeln!(out, "  Intel) needs, the driver doesn't load and no window opens (\"Couldn't find matching GLX");
                    let _ = writeln!(out, "  visual\": Skeletris, RESYNC).");
                    match preload {
                        Some(p) => {
                            let _ = writeln!(out, "    Fix (at launch): LD_PRELOAD={p}");
                        }
                        None => {
                            let _ = writeln!(out, "    Fix (at launch): preload the system's libstdc++.so.6 (not found here).");
                        }
                    }
                }
                Problem::OwnLibstdcxx(preload) => {
                    let _ = writeln!(out, "✗ The program has its own old copy of the C++ standard library built in. When the graphics");
                    let _ = writeln!(out, "  driver loads the system's copy, that one gets the program's old functions and the game");
                    let _ = writeln!(out, "  crashes at start (2:22AM, Unity 5.0: in std::ios_base::Init).");
                    match preload {
                        Some(p) => {
                            let _ = writeln!(out, "    Fix (at launch): LD_PRELOAD={p}");
                        }
                        None => {
                            let _ = writeln!(out, "    Fix (at launch): preload the system's libstdc++.so.6 (not found here).");
                        }
                    }
                }
                Problem::Sdl2Mix(path) => {
                    let _ = writeln!(out, "✗ SDL2 add-ons from Steam's runtime were built for the real SDL2, but this system's SDL2");
                    let _ = writeln!(out, "  is sdl2-compat (SDL3 underneath); some games crash with that mix (C-Dogs SDL did).");
                    let _ = writeln!(out, "    Fix (at launch): LD_PRELOAD={path} (the runtime's own SDL2)");
                }
                Problem::KickFiles(files) => {
                    let _ = writeln!(out, "✗ The download lacks files the Mono it brings needs ({}), so it quits at once", files.join(", "));
                    let _ = writeln!(out, "  (\"{} not found!\"). Only the developer can add them (Puzzle Kids left them out).", files[0]);
                    let _ = writeln!(out, "    Fix: the game's .exe with Proton, or a Windows download.");
                }
                Problem::Shadowed(copy, user, example, preload) => {
                    let _ = writeln!(out, "✗ The game's own {} is older than the system's {user} expects", copy.display());
                    let _ = writeln!(out, "  ({example} is missing from it). Loaded first, it makes {user} fail to load, and the game");
                    let _ = writeln!(out, "  quits or loses that feature (Darkness Looming: libgio, g_module_open_full).");
                    match preload {
                        Some(p) => {
                            let _ = writeln!(out, "    Fix (at launch): LD_PRELOAD={p}");
                        }
                        None => {
                            let _ = writeln!(out, "    Fix (at launch): preload the system's copy (not found here).");
                        }
                    }
                }
                Problem::JavaSound(_) => {
                    let _ = writeln!(out, "✗ It brings its own Java, and Java's own sound opens the sound card directly, which PipeWire");
                    let _ = writeln!(out, "  already holds: no sound (Dial, Black & White, Rainbeats had none).");
                    let _ = writeln!(out, "    Fix (at launch): an ALSA setting for this game that sends Java's sound to PipeWire.");
                }
                Problem::OldPulse(server) => {
                    let _ = writeln!(out, "✗ It brings its own PulseAudio library from before 2011, which doesn't know where today's");
                    let _ = writeln!(out, "  sound server (PipeWire) is, so it gets no sound or quits loading one (Sascha did).");
                    let _ = writeln!(out, "    Fix (at launch): PULSE_SERVER={server}");
                }
                Problem::Alsa32(plugins) => {
                    let _ = writeln!(out, "✗ It plays sound only through ALSA, as a 32-bit program, and this system has no 32-bit");
                    let _ = writeln!(out, "  plugin for ALSA's default device (PipeWire's comes in 64-bit only). It gets no sound,");
                    let _ = writeln!(out, "  and some games quit (RogueBox Adventures does, loading its first sound).");
                    if plugins.is_some() {
                        let _ = writeln!(out, "    Fix (at launch): an ALSA setting that sends its sound through Steam's runtime's 32-bit");
                        let _ = writeln!(out, "    PulseAudio plugin (PipeWire plays it).");
                    } else {
                        let _ = writeln!(out, "    Fix: Steam's runtime has a 32-bit plugin that would work; install Steam to get it.");
                    }
                }
                Problem::DotnetStatic(files) => {
                    let _ = writeln!(out, "✗ Its .NET list of files names {} the download left out: {}.", if files.len() == 1 { "one" } else { "some" }, files.join(", "));
                    let _ = writeln!(out, "  They're static libraries, only used when building, never loaded; but .NET won't start");
                    let _ = writeln!(out, "  while any listed file is missing (\"An assembly specified in the application dependencies");
                    let _ = writeln!(out, "  manifest ... was not found\").");
                    let _ = writeln!(out, "    Fix (once): empty files with those names next to the program (gamefix setup makes them).");
                }
                Problem::DotnetRuntime(version) => {
                    let _ = writeln!(out, "✗ It was published without .NET: it needs Microsoft's .NET runtime {version} installed, and");
                    let _ = writeln!(out, "  there's none here, so it quits at once (\"libhostfxr.so could not be found\").");
                    let _ = writeln!(out, "    Fix: gamefix setup downloads that runtime from Microsoft into ~/.local/share/gamefix/dotnet");
                    let _ = writeln!(out, "    (30-100 MB, shared by every game of that version) and starts the game with DOTNET_ROOT set.");
                    let _ = writeln!(out, "    Older versions get no security fixes; a Windows build with Proton avoids that.");
                }
                Problem::RpgMaker { dir, easyrpg, rtp } => {
                    let shown = if dir.as_os_str().is_empty() { ".".into() } else { dir.display().to_string() };
                    match easyrpg {
                        Some(easyrpg) => {
                            let _ = writeln!(out, "• This is an RPG Maker 2000/2003 game ({shown}); EasyRPG Player, which plays them, is installed:");
                            let _ = writeln!(out, "  {} --project-path \"{shown}\"", easyrpg.join(" "));
                            let _ = writeln!(out, "  gamefix setup adds that to the app menu.");
                        }
                        None => {
                            let _ = writeln!(out, "✗ This is an RPG Maker 2000/2003 game ({shown}): data for RPG Maker's player, which is a Windows program.");
                            let _ = writeln!(out, "    Fix: EasyRPG Player from Flathub (org.easyrpg.player) plays it natively: gamefix setup");
                            let _ = writeln!(out, "    offers to install it, then adds the game to the app menu or Steam.");
                        }
                    }
                    if *rtp {
                        let _ = writeln!(out, "  It uses graphics and music from RPG Maker's runtime package (RPG_RT.ini has no");
                        let _ = writeln!(out, "  FullPackageFlag=1); EasyRPG plays it without, but those may be missing ({}).", crate::rpgmaker::RTP_PAGE);
                    }
                }
                Problem::RpgMakerPacked { file, kind, dest } => {
                    let _ = writeln!(out, "✗ The game ({kind}) is still packed in {}: a self-extracting package", file.display());
                    let _ = writeln!(out, "  whose Windows setup wants RPG Maker's own files installed first.");
                    let _ = writeln!(out, "    Fix (once): gamefix setup unpacks it into \"{dest}\" beside it and sets that up.");
                }
                Problem::RgssRtp(kind) => {
                    let _ = writeln!(out, "✗ It's an RPG Maker {kind} game that uses RPG Maker {kind}'s runtime package (its RGSS library,");
                    let _ = writeln!(out, "  graphics and music aren't in its folder): it can't start until that's installed where it runs.");
                    if *kind == "XP" {
                        let _ = writeln!(out, "    Fix (once): gamefix setup --steam installs the package (from rpgmakerweb.com) into a Proton");
                        let _ = writeln!(out, "    prefix shared by RPG Maker XP games, and has Steam run the game there.");
                    } else {
                        let _ = writeln!(out, "    Fix: install RPG Maker {kind}'s runtime package ({}) into the game's Proton prefix", crate::rpgmaker::RTP_PAGE);
                        let _ = writeln!(out, "    (gamefix does that for RPG Maker XP only so far).");
                    }
                }
                Problem::Dotnet { file, version, dotnet: Some(dotnet) } => {
                    let _ = writeln!(out, "• This is a .NET {version} game built for any system, with its Linux libraries; Microsoft's .NET");
                    let _ = writeln!(out, "  is installed in {}, so it runs with:", dotnet.display());
                    let _ = writeln!(out, "  {}/dotnet \"{}\"", dotnet.display(), file.display());
                    let _ = writeln!(out, "  gamefix setup adds that to the app menu.");
                }
                Problem::Dotnet { file, version, dotnet: None } => {
                    let channel = crate::dotnet::channel(version);
                    let _ = writeln!(out, "✗ This is a .NET {version} game built for any system ({}), with its Linux libraries;", file.display());
                    let _ = writeln!(out, "  it needs Microsoft's .NET runtime {channel}, which isn't installed (its .exe only starts .NET on Windows).");
                    let _ = writeln!(out, "    Fix: gamefix setup downloads that runtime from Microsoft into ~/.local/share/gamefix/dotnet");
                    let _ = writeln!(out, "    (30-100 MB, shared by every game of that version) and adds the game to the app menu.");
                    let _ = writeln!(out, "    Older versions get no security fixes; the Windows build with Proton avoids that.");
                }
                Problem::PygameFont { missing, font, .. } => {
                    let _ = writeln!(out, "✗ It's a pygame 1 game packed by PyInstaller 3, and its code asks for system fonts (SysFont). For");
                    let _ = writeln!(out, "  one that isn't installed pygame falls back to its own font, which it looks up through");
                    let _ = writeln!(out, "  pkg_resources first; PyInstaller 3 left that unable to answer, so the game crashes there");
                    let _ = writeln!(out, "  (\"NotImplementedError ... unregistered loader type\", then a segfault).");
                    if let Some(missing) = missing {
                        let place = if matches!(missing, PygameFont::Bundle) { "packed in the program" } else { "in its pygame folder" };
                        let _ = writeln!(out, "  That font ({PYGAME_FONT}) isn't {place} either (the build left it out).");
                    }
                    match (missing, font) {
                        (Some(_), None) => {
                            let _ = writeln!(out, "    Fix: hide pkg_resources from its Python and add a stand-in font (any bold sans .ttf), but");
                            let _ = writeln!(out, "    no TrueType font was found on this system.");
                        }
                        _ => {
                            let _ = writeln!(out, "    Fix (once): gamefix setup hides pkg_resources from the program's Python, so pygame opens the");
                            match (missing, font) {
                                (Some(PygameFont::Bundle), Some(f)) => {
                                    let _ = writeln!(out, "    font file itself, and packs a stand-in for it ({}) into the program.", f.file_name().unwrap_or_default().to_string_lossy());
                                }
                                (Some(PygameFont::Folder(_)), Some(f)) => {
                                    let _ = writeln!(out, "    font file itself, and copies a stand-in for it ({}) into its pygame folder.", f.file_name().unwrap_or_default().to_string_lossy());
                                }
                                _ => {
                                    let _ = writeln!(out, "    font file itself.");
                                }
                            }
                            let _ = writeln!(out, "    The original program is kept in ~/.local/share/gamefix/backups.");
                        }
                    }
                }
                Problem::DotnetIcu(version) => {
                    let _ = writeln!(out, "✗ It's built on .NET Core {version}, which only looks for older versions of the ICU library");
                    let _ = writeln!(out, "  (language and text rules) than this system has, so it quits at start.");
                    let _ = writeln!(out, "    Fix (at launch): DOTNET_SYSTEM_GLOBALIZATION_INVARIANT=1 (one set of rules for every");
                    let _ = writeln!(out, "    language, which games don't notice).");
                }
                Problem::ExecStack(paths) => {
                    let (a, b) = self.glibc.unwrap_or_default();
                    let _ = writeln!(out, "✗ {} librar{} won't load: {} built asking for an \"executable stack\",", paths.len(), if paths.len() == 1 { "y" } else { "ies" }, if paths.len() == 1 { "it was" } else { "they were" });
                    let _ = writeln!(out, "  which this system's glibc ({a}.{b}) refuses for security (since 2.41). Nothing shows on");
                    let _ = writeln!(out, "  screen; whatever uses it (often the sound) just doesn't work:");
                    list(&mut out, paths);
                    let _ = writeln!(out, "    Fix (at launch): GLIBC_TUNABLES=glibc.rtld.execstack=2");
                }
                Problem::SymbolClash(clashes) => {
                    let _ = writeln!(out, "✗ Libraries that get the wrong functions: another library, loaded first, has functions");
                    let _ = writeln!(out, "  with the same names, and Linux hands out those instead of the ones they were built for.");
                    let _ = writeln!(out, "  Usually a feature (often the sound) quietly fails to start:");
                    for c in clashes {
                        let file = |p: &PathBuf| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                        let _ = writeln!(out, "      {} expects {} function{} from {} but gets {}'s (e.g. {})", file(&c.victim), c.count, if c.count == 1 { "" } else { "s" }, file(&c.provider), file(&c.shadow), c.example);
                    }
                    let preload: Vec<String> = Problem::preload(clashes).iter().map(|p| game.root.join(p).display().to_string()).collect();
                    let _ = writeln!(out, "    Fix (at launch): LD_PRELOAD={}", preload.join(":"));
                }
                Problem::UnityH264(files) => {
                    let _ = writeln!(out, "✗ Its videos (intros, cutscenes) may not play (black or skipped): they look like H.264,");
                    let _ = writeln!(out, "  which Unity's Linux player can't decode (it plays VP8/WebM only). The game still runs:");
                    list(&mut out, files);
                    let _ = writeln!(out, "    Fix: if the game has a Windows download, that with Proton plays them.");
                }
                Problem::UnityFont(names) => {
                    let _ = writeln!(out, "✗ Its text may not show: this Unity player draws its built-in font with a system font named");
                    let _ = writeln!(out, "  {}, which it looks for in /usr/share/fonts only, and this system has none of them.", names.join(" or "));
                    let _ = writeln!(out, "  Menus and labels drawn with it come out blank (Zandvox, Sector Plural Zed).");
                    let _ = writeln!(out, "    Fix (at launch): a copy of Liberation Sans named Arial, shown to the game in that folder");
                    let _ = writeln!(out, "    (gamefix setup does that with bwrap; /usr itself isn't changed).");
                }
                Problem::UnityShaders(n) => {
                    let _ = writeln!(out, "• Its first start stays black for a while, maybe minutes: this old Unity has the graphics");
                    let _ = writeln!(out, "  driver compile all its {n} shaders before it draws anything. Let it be; the driver");
                    let _ = writeln!(out, "  keeps them, so later starts are quick.");
                }
                Problem::Terminal { .. } => {
                    let _ = writeln!(out, "• It's a text game: it draws in a terminal, not a window, so started without one it");
                    let _ = writeln!(out, "  quits at once. gamefix setup's menu entry opens a terminal for it (Steam can't).");
                }
                Problem::UnitySingleInstance => {
                    let _ = writeln!(out, "✗ Unity before 5.4 may quit at once with \"Player is already running\" while Steam or");
                    let _ = writeln!(out, "  another Chrome-based app is open: its check that it isn't running twice mistakes their");
                    let _ = writeln!(out, "  helper processes for the game (Particle Fleet, Creeper World 3).");
                    let _ = writeln!(out, "    Fix (at launch): hide those helpers' command lines from it (gamefix setup does that");
                    let _ = writeln!(out, "    with bwrap), or run the Windows build with Proton.");
                }
            }
            let _ = writeln!(out);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folders_a_config_names() {
        let root = std::env::temp_dir().join(format!("gamefix-folders-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for d in ["game/data", "game/data-hd"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        std::fs::write(root.join("game/config.labb"), "data editor_data\n# data data-sd\ndata data-hd\ndata data\nresolution 1024 768\n").unwrap();
        // One folder there isn't enough to tell a folder list from other settings.
        std::fs::write(root.join("game/other.cfg"), "music on\nmusic off\n").unwrap();
        let found = missing_folders(&root, Path::new("game"));
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(found, Some((PathBuf::from("game/config.labb"), vec![PathBuf::from("game/editor_data")])));
    }

    #[test]
    fn unity_versions_starting_tiny() {
        assert!(unity_tiny_start("2018.2.14f1") && unity_tiny_start("2018.3.0b12") && unity_tiny_start("2018.2.21f1"));
        assert!(!unity_tiny_start("2018.2.1f1") && !unity_tiny_start("2018.4.26f1") && !unity_tiny_start("2019.2.6f1"));
    }

    #[test]
    fn renamed_copies_are_the_same_library() {
        assert!(same_library(Path::new("x/libz-a147dcb0.so.1.2.3"), Path::new("libz.so.1")));
        assert!(!same_library(Path::new("libfmod.so"), Path::new("UnityPlayer.so")));
        assert!(!same_library(Path::new("libSDL2-2.0.so.0"), Path::new("libSDL2.so")));
    }

    #[test]
    fn origin_means_the_files_folder() {
        let dirs = ["$ORIGIN/../lib".to_string(), "${ORIGIN}".to_string(), "lib".to_string(), "/opt/x".to_string()];
        let out = expand(&dirs, Path::new("/g/bin/x"), Path::new("/g/bin"));
        assert_eq!(out, [PathBuf::from("/g/bin/x/../lib"), PathBuf::from("/g/bin/x"), PathBuf::from("/g/bin/lib"), PathBuf::from("/opt/x")]);
    }
}
