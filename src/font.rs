//! Font files made for games. Old Unity players draw their built-in GUI font with a system font
//! they find by name ("Arial") in /usr/share/fonts only; Linux systems have Liberation Sans,
//! made to replace Arial, under its own name. A copy with the names in its `name` table changed
//! is all they need. Liberation's licence (SIL OFL) allows changed copies under another name.

fn u16_at(d: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(d.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(d.get(at..at + 4)?.try_into().ok()?))
}

/// The sum of a table's 32-bit words, as the table directory keeps it.
fn checksum(d: &[u8]) -> u32 {
    d.chunks(4).fold(0u32, |sum, c| {
        let mut w = [0u8; 4];
        w[..c.len()].copy_from_slice(c);
        sum.wrapping_add(u32::from_be_bytes(w))
    })
}

/// (offset of its directory record, table offset, table length) of a table.
fn table(d: &[u8], tag: &[u8; 4]) -> Option<(usize, usize, usize)> {
    let count = u16_at(d, 4)? as usize;
    (0..count).map(|i| 12 + 16 * i).find(|&r| d.get(r..r + 4) == Some(tag)).and_then(|r| Some((r, u32_at(d, r + 8)? as usize, u32_at(d, r + 12)? as usize)))
}

/// A TrueType font with the strings of these name IDs replaced (1 family, 4 full name,
/// 6 PostScript name, 16 typographic family) in every language and platform it has them for.
/// The new `name` table goes at the end of the file; the old one stays as unused bytes.
pub fn renamed(data: &[u8], names: &[(u16, &str)]) -> Option<Vec<u8>> {
    if !matches!(u32_at(data, 0)?, 0x0001_0000 | 0x7472_7565) {
        return None;
    }
    let (record, start, len) = table(data, b"name")?;
    let name = data.get(start..start + len)?;
    // Format 1 adds language tags after the records; Liberation's is format 0.
    if u16_at(name, 0)? != 0 {
        return None;
    }
    let count = u16_at(name, 2)? as usize;
    let storage = u16_at(name, 4)? as usize;
    let mut records = Vec::new();
    let mut strings: Vec<u8> = Vec::new();
    for i in 0..count {
        let r = 6 + 12 * i;
        let (platform, id) = (u16_at(name, r)?, u16_at(name, r + 6)?);
        let (length, offset) = (u16_at(name, r + 8)? as usize, u16_at(name, r + 10)? as usize);
        let old = name.get(storage + offset..storage + offset + length)?;
        let text = match names.iter().find(|(n, _)| *n == id) {
            Some((_, new)) if platform == 0 || platform == 3 => new.encode_utf16().flat_map(u16::to_be_bytes).collect(),
            Some((_, new)) if platform == 1 && new.is_ascii() => new.as_bytes().to_vec(),
            _ => old.to_vec(),
        };
        let mut rec = name[r..r + 8].to_vec();
        rec.extend((text.len() as u16).to_be_bytes());
        rec.extend((strings.len() as u16).to_be_bytes());
        records.extend(rec);
        strings.extend(text);
    }
    let mut new_name = Vec::new();
    new_name.extend(0u16.to_be_bytes());
    new_name.extend((count as u16).to_be_bytes());
    new_name.extend(((6 + 12 * count) as u16).to_be_bytes());
    new_name.extend(records);
    new_name.extend(strings);

    let mut out = data.to_vec();
    out.resize(out.len().next_multiple_of(4), 0);
    let at = out.len();
    out.extend(&new_name);
    out.resize(out.len().next_multiple_of(4), 0);
    out[record + 4..record + 8].copy_from_slice(&checksum(&new_name).to_be_bytes());
    out[record + 8..record + 12].copy_from_slice(&(at as u32).to_be_bytes());
    out[record + 12..record + 16].copy_from_slice(&(new_name.len() as u32).to_be_bytes());
    // The whole file's checksum, kept in `head`, changed with it.
    if let Some((_, head, _)) = table(&out, b"head") {
        out.get_mut(head + 8..head + 12)?.copy_from_slice(&[0; 4]);
        let adjust = 0xB1B0_AFBAu32.wrapping_sub(checksum(&out));
        out[head + 8..head + 12].copy_from_slice(&adjust.to_be_bytes());
    }
    Some(out)
}

/// A font's family name (ID 1, Windows platform, English).
#[cfg(test)]
pub fn family(data: &[u8]) -> Option<String> {
    let (_, start, len) = table(data, b"name")?;
    let name = data.get(start..start + len)?;
    let storage = u16_at(name, 4)? as usize;
    (0..u16_at(name, 2)? as usize).map(|i| 6 + 12 * i).find_map(|r| {
        (u16_at(name, r)? == 3 && u16_at(name, r + 4)? == 0x409 && u16_at(name, r + 6)? == 1).then_some(())?;
        let (length, offset) = (u16_at(name, r + 8)? as usize, u16_at(name, r + 10)? as usize);
        let units: Vec<u16> = name.get(storage + offset..storage + offset + length)?.chunks(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
        String::from_utf16(&units).ok()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renames_liberation_sans() {
        let Some(file) = ["/usr/share/fonts/liberation-sans-fonts/LiberationSans-Regular.ttf", "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf"].iter().find(|f| std::path::Path::new(f).is_file()) else { return };
        let data = std::fs::read(file).unwrap();
        assert_eq!(family(&data).as_deref(), Some("Liberation Sans"));
        let out = renamed(&data, &[(1, "Arial"), (4, "Arial"), (6, "ArialMT"), (16, "Arial")]).unwrap();
        assert_eq!(family(&out).as_deref(), Some("Arial"));
        assert_eq!(checksum(&out), 0xB1B0_AFBA);
    }
}
