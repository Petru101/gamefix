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

/// Whether a folder is Steam's old runtime with its libraries, not Steam's stub (run.sh only).
pub fn real_runtime(dir: &Path) -> bool {
    dir.join("run.sh").is_file() && (dir.join("usr/lib/x86_64-linux-gnu").is_dir() || dir.join("lib/x86_64-linux-gnu").is_dir())
}

/// Whether a path names the old runtime's run.sh, wherever Steam keeps it.
pub fn runtime_run(path: &str) -> bool {
    let path = Path::new(path);
    path.file_name().is_some_and(|n| n == "run.sh") && path.parent().and_then(Path::file_name).is_some_and(|d| d == "steam-runtime" || d == "LegacySteamRuntime")
}

/// An emulator: its name, Flathub app, program name outside Flathub, and the arguments that go
/// before the game's file.
pub struct Emulator {
    pub name: &'static str,
    pub app: &'static str,
    pub program: &'static str,
    pub args: &'static [&'static str],
}

/// The emulator for a console's games (`binary::console_image`'s names), one each, all on
/// Flathub and needing no BIOS: mGBA for the Game Boys; ares (one app, `--system` picking the
/// console; `--no-file-prompt` skips its question about a 64DD disc) for NES, Super Nintendo,
/// Nintendo 64, Mega Drive, Master System, Game Gear, PC Engine, Atari 2600; melonDS for the
/// DS; Dolphin for GameCube and Wii (`-b`: quit with the game, `-e`: run this file); PPSSPP for
/// the PSP; Flycast for the Dreamcast (its own BIOS stand-in plays most games). None for the
/// PlayStation: DuckStation and PCSX2 won't start without Sony's BIOS, which can't be
/// downloaded legally.
pub fn emulator_for(console: &str) -> Option<Emulator> {
    let ares = |system: &'static [&'static str]| Emulator { name: "ares", app: "dev.ares.ares", program: "ares", args: system };
    Some(match console {
        "Game Boy" | "Game Boy Color" | "Game Boy Advance" => Emulator { name: "mGBA", app: "io.mgba.mGBA", program: "mgba-qt", args: &[] },
        "NES" => ares(&["--system", "Famicom"]),
        "Super Nintendo" => ares(&["--system", "Super Famicom"]),
        "Nintendo 64" => ares(&["--system", "Nintendo 64", "--no-file-prompt"]),
        "Mega Drive" => ares(&["--system", "Mega Drive"]),
        "Master System" => ares(&["--system", "Master System"]),
        "Game Gear" => ares(&["--system", "Game Gear"]),
        "PC Engine" => ares(&["--system", "PC Engine"]),
        "Atari 2600" => ares(&["--system", "Atari 2600"]),
        "Nintendo DS" => Emulator { name: "melonDS", app: "net.kuribo64.melonDS", program: "melonDS", args: &[] },
        "GameCube" | "Wii" | "GameCube or Wii" => Emulator { name: "Dolphin", app: "org.DolphinEmu.dolphin-emu", program: "dolphin-emu", args: &["-b", "-e"] },
        "PSP" => Emulator { name: "PPSSPP", app: "org.ppsspp.PPSSPP", program: "PPSSPPSDL", args: &[] },
        "Dreamcast" => Emulator { name: "Flycast", app: "org.flycast.Flycast", program: "flycast", args: &[] },
        _ => return None,
    })
}

/// "a" or "an" before a console's name, as it's said ("an NES game", "an Atari 2600 game").
pub fn article(console: &str) -> &'static str {
    if console == "NES" || console.starts_with(['A', 'E', 'I', 'O', 'U']) { "an" } else { "a" }
}

/// Whether a Flathub app is installed, for the system or the user.
pub fn flatpak_installed(app: &str) -> bool {
    let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    [PathBuf::from("/var/lib/flatpak"), home.join(".local/share/flatpak")].iter().any(|d| d.join("app").join(app).join("current").exists())
}

impl OldRuntime {
    pub(crate) fn find() -> Option<OldRuntime> {
        let home = PathBuf::from(std::env::var_os("HOME")?);
        // Not the Flatpak Steam's: its runtime only works inside its sandbox. Steam's own copy
        // first, if it's still the real runtime: in November 2026 Steam replaced it with a stub
        // (run.sh -> ../legacy-run-stub.sh, "Running ... on the host, not in the legacy
        // runtime"), pointing to steamapps/common/LegacySteamRuntime. Else that, else the scout
        // container tool's own copy (SteamLinuxRuntime/steam-runtime: run.sh, lib/, usr/lib/),
        // in whichever Steam library holds it.
        let real = |d: &Path| real_runtime(d);
        let mut candidates: Vec<PathBuf> = [".local/share/Steam", ".steam/steam"].iter().map(|p| home.join(p).join("ubuntu12_32/steam-runtime")).collect();
        let steam = crate::steam::Steam::find().filter(|s| !s.is_flatpak());
        for library in steam.map(|s| s.libraries()).unwrap_or_default() {
            let common = library.join("steamapps/common");
            candidates.extend([common.join("LegacySteamRuntime"), common.join("LegacySteamRuntime/steam-runtime"), common.join("SteamLinuxRuntime/steam-runtime")]);
        }
        let dir = candidates.into_iter().find(|d| real(d))?;
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

    /// Whether `java` is on the PATH or in Homebrew (Bazzite's way to install it), else the
    /// newest runtime gamefix downloaded.
    pub fn java(&self) -> Option<PathBuf> {
        let path = std::env::var_os("PATH").unwrap_or_default();
        std::env::split_paths(&path).chain([PathBuf::from("/home/linuxbrew/.linuxbrew/bin")]).map(|d| d.join("java")).find(|j| j.is_file()).or_else(|| crate::java::installed().pop().map(|(_, j)| j))
    }

    /// Mono, for .NET games built to run with it on Linux: on the PATH, or Homebrew's (`brew
    /// install mono`), which menu entries' PATH lacks.
    pub fn mono(&self) -> Option<PathBuf> {
        let path = std::env::var_os("PATH").unwrap_or_default();
        std::env::split_paths(&path).chain([PathBuf::from("/home/linuxbrew/.linuxbrew/bin")]).map(|d| d.join("mono")).find(|m| m.is_file())
    }

    /// Homebrew's newest openjdk@N up to 25: version strings with dots and applets still in,
    /// which old Processing games need (Java 26 removed applets; "27" has no dot).
    pub fn java_lts(&self) -> Option<PathBuf> {
        self.java_up_to(25)
    }

    /// Homebrew's newest openjdk@N up to `max`, or a runtime gamefix downloaded.
    pub fn java_up_to(&self, max: u32) -> Option<PathBuf> {
        let opt = Path::new("/home/linuxbrew/.linuxbrew/opt");
        let mut found: Vec<(u32, PathBuf)> = std::fs::read_dir(opt)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let n: u32 = e.file_name().to_string_lossy().strip_prefix("openjdk@")?.parse().ok()?;
                let java = e.path().join("bin/java");
                (n <= max && java.is_file()).then_some((n, java))
            })
            .collect();
        found.extend(crate::java::installed().into_iter().filter(|(n, _)| *n <= max));
        found.sort();
        found.pop().map(|(_, j)| j)
    }

    /// A Java from `min` to `max`: the one `java()` finds when it's in range, else the newest
    /// in range among Homebrew's openjdk@N and the runtimes gamefix downloaded. A Java whose
    /// version can't be read counts only when there's no upper limit and the jar runs on 8.
    pub fn java_between(&self, min: u32, max: u32) -> Option<PathBuf> {
        let fits = |j: &Path| Self::java_major(j).map_or(max == u32::MAX && min <= 8, |v| (min..=max).contains(&v));
        if let Some(java) = self.java().filter(|j| fits(j)) {
            return Some(java);
        }
        self.java_up_to(max).filter(|j| fits(j))
    }

    /// A Java's major version: one of Homebrew's openjdk@N by its folder, any other by its
    /// `release` file.
    pub fn java_major(java: &Path) -> Option<u32> {
        let homebrew = || java.to_string_lossy().split("/openjdk@").nth(1)?.split('/').next()?.parse().ok();
        homebrew().or_else(|| crate::java::major(java))
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

    /// How to start Ruffle, the Flash player: Flathub's (it can read the home folder only, so
    /// setup adds the game's folder) or one on the PATH.
    pub fn ruffle(&self) -> Option<Vec<String>> {
        let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
        let flatpak = [PathBuf::from("/var/lib/flatpak"), home.join(".local/share/flatpak")].iter().any(|d| d.join("app/rs.ruffle.Ruffle/current").exists());
        if flatpak {
            return Some(vec!["flatpak".into(), "run".into(), "rs.ruffle.Ruffle".into()]);
        }
        let path = std::env::var_os("PATH").unwrap_or_default();
        std::env::split_paths(&path).map(|d| d.join("ruffle")).find(|l| l.is_file()).map(|l| vec![l.display().to_string()])
    }

    /// How to start TIC-80, which plays .tic cartridges: Flathub's (it can read the home folder
    /// only, so setup adds the game's folder) or a `tic80` on the PATH. `--skip` leaves out
    /// its start-up animation.
    pub fn tic80(&self) -> Option<Vec<String>> {
        let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
        let flatpak = [PathBuf::from("/var/lib/flatpak"), home.join(".local/share/flatpak")].iter().any(|d| d.join("app/com.tic80.TIC_80/current").exists());
        if flatpak {
            return Some(vec!["flatpak".into(), "run".into(), "com.tic80.TIC_80".into(), "--skip".into()]);
        }
        let path = std::env::var_os("PATH").unwrap_or_default();
        std::env::split_paths(&path).map(|d| d.join("tic80")).find(|l| l.is_file()).map(|l| vec![l.display().to_string(), "--skip".into()])
    }

    /// How to start the emulator that plays a console's games (`emulator_for`): its Flathub app
    /// if installed (system or user), with its arguments, else its program on the PATH. None
    /// when it isn't installed, or no emulator here plays it without a BIOS (PlayStation).
    pub fn emulator(&self, console: &str) -> Option<Vec<String>> {
        let e = emulator_for(console)?;
        if flatpak_installed(e.app) {
            return Some(["flatpak", "run", e.app].iter().chain(e.args).map(|s| s.to_string()).collect());
        }
        let path = std::env::var_os("PATH").unwrap_or_default();
        std::env::split_paths(&path).map(|d| d.join(e.program)).find(|l| l.is_file()).map(|l| std::iter::once(l.display().to_string()).chain(e.args.iter().map(|s| s.to_string())).collect())
    }

    /// How to start ScummVM, which plays classic adventure games from their data files.
    pub fn scummvm(&self) -> Option<Vec<String>> {
        if flatpak_installed("org.scummvm.ScummVM") {
            return Some(vec!["flatpak".into(), "run".into(), "org.scummvm.ScummVM".into()]);
        }
        let path = std::env::var_os("PATH").unwrap_or_default();
        std::env::split_paths(&path).map(|d| d.join("scummvm")).find(|l| l.is_file()).map(|l| vec![l.display().to_string()])
    }

    /// How to start EasyRPG Player, which plays RPG Maker 2000/2003 games: Flathub's (it sees
    /// the home folder; setup adds the game's), its player itself, since the Flatpak's own
    /// command runs a folder-picking wizard on first start even when given a game; else an
    /// `easyrpg-player` on the PATH. The game's folder goes after `--project-path`.
    pub fn easyrpg(&self) -> Option<Vec<String>> {
        let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
        let flatpak = [PathBuf::from("/var/lib/flatpak"), home.join(".local/share/flatpak")].iter().any(|d| d.join("app/org.easyrpg.player/current").exists());
        if flatpak {
            return Some(vec!["flatpak".into(), "run".into(), "--command=org.easyrpg.player.real".into(), "org.easyrpg.player".into()]);
        }
        let path = std::env::var_os("PATH").unwrap_or_default();
        std::env::split_paths(&path).map(|d| d.join("easyrpg-player")).find(|l| l.is_file()).map(|l| vec![l.display().to_string()])
    }

    /// How to start DOSBox: Flathub's DOSBox Staging (it can read the home folder only, so
    /// setup adds the game's folder), else a `dosbox` on the PATH (Staging or the original).
    pub fn dosbox(&self) -> Option<Vec<String>> {
        let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
        let flatpak = [PathBuf::from("/var/lib/flatpak"), home.join(".local/share/flatpak")].iter().any(|d| d.join("app/io.github.dosbox-staging/current").exists());
        if flatpak {
            return Some(vec!["flatpak".into(), "run".into(), "io.github.dosbox-staging".into()]);
        }
        let path = std::env::var_os("PATH").unwrap_or_default();
        std::env::split_paths(&path).map(|d| d.join("dosbox")).find(|l| l.is_file()).map(|l| vec![l.display().to_string()])
    }

    /// PipeWire's ALSA plugin for this many bits, when ALSA's default goes to PipeWire. Java's
    /// own sound needs it named by path: it opens the card itself (hw/plughw, which PipeWire
    /// holds), and Homebrew's Java brings an ALSA library that only looks in Homebrew's folder.
    pub fn pipewire_alsa(&self, bits: u8) -> Option<PathBuf> {
        let pipewire_default = ["/etc/alsa/conf.d/99-pipewire-default.conf", "/usr/share/alsa/alsa.conf.d/99-pipewire-default.conf"].iter().any(|p| Path::new(p).exists());
        let dirs: &[&str] = if bits == 64 { &["/usr/lib64/alsa-lib", "/usr/lib/x86_64-linux-gnu/alsa-lib"] } else { &["/usr/lib/alsa-lib", "/usr/lib/i386-linux-gnu/alsa-lib", "/usr/lib32/alsa-lib"] };
        let plugin = dirs.iter().map(|d| Path::new(d).join("libasound_module_pcm_pipewire.so")).find(|p| p.is_file());
        plugin.filter(|_| pipewire_default)
    }

    pub fn refuses_exec_stack(&self) -> bool {
        self.glibc.is_some_and(|v| v >= (2, 41))
    }
}
