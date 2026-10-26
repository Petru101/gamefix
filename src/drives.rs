//! The computer's drives as Windows drive letters while an installer runs, so its Browse
//! lists them by name and the player picks one, as on Windows. Every drive mounted under
//! /run/media, /media or /mnt (udisks', Dolphin's and fstab's usual places) gets D:, E: ...
//! in the prefix, a link in its dosdevices. Proton makes Z: (all of /) and remakes it when
//! it's missing, but leaves one pointing nowhere, which Wine then doesn't list: so Z: points
//! nowhere while the installer runs, and back at / after (Steam's Proton runs its own helpers
//! through it). The drive letters stay: the installed game's registry and shortcuts name them.
//!
//! Names: Wine asks udisks, but only for drives whose ID has a GUID's shape (btrfs, ext4);
//! NTFS, FAT and exFAT drives ("7438478B38474B76") show unnamed. For those a `.windows-label`
//! file at the drive's top names it (Wine's own way); gamefix writes it for the install and
//! removes it after.

use std::fs;
use std::path::{Path, PathBuf};

/// A mount from /proc/self/mountinfo.
struct Mount {
    /// The folder of the filesystem mounted here ("/" for a whole drive, "/Downloads" for a
    /// folder of one mounted again elsewhere).
    root: PathBuf,
    point: PathBuf,
    device: String,
    writable: bool,
}

/// Mountinfo writes spaces and the like as `\040`.
fn unescape(s: &str) -> PathBuf {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b.get(i + 1..i + 4).and_then(|o| u8::from_str_radix(std::str::from_utf8(o).ok()?, 8).ok()) {
            Some(c) if b[i] == b'\\' => {
                out.push(c);
                i += 4;
            }
            _ => {
                out.push(b[i]);
                i += 1;
            }
        }
    }
    PathBuf::from(<std::ffi::OsString as std::os::unix::ffi::OsStringExt>::from_vec(out))
}

fn udev_unescape(s: &str) -> String {
    let mut out = Vec::new();
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b.get(i + 2..i + 4).and_then(|h| u8::from_str_radix(std::str::from_utf8(h).ok()?, 16).ok()) {
            Some(c) if b[i] == b'\\' && b[i + 1] == b'x' => {
                out.push(c);
                i += 4;
            }
            _ => {
                out.push(b[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn mounts(mountinfo: &str) -> Vec<Mount> {
    mountinfo
        .lines()
        .filter_map(|line| {
            let (before, after) = line.split_once(" - ")?;
            let f: Vec<&str> = before.split(' ').collect();
            let a: Vec<&str> = after.split(' ').collect();
            Some(Mount {
                root: unescape(f.get(3)?),
                point: unescape(f.get(4)?),
                device: f.get(2)?.to_string(),
                writable: f.get(5)?.split(',').next() == Some("rw") && a.get(2)?.split(',').next() != Some("ro"),
            })
            .filter(|_| a.get(1).is_some_and(|s| s.starts_with("/dev/")) && !["iso9660", "udf", "squashfs"].contains(a.first().unwrap_or(&"")))
        })
        .collect()
}

/// A drive the player could install to: where it's mounted, its name, whether Wine finds the
/// name by itself, and its block device ("/dev/sda2").
struct Drive {
    point: PathBuf,
    name: String,
    named_by_wine: bool,
    device: String,
}

/// Whole drives mounted where removable and extra drives go, writable, in path order.
fn drives(mountinfo: &str) -> Vec<Drive> {
    let sources: Vec<(String, String)> = mountinfo
        .lines()
        .filter_map(|l| {
            let (before, after) = l.split_once(" - ")?;
            Some((before.split(' ').nth(2)?.to_string(), after.split(' ').nth(1)?.to_string()))
        })
        .collect();
    // udev's names write a space as \x20.
    let named = |dir: &str, dev: &str| {
        fs::read_dir(dir).into_iter().flatten().flatten().find_map(|e| {
            let to = fs::canonicalize(e.path()).ok()?;
            (to == Path::new(dev)).then(|| udev_unescape(&e.file_name().to_string_lossy()))
        })
    };
    let mut found: Vec<Drive> = mounts(mountinfo)
        .into_iter()
        .filter(|m| m.root == Path::new("/") && m.writable && ["/run/media/", "/media/", "/mnt/"].iter().any(|p| m.point.starts_with(p) && m.point != Path::new(p)))
        .filter_map(|m| {
            let device = fs::canonicalize(&sources.iter().find(|(d, _)| *d == m.device)?.1).ok()?.display().to_string();
            let label = named("/dev/disk/by-label", &device);
            let uuid = named("/dev/disk/by-uuid", &device).unwrap_or_default();
            let guid = uuid.split('-').map(str::len).eq([8, 4, 4, 4, 12]) && uuid.chars().all(|c| c == '-' || c.is_ascii_hexdigit());
            let name = label.unwrap_or_else(|| m.point.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
            Some(Drive { point: m.point, name, named_by_wine: guid, device })
        })
        .collect();
    found.sort_by(|a, b| a.point.cmp(&b.point));
    // A drive mounted twice gets one letter.
    found.dedup_by(|a, b| a.device == b.device);
    found
}

/// The drives given to a prefix for an install, undone by `finish`.
pub struct Lettered {
    dosdevices: PathBuf,
    /// The drives' mount points with their letters ("d:").
    letters: Vec<(PathBuf, String)>,
    /// The `.windows-label` files written for it.
    labels: Vec<PathBuf>,
}

/// Where Z: points while the installer runs: nowhere, so Wine leaves it out.
const NOWHERE: &str = "/nonexistent/gamefix-hides-z";

/// Gives the prefix (a new one: its dosdevices is made here, before Proton fills it) a letter
/// for each drive, names the drives Wine can't, and hides Z:.
pub fn letter(prefix: &Path) -> Lettered {
    let dosdevices = prefix.join("dosdevices");
    let _ = fs::create_dir_all(&dosdevices);
    let mountinfo = fs::read_to_string("/proc/self/mountinfo").unwrap_or_default();
    let mut lettered = Lettered { dosdevices: dosdevices.clone(), letters: Vec::new(), labels: Vec::new() };
    let free = (b'd'..b'z').map(|c| format!("{}:", c as char)).filter(|l| fs::symlink_metadata(dosdevices.join(l)).is_err());
    for (drive, letter) in drives(&mountinfo).into_iter().zip(free) {
        if std::os::unix::fs::symlink(&drive.point, dosdevices.join(&letter)).is_err() {
            continue;
        }
        let label = drive.point.join(".windows-label");
        if !drive.named_by_wine && !drive.name.is_empty() && fs::symlink_metadata(&label).is_err() && fs::write(&label, &drive.name).is_ok() {
            lettered.labels.push(label);
        }
        lettered.letters.push((drive.point, letter));
    }
    let z = dosdevices.join("z:");
    let _ = fs::remove_file(&z);
    let _ = std::os::unix::fs::symlink(NOWHERE, &z);
    lettered
}

impl Lettered {
    /// A file's path through the drive it's on, so Wine names it by that drive's letter:
    /// ~/Downloads mounted from the Storage drive's Downloads folder becomes
    /// /run/media/<user>/Storage/Downloads, E:\Downloads in the prefix. None when it's on no
    /// lettered drive.
    pub fn on_drive(&self, path: &Path) -> Option<PathBuf> {
        let path = fs::canonicalize(path).ok()?;
        let all = mounts(&fs::read_to_string("/proc/self/mountinfo").ok()?);
        let mount = all.iter().filter(|m| path.starts_with(&m.point)).max_by_key(|m| m.point.components().count())?;
        let inside = mount.root.join(path.strip_prefix(&mount.point).ok()?);
        let (point, _) = self.letters.iter().find(|(point, _)| all.iter().any(|m| m.point == *point && m.device == mount.device && m.root == Path::new("/")))?;
        Some(point.join(inside.strip_prefix("/").unwrap_or(&inside)))
    }

    /// The tops of the drives given letters.
    pub fn roots(&self) -> Vec<PathBuf> {
        self.letters.iter().map(|(point, _)| point.clone()).collect()
    }

    /// Z: back at /, and the drive names written for the install removed.
    pub fn finish(self) {
        let z = self.dosdevices.join("z:");
        if fs::read_link(&z).is_ok_and(|t| t == Path::new(NOWHERE)) {
            let _ = fs::remove_file(&z);
            let _ = std::os::unix::fs::symlink("/", &z);
        }
        for label in self.labels {
            let _ = fs::remove_file(label);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    const MOUNTINFO: &str = "\
2734 2733 0:57 / /run/media/player/Games rw,noatime master:1328 - btrfs /dev/nvme0n1p1 rw,seclabel,compress=zstd:1
2737 2733 8:2 / /run/media/player/My\\040Storage rw,noatime master:1331 - fuseblk /dev/sda2 rw,user_id=0
3391 3390 8:2 /Downloads /home/player/Downloads rw,noatime shared:1360 master:247 - fuseblk /dev/sda2 rw,user_id=0
2740 2733 8:3 / /run/media/player/Windows ro,noatime master:1332 - fuseblk /dev/sdc3 ro,user_id=0
2741 2733 7:0 / /mnt/disc ro - iso9660 /dev/loop0 ro
29 1 0:26 / /tmp rw,nosuid - tmpfs tmpfs rw";

    #[test]
    fn reads_mounts() {
        let m = super::mounts(MOUNTINFO);
        assert_eq!(m.len(), 4);
        assert_eq!(m[1].point, Path::new("/run/media/player/My Storage"));
        assert_eq!((m[2].root.as_path(), m[2].device.as_str(), m[2].writable), (Path::new("/Downloads"), "8:2", true));
        assert!(!m[3].writable);
        assert_eq!(super::udev_unescape("System\\x20Reserved"), "System Reserved");
    }
}
