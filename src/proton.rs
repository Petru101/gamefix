//! Which Proton a Windows game should run with, getting GE-Proton when that's the one, and the
//! anti-cheat systems nothing on this side can get past.
//!
//! What works for Steam games comes from ProtonDB's reports (data/protondb-proton.tsv, made by
//! tools/protondb-proton.py): for many games Steam's default Proton fails on, players found
//! that GE-Proton or an older Proton runs them.

use crate::steam::{self, Steam};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const TABLE: &str = include_str!("../data/protondb-proton.tsv");

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
    fn release_tarball() {
        let release = r#"{"assets": [{"name": "GE-Proton11-7-aarch64.tar.gz", "browser_download_url": "https://x/GE-Proton11-7-aarch64.tar.gz"},
            {"browser_download_url": "https://x/GE-Proton11-7-x86_64.sha512sum"}, {"browser_download_url": "https://x/GE-Proton11-7-x86_64.tar.gz"}]}"#;
        assert_eq!(tarball_url(release).as_deref(), Some("https://x/GE-Proton11-7-x86_64.tar.gz"));
        assert_eq!(tarball_url(r#"{"browser_download_url": "https://x/GE-Proton10-25.tar.gz"}"#).as_deref(), Some("https://x/GE-Proton10-25.tar.gz"));
    }
}
