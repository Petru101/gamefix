//! Bug reports and feedback, sent to gamefix's server (`server/`, a Cloudflare Worker) with
//! curl: what the person writes, plus gamefix's report on the game, what it last said about it
//! and what it changed for it. They see exactly what goes before it's sent (`Report::preview`);
//! the home folder's path, the user name and the Steam account number are taken out first.

use crate::scan::Game;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// Where reports go; `GAMEFIX_SERVER` points a test build at a local `wrangler dev`.
const SERVER: &str = "https://gamefix.petru101.workers.dev";

/// What's kept and by whom, for the person to read before sending.
pub fn privacy_url() -> String {
    format!("{}/privacy", server())
}

fn server() -> String {
    std::env::var("GAMEFIX_SERVER").unwrap_or(SERVER.into())
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
}

pub struct Report {
    /// A random id for this report (`new_id`), shown once it's sent so the person can point to
    /// it; the server refuses a second report with the same one.
    pub id: String,
    pub message: String,
    /// How the person would like an answer, if at all.
    pub contact: String,
    pub game: Option<String>,
    pub system: String,
    /// `gamefix check`'s report.
    pub report: Option<String>,
    /// What the last check, setup or undo of the game said (`remember`).
    pub last: Option<String>,
    /// What gamefix changed for the game: its record.
    pub record: Option<String>,
}

impl Report {
    /// Everything gamefix knows about the game, ready for a message.
    pub fn new(game: Option<&Game>, report: Option<String>) -> Report {
        let read = |path: PathBuf| fs::read_to_string(path).ok().map(|t| scrub(&t)).filter(|t| !t.trim().is_empty());
        Report {
            id: new_id(),
            message: String::new(),
            contact: String::new(),
            game: game.map(|g| g.name.clone()),
            system: system(),
            report: report.map(|r| scrub(&r)),
            last: game.and_then(|g| read(last_path(&g.name))),
            record: game.and_then(|g| read(crate::setup::record_path(g))),
        }
    }

    /// What the server gets.
    fn body(&self, install: &str) -> String {
        let field = |name: &str, v: Option<&str>| v.map(|v| format!(",{}:{}", json_string(name), json_string(v))).unwrap_or_default();
        format!(
            "{{\"id\":{},\"install\":{},\"version\":{},\"message\":{}{}{}{}{}{}{}}}",
            json_string(&self.id),
            json_string(install),
            json_string(env!("CARGO_PKG_VERSION")),
            json_string(self.message.trim()),
            field("contact", Some(self.contact.trim()).filter(|c| !c.is_empty())),
            field("game", self.game.as_deref()),
            field("system", Some(&self.system)),
            field("report", self.report.as_deref()),
            field("last", self.last.as_deref()),
            field("record", self.record.as_deref()),
        )
    }

    /// Everything that is sent, as the person reads it.
    pub fn preview(&self) -> String {
        let mut text = format!("Report {}\ngamefix {}\nSystem: {}\n", self.id, env!("CARGO_PKG_VERSION"), self.system);
        if let Some(g) = &self.game {
            text += &format!("Game: {g}\n");
        }
        if !self.contact.trim().is_empty() {
            text += &format!("Contact: {}\n", self.contact.trim());
        }
        text += &format!("\n{}\n", self.message.trim());
        for (title, part) in [("gamefix's report on the game", &self.report), ("What gamefix last said about it", &self.last), ("What gamefix changed for it", &self.record)] {
            if let Some(p) = part {
                text += &format!("\n--- {title}\n{}\n", p.trim_end());
            }
        }
        text += "\n--- Also sent: a random id gamefix made for this install (it limits floods of reports).\n";
        text
    }

    /// Sends it (up to 30 s). On success the person is shown its id.
    pub fn send(&self) -> Result<(), String> {
        if self.message.trim().is_empty() {
            return Err("Write what went wrong first.".into());
        }
        let mut curl = Command::new("curl")
            .args(["-sS", "--max-time", "30", "-X", "POST", "-H", "content-type: application/json", "--data-binary", "@-", "-w", "\n%{http_code}"])
            .arg(format!("{}/v1/feedback", server()))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("curl couldn't start ({e}), so nothing was sent."))?;
        let _ = curl.stdin.take().unwrap().write_all(self.body(&install_id()).as_bytes());
        let out = curl.wait_with_output().map_err(|e| e.to_string())?;
        let text = String::from_utf8_lossy(&out.stdout);
        let (body, status) = text.rsplit_once('\n').unwrap_or(("", &text));
        match status.trim() {
            "200" => Ok(()),
            _ if !out.status.success() => Err(format!("Couldn't reach gamefix's server ({}). Check your connection and try again.", String::from_utf8_lossy(&out.stderr).trim())),
            "429" => Err("Too many reports from here in the last hour: try again later.".into()),
            "413" => Err("This report is too big to send.".into()),
            "409" => Err("A report with this id was sent already: start again.".into()),
            status => {
                let why = body.split_once("\"error\":\"").and_then(|(_, r)| r.split_once('"')).map_or(body.trim(), |(e, _)| e);
                Err(format!("gamefix's server refused it ({status} {why})."))
            }
        }
    }
}

/// Where what gamefix last said about a game is kept, for a report on it.
fn last_path(name: &str) -> PathBuf {
    home().join(".cache/gamefix/last").join(format!("{}.txt", crate::setup::slug(name)))
}

/// Keeps what a check, setup or undo of a game said, so a report on it can include it.
pub fn remember(name: &str, command: &str, text: &str) {
    let path = last_path(name);
    let _ = fs::create_dir_all(path.parent().unwrap());
    let _ = fs::write(path, format!("gamefix {command}\n{text}"));
}

/// One line about the system: the distribution, graphics card makers, Steam's old runtime.
fn system() -> String {
    // Bazzite's PRETTY_NAME is just "Bazzite"; VERSION says which build ("44.20261002.0 (Kinoite)").
    let release = fs::read_to_string("/etc/os-release").unwrap_or_default();
    let field = |key: &str| release.lines().find_map(|l| l.strip_prefix(key).map(|v| v.trim_matches('"').to_owned())).filter(|v| !v.is_empty());
    let os = match (field("PRETTY_NAME="), field("VERSION=")) {
        (Some(name), Some(version)) if !name.chars().any(|c| c.is_ascii_digit()) => format!("{name} {version}"),
        (Some(name), _) => name,
        _ => "unknown Linux".into(),
    };
    let mut gpus: Vec<&str> = fs::read_dir("/sys/class/drm")
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| fs::read_to_string(e.path().join("device/vendor")).ok())
        .map(|v| match v.trim() {
            "0x10de" => "NVIDIA",
            "0x1002" => "AMD",
            "0x8086" => "Intel",
            _ => "other",
        })
        .collect();
    gpus.sort();
    gpus.dedup();
    let runtime = if home().join(".local/share/Steam/ubuntu12_32/steam-runtime/run.sh").is_file() { "yes" } else { "no" };
    format!("{os}; graphics: {}; Steam's old runtime: {runtime}", if gpus.is_empty() { "unknown".into() } else { gpus.join(", ") })
}

/// The person's home folder, user name and Steam account number out of a text. Bazzite's
/// /home is a link to /var/home, removable drives are under /run/media/<user>, and Steam keeps
/// each account's settings under userdata/<account number>.
fn scrub(text: &str) -> String {
    let home = home().display().to_string();
    let home = home.trim_end_matches('/');
    let mut text = text.to_owned();
    if home.len() > 1 {
        let real = home.strip_prefix("/var").unwrap_or(home);
        for path in [format!("/var{real}"), real.to_owned()] {
            text = text.replace(&path.replace('/', "\\"), "~").replace(&path, "~");
        }
    }
    if let Some(user) = std::env::var("USER").ok().filter(|u| u.len() > 1) {
        text = text.replace(&format!("/run/media/{user}/"), "/run/media/<user>/");
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text.as_str();
    while let Some(at) = rest.find("userdata/") {
        let (before, after) = rest.split_at(at + "userdata/".len());
        out += before;
        let digits = after.bytes().take_while(u8::is_ascii_digit).count();
        if digits > 0 {
            out += "<account>";
        }
        rest = &after[digits..];
    }
    out + rest
}

/// Letters and digits that can't be mistaken for each other when read out or typed (no I, L,
/// O, U: Crockford's base 32).
const ID_CHARS: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

fn random<const N: usize>() -> [u8; N] {
    let mut bytes = [0u8; N];
    if let Ok(mut f) = fs::File::open("/dev/urandom") {
        std::io::Read::read_exact(&mut f, &mut bytes).ok();
    }
    bytes
}

/// An id to show: "K7Q2-9XMB".
fn new_id() -> String {
    let chars: Vec<char> = random::<8>().iter().map(|b| ID_CHARS[(b % 32) as usize] as char).collect();
    format!("{}-{}", chars[..4].iter().collect::<String>(), chars[4..].iter().collect::<String>())
}

/// A random id made once per install: the server's flood limits count by it. Nothing else is
/// tied to it.
fn install_id() -> String {
    let path = home().join(".config/gamefix/install-id");
    if let Some(id) = fs::read_to_string(&path).ok().map(|s| s.trim().to_owned()).filter(|s| s.len() == 32) {
        return id;
    }
    let id: String = random::<16>().iter().map(|b| format!("{b:02x}")).collect();
    let _ = fs::create_dir_all(path.parent().unwrap());
    let _ = fs::write(&path, &id);
    id
}

fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn takes_out_who_it_is() {
        let home = std::env::var("HOME").unwrap();
        let real = home.strip_prefix("/var").unwrap_or(&home).to_owned();
        let wine = format!("Z:/var{real}/Games").replace('/', "\\");
        let text = format!("{real}/Downloads/x and /var{real}/.local/share/Steam/userdata/12345678/config/shortcuts.vdf, {wine}");
        assert_eq!(scrub(&text), "~/Downloads/x and ~/.local/share/Steam/userdata/<account>/config/shortcuts.vdf, Z:~\\Games");
    }

    #[test]
    fn sends_what_was_written() {
        let r = Report { id: new_id(), message: " hi \"there\"\n".into(), contact: " ".into(), game: Some("Bookworm".into()), system: "Linux".into(), report: None, last: None, record: None };
        let body = r.body("0123456789abcdef0123456789abcdef");
        assert!(body.starts_with(&format!("{{\"id\":\"{}\"", r.id)));
        assert!(body.contains("\"message\":\"hi \\\"there\\\"\""));
        assert!(body.contains("\"game\":\"Bookworm\"") && !body.contains("contact") && !body.contains("report"));
        assert_eq!(r.id.len(), 9);
        assert!(r.id.chars().filter(|&c| c != '-').all(|c| ID_CHARS.contains(&(c as u8))));
    }
}
