//! A raw scan of a ZIP's bytes before anything is inflated. It finds the last end record, reads
//! the central directory and every local header, and refuses a file that two readers could
//! read differently: an end record outside every entry's data, overlapping entries, a local
//! header that disagrees with the directory, a local header the directory doesn't list,
//! duplicate or case-equal names, or a Unicode-path field that disagrees with the name.

use std::collections::HashSet;
use std::ops::Range;

use crate::report::Error;

/// The most entries a package may have.
pub const MAX_ENTRIES: u64 = 10_000;

const LOCAL: &[u8] = b"PK\x03\x04";
const CENTRAL: &[u8] = b"PK\x01\x02";
const END: &[u8] = b"PK\x05\x06";
const END64: &[u8] = b"PK\x06\x06";
const LOCATOR: &[u8] = b"PK\x06\x07";
const DESCRIPTOR: &[u8] = b"PK\x07\x08";
const MAX32: u64 = 0xFFFF_FFFF;

/// One entry: its name, how its data is stored, and where the data is in the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub method: u16,
    pub crc: u32,
    pub data: Range<usize>,
}

/// What the scan found.
#[derive(Debug, PartialEq, Eq)]
pub struct Zip {
    /// In directory order.
    pub entries: Vec<Entry>,
    /// Bytes before the first entry (prepended data).
    pub before: usize,
    /// Bytes between entries that no entry covers.
    pub between: usize,
    /// Bytes after the end record and its comment.
    pub after: usize,
    pub comment: Range<usize>,
}

fn u16_at(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from(u16::from_le_bytes(
        b.get(at..at + 2)?.try_into().ok()?,
    )))
}

fn u32_at(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from(u32::from_le_bytes(
        b.get(at..at + 4)?.try_into().ok()?,
    )))
}

fn u64_at(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

fn at(b: &[u8], pos: usize, sig: &[u8]) -> bool {
    b.get(pos..pos + sig.len()) == Some(sig)
}

fn structure() -> Error {
    Error::damaged("its structure can't be read")
}

fn index() -> Error {
    Error::ambiguous("its file index doesn't match its contents")
}

/// The central directory an end record at `p` describes: (start, end, entry count, declared
/// offset), or None when the record doesn't lead to one.
fn directory(b: &[u8], p: usize) -> Option<(usize, usize, u64, u64)> {
    let mut count = u16_at(b, p + 10)?;
    let mut size = u32_at(b, p + 12)?;
    let mut offset = u32_at(b, p + 16)?;
    let mut end = p;
    if count == 0xFFFF || size == MAX32 || offset == MAX32 {
        let record = p.checked_sub(20 + 56)?;
        if !at(b, p - 20, LOCATOR) || !at(b, record, END64) {
            return None;
        }
        count = u64_at(b, record + 32)?;
        size = u64_at(b, record + 40)?;
        offset = u64_at(b, record + 48)?;
        end = record;
    }
    let start = end.checked_sub(usize::try_from(size).ok()?)?;
    let listed = if count == 0 {
        size == 0
    } else {
        at(b, start, CENTRAL)
    };
    listed.then_some((start, end, count, offset))
}

pub fn scan(b: &[u8]) -> Result<Zip, Error> {
    let ends: Vec<usize> = (0..b.len().saturating_sub(21))
        .filter(|&p| at(b, p, END) && directory(b, p).is_some())
        .collect();
    let Some((&p, others)) = ends.split_last() else {
        return Err(match b.starts_with(LOCAL) {
            true => structure(),
            false => Error::unsupported_format(),
        });
    };
    let zip = read(b, p);
    if others.is_empty() {
        return zip;
    }
    // Office stores an embedded workbook uncompressed, so its end record is a part's bytes.
    // Any other is a second index.
    let inside = |z: &Zip, at| z.entries.iter().any(|e| e.data.contains(at));
    zip.ok()
        .filter(|z| others.iter().all(|at| inside(z, at)))
        .ok_or_else(index)
}

/// The package the end record at `p` describes.
fn read(b: &[u8], p: usize) -> Result<Zip, Error> {
    let (start, end, count, offset) = directory(b, p).ok_or_else(structure)?;
    if count > MAX_ENTRIES {
        return Err(Error::too_large("its entry count", "10,000"));
    }
    let shift = usize::try_from(offset)
        .ok()
        .and_then(|o| start.checked_sub(o))
        .ok_or_else(structure)?;
    let comment_len = u16_at(b, p + 20).ok_or_else(structure)? as usize;
    let comment = p + 22..(p + 22 + comment_len).min(b.len());

    let mut entries = vec![];
    let mut spans = vec![];
    let mut names = HashSet::new();
    let mut c = start;
    for _ in 0..count {
        let (entry, span, next) = central(b, c, shift, start)?;
        if !names.insert(entry.name.to_ascii_lowercase()) {
            return Err(Error::two_versions(&entry.name));
        }
        entries.push(entry);
        spans.push(span);
        c = next;
    }
    if c != end {
        return Err(index());
    }

    spans.sort_by_key(|s| s.start);
    let first = spans.first().map_or(start, |s| s.start);
    // A local header the directory doesn't list is a part only some readers see.
    let unlisted = |gap: Range<usize>| b[gap].windows(4).any(|w| w == LOCAL);
    if unlisted(0..first) {
        return Err(index());
    }
    let mut between = 0;
    for (s, next) in spans
        .iter()
        .zip(spans.iter().skip(1).map(|s| s.start).chain([start]))
    {
        if next < s.end {
            return Err(Error::ambiguous("its parts overlap"));
        }
        if unlisted(s.end..next) {
            return Err(index());
        }
        between += next - s.end;
    }
    Ok(Zip {
        entries,
        before: first,
        between,
        after: b.len() - comment.end,
        comment,
    })
}

/// The central record at `c`: its entry, the span its local header and data take in the
/// file, and where the next record starts.
fn central(
    b: &[u8],
    c: usize,
    shift: usize,
    cd: usize,
) -> Result<(Entry, Range<usize>, usize), Error> {
    let get = |o| u32_at(b, c + o).ok_or_else(structure);
    let get16 = |o| u16_at(b, c + o).ok_or_else(structure);
    if !at(b, c, CENTRAL) {
        return Err(index());
    }
    let flags = get16(8)?;
    let method = get16(10)? as u16;
    let crc = get(16)? as u32;
    let (mut csize, size, mut offset) = (get(20)?, get(24)?, get(42)?);
    let (nlen, xlen, klen) = (
        get16(28)? as usize,
        get16(30)? as usize,
        get16(32)? as usize,
    );
    let raw_name = b.get(c + 46..c + 46 + nlen).ok_or_else(structure)?;
    let extra = b
        .get(c + 46 + nlen..c + 46 + nlen + xlen)
        .ok_or_else(structure)?;
    let name = String::from_utf8_lossy(raw_name).into_owned();
    if flags & 1 != 0 {
        return Err(Error::damaged(
            "it has an encrypted part, which Office files never have",
        ));
    }
    let mut zip64 = false;
    for (id, data) in fields(extra) {
        match id {
            0x0001 => {
                zip64 = true;
                let mut values = data.chunks(8).map(|v| v.try_into().map(u64::from_le_bytes));
                // Fields appear only for the header values that are 0xFFFFFFFF, in order.
                if size == MAX32 {
                    values.next();
                }
                if csize == MAX32 {
                    csize = values.next().and_then(Result::ok).ok_or_else(structure)?;
                }
                if offset == MAX32 {
                    offset = values.next().and_then(Result::ok).ok_or_else(structure)?;
                }
            }
            // The Info-ZIP Unicode path: version 1, the CRC of the header name, the name.
            0x7075 if data.get(5..).is_some_and(|n| n != raw_name) => return Err(index()),
            _ => {}
        }
    }
    let to_usize = |v: u64| usize::try_from(v).map_err(|_| structure());
    let local = shift.checked_add(to_usize(offset)?).ok_or_else(structure)?;
    if !at(b, local, LOCAL) {
        return Err(index());
    }
    let lget = |o| u32_at(b, local + o).ok_or_else(structure);
    let lflags = u16_at(b, local + 6).ok_or_else(structure)?;
    let lmethod = u16_at(b, local + 8).ok_or_else(structure)?;
    let (lcrc, lcsize) = (lget(14)?, lget(18)?);
    let lnlen = u16_at(b, local + 26).ok_or_else(structure)? as usize;
    let lxlen = u16_at(b, local + 28).ok_or_else(structure)? as usize;
    let deferred = lflags & 8 != 0;
    let differs = b.get(local + 30..local + 30 + lnlen) != Some(raw_name)
        || lmethod != u64::from(method)
        || (lflags & 1) != 0
        || (!deferred && lcrc != u64::from(crc))
        || (!deferred && lcsize != MAX32 && lcsize != csize);
    if differs {
        return Err(index());
    }
    let data_start = local + 30 + lnlen + lxlen;
    let data_end = data_start
        .checked_add(to_usize(csize)?)
        .ok_or_else(structure)?;
    if data_end > cd {
        return Err(structure());
    }
    let mut span_end = data_end;
    if deferred {
        let sig = if at(b, data_end, DESCRIPTOR) { 4 } else { 0 };
        span_end += sig + if zip64 { 20 } else { 12 };
    }
    let entry = Entry {
        name,
        method,
        crc,
        data: data_start..data_end,
    };
    Ok((entry, local..span_end, c + 46 + nlen + xlen + klen))
}

/// The (id, data) fields of an extra field block.
fn fields(mut extra: &[u8]) -> impl Iterator<Item = (u16, &[u8])> {
    std::iter::from_fn(move || {
        let id = u16::from_le_bytes(extra.get(..2)?.try_into().ok()?);
        let len = usize::from(u16::from_le_bytes(extra.get(2..4)?.try_into().ok()?));
        let data = extra.get(4..4 + len)?;
        extra = &extra[4 + len..];
        Some((id, data))
    })
}
