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
    /// Nothing to run, but an archive the game may still be packed in (C-Dogs SDL: a .tar.gz
    /// inside the .zip).
    Archive(PathBuf),
    /// The Linux program is there but damaged (went through a text conversion): no fixing it.
    Damaged(PathBuf),
    /// A Linux installer (MojoSetup) with the game packed inside, and no program besides.
    LinuxInstaller(PathBuf),
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
    /// H.264 videos in a Unity Linux build: its player only decodes VP8 there.
    UnityH264(Vec<PathBuf>),
    /// The program has the C++ standard library built in and exports it; the system's copy,
    /// loaded later by the graphics driver, then gets the old functions and crashes (2:22AM).
    /// The system's libstdc++ to preload, if found.
    OwnLibstdcxx(Option<String>),
    /// A PyInstaller build carrying its own libstdc++.so.6, which goes first; Mesa's GL driver
    /// needs the system's newer one and isn't loaded: "Couldn't find matching GLX visual" from
    /// SDL (Skeletris, RESYNC). The system's copy to preload.
    BundledLibstdcxx(Option<String>),
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
    /// The game's own .NET libraries built for .NET Standard, which the Mono it brings
    /// (MonoKickstart) can't load: no System.Runtime / netstandard in its folder.
    NetStandard(Vec<String>),
    /// .NET Core before 5 only looks for older ICU versions than current systems have, and
    /// quits at start (The Heart's Illumination, 2.1).
    DotnetIcu(String),
    /// A framework-dependent .NET program (no libhostfxr.so beside it; its runtimeconfig.json
    /// names the framework) and no .NET runtime of that major version installed: Incremental
    /// Unlimited ("the required library libhostfxr.so could not be found", .NET Core 2.1).
    DotnetRuntime(String),
    /// A self-contained .NET program whose deps.json lists static libraries (.a) the download
    /// left out; the host refuses to start while any listed file is missing, though .a files
    /// are only for building and never loaded (Sokoban, 3.1.11: System.IO.Compression.Native.a
    /// and three more). Their names, in the program's folder.
    DotnetStatic(Vec<String>),
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

/// A project's build file at the top or one folder down, when there's nothing to run.
fn source_project(root: &Path) -> Option<(PathBuf, &'static str)> {
    const MARKERS: [(&str, &str); 6] = [("project.godot", "a Godot project"), ("CMakeLists.txt", "a CMake project"), ("Cargo.toml", "a Rust project"), ("meson.build", "a Meson project"), ("configure.ac", "an Autotools project"), ("Makefile", "a Makefile project")];
    let dirs = std::iter::once(root.to_path_buf()).chain(std::fs::read_dir(root).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.is_dir()));
    dirs.flat_map(|d| MARKERS.iter().map(move |(m, kind)| (d.join(m), *kind))).find(|(p, _)| p.is_file()).map(|(p, kind)| (p.strip_prefix(root).unwrap_or(&p).to_path_buf(), kind))
}

fn optional_plugin(path: &std::path::Path) -> Option<&'static str> {
    let name = path.file_name()?;
    OPTIONAL_PLUGINS.iter().find(|(n, _)| name == *n).map(|(_, what)| *what)
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
                None if !scan::not_the_game(&f.path) && !(pe.dotnet && game.main.is_some()) => windows.push((f.path.clone(), pe.bits)),
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
            Some(f) if matches!(f.binary, Binary::Dos) => problems.push(Problem::Dos { file: f.path.clone(), dosbox: system.dosbox() }),
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
                let source = if game.windows.is_none() { source_project(&game.root) } else { None };
                let web = if game.windows.is_none() { web_export(&game.root) } else { None };
                problems.push(match (damaged, game.archive, package) {
                    (Some(f), _, _) => Problem::Damaged(f.path.clone()),
                    _ if game.linux_installer.is_some() => Problem::LinuxInstaller(game.files[game.linux_installer.unwrap()].path.clone()),
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
    let mode = std::fs::metadata(game.full(main)).map(|m| m.permissions().mode()).unwrap_or(0);
    if mode & 0o100 == 0 {
        problems.push(Problem::NotExecutable(main.path.clone()));
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
    if main.path.to_string_lossy().contains(".bin.x86") {
        let libs = net_standard(&main_dir);
        if !libs.is_empty() {
            problems.push(Problem::NetStandard(libs));
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
    let first: Vec<(&String, &PathBuf)> = by_name
        .iter()
        .filter_map(|(n, (p, _))| {
            let name = n.to_str()?;
            let needed_from = same.iter().any(|(q, e)| e.needed.iter().any(|l| l == name) && (launcher_path || search(q, e).iter().any(|d| d.join(name).is_file())));
            (system.has(name, elf.bits) && !glibc_part(name) && needed_from).then(|| (same.iter().find_map(|(_, e)| e.needed.iter().find(|l| *l == name)), *p))
        })
        .filter_map(|(n, p)| n.map(|n| (n, p)))
        .collect();
    if !first.is_empty() {
        let mut users: BTreeSet<&String> = BTreeSet::new();
        for (_, e) in &same {
            users.extend(e.needed.iter().filter(|l| !by_name.contains_key(OsStr::new(l.as_str())) && system.has(l, elf.bits) && !glibc_part(l)));
        }
        let mut seen: BTreeSet<&String> = BTreeSet::new();
        for user in users {
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
    if !unity4 && !elf.needed.iter().any(|l| l == "libstdc++.so.6") && binary::symbols(&game.full(main)).is_some_and(|s| s.defined.iter().any(|d| d.starts_with("_ZNSt8ios_base4Init"))) {
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
    if let Some(version) = dotnet_framework(&main_dir, &main.path).filter(|v| !dotnet_installed(v)) {
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
    if game.engine.as_ref().is_some_and(|e| e.name == "Unity") {
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
/// The game's own .NET libraries next to a MonoKickstart program that reference .NET Standard's
/// assemblies (System.Runtime, netstandard) when the folder has neither; the framework's own
/// files mention those names too, so they're skipped. Mario is Doomed: Comora.dll.
fn net_standard(dir: &Path) -> Vec<String> {
    if ["System.Runtime.dll", "netstandard.dll", "Facades"].iter().any(|f| dir.join(f).exists()) {
        return Vec::new();
    }
    let framework = |n: &str| ["System", "mscorlib", "Mono.", "Microsoft.", "MonoGame.", "FNA", "OpenTK", "Tao.", "SDL2-CS"].iter().any(|p| n.starts_with(p));
    let mut out: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let ext = name.rsplit('.').next().unwrap_or("").to_lowercase();
            ((ext == "dll" || ext == "exe") && !framework(&name) && binary::contains_groups(&e.path(), &[&[b"\0System.Runtime\0", b"\0netstandard\0"]])).then_some(name)
        })
        .collect();
    out.sort();
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

/// The .NET version a framework-dependent program asks for: its runtimeconfig.json names
/// Microsoft.NETCore.App under "framework" ("frameworks" from .NET 7), and libhostfxr.so isn't
/// beside it (a self-contained one brings it; a single-file one has its own runtimeconfig inside).
fn dotnet_framework(dir: &Path, main: &Path) -> Option<String> {
    let stem = main.file_name()?.to_string_lossy().into_owned();
    if dir.join("libhostfxr.so").is_file() {
        return None;
    }
    let config = std::fs::read_to_string(dir.join(format!("{stem}.runtimeconfig.json"))).ok()?;
    let at = config.find("\"Microsoft.NETCore.App\"")?;
    let rest = &config[at..];
    let v = rest.find("\"version\"").map(|i| &rest[i + 9..])?;
    let v = v.trim_start_matches([' ', ':', '\n', '\r', '\t', '"']);
    Some(v.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect()).filter(|v: &String| !v.is_empty())
}

/// Whether a .NET runtime of that major version is in one of the usual places (its default
/// roll-forward takes a newer minor or patch, not a newer major).
fn dotnet_installed(version: &str) -> bool {
    let major = version.split('.').next().unwrap_or("");
    let mut roots: Vec<PathBuf> = ["/usr/share/dotnet", "/usr/lib64/dotnet", "/usr/lib/dotnet", "/opt/dotnet"].iter().map(PathBuf::from).collect();
    roots.extend(std::env::var_os("DOTNET_ROOT").map(PathBuf::from));
    roots.extend(std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".dotnet")));
    roots.iter().any(|r| {
        std::fs::read_dir(r.join("shared/Microsoft.NETCore.App")).is_ok_and(|d| d.flatten().any(|e| e.file_name().to_string_lossy().split('.').next() == Some(major)))
    })
}

/// Its major version.
fn dotnet_core(dir: &Path, main: &Path) -> Option<u32> {
    dotnet_core_version(dir, main)?.split('.').next()?.parse().ok()
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
                (Binary::Dos, _) => "DOS game".into(),
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
                        let _ = writeln!(out, "    Fix: install {} into its Proton prefix (protontricks); gamefix setup --steam does", components.join(" "));
                        let _ = writeln!(out, "    that once the game has been started from Steam once.");
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
                    let _ = writeln!(out, "    Fix (at launch): WINEDLLOVERRIDES={}; gamefix setup --steam adds it.", crate::proton::overrides(dlls));
                }
                Problem::Web(file) => {
                    let _ = writeln!(out, "✗ This is a web (HTML5) game ({}): it runs in a browser, not as a program.", file.display());
                    let _ = writeln!(out, "  Opened straight from the disk it often stays blank: browsers stop a page opened from a");
                    let _ = writeln!(out, "  file from loading the rest of the game.");
                    let _ = writeln!(out, "    Fix: play it on its itch.io page, where it runs in the browser as made.");
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
                                let _ = writeln!(out, "    Fix: run {} with Proton: gamefix setup --steam adds it to Steam that way.", game.files[i].path.display());
                            }
                        }
                        for (what, _) in &anticheat {
                            let _ = writeln!(out, "• It uses {what}: that runs under Proton only if the game's developer turned it on.");
                            let _ = writeln!(out, "  If it won't start or can't go online, nothing on this side can change that.");
                        }
                    } else if !self.installers.is_empty() {
                        let _ = writeln!(out, "    It's an installer: gamefix setup --steam runs it with Proton (you click through it");
                        let _ = writeln!(out, "    once) and adds the game it installs to Steam.");
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
                Problem::CdFiles { installer } => {
                    let _ = writeln!(out, "• These are a Windows game CD's files; its installer is {}.", installer.display());
                    let _ = writeln!(out, "  gamefix setup --steam installs it with Proton (you click through the installer once)");
                    let _ = writeln!(out, "  and adds the installed game to Steam.");
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
                    let _ = writeln!(out, "  gamefix setup --steam installs it with Proton (you click through the installer once)");
                    let _ = writeln!(out, "  and adds the installed game to Steam.");
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
                        let _ = writeln!(out, "    Fix: run {} with Proton instead: gamefix setup --steam adds it to Steam that way.", game.files[i].path.display());
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
                Problem::NetStandard(libs) => {
                    let _ = writeln!(out, "✗ Part of the game was built for .NET Standard, which the Mono it brings can't load");
                    let _ = writeln!(out, "  (its folder has no System.Runtime or netstandard), so it quits at start: {}.", libs.join(", "));
                    let _ = writeln!(out, "  That's how the developer packaged it; nothing on this system can fill it in.");
                    let _ = writeln!(out, "    Fix: the game's .exe with Proton (Proton's Mono has those), or a Windows download.");
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
                    let _ = writeln!(out, "    Fix: Microsoft's dotnet-install script (dot.net/v1/dotnet-install.sh, --runtime dotnet");
                    let _ = writeln!(out, "    --version {version}) puts it in ~/.dotnet, and the game starts with DOTNET_ROOT=~/.dotnet.");
                    let _ = writeln!(out, "    Older versions get no security fixes; a Windows build with Proton avoids that.");
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
