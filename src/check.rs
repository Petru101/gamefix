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
    /// A .love file: needs the LÖVE version it was made for.
    Love { file: PathBuf, version: Option<String> },
    /// Python code: needs Python (2 is gone from current systems) and maybe pygame.
    Python { file: PathBuf, v2: bool, pygame: bool },
    WrongMachine(u16),
    NotExecutable(PathBuf),
    /// Library name → the game's files that need it; and which of them Steam's runtime for
    /// older games has.
    MissingLibs { bits: u8, libs: BTreeMap<String, Vec<PathBuf>>, in_old_runtime: BTreeSet<String> },
    ExecStack(Vec<PathBuf>),
    /// Libraries that get another library's functions of the same name instead of the ones
    /// they were built against (Final Shot: Unity's built-in FMOD answering for the game's).
    SymbolClash(Vec<Clash>),
    /// Unity before 5.4: its single-instance check mistakes other programs for the game.
    UnitySingleInstance,
    /// H.264 videos in a Unity Linux build: its player only decodes VP8 there.
    UnityH264(Vec<PathBuf>),
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
            Some(f) => {
                let python = game.files.iter().filter_map(|f| match &f.binary {
                    Binary::Python(p) => Some(p),
                    _ => None,
                });
                let (v2, pygame) = python.fold((false, false), |(v2, pg), p| (v2 || p.v2, pg || p.pygame));
                problems.push(Problem::Python { file: f.path.clone(), v2, pygame });
            }
            None => problems.push(Problem::NoLinuxBuild),
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
    let bundled: HashSet<&std::ffi::OsStr> = same.iter().filter_map(|(p, _)| p.file_name()).collect();
    // The program's DT_RPATH also applies to everything it loads that has no DT_RUNPATH.
    let main_dir = game.root.join(main.path.parent().unwrap_or(Path::new("")));
    let main_rpath = if elf.runpath.is_empty() { expand(&elf.rpath, &main_dir) } else { Vec::new() };
    let mut missing: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
    let mut optional: BTreeMap<&'static str, Vec<String>> = BTreeMap::new();
    for (path, e) in &same {
        let dir = game.root.join(path.parent().unwrap_or(Path::new("")));
        let search: Vec<PathBuf> = if e.runpath.is_empty() { expand(&e.rpath, &dir).into_iter().chain(main_rpath.iter().cloned()).collect() } else { expand(&e.runpath, &dir) };
        for lib in &e.needed {
            if system.has(lib, elf.bits) || bundled.contains(std::ffi::OsStr::new(lib)) || search.iter().any(|d| d.join(lib).is_file()) {
                continue;
            }
            match optional_plugin(path).or_else(|| java_window_part(path, lib)) {
                Some(what) => optional.entry(what).or_default().push(lib.clone()),
                None => missing.entry(lib.clone()).or_default().push((*path).clone()),
            }
        }
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
    if !missing.is_empty() {
        let in_old_runtime = missing.keys().filter(|l| system.old_runtime.as_ref().is_some_and(|r| r.has(l, elf.bits))).cloned().collect();
        problems.push(Problem::MissingLibs { bits: elf.bits, libs: missing, in_old_runtime });
    }

    // glibc only refuses to make the stack executable later; if the program already asks for
    // one, these libraries load fine.
    let exec_stack: Vec<PathBuf> = same.iter().filter(|(_, e)| !e.program && e.exec_stack).map(|(p, _)| (*p).clone()).collect();
    if !exec_stack.is_empty() && !elf.exec_stack && system.refuses_exec_stack() {
        problems.push(Problem::ExecStack(exec_stack));
    }

    let clashes = symbol_clashes(game, &main.path, &same);
    if !clashes.is_empty() {
        problems.push(Problem::SymbolClash(clashes));
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
                Problem::MissingLibs { bits, libs, in_old_runtime } => {
                    let _ = writeln!(out, "✗ {} {bits}-bit librar{} the game needs {} not installed:", libs.len(), if libs.len() == 1 { "y" } else { "ies" }, if libs.len() == 1 { "is" } else { "are" });
                    let mixed = !in_old_runtime.is_empty() && in_old_runtime.len() < libs.len();
                    for (lib, users) in libs {
                        let mark = if mixed && in_old_runtime.contains(lib) { " (Steam's runtime has it)" } else { "" };
                        let _ = writeln!(out, "    {lib}{mark}, needed by:");
                        list(&mut out, users);
                    }
                    match &self.old_runtime {
                        Some(run) if !in_old_runtime.is_empty() => {
                            let which = if mixed { "Some of them are" } else if libs.len() == 1 { "It's" } else { "They're all" };
                            let _ = writeln!(out, "  {which} in Steam's runtime for older games, where Steam keeps libraries newer systems dropped.");
                            let _ = writeln!(out, "    Fix (at launch): start it through {}", run.display());
                            let _ = writeln!(out, "    Started from Steam it gets them by itself; gamefix setup does it for the app-menu entry.");
                            if mixed {
                                let _ = writeln!(out, "  The others aren't there either.");
                            }
                        }
                        Some(_) => {
                            let _ = writeln!(out, "  Steam's runtime for older games doesn't have {} either.", if libs.len() == 1 { "it" } else { "them" });
                        }
                        None => {
                            let _ = writeln!(out, "  Steam's runtime for older games often has libraries like these, but Steam isn't installed.");
                        }
                    }
                    let _ = writeln!(out, "    If only a plugin needs one, the game may run without it (that feature won't work).");
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
                    let _ = writeln!(out, "✗ Its videos (often the cutscenes) may stay black: they look like H.264, which Unity's");
                    let _ = writeln!(out, "  Linux player can't decode (it plays VP8/WebM only). The game itself still runs:");
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
