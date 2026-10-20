//! Telling protonfixes which game it runs. protonfixes, inside GE-Proton, UMU-Proton and other
//! Protons (not Valve's), holds fixes for single games: Steam games by their app id, games from
//! GOG, Epic and other stores by their umu ID (from umu-database). A non-Steam shortcut's game
//! it only knows when the launch options say `UMU_ID` (and `STORE`, which picks the store's own
//! fixes). data/umu-fixes.tsv (made by tools/umu-table.py) lists the games that have a fix, by
//! their store's own id: GOG's in `goggame-<id>.info`, Epic's AppName in `.egstore/*.mancpn`.

use std::fs;
use std::path::Path;

const TABLE: &str = include_str!("../data/umu-fixes.tsv");

/// A game protonfixes has a fix for.
#[derive(Debug, PartialEq)]
pub struct Game {
    pub id: String,
    /// What STORE has to be for protonfixes to find the fix (none: its fixes for any store).
    pub store: Option<String>,
    pub title: String,
}

impl Game {
    /// The launch settings telling protonfixes which game this is.
    pub fn env(&self) -> Vec<(&'static str, String)> {
        let mut env = vec![("UMU_ID", self.id.clone())];
        env.extend(self.store.clone().map(|s| ("STORE", s)));
        env
    }

    /// Whether the protonfixes in this Proton has the fix.
    pub fn fixed_by(&self, tool_dir: &Path) -> bool {
        let folder = format!("gamefixes-{}", self.store.as_deref().unwrap_or("umu"));
        tool_dir.join("protonfixes").join(folder).join(format!("{}.py", self.id)).is_file()
    }
}

/// The game a Windows program belongs to, when protonfixes has a fix for it: by the store's id
/// in its folder or one above it (up to `root`), else by the end of the program's path, which
/// umu-database gives for a few games without a store id.
pub fn find(root: &Path, program: &Path) -> Option<Game> {
    let dirs: Vec<&Path> = match program.parent() {
        Some(dir) if dir.starts_with(root) => dir.ancestors().take_while(|d| d.starts_with(root)).collect(),
        Some(dir) => vec![dir],
        None => Vec::new(),
    };
    let mut ids = Vec::new();
    for dir in dirs {
        for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some(id) = name.strip_prefix("goggame-").and_then(|n| n.strip_suffix(".info")) {
                // A DLC's file names the game it belongs to.
                let text = fs::read_to_string(entry.path()).unwrap_or_default();
                ids.push(("gog", json_field(&text, "rootGameId").unwrap_or_else(|| id.to_string())));
            }
        }
        for entry in fs::read_dir(dir.join(".egstore")).into_iter().flatten().flatten() {
            if entry.path().extension().is_some_and(|e| e == "mancpn") {
                ids.extend(json_field(&fs::read_to_string(entry.path()).unwrap_or_default(), "AppName").map(|a| ("egs", a)));
            }
        }
    }
    let path = program.to_string_lossy().to_lowercase();
    let rows = TABLE.lines().filter(|l| !l.starts_with('#')).filter_map(|l| {
        let f: Vec<&str> = l.split('\t').collect();
        let [store, codename, id, env, ending, title] = f[..] else { return None };
        Some((store, codename, ending, Game { id: id.into(), store: (env != "-").then(|| env.into()), title: title.into() }))
    });
    let mut by_path = None;
    for (store, codename, ending, game) in rows {
        if ids.iter().any(|(s, c)| *s == store && c == codename) {
            return Some(game);
        }
        let ending = ending.to_lowercase();
        if by_path.is_none() && ending != "-" && (path.ends_with(&format!("/{ending}")) || path == ending) {
            by_path = Some(game);
        }
    }
    by_path
}

/// A string field's value in a JSON file, without a JSON reader: `"key": "value"`.
fn json_field(text: &str, key: &str) -> Option<String> {
    let rest = text.split_once(&format!("\"{key}\""))?.1.trim_start().strip_prefix(':')?.trim_start().strip_prefix('"')?;
    Some(rest.split_once('"')?.0.to_string()).filter(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_reads() {
        for line in TABLE.lines().filter(|l| !l.starts_with('#')) {
            assert_eq!(line.split('\t').count(), 6, "{line}");
        }
    }

    #[test]
    fn finds_games_by_their_store_ids() {
        let dir = std::env::temp_dir().join(format!("gamefix-umu-test-{}", std::process::id()));
        let game = dir.join("Dragons Dogma/DDDA");
        fs::create_dir_all(game.join("nativePC")).unwrap();
        // Dragon's Dogma: Dark Arisen's GOG file, and a DLC's naming it as its game.
        fs::write(game.join("goggame-1242384383.info"), "{\n    \"gameId\": \"1242384383\",\n    \"rootGameId\": \"1242384383\"\n}").unwrap();
        fs::write(game.join("goggame-1.info"), "{\"gameId\": \"1\", \"rootGameId\": \"1242384383\"}").unwrap();
        let found = find(&dir.join("Dragons Dogma"), &game.join("DDDA.exe")).unwrap();
        assert_eq!((found.id.as_str(), found.store.as_deref()), ("umu-367500", Some("gog")));
        assert_eq!(found.env(), vec![("UMU_ID", "umu-367500".to_string()), ("STORE", "gog".to_string())]);
        // Its program a folder down; nothing from above the game's own folder.
        assert!(find(&game, &game.join("nativePC/DDDA.exe")).is_some());
        assert!(find(&game.join("nativePC"), &game.join("nativePC/DDDA.exe")).is_none());
        // Epic's AppName.
        let epic = dir.join("AC");
        fs::create_dir_all(epic.join(".egstore")).unwrap();
        fs::write(epic.join(".egstore/X.mancpn"), "{\n\t\"FormatVersion\": 0,\n\t\"AppName\": \"0b9d1072cd674b8b91c8e25e9d695ed9\"\n}").unwrap();
        assert_eq!(find(&epic, &epic.join("AssassinsCreed_Game.exe")).map(|g| g.id), Some("umu-15100".into()));
        // A game known only by its program's path.
        assert_eq!(find(&dir, &dir.join("NFS/SPEED2.EXE")).map(|g| (g.id, g.store)), Some(("umu-nfsu2".into(), None)));
        assert!(find(&dir, &dir.join("NFS/notspeed2.exe")).is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn json_fields() {
        assert_eq!(json_field("{\"gameId\": \"12\"}", "gameId").as_deref(), Some("12"));
        assert_eq!(json_field("{\"gameId\":\"\"}", "gameId"), None);
        assert_eq!(json_field("{}", "gameId"), None);
    }
}
