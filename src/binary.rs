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
}

pub struct Elf {
    pub bits: u8,
    pub machine: u16,
    /// Has an interpreter (ld.so), so it is a program rather than a library.
    pub program: bool,
    pub needed: Vec<String>,
    /// Asks for an executable stack: glibc 2.41+ refuses to dlopen such libraries.
    /// A missing PT_GNU_STACK counts, as x86 defaults to executable.
    pub exec_stack: bool,
}

pub struct Pe {
    pub bits: u8,
    pub installer: Option<&'static str>,
}

pub fn identify(path: &Path) -> Option<Binary> {
    let mut f = File::open(path).ok()?;
    let mut head = [0u8; 64];
    let n = f.read(&mut head).ok()?;
    let head = &head[..n];
    if head.starts_with(b"\x7fELF") && n == 64 {
        elf(&mut f, head).map(Binary::Elf)
    } else if head.starts_with(b"MZ") && n == 64 {
        pe(&mut f, head).map(Binary::Pe)
    } else if head.starts_with(b"#!") {
        Some(Binary::Script)
    } else {
        None
    }
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

    let mut needed = Vec::new();
    if let Some((offset, size)) = dynamic {
        let entry = if wide { 16 } else { 8 };
        let dynsec = read_at(f, offset, size.min(1 << 20) as usize)?;
        let (mut strtab, mut strsz, mut names) = (0, 0, Vec::new());
        for d in dynsec.chunks_exact(entry) {
            let (tag, val) = if wide { (u64le(d, 0), u64le(d, 8)) } else { (u32le(d, 0) as u64, u32le(d, 4) as u64) };
            match tag {
                0 => break,
                1 => names.push(val),
                5 => strtab = val,
                10 => strsz = val,
                _ => {}
            }
        }
        // DT_STRTAB is an address; find the file offset through the segment that holds it.
        let file_off = loads.iter().find(|(va, _, sz)| strtab >= *va && strtab < va + sz).map(|(va, off, _)| strtab - va + off);
        if let Some(off) = file_off {
            let strings = read_at(f, off, strsz.min(1 << 20) as usize)?;
            for n in names {
                let start = n as usize;
                if start < strings.len() {
                    let end = strings[start..].iter().position(|&c| c == 0).map_or(strings.len(), |e| start + e);
                    needed.push(String::from_utf8_lossy(&strings[start..end]).into_owned());
                }
            }
        }
    }

    Some(Elf { bits, machine, program, needed, exec_stack })
}

fn pe(f: &mut File, h: &[u8]) -> Option<Pe> {
    let lfanew = u32le(h, 0x3c) as u64;
    let nt = read_at(f, lfanew, 26)?;
    if &nt[..4] != b"PE\0\0" {
        return None;
    }
    let bits = match u16le(&nt, 24) {
        0x10b => 32,
        0x20b => 64,
        _ => return None,
    };
    // Installer stubs name themselves in their resources, near the start of the file.
    let mut stub = Vec::new();
    f.seek(SeekFrom::Start(0)).ok()?;
    f.take(8 << 20).read_to_end(&mut stub).ok()?;
    let installer = [("Inno Setup", "Inno Setup"), ("Nullsoft", "NSIS"), ("InstallShield", "InstallShield")]
        .into_iter()
        .find(|(marker, _)| stub.windows(marker.len()).any(|w| w == marker.as_bytes()))
        .map(|(_, name)| name);
    Some(Pe { bits, installer })
}
