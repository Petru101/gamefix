//! Steam: finding its folder and user, and adding non-Steam games to `shortcuts.vdf`.
//!
//! Steam rewrites its config files when it quits, so they are only changed while it's closed.

use crate::vdf;
use std::fs;
use std::path::{Path, PathBuf};

pub struct Steam {
    pub root: PathBuf,
    /// `userdata/<account id>` of the most recent user.
    pub user: PathBuf,
}

impl Steam {
    pub fn find() -> Option<Steam> {
        let home = PathBuf::from(std::env::var_os("HOME")?);
        let root = [".local/share/Steam", ".steam/steam", ".var/app/com.valvesoftware.Steam/.local/share/Steam"]
            .iter()
            .map(|p| home.join(p))
            .find(|p| p.join("userdata").is_dir())?;
        let user = most_recent_user(&root)?;
        Some(Steam { root, user })
    }

    pub fn shortcuts(&self) -> PathBuf {
        self.user.join("config/shortcuts.vdf")
    }

    pub fn localconfig(&self) -> PathBuf {
        self.user.join("config/localconfig.vdf")
    }

    pub fn config(&self) -> PathBuf {
        self.root.join("config/config.vdf")
    }

    /// Steam's library folders (where games and Valve's Protons are installed), its own first.
    pub fn libraries(&self) -> Vec<PathBuf> {
        let mut out = vec![self.root.clone()];
        let text = fs::read_to_string(self.root.join("steamapps/libraryfolders.vdf")).unwrap_or_default();
        let Ok(doc) = vdf::parse(&text) else { return out };
        let Some(vdf::Node::Map(folders)) = vdf::get(&doc, &["libraryfolders"]) else { return out };
        for (_, folder) in folders {
            if let vdf::Node::Map(fields) = folder {
                if let Some((_, vdf::Node::Str(path))) = fields.iter().find(|(k, _)| k == "path") {
                    let path = PathBuf::from(vdf::unescape(path));
                    if !out.iter().any(|p| same_dir(p, &path)) {
                        out.push(path);
                    }
                }
            }
        }
        out
    }

    /// The compatibility tools installed, by Steam's internal names: Valve's Protons from their
    /// app manifests ("Proton 9.0" is proton_9 wherever it is: its folder may still be called
    /// "Proton 9.0 (Beta)"), then custom ones (GE-Proton) from compatibilitytools.d.
    pub fn tools(&self) -> Vec<String> {
        let mut out = Vec::new();
        for library in self.libraries() {
            let Ok(entries) = fs::read_dir(library.join("steamapps")) else { continue };
            for entry in entries.flatten() {
                if !entry.file_name().to_string_lossy().starts_with("appmanifest_") {
                    continue;
                }
                let Ok(doc) = vdf::parse(&fs::read_to_string(entry.path()).unwrap_or_default()) else { continue };
                let field = |key| match vdf::get(&doc, &["AppState", key]) {
                    Some(vdf::Node::Str(v)) => vdf::unescape(v),
                    _ => String::new(),
                };
                let name = match field("name").as_str() {
                    "Proton Experimental" => "proton_experimental".to_string(),
                    "Proton Hotfix" => "proton_hotfix".to_string(),
                    n => match n.strip_prefix("Proton ") {
                        Some(v) if !version_key(v).is_empty() && v.chars().all(|c| c.is_ascii_digit() || c == '.') => official_name(v),
                        _ => continue,
                    },
                };
                if library.join("steamapps/common").join(field("installdir")).join("proton").is_file() && !out.contains(&name) {
                    out.push(name);
                }
            }
        }
        let mut custom: Vec<String> = fs::read_dir(self.root.join("compatibilitytools.d")).into_iter().flatten().flatten().filter_map(|d| custom_tool(&d.path())).collect();
        custom.sort();
        out.extend(custom);
        out
    }

    /// The Proton for Windows games nothing better is known for: Valve's Experimental if
    /// installed, else its newest numbered one, else a custom one.
    pub fn proton(&self) -> Option<String> {
        let tools = self.tools();
        let numbered = |t: &&String| t.strip_prefix("proton_").is_some_and(|v| v.chars().all(|c| c.is_ascii_digit()));
        tools
            .iter()
            .find(|t| *t == "proton_experimental")
            .or_else(|| tools.iter().filter(numbered).max_by_key(|t| version_key(t)))
            .or_else(|| tools.iter().find(|t| !t.starts_with("proton_")))
            .cloned()
    }

    /// Where a compatibility tool (by Steam's internal name) is installed.
    pub fn tool_dir(&self, name: &str) -> Option<PathBuf> {
        if !name.starts_with("proton_") {
            return fs::read_dir(self.root.join("compatibilitytools.d")).ok()?.flatten().map(|d| d.path()).find(|d| custom_tool(d).as_deref() == Some(name));
        }
        self.libraries().into_iter().find_map(|library| {
            fs::read_dir(library.join("steamapps")).ok()?.flatten().find_map(|entry| {
                let doc = vdf::parse(&fs::read_to_string(entry.path()).ok()?).ok()?;
                let field = |key| match vdf::get(&doc, &["AppState", key]) {
                    Some(vdf::Node::Str(v)) => Some(vdf::unescape(v)),
                    _ => None,
                };
                let tool = match field("name")?.as_str() {
                    "Proton Experimental" => "proton_experimental".to_string(),
                    "Proton Hotfix" => "proton_hotfix".to_string(),
                    n => official_name(n.strip_prefix("Proton ")?),
                };
                let dir = library.join("steamapps/common").join(field("installdir")?);
                (tool == name && dir.join("proton").is_file()).then_some(dir)
            })
        })
    }

    /// The newest GE-Proton installed, by Steam's internal name.
    pub fn newest_ge(&self) -> Option<String> {
        self.tools().into_iter().filter(|t| t.starts_with("GE-Proton")).max_by_key(|t| version_key(t))
    }

    /// Asks Steam to quit properly (saving its settings) and waits up to a minute.
    pub fn shut_down(&self) -> bool {
        let _ = std::process::Command::new("steam").arg("-shutdown").stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn();
        for _ in 0..60 {
            std::thread::sleep(std::time::Duration::from_secs(1));
            if !self.running() {
                return true;
            }
        }
        false
    }

    pub fn running(&self) -> bool {
        let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
        [home.join(".steam/steam.pid"), home.join(".var/app/com.valvesoftware.Steam/.steam/steam.pid")].iter().any(|f| {
            fs::read_to_string(f).ok().and_then(|pid| pid.trim().parse::<u32>().ok()).is_some_and(|pid| {
                fs::read_link(format!("/proc/{pid}/exe")).is_ok_and(|exe| exe.file_name().is_some_and(|n| n == "steam"))
            })
        })
    }
}

/// Steam's internal name for one of Valve's Protons: "7.0" is proton_7, "5.13" proton_513.
pub fn official_name(version: &str) -> String {
    let (major, minor) = version.split_once('.').unwrap_or((version, "0"));
    if minor == "0" { format!("proton_{major}") } else { format!("proton_{major}{minor}") }
}

/// A custom tool's internal name, from the compatibilitytool.vdf in its folder.
pub fn custom_tool(dir: &Path) -> Option<String> {
    let doc = vdf::parse(&fs::read_to_string(dir.join("compatibilitytool.vdf")).ok()?).ok()?;
    match vdf::get(&doc, &["compatibilitytools", "compat_tools"])? {
        vdf::Node::Map(tools) => tools.first().map(|(name, _)| vdf::unescape(name)),
        _ => None,
    }
}

/// The numbers in a tool's name, for comparing versions: GE-Proton10-9 before GE-Proton10-25.
fn version_key(name: &str) -> Vec<u32> {
    name.split(|c: char| !c.is_ascii_digit()).filter_map(|p| p.parse().ok()).collect()
}

fn same_dir(a: &Path, b: &Path) -> bool {
    a == b || fs::canonicalize(a).ok().is_some_and(|a| fs::canonicalize(b).ok() == Some(a))
}

/// The Steam app installed in this folder or one it's in (`<library>/steamapps/common/<installdir>`),
/// from the library's `appmanifest_<appid>.acf` files.
pub fn installed_app(dir: &Path) -> Option<u32> {
    dir.ancestors().find_map(app_in)
}

fn app_in(dir: &Path) -> Option<u32> {
    let common = dir.parent()?;
    if common.file_name()? != "common" {
        return None;
    }
    let installdir = dir.file_name()?.to_str()?;
    fs::read_dir(common.parent()?).ok()?.flatten().find_map(|entry| {
        let name = entry.file_name().into_string().ok()?;
        let appid: u32 = name.strip_prefix("appmanifest_")?.strip_suffix(".acf")?.parse().ok()?;
        let doc = vdf::parse(&fs::read_to_string(entry.path()).ok()?).ok()?;
        match vdf::get(&doc, &["AppState", "installdir"])? {
            vdf::Node::Str(dir) if vdf::unescape(dir) == installdir => Some(appid),
            _ => None,
        }
    })
}

const LAUNCH_OPTIONS: [&str; 6] = ["UserLocalConfigStore", "Software", "Valve", "Steam", "apps", ""];

/// A Steam game's launch options (unescaped), if it has any.
pub fn launch_options(file: &Path, appid: u32) -> Result<Option<String>, String> {
    let doc = vdf::parse(&fs::read_to_string(file).map_err(|e| format!("{}: {e}", file.display()))?)?;
    let id = appid.to_string();
    let mut path = LAUNCH_OPTIONS;
    path[5] = &id;
    Ok(match vdf::get(&doc, &[&path[..], &["LaunchOptions"]].concat()) {
        Some(vdf::Node::Str(v)) => Some(vdf::unescape(v)),
        _ => None,
    })
}

/// Sets (or with None removes) a Steam game's launch options.
pub fn set_launch_options(file: &Path, appid: u32, options: Option<&str>) -> Result<(), String> {
    let text = fs::read_to_string(file).map_err(|e| format!("{}: {e}", file.display()))?;
    let mut doc = vdf::parse(&text).map_err(|e| format!("{}: {e}, not changing it", file.display()))?;
    let id = appid.to_string();
    let mut path = LAUNCH_OPTIONS;
    path[5] = &id;
    let block = vdf::block(&mut doc, &path)?;
    vdf::replace(block, "LaunchOptions", options.map(|o| vdf::Node::Str(vdf::escape(o))));
    fs::copy(file, file.with_extension("vdf.gamefix-backup")).map_err(|e| e.to_string())?;
    let tmp = file.with_extension("vdf.gamefix-tmp");
    fs::write(&tmp, vdf::write(&doc)).map_err(|e| e.to_string())?;
    fs::rename(&tmp, file).map_err(|e| e.to_string())
}

const COMPAT_TOOLS: [&str; 5] = ["InstallConfigStore", "Software", "Valve", "Steam", "CompatToolMapping"];

/// The compatibility tool forced for an app or shortcut, if any.
pub fn compat_tool(file: &Path, appid: u32) -> Result<Option<String>, String> {
    let doc = vdf::parse(&fs::read_to_string(file).map_err(|e| format!("{}: {e}", file.display()))?)?;
    let id = appid.to_string();
    Ok(match vdf::get(&doc, &[&COMPAT_TOOLS[..], &[id.as_str(), "name"]].concat()) {
        Some(vdf::Node::Str(name)) => Some(vdf::unescape(name)),
        _ => None,
    })
}

/// Forces (or with None un-forces) a compatibility tool, as Properties → Compatibility does.
pub fn set_compat_tool(file: &Path, appid: u32, tool: Option<&str>) -> Result<(), String> {
    let text = fs::read_to_string(file).map_err(|e| format!("{}: {e}", file.display()))?;
    let mut doc = vdf::parse(&text).map_err(|e| format!("{}: {e}, not changing it", file.display()))?;
    let mapping = vdf::block(&mut doc, &COMPAT_TOOLS)?;
    let entry = tool.map(|t| {
        vdf::Node::Map(vec![
            ("name".into(), vdf::Node::Str(vdf::escape(t))),
            ("config".into(), vdf::Node::Str(String::new())),
            ("priority".into(), vdf::Node::Str("250".into())),
        ])
    });
    vdf::replace(mapping, &appid.to_string(), entry);
    fs::copy(file, file.with_extension("vdf.gamefix-backup")).map_err(|e| e.to_string())?;
    let tmp = file.with_extension("vdf.gamefix-tmp");
    fs::write(&tmp, vdf::write(&doc)).map_err(|e| e.to_string())?;
    fs::rename(&tmp, file).map_err(|e| e.to_string())
}

/// The user marked "MostRecent" in loginusers.vdf; their folder is the SteamID64's low 32 bits.
fn most_recent_user(root: &Path) -> Option<PathBuf> {
    let users: Vec<PathBuf> = fs::read_dir(root.join("userdata")).ok()?.flatten().map(|e| e.path()).filter(|p| p.join("config").is_dir()).collect();
    let login = fs::read_to_string(root.join("config/loginusers.vdf")).unwrap_or_default();
    let mut current = None;
    for line in login.lines() {
        let fields: Vec<&str> = line.split('"').filter(|s| !s.trim().is_empty()).collect();
        match fields.as_slice() {
            [id] if id.len() == 17 && id.chars().all(|c| c.is_ascii_digit()) => current = id.parse::<u64>().ok(),
            ["MostRecent", "1"] => {
                if let Some(id) = current {
                    let dir = root.join("userdata").join((id & 0xffff_ffff).to_string());
                    if dir.is_dir() {
                        return Some(dir);
                    }
                }
            }
            _ => {}
        }
    }
    (users.len() == 1).then(|| users[0].clone())
}

/// Steam's binary VDF: 0x00 opens a map, 0x01 is a string, 0x02 an int32, 0x08 closes a map.
/// Keys and strings are kept as bytes so unknown text survives a rewrite unchanged.
#[derive(Clone, PartialEq, Debug)]
pub enum Node {
    Map(Vec<(Vec<u8>, Node)>),
    Str(Vec<u8>),
    Int(u32),
}

fn cstr(data: &[u8], pos: &mut usize) -> Result<Vec<u8>, String> {
    let end = data[*pos..].iter().position(|&b| b == 0).ok_or("shortcuts.vdf: unterminated string")?;
    let s = data[*pos..*pos + end].to_vec();
    *pos += end + 1;
    Ok(s)
}

fn parse_map(data: &[u8], pos: &mut usize) -> Result<Vec<(Vec<u8>, Node)>, String> {
    let mut items = Vec::new();
    loop {
        let kind = *data.get(*pos).ok_or("shortcuts.vdf: ends early")?;
        *pos += 1;
        if kind == 8 {
            return Ok(items);
        }
        let key = cstr(data, pos)?;
        let node = match kind {
            0 => Node::Map(parse_map(data, pos)?),
            1 => Node::Str(cstr(data, pos)?),
            2 => {
                let b = data.get(*pos..*pos + 4).ok_or("shortcuts.vdf: ends early")?;
                *pos += 4;
                Node::Int(u32::from_le_bytes(b.try_into().unwrap()))
            }
            // Anything else would be rewritten wrongly; better to leave the file alone.
            k => return Err(format!("shortcuts.vdf: unknown value type {k}, not changing it")),
        };
        items.push((key, node));
    }
}

pub fn parse(data: &[u8]) -> Result<Vec<(Vec<u8>, Node)>, String> {
    let mut pos = 0;
    // The document itself is a map: its closing 0x08 is the file's last byte.
    let items = parse_map(data, &mut pos)?;
    if pos != data.len() {
        return Err("shortcuts.vdf: unexpected data at the end, not changing it".into());
    }
    Ok(items)
}

fn write_map(items: &[(Vec<u8>, Node)], out: &mut Vec<u8>) {
    for (key, node) in items {
        let kind = match node {
            Node::Map(_) => 0,
            Node::Str(_) => 1,
            Node::Int(_) => 2,
        };
        out.push(kind);
        out.extend_from_slice(key);
        out.push(0);
        match node {
            Node::Map(m) => write_map(m, out),
            Node::Str(s) => {
                out.extend_from_slice(s);
                out.push(0);
            }
            Node::Int(i) => out.extend_from_slice(&i.to_le_bytes()),
        }
    }
    out.push(8);
}

pub fn write(items: &[(Vec<u8>, Node)]) -> Vec<u8> {
    let mut out = Vec::new();
    write_map(items, &mut out);
    out
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

pub struct Shortcut<'a> {
    pub name: &'a str,
    pub exe: &'a Path,
    pub start_dir: &'a Path,
    pub icon: &'a str,
    pub launch_options: &'a str,
}

fn read_shortcuts(file: &Path) -> Result<Vec<(Vec<u8>, Node)>, String> {
    match fs::read(file) {
        Ok(data) => parse(&data),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(vec![(b"shortcuts".to_vec(), Node::Map(Vec::new()))]),
        Err(e) => Err(format!("{}: {e}", file.display())),
    }
}

/// Writes through a temporary file, keeping the previous version as `shortcuts.vdf.gamefix-backup`.
fn save_shortcuts(file: &Path, doc: &[(Vec<u8>, Node)]) -> Result<(), String> {
    if file.exists() {
        fs::copy(file, file.with_extension("vdf.gamefix-backup")).map_err(|e| e.to_string())?;
    }
    let tmp = file.with_extension("vdf.gamefix-tmp");
    fs::create_dir_all(file.parent().unwrap()).map_err(|e| e.to_string())?;
    fs::write(&tmp, write(doc)).map_err(|e| e.to_string())?;
    fs::rename(&tmp, file).map_err(|e| e.to_string())
}

fn shortcut_list(doc: &mut [(Vec<u8>, Node)]) -> Result<&mut Vec<(Vec<u8>, Node)>, String> {
    match doc.iter_mut().find(|(k, _)| k.eq_ignore_ascii_case(b"shortcuts")) {
        Some((_, Node::Map(list))) => Ok(list),
        _ => Err("shortcuts.vdf: no shortcuts list, not changing it".into()),
    }
}

/// Adds the shortcut and returns its app id (what Steam's compatibility settings are keyed by).
/// The id is the one Steam itself derives from the program and the name.
pub fn add_shortcut(file: &Path, s: &Shortcut) -> Result<u32, String> {
    let mut doc = read_shortcuts(file)?;
    let list = shortcut_list(&mut doc)?;
    let exe = format!("\"{}\"", s.exe.display());
    let appid = crc32(format!("{exe}{}", s.name).as_bytes()) | 0x8000_0000;
    let text = |v: &str| Node::Str(v.as_bytes().to_vec());
    let entry = vec![
        (b"appid".to_vec(), Node::Int(appid)),
        (b"AppName".to_vec(), text(s.name)),
        (b"Exe".to_vec(), text(&exe)),
        (b"StartDir".to_vec(), text(&format!("\"{}\"", s.start_dir.display()))),
        (b"icon".to_vec(), text(s.icon)),
        (b"ShortcutPath".to_vec(), text("")),
        (b"LaunchOptions".to_vec(), text(s.launch_options)),
        (b"IsHidden".to_vec(), Node::Int(0)),
        (b"AllowDesktopConfig".to_vec(), Node::Int(1)),
        (b"AllowOverlay".to_vec(), Node::Int(1)),
        (b"OpenVR".to_vec(), Node::Int(0)),
        (b"Devkit".to_vec(), Node::Int(0)),
        (b"DevkitGameID".to_vec(), text("")),
        (b"DevkitOverrideAppID".to_vec(), Node::Int(0)),
        (b"LastPlayTime".to_vec(), Node::Int(0)),
        (b"FlatpakAppID".to_vec(), text("")),
        (b"tags".to_vec(), Node::Map(Vec::new())),
    ];
    list.retain(|(_, n)| !has_appid(n, appid));
    list.push((Vec::new(), Node::Map(entry)));
    renumber(list);
    save_shortcuts(file, &doc)?;
    Ok(appid)
}

/// Removes the shortcut with this app id; false if it was already gone.
pub fn remove_shortcut(file: &Path, appid: u32) -> Result<bool, String> {
    let mut doc = read_shortcuts(file)?;
    let list = shortcut_list(&mut doc)?;
    let before = list.len();
    list.retain(|(_, n)| !has_appid(n, appid));
    if list.len() == before {
        return Ok(false);
    }
    renumber(list);
    save_shortcuts(file, &doc)?;
    Ok(true)
}

fn has_appid(node: &Node, appid: u32) -> bool {
    matches!(node, Node::Map(fields) if fields.iter().any(|(k, v)| k.eq_ignore_ascii_case(b"appid") && *v == Node::Int(appid)))
}

/// Entries are keyed "0", "1", ... in order.
fn renumber(list: &mut [(Vec<u8>, Node)]) {
    for (i, (key, _)) in list.iter_mut().enumerate() {
        *key = i.to_string().into_bytes();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_names() {
        assert_eq!(official_name("7.0"), "proton_7");
        assert_eq!(official_name("5.13"), "proton_513");
        assert_eq!(official_name("10.0"), "proton_10");
        assert!(version_key("GE-Proton10-25") > version_key("GE-Proton10-9"));
        assert!(version_key("GE-Proton11-7-x86_64") > version_key("GE-Proton10-25"));
    }

    #[test]
    fn crc_matches_the_standard() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn round_trip_keeps_bytes() {
        let doc = vec![(
            b"shortcuts".to_vec(),
            Node::Map(vec![(b"0".to_vec(), Node::Map(vec![(b"appid".to_vec(), Node::Int(0xE083_73FA)), (b"AppName".to_vec(), Node::Str(b"Game".to_vec()))]))]),
        )];
        let bytes = write(&doc);
        assert_eq!(parse(&bytes).unwrap(), doc);
        assert_eq!(write(&parse(&bytes).unwrap()), bytes);
    }

    /// A real file, if one is given: GAMEFIX_VDF=path cargo test
    #[test]
    fn real_file_round_trips() {
        let Some(path) = std::env::var_os("GAMEFIX_VDF") else { return };
        let bytes = fs::read(path).unwrap();
        assert_eq!(write(&parse(&bytes).unwrap()), bytes);
    }
}
