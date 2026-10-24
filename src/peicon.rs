//! The icon a Windows program carries (its biggest one), as a PNG for app-menu entries and
//! Steam shortcuts. The program's resources hold icon groups (RT_GROUP_ICON) listing images
//! (RT_ICON), each a PNG already or a bitmap (DIB), which is turned into one here. Taken over
//! from lobby's peicon.rs, with the PNG writing gamefix needs to stay std only.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

const RT_ICON: u32 = 3;
const RT_GROUP_ICON: u32 = 14;

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

/// The resource section's bytes and the address (RVA) it starts at.
fn resources(path: &Path) -> Option<(Vec<u8>, u32)> {
    let mut f = File::open(path).ok()?;
    let mut head = vec![0u8; 4096];
    let n = f.read(&mut head).ok()?;
    head.truncate(n);
    let pe = u32_at(&head, 0x3c)? as usize;
    if head.get(pe..pe + 4)? != b"PE\0\0" {
        return None;
    }
    let sections = u16_at(&head, pe + 6)? as usize;
    let optional = pe + 24;
    let opt_size = u16_at(&head, pe + 20)? as usize;
    let dirs = match u16_at(&head, optional)? {
        0x10b => optional + 96,
        0x20b => optional + 112,
        _ => return None,
    };
    let rva = u32_at(&head, dirs + 2 * 8)?;
    if rva == 0 {
        return None;
    }
    let table = optional + opt_size;
    for i in 0..sections {
        let s = table + i * 40;
        let (va, raw_size, raw_at) = (u32_at(&head, s + 12)?, u32_at(&head, s + 16)?, u32_at(&head, s + 20)?);
        let virtual_size = u32_at(&head, s + 8)?.max(raw_size);
        if rva >= va && rva < va + virtual_size {
            // Big resource sections are videos or data, not icons worth reading 100 MB for.
            if raw_size > 64 << 20 {
                return None;
            }
            let mut data = vec![0u8; raw_size as usize];
            f.seek(SeekFrom::Start(raw_at as u64)).ok()?;
            f.read_exact(&mut data).ok()?;
            return Some((data, va));
        }
    }
    None
}

/// A resource directory's entries: (id, offset, whether it's a directory). Named entries get
/// id u32::MAX.
fn entries(rsrc: &[u8], at: usize) -> Vec<(u32, usize, bool)> {
    let (Some(named), Some(ids)) = (u16_at(rsrc, at + 12), u16_at(rsrc, at + 14)) else { return Vec::new() };
    (0..(named + ids) as usize)
        .filter_map(|i| {
            let e = at + 16 + i * 8;
            let (name, off) = (u32_at(rsrc, e)?, u32_at(rsrc, e + 4)?);
            let id = if name & 0x8000_0000 != 0 { u32::MAX } else { name };
            Some((id, (off & 0x7fff_ffff) as usize, off & 0x8000_0000 != 0))
        })
        .collect()
}

/// The data of the first resource of a type (with a given id, if any), whatever its language.
fn resource(rsrc: &[u8], va: u32, kind: u32, id: Option<u32>) -> Option<&[u8]> {
    let (_, names, _) = entries(rsrc, 0).into_iter().find(|(t, _, dir)| *t == kind && *dir)?;
    let (_, langs, _) = entries(rsrc, names).into_iter().find(|(i, _, dir)| *dir && id.is_none_or(|id| *i == id))?;
    let (_, data, _) = entries(rsrc, langs).into_iter().find(|(_, _, dir)| !dir)?;
    let (at, size) = (u32_at(rsrc, data)?.checked_sub(va)? as usize, u32_at(rsrc, data + 4)? as usize);
    rsrc.get(at..at + size)
}

/// A program's RCDATA resource with this id (Unreal's launcher keeps the program it starts and
/// its arguments in 201 and 202).
pub(crate) fn rcdata(path: &Path, id: u32) -> Option<Vec<u8>> {
    const RT_RCDATA: u32 = 10;
    let (rsrc, va) = resources(path)?;
    resource(&rsrc, va, RT_RCDATA, Some(id)).map(<[u8]>::to_vec)
}

/// The program's biggest icon as a PNG's bytes.
pub fn icon_png(path: &Path) -> Option<Vec<u8>> {
    let (rsrc, va) = resources(path)?;
    let group = resource(&rsrc, va, RT_GROUP_ICON, None)?;
    let count = u16_at(group, 4)? as usize;
    // The biggest (0 means 256), then the most colours.
    let (_, _, id) = (0..count)
        .filter_map(|i| {
            let e = 6 + i * 14;
            let size = |b: u8| if b == 0 { 256u32 } else { b as u32 };
            Some((size(*group.get(e)?), u16_at(group, e + 6)?, u16_at(group, e + 12)?))
        })
        .max_by_key(|(w, bits, _)| (*w, *bits))?;
    let image = resource(&rsrc, va, RT_ICON, Some(id as u32))?;
    if image.starts_with(b"\x89PNG") {
        return Some(image.to_vec());
    }
    let (w, h, rgba) = dib_rgba(image)?;
    Some(png(w, h, &rgba))
}

/// An icon's bitmap (a BITMAPINFOHEADER, a palette for 8 bits or fewer, the colour rows, then
/// the 1-bit transparency mask; rows bottom up, the height counting both) as RGBA rows, top down.
fn dib_rgba(dib: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let header = u32_at(dib, 0)? as usize;
    let w = u32_at(dib, 4)? as usize;
    let h = (u32_at(dib, 8)? as i32).unsigned_abs() as usize / 2;
    let bits = u16_at(dib, 14)? as usize;
    if w == 0 || h == 0 || w > 1024 || h > 1024 || u32_at(dib, 16)? != 0 || ![1, 4, 8, 24, 32].contains(&bits) {
        return None;
    }
    let colors = if bits <= 8 { match u32_at(dib, 32)? as usize { 0 => 1 << bits, n => n } } else { 0 };
    let palette = dib.get(header..header + colors * 4)?;
    let pixels = header + colors * 4;
    let stride = (w * bits).div_ceil(32) * 4;
    let mask_at = pixels + stride * h;
    let mask_stride = w.div_ceil(32) * 4;
    let mut out = Vec::with_capacity(w * h * 4);
    let mut alpha_seen = false;
    for y in 0..h {
        let row = pixels + (h - 1 - y) * stride;
        let mask_row = mask_at + (h - 1 - y) * mask_stride;
        for x in 0..w {
            let [b, g, r, a] = match bits {
                32 => {
                    let p = dib.get(row + x * 4..row + x * 4 + 4)?;
                    alpha_seen |= p[3] != 0;
                    [p[0], p[1], p[2], p[3]]
                }
                24 => {
                    let p = dib.get(row + x * 3..row + x * 3 + 3)?;
                    [p[0], p[1], p[2], 255]
                }
                _ => {
                    let byte = *dib.get(row + x * bits / 8)?;
                    let shift = 8 - bits - (x * bits % 8);
                    let i = ((byte >> shift) & ((1 << bits) - 1) as u8) as usize;
                    let p = palette.get(i * 4..i * 4 + 3)?;
                    [p[0], p[1], p[2], 255]
                }
            };
            // The mask says which pixels are see-through, unless the colours carry alpha.
            let masked = dib.get(mask_row + x / 8).is_some_and(|m| m >> (7 - x % 8) & 1 == 1);
            out.extend([r, g, b, if bits == 32 { a } else if masked { 0 } else { a }]);
        }
    }
    // 32-bit icons from before Windows XP leave alpha at 0 and use the mask alone.
    if bits == 32 && !alpha_seen {
        for y in 0..h {
            let mask_row = mask_at + (h - 1 - y) * mask_stride;
            for x in 0..w {
                let masked = dib.get(mask_row + x / 8).is_some_and(|m| m >> (7 - x % 8) & 1 == 1);
                out[(y * w + x) * 4 + 3] = if masked { 0 } else { 255 };
            }
        }
    }
    Some((w as u32, h as u32, out))
}

/// An RGBA image as a PNG, its rows stored without compression (zlib's "stored" blocks): an
/// icon is small, and gamefix has no deflate.
fn png(w: u32, h: u32, rgba: &[u8]) -> Vec<u8> {
    let mut raw = Vec::with_capacity(rgba.len() + h as usize);
    for row in rgba.chunks(w as usize * 4) {
        raw.push(0);
        raw.extend(row);
    }
    let mut zlib = vec![0x78, 0x01];
    let mut blocks = raw.chunks(0xffff).peekable();
    while let Some(block) = blocks.next() {
        zlib.push(blocks.peek().is_none() as u8);
        zlib.extend((block.len() as u16).to_le_bytes());
        zlib.extend((!(block.len() as u16)).to_le_bytes());
        zlib.extend(block);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &byte in &raw {
        a = (a + byte as u32) % 65521;
        b = (b + a) % 65521;
    }
    zlib.extend(((b << 16) | a).to_be_bytes());
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut chunk = |kind: &[u8], data: &[u8]| {
        out.extend((data.len() as u32).to_be_bytes());
        let start = out.len();
        out.extend(kind);
        out.extend(data);
        let crc = crate::steam::crc32(&out[start..]);
        out.extend(crc.to_be_bytes());
    };
    let mut ihdr = Vec::new();
    ihdr.extend(w.to_be_bytes());
    ihdr.extend(h.to_be_bytes());
    ihdr.extend([8, 6, 0, 0, 0]);
    chunk(b"IHDR", &ihdr);
    chunk(b"IDAT", &zlib);
    chunk(b"IEND", &[]);
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn turns_bitmaps_into_pngs() {
        // A 2x2 32-bit icon bitmap: rows bottom up, then a 2-row mask (4 bytes a row).
        let mut dib = Vec::new();
        dib.extend(40u32.to_le_bytes());
        dib.extend(2u32.to_le_bytes());
        dib.extend(4u32.to_le_bytes());
        dib.extend(1u16.to_le_bytes());
        dib.extend(32u16.to_le_bytes());
        dib.extend([0; 24]);
        dib.extend([0, 0, 255, 255, 0, 255, 0, 255]); // bottom row: red, green
        dib.extend([255, 0, 0, 255, 0, 0, 0, 0]); // top row: blue, clear
        dib.extend([0; 8]);
        let (w, h, rgba) = super::dib_rgba(&dib).unwrap();
        assert_eq!((w, h), (2, 2));
        assert_eq!(rgba, [0, 0, 255, 255, 0, 0, 0, 0, 255, 0, 0, 255, 0, 255, 0, 255]);
        let png = super::png(w, h, &rgba);
        assert!(png.starts_with(b"\x89PNG\r\n\x1a\n") && png.ends_with(b"IEND\xaeB`\x82"));
    }
}
