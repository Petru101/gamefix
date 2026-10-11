//! RPG Maker games. 2000/2003 ones (RPG_RT.ldb + RPG_RT.lmt) play natively in EasyRPG Player
//! (Flathub). XP ones (a Game.ini naming RGSS1) run their Game.exe under Proton, with the RPG
//! Maker XP runtime package (the RTP: the RGSS library, graphics and music most games share)
//! installed once into a Wine prefix of gamefix's own. Games also come packed: RPG Maker
//! 2000/2003's "game disk" (Setup.exe, Setup.ini and the game in a .lzh: Mouth Sweet) and CAB
//! self-extractors (Carpe Diem's CarpeDiem.exe asks where to put an RPG Maker XP game).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Enterbrain's RPG Maker XP runtime package (Inno Setup), as rpgmakerweb.com offers it.
const XP_RTP: (&str, &str) = ("https://assets.rpgmakerweb.com/xp_rtp104e.exe", "b3bd20ad7f413b40ac233aafd2e061de1dc429c2eadb59d0b3157ba3c47f16b2");

/// Where each RGSS version's runtime package comes from, for the report.
pub const RTP_PAGE: &str = "https://www.rpgmakerweb.com/run-time-package";

/// A key's value in an .ini file, the key's case ignored.
fn ini_value(text: &str, key: &str) -> Option<String> {
    text.lines().find_map(|l| {
        let (k, v) = l.split_once('=')?;
        k.trim().eq_ignore_ascii_case(key).then(|| v.trim().to_string())
    })
}

/// A file in a folder, its name's case ignored (Windows games mix them), `\` as the separator.
fn find_nocase(dir: &Path, rel: &str) -> Option<PathBuf> {
    rel.split(['\\', '/']).filter(|p| !p.is_empty()).try_fold(dir.to_path_buf(), |at, part| {
        fs::read_dir(&at).ok()?.flatten().map(|e| e.path()).find(|p| p.file_name().is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case(part)))
    })
}

/// Which RGSS RPG Maker a Game.ini belongs to ("XP" for RGSS1, "VX" for RGSS2, "VX Ace" for
/// RGSS3), and whether the game needs that version's runtime package: its RGSS library isn't
/// with it (the package puts it in Windows' system folder; Carpe Diem has only its Data/), or
/// it names the package and has no Graphics/ of its own.
pub fn rgss(ini: &Path) -> Option<(&'static str, bool)> {
    let text = String::from_utf8_lossy(&fs::read(ini).ok()?).into_owned();
    let library = ini_value(&text, "Library")?;
    let name = library.rsplit(['\\', '/']).next().unwrap_or("").to_lowercase();
    let kind = match name.get(..5)? {
        "rgss1" => "XP",
        "rgss2" => "VX",
        "rgss3" => "VX Ace",
        _ => return None,
    };
    let dir = ini.parent()?;
    let declared = ["RTP1", "RTP"].iter().filter_map(|k| ini_value(&text, k)).any(|v| !v.is_empty());
    let needs = find_nocase(dir, &library).is_none() || (declared && find_nocase(dir, "Graphics").is_none());
    Some((kind, needs))
}

/// Whether a 2000/2003 game carries everything it uses (RPG_RT.ini: FullPackageFlag=1), so it
/// doesn't need that version's runtime package.
pub fn full_package(dir: &Path) -> bool {
    find_nocase(dir, "RPG_RT.ini").and_then(|f| fs::read(f).ok()).is_some_and(|t| ini_value(&String::from_utf8_lossy(&t), "FullPackageFlag").as_deref() == Some("1"))
}

/// The RPG Maker a packed game was made with, from what the package holds (7-Zip lists LZH and
/// CAB archives, including ones inside a program).
fn kind_of(names: &[String]) -> Option<&'static str> {
    let has = |n: &str| names.iter().any(|x| x.rsplit('/').next().is_some_and(|f| f.eq_ignore_ascii_case(n)));
    let ext = |e: &str| names.iter().any(|x| x.to_lowercase().ends_with(e));
    if has("RPG_RT.ldb") {
        Some("RPG Maker 2000/2003")
    } else if has("Game.ini") && (ext(".rxdata") || ext(".rgssad")) {
        Some("RPG Maker XP")
    } else if has("Game.ini") && (ext(".rvdata") || ext(".rgss2a")) {
        Some("RPG Maker VX")
    } else if has("Game.ini") && (ext(".rvdata2") || ext(".rgss3a")) {
        Some("RPG Maker VX Ace")
    } else {
        None
    }
}

fn listing(file: &Path) -> Vec<String> {
    let out = Command::new("7z").args(["l", "-slt", "-ba"]).arg(file).stdin(Stdio::null()).stderr(Stdio::null()).output();
    out.map(|o| String::from_utf8_lossy(&o.stdout).lines().filter_map(|l| l.strip_prefix("Path = ")).map(|p| p.replace('\\', "/")).collect()).unwrap_or_default()
}

/// An RPG Maker game packed in a self-extracting package: a .lzh beside a Setup.ini (RPG Maker
/// 2000/2003's game disk), or a Windows program with a CAB archive inside ("MSCF"; IExpress
/// packages are CABs too, but hold other games, which they start themselves). The RPG Maker.
pub fn packed(file: &Path) -> Option<&'static str> {
    let ext = file.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    let worth = match ext.as_str() {
        "lzh" => file.parent().and_then(|d| find_nocase(d, "Setup.ini")).is_some(),
        "exe" => fs::metadata(file).is_ok_and(|m| m.len() < 256 << 20) && crate::binary::contains(file, b"MSCF\0\0\0\0"),
        _ => false,
    };
    if !worth {
        return None;
    }
    kind_of(&listing(file))
}

/// The folder a packed game goes into, beside the package: the game disk's title from its
/// Setup.ini ("Mouth Sweet"), else the self-extractor's name ("CarpeDiem", where it unpacks to
/// itself).
pub fn unpacked_dir(file: &Path) -> PathBuf {
    let dir = file.parent().unwrap_or(Path::new("/"));
    let title = find_nocase(dir, "Setup.ini")
        .filter(|_| file.extension().is_some_and(|e| e.eq_ignore_ascii_case("lzh")))
        .and_then(|f| fs::read(f).ok())
        .and_then(|t| {
            let t = String::from_utf8_lossy(&t).into_owned();
            ini_value(&t, "GameTitle").or_else(|| ini_value(&t, "ProjectName"))
        })
        .map(|t| t.chars().map(|c| if c == '/' || c.is_control() { ' ' } else { c }).collect::<String>().trim().trim_start_matches('.').to_string())
        .filter(|t| !t.is_empty());
    dir.join(title.unwrap_or_else(|| file.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "game".into())))
}

/// Unpacks a package with 7-Zip into a folder.
pub fn unpack(file: &Path, to: &Path) -> Result<(), String> {
    let out = Command::new("7z").args(["x", "-y", "-bd"]).arg(format!("-o{}", to.display())).arg(file).stdin(Stdio::null()).output().map_err(|e| format!("7z: {e}"))?;
    if !out.status.success() || fs::read_dir(to).map_or(true, |mut d| d.next().is_none()) {
        return Err(format!("Couldn't unpack {}: {}", file.display(), String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(())
}

fn data_home() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/share"))
}

/// The Wine prefix with the RPG Maker XP runtime package installed, shared by every XP game
/// (they keep their saves in their own folders) and kept on undo: made the first time, by
/// running the package's installer silently with umu-run and the Proton Steam will use. A
/// Steam shortcut points to it with STEAM_COMPAT_DATA_PATH (umu makes it a compat-data folder,
/// `pfx -> .`). Its path, what that took, and what didn't work out, to tell the user.
pub fn xp_prefix(umu: &Path, tool_dir: &Path, tool: &str) -> Result<(PathBuf, Vec<String>, Option<String>), String> {
    let dir = data_home().join("gamefix/prefixes/rpg-maker-xp");
    if dir.join("ready").is_file() {
        let dir = fs::canonicalize(&dir).unwrap_or(dir);
        return Ok(match midi_bank(&dir, umu, tool_dir) {
            Some(Ok(note)) => (dir, vec![note], None),
            Some(Err(e)) => (dir, Vec::new(), Some(e)),
            None => (dir, Vec::new(), None),
        });
    }
    let failed = |what: &str| format!("Couldn't set up RPG Maker XP's runtime package: {what}.");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).map_err(|e| failed(&e.to_string()))?;
    // Its real path (/var/home, not /home, on Bazzite), as Steam will be given it.
    let dir = fs::canonicalize(&dir).map_err(|e| failed(&e.to_string()))?;
    let result = (|| {
        let (url, sha) = XP_RTP;
        let setup = std::env::temp_dir().join(format!("gamefix-xp-rtp-{}.exe", std::process::id()));
        let got = Command::new("curl").args(["-sfL", "--max-time", "300", "-o"]).arg(&setup).arg(url).status().is_ok_and(|s| s.success());
        let sum = Command::new("sha256sum").arg(&setup).output().map(|o| String::from_utf8_lossy(&o.stdout).split_whitespace().next().unwrap_or("").to_string()).unwrap_or_default();
        if !got || sum != sha {
            let _ = fs::remove_file(&setup);
            return Err(failed(if got { "the download doesn't match its checksum" } else { "the download from rpgmakerweb.com failed (no network?)" }));
        }
        let status = Command::new(umu)
            .arg(&setup)
            .args(["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART"])
            .env("WINEPREFIX", &dir)
            .env("PROTONPATH", tool_dir)
            .env("GAMEID", "umu-0")
            .env("PROTON_SET_GAME_DRIVE", "0")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        crate::disc::wait_for_prefix(&dir);
        let _ = fs::remove_file(&setup);
        status.map_err(|e| failed(&format!("umu-run: {e}")))?;
        let windows = dir.join("drive_c/windows");
        if !["syswow64", "system32"].iter().any(|s| find_nocase(&windows.join(s), "RGSS102E.dll").is_some()) {
            return Err(failed("its installer ran but installed nothing (it needs the desktop session)"));
        }
        fs::write(dir.join("ready"), format!("RPG Maker XP runtime package from {url}, installed with {tool}\n")).map_err(|e| failed(&e.to_string()))?;
        Ok(format!("Installed RPG Maker XP's runtime package (from rpgmakerweb.com) with {tool} into a Wine prefix, ~/.local/share/gamefix/prefixes/rpg-maker-xp, shared by every RPG Maker XP game (undo leaves it)."))
    })();
    match result {
        Ok(note) => Ok(match midi_bank(&dir, umu, tool_dir) {
            Some(Ok(midi)) => (dir, vec![note, midi], None),
            Some(Err(e)) => (dir, vec![note], Some(e)),
            None => (dir, vec![note], None),
        }),
        Err(e) => {
            let _ = fs::remove_dir_all(&dir);
            Err(e)
        }
    }
}

/// RGSS plays the package's music, all MIDI, through DirectMusic, whose synthesizer in Wine
/// needs Windows' General MIDI sound bank (gm.dls), which Proton doesn't have: Carpe Diem's
/// title was silent. winetricks' gmdls puts it in (from Microsoft's April 2006 DirectX package,
/// which only the Wayback Machine still has: 52 MB, minutes; checked and cached by winetricks).
/// What happened, to tell the user, unless it was already there.
fn midi_bank(dir: &Path, umu: &Path, tool_dir: &Path) -> Option<Result<String, String>> {
    let bank = || ["syswow64", "system32"].iter().any(|s| fs::metadata(dir.join("drive_c/windows").join(s).join("drivers/gm.dls")).is_ok_and(|m| m.len() > 1 << 20));
    if bank() {
        return None;
    }
    let _ = Command::new(umu)
        .args(["winetricks", "-q", "gmdls"])
        .env("WINEPREFIX", dir)
        .env("PROTONPATH", tool_dir)
        .env("GAMEID", "umu-0")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    crate::disc::wait_for_prefix(dir);
    Some(if bank() {
        Ok("Installed Windows' General MIDI sound bank there (winetricks gmdls), which RPG Maker XP's MIDI music needs.".into())
    } else {
        Err("Windows' General MIDI sound bank couldn't be installed (winetricks gmdls), so its MIDI music stays silent; setting it up again tries again.".into())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packed_kinds_and_ini() {
        let names = |l: &[&str]| l.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(kind_of(&names(&["Backdrop/Arena.png", "RPG_RT.LDB", "Map0001.lmu"])), Some("RPG Maker 2000/2003"));
        assert_eq!(kind_of(&names(&["Data/Actors.rxdata", "Game.exe", "Game.ini"])), Some("RPG Maker XP"));
        assert_eq!(kind_of(&names(&["Game.ini", "Game.rgss3a"])), Some("RPG Maker VX Ace"));
        assert_eq!(kind_of(&names(&["SKELETON.exe", "data.win"])), None);
        assert_eq!(ini_value("[Game]\r\nLibrary=RGSS102E.dll\r\nRTP1=Standard\r\n", "library").as_deref(), Some("RGSS102E.dll"));
    }
}
