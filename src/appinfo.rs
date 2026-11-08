//! What Steam knows about a game, from its own cache (`appcache/appinfo.vdf`), offline: whether
//! the game has Valve Anti-Cheat, which no file in its folder says, and whether Steam always runs
//! its Linux build in a Steam Linux Runtime container (Dota 2, Counter-Strike 2).
//!
//! The file: a magic number (version 28 or 29) and the universe; in 29, where the table of key
//! names starts; then each app: id, size of the rest, four fields and two hashes, and its data as
//! binary VDF. In 29 keys are indexes into the table, in 28 text.

use std::fs;
use std::path::Path;

const V28: u32 = 0x0756_4428;
const V29: u32 = 0x0756_4429;

#[derive(Debug, PartialEq)]
pub struct AppFacts {
    pub vac: bool,
    /// The tool Steam runs its Linux build in ("SteamLinuxRuntime_sniper").
    pub linux_runtime: Option<String>,
}

#[derive(Debug)]
enum Value {
    Map(Vec<(String, Value)>),
    Text(String),
    Number(i64),
}

impl Value {
    fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Map(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
    /// Version 29's key names.
    keys: Option<Vec<&'a [u8]>>,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let out = self.bytes.get(self.at..self.at.checked_add(n)?)?;
        self.at += n;
        Some(out)
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    fn text(&mut self) -> Option<&'a [u8]> {
        let len = self.bytes.get(self.at..)?.iter().position(|b| *b == 0)?;
        let out = self.take(len)?;
        self.at += 1;
        Some(out)
    }

    fn map(&mut self, depth: usize) -> Option<Value> {
        if depth > 32 {
            return None;
        }
        let mut fields = Vec::new();
        loop {
            let kind = *self.take(1)?.first()?;
            if kind == 8 {
                return Some(Value::Map(fields));
            }
            let key = if self.keys.is_some() {
                let index = self.u32()? as usize;
                *self.keys.as_ref()?.get(index)?
            } else {
                self.text()?
            };
            let value = match kind {
                0 => self.map(depth + 1)?,
                1 => Value::Text(String::from_utf8_lossy(self.text()?).into_owned()),
                2 => Value::Number(self.u32()? as i32 as i64),
                3 | 4 | 6 => {
                    self.take(4)?;
                    Value::Number(0)
                }
                7 | 10 => Value::Number(i64::from_le_bytes(self.take(8)?.try_into().ok()?)),
                _ => return None,
            };
            fields.push((String::from_utf8_lossy(key).into_owned(), value));
        }
    }
}

fn parse(bytes: &[u8], appid: u32) -> Option<AppFacts> {
    let mut r = Reader { bytes, at: 0, keys: None };
    let version = r.u32()?;
    r.u32()?;
    let mut keys = None;
    match version {
        V28 => {}
        V29 => {
            let table = usize::try_from(u64::from_le_bytes(r.take(8)?.try_into().ok()?)).ok()?;
            let mut t = Reader { bytes, at: table, keys: None };
            let count = t.u32()? as usize;
            let mut names = Vec::new();
            for _ in 0..count {
                names.push(t.text()?);
            }
            keys = Some(names);
        }
        _ => return None,
    }
    r.keys = keys;
    loop {
        let id = r.u32()?;
        if id == 0 {
            return None;
        }
        let size = r.u32()? as usize;
        if id != appid {
            r.take(size)?;
            continue;
        }
        r.take(4 + 4 + 8 + 20 + 4 + 20)?;
        let data = r.map(0)?;
        let app = data.get("appinfo").unwrap_or(&data);
        let vac = app.get("common").and_then(|c| c.get("category")).and_then(|c| c.get("category_8")).is_some_and(|v| matches!(v, Value::Number(1)) || matches!(v, Value::Text(t) if t == "1"));
        let text = |m: &Value, key: &str| match m.get(key) {
            Some(Value::Text(t)) => Some(t.clone()),
            _ => None,
        };
        let linux_runtime = match app.get("config").and_then(|c| c.get("app_mappings")) {
            Some(Value::Map(mappings)) => mappings.iter().find_map(|(_, m)| text(m, "tool").filter(|_| text(m, "platform").as_deref() == Some("linux"))),
            _ => None,
        };
        return Some(AppFacts { vac, linux_runtime });
    }
}

/// None: Steam has no such app in its cache, or the file's format is one this doesn't read.
pub fn read(steam_root: &Path, appid: u32) -> Option<AppFacts> {
    parse(&fs::read(steam_root.join("appcache/appinfo.vdf")).ok()?, appid)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A version 29 file with one app, whose data is built from the key table's indexes.
    fn file(appid: u32, data: &[u8], keys: &[&str]) -> Vec<u8> {
        let mut app = vec![0u8; 4 + 4 + 8 + 20 + 4 + 20];
        app.extend_from_slice(data);
        let mut body = appid.to_le_bytes().to_vec();
        body.extend_from_slice(&(app.len() as u32).to_le_bytes());
        body.extend_from_slice(&app);
        body.extend_from_slice(&0u32.to_le_bytes());
        let mut out = V29.to_le_bytes().to_vec();
        out.extend_from_slice(&1u32.to_le_bytes());
        out.extend_from_slice(&((16 + body.len()) as u64).to_le_bytes());
        out.extend_from_slice(&body);
        out.extend_from_slice(&(keys.len() as u32).to_le_bytes());
        for key in keys {
            out.extend_from_slice(key.as_bytes());
            out.push(0);
        }
        out
    }

    fn key(kind: u8, index: u32) -> Vec<u8> {
        [vec![kind], index.to_le_bytes().to_vec()].concat()
    }

    #[test]
    fn reads_vac_and_the_runtime() {
        let keys = ["appinfo", "common", "category", "category_8", "config", "app_mappings", "1", "platform", "tool"];
        let data = [
            key(0, 0),
            key(0, 1),
            key(0, 2),
            key(2, 3),
            1i32.to_le_bytes().to_vec(),
            vec![8, 8],
            key(0, 4),
            key(0, 5),
            key(0, 6),
            key(1, 7),
            b"linux\0".to_vec(),
            key(1, 8),
            b"SteamLinuxRuntime_sniper\0".to_vec(),
            vec![8, 8, 8, 8, 8],
        ]
        .concat();
        let bytes = file(570, &data, &keys);
        assert_eq!(parse(&bytes, 570), Some(AppFacts { vac: true, linux_runtime: Some("SteamLinuxRuntime_sniper".into()) }));
        assert_eq!(parse(&bytes, 571), None);
        // Anything cut off or of another format says nothing.
        assert_eq!(parse(&bytes[..40], 570), None);
        assert_eq!(parse(b"not this file at all", 570), None);
    }

    /// Steam's own file, when asked for: GAMEFIX_APPINFO=<appinfo.vdf> cargo test.
    #[test]
    fn real_file() {
        let Some(path) = std::env::var_os("GAMEFIX_APPINFO") else { return };
        let bytes = fs::read(path).unwrap();
        assert_eq!(parse(&bytes, 570), Some(AppFacts { vac: true, linux_runtime: Some("SteamLinuxRuntime_sniper".into()) }));
    }
}
