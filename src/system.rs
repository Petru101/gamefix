//! What the host has installed: its libraries (from ld.so's cache), its glibc version, and
//! Steam's runtime for older games.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct System {
    /// Library file name → bit 1 for 32-bit, bit 2 for 64-bit x86.
    libs: HashMap<String, u8>,
    /// (library file name, 32 or 64) → where ld.so's cache has it.
    paths: HashMap<(String, u8), String>,
    pub glibc: Option<(u32, u32)>,
    pub old_runtime: Option<OldRuntime>,
}

/// Steam's first runtime ("scout", Ubuntu 12.04 libraries), which every Steam install keeps for
/// native games that need libraries newer systems dropped (libssl 1.0, libgconf, GTK 2). Steam
/// starts native games with it by itself; elsewhere its run.sh does the same: the host's
/// libraries first, the runtime's filling the gaps.
pub struct OldRuntime {
    pub run: PathBuf,
    dir: PathBuf,
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
        Some(OldRuntime { run: dir.join("run.sh"), dir, libs })
    }

    pub fn has(&self, name: &str, bits: u8) -> bool {
        has(&self.libs, name, bits)
    }

    /// Its 32-bit ALSA plugins for PulseAudio (pcm, ctl), which PipeWire also serves.
    pub fn alsa_pulse32(&self) -> Option<(PathBuf, PathBuf)> {
        let dir = self.dir.join("usr/lib/i386-linux-gnu/alsa-lib");
        let (pcm, ctl) = (dir.join("libasound_module_pcm_pulse.so"), dir.join("libasound_module_ctl_pulse.so"));
        (pcm.is_file() && ctl.is_file()).then_some((pcm, ctl))
    }

    /// Where the runtime keeps a library, for preloading it.
    pub fn path(&self, name: &str, bits: u8) -> Option<PathBuf> {
        let arch = if bits == 64 { "x86_64-linux-gnu" } else { "i386-linux-gnu" };
        [format!("lib/{arch}"), format!("usr/lib/{arch}")].iter().map(|d| self.dir.join(d).join(name)).find(|p| p.exists())
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
        let mut paths = HashMap::new();
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
            if let Some((_, path)) = rest.split_once(" => ") {
                paths.entry((name.to_string(), if bit == 2 { 64 } else { 32 })).or_insert_with(|| path.trim().to_string());
            }
        }
        // "ldd (GNU libc) 2.43": the host's glibc, which a static build can't ask about directly.
        let glibc = run(&["/usr/bin/ldd", "ldd"], &["--version"]).and_then(|out| {
            let version = out.lines().next()?.rsplit(' ').next()?.to_string();
            let (major, minor) = version.split_once('.')?;
            Some((major.parse().ok()?, minor.parse().ok()?))
        });
        System { libs, paths, glibc, old_runtime: OldRuntime::find() }
    }

    pub fn has(&self, name: &str, bits: u8) -> bool {
        has(&self.libs, name, bits)
    }

    /// Where the system's copy of a library is, for preloading it.
    pub fn path(&self, name: &str, bits: u8) -> Option<&str> {
        self.paths.get(&(name.to_string(), bits)).map(String::as_str)
    }

    /// Whether `java` is on the PATH or in Homebrew (Bazzite's way to install it).
    pub fn java(&self) -> Option<PathBuf> {
        let path = std::env::var_os("PATH").unwrap_or_default();
        std::env::split_paths(&path).chain([PathBuf::from("/home/linuxbrew/.linuxbrew/bin")]).map(|d| d.join("java")).find(|j| j.is_file())
    }

    /// How to start LÖVE: Flathub's (Bazzite's way to install it; its sandbox can read the whole
    /// system) or one on the PATH.
    pub fn love(&self) -> Option<Vec<String>> {
        let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
        let flatpak = [PathBuf::from("/var/lib/flatpak"), home.join(".local/share/flatpak")].iter().any(|d| d.join("app/org.love2d.love2d/current").exists());
        if flatpak {
            return Some(vec!["flatpak".into(), "run".into(), "org.love2d.love2d".into()]);
        }
        let path = std::env::var_os("PATH").unwrap_or_default();
        std::env::split_paths(&path).map(|d| d.join("love")).find(|l| l.is_file()).map(|l| vec![l.display().to_string()])
    }

    /// Whether 32-bit programs find no sound device through ALSA: its default device is
    /// PipeWire's plugin, which Fedora and Bazzite only install in 64-bit.
    pub fn alsa32_broken(&self) -> bool {
        let pipewire_default = ["/etc/alsa/conf.d/99-pipewire-default.conf", "/usr/share/alsa/alsa.conf.d/99-pipewire-default.conf"].iter().any(|p| Path::new(p).exists());
        let plugin32 = ["/usr/lib/alsa-lib", "/usr/lib/i386-linux-gnu/alsa-lib", "/usr/lib32/alsa-lib"].iter().any(|d| Path::new(d).join("libasound_module_pcm_pipewire.so").exists());
        pipewire_default && !plugin32
    }

    pub fn refuses_exec_stack(&self) -> bool {
        self.glibc.is_some_and(|v| v >= (2, 41))
    }
}
