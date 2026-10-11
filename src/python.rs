//! Python 3 games shipped as source: what the game imports that neither Python nor the game
//! itself has (pygame, mostly), which gamefix setup installs from PyPI into a folder of the
//! game's own (a venv in ~/.local/share/gamefix/python/<slug>), whether it runs in a terminal
//! (End User: a text adventure through picotui), and the developer's script that starts it.
//! Python 3 itself is part of Bazzite and Fedora.

use crate::binary::Binary;
use crate::scan::Found;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct Plan {
    /// The system's Python 3.
    pub python: Option<PathBuf>,
    /// The developer's script that starts the game (End User's run.sh sets PYSDL2_DLL_PATH and
    /// runs `python3 main.py`), relative to the game's root.
    pub launcher: Option<PathBuf>,
    /// PyPI packages for what it imports that Python and the game lack, in install order.
    pub packages: Vec<String>,
    /// Modules the game has only as Cython source (`.pyx`), which need compiling first
    /// (Injection's fx.pyx).
    pub compile: Vec<String>,
    /// A text game that needs a terminal.
    pub terminal: bool,
    /// Files the code names that exist only with other upper/lower case (Pixel Adventure:
    /// `graphics/Titlescreen/title.png` is `Title.png`): Windows doesn't mind, Linux does.
    /// (name the code uses, name on disk), both relative to the game's root; the first
    /// differing part of the path, so a folder can be one.
    pub case_links: Vec<(PathBuf, PathBuf)>,
    /// The newest Python 3 minor version the code still runs on, when the system's is newer,
    /// and what it uses that later ones removed (PPU: ConfigParser.readfp, gone in 3.12).
    pub older: Option<(u32, String)>,
    /// The system's Python 3 minor version (14 for 3.14).
    pub system_minor: Option<u32>,
}

/// What Python removed that old games use: (the code's text, the first 3.x without it, its
/// name). Module removals are in `REMOVED_MODULES`.
const REMOVED: &[(&str, u32, &str)] = &[
    ("time.clock(", 8, "time.clock"),
    ("cgi.escape(", 8, "cgi.escape"),
    ("platform.linux_distribution(", 8, "platform.linux_distribution"),
    ("platform.dist(", 8, "platform.dist"),
    ("sys.setcheckinterval(", 9, "sys.setcheckinterval"),
    (".isAlive()", 9, "Thread.isAlive"),
    ("base64.encodestring(", 9, "base64.encodestring"),
    ("base64.decodestring(", 9, "base64.decodestring"),
    (".getchildren()", 9, "Element.getchildren"),
    ("fractions.gcd(", 9, "fractions.gcd"),
    ("collections.Mapping", 10, "collections.Mapping"),
    ("collections.MutableMapping", 10, "collections.MutableMapping"),
    ("collections.Sequence", 10, "collections.Sequence"),
    ("collections.Iterable", 10, "collections.Iterable"),
    ("collections.Callable", 10, "collections.Callable"),
    ("inspect.getargspec(", 11, "inspect.getargspec"),
    ("asyncio.coroutine", 11, "asyncio.coroutine"),
    (".readfp(", 12, "ConfigParser.readfp"),
    ("SafeConfigParser", 12, "SafeConfigParser"),
    ("locale.format(", 12, "locale.format"),
    ("ssl.wrap_socket(", 12, "ssl.wrap_socket"),
    ("locale.resetlocale(", 13, "locale.resetlocale"),
];

/// Standard modules Python removed, and the first 3.x without them.
const REMOVED_MODULES: &[(&str, u32)] = &[
    ("imp", 12), ("distutils", 12), ("asynchat", 12), ("asyncore", 12), ("smtpd", 12),
    ("cgi", 13), ("cgitb", 13), ("crypt", 13), ("imghdr", 13), ("mailcap", 13), ("nntplib", 13), ("pipes", 13), ("sndhdr", 13),
    ("spwd", 13), ("sunau", 13), ("telnetlib", 13), ("uu", 13), ("xdrlib", 13), ("audioop", 13), ("chunk", 13), ("lib2to3", 13),
    ("msilib", 13), ("nis", 13), ("ossaudiodev", 13),
];

/// Names `from collections import ...` lost in 3.10 (they moved to collections.abc).
const COLLECTIONS_ABC: &[&str] = &["Mapping", "MutableMapping", "Sequence", "MutableSequence", "Iterable", "Iterator", "Callable", "Set", "MutableSet", "Hashable", "Sized", "Container"];

/// python-build-standalone's relocatable Pythons (Astral's builds, the ones uv installs), for
/// games whose code needs an older Python than the system's: (3.x, release, version, SHA-256,
/// download size in MB). Nothing older than 3.8 is built that way.
const BUILDS: &[(u32, &str, &str, &str, u32)] = &[
    (8, "20241002", "3.8.20", "285e141c36f88b2e9357654c5f77d1f8fb29cc25132698fe35bb30d787f38e87", 27),
    (9, "20251031", "3.9.25", "42834f61eb6df43432c3dd6ab9ca3fdf8c06d10a404ebdb53d6902e6b9570b08", 41),
    (10, "20261009", "3.10.22", "1780de9ddda04e956d2a4a0ad09b7f5667d92f4f12cd0881ec939de5d542903f", 44),
    (11, "20261009", "3.11.17", "035b0ccfe7f3d38ab3ddab505290d200c6a265b95fbd5fac76c358751d6bb6a8", 49),
    (12, "20261009", "3.12.15", "5864001bf969db329fadc2b654c4878c6027d170c6bd77e28f88666260bcaf63", 67),
    (13, "20261009", "3.13.16", "1cca1041f693f110a2e0d16417090b8996258c7a121e90eeab66c2f40ea407f2", 75),
];

/// The Python build for 3.`minor`: its full version and download size in MB.
pub fn build_info(minor: u32) -> Option<(&'static str, u32)> {
    BUILDS.iter().find(|b| b.0 == minor).map(|b| (b.2, b.4))
}

fn builds_folder() -> PathBuf {
    let data = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/share"));
    data.join("gamefix/python-builds")
}

/// Python 3.`minor` from python-build-standalone, downloaded and unpacked once into
/// ~/.local/share/gamefix/python-builds/3.x for every game that needs it (undo leaves it):
/// its python3, and what was done when it had to be fetched.
pub fn older_python(minor: u32) -> Result<(PathBuf, Option<String>), String> {
    let &(_, release, version, sha, size) = BUILDS.iter().find(|b| b.0 == minor).ok_or(format!("No Python 3.{minor} build is available."))?;
    let dir = builds_folder().join(format!("3.{minor}"));
    let python = dir.join("python/bin/python3");
    if dir.join("ready").is_file() && python.is_file() {
        return Ok((python, None));
    }
    let failed = |what: &str| format!("Couldn't set up Python {version}: {what}.");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).map_err(|e| failed(&e.to_string()))?;
    let result = (|| {
        let name = format!("cpython-{version}+{release}-x86_64-unknown-linux-gnu-install_only.tar.gz");
        let file = dir.join(&name);
        let url = format!("https://github.com/astral-sh/python-build-standalone/releases/download/{release}/{name}");
        let got = Command::new("curl").args(["-sfL", "--max-time", "600", "-o"]).arg(&file).arg(&url).status().is_ok_and(|s| s.success());
        if !got {
            return Err(failed("the download from GitHub failed (no network?)"));
        }
        let sum = Command::new("sha256sum").arg(&file).output().map_err(|e| failed(&e.to_string()))?;
        if String::from_utf8_lossy(&sum.stdout).split_whitespace().next() != Some(sha) {
            return Err(failed("the download doesn't match its checksum"));
        }
        let out = Command::new("tar").arg("xzf").arg(&file).arg("-C").arg(&dir).output().map_err(|e| failed(&e.to_string()))?;
        let _ = fs::remove_file(&file);
        if !out.status.success() || !python.is_file() {
            return Err(failed("it couldn't be unpacked"));
        }
        fs::write(dir.join("ready"), format!("Python {version}, python-build-standalone {release}\n")).map_err(|e| failed(&e.to_string()))?;
        Ok(format!("Set up Python {version} (python-build-standalone, {size} MB) in {}, for every game that needs it (undo leaves it).", dir.display()))
    })();
    match result {
        Ok(note) => Ok((python, Some(note))),
        Err(e) => {
            let _ = fs::remove_dir_all(&dir);
            Err(e)
        }
    }
}

/// Modules for a terminal's screen and keyboard; with none of `GUI`, the game is a text game.
const TERMINAL: &[&str] = &["curses", "termios", "tty", "picotui", "blessed", "urwid", "npyscreen", "asciimatics", "prompt_toolkit"];
const GUI: &[&str] = &["pygame", "pyglet", "arcade", "tkinter", "Tkinter", "kivy", "PyQt5", "PyQt6", "PySide2", "PySide6", "wx", "OpenGL", "ursina", "panda3d", "pyray", "raylib"];

/// The PyPI package for an imported module, where the names differ. pygame itself has no
/// wheels for current Python (3.14 on Fedora 44) and would need building; pygame-ce, the
/// community fork that keeps its API and module name, has them.
fn package(module: &str, older: bool) -> String {
    match module {
        // pygame itself for an older Python (wheels up to 3.13): what old games were made with.
        "pygame" if older => "pygame",
        "pygame" => "pygame-ce",
        "PIL" => "pillow",
        "yaml" => "pyyaml",
        "OpenGL" => "PyOpenGL",
        "cv2" => "opencv-python",
        "serial" => "pyserial",
        "pyray" | "raylib" => "raylib",
        "sklearn" => "scikit-learn",
        "bs4" => "beautifulsoup4",
        m => m,
    }
    .to_string()
}

pub fn python3() -> Option<PathBuf> {
    ["/usr/bin/python3", "/bin/python3"].iter().map(PathBuf::from).find(|p| p.is_file())
}

/// The system Python's minor version and the names of its own modules (3.10 and later list
/// them).
fn stdlib(python: &Path) -> (Option<u32>, BTreeSet<String>) {
    let out = Command::new(python).args(["-I", "-c", "import sys; print(sys.version_info[1]); print('\\n'.join(sys.stdlib_module_names))"]).output();
    let text = out.map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
    let mut lines = text.lines();
    let minor = lines.next().and_then(|l| l.trim().parse().ok());
    (minor, lines.map(String::from).collect())
}

/// Modules a file imports at its top level (`import a, b.c as d`, `from e.f import g`).
/// Indented imports are left out: those inside `try:` are optional by design (Gunship
/// Diplomacy's moviepy and android), and those in functions or `if` blocks run only sometimes
/// (pgfw's pyaudio, for recording videos).
fn top_imports(text: &str) -> Vec<String> {
    let mut names = Vec::new();
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim_end();
        if let Some(rest) = line.strip_prefix("import ") {
            names.extend(rest.split(',').filter_map(|p| p.split_whitespace().next()).map(|n| n.split('.').next().unwrap_or(n).to_string()));
        } else if let Some(rest) = line.strip_prefix("from ") {
            let module = rest.split_whitespace().next().unwrap_or("");
            if !module.starts_with('.') {
                names.push(module.split('.').next().unwrap_or(module).to_string());
            }
        }
    }
    names.retain(|n| !n.is_empty() && n.chars().all(|c| c.is_alphanumeric() || c == '_'));
    names
}

/// Every module name any line imports, indented or not.
fn all_imports(text: &str) -> Vec<String> {
    top_imports(&text.lines().map(str::trim_start).collect::<Vec<_>>().join("\n"))
}

/// Build scripts, not the game: setup.py (distutils, py2exe, Cython) and the like.
fn build_script(path: &Path) -> bool {
    path.file_name().is_some_and(|n| n.to_string_lossy().to_lowercase().starts_with("setup"))
}

/// The script among the game's files that starts this Python file: a line naming `python`
/// and the file (`python3 main.py $1`), in the file's folder or above it.
pub fn launcher(root: &Path, files: &[Found], start: &Path) -> Option<PathBuf> {
    let name = start.file_name()?.to_string_lossy().into_owned();
    let dir = start.parent().unwrap_or(Path::new(""));
    files
        .iter()
        .filter(|f| matches!(f.binary, Binary::Script) && dir.starts_with(f.path.parent().unwrap_or(Path::new(""))))
        .filter(|f| {
            let text = fs::read(root.join(&f.path)).map(|b| String::from_utf8_lossy(&b[..b.len().min(64 << 10)]).into_owned()).unwrap_or_default();
            text.lines().any(|l| !l.trim_start().starts_with('#') && l.contains("python") && l.split(|c: char| c.is_whitespace() || c == '/' || c == '"' || c == '\'').any(|w| w == name))
        })
        .min_by_key(|f| f.path.components().count())
        .map(|f| f.path.clone())
}

/// What a Python 3 game needs, from its files: `start` (relative to `root`) is the file that
/// starts it. Only what it reaches counts: imports are followed from it through the game's own
/// modules (End User's old game.py imports a `content` module that no longer exists; main.py
/// never loads it), each found beside the start file first, else anywhere in the game (a game
/// may add its folders to sys.path itself). A package counts with every file in it.
pub fn plan(root: &Path, files: &[Found], start: &Path) -> Plan {
    let python = python3();
    let launcher = launcher(root, files, start);
    let base = root.join(start.parent().unwrap_or(Path::new("")));
    // The game's modules by name: .py files and compiled ones (.so), packages (folders), and
    // Cython sources.
    let mut modules: std::collections::BTreeMap<String, Vec<PathBuf>> = Default::default();
    let mut cython = BTreeSet::new();
    walk(root, 0, &mut |path, is_dir| {
        let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else { return };
        let stem = name.split('.').next().unwrap_or("").to_string();
        match path.extension().and_then(|e| e.to_str()) {
            _ if is_dir => modules.entry(name).or_default().push(path.to_path_buf()),
            Some("py") | Some("so") | Some("pyd") => modules.entry(stem).or_default().push(path.to_path_buf()),
            Some("pyx") => {
                cython.insert(stem);
            }
            _ => {}
        }
    });
    let (system_minor, std) = python.as_deref().map(stdlib).unwrap_or_default();
    let mut removed: Vec<(u32, String)> = Vec::new();
    let mut needed = BTreeSet::new();
    let mut compile = BTreeSet::new();
    let mut seen: BTreeSet<PathBuf> = BTreeSet::new();
    let mut texts = Vec::new();
    let mut queue = vec![root.join(start)];
    while let Some(file) = queue.pop() {
        if !seen.insert(file.clone()) || build_script(&file) {
            continue;
        }
        let Ok(text) = fs::read_to_string(&file) else { continue };
        // The game's own modules are followed wherever they're imported (OPEN-GAME and
        // functions import them late); others count only at the top (see top_imports).
        let top = top_imports(&text);
        for module in all_imports(&text) {
            if let Some(&(_, gone)) = REMOVED_MODULES.iter().find(|(m, _)| *m == module).filter(|_| top.contains(&module)) {
                removed.push((gone, format!("the {module} module")));
                continue;
            }
            if module == "__future__" || std.contains(&module) {
                continue;
            }
            match modules.get(&module) {
                Some(paths) => {
                    let near: Vec<&PathBuf> = paths.iter().filter(|p| p.parent() == Some(&base)).collect();
                    for path in if near.is_empty() { paths.iter().collect() } else { near } {
                        if path.is_dir() {
                            walk(path, 0, &mut |p, is_dir| {
                                if !is_dir && p.extension().is_some_and(|e| e == "py") {
                                    queue.push(p.to_path_buf());
                                }
                            });
                        } else if path.extension().is_some_and(|e| e == "py") {
                            queue.push(path.clone());
                        }
                    }
                }
                None if !top.contains(&module) => {}
                None if cython.contains(&module) => {
                    compile.insert(module);
                }
                None => {
                    needed.insert(module);
                }
            }
        }
        // Not inside a lambda, which runs only if called: PySDL2's compatibility helper
        // `callable = lambda x: isinstance(x, collections.Callable)` (End User runs on 3.14).
        for &(code, gone, name) in REMOVED {
            if text.lines().any(|l| l.contains(code) && !l.contains("lambda") && !l.trim_start().starts_with('#')) {
                removed.push((gone, name.to_string()));
            }
        }
        for line in text.lines().filter(|l| l.trim_start().starts_with("from collections import")) {
            if let Some(name) = COLLECTIONS_ABC.iter().find(|n| line.split(|c: char| !c.is_alphanumeric() && c != '_').any(|w| w == **n)) {
                removed.push((10, format!("collections.{name}")));
            }
        }
        texts.push(text);
    }
    // The newest Python that still has all of it, when the system's is newer.
    let older = removed.iter().min_by_key(|(gone, _)| *gone).filter(|(gone, _)| system_minor.is_some_and(|m| m >= *gone)).map(|(gone, what)| (gone - 1, format!("{what}, which Python 3.{gone} removed")));
    // Without the system's list nothing can be told apart; say nothing rather than everything.
    if std.is_empty() {
        needed.clear();
    }
    let imported: BTreeSet<String> = texts.iter().flat_map(|t| all_imports(t)).collect();
    let gui = GUI.iter().any(|m| imported.contains(*m));
    let terminal = !gui && (TERMINAL.iter().any(|m| imported.contains(*m)) || texts.iter().any(|t| t.contains("input(")));
    let mut packages: Vec<String> = needed.iter().map(|m| package(m, older.is_some())).collect();
    packages.dedup();
    let case_links = case_links(root, &base, &texts);
    Plan { python, launcher, packages, compile: compile.into_iter().collect(), terminal, case_links, older, system_minor }
}

/// File types games load by name.
const ASSETS: &[&str] = &["png", "jpg", "jpeg", "gif", "bmp", "tga", "ogg", "wav", "mp3", "flac", "ttf", "otf", "fnt", "json", "txt", "csv", "tmx", "tsx", "xml", "dat"];

/// Paths in the code's strings (`'graphics/Titlescreen/title.png'`, relative to the start
/// file's folder, where the entry starts it) that don't exist as written but do with other
/// case: the first part that differs and what it's called on disk.
fn case_links(root: &Path, base: &Path, texts: &[String]) -> Vec<(PathBuf, PathBuf)> {
    let mut links = BTreeSet::new();
    for text in texts {
        for quote in ['"', '\''] {
            for (i, literal) in text.split(quote).enumerate() {
                if i % 2 == 0 || literal.contains('\n') || literal.starts_with('/') || !literal.rsplit_once('.').is_some_and(|(_, e)| ASSETS.contains(&e.to_lowercase().as_str())) {
                    continue;
                }
                let wanted = Path::new(literal);
                if base.join(wanted).exists() {
                    continue;
                }
                let mut here = base.to_path_buf();
                for part in wanted.components().map(|c| c.as_os_str().to_string_lossy().into_owned()) {
                    if part == "." || part == ".." {
                        here.push(&part);
                        continue;
                    }
                    if here.join(&part).exists() {
                        here.push(&part);
                        continue;
                    }
                    let lower = part.to_lowercase();
                    let found = fs::read_dir(&here).into_iter().flatten().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).find(|n| n.to_lowercase() == lower);
                    if let (Some(found), Ok(dir)) = (found, here.strip_prefix(root)) {
                        // Only when the rest of the path is there under the real name.
                        let rest: PathBuf = wanted.components().skip_while(|c| c.as_os_str() != part.as_str()).skip(1).collect();
                        if case_insensitive_exists(&here.join(&found), &rest) {
                            links.insert((dir.join(&part), dir.join(&found)));
                        }
                    }
                    break;
                }
            }
        }
    }
    links.into_iter().collect()
}

/// Whether `rest` exists under `dir` with any upper/lower case.
fn case_insensitive_exists(dir: &Path, rest: &Path) -> bool {
    let mut here = dir.to_path_buf();
    for part in rest.components() {
        let lower = part.as_os_str().to_string_lossy().to_lowercase();
        match fs::read_dir(&here).into_iter().flatten().flatten().find(|e| e.file_name().to_string_lossy().to_lowercase() == lower) {
            Some(e) => here = e.path(),
            None => return false,
        }
    }
    true
}

/// Files and folders under `dir`, not following links, at most 6 deep, skipping caches,
/// virtual environments and version control.
fn walk(dir: &Path, depth: usize, f: &mut dyn FnMut(&Path, bool)) {
    if depth > 6 {
        return;
    }
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else { continue };
        if kind.is_dir() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || ["__pycache__", "venv", "env", "site-packages", "download"].contains(&name.as_str()) {
                continue;
            }
            f(&path, true);
            walk(&path, depth + 1, f);
        } else if kind.is_file() {
            f(&path, false);
        }
    }
}

/// Installs `packages` from PyPI into a new virtual environment at `dir`, from wheels only
/// (nothing to compile: Bazzite has no compiler).
pub fn install(python: &Path, dir: &Path, packages: &[String]) -> Result<(), String> {
    let out = Command::new(python).arg("-m").arg("venv").arg(dir).stdin(std::process::Stdio::null()).output().map_err(|e| format!("python3 -m venv: {e}"))?;
    if !out.status.success() {
        return Err(format!("Couldn't make a Python environment in {}: {}", dir.display(), String::from_utf8_lossy(&out.stderr).trim()));
    }
    let out = Command::new(dir.join("bin/python"))
        .args(["-m", "pip", "install", "--quiet", "--disable-pip-version-check", "--only-binary=:all:"])
        .args(packages)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("pip: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let last: Vec<&str> = err.lines().filter(|l| !l.trim().is_empty()).collect();
        return Err(format!("pip couldn't install {}: {}", packages.join(", "), last[last.len().saturating_sub(3)..].join(" ")));
    }
    Ok(())
}

/// The interpreter a script names on its `#!` line (`/bin/bash`), else /bin/sh.
pub fn interpreter(script: &Path) -> String {
    let text = fs::read(script).unwrap_or_default();
    let first = String::from_utf8_lossy(&text[..text.len().min(256)]).lines().next().unwrap_or("").to_string();
    let words: Vec<&str> = first.strip_prefix("#!").map(|l| l.split_whitespace().collect()).unwrap_or_default();
    match words.as_slice() {
        [env, name, ..] if env.ends_with("/env") => format!("/usr/bin/{name}"),
        [path, ..] => path.to_string(),
        [] => "/bin/sh".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imports() {
        let text = "import os, sys\nimport pygame.mixer as mixer\nfrom config import path\nfrom . import x\ntry:\n    import moviepy\nexcept ImportError:\n    pass\ndef f():\n    import pyaudio\n# import numpy\n";
        assert_eq!(top_imports(text), ["os", "sys", "pygame", "config"]);
        assert!(all_imports(text).contains(&"moviepy".to_string()));
        assert!(all_imports(text).contains(&"pyaudio".to_string()));
        assert_eq!(package("pygame", false), "pygame-ce");
        assert_eq!(package("pygame", true), "pygame");
    }

    #[test]
    fn case_only_differences() {
        let root = std::env::temp_dir().join(format!("gamefix-case-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("game/graphics/Titlescreen")).unwrap();
        fs::write(root.join("game/graphics/Titlescreen/Title.png"), "").unwrap();
        fs::write(root.join("game/graphics/Titlescreen/play.png"), "").unwrap();
        let code = "a = load('graphics/Titlescreen/title.png')\nb = load(\"graphics/titlescreen/play.png\")\nc = load('graphics/Titlescreen/play.png')\nd = load('graphics/missing.png')".to_string();
        let links = case_links(&root, &root.join("game"), &[code]);
        let _ = fs::remove_dir_all(&root);
        assert_eq!(links, [(PathBuf::from("game/graphics/Titlescreen/title.png"), PathBuf::from("game/graphics/Titlescreen/Title.png")), (PathBuf::from("game/graphics/titlescreen"), PathBuf::from("game/graphics/Titlescreen"))]);
    }
}
