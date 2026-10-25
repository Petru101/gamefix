//! Repacks (FitGirl's, and others built the same way): an Inno Setup installer beside FreeArc
//! archives (`fg-01.bin` ...), which it unpacks with ISDone's 32-bit `unarc.dll`. Three things
//! go wrong under Proton, found by the lobby session installing Borderlands 4 (129 GB):
//!
//! - unarc sizes its buffers by grabbing the biggest free block of memory. Proton's default
//!   runs 32-bit programs in a 32-bit Linux process, leaving ~1.65 GB in one piece (Windows
//!   ~2.0 GB), so a later 119 MB allocation fails and its retry overflows the stack ("Unable to
//!   write data to disk / archive data corrupted"). Proton Experimental's WoW64 leaves the whole
//!   2 GB high half free and unarc's search for the biggest block overflows and loops forever.
//!   GE-Proton's WoW64 keeps its top 1 MB reserved, as Windows does in effect: that works.
//! - Torrent clients make the optional parts left out of a download anyway, full size but
//!   empty (sparse, zeros), and the installer offers every archive there is.
//! - An unpack error can end the install quietly: the repack's `_Redist/fitgirl.md5` lists
//!   every file it should have made, which tells.

use std::fs;
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

const FREEARC: &[u8] = b"ArC\x01";

/// A repack's installer folder.
pub struct Repack {
    /// Archives the download didn't finish, which only the optional parts may be.
    pub unfinished: Vec<PathBuf>,
}

fn starts_with(path: &Path, magic: &[u8]) -> bool {
    let mut head = vec![0u8; magic.len()];
    fs::File::open(path).and_then(|mut f| f.read_exact(&mut head)).is_ok() && head == magic
}

/// An archive torrent's placeholder: most of it not on disk yet, or its start still zeros.
fn unfinished(path: &Path) -> bool {
    let Ok(meta) = fs::metadata(path) else { return false };
    meta.blocks() * 512 < meta.len() / 2 || starts_with(path, &[0; 16])
}

/// The installer `setup` as a repack: FreeArc archives beside it.
pub fn find(setup: &Path) -> Option<Repack> {
    let dir = setup.parent()?;
    let files: Vec<PathBuf> = fs::read_dir(dir).ok()?.flatten().map(|e| e.path()).filter(|p| p.is_file()).collect();
    let extension = |p: &Path| p.extension().map(|e| e.to_ascii_lowercase());
    let kinds: Vec<_> = files.iter().filter(|p| starts_with(p, FREEARC)).map(|p| extension(p)).collect();
    if kinds.is_empty() {
        return None;
    }
    // Every file of an archive's kind ("bin"), a placeholder possibly not even starting right.
    let unfinished = files.into_iter().filter(|p| kinds.contains(&extension(p)) && unfinished(p)).collect();
    Some(Repack { unfinished })
}

/// Whether a repack's archive is an optional part ("fg-optional-french-vo.bin", older repacks'
/// "fg-selective-english.bin"), which a download may leave out.
pub fn optional(archive: &Path) -> bool {
    let name = archive.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    name.contains("optional") || name.contains("selective")
}

/// How an installed repack's files compare with its `_Redist/fitgirl.md5` (`hash *..\path`,
/// relative to _Redist), found in the program's folder or one above: how many there are, those
/// that differ, those missing. None without the list.
pub struct Checked {
    pub listed: usize,
    pub differ: Vec<String>,
    pub missing: Vec<String>,
}

pub fn check(program: &Path) -> Option<Checked> {
    let redist = program.ancestors().skip(1).map(|d| d.join("_Redist")).find(|r| r.join("fitgirl.md5").is_file())?;
    let list = fs::read_to_string(redist.join("fitgirl.md5")).ok()?;
    let base = redist.parent()?;
    let mut expected: Vec<(PathBuf, String)> = Vec::new();
    let mut missing = Vec::new();
    let mut listed = 0;
    for line in list.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with(';')) {
        let Some((hash, name)) = line.split_once(' ') else { continue };
        let name = name.trim_start_matches('*');
        listed += 1;
        let inside = name.strip_prefix("..\\").unwrap_or(name);
        match crate::disc::resolve(base, inside).filter(|p| p.is_file()) {
            Some(path) => expected.push((path, hash.to_lowercase())),
            None => missing.push(inside.to_string()),
        }
    }
    // md5sum on 8 at once: 129 GB took 2.5 minutes.
    let chunks: Vec<Vec<(PathBuf, String)>> = (0..8).map(|i| expected.iter().skip(i).step_by(8).cloned().collect()).collect();
    let differ = std::thread::scope(|s| {
        let jobs: Vec<_> = chunks
            .iter()
            .filter(|c| !c.is_empty())
            .map(|chunk| {
                s.spawn(move || {
                    let out = std::process::Command::new("md5sum").arg("-b").arg("--").args(chunk.iter().map(|(p, _)| p)).stderr(std::process::Stdio::null()).output();
                    let sums = out.map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
                    // "<hash> *<path>" for each file it could read (one starting with \ has
                    // its path escaped: such names don't come from Windows installs).
                    let got: std::collections::HashMap<&str, &str> = sums.lines().filter_map(|l| l.split_once(" *")).map(|(hash, path)| (path, hash)).collect();
                    chunk.iter().filter(|(p, want)| got.get(p.to_string_lossy().as_ref()) != Some(&want.as_str())).map(|(p, _)| p.strip_prefix(base).unwrap_or(p).display().to_string()).collect::<Vec<_>>()
                })
            })
            .collect();
        jobs.into_iter().flat_map(|j| j.join().unwrap_or_default()).collect()
    });
    Some(Checked { listed, differ, missing })
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    #[test]
    fn finds_repacks_and_their_unfinished_parts() {
        let dir = std::env::temp_dir().join(format!("gamefix-repack-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("setup.exe"), b"MZ").unwrap();
        fs::write(dir.join("fg-01.bin"), [b"ArC\x01".as_slice(), &[7; 4096]].concat()).unwrap();
        fs::write(dir.join("fg-optional-french-vo.bin"), [0u8; 4096]).unwrap();
        let repack = super::find(&dir.join("setup.exe")).unwrap();
        assert_eq!(repack.unfinished, [dir.join("fg-optional-french-vo.bin")]);
        assert!(super::optional(&repack.unfinished[0]) && !super::optional(Path::new("fg-02.bin")));
        fs::remove_file(dir.join("fg-01.bin")).unwrap();
        assert!(super::find(&dir.join("setup.exe")).is_none());

        // An install: one file right, one wrong, one missing.
        let game = dir.join("Game");
        fs::create_dir_all(game.join("_Redist")).unwrap();
        fs::create_dir_all(game.join("Bin")).unwrap();
        fs::write(game.join("Bin/Game.exe"), "hello\n").unwrap();
        fs::write(game.join("data.pak"), "x").unwrap();
        let list = "; comment\nb1946ac92492d2347c6235b4d2611184 *..\\bin\\game.exe\n00000000000000000000000000000000 *..\\data.pak\n11111111111111111111111111111111 *..\\gone.pak\n";
        fs::write(game.join("_Redist/fitgirl.md5"), list).unwrap();
        let checked = super::check(&game.join("Bin/Game.exe")).unwrap();
        assert_eq!((checked.listed, checked.differ, checked.missing), (3, vec!["data.pak".to_string()], vec!["gone.pak".to_string()]));
        fs::remove_dir_all(&dir).unwrap();
    }
}
