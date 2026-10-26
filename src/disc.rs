//! Windows game CDs (ISO images) and installers: what's on a disc, unpacking it with 7-Zip, and
//! finding the game an installer put into a Wine prefix.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// 7-Zip, which reads ISO 9660 and UDF images (Bazzite has `7z`).
pub fn seven_zip() -> Option<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    ["7z", "7za", "7zz"].iter().find_map(|name| std::env::split_paths(&path).map(|d| d.join(name)).find(|p| p.is_file()))
}

/// The files on a disc image (not its folders), as 7-Zip names them.
pub fn files(iso: &Path) -> Option<Vec<String>> {
    let out = Command::new(seven_zip()?).args(["l", "-ba", "-slt"]).arg(iso).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut files = Vec::new();
    let mut path = None;
    for line in text.lines() {
        if let Some(p) = line.strip_prefix("Path = ") {
            path = Some(p.to_string());
        } else if line == "Folder = -" {
            files.extend(path.take());
        }
    }
    (!files.is_empty()).then_some(files)
}

/// Words of a game's name worth matching against file names ("Bookworm-Deluxe_Win_EN_ISO":
/// bookworm, deluxe).
fn words(name: &str) -> Vec<String> {
    name.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|w| w.len() >= 4 && !["setup", "install", "game", "disc", "disk", "release"].contains(w)).map(String::from).collect()
}

/// Programs on a disc that install something other than the game.
const NOT_THE_INSTALLER: &[&str] = &["dxsetup", "directx", "redist", "vcredist", "dotnet", "uninst", "isdel", "msxml", "quicktime", "acrobat", "adobe", "indeo", "codec", "wmp", "ie5", "ie6", "mdac"];

/// The program on a disc that installs the game: a setup/install program near the top, one
/// named like the game first (a PopCap disc holds BookwormSetup.exe next to four trials'
/// installers and a menu); else what autorun.inf starts.
pub fn installer(iso: &Path, files: &[String], name: &str) -> Option<String> {
    let words = words(name);
    let best = files
        .iter()
        .filter_map(|f| {
            let lower = f.to_lowercase();
            let stem = Path::new(&lower).file_stem()?.to_string_lossy().into_owned();
            let depth = lower.matches('/').count();
            if !lower.ends_with(".exe") || depth > 2 || !(stem.contains("setup") || stem.contains("install")) || NOT_THE_INSTALLER.iter().any(|n| lower.contains(n)) {
                return None;
            }
            let mut score = -(depth as i32);
            if words.iter().any(|w| lower.contains(w.as_str())) {
                score += 4;
            }
            if lower.contains("trial") || lower.contains("demo") {
                score -= 3;
            }
            if stem == "setup" {
                score += 1;
            }
            Some((score, f))
        })
        .max_by_key(|(score, _)| *score)
        .map(|(_, f)| f.clone());
    best.or_else(|| {
        let inf = files.iter().find(|f| f.eq_ignore_ascii_case("autorun.inf"))?;
        let out = Command::new(seven_zip()?).args(["e", "-so"]).arg(iso).arg(inf).output().ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        let open = text.lines().find_map(|l| {
            let (key, value) = l.split_once('=')?;
            key.trim().eq_ignore_ascii_case("open").then(|| value.trim().split_whitespace().next().unwrap_or("").replace('\\', "/"))
        })?;
        files.iter().find(|f| f.eq_ignore_ascii_case(&open)).cloned()
    })
}

/// Unpacks a disc image into `dir`.
pub fn unpack(iso: &Path, dir: &Path) -> Result<(), String> {
    let seven = seven_zip().ok_or("7-Zip (7z) is needed to unpack the disc image.")?;
    let out = Command::new(seven).arg("x").arg("-y").arg(format!("-o{}", dir.display())).arg(iso).output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!("7-Zip couldn't unpack {}: {}", iso.display(), String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(())
}

/// Waits until no program runs in a Wine prefix any more: an installer often starts another
/// one and quits (InstallShield's setup.exe), so umu-run returning isn't the end. Proton gives
/// its programs WINEPREFIX=<prefix>/pfx/, umu-run STEAM_COMPAT_DATA_PATH=<prefix>.
pub fn wait_for_prefix(prefix: &Path) {
    let keys: Vec<String> = ["WINEPREFIX=", "STEAM_COMPAT_DATA_PATH="].iter().map(|k| format!("{k}{}", prefix.display())).collect();
    let mut quiet = 0;
    while quiet < 3 {
        let busy = fs::read_dir("/proc").into_iter().flatten().flatten().any(|e| {
            fs::read(e.path().join("environ")).is_ok_and(|env| {
                env.split(|&b| b == 0).any(|v| keys.iter().any(|k| v == k.as_bytes() || (v.starts_with(k.as_bytes()) && v.get(k.len()) == Some(&b'/'))))
            })
        });
        quiet = if busy { 0 } else { quiet + 1 };
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}

/// Names of installed programs that aren't the game.
const NOT_THE_PROGRAM: &[&str] = &["unins", "uninst", "readme", "help", "setup", "install", "regist", "update", "website", "manual", "support", "dxsetup"];

/// What an installer left in a prefix (`<prefix>/pfx`).
pub enum Installed {
    /// The game's program and its name: what the Start Menu or desktop shortcuts it made point
    /// to (their names are the game's), on whichever drive (an installer may pick Proton's S:,
    /// the real folder around the one it was started from), else the biggest program it put
    /// under Program Files.
    Program(PathBuf, String),
    /// Shortcuts, but none to a program that could be found: something was installed.
    Unknown,
    Nothing,
}

pub fn installed_program(pfx: &Path, name: &str) -> Installed {
    installed_program_on(pfx, name, &[], 0)
}

/// Right after an install: also a program new on the drives the install was given (`drives`,
/// their tops), made since `since` (Unix time). Only then: in an older prefix, "new" would mean
/// anything made on those drives since (lobby found Borderlands 4's program that way).
pub fn installed_program_on(pfx: &Path, name: &str, drives: &[PathBuf], since: i64) -> Installed {
    match find_installed(pfx, name, drives, since) {
        Some((exe, label)) => Installed::Program(exe, label),
        None if !links(pfx).is_empty() => Installed::Unknown,
        None => Installed::Nothing,
    }
}

/// The .lnk files in a prefix's Start Menu and desktop folders, of every user (Proton's
/// steamuser, plain Wine's own user name).
fn links(pfx: &Path) -> Vec<PathBuf> {
    let c = pfx.join("drive_c");
    let mut links = Vec::new();
    find_files(&c.join("ProgramData/Microsoft/Windows/Start Menu"), "lnk", 0, &mut links);
    for user in fs::read_dir(c.join("users")).into_iter().flatten().flatten() {
        for dir in ["Desktop", "AppData/Roaming/Microsoft/Windows/Start Menu"] {
            find_files(&user.path().join(dir), "lnk", 0, &mut links);
        }
    }
    links
}

fn unwanted(p: &Path) -> bool {
    let file = p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    NOT_THE_PROGRAM.iter().any(|n| file.contains(n)) || crate::scan::not_the_game(p)
}

/// The programs a prefix's Start Menu and desktop shortcuts point to, with the shortcuts'
/// names: what installers put there, leaving out uninstallers, readmes and the like.
pub fn shortcuts(pfx: &Path) -> Vec<(PathBuf, String)> {
    links(pfx)
        .iter()
        .filter_map(|lnk| {
            let target = link_target(&fs::read(lnk).ok()?)?;
            // Drives are links in dosdevices ("c:" -> ../drive_c, "s:" -> a real folder).
            let letter = target.get(..2)?.to_lowercase();
            let drive = fs::canonicalize(pfx.join("dosdevices").join(&letter)).ok().or_else(|| (letter == "c:").then(|| pfx.join("drive_c")))?;
            let exe = resolve(&drive, target.get(3..)?)?;
            if !exe.is_file() || unwanted(&exe) {
                return None;
            }
            Some((exe, lnk.file_stem()?.to_string_lossy().into_owned()))
        })
        .collect()
}

fn find_installed(pfx: &Path, name: &str, drives: &[PathBuf], since: i64) -> Option<(PathBuf, String)> {
    let c = pfx.join("drive_c");
    let words = words(name);
    let size = |p: &Path| fs::metadata(p).map_or(0, |m| m.len());
    let from_links = shortcuts(pfx)
        .into_iter()
        .map(|(exe, label)| {
            let named = words.iter().any(|w| label.to_lowercase().contains(w.as_str()) || exe.to_string_lossy().to_lowercase().contains(w.as_str()));
            ((named, size(&exe)), exe, label)
        })
        .max_by_key(|(key, ..)| *key)
        .map(|(_, exe, label)| (exe, label));
    let (exe, label) = from_links.or_else(|| {
        // Wine's own folders, which every prefix has.
        let mut exes = Vec::new();
        for dir in ["Program Files", "Program Files (x86)"] {
            for entry in fs::read_dir(c.join(dir)).into_iter().flatten().flatten() {
                let n = entry.file_name().to_string_lossy().to_lowercase();
                if !["common files", "internet explorer", "windows media player", "windows nt", "windowspowershell", "microsoft.net"].contains(&n.as_str()) {
                    find_files(&entry.path(), "exe", 0, &mut exes);
                }
            }
        }
        for entry in fs::read_dir(&c).into_iter().flatten().flatten() {
            let n = entry.file_name().to_string_lossy().to_lowercase();
            if !["windows", "users", "programdata", "program files", "program files (x86)"].contains(&n.as_str()) {
                find_files(&entry.path(), "exe", 0, &mut exes);
            }
        }
        // On another drive (E:\Games\X) with no shortcut (FitGirl's repacks make none by
        // default): what's new there since the install started.
        if !exes.iter().any(|e| !unwanted(e)) {
            let own = fs::canonicalize(pfx).unwrap_or_default();
            for drive in drives {
                new_files(drive, since, &own, 0, &mut exes);
            }
        }
        let exe = exes.into_iter().filter(|e| !unwanted(e)).max_by_key(|e| size(e))?;
        // Named after its folder, or the one above a System/Bin one (Harry Potter\System\HP.exe).
        let label = exe.ancestors().skip(1).filter_map(|d| d.file_name()).map(|n| n.to_string_lossy().into_owned()).find(|n| !["system", "bin", "binaries", "win32", "win64", "x86", "x64", "exe"].contains(&n.to_lowercase().as_str()))?;
        Some((exe, label))
    })?;
    Some((exe, label))
}

fn find_files(dir: &Path, ext: &str, depth: usize, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            if depth < 6 {
                find_files(&path, ext, depth + 1, out);
            }
        } else if path.extension().is_some_and(|e| e.eq_ignore_ascii_case(ext)) {
            out.push(path);
        }
    }
}

/// The .exe files made or changed in `dir` since `since` (Unix time), going only into folders changed
/// since then too (a folder's times change when something is made in it), so a whole drive
/// costs a few folders. Leaves out `skip` (the prefix), hidden folders and Windows' and Steam's.
fn new_files(dir: &Path, since: i64, skip: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    use std::os::unix::fs::MetadataExt;
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_lowercase();
        let Ok(meta) = fs::symlink_metadata(&path) else { continue };
        if meta.ctime().max(meta.mtime()) < since || name.starts_with('.') || path == skip || ["$recycle.bin", "system volume information", "steamapps"].contains(&name.as_str()) {
            continue;
        }
        if meta.is_dir() && depth < 6 {
            new_files(&path, since, skip, depth + 1, out);
        } else if meta.is_file() && path.extension().is_some_and(|e| e.eq_ignore_ascii_case("exe")) {
            out.push(path);
        }
    }
}

/// The program a Windows shortcut (.lnk) points to: the first `X:\...\name.exe` it holds,
/// written as 8-bit text (LinkInfo's local path) or UTF-16 (its string data).
fn link_target(data: &[u8]) -> Option<String> {
    let exe = |s: String| s.to_lowercase().ends_with(".exe").then_some(s);
    for i in 0..data.len().saturating_sub(3) {
        if data[i].is_ascii_alphabetic() && data[i + 1] == b':' && data[i + 2] == b'\\' {
            let end = data[i..].iter().position(|&b| b == 0).map_or(data.len(), |n| i + n);
            if let Some(s) = exe(String::from_utf8_lossy(&data[i..end]).into_owned()) {
                return Some(s);
            }
        }
        if i + 6 <= data.len() && data[i].is_ascii_alphabetic() && data[i + 1] == 0 && data[i + 2] == b':' && data[i + 3] == 0 && data[i + 4] == b'\\' && data[i + 5] == 0 {
            let units: Vec<u16> = data[i..].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|&u| u != 0).take(260).collect();
            if let Some(s) = exe(String::from_utf16_lossy(&units)) {
                return Some(s);
            }
        }
    }
    None
}

/// A Windows path (`Program Files\Game\game.exe`) under `root`, each part matched ignoring
/// case as Windows does. A short 8.3 name (`HARR~FNR` in Harry Potter's shortcut: Wine makes
/// up the part after the ~) matches the one name starting like it, with the same extension.
pub(crate) fn resolve(root: &Path, win: &str) -> Option<PathBuf> {
    let mut path = root.to_path_buf();
    for part in win.split('\\').filter(|p| !p.is_empty()) {
        let exact = path.join(part);
        if exact.exists() {
            path = exact;
            continue;
        }
        let mut names: Vec<_> = fs::read_dir(&path).ok()?.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        let lower = part.to_lowercase();
        let found = names.iter().find(|n| n.eq_ignore_ascii_case(part)).or_else(|| {
            let (stem, ext) = lower.split_once('.').map_or((lower.as_str(), ""), |(s, e)| (s, e));
            let head = stem.split_once('~')?.0;
            let ext_of = |n: &str| n.rsplit_once('.').map_or(String::new(), |(_, e)| e.to_lowercase());
            let mut like = names.iter().filter(|n| n.to_lowercase().starts_with(head) && ext_of(n) == ext);
            let first = like.next();
            if like.next().is_some() { None } else { first }
        })?;
        path = path.join(found);
    }
    Some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_targets() {
        let mut lnk = b"L\0\0\0junk".to_vec();
        lnk.extend(b"C:\\Program Files (x86)\\PopCap Games\\Bookworm Deluxe\\Bookworm.exe\0more");
        assert_eq!(link_target(&lnk).as_deref(), Some("C:\\Program Files (x86)\\PopCap Games\\Bookworm Deluxe\\Bookworm.exe"));
        let wide: Vec<u8> = "C:\\Games\\HP.exe\0".encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        assert_eq!(link_target(&wide).as_deref(), Some("C:\\Games\\HP.exe"));
        assert_eq!(link_target(b"C:\\readme.txt\0"), None);
    }

    #[test]
    fn short_names() {
        let root = std::env::temp_dir().join(format!("gamefix-short-{}", std::process::id()));
        std::fs::create_dir_all(root.join("Games/Harry Potter/System")).unwrap();
        std::fs::write(root.join("Games/Harry Potter/System/HP.exe"), b"").unwrap();
        assert_eq!(resolve(&root, "Games\\HARR~FNR\\SYSTEM\\HP.exe"), Some(root.join("Games/Harry Potter/System/HP.exe")));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn new_programs_on_other_drives() {
        let drive = std::env::temp_dir().join(format!("gamefix-new-files-{}", std::process::id()));
        for dir in ["Games/X/Bin", "Games/.hidden", "prefix/drive_c"] {
            fs::create_dir_all(drive.join(dir)).unwrap();
        }
        for file in ["Games/X/Bin/x.exe", "Games/.hidden/h.exe", "prefix/drive_c/wine.exe", "Games/X/readme.txt"] {
            fs::write(drive.join(file), "").unwrap();
        }
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
        let mut found = Vec::new();
        new_files(&drive, now - 5, &drive.join("prefix"), 0, &mut found);
        assert_eq!(found, [drive.join("Games/X/Bin/x.exe")]);
        found.clear();
        new_files(&drive, now + 60, &drive.join("prefix"), 0, &mut found);
        assert!(found.is_empty());
        fs::remove_dir_all(&drive).unwrap();
    }

    #[test]
    fn name_words() {
        assert_eq!(words("Bookworm-Deluxe_Win_EN_ISO"), ["bookworm", "deluxe"]);
    }
}
