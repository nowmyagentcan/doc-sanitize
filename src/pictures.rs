//! What pictures carry besides their pixels: EXIF (with GPS), XMP, IPTC, comments, text
//! chunks, thumbnails and bytes after the image. The walkers read each format's own
//! structure and never decode pixels.

use quick_xml::events::Event;

use crate::office::xml::Reader;
use crate::report::Error;

/// What a picture holds, in the words a detail uses.
pub type Labels = Vec<&'static str>;

/// The picture's metadata labels, or None when its structure is cut short or broken. Formats
/// other than JPEG, PNG, GIF, WebP, SVG and TIFF give none.
pub fn metadata(b: &[u8]) -> Option<Labels> {
    walk(b).ok()
}

fn walk(b: &[u8]) -> Result<Labels, ()> {
    let mut labels = if b.starts_with(&[0xFF, 0xD8]) {
        jpeg(b)?
    } else if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        png(b)?
    } else if b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a") {
        gif(b)?
    } else if b.len() >= 12 && &b[..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        webp(b)?
    } else if b.starts_with(b"II*\0") || b.starts_with(b"MM\0*") {
        tiff(b)?
    } else {
        vec![]
    };
    labels.sort_unstable_by_key(|l| ORDER.iter().position(|o| o == l));
    labels.dedup();
    Ok(labels)
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

fn jpeg(b: &[u8]) -> Result<Labels, ()> {
    let mut labels = vec![];
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
                if trailing(&b[i..]) {
                    labels.push("trailing bytes");
                }
                return Ok(labels);
            }
            0xD0..=0xD7 | 0x01 | 0xD8 => continue,
            _ => {}
        }
        let len = be16(b, i)?;
        let seg = b.get(i + 2..i + len).filter(|_| len >= 2).ok_or(())?;
        i += len;
        match marker {
            0xE0 if seg.starts_with(b"JFIF\0") => {
                if seg.get(12..14).is_some_and(|t| t != [0, 0]) {
                    labels.push("a thumbnail");
                }
            }
            0xE0 if seg.starts_with(b"JFXX\0") => labels.push("a thumbnail"),
            0xE1 if seg.starts_with(b"Exif\0\0") => labels.extend(exif(&seg[6..])?),
            0xE1 if seg.starts_with(b"http://ns.adobe.com/xap/1.0/\0")
                || seg.starts_with(b"http://ns.adobe.com/xmp/extension/\0") =>
            {
                labels.push("XMP")
            }
            0xE2 if seg.starts_with(b"ICC_PROFILE\0") => {}
            0xED => labels.push("IPTC"),
            0xEE if seg.starts_with(b"Adobe") => {}
            0xE0..=0xEF => labels.push("other metadata"),
            0xFE => labels.push("comments"),
            0xDA => {
                // Entropy-coded data runs to the next marker that isn't a stuffed byte or a
                // restart.
                loop {
                    let (c, next) = (*b.get(i).ok_or(())?, *b.get(i + 1).ok_or(())?);
                    if c == 0xFF && next != 0 && !(0xD0..=0xD7).contains(&next) {
                        break;
                    }
                    i += 1;
                }
            }
            _ => {}
        }
    }
}

/// What an EXIF block (a TIFF structure) holds: nothing when it is only the orientation.
fn exif(b: &[u8]) -> Result<Labels, ()> {
    let tags = ifd0(b)?;
    if tags == [0x0112] {
        return Ok(vec![]);
    }
    let mut labels = vec!["EXIF"];
    if tags.contains(&0x8825) {
        labels.push("GPS");
    }
    Ok(labels)
}

/// The tags of a TIFF structure's first directory, and an empty list when another
/// directory follows it (so a lone orientation tag is only that when nothing follows).
fn ifd0(b: &[u8]) -> Result<Vec<u16>, ()> {
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
    let tags = (0..n)
        .map(|k| u16_at(start + 2 + 12 * k))
        .collect::<Result<Vec<_>, _>>()?;
    let next = u32_at(start + 2 + 12 * n)?;
    Ok(if next == 0 {
        tags
    } else {
        [tags, vec![0]].concat()
    })
}

/// TIFF tags that describe the picture's origin rather than its pixels.
fn tiff(b: &[u8]) -> Result<Labels, ()> {
    let mut labels = vec![];
    for tag in ifd0(b)? {
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

const PNG_KEPT: [&[u8; 4]; 19] = [
    b"IHDR", b"PLTE", b"IDAT", b"IEND", b"tRNS", b"iCCP", b"sRGB", b"gAMA", b"cHRM", b"sBIT",
    b"cICP", b"mDCV", b"cLLI", b"pHYs", b"bKGD", b"sPLT", b"acTL", b"fcTL", b"fdAT",
];

fn png(b: &[u8]) -> Result<Labels, ()> {
    let mut labels = vec![];
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
        i += 12 + len;
        match kind {
            b"IEND" => {
                if trailing(&b[i..]) {
                    labels.push("trailing bytes");
                }
                return Ok(labels);
            }
            b"tEXt" | b"zTXt" | b"iTXt" => labels.push("text chunks"),
            b"eXIf" => labels.extend(exif(data)?),
            k if PNG_KEPT.iter().any(|p| p == &k) => {}
            k if k[0].is_ascii_lowercase() => labels.push("other metadata"),
            _ => {}
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

fn gif(b: &[u8]) -> Result<Labels, ()> {
    let mut labels = vec![];
    let table = |flags: u8| {
        if flags & 0x80 != 0 {
            3 << ((flags & 7) + 1)
        } else {
            0
        }
    };
    let mut i = 13 + table(*b.get(10).ok_or(())?);
    loop {
        match *b.get(i).ok_or(())? {
            0x3B => {
                if trailing(&b[i + 1..]) {
                    labels.push("trailing bytes");
                }
                return Ok(labels);
            }
            0x21 => {
                let label = *b.get(i + 1).ok_or(())?;
                let (first, next) = sub_blocks(b, i + 2)?;
                match label {
                    0xFE => labels.push("comments"),
                    0xFF if first.starts_with(b"XMP DataXMP") => labels.push("XMP"),
                    0xFF if first.starts_with(b"NETSCAPE2.0")
                        || first.starts_with(b"ANIMEXTS1.0")
                        || first.starts_with(b"ICCRGBG1") => {}
                    0xFF => labels.push("other metadata"),
                    _ => {}
                }
                i = next;
            }
            0x2C => {
                let flags = *b.get(i + 9).ok_or(())?;
                i = sub_blocks(b, i + 11 + table(flags))?.1;
            }
            _ => return Err(()),
        }
    }
}

fn webp(b: &[u8]) -> Result<Labels, ()> {
    let mut labels = vec![];
    let mut i = 12;
    while i < b.len() {
        let kind = b.get(i..i + 4).ok_or(())?;
        let quad: [u8; 4] = b.get(i + 4..i + 8).ok_or(())?.try_into().map_err(drop)?;
        let len = u32::from_le_bytes(quad) as usize;
        let data = b.get(i + 8..i + 8 + len).ok_or(())?;
        match kind {
            b"EXIF" => labels.extend(exif(data.strip_prefix(b"Exif\0\0").unwrap_or(data))?),
            b"XMP " => labels.push("XMP"),
            _ => {}
        }
        i += 8 + len + len % 2;
    }
    Ok(labels)
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
    labels.sort_unstable_by_key(|l| ORDER.iter().position(|o| o == l));
    labels.dedup();
    Ok(labels)
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

    fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut crc = flate2::Crc::new();
        crc.update(kind);
        crc.update(data);
        [
            (data.len() as u32).to_be_bytes().to_vec(),
            kind.to_vec(),
            data.to_vec(),
            crc.sum().to_be_bytes().to_vec(),
        ]
        .concat()
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
}
