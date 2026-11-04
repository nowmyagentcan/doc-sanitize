//! PDF files: opening them with lopdf under limits on objects, pages and decoded streams.

mod bytes;
pub mod clean;
mod layers;
mod raster;
mod visible;

use std::collections::HashSet;
use std::io::{self, Write};

use flate2::write::{DeflateDecoder, ZlibDecoder};
use lopdf::content::{Content, Operation};
use lopdf::{Dictionary, Document, LoadOptions, Object, ObjectId};

use crate::pictures;
use crate::report::{CheckId, Error, Finding, Where, counted};

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
    /// It had /Encrypt, with or without a user password.
    pub encrypted: bool,
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
    let encrypted = doc.trailer.has(b"Encrypt") || doc.was_encrypted();
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
        encrypted,
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
fn name(dict: &Dictionary, key: &[u8]) -> Option<String> {
    let n = dict.get(key).ok()?.as_name().ok()?;
    Some(String::from_utf8_lossy(n).into())
}

/// The checks every PDF gets, in report order.
pub const CHECKS: [CheckId; 13] = [
    CheckId::Metadata,
    CheckId::Comments,
    CheckId::HiddenText,
    CheckId::FakeRedactions,
    CheckId::HiddenLayers,
    CheckId::EmbeddedFiles,
    CheckId::Macros,
    CheckId::ExternalLinks,
    CheckId::ImageMetadata,
    CheckId::OldRevisions,
    CheckId::LeftoverData,
    CheckId::Signatures,
    CheckId::FormData,
];

/// What the checks found, and why some couldn't look everywhere.
#[derive(Default)]
pub struct Found {
    pub findings: Vec<Finding>,
    pub incomplete: Vec<(CheckId, String)>,
}

/// Runs every PDF check on `pdf`.
pub fn check(pdf: &Pdf, bytes: &[u8], values: bool) -> Found {
    let c = Ctx { pdf, values };
    let mut found = Found::default();
    found.findings.extend(metadata(&c));
    found.findings.extend(comments(&c));
    visible::check(&c, bytes, &mut found);
    found.findings.extend(layers::check(&c));
    found.findings.extend(embedded_files(&c));
    found.findings.extend(actions(&c));
    found.findings.extend(fields(&c));
    image_metadata(&c, &mut found);
    bytes::check(&c, bytes, &mut found);
    found
}

/// The document and how findings are made.
struct Ctx<'a> {
    pdf: &'a Pdf,
    values: bool,
}

impl<'a> Ctx<'a> {
    fn doc(&self) -> &'a Document {
        &self.pdf.doc
    }

    /// `o` with references followed; Null when they lead nowhere.
    fn resolve(&self, o: &'a Object) -> &'a Object {
        self.doc().dereference(o).map_or(&Object::Null, |(_, o)| o)
    }

    fn get(&self, d: &'a Dictionary, key: &[u8]) -> Option<&'a Object> {
        d.get(key).ok().map(|o| self.resolve(o))
    }

    /// The dictionary under `key`, or a stream's dictionary.
    fn dict(&self, d: &'a Dictionary, key: &[u8]) -> Option<&'a Dictionary> {
        self.get(d, key).and_then(as_dict)
    }

    fn array(&self, d: &'a Dictionary, key: &[u8]) -> &'a [Object] {
        match self.get(d, key) {
            Some(Object::Array(a)) => a,
            _ => &[],
        }
    }

    fn catalog(&self) -> Option<&'a Dictionary> {
        self.doc().catalog().ok()
    }

    /// Each page as (number, dictionary).
    fn pages(&self) -> impl Iterator<Item = (usize, &'a Dictionary)> {
        let doc = self.doc();
        self.pdf
            .pages
            .iter()
            .enumerate()
            .filter_map(move |(i, id)| Some((i + 1, doc.get_dictionary(*id).ok()?)))
    }

    /// A page's annotations as (object, dictionary).
    fn annots(&self, page: &'a Dictionary) -> Vec<(Option<ObjectId>, &'a Dictionary)> {
        self.array(page, b"Annots")
            .iter()
            .filter_map(|a| Some((a.as_reference().ok(), as_dict(self.resolve(a))?)))
            .collect()
    }

    /// Text for a value: a text string, a name, a file specification's file name, or a
    /// stream's bytes.
    fn text(&self, o: &'a Object) -> Option<String> {
        match self.resolve(o) {
            o @ Object::String(..) => lopdf::decode_text_string(o).ok(),
            Object::Name(n) => Some(String::from_utf8_lossy(n).into()),
            Object::Dictionary(d) => [b"UF".as_slice(), b"F", b"URL"]
                .iter()
                .find_map(|k| self.text(d.get(k).ok()?)),
            Object::Stream(s) => s
                .get_plain_content_with_limit(STREAM_LIMIT)
                .ok()
                .map(|b| String::from_utf8_lossy(&b).into()),
            _ => None,
        }
    }

    fn finding(
        &self,
        (check, kind): (CheckId, &'static str),
        place: Where,
        (count, unit): (usize, &'static str),
        detail: String,
        value: Option<String>,
    ) -> Finding {
        Finding::new(check, kind, place, count as u64, unit, detail)
            .with_value(self.values, value.as_deref())
    }
}

fn as_dict(o: &Object) -> Option<&Dictionary> {
    match o {
        Object::Dictionary(d) => Some(d),
        Object::Stream(s) => Some(&s.dict),
        _ => None,
    }
}

/// `1 thing` or `n things`.
fn n(count: usize, one: &str, many: &str) -> String {
    counted(count as u64, one, many)
}

fn metadata(c: &Ctx) -> Vec<Finding> {
    const ID: CheckId = CheckId::Metadata;
    let mut found = vec![];
    if let Some(info) = c.dict(&c.doc().trailer, b"Info").filter(|d| !d.is_empty()) {
        let pairs: Vec<_> = info
            .iter()
            .map(|(k, v)| {
                let shown = c.text(v).unwrap_or_else(|| format!("{:?}", c.resolve(v)));
                format!("/{}={shown}", String::from_utf8_lossy(k))
            })
            .collect();
        let mut detail = n(info.len(), "document property", "document properties");
        if info.has(b"Author") {
            detail += ", including the author";
        }
        found.push(c.finding(
            (ID, "info_dict"),
            Where::field("/Info"),
            (info.len(), "fields"),
            detail,
            Some(pairs.join("; ")),
        ));
    }
    if let Some(Ok(id)) = c
        .catalog()
        .map(|d| d.get(b"Metadata").and_then(Object::as_reference))
        && let Some(props) = xmp_of(c.doc().get_object(id).unwrap_or(&Object::Null))
        && !pdfa_only(&props)
    {
        found.push(c.finding(
            (ID, "xmp"),
            Where::object(id),
            (1, "packets"),
            "1 XMP metadata packet".into(),
            Some(joined(&props)),
        ));
    }
    let catalog = c
        .doc()
        .trailer
        .get(b"Root")
        .and_then(Object::as_reference)
        .ok();
    let (mut own, mut first, mut pieces, mut thumbs) = (0, None, 0, 0);
    for (id, o) in &c.doc().objects {
        let Some(d) = as_dict(o) else { continue };
        if Some(*id) != catalog
            && let Some(props) = c.get(d, b"Metadata").and_then(xmp_of)
        {
            own += 1;
            first.get_or_insert(joined(&props));
        }
        pieces += usize::from(d.has(b"PieceInfo"));
        thumbs += usize::from(d.has(b"Thumb") && d.has_type(b"Page"));
    }
    for (kind, count, unit, one, many, value) in [
        (
            "object_xmp",
            own,
            "objects",
            "object with its own XMP metadata",
            "objects with their own XMP metadata",
            first,
        ),
        (
            "app_data",
            pieces,
            "objects",
            "object with private application data",
            "objects with private application data",
            None,
        ),
        (
            "thumbnail",
            thumbs,
            "pictures",
            "page thumbnail",
            "page thumbnails",
            None,
        ),
    ] {
        if count > 0 {
            let detail = n(count, one, many);
            found.push(c.finding((ID, kind), Where::default(), (count, unit), detail, value));
        }
    }
    found
}

/// The properties a metadata stream sets as (name, value), or None when it isn't a stream.
fn xmp_of(o: &Object) -> Option<Vec<(String, String)>> {
    let Object::Stream(s) = o else { return None };
    Some(xmp(&s.get_plain_content_with_limit(STREAM_LIMIT).ok()?))
}

/// The properties an XMP packet sets, as (qualified name, value): the elements and
/// attributes inside each rdf:Description, a list item named by its property.
fn xmp(bytes: &[u8]) -> Vec<(String, String)> {
    use quick_xml::events::Event;
    let Ok(text) = crate::office::xml::decode(bytes, "XMP") else {
        return vec![];
    };
    let mut reader = quick_xml::Reader::from_str(&text);
    let (mut names, mut props) = (Vec::<String>::new(), Vec::<(String, String)>::new());
    // A text after an element boundary starts a new item of the current property.
    let mut fresh = true;
    loop {
        let event = match reader.read_event() {
            Ok(Event::Eof) | Err(_) => break,
            Ok(e) => e,
        };
        let inside = names.iter().any(|n| n == "rdf:Description");
        match &event {
            Event::Start(e) | Event::Empty(e) => {
                let name = e.name().as_ref().to_string();
                if name == "rdf:Description" {
                    for a in e.attributes().flatten() {
                        let key = a.key.as_ref().to_string();
                        if !key.starts_with("xmlns") && !key.starts_with("rdf:") {
                            props.push((key, a.value.to_string()));
                        }
                    }
                } else if inside && !name.starts_with("rdf:") {
                    props.push((name.clone(), String::new()));
                }
                if matches!(event, Event::Start(_)) {
                    names.push(name);
                }
                fresh = true;
            }
            Event::End(_) => {
                names.pop();
                fresh = true;
            }
            event if inside => {
                if let (Some(t), Some(last)) = (crate::office::xml::text(event), props.last_mut())
                    && !t.trim().is_empty()
                {
                    if fresh && !last.1.is_empty() {
                        last.1 += ", ";
                    }
                    last.1 += t.trim();
                    fresh = false;
                }
            }
            _ => {}
        }
    }
    props
}

/// Only PDF/A and PDF/UA identification, which a copy keeps and which is never a finding.
fn pdfa_only(props: &[(String, String)]) -> bool {
    props
        .iter()
        .all(|(k, _)| k.starts_with("pdfaid:") || k.starts_with("pdfuaid:"))
}

fn joined(props: &[(String, String)]) -> String {
    let pairs: Vec<_> = props
        .iter()
        .filter(|p| !p.1.is_empty())
        .map(|(k, v)| format!("{k}={v}"))
        .collect();
    pairs.join("; ")
}

/// Annotation types that are comments, as Acrobat lists them.
const COMMENTS: [&[u8]; 16] = [
    b"Text",
    b"FreeText",
    b"Line",
    b"Square",
    b"Circle",
    b"Polygon",
    b"PolyLine",
    b"Highlight",
    b"Underline",
    b"Squiggly",
    b"StrikeOut",
    b"Caret",
    b"Ink",
    b"Stamp",
    b"Sound",
    b"Projection",
];

fn subtype(d: &Dictionary) -> &[u8] {
    d.get(b"Subtype")
        .and_then(Object::as_name)
        .unwrap_or_default()
}

/// One finding per page with comment annotations, and one per page with file attachments.
fn comments(c: &Ctx) -> Vec<Finding> {
    let mut found = vec![];
    for (page, dict) in c.pages() {
        let annots: Vec<_> = c
            .annots(dict)
            .into_iter()
            .filter(|(_, a)| COMMENTS.contains(&subtype(a)))
            .collect();
        if annots.is_empty() {
            continue;
        }
        let mut authors: Vec<_> = annots
            .iter()
            .filter_map(|(_, a)| c.text(a.get(b"T").ok()?))
            .collect();
        authors.sort();
        authors.dedup();
        let mut detail = n(annots.len(), "annotation", "annotations");
        if !authors.is_empty() {
            detail += &format!(" by {}", n(authors.len(), "author", "authors"));
        }
        let value = annots.iter().find_map(|(_, a)| {
            ["Contents", "T"]
                .iter()
                .find_map(|k| c.text(a.get(k.as_bytes()).ok()?))
        });
        found.push(c.finding(
            (CheckId::Comments, "annotation"),
            Where::page(page),
            (annots.len(), "annotations"),
            detail,
            value,
        ));
    }
    found
}

/// The (key, value) leaves of a name tree.
fn name_tree<'a>(c: &Ctx<'a>, root: &'a Dictionary) -> Vec<(String, &'a Object)> {
    let mut leaves = vec![];
    let mut seen = HashSet::new();
    let mut pending = vec![root];
    while let Some(node) = pending.pop() {
        for pair in c.array(node, b"Names").chunks(2) {
            if let [key, value] = pair {
                leaves.push((c.text(key).unwrap_or_default(), c.resolve(value)));
            }
        }
        for kid in c.array(node, b"Kids") {
            if kid.as_reference().is_ok_and(|id| seen.insert(id))
                && let Some(d) = as_dict(c.resolve(kid))
            {
                pending.push(d);
            }
        }
    }
    leaves
}

/// The name tree `key` of the catalog's /Names.
fn names<'a>(c: &Ctx<'a>, key: &[u8]) -> Vec<(String, &'a Object)> {
    c.catalog()
        .and_then(|cat| c.dict(cat, b"Names"))
        .and_then(|names| c.dict(names, key))
        .map(|tree| name_tree(c, tree))
        .unwrap_or_default()
}

fn embedded_files(c: &Ctx) -> Vec<Finding> {
    const ID: CheckId = CheckId::EmbeddedFiles;
    let mut found = vec![];
    let attached = names(c, b"EmbeddedFiles");
    if !attached.is_empty() {
        let value = c.text(attached[0].1).or(Some(attached[0].0.clone()));
        found.push(c.finding(
            (ID, "attachment"),
            Where::default(),
            (attached.len(), "files"),
            n(attached.len(), "attached file", "attached files"),
            value,
        ));
    }
    for (page, dict) in c.pages() {
        let files: Vec<_> = c
            .annots(dict)
            .into_iter()
            .filter(|(_, a)| subtype(a) == b"FileAttachment")
            .collect();
        if let Some((_, first)) = files.first() {
            found.push(c.finding(
                (ID, "file_annotation"),
                Where::page(page),
                (files.len(), "files"),
                n(
                    files.len(),
                    "file attached to an annotation",
                    "files attached to annotations",
                ),
                first.get(b"FS").ok().and_then(|f| c.text(f)),
            ));
        }
    }
    let mut associated = vec![];
    for o in c.doc().objects.values() {
        if let Some(d) = as_dict(o) {
            match c.get(d, b"AF") {
                Some(Object::Array(a)) => associated.extend(a),
                Some(o @ Object::Dictionary(_)) => associated.push(o),
                _ => {}
            }
        }
    }
    if let Some(first) = associated.first() {
        found.push(c.finding(
            (ID, "associated_file"),
            Where::default(),
            (associated.len(), "files"),
            n(associated.len(), "associated file", "associated files"),
            c.text(first),
        ));
    }
    if c.catalog().is_some_and(|cat| cat.has(b"Collection")) {
        found.push(c.finding(
            (ID, "portfolio"),
            Where::default(),
            (1, "files"),
            "the file is a portfolio that shows its attachments as its content".into(),
            None,
        ));
    }
    found
}

/// Scripts, risky actions and links to other files, from every place an action can sit.
fn actions(c: &Ctx) -> Vec<Finding> {
    let mut roots: Vec<&Object> = vec![];
    if let Some(cat) = c.catalog() {
        roots.extend(cat.get(b"OpenAction").ok());
        roots.extend(triggers(c, cat));
    }
    roots.extend(names(c, b"JavaScript").into_iter().map(|(_, v)| v));
    for (_, page) in c.pages() {
        roots.extend(triggers(c, page));
        for (_, a) in c.annots(page) {
            roots.extend(a.get(b"A").ok());
            roots.extend(triggers(c, a));
        }
    }
    for field in form_fields(c) {
        roots.extend(triggers(c, field.dict));
    }

    // (kind, values) for javascript, action, remote_go_to, file_uri.
    let mut kinds: [Vec<Option<String>>; 4] = Default::default();
    let mut seen = HashSet::new();
    while let Some(o) = roots.pop() {
        if let Ok(id) = o.as_reference()
            && !seen.insert(id)
        {
            continue;
        }
        let o = c.resolve(o);
        if let Object::Array(next) = o {
            roots.extend(next);
            continue;
        }
        let Some(action) = as_dict(o) else { continue };
        let text = |key: &[u8]| action.get(key).ok().and_then(|v| c.text(v));
        match action
            .get(b"S")
            .and_then(Object::as_name)
            .unwrap_or_default()
        {
            b"JavaScript" => kinds[0].push(text(b"JS")),
            b"Launch" | b"ImportData" | b"SubmitForm" | b"RichMediaExecute" => {
                kinds[1].push(text(b"F"));
            }
            b"GoToR" | b"GoToE" => kinds[2].push(text(b"F")),
            b"URI" => {
                if let Some(uri) = text(b"URI").filter(|u| u.to_lowercase().starts_with("file:")) {
                    kinds[3].push(Some(uri));
                }
            }
            _ => {}
        }
        roots.extend(action.get(b"Next").ok());
    }
    let what = [
        (
            (CheckId::Macros, "javascript"),
            "scripts",
            "script that runs when the file is opened or used",
            "scripts that run when the file is opened or used",
        ),
        (
            (CheckId::Macros, "action"),
            "links",
            "action that opens a file, imports data or sends form data",
            "actions that open files, import data or send form data",
        ),
        (
            (CheckId::ExternalLinks, "remote_go_to"),
            "links",
            "link to another file",
            "links to other files",
        ),
        (
            (CheckId::ExternalLinks, "file_uri"),
            "links",
            "link to a local file",
            "links to local files",
        ),
    ];
    kinds
        .into_iter()
        .zip(what)
        .filter(|(values, _)| !values.is_empty())
        .map(|(values, (kind, unit, one, many))| {
            let value = values.iter().flatten().next().cloned();
            c.finding(
                kind,
                Where::default(),
                (values.len(), unit),
                n(values.len(), one, many),
                value,
            )
        })
        .collect()
}

/// The actions of a dictionary's /AA triggers.
fn triggers<'a>(c: &Ctx<'a>, d: &'a Dictionary) -> Vec<&'a Object> {
    c.dict(d, b"AA")
        .map(|aa| aa.iter().map(|(_, v)| v).collect())
        .unwrap_or_default()
}

/// What a field stores and hands down to the fields below it: its value, the default a reset
/// restores, and its rich text.
const VALUE_KEYS: [&[u8]; 3] = [b"V", b"DV", b"RV"];

/// A terminal form field with its inherited type and values, its widgets and the fields above
/// it.
struct Field<'a> {
    id: Option<ObjectId>,
    dict: &'a Dictionary,
    kind: &'a [u8],
    /// What each of [`VALUE_KEYS`] holds, its own or inherited.
    values: [Option<&'a Object>; 3],
    widgets: Vec<(Option<ObjectId>, &'a Dictionary)>,
    parents: Vec<ObjectId>,
}

/// The terminal fields of the AcroForm, walking /Kids with inheritance.
fn form_fields<'a>(c: &Ctx<'a>) -> Vec<Field<'a>> {
    let Some(form) = c.catalog().and_then(|cat| c.dict(cat, b"AcroForm")) else {
        return vec![];
    };
    let mut fields = vec![];
    let mut seen = HashSet::new();
    let mut pending: Vec<_> = c
        .array(form, b"Fields")
        .iter()
        .map(|f| (f, &b""[..], [None; 3], vec![]))
        .collect();
    while let Some((o, kind, mut values, mut parents)) = pending.pop() {
        if o.as_reference().is_ok_and(|id| !seen.insert(id)) {
            continue;
        }
        let Some(dict) = as_dict(c.resolve(o)) else {
            continue;
        };
        let kind = dict.get(b"FT").and_then(Object::as_name).unwrap_or(kind);
        for (value, key) in values.iter_mut().zip(VALUE_KEYS) {
            *value = c.get(dict, key).or(*value);
        }
        let kids = c.array(dict, b"Kids");
        let (children, widgets): (Vec<_>, Vec<_>) = kids
            .iter()
            .partition(|k| as_dict(c.resolve(k)).is_some_and(|d| d.has(b"T")));
        if children.is_empty() {
            let mut own: Vec<_> = widgets
                .iter()
                .filter_map(|w| Some((w.as_reference().ok(), as_dict(c.resolve(w))?)))
                .collect();
            if subtype(dict) == b"Widget" {
                own.push((o.as_reference().ok(), dict));
            }
            fields.push(Field {
                id: o.as_reference().ok(),
                dict,
                kind,
                values,
                widgets: own,
                parents,
            });
        } else {
            parents.extend(o.as_reference().ok());
            pending.extend(
                children
                    .into_iter()
                    .map(|k| (k, kind, values, parents.clone())),
            );
        }
    }
    fields
}

/// A value that isn't empty or an unticked box.
fn filled(value: Option<&Object>) -> bool {
    match value {
        None | Some(Object::Null) => false,
        Some(Object::String(s, _)) => !s.is_empty(),
        Some(Object::Name(n)) => n != b"Off",
        Some(_) => true,
    }
}

/// The widgets the pages list.
fn listed_widgets(c: &Ctx) -> HashSet<ObjectId> {
    c.pages()
        .flat_map(|(_, p)| c.annots(p))
        .filter_map(|(id, _)| id)
        .collect()
}

impl Field<'_> {
    fn stores_anything(&self) -> bool {
        self.values.iter().any(|v| filled(*v))
    }

    /// Whether no page shows it: a field that isn't a signature, with every widget Hidden,
    /// NoView or on no page.
    fn unseen(&self, listed: &HashSet<ObjectId>) -> bool {
        self.kind != b"Sig"
            && self.widgets.iter().all(|(id, w)| {
                let flags = w.get(b"F").and_then(Object::as_i64).unwrap_or(0);
                flags & (2 | 32) != 0 || !id.is_some_and(|id| listed.contains(&id))
            })
    }
}

/// Signatures (signed fields and usage rights), XFA forms and hidden field values.
fn fields(c: &Ctx) -> Vec<Finding> {
    let mut found = vec![];
    let fields = form_fields(c);
    let signed = fields
        .iter()
        .filter(|f| f.kind == b"Sig")
        .filter(|f| filled(f.values[0]))
        .count();
    let rights = c
        .catalog()
        .and_then(|cat| c.dict(cat, b"Perms"))
        .map_or(0, Dictionary::len);
    if signed + rights > 0 {
        let mut detail = n(signed + rights, "digital signature", "digital signatures");
        if rights > 0 {
            detail += ", including usage rights";
        }
        found.push(c.finding(
            (CheckId::Signatures, "pdf_signature"),
            Where::default(),
            (signed + rights, "signatures"),
            detail,
            None,
        ));
    }
    let form = c.catalog().and_then(|cat| c.dict(cat, b"AcroForm"));
    if let Some(xfa) = form.and_then(|f| c.get(f, b"XFA")) {
        let parts = match xfa {
            Object::Array(a) => a.len() / 2,
            _ => 1,
        };
        found.push(c.finding(
            (CheckId::FormData, "xfa"),
            Where::default(),
            (parts, "parts"),
            format!("an XFA form in {}", n(parts, "part", "parts")),
            None,
        ));
    }
    let listed = listed_widgets(c);
    let hidden: Vec<_> = fields
        .iter()
        .filter(|f| f.stores_anything() && f.unseen(&listed))
        .collect();
    if let Some(first) = hidden.first() {
        found.push(c.finding(
            (CheckId::FormData, "hidden_field_value"),
            Where::default(),
            (hidden.len(), "fields"),
            n(
                hidden.len(),
                "form field with a value no page shows",
                "form fields with values no page shows",
            ),
            first.values.iter().flatten().find_map(|v| c.text(v)),
        ));
    }
    found
}

/// JPEG image streams carrying metadata; a JPEG cut short makes the check incomplete.
fn image_metadata(c: &Ctx, found: &mut Found) {
    for (&id, o) in &c.doc().objects {
        let Object::Stream(s) = o else { continue };
        if subtype(&s.dict) != b"Image" || s.filters().unwrap_or_default() != [b"DCTDecode"] {
            continue;
        }
        match pictures::metadata(&s.content) {
            Some(labels) if !labels.is_empty() => found.findings.push(c.finding(
                (CheckId::ImageMetadata, "picture_metadata"),
                Where::object(id),
                (1, "pictures"),
                format!("1 picture with {}", pictures::joined(&labels)),
                None,
            )),
            Some(_) => {}
            None => found.incomplete.push((
                CheckId::ImageMetadata,
                format!(
                    "object {} {}: its JPEG is cut short, so its metadata wasn't checked",
                    id.0, id.1
                ),
            )),
        }
    }
}

/// A page's content streams decoded and joined, or None when one can't be decoded.
fn page_content(c: &Ctx, page: &Dictionary) -> Option<Vec<u8>> {
    let streams: Vec<&Object> = match c.get(page, b"Contents") {
        Some(Object::Array(a)) => a.iter().collect(),
        Some(o) => vec![o],
        None => vec![],
    };
    let mut content = vec![];
    for s in streams {
        if let Object::Stream(s) = c.resolve(s) {
            content.extend(s.decompressed_content_with_limit(STREAM_LIMIT).ok()?);
            content.push(b'\n');
        }
    }
    Some(content)
}

/// A page's resources, inherited from its ancestors when it has none.
fn page_resources<'a>(c: &Ctx<'a>, page: &'a Dictionary) -> Option<&'a Dictionary> {
    let mut node = page;
    for _ in 0..64 {
        if let Some(r) = c.dict(node, b"Resources") {
            return Some(r);
        }
        node = c.dict(node, b"Parent")?;
    }
    None
}

/// Calls `visit` with each operator of `content` and the forms it draws, in drawing order,
/// with the resources it was drawn with. A form drawn twice is walked once.
fn walk<'a>(
    c: &Ctx<'a>,
    content: &[u8],
    resources: Option<&'a Dictionary>,
    visit: &mut impl FnMut(&Operation, Option<&'a Dictionary>),
) {
    let mut seen = HashSet::new();
    walk_from(c, content, resources, visit, &mut seen, 0);
}

fn walk_from<'a>(
    c: &Ctx<'a>,
    content: &[u8],
    resources: Option<&'a Dictionary>,
    visit: &mut impl FnMut(&Operation, Option<&'a Dictionary>),
    seen: &mut HashSet<ObjectId>,
    depth: usize,
) {
    for op in Content::decode_iter(content).map_while(Result::ok) {
        visit(&op, resources);
        if op.operator != "Do" || depth > 32 {
            continue;
        }
        let Some(name) = op.operands.first().and_then(|o| o.as_name().ok()) else {
            continue;
        };
        let Some(Object::Reference(id)) = resources
            .and_then(|r| c.dict(r, b"XObject"))
            .and_then(|x| x.get(name).ok())
        else {
            continue;
        };
        if let Ok(Object::Stream(form)) = c.doc().get_object(*id)
            && subtype(&form.dict) == b"Form"
            && seen.insert(*id)
            && let Ok(inner) = form.decompressed_content_with_limit(STREAM_LIMIT)
        {
            let own = c.dict(&form.dict, b"Resources").or(resources);
            walk_from(c, &inner, own, visit, seen, depth + 1);
        }
    }
}

/// XObjects a page's or a drawn form's resources name that no `Do` draws.
fn unused_resources<'a>(c: &Ctx<'a>, page: &'a Dictionary) -> usize {
    let Some(content) = page_content(c, page) else {
        return 0;
    };
    let resources = page_resources(c, page);
    let mut named: Vec<_> = resources.map(|r| xobjects(c, r)).unwrap_or_default();
    let mut drawn = HashSet::new();
    walk(c, &content, resources, &mut |op, resources| {
        let Some(r) = resources else { return };
        let key = std::ptr::from_ref(r);
        if !named.iter().any(|n| n.0 == key) {
            named.extend(xobjects(c, r));
        }
        if op.operator == "Do"
            && let Some(name) = op.operands.first().and_then(|o| o.as_name().ok())
        {
            drawn.insert((key, name.to_vec()));
        }
    });
    named.iter().filter(|n| !drawn.contains(n)).count()
}

/// The XObject names of `r`, keyed by its address so a form's own resources are told from
/// the page's.
fn xobjects<'a>(c: &Ctx<'a>, r: &'a Dictionary) -> Vec<(*const Dictionary, Vec<u8>)> {
    let key = std::ptr::from_ref(r);
    let names = c
        .dict(r, b"XObject")
        .map(|x| x.iter().map(|(k, _)| (key, k.clone())));
    names.into_iter().flatten().collect()
}

/// The XObject names `content` and the forms it draws reach with `Do`, keyed by the resources
/// that name them.
fn drawn<'a>(
    c: &Ctx<'a>,
    content: &[u8],
    resources: Option<&'a Dictionary>,
) -> HashSet<(*const Dictionary, Vec<u8>)> {
    let mut drawn = HashSet::new();
    walk(c, content, resources, &mut |op, resources| {
        if let Some(r) = resources
            && op.operator == "Do"
            && let Some(name) = op.operands.first().and_then(|o| o.as_name().ok())
        {
            drawn.insert((std::ptr::from_ref(r), name.to_vec()));
        }
    });
    drawn
}

/// Each page whose resources name XObjects it never draws, with resources of its own that
/// name only what it draws (other pages may share the ones it has).
fn undrawn_on_pages(c: &Ctx) -> Vec<(ObjectId, Dictionary)> {
    let mut out = vec![];
    for &id in &c.pdf.pages {
        let Ok(page) = c.doc().get_dictionary(id) else {
            continue;
        };
        let (Some(content), Some(r)) = (page_content(c, page), page_resources(c, page)) else {
            continue;
        };
        let Some(named) = c.dict(r, b"XObject") else {
            continue;
        };
        let drawn = drawn(c, &content, Some(r));
        let is_drawn = |k: &Vec<u8>| drawn.contains(&(std::ptr::from_ref(r), k.clone()));
        if named.iter().all(|(k, _)| is_drawn(k)) {
            continue;
        }
        let mut kept = Dictionary::new();
        for (k, v) in named.iter().filter(|(k, _)| is_drawn(k)) {
            kept.set(k.clone(), v.clone());
        }
        let mut own = r.clone();
        own.set("XObject", kept);
        out.push((id, own));
    }
    out
}

/// Each form with resources of its own (shared with no page) and the XObject names they hold
/// that its content never draws.
fn undrawn_in_forms(c: &Ctx) -> Vec<(ObjectId, Vec<Vec<u8>>)> {
    let on_pages: HashSet<*const Dictionary> = c
        .pages()
        .filter_map(|(_, p)| page_resources(c, p))
        .map(std::ptr::from_ref)
        .collect();
    let mut out = vec![];
    for (&id, o) in &c.doc().objects {
        let Object::Stream(s) = o else { continue };
        let Some(r) = c.dict(&s.dict, b"Resources") else {
            continue;
        };
        if subtype(&s.dict) != b"Form" || on_pages.contains(&std::ptr::from_ref(r)) {
            continue;
        }
        let (Some(named), Ok(content)) = (
            c.dict(r, b"XObject"),
            s.decompressed_content_with_limit(STREAM_LIMIT),
        ) else {
            continue;
        };
        let drawn = drawn(c, &content, Some(r));
        let names: Vec<_> = named
            .iter()
            .map(|(k, _)| k.clone())
            .filter(|k| !drawn.contains(&(std::ptr::from_ref(r), k.clone())))
            .collect();
        if !names.is_empty() {
            out.push((id, names));
        }
    }
    out
}

/// `page 3`, `pages 1 and 4`, `pages 1, 4 and 9`.
fn pages_text(numbers: &[usize]) -> String {
    let shown: Vec<_> = numbers.iter().map(ToString::to_string).collect();
    let shown: Vec<_> = shown.iter().map(String::as_str).collect();
    match shown.as_slice() {
        [one] => format!("page {one}"),
        _ => format!("pages {}", pictures::joined(&shown)),
    }
}
