//! What the host has installed: its libraries (from ld.so's cache), its glibc version, and
//! Steam's runtime for older games.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;

pub struct System {
    /// Library file name → bit 1 for 32-bit, bit 2 for 64-bit x86.
    libs: HashMap<String, u8>,
    pub glibc: Option<(u32, u32)>,
    pub old_runtime: Option<OldRuntime>,
}

/// Steam's first runtime ("scout", Ubuntu 12.04 libraries), which every Steam install keeps for
/// native games that need libraries newer systems dropped (libssl 1.0, libgconf, GTK 2). Steam
/// starts native games with it by itself; elsewhere its run.sh does the same: the host's
/// libraries first, the runtime's filling the gaps.
pub struct OldRuntime {
    pub run: PathBuf,
    libs: HashMap<String, u8>,
}

impl OldRuntime {
    fn find() -> Option<OldRuntime> {
        let home = PathBuf::from(std::env::var_os("HOME")?);
        // Not the Flatpak Steam's: its runtime only works inside its sandbox.
        let dir = [".local/share/Steam", ".steam/steam"].iter().map(|p| home.join(p).join("ubuntu12_32/steam-runtime")).find(|d| d.join("run.sh").is_file())?;
        let mut libs = HashMap::new();
        for (sub, bit) in [("lib/i386-linux-gnu", 1), ("usr/lib/i386-linux-gnu", 1), ("lib/x86_64-linux-gnu", 2), ("usr/lib/x86_64-linux-gnu", 2)] {
            for entry in std::fs::read_dir(dir.join(sub)).into_iter().flatten().flatten() {
                *libs.entry(entry.file_name().to_string_lossy().into_owned()).or_insert(0) |= bit;
            }
        }
        Some(OldRuntime { run: dir.join("run.sh"), libs })
    }

    pub fn has(&self, name: &str, bits: u8) -> bool {
        has(&self.libs, name, bits)
    }
}

fn has(libs: &HashMap<String, u8>, name: &str, bits: u8) -> bool {
    let bit = if bits == 64 { 2 } else { 1 };
    libs.get(name).is_some_and(|b| b & bit != 0)
}

fn run(candidates: &[&str], args: &[&str]) -> Option<String> {
    candidates.iter().find_map(|cmd| {
        let out = Command::new(cmd).args(args).output().ok()?;
        out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    })
}

impl System {
    pub fn load() -> System {
        let mut libs = HashMap::new();
        // Lines look like "\tlibGL.so.1 (libc6,x86-64) => /lib64/libGL.so.1"; 32-bit x86 is plain "(libc6)".
        for line in run(&["/usr/sbin/ldconfig", "/sbin/ldconfig", "ldconfig"], &["-p"]).unwrap_or_default().lines().skip(1) {
            let Some((name, rest)) = line.trim().split_once(" (") else { continue };
            let tag = rest.split(')').next().unwrap_or("");
            let bit = match tag.split(',').nth(1).map(str::trim) {
                Some("x86-64") => 2,
                None => 1,
                Some(a) if a.starts_with("OS ABI") || a.starts_with("hwcap") => 1,
                _ => continue,
            };
            *libs.entry(name.to_string()).or_insert(0) |= bit;
        }
        // "ldd (GNU libc) 2.43": the host's glibc, which a static build can't ask about directly.
        let glibc = run(&["/usr/bin/ldd", "ldd"], &["--version"]).and_then(|out| {
            let version = out.lines().next()?.rsplit(' ').next()?.to_string();
            let (major, minor) = version.split_once('.')?;
            Some((major.parse().ok()?, minor.parse().ok()?))
        });
        System { libs, glibc, old_runtime: OldRuntime::find() }
    }

    pub fn has(&self, name: &str, bits: u8) -> bool {
        has(&self.libs, name, bits)
    }

    pub fn refuses_exec_stack(&self) -> bool {
        self.glibc.is_some_and(|v| v >= (2, 41))
    }
}
