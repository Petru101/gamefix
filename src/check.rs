//! Turns what the scan found into problems, each with what it means and how to fix it.

use crate::binary::{self, Binary, Elf, EM_386, EM_X86_64};
use crate::scan::{self, Game};
use crate::system::System;
use std::collections::{BTreeMap, HashSet};
use std::fmt::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

pub enum Problem {
    NoLinuxBuild,
    WrongMachine(u16),
    NotExecutable(PathBuf),
    /// Library name → the game's files that need it.
    MissingLibs { bits: u8, libs: BTreeMap<String, Vec<PathBuf>> },
    ExecStack(Vec<PathBuf>),
    /// Unity's player carries its own FMOD and exports it, so the game's FMOD Studio library
    /// binds to that one and refuses the version: no sound. Holds the libraries to preload.
    FmodClash(Vec<PathBuf>),
    /// Unity before 5.4: its single-instance check mistakes other programs for the game.
    UnitySingleInstance,
}

pub struct Report {
    pub problems: Vec<Problem>,
    pub libs_checked: usize,
    pub windows: Vec<(PathBuf, u8)>,
    pub installers: Vec<(PathBuf, &'static str)>,
    pub glibc: Option<(u32, u32)>,
    /// Missing libraries only optional plugins need: what is lost, and the libraries.
    pub harmless: Vec<(&'static str, Vec<String>)>,
}

/// Plugins the game starts without when they can't load, and what is lost then.
const OPTIONAL_PLUGINS: &[(&str, &str)] = &[
    ("ScreenSelector.so", "Unity's start-up resolution window"),
    ("libcoreclrtraceptprovider.so", ".NET's developer tracing"),
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
                None if !scan::not_the_game(&f.path) => windows.push((f.path.clone(), pe.bits)),
                None => {}
            }
        }
    }
    let mut problems = Vec::new();
    let mut report = Report { problems: Vec::new(), libs_checked: 0, windows, installers, glibc: system.glibc, harmless: Vec::new() };

    let Some((main, elf)) = game.main() else {
        problems.push(Problem::NoLinuxBuild);
        report.problems = problems;
        return report;
    };
    if elf.machine != EM_X86_64 && elf.machine != EM_386 {
        problems.push(Problem::WrongMachine(elf.machine));
    }
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
    let mut missing: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
    let mut optional: BTreeMap<&'static str, Vec<String>> = BTreeMap::new();
    for (path, e) in &same {
        for lib in &e.needed {
            if system.has(lib, elf.bits) || bundled.contains(std::ffi::OsStr::new(lib)) {
                continue;
            }
            match optional_plugin(path) {
                Some(what) => optional.entry(what).or_default().push(lib.clone()),
                None => missing.entry(lib.clone()).or_default().push((*path).clone()),
            }
        }
    }
    report.libs_checked = same.iter().filter(|(_, e)| !e.program).count();
    report.harmless = optional.into_iter().collect();
    if !missing.is_empty() {
        problems.push(Problem::MissingLibs { bits: elf.bits, libs: missing });
    }

    // glibc only refuses to make the stack executable later; if the program already asks for
    // one, these libraries load fine.
    let exec_stack: Vec<PathBuf> = same.iter().filter(|(_, e)| !e.program && e.exec_stack).map(|(p, _)| (*p).clone()).collect();
    if !exec_stack.is_empty() && !elf.exec_stack && system.refuses_exec_stack() {
        problems.push(Problem::ExecStack(exec_stack));
    }

    if let Some(libs) = fmod_clash(game, &same) {
        problems.push(Problem::FmodClash(libs));
    }

    let old_unity = game.engine.as_ref().filter(|e| e.name == "Unity").and_then(|e| e.version.as_deref()).and_then(|v| {
        let mut parts = v.split('.');
        Some((parts.next()?.parse::<u32>().ok()?, parts.next()?.parse::<u32>().ok()?))
    });
    if old_unity.is_some_and(|v| v < (5, 4)) {
        problems.push(Problem::UnitySingleInstance);
    }

    report.problems = problems;
    report
}

/// FMOD Studio resolves its core calls (FMOD::System::getVersion first) through the global
/// symbol scope, where UnityPlayer.so, loaded before any plugin, already exports Unity's own FMOD.
fn fmod_clash(game: &Game, same: &[(&PathBuf, &Elf)]) -> Option<Vec<PathBuf>> {
    const PROBE: &[u8] = b"_ZN4FMOD6System10getVersionEPj";
    let find = |name: &str| same.iter().find(|(p, _)| p.file_name().is_some_and(|n| n == name)).map(|(p, _)| (*p).clone());
    let player = find("UnityPlayer.so")?;
    let studio = find("libfmodstudio.so")?;
    let core = find("libfmod.so")?;
    let full = |p: &PathBuf| game.root.join(p);
    (binary::contains(&full(&player), PROBE) && binary::contains(&full(&studio), PROBE)).then(|| vec![core, studio])
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
        }
        if !self.windows.is_empty() {
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

        if self.problems.is_empty() {
            let _ = writeln!(out, "No problems found in its files ({} libraries checked).", self.libs_checked);
            let _ = writeln!(out, "If it still doesn't start, the next step is running it and reading what it prints.");
            return out;
        }
        for problem in &self.problems {
            match problem {
                Problem::NoLinuxBuild => {
                    let _ = writeln!(out, "✗ No Linux program found in this folder.");
                    if !self.windows.is_empty() {
                        let _ = writeln!(out, "    Fix: run the Windows build with Proton.");
                    } else if !self.installers.is_empty() {
                        let _ = writeln!(out, "    It's an installer: run it with Proton first, then check the folder it installs to.");
                    }
                }
                Problem::WrongMachine(m) => {
                    let _ = writeln!(out, "✗ The program is built for a different processor (ELF machine {m}), not a PC.");
                }
                Problem::NotExecutable(p) => {
                    let _ = writeln!(out, "✗ {} isn't marked as a program, so double-clicking it does nothing.", p.display());
                    let _ = writeln!(out, "    Usually lost when unpacking a zip. Fix (once): chmod +x \"{}\"", p.display());
                }
                Problem::MissingLibs { bits, libs } => {
                    let _ = writeln!(out, "✗ {} {bits}-bit librar{} the game needs {} not installed:", libs.len(), if libs.len() == 1 { "y" } else { "ies" }, if libs.len() == 1 { "is" } else { "are" });
                    for (lib, users) in libs {
                        let _ = writeln!(out, "    {lib}, needed by:");
                        list(&mut out, users);
                    }
                    let _ = writeln!(out, "    Fix: run it in the Steam Linux Runtime, which brings its own libraries.");
                    let _ = writeln!(out, "    If only a plugin needs it, the game may run without it (that feature won't work).");
                }
                Problem::ExecStack(paths) => {
                    let (a, b) = self.glibc.unwrap_or_default();
                    let _ = writeln!(out, "✗ {} librar{} won't load: {} built asking for an \"executable stack\",", paths.len(), if paths.len() == 1 { "y" } else { "ies" }, if paths.len() == 1 { "it was" } else { "they were" });
                    let _ = writeln!(out, "  which this system's glibc ({a}.{b}) refuses for security (since 2.41). Nothing shows on");
                    let _ = writeln!(out, "  screen; whatever uses it (often the sound) just doesn't work:");
                    list(&mut out, paths);
                    let _ = writeln!(out, "    Fix (at launch): GLIBC_TUNABLES=glibc.rtld.execstack=2");
                }
                Problem::FmodClash(libs) => {
                    let _ = writeln!(out, "✗ The game's sound (FMOD) can't start: Unity's player has its own, older FMOD built in,");
                    let _ = writeln!(out, "  and the game's FMOD library ends up talking to that copy and refuses the version.");
                    let preload: Vec<String> = libs.iter().map(|p| game.root.join(p).display().to_string()).collect();
                    let _ = writeln!(out, "    Fix (at launch): LD_PRELOAD={}", preload.join(":"));
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
