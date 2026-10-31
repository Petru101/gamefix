use gamefix::*;
use std::io::{BufRead, IsTerminal, Write};
use std::path::PathBuf;
use std::process::{Command, ExitCode};

const USAGE: &str = "usage: gamefix check <game>
       gamefix setup <game> [--menu] [--steam]   (default: --menu)
       gamefix undo <game>
       gamefix right-click [on|off]              Dolphin's right-click menu
       gamefix games-folder [<folder>]           where installed Windows games go
       gamefix feedback [<game>]                 report a problem to gamefix's developer
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
    // Entries whose Proton moved with a Steam library point at it again.
    setup::repair_protons();

    let result = match args.as_slice() {
        ["right-click", "on"] => said("gamefix", rightclick::enable()),
        ["right-click", "off"] => said("gamefix", rightclick::disable()),
        ["right-click"] => said("gamefix", Ok(vec![format!("The right-click menu is {}.", if rightclick::enabled() { "on" } else { "off" })])),
        ["games-folder"] => said("gamefix", Ok(vec![format!("Windows games gamefix installs go into {}.", setup::games_folder().display())])),
        ["games-folder", path] => said("gamefix", setup::set_games_folder(std::path::Path::new(path))),
        ["feedback", rest @ ..] if rest.len() <= 1 => match feedback(rest.first().copied(), dialog) {
            Some(said) => said,
            // Cancelled: nothing to say.
            None => return ExitCode::SUCCESS,
        },
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
                Some(game) => {
                    let said = run(command, &game, &setup::Targets { menu: menu || !steam, steam }, dialog);
                    if *command != "check" {
                        feedback::remember(&game.name, command, &said.text);
                    }
                    said
                }
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
    let mut report = check::check(game, &system::System::load());
    if command == "check" {
        return Said { ok: true, title, text: report.render(game) };
    }
    // A game run by a program from Flathub that isn't installed: offer to install it.
    let mut installed = None;
    if command == "setup" {
        if let Some(runner) = setup::missing_runner(game, &report) {
            let size = setup::flathub_size(runner.app).map(|s| format!(" ({s} to download)")).unwrap_or_default();
            let question = format!("{} runs in {}, which isn't installed.\n\ngamefix can install it from Flathub{size}, for you only: no password needed.\n\nInstall {} now?", game.name, runner.name, runner.name);
            if confirm(&title, &question, dialog) {
                if dialog {
                    let _ = Command::new("notify-send").args([title.as_str(), &markup_escaped(&format!("Installing {} from Flathub…", runner.name))]).status();
                } else {
                    println!("Installing {} from Flathub…", runner.name);
                }
                if let Err(e) = setup::install_flatpak(runner.app) {
                    return said(&title, Err(format!("{} couldn't be installed: {e}", runner.name)));
                }
                report = check::check(game, &system::System::load());
                installed = Some(format!("Installed {} from Flathub (it stays after undo: other games can use it).", runner.name));
            }
        }
        // A Java game with no Java that fits it: offer to download one.
        if let Some(major) = setup::missing_java(game, &report) {
            let size = java::latest(major).map(|r| format!(", {} to download", r.size_text())).unwrap_or_default();
            let question = format!("{} is a Java game and needs Java {major}, which isn't installed.\n\ngamefix can download Java {major} (Eclipse Temurin, from adoptium.net{size}) into ~/.local/share/gamefix/java, for you only: no password needed.\n\nDownload it now?", game.name);
            if confirm(&title, &question, dialog) {
                if dialog {
                    let _ = Command::new("notify-send").args([title.as_str(), &markup_escaped(&format!("Downloading Java {major}…"))]).status();
                } else {
                    println!("Downloading Java {major}…");
                }
                if let Err(e) = java::install(major) {
                    return said(&title, Err(e));
                }
                report = check::check(game, &system::System::load());
                installed = Some(format!("Downloaded Java {major} (Eclipse Temurin) into ~/.local/share/gamefix/java (it stays after undo: other games can use it)."));
            }
        }
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
            let mut lines: Vec<String> = installed.iter().chain(&o.done).map(|l| format!("✓ {l}")).collect();
            lines.extend(o.not_fixed.iter().map(|l| format!("✗ {l}")));
            lines
        }),
        _ => setup::undo(game).map(|undone| undone.iter().map(|l| format!("✓ {l}")).collect()),
    };
    said(&title, result)
}

/// A bug report or feedback, about a game when one is given: what the person writes, then
/// everything that goes, then a yes or no. In windows from the right-click menu; on a terminal
/// otherwise (from a script, the message comes on stdin and is sent as it is). None: cancelled.
fn feedback(path: Option<&str>, dialog: bool) -> Option<Said> {
    let game = match path.map(|p| scan::scan(std::path::Path::new(p))) {
        Some(None) => return Some(said("gamefix", Err(format!("{}: not a folder or a file that can be read", path.unwrap())))),
        Some(Some(game)) => Some(game),
        None => None,
    };
    let checked = game.as_ref().map(|g| check::check(g, &system::System::load()).render(g));
    let mut report = feedback::Report::new(game.as_ref(), checked);
    let title = match &game {
        Some(g) => format!("gamefix: report a problem with {}", g.name),
        None => "gamefix: report a problem".into(),
    };
    let sent = |report: &feedback::Report| match report.send() {
        Ok(()) => said(&title, Ok(vec![format!("Sent, thank you. Its id is {}: mention it if you write about it elsewhere.", report.id)])),
        Err(e) => said(&title, Err(format!("{e}\nNothing was sent."))),
    };
    let along = if game.is_some() { "gamefix's report on the game and what it changed for it go along with it" } else { "your system's name and graphics card maker go along with it" };
    if dialog {
        let input = |kind: &str, text: &str| {
            let out = Command::new("kdialog").args(["--title", &title, kind, &qt_text(text), ""]).output().ok().filter(|o| o.status.success())?;
            Some(String::from_utf8_lossy(&out.stdout).trim_end().to_owned())
        };
        report.message = input("--textinputbox", &format!("What went wrong, or what would you like to tell the developer?\n({along}; you'll see everything before it's sent.)"))?;
        if report.message.trim().is_empty() {
            return None;
        }
        report.contact = input("--inputbox", "If you'd like an answer: how can the developer reach you? (optional: an email address, a Reddit or Discord name)")?;
        let preview = PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".cache/gamefix/report-preview.txt");
        let _ = std::fs::create_dir_all(preview.parent().unwrap());
        std::fs::write(&preview, report.preview()).ok()?;
        let _ = Command::new("kdialog").args(["--title", "What will be sent"]).arg("--textbox").arg(&preview).args(["800", "600"]).status();
        let _ = std::fs::remove_file(&preview);
        let question = format!("Send this report to gamefix's developer?\n\nWhat the server keeps: {}", feedback::privacy_url());
        return ask(&title, &question).then(|| sent(&report));
    }
    let stdin = std::io::stdin();
    if !stdin.is_terminal() {
        report.message = std::io::read_to_string(stdin).unwrap_or_default();
        print!("{}", report.preview());
        return Some(sent(&report));
    }
    println!("What went wrong, or what would you like to tell the developer? ({along}.)\nEnd with an empty line:");
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            break;
        }
        report.message += &format!("{line}\n");
    }
    if report.message.trim().is_empty() {
        return Some(said(&title, Err("Nothing written, nothing sent.".into())));
    }
    print!("If you'd like an answer: how can the developer reach you? (optional, Enter to skip) ");
    let _ = std::io::stdout().flush();
    let _ = stdin.lock().read_line(&mut report.contact);
    println!("\n{}", report.preview());
    let question = format!("Send this report to gamefix's developer? (What the server keeps: {})", feedback::privacy_url());
    Some(if confirm(&title, &question, false) { sent(&report) } else { said(&title, Err("Nothing was sent.".into())) })
}

/// KDE's own dialogs; without kdialog (another desktop), a notification instead.
fn show(said: &Said) {
    let kind = if said.ok { "--msgbox" } else { "--error" };
    let shown = Command::new("kdialog").args(["--title", &said.title, kind, &qt_text(said.text.trim_end())]).status().is_ok_and(|s| s.success());
    if !shown {
        let _ = Command::new("notify-send").args([&said.title, &markup_escaped(said.text.trim_end())]).status();
    }
}

fn ask(title: &str, question: &str) -> bool {
    Command::new("kdialog").args(["--title", title, "--yesno", &qt_text(question)]).status().is_ok_and(|s| s.success())
}

/// `<`, `>` and `&` as HTML writes them. Notifications read a little HTML (links too), and
/// game names and paths come from downloads.
fn markup_escaped(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// A dialog's text as Qt rich text showing every character as it is, with its line breaks and
/// indents. Left plain, Qt takes a text whose first line has something tag-like (a game named
/// "<b>...") for HTML, the rest of it included.
fn qt_text(text: &str) -> String {
    format!("<qt><p style=\"white-space:pre-wrap\">{}</p></qt>", markup_escaped(text))
}

/// A yes/no question: in a window from the right-click menu, else on the terminal (no terminal
/// to answer on, as from a script: no).
fn confirm(title: &str, question: &str, dialog: bool) -> bool {
    if dialog {
        return ask(title, question);
    }
    if !std::io::stdin().is_terminal() {
        return false;
    }
    print!("{}\n[y/N] ", question.replace("\n\n", "\n"));
    let _ = std::io::stdout().flush();
    let mut answer = String::new();
    std::io::stdin().lock().read_line(&mut answer).is_ok() && matches!(answer.trim().to_lowercase().as_str(), "y" | "yes")
}
