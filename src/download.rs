//! Whether a download has finished, from its files alone. Browsers and torrent apps give it away
//! in different ways: a name they add to the file, a file that's mostly holes (a torrent's
//! placeholder, full size but not on disk), one reserved in full and still blank in places, or
//! one being written right now. RoadCraft's repack (FitGirl's) read as downloaded at 30% and its
//! installer would have run.
//!
//! Not told apart: a download paused with its files reserved in full and nearly all there. The
//! blocks looked at can miss what's left; a repack's own file list catches that after the install.

use crate::scan::Game;
use std::fs;
use std::os::unix::fs::{FileExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How long after its last write a file may still be coming: a torrent app can reserve a file in
/// full and fill it in later, so all of it on disk doesn't say it's finished.
const STILL_WRITTEN: Duration = Duration::from_secs(120);

/// What browsers and torrent apps add to the name of a file they haven't finished. aria2's and
/// Firefox's sit beside the file, which already has its own name.
const ENDINGS: &[&str] = &[".part", ".crdownload", ".aria2", ".!qB", ".!ut"];

/// Blocks read from a file to see whether it's all there. Reading them is what takes the time: about
/// 5 ms each from a hard drive, nothing once cached.
const SAMPLES: u64 = 16;
const BLOCK: usize = 4096;

fn lower_name(file: &Path) -> String {
    file.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default()
}

fn named_unfinished(file: &Path) -> bool {
    let name = lower_name(file);
    let ending = ENDINGS.iter().any(|e| {
        // Transmission's and Firefox's end a whole name ("map.pak.part"): a game's own
        // "level1.part" isn't one.
        name.strip_suffix(&e.to_lowercase()).is_some_and(|stem| *e != ".part" || Path::new(stem).extension().is_some())
    });
    ending
        || ENDINGS.iter().any(|e| {
            let mut beside = file.as_os_str().to_os_string();
            beside.push(e);
            Path::new(&beside).exists()
        })
}

/// A file with holes: not all of it on disk. Only compressed data can't have any of its own (1%
/// is allowed for, so that nothing finished can stay unfinished forever): everything else may.
/// Steam and btrfs keep zeros as holes, in programs and games' assets alike (Particle Fleet's
/// sharedassets3.assets is 12% on disk, From_Dust.exe 99%), so those must be mostly empty.
fn holes(file: &Path, meta: &fs::Metadata) -> bool {
    let on_disk = meta.blocks() * 512;
    // Sizes first: telling what a .bin is reads the files beside it.
    meta.len() >= 1 << 20 && on_disk < meta.len() / 100 * 99 && (compressed(file) || (on_disk < meta.len() / 2 && packed(file)))
}

/// What's compressed: nothing in it should be blank for a whole block. Not disc images, which
/// have long empty stretches. A .bin can be anything (a disc's raw track, a game's data), so
/// only an installer's parts count: Inno Setup's (setup-1.bin, setup_game-1.bin), a repack's,
/// or any beside an installer (a repack named otherwise, none of its parts' starts here yet).
fn compressed(file: &Path) -> bool {
    let extension = file.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    match extension.as_str() {
        "bin" => {
            let name = lower_name(file);
            name.starts_with("setup") || name.starts_with("fg-") || crate::repack::beside_freearc(file) || beside_installer(file)
        }
        "rar" | "7z" | "zip" | "cab" => true,
        // Split archives: game.7z.001, game.r00.
        e => e.len() >= 2 && e.trim_start_matches('r').len() >= 2 && e.trim_start_matches('r').chars().all(|c| c.is_ascii_digit()),
    }
}

fn beside_installer(file: &Path) -> bool {
    let Some(dir) = file.parent() else { return false };
    fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()).any(|p| {
        p.extension().is_some_and(|e| e.eq_ignore_ascii_case("exe")) && matches!(crate::binary::identify(&p), Some(crate::binary::Binary::Pe(pe)) if pe.installer.is_some())
    })
}

fn is_blank(block: &[u8]) -> bool {
    block.iter().all(|b| *b == 0)
}

/// How many of the blocks looked at are blank, the first one apart: (first, others).
fn blank_blocks(file: &Path, len: u64) -> Option<(bool, u64)> {
    let f = fs::File::open(file).ok()?;
    let mut block = [0u8; BLOCK];
    let mut read = |at: u64| f.read_at(&mut block, at).ok().map(|n| is_blank(&block[..n]));
    let first = read(0)?;
    let others = (1..SAMPLES).filter(|i| read(len / SAMPLES * i).unwrap_or(false)).count() as u64;
    Some((first, others))
}

/// Reserved but not written: blank at its start (a program or an archive never is), or blank in
/// two places of something compressed (one could be a stored file full of zeros: of 295 real
/// downloads none was, but a whole file wrongly refused has no way out).
fn reserved(file: &Path, meta: &fs::Metadata) -> bool {
    let program = file.extension().is_some_and(|e| e.eq_ignore_ascii_case("exe"));
    if !compressed(file) && !program {
        return false;
    }
    if meta.len() == 0 {
        return true;
    }
    blank_blocks(file, meta.len()).is_some_and(|(first, others)| first || (compressed(file) && others >= 2))
}

fn written_lately(meta: &fs::Metadata) -> bool {
    meta.modified().ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age < STILL_WRITTEN)
}

/// The download's files: what's beside an installer in its own folder (`own_folder`: a repack's
/// parts), a split archive's parts, else the file alone.
pub fn parts(file: &Path, own_folder: Option<&Path>) -> Vec<PathBuf> {
    let name = lower_name(file);
    let split = name.rsplit_once(".part").map(|(stem, _)| format!("{stem}.part"));
    let Some(dir) = own_folder.or_else(|| split.as_ref().and_then(|_| file.parent())) else { return vec![file.to_path_buf()] };
    let same_set = |p: &Path| split.as_ref().is_none_or(|s| lower_name(p).starts_with(s));
    fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.is_file() && same_set(p)).collect()
}

/// The quick look, for asking often (a launcher's badge): names and sizes, read further only for
/// a big .bin with holes (what's beside it says what it is). A file finished in the last two
/// minutes looks unfinished here. A part left out on purpose keeps its holes: that's no sign for
/// an optional one.
pub fn looks_unfinished(file: &Path) -> bool {
    let Ok(meta) = fs::metadata(file) else { return false };
    written_lately(&meta) || named_unfinished(file) || (holes(file, &meta) && !crate::repack::optional(file))
}

/// A part a repack's installer shouldn't be offered: not all there.
pub fn left_out(file: &Path) -> bool {
    fs::metadata(file).is_ok_and(|meta| named_unfinished(file) || holes(file, &meta) || reserved(file, &meta))
}

/// Those of the download's files that aren't all there, or are being written right now. Written
/// lately isn't enough here: an archive unpacked a moment ago, its installer inside, is finished.
/// So those are looked at again after 3 s. Optional parts count only while being written.
pub fn unfinished(files: &[PathBuf]) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = files.iter().filter(|f| !crate::repack::optional(f) && left_out(f)).cloned().collect();
    let stamp = |f: &Path| fs::metadata(f).ok().map(|m| (m.len(), m.modified().ok()));
    let lately: Vec<_> = files.iter().filter(|f| !found.contains(f) && fs::metadata(f).is_ok_and(|m| written_lately(&m))).map(|f| (f, stamp(f))).collect();
    if !lately.is_empty() {
        std::thread::sleep(Duration::from_secs(3));
        found.extend(lately.into_iter().filter(|(f, before)| stamp(f) != *before).map(|(f, _)| f.clone()));
    }
    found
}

/// What can be a download's own file, at the top of its folder.
fn packed(file: &Path) -> bool {
    compressed(file) || file.extension().is_some_and(|e| ["exe", "iso", "img"].iter().any(|x| e.eq_ignore_ascii_case(x)))
}

/// Everywhere in a game's folder: only what can't be the game's own, a name an app adds or a big
/// file with next to nothing on disk (one a torrent app hasn't started). A game folder further
/// along than that, with no such names, can't be told: holes are normal in games' files.
fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        let Ok(meta) = fs::symlink_metadata(&path) else { continue };
        if meta.is_dir() {
            // A Wine prefix in the folder isn't the download.
            if depth < 8 && entry.file_name() != "drive_c" {
                walk(&path, depth + 1, out);
            }
        } else if meta.is_file() && !(depth == 0 && packed(&path)) && !crate::repack::optional(&path) {
            if named_unfinished(&path) || (meta.len() >= 16 << 20 && meta.blocks() * 512 < meta.len() / 100) {
                out.push(path);
            }
        }
    }
}

/// The files of a game's download that aren't finished. Of a file that stands alone, that file
/// and its parts: other downloads beside it say nothing about it.
pub fn of_game(game: &Game) -> Vec<PathBuf> {
    if let Some(file) = &game.alone {
        return unfinished(&parts(&game.root.join(file), None));
    }
    if crate::scan::shared_folder(&game.root) {
        return Vec::new();
    }
    let top: Vec<PathBuf> = fs::read_dir(&game.root).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.is_file() && packed(p)).collect();
    let mut found = unfinished(&top);
    walk(&game.root, 0, &mut found);
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

    #[test]
    fn tells_a_download_still_coming() {
        let dir = std::env::temp_dir().join(format!("gamefix-download-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let old = SystemTime::now() - Duration::from_secs(3600);
        let aged = |path: &Path| fs::File::options().write(true).open(path).unwrap().set_modified(old).unwrap();
        // Random, like anything compressed.
        let bytes = |n: usize| (0..n as u32).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8 | 1).collect::<Vec<u8>>();
        let done = |name: &str| {
            let path = dir.join(name);
            fs::write(&path, bytes(2 << 20)).unwrap();
            aged(&path);
            path
        };
        let placeholder = |name: &str| {
            let path = dir.join(name);
            fs::File::create(&path).unwrap().set_len(2 << 20).unwrap();
            aged(&path);
            path
        };
        let list = |paths: &[&Path]| paths.iter().map(|p| p.to_path_buf()).collect::<Vec<_>>();

        // A game's own file with holes is finished; the same in an archive isn't.
        let holed = |name: &str| {
            let path = dir.join(name);
            let mut data = bytes(4 << 20);
            data[1 << 20..3 << 20].fill(0);
            fs::File::create(&path).unwrap().set_len(4 << 20).unwrap();
            let f = fs::File::options().write(true).open(&path).unwrap();
            f.write_all_at(&data[..1 << 20], 0).unwrap();
            f.write_all_at(&data[3 << 20..], 3 << 20).unwrap();
            f.set_modified(old).unwrap();
            path
        };
        let own = holed("Game.exe");
        assert!(!left_out(&own));
        let data = holed("data.bin");
        assert!(!left_out(&data));
        fs::remove_file(&own).unwrap();
        fs::remove_file(&data).unwrap();

        let setup = done("setup.exe");
        assert!(unfinished(&list(&[&setup])).is_empty());
        assert!(!looks_unfinished(&setup));

        // A main part as a placeholder, a part's name, and a name beside the file.
        let part = placeholder("fg-01.bin");
        assert_eq!(unfinished(&list(&[&setup, &part])), [part.clone()]);
        assert!(looks_unfinished(&part));
        let named = done("fg-02.bin.!qB");
        assert_eq!(unfinished(&list(&[&named])), [named.clone()]);
        let firefox = done("game.zip");
        assert!(unfinished(&list(&[&firefox])).is_empty());
        fs::write(dir.join("game.zip.part"), "").unwrap();
        assert_eq!(unfinished(&list(&[&firefox])), [firefox.clone()]);
        // A game's own file named like that isn't one.
        assert!(!named_unfinished(&done("level1.part")));

        // Reserved in full, written in places: nothing says so but the blanks.
        let reserved = dir.join("game.rar");
        let mut half = bytes(2 << 20);
        half[1 << 20..].fill(0);
        fs::write(&reserved, &half).unwrap();
        aged(&reserved);
        assert_eq!(unfinished(&list(&[&reserved])), [reserved.clone()]);
        assert!(!looks_unfinished(&reserved));
        // The same in a disc image is just its empty stretches.
        let disc = dir.join("game.iso");
        fs::write(&disc, &half).unwrap();
        aged(&disc);
        assert!(unfinished(&list(&[&disc])).is_empty());

        // An optional part left out isn't waited for, until it's being written.
        let optional = placeholder("fg-optional-4K.bin");
        assert!(unfinished(&list(&[&optional])).is_empty());
        assert!(left_out(&optional));
        assert!(!looks_unfinished(&optional));

        // Written a moment ago but not any more: finished.
        let fresh = dir.join("fresh.rar");
        fs::write(&fresh, bytes(2 << 20)).unwrap();
        assert!(looks_unfinished(&fresh));
        assert!(unfinished(&list(&[&fresh])).is_empty());

        // Still being written.
        let growing = dir.join("growing.rar");
        fs::write(&growing, bytes(2 << 20)).unwrap();
        let writer = {
            let growing = growing.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_secs(1));
                fs::File::options().append(true).open(&growing).unwrap().set_modified(SystemTime::now() + Duration::from_secs(5)).unwrap();
            })
        };
        assert_eq!(unfinished(&list(&[&growing])), [growing.clone()]);
        writer.join().unwrap();

        // A split archive's other parts count; that folder's other files don't.
        let first = done("g.part1.rar");
        let second = placeholder("g.part2.rar");
        assert_eq!(unfinished(&parts(&first, None)), [second.clone()]);
        assert_eq!(parts(&setup, None), [setup.clone()]);
        fs::remove_dir_all(&dir).unwrap();
    }
}
