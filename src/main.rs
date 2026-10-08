mod binary;
mod check;
mod scan;
mod system;

use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "usage: gamefix check <game folder or executable>";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        ["check", path] => {
            let path = PathBuf::from(path);
            let Some(game) = scan::scan(&path) else {
                eprintln!("{}: not a folder or a file that can be read", path.display());
                return ExitCode::FAILURE;
            };
            let system = system::System::load();
            print!("{}", check::check(&game, &system).render(&game));
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}
