//! RPG Maker games. 2000/2003 ones (RPG_RT.ldb + RPG_RT.lmt) play natively in EasyRPG Player
//! (Flathub). XP ones (a Game.ini naming RGSS1) run their Game.exe under Proton, with the RPG
//! Maker XP runtime package (the RTP: the RGSS library, graphics and music most games share)
//! installed once into a Wine prefix of gamefix's own. Games also come packed: RPG Maker
//! 2000/2003's "game disk" (Setup.exe, Setup.ini and the game in a .lzh: Mouth Sweet) and CAB
//! self-extractors (Carpe Diem's CarpeDiem.exe asks where to put an RPG Maker XP game).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::gog::find_nocase;

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

/// RPG Maker MV/MZ files the game asks for by a name whose upper and lower case differ from
/// the file's: Windows finds them anyway, Linux doesn't, and the game stops with its own
/// "Loading Error" (Quidget: its title plugin asks for img/titles1/Ninja_00, the file is
/// ninja_00.png). The names are every quoted or path piece of its data (`data/*.json`),
/// plugins (`js/plugins.js`, `js/plugins/*.js`) and style sheets (`fonts/*.css`, `css/*.css`:
/// Natari's gamefont.css asks for MainFont.TTF, the file is MainFont.ttf, and its boot scene
/// waits for the font on "Now Loading..." for ever) that matches an image, sound, movie or
/// font's name apart from case, with or without its ending. Each as (the file, the name to add beside it,
/// with the file's own ending). The game's folder is `www/` (MV) or the top (MZ), at most one
/// folder down.
pub fn wrong_case(root: &Path) -> Vec<(PathBuf, String)> {
    let candidates = [root.to_path_buf(), root.join("www")].into_iter().chain(fs::read_dir(root).into_iter().flatten().flatten().flat_map(|e| [e.path(), e.path().join("www")]));
    let Some(dir) = candidates.into_iter().find(|d| d.join("data/System.json").is_file() && d.join("js").is_dir()) else { return Vec::new() };
    // Asset files by their name in lower case without the ending (and with it).
    let mut assets: std::collections::HashMap<String, Vec<PathBuf>> = std::collections::HashMap::new();
    let mut todo: Vec<(PathBuf, u32)> = ["img", "audio", "movies", "fonts"].iter().map(|d| (dir.join(d), 0)).collect();
    while let Some((d, depth)) = todo.pop() {
        for entry in fs::read_dir(&d).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                if depth < 3 {
                    todo.push((path, depth + 1));
                }
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            // Fonts only from fonts/: its gamefont.css would match the "GameFont" family name
            // plugins write (Koi, Quidget), a name nothing loads as a file.
            let font = [".ttf", ".otf", ".woff", ".woff2"].iter().any(|x| name.to_lowercase().ends_with(x));
            if d.starts_with(dir.join("fonts")) && !font {
                continue;
            }
            let stem = name.split('.').next().unwrap_or(&name).to_lowercase();
            assets.entry(stem).or_default().push(path.clone());
            assets.entry(name.to_lowercase()).or_default().push(path);
        }
    }
    let mut sources: Vec<PathBuf> = fs::read_dir(dir.join("data")).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "json")).collect();
    sources.push(dir.join("js/plugins.js"));
    sources.extend(fs::read_dir(dir.join("js/plugins")).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "js")));
    sources.extend(["fonts", "css"].iter().flat_map(|d| fs::read_dir(dir.join(d)).into_iter().flatten().flatten()).map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("css"))));
    let mut out: Vec<(PathBuf, String)> = Vec::new();
    for source in sources {
        let Ok(text) = fs::read_to_string(&source) else { continue };
        for piece in quoted_values(&text).iter().flat_map(|v| v.split(['/', ',']).map(str::trim).collect::<Vec<_>>()) {
            if piece.is_empty() || piece.len() > 120 {
                continue;
            }
            let Some(files) = assets.get(&piece.to_lowercase()) else { continue };
            for file in files {
                let name = file.file_name().unwrap_or_default().to_string_lossy().into_owned();
                // The asked-for name with the file's own ending ("Ninja_00" + ".png").
                let wanted = if name.to_lowercase() == piece.to_lowercase() { piece.to_string() } else { format!("{piece}{}", &name[name.find('.').unwrap_or(name.len())..]) };
                if wanted != name && !file.with_file_name(&wanted).exists() && !out.iter().any(|(f, w)| f == file && *w == wanted) {
                    out.push((file.clone(), wanted));
                }
            }
        }
    }
    out
}

/// The quoted strings of JSON or JavaScript that aren't keys (not followed by ':': the data's
/// `"damage": {...}` isn't img/system/Damage.png), escapes honoured; a string holding quotes
/// itself (plugins.js keeps plugin settings as JSON inside strings) is read the same way.
fn quoted_values(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let quote = chars[i];
        if quote != '"' && quote != '\'' && quote != '`' {
            i += 1;
            continue;
        }
        let mut value = String::new();
        let mut j = i + 1;
        while j < chars.len() && chars[j] != quote && chars[j] != '\n' {
            if chars[j] == '\\' && j + 1 < chars.len() {
                j += 1;
            }
            value.push(chars[j]);
            j += 1;
        }
        if j >= chars.len() || chars[j] == '\n' {
            i = j + 1;
            continue;
        }
        let key = chars[j + 1..].iter().find(|c| !c.is_whitespace()) == Some(&':');
        if !key && value.contains('"') {
            out.extend(quoted_values(&value));
        } else if !key && !value.is_empty() && value.len() <= 300 {
            out.push(value);
        }
        i = j + 1;
    }
    out
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

#[cfg(test)]
mod case_tests {
    use super::*;

    #[test]
    fn wrong_case_names() {
        let root = std::env::temp_dir().join(format!("gamefix-rpgcase-{}", std::process::id()));
        let www = root.join("Game/www");
        for d in ["data", "js/plugins", "img/titles1", "img/system", "audio/se", "fonts"] {
            fs::create_dir_all(www.join(d)).unwrap();
        }
        fs::write(www.join("data/System.json"), r#"{"damage":{"x":1},"title1Name":"empty","sounds":[{"name":"Cursor1"}]}"#).unwrap();
        fs::write(www.join("js/plugins.js"), r#"var $plugins = [{"parameters":{"Logo":"{\"file\":\"img/titles1/Logo\"}"}}];"#).unwrap();
        fs::write(www.join("js/plugins/Title.js"), "var images = [\"Ninja_00\", 'Ninja_01']; var f = 'GameFont';").unwrap();
        for f in ["img/titles1/ninja_00.png", "img/titles1/ninja_01.rpgmvp", "img/titles1/logo.png", "img/system/Damage.png", "audio/se/Cursor1.ogg", "fonts/MainFont.ttf", "fonts/mplus-1m-regular.ttf"] {
            fs::write(www.join(f), b"x").unwrap();
        }
        // Natari's gamefont.css (CRLF, and a missing ";" as in the real one); the stock one matches.
        fs::write(www.join("fonts/gamefont.css"), "@font-face {\r\n    font-family: GameFont;\r\n    src: url(\"MainFont.TTF\");\r\n}\r\n@font-face { font-family: B\r\n src: url(\"mplus-1m-regular.ttf\"); }\r\n").unwrap();
        let mut found: Vec<String> = wrong_case(&root.join("Game")).into_iter().map(|(f, w)| format!("{}>{w}", f.file_name().unwrap().to_string_lossy())).collect();
        found.sort();
        assert_eq!(found, ["MainFont.ttf>MainFont.TTF", "logo.png>Logo.png", "ninja_00.png>Ninja_00.png", "ninja_01.rpgmvp>Ninja_01.rpgmvp"]);
        let _ = fs::remove_dir_all(&root);
    }
}
