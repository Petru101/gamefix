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
    /// Nothing to run, but an archive the game may still be packed in (C-Dogs SDL: a .tar.gz
    /// inside the .zip).
    Archive(PathBuf),
    /// A .love file: needs the LÖVE version it was made for.
    Love { file: PathBuf, version: Option<String> },
    /// Python code: needs Python (2 is gone from current systems) and maybe pygame.
    Python { file: PathBuf, v2: bool, pygame: bool },
    /// A .jar: needs Java; `java` is where one was found.
    Java { file: PathBuf, java: Option<PathBuf> },
    WrongMachine(u16),
    NotExecutable(PathBuf),
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
    /// NW.js on Chromium 56-90 quits at start with "Could not find the default font" under
    /// current fontconfig rules (lilac & her light, 65; 38-55 are fine). The Chromium version.
    NwjsFonts(String),
    /// Native libraries FNA.dll.config maps to that are neither shipped nor installed
    /// (Shipwreck's download has empty lib/ and lib64/).
    FnaLibs(Vec<String>),
    /// SDL2 add-ons (mixer, image, ...) come from Steam's runtime, built for the real SDL2, while
    /// the system's SDL2 is sdl2-compat on top of SDL3: C-Dogs SDL crashed with that mix and
    /// runs with the runtime's own SDL2 preloaded. That library's path.
    Sdl2Mix(String),
    /// .NET Core before 5 only looks for older ICU versions than current systems have, and
    /// quits at start (The Heart's Illumination, 2.1).
    DotnetIcu(String),
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
    /// The folders, as absolute paths, holding the game's own copies of libraries ld.so misses.
    pub fn library_path(misplaced: &BTreeMap<String, PathBuf>, root: &Path) -> Vec<String> {
        let mut dirs: Vec<String> = misplaced.values().map(|d| root.join(d).display().to_string()).collect();
        dirs.sort();
        dirs.dedup();
        dirs
    }

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
    /// run.sh of Steam's runtime for older games, if installed.
    pub old_runtime: Option<PathBuf>,
    /// Missing libraries only optional plugins need: what is lost, and the libraries.
    pub harmless: Vec<(&'static str, Vec<String>)>,
}

/// Plugins the game starts without when they can't load, and what is lost then.
const OPTIONAL_PLUGINS: &[(&str, &str)] = &[
    ("ScreenSelector.so", "Unity's start-up resolution window"),
    ("libcoreclrtraceptprovider.so", ".NET's developer tracing"),
    ("yad", "GOG's error-message window"),
];

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
                None if !scan::not_the_game(&f.path) && !(pe.dotnet && game.main.is_some()) => windows.push((f.path.clone(), pe.bits)),
                None => {}
            }
        }
    }
    let mut problems = Vec::new();
    let mut report = Report { problems: Vec::new(), libs_checked: 0, windows, installers, glibc: system.glibc, packed: false, old_runtime: system.old_runtime.as_ref().map(|r| r.run.clone()), harmless: Vec::new() };

    let Some((main, elf)) = game.main() else {
        match game.portable.map(|i| &game.files[i]) {
            Some(f) if matches!(f.binary, Binary::Love) => problems.push(Problem::Love { file: f.path.clone(), version: game.engine.as_ref().and_then(|e| e.version.clone()) }),
            Some(f) if matches!(f.binary, Binary::Jar) => problems.push(Problem::Java { file: f.path.clone(), java: system.java() }),
            Some(f) => {
                let python = game.files.iter().filter_map(|f| match &f.binary {
                    Binary::Python(p) => Some(p),
                    _ => None,
                });
                let (v2, pygame) = python.fold((false, false), |(v2, pg), p| (v2 || p.v2, pg || p.pygame));
                problems.push(Problem::Python { file: f.path.clone(), v2, pygame });
            }
            None => problems.push(match game.archive {
                Some(i) => Problem::Archive(game.files[i].path.clone()),
                None => Problem::NoLinuxBuild,
            }),
        }
        report.problems = problems;
        return report;
    };
    if elf.machine != EM_X86_64 && elf.machine != EM_386 {
        problems.push(Problem::WrongMachine(elf.machine));
    }
    report.packed = elf.packed;
    let mode = std::fs::metadata(game.full(main)).map(|m| m.permissions().mode()).unwrap_or(0);
    if mode & 0o100 == 0 {
        problems.push(Problem::NotExecutable(main.path.clone()));
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
    let main_rpath = if elf.runpath.is_empty() { expand(&elf.rpath, &main_dir) } else { Vec::new() };
    let search = |path: &Path, e: &Elf| -> Vec<PathBuf> {
        let dir = game.root.join(path.parent().unwrap_or(Path::new("")));
        if e.runpath.is_empty() { expand(&e.rpath, &dir).into_iter().chain(main_rpath.iter().cloned()).collect() } else { expand(&e.runpath, &dir) }
    };
    // A launcher script next to the program or above it may point LD_LIBRARY_PATH anywhere in
    // the folder; it isn't evaluated.
    let launcher_path = game.files.iter().any(|f| {
        matches!(f.binary, Binary::Script) && f.path.parent().is_some_and(|d| main.path.starts_with(d)) && binary::contains(&game.full(f), b"LD_LIBRARY_PATH")
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
            // ld.so gets the game's copy through the search path or a launcher's LD_LIBRARY_PATH.
            let loads_copy = launcher_path || search(path, e).iter().any(|d| d.join(lib).is_file());
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
            if reachable(path, e, lib) || (copy.is_some() && (!at_start || launcher_path)) {
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
    // A library missing at start and later is a start problem.
    later.retain(|lib, _| !missing.contains_key(lib));
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

    // glibc only refuses to make the stack executable for libraries loaded later; if the program
    // already asks for one, these load fine too.
    let exec_stack: Vec<PathBuf> = same.iter().filter(|(p, e)| !e.program && e.exec_stack && !startup.contains(p)).map(|(p, _)| (*p).clone()).collect();
    if !exec_stack.is_empty() && !elf.exec_stack && system.refuses_exec_stack() {
        problems.push(Problem::ExecStack(exec_stack));
    }

    let clashes = symbol_clashes(game, &main.path, &same);
    if !clashes.is_empty() {
        problems.push(Problem::SymbolClash(clashes));
    }
    if !elf.needed.iter().any(|l| l == "libstdc++.so.6") && binary::symbols(&game.full(main)).is_some_and(|s| s.defined.iter().any(|d| d.starts_with("_ZNSt8ios_base4Init"))) {
        problems.push(Problem::OwnLibstdcxx(system.path("libstdc++.so.6", elf.bits).map(String::from)));
    }
    if let Some(version) = old_dotnet(&main_dir, &main.path) {
        problems.push(Problem::DotnetIcu(version));
    }
    if game.engine.as_ref().is_some_and(|e| e.name == "NW.js") {
        let lib = [main_dir.join("lib/libnw.so"), game.full(main)].into_iter().find(|p| p.is_file());
        if let Some(version) = lib.and_then(|l| chromium_version(&l)).filter(|v| v.split('.').next().and_then(|m| m.parse::<u32>().ok()).is_some_and(|m| (56..=90).contains(&m))) {
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
fn old_dotnet(dir: &Path, main: &Path) -> Option<String> {
    if !dir.join("libhostfxr.so").is_file() {
        return None;
    }
    let stem = main.file_name()?.to_string_lossy().into_owned();
    let config = std::fs::read_to_string(dir.join(format!("{stem}.runtimeconfig.json"))).unwrap_or_default();
    if config.contains("System.Globalization.Invariant\":true") || config.contains("System.Globalization.Invariant\": true") {
        return None;
    }
    // "runtimeTarget": {"name": ".NETCoreApp,Version=v2.1/linux-x64", ...}
    let deps = std::fs::read_to_string(dir.join(format!("{stem}.deps.json"))).ok()?;
    let at = deps.find(".NETCoreApp,Version=v")? + ".NETCoreApp,Version=v".len();
    let version: String = deps[at..].chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
    let major: u32 = version.split('.').next()?.parse().ok()?;
    (major < 5).then_some(version)
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
/// Relative entries would depend on where the game is started from, so they're left out.
fn expand(dirs: &[String], origin: &Path) -> Vec<PathBuf> {
    let origin = origin.to_string_lossy();
    dirs.iter().map(|d| PathBuf::from(d.replace("${ORIGIN}", &origin).replace("$ORIGIN", &origin))).filter(|d| d.is_absolute()).collect()
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
        } else if let Some(Binary::Pe(pe)) = game.windows.map(|i| &game.files[i].binary) {
            let engine = game.engine.as_ref().map(|e| format!(", {}{}", e.name, e.version.as_ref().map(|v| format!(" {v}")).unwrap_or_default())).unwrap_or_default();
            let _ = writeln!(out, "  Runs:  {} (Windows, {}-bit{engine})", game.files[game.windows.unwrap()].path.display(), pe.bits);
        }
        if let Some(f) = game.portable.filter(|_| game.main().is_none()).map(|i| &game.files[i]) {
            let what = match (&f.binary, &game.engine) {
                (Binary::Love, Some(e)) => format!("LÖVE game{}", e.version.as_ref().map(|v| format!(", made for LÖVE {v}")).unwrap_or_default()),
                (Binary::Python(p), _) if p.v2 => "Python 2 game".into(),
                (Binary::Jar, _) => "Java game".into(),
                _ => "Python game".into(),
            };
            let _ = writeln!(out, "  Runs:  {} ({what})", f.path.display());
        }
        if !self.windows.is_empty() && game.windows.is_none() {
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

        if self.packed {
            let _ = writeln!(out, "• Its program is packed (UPX), so the libraries it needs can't be read until it runs.");
            if self.old_runtime.is_some() {
                let _ = writeln!(out, "  Steam's runtime for older games has most libraries old games miss; gamefix setup");
                let _ = writeln!(out, "  starts it through that in case.");
            }
            let _ = writeln!(out);
        }
        if self.problems.is_empty() {
            let _ = writeln!(out, "No {}problems found in its files ({} libraries checked).", if self.packed { "other " } else { "" }, self.libs_checked);
            let _ = writeln!(out, "If it still doesn't start, the next step is running it and reading what it prints.");
            return out;
        }
        for problem in &self.problems {
            match problem {
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
                Problem::NoLinuxBuild => {
                    let _ = writeln!(out, "✗ There is no Linux version of this game.");
                    if let Some(i) = game.windows {
                        let _ = writeln!(out, "    Fix: run {} with Proton: gamefix setup --steam adds it to Steam that way.", game.files[i].path.display());
                    } else if !self.installers.is_empty() {
                        let _ = writeln!(out, "    It's an installer: run it with Proton first, then check the folder it installs to.");
                    }
                }
                Problem::Love { file, version } => {
                    let _ = writeln!(out, "✗ This is a LÖVE game: {} is run by the LÖVE program, which isn't part of the system.", file.display());
                    // LÖVE 11 changed its API; 0.x games mostly fail on it (0.10.1: crashes loading sounds).
                    if version.as_deref().is_some_and(|v| v.starts_with("0.")) {
                        let _ = writeln!(out, "  It was made for LÖVE {}, and the LÖVE you can install (11, on Flathub) can't run", version.as_deref().unwrap_or_default());
                        let _ = writeln!(out, "  games made for 0.10 or older.");
                        let _ = writeln!(out, "    Fix: if the game has a Windows download, use that with Proton (it brings its own LÖVE).");
                    } else {
                        let _ = writeln!(out, "    Fix: install LÖVE from Flathub (org.love2d.love2d) and open the .love file with it.");
                    }
                }
                Problem::Python { file, v2: true, .. } => {
                    let _ = writeln!(out, "✗ This is a Python 2 game ({}). Python 2 was retired in 2020 and current", file.display());
                    let _ = writeln!(out, "  Linux systems don't include it anymore, so it can't run here.");
                    let _ = writeln!(out, "    Fix: if the game has a Windows download, use that with Proton.");
                }
                Problem::Python { file, pygame, .. } => {
                    let _ = writeln!(out, "✗ This is a Python game ({}): it needs Python 3{} to run.", file.display(), if *pygame { " with pygame" } else { "" });
                    if *pygame {
                        let _ = writeln!(out, "    pygame isn't part of the system; if the game has a Windows download, that with Proton is simpler.");
                    }
                }
                Problem::WrongMachine(m) => {
                    let _ = writeln!(out, "✗ The program is built for a different processor (ELF machine {m}), not a PC.");
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
                        Some(run) if !in_old_runtime.is_empty() => {
                            let _ = writeln!(out, "  Steam's runtime for older games keeps libraries newer systems dropped{}.", if in_old_runtime.len() < libs.len() + later.len() { " (marked)" } else { ", all of these" });
                            let _ = writeln!(out, "    Fix (at launch): start it through {}", run.display());
                            let _ = writeln!(out, "    Started from Steam it gets them by itself; gamefix setup does it for the app-menu entry.");
                        }
                        Some(_) => {}
                        None => {
                            let _ = writeln!(out, "  Steam's runtime for older games often has libraries like these, but Steam isn't installed.");
                        }
                    }
                    if !misplaced.is_empty() {
                        let _ = writeln!(out, "  The game ships some itself (marked), in a folder Linux doesn't look in for them.");
                        let _ = writeln!(out, "    Fix (at launch): LD_LIBRARY_PATH={}", Problem::library_path(misplaced, &game.root).join(":"));
                    }
                    let unfixable: Vec<&str> = libs.keys().filter(|l| !in_old_runtime.contains(*l) && !misplaced.contains_key(*l)).map(String::as_str).collect();
                    if !unfixable.is_empty() && self.old_runtime.is_some() {
                        let _ = writeln!(out, "  Nothing here has {}.", unfixable.join(", "));
                    }
                }
                Problem::Java { file, java: None } => {
                    let _ = writeln!(out, "✗ This is a Java game ({}): it runs with \"java -jar\", and Java isn't installed.", file.display());
                    let _ = writeln!(out, "    Fix: on Bazzite, run \"brew install openjdk\" in a terminal; then gamefix setup adds it to the app menu.");
                }
                Problem::Java { file, java: Some(java) } => {
                    let _ = writeln!(out, "• This is a Java game; Java is installed, so it runs with: {} -jar \"{}\"", java.display(), file.display());
                    let _ = writeln!(out, "  gamefix setup adds that to the app menu.");
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
                    let _ = writeln!(out, "    Fix: start it where it can't see other programs (a private process namespace),");
                    let _ = writeln!(out, "    or run the Windows build with Proton.");
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
    fn origin_means_the_files_folder() {
        let dirs = ["$ORIGIN/../lib".to_string(), "${ORIGIN}".to_string(), "lib".to_string(), "/opt/x".to_string()];
        let out = expand(&dirs, Path::new("/g/bin"));
        assert_eq!(out, [PathBuf::from("/g/bin/../lib"), PathBuf::from("/g/bin"), PathBuf::from("/opt/x")]);
    }
}
