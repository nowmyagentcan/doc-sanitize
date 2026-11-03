//! PDFs made at test time: through lopdf for well-formed files, or as raw bytes with a
//! hand-written cross-reference section for broken ones.

use std::io::Write;

use lopdf::{Dictionary, Document, Object, ObjectId, Stream, dictionary};

/// A4, in points.
pub const A4: [i64; 4] = [0, 0, 595, 842];

/// A document being built: pages draw with Helvetica as /F1.
pub struct PdfBuilder {
    pub doc: Document,
    pub catalog: Dictionary,
    pages: ObjectId,
    kids: Vec<Object>,
    font: ObjectId,
}

impl Default for PdfBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl PdfBuilder {
    pub fn new() -> Self {
        let mut doc = Document::with_version("1.7");
        let pages = doc.new_object_id();
        let font = doc.add_object(dictionary! {
            "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica",
        });
        Self {
            doc,
            catalog: Dictionary::new(),
            pages,
            kids: vec![],
            font,
        }
    }

    pub fn add(&mut self, object: impl Into<Object>) -> ObjectId {
        self.doc.add_object(object)
    }

    /// A stream with `content`, unfiltered.
    pub fn stream(&mut self, dict: Dictionary, content: &[u8]) -> ObjectId {
        self.add(Stream::new(dict, content.to_vec()))
    }

    /// An A4 page drawing `content`.
    pub fn page(&mut self, content: &str) -> ObjectId {
        self.page_with(content, Dictionary::new())
    }

    /// An A4 page drawing `content`, with `extra` entries (an /XObject dictionary in its
    /// /Resources merges with the font).
    pub fn page_with(&mut self, content: &str, mut extra: Dictionary) -> ObjectId {
        let contents = self.stream(Dictionary::new(), content.as_bytes());
        let mut resources = dictionary! { "Font" => dictionary! { "F1" => self.font } };
        if let Some(Object::Dictionary(more)) = extra.remove(b"Resources") {
            resources.extend(&more);
        }
        let mut page = dictionary! {
            "Type" => "Page",
            "Parent" => self.pages,
            "MediaBox" => A4.map(Object::from).to_vec(),
            "Contents" => contents,
            "Resources" => resources,
        };
        page.extend(&extra);
        let id = self.add(page);
        self.kids.push(id.into());
        id
    }

    /// The document with its page tree and catalog, before saving.
    pub fn finish(mut self) -> Document {
        let count = self.kids.len() as i64;
        let kids = std::mem::take(&mut self.kids);
        self.doc.objects.insert(
            self.pages,
            dictionary! { "Type" => "Pages", "Kids" => kids, "Count" => count }.into(),
        );
        let mut catalog = dictionary! { "Type" => "Catalog", "Pages" => self.pages };
        catalog.extend(&self.catalog);
        let root = self.doc.add_object(catalog);
        self.doc.trailer.set("Root", root);
        self.doc
    }

    pub fn bytes(self) -> Vec<u8> {
        save(&mut self.finish())
    }
}

pub fn save(doc: &mut Document) -> Vec<u8> {
    let mut out = vec![];
    doc.save_to(&mut out).unwrap();
    out
}

/// A one-page PDF drawing `content`.
pub fn one_page(content: &str) -> Vec<u8> {
    let mut b = PdfBuilder::new();
    b.page(content);
    b.bytes()
}

/// `text` shown at (x, y) in 12 pt Helvetica, as content stream operators.
pub fn text_at(x: i32, y: i32, text: &str) -> String {
    format!("BT /F1 12 Tf {x} {y} Td ({text}) Tj ET\n")
}

/// A PDF from raw object bodies (number, body) with a classic cross-reference table and
/// `trailer` entries; numbers missing from the list are free entries.
pub fn raw(objects: &[(u32, Vec<u8>)], trailer: &str) -> Vec<u8> {
    let mut out = b"%PDF-1.7\n".to_vec();
    let at = write_objects(&mut out, objects);
    let size = objects.iter().map(|o| o.0).max().unwrap_or(0) + 1;
    let xref = out.len();
    writeln!(out, "xref\n0 {size}").unwrap();
    for n in 0..size {
        match objects.iter().position(|o| o.0 == n) {
            Some(i) => writeln!(out, "{:010} 00000 n ", at[i]).unwrap(),
            None => writeln!(out, "0000000000 65535 f ").unwrap(),
        }
    }
    writeln!(
        out,
        "trailer\n<< /Size {size} {trailer} >>\nstartxref\n{xref}\n%%EOF"
    )
    .unwrap();
    out
}

/// The catalog, page tree and one page of a raw PDF, with `page` entries on the page.
pub fn raw_page(page: &str) -> Vec<(u32, Vec<u8>)> {
    vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] {page} >>").into(),
        ),
    ]
}

/// A stream object body holding `data`.
pub fn stream_body(dict: &str, data: &[u8]) -> Vec<u8> {
    let mut body = format!("<< {dict} /Length {} >>\nstream\n", data.len()).into_bytes();
    body.extend(data);
    body.extend(b"\nendstream");
    body
}

/// `n` bytes of zeros compressed with zlib.
pub fn zlib_zeros(n: usize) -> Vec<u8> {
    let mut z = flate2::write::ZlibEncoder::new(vec![], flate2::Compression::best());
    let chunk = vec![0u8; 1 << 20];
    let mut left = n;
    while left > 0 {
        let k = left.min(chunk.len());
        z.write_all(&chunk[..k]).unwrap();
        left -= k;
    }
    z.finish().unwrap()
}

/// One entry of a cross-reference stream: free, at an offset, or inside an object stream.
pub enum XrefEntry {
    Free,
    At(usize),
    In(u32, u16),
}

/// The body of cross-reference stream object listing `entries` from object 0, with `dict`
/// entries (such as /Root) added.
pub fn xref_stream(entries: &[XrefEntry], dict: &str) -> Vec<u8> {
    let mut data = vec![];
    for e in entries {
        let (kind, a, b) = match e {
            XrefEntry::Free => (0u8, 0u32, 0u16),
            XrefEntry::At(at) => (1, *at as u32, 0),
            XrefEntry::In(stream, index) => (2, *stream, *index),
        };
        data.push(kind);
        data.extend(a.to_be_bytes());
        data.extend(b.to_be_bytes());
    }
    let dict = format!("/Type /XRef /Size {} /W [1 4 2] {dict}", entries.len());
    stream_body(&dict, &data)
}

/// Writes `n 0 obj … endobj` for each object onto `out`, returning each one's offset.
pub fn write_objects(out: &mut Vec<u8>, objects: &[(u32, Vec<u8>)]) -> Vec<usize> {
    objects
        .iter()
        .map(|(n, body)| {
            let at = out.len();
            writeln!(out, "{n} 0 obj").unwrap();
            out.extend(body);
            out.extend(b"\nendobj\n");
            at
        })
        .collect()
}
