mod binary;
mod check;
mod disc;
mod fetch;
mod love;
mod proton;
mod rightclick;
mod scan;
mod setup;
mod steam;
mod system;
mod vdf;

use std::path::PathBuf;
use std::process::{Command, ExitCode};

const USAGE: &str = "usage: gamefix check <game>
       gamefix setup <game> [--menu] [--steam]   (default: --menu)
       gamefix undo <game>
       gamefix right-click [on|off]              Dolphin's right-click menu
<game> is the game's folder or its program; --dialog shows the result in a window";

/// What a command has to say, and whether it went well.
struct Said {
    ok: bool,
    title: String,
    text: String,
}

/// One line per item, or the error.
fn said(title: &str, result: Result<Vec<String>, String>) -> Said {
    match result {
        Ok(lines) => Said { ok: true, title: title.into(), text: lines.iter().map(|l| format!("{l}\n")).collect() },
        Err(e) => Said { ok: false, title: title.into(), text: format!("{e}\n") },
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut args: Vec<&str> = args.iter().map(String::as_str).collect();
    let dialog = args.contains(&"--dialog");
    args.retain(|a| *a != "--dialog");

    let result = match args.as_slice() {
        ["right-click", "on"] => said("gamefix", rightclick::enable()),
        ["right-click", "off"] => said("gamefix", rightclick::disable()),
        ["right-click"] => said("gamefix", Ok(vec![format!("The right-click menu is {}.", if rightclick::enabled() { "on" } else { "off" })])),
        [command @ ("check" | "setup" | "undo"), rest @ ..] => {
            // Dolphin passes the path last, after the flags.
            let (flags, paths): (Vec<&str>, Vec<&str>) = rest.iter().partition(|a| a.starts_with("--"));
            let (menu, steam) = (flags.contains(&"--menu"), flags.contains(&"--steam"));
            if paths.len() != 1 || flags.iter().any(|f| !matches!(*f, "--menu" | "--steam")) || (*command != "setup" && !flags.is_empty()) {
                eprintln!("{USAGE}");
                return ExitCode::from(2);
            }
            let path = PathBuf::from(paths[0]);
            match scan::scan(&path) {
                None => said("gamefix", Err(format!("{}: not a folder or a file that can be read", path.display()))),
                Some(game) => run(command, &game, &setup::Targets { menu: menu || !steam, steam }, dialog),
            }
        }
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };

    if dialog {
        show(&result);
    } else if result.ok {
        print!("{}", result.text);
    } else {
        eprint!("{}", result.text);
    }
    if result.ok { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

fn run(command: &str, game: &scan::Game, targets: &setup::Targets, dialog: bool) -> Said {
    let title = format!("gamefix: {}", game.name);
    let report = check::check(game, &system::System::load());
    if command == "check" {
        return Said { ok: true, title, text: report.render(game) };
    }
    // From the right-click menu there's no terminal to say "close Steam first" in, so ask.
    let needs_steam = targets.steam || (command == "undo" && setup::touches_steam(game));
    if dialog && needs_steam {
        if let Some(steam) = steam::Steam::find().filter(|s| s.running()) {
            let question = "Steam has to be closed while gamefix changes its settings: it rewrites them when it quits.\n\nClose Steam now and continue?";
            if !ask(&title, question) {
                return said(&title, Err("Nothing was changed.".into()));
            }
            if !steam.shut_down() {
                return said(&title, Err("Steam didn't close within a minute, so nothing was changed. Close it yourself (Steam → Exit) and try again.".into()));
            }
        }
    }
    let result = match command {
        "setup" => setup::setup(game, &report, targets).map(|o| {
            let mut lines: Vec<String> = o.done.iter().map(|l| format!("✓ {l}")).collect();
            lines.extend(o.not_fixed.iter().map(|l| format!("✗ {l}")));
            lines
        }),
        _ => setup::undo(game).map(|undone| undone.iter().map(|l| format!("✓ {l}")).collect()),
    };
    said(&title, result)
}

/// KDE's own dialogs; without kdialog (another desktop), a notification instead.
fn show(said: &Said) {
    let kind = if said.ok { "--msgbox" } else { "--error" };
    let shown = Command::new("kdialog").args(["--title", &said.title, kind, said.text.trim_end()]).status().is_ok_and(|s| s.success());
    if !shown {
        let _ = Command::new("notify-send").args([&said.title, said.text.trim_end()]).status();
    }
}

fn ask(title: &str, question: &str) -> bool {
    Command::new("kdialog").args(["--title", title, "--yesno", question]).status().is_ok_and(|s| s.success())
}
