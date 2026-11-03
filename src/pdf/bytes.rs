//! What a PDF's bytes hold beyond what its final revision shows: earlier revisions, object
//! bodies that no cross-reference section lists, objects and resources nothing uses, and
//! bytes outside the file.

use std::collections::HashSet;

use lopdf::xref::XrefEntry;
use lopdf::{Document, LoadOptions, Object, ObjectId};

use super::{Ctx, STREAM_LIMIT, as_dict, n};
use crate::report::{CheckId, Where};

/// At most this many earlier revisions are loaded and compared.
const MAX_COMPARED: usize = 50;

/// old-revisions and leftover-data.
pub(super) fn check(c: &Ctx, bytes: &[u8], found: &mut super::Found) {
    let start = header(bytes);
    let ends = revision_ends(bytes, start);
    let earlier = &ends[..ends.len().saturating_sub(1)];
    let skipped = earlier.len().saturating_sub(MAX_COMPARED);
    let mut tables = vec![table(c.doc())];
    for (i, &end) in earlier.iter().enumerate().skip(skipped) {
        let old = Document::load_mem_with_options(
            &bytes[..end],
            LoadOptions {
                password: Some(c.pdf.password.clone()).filter(|p| !p.is_empty()),
                max_decompressed_size: Some(STREAM_LIMIT),
                ..LoadOptions::default()
            },
        )
        .ok();
        let changed = old.as_ref().map(|old| changed(c.doc(), old));
        let detail = match &changed {
            Some((0, _)) => "1 earlier revision; nothing in it was changed or removed later".into(),
            Some((count, kinds)) => {
                let mut detail = format!(
                    "1 earlier revision; {} changed or removed after it",
                    n(*count, "object was", "objects were")
                );
                if !kinds.is_empty() {
                    detail += &format!(", including {}", crate::pictures::joined(kinds));
                }
                detail
            }
            None => "1 earlier revision, which couldn't be read".into(),
        };
        found.findings.push(c.finding(
            (CheckId::OldRevisions, "earlier_revision"),
            Where {
                revision: Some(i + 1),
                ..Where::default()
            },
            (1, "revisions"),
            detail,
            None,
        ));
        tables.extend(old.as_ref().map(table));
    }
    if skipped > 0 {
        let note = format!("only the last {MAX_COMPARED} revisions were compared");
        found.incomplete.push((CheckId::OldRevisions, note));
    }
    leftovers(c, bytes, start, &tables, found);
}

/// Where the PDF starts: its `%PDF-` header, as lopdf reads it.
fn header(bytes: &[u8]) -> usize {
    bytes.windows(5).position(|w| w == b"%PDF-").unwrap_or(0)
}

/// The end (after `%%EOF`) of each revision in file order: each `startxref` whose offset
/// points at a cross-reference table or stream. A linearized file's first-page section
/// isn't a revision.
fn revision_ends(bytes: &[u8], start: usize) -> Vec<usize> {
    let pdf = &bytes[start..];
    let mut ends = vec![];
    let mut at = 0;
    while let Some(i) = find(&pdf[at..], b"startxref") {
        let after = at + i + 9;
        at = after;
        let digits: Vec<u8> = pdf[after..]
            .iter()
            .skip_while(|b| b.is_ascii_whitespace())
            .take_while(|b| b.is_ascii_digit())
            .copied()
            .collect();
        let Some(offset) = std::str::from_utf8(&digits)
            .ok()
            .and_then(|d| d.parse::<usize>().ok())
        else {
            continue;
        };
        let Some(eof) = find(&pdf[after..], b"%%EOF") else {
            continue;
        };
        let target = pdf.get(offset..).unwrap_or_default();
        if target.starts_with(b"xref") || object_header(target).is_some() {
            ends.push(start + after + eof + 5);
        }
    }
    if linearized(pdf) && ends.len() > 1 {
        ends.remove(0);
    }
    ends
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// `n g obj` at the start of `b`, as (n, g, its length).
fn object_header(b: &[u8]) -> Option<(u32, u16, usize)> {
    let mut i = 0;
    let num = number(b, &mut i)?;
    let generation = u16::try_from(number(b, &mut i)?).ok()?;
    b[i..]
        .starts_with(b"obj")
        .then_some((num, generation, i + 3))
}

/// A number at `i` followed by whitespace, moving `i` past both.
fn number(b: &[u8], i: &mut usize) -> Option<u32> {
    let from = *i;
    while b.get(*i).is_some_and(u8::is_ascii_digit) {
        *i += 1;
    }
    let n = std::str::from_utf8(&b[from..*i]).ok()?.parse().ok();
    let digits = *i;
    while b.get(*i).is_some_and(u8::is_ascii_whitespace) {
        *i += 1;
    }
    n.filter(|_| *i > digits)
}

/// The first object's dictionary holds /Linearized.
fn linearized(pdf: &[u8]) -> bool {
    let head = &pdf[..pdf.len().min(4096)];
    find(head, b" obj").is_some_and(|i| {
        let rest = &head[i..];
        let end = find(rest, b"endobj").unwrap_or(rest.len());
        find(&rest[..end], b"/Linearized").is_some()
    })
}

/// How many objects of `old` the final document changed or removed, and what kinds of
/// content they were.
fn changed(new: &Document, old: &Document) -> (usize, Vec<&'static str>) {
    let contents: HashSet<ObjectId> = old
        .page_iter()
        .flat_map(|p| old.get_page_contents(p))
        .collect();
    let info = old.trailer.get(b"Info").and_then(Object::as_reference).ok();
    let mut kinds = [false; 5];
    let mut count = 0;
    for (id, o) in &old.objects {
        if new.objects.get(id) == Some(o) {
            continue;
        }
        count += 1;
        let d = as_dict(o);
        let is = |key: &[u8], value: &[u8]| {
            d.and_then(|d| d.get(key).ok()?.as_name().ok()) == Some(value)
        };
        kinds[0] |= contents.contains(id) || is(b"Type", b"Page");
        kinds[1] |= Some(*id) == info || is(b"Type", b"Metadata");
        kinds[2] |= is(b"Type", b"Annot");
        kinds[3] |= is(b"Type", b"Font") || is(b"Type", b"FontDescriptor");
        kinds[4] |= is(b"Subtype", b"Image");
    }
    let names = [
        "page content",
        "document properties",
        "annotations",
        "fonts",
        "images",
    ];
    let kinds = names
        .into_iter()
        .zip(kinds)
        .filter(|k| k.1)
        .map(|k| k.0)
        .collect();
    (count, kinds)
}

/// The (number, offset) of each object a document's cross-reference data places in the
/// file.
fn table(doc: &Document) -> Vec<(u32, usize)> {
    doc.reference_table
        .entries
        .iter()
        .filter_map(|(&n, e)| match e {
            XrefEntry::Normal { offset, .. } => Some((n, *offset as usize)),
            _ => None,
        })
        .collect()
}

fn leftovers(
    c: &Ctx,
    bytes: &[u8],
    start: usize,
    tables: &[Vec<(u32, usize)>],
    found: &mut super::Found,
) {
    const ID: CheckId = CheckId::LeftoverData;
    let unused = unused_objects(c.doc());
    if unused > 0 {
        found.findings.push(c.finding(
            (ID, "unused_objects"),
            Where::default(),
            (unused, "objects"),
            n(
                unused,
                "object that nothing in the file uses",
                "objects that nothing in the file uses",
            ),
            None,
        ));
    }
    if !c.pdf.repaired {
        let unlisted = bodies(&bytes[start..])
            .into_iter()
            .filter(|&(num, at)| {
                !tables
                    .iter()
                    .flatten()
                    .any(|&(n, offset)| n == num && skip_space(&bytes[start..], offset) == at)
            })
            .count();
        if unlisted > 0 {
            found.findings.push(c.finding(
                (ID, "unlisted_objects"),
                Where::default(),
                (unlisted, "objects"),
                n(
                    unlisted,
                    "object body that no cross-reference table lists",
                    "object bodies that no cross-reference table lists",
                ),
                None,
            ));
        }
    }
    for (page, dict) in c.pages() {
        let unused = super::unused_resources(c, dict);
        if unused > 0 {
            found.findings.push(c.finding(
                (ID, "unused_resources"),
                Where::page(page),
                (unused, "objects"),
                n(
                    unused,
                    "picture or drawing that nothing on the page shows",
                    "pictures or drawings that nothing on the page shows",
                ),
                None,
            ));
        }
    }
    let end = bytes
        .windows(5)
        .rposition(|w| w == b"%%EOF")
        .map_or(bytes.len(), |i| i + 5);
    let blank = |b: &u8| b.is_ascii_whitespace() || *b == 0;
    let tail = &bytes[end..];
    let after = match (
        tail.iter().position(|b| !blank(b)),
        tail.iter().rposition(|b| !blank(b)),
    ) {
        (Some(first), Some(last)) => last + 1 - first,
        _ => 0,
    };
    for (count, what) in [
        (start, "before the PDF starts"),
        (after, "after the PDF ends"),
    ] {
        if count > 0 {
            found.findings.push(c.finding(
                (ID, "data_outside_pdf"),
                Where::default(),
                (count, "bytes"),
                format!("{} {what}", n(count, "byte", "bytes")),
                None,
            ));
        }
    }
}

fn skip_space(pdf: &[u8], mut at: usize) -> usize {
    while pdf.get(at).is_some_and(u8::is_ascii_whitespace) {
        at += 1;
    }
    at
}

/// Every `n g obj` at a line start outside stream data, as (n, offset).
fn bodies(pdf: &[u8]) -> Vec<(u32, usize)> {
    let mut out = vec![];
    let mut i = 0;
    while i < pdf.len() {
        let line_start = i == 0 || matches!(pdf[i - 1], b'\n' | b'\r');
        if line_start && pdf[i].is_ascii_digit() {
            if let Some((num, _, len)) = object_header(&pdf[i..]) {
                out.push((num, i));
                i += len;
                continue;
            }
        } else if pdf[i..].starts_with(b"stream")
            && matches!(pdf.get(i + 6), Some(b'\r' | b'\n'))
            && (i == 0 || !pdf[i - 1].is_ascii_alphabetic())
        {
            i = find(&pdf[i..], b"endstream").map_or(pdf.len(), |e| i + e + 9);
            continue;
        }
        i += 1;
    }
    out
}

/// Objects that nothing reachable from the trailer refers to, leaving out the file's own
/// machinery (cross-reference and object streams, the linearization dictionary and hints).
fn unused_objects(doc: &Document) -> usize {
    let mut seen = HashSet::new();
    let mut pending: Vec<&Object> = doc.trailer.iter().map(|(_, v)| v).collect();
    while let Some(o) = pending.pop() {
        match o {
            Object::Reference(id) => {
                if seen.insert(*id)
                    && let Ok(o) = doc.get_object(*id)
                {
                    pending.push(o);
                }
            }
            Object::Array(a) => pending.extend(a),
            Object::Dictionary(d) => pending.extend(d.iter().map(|(_, v)| v)),
            Object::Stream(s) => pending.extend(s.dict.iter().map(|(_, v)| v)),
            _ => {}
        }
    }
    doc.objects
        .iter()
        .filter(|(id, o)| {
            let d = as_dict(o);
            let machinery = d.is_some_and(|d| {
                d.has(b"Linearized")
                    || d.has_type(b"XRef")
                    || d.has_type(b"ObjStm")
                    || (matches!(o, Object::Stream(_)) && d.has(b"S") && !d.has(b"Type"))
            });
            !seen.contains(id) && !machinery
        })
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn object_headers_need_two_numbers_and_obj() {
        assert_eq!(object_header(b"12 0 obj\n<<>>"), Some((12, 0, 8)));
        assert_eq!(object_header(b"7 0\nobj"), Some((7, 0, 7)));
        assert_eq!(object_header(b"12 obj"), None);
        assert_eq!(object_header(b"120obj"), None);
        assert_eq!(object_header(b"1 99999 obj"), None);
    }

    #[test]
    fn bodies_inside_streams_are_skipped() {
        let pdf = b"%PDF-1.7\n1 0 obj\n<< /Length 9 >>\nstream\n2 0 obj\nendstream\nendobj\n3 0 obj\nnull\nendobj\n";
        assert_eq!(bodies(pdf), [(1, 9), (3, 65)]);
    }

    #[test]
    fn each_startxref_to_a_table_ends_a_revision() {
        let one = b"%PDF-1.7\nxref\n0 1\ntrailer<<>>\nstartxref\n9\n%%EOF\n";
        let mut two = one.to_vec();
        two.extend(b"2 0 obj\n<</Type/XRef>>\nendobj\nstartxref\n48\n%%EOF");
        assert_eq!(revision_ends(one, 0), [47]);
        assert_eq!(revision_ends(&two, 0), [47, two.len()]);
        let mut junk = b"junk".to_vec();
        junk.extend(one);
        junk.extend(b"startxref\n999\n%%EOF\n");
        assert_eq!(revision_ends(&junk, 4), [51]);
    }
}
