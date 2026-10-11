//! Sets a game up once so it starts from the app menu and/or Steam, and undoes that.
//!
//! Launch-time fixes go into the entry's command or Steam's launch options as plain settings,
//! so they work without gamefix. What was changed is recorded under ~/.local/share/gamefix/games/.

use crate::check::{Problem, Report};
use crate::proton::{self, Advice};
use crate::scan::Game;
use crate::steam::{self, Steam};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub struct Outcome {
    pub done: Vec<String>,
    pub not_fixed: Vec<String>,
}

/// Where the game should be startable from.
pub struct Targets {
    pub menu: bool,
    pub steam: bool,
}

const BWRAP: &str = "/usr/bin/bwrap";

/// Shell code that leaves `"$@"` as a bwrap command hiding the command line of every program
/// started as `/proc/self/exe` (Steam's and Chrome's helpers), which old Unity mistakes for
/// itself. Everything else, Steam included, stays visible: a private PID namespace hid Steam
/// too, so Steamworks games hung and Steam lost track of them. The helpers' numbers change, so
/// they are looked up at each launch.
const HIDE_HELPERS: &str = "set -- /usr/bin/bwrap --dev-bind / /; for p in /proc/[0-9]*; do [ \"$(tr '\\0' '\\n' < $p/cmdline 2>/dev/null | head -n 1)\" = /proc/self/exe ] && set -- \"$@\" --ro-bind /dev/null $p/cmdline; done;";

/// HIDE_HELPERS with more bwrap arguments (shell-quoted here) after the root's.
fn hide_helpers_with(extra: &[String]) -> String {
    let quoted: Vec<String> = extra.iter().map(|a| format!("'{}'", a.replace('\'', "'\\''"))).collect();
    HIDE_HELPERS.replacen("--dev-bind / /;", &format!("--dev-bind / / {};", quoted.join(" ")), 1).replace(" / / ;", " / /;")
}

const STEAM_OPEN: &str = "Steam is open. Close it first (Steam → Exit): it rewrites its game list when it quits, which would undo the change.";

fn data_home() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/share"))
}

/// File-name-safe form of a game's name. Letters of any alphabet stay ("ᗢ" is a game), and a
/// name with nothing usable falls back to a number, so records never collide on an empty name.
fn slug(name: &str) -> String {
    let s: String = name.chars().map(|c| if c.is_alphanumeric() { c.to_lowercase().next().unwrap_or(c) } else { '-' }).collect();
    let s = s.split('-').filter(|p| !p.is_empty()).collect::<Vec<_>>().join("-");
    if s.is_empty() { format!("game-{:08x}", name.bytes().fold(0u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32))) } else { s }
}

fn record_path(game: &Game) -> PathBuf {
    data_home().join("gamefix/games").join(format!("{}.log", slug(&game.name)))
}

/// One argument of a desktop entry's Exec line, quoted by the spec's rules: inside double
/// quotes `"`, `` ` ``, `$` and `\` take a backslash, and string escaping then doubles every `\`.
pub fn exec_arg(arg: &str) -> String {
    let arg = arg.replace('%', "%%");
    if !arg.is_empty() && arg.chars().all(|c| c.is_ascii_alphanumeric() || "/-_.=+:,".contains(c)) {
        return arg;
    }
    let mut out = String::from("\"");
    for c in arg.chars() {
        match c {
            '"' | '`' | '$' => out.push_str(&format!("\\\\{c}")),
            '\\' => out.push_str("\\\\\\\\"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Desktop entry string values can't hold newlines; the name and paths come from the game.
fn value(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\n', " ")
}

fn icon(game: &Game, main: &Path) -> String {
    let dir = game.root.join(main.parent().unwrap_or(Path::new("")));
    let stem = main.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let unity = dir.join(format!("{stem}_Data/Resources/UnityPlayer.png"));
    if unity.is_file() {
        return unity.display().to_string();
    }
    "applications-games".into()
}

pub fn setup(game: &Game, report: &Report, targets: &Targets) -> Result<Outcome, String> {
    if record_path(game).exists() {
        if targets.steam && Steam::find().is_some_and(|s| s.running()) {
            return Err(STEAM_OPEN.into());
        }
        undo(game)?;
        // The report saw the game as the last setup left it: undo takes back the exec bit, the
        // converted launcher and case links, which this setup must then redo (Brownie Cove, set
        // up twice, got "Permission denied"). And before anything new is made: a runner's
        // setup undoing later removed the Python environment it had just installed.
        return setup(game, &crate::check::check(game, &crate::system::System::load()), targets);
    }
    if report.problems.iter().any(|p| matches!(p, Problem::UnityDataMissing(_))) {
        return Err("The download lacks the game's data folder (see gamefix check): nothing to set up.".into());
    }
    if let Some(Problem::LinuxInstaller(file)) = report.problems.iter().find(|p| matches!(p, Problem::LinuxInstaller(_))) {
        return setup_unpacked(game, file, targets);
    }
    let Some((main, _)) = game.main() else {
        // A game another program runs: that program and the file to give it.
        let java_audio = crate::check::uses_java_sound(game);
        let runner = report.problems.iter().find_map(|p| match p {
            Problem::Java { file, java: Some(java) } if file.extension().is_some_and(|e| e == "jar") => {
                let mut command = java_sound(vec![java.display().to_string(), "-jar".into()], java_audio);
                let mut setting = |s: String| {
                    command = match command[0].as_str() {
                        "env" => [vec!["env".into(), s], command[1..].to_vec()].concat(),
                        _ => [vec!["env".into(), s], command.clone()].concat(),
                    };
                };
                // Mesa reads the game's mismatched shaders as GLSL 1.30 (Aloisius); other drivers
                // ignore the setting.
                if crate::binary::jar_needs(&game.root.join(file)).shader.is_some() {
                    setting("force_glsl_version=130".into());
                }
                // A launcher inside the jar may start the game in a second `java` found on the
                // PATH (JarSplice: Dungeon Colony), which Homebrew's isn't on in menu entries.
                if let Some(dir) = java.parent().filter(|d| !matches!(d.to_str(), Some("/usr/bin" | "/bin"))) {
                    setting(format!("PATH={}:/usr/bin:/bin", dir.display()));
                }
                Some((command, file, "Java"))
            }
            // A Processing export's script runs plain `java`, which may be Homebrew's, off the
            // PATH menu entries get; and its exec bit is often lost.
            Problem::Java { file, java: Some(java) } => {
                let dir = java.parent().map(|d| d.display().to_string()).unwrap_or_default();
                Some((java_sound(vec!["env".into(), format!("PATH={dir}:/usr/bin:/bin"), "/bin/sh".into()], java_audio), file, "Java"))
            }
            Problem::Love { file, version, love: Some(love) } if !crate::check::old_love(version.as_deref()) => Some((love.clone(), file, "LÖVE")),
            // Flathub's Ruffle sees only the home folder; give it the game's folder too.
            Problem::Flash { file, ruffle: Some(ruffle) } => {
                let mut command = ruffle.clone();
                if command[0] == "flatpak" {
                    let dir = game.root.join(file).parent().map(|d| d.display().to_string()).unwrap_or_default();
                    command.insert(2, format!("--filesystem={dir}:ro"));
                }
                Some((command, file, "Ruffle"))
            }
            Problem::Tic { file, tic80: Some(tic80) } => {
                let mut command = tic80.clone();
                if command[0] == "flatpak" {
                    let dir = game.root.join(file).parent().map(|d| d.display().to_string()).unwrap_or_default();
                    command.insert(2, format!("--filesystem={dir}:ro"));
                }
                Some((command, file, "TIC-80"))
            }
            // Flathub's DOSBox likewise, writable: DOS games keep their settings and saves in
            // their folder. Staging quits with the game; the original DOSBox needs -exit for that.
            Problem::Dos { file, dosbox: Some(dosbox) } => {
                let mut command = dosbox.clone();
                if command[0] == "flatpak" {
                    let dir = game.root.join(file).parent().map(|d| d.display().to_string()).unwrap_or_default();
                    command.insert(2, format!("--filesystem={dir}"));
                }
                command.push("-exit".into());
                Some((command, file, "DOSBox"))
            }
            _ => None,
        });
        // A game made for LÖVE 0.9 or 0.10: LÖVE's own build of that series, set up once.
        let old_love = report.problems.iter().find_map(|p| match p {
            Problem::Love { file, version: Some(v), .. } if crate::check::old_love(Some(v)) => crate::love::build_for(v).map(|b| (file, b)),
            _ => None,
        });
        let python = report.problems.iter().find_map(|p| match p {
            Problem::Python { file, v2: false, plan: Some(plan) } => Some((file, plan)),
            _ => None,
        });
        let disc = report.problems.iter().find_map(|p| match p {
            Problem::Disc { file, installer: Some(installer), .. } => Some(Installer::Disc { iso: game.root.join(file), inside: installer.clone() }),
            _ => None,
        });
        // A game another program runs comes first, as in the report: Tikotep's download carries
        // a 35 KB Windows .exe that only starts its jar.
        return match game.windows {
            _ if runner.is_some() => {
                let (command, file, name) = runner.unwrap();
                let mut extra = Extra::default();
                // The folder a Java game keeps its files in and may not make (Sheep Farm
                // Simulator); undo removes it only while it's still empty.
                if file.extension().is_some_and(|e| e == "jar") {
                    if let Some(dir) = crate::binary::jar_needs(&game.root.join(file)).home_dir {
                        let path = PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(&dir);
                        if !path.exists() {
                            fs::create_dir(&path).map_err(|e| format!("Couldn't create {}: {e}", path.display()))?;
                            extra.record.push(format!("created-dir\t{}", path.display()));
                            extra.done.push(format!("Created ~/{dir}, the folder it keeps its files in."));
                        }
                    }
                }
                setup_runner(game, command, file, name, targets, extra)
            }
            _ if old_love.is_some() => {
                let (file, build) = old_love.unwrap();
                let (command, note) = crate::love::runtime(build)?;
                let mut outcome = setup_runner(game, command, file, &format!("LÖVE {build}"), targets, Extra::default())?;
                outcome.done.splice(0..0, note);
                Ok(outcome)
            }
            None if python.is_some() => {
                let (file, plan) = python.unwrap();
                setup_python(game, file, plan, targets)
            }
            Some(i) => setup_windows(game, report, &game.files[i], targets),
            None if game.cd_installer.is_some() => setup_install(game, Installer::File(game.full(&game.files[game.cd_installer.unwrap()])), targets),
            None if disc.is_some() => setup_install(game, disc.unwrap(), targets),
            None if !report.installers.is_empty() => setup_install(game, Installer::File(game.root.join(&report.installers[0].0)), targets),
            None if report.problems.iter().any(|p| matches!(p, Problem::Disc { .. })) => Err("No installer was found on the disc image (see gamefix check).".into()),
            None if report.problems.iter().any(|p| matches!(p, Problem::Archive(_))) => Err("The game is still packed in an archive: unpack it, then set up the folder it makes.".into()),
            None if report.problems.iter().any(|p| matches!(p, Problem::Web(_))) => Err("It's a web game: play it on its itch.io page.".into()),
            None if report.problems.iter().any(|p| matches!(p, Problem::Source(..))) => Err("This is the game's source code: it has to be built first.".into()),
            None if report.problems.iter().any(|p| matches!(p, Problem::Package(_, "apk"))) => Err("It's an Android app; get the game's Linux or Windows download instead.".into()),
            None if report.problems.iter().any(|p| matches!(p, Problem::Package(..))) => Err("It's a package to install into the system; get the game's .tar.gz, .zip or AppImage download instead.".into()),
            None if runner_needed(report).is_some() => Err(runner_needed(report).unwrap()),
            None => Err("No game program in this folder. If it's an installer, install the game first and set up the folder it installs to.".into()),
        };
    };
    if report.problems.iter().any(|p| matches!(p, Problem::WrongMachine(_))) {
        return Err("The program is built for a different processor; nothing can make it run here.".into());
    }
    // A text game runs in a terminal, which Steam doesn't open.
    let terminal = report.problems.iter().any(|p| matches!(p, Problem::Terminal { .. }));
    if terminal && targets.steam && !targets.menu {
        return Err("It's a text game that runs in a terminal, which Steam doesn't open: set it up for the app menu.".into());
    }
    let steam = if targets.steam && !terminal {
        let steam = Steam::find().ok_or("Steam isn't installed, or nobody has logged in to it yet.")?;
        if steam.running() {
            return Err(STEAM_OPEN.into());
        }
        Some(steam)
    } else {
        None
    };
    let program = game.full(main);
    // Launch-time settings, as (name, value).
    let mut env: Vec<(&str, String)> = Vec::new();
    // Whether to start it through HIDE_HELPERS, and what else bwrap does then.
    let mut hide_helpers = false;
    let mut bwrap_extra: Vec<String> = Vec::new();
    // Steam's runtime for older games: Steam itself doesn't pass its libraries on to games.
    let mut old_runtime = None;
    // Libraries to load first, joined into one LD_PRELOAD.
    let mut preload: Vec<String> = Vec::new();
    let mut done = Vec::new();
    let mut not_fixed = Vec::new();
    let mut record = Vec::new();
    // Inside an AppImage only what's fixed at launch applies: the files there can't be changed.
    let inside = report.appimage.iter().flat_map(|(_, problems)| problems).filter(|p| {
        matches!(
            p,
            Problem::OwnLibstdcxx(Some(_)) | Problem::OwnHarfbuzz(_, Some(_), _) | Problem::Shadowed(.., Some(_)) | Problem::ExecStack(_) | Problem::DotnetIcu(_) | Problem::NwjsFonts(_) | Problem::OldPulse(_) | Problem::Alsa32(Some(_))
        )
    });
    for problem in report.problems.iter().chain(inside) {
        match problem {
            Problem::NotExecutable(_) => {
                let mode = fs::metadata(&program).map_err(|e| e.to_string())?.permissions().mode();
                fs::set_permissions(&program, fs::Permissions::from_mode(mode | 0o111)).map_err(|e| e.to_string())?;
                record.push(format!("mode\t{:o}\t{}", mode & 0o7777, program.display()));
                done.push(format!("Marked {} as a program.", main.path.display()));
            }
            Problem::ExecStack(_) => {
                env.push(("GLIBC_TUNABLES", "glibc.rtld.execstack=2".into()));
                done.push("Lets its libraries that ask for an executable stack load (at launch).".into());
            }
            Problem::MissingFolders(_, folders) => {
                for folder in folders {
                    let path = game.root.join(folder);
                    fs::create_dir(&path).map_err(|e| format!("Couldn't create {}: {e}", path.display()))?;
                    record.push(format!("created-dir\t{}", path.display()));
                }
                done.push(format!("Created the folders its config names that the download lacks ({}).", folders.iter().map(|f| f.display().to_string()).collect::<Vec<_>>().join(", ")));
            }
            Problem::WindowsLineEndings(p) => {
                // The original goes outside the game's folder, where it would count as a script.
                let script = game.root.join(p);
                let text = fs::read(&script).map_err(|e| e.to_string())?;
                let backups = data_home().join("gamefix/backups").join(slug(&game.name));
                if !backups.is_dir() {
                    fs::create_dir_all(&backups).map_err(|e| e.to_string())?;
                    record.push(format!("created-dir\t{}", backups.display()));
                }
                let backup = backups.join(p.file_name().unwrap_or_default());
                fs::copy(&script, &backup).map_err(|e| e.to_string())?;
                let fixed: Vec<u8> = text.iter().enumerate().filter(|&(i, &b)| !(b == b'\r' && text.get(i + 1) == Some(&b'\n'))).map(|(_, &b)| b).collect();
                fs::write(&script, fixed).map_err(|e| e.to_string())?;
                record.push(format!("replaced\t{}\t{}", backup.display(), script.display()));
                done.push(format!("Converted {} to Linux line endings (the original is kept in ~/.local/share/gamefix/backups).", p.display()));
            }
            Problem::SymbolClash(clashes) => {
                let paths: Vec<String> = Problem::preload(clashes).iter().map(|p| game.root.join(p).display().to_string()).collect();
                // LD_PRELOAD splits on spaces and colons, so such paths can't be listed in it.
                if paths.iter().any(|p| p.contains([' ', ':'])) {
                    not_fixed.push("Libraries getting the wrong functions: the game's path has a space or colon, which the fix can't handle; move the game to a folder without them.".into());
                } else {
                    preload.extend(paths);
                    done.push("Makes its libraries load first so they get their own functions (at launch).".into());
                }
            }
            Problem::OwnLibstdcxx(Some(path)) => {
                preload.push(path.clone());
                done.push("Loads the system's C++ library first, so the graphics driver doesn't crash the game (at launch).".into());
            }
            Problem::BundledLibstdcxx(Some(path)) => {
                preload.push(path.clone());
                done.push("Loads the system's C++ library first, so the graphics driver can load (at launch).".into());
            }
            Problem::DotnetRuntime(version) => not_fixed.push(format!("It needs Microsoft's .NET runtime {version} installed (see gamefix check).")),
            Problem::BundledLibstdcxx(None) => not_fixed.push("Its bundled C++ library: the system's libstdc++.so.6 to preload wasn't found.".into()),
            Problem::OwnHarfbuzz(_, Some(path), false) => {
                preload.push(path.clone());
                done.push("Loads the system's HarfBuzz first, so its text libraries don't get the game's old copy (at launch).".into());
            }
            Problem::OwnHarfbuzz(_, Some(path), true) => {
                preload.push(path.clone());
                done.push("Loads the old pango from Steam's runtime, which doesn't use the game's old HarfBuzz (at launch).".into());
            }
            Problem::Shadowed(copy, user, _, Some(path)) => {
                preload.push(path.clone());
                let name = copy.file_name().unwrap_or_default().to_string_lossy();
                done.push(format!("Loads the system's newer {name} instead of the game's old one, so the system's {user} gets what it needs (at launch)."));
            }
            Problem::Shadowed(_, _, _, None) => not_fixed.push("An old library the game brings: the system's copy to preload wasn't found.".into()),
            Problem::OwnHarfbuzz(_, None, false) => not_fixed.push("Its built-in HarfBuzz: the system's libharfbuzz.so.0 to preload wasn't found.".into()),
            Problem::OwnHarfbuzz(_, None, true) => not_fixed.push("Its built-in HarfBuzz: Steam's runtime, whose old pango avoids it, wasn't found.".into()),
            Problem::OwnLibstdcxx(None) => not_fixed.push("Its built-in C++ library: the system's libstdc++.so.6 to preload wasn't found.".into()),
            Problem::UnityResolution(Some(run)) => {
                if old_runtime.is_none() {
                    old_runtime = Some(run.clone());
                }
                done.push("Starts it with Steam's runtime for older games, whose GTK 2 lets Unity's resolution window open (it started at 1x1 without).".into());
            }
            Problem::UnityResolution(None) => not_fixed.push("Unity's resolution window needs GTK 2, which only Steam's runtime has here: install Steam, then set it up again.".into()),
            Problem::Sdl2Mix(path) => {
                preload.push(path.clone());
                done.push("Loads the runtime's own SDL2, to match its SDL2 add-ons (at launch).".into());
            }
            Problem::DotnetIcu(_) => {
                env.push(("DOTNET_SYSTEM_GLOBALIZATION_INVARIANT", "1".into()));
                done.push("Lets its old .NET start without the ICU library (at launch).".into());
            }
            Problem::DotnetStatic(files) => {
                let dir = game.full(main).parent().map(Path::to_path_buf).unwrap_or_else(|| game.root.clone());
                for name in files {
                    let file = dir.join(name);
                    if !file.exists() {
                        fs::write(&file, b"").map_err(|e| e.to_string())?;
                        record.push(format!("created\t{}", file.display()));
                    }
                }
                done.push(format!("Made empty stand-ins for the {} static librar{} its .NET list names but the download left out.", files.len(), if files.len() == 1 { "y" } else { "ies" }));
            }
            Problem::MissingLibs { bits, libs, later, in_old_runtime, misplaced } => {
                if let Some(run) = report.old_runtime.as_ref().filter(|_| !in_old_runtime.is_empty()) {
                    old_runtime = Some(run.clone());
                    done.push("Starts it with Steam's runtime for older games, which has missing libraries.".into());
                }
                // Just the libraries the system lacks, linked into a folder of their own: putting
                // the game's folders on LD_LIBRARY_PATH would put everything else in them first
                // too (Liberation Circuit's bundle holds its own glibc and an old libpulse).
                // Ones nothing here has are fetched there from Fedora's or Debian's packages.
                // Also what only plugins need: with the game's own folders first on the path, a
                // bundled library may load at start after all (Because's SDL needs libcaca).
                let others: Vec<String> = libs.keys().chain(later.keys()).filter(|l| !in_old_runtime.contains(*l) && !misplaced.contains_key(*l)).cloned().collect();
                let links = data_home().join("gamefix/libs").join(slug(&game.name));
                // LD_LIBRARY_PATH splits on colons.
                if (!misplaced.is_empty() || !others.is_empty()) && links.display().to_string().contains(':') {
                    not_fixed.push("Libraries the game needs: the home folder's path has a colon, which the fix can't handle.".into());
                } else if !misplaced.is_empty() || !others.is_empty() {
                    // An empty one left from before counts as new, so undo removes it too.
                    let new_dir = fs::read_dir(&links).map(|mut d| d.next().is_none()).unwrap_or(true);
                    fs::create_dir_all(&links).map_err(|e| e.to_string())?;
                    // Before what goes in it: undo goes backwards, so the folder is empty by then.
                    if new_dir {
                        record.push(format!("created-dir\t{}", links.display()));
                    }
                    for (lib, dir) in misplaced {
                        let link = links.join(lib);
                        let _ = fs::remove_file(&link);
                        // An unversioned name a game loads itself (LuaJIT's ffi.load "GLEW") points
                        // to the versioned copy it ships (libGLEW.so.1.11).
                        let dir = game.root.join(dir);
                        let target = Some(dir.join(lib)).filter(|t| t.exists()).or_else(|| {
                            fs::read_dir(&dir).ok()?.flatten().map(|e| e.path()).find(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with(&format!("{lib}."))))
                        });
                        std::os::unix::fs::symlink(target.unwrap_or(dir.join(lib)), &link).map_err(|e| e.to_string())?;
                        record.push(format!("created\t{}", link.display()));
                    }
                    if !misplaced.is_empty() {
                        done.push("Points Linux to the libraries the game ships itself (at launch, through links in ~/.local/share/gamefix/libs).".into());
                    }
                    if !others.is_empty() {
                        let fetched = crate::fetch::fetch(&others, *bits, &links);
                        record.extend(fetched.files.iter().map(|f| format!("created\t{}", f.display())));
                        if !fetched.sources.is_empty() {
                            done.push(format!("Fetched libraries the system lacks into ~/.local/share/gamefix/libs, from {} (at launch).", fetched.sources.join(", ")));
                        }
                        if !fetched.missing.is_empty() {
                            not_fixed.push(format!("Missing libraries no Fedora, Debian or Ubuntu package has: {}.", fetched.missing.join(", ")));
                        }
                        if fetched.needs_runtime && old_runtime.is_none() {
                            if let Some(run) = &report.old_runtime {
                                old_runtime = Some(run.clone());
                                done.push("Starts it with Steam's runtime for older games, which has libraries the fetched ones need.".into());
                            }
                        }
                    }
                    if fs::read_dir(&links).map(|mut d| d.next().is_some()).unwrap_or(false) {
                        env.push(("LD_LIBRARY_PATH", links.display().to_string()));
                    } else if new_dir && fs::remove_dir(&links).is_ok() {
                        record.retain(|r| r != &format!("created-dir\t{}", links.display()));
                    }
                }
            }
            Problem::UnitySingleInstance => {
                if Path::new(BWRAP).is_file() {
                    hide_helpers = true;
                    done.push("Hides the browser helpers it would mistake for itself (at launch).".into());
                } else {
                    not_fixed.push("Old Unity \"Player is already running\": needs bwrap, which isn't installed. If it happens, close Steam before starting.".into());
                }
            }
            Problem::UnityShaders(n) => done.push(format!("Nothing to change for its first start, which stays black for a while (maybe minutes) while the graphics driver compiles its {n} shaders; the driver keeps them, so later starts are quick.")),
            Problem::UnityFont(_) => match (bwrap_overlays(), arial_fonts()) {
                (true, Ok(dir)) => {
                    hide_helpers = true;
                    bwrap_extra.extend(["--overlay-src", "/usr/share/fonts", "--tmp-overlay", "/usr/share/fonts", "--ro-bind"].map(String::from));
                    bwrap_extra.extend([dir.display().to_string(), "/usr/share/fonts/gamefix-arial".into()]);
                    done.push("Shows it a copy of Liberation Sans named Arial in /usr/share/fonts, so its text appears (at launch).".into());
                }
                (false, _) => not_fixed.push("Its text: the fix needs bwrap 0.10 or newer (overlays), which isn't installed.".into()),
                (_, Err(e)) => not_fixed.push(format!("Its text: {e}")),
            },
            // The entry's Terminal=true is handled above.
            Problem::Terminal { old_curses } => {
                if *old_curses {
                    env.push(("TERM", "xterm".into()));
                    done.push("Tells its old ncurses the terminal is a plain xterm, a description it can read (at launch).".into());
                }
            }
            Problem::UnityH264(_) => not_fixed.push("Its H.264 videos: Unity's Linux player can't play them; the Windows download with Proton can.".into()),
            Problem::NwjsFonts(_) => {
                env.push(("FONTCONFIG_FILE", plain_fonts()?.display().to_string()));
                done.push("Gives its NW.js a plain font list it can read (at launch; the first start takes longer).".into());
            }
            Problem::Alsa32(Some((pcm, ctl))) => {
                env.push(("ALSA_CONFIG_PATH", alsa_pulse32(pcm, ctl)?));
                done.push("Sends its 32-bit ALSA sound through Steam's runtime's PulseAudio plugin (at launch).".into());
            }
            Problem::JavaSound(plugin) => {
                env.push(("ALSA_CONFIG_PATH", java_sound_conf(plugin)?));
                done.push("Sends its Java's own sound to PipeWire (at launch).".into());
            }
            Problem::OldPulse(server) => {
                env.push(("PULSE_SERVER", server.clone()));
                done.push("Tells its old PulseAudio library where the sound server is (at launch).".into());
            }
            Problem::NetStandard(_) => not_fixed.push("Its .NET Standard parts: the Mono it brings can't load them; the .exe with Proton can.".into()),
            Problem::Alsa32(None) => not_fixed.push("Its 32-bit ALSA sound: needs Steam's runtime (install Steam), which has a plugin that works.".into()),
            Problem::KickFiles(files) => not_fixed.push(format!("Its missing Mono files ({}): only the developer can add them; the .exe with Proton works.", files.join(", "))),
            Problem::FnaLibs(libs) => not_fixed.push(format!("Its missing native libraries ({}): get FNA's fnalibs, or use the Windows download with Proton.", libs.join(", "))),
            Problem::NoLinuxBuild | Problem::UnityDataMissing(_) | Problem::LinuxInstaller(_) | Problem::OwnWindowsDlls(_) | Problem::MissingWindowsParts(_) | Problem::Source(..) | Problem::Web(_) | Problem::Damaged(_) | Problem::Package(..) | Problem::Flash { .. } | Problem::Tic { .. } | Problem::Dos { .. } | Problem::Disc { .. } | Problem::CdFiles { .. } | Problem::Protected { .. } | Problem::Archive(_) | Problem::WrongMachine(_) | Problem::Love { .. } | Problem::Python { .. } | Problem::Java { .. } => {}
        }
    }
    if !preload.is_empty() {
        env.push(("LD_PRELOAD", preload.join(":")));
    }
    if report.pyinstaller && !env.iter().any(|(k, _)| *k == "GLIBC_TUNABLES") {
        env.push(("GLIBC_TUNABLES", "glibc.rtld.execstack=2".into()));
        done.push("Lets its packed libraries load even if they ask for an executable stack (at launch, in case).".into());
    }
    // What a packed program needs can't be read; the runtime only fills gaps, so it's safe.
    if report.packed && old_runtime.is_none() {
        if let Some(run) = &report.old_runtime {
            old_runtime = Some(run.clone());
            done.push("Starts it with Steam's runtime for older games, in case it needs libraries this system dropped (it's packed, so they can't be read).".into());
        }
    }
    // The developer's launcher script (see Game::launcher); else the program.
    let mut start = game.launcher().map(|l| game.full(l)).unwrap_or_else(|| program.clone());
    // A launcher that replaces LD_LIBRARY_PATH would drop the folder set here, or Steam's
    // runtime's (The Fate of Marcus' `run`: `LD_LIBRARY_PATH=:./lib64`, libFLAC.so.8 then not
    // found): start the program itself then, from the launcher's folder, with the game's
    // libraries the launcher pointed it to linked into that folder, only those the system lacks;
    // its whole folder first would hide newer system ones (Because's old libogg broke the
    // system's libsndfile).
    let mut launcher_dir = None;
    if start != program && crate::binary::sets_library_path(&start) && !crate::binary::keeps_library_path(&start) && (env.iter().any(|(k, _)| *k == "LD_LIBRARY_PATH") || old_runtime.is_some()) {
        let links = data_home().join("gamefix/libs").join(slug(&game.name));
        if !report.launcher_libs.is_empty() && !env.iter().any(|(k, _)| *k == "LD_LIBRARY_PATH") {
            if !links.is_dir() {
                fs::create_dir_all(&links).map_err(|e| e.to_string())?;
                record.push(format!("created-dir\t{}", links.display()));
            }
            env.push(("LD_LIBRARY_PATH", links.display().to_string()));
        }
        for (lib, dir) in &report.launcher_libs {
            let link = links.join(lib);
            if link.symlink_metadata().is_err() {
                std::os::unix::fs::symlink(game.root.join(dir).join(lib), &link).map_err(|e| e.to_string())?;
                record.push(format!("created\t{}", link.display()));
            }
        }
        launcher_dir = start.parent().map(Path::to_path_buf);
        start = program.clone();
    }
    if start != program {
        let mode = fs::metadata(&start).map_err(|e| e.to_string())?.permissions().mode();
        if mode & 0o100 == 0 {
            fs::set_permissions(&start, fs::Permissions::from_mode(mode | 0o111)).map_err(|e| e.to_string())?;
            record.push(format!("mode\t{:o}\t{}", mode & 0o7777, start.display()));
        }
        done.push(format!("Starts it through its own launcher script, {}.", start.file_name().unwrap_or_default().to_string_lossy()));
    }
    // An Exodus bundle's program sits deep inside it; the launcher link at its top is meant to
    // be started from there, next to the game's data (Liberation Circuit).
    let exodus_top = start.ancestors().find(|d| d.join("exodus/bundles").is_dir() && start.starts_with(d.join("exodus")));
    let workdir = exodus_top.or(launcher_dir.as_deref()).or(start.parent()).unwrap_or(&game.root);
    let icon = icon(game, &main.path);

    let wrap = hide_helpers.then(|| hide_helpers_with(&bwrap_extra));
    if targets.menu {
        let mut exec = Vec::new();
        if !env.is_empty() {
            exec.push("env".to_string());
            exec.extend(env.iter().map(|(k, v)| exec_arg(&format!("{k}={v}"))));
        }
        if let Some(run) = &old_runtime {
            exec.push(exec_arg(&run.display().to_string()));
        }
        if let Some(wrap) = &wrap {
            exec.extend(["sh", "-c", &format!("{wrap} exec \"$@\" \"$0\"")].map(exec_arg));
        }
        exec.push(exec_arg(&start.display().to_string()));
        let entry = write_entry(game, &exec, workdir, &icon, terminal)?;
        record.push(format!("created\t{}", entry.display()));
        done.push(format!("Added \"{}\" to the app menu, under Games{}.", game.name, if terminal { " (it opens in a terminal)" } else { "" }));
    }
    if terminal && targets.steam {
        not_fixed.push("Not added to Steam: it's a text game that runs in a terminal, which Steam doesn't open.".into());
    }

    if let (Some(steam), Some(appid)) = (&steam, steam::installed_app(&game.root)) {
        // A Steam game: its own launch options, keeping whatever was there.
        let file = steam.localconfig();
        let previous = steam::launch_options(&file, appid)?;
        let options = launch_options(&env, &[], old_runtime.as_deref(), wrap.as_deref(), previous.as_deref().unwrap_or(""));
        if options != previous.clone().unwrap_or_default() {
            steam::set_launch_options(&file, appid, Some(&options))?;
            let previous = previous.map_or("-".to_string(), |p| format!("={p}"));
            record.push(format!("steam-launch\t{appid}\t{}\t{previous}", file.display()));
            done.push(format!("Set its launch options in Steam: {options}"));
        }
    } else if let Some(steam) = &steam {
        let file = steam.shortcuts();
        let shortcut = steam::Shortcut {
            name: &game.name,
            exe: &start,
            start_dir: workdir,
            icon: if icon.starts_with('/') { &icon } else { "" },
            launch_options: &launch_options(&env, &[], old_runtime.as_deref(), wrap.as_deref(), ""),
        };
        let appid = steam::add_shortcut(&file, &shortcut)?;
        record.push(format!("steam-shortcut\t{appid}\t{}", file.display()));
        done.push(format!("Added \"{}\" to Steam as a non-Steam game.", game.name));
    }

    let record_file = record_path(game);
    fs::create_dir_all(record_file.parent().unwrap()).map_err(|e| e.to_string())?;
    fs::write(&record_file, record.join("\n") + "\n").map_err(|e| e.to_string())?;
    if targets.menu {
        refresh_menu();
    }
    Ok(Outcome { done, not_fixed })
}

/// A program from Flathub that runs a kind of game nothing on the system runs.
pub struct Runner {
    pub name: &'static str,
    pub app: &'static str,
}

/// The Flathub program this game needs and lacks, if setup has nothing else to start it with
/// (no Linux program, no Windows one, no other runner installed). Installing it is the
/// caller's to offer: it asks first.
pub fn missing_runner(game: &Game, report: &Report) -> Option<Runner> {
    if game.main().is_some() || game.windows.is_some() {
        return None;
    }
    let runnable = report.problems.iter().any(|p| match p {
        Problem::Java { java, .. } => java.is_some(),
        Problem::Love { love, version, .. } => love.is_some() || crate::check::old_love(version.as_deref()),
        Problem::Flash { ruffle, .. } => ruffle.is_some(),
        Problem::Tic { tic80, .. } => tic80.is_some(),
        Problem::Dos { dosbox, .. } => dosbox.is_some(),
        _ => false,
    });
    if runnable {
        return None;
    }
    report.problems.iter().find_map(|p| match p {
        Problem::Love { love: None, .. } => Some(Runner { name: "LÖVE", app: "org.love2d.love2d" }),
        Problem::Flash { ruffle: None, .. } => Some(Runner { name: "Ruffle", app: "rs.ruffle.Ruffle" }),
        Problem::Tic { tic80: None, .. } => Some(Runner { name: "TIC-80", app: "com.tic80.TIC_80" }),
        Problem::Dos { dosbox: None, .. } => Some(Runner { name: "DOSBox Staging", app: "io.github.dosbox-staging" }),
        _ => None,
    })
}

/// What setup says when a game's runner isn't installed and nothing installed it.
fn runner_needed(report: &Report) -> Option<String> {
    let then = "then set it up again.";
    report.problems.iter().find_map(|p| match p {
        Problem::Java { java: None, .. } => Some(format!("This is a Java game, and Java isn't installed. On Bazzite, run \"brew install openjdk\" in a terminal, {then}")),
        Problem::Love { love: None, version, .. } if !crate::check::old_love(version.as_deref()) => Some(format!("This is a LÖVE game, and LÖVE isn't installed. Install LÖVE from Discover (it's on Flathub), {then}")),
        Problem::Flash { ruffle: None, .. } => Some(format!("This is a Flash game; it plays in Ruffle, which isn't installed. Install Ruffle from Discover (it's on Flathub), {then}")),
        Problem::Tic { tic80: None, .. } => Some(format!("This is a TIC-80 game; it plays in TIC-80, which isn't installed. Install TIC-80 from Discover (it's on Flathub), {then}")),
        Problem::Dos { dosbox: None, .. } => Some(format!("This is a DOS game; it runs in DOSBox Staging, which isn't installed. Install DOSBox Staging from Discover (it's on Flathub), {then}")),
        Problem::Python { v2: true, .. } => Some("This is a Python 2 game; Python 2 is gone from current systems (see gamefix check).".into()),
        Problem::Love { .. } => Some("This LÖVE game needs an older LÖVE than any gamefix can set up (see gamefix check).".into()),
        _ => None,
    })
}

/// Flathub's download size for an app, plus its runtime's when that isn't installed yet, as
/// flatpak prints them ("145.5 MB").
pub fn flathub_size(app: &str) -> Option<String> {
    let info = |r: &str| -> Option<String> {
        ["--user", "--system"].iter().find_map(|i| {
            let out = Command::new("flatpak").args(["remote-info", i, "flathub", r]).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
            out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
        })
    };
    let field = |text: &str, name: &str| text.lines().find_map(|l| l.trim().strip_prefix(name).map(|v| v.trim().to_string()));
    let text = info(app)?;
    let size = field(&text, "Download Size:")?;
    let runtime = field(&text, "Runtime:").map(|r| format!("runtime/{r}"));
    let installed = |r: &str| Command::new("flatpak").args(["info", r]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success());
    match runtime.filter(|r| !installed(r)).and_then(|r| info(&r)).and_then(|t| field(&t, "Download Size:")) {
        Some(more) => Some(format!("{size}, plus {more} for the Flatpak runtime it needs")),
        None => Some(size),
    }
}

/// Installs an app from Flathub for this user (no password; the Flathub remote is added for
/// the user if only the system has it). Kept on undo, like the LÖVE builds: other games use it.
pub fn install_flatpak(app: &str) -> Result<(), String> {
    let flatpak = |args: &[&str]| -> Result<(), String> {
        let out = Command::new("flatpak").args(args).stdin(Stdio::null()).output().map_err(|e| format!("flatpak: {e}"))?;
        if out.status.success() {
            return Ok(());
        }
        let err = String::from_utf8_lossy(&out.stderr);
        Err(err.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("flatpak failed").trim().to_string())
    };
    flatpak(&["remote-add", "--user", "--if-not-exists", "flathub", "https://dl.flathub.org/repo/flathub.flatpakrepo"])?;
    flatpak(&["install", "--user", "--noninteractive", "-y", "flathub", app])
}

/// A font list for old Chromium (NW.js) that can't handle the system's font rules: just the font
/// folders, with its own cache. Shared by every game that needs it, so undo leaves it.
fn plain_fonts() -> Result<PathBuf, String> {
    let file = data_home().join("gamefix/fonts.conf");
    if !file.exists() {
        fs::create_dir_all(file.parent().unwrap()).map_err(|e| e.to_string())?;
        let text = "<?xml version=\"1.0\"?>\n<!DOCTYPE fontconfig SYSTEM \"fonts.dtd\">\n<!-- Written by gamefix for old NW.js games: the font folders without the system's rules. -->\n<fontconfig>\n  <dir>/usr/share/fonts</dir>\n  <dir>/usr/local/share/fonts</dir>\n  <dir prefix=\"xdg\">fonts</dir>\n  <cachedir prefix=\"xdg\">gamefix-fontconfig</cachedir>\n</fontconfig>\n";
        fs::write(&file, text).map_err(|e| e.to_string())?;
    }
    Ok(file)
}

/// ALSA settings for 32-bit games, as an ALSA_CONFIG_PATH value: a file of its own, not the
/// system's alsa.conf (which loads conf.d, where PipeWire becomes the default after anything
/// added to the path, and which old bundled ALSA libraries can't parse), making the default
/// device PulseAudio through the given plugins. Rewritten each time, as the plugins' paths
/// come from Steam's install.
fn alsa_pulse32(pcm: &Path, ctl: &Path) -> Result<String, String> {
    let file = data_home().join("gamefix/alsa32.conf");
    fs::create_dir_all(file.parent().unwrap()).map_err(|e| e.to_string())?;
    let text = format!(
        "# Written by gamefix for 32-bit games that play sound through ALSA: PipeWire's ALSA plugin\n# is 64-bit only here, so the default device goes to PulseAudio (which PipeWire serves).\npcm_type.pulse {{ lib \"{}\" }}\nctl_type.pulse {{ lib \"{}\" }}\npcm.!default {{ type pulse }}\nctl.!default {{ type pulse }}\n",
        pcm.display(),
        ctl.display()
    );
    fs::write(&file, text).map_err(|e| e.to_string())?;
    Ok(file.display().to_string())
}

/// Whether the installed bwrap can lay an overlay over a folder (0.10 and newer).
fn bwrap_overlays() -> bool {
    let Ok(out) = Command::new(BWRAP).arg("--version").stdin(Stdio::null()).output() else { return false };
    let text = String::from_utf8_lossy(&out.stdout);
    let mut v = text.split_whitespace().last().unwrap_or("").split('.').map(|n| n.parse::<u32>().unwrap_or(0));
    (v.next().unwrap_or(0), v.next().unwrap_or(0)) >= (0, 10)
}

/// Copies of the system's Liberation Sans named Arial (`~/.local/share/gamefix/fonts/arial`,
/// shared by every game and kept on undo, like the plain font list), for Unity players that
/// look for Arial by name in /usr/share/fonts only.
fn arial_fonts() -> Result<PathBuf, String> {
    let dir = data_home().join("gamefix/fonts/arial");
    let styles = [("Regular", "Arial", "ArialMT"), ("Bold", "Arial Bold", "Arial-BoldMT"), ("Italic", "Arial Italic", "Arial-ItalicMT"), ("BoldItalic", "Arial Bold Italic", "Arial-BoldItalicMT")];
    let mut sources = Vec::new();
    let mut todo = vec![(PathBuf::from("/usr/share/fonts"), 0)];
    while let Some((d, depth)) = todo.pop() {
        for entry in fs::read_dir(&d).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() && depth < 3 {
                todo.push((path, depth + 1));
            } else if let Some(style) = path.file_name().and_then(|n| n.to_str()).and_then(|n| n.strip_prefix("LiberationSans-")).and_then(|n| n.strip_suffix(".ttf")) {
                sources.push((style.to_string(), path));
            }
        }
    }
    if !sources.iter().any(|(s, _)| s == "Regular") {
        return Err("it needs Liberation Sans (Fedora's liberation-sans-fonts), which isn't installed.".into());
    }
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    for (style, full, ps) in styles {
        let target = dir.join(format!("{}.ttf", full.replace(' ', "")));
        let Some((_, source)) = sources.iter().find(|(s, _)| s == style) else { continue };
        if target.is_file() {
            continue;
        }
        let data = fs::read(source).map_err(|e| e.to_string())?;
        let font = crate::font::renamed(&data, &[(1, "Arial"), (4, full), (6, ps), (16, "Arial")]).ok_or(format!("couldn't read {}", source.display()))?;
        fs::write(&target, font).map_err(|e| e.to_string())?;
    }
    Ok(dir)
}

/// Java's own sound for a game gamefix starts with Java: the command with the ALSA setting in
/// front, when PipeWire's plugin is there (else, or if the setting can't be written, as it was).
fn java_sound(command: Vec<String>, needed: bool) -> Vec<String> {
    if !needed {
        return command;
    }
    let Some(conf) = crate::system::System::load().pipewire_alsa(64).and_then(|p| java_sound_conf(&p).ok()) else { return command };
    let setting = format!("ALSA_CONFIG_PATH={conf}");
    match command.first().map(String::as_str) {
        Some("env") => [vec!["env".into(), setting], command[1..].to_vec()].concat(),
        _ => [vec!["env".into(), setting], command].concat(),
    }
}

/// ALSA settings for Java's own sound, as an ALSA_CONFIG_PATH value: the system's alsa.conf,
/// then PipeWire's plugin named by path (Homebrew's ALSA library only looks in its own folder)
/// and the card's names (hw, plughw), which Java opens directly, sent to PipeWire. Tested:
/// Dial, Black & White and a tone at the expected level.
fn java_sound_conf(plugin: &Path) -> Result<String, String> {
    let file = data_home().join("gamefix/java-sound.conf");
    fs::create_dir_all(file.parent().unwrap()).map_err(|e| e.to_string())?;
    let card = |kind: &str, body: &str| format!("pcm.!{kind} {{\n\t@args [ CARD DEV SUBDEV ]\n\t@args.CARD {{ type string default \"0\" }}\n\t@args.DEV {{ type integer default 0 }}\n\t@args.SUBDEV {{ type integer default -1 }}\n{body}}}\n");
    let text = format!(
        "# Written by gamefix for Java games: Java's own sound opens the card (hw, plughw), which PipeWire holds;\n# these send it to PipeWire instead.\npcm_type.pipewire {{ lib \"{}\" }}\n{}{}",
        plugin.display(),
        card("hw", "\ttype pipewire\n\tplayback_node \"-1\"\n\tcapture_node \"-1\"\n"),
        card("plughw", "\ttype plug\n\tslave.pcm \"hw\"\n")
    );
    fs::write(&file, text).map_err(|e| e.to_string())?;
    Ok(format!("/usr/share/alsa/alsa.conf:{}", file.display()))
}

/// Writes the game's app-menu entry running `exec` (already quoted) in `workdir`; a text game
/// gets a terminal (`Terminal=true`: the desktop opens its own, Konsole on KDE).
fn write_entry(game: &Game, exec: &[String], workdir: &Path, icon: &str, terminal: bool) -> Result<PathBuf, String> {
    let entry_text = format!(
        "[Desktop Entry]\nType=Application\nName={}\nComment=Set up by gamefix\nExec={}\nPath={}\nIcon={}\nCategories=Game;\nTerminal={terminal}\nX-Gamefix-Game={}\n",
        value(&game.name),
        exec.join(" "),
        value(&workdir.display().to_string()),
        value(icon),
        value(&game.root.display().to_string()),
    );
    let apps = data_home().join("applications");
    fs::create_dir_all(&apps).map_err(|e| e.to_string())?;
    let entry = apps.join(format!("gamefix-{}.desktop", slug(&game.name)));
    fs::write(&entry, entry_text).map_err(|e| e.to_string())?;
    Ok(entry)
}

/// What a runner's setup did before writing the entries: record lines and messages, and
/// whether the game runs in a terminal.
#[derive(Default)]
struct Extra {
    record: Vec<String>,
    done: Vec<String>,
    terminal: bool,
}

/// A Python 3 game: what it imports that Python lacks installed from PyPI into a virtual
/// environment of its own (wheels only), then an entry running it with that Python, or
/// through the developer's script with that Python first on the PATH (End User's run.sh
/// sets up its bundled SDL first).
fn setup_python(game: &Game, file: &Path, plan: &crate::python::Plan, targets: &Targets) -> Result<Outcome, String> {
    let mut python = plan.python.clone().ok_or("Python 3 isn't installed.")?;
    if !plan.compile.is_empty() {
        return Err("Part of this Python game is Cython source that has to be compiled first (see gamefix check).".into());
    }
    if plan.terminal && targets.steam {
        return Err("It's a text game that runs in a terminal, which Steam doesn't open: set it up for the app menu.".into());
    }
    let mut extra = Extra { terminal: plan.terminal, ..Default::default() };
    if let Some((minor, _)) = &plan.older {
        if crate::python::build_info(*minor).is_none() {
            return Err(format!("This game's code needs Python 3.{minor} or older, which gamefix can't get (see gamefix check)."));
        }
        let (older, note) = crate::python::older_python(*minor)?;
        python = older;
        extra.done.extend(note);
    }
    for (wanted, real) in &plan.case_links {
        let link = game.root.join(wanted);
        std::os::unix::fs::symlink(real.file_name().unwrap_or_default(), &link).map_err(|e| format!("Couldn't link {}: {e}", link.display()))?;
        extra.record.push(format!("created\t{}", link.display()));
    }
    if !plan.case_links.is_empty() {
        extra.done.push(format!("Linked {} files under the upper/lower case its code uses.", plan.case_links.len()));
    }
    let mut bin = None;
    if !plan.packages.is_empty() {
        let dir = data_home().join("gamefix/python").join(slug(&game.name));
        // A leftover from a setup that failed half-way; undo has already run.
        let _ = fs::remove_dir_all(&dir);
        if let Err(e) = crate::python::install(&python, &dir, &plan.packages) {
            let _ = fs::remove_dir_all(&dir);
            return Err(e);
        }
        extra.record.push(format!("created-tree\t{}", dir.display()));
        extra.done.push(format!("Installed {} from PyPI into {}.", plan.packages.join(", "), dir.display()));
        bin = Some(dir.join("bin"));
    }
    match &plan.launcher {
        Some(script) => {
            let mut command = Vec::new();
            if let Some(bin) = &bin {
                command = vec!["env".into(), format!("PATH={}:/usr/bin:/bin", bin.display())];
            }
            command.push(crate::python::interpreter(&game.root.join(script)));
            setup_runner(game, command, script, "Python 3", targets, extra)
        }
        None => {
            let python = bin.map_or_else(|| python.display().to_string(), |b| b.join("python").display().to_string());
            setup_runner(game, vec![python], file, "Python 3", targets, extra)
        }
    }
}

/// A game another program runs (`java -jar`, LÖVE): an app-menu entry running it on the file.
fn setup_runner(game: &Game, command: Vec<String>, file: &Path, name: &str, targets: &Targets, extra: Extra) -> Result<Outcome, String> {
    let steam = if targets.steam {
        let steam = Steam::find().ok_or("Steam isn't installed, or nobody has logged in to it yet.")?;
        if steam.running() {
            return Err(STEAM_OPEN.into());
        }
        Some(steam)
    } else {
        None
    };
    if record_path(game).exists() {
        undo(game)?;
    }
    let mut done = extra.done;
    let mut not_fixed = Vec::new();
    let mut record = extra.record;
    // A Processing export with only 32-bit OpenGL parts, on 64-bit Java: JOGL also looks for
    // `<jar>-natives-linux-amd64.jar` next to its jars, and Maven Central keeps those.
    if crate::scan::processing_script(&game.root, file) && crate::scan::processing_notes(&game.root, file).1 {
        let lib = game.root.join(file.parent().unwrap_or(Path::new(""))).join("lib");
        match jogl_natives(&lib) {
            Ok((files, _)) if files.is_empty() => {}
            Ok((files, version)) => {
                record.extend(files.iter().map(|f| format!("created\t{}", f.display())));
                done.push(format!("Downloaded the 64-bit OpenGL parts it lacks (JOGL {version}, from Maven Central) into its lib folder."));
            }
            Err(e) => not_fixed.push(e),
        }
    }
    let file = game.root.join(file);
    let workdir = file.parent().unwrap_or(&game.root);
    let mut command = command;
    let mut tail = vec![file.display().to_string()];
    // An Eclipse jar-in-jar export: its inner jars unpacked, then the game's own main class
    // started with them on the class path (Java expands `<dir>/*`).
    if command.last().is_some_and(|a| a == "-jar") {
        if let Some(inner) = crate::binary::jar_in_jar(&file) {
            let dir = data_home().join("gamefix/jars").join(slug(&game.name));
            if !dir.is_dir() {
                fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
                record.push(format!("created-dir\t{}", dir.display()));
            }
            let out = Command::new("unzip").args(["-q", "-o", "-j"]).arg(&file).args(&inner.jars).arg("-d").arg(&dir).output().map_err(|e| format!("unzip: {e}"))?;
            if !out.status.success() {
                return Err(format!("Couldn't unpack the jars inside {}: {}", file.display(), String::from_utf8_lossy(&out.stderr).trim()));
            }
            record.extend(inner.jars.iter().filter_map(|j| Path::new(j).file_name()).map(|n| format!("created\t{}", dir.join(n).display())));
            command.pop();
            command.extend(["-cp".into(), format!("{}:{}/*", file.display(), dir.display())]);
            tail = vec![inner.main_class];
            done.push(format!("Unpacked the {} jars packed inside it into ~/.local/share/gamefix/jars, which its own loader doesn't find on current Java.", inner.jars.len()));
        }
    }
    if targets.menu {
        let exec: Vec<String> = command.iter().chain(&tail).map(|a| exec_arg(a)).collect();
        let entry = write_entry(game, &exec, workdir, "applications-games", extra.terminal)?;
        // Undo goes backwards: the entry first in the file is removed last.
        record.insert(0, format!("created\t{}", entry.display()));
        done.insert(0, format!("Added \"{}\" to the app menu, under Games (run with {name}).", game.name));
    }
    // A non-Steam shortcut to the runner itself, the rest as its launch options: Steam puts
    // those after the program. Not `%command% ...`: Steam drops %command% and appends its
    // launch chain and the program, so `run ...` came first and the shell failed on it.
    if let Some(steam) = &steam {
        let exe = program_path(&command[0]).ok_or(format!("{} isn't on the PATH, so Steam couldn't start it.", command[0]))?;
        let quote = |v: &str| if v.chars().all(|c| c.is_ascii_alphanumeric() || "/-_.=+:,".contains(c)) { v.to_string() } else { format!("'{}'", v.replace('\'', "'\\''")) };
        let args: Vec<String> = command[1..].iter().chain(&tail).map(|a| quote(a)).collect();
        let line = args.join(" ");
        let shortcuts = steam.shortcuts();
        let shortcut = steam::Shortcut { name: &game.name, exe: &exe, start_dir: workdir, icon: "", launch_options: &line };
        let appid = steam::add_shortcut(&shortcuts, &shortcut)?;
        record.push(format!("steam-shortcut\t{appid}\t{}", shortcuts.display()));
        done.push(format!("Added \"{}\" to Steam as a non-Steam game (run with {name}).", game.name));
    }
    let record_file = record_path(game);
    fs::create_dir_all(record_file.parent().unwrap()).map_err(|e| e.to_string())?;
    fs::write(&record_file, record.iter().map(|l| format!("{l}\n")).collect::<String>()).map_err(|e| e.to_string())?;
    if targets.menu {
        refresh_menu();
    }
    Ok(Outcome { done, not_fixed })
}

/// The folder a Linux installer's game is unpacked into: its name without words like
/// "Linux" and "Installer" ("MazeBurrow_v0_7_LinuxInstaller" → "MazeBurrow_v0_7").
pub fn unpacked_name(installer: &Path) -> String {
    let name = installer.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let mut base = name.strip_suffix(".run").or_else(|| name.strip_suffix(".bin")).unwrap_or(&name).to_string();
    loop {
        let lower = base.to_lowercase();
        let Some(word) = ["installer", "install", "setup", "linux64", "linux32", "linux", "x86_64", "x86", "64bit", "32bit"].iter().find(|w| lower.ends_with(*w)) else { break };
        base.truncate(base.len() - word.len());
        base = base.trim_end_matches(['_', '-', '.', ' ']).to_string();
    }
    if base.is_empty() || base == name { format!("{name} (unpacked)") } else { base }
}

/// A MojoSetup installer: the game it carries (its zip's `data/`, what the installer would copy)
/// unpacked into a folder named after it beside it, then that set up. Undo leaves the folder,
/// like an installed Windows game: the game may keep its saves there.
fn setup_unpacked(game: &Game, file: &Path, targets: &Targets) -> Result<Outcome, String> {
    let installer = game.root.join(file);
    let dir = installer.parent().unwrap_or(&game.root);
    let dest = dir.join(unpacked_name(&installer));
    if dest.exists() {
        return Err(format!("{} is already there but holds no game program: move it away and set up again.", dest.display()));
    }
    let tmp = dir.join(format!(".gamefix-unpacking-{}", std::process::id()));
    // unzip warns (exit 1) about the installer's program before the zip.
    let out = Command::new("unzip").args(["-q", "-o"]).arg(&installer).arg("data/*").arg("-d").arg(&tmp).output().map_err(|e| format!("unzip: {e}"))?;
    if out.status.code().is_none_or(|c| c > 1) || !tmp.join("data").is_dir() {
        let _ = fs::remove_dir_all(&tmp);
        return Err(format!("Couldn't unpack the game from {}: {}", installer.display(), String::from_utf8_lossy(&out.stderr).trim()));
    }
    let moved = fs::rename(tmp.join("data"), &dest);
    let _ = fs::remove_dir_all(&tmp);
    moved.map_err(|e| e.to_string())?;
    let unpacked = crate::scan::scan(&game.root).ok_or("The unpacked game couldn't be read.")?;
    let report = crate::check::check(&unpacked, &crate::system::System::load());
    let mut outcome = setup(&unpacked, &report, targets)?;
    let record = record_path(&unpacked);
    let mut lines = fs::read_to_string(&record).unwrap_or_default();
    lines.push_str(&format!("installed-game\t{}\n", dest.display()));
    fs::write(&record, lines).map_err(|e| e.to_string())?;
    outcome.done.insert(0, format!("Unpacked the game from its installer, {}, into {}.", file.display(), dest.display()));
    Ok(outcome)
}

/// A program's full path: as given when it has a slash, else the first match on the PATH.
fn program_path(name: &str) -> Option<PathBuf> {
    if name.contains('/') {
        return Some(PathBuf::from(name));
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path).map(|d| d.join(name)).find(|p| p.is_file())
}

/// What a Windows game is installed from.
enum Installer {
    /// A disc image and the installer's path on it.
    Disc { iso: PathBuf, inside: String },
    File(PathBuf),
}

/// A game's name as a folder name: as it is, but for slashes and control characters
/// ("Harry Potter", like the user's other game folders).
fn folder_name(name: &str) -> String {
    let clean: String = name.chars().map(|c| if c == '/' || c.is_control() { ' ' } else { c }).collect();
    let clean = clean.trim().trim_start_matches('.').to_string();
    if clean.is_empty() { "Game".into() } else { clean }
}

fn games_folder_file() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config")).join("gamefix/games-folder")
}

/// Where installed Windows games go, each in a folder named after it: what
/// `gamefix games-folder <path>` set, else ~/Games.
pub fn games_folder() -> PathBuf {
    fs::read_to_string(games_folder_file()).ok().map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).map(PathBuf::from).unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join("Games"))
}

/// Sets the games folder. It has to be on a Linux drive: a Wine prefix needs links (its drives
/// are links named "c:"), Unix permissions and case-sensitive names, which NTFS (here through
/// ntfs-3g, "fuseblk"), FAT and exFAT drives don't give it properly.
pub fn set_games_folder(path: &Path) -> Result<Vec<String>, String> {
    let path = fs::canonicalize(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if !path.is_dir() {
        return Err(format!("{} isn't a folder.", path.display()));
    }
    let kind = std::process::Command::new("stat").args(["-f", "-c", "%T"]).arg(&path).output().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default();
    if ["fuse", "fuseblk", "ntfs", "ntfs3", "msdos", "vfat", "exfat"].contains(&kind.as_str()) {
        return Err(format!("{} is on a Windows-format drive ({kind}), which can't hold Wine's files properly: pick a folder on a Linux drive.", path.display()));
    }
    let probe = path.join(format!(".gamefix-check-{}-c:", std::process::id()));
    std::os::unix::fs::symlink("drive_c", &probe).map_err(|_| format!("{} is on a drive that can't hold Wine's files: pick a folder on a Linux drive.", path.display()))?;
    let _ = fs::remove_file(&probe);
    let file = games_folder_file();
    fs::create_dir_all(file.parent().unwrap()).map_err(|e| e.to_string())?;
    fs::write(&file, format!("{}\n", path.display())).map_err(|e| e.to_string())?;
    Ok(vec![format!("Windows games gamefix installs now go into {}, each in a folder named after it.", path.display())])
}

/// In a folder setup_install installed a game into: what from, the program, its name.
const INSTALL_NOTE: &str = "gamefix-install.txt";

/// A Windows game that has to be installed first (a disc image, an installer): runs the
/// installer once with Proton (umu-run, with the Proton Steam will use) in a prefix of its own
/// in ~/Games, then adds the installed program to Steam, with launch options pointing Steam at
/// that prefix (its registry and saves are there). Undo leaves the installed game.
fn setup_install(game: &Game, installer: Installer, targets: &Targets) -> Result<Outcome, String> {
    if !targets.steam {
        return Err("Windows games can only be set up in Steam for now: add --steam.".into());
    }
    let steam = Steam::find().ok_or("Steam isn't installed, or nobody has logged in to it yet.")?;
    if steam.running() {
        return Err(STEAM_OPEN.into());
    }
    let tool = steam.proton().ok_or("No Proton is installed in Steam: install Proton Experimental (Library → Tools), then set it up again.")?;
    let tool_dir = steam.tool_dir(&tool).ok_or(format!("Couldn't find where {tool} is installed."))?;
    let source = match &installer {
        Installer::Disc { iso, .. } => iso.clone(),
        Installer::File(file) => file.clone(),
    };
    // A game an earlier setup installed from the same disc or installer (undo leaves it) is
    // used again: its folder in ~/Games names its source in gamefix-install.txt.
    let games = games_folder();
    let home_games = PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join("Games");
    let earlier = [&games, &home_games].into_iter().flat_map(|g| fs::read_dir(g).into_iter().flatten().flatten()).find_map(|entry| {
        let text = fs::read_to_string(entry.path().join(INSTALL_NOTE)).ok()?;
        let field = |key: &str| text.lines().find_map(|l| l.strip_prefix(key)?.strip_prefix('\t').map(String::from));
        let exe = entry.path().join(field("program")?);
        (Path::new(&field("source")?) == source && exe.is_file()).then(|| (entry.path(), exe, field("name").unwrap_or_else(|| game.name.clone())))
    });
    if record_path(game).exists() {
        undo(game)?;
    }
    let mut done = Vec::new();
    let mut not_fixed = Vec::new();
    let (prefix, exe, name) = match earlier {
        Some(found) => found,
        None => {
            let umu = program_path("umu-run").ok_or("umu-run (umu-launcher) is needed to run the installer with Proton.")?;
            let free = |base: &str| (1..).map(|n| games.join(if n == 1 { base.to_string() } else { format!("{base} {n}") })).find(|d| !d.exists()).unwrap();
            let prefix = games.join(format!(".gamefix-installing-{}", std::process::id()));
            fs::create_dir_all(&prefix).map_err(|e| format!("Couldn't make {}: {e}", prefix.display()))?;
            // Its real path (/var/home, not /home, on Bazzite), as the installed program's will be.
            let prefix = fs::canonicalize(&prefix).map_err(|e| e.to_string())?;
            let (setup, unpacked) = match &installer {
                Installer::Disc { iso, inside } => {
                    let dir = std::env::temp_dir().join(format!("gamefix-disc-{}", std::process::id()));
                    crate::disc::unpack(iso, &dir)?;
                    (dir.join(inside), Some(dir))
                }
                Installer::File(file) => (file.clone(), None),
            };
            let status = std::process::Command::new(&umu)
                .arg(&setup)
                .current_dir(setup.parent().unwrap_or(Path::new("/")))
                .env("WINEPREFIX", &prefix)
                .env("PROTONPATH", &tool_dir)
                .env("GAMEID", "umu-0")
                // No S: drive (umu points it at the whole disk the installer is on): installers
                // that pick the drive with the most free space chose it, and Harry Potter went
                // into the user's Downloads/Games. They get their own C: only.
                .env("PROTON_SET_GAME_DRIVE", "0")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
            crate::disc::wait_for_prefix(&prefix);
            if let Some(dir) = unpacked {
                let _ = fs::remove_dir_all(dir);
            }
            status.map_err(|e| format!("Couldn't start the installer with umu-run: {e}"))?;
            let (exe, name) = match crate::disc::installed_program(&prefix.join("pfx"), &game.name) {
                crate::disc::Installed::Program(exe, name) => (exe, name),
                crate::disc::Installed::Unknown => {
                    let target = free(&folder_name(&game.name));
                    let prefix = if fs::rename(&prefix, &target).is_ok() { target } else { prefix };
                    return Err(format!("The installer made shortcuts, but gamefix couldn't find the program they start. The installed game's Windows folder is {}.", prefix.display()));
                }
                crate::disc::Installed::Nothing => {
                    // Only Wine's own files: the prefix was made just now, for this.
                    let _ = fs::remove_dir_all(&prefix);
                    return Err("The installer finished, but it installed no game (was it cancelled?). Nothing was changed.".into());
                }
            };
            // Named after the game now that its name is known (Bookworm Deluxe, not the download's).
            let named = free(&folder_name(&name));
            let renamed = fs::rename(&prefix, &named).is_ok();
            let exe = match exe.strip_prefix(&prefix) {
                Ok(rel) if renamed => named.join(rel),
                _ => exe,
            };
            let prefix = if renamed { named } else { prefix };
            // The program's path from the prefix when it's inside, else as it is (a game the
            // installer put on another drive stays there).
            let program = exe.strip_prefix(&prefix).unwrap_or(&exe);
            let note = format!("source\t{}\nprogram\t{}\nname\t{name}\n", source.display(), program.display());
            fs::write(prefix.join(INSTALL_NOTE), note).map_err(|e| e.to_string())?;
            done.push(if exe.starts_with(&prefix) {
                format!("Installed it with {tool} into {}.", prefix.display())
            } else {
                format!("Installed it with {tool}: the game is in {}, its Windows settings in {}.", exe.parent().and_then(Path::parent).unwrap_or(&exe).display(), prefix.display())
            });
            (prefix.clone(), exe, name)
        }
    };
    let record_file = record_path(game);
    fs::create_dir_all(record_file.parent().unwrap()).map_err(|e| e.to_string())?;
    let mut record = vec![format!("installed-game\t{}\t{}\t{name}", prefix.display(), exe.display())];
    fs::write(&record_file, record.join("\n") + "\n").map_err(|e| e.to_string())?;
    if steam.running() {
        return Err(format!("Steam was opened while it installed. Close Steam and set it up again: the installed game in {} is used, not installed twice.", prefix.display()));
    }
    if let Some(crate::binary::Binary::Pe(pe)) = crate::binary::identify(&exe) {
        if let Some(what) = pe.protection {
            not_fixed.push(format!("The installed program is protected by {what}, which Wine/Proton can't run: it won't start from Steam as it is."));
        }
    }
    let own = proton::ignored_dlls(&exe);
    let mut env = vec![("STEAM_COMPAT_DATA_PATH", prefix.display().to_string())];
    add_overrides(&mut env, &own);
    let line = launch_options(&env, &[], None, None, "");
    let shortcuts = steam.shortcuts();
    let workdir = exe.parent().unwrap_or(&prefix);
    let shortcut = steam::Shortcut { name: &name, exe: &exe, start_dir: workdir, icon: "", launch_options: &line };
    let appid = steam::add_shortcut(&shortcuts, &shortcut)?;
    record.push(format!("steam-shortcut\t{appid}\t{}", shortcuts.display()));
    let config = steam.config();
    let previous = steam::compat_tool(&config, appid)?;
    steam::set_compat_tool(&config, appid, Some(&tool))?;
    record.push(format!("steam-compat\t{appid}\t{}\t{}", config.display(), previous.map_or("-".to_string(), |p| format!("={p}"))));
    fs::write(&record_file, record.join("\n") + "\n").map_err(|e| e.to_string())?;
    done.push(format!("Added \"{name}\" to Steam as a non-Steam game, run with {tool} in that folder's Wine prefix."));
    if !own.is_empty() {
        done.push(format!("Wine uses the game's own {} instead of its built-in copy, as Windows would.", dll_files(&own)));
    }
    if targets.menu {
        not_fixed.push("App-menu entries for Windows games aren't supported yet; start it from Steam.".into());
    }
    Ok(Outcome { done, not_fixed })
}

/// JOGL's 64-bit native jars for the version an export carries (from its jars' manifests),
/// downloaded next to them: `gluegen-rt-natives-linux-amd64.jar`, `jogl-all-natives-linux-amd64.jar`.
/// Maven Central names some releases with a suffix (Rainbeats' 2.1.5 is 2.1.5-01 there).
fn jogl_natives(lib: &Path) -> Result<(Vec<PathBuf>, String), String> {
    let mut created = Vec::new();
    let mut version = String::new();
    for (artifact, group) in [("gluegen-rt", "org/jogamp/gluegen"), ("jogl-all", "org/jogamp/jogl")] {
        let target = lib.join(format!("{artifact}-natives-linux-amd64.jar"));
        if target.exists() {
            continue;
        }
        let manifest = std::process::Command::new("unzip").arg("-p").arg(lib.join(format!("{artifact}.jar"))).arg("META-INF/MANIFEST.MF").output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
        let v = manifest.lines().find_map(|l| l.strip_prefix("Implementation-Version:")).map(|v| v.trim().to_string()).ok_or(format!("Its {artifact}.jar doesn't say which JOGL version it is, so its 64-bit parts couldn't be fetched."))?;
        let fetched = [v.clone(), format!("{v}-01")].iter().any(|v| {
            let url = format!("https://repo1.maven.org/maven2/{group}/{artifact}/{v}/{artifact}-{v}-natives-linux-amd64.jar");
            let ok = std::process::Command::new("curl").args(["-sfL", "--max-time", "60", "-o"]).arg(&target).arg(&url).status().is_ok_and(|s| s.success());
            let zip = fs::read(&target).is_ok_and(|b| b.starts_with(b"PK\x03\x04"));
            if !(ok && zip) {
                let _ = fs::remove_file(&target);
            }
            ok && zip
        });
        if !fetched {
            return Err(format!("JOGL {v}'s 64-bit parts weren't found on Maven Central; 64-bit Java can't draw it with OpenGL."));
        }
        created.push(target);
        version = v;
    }
    Ok((created, version))
}

/// A Windows-only game, run with Proton by Steam, like ticking "Force the use of a specific
/// Steam Play compatibility tool" in its properties. A Steam game gets the Proton ProtonDB's
/// reports say runs it (Steam's own choice stays when there's none); a download becomes a
/// non-Steam shortcut with Experimental. Games whose anti-cheat stops them are left alone.
fn setup_windows(game: &Game, report: &Report, exe: &crate::scan::Found, targets: &Targets) -> Result<Outcome, String> {
    if !targets.steam {
        return Err("Windows games can only be set up in Steam for now: add --steam.".into());
    }
    let steam = Steam::find().ok_or("Steam isn't installed, or nobody has logged in to it yet.")?;
    if steam.running() {
        return Err(STEAM_OPEN.into());
    }
    let appid = steam::installed_app(&game.root);
    let own: Vec<String> = report.problems.iter().find_map(|p| if let Problem::OwnWindowsDlls(d) = p { Some(d.clone()) } else { None }).unwrap_or_default();
    let mut components: Vec<&str> = report.problems.iter().flat_map(|p| match p {
        Problem::MissingWindowsParts(parts) => parts.iter().filter_map(|(_, p)| if let proton::Provider::Component(c) = p { Some(*c) } else { None }).collect(),
        _ => Vec::new(),
    }).collect();
    components.dedup();
    let hint = appid.and_then(proton::hint);
    let mut done = Vec::new();
    let mut not_fixed = Vec::new();
    if let Some(hint) = hint.as_ref().filter(|h| h.advice == Advice::AntiCheat) {
        not_fixed.push(format!("Its anti-cheat stops it under Proton ({}). Nothing on this side gets past that, so nothing was changed.", hint.reason()));
        return Ok(Outcome { done, not_fixed });
    }
    let anticheat = proton::anticheat(&game.root);
    if let Some((what, _)) = anticheat.iter().find(|(_, works)| !works) {
        not_fixed.push(format!("It uses {what}, an anti-cheat that doesn't run under Proton. Nothing on this side gets past that, so nothing was changed."));
        return Ok(Outcome { done, not_fixed });
    }
    for (what, _) in &anticheat {
        not_fixed.push(format!("It uses {what}: that runs under Proton only if the game's developer turned it on. If it won't start or can't go online, nothing on this side can change that."));
    }
    if record_path(game).exists() {
        undo(game)?;
    }
    let advised = match hint.as_ref().map(|h| &h.advice) {
        Some(Advice::Ge) => match steam.newest_ge() {
            Some(tool) => Some(tool),
            None => match proton::fetch_ge(&steam) {
                Ok(tool) => {
                    done.push(format!("Downloaded {tool} into Steam's compatibility tools (undo leaves it: other games can use it)."));
                    Some(tool)
                }
                Err(e) => {
                    not_fixed.push(format!("{e} ProtonDB's reports say it runs with GE-Proton; ProtonUp-Qt (Flathub) can install that."));
                    None
                }
            },
        },
        Some(Advice::Official(version)) => Some(steam::official_name(version)),
        _ => None,
    };
    if let Some(appid) = appid {
        let options = proton::options(appid, &proton::gpu_makers());
        let mut record = Vec::new();
        if let (Some(tool), Some(hint)) = (advised, &hint) {
            let config = steam.config();
            let previous = steam::compat_tool(&config, appid)?;
            steam::set_compat_tool(&config, appid, Some(&tool))?;
            record.push(format!("steam-compat\t{appid}\t{}\t{}", config.display(), previous.map_or("-".to_string(), |p| format!("={p}"))));
            done.push(format!("Set it to run with {tool} in Steam: {}.", hint.reason()));
            // Steam fetches one of Valve's Protons a game is set to by itself (tested: Cookard, 8.0).
            if let Advice::Official(version) = &hint.advice {
                if !steam.tools().contains(&tool) {
                    done.push(format!("Steam downloads Proton {version} the first time it starts the game."));
                }
            }
        }
        let mut env: Vec<(&str, String)> = options.iter().flat_map(|o| o.env.iter().map(|(k, v)| (k.as_str(), v.clone()))).collect();
        add_overrides(&mut env, &own);
        let args = options.as_ref().map(|o| o.args.clone()).unwrap_or_default();
        let file = steam.localconfig();
        let previous = steam::launch_options(&file, appid)?;
        let line = launch_options(&env, &args, None, None, previous.as_deref().unwrap_or(""));
        if line != previous.clone().unwrap_or_default() {
            steam::set_launch_options(&file, appid, Some(&line))?;
            done.push(match &previous {
                Some(p) if !p.trim().is_empty() => format!("Changed its launch options in Steam to {line} (they were {p})."),
                _ => format!("Set its launch options in Steam to {line}."),
            });
            record.push(format!("steam-launch\t{appid}\t{}\t{}", file.display(), previous.map_or("-".to_string(), |p| format!("={p}"))));
            if let Some(o) = &options {
                done.push(format!("  {}: on ProtonDB these fixed it for {} players, where Steam's default Proton failed for {}.", o.text(), o.reports, o.of));
            }
            if !own.is_empty() {
                done.push(format!("  WINEDLLOVERRIDES={}: Wine uses the game's own {} instead of its built-in copy, as Windows would.", proton::overrides(&own), dll_files(&own)));
            }
        }
        install_components(&steam, game, appid, &components, &mut done, &mut not_fixed, &mut record);
        if record.is_empty() {
            if not_fixed.is_empty() {
                not_fixed.push("ProtonDB's reports don't name a Proton or launch options that run it better than Steam's own setup, so nothing was changed.".into());
            }
            return Ok(Outcome { done, not_fixed });
        }
        let record_file = record_path(game);
        fs::create_dir_all(record_file.parent().unwrap()).map_err(|e| e.to_string())?;
        fs::write(&record_file, record.join("\n") + "\n").map_err(|e| e.to_string())?;
        return Ok(Outcome { done, not_fixed });
    }
    let proton = steam.proton().ok_or("No Proton is installed in Steam. Install \"Proton Experimental\" from the Steam library (it's listed under Tools) and try again.")?;
    let program = game.full(exe);
    let workdir = program.parent().unwrap_or(&game.root);
    let file = steam.shortcuts();
    let mut env = Vec::new();
    add_overrides(&mut env, &own);
    let line = launch_options(&env, &[], None, None, "");
    let shortcut = steam::Shortcut { name: &game.name, exe: &program, start_dir: workdir, icon: "", launch_options: &line };
    let appid = steam::add_shortcut(&file, &shortcut)?;
    let mut record = vec![format!("steam-shortcut\t{appid}\t{}", file.display())];
    let config = steam.config();
    let previous = steam::compat_tool(&config, appid)?;
    steam::set_compat_tool(&config, appid, Some(&proton))?;
    record.push(format!("steam-compat\t{appid}\t{}\t{}", config.display(), previous.map_or("-".to_string(), |p| format!("={p}"))));
    install_components(&steam, game, appid, &components, &mut done, &mut not_fixed, &mut record);

    let record_file = record_path(game);
    fs::create_dir_all(record_file.parent().unwrap()).map_err(|e| e.to_string())?;
    fs::write(&record_file, record.join("\n") + "\n").map_err(|e| e.to_string())?;
    if targets.menu {
        not_fixed.push("App-menu entries for Windows games aren't supported yet; start it from Steam.".into());
    }
    done.extend([format!("Added \"{}\" to Steam as a non-Steam game.", game.name), format!("Set it to run with Proton ({proton}).")]);
    if !own.is_empty() {
        done.push(format!("Set its launch options to {line}: Wine uses the game's own {} instead of its built-in copy, as Windows would.", dll_files(&own)));
    }
    Ok(Outcome { done, not_fixed })
}

/// Installs winetricks components into the game's Proton prefix with protontricks (the host's,
/// else Flathub's). The prefix only exists once Steam has started the game, so before that
/// it says to start it once and set it up again. Undo leaves them: a prefix can't be cleanly
/// rolled back, and they're harmless there.
fn install_components(steam: &Steam, game: &Game, appid: u32, components: &[&str], done: &mut Vec<String>, not_fixed: &mut Vec<String>, record: &mut Vec<String>) {
    if components.is_empty() {
        return;
    }
    let list = components.join(" ");
    if !steam.prefix(&game.root, appid).is_dir() {
        not_fixed.push(format!("The Windows parts it needs ({list}) go into its Proton prefix, which Steam makes the first time it starts the game: start it once (it may fail), close Steam, then run gamefix setup again."));
        return;
    }
    let run = |mut command: Command| command.arg(appid.to_string()).arg("-q").args(components).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status().is_ok_and(|s| s.success());
    let installed = match Command::new("protontricks").arg("--version").output() {
        Ok(_) => run(Command::new("protontricks")),
        Err(_) => {
            let mut flatpak = Command::new("flatpak");
            flatpak.args(["run", "com.github.Matoking.protontricks"]);
            run(flatpak)
        }
    };
    if installed {
        record.push(format!("installed\t{appid}\t{list}"));
        done.push(format!("Installed {list} into its Proton prefix (protontricks; undo leaves them, they're harmless)."));
    } else {
        not_fixed.push(format!("Installing {list} with protontricks failed; `protontricks {appid} {list}` in a terminal shows why."));
    }
}

/// Adds WINEDLLOVERRIDES making Wine use the game's own copies of these DLLs to the settings.
fn add_overrides(env: &mut Vec<(&str, String)>, dlls: &[String]) {
    if dlls.is_empty() {
        return;
    }
    let value = proton::overrides(dlls);
    match env.iter_mut().find(|(k, _)| *k == "WINEDLLOVERRIDES") {
        Some((_, v)) => *v = format!("{v};{value}"),
        None => env.push(("WINEDLLOVERRIDES", value)),
    }
}

fn dll_files(dlls: &[String]) -> String {
    dlls.iter().map(|d| format!("{d}.dll")).collect::<Vec<_>>().join(", ")
}

/// Steam runs launch options with `sh -c`, `%command%` standing for its launch helpers and the
/// game. Settings go first and the wrappers (Steam's old runtime's run.sh, then the helper
/// hiding) right before `%command%`, so they fit around options already there (`gamemoderun
/// %command%`, or bare game arguments like `-insecure`). Steam itself preloads its overlay, so
/// LD_PRELOAD adds to it instead of replacing it. Game arguments go right after `%command%`. A
/// setting or argument the options already have is left as it is.
fn launch_options(env: &[(&str, String)], args: &[String], runtime: Option<&Path>, wrap: Option<&str>, existing: &str) -> String {
    let words: Vec<&str> = existing.split_whitespace().collect();
    let env: Vec<&(&str, String)> = env.iter().filter(|(k, _)| matches!(*k, "LD_PRELOAD" | "LD_LIBRARY_PATH") || !words.iter().any(|w| w.starts_with(&format!("{k}=")))).collect();
    let args: Vec<&String> = args.iter().filter(|a| !words.contains(&a.split(' ').next().unwrap_or(""))).collect();
    let runtime = runtime.map(|r| r.display().to_string()).filter(|r| !existing.contains(r.as_str()));
    if env.is_empty() && args.is_empty() && runtime.is_none() && wrap.is_none() {
        return existing.to_string();
    }
    let quote = |v: &str| if v.chars().all(|c| c.is_ascii_alphanumeric() || "/-_.=+:,".contains(c)) { v.to_string() } else { format!("'{}'", v.replace('\'', "'\\''")) };
    let settings: Vec<String> = env
        .iter()
        .map(|(k, v)| if matches!(*k, "LD_PRELOAD" | "LD_LIBRARY_PATH") { format!("{k}=\"${{{k}:+${k}:}}{v}\"") } else { format!("{k}={}", quote(v)) })
        .collect();
    let command = if wrap.is_some() { "\"$@\" %command%".to_string() } else { "%command%".to_string() };
    let command = match &runtime {
        Some(run) => format!("{} {command}", quote(run)),
        None => command,
    };
    let command = args.iter().fold(command, |c, a| format!("{c} {a}"));
    let command = command.as_str();
    let existing = existing.trim();
    let rest = if existing.contains("%command%") {
        existing.replacen("%command%", command, 1)
    } else if existing.is_empty() {
        command.to_string()
    } else {
        format!("{command} {existing}")
    };
    let line = settings.into_iter().chain([rest]).collect::<Vec<_>>().join(" ");
    match wrap {
        Some(wrap) => format!("{wrap} {line}"),
        None => line,
    }
}

/// Whether undoing this game's setup changes Steam's files.
pub fn touches_steam(game: &Game) -> bool {
    fs::read_to_string(record_path(game)).is_ok_and(|r| r.lines().any(|l| l.starts_with("steam-")))
}

/// Reverses a setup, newest change first. Returns what was undone.
pub fn undo(game: &Game) -> Result<Vec<String>, String> {
    let record_file = record_path(game);
    let record = fs::read_to_string(&record_file).map_err(|_| format!("{} wasn't set up by gamefix.", game.name))?;
    if record.lines().any(|l| l.starts_with("steam-")) && Steam::find().is_some_and(|s| s.running()) {
        return Err(STEAM_OPEN.into());
    }
    let mut undone = Vec::new();
    for line in record.lines().rev() {
        match line.splitn(4, '\t').collect::<Vec<_>>().as_slice() {
            ["steam-launch", appid, file, previous] => {
                let previous = previous.strip_prefix('=');
                steam::set_launch_options(Path::new(file), appid.parse().map_err(|_| "bad record")?, previous)?;
                undone.push(format!("Restored its launch options in Steam ({})", previous.unwrap_or("none")));
                continue;
            }
            ["steam-compat", appid, file, previous] => {
                let previous = previous.strip_prefix('=');
                steam::set_compat_tool(Path::new(file), appid.parse().map_err(|_| "bad record")?, previous)?;
                undone.push(format!("Restored its compatibility tool in Steam ({})", previous.unwrap_or("none")));
                continue;
            }
            _ => {}
        }
        let parts: Vec<&str> = line.splitn(3, '\t').collect();
        match parts.as_slice() {
            ["created", path] => {
                if fs::remove_file(path).is_ok() {
                    undone.push(format!("Removed {path}"));
                }
            }
            ["created-dir", path] => {
                if fs::remove_dir(path).is_ok() {
                    undone.push(format!("Removed {path}"));
                }
            }
            // A folder gamefix filled through another program (pip's thousands of files), only
            // ever inside gamefix's own data folder.
            ["created-tree", path] if Path::new(path).starts_with(data_home().join("gamefix")) => {
                if fs::remove_dir_all(path).is_ok() {
                    undone.push(format!("Removed {path}"));
                }
            }
            ["replaced", backup, path] => {
                if fs::copy(backup, path).is_ok() && fs::remove_file(backup).is_ok() {
                    undone.push(format!("Restored {path}"));
                }
            }
            ["mode", mode, path] => {
                if let Ok(mode) = u32::from_str_radix(mode, 8) {
                    if fs::set_permissions(path, fs::Permissions::from_mode(mode)).is_ok() {
                        undone.push(format!("Restored the permissions of {path}"));
                    }
                }
            }
            ["installed-game", prefix, ..] => undone.push(format!("Left the installed game in {prefix}: delete that folder to remove it.")),
            ["steam-shortcut", appid, file] => {
                if let Ok(appid) = appid.parse() {
                    if steam::remove_shortcut(Path::new(file), appid)? {
                        undone.push(format!("Removed it from Steam ({file})"));
                    }
                }
            }
            _ => {}
        }
    }
    fs::remove_file(&record_file).map_err(|e| e.to_string())?;
    refresh_menu();
    Ok(undone)
}

/// KDE notices new entries by itself, but only after a while; this makes it immediate.
fn refresh_menu() {
    let _ = Command::new("kbuildsycoca6").arg("--noincremental").output();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installer_folder_names() {
        assert_eq!(unpacked_name(Path::new("/g/MazeBurrow_v0_7_LinuxInstaller")), "MazeBurrow_v0_7");
        assert_eq!(unpacked_name(Path::new("/g/game-linux-x86_64-setup.run")), "game");
        assert_eq!(unpacked_name(Path::new("/g/Installer")), "Installer (unpacked)");
    }

    #[test]
    fn slugs_keep_any_alphabet() {
        assert_eq!(slug("Final Shot"), "final-shot");
        assert_eq!(slug("ᗢ"), "ᗢ");
        assert!(slug("!!!").starts_with("game-"));
    }

    #[test]
    fn launch_options_fit_around_existing_ones() {
        let env = [("GLIBC_TUNABLES", "glibc.rtld.execstack=2".to_string())];
        assert_eq!(launch_options(&env, &[], None, None, ""), "GLIBC_TUNABLES=glibc.rtld.execstack=2 %command%");
        assert_eq!(launch_options(&[], &[], None, Some(HIDE_HELPERS), "STEAM_COMPAT_LAUNCHER_SERVICE=proton %command%"), format!("{HIDE_HELPERS} STEAM_COMPAT_LAUNCHER_SERVICE=proton \"$@\" %command%"));
        assert_eq!(launch_options(&env, &[], None, None, "-insecure"), "GLIBC_TUNABLES=glibc.rtld.execstack=2 %command% -insecure");
        assert_eq!(launch_options(&[], &[], None, None, "-insecure"), "-insecure");
        // From ProtonDB's table: arguments right after %command%, nothing the options already have.
        let wined3d = [("PROTON_USE_WINED3D", "1".to_string())];
        let args = ["-force-d3d11".to_string(), "--rendering-driver vulkan".to_string()];
        assert_eq!(launch_options(&wined3d, &args, None, None, ""), "PROTON_USE_WINED3D=1 %command% -force-d3d11 --rendering-driver vulkan");
        assert_eq!(launch_options(&wined3d, &args[..1], None, None, "gamemoderun %command% -skipintro"), "PROTON_USE_WINED3D=1 gamemoderun %command% -force-d3d11 -skipintro");
        assert_eq!(launch_options(&wined3d, &args[..1], None, None, "PROTON_USE_WINED3D=0 %command% -force-d3d11"), "PROTON_USE_WINED3D=0 %command% -force-d3d11");
        // Steam doesn't pass its old runtime's libraries on to games: its run.sh goes in front.
        let run = Path::new("/home/a b/.local/share/Steam/ubuntu12_32/steam-runtime/run.sh");
        let libs = [("LD_LIBRARY_PATH", "/x/libs".to_string())];
        assert_eq!(launch_options(&libs, &[], Some(run), None, ""), "LD_LIBRARY_PATH=\"${LD_LIBRARY_PATH:+$LD_LIBRARY_PATH:}/x/libs\" '/home/a b/.local/share/Steam/ubuntu12_32/steam-runtime/run.sh' %command%");
        assert_eq!(launch_options(&[], &[], Some(run), Some(HIDE_HELPERS), "gamemoderun %command%"), format!("{HIDE_HELPERS} gamemoderun '{}' \"$@\" %command%", run.display()));
        assert_eq!(launch_options(&[], &[], Some(run), None, "'/home/a b/.local/share/Steam/ubuntu12_32/steam-runtime/run.sh' %command%"), "'/home/a b/.local/share/Steam/ubuntu12_32/steam-runtime/run.sh' %command%");
    }

    /// The snippet must hide exactly the /proc/self/exe helpers and run the command after it.
    #[test]
    fn hide_helpers_runs_the_command() {
        let out = std::process::Command::new("sh").arg("-c").arg(format!("{HIDE_HELPERS} printf '%s ' \"$@\"")).output().unwrap();
        assert!(String::from_utf8_lossy(&out.stdout).starts_with("/usr/bin/bwrap --dev-bind / / "));
        let extra = ["--ro-bind".to_string(), "/a b's".to_string(), "/x".to_string()];
        let out = std::process::Command::new("sh").arg("-c").arg(format!("{} printf '%s|' \"$@\"", hide_helpers_with(&extra))).output().unwrap();
        assert!(String::from_utf8_lossy(&out.stdout).starts_with("/usr/bin/bwrap|--dev-bind|/|/|--ro-bind|/a b's|/x|"));
        assert_eq!(hide_helpers_with(&[]), HIDE_HELPERS);
    }
}
