//! Microsoft's .NET runtime for framework-dependent programs (built to run on a .NET installed
//! on the system): found where it's usually installed, else downloaded from Microsoft into
//! `~/.local/share/gamefix/dotnet/<channel>`, shared by every game of that version and kept on
//! undo, like the LÖVE builds.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const METADATA: &str = "https://builds.dotnet.microsoft.com/dotnet/release-metadata";

/// The version a runtimeconfig.json asks for when it names Microsoft.NETCore.App (under
/// "framework", or "frameworks" from .NET 7). A WinForms/WPF program names
/// Microsoft.WindowsDesktop.App, which only exists on Windows.
pub fn framework(config: &Path) -> Option<String> {
    let text = fs::read_to_string(config).ok()?;
    if text.contains("Microsoft.WindowsDesktop.App") {
        return None;
    }
    let at = text.find("\"Microsoft.NETCore.App\"")?;
    let rest = &text[at..];
    let v = rest.find("\"version\"").map(|i| &rest[i + 9..])?;
    let v = v.trim_start_matches([' ', ':', '\n', '\r', '\t', '"']);
    Some(v.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect()).filter(|v: &String| !v.is_empty())
}

/// Microsoft's release channel for a version: "6.0.0" → "6.0", "2.1.30" → "2.1".
pub fn channel(version: &str) -> String {
    version.split('.').take(2).collect::<Vec<_>>().join(".")
}

fn folder(version: &str) -> PathBuf {
    let data = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/share"));
    data.join("gamefix/dotnet").join(channel(version))
}

/// A .NET folder (holding `dotnet` and `shared/Microsoft.NETCore.App/<version>`) with a runtime
/// of that major version: the usual install places, then gamefix's own. Its default
/// roll-forward takes a newer minor or patch, not a newer major.
pub fn installed(version: &str) -> Option<PathBuf> {
    let major = version.split('.').next().unwrap_or("");
    let mut roots: Vec<PathBuf> = ["/usr/share/dotnet", "/usr/lib64/dotnet", "/usr/lib/dotnet", "/opt/dotnet"].iter().map(PathBuf::from).collect();
    roots.extend(std::env::var_os("DOTNET_ROOT").map(PathBuf::from));
    roots.extend(std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".dotnet")));
    roots.push(folder(version));
    roots.into_iter().find(|r| {
        r.join("dotnet").is_file() && fs::read_dir(r.join("shared/Microsoft.NETCore.App")).is_ok_and(|d| d.flatten().any(|e| e.file_name().to_string_lossy().split('.').next() == Some(major)))
    })
}

/// The newest runtime of a channel for 64-bit Linux, from Microsoft's release metadata: its
/// version, download URL and SHA-512. The channel's releases.json lists the newest release
/// first, the runtime's files before the SDK's.
fn latest(json: &str) -> Option<(String, String, String)> {
    let field = |text: &str, key: &str| {
        let at = text.find(&format!("\"{key}\""))? + key.len() + 2;
        let rest = text[at..].trim_start_matches([' ', ':', '\n', '\r', '\t']);
        rest.strip_prefix('"')?.split('"').next().map(String::from)
    };
    let version = field(json, "latest-runtime")?;
    let at = json.find("\"dotnet-runtime-linux-x64.tar.gz\"")?;
    let entry = &json[at..at + json[at..].find('}')?];
    let url = field(entry, "url")?;
    let hash = field(entry, "hash")?;
    url.contains(&format!("/{version}/")).then_some((version, url, hash))
}

/// The runtime's folder, downloading it first if needed; and what that took, to tell the user.
pub fn runtime(version: &str) -> Result<(PathBuf, Option<String>), String> {
    if let Some(root) = installed(version) {
        return Ok((root, None));
    }
    let channel = channel(version);
    let failed = |what: &str| format!("Couldn't set up .NET {channel}: {what}.");
    let get = |url: &str, to: &Path| Command::new("curl").args(["-sfL", "--max-time", "300", "-o"]).arg(to).arg(url).status().is_ok_and(|s| s.success());
    let dir = folder(version);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).map_err(|e| failed(&e.to_string()))?;
    let result = (|| {
        let meta = dir.join("releases.json");
        if !get(&format!("{METADATA}/{channel}/releases.json"), &meta) {
            return Err(failed("Microsoft's release list couldn't be downloaded (no network?)"));
        }
        let (latest, url, hash) = latest(&fs::read_to_string(&meta).unwrap_or_default()).ok_or_else(|| failed("Microsoft's release list has no 64-bit Linux runtime for it"))?;
        let _ = fs::remove_file(&meta);
        let archive = dir.join("runtime.tar.gz");
        if !get(&url, &archive) {
            return Err(failed("the download from Microsoft failed"));
        }
        let sum = Command::new("sha512sum").arg(&archive).output().map_err(|e| failed(&e.to_string()))?;
        if !String::from_utf8_lossy(&sum.stdout).split_whitespace().next().is_some_and(|s| s.eq_ignore_ascii_case(&hash)) {
            return Err(failed("the download doesn't match its checksum"));
        }
        let unpacked = Command::new("tar").arg("xzf").arg(&archive).arg("-C").arg(&dir).status().is_ok_and(|s| s.success());
        let _ = fs::remove_file(&archive);
        if !unpacked || !dir.join("dotnet").is_file() {
            return Err(failed("the download couldn't be unpacked"));
        }
        fs::write(dir.join("ready"), format!(".NET runtime {latest}, from {url}\n")).map_err(|e| failed(&e.to_string()))?;
        Ok(format!("Set up Microsoft's .NET runtime {latest} in ~/.local/share/gamefix/dotnet/{channel}, for every game made for .NET {channel} (undo leaves it)."))
    })();
    match result {
        Ok(note) => Ok((dir, Some(note))),
        Err(e) => {
            let _ = fs::remove_dir_all(&dir);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_and_metadata() {
        assert_eq!(channel("6.0.0"), "6.0");
        assert_eq!(channel("2.1"), "2.1");
        let json = r#"{"channel-version": "6.0", "latest-runtime": "6.0.36", "releases": [{"release-version": "6.0.36",
            "runtime": {"files": [{"name": "dotnet-runtime-linux-arm64.tar.gz", "rid": "linux-arm64", "url": "https://x/Runtime/6.0.36/a.tar.gz", "hash": "aa"},
            {"name": "dotnet-runtime-linux-x64.tar.gz", "rid": "linux-x64", "url": "https://x/Runtime/6.0.36/dotnet-runtime-6.0.36-linux-x64.tar.gz", "hash": "afb6"}]}}]}"#;
        assert_eq!(latest(json), Some(("6.0.36".into(), "https://x/Runtime/6.0.36/dotnet-runtime-6.0.36-linux-x64.tar.gz".into(), "afb6".into())));
    }
}
