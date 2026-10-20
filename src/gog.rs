//! GOG's own description of a game, as GOG Galaxy reads it: `goggame-<id>.info` names the
//! program its primary play task starts, with arguments and working folder, which may not be the
//! one gamefix would pick (Beyond Good and Evil: CheckApplication.exe, which asks for the
//! settings once and then starts BGE.exe; BGE.exe alone stops at "Application settings not
//! correctly set"). `gog-support/<id>/*.exe` is the helper Galaxy runs once after installing,
//! which sets up what the game expects from Windows (Beyond Good and Evil's puts Creative's
//! eax.dll into the system folder and writes Ubisoft's registry keys).

use std::fs;
use std::path::{Path, PathBuf};

pub struct Info {
    pub id: String,
    /// The folder holding the .info, which its paths are relative to.
    pub dir: PathBuf,
    /// The primary play task's program, as found on disk (GOG's names ignore case).
    pub program: PathBuf,
    pub args: Vec<String>,
    pub workdir: Option<PathBuf>,
    /// The first-run helper, if GOG ships one.
    pub helper: Option<PathBuf>,
}

/// The game's .info in its folder or up to two folders down (Galaxy-style folders keep it next
/// to the game: `Beyond Good and Evil/Beyond Good and Evil/goggame-1207658746.info`). A DLC's
/// .info names its game in rootGameId and is skipped.
pub fn read(root: &Path) -> Option<Info> {
    let mut dirs = vec![(root.to_path_buf(), 0)];
    while let Some((dir, depth)) = dirs.pop() {
        for entry in fs::read_dir(&dir).into_iter().flatten().flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                if depth < 2 && !name.starts_with('.') && name != "gog-support" {
                    dirs.push((entry.path(), depth + 1));
                }
                continue;
            }
            if !(name.starts_with("goggame-") && name.ends_with(".info")) {
                continue;
            }
            let text = fs::read_to_string(entry.path()).unwrap_or_default();
            if let Some(info) = parse(text.trim_start_matches('\u{feff}'), &dir) {
                return Some(info);
            }
        }
    }
    None
}

fn parse(text: &str, dir: &Path) -> Option<Info> {
    let doc = Json::parse(text)?;
    let id = doc.get("gameId")?.str()?.to_string();
    if doc.get("rootGameId").and_then(Json::str).is_some_and(|root| root != id) {
        return None;
    }
    let Json::Array(tasks) = doc.get("playTasks")? else { return None };
    let task = tasks.iter().find(|t| matches!(t.get("isPrimary"), Some(Json::Bool(true))) && t.get("type").and_then(Json::str) == Some("FileTask"))?;
    let program = find_nocase(dir, task.get("path")?.str()?)?;
    let args = task.get("arguments").and_then(Json::str).map(split_args).unwrap_or_default();
    let workdir = task.get("workingDir").and_then(Json::str).filter(|w| !w.is_empty()).and_then(|w| find_nocase(dir, w));
    let helper = fs::read_dir(dir.join("gog-support").join(&id))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("exe")));
    Some(Info { id, dir: dir.to_path_buf(), program, args, workdir, helper })
}

/// A file in a folder written the Windows way (`\` as the separator, any case), as it is on disk.
pub fn find_nocase(dir: &Path, rel: &str) -> Option<PathBuf> {
    rel.split(['\\', '/']).filter(|p| !p.is_empty() && *p != ".").try_fold(dir.to_path_buf(), |at, part| {
        fs::read_dir(&at).ok()?.flatten().map(|e| e.path()).find(|p| p.file_name().is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case(part)))
    })
}

/// A Windows command line's arguments: split at spaces outside double quotes, the quotes dropped.
fn split_args(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut word: Option<String> = None;
    let mut quoted = false;
    for c in line.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                word.get_or_insert_with(String::new);
            }
            c if c.is_whitespace() && !quoted => out.extend(word.take()),
            c => word.get_or_insert_with(String::new).push(c),
        }
    }
    out.extend(word);
    out
}

/// Just enough JSON for GOG's .info files.
#[derive(Debug, PartialEq)]
enum Json {
    Null,
    Bool(bool),
    Num(String),
    Str(String),
    Array(Vec<Json>),
    Object(Vec<(String, Json)>),
}

impl Json {
    fn parse(text: &str) -> Option<Json> {
        let mut chars = text.chars().peekable();
        let value = Self::value(&mut chars)?;
        Self::space(&mut chars);
        chars.peek().is_none().then_some(value)
    }

    fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Object(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    fn str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }

    fn space(chars: &mut std::iter::Peekable<std::str::Chars>) {
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
    }

    fn value(chars: &mut std::iter::Peekable<std::str::Chars>) -> Option<Json> {
        Self::space(chars);
        match *chars.peek()? {
            '{' => {
                chars.next();
                let mut fields = Vec::new();
                loop {
                    Self::space(chars);
                    if chars.peek() == Some(&'}') {
                        chars.next();
                        return Some(Json::Object(fields));
                    }
                    let Json::Str(key) = Self::value(chars)? else { return None };
                    Self::space(chars);
                    (chars.next()? == ':').then_some(())?;
                    fields.push((key, Self::value(chars)?));
                    Self::space(chars);
                    match chars.next()? {
                        ',' => {}
                        '}' => return Some(Json::Object(fields)),
                        _ => return None,
                    }
                }
            }
            '[' => {
                chars.next();
                let mut items = Vec::new();
                loop {
                    Self::space(chars);
                    if chars.peek() == Some(&']') {
                        chars.next();
                        return Some(Json::Array(items));
                    }
                    items.push(Self::value(chars)?);
                    Self::space(chars);
                    match chars.next()? {
                        ',' => {}
                        ']' => return Some(Json::Array(items)),
                        _ => return None,
                    }
                }
            }
            '"' => {
                chars.next();
                let mut s = String::new();
                loop {
                    match chars.next()? {
                        '"' => return Some(Json::Str(s)),
                        '\\' => match chars.next()? {
                            'n' => s.push('\n'),
                            't' => s.push('\t'),
                            'r' => s.push('\r'),
                            'b' => s.push('\u{8}'),
                            'f' => s.push('\u{c}'),
                            'u' => {
                                let hex: String = (0..4).filter_map(|_| chars.next()).collect();
                                s.push(char::from_u32(u32::from_str_radix(&hex, 16).ok()?).unwrap_or('\u{fffd}'));
                            }
                            c => s.push(c),
                        },
                        c => s.push(c),
                    }
                }
            }
            _ => {
                let mut word = String::new();
                while chars.peek().is_some_and(|c| c.is_alphanumeric() || "+-.".contains(*c)) {
                    word.push(chars.next()?);
                }
                match word.as_str() {
                    "true" => Some(Json::Bool(true)),
                    "false" => Some(Json::Bool(false)),
                    "null" => Some(Json::Null),
                    w if w.parse::<f64>().is_ok() => Some(Json::Num(word)),
                    _ => None,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json() {
        let doc = Json::parse(r#" {"a": [1, -2.5e3, true, null], "b": {"c": "x\"y\\z\u00e9"}, "d": []} "#).unwrap();
        assert_eq!(doc.get("b").and_then(|b| b.get("c")).and_then(Json::str), Some("x\"y\\zé"));
        assert_eq!(doc.get("a"), Some(&Json::Array(vec![Json::Num("1".into()), Json::Num("-2.5e3".into()), Json::Bool(true), Json::Null])));
        assert!(Json::parse("{\"a\": 1} x").is_none());
    }

    #[test]
    fn windows_arguments() {
        assert_eq!(split_args(r#"-conf "..\dosboxT2K.conf" -noconsole -c "exit""#), vec!["-conf", r"..\dosboxT2K.conf", "-noconsole", "-c", "exit"]);
        assert_eq!(split_args(r".\videos\cdv.avi  dmln.exe"), vec![r".\videos\cdv.avi", "dmln.exe"]);
        assert_eq!(split_args(r#""""#), vec![""]);
        assert!(split_args("  ").is_empty());
    }

    #[test]
    fn reads_the_primary_task() {
        let root = std::env::temp_dir().join(format!("gamefix-gog-test-{}", std::process::id()));
        let game = root.join("Beyond Good and Evil");
        fs::create_dir_all(game.join("gog-support/1207658746")).unwrap();
        fs::create_dir_all(game.join("Bin")).unwrap();
        for f in ["CheckApplication.exe", "BGE.exe", "Bin/Tool.exe", "gog-support/1207658746/galaxy_bge.exe"] {
            fs::write(game.join(f), "").unwrap();
        }
        // As GOG writes it: a byte-order mark, its own case for the name, a URL task.
        let info = "\u{feff}{\"gameId\": \"1207658746\", \"rootGameId\": \"1207658746\", \"playTasks\": [
            {\"category\": \"document\", \"type\": \"URLTask\", \"link\": \"https://gog.com\"},
            {\"isPrimary\": true, \"type\": \"FileTask\", \"path\": \"checkapplication.exe\", \"arguments\": \"-a \\\"b c\\\"\", \"workingDir\": \"bin\"}]}";
        fs::write(game.join("goggame-1207658746.info"), info).unwrap();
        fs::write(game.join("goggame-1.info"), "{\"gameId\": \"1\", \"rootGameId\": \"1207658746\", \"playTasks\": []}").unwrap();
        let found = read(&root).unwrap();
        assert_eq!(found.id, "1207658746");
        assert_eq!(found.program, game.join("CheckApplication.exe"));
        assert_eq!(found.args, vec!["-a", "b c"]);
        assert_eq!(found.workdir, Some(game.join("Bin")));
        assert_eq!(found.helper, Some(game.join("gog-support/1207658746/galaxy_bge.exe")));
        // A primary task naming a file that isn't there.
        fs::write(game.join("goggame-1207658746.info"), "{\"gameId\": \"1207658746\", \"playTasks\": [{\"isPrimary\": true, \"type\": \"FileTask\", \"path\": \"gone.exe\"}]}").unwrap();
        assert!(read(&root).is_none());
        let _ = fs::remove_dir_all(&root);
    }
}
