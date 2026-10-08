mod binary;
mod check;
mod scan;
mod setup;
mod steam;
mod vdf;
mod system;

use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "usage: gamefix check <game>
       gamefix setup <game> [--menu] [--steam]   (default: --menu)
       gamefix undo <game>
<game> is the game's folder or its program";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        [command @ ("check" | "setup" | "undo"), path, flags @ ..] => {
            let (menu, steam) = (flags.contains(&"--menu"), flags.contains(&"--steam"));
            if flags.iter().any(|f| !matches!(*f, "--menu" | "--steam")) || (*command != "setup" && !flags.is_empty()) {
                eprintln!("{USAGE}");
                return ExitCode::from(2);
            }
            let targets = setup::Targets { menu: menu || !steam, steam };
            let path = PathBuf::from(path);
            let Some(game) = scan::scan(&path) else {
                eprintln!("{}: not a folder or a file that can be read", path.display());
                return ExitCode::FAILURE;
            };
            let system = system::System::load();
            let report = check::check(&game, &system);
            let result = match *command {
                "check" => {
                    print!("{}", report.render(&game));
                    return ExitCode::SUCCESS;
                }
                "setup" => setup::setup(&game, &report, &targets).map(|o| {
                    for line in &o.done {
                        println!("✓ {line}");
                    }
                    for line in &o.not_fixed {
                        println!("✗ {line}");
                    }
                }),
                _ => setup::undo(&game).map(|undone| {
                    for line in &undone {
                        println!("✓ {line}");
                    }
                }),
            };
            match result {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("{e}");
                    ExitCode::FAILURE
                }
            }
        }
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}
