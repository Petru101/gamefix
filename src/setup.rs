//! Sets a game up once so it starts from the app menu and/or Steam, and undoes that.
//!
//! Launch-time fixes go into the entry's command or Steam's launch options as plain settings,
//! so they work without gamefix. What was changed is recorded under ~/.local/share/gamefix/games/.

use crate::check::{Problem, Report};
use crate::scan::Game;
use crate::steam::{self, Steam};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

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
    let Some((main, _)) = game.main() else {
        // A game another program runs: that program and the file to give it.
        let runner = report.problems.iter().find_map(|p| match p {
            Problem::Java { file, java: Some(java) } if file.extension().is_some_and(|e| e == "jar") => Some((vec![java.display().to_string(), "-jar".into()], file, "Java")),
            // A Processing export's script runs plain `java`, which may be Homebrew's, off the
            // PATH menu entries get; and its exec bit is often lost.
            Problem::Java { file, java: Some(java) } => {
                let dir = java.parent().map(|d| d.display().to_string()).unwrap_or_default();
                Some((vec!["env".into(), format!("PATH={dir}:/usr/bin:/bin"), "/bin/sh".into()], file, "Java"))
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
            _ => None,
        });
        return match game.windows {
            Some(i) => setup_windows(game, &game.files[i], targets),
            None if runner.is_some() => {
                let (command, file, name) = runner.unwrap();
                setup_runner(game, command, file, name, targets)
            }
            None if report.problems.iter().any(|p| matches!(p, Problem::Archive(_))) => Err("The game is still packed in an archive: unpack it, then set up the folder it makes.".into()),
            None if report.problems.iter().any(|p| matches!(p, Problem::Package(_, "apk"))) => Err("It's an Android app; get the game's Linux or Windows download instead.".into()),
            None if report.problems.iter().any(|p| matches!(p, Problem::Package(..))) => Err("It's a package to install into the system; get the game's .tar.gz, .zip or AppImage download instead.".into()),
            None if report.problems.iter().any(|p| matches!(p, Problem::Love { .. } | Problem::Python { .. } | Problem::Java { .. } | Problem::Flash { .. })) => {
                Err("This game needs another program to run it (see gamefix check); setting it up isn't supported yet.".into())
            }
            None => Err("No game program in this folder. If it's an installer, install the game first and set up the folder it installs to.".into()),
        };
    };
    if report.problems.iter().any(|p| matches!(p, Problem::WrongMachine(_))) {
        return Err("The program is built for a different processor; nothing can make it run here.".into());
    }
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

    let program = game.full(main);
    // Launch-time settings, as (name, value).
    let mut env: Vec<(&str, String)> = Vec::new();
    // Whether to start it through HIDE_HELPERS.
    let mut hide_helpers = false;
    // Steam's runtime for older games, for the app-menu entry (Steam uses it by itself).
    let mut old_runtime = None;
    // Libraries to load first, joined into one LD_PRELOAD.
    let mut preload: Vec<String> = Vec::new();
    let mut done = Vec::new();
    let mut not_fixed = Vec::new();
    let mut record = Vec::new();
    for problem in &report.problems {
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
            Problem::OwnHarfbuzz(_, Some(path)) => {
                preload.push(path.clone());
                done.push("Loads the system's HarfBuzz first, so its text libraries don't get the game's old copy (at launch).".into());
            }
            Problem::OwnHarfbuzz(_, None) => not_fixed.push("Its built-in HarfBuzz: the system's libharfbuzz.so.0 to preload wasn't found.".into()),
            Problem::OwnLibstdcxx(None) => not_fixed.push("Its built-in C++ library: the system's libstdc++.so.6 to preload wasn't found.".into()),
            Problem::Sdl2Mix(path) => {
                preload.push(path.clone());
                done.push("Loads the runtime's own SDL2, to match its SDL2 add-ons (at launch).".into());
            }
            Problem::DotnetIcu(_) => {
                env.push(("DOTNET_SYSTEM_GLOBALIZATION_INVARIANT", "1".into()));
                done.push("Lets its old .NET start without the ICU library (at launch).".into());
            }
            Problem::MissingLibs { libs, in_old_runtime, misplaced, .. } => {
                if let Some(run) = report.old_runtime.as_ref().filter(|_| !in_old_runtime.is_empty()) {
                    old_runtime = Some(run.clone());
                    done.push("Starts it with Steam's runtime for older games, which has missing libraries (Steam does that by itself).".into());
                }
                // Just the libraries the system lacks, linked into a folder of their own: putting
                // the game's folders on LD_LIBRARY_PATH would put everything else in them first
                // too (Liberation Circuit's bundle holds its own glibc and an old libpulse).
                if !misplaced.is_empty() {
                    let links = data_home().join("gamefix/libs").join(slug(&game.name));
                    // LD_LIBRARY_PATH splits on colons.
                    if links.display().to_string().contains(':') {
                        not_fixed.push("Libraries the game ships in a folder Linux doesn't look in: the home folder's path has a colon, which the fix can't handle.".into());
                    } else {
                        let new_dir = !links.exists();
                        fs::create_dir_all(&links).map_err(|e| e.to_string())?;
                        for (lib, dir) in misplaced {
                            let link = links.join(lib);
                            let _ = fs::remove_file(&link);
                            std::os::unix::fs::symlink(game.root.join(dir).join(lib), &link).map_err(|e| e.to_string())?;
                            record.push(format!("created\t{}", link.display()));
                        }
                        // After the links: undo goes in order and removes the folder once empty.
                        if new_dir {
                            record.push(format!("created-dir\t{}", links.display()));
                        }
                        env.push(("LD_LIBRARY_PATH", links.display().to_string()));
                        done.push("Points Linux to the libraries the game ships itself (at launch, through links in ~/.local/share/gamefix/libs).".into());
                    }
                }
                let others: Vec<&str> = libs.keys().filter(|l| !in_old_runtime.contains(*l) && !misplaced.contains_key(*l)).map(String::as_str).collect();
                if !others.is_empty() {
                    not_fixed.push(format!("Missing libraries nothing here provides: {}.", others.join(", ")));
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
            Problem::UnityH264(_) => not_fixed.push("Its H.264 videos: Unity's Linux player can't play them; the Windows download with Proton can.".into()),
            Problem::NwjsFonts(_) => {
                env.push(("FONTCONFIG_FILE", plain_fonts()?.display().to_string()));
                done.push("Gives its NW.js a plain font list it can read (at launch; the first start takes longer).".into());
            }
            Problem::Alsa32(Some((pcm, ctl))) => {
                env.push(("ALSA_CONFIG_PATH", alsa_pulse32(pcm, ctl)?));
                done.push("Sends its 32-bit ALSA sound through Steam's runtime's PulseAudio plugin (at launch).".into());
            }
            Problem::OldPulse(server) => {
                env.push(("PULSE_SERVER", server.clone()));
                done.push("Tells its old PulseAudio library where the sound server is (at launch).".into());
            }
            Problem::NetStandard(_) => not_fixed.push("Its .NET Standard parts: the Mono it brings can't load them; the .exe with Proton can.".into()),
            Problem::Alsa32(None) => not_fixed.push("Its 32-bit ALSA sound: needs Steam's runtime (install Steam), which has a plugin that works.".into()),
            Problem::KickFiles(files) => not_fixed.push(format!("Its missing Mono files ({}): only the developer can add them; the .exe with Proton works.", files.join(", "))),
            Problem::FnaLibs(libs) => not_fixed.push(format!("Its missing native libraries ({}): get FNA's fnalibs, or use the Windows download with Proton.", libs.join(", "))),
            Problem::NoLinuxBuild | Problem::Damaged(_) | Problem::Package(..) | Problem::Flash { .. } | Problem::Archive(_) | Problem::WrongMachine(_) | Problem::Love { .. } | Problem::Python { .. } | Problem::Java { .. } => {}
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
    // The developer's launcher script, when it sets up where the libraries are; else the program.
    let start = game.launcher().map(|l| game.full(l)).unwrap_or_else(|| program.clone());
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
    let workdir = exodus_top.or(start.parent()).unwrap_or(&game.root);
    let icon = icon(game, &main.path);

    if targets.menu {
        let mut exec = Vec::new();
        if !env.is_empty() {
            exec.push("env".to_string());
            exec.extend(env.iter().map(|(k, v)| exec_arg(&format!("{k}={v}"))));
        }
        if let Some(run) = &old_runtime {
            exec.push(exec_arg(&run.display().to_string()));
        }
        if hide_helpers {
            exec.extend(["sh", "-c", &format!("{HIDE_HELPERS} exec \"$@\" \"$0\"")].map(exec_arg));
        }
        exec.push(exec_arg(&start.display().to_string()));
        let entry = write_entry(game, &exec, workdir, &icon)?;
        record.push(format!("created\t{}", entry.display()));
        done.push(format!("Added \"{}\" to the app menu, under Games.", game.name));
    }

    if let (Some(steam), Some(appid)) = (&steam, steam::installed_app(&game.root)) {
        // A Steam game: its own launch options, keeping whatever was there.
        let file = steam.localconfig();
        let previous = steam::launch_options(&file, appid)?;
        let options = launch_options(&env, hide_helpers, previous.as_deref().unwrap_or(""));
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
            launch_options: &launch_options(&env, hide_helpers, ""),
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

/// Writes the game's app-menu entry running `exec` (already quoted) in `workdir`.
fn write_entry(game: &Game, exec: &[String], workdir: &Path, icon: &str) -> Result<PathBuf, String> {
    let entry_text = format!(
        "[Desktop Entry]\nType=Application\nName={}\nComment=Set up by gamefix\nExec={}\nPath={}\nIcon={}\nCategories=Game;\nTerminal=false\nX-Gamefix-Game={}\n",
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

/// A game another program runs (`java -jar`, LÖVE): an app-menu entry running it on the file.
fn setup_runner(game: &Game, command: Vec<String>, file: &Path, name: &str, targets: &Targets) -> Result<Outcome, String> {
    if !targets.menu {
        return Err(format!("{name} games can only be set up in the app menu for now: add --menu."));
    }
    if record_path(game).exists() {
        undo(game)?;
    }
    let file = game.root.join(file);
    let exec: Vec<String> = command.iter().chain([&file.display().to_string()]).map(|a| exec_arg(a)).collect();
    let entry = write_entry(game, &exec, file.parent().unwrap_or(&game.root), "applications-games")?;
    let record_file = record_path(game);
    fs::create_dir_all(record_file.parent().unwrap()).map_err(|e| e.to_string())?;
    fs::write(&record_file, format!("created\t{}\n", entry.display())).map_err(|e| e.to_string())?;
    refresh_menu();
    let not_fixed = if targets.steam { vec![format!("Steam shortcuts for {name} games aren't supported yet; start it from the app menu.")] } else { Vec::new() };
    Ok(Outcome { done: vec![format!("Added \"{}\" to the app menu, under Games (run with {name}).", game.name)], not_fixed })
}

/// A Windows-only game: a non-Steam shortcut forced to Proton, like ticking "Force the use of a
/// specific Steam Play compatibility tool" in its properties.
fn setup_windows(game: &Game, exe: &crate::scan::Found, targets: &Targets) -> Result<Outcome, String> {
    if !targets.steam {
        return Err("Windows games can only be set up in Steam for now: add --steam.".into());
    }
    let steam = Steam::find().ok_or("Steam isn't installed, or nobody has logged in to it yet.")?;
    if steam.running() {
        return Err(STEAM_OPEN.into());
    }
    let proton = steam.proton().ok_or("No Proton is installed in Steam. Install \"Proton Experimental\" from the Steam library (it's listed under Tools) and try again.")?;
    if record_path(game).exists() {
        undo(game)?;
    }
    let program = game.full(exe);
    let workdir = program.parent().unwrap_or(&game.root);
    let file = steam.shortcuts();
    let shortcut = steam::Shortcut { name: &game.name, exe: &program, start_dir: workdir, icon: "", launch_options: "" };
    let appid = steam::add_shortcut(&file, &shortcut)?;
    let mut record = vec![format!("steam-shortcut\t{appid}\t{}", file.display())];
    let config = steam.config();
    let previous = steam::compat_tool(&config, appid)?;
    steam::set_compat_tool(&config, appid, Some(&proton))?;
    record.push(format!("steam-compat\t{appid}\t{}\t{}", config.display(), previous.map_or("-".to_string(), |p| format!("={p}"))));

    let record_file = record_path(game);
    fs::create_dir_all(record_file.parent().unwrap()).map_err(|e| e.to_string())?;
    fs::write(&record_file, record.join("\n") + "\n").map_err(|e| e.to_string())?;
    let mut not_fixed = Vec::new();
    if targets.menu {
        not_fixed.push("App-menu entries for Windows games aren't supported yet; start it from Steam.".into());
    }
    Ok(Outcome {
        done: vec![format!("Added \"{}\" to Steam as a non-Steam game.", game.name), format!("Set it to run with Proton ({proton}).")],
        not_fixed,
    })
}

/// Steam runs launch options with `sh -c`, `%command%` standing for its launch helpers and the
/// game. Settings go first and the wrapper right before `%command%`, so they fit around options
/// already there (`gamemoderun %command%`, or bare game arguments like `-insecure`). Steam
/// itself preloads its overlay, so LD_PRELOAD adds to it instead of replacing it.
fn launch_options(env: &[(&str, String)], hide_helpers: bool, existing: &str) -> String {
    if env.is_empty() && !hide_helpers {
        return existing.to_string();
    }
    let quote = |v: &str| if v.chars().all(|c| c.is_ascii_alphanumeric() || "/-_.=+:,".contains(c)) { v.to_string() } else { format!("'{}'", v.replace('\'', "'\\''")) };
    let settings: Vec<String> = env
        .iter()
        .map(|(k, v)| if matches!(*k, "LD_PRELOAD" | "LD_LIBRARY_PATH") { format!("{k}=\"${k}:{v}\"") } else { format!("{k}={}", quote(v)) })
        .collect();
    let command = if hide_helpers { "\"$@\" %command%" } else { "%command%" };
    let existing = existing.trim();
    let rest = if existing.contains("%command%") {
        existing.replacen("%command%", command, 1)
    } else if existing.is_empty() {
        command.to_string()
    } else {
        format!("{command} {existing}")
    };
    let line = settings.into_iter().chain([rest]).collect::<Vec<_>>().join(" ");
    if hide_helpers { format!("{HIDE_HELPERS} {line}") } else { line }
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
            ["mode", mode, path] => {
                if let Ok(mode) = u32::from_str_radix(mode, 8) {
                    if fs::set_permissions(path, fs::Permissions::from_mode(mode)).is_ok() {
                        undone.push(format!("Restored the permissions of {path}"));
                    }
                }
            }
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
    fn slugs_keep_any_alphabet() {
        assert_eq!(slug("Final Shot"), "final-shot");
        assert_eq!(slug("ᗢ"), "ᗢ");
        assert!(slug("!!!").starts_with("game-"));
    }

    #[test]
    fn launch_options_fit_around_existing_ones() {
        let env = [("GLIBC_TUNABLES", "glibc.rtld.execstack=2".to_string())];
        assert_eq!(launch_options(&env, false, ""), "GLIBC_TUNABLES=glibc.rtld.execstack=2 %command%");
        assert_eq!(launch_options(&[], true, "STEAM_COMPAT_LAUNCHER_SERVICE=proton %command%"), format!("{HIDE_HELPERS} STEAM_COMPAT_LAUNCHER_SERVICE=proton \"$@\" %command%"));
        assert_eq!(launch_options(&env, false, "-insecure"), "GLIBC_TUNABLES=glibc.rtld.execstack=2 %command% -insecure");
        assert_eq!(launch_options(&[], false, "-insecure"), "-insecure");
    }

    /// The snippet must hide exactly the /proc/self/exe helpers and run the command after it.
    #[test]
    fn hide_helpers_runs_the_command() {
        let out = std::process::Command::new("sh").arg("-c").arg(format!("{HIDE_HELPERS} printf '%s ' \"$@\"")).output().unwrap();
        assert!(String::from_utf8_lossy(&out.stdout).starts_with("/usr/bin/bwrap --dev-bind / / "));
    }
}
