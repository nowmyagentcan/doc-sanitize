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

/// A PDF whose structure leaks: properties, XMP, comments, attachments, scripts, actions,
/// links to local files, signatures, form data and a JPEG with EXIF and GPS. Every planted
/// value starts with `SECRET-pdf-`.
pub fn leaky_structure() -> Vec<u8> {
    let mut b = PdfBuilder::new();
    let xmp = b.stream(
        dictionary! { "Type" => "Metadata", "Subtype" => "XML" },
        b"<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"><rdf:Description xmlns:dc=\"http://purl.org/dc/elements/1.1/\" xmlns:xmp=\"http://ns.adobe.com/xap/1.0/\" xmp:CreatorTool=\"SECRET-pdf-tool-1\"><dc:creator><rdf:Seq><rdf:li>SECRET-pdf-xmp-1</rdf:li></rdf:Seq></dc:creator></rdf:Description></rdf:RDF></x:xmpmeta>",
    );
    let jpeg = super::jpeg(&[0x010F, 0x8825], b"");
    let image = b.stream(
        dictionary! {
            "Type" => "XObject", "Subtype" => "Image", "Width" => 1, "Height" => 1,
            "ColorSpace" => "DeviceGray", "BitsPerComponent" => 8, "Filter" => "DCTDecode",
            "Metadata" => xmp,
        },
        &jpeg,
    );
    let file = b.stream(
        dictionary! { "Type" => "EmbeddedFile" },
        b"SECRET-pdf-file-1",
    );
    let spec = |b: &mut PdfBuilder, name: &str| {
        b.add(dictionary! {
            "Type" => "Filespec", "F" => Object::string_literal(name),
            "EF" => dictionary! { "F" => file },
        })
    };
    let attached = spec(&mut b, "SECRET-pdf-attached-1.txt");
    let on_note = spec(&mut b, "SECRET-pdf-note-file-1.txt");
    let script = b.add(dictionary! {
        "S" => "JavaScript", "JS" => Object::string_literal("app.alert('SECRET-pdf-js-1')"),
    });
    let launch = b.add(dictionary! {
        "S" => "Launch", "F" => Object::string_literal("SECRET-pdf-launch-1.exe"),
    });
    let widget = b.add(dictionary! {
        "Type" => "Annot", "Subtype" => "Widget", "Rect" => vec![0.into(), 0.into(), 10.into(), 10.into()],
        "F" => 2, "FT" => "Tx", "T" => Object::string_literal("Notes"),
        "V" => Object::string_literal("SECRET-pdf-field-1"),
    });
    let sig = b.add(dictionary! {
        "FT" => "Sig", "T" => Object::string_literal("Signature1"),
        "V" => dictionary! { "Type" => "Sig", "Contents" => Object::string_literal("0") },
    });
    let link = |uri: &str| -> Object {
        dictionary! {
            "Type" => "Annot", "Subtype" => "Link", "Rect" => vec![0.into(), 0.into(), 9.into(), 9.into()],
            "A" => dictionary! { "S" => "URI", "URI" => Object::string_literal(uri) },
        }
        .into()
    };
    let annots: Vec<Object> = vec![
        dictionary! {
            "Type" => "Annot", "Subtype" => "Text", "Rect" => vec![0.into(), 0.into(), 9.into(), 9.into()],
            "T" => Object::string_literal("Sam Sample"), "Contents" => Object::string_literal("SECRET-pdf-comment-1"),
        }
        .into(),
        dictionary! {
            "Type" => "Annot", "Subtype" => "FileAttachment", "Rect" => vec![0.into(), 0.into(), 9.into(), 9.into()],
            "FS" => on_note,
        }
        .into(),
        dictionary! {
            "Type" => "Annot", "Subtype" => "Link", "Rect" => vec![0.into(), 0.into(), 9.into(), 9.into()],
            "A" => dictionary! { "S" => "GoToR", "F" => Object::string_literal(r"D:\Projects\avery\SECRET-pdf-goto-1.pdf"), "D" => vec![0.into()] },
        }
        .into(),
        link("file:///home/avery/SECRET-pdf-uri-1.txt"),
        link("https://example.com/visible"),
        widget.into(),
    ];
    b.page_with(
        &text_at(72, 720, "Visible text"),
        dictionary! {
            "Annots" => annots, "PieceInfo" => dictionary! { "App" => dictionary! { "Private" => Object::string_literal("SECRET-pdf-piece-1") } },
            "Thumb" => image,
            "Resources" => dictionary! { "XObject" => dictionary! { "Im1" => image } },
        },
    );
    let info = b.add(dictionary! {
        "Author" => Object::string_literal("Avery Example"),
        "Title" => Object::string_literal("SECRET-pdf-title-1"),
    });
    b.catalog = dictionary! {
        "Metadata" => xmp,
        "OpenAction" => launch,
        "Names" => dictionary! {
            "EmbeddedFiles" => dictionary! { "Names" => vec![Object::string_literal("a.txt"), attached.into()] },
            "JavaScript" => dictionary! { "Names" => vec![Object::string_literal("init"), script.into()] },
        },
        "AcroForm" => dictionary! {
            "Fields" => vec![widget.into(), sig.into()],
            "XFA" => vec![Object::string_literal("template"), Object::string_literal("<template/>")],
        },
        "Perms" => dictionary! { "UR3" => dictionary! { "Type" => "Sig" } },
        "AF" => vec![attached.into()],
        "Collection" => dictionary! { "Type" => "Collection" },
    };
    let mut doc = b.finish();
    doc.trailer.set("Info", info);
    save(&mut doc)
}

/// The no-leak twin of [`leaky_structure`]: visible text, a web link, a visible field value
/// and a JPEG without metadata.
pub fn plain_structure() -> Vec<u8> {
    let mut b = PdfBuilder::new();
    let image = b.stream(
        dictionary! {
            "Type" => "XObject", "Subtype" => "Image", "Width" => 8, "Height" => 8,
            "ColorSpace" => "DeviceRGB", "BitsPerComponent" => 8, "Filter" => "DCTDecode",
        },
        &super::photo(8, 8),
    );
    let widget = b.add(dictionary! {
        "Type" => "Annot", "Subtype" => "Widget", "Rect" => vec![0.into(), 0.into(), 10.into(), 10.into()],
        "FT" => "Tx", "T" => Object::string_literal("Name"), "V" => Object::string_literal("Shown on the page"),
    });
    let link: Object = dictionary! {
        "Type" => "Annot", "Subtype" => "Link", "Rect" => vec![0.into(), 0.into(), 9.into(), 9.into()],
        "A" => dictionary! { "S" => "URI", "URI" => Object::string_literal("https://example.com/") },
    }
    .into();
    b.page_with(
        &format!(
            "{}q 10 0 0 10 72 600 cm /Im1 Do Q\n",
            text_at(72, 720, "Visible text")
        ),
        dictionary! {
            "Annots" => vec![link, widget.into()],
            "Resources" => dictionary! { "XObject" => dictionary! { "Im1" => image } },
        },
    );
    b.catalog = dictionary! { "AcroForm" => dictionary! { "Fields" => vec![widget.into()] } };
    b.bytes()
}

/// A 1×1 grey image XObject (0 black, 255 white), stretched by the page that draws it.
pub fn grey_image(b: &mut PdfBuilder, level: u8) -> ObjectId {
    b.stream(
        dictionary! {
            "Type" => "XObject", "Subtype" => "Image", "Width" => 1, "Height" => 1,
            "ColorSpace" => "DeviceGray", "BitsPerComponent" => 8,
        },
        &[level],
    )
}

/// The case pages of redacted.pdf, in order, as (name, expected kind or "").
pub const REDACTED_CASES: [(&str, &str); 15] = [
    ("black box", "text_under_box"),
    ("black square annotation", "text_under_box"),
    ("black box with a label", "text_under_box"),
    ("white box", "text_under_box"),
    ("black image", "text_under_image"),
    ("white text", "same_colour"),
    ("black on black", "same_colour"),
    ("invisible text", "invisible"),
    ("invisible text under a box on a scan", "text_under_box"),
    ("outside the page", "outside_page"),
    ("zero-area clip", "clipped"),
    ("half-point text", "tiny"),
    ("unapplied redaction", "unapplied_redaction"),
    ("hidden layer", "hidden_layer"),
    ("real redaction", ""),
];

/// One page per redaction case, each hiding `SECRET-pdf-case-N` (N = page) at (72, 700)
/// beside a visible line at (72, 600); the last page is a real redaction with no text under
/// its box.
pub fn redacted() -> Vec<u8> {
    let mut b = PdfBuilder::new();
    let black = grey_image(&mut b, 0);
    let scan = grey_image(&mut b, 230);
    let layer = b.add(dictionary! { "Type" => "OCG", "Name" => Object::string_literal("Draft") });
    let rect: Object = vec![70.into(), 695.into(), 300.into(), 715.into()].into();
    for (i, _) in REDACTED_CASES.iter().enumerate() {
        let page = i + 1;
        let secret = format!("SECRET-pdf-case-{page}");
        let hidden = text_at(72, 700, &secret);
        let visible = text_at(72, 600, "A visible line of ordinary text, Illicit 1111.");
        let resources = match page {
            5 => dictionary! { "XObject" => dictionary! { "Black" => black } },
            9 => dictionary! { "XObject" => dictionary! { "Scan" => scan } },
            14 => dictionary! { "Properties" => dictionary! { "draft" => layer } },
            _ => Dictionary::new(),
        };
        let mut extra = dictionary! { "Resources" => resources };
        let box_ = "70 695 230 20 re f\n";
        let content = match page {
            1 => format!("{hidden}0 g {box_}"),
            2 => {
                let ap = b.stream(
                    dictionary! { "Type" => "XObject", "Subtype" => "Form", "BBox" => vec![0.into(), 0.into(), 230.into(), 20.into()] },
                    b"0 g 0 0 230 20 re f",
                );
                extra.set(
                    "Annots",
                    vec![Object::from(dictionary! {
                        "Type" => "Annot", "Subtype" => "Square", "Rect" => rect.clone(),
                        "IC" => vec![0.into(), 0.into(), 0.into()],
                        "AP" => dictionary! { "N" => ap },
                    })],
                );
                hidden
            }
            3 => format!(
                "{hidden}0 g {box_}1 g {}0 g\n",
                text_at(80, 700, "REDACTED")
            ),
            4 => format!("{hidden}1 g {box_}0 g\n"),
            5 => format!("{hidden}q 230 0 0 20 70 695 cm /Black Do Q\n"),
            6 => format!("1 g {hidden}0 g\n"),
            7 => format!("0 g {box_}{hidden}"),
            8 => format!("BT 3 Tr /F1 12 Tf 72 700 Td ({secret}) Tj 0 Tr ET\n"),
            9 => format!(
                "q 595 0 0 842 0 0 cm /Scan Do Q BT 3 Tr /F1 12 Tf 72 700 Td ({secret}) Tj 0 Tr ET 0 g {box_}"
            ),
            10 => text_at(-500, 700, &secret),
            11 => format!("q 0 0 0 0 re W n {hidden}Q\n"),
            12 => format!("BT /F1 0.5 Tf 72 700 Td ({secret}) Tj 0 Tr ET\n"),
            13 => {
                let quads: Vec<Object> = [70, 715, 300, 715, 70, 695, 300, 695]
                    .map(Object::from)
                    .to_vec();
                extra.set(
                    "Annots",
                    vec![Object::from(dictionary! {
                        "Type" => "Annot", "Subtype" => "Redact", "Rect" => rect.clone(), "QuadPoints" => quads,
                    })],
                );
                hidden
            }
            14 => format!("/OC /draft BDC\n{hidden}EMC\n"),
            // Big strokes fill their own ink boxes, yet they stand out from the page.
            _ => format!("0 g {box_}BT /F1 60 Tf 72 300 Td (Ill|-_) Tj ET\n"),
        };
        b.page_with(&format!("{content}{visible}"), extra);
    }
    b.catalog = dictionary! {
        "OCProperties" => dictionary! {
            "OCGs" => vec![layer.into()],
            "D" => dictionary! { "OFF" => vec![layer.into()] },
        },
    };
    b.bytes()
}

/// `pages` A4 pages of 58 lines of Helvetica 10 pt, about 95 characters each; the first
/// `boxed` pages have a black box over two lines.
pub fn book(pages: usize, boxed: usize) -> Vec<u8> {
    const WORDS: [&str; 12] = [
        "lorem",
        "ipsum",
        "dolor",
        "sit",
        "amet",
        "consectetur",
        "adipiscing",
        "elit",
        "sed",
        "eiusmod",
        "tempor",
        "labore",
    ];
    let mut b = PdfBuilder::new();
    let mut w = 0;
    for p in 0..pages {
        let mut content = String::from("BT /F1 10 Tf 13 TL 56 790 Td\n");
        for _ in 0..58 {
            let mut line = String::new();
            while line.len() < 95 {
                line += WORDS[w % WORDS.len()];
                line.push(' ');
                w += 7;
            }
            content += &format!("({line}) '\n");
        }
        content += "ET\n";
        if p < boxed {
            content += "0 g 100 600 250 22 re f\n";
        }
        b.page(&content);
    }
    let mut doc = b.finish();
    doc.compress();
    save(&mut doc)
}
