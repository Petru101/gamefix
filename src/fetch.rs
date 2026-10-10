//! Libraries neither the system nor Steam's runtime has, taken from a distribution's packages:
//! Fedora's first (the release Bazzite is built on, so they match the system), then Debian's
//! for names Fedora doesn't have (Debian-only names like liblua5.2.so.0, or older versions).
//! Unpacked into gamefix's own folder for the game, never installed into the system.

use crate::binary::{self, Binary};
use crate::system::System;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Debian releases to look in, oldest supported first: the older the glibc a library was built
/// against, the surer it runs on this one.
const DEBIAN_SUITES: [&str; 3] = ["bookworm", "trixie", "bullseye"];

pub struct Fetched {
    /// Files written into the folder (libraries and their links), for undo.
    pub files: Vec<PathBuf>,
    /// Where they came from, like "Fedora's allegro5".
    pub sources: Vec<String>,
    /// Libraries still found nowhere, the asked-for ones or what the fetched ones need.
    pub missing: Vec<String>,
}

/// Fetches `libs` (and what they need that the system lacks) into `dest`.
pub fn fetch(libs: &[String], bits: u8, dest: &Path) -> Fetched {
    let system = System::load();
    let tmp = std::env::temp_dir().join(format!("gamefix-fetch-{}", std::process::id()));
    let root = tmp.join("root");
    let mut out = Fetched { files: Vec::new(), sources: Vec::new(), missing: Vec::new() };
    let mut want: Vec<String> = libs.to_vec();
    let mut tried: BTreeSet<String> = BTreeSet::new();
    // A few rounds: a fetched library may need another one the system lacks.
    for _ in 0..4 {
        want.retain(|l| tried.insert(l.clone()));
        if want.is_empty() {
            break;
        }
        let _ = fs::remove_dir_all(&tmp);
        if fs::create_dir_all(&root).is_err() {
            out.missing.append(&mut want);
            break;
        }
        // One package at a time, so one that won't download doesn't take the others with it;
        // Debian's copy when Fedora has none or it fails.
        for lib in std::mem::take(&mut want) {
            if let Some(pkg) = fedora_package(&lib, bits) {
                let dir = tmp.join("rpms").join(&pkg);
                if fedora_download(&pkg, bits, &dir) {
                    for rpm in fs::read_dir(&dir).into_iter().flatten().flatten() {
                        extract_rpm(&rpm.path(), &root);
                    }
                    out.sources.push(format!("Fedora's {pkg}"));
                    continue;
                }
            }
            match debian_package(&lib, bits) {
                Some((suite, pkg, url)) => {
                    let deb = tmp.join(format!("{pkg}.deb"));
                    if download(&url, &deb) && extract_deb(&deb, &root, &tmp.join(&pkg)) {
                        out.sources.push(format!("Debian {suite}'s {pkg}"));
                    }
                }
                None => out.missing.push(lib),
            }
        }
        copy_libs(&root, bits, dest, &mut out.files);
        // What the copied libraries need that's nowhere yet.
        want = needs(dest, bits, &system);
    }
    let _ = fs::remove_dir_all(&tmp);
    // Asked for but not there after all (a package that didn't download or unpack).
    for lib in libs {
        if !dest.join(lib).exists() && !out.missing.contains(lib) {
            out.missing.push(lib.clone());
        }
    }
    out.missing.retain(|l| !dest.join(l).exists());
    out.missing.sort();
    out.missing.dedup();
    out
}

/// The Fedora package providing a library, from the configured repositories (`dnf5 repoquery`).
fn fedora_package(lib: &str, bits: u8) -> Option<String> {
    let (capability, arch) = if bits == 64 { (format!("{lib}()(64bit)"), "x86_64") } else { (lib.to_string(), "i686") };
    let out = Command::new("dnf5").args(["repoquery", "-q", "--arch", arch, "--whatprovides", &capability]).stderr(Stdio::null()).output().ok()?;
    // "allegro5-0:5.2.11.3-1.fc44.x86_64": the name is what's left of the last two dashes.
    let nevra = String::from_utf8_lossy(&out.stdout).lines().next()?.trim().to_string();
    nevra.rsplitn(3, '-').nth(2).map(String::from)
}

/// Downloads the package and what it needs that the system doesn't have installed.
fn fedora_download(pkg: &str, bits: u8, dir: &Path) -> bool {
    let arch = if bits == 64 { "x86_64" } else { "i686" };
    Command::new("dnf5")
        .args(["download", "-q", "--resolve", "--arch", arch, "--arch", "noarch", "--destdir"])
        .arg(dir)
        .arg(pkg)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// The Debian package with a file of that name (packages.debian.org's contents search), and
/// where to download it.
fn debian_package(lib: &str, bits: u8) -> Option<(&'static str, String, String)> {
    let arch = if bits == 64 { "amd64" } else { "i386" };
    let keyword = lib.replace('+', "%2B");
    DEBIAN_SUITES.iter().find_map(|&suite| {
        let page = get(&format!("https://packages.debian.org/search?searchon=contents&keywords={keyword}&mode=exactfilename&suite={suite}&arch={arch}"))?;
        let marker = format!("<a href=\"/{suite}/");
        let start = page.find(&marker)? + marker.len();
        let pkg = page[start..].split('"').next()?.to_string();
        let download = get(&format!("https://packages.debian.org/{suite}/{arch}/{pkg}/download"))?;
        // Mirror links look like http://ftp.us.debian.org/debian/pool/main/l/lua5.2/x.deb.
        let path = download.split('"').find(|s| s.contains("/debian/pool/") && s.ends_with(".deb"))?;
        let pool = &path[path.find("/pool/")?..];
        Some((suite, pkg, format!("https://deb.debian.org/debian{pool}")))
    })
}

fn get(url: &str) -> Option<String> {
    let out = Command::new("curl").args(["-sfL", "--max-time", "30", url]).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

fn download(url: &str, to: &Path) -> bool {
    Command::new("curl").args(["-sfL", "--max-time", "120", "-o"]).arg(to).arg(url).status().is_ok_and(|s| s.success())
}

/// A .deb is an `ar` archive holding data.tar.*: unpack that into `root`.
fn extract_deb(deb: &Path, root: &Path, work: &Path) -> bool {
    if fs::create_dir_all(work).is_err() || !Command::new("ar").arg("x").arg(deb).current_dir(work).status().is_ok_and(|s| s.success()) {
        return false;
    }
    let Some(data) = fs::read_dir(work).into_iter().flatten().flatten().map(|e| e.path()).find(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("data.tar"))) else { return false };
    Command::new("tar").arg("-xf").arg(&data).arg("-C").arg(root).status().is_ok_and(|s| s.success())
}

/// `rpm2cpio x.rpm | cpio -idm` in `root`.
fn extract_rpm(rpm: &Path, root: &Path) {
    let Ok(mut cpio_in) = Command::new("rpm2cpio").arg(rpm).stdout(Stdio::piped()).spawn() else { return };
    if let Some(stdout) = cpio_in.stdout.take() {
        let _ = Command::new("cpio").args(["-idm", "--quiet"]).current_dir(root).stdin(stdout).stderr(Stdio::null()).status();
    }
    let _ = cpio_in.wait();
}

/// Copies the libraries (files and their version links) from the unpacked packages.
fn copy_libs(root: &Path, bits: u8, dest: &Path, files: &mut Vec<PathBuf>) {
    let dirs: &[&str] = if bits == 64 { &["usr/lib64", "lib64", "usr/lib/x86_64-linux-gnu", "lib/x86_64-linux-gnu"] } else { &["usr/lib", "lib", "usr/lib/i386-linux-gnu", "lib/i386-linux-gnu"] };
    for dir in dirs {
        for entry in fs::read_dir(root.join(dir)).into_iter().flatten().flatten() {
            let name = entry.file_name();
            let to = dest.join(&name);
            if !name.to_string_lossy().contains(".so") || to.symlink_metadata().is_ok() {
                continue;
            }
            let copied = match fs::read_link(entry.path()) {
                Ok(target) => std::os::unix::fs::symlink(target, &to).is_ok(),
                Err(_) => entry.file_type().is_ok_and(|t| t.is_file()) && fs::copy(entry.path(), &to).is_ok(),
            };
            if copied {
                files.push(to);
            }
        }
    }
}

/// Libraries the ones in `dest` need that neither the system nor `dest` has.
fn needs(dest: &Path, bits: u8, system: &System) -> Vec<String> {
    let mut out = BTreeSet::new();
    for entry in fs::read_dir(dest).into_iter().flatten().flatten() {
        if let Some(Binary::Elf(elf)) = binary::identify(&entry.path()) {
            for lib in elf.needed.iter().filter(|l| elf.bits == bits && !system.has(l, bits) && !dest.join(l).exists()) {
                out.insert(lib.clone());
            }
        }
    }
    out.into_iter().collect()
}
