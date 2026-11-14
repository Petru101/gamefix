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
}

/// The winetricks component providing a DLL Wine lacks, from what old games use: MFC, the
/// Visual Basic runtimes, OpenAL, PhysX's loader, an MP3 codec, Games for Windows Live (gone
/// since 2014; xliveless stands in for it, as protonfixes does for Dirt 2 and Fable III).
fn provider(dll: &str) -> Provider {
    let stem = dll.trim_end_matches(".dll");
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

/// Follows the program's imports through the DLLs next to it (where Windows looks first), and
/// lists what's left that Proton's Wine doesn't have built in. Delay-loaded DLLs are left out:
/// a game may never use them. A .NET program referencing XNA without bringing it needs XNA
/// (Wine has none): 3.1 for programs built for .NET 2, else 4.0. Nothing without an installed
/// Proton to compare with, or when the program is packed (its real imports are compressed).
pub fn missing_parts(program: &Path) -> Vec<(String, Provider)> {
    let mut missing = Vec::new();
    let Some(builtins) = wine_builtins() else { return missing };
    let shipped: std::collections::HashMap<String, PathBuf> = fs::read_dir(program.parent().unwrap_or(Path::new(".")))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| (e.file_name().to_string_lossy().to_lowercase(), e.path()))
        .collect();
    let Some(first) = crate::binary::imports(program) else { return missing };
    // A reference to Microsoft's XNA assemblies carries their public key token; MonoGame and FNA
    // keep XNA's namespaces but not its signature (Picross for a Cause, MonoGame).
    const XNA_TOKEN: &[u8] = b"\x84\x2c\xf8\xbe\x1d\xe5\x05\x53";
    if first.dotnet && crate::binary::contains(program, XNA_TOKEN) && !shipped.contains_key("microsoft.xna.framework.dll") {
        let v2 = crate::binary::dotnet_runtime(program).is_some_and(|v| v.starts_with("v2."));
        missing.push(("Microsoft.Xna.Framework.dll".into(), Provider::Component(if v2 { "xna31" } else { "xna40" })));
    }
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
/// shortcuts start either way. `allow_umu` adds a rule for umu-run.
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

/// Adds an AppArmor rule letting umu-run (downloaded first if there's none) make user
/// namespaces, as Steam's own rule does for Steam: /etc/apparmor.d/gamefix-umu, through sudo
/// in a terminal, else pkexec (asks for the password in a window). The rule names a file in
/// the home folder, which anything running as the user could replace: it opens user namespaces
/// to that one path, nothing more (Lobby's rule makes the same trade).
pub fn allow_umu() -> Result<Vec<String>, String> {
    let mut done = Vec::new();
    let umu = umu_run(&mut done)?;
    let umu = fs::canonicalize(&umu).unwrap_or(umu);
    if umu.to_string_lossy().contains(['"', '\n', '\\']) {
        return Err(format!("umu-run's path has a character an AppArmor rule can't hold: {}", umu.display()));
    }
    if fs::read_to_string("/proc/sys/kernel/apparmor_restrict_unprivileged_userns").map(|v| v.trim() != "1").unwrap_or(true) {
        done.push("This system doesn't restrict Proton's sandbox: nothing to allow.".into());
        return Ok(done);
    }
    let rule = format!("# Written by gamefix: lets umu-run, which starts Windows games with Proton outside Steam,\n# make the user namespaces Proton's sandbox needs. Remove this file to take it back.\nabi <abi/4.0>,\ninclude <tunables/global>\n\nprofile gamefix-umu \"{}\" flags=(unconfined) {{\n  userns,\n\n  include if exists <local/gamefix-umu>\n}}\n", umu.display());
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
            done.push(format!("Added an AppArmor rule for {} (/etc/apparmor.d/gamefix-umu): Windows games now start from the app menu. Remove that file to take it back.", umu.display()));
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
