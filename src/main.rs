mod binary;
mod check;
mod scan;
mod setup;
mod system;

use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "usage: gamefix check|setup|undo <game folder or executable>";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        [command @ ("check" | "setup" | "undo"), path] => {
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
                "setup" => setup::setup(&game, &report).map(|o| {
                    for line in &o.done {
                        println!("✓ {line}");
                    }
                    for line in &o.not_fixed {
                        println!("✗ {line}");
                    }
                    println!("Entry: {}", o.entry.display());
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
