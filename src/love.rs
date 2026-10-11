//! LÖVE for games made before LÖVE 11, which changed its API so 0.x games mostly fail on it
//! (Flathub has only 11). LÖVE's own Linux builds of 0.9.2 and 0.10.2 (the .deb packages on its
//! GitHub releases, built for Ubuntu then) are unpacked once into
//! ~/.local/share/gamefix/love/<version>/, with the libraries they need that the system lacks
//! fetched into its lib/ folder (PhysFS 1; DevIL for 0.9), and shared by every game of that
//! series. Undo leaves them for the others.

use crate::binary::{self, Binary};
use crate::system::System;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

const RELEASES: &str = "https://github.com/love2d/love/releases/download";

/// (version prefix games declare, the build, its packages with their SHA-256).
const BUILDS: [(&str, &str, [(&str, &str); 2]); 2] = [
    (
        "0.10.",
        "0.10.2",
        [
            ("love_0.10.2ppa1_amd64.deb", "b8d86dd07c70642e31515b6abdc7f16bf3e9b104931a46a92cc06aaa8f1b5536"),
            ("liblove0_0.10.2ppa1_amd64.deb", "3cb507a992fd3523855e1a449a445e537df39a5080d7e1875e5a5ba0c8272c86"),
        ],
    ),
    (
        "0.9.",
        "0.9.2",
        [
            ("love_0.9.2ppa1_amd64.deb", "0f107827b27fe193bdf63d32eb64907ec21c85f3431f602d90e7b270530400cb"),
            ("liblove_0.9.2ppa1_amd64.deb", "5d5cf11255ec57176527593b63472b01c9c5a95223d88c7fd6616d2b001635b3"),
        ],
    ),
];

/// The LÖVE build gamefix can run a game made for this version with (patch releases of a
/// series kept its API: 0.10.0 and 0.10.1 games run on 0.10.2). A series alone ("0.9", guessed
/// from the code) counts too.
pub fn build_for(version: &str) -> Option<&'static str> {
    BUILDS.iter().find(|(prefix, ..)| format!("{version}.").starts_with(prefix)).map(|(_, build, _)| *build)
}

fn folder(build: &str) -> PathBuf {
    let data = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/share"));
    data.join("gamefix/love").join(build)
}

/// Whether that build is already set up here.
pub fn installed(build: &str) -> bool {
    folder(build).join("ready").is_file()
}

/// The command starting a game with the build (the game's file goes last), setting it up first
/// if needed; and what that took, to tell the user.
pub fn runtime(build: &str) -> Result<(Vec<String>, Option<String>), String> {
    let dir = folder(build);
    let libs = dir.join("lib");
    let command = vec!["env".into(), format!("LD_LIBRARY_PATH={}:{}", libs.display(), dir.join("usr/lib/x86_64-linux-gnu").display()), dir.join("usr/bin/love").display().to_string()];
    if installed(build) {
        return Ok((command, None));
    }
    let (_, _, packages) = BUILDS.iter().find(|(_, b, _)| *b == build).ok_or("no such LÖVE build")?;
    let failed = |what: &str| format!("Couldn't set up LÖVE {build}: {what}.");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&libs).map_err(|e| failed(&e.to_string()))?;
    let result = (|| {
        let work = dir.join(".download");
        for (name, sha) in packages {
            let deb = work.join(name);
            fs::create_dir_all(&work).map_err(|e| failed(&e.to_string()))?;
            let got = Command::new("curl").args(["-sfL", "--max-time", "120", "-o"]).arg(&deb).arg(format!("{RELEASES}/{build}/{name}")).status().is_ok_and(|s| s.success());
            if !got {
                return Err(failed("the download from LÖVE's releases failed (no network?)"));
            }
            let sum = Command::new("sha256sum").arg(&deb).output().map_err(|e| failed(&e.to_string()))?;
            if String::from_utf8_lossy(&sum.stdout).split_whitespace().next() != Some(sha) {
                return Err(failed(&format!("{name} doesn't match its checksum")));
            }
            if !crate::fetch::extract_deb(&deb, &dir, &work.join(format!("{name}.parts"))) {
                return Err(failed(&format!("{name} couldn't be unpacked")));
            }
        }
        let _ = fs::remove_dir_all(&work);
        // What the program and its library need that neither the build nor the system has.
        let system = System::load();
        let mut wanted: Vec<String> = [dir.join("usr/bin/love"), dir.join("usr/lib/x86_64-linux-gnu/liblove.so.0")]
            .iter()
            .filter_map(|f| match binary::identify(f) {
                Some(Binary::Elf(elf)) => Some(elf.needed),
                _ => None,
            })
            .flatten()
            .filter(|l| !system.has(l, 64) && !dir.join("usr/lib/x86_64-linux-gnu").join(l).exists())
            .collect();
        wanted.sort();
        wanted.dedup();
        let mut sources = Vec::new();
        if !wanted.is_empty() {
            let fetched = crate::fetch::fetch(&wanted, 64, &libs);
            if !fetched.missing.is_empty() {
                return Err(failed(&format!("no Fedora, Debian or Ubuntu package has {}", fetched.missing.join(", "))));
            }
            sources = fetched.sources;
        }
        fs::write(dir.join("ready"), format!("LÖVE {build}, from LÖVE's releases; libraries from: {}\n", sources.join(", "))).map_err(|e| failed(&e.to_string()))?;
        let with = if sources.is_empty() { String::new() } else { format!(", with {}", sources.join(", ")) };
        Ok(format!("Set up LÖVE {build} (LÖVE's own Linux build{with}) in ~/.local/share/gamefix/love/{build}, for every game made for it (undo leaves it)."))
    })();
    match result {
        Ok(note) => Ok((command, Some(note))),
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
    fn builds_for_versions() {
        assert_eq!(build_for("0.10.1"), Some("0.10.2"));
        assert_eq!(build_for("0.10.0"), Some("0.10.2"));
        assert_eq!(build_for("0.9.1"), Some("0.9.2"));
        assert_eq!(build_for("0.8.0"), None);
        assert_eq!(build_for("11.4"), None);
    }
}
