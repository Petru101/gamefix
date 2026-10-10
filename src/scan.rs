//! Walks a game folder: finds its programs, libraries and Windows builds, picks the program to
//! run and recognises the engine.

use crate::binary::{self, Binary, EM_386, EM_X86_64};
use std::fs;
use std::path::{Path, PathBuf};

const MAX_DEPTH: usize = 8;
const MAX_FILES: usize = 50_000;

/// Programs that ship next to games but aren't the game.
const NOT_THE_GAME: &[&str] = &["crashhandler", "crashpad", "createdump", "crashreport", "chrome-sandbox", "unins", "redist", "vc_redist", "dxsetup", "steamwebhelper", "quicksfv", "nacl_helper", "nwjc", "chromedriver", "minidump_stackwalk"];

pub struct Found {
    /// Relative to the game's root.
    pub path: PathBuf,
    pub binary: Binary,
}

pub struct Engine {
    pub name: &'static str,
    pub version: Option<String>,
}

pub struct Game {
    pub root: PathBuf,
    pub name: String,
    pub files: Vec<Found>,
    /// Index into `files` of the Linux program to run.
    pub main: Option<usize>,
    /// Index into `files` of the Windows program to run, when there's no Linux one.
    pub windows: Option<usize>,
    /// Index into `files` of a game another program runs (a .love file, Python code), when
    /// there's no Linux program.
    pub portable: Option<usize>,
    /// Index into `files` of an archive the game may still be packed in, when there's nothing
    /// else to run.
    pub archive: Option<usize>,
    pub engine: Option<Engine>,
}

impl Game {
    /// A script next to the program or in a folder above it that starts it the way the
    /// developer meant: setting LD_LIBRARY_PATH (Gravitus Minus), handing it one of the game's
    /// files (Quick Space Dungeon), or, from a folder above, starting it by its path there
    /// (Dungeon in my Pocket: `./bin/<name>` from the folder holding data.pck; AGS ports:
    /// `$scriptdir/data/ags64 ... $scriptdir/data/` with ALLEGRO_MODULES). Not just any script
    /// naming it: MonoKickstart's `Kick` scripts next to it break on spaces (Mario is Doomed).
    pub fn launcher(&self) -> Option<&Found> {
        let (main, _) = self.main()?;
        let name = main.path.file_name()?.to_string_lossy().into_owned();
        let main_dir = main.path.parent().unwrap_or(Path::new(""));
        self.files.iter().find(|f| {
            let Some(dir) = f.path.parent() else { return false };
            matches!(f.binary, Binary::Script)
                && main.path.starts_with(dir)
                && ((binary::sets_library_path(&self.full(f)) && binary::contains(&self.full(f), name.as_bytes()))
                    || binary::passes_file(&self.full(f), &name)
                    || (dir != main_dir && binary::contains(&self.full(f), format!("/{name}").as_bytes())))
                // Not one starting it through a bundled ld.so (Exodus: Liberation Circuit), which
                // setup bypasses on purpose.
                && !binary::contains(&self.full(f), b"--library-path")
        })
    }

    pub fn main(&self) -> Option<(&Found, &binary::Elf)> {
        let found = &self.files[self.main?];
        match &found.binary {
            Binary::Elf(elf) => Some((found, elf)),
            _ => None,
        }
    }

    pub fn full(&self, found: &Found) -> PathBuf {
        self.root.join(&found.path)
    }
}

pub fn scan(path: &Path) -> Option<Game> {
    // Absolute, so what setup writes works from anywhere.
    let path = &fs::canonicalize(path).ok()?;
    let meta = fs::metadata(path).ok()?;
    let (root, chosen) = if meta.is_dir() {
        (path.to_path_buf(), None)
    } else {
        (path.parent()?.to_path_buf(), Some(PathBuf::from(path.file_name()?)))
    };
    let mut files = Vec::new();
    match &chosen {
        // A loose download among others: the file is the whole game.
        Some(c) if shared_folder(&root) => {
            if let Some(binary) = binary::identify(&root.join(c)) {
                files.push(Found { path: c.clone(), binary });
            }
        }
        _ => walk(&root, Path::new(""), 0, &mut files),
    }

    let picked = |want_elf: bool| {
        let c = chosen.as_ref()?;
        files.iter().position(|f| &f.path == c && matches!((&f.binary, want_elf), (Binary::Elf(_), true) | (Binary::Pe(_), false)))
    };
    let main = if chosen.is_some() { picked(true) } else { pick_main(&root, &files) };
    let windows = match main {
        Some(_) => None,
        None if chosen.is_some() => picked(false),
        None => pick_windows(&root, &files),
    };
    let portable = match main {
        Some(_) => None,
        None if chosen.is_some() => chosen.as_ref().and_then(|c| files.iter().position(|f| &f.path == c && matches!(f.binary, Binary::Love | Binary::Jar | Binary::Flash | Binary::Python(_)))),
        None => pick_portable(&root, &files),
    };
    let archive = (main.is_none() && windows.is_none() && portable.is_none())
        .then(|| files.iter().enumerate().filter(|(_, f)| matches!(f.binary, Binary::Archive)).min_by_key(|(_, f)| f.path.components().count()).map(|(i, _)| i))
        .flatten();
    let program = main.or(windows);
    let engine = match portable {
        Some(i) if matches!(files[i].binary, Binary::Love) => Some(Engine { name: "LÖVE", version: love_version(&root.join(&files[i].path)) }),
        _ => program.and_then(|i| engine(&root, &files[i].path, &files)),
    };
    let name = program
        .and_then(|i| unity_product(&root, &files[i].path))
        .or_else(|| chosen.as_ref().filter(|_| shared_folder(&root)).map(|c| stem(c)))
        .or_else(|| root.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| root.display().to_string());
    Some(Game { root, name, files, main, windows, portable, archive, engine })
}

/// Folders that hold unrelated downloads rather than one game.
fn shared_folder(dir: &Path) -> bool {
    let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    let dir = fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    ["", "Downloads", "Desktop", "Documents"].iter().any(|d| fs::canonicalize(home.join(d)).is_ok_and(|h| h == dir))
}

fn walk(root: &Path, rel: &Path, depth: usize, out: &mut Vec<Found>) {
    let Ok(entries) = fs::read_dir(root.join(rel)) else { return };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        if out.len() >= MAX_FILES {
            return;
        }
        let path = rel.join(entry.file_name());
        let Ok(kind) = entry.file_type() else { continue };
        // A link out of the folder (an unpacked AppImage has some) would read the system's files.
        if kind.is_symlink() && fs::read_link(entry.path()).is_ok_and(|t| t.is_absolute()) {
            continue;
        }
        if kind.is_dir() {
            if depth < MAX_DEPTH {
                walk(root, &path, depth + 1, out);
            }
        } else if let Some(binary) = binary::identify(&root.join(&path)) {
            let is_exe = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("exe"));
            // Windows DLLs aren't interesting, and scripts only near the top, where launchers live.
            let keep = match &binary {
                Binary::Pe(_) => is_exe,
                Binary::Script => depth <= 2,
                Binary::Love | Binary::Jar | Binary::Flash | Binary::Python(_) | Binary::Archive | Binary::Package(_) => depth <= 1,
                Binary::Elf(_) | Binary::Damaged => true,
            };
            if keep {
                out.push(Found { path, binary });
            }
        }
    }
}

fn stem(path: &Path) -> String {
    path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

pub fn not_the_game(path: &Path) -> bool {
    let name = path.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    NOT_THE_GAME.iter().any(|n| name.contains(n))
}

fn pick_main(root: &Path, files: &[Found]) -> Option<usize> {
    files
        .iter()
        .enumerate()
        .filter_map(|(i, f)| {
            let Binary::Elf(elf) = &f.binary else { return None };
            // glibc's libc.so.6 has an interpreter too; a bundle's own (Exodus) isn't the game.
            let name = f.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            // Nor is a bundled Java (a Processing export's java/bin/java) or anything else in it
            // (Age of Conquest IV's jre/bin/jjs, jre/lib/jexec): a JRE has `release` and bin/java.
            let jre_tool = f.path.ancestors().skip(1).any(|j| root.join(j).join("release").is_file() && root.join(j).join("bin/java").is_file());
            if !elf.program || not_the_game(&f.path) || (name.starts_with("lib") && name.contains(".so")) || name.starts_with("ld-linux") || (name == "java" && f.path.parent().is_some_and(|d| d.ends_with("bin"))) || jre_tool {
                return None;
            }
            let dir = root.join(f.path.parent().unwrap_or(Path::new("")));
            let s = stem(&f.path);
            let mut score = 0;
            if dir.join(format!("{s}_Data")).is_dir() || dir.join(format!("{s}.pck")).is_file() || dir.join("data.pck").is_file() || dir.join("package.nw").exists() {
                score += 10;
            }
            if f.path.components().count() == 1 {
                score += 3;
            }
            if elf.machine == EM_X86_64 {
                score += 2;
            } else if elf.machine == EM_386 {
                score += 1;
            }
            // A dedicated server next to the game (TACS: server.exe and tacs.exe, both Linux).
            let lower = s.to_lowercase();
            if lower == "server" || lower.ends_with("_server") || lower.ends_with("-server") || lower.contains("dedicated") {
                score -= 5;
            }
            Some((score, i))
        })
        .max_by_key(|&(score, i)| (score, std::cmp::Reverse(i)))
        .map(|(_, i)| i)
}

/// The Windows game program: not an installer, helper or .NET part of a Linux build.
fn pick_windows(root: &Path, files: &[Found]) -> Option<usize> {
    let folder = root.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    files
        .iter()
        .enumerate()
        .filter_map(|(i, f)| {
            let Binary::Pe(pe) = &f.binary else { return None };
            if pe.installer.is_some() || not_the_game(&f.path) {
                return None;
            }
            let dir = root.join(f.path.parent().unwrap_or(Path::new("")));
            let s = stem(&f.path);
            let mut score = 0;
            if dir.join(format!("{s}_Data")).is_dir() {
                score += 10;
            }
            if f.path.components().count() == 1 {
                score += 3;
            }
            if folder.contains(&s.to_lowercase()) || s.to_lowercase().contains(&folder) {
                score += 2;
            }
            if pe.bits == 64 {
                score += 1;
            }
            Some((score, i))
        })
        .max_by_key(|&(score, i)| (score, std::cmp::Reverse(i)))
        .map(|(_, i)| i)
}

/// Whether a script is a Processing export's launcher: next to lib/core.jar.
pub fn processing_script(root: &Path, path: &Path) -> bool {
    root.join(path.parent().unwrap_or(Path::new(""))).join("lib/core.jar").is_file()
}

/// A Processing export's own Java (java/bin/java next to its script), or an install4j
/// launcher's (jre/bin/java), if it brings one.
pub fn processing_java(root: &Path, path: &Path) -> Option<PathBuf> {
    let dir = root.join(path.parent().unwrap_or(Path::new("")));
    ["java/bin/java", "jre/bin/java"].iter().map(|j| dir.join(j)).find(|j| j.is_file())
}

/// An install4j launcher script: it finds a Java (the bundled jre/ first) and starts the game
/// with it (Age of Conquest IV: no game program, only the JRE's own tools).
pub fn install4j_script(root: &Path, path: &Path) -> bool {
    binary::contains(&root.join(path), b"INSTALL4J")
}

/// What a Processing export needs to know about its Java: built on Processing 2 or older
/// (PApplet is a java.applet.Applet, which Java 26 removed), only 32-bit native parts, and
/// only ARM ones (application.linux-armv6hf, for the Raspberry Pi: Fairy Piano's sound library
/// fails with "can't load ARM .so on a AMD 64 platform").
pub fn processing_notes(root: &Path, path: &Path) -> (bool, bool, bool) {
    let lib = root.join(path.parent().unwrap_or(Path::new(""))).join("lib");
    let class = std::process::Command::new("unzip").arg("-p").arg(lib.join("core.jar")).arg("processing/core/PApplet.class").output().map(|o| o.stdout).unwrap_or_default();
    let applet = class.windows(16).any(|w| w == b"java/applet/Appl");
    let names: Vec<String> = fs::read_dir(&lib).into_iter().flatten().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    let only32 = names.iter().any(|n| n.contains("natives-linux-i586")) && !names.iter().any(|n| n.contains("natives-linux-amd64"));
    let x86 = names.iter().any(|n| n.contains("natives-linux-amd64") || n.contains("natives-linux-i586"));
    let arm = !x86 && (path.to_string_lossy().contains("linux-arm") || path.to_string_lossy().contains("linux-aarch64") || names.iter().any(|n| n.contains("natives-linux-arm") || n.contains("natives-linux-aarch64")));
    (applet, only32, arm)
}

/// A .love file, else a .jar, else the Python file that looks like the game's start: named like
/// the folder or main/game/run, preferring ones that use pygame. Nearest the top first.
fn pick_portable(root: &Path, files: &[Found]) -> Option<usize> {
    // A Processing export: a script starting Java next to lib/core.jar (application.linux64 etc.,
    // the 64-bit one first).
    let processing = files
        .iter()
        .enumerate()
        .filter(|(_, f)| matches!(f.binary, Binary::Script) && processing_script(root, &f.path))
        .min_by_key(|(_, f)| (!f.path.to_string_lossy().contains("linux64"), f.path.to_string_lossy().contains("linux-a"), f.path.components().count()));
    if let Some((i, _)) = processing {
        return Some(i);
    }
    // Then an install4j launcher; when there are two (Age of Conquest IV: "Age of Conquest IV"
    // and "conquest"), the one with the longer name, usually the game's full name.
    let install4j = files.iter().enumerate().filter(|(_, f)| matches!(f.binary, Binary::Script) && install4j_script(root, &f.path)).max_by_key(|(_, f)| f.path.file_name().map_or(0, |n| n.len()));
    if let Some((i, _)) = install4j {
        return Some(i);
    }
    for kind in [|b: &Binary| matches!(b, Binary::Love), |b: &Binary| matches!(b, Binary::Jar), |b: &Binary| matches!(b, Binary::Flash)] {
        if let Some((i, _)) = files.iter().enumerate().filter(|(_, f)| kind(&f.binary)).min_by_key(|(_, f)| f.path.components().count()) {
            return Some(i);
        }
    }
    let folder: String = root.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default().chars().filter(|c| c.is_alphanumeric()).collect();
    files
        .iter()
        .enumerate()
        .filter_map(|(i, f)| {
            let Binary::Python(py) = &f.binary else { return None };
            let s = stem(&f.path).to_lowercase();
            let mut score = 0;
            if (!s.is_empty() && folder.contains(&s)) || ["main", "game", "run"].contains(&s.as_str()) {
                score += 3;
            }
            if py.pygame {
                score += 1;
            }
            Some((score, i))
        })
        .max_by_key(|&(score, i)| (score, std::cmp::Reverse(i)))
        .map(|(_, i)| i)
}

/// The LÖVE version the game says it was made for: `t.version = "0.10.1"` in its conf.lua.
/// The .love is a zip; unzip reads it (missing it, the version stays unknown).
fn love_version(file: &Path) -> Option<String> {
    let out = std::process::Command::new("unzip").arg("-p").arg(file).arg("conf.lua").output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().find(|l| l.contains(".version") && l.contains('='))?;
    let value = line.split('=').nth(1)?.trim().trim_start_matches(['"', '\'']);
    let v: String = value.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
    (!v.is_empty()).then_some(v)
}

fn engine(root: &Path, main: &Path, files: &[Found]) -> Option<Engine> {
    let dir = root.join(main.parent().unwrap_or(Path::new("")));
    let s = stem(main);
    let data = dir.join(format!("{s}_Data"));
    if data.is_dir() {
        let version = ["globalgamemanagers", "mainData", "data.unity3d"].iter().find_map(|f| unity_version(&data.join(f)));
        return Some(Engine { name: "Unity", version });
    }
    let named = |n: &str| files.iter().any(|f| f.path.file_name().is_some_and(|x| x == n));
    if dir.join(format!("{s}.pck")).is_file() || dir.join("data.pck").is_file() || ends_with_pck(&dir.join(main.file_name()?)) {
        return Some(Engine { name: "Godot", version: None });
    }
    if main.components().any(|c| c.as_os_str() == "Binaries") || root.join("Engine").is_dir() {
        return Some(Engine { name: "Unreal Engine", version: None });
    }
    if dir.join("assets/game.unx").is_file() || dir.join("game.unx").is_file() {
        return Some(Engine { name: "GameMaker", version: None });
    }
    // GameMaker games ship data.win next to the program, or packed inside it.
    if dir.join("data.win").is_file() || (main.extension().is_some_and(|e| e.eq_ignore_ascii_case("exe")) && binary::contains(&dir.join(main.file_name()?), b"data.win")) {
        return Some(Engine { name: "GameMaker", version: None });
    }
    if main.ancestors().any(|a| root.join(a).join("renpy").is_dir()) {
        return Some(Engine { name: "Ren'Py", version: None });
    }
    if named("UnityPlayer.so") {
        return Some(Engine { name: "Unity", version: None });
    }
    let here = |n: &str| dir.join(n).exists();
    let (name, found) = [("Adventure Game Studio", here("acsetup.cfg") && main.file_name().is_some_and(|n| n.to_string_lossy().starts_with("ags"))), ("Defold", here("game.projectc")), ("MonoGame", here("MonoGame.Framework.dll")), ("FNA", here("FNA.dll")), ("NW.js", here("nw.pak") || here("lib/libnw.so")), ("Java", here("jre") && std::fs::read_dir(&dir).into_iter().flatten().flatten().any(|e| e.path().extension().is_some_and(|x| x == "jar")))]
        .into_iter()
        .find(|(_, found)| *found)?;
    found.then_some(Engine { name, version: None })
}

/// Godot can embed its data in the program; such files end with "GDPC".
fn ends_with_pck(path: &Path) -> bool {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut f) = fs::File::open(path) else { return false };
    let mut tail = [0u8; 4];
    f.seek(SeekFrom::End(-4)).is_ok() && f.read_exact(&mut tail).is_ok() && &tail == b"GDPC"
}

/// Unity's data files carry the editor version ("2019.4.1f1", "5.6.3p2") near their start.
fn unity_version(path: &Path) -> Option<String> {
    use std::io::Read;
    let mut head = [0u8; 128];
    let n = fs::File::open(path).ok()?.read(&mut head).ok()?;
    head[..n]
        .split(|&b| b == 0)
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .find(|s| s.len() >= 5 && s.len() <= 20 && s.starts_with(|c: char| c.is_ascii_digit()) && s.matches('.').count() == 2)
}

/// Unity's `<Game>_Data/app.info` holds the company and the product name.
fn unity_product(root: &Path, main: &Path) -> Option<String> {
    let dir = root.join(main.parent().unwrap_or(Path::new("")));
    let info = fs::read_to_string(dir.join(format!("{}_Data/app.info", stem(main)))).ok()?;
    info.lines().nth(1).map(str::trim).filter(|s| !s.is_empty()).map(String::from)
}
