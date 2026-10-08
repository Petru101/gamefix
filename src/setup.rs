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

fn slug(name: &str) -> String {
    let s: String = name.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' }).collect();
    s.split('-').filter(|p| !p.is_empty()).collect::<Vec<_>>().join("-")
}

fn record_path(game: &Game) -> PathBuf {
    data_home().join("gamefix/games").join(format!("{}.log", slug(&game.name)))
}

/// One argument of a desktop entry's Exec line, quoted by the spec's rules: inside double
/// quotes `"`, `` ` ``, `$` and `\` take a backslash, and string escaping then doubles every `\`.
fn exec_arg(arg: &str) -> String {
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
        return match game.windows {
            Some(i) => setup_windows(game, &game.files[i], targets),
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
                    env.push(("LD_PRELOAD", paths.join(":")));
                    done.push("Makes its libraries load first so they get their own functions (at launch).".into());
                }
            }
            Problem::MissingLibs { .. } => not_fixed.push("Missing libraries: needs the Steam Linux Runtime, which setup doesn't do yet.".into()),
            Problem::UnitySingleInstance => {
                if Path::new(BWRAP).is_file() {
                    hide_helpers = true;
                    done.push("Hides the browser helpers it would mistake for itself (at launch).".into());
                } else {
                    not_fixed.push("Old Unity \"Player is already running\": needs bwrap, which isn't installed. If it happens, close Steam before starting.".into());
                }
            }
            Problem::NoLinuxBuild | Problem::WrongMachine(_) => {}
        }
    }
    let workdir = program.parent().unwrap_or(&game.root);
    let icon = icon(game, &main.path);

    if targets.menu {
        let mut exec = Vec::new();
        if !env.is_empty() {
            exec.push("env".to_string());
            exec.extend(env.iter().map(|(k, v)| exec_arg(&format!("{k}={v}"))));
        }
        if hide_helpers {
            exec.extend(["sh", "-c", &format!("{HIDE_HELPERS} exec \"$@\" \"$0\"")].map(exec_arg));
        }
        exec.push(exec_arg(&program.display().to_string()));
        let entry_text = format!(
            "[Desktop Entry]\nType=Application\nName={}\nComment=Set up by gamefix\nExec={}\nPath={}\nIcon={}\nCategories=Game;\nTerminal=false\nX-Gamefix-Game={}\n",
            value(&game.name),
            exec.join(" "),
            value(&workdir.display().to_string()),
            value(&icon),
            value(&game.root.display().to_string()),
        );
        let apps = data_home().join("applications");
        fs::create_dir_all(&apps).map_err(|e| e.to_string())?;
        let entry = apps.join(format!("gamefix-{}.desktop", slug(&game.name)));
        fs::write(&entry, entry_text).map_err(|e| e.to_string())?;
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
            exe: &program,
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
        .map(|(k, v)| if *k == "LD_PRELOAD" { format!("LD_PRELOAD=\"$LD_PRELOAD:{v}\"") } else { format!("{k}={}", quote(v)) })
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
