//! Walks a game folder: finds its programs, libraries and Windows builds, picks the program to
//! run and recognises the engine.

use crate::binary::{self, Binary, EM_386, EM_X86_64};
use std::fs;
use std::path::{Path, PathBuf};

const MAX_DEPTH: usize = 5;
const MAX_FILES: usize = 50_000;

/// Programs that ship next to games but aren't the game.
const NOT_THE_GAME: &[&str] = &["crashhandler", "crashpad", "createdump", "crashreport", "chrome-sandbox", "unins", "redist", "vc_redist", "dxsetup", "steamwebhelper", "quicksfv"];

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
    pub engine: Option<Engine>,
}

impl Game {
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
    let program = main.or(windows);
    let engine = program.and_then(|i| engine(&root, &files[i].path, &files));
    let name = program
        .and_then(|i| unity_product(&root, &files[i].path))
        .or_else(|| chosen.as_ref().filter(|_| shared_folder(&root)).map(|c| stem(c)))
        .or_else(|| root.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| root.display().to_string());
    Some(Game { root, name, files, main, windows, engine })
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
        if kind.is_dir() {
            if depth < MAX_DEPTH {
                walk(root, &path, depth + 1, out);
            }
        } else if let Some(binary) = binary::identify(&root.join(&path)) {
            let is_exe = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("exe"));
            // Windows DLLs aren't interesting, and scripts only at the top, where launchers live.
            let keep = match &binary {
                Binary::Pe(_) => is_exe,
                Binary::Script => depth == 0,
                Binary::Elf(_) => true,
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
            if !elf.program || not_the_game(&f.path) {
                return None;
            }
            let dir = root.join(f.path.parent().unwrap_or(Path::new("")));
            let s = stem(&f.path);
            let mut score = 0;
            if dir.join(format!("{s}_Data")).is_dir() || dir.join(format!("{s}.pck")).is_file() {
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

fn engine(root: &Path, main: &Path, files: &[Found]) -> Option<Engine> {
    let dir = root.join(main.parent().unwrap_or(Path::new("")));
    let s = stem(main);
    let data = dir.join(format!("{s}_Data"));
    if data.is_dir() {
        let version = ["globalgamemanagers", "mainData", "data.unity3d"].iter().find_map(|f| unity_version(&data.join(f)));
        return Some(Engine { name: "Unity", version });
    }
    let named = |n: &str| files.iter().any(|f| f.path.file_name().is_some_and(|x| x == n));
    if dir.join(format!("{s}.pck")).is_file() || ends_with_pck(&dir.join(main.file_name()?)) {
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
    if root.join("renpy").is_dir() {
        return Some(Engine { name: "Ren'Py", version: None });
    }
    if named("UnityPlayer.so") {
        return Some(Engine { name: "Unity", version: None });
    }
    None
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
