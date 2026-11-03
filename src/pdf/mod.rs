//! PDF files: opening them with lopdf under limits on objects, pages and decoded streams.

use std::collections::HashSet;
use std::io::{self, Write};

use flate2::write::{DeflateDecoder, ZlibDecoder};
use lopdf::{Dictionary, Document, LoadOptions, Object, ObjectId};

use crate::report::Error;

/// The most any one stream may decode to.
pub const STREAM_LIMIT: usize = 256 << 20;
const MAX_OBJECTS: usize = 1_000_000;
const MAX_PAGES: usize = 10_000;

/// An opened, decrypted PDF.
pub struct Pdf {
    pub doc: Document,
    /// The page objects in page order.
    pub pages: Vec<ObjectId>,
    /// The password that opened it; empty when it opened without one.
    pub password: String,
    /// lopdf rebuilt its cross-reference table.
    pub repaired: bool,
}

/// Opens `bytes`: the empty password first, then `password`. Every stream must decode within
/// [`STREAM_LIMIT`], so later passes (ours and the renderer's) never meet a bomb.
pub fn open(bytes: &[u8], password: Option<&str>) -> Result<Pdf, Error> {
    let load = |password: Option<&str>| {
        Document::load_mem_with_options(
            bytes,
            LoadOptions {
                password: password.map(Into::into),
                max_decompressed_size: Some(STREAM_LIMIT),
                ..LoadOptions::default()
            },
        )
    };
    let mut doc = load(None).map_err(load_error)?;
    let mut used = String::new();
    if doc.trailer.has(b"Encrypt") {
        handler(&doc)?;
        let password = password.ok_or_else(Error::pdf_password)?;
        doc = load(Some(password)).map_err(|e| match e {
            lopdf::Error::InvalidPassword => Error::wrong_password(),
            e => load_error(e),
        })?;
        if doc.trailer.has(b"Encrypt") {
            return Err(Error::wrong_password());
        }
        used = password.into();
    }
    if doc.objects.len() > MAX_OBJECTS {
        return Err(Error::too_large("its object count", "1,000,000"));
    }
    for object in doc.objects.values() {
        if let Object::Stream(s) = object {
            fits(s)?;
        }
    }
    Ok(Pdf {
        pages: pages(&doc)?,
        repaired: doc.xref_recovery.is_some(),
        password: used,
        doc,
    })
}

fn load_error(e: lopdf::Error) -> Error {
    match e {
        lopdf::Error::Decompress(lopdf::DecompressError::MemoryLimitExceeded { .. }) => {
            too_large_stream()
        }
        _ => Error::damaged("its structure can't be read"),
    }
}

fn too_large_stream() -> Error {
    Error::too_large("a stream once decoded", "256 MiB")
}

/// `unsupported_encryption` unless the file uses the standard handler in a version lopdf reads.
fn handler(doc: &Document) -> Result<(), Error> {
    let dict = doc.get_encrypted().map_err(load_error)?;
    let int = |key: &[u8]| dict.get(key).and_then(Object::as_i64).unwrap_or(0);
    let filter = name(dict, b"Filter").unwrap_or_default();
    if filter != "Standard" {
        return Err(Error::unsupported_encryption(&format!("/{filter}")));
    }
    let (v, r) = (int(b"V"), int(b"R"));
    if !matches!(v, 1 | 2 | 4 | 5) || !(2..=6).contains(&r) {
        return Err(Error::unsupported_encryption(&format!(
            "/Standard V {v} R {r}"
        )));
    }
    Ok(())
}

/// `Ok` when `s` decodes within the limit, or can't be decoded by lopdf at all (images and
/// unknown filters are left to the renderer, which then reports the page). FlateDecode alone,
/// the usual bomb, is counted into a sink so a bomb never holds 256 MiB.
fn fits(s: &lopdf::Stream) -> Result<(), Error> {
    let filters = s.filters().unwrap_or_default();
    if filters == [b"FlateDecode"] {
        // lopdf falls back to raw deflate when the zlib header is broken, so count both.
        let mut z = ZlibDecoder::new(Counter(0));
        let mut d = DeflateDecoder::new(Counter(0));
        let zlib = z.write_all(&s.content).and_then(|()| z.flush());
        if zlib.is_err() {
            let _ = d.write_all(&s.content).and_then(|()| d.flush());
        }
        return match z.get_ref().0.max(d.get_ref().0) > STREAM_LIMIT {
            true => Err(too_large_stream()),
            false => Ok(()),
        };
    }
    match s.decompressed_content_with_limit(STREAM_LIMIT) {
        Err(lopdf::Error::Decompress(lopdf::DecompressError::MemoryLimitExceeded { .. })) => {
            Err(too_large_stream())
        }
        _ => Ok(()),
    }
}

/// Counts bytes written to it, and refuses them past the limit.
struct Counter(usize);

impl Write for Counter {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.0 += b.len();
        match self.0 > STREAM_LIMIT {
            true => Err(io::ErrorKind::FileTooLarge.into()),
            false => Ok(b.len()),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// The page objects, walking /Kids from the catalog's /Pages in order. A node met twice
/// means the tree loops.
fn pages(doc: &Document) -> Result<Vec<ObjectId>, Error> {
    let broken = || Error::damaged("its structure can't be read");
    let root = doc
        .catalog()
        .ok()
        .and_then(|c| c.get(b"Pages").ok()?.as_reference().ok())
        .ok_or_else(broken)?;
    let mut seen = HashSet::from([root]);
    let mut pending = vec![root];
    let mut pages = vec![];
    while let Some(id) = pending.pop() {
        let Ok(node) = doc.get_dictionary(id) else {
            continue;
        };
        match node.get(b"Kids").and_then(Object::as_array) {
            Ok(kids) => {
                for kid in kids.iter().rev() {
                    let Ok(kid) = kid.as_reference() else {
                        continue;
                    };
                    if !seen.insert(kid) {
                        return Err(Error::damaged("its page tree loops"));
                    }
                    pending.push(kid);
                }
            }
            Err(_) => pages.push(id),
        }
        if pages.len() > MAX_PAGES {
            return Err(Error::too_large("its page count", "10,000"));
        }
    }
    Ok(pages)
}

/// The name under `key`, without its slash.
pub fn name(dict: &Dictionary, key: &[u8]) -> Option<String> {
    let n = dict.get(key).ok()?.as_name().ok()?;
    Some(String::from_utf8_lossy(n).into())
}
