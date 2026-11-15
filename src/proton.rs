//! Which Proton and launch options a Windows game should run with, getting GE-Proton when
//! that's the one, and the anti-cheat systems nothing on this side can get past.
//!
//! What works for Steam games comes from ProtonDB's reports (data/protondb-proton.tsv and
//! data/protondb-options.tsv, made by tools/protondb-tables.py): for some games Steam's default
//! Proton fails on, players found that GE-Proton, an older Proton or launch options run them.

use crate::steam::{self, Steam};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const TABLE: &str = include_str!("../data/protondb-proton.tsv");
const OPTIONS: &str = include_str!("../data/protondb-options.tsv");

/// Launch options ProtonDB's reports say fix a Steam game on Steam's default Proton: settings
/// to go before `%command%` and game arguments after it, the fewest reports any of them fixed
/// it in, and the reports Steam's default failed in.
#[derive(Debug, PartialEq)]
pub struct Options {
    pub env: Vec<(String, String)>,
    pub args: Vec<String>,
    pub reports: u32,
    pub of: u32,
}

/// The launch options for a game: those for any GPU and those for the makers given (some
/// settings only help on one maker's GPUs: PROTON_HIDE_NVIDIA_GPU).
pub fn options(appid: u32, makers: &[&str]) -> Option<Options> {
    let mut found: Option<Options> = None;
    for line in OPTIONS.lines().filter(|l| !l.starts_with('#')) {
        let f: Vec<&str> = line.split('\t').collect();
        let [id, gpu, words, reports, of, ..] = f[..] else { continue };
        if id.parse::<u32>().ok() != Some(appid) || !(gpu == "any" || makers.contains(&gpu)) {
            continue;
        }
        let o = found.get_or_insert(Options { env: Vec::new(), args: Vec::new(), reports: u32::MAX, of: 0 });
        let (before, after) = words.split_once("%command%").unwrap_or((words, ""));
        for setting in before.split_whitespace() {
            if let Some((k, v)) = setting.split_once('=') {
                if !o.env.iter().any(|(e, _)| e == k) {
                    o.env.push((k.to_string(), v.to_string()));
                }
            }
        }
        // Each argument with its value, if it has one: "--rendering-driver vulkan".
        for word in after.split_whitespace() {
            match o.args.last_mut() {
                Some(last) if !word.starts_with('-') => *last = format!("{last} {word}"),
                _ => o.args.push(word.to_string()),
            }
        }
        o.reports = o.reports.min(reports.parse().unwrap_or(0));
        o.of = o.of.max(of.parse().unwrap_or(0));
    }
    found
}

/// The makers of this computer's GPUs, as the options table names them.
pub fn gpu_makers() -> Vec<&'static str> {
    let mut out = Vec::new();
    for card in fs::read_dir("/sys/class/drm").into_iter().flatten().flatten() {
        let maker = match fs::read_to_string(card.path().join("device/vendor")).unwrap_or_default().trim() {
            "0x10de" => "nvidia",
            "0x1002" => "amd",
            "0x8086" => "intel",
            _ => continue,
        };
        if !out.contains(&maker) {
            out.push(maker);
        }
    }
    out
}

impl Options {
    /// As the user would write them in Steam.
    pub fn text(&self) -> String {
        let env: Vec<String> = self.env.iter().map(|(k, v)| format!("{k}={v}")).collect();
        [env, vec!["%command%".to_string()], self.args.clone()].concat().join(" ")
    }
}

#[derive(Debug, PartialEq)]
pub enum Advice {
    Ge,
    /// One of Valve's Protons, by version ("7.0").
    Official(String),
    AntiCheat,
}

/// What ProtonDB's reports say about a Steam game: for a Proton, the reports it worked in and
/// the ones Steam's default failed in; for anti-cheat, the failing reports blaming it and all
/// failing ones.
#[derive(Debug, PartialEq)]
pub struct Hint {
    pub advice: Advice,
    pub reports: u32,
    pub of: u32,
}

pub fn hint(appid: u32) -> Option<Hint> {
    TABLE.lines().filter(|l| !l.starts_with('#')).find_map(|line| {
        let mut f = line.split('\t');
        if f.next()?.parse::<u32>().ok()? != appid {
            return None;
        }
        let advice = match f.next()? {
            "ge" => Advice::Ge,
            "anticheat" => Advice::AntiCheat,
            v => Advice::Official(v.to_string()),
        };
        Some(Hint { advice, reports: f.next()?.parse().ok()?, of: f.next()?.parse().ok()? })
    })
}

impl Hint {
    /// The Proton advised, as the user would name it.
    pub fn tool(&self) -> String {
        match &self.advice {
            Advice::Ge => "GE-Proton".into(),
            Advice::Official(v) => format!("Proton {v}"),
            Advice::AntiCheat => "anti-cheat".into(),
        }
    }

    pub fn reason(&self) -> String {
        match self.advice {
            Advice::AntiCheat => format!("{} of the {} failing reports on ProtonDB blame its anti-cheat", self.reports, self.of),
            _ => format!("on ProtonDB it worked with {} for {} players, where Steam's default Proton failed for {}", self.tool(), self.reports, self.of),
        }
    }
}

/// Anti-cheat systems found in the game's folder (their own folders, up to 3 deep): the name,
/// and whether it can work under Proton at all. Easy Anti-Cheat and BattlEye can, when the
/// developer turns that on; the others never do.
pub fn anticheat(root: &Path) -> Vec<(&'static str, bool)> {
    const KNOWN: [(&str, &str, bool); 6] = [
        ("easyanticheat", "Easy Anti-Cheat", true),
        ("battleye", "BattlEye", true),
        ("gameguard", "nProtect GameGuard", false),
        ("xigncode", "XIGNCODE3", false),
        ("anticheatexpert", "Anti-Cheat Expert", false),
        ("wellbia", "XIGNCODE3", false),
    ];
    let mut found = Vec::new();
    let mut dirs = vec![(root.to_path_buf(), 0)];
    while let Some((dir, depth)) = dirs.pop() {
        for entry in fs::read_dir(&dir).into_iter().flatten().flatten() {
            if !entry.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_lowercase();
            if let Some((_, what, proton)) = KNOWN.iter().find(|(k, _, _)| name == *k) {
                if !found.iter().any(|(w, _)| w == what) {
                    found.push((*what, *proton));
                }
            } else if depth < 3 {
                dirs.push((entry.path(), depth + 1));
            }
        }
    }
    found
}

/// DLLs Proton already sends to its own native copies (DXVK, vkd3d-proton, NVAPI), so a game's
/// own copy next to it loads anyway.
const PROTON_NATIVE: [&str; 9] = ["d3d8", "d3d9", "d3d10core", "d3d11", "d3d12", "d3d12core", "dxgi", "nvapi", "nvapi64"];
/// Windows' KnownDLLs: Windows loads its own even when a game brings a copy.
const KNOWN_DLLS: [&str; 30] = [
    "advapi32", "clbcatq", "combase", "comdlg32", "coml2", "difxapi", "gdi32", "gdiplus", "imagehlp", "imm32", "kernel32", "kernelbase", "msctf", "msvcrt", "normaliz",
    "nsi", "ntdll", "ole32", "oleaut32", "psapi", "rpcrt4", "sechost", "setupapi", "shcore", "shell32", "shlwapi", "user32", "wldap32", "ws2_32", "wow64",
];
/// Used without an installed Proton to read the list from: Windows parts that wrappers replace.
const WRAPPED: [&str; 10] = ["ddraw", "d3dimm", "dinput", "dinput8", "dsound", "winmm", "version", "opengl32", "d3d10", "d3d10_1"];

/// The files Proton's Wine has built in (DLLs, drivers: "ddraw.dll"), from the Proton games
/// that aren't Steam's run with: GE-Proton, else Valve's default.
fn wine_builtins() -> Option<Vec<String>> {
    let steam = Steam::find()?;
    let lib = steam.tool_dir(&steam.newest_ge().or_else(|| steam.proton())?)?.join("files/lib/wine");
    let mut names: Vec<String> = ["i386-windows", "x86_64-windows"]
        .iter()
        .flat_map(|d| fs::read_dir(lib.join(d)).into_iter().flatten().flatten())
        .map(|e| e.file_name().to_string_lossy().to_lowercase())
        .collect();
    names.sort();
    names.dedup();
    (!names.is_empty()).then_some(names)
}

/// The version of gilrs-core a Windows program reads controllers with, when it's one that
/// crashes under Wine as soon as a controller is connected: its Windows.Gaming.Input backend
/// (gilrs-core 0.5 to 0.6.7) unwraps RawGameController.NonRoamableId, which Wine answers with
/// E_NOTIMPL. tinywar (Bevy 0.18, gilrs-core 0.6.7) quit with code 101 five seconds in, with an
/// 8BitDo pad plugged in; 0.6.8 copes. Bevy 0.10 and later use it, as do other Rust games.
/// Builds on the older XInput backend don't name RawGameController and are fine; a build
/// whose paths were stripped (no version text) isn't flagged.
pub fn gilrs_crash(program: &Path) -> Option<String> {
    let wgi16: Vec<u8> = "Windows.Gaming.Input.RawGameController".encode_utf16().flat_map(u16::to_le_bytes).collect();
    let found = crate::binary::find_groups(program, &[&[b"Windows.Gaming.Input.RawGameController", &wgi16], &[b"gilrs-core-"]]);
    if !found.iter().all(|&f| f) {
        return None;
    }
    let version = crate::binary::find_after(program, b"gilrs-core-", 12)?;
    let version: String = String::from_utf8_lossy(&version).chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
    let parts: Vec<u32> = version.trim_end_matches('.').split('.').filter_map(|p| p.parse().ok()).collect();
    match parts[..] {
        [0, 5, _] => Some(version.trim_end_matches('.').to_string()),
        [0, 6, patch] if patch <= 7 => Some(version.trim_end_matches('.').to_string()),
        _ => None,
    }
}

/// DLLs next to a Windows program that Windows would load from there but Wine ignores: it uses
/// its own copy of any DLL it has built in unless told otherwise. Games bring such a copy when
/// it's a replacement (Empire Earth's GOG release: a ddraw.dll drawing its DirectDraw/Direct3D 7
/// through Direct3D 9; with Wine's own the picture flickers whenever the mouse moves). Copies
/// from Microsoft (redistributable leftovers) are left to Wine's own. Names without ".dll".
pub fn ignored_dlls(program: &Path) -> Vec<String> {
    let builtins = wine_builtins();
    let built_in = |name: &str| match &builtins {
        Some(list) => list.binary_search(&format!("{name}.dll")).is_ok(),
        None => WRAPPED.contains(&name),
    };
    let mut out: Vec<String> = fs::read_dir(program.parent().unwrap_or(Path::new(".")))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_lowercase().strip_suffix(".dll")?.to_string();
            let wanted = built_in(&name) && !PROTON_NATIVE.contains(&name.as_str()) && !KNOWN_DLLS.contains(&name.as_str());
            (wanted && !crate::binary::company(&e.path()).is_some_and(|c| c.to_lowercase().contains("microsoft"))).then_some(name)
        })
        .collect();
    out.sort();
    out
}

/// What provides a Windows part a game needs and Proton's Wine lacks.
#[derive(Debug, PartialEq, Clone)]
pub enum Provider {
    /// A winetricks component (protontricks installs it into the game's prefix).
    Component(&'static str),
    /// Microsoft's debug C++ runtime: only on developers' machines, never redistributable.
    DebugBuild,
    Unknown,
    /// XNA 1.0 to 3.0: Microsoft no longer offers them and winetricks has only 3.1 and 4.0 (the
    /// version, from the game's content files).
    OldXna(&'static str),
}

/// The winetricks component providing a DLL Wine lacks: Microsoft's debug runtimes are a
/// developer's build (never redistributable); else what winetricks itself says installs it
/// (`winetricks_dlls`); else the short list of what old games use, for when no winetricks is
/// found: MFC, the Visual Basic runtimes, OpenAL, PhysX's loader, an MP3 codec, Games for
/// Windows Live (gone since 2014; xliveless stands in for it, as protonfixes does for Dirt 2
/// and Fable III).
pub(crate) fn provider(dll: &str) -> Provider {
    let dll = dll.to_lowercase();
    let stem = dll.trim_end_matches(".dll");
    if ["msvcp", "msvcr", "vcruntime", "ucrtbase", "concrt", "vcomp"].iter().any(|p| stem.starts_with(p)) && stem.ends_with('d') {
        return Provider::DebugBuild;
    }
    // Games for Windows Live's servers are gone: protonfixes' stand-in, not winetricks' own
    // runtime (`gfw`).
    if stem == "xlive" {
        return Provider::Component("xliveless");
    }
    if let Some(verb) = winetricks_dlls().get(&format!("{stem}.dll")) {
        return Provider::Component(verb);
    }
    short_list(stem)
}

fn short_list(stem: &str) -> Provider {
    let stem = stem.strip_suffix('u').filter(|s| s.starts_with("mfc")).unwrap_or(stem);
    const MFC: [&str; 10] = ["mfc40", "mfc42", "mfc70", "mfc71", "mfc80", "mfc90", "mfc100", "mfc110", "mfc120", "mfc140"];
    if let Some(mfc) = MFC.iter().find(|m| **m == stem) {
        return Provider::Component(mfc);
    }
    match stem {
        "msvbvm60" => Provider::Component("vb6run"),
        "msvbvm50" => Provider::Component("vb5run"),
        "vb40032" => Provider::Component("vb4run"),
        "openal32" => Provider::Component("openal"),
        "physxloader" | "physxcore" | "physxcooking" | "physxdevice" => Provider::Component("physx"),
        "l3codeca" => Provider::Component("l3codecx"),
        "xlive" => Provider::Component("xliveless"),
        s if ["msvcp", "msvcr", "vcruntime", "ucrtbase", "concrt", "vcomp"].iter().any(|p| s.starts_with(p)) && s.ends_with('d') => Provider::DebugBuild,
        _ => Provider::Unknown,
    }
}

/// Components as winetricks takes them: protonfixes' own (xliveless, dgvoodoo2: `.verb` files in
/// a GE-Proton's protonfixes/verbs) by their file's path, which winetricks loads; it doesn't
/// know them by name.
pub fn winetricks_args(tool_dir: Option<&Path>, components: &[&str]) -> Vec<String> {
    let steam = Steam::find().unwrap_or_else(|| Steam::umu_only(&crate::setup::data_home()));
    let dirs: Vec<PathBuf> = tool_dir.map(Path::to_path_buf).into_iter().chain(steam.newest_ge().and_then(|ge| steam.tool_dir(&ge))).collect();
    components
        .iter()
        .map(|c| dirs.iter().map(|d| d.join("protonfixes/verbs").join(format!("{c}.verb"))).find(|v| v.is_file()).map_or(c.to_string(), |v| v.display().to_string()))
        .collect()
}

/// Where winetricks downloads XNA's redistributable for `component` (xna31, xna40) and its
/// SHA-256: the `w_download <url> <sha256>` line of its recipe, `load_<component>()`.
pub fn xna_download(component: &str) -> Option<(String, String)> {
    let text = fs::read_to_string(winetricks_script()?).ok()?;
    let body = text.split(&format!("\nload_{component}()")).nth(1)?.split("\n}").next()?;
    let line = body.lines().map(str::trim).find(|l| l.starts_with("w_download "))?;
    let mut words = line.split_whitespace().skip(1);
    let (url, sha) = (words.next()?.to_string(), words.next()?.to_string());
    (url.starts_with("https://") && sha.len() == 64).then_some((url, sha))
}

/// The format version of the first XNA content file (`.xnb`, or ClickOnce's `.xnb.deploy`)
/// within three folders of `dir`: the byte after "XNBw" (or another platform letter) is 1 for
/// XNA 1.0, 2 for 2.0, 3 for 3.0, 4 for 3.1, 5 for 4.0.
fn xnb_version(dir: &Path) -> Option<u8> {
    let mut todo = vec![(dir.to_path_buf(), 0)];
    while let Some((d, depth)) = todo.pop() {
        for entry in fs::read_dir(&d).into_iter().flatten().flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_lowercase();
            if path.is_dir() && depth < 3 {
                todo.push((path, depth + 1));
            } else if name.ends_with(".xnb") || name.ends_with(".xnb.deploy") {
                let mut head = [0u8; 5];
                if fs::File::open(&path).and_then(|mut f| std::io::Read::read_exact(&mut f, &mut head)).is_ok() && head.starts_with(b"XNB") {
                    return Some(head[4]);
                }
            }
        }
    }
    None
}

/// The winetricks that installs components: the newest GE-Proton's own (umu-run's `winetricks`
/// verb runs protonfixes' copy in the Proton it's given), else the system's.
fn winetricks_script() -> Option<PathBuf> {
    let steam = Steam::find().unwrap_or_else(|| Steam::umu_only(&crate::setup::data_home()));
    steam.newest_ge().and_then(|ge| steam.tool_dir(&ge)).map(|d| d.join("protonfixes/winetricks")).filter(|p| p.is_file()).or_else(|| on_path("winetricks"))
}

/// DLL (lower case, with ".dll") → winetricks component, read once from winetricks itself.
fn winetricks_dlls() -> &'static std::collections::HashMap<String, &'static str> {
    static DLLS: std::sync::OnceLock<std::collections::HashMap<String, &'static str>> = std::sync::OnceLock::new();
    DLLS.get_or_init(|| winetricks_script().and_then(|p| fs::read_to_string(p).ok()).map(|t| winetricks_map(&t)).unwrap_or_default())
}

/// Which "dlls" component installs each DLL, from winetricks' script: a component named after
/// the DLL that installs it, then the file its metadata names
/// (`installed_file1="${W_SYSTEM32_DLLS_WIN}/d3dx9_43.dll"`), then the DLLs its load function extracts (`-F 'x3daudio1_7.dll'`) or makes
/// Wine prefer (`w_override_dlls native xinput1_1 ...`, the whole msvcp140 family for
/// vcrun2022). On a tie a component that isn't only for 64-bit wins (xact over xact_x64: most
/// games needing XAudio are 32-bit), then the later one (winetricks lists older versions first).
fn winetricks_map(script: &str) -> std::collections::HashMap<String, &'static str> {
    let mut category: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    // dll → ((strength, not 64-bit only, order), component)
    let mut best: std::collections::HashMap<String, ((u8, bool, usize), String)> = std::collections::HashMap::new();
    let mut offer = |dll: String, strength: u8, order: usize, verb: &str| {
        let dll = dll.to_lowercase();
        if dll.contains('*') || !dll.ends_with(".dll") || !dll.starts_with(|c: char| c.is_ascii_alphabetic()) || dll.len() < 5 {
            return;
        }
        // Named after the DLL counts most, but only when it really installs it: riched30
        // installs RichEdit 3.0 as riched20.dll, and no riched30.dll exists.
        let strength = if dll.trim_end_matches(".dll") == verb { 3 } else { strength };
        let rank = (strength, !verb.ends_with("_x64"), order);
        if best.get(&dll).is_none_or(|(r, _)| *r < rank) {
            best.insert(dll, (rank, verb.to_string()));
        }
    };
    let mut meta: Option<String> = None;
    let mut load: Option<String> = None;
    // Wildcards a load function extracts by (`-F "mfc42*.dll"`): its own name's DLL among them
    // counts as named (mfc42 installs mfc42.dll and mfc42u.dll that way).
    let mut patterns: Vec<(String, String, usize)> = Vec::new();
    for (order, line) in script.lines().enumerate() {
        let trimmed = line.trim();
        if let Some(rest) = line.strip_prefix("w_metadata ") {
            let mut words = rest.split_whitespace();
            if let (Some(verb), Some(cat)) = (words.next(), words.next()) {
                category.insert(verb.to_string(), cat.to_string());
                meta = (cat == "dlls").then(|| verb.to_string());
            }
            continue;
        }
        if let Some(verb) = &meta {
            if let Some(value) = trimmed.strip_prefix("installed_file").and_then(|r| r.split_once('=')).map(|(_, v)| v.trim_matches(|c| c == '"' || c == '\\' || c == ' ')) {
                offer(value.rsplit('/').next().unwrap_or_default().to_string(), 2, order, verb);
            }
            if !line.trim_end().ends_with('\\') {
                meta = None;
            }
            continue;
        }
        if let Some(verb) = line.strip_prefix("load_").and_then(|r| r.strip_suffix("()")) {
            load = (category.get(verb).map(String::as_str) == Some("dlls")).then(|| verb.to_string());
            continue;
        }
        if trimmed == "}" && !line.starts_with(' ') {
            load = None;
            continue;
        }
        let Some(verb) = &load else { continue };
        if let Some(names) = trimmed.strip_prefix("w_override_dlls ") {
            for name in names.split_whitespace().skip(1).filter(|w| w.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')) {
                offer(format!("{name}.dll"), 1, order, verb);
            }
        }
        for word in trimmed.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '*')) {
            if word.to_lowercase().ends_with(".dll") {
                if word.contains('*') {
                    patterns.push((verb.clone(), word.to_lowercase(), order));
                }
                offer(word.to_string(), 1, order, verb);
            }
        }
    }
    let glob = |pattern: &str, name: &str| {
        let parts: Vec<&str> = pattern.split('*').collect();
        let mut rest = name;
        for (i, part) in parts.iter().enumerate() {
            match rest.find(part) {
                Some(at) if i > 0 || at == 0 => rest = &rest[at + part.len()..],
                _ => return false,
            }
        }
        parts.last().is_some_and(|l| l.is_empty()) || rest.is_empty()
    };
    for (verb, pattern, order) in patterns {
        let own = format!("{verb}.dll");
        if glob(&pattern, &own) {
            offer(own, 3, order, &verb);
        }
    }
    best.into_iter().map(|(dll, (_, verb))| (dll, &*Box::leak(verb.into_boxed_str()))).collect()
}

/// Follows the program's imports through the DLLs next to it (where Windows looks first), and
/// lists what's left that Proton's Wine doesn't have built in. Delay-loaded DLLs are left out:
/// a game may never use them. A .NET program referencing XNA without bringing it needs XNA
/// (Wine has none): 3.1 for programs built for .NET 2, else 4.0. Nothing without an installed
/// Proton to compare with, or when the program is packed (its real imports are compressed).
pub fn missing_parts(program: &Path) -> Vec<(String, Provider)> {
    let mut missing = Vec::new();
    let shipped: std::collections::HashMap<String, PathBuf> = fs::read_dir(program.parent().unwrap_or(Path::new(".")))
        .into_iter()
        .flatten()
        .flatten()
        // A ClickOnce folder's files end in ".deploy" (`scan::clickonce`).
        .map(|e| (e.file_name().to_string_lossy().to_lowercase().trim_end_matches(".deploy").to_string(), e.path()))
        .collect();
    let Some(first) = crate::binary::imports(program) else { return missing };
    // A reference to Microsoft's XNA assemblies carries their public key token; MonoGame and FNA
    // keep XNA's namespaces but not its signature (Picross for a Cause, MonoGame). XNA 4.0 signs
    // with 842cf8be1de50553, XNA 1.0 to 3.1 with 6d5c3888ef60e27d (ColorLogic and Skully, XNA
    // 3.1; ColorLogic ships xnafx31_redist.msi). Winetricks has 3.1 and 4.0 only. Wine has none,
    // so this doesn't wait for Proton's list of built-in DLLs.
    const XNA4: &[u8] = b"\x84\x2c\xf8\xbe\x1d\xe5\x05\x53";
    const XNA3: &[u8] = b"\x6d\x5c\x38\x88\xef\x60\xe2\x7d";
    if first.dotnet && !shipped.contains_key("microsoft.xna.framework.dll") {
        let found = crate::binary::find_groups(program, &[&[XNA4], &[XNA3]]);
        if found[0] {
            missing.push(("Microsoft.Xna.Framework.dll".into(), Provider::Component("xna40")));
        } else if found[1] {
            // 1.0 to 3.1 share the signature; the content files say which (`xnb_version`).
            // Skully's are 3.0's, which XNA 3.1 refuses ("compiled using the wrong version of
            // the XNA Framework"), so redirecting the game to 3.1 doesn't help.
            let provider = match xnb_version(program.parent().unwrap_or(Path::new("."))) {
                Some(1) => Provider::OldXna("1.0"),
                Some(2) => Provider::OldXna("2.0"),
                Some(3) => Provider::OldXna("3.0"),
                _ => Provider::Component("xna31"),
            };
            missing.push(("Microsoft.Xna.Framework.dll".into(), provider));
        }
    }
    let Some(builtins) = wine_builtins() else { return missing };
    let mut seen = std::collections::HashSet::new();
    let mut todo = vec![first];
    while let Some(file) = todo.pop() {
        for dll in file.needed {
            if dll.starts_with("api-ms-win-") || dll.starts_with("ext-ms-") || !seen.insert(dll.clone()) {
                continue;
            }
            match shipped.get(&dll) {
                Some(path) => todo.extend(crate::binary::imports(path)),
                None if builtins.binary_search(&dll).is_err() => missing.push((dll.clone(), provider(&dll))),
                None => {}
            }
        }
    }
    // PhysX 2's loader shipped without its engine (PhysXCore.dll), which comes from NVIDIA's PhysX
    // System Software: Unreal Engine 3 games load it by name at run time, so it's in no import
    // table (Alien Breed 2: PhysXLocal/PhysXLoader.dll only, "PhysX not installed or corrupt";
    // Dishonored brings both and runs).
    if !missing.iter().any(|(_, p)| *p == Provider::Component("physx")) {
        if let Some(dir) = program.parent() {
            let files = files_below(dir, 2);
            let has = |name: &str| files.iter().any(|f| f.eq_ignore_ascii_case(name));
            if has("physxloader.dll") && !has("physxcore.dll") {
                missing.push(("PhysXCore.dll".into(), Provider::Component("physx")));
            }
        }
    }
    missing
}

/// File names in a folder and the folders below it, `depth` levels down.
fn files_below(dir: &Path, depth: usize) -> Vec<String> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        match entry.file_type() {
            Ok(t) if t.is_dir() && depth > 0 => out.extend(files_below(&entry.path(), depth - 1)),
            Ok(t) if t.is_file() => out.push(entry.file_name().to_string_lossy().into_owned()),
            _ => {}
        }
    }
    out
}

/// A WINEDLLOVERRIDES value making Wine use the game's own copies first.
/// Wine's Windows.Gaming.Input turned off: games whose controller code crashes on Wine's
/// (`gilrs_crash`) start without controllers instead.
pub const NO_GAMEPAD: &str = "windows.gaming.input=d";

pub fn overrides(dlls: &[String]) -> String {
    dlls.iter().map(|d| format!("{d}=n,b")).collect::<Vec<_>>().join(";")
}

const GE_RELEASES: &str = "https://api.github.com/repos/GloriousEggroll/proton-ge-custom/releases/latest";

/// Downloads the latest GE-Proton (about 560 MB) from its GitHub releases into Steam's
/// compatibilitytools.d, checking its SHA-512, and returns its internal name. Unpacked next to
/// it first and then moved, so Steam never sees half a tool.
pub fn fetch_ge(steam: &Steam) -> Result<String, String> {
    let failed = |what: &str| format!("Couldn't get GE-Proton: {what}.");
    let release = Command::new("curl").args(["-sfL", "--max-time", "30", GE_RELEASES]).output().map_err(|e| failed(&e.to_string()))?;
    let url = tarball_url(&String::from_utf8_lossy(&release.stdout)).ok_or_else(|| failed("its release page didn't list a download (no network?)"))?;
    let file_name = url.rsplit('/').next().unwrap_or_default().to_string();
    let top = file_name.trim_end_matches(".tar.gz").to_string();
    let tools = steam.root.join("compatibilitytools.d");
    if let Some(name) = steam::custom_tool(&tools.join(&top)) {
        return Ok(name);
    }
    let sums = Command::new("curl").args(["-sfL", "--max-time", "30", &url.replace(".tar.gz", ".sha512sum")]).output().map_err(|e| failed(&e.to_string()))?;
    let expected = String::from_utf8_lossy(&sums.stdout).split_whitespace().next().unwrap_or_default().to_lowercase();
    if expected.len() != 128 {
        return Err(failed("its checksum couldn't be downloaded"));
    }
    let cache = PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".cache/gamefix");
    fs::create_dir_all(&cache).map_err(|e| failed(&e.to_string()))?;
    let tarball = cache.join(&file_name);
    let downloaded = Command::new("curl").args(["-sfL", "--retry", "3", "--max-time", "3600", "-o"]).arg(&tarball).arg(&url).status().is_ok_and(|s| s.success());
    let result = (|| {
        if !downloaded {
            return Err(failed("the download failed"));
        }
        let sum = Command::new("sha512sum").arg(&tarball).output().map_err(|e| failed(&e.to_string()))?;
        if String::from_utf8_lossy(&sum.stdout).split_whitespace().next() != Some(expected.as_str()) {
            return Err(failed("the download doesn't match its checksum"));
        }
        let unpack = tools.join(format!(".gamefix-unpack-{}", std::process::id()));
        fs::create_dir_all(&unpack).map_err(|e| failed(&e.to_string()))?;
        let unpacked = Command::new("tar").arg("-xzf").arg(&tarball).arg("-C").arg(&unpack).stderr(Stdio::null()).status().is_ok_and(|s| s.success());
        let moved = unpacked && fs::rename(unpack.join(&top), tools.join(&top)).is_ok();
        let _ = fs::remove_dir_all(&unpack);
        if !moved {
            return Err(failed("it couldn't be unpacked"));
        }
        steam::custom_tool(&tools.join(&top)).ok_or_else(|| failed("the unpacked folder isn't a Steam compatibility tool"))
    })();
    let _ = fs::remove_file(&tarball);
    result
}

/// umu-launcher's own release, for systems that don't install umu-run (Ubuntu, Fedora, Arch:
/// only Bazzite and umu's own packages do): one Python file, pinned and checked by its SHA-256,
/// the release Lobby uses.
const UMU_VERSION: &str = "1.4.4";
const UMU_URL: &str = "https://github.com/Open-Wine-Components/umu-launcher/releases/download/1.4.4/umu-launcher-1.4.4-zipapp.tar";
const UMU_SHA256: &str = "eb590691841f7fad3fc3ad8fd5db4ccb87849fe7948e62b28ece7a4ee48cc851";

fn on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path).map(|d| d.join(name)).find(|p| p.is_file())
}

/// umu-run if it's here: the system's, gamefix's own download, or Lobby's.
pub fn umu_run_found() -> Option<PathBuf> {
    let data = crate::setup::data_home();
    on_path("umu-run").or_else(|| [data.join(format!("gamefix/tools/umu-run-{UMU_VERSION}")), data.join("lobby/tools/bin/umu-run")].into_iter().find(|p| p.is_file()))
}

/// Where umu-run is, or where gamefix would download it.
pub fn umu_run_path() -> PathBuf {
    umu_run_found().unwrap_or_else(|| crate::setup::data_home().join(format!("gamefix/tools/umu-run-{UMU_VERSION}")))
}

/// Whether AppArmor stops umu-run's sandbox here: Ubuntu 23.10 and later let only programs with
/// a rule make the user namespaces bubblewrap needs, and Proton runs inside pressure-vessel's
/// bubblewrap. Ubuntu 26.04 brings a rule for /usr/bin/bwrap that lets it through; 24.04 has
/// none ("bwrap: setting up uid map: Permission denied"). Steam has its own rule, so its
/// shortcuts start either way. `allow_sandbox` adds a rule for umu-run.
pub fn umu_blocked() -> bool {
    if fs::read_to_string("/proc/sys/kernel/apparmor_restrict_unprivileged_userns").map(|v| v.trim() != "1").unwrap_or(true) {
        return false;
    }
    let umu = umu_run_path();
    let named = format!("\"{}\"", fs::canonicalize(&umu).unwrap_or(umu).display());
    let has_rule = fs::read_dir("/etc/apparmor.d").into_iter().flatten().flatten().any(|e| fs::read_to_string(e.path()).is_ok_and(|t| t.contains(&named)));
    let bwrap_works = Command::new("bwrap").args(["--unshare-user", "--ro-bind", "/", "/", "true"]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success());
    !has_rule && !bwrap_works
}

/// The bwrap app-menu entries start (gamefix's fixes for old Unity games): the system's where
/// it can make a sandbox; where AppArmor stops it (Ubuntu 24.04 has no rule for it: "setting
/// up uid map: Permission denied"), gamefix's copy once `allow_sandbox` gave it a rule; else
/// none. Steam's shortcuts don't need this: Ubuntu's rule for Steam carries over to what Steam
/// starts.
pub fn menu_bwrap() -> Option<PathBuf> {
    let system = Path::new(SYSTEM_BWRAP);
    if sandbox_works(system) {
        return Some(system.to_path_buf());
    }
    if !own_bwrap_path().exists() {
        return None;
    }
    own_bwrap().filter(|b| sandbox_works(b))
}

const SYSTEM_BWRAP: &str = "/usr/bin/bwrap";

fn sandbox_works(bwrap: &Path) -> bool {
    Command::new(bwrap).args(["--dev-bind", "/", "/", "true"]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
}

fn own_bwrap_path() -> PathBuf {
    crate::setup::data_home().join("gamefix/tools/bwrap")
}

/// gamefix's copy of the system's bwrap (~/.local/share/gamefix/tools/bwrap), made again when
/// the system's changes, so the rule `allow_sandbox` adds can name it without opening
/// /usr/bin/bwrap to everything (Lobby does the same).
fn own_bwrap() -> Option<PathBuf> {
    let copy = own_bwrap_path();
    let source = fs::metadata(SYSTEM_BWRAP).ok()?;
    let same = fs::metadata(&copy).is_ok_and(|c| c.len() == source.len() && c.modified().ok() == source.modified().ok());
    if !same {
        let part = copy.with_extension("part");
        fs::create_dir_all(copy.parent()?).ok()?;
        fs::copy(SYSTEM_BWRAP, &part).ok()?;
        if let Ok(time) = source.modified() {
            let _ = fs::File::options().write(true).open(&part).and_then(|f| f.set_modified(time));
        }
        fs::rename(&part, &copy).ok()?;
    }
    Some(copy)
}

/// Adds AppArmor rules letting umu-run (downloaded first if there's none) and gamefix's copy of
/// bwrap make user namespaces, as Steam's own rule does for Steam: /etc/apparmor.d/gamefix-umu,
/// through sudo in a terminal, else pkexec (asks for the password in a window). The rules name
/// files in the home folder, which anything running as the user could replace: they open user
/// namespaces to those paths, nothing more (Lobby's rule makes the same trade).
pub fn allow_sandbox() -> Result<Vec<String>, String> {
    let mut done = Vec::new();
    if fs::read_to_string("/proc/sys/kernel/apparmor_restrict_unprivileged_userns").map(|v| v.trim() != "1").unwrap_or(true) {
        done.push("This system doesn't restrict sandboxes: nothing to allow.".into());
        return Ok(done);
    }
    let umu = umu_run(&mut done)?;
    let umu = fs::canonicalize(&umu).unwrap_or(umu);
    let bwrap = own_bwrap().map(|b| fs::canonicalize(&b).unwrap_or(b));
    for path in std::iter::once(&umu).chain(bwrap.as_ref()) {
        if path.to_string_lossy().contains(['"', '\n', '\\']) {
            return Err(format!("This path has a character an AppArmor rule can't hold: {}", path.display()));
        }
    }
    let profile = |name: &str, path: &Path| format!("profile {name} \"{}\" flags=(unconfined) {{\n  userns,\n\n  include if exists <local/{name}>\n}}\n", path.display());
    let mut rule = format!("# Written by gamefix: lets umu-run, which starts Windows games with Proton outside Steam,\n# and gamefix's copy of bwrap (fixes for old Unity games) make the user namespaces their\n# sandboxes need. Remove this file to take it back.\nabi <abi/4.0>,\ninclude <tunables/global>\n\n{}", profile("gamefix-umu", &umu));
    if let Some(b) = &bwrap {
        rule.push('\n');
        rule.push_str(&profile("gamefix-bwrap", b));
    }
    let tmp = std::env::temp_dir().join(format!("gamefix-umu-{}", std::process::id()));
    fs::write(&tmp, rule).map_err(|e| e.to_string())?;
    let script = "install -m 644 \"$1\" /etc/apparmor.d/gamefix-umu && apparmor_parser -r /etc/apparmor.d/gamefix-umu";
    let terminal = std::io::IsTerminal::is_terminal(&std::io::stdin());
    let mut command = Command::new(if terminal { "sudo" } else { "pkexec" });
    command.args(["sh", "-c", script, "sh"]).arg(&tmp);
    let status = command.status();
    let _ = fs::remove_file(&tmp);
    match status {
        Ok(s) if s.success() => {
            let what = if bwrap.is_some() { format!("{} and {}", umu.display(), bwrap.as_ref().unwrap().display()) } else { umu.display().to_string() };
            done.push(format!("Added an AppArmor rule for {what} (/etc/apparmor.d/gamefix-umu): Windows games and the fixes for old Unity games now work from the app menu (set those games up again). Remove that file to take it back."));
            Ok(done)
        }
        Ok(_) => Err("The rule wasn't added (the password was refused or the window was closed). Nothing changed.".into()),
        Err(e) => Err(format!("Couldn't ask for the password ({}): {e}", if terminal { "sudo" } else { "pkexec" })),
    }
}

/// umu-run, downloading umu's release into ~/.local/share/gamefix/tools when there's none
/// (it then runs with the system's python3). What was downloaded goes into `done`. Shared by
/// every game and kept on undo, like GE-Proton.
pub fn umu_run(done: &mut Vec<String>) -> Result<PathBuf, String> {
    if let Some(found) = umu_run_found() {
        return Ok(found);
    }
    let failed = |what: &str| format!("umu-run (umu-launcher) starts Windows games outside Steam, and this system has none; gamefix couldn't download it: {what}.");
    if on_path("python3").is_none() {
        return Err(format!("umu-run (umu-launcher) starts Windows games outside Steam, and it needs Python: {}.", crate::distro::how_to_install(&crate::distro::PYTHON, "it")));
    }
    let cache = PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".cache/gamefix");
    fs::create_dir_all(&cache).map_err(|e| failed(&e.to_string()))?;
    let tarball = cache.join(format!("umu-launcher-{UMU_VERSION}-zipapp.tar"));
    let downloaded = Command::new("curl").args(["-sfL", "--retry", "3", "--max-time", "600", "-o"]).arg(&tarball).arg(UMU_URL).status().is_ok_and(|s| s.success());
    let result = (|| {
        if !downloaded {
            return Err(failed("the download failed (no network?)"));
        }
        let sum = Command::new("sha256sum").arg(&tarball).output().map_err(|e| failed(&e.to_string()))?;
        if String::from_utf8_lossy(&sum.stdout).split_whitespace().next() != Some(UMU_SHA256) {
            return Err(failed("the download doesn't match its checksum"));
        }
        let out = Command::new("tar").arg("-xOf").arg(&tarball).arg("umu/umu-run").stderr(Stdio::null()).output().map_err(|e| failed(&e.to_string()))?;
        if !out.status.success() || out.stdout.is_empty() {
            return Err(failed("it couldn't be unpacked"));
        }
        let program = crate::setup::data_home().join(format!("gamefix/tools/umu-run-{UMU_VERSION}"));
        fs::create_dir_all(program.parent().unwrap()).map_err(|e| failed(&e.to_string()))?;
        let part = program.with_extension("part");
        use std::os::unix::fs::PermissionsExt;
        fs::write(&part, &out.stdout).and_then(|()| fs::set_permissions(&part, fs::Permissions::from_mode(0o755))).and_then(|()| fs::rename(&part, &program)).map_err(|e| failed(&e.to_string()))?;
        done.push(format!("Downloaded umu-launcher {UMU_VERSION} (umu-run, which starts Windows games with Proton outside Steam) into {}; undo leaves it, other games use it too.", program.display()));
        Ok(program)
    })();
    let _ = fs::remove_file(&tarball);
    result
}

/// The x86_64 tarball among a GitHub release's downloads (older releases have no arch in the name).
fn tarball_url(release: &str) -> Option<String> {
    release
        .split("\"browser_download_url\"")
        .skip(1)
        .filter_map(|rest| rest.split('"').nth(1))
        .find(|url| url.ends_with(".tar.gz") && !url.contains("aarch64"))
        .map(String::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn winetricks_list() {
        let script = r#"
w_metadata d3dx9_43 dlls \
    title="MS d3dx9_43.dll" \
    installed_file1="${W_SYSTEM32_DLLS_WIN}/d3dx9_43.dll"

load_d3dx9_43()
{
    helper_d3dx9_xx 43
}

w_metadata xinput dlls \
    title="Microsoft XInput" \
    installed_file1="${W_SYSTEM32_DLLS_WIN}/xinput1_1.dll"

load_xinput()
{
    w_try_cabextract -d "${W_SYSTEM32_DLLS}" -L -F 'xinput*.dll' "${x}"
    w_override_dlls native xinput1_3
}

w_metadata vcrun2019 dlls \
    installed_file1="${W_SYSTEM32_DLLS_WIN}/vcruntime140.dll"

load_vcrun2019()
{
    w_override_dlls native,builtin msvcp140 vcruntime140
}

w_metadata vcrun2022 dlls \
    installed_file1="${W_SYSTEM32_DLLS_WIN}/vcruntime140.dll"

load_vcrun2022()
{
    w_override_dlls native,builtin msvcp140 vcruntime140
    w_try mv -f "${W_TMP}/win32/msvcp140_2.dll_x86" "${W_SYSTEM32_DLLS}/msvcp140_2.dll"
}

w_metadata steam apps \
    installed_file1="${W_PROGRAMS_X86_WIN}/Steam/steam.dll"
"#;
        let map = winetricks_map(script);
        assert_eq!(map.get("d3dx9_43.dll").copied(), Some("d3dx9_43"));
        assert_eq!(map.get("xinput1_1.dll").copied(), Some("xinput"));
        assert_eq!(map.get("xinput1_3.dll").copied(), Some("xinput"));
        // The newer component wins; apps aren't components for DLLs.
        assert_eq!(map.get("msvcp140.dll").copied(), Some("vcrun2022"));
        assert_eq!(map.get("vcruntime140.dll").copied(), Some("vcrun2022"));
        assert_eq!(map.get("msvcp140_2.dll").copied(), Some("vcrun2022"));
        assert_eq!(map.get("steam.dll"), None);
        // A name alone isn't a file: riched30 installs riched20.dll.
        let r = winetricks_map("w_metadata riched30 dlls \\\n    installed_file1=\"${W_SYSTEM32_DLLS_WIN}/riched20.dll\"\n\nload_riched30()\n{\n    w_override_dlls native,builtin riched20\n}\n");
        assert_eq!((r.get("riched30.dll"), r.get("riched20.dll").copied()), (None, Some("riched30")));
        // Its own DLL through a wildcard: mfc42 over vcrun6sp6, which names mfc42.dll.
        let m = winetricks_map("w_metadata mfc42 dlls \\\n    installed_file1=\"${W_SYSTEM32_DLLS_WIN}/mfc42u.dll\"\n\nload_mfc42()\n{\n    w_try_cabextract x -d y -F \"mfc42*.dll\"\n}\n\nw_metadata vcrun6sp6 dlls \\\n    installed_file1=\"${W_SYSTEM32_DLLS_WIN}/mfc42.dll\"\n");
        assert_eq!(m.get("mfc42.dll").copied(), Some("mfc42"));
        let x = winetricks_map("w_metadata xact dlls \\\n    installed_file1=\"${W_SYSTEM32_DLLS_WIN}/xactengine3_7.dll\"\n\nw_metadata xact_x64 dlls \\\n    installed_file1=\"${W_SYSTEM32_DLLS_WIN}/xactengine3_7.dll\"\n");
        assert_eq!(x.get("xactengine3_7.dll").copied(), Some("xact"));
        // A real winetricks: GAMEFIX_WINETRICKS=<script> cargo test -- --nocapture
        if let Ok(path) = std::env::var("GAMEFIX_WINETRICKS") {
            let map = winetricks_map(&fs::read_to_string(path).unwrap());
            eprintln!("{} DLLs", map.len());
            if std::env::var("GAMEFIX_WINETRICKS_ALL").is_ok() {
                let mut all: Vec<_> = map.iter().collect();
                all.sort();
                for (dll, verb) in all {
                    eprintln!("ALL {dll} {verb}");
                }
            }
            for dll in ["d3dx9_43.dll", "xinput1_3.dll", "msvcp140.dll", "mfc42u.dll", "mfc42.dll", "msvbvm60.dll", "physxloader.dll", "xactengine3_7.dll", "d3dcompiler_47.dll", "x3daudio1_7.dll", "openal32.dll", "l3codeca.acm", "mscoree.dll", "vb6run.dll", "xlive.dll", "msxml4.dll", "quartz.dll", "devenum.dll", "atl71.dll", "riched30.dll", "riched20.dll"] {
                eprintln!("  {dll} -> {:?}", map.get(dll));
            }
        }
    }

    #[test]
    fn table_reads() {
        assert!(TABLE.lines().filter(|l| !l.starts_with('#')).all(|l| l.split('\t').count() == 5));
        let first = TABLE.lines().find(|l| !l.starts_with('#')).unwrap();
        let appid: u32 = first.split('\t').next().unwrap().parse().unwrap();
        assert!(hint(appid).is_some());
        assert_eq!(hint(0), None);
    }

    #[test]
    fn options_table_reads() {
        for line in OPTIONS.lines().filter(|l| !l.starts_with('#')) {
            let f: Vec<&str> = line.split('\t').collect();
            assert_eq!(f.len(), 6, "{line}");
            assert!(f[2].contains("%command%"));
            let appid: u32 = f[0].parse().unwrap();
            let o = options(appid, &[f[1]]).unwrap();
            assert!(o.reports >= 2 && !(o.env.is_empty() && o.args.is_empty()));
        }
        // Supreme Commander: two settings; Horse Magnifier: an argument with its value.
        assert_eq!(options(9350, &[]).map(|o| o.text()).as_deref(), Some("PROTON_NO_ESYNC=1 PROTON_NO_FSYNC=1 %command%"));
        assert_eq!(options(4585340, &[]).map(|o| o.args), Some(vec!["--rendering-driver vulkan".to_string()]));
        // NVIDIA-only: nothing without an NVIDIA GPU.
        assert!(options(1088850, &["amd"]).is_none());
        assert!(options(1088850, &["nvidia"]).is_some());
    }

    #[test]
    fn providers() {
        assert_eq!(provider("mfc42u.dll"), Provider::Component("mfc42"));
        assert_eq!(provider("mfc140.dll"), Provider::Component("mfc140"));
        assert_eq!(provider("msvbvm60.dll"), Provider::Component("vb6run"));
        assert_eq!(provider("physxloader.dll"), Provider::Component("physx"));
        assert_eq!(provider("msvcp140d.dll"), Provider::DebugBuild);
        assert_eq!(provider("ucrtbased.dll"), Provider::DebugBuild);
        assert_eq!(provider("xlive.dll"), Provider::Component("xliveless"));
        assert_eq!(provider("somegame.dll"), Provider::Unknown);
    }

    #[test]
    fn release_tarball() {
        let release = r#"{"assets": [{"name": "GE-Proton11-7-aarch64.tar.gz", "browser_download_url": "https://x/GE-Proton11-7-aarch64.tar.gz"},
            {"browser_download_url": "https://x/GE-Proton11-7-x86_64.sha512sum"}, {"browser_download_url": "https://x/GE-Proton11-7-x86_64.tar.gz"}]}"#;
        assert_eq!(tarball_url(release).as_deref(), Some("https://x/GE-Proton11-7-x86_64.tar.gz"));
        assert_eq!(tarball_url(r#"{"browser_download_url": "https://x/GE-Proton10-25.tar.gz"}"#).as_deref(), Some("https://x/GE-Proton10-25.tar.gz"));
    }
}
