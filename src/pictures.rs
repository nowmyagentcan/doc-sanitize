//! What pictures carry besides their pixels: EXIF (with GPS), XMP, IPTC, comments, text
//! chunks, thumbnails and bytes after the image. The walkers read each format's own
//! structure, never decode pixels, and when stripping copy every other byte as it was.

use quick_xml::events::Event;

use crate::office::tree::{Doc, El, Node};
use crate::office::xml::Reader;
use crate::report::Error;

/// What a picture holds, in the words a detail uses.
pub type Labels = Vec<&'static str>;

/// The picture's metadata labels, or None when its structure is cut short or broken. Formats
/// other than JPEG, PNG, GIF, WebP, SVG and TIFF give none.
pub fn metadata(b: &[u8]) -> Option<Labels> {
    let mut w = Walk::default();
    walk(b, &mut w).ok()?;
    let mut labels = w.labels;
    sort(&mut labels);
    Some(labels)
}

/// The picture without its metadata, every pixel byte copied as it was; None for a TIFF or a
/// broken picture. For `office` an EXIF orientation other than 1 stays, in an EXIF block
/// holding only that tag; otherwise every EXIF block goes.
pub fn strip(b: &[u8], office: bool) -> Option<Vec<u8>> {
    if is_tiff(b) {
        return None;
    }
    let mut w = Walk {
        out: Some(vec![]),
        office,
        ..Walk::default()
    };
    match walk(b, &mut w).ok()? {
        true => w.out,
        false => Some(b.to_vec()),
    }
}

pub fn is_tiff(b: &[u8]) -> bool {
    b.starts_with(b"II*\0") || b.starts_with(b"MM\0*")
}

/// Walks a picture; false when it isn't a format with a walker.
fn walk(b: &[u8], w: &mut Walk) -> Result<bool, ()> {
    if b.starts_with(&[0xFF, 0xD8]) {
        jpeg(b, w)?;
    } else if b.starts_with(PNG) {
        png(b, w)?;
    } else if b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a") {
        gif(b, w)?;
    } else if b.len() >= 12 && &b[..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        webp(b, w)?;
    } else if is_tiff(b) {
        w.labels = tiff(b)?;
    } else {
        return Ok(false);
    }
    Ok(true)
}

/// A walk over a picture: the labels of what it holds and, when stripping, the bytes of
/// every piece that holds none.
#[derive(Default)]
struct Walk {
    labels: Labels,
    out: Option<Vec<u8>>,
    office: bool,
}

/// What becomes of an EXIF block.
enum Exif {
    Keep,
    /// Dropped, with the orientation to write back in its place.
    Drop(Option<u16>),
}

impl Walk {
    fn keep(&mut self, pieces: &[&[u8]]) {
        if let Some(out) = &mut self.out {
            for p in pieces {
                out.extend_from_slice(p);
            }
        }
    }

    /// An EXIF block (a TIFF structure) holding only the orientation is no metadata: Office
    /// copies keep it. Any other is labelled and dropped, its orientation kept for Office.
    fn exif(&mut self, tiff: &[u8]) -> Result<Exif, ()> {
        let entries = ifd0(tiff)?;
        let turn = entries.iter().find(|e| e.0 == 0x0112).map(|e| e.1);
        if entries.len() == 1 && turn.is_some() {
            return Ok(if self.office {
                Exif::Keep
            } else {
                Exif::Drop(None)
            });
        }
        self.labels.push("EXIF");
        if entries.iter().any(|e| e.0 == 0x8825) {
            self.labels.push("GPS");
        }
        Ok(Exif::Drop(turn.filter(|&t| self.office && t != 1)))
    }
}

/// The order labels are named in.
const ORDER: [&str; 9] = [
    "EXIF",
    "GPS",
    "XMP",
    "IPTC",
    "comments",
    "text chunks",
    "a thumbnail",
    "other metadata",
    "trailing bytes",
];

fn sort(labels: &mut Labels) {
    labels.sort_unstable_by_key(|l| ORDER.iter().position(|o| o == l));
    labels.dedup();
}

fn be16(b: &[u8], at: usize) -> Result<usize, ()> {
    Ok(usize::from(u16::from_be_bytes(
        b.get(at..at + 2).ok_or(())?.try_into().map_err(drop)?,
    )))
}

fn be32(b: &[u8], at: usize) -> Result<usize, ()> {
    Ok(u32::from_be_bytes(b.get(at..at + 4).ok_or(())?.try_into().map_err(drop)?) as usize)
}

/// Whether bytes after the end of an image hold anything but padding.
fn trailing(rest: &[u8]) -> bool {
    rest.iter().any(|&c| c != 0 && !c.is_ascii_whitespace())
}

/// A little-endian TIFF structure holding only the orientation `turn`.
fn turned(turn: u16) -> Vec<u8> {
    let mut b = b"II*\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0".to_vec();
    b.extend(turn.to_le_bytes());
    b.extend([0; 6]);
    b
}

fn jpeg(b: &[u8], w: &mut Walk) -> Result<(), ()> {
    w.keep(&[&b[..2]]);
    let mut i = 2;
    loop {
        if b.get(i) != Some(&0xFF) {
            return Err(());
        }
        while b.get(i) == Some(&0xFF) {
            i += 1;
        }
        let marker = *b.get(i).ok_or(())?;
        i += 1;
        match marker {
            0xD9 => {
                w.keep(&[&[0xFF, 0xD9]]);
                if trailing(&b[i..]) {
                    w.labels.push("trailing bytes");
                }
                return Ok(());
            }
            0xD0..=0xD7 | 0x01 | 0xD8 => {
                w.keep(&[&[0xFF, marker]]);
                continue;
            }
            _ => {}
        }
        let len = be16(b, i)?;
        let seg = b.get(i + 2..i + len).filter(|_| len >= 2).ok_or(())?;
        let whole = &b[i..i + len];
        i += len;
        let keep = match marker {
            0xE0 if seg.starts_with(b"JFIF\0") => {
                let thumbnail = seg.get(12..14).is_some_and(|t| t != [0, 0]);
                if thumbnail {
                    w.labels.push("a thumbnail");
                    w.keep(&[&[0xFF, 0xE0, 0, 16], seg.get(..12).ok_or(())?, &[0, 0]]);
                }
                !thumbnail
            }
            0xE0 if seg.starts_with(b"JFXX\0") => {
                w.labels.push("a thumbnail");
                false
            }
            0xE1 if seg.starts_with(b"Exif\0\0") => match w.exif(&seg[6..])? {
                Exif::Keep => true,
                Exif::Drop(turn) => {
                    if let Some(t) = turn {
                        let body = [b"Exif\0\0".as_slice(), &turned(t)].concat();
                        let len = (body.len() as u16 + 2).to_be_bytes();
                        w.keep(&[&[0xFF, 0xE1], &len, &body]);
                    }
                    false
                }
            },
            0xE1 if seg.starts_with(b"http://ns.adobe.com/xap/1.0/\0")
                || seg.starts_with(b"http://ns.adobe.com/xmp/extension/\0") =>
            {
                w.labels.push("XMP");
                false
            }
            0xE2 if seg.starts_with(b"ICC_PROFILE\0") => true,
            0xED => {
                w.labels.push("IPTC");
                false
            }
            0xEE if seg.starts_with(b"Adobe") => true,
            0xE0..=0xEF => {
                w.labels.push("other metadata");
                false
            }
            0xFE => {
                w.labels.push("comments");
                false
            }
            _ => true,
        };
        if keep {
            w.keep(&[&[0xFF, marker], whole]);
        }
        if marker == 0xDA {
            // Entropy-coded data runs to the next marker that isn't a stuffed byte or a
            // restart.
            let start = i;
            loop {
                let (c, next) = (*b.get(i).ok_or(())?, *b.get(i + 1).ok_or(())?);
                if c == 0xFF && next != 0 && !(0xD0..=0xD7).contains(&next) {
                    break;
                }
                i += 1;
            }
            w.keep(&[&b[start..i]]);
        }
    }
}

/// The (tag, first 16-bit value) entries of a TIFF structure's first directory, with a
/// (0, 0) entry added when another directory follows it (so a lone orientation is only that
/// when nothing follows).
fn ifd0(b: &[u8]) -> Result<Vec<(u16, u16)>, ()> {
    let little = match b.get(..4) {
        Some(b"II*\0") => true,
        Some(b"MM\0*") => false,
        _ => return Err(()),
    };
    let u16_at = |at: usize| -> Result<u16, ()> {
        let pair: [u8; 2] = b.get(at..at + 2).ok_or(())?.try_into().map_err(drop)?;
        Ok(if little {
            u16::from_le_bytes(pair)
        } else {
            u16::from_be_bytes(pair)
        })
    };
    let u32_at = |at: usize| -> Result<usize, ()> {
        let quad: [u8; 4] = b.get(at..at + 4).ok_or(())?.try_into().map_err(drop)?;
        Ok((if little {
            u32::from_le_bytes(quad)
        } else {
            u32::from_be_bytes(quad)
        }) as usize)
    };
    let start = u32_at(4)?;
    let n = usize::from(u16_at(start)?);
    let mut entries = (0..n)
        .map(|k| Ok((u16_at(start + 2 + 12 * k)?, u16_at(start + 10 + 12 * k)?)))
        .collect::<Result<Vec<_>, _>>()?;
    if u32_at(start + 2 + 12 * n)? != 0 {
        entries.push((0, 0));
    }
    Ok(entries)
}

/// The EXIF orientation of a JPEG (1 when it has none), or None when it can't be read.
pub fn orientation(b: &[u8]) -> Option<u16> {
    let mut i = 2;
    while b.get(i) == Some(&0xFF) {
        let marker = *b.get(i + 1)?;
        let len = be16(b, i + 2).ok()?;
        let seg = b.get(i + 4..i + 2 + len)?;
        match marker {
            0xE1 if seg.starts_with(b"Exif\0\0") => {
                let entries = ifd0(&seg[6..]).ok()?;
                return Some(entries.iter().find(|e| e.0 == 0x0112).map_or(1, |e| e.1));
            }
            0xDA => break,
            _ => i += 2 + len,
        }
    }
    Some(1)
}

/// TIFF tags that describe the picture's origin rather than its pixels.
fn tiff(b: &[u8]) -> Result<Labels, ()> {
    let mut labels = vec![];
    for (tag, _) in ifd0(b)? {
        labels.push(match tag {
            0x8769 => "EXIF",
            0x8825 => "GPS",
            700 => "XMP",
            33723 | 34377 => "IPTC",
            269 | 270 | 271 | 272 | 285 | 305 | 306 | 315 | 316 | 33432 => "other metadata",
            _ => continue,
        });
    }
    Ok(labels)
}

const PNG: &[u8] = b"\x89PNG\r\n\x1a\n";

const PNG_KEPT: [&[u8; 4]; 19] = [
    b"IHDR", b"PLTE", b"IDAT", b"IEND", b"tRNS", b"iCCP", b"sRGB", b"gAMA", b"cHRM", b"sBIT",
    b"cICP", b"mDCV", b"cLLI", b"pHYs", b"bKGD", b"sPLT", b"acTL", b"fcTL", b"fdAT",
];

/// A PNG chunk with its length and CRC.
pub fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut crc = flate2::Crc::new();
    crc.update(kind);
    crc.update(data);
    [
        &(data.len() as u32).to_be_bytes()[..],
        kind,
        data,
        &crc.sum().to_be_bytes(),
    ]
    .concat()
}

fn png(b: &[u8], w: &mut Walk) -> Result<(), ()> {
    w.keep(&[PNG]);
    let mut i = 8;
    loop {
        let len = be32(b, i)?;
        let kind = b.get(i + 4..i + 8).ok_or(())?;
        let data = b.get(i + 8..i + 8 + len).ok_or(())?;
        let mut crc = flate2::Crc::new();
        crc.update(kind);
        crc.update(data);
        if be32(b, i + 8 + len)? != crc.sum() as usize {
            return Err(());
        }
        let whole = &b[i..i + 12 + len];
        i += 12 + len;
        let keep = match kind {
            b"IEND" => {
                w.keep(&[whole]);
                if trailing(&b[i..]) {
                    w.labels.push("trailing bytes");
                }
                return Ok(());
            }
            b"tEXt" | b"zTXt" | b"iTXt" => {
                w.labels.push("text chunks");
                false
            }
            b"eXIf" => match w.exif(data)? {
                Exif::Keep => true,
                Exif::Drop(turn) => {
                    if let Some(t) = turn {
                        w.keep(&[&chunk(b"eXIf", &turned(t))]);
                    }
                    false
                }
            },
            k if PNG_KEPT.iter().any(|p| p == &k) => true,
            k if k[0].is_ascii_lowercase() => {
                w.labels.push("other metadata");
                false
            }
            _ => true,
        };
        if keep {
            w.keep(&[whole]);
        }
    }
}

/// Sub-blocks from `i` to their terminator; gives the first one and the index after.
fn sub_blocks(b: &[u8], mut i: usize) -> Result<(&[u8], usize), ()> {
    let mut first: Option<&[u8]> = None;
    loop {
        let n = usize::from(*b.get(i).ok_or(())?);
        i += 1;
        if n == 0 {
            return Ok((first.unwrap_or_default(), i));
        }
        let block = b.get(i..i + n).ok_or(())?;
        first.get_or_insert(block);
        i += n;
    }
}

fn gif(b: &[u8], w: &mut Walk) -> Result<(), ()> {
    let table = |flags: u8| {
        if flags & 0x80 != 0 {
            3 << ((flags & 7) + 1)
        } else {
            0
        }
    };
    let mut i = 13 + table(*b.get(10).ok_or(())?);
    w.keep(&[b.get(..i).ok_or(())?]);
    loop {
        match *b.get(i).ok_or(())? {
            0x3B => {
                w.keep(&[&[0x3B]]);
                if trailing(&b[i + 1..]) {
                    w.labels.push("trailing bytes");
                }
                return Ok(());
            }
            0x21 => {
                let label = *b.get(i + 1).ok_or(())?;
                let (first, next) = sub_blocks(b, i + 2)?;
                let found = match label {
                    0xFE => Some("comments"),
                    0xFF if first.starts_with(b"XMP DataXMP") => Some("XMP"),
                    0xFF if first.starts_with(b"NETSCAPE2.0")
                        || first.starts_with(b"ANIMEXTS1.0")
                        || first.starts_with(b"ICCRGBG1") =>
                    {
                        None
                    }
                    0xFF => Some("other metadata"),
                    _ => None,
                };
                match found {
                    Some(l) => w.labels.push(l),
                    None => w.keep(&[&b[i..next]]),
                }
                i = next;
            }
            0x2C => {
                let flags = *b.get(i + 9).ok_or(())?;
                let next = sub_blocks(b, i + 11 + table(flags))?.1;
                w.keep(&[&b[i..next]]);
                i = next;
            }
            _ => return Err(()),
        }
    }
}

fn webp(b: &[u8], w: &mut Walk) -> Result<(), ()> {
    w.keep(&[&b[..12]]);
    let mut exif_kept = false;
    let mut i = 12;
    while i < b.len() {
        let kind = b.get(i..i + 4).ok_or(())?;
        let quad: [u8; 4] = b.get(i + 4..i + 8).ok_or(())?.try_into().map_err(drop)?;
        let len = u32::from_le_bytes(quad) as usize;
        let data = b.get(i + 8..i + 8 + len).ok_or(())?;
        let whole = &b[i..(i + 8 + len + len % 2).min(b.len())];
        let keep = match kind {
            b"EXIF" => match w.exif(data.strip_prefix(b"Exif\0\0").unwrap_or(data))? {
                Exif::Keep => {
                    exif_kept = true;
                    true
                }
                Exif::Drop(turn) => {
                    if let Some(t) = turn {
                        let t = turned(t);
                        w.keep(&[b"EXIF", &(t.len() as u32).to_le_bytes(), &t]);
                        exif_kept = true;
                    }
                    false
                }
            },
            b"XMP " => {
                w.labels.push("XMP");
                false
            }
            _ => true,
        };
        if keep {
            w.keep(&[whole]);
        }
        i += 8 + len + len % 2;
    }
    if let Some(out) = &mut w.out {
        let size = (out.len() - 8) as u32;
        out[4..8].copy_from_slice(&size.to_le_bytes());
        if out.get(12..16) == Some(b"VP8X") && out.len() > 20 {
            // The flags say which metadata chunks follow: X (XMP) always goes, E (EXIF)
            // unless an EXIF chunk stayed.
            out[20] &= if exif_kept { !0x04 } else { !0x0C };
        }
    }
    Ok(())
}

const SVG: &str = "http://www.w3.org/2000/svg";
const EDITORS: [&str; 2] = [
    "http://sodipodi.sourceforge.net/DTD/sodipodi-0.dtd",
    "http://www.inkscape.org/namespaces/inkscape",
];

/// What an SVG picture holds: `<metadata>`, comments, and Inkscape or Sodipodi data.
pub fn svg(text: &str, part: &str) -> Result<Labels, Error> {
    let mut labels = vec![];
    let mut r = Reader::new(text, part);
    loop {
        match r.event()? {
            Event::Eof => break,
            Event::Comment(_) => labels.push("comments"),
            Event::Start(e) | Event::Empty(e) => {
                if r.is(&e, SVG, "metadata") {
                    labels.push("XMP");
                }
                let editor = EDITORS.contains(&r.ns(&e))
                    || r.attrs(&e)?
                        .iter()
                        .any(|(ns, ..)| EDITORS.contains(&ns.as_str()));
                if editor {
                    labels.push("other metadata");
                }
            }
            _ => {}
        }
    }
    sort(&mut labels);
    Ok(labels)
}

/// The SVG picture without `<metadata>`, comments, and Inkscape or Sodipodi elements,
/// attributes and declarations; `<title>` and `<desc>` stay.
pub fn strip_svg(bytes: &[u8], part: &str) -> Result<Vec<u8>, Error> {
    fn clean(e: &mut El) {
        e.attrs
            .retain(|a| !EDITORS.contains(&&*a.ns) && !EDITORS.contains(&a.value.as_str()));
        e.kids.retain(|k| match k {
            Node::Raw(r) => !r.starts_with("<!--"),
            Node::El(c) => !c.is(SVG, "metadata") && !EDITORS.contains(&&*c.ns),
            Node::Text(_) => true,
        });
        e.els_mut().for_each(clean);
    }
    let mut doc = Doc::parse(bytes, part)?;
    let mut prolog = String::new();
    let mut rest = doc.prolog.as_str();
    while let Some(at) = rest.find("<!--") {
        prolog += &rest[..at];
        rest = rest[at..]
            .find("-->")
            .map_or("", |end| &rest[at + end + 3..]);
    }
    doc.prolog = prolog + rest;
    clean(&mut doc.root);
    Ok(doc.write())
}

/// The labels joined for a detail: `EXIF`, `EXIF and GPS`, `EXIF, GPS and XMP`.
pub fn joined(labels: &[&str]) -> String {
    match labels {
        [] => String::new(),
        [one] => (*one).into(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal JPEG with `segments` (marker, body) before its scan.
    fn jpeg_with(segments: &[(u8, &[u8])], after: &[u8]) -> Vec<u8> {
        let mut b = vec![0xFF, 0xD8];
        for (marker, body) in segments {
            b.extend([0xFF, *marker]);
            b.extend(((body.len() + 2) as u16).to_be_bytes());
            b.extend(*body);
        }
        b.extend([
            0xFF, 0xDA, 0, 2, 0x12, 0xFF, 0x00, 0x34, 0xFF, 0xD0, 0x56, 0xFF, 0xD9,
        ]);
        b.extend(after);
        b
    }

    /// A little-endian TIFF block with one directory of `tags`.
    fn tiff_with(tags: &[u16]) -> Vec<u8> {
        let mut b = b"II*\0\x08\0\0\0".to_vec();
        b.extend((tags.len() as u16).to_le_bytes());
        for t in tags {
            b.extend(t.to_le_bytes());
            b.extend([3, 0, 1, 0, 0, 0, 1, 0, 0, 0]);
        }
        b.extend([0; 4]);
        b
    }

    #[test]
    fn jpeg_segments_are_named() {
        assert_eq!(
            metadata(&jpeg_with(
                &[(0xE0, b"JFIF\0\x01\x02\0\0\x01\0\x01\0\0")],
                b""
            )),
            Some(vec![])
        );
        let gps = [b"Exif\0\0".to_vec(), tiff_with(&[0x0112, 0x8825])].concat();
        assert_eq!(
            metadata(&jpeg_with(&[(0xE1, &gps)], b"")),
            Some(vec!["EXIF", "GPS"])
        );
        let turned = [b"Exif\0\0".to_vec(), tiff_with(&[0x0112])].concat();
        assert_eq!(
            metadata(&jpeg_with(
                &[(0xE1, &turned), (0xE2, b"ICC_PROFILE\0\x01\x01")],
                b""
            )),
            Some(vec![])
        );
        let all = jpeg_with(
            &[
                (0xFE, b"note"),
                (0xED, b"Photoshop 3.0\0"),
                (0xE1, b"http://ns.adobe.com/xap/1.0/\0<x/>"),
                (0xEC, b"Ducky"),
            ],
            b"MPF second image",
        );
        assert_eq!(
            metadata(&all),
            Some(vec![
                "XMP",
                "IPTC",
                "comments",
                "other metadata",
                "trailing bytes"
            ])
        );
        assert_eq!(metadata(&jpeg_with(&[], b"\0\0\r\n")), Some(vec![]));
        let cut = jpeg_with(&[(0xFE, b"note")], b"");
        assert_eq!(metadata(&cut[..cut.len() - 4]), None);
        assert_eq!(metadata(&[0xFF, 0xD8, 0xFF, 0xE1, 0, 40, 1]), None);
    }

    #[test]
    fn png_chunks_are_named_and_checked() {
        let png = |chunks: &[Vec<u8>]| [b"\x89PNG\r\n\x1a\n".to_vec(), chunks.concat()].concat();
        let ihdr = chunk(b"IHDR", &[0; 13]);
        let end = chunk(b"IEND", b"");
        assert_eq!(
            metadata(&png(&[ihdr.clone(), chunk(b"pHYs", &[0; 9]), end.clone()])),
            Some(vec![])
        );
        let leaky = png(&[
            ihdr.clone(),
            chunk(b"tEXt", b"Author\0Avery"),
            chunk(b"tIME", &[0; 7]),
            end.clone(),
            b"zip".to_vec(),
        ]);
        assert_eq!(
            metadata(&leaky),
            Some(vec!["text chunks", "other metadata", "trailing bytes"])
        );
        let mut bad = png(&[ihdr, end]);
        bad[20] ^= 1;
        assert_eq!(metadata(&bad), None);
    }

    #[test]
    fn gif_webp_and_tiff_are_named() {
        let mut gif = b"GIF89a\x01\0\x01\0\x80\0\0".to_vec();
        gif.extend([0; 6]);
        gif.extend(b"\x21\xFE\x04note\0");
        gif.extend(b"\x21\xFF\x0bNETSCAPE2.0\x03\x01\0\0\0");
        gif.extend(b"\x2C\0\0\0\0\x01\0\x01\0\0\x02\x02\x4c\x01\0\x3B");
        assert_eq!(metadata(&gif), Some(vec!["comments"]));
        assert_eq!(metadata(&gif[..gif.len() - 3]), None);

        let mut webp = b"RIFF\0\0\0\0WEBPVP8 \x01\0\0\0\0\0".to_vec();
        webp.extend(b"XMP \x03\0\0\0<x>\0");
        assert_eq!(metadata(&webp), Some(vec!["XMP"]));

        assert_eq!(
            metadata(&tiff_with(&[256, 257, 0x8825, 315])),
            Some(vec!["GPS", "other metadata"])
        );
        assert_eq!(metadata(&tiff_with(&[256, 257])), Some(vec![]));
        assert_eq!(metadata(b"\x01\x00\x09\x00 EMF"), Some(vec![]));
    }

    #[test]
    fn labels_join_like_a_sentence() {
        assert_eq!(joined(&["EXIF"]), "EXIF");
        assert_eq!(joined(&["EXIF", "GPS"]), "EXIF and GPS");
        assert_eq!(joined(&["EXIF", "GPS", "XMP"]), "EXIF, GPS and XMP");
    }

    /// An EXIF block whose one directory holds `entries` as (tag, short value).
    fn exif_with(entries: &[(u16, u16)]) -> Vec<u8> {
        let mut b = b"Exif\0\0II*\0\x08\0\0\0".to_vec();
        b.extend((entries.len() as u16).to_le_bytes());
        for (tag, v) in entries {
            b.extend(tag.to_le_bytes());
            b.extend([3, 0, 1, 0, 0, 0]);
            b.extend(v.to_le_bytes());
            b.extend([0, 0]);
        }
        b.extend([0; 4]);
        b
    }

    #[test]
    fn jpeg_metadata_goes_and_the_scan_stays_byte_for_byte() {
        let leaky = jpeg_with(
            &[
                (0xE0, b"JFIF\0\x01\x02\0\0\x01\0\x01\x01\x01\x10\x20\x30"),
                (0xE1, &exif_with(&[(0x0112, 6), (0x8825, 1)])),
                (0xE1, b"http://ns.adobe.com/xap/1.0/\0<x/>"),
                (0xE2, b"ICC_PROFILE\0\x01\x01icc"),
                (0xEE, b"Adobe\0\x64"),
                (0xFE, b"note"),
            ],
            b"more",
        );
        let scan = &leaky[leaky.len() - 17..leaky.len() - 4];
        let office = strip(&leaky, true).unwrap();
        assert_eq!(metadata(&office), Some(vec![]));
        assert_eq!(orientation(&office), Some(6));
        assert!(office.ends_with(scan), "{office:x?}");
        let icc = b"ICC_PROFILE\0\x01\x01icc";
        assert!(office.windows(icc.len()).any(|w| w == icc));
        assert!(office.windows(6).any(|w| w == b"Adobe\0"));
        assert!(office.windows(4).all(|w| w != b"note"));
        let pdf = strip(&leaky, false).unwrap();
        assert_eq!(metadata(&pdf), Some(vec![]));
        assert_eq!(orientation(&pdf), Some(1));
        assert_eq!(pdf.len(), office.len() - 36);
        // An orientation-only block is no metadata: Office keeps it, a PDF copy doesn't.
        let turned = jpeg_with(&[(0xE1, &exif_with(&[(0x0112, 3)]))], b"");
        assert_eq!(strip(&turned, true).unwrap(), turned);
        assert_eq!(orientation(&strip(&turned, false).unwrap()), Some(1));
        let cut = jpeg_with(&[(0xFE, b"note")], b"");
        assert_eq!(strip(&cut[..cut.len() - 4], true), None);
    }

    #[test]
    fn png_gif_and_webp_metadata_goes() {
        let png = |chunks: &[Vec<u8>]| [PNG.to_vec(), chunks.concat()].concat();
        let idat = chunk(b"IDAT", b"pixels");
        let kept = [
            chunk(b"IHDR", &[0; 13]),
            chunk(b"iCCP", b"icc"),
            idat.clone(),
            chunk(b"IEND", b""),
        ];
        let leaky = png(&[
            kept[0].clone(),
            chunk(b"tEXt", b"Author\0Avery"),
            chunk(b"eXIf", &exif_with(&[(0x8825, 1)])[6..]),
            kept[1].clone(),
            chunk(b"tIME", &[0; 7]),
            kept[2].clone(),
            kept[3].clone(),
            b"zip".to_vec(),
        ]);
        assert_eq!(strip(&leaky, true).unwrap(), png(&kept));

        let mut gif = b"GIF89a\x01\0\x01\0\x80\0\0".to_vec();
        gif.extend([0; 6]);
        let netscape = b"\x21\xFF\x0bNETSCAPE2.0\x03\x01\0\0\0";
        let image = b"\x2C\0\0\0\0\x01\0\x01\0\0\x02\x02\x4c\x01\0";
        let clean = [gif.as_slice(), netscape, image, b"\x3B"].concat();
        let leaky = [
            gif.as_slice(),
            b"\x21\xFE\x04note\0",
            netscape,
            b"\x21\xFF\x0bXMP DataXMP\x01x\0",
            image,
            b"\x3Btail",
        ]
        .concat();
        assert_eq!(strip(&leaky, true).unwrap(), clean);

        let mut webp = b"RIFF\0\0\0\0WEBPVP8X\x0a\0\0\0\x2c\0\0\0\0\0\0\0\0\0".to_vec();
        webp.extend(b"VP8 \x01\0\0\0\x07\0");
        webp.extend(b"EXIF");
        let exif = exif_with(&[(0x0112, 8), (0x010F, 1)]);
        webp.extend((exif.len() as u32).to_le_bytes());
        webp.extend(&exif);
        webp.extend(b"XMP \x03\0\0\0<x>\0");
        let out = strip(&webp, true).unwrap();
        assert_eq!(metadata(&out), Some(vec![]));
        assert_eq!(&out[4..8], &((out.len() - 8) as u32).to_le_bytes());
        // E stays for the orientation written back; X goes.
        assert_eq!(out[20], 0x28);
        assert!(out.windows(10).any(|w| w == b"VP8 \x01\0\0\0\x07\0"));
        assert_eq!(strip(&webp, false).unwrap()[20], 0x20);
    }

    #[test]
    fn tiff_and_unknown_pictures() {
        assert_eq!(strip(&tiff_with(&[256, 0x8825]), true), None);
        let emf = b"\x01\x00\x09\x00 EMF".to_vec();
        assert_eq!(strip(&emf, true), Some(emf));
    }

    #[test]
    fn svg_metadata_goes_and_titles_stay() {
        let svg_text = format!(
            "<?xml version=\"1.0\"?><!-- Created with Inkscape --><svg xmlns=\"{SVG}\" xmlns:inkscape=\"{}\" inkscape:version=\"1.3\"><title>Logo</title><!-- by Avery --><metadata><x/></metadata><inkscape:grid/><rect width=\"1\"/></svg>",
            EDITORS[1]
        );
        assert_eq!(
            svg(&svg_text, "a.svg").unwrap(),
            ["XMP", "comments", "other metadata"]
        );
        let out = strip_svg(svg_text.as_bytes(), "a.svg").unwrap();
        let out = String::from_utf8(out).unwrap();
        assert_eq!(
            out,
            format!(
                "<?xml version=\"1.0\"?><svg xmlns=\"{SVG}\"><title>Logo</title><rect width=\"1\"/></svg>"
            )
        );
        assert_eq!(svg(&out, "a.svg").unwrap(), Vec::<&str>::new());
    }
}
