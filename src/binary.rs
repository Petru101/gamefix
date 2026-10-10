//! Reads just enough of ELF and PE files to tell what they are and what they need.

use std::collections::HashSet;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

pub const EM_386: u16 = 3;
pub const EM_X86_64: u16 = 62;

pub enum Binary {
    Elf(Elf),
    Pe(Pe),
    Script,
    /// A LÖVE game: a zip of Lua code that the `love` program runs.
    Love,
    /// A Java program, which `java -jar` runs.
    Jar,
    /// A zip, tar or 7z archive: the game may still be packed inside.
    Archive,
    Python(Python),
    /// A Flash movie or game (.swf), which Flash Player ran; Ruffle does now.
    Flash,
    /// A distribution package (.deb or .rpm): meant to be installed into /usr.
    Package(&'static str),
    /// A Linux program whose bytes above 0x7F were each replaced with U+FFFD's UTF-8 (EF BF BD):
    /// it went through a text conversion on the developer's side and can't run.
    Damaged,
}

pub struct Python {
    /// Bytecode or source for Python 2, which current systems no longer have.
    pub v2: bool,
    pub pygame: bool,
}

pub struct Elf {
    pub bits: u8,
    pub machine: u16,
    /// Has an interpreter (ld.so), so it is a program rather than a library.
    pub program: bool,
    pub needed: Vec<String>,
    /// Where it tells ld.so to look for those first (DT_RPATH, or DT_RUNPATH which replaces
    /// it), as written: usually relative to its own folder through $ORIGIN.
    pub rpath: Vec<String>,
    pub runpath: Vec<String>,
    /// Compressed by UPX: what it needs is only known once it unpacks itself at start.
    pub packed: bool,
    /// A PyInstaller one-file bundle: Python and its libraries are packed at the end and
    /// unpacked to /tmp at start, so they can't be read here.
    pub pyinstaller: bool,
    /// Asks for an executable stack: glibc 2.41+ refuses to dlopen such libraries.
    /// A missing PT_GNU_STACK counts, as x86 defaults to executable.
    pub exec_stack: bool,
}

pub struct Pe {
    pub bits: u8,
    pub installer: Option<&'static str>,
    /// A .NET program (it has a CLR header): Mono runs these on Linux too, so next to a Linux
    /// program it's usually part of the Linux build, not a Windows one (Terraria).
    pub dotnet: bool,
}

pub fn identify(path: &Path) -> Option<Binary> {
    let mut f = File::open(path).ok()?;
    let mut head = [0u8; 64];
    let n = f.read(&mut head).ok()?;
    let head = &head[..n];
    let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    if head.starts_with(b"\x7fELF") && n == 64 {
        match elf(&mut f, head) {
            Some(elf) => Some(Binary::Elf(elf)),
            None if head.windows(3).any(|w| w == b"\xef\xbf\xbd") => Some(Binary::Damaged),
            None => None,
        }
    } else if head.starts_with(b"MZ") && n == 64 {
        pe(&mut f, head).map(Binary::Pe)
    } else if ext == "love" && (head.starts_with(b"PK\x03\x04") || (head.starts_with(b"#!") && head.windows(4).any(|w| w == b"PK\x03\x04"))) {
        // Some start with "#!/usr/bin/env love" so they run when double-clicked (NEON PHASE).
        Some(Binary::Love)
    } else if head.starts_with(b"#!") && ext != "py" {
        Some(Binary::Script)
    } else if ext == "jar" && head.starts_with(b"PK\x03\x04") {
        Some(Binary::Jar)
    } else if ext == "swf" && [&b"FWS"[..], b"CWS", b"ZWS"].iter().any(|m| head.starts_with(m)) {
        Some(Binary::Flash)
    } else if ext == "apk" && head.starts_with(b"PK\x03\x04") {
        Some(Binary::Package("apk"))
    } else if ext == "deb" && head.starts_with(b"!<arch>\ndebian") {
        Some(Binary::Package("deb"))
    } else if ext == "rpm" && head.starts_with(b"\xed\xab\xee\xdb") {
        Some(Binary::Package("rpm"))
    } else if ext == "snap" && head.starts_with(b"hsqs") {
        // A Snap package is a squashfs image (Fantasim).
        Some(Binary::Package("snap"))
    } else if ["zip", "gz", "tgz", "xz", "bz2", "7z", "rar"].contains(&ext.as_str())
        && [&b"PK\x03\x04"[..], b"\x1f\x8b", b"BZh", b"\xfd7zXZ\0", b"7z\xbc\xaf\x27\x1c", b"Rar!"].iter().any(|m| head.starts_with(m))
    {
        Some(Binary::Archive)
    } else if ext == "pyc" && n >= 4 && head[2..4] == *b"\r\n" {
        // The first two bytes number the bytecode format: Python 2 used 20121-62211 (2.7),
        // Python 3 started again at 3000.
        Some(Binary::Python(Python { v2: u16le(head, 0) >= 20000, pygame: contains(path, b"pygame") }))
    } else if ext == "py" {
        let mut text = String::new();
        File::open(path).ok()?.take(1 << 20).read_to_string(&mut text).ok()?;
        Some(Binary::Python(Python { v2: python2_source(&text), pygame: text.contains("import pygame") || text.contains("from pygame") }))
    } else {
        None
    }
}

/// Statements only Python 2 accepts: `print "..."` and `except Error, e:`.
fn python2_source(text: &str) -> bool {
    text.lines().map(str::trim_start).any(|l| {
        l.strip_prefix("print ").is_some_and(|r| r.starts_with(['"', '\''])) || (l.starts_with("except ") && l.contains(", ") && l.trim_end().ends_with(':') && !l.contains('('))
    })
}

/// A library's dynamic symbols: what it offers to others and what it expects from them.
pub struct Symbols {
    /// Global (not weak) functions and variables it defines.
    pub defined: HashSet<String>,
    pub undefined: HashSet<String>,
}

/// Reads `.dynsym` through the section headers (shipped libraries keep them even when stripped).
pub fn symbols(path: &Path) -> Option<Symbols> {
    let mut f = File::open(path).ok()?;
    let h = read_at(&mut f, 0, 64)?;
    if !h.starts_with(b"\x7fELF") || h[5] != 1 {
        return None;
    }
    let wide = h[4] == 2;
    let (shoff, shentsize, shnum) = if wide {
        (u64le(&h, 40), u16le(&h, 58) as usize, u16le(&h, 60) as usize)
    } else {
        (u32le(&h, 32) as u64, u16le(&h, 46) as usize, u16le(&h, 48) as usize)
    };
    if shoff == 0 || shnum == 0 || shentsize < if wide { 64 } else { 40 } {
        return None;
    }
    let sh = read_at(&mut f, shoff, shentsize * shnum)?;
    // (type, offset, size, link)
    let section = |i: usize| {
        let s = &sh[i * shentsize..];
        if wide {
            (u32le(s, 4), u64le(s, 24), u64le(s, 32), u32le(s, 40) as usize)
        } else {
            (u32le(s, 4), u32le(s, 16) as u64, u32le(s, 20) as u64, u32le(s, 24) as usize)
        }
    };
    let (_, symoff, symsize, link) = (0..shnum).map(section).find(|s| s.0 == 11)?;
    if link >= shnum {
        return None;
    }
    let (_, stroff, strsize, _) = section(link);
    let syms = read_at(&mut f, symoff, symsize.min(256 << 20) as usize)?;
    let strs = read_at(&mut f, stroff, strsize.min(256 << 20) as usize)?;

    let mut out = Symbols { defined: HashSet::new(), undefined: HashSet::new() };
    for s in syms.chunks_exact(if wide { 24 } else { 16 }).skip(1) {
        let (name, info, shndx) = if wide { (u32le(s, 0), s[4], u16le(s, 6)) } else { (u32le(s, 0), s[12], u16le(s, 14)) };
        let (bind, kind) = (info >> 4, info & 0xf);
        let start = name as usize;
        if start == 0 || start >= strs.len() {
            continue;
        }
        let end = strs[start..].iter().position(|&c| c == 0).map_or(strs.len(), |e| start + e);
        let name = String::from_utf8_lossy(&strs[start..end]).into_owned();
        if shndx == 0 {
            if bind == 1 || bind == 2 {
                out.undefined.insert(name);
            }
        } else if bind == 1 && matches!(kind, 1 | 2 | 10) {
            // Weak definitions (C++ templates and inline functions) are duplicated on purpose.
            out.defined.insert(name);
        }
    }
    Some(out)
}

/// Whether the file holds these bytes anywhere.
pub fn contains(path: &Path, needle: &[u8]) -> bool {
    contains_groups(path, &[&[needle]])
}

/// Whether the first `limit` bytes of a file hold `needle` (a .NET single-file program's
/// launcher, at the start, names libhostfxr.so; the bundle follows).
pub fn head_contains(path: &Path, needle: &[u8], limit: u64) -> bool {
    let Ok(f) = File::open(path) else { return false };
    let mut head = Vec::new();
    f.take(limit).read_to_end(&mut head).is_ok() && head.windows(needle.len()).any(|w| w == needle)
}

/// The `len` bytes after the first `needle` in a file, read in chunks (large files: .NET
/// single-file bundles are ~90 MB).
pub fn find_after(path: &Path, needle: &[u8], len: usize) -> Option<Vec<u8>> {
    let mut f = File::open(path).ok()?;
    let mut buf = vec![0u8; 1 << 20];
    let mut window: Vec<u8> = Vec::new();
    loop {
        let n = f.read(&mut buf).ok()?;
        window.extend_from_slice(&buf[..n]);
        if let Some(at) = window.windows(needle.len()).position(|w| w == needle) {
            let start = at + needle.len();
            while window.len() < start + len {
                let n = f.read(&mut buf).ok()?;
                if n == 0 {
                    break;
                }
                window.extend_from_slice(&buf[..n]);
            }
            return Some(window[start..(start + len).min(window.len())].to_vec());
        }
        if n == 0 {
            return None;
        }
        window.drain(..window.len().saturating_sub(needle.len() + len));
    }
}

/// Whether a zip ends with its central directory (the end record, "PK\x05\x06", in the last
/// 64 KB + 22 bytes). Without it the upload was cut off or damaged, and unpackers refuse it
/// (Flyer Kid: "End-of-central-directory signature not found").
pub fn zip_complete(path: &Path) -> bool {
    let Ok(mut f) = File::open(path) else { return true };
    let Ok(size) = f.seek(SeekFrom::End(0)) else { return true };
    let start = size.saturating_sub(65_557);
    let Some(tail) = read_at(&mut f, start, (size - start) as usize) else { return true };
    tail.windows(4).any(|w| w == b"PK\x05\x06")
}

/// Whether a script runs `path` (a path ending in /<program name>) on some line, rather than
/// only preparing it: Alcarys Complex's install script `chmod`s Alcarys_Complex/runner and
/// writes a desktop shortcut, and doesn't start the game.
pub fn runs_path(script: &Path, name: &str) -> bool {
    let Ok(text) = std::fs::read(script) else { return false };
    let needle = format!("/{name}");
    String::from_utf8_lossy(&text).lines().any(|line| {
        let l = line.trim();
        if l.starts_with('#') || !l.contains(&needle) {
            return false;
        }
        // The command on the line, after any VAR=value settings and `exec`.
        let first = l.split_whitespace().find(|w| !(w.contains('=') && !w.starts_with(['"', '\'', '$', '/', '.'])) && *w != "exec").unwrap_or("");
        !["chmod", "chown", "cp", "mv", "ln", "rm", "sed", "echo", "printf", "[", "[[", "test", "if", "cat", "mkdir", "ls", "touch", "install", "file", "readlink", "basename", "dirname"].contains(&first)
    })
}

/// Whether a jar's own classes use java.applet, which Java 26 removed (Sheep Farm Simulator:
/// NoClassDefFoundError java/applet/AppletStub from its obfuscated game code). Libraries it
/// bundles don't count: libGDX and Slick jars carry LWJGL 2's and Slick's unused applet
/// containers and run on Java 27.
pub fn jar_uses_applet(jar: &Path) -> bool {
    const LIBRARIES: &[&str] = &["org/lwjgl/", "org/newdawn/", "com/badlogic/", "net/java/", "com/jcraft/", "javazoom/", "paulscode/", "de/matthiasmann/", "org/jogamp/", "com/jogamp/", "jogamp/", "javax/", "kotlin/", "org/apache/", "com/google/", "org/json/", "com/esotericsoftware/", "org/ninjacave/"];
    let Ok(list) = std::process::Command::new("unzip").arg("-Z1").arg(jar).output() else { return false };
    let names: Vec<String> = String::from_utf8_lossy(&list.stdout).lines().filter(|n| n.ends_with(".class") && !LIBRARIES.iter().any(|l| n.starts_with(l))).map(String::from).collect();
    names.chunks(500).any(|chunk| {
        let out = std::process::Command::new("unzip").arg("-p").arg(jar).args(chunk).output().map(|o| o.stdout).unwrap_or_default();
        out.windows(12).any(|w| w == b"java/applet/")
    })
}

/// Where a type-2 AppImage's squashfs image starts: right after its ELF runtime, whose section
/// headers come last. None for anything else (checked against the squashfs magic).
pub fn appimage_offset(path: &Path) -> Option<u64> {
    let mut f = File::open(path).ok()?;
    let h = read_at(&mut f, 0, 64)?;
    if !h.starts_with(b"\x7fELF") || &h[8..11] != b"AI\x02" {
        return None;
    }
    let end = if h[4] == 2 { u64le(&h, 40) + u16le(&h, 58) as u64 * u16le(&h, 60) as u64 } else { u32le(&h, 32) as u64 + u16le(&h, 46) as u64 * u16le(&h, 48) as u64 };
    (read_at(&mut f, end, 4)? == b"hsqs").then_some(end)
}

/// Whether a script sets LD_LIBRARY_PATH; macOS's DYLD_LIBRARY_PATH, which MonoKickstart's
/// Kick script sets, doesn't count.
pub fn sets_library_path(path: &Path) -> bool {
    let Ok(text) = std::fs::read(path) else { return false };
    let needle = b"LD_LIBRARY_PATH";
    text.windows(needle.len()).enumerate().any(|(i, w)| w == needle && (i == 0 || text[i - 1] != b'Y'))
}

/// Whether a script starts `program` with one of the game's files after it (Quick Space
/// Dungeon's run_game.sh: `./love quick_space_dungeon.love`, a bundled LÖVE that shows its
/// no-game screen when started alone). Files are looked for next to the script.
pub fn passes_file(script: &Path, program: &str) -> bool {
    let Ok(text) = std::fs::read(script) else { return false };
    let text = String::from_utf8_lossy(&text);
    let dir = script.parent().unwrap_or(Path::new(""));
    text.match_indices(program).any(|(i, _)| {
        let rest = &text[i + program.len()..];
        // The name itself, not part of a longer word ("love" in "lovely").
        if !rest.starts_with([' ', '\t']) {
            return false;
        }
        let arg = rest.split_whitespace().next().unwrap_or("").trim_matches(['"', '\'']);
        let arg = if arg.starts_with('$') { arg.rsplit('/').next().unwrap_or("") } else { arg };
        !arg.is_empty() && !arg.starts_with(['-', '$']) && dir.join(arg).is_file()
    })
}

/// Whether a script that sets LD_LIBRARY_PATH keeps what was there ("$LD_LIBRARY_PATH"),
/// rather than replacing it (labb.sh: `LD_LIBRARY_PATH=. ./labb`).
pub fn keeps_library_path(path: &Path) -> bool {
    contains_groups(path, &[&[b"$LD_LIBRARY_PATH", b"${LD_LIBRARY_PATH"]])
}

/// Whether the file holds, for every group, at least one of its byte strings; read once.
pub fn contains_groups(path: &Path, groups: &[&[&[u8]]]) -> bool {
    find_groups(path, groups).iter().all(|&f| f)
}

/// For each group, whether the file holds at least one of its byte strings; read once.
pub fn find_groups(path: &Path, groups: &[&[&[u8]]]) -> Vec<bool> {
    let mut found = vec![false; groups.len()];
    let Ok(mut f) = File::open(path) else { return found };
    let longest = groups.iter().flat_map(|g| g.iter()).map(|n| n.len()).max().unwrap_or(1);
    let mut buf = vec![0u8; 1 << 20];
    let mut carry = 0;
    loop {
        let Ok(n) = f.read(&mut buf[carry..]) else { return found };
        if n == 0 {
            return found;
        }
        let end = carry + n;
        for (g, group) in groups.iter().enumerate() {
            found[g] = found[g] || group.iter().any(|needle| buf[..end].windows(needle.len()).any(|w| w == *needle));
        }
        if found.iter().all(|&f| f) {
            return found;
        }
        carry = (longest - 1).min(end);
        buf.copy_within(end - carry..end, 0);
    }
}

fn read_at(f: &mut File, off: u64, len: usize) -> Option<Vec<u8>> {
    let mut buf = vec![0; len];
    f.seek(SeekFrom::Start(off)).ok()?;
    f.read_exact(&mut buf).ok()?;
    Some(buf)
}

fn u16le(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn u32le(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

fn u64le(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

fn elf(f: &mut File, h: &[u8]) -> Option<Elf> {
    if h[5] != 1 {
        return None;
    }
    let bits = match h[4] {
        1 => 32,
        2 => 64,
        _ => return None,
    };
    let wide = bits == 64;
    let machine = u16le(h, 18);
    let kind = u16le(h, 16);
    let (phoff, phentsize, phnum) = if wide {
        (u64le(h, 32), u16le(h, 54) as usize, u16le(h, 56) as usize)
    } else {
        (u32le(h, 28) as u64, u16le(h, 42) as usize, u16le(h, 44) as usize)
    };
    if phentsize < if wide { 56 } else { 32 } || phnum > 4096 {
        return None;
    }
    let ph = read_at(f, phoff, phentsize * phnum)?;

    let mut loads = Vec::new();
    let mut dynamic = None;
    let mut program = false;
    let mut exec_stack = true;
    for p in ph.chunks_exact(phentsize) {
        let (kind, flags, offset, vaddr, filesz) = if wide {
            (u32le(p, 0), u32le(p, 4), u64le(p, 8), u64le(p, 16), u64le(p, 32))
        } else {
            (u32le(p, 0), u32le(p, 24), u32le(p, 4) as u64, u32le(p, 8) as u64, u32le(p, 16) as u64)
        };
        match kind {
            1 => loads.push((vaddr, offset, filesz)),
            2 => dynamic = Some((offset, filesz)),
            3 => program = true,
            0x6474e551 => exec_stack = flags & 1 != 0,
            _ => {}
        }
    }

    let (mut needed, mut rpath, mut runpath) = (Vec::new(), Vec::new(), Vec::new());
    if let Some((offset, size)) = dynamic {
        let entry = if wide { 16 } else { 8 };
        let dynsec = read_at(f, offset, size.min(1 << 20) as usize)?;
        let (mut strtab, mut strsz, mut names, mut rpaths, mut runpaths) = (0, 0, Vec::new(), Vec::new(), Vec::new());
        for d in dynsec.chunks_exact(entry) {
            let (tag, val) = if wide { (u64le(d, 0), u64le(d, 8)) } else { (u32le(d, 0) as u64, u32le(d, 4) as u64) };
            match tag {
                0 => break,
                1 => names.push(val),
                15 => rpaths.push(val),
                29 => runpaths.push(val),
                // DT_FLAGS_1 with DF_1_PIE: a static-pie program has it and no interpreter
                // (AppImages with the new static runtime, OpenRA).
                0x6fff_fffb if val & 0x0800_0000 != 0 => program = true,
                5 => strtab = val,
                10 => strsz = val,
                _ => {}
            }
        }
        // DT_STRTAB is an address; find the file offset through the segment that holds it.
        let file_off = loads.iter().find(|(va, _, sz)| strtab >= *va && strtab < va + sz).map(|(va, off, _)| strtab - va + off);
        if let Some(off) = file_off {
            // Programs exporting many symbols (NW.js) have string tables of several MB.
            let strings = read_at(f, off, strsz.min(64 << 20) as usize)?;
            let string = |n: u64| {
                let start = n as usize;
                (start < strings.len()).then(|| {
                    let end = strings[start..].iter().position(|&c| c == 0).map_or(strings.len(), |e| start + e);
                    String::from_utf8_lossy(&strings[start..end]).into_owned()
                })
            };
            needed = names.into_iter().filter_map(string).collect();
            let split = |list: Vec<u64>| list.into_iter().filter_map(string).flat_map(|s| s.split(':').filter(|d| !d.is_empty()).map(String::from).collect::<Vec<_>>()).collect();
            rpath = split(rpaths);
            runpath = split(runpaths);
        }
    }

    // A static program has no interpreter and no dynamic section (UPX-packed ones look like that;
    // a packed PIE is ET_DYN, but a library always has a dynamic section: ExpelledRL).
    let program = program || ((kind == 2 || kind == 3) && dynamic.is_none());
    let mut start = Vec::new();
    f.seek(SeekFrom::Start(0)).ok()?;
    f.take(4096).read_to_end(&mut start).ok()?;
    let packed = start.windows(4).any(|w| w == b"UPX!");
    // PyInstaller's archive ends with its cookie ("MEI\x0c\x0b\x0a\x0b\x0e" and a few fields).
    let mut end = Vec::new();
    let size = f.seek(SeekFrom::End(0)).ok()?;
    f.seek(SeekFrom::Start(size.saturating_sub(4096))).ok()?;
    f.read_to_end(&mut end).ok()?;
    let pyinstaller = end.windows(8).any(|w| w == b"MEI\x0c\x0b\x0a\x0b\x0e");
    Some(Elf { bits, machine, program, needed, rpath, runpath, packed, pyinstaller, exec_stack })
}

/// The DLLs a Windows program or library needs to start (delay-loaded ones, loaded the first
/// time they're used, aren't counted).
pub struct Imports {
    /// A .NET program: its own references (XNA) aren't imports.
    pub dotnet: bool,
    pub needed: Vec<String>,
}

/// Reads the import tables through the section headers. None when they don't hold plausible
/// DLL names: a packed program (Cookard) keeps the real ones compressed.
pub fn imports(path: &Path) -> Option<Imports> {
    let data = std::fs::read(path).ok()?;
    let lfanew = u32le(data.get(..0x40)?, 0x3c) as usize;
    if data.get(lfanew..lfanew + 4)? != b"PE\0\0" {
        return None;
    }
    let sections = u16le(&data, lfanew + 6) as usize;
    let opt = lfanew + 24;
    let opt_size = u16le(&data, lfanew + 20) as usize;
    let dirs = match u16le(data.get(opt..opt + 2)?, 0) {
        0x10b => opt + 96,
        0x20b => opt + 112,
        _ => return None,
    };
    let dir_count = u32le(&data, dirs - 4) as usize;
    let dir = |i: usize| if i < dir_count && dirs + i * 8 + 8 <= data.len() { u32le(&data, dirs + i * 8) as u64 } else { 0 };
    // (virtual address, size, file offset) of each section.
    let table: Vec<(u64, u64, u64)> = (0..sections)
        .filter_map(|i| {
            let s = data.get(opt + opt_size + i * 40..opt + opt_size + i * 40 + 40)?;
            Some((u32le(s, 12) as u64, u32le(s, 8).max(u32le(s, 16)) as u64, u32le(s, 20) as u64))
        })
        .collect();
    let at = |rva: u64| table.iter().find(|(va, size, _)| *va <= rva && rva < va + size).map(|(va, _, raw)| (raw + rva - va) as usize).filter(|&o| o < data.len());
    let name_at = |rva: u64| -> Option<String> {
        let start = at(rva)?;
        let end = start + data[start..].iter().take(256).position(|&b| b == 0)?;
        let name = String::from_utf8(data[start..end].to_vec()).ok()?.to_lowercase();
        let plausible = name.chars().all(|c| c.is_ascii_alphanumeric() || "_.+- ".contains(c)) && [".dll", ".drv", ".exe", ".ocx", ".sys", ".cpl"].iter().any(|e| name.ends_with(e));
        plausible.then_some(name)
    };
    // Import descriptors (directory entry 1): 20 bytes each, the DLL's name at 12, all zero at the end.
    let mut needed = Vec::new();
    if dir(1) != 0 {
        for i in 0..4096u64 {
            let d = at(dir(1) + i * 20)?;
            let desc = data.get(d..d + 20)?;
            if desc.iter().all(|&b| b == 0) {
                break;
            }
            needed.push(name_at(u32le(desc, 12) as u64)?);
        }
    }
    Some(Imports { dotnet: dir(14) != 0, needed })
}

/// The .NET runtime a .NET program was built for, from its metadata ("v2.0.50727", "v4.0.30319").
pub fn dotnet_runtime(path: &Path) -> Option<String> {
    let data = std::fs::read(path).ok()?;
    let at = data.windows(4).position(|w| w == b"BSJB")?;
    let len = u32le(data.get(at..at + 16)?, 12) as usize;
    let version = data.get(at + 16..at + 16 + len.min(32))?;
    Some(String::from_utf8_lossy(version).trim_end_matches('\0').to_string())
}

/// The maker a Windows file names in its version information ("CompanyName"), if any.
pub fn company(path: &Path) -> Option<String> {
    let data = std::fs::read(path).ok()?;
    let key: Vec<u8> = "CompanyName\0".encode_utf16().flat_map(|c| c.to_le_bytes()).collect();
    let at = data.windows(key.len()).position(|w| w == key)? + key.len();
    // The value starts at the next 4-byte boundary and ends at a UTF-16 NUL.
    let start = (at + 3) & !3;
    let units: Vec<u16> = data.get(start..)?.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|&u| u != 0).take(128).collect();
    let name = String::from_utf16_lossy(&units).trim().to_string();
    (!name.is_empty()).then_some(name)
}

fn pe(f: &mut File, h: &[u8]) -> Option<Pe> {
    let lfanew = u32le(h, 0x3c) as u64;
    let nt = read_at(f, lfanew, 24 + 240)?;
    if &nt[..4] != b"PE\0\0" {
        return None;
    }
    // The optional header follows the 24-byte signature and COFF header; its data directories
    // start at 96 (PE32) or 112 (PE32+), and entry 14 is the CLR runtime header.
    let (bits, dirs) = match u16le(&nt, 24) {
        0x10b => (32, 24 + 96),
        0x20b => (64, 24 + 112),
        _ => return None,
    };
    let count = u32le(&nt, dirs - 4) as usize;
    let dotnet = count > 14 && u32le(&nt, dirs + 14 * 8) != 0;
    // Installer stubs name themselves in their resources, near the start of the file.
    let mut stub = Vec::new();
    f.seek(SeekFrom::Start(0)).ok()?;
    f.take(8 << 20).read_to_end(&mut stub).ok()?;
    let installer = [("Inno Setup", "Inno Setup"), ("Nullsoft", "NSIS"), ("InstallShield", "InstallShield")]
        .into_iter()
        .find(|(marker, _)| stub.windows(marker.len()).any(|w| w == marker.as_bytes()))
        .map(|(_, name)| name);
    Some(Pe { bits, installer, dotnet })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal 32-bit Windows program: one section holding the import table, naming two DLLs.
    #[test]
    fn windows_imports() {
        let mut f = vec![0u8; 0x600];
        f[..2].copy_from_slice(b"MZ");
        f[0x3c..0x40].copy_from_slice(&0x40u32.to_le_bytes());
        f[0x40..0x44].copy_from_slice(b"PE\0\0");
        let (coff, opt) = (0x44, 0x58);
        f[coff..coff + 2].copy_from_slice(&0x14cu16.to_le_bytes());
        f[coff + 2..coff + 4].copy_from_slice(&1u16.to_le_bytes());
        f[coff + 16..coff + 18].copy_from_slice(&224u16.to_le_bytes());
        f[opt..opt + 2].copy_from_slice(&0x10bu16.to_le_bytes());
        f[opt + 92..opt + 96].copy_from_slice(&16u32.to_le_bytes());
        f[opt + 104..opt + 108].copy_from_slice(&0x1000u32.to_le_bytes());
        // The section: 0x200 bytes at file offset 0x400, mapped at 0x1000.
        let s = opt + 224;
        for (at, v) in [(8, 0x200u32), (12, 0x1000), (16, 0x200), (20, 0x400)] {
            f[s + at..s + at + 4].copy_from_slice(&v.to_le_bytes());
        }
        for (i, (rva, name)) in [(0x1100u32, &b"MFC42.DLL\0"[..]), (0x1110, b"kernel32.dll\0")].into_iter().enumerate() {
            f[0x400 + i * 20 + 12..0x400 + i * 20 + 16].copy_from_slice(&rva.to_le_bytes());
            let at = 0x400 + rva as usize - 0x1000;
            f[at..at + name.len()].copy_from_slice(name);
        }
        let path = std::env::temp_dir().join(format!("gamefix-pe-{}", std::process::id()));
        std::fs::write(&path, &f).unwrap();
        let imports = imports(&path).unwrap();
        assert_eq!(imports.needed, ["mfc42.dll", "kernel32.dll"]);
        assert!(!imports.dotnet);
        // A name that isn't one (a packed program's table): unreadable, not a list of nonsense.
        f[0x500..0x503].copy_from_slice(b"\xc3\xa9h");
        std::fs::write(&path, &f).unwrap();
        assert!(super::imports(&path).is_none());
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn library_path_scripts() {
        let dir = std::env::temp_dir().join(format!("gamefix-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (kick, launch) = (dir.join("Kick"), dir.join("launch"));
        std::fs::write(&kick, "#!/bin/bash\nexport DYLD_LIBRARY_PATH=$DYLD_LIBRARY_PATH:./\n./game.bin.x86_64 $@\n").unwrap();
        std::fs::write(&launch, "#!/bin/bash\nLD_LIBRARY_PATH=$LD_LIBRARY_PATH:./x64 ./x64/insatia\n").unwrap();
        assert!(!sets_library_path(&kick));
        assert!(sets_library_path(&launch));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn python2_statements() {
        assert!(python2_source("import pygame\nprint \"hello\"\n"));
        assert!(python2_source("try:\n    x()\nexcept IOError, e:\n    pass\n"));
        assert!(!python2_source("print(\"hello\")\ntry:\n    x()\nexcept (IOError, OSError):\n    pass\n"));
    }
}
