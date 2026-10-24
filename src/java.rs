//! A Java for Java games when the system has none that fits: Eclipse Temurin's runtime (JRE)
//! from Adoptium (adoptium.net), unpacked into ~/.local/share/gamefix/java/<major>. It's for
//! people who'd otherwise face "brew install openjdk" in a terminal. Shared by every game, so
//! undo leaves it, like the LÖVE builds and Flathub runners.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Where gamefix keeps the Java runtimes it downloads, one folder per major version.
pub fn dir() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/share"))
        .join("gamefix/java")
}

/// The runtimes gamefix downloaded: (major version, its `java`), oldest first.
pub fn installed() -> Vec<(u32, PathBuf)> {
    let mut found: Vec<(u32, PathBuf)> = fs::read_dir(dir())
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let major = e.file_name().to_string_lossy().parse().ok()?;
            let java = e.path().join("bin/java");
            java.is_file().then_some((major, java))
        })
        .collect();
    found.sort();
    found
}

/// A Java's major version from the `release` file every OpenJDK build has next to its bin/
/// (`JAVA_VERSION="21.0.13"`, `"1.8.0_412"` for 8), following links (/usr/bin/java).
pub fn major(java: &Path) -> Option<u32> {
    let home = fs::canonicalize(java).ok()?.parent()?.parent()?.to_path_buf();
    let release = fs::read_to_string(home.join("release")).ok()?;
    let version = release.lines().find_map(|l| l.strip_prefix("JAVA_VERSION="))?.trim_matches('"');
    let mut parts = version.split(['.', '_', '+', '-']);
    match parts.next()?.parse().ok()? {
        1 => parts.next()?.parse().ok(),
        n => Some(n),
    }
}

/// Temurin's newest runtime of a major version: where to download it, its size and SHA-256.
pub struct Release {
    pub version: String,
    pub link: String,
    pub size: u64,
    sha256: String,
}

impl Release {
    /// The download's size as people read it ("52 MB").
    pub fn size_text(&self) -> String {
        format!("{} MB", (self.size + 500_000) / 1_000_000)
    }
}

const API: &str = "https://api.adoptium.net/v3/assets/latest";

/// Asks Adoptium for the newest x64 Linux runtime of `major`.
pub fn latest(major: u32) -> Result<Release, String> {
    let url = format!("{API}/{major}/hotspot?architecture=x64&image_type=jre&os=linux&vendor=eclipse");
    let out = Command::new("curl").args(["-sfL", "--max-time", "30", &url]).stdin(Stdio::null()).output().map_err(|e| format!("curl: {e}"))?;
    parse_release(&String::from_utf8_lossy(&out.stdout)).ok_or_else(|| format!("Adoptium didn't list a Java {major} runtime (no network?)"))
}

fn parse_release(json: &str) -> Option<Release> {
    let package = json.split_once("\"package\"")?.1;
    let link = crate::umu::json_field(package, "link")?;
    let sha256 = crate::umu::json_field(package, "checksum")?.to_lowercase();
    let size = package.split_once("\"size\"")?.1.trim_start().strip_prefix(':')?.trim_start().split(|c: char| !c.is_ascii_digit()).next()?.parse().ok()?;
    let version = crate::umu::json_field(json, "openjdk_version").unwrap_or_default();
    (link.starts_with("https://") && link.ends_with(".tar.gz") && sha256.len() == 64).then_some(Release { version, link, size, sha256 })
}

/// Downloads and unpacks Temurin's runtime `major` (checked against Adoptium's SHA-256) into
/// gamefix's java folder; returns its `java`. One already there is used as it is.
pub fn install(major: u32) -> Result<PathBuf, String> {
    let target = dir().join(major.to_string());
    if target.join("bin/java").is_file() {
        return Ok(target.join("bin/java"));
    }
    let failed = |what: &str| format!("Couldn't get Java {major}: {what}.");
    let release = latest(major).map_err(|e| failed(&e))?;
    let cache = PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".cache/gamefix");
    fs::create_dir_all(&cache).map_err(|e| failed(&e.to_string()))?;
    let tarball = cache.join(format!("temurin-jre-{major}-{}.tar.gz", std::process::id()));
    let downloaded = Command::new("curl").args(["-sfL", "--retry", "3", "--max-time", "3600", "-o"]).arg(&tarball).arg(&release.link).status().is_ok_and(|s| s.success());
    let result = (|| {
        if !downloaded {
            return Err(failed("the download failed"));
        }
        let sum = Command::new("sha256sum").arg(&tarball).output().map_err(|e| failed(&e.to_string()))?;
        if String::from_utf8_lossy(&sum.stdout).split_whitespace().next() != Some(release.sha256.as_str()) {
            return Err(failed("the download doesn't match its checksum"));
        }
        fs::create_dir_all(dir()).map_err(|e| failed(&e.to_string()))?;
        let unpack = dir().join(format!(".gamefix-unpack-{}", std::process::id()));
        fs::create_dir_all(&unpack).map_err(|e| failed(&e.to_string()))?;
        let unpacked = Command::new("tar").arg("-xzf").arg(&tarball).arg("-C").arg(&unpack).stderr(Stdio::null()).status().is_ok_and(|s| s.success());
        // One folder inside ("jdk-21.0.13+9-jre"), which becomes java/21.
        let top = fs::read_dir(&unpack).ok().and_then(|mut d| d.next()).and_then(|e| e.ok()).map(|e| e.path());
        let moved = unpacked && top.is_some_and(|t| t.join("bin/java").is_file() && fs::rename(&t, &target).is_ok());
        let _ = fs::remove_dir_all(&unpack);
        if !moved {
            return Err(failed("it couldn't be unpacked"));
        }
        Ok(target.join("bin/java"))
    })();
    let _ = fs::remove_file(&tarball);
    result
}

#[cfg(test)]
mod tests {
    #[test]
    fn reads_adoptiums_answer() {
        let json = r#"[{"binary": {"architecture": "x64", "image_type": "jre", "package": {"checksum": "A535A1DF9C6284D9618872984E026AE0352D1F7E9E0605425C0E6114F47351DD", "link": "https://github.com/adoptium/temurin21-binaries/releases/download/jdk-21.0.13%2B9/OpenJDK21U-jre_x64_linux_hotspot_21.0.13_9.tar.gz", "size": 52181297}}, "version": {"openjdk_version": "21.0.13+9-LTS"}}]"#;
        let r = super::parse_release(json).unwrap();
        assert_eq!((r.version.as_str(), r.size, r.size_text().as_str()), ("21.0.13+9-LTS", 52181297, "52 MB"));
        assert!(r.link.ends_with("_21.0.13_9.tar.gz") && r.sha256.starts_with("a535a1"));
        assert!(super::parse_release("[]").is_none());
    }
}
