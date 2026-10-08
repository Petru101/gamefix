//! Sets a game up once so it starts from an app-menu entry, and undoes that.
//!
//! Launch-time fixes go into the entry's command as plain `env` settings, so the entry works
//! without gamefix. What was changed is recorded under ~/.local/share/gamefix/games/.

use crate::check::{Problem, Report};
use crate::scan::Game;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct Outcome {
    pub entry: PathBuf,
    pub done: Vec<String>,
    pub not_fixed: Vec<String>,
}

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

pub fn setup(game: &Game, report: &Report) -> Result<Outcome, String> {
    let Some((main, _)) = game.main() else {
        return Err("No Linux program in this folder. Setting up Windows games isn't supported yet.".into());
    };
    if report.problems.iter().any(|p| matches!(p, Problem::WrongMachine(_))) {
        return Err("The program is built for a different processor; nothing can make it run here.".into());
    }
    if record_path(game).exists() {
        undo(game)?;
    }

    let program = game.full(main);
    let mut env = Vec::new();
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
                env.push("GLIBC_TUNABLES=glibc.rtld.execstack=2".to_string());
                done.push("Lets its libraries that ask for an executable stack load (in the entry's command).".into());
            }
            Problem::FmodClash(libs) => {
                let paths: Vec<String> = libs.iter().map(|p| game.root.join(p).display().to_string()).collect();
                // LD_PRELOAD splits on spaces and colons, so such paths can't be listed in it.
                if paths.iter().any(|p| p.contains([' ', ':'])) {
                    not_fixed.push("Sound (FMOD clash): the game's path has a space or colon, which the fix can't handle; move the game to a folder without them.".into());
                } else {
                    env.push(format!("LD_PRELOAD={}", paths.join(":")));
                    done.push("Makes the game's sound library use its own FMOD instead of Unity's built-in one (in the entry's command).".into());
                }
            }
            Problem::MissingLibs { .. } => not_fixed.push("Missing libraries: needs the Steam Linux Runtime, which setup doesn't do yet.".into()),
            Problem::UnitySingleInstance => not_fixed.push("Old Unity \"Player is already running\": not handled yet. If it happens, close Steam before starting.".into()),
            Problem::NoLinuxBuild | Problem::WrongMachine(_) => {}
        }
    }

    let mut exec = Vec::new();
    if !env.is_empty() {
        exec.push("env".to_string());
        exec.extend(env.iter().map(|e| exec_arg(e)));
    }
    exec.push(exec_arg(&program.display().to_string()));
    let workdir = program.parent().unwrap_or(&game.root);
    let entry_text = format!(
        "[Desktop Entry]\nType=Application\nName={}\nComment=Set up by gamefix\nExec={}\nPath={}\nIcon={}\nCategories=Game;\nTerminal=false\nX-Gamefix-Game={}\n",
        value(&game.name),
        exec.join(" "),
        value(&workdir.display().to_string()),
        value(&icon(game, &main.path)),
        value(&game.root.display().to_string()),
    );
    let apps = data_home().join("applications");
    fs::create_dir_all(&apps).map_err(|e| e.to_string())?;
    let entry = apps.join(format!("gamefix-{}.desktop", slug(&game.name)));
    fs::write(&entry, entry_text).map_err(|e| e.to_string())?;
    record.push(format!("created\t{}", entry.display()));
    done.push(format!("Added \"{}\" to the app menu, under Games.", game.name));

    let record_file = record_path(game);
    fs::create_dir_all(record_file.parent().unwrap()).map_err(|e| e.to_string())?;
    fs::write(&record_file, record.join("\n") + "\n").map_err(|e| e.to_string())?;
    refresh_menu();
    Ok(Outcome { entry, done, not_fixed })
}

/// Reverses a setup, newest change first. Returns what was undone.
pub fn undo(game: &Game) -> Result<Vec<String>, String> {
    let record_file = record_path(game);
    let record = fs::read_to_string(&record_file).map_err(|_| format!("{} wasn't set up by gamefix.", game.name))?;
    let mut undone = Vec::new();
    for line in record.lines().rev() {
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
