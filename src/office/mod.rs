//! Office packages (OPC): ZIP files of XML parts tied together by relationships. Opening one
//! scans the ZIP, inflates every entry once within the limits, reads the content types and
//! relationships, decides the variant from the main part's type and checks that every XML part
//! is well-formed within the XML limits.

pub mod clean;
pub mod colour;
pub mod common;
pub mod drawing;
pub mod excel;
pub mod powerpoint;
pub mod revisions;
pub mod tree;
pub mod word;
pub mod xml;
pub mod zipscan;

use std::collections::{HashMap, HashSet};
use std::io::{self, Read, Write};

use flate2::CrcWriter;
use flate2::read::DeflateDecoder;
use quick_xml::events::Event;

use crate::report::{CheckId, Error, Finding, Format, Where, safe};
use zipscan::{Entry, Zip};

/// The namespaces the checks read, by their usual prefixes.
pub mod ns {
    pub const W: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
    pub const W14: &str = "http://schemas.microsoft.com/office/word/2010/wordml";
    pub const W15: &str = "http://schemas.microsoft.com/office/word/2012/wordml";
    pub const S: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
    pub const XR: &str = "http://schemas.microsoft.com/office/spreadsheetml/2014/revision";
    pub const P: &str = "http://schemas.openxmlformats.org/presentationml/2006/main";
    pub const P14: &str = "http://schemas.microsoft.com/office/powerpoint/2010/main";
    pub const A: &str = "http://schemas.openxmlformats.org/drawingml/2006/main";
    pub const R: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
    pub const CP: &str = "http://schemas.openxmlformats.org/package/2006/metadata/core-properties";
    pub const DC: &str = "http://purl.org/dc/elements/1.1/";
    pub const DCTERMS: &str = "http://purl.org/dc/terms/";
    pub const APP: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/extended-properties";
    pub const CUSTOM: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/custom-properties";
}

/// The checks of each format, in check order.
pub fn checks(format: Format) -> &'static [CheckId] {
    use CheckId::*;
    match format {
        Format::Docx => &[
            Metadata,
            Comments,
            TrackedChanges,
            HiddenText,
            FakeRedactions,
            HiddenObjects,
            EmbeddedFiles,
            CustomData,
            Macros,
            ExternalLinks,
            ImageMetadata,
            CroppedImages,
            LeftoverData,
            Signatures,
        ],
        Format::Xlsx => &[
            Metadata,
            Comments,
            TrackedChanges,
            HiddenText,
            FakeRedactions,
            HiddenSheets,
            HiddenRowsColumns,
            HiddenObjects,
            EmbeddedFiles,
            CustomData,
            Macros,
            ExternalLinks,
            ImageMetadata,
            CroppedImages,
            PivotCaches,
            LeftoverData,
            Signatures,
        ],
        Format::Pptx => &[
            Metadata,
            Comments,
            HiddenSlides,
            SpeakerNotes,
            HiddenObjects,
            EmbeddedFiles,
            CustomData,
            Macros,
            ExternalLinks,
            ImageMetadata,
            CroppedImages,
            LeftoverData,
            Signatures,
        ],
        Format::Pdf => &[],
    }
}

/// Runs every check of the package's format.
pub fn check(pkg: &Package, values: bool) -> Result<Vec<Finding>, Error> {
    let ctx = Ctx::new(pkg, values)?;
    let mut found = common::metadata(&ctx)?;
    found.extend(common::comments(&ctx)?);
    match pkg.format {
        Format::Docx => {
            found.extend(word::tracked_changes(&ctx)?);
            found.extend(word::hidden_text(&ctx)?);
            found.extend(word::story_items(&ctx)?);
        }
        Format::Xlsx => {
            found.extend(excel::shared_workbook_log(&ctx)?);
            found.extend(excel::cells(&ctx)?);
            found.extend(excel::hidden_sheets(&ctx)?);
            found.extend(excel::pivot_caches(&ctx)?);
        }
        Format::Pptx => {
            found.extend(powerpoint::hidden_slides(&ctx)?);
            found.extend(powerpoint::speaker_notes(&ctx)?);
        }
        Format::Pdf => {}
    }
    found.extend(drawing::hidden_objects(&ctx)?);
    found.extend(drawing::crops_and_alt_text(&ctx)?);
    found.extend(common::linked_parts(&ctx)?);
    found.extend(common::external_links(&ctx)?);
    found.extend(common::image_metadata(&ctx)?);
    found.extend(common::leftover_data(pkg, values)?);
    Ok(found)
}

/// A sheet of a workbook, in the workbook's order.
#[derive(Debug)]
pub struct Sheet {
    pub name: String,
    /// The sheet's part, when the package has it.
    pub part: Option<String>,
    /// `visible`, `hidden` or `veryHidden`.
    pub state: String,
}

/// What every check of one package needs: the package, whether to show values, and the
/// sheets or slides in their order.
pub struct Ctx<'p> {
    pub pkg: &'p Package<'p>,
    pub values: bool,
    pub sheets: Vec<Sheet>,
    /// Slide parts in the presentation's order.
    pub slides: Vec<String>,
    /// The slide width and height in EMU.
    pub slide_size: Option<(i64, i64)>,
}

impl<'p> Ctx<'p> {
    pub fn new(pkg: &'p Package, values: bool) -> Result<Self, Error> {
        let mut ctx = Self {
            pkg,
            values,
            sheets: vec![],
            slides: vec![],
            slide_size: None,
        };
        let main = pkg.main.as_str();
        let target = |id: &str| {
            pkg.links
                .iter()
                .find(|l| l.source == main && l.rel.id == id)
                .and_then(|l| l.part.clone())
        };
        pkg.walk(main, |r, ev| {
            let (Event::Start(e) | Event::Empty(e)) = ev else {
                return Ok(());
            };
            let id = || r.attr_ns(e, ns::R, "id");
            if r.is(e, ns::S, "sheet") {
                let name = r.attr(e, "name")?.unwrap_or_default().into_owned();
                let state = r.attr(e, "state")?.unwrap_or("visible".into()).into_owned();
                let part = id()?.and_then(|id| target(&id));
                ctx.sheets.push(Sheet { name, part, state });
            } else if r.is(e, ns::P, "sldSz") {
                let num = |a| {
                    r.attr(e, a)
                        .map(|v| v.and_then(|v| v.parse().ok()).unwrap_or(0))
                };
                ctx.slide_size = Some((num("cx")?, num("cy")?));
            } else if r.is(e, ns::P, "sldId")
                && let Some(part) = id()?.and_then(|id| target(&id))
            {
                ctx.slides.push(part);
            }
            Ok(())
        })?;
        Ok(ctx)
    }

    /// Where `part` is: its slide or sheet, or the slide or sheet that links to it.
    pub fn place(&self, part: &str) -> Where {
        let k = key(part);
        let sources = self
            .pkg
            .links
            .iter()
            .filter(|l| l.part.as_deref().is_some_and(|p| key(p) == k));
        let mut at = Where::part(part);
        for p in std::iter::once(part).chain(sources.map(|l| l.source.as_str())) {
            let k = key(p);
            if let Some(i) = self.slides.iter().position(|s| key(s) == k) {
                at.slide = Some(i + 1);
                break;
            }
            let sheet = |s: &Sheet| s.part.as_deref().is_some_and(|s| key(s) == k);
            if let Some(i) = self.sheets.iter().position(sheet) {
                let sheet = &self.sheets[i];
                at.sheet_index = Some(i + 1);
                if sheet.state == "visible" || self.values {
                    at.sheet = Some(safe(&sheet.name));
                }
                break;
            }
        }
        at
    }
}

/// The most one entry may inflate to, and the whole package.
const MAX_PART: u64 = 256 << 20;
const MAX_PACKAGE: u64 = 1 << 30;
/// The most a Word story, a slide, a drawing or a chart may hold: each is always read as a
/// tree. Any other part is streamed; `clean` bounds a tree it does read in [`tree::Doc::parse`].
const MAX_TREE: u64 = 64 << 20;

pub const CONTENT_TYPES: &str = "http://schemas.openxmlformats.org/package/2006/content-types";
pub const RELATIONSHIPS: &str = "http://schemas.openxmlformats.org/package/2006/relationships";
const OFFICE_DOCUMENT: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument";
const STRICT: &str = "http://purl.oclc.org/ooxml/";

/// The variant each main part's content type makes a package.
pub const MAIN_TYPES: [(&str, Format, &str); 14] = [
    (
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml",
        Format::Docx,
        "docx",
    ),
    (
        "application/vnd.ms-word.document.macroEnabled.main+xml",
        Format::Docx,
        "docm",
    ),
    (
        "application/vnd.openxmlformats-officedocument.wordprocessingml.template.main+xml",
        Format::Docx,
        "dotx",
    ),
    (
        "application/vnd.ms-word.template.macroEnabledTemplate.main+xml",
        Format::Docx,
        "dotm",
    ),
    (
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml",
        Format::Xlsx,
        "xlsx",
    ),
    (
        "application/vnd.ms-excel.sheet.macroEnabled.main+xml",
        Format::Xlsx,
        "xlsm",
    ),
    (
        "application/vnd.openxmlformats-officedocument.spreadsheetml.template.main+xml",
        Format::Xlsx,
        "xltx",
    ),
    (
        "application/vnd.ms-excel.template.macroEnabled.main+xml",
        Format::Xlsx,
        "xltm",
    ),
    (
        "application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml",
        Format::Pptx,
        "pptx",
    ),
    (
        "application/vnd.ms-powerpoint.presentation.macroEnabled.main+xml",
        Format::Pptx,
        "pptm",
    ),
    (
        "application/vnd.openxmlformats-officedocument.presentationml.template.main+xml",
        Format::Pptx,
        "potx",
    ),
    (
        "application/vnd.ms-powerpoint.template.macroEnabled.main+xml",
        Format::Pptx,
        "potm",
    ),
    (
        "application/vnd.openxmlformats-officedocument.presentationml.slideshow.main+xml",
        Format::Pptx,
        "ppsx",
    ),
    (
        "application/vnd.ms-powerpoint.slideshow.macroEnabled.main+xml",
        Format::Pptx,
        "ppsm",
    ),
];
const XLSB: &str = "application/vnd.ms-excel.sheet.binary.macroEnabled.main";

/// One relationship from a part's `.rels`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rel {
    pub id: String,
    pub kind: String,
    pub target: String,
    pub external: bool,
}

impl Rel {
    /// The last segment of the relationship type, e.g. `comments`.
    pub fn short(&self) -> &str {
        self.kind.rsplit('/').next().unwrap_or_default()
    }
}

/// A relationship of a part reachable from the package.
#[derive(Debug, Clone)]
pub struct Link {
    /// The part whose `.rels` holds it ("" for the package).
    pub source: String,
    pub rel: Rel,
    /// The internal target in the package's own spelling, when the package has it.
    pub part: Option<String>,
}

/// An opened package.
pub struct Package<'a> {
    bytes: &'a [u8],
    pub zip: Zip,
    /// Inflated size of each entry.
    sizes: Vec<u64>,
    /// Entry index by part key (see [`key`]).
    index: HashMap<String, usize>,
    defaults: HashMap<String, String>,
    overrides: HashMap<String, String>,
    pub format: Format,
    pub variant: &'static str,
    /// The main part (document, workbook or presentation).
    pub main: String,
    /// Every relationship of every reachable part, found breadth first from the package.
    pub links: Vec<Link>,
    /// The reachable parts, in the order they are found.
    pub parts: Vec<String>,
}

/// How part names compare: percent-escapes decoded, ASCII case folded (OPC part names are
/// case-insensitive).
pub fn key(name: &str) -> String {
    let b = name.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = b
            .get(i + 1..i + 3)
            .and_then(|h| u8::from_str_radix(std::str::from_utf8(h).ok()?, 16).ok());
        match (b[i], hex) {
            (b'%', Some(byte)) => {
                out.push(byte);
                i += 3;
            }
            (c, _) => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).to_ascii_lowercase()
}

/// Whether `name` can be an OPC part name: no backslash, no leading or trailing slash, and no
/// empty, `.` or `..` segment.
pub fn part_name(name: &str) -> bool {
    !name.contains('\\')
        && name
            .split('/')
            .all(|s| !s.is_empty() && s != "." && s != ".." && !s.ends_with('.'))
}

/// The `.rels` part of `source` ("" for the package itself).
pub fn rels_name(source: &str) -> String {
    match source.rsplit_once('/') {
        Some((dir, file)) => format!("{dir}/_rels/{file}.rels"),
        None if source.is_empty() => "_rels/.rels".into(),
        None => format!("_rels/{source}.rels"),
    }
}

/// The part a relative or absolute relationship target points at, from `source`; None when it
/// climbs out of the package.
pub fn resolve(source: &str, target: &str) -> Option<String> {
    let target = target.split(['#', '?']).next().unwrap_or_default();
    let mut parts: Vec<&str> = match target.strip_prefix('/') {
        Some(_) => vec![],
        None => source.split('/').collect(),
    };
    parts.pop();
    for seg in target.trim_start_matches('/').split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            s => parts.push(s),
        }
    }
    Some(parts.join("/"))
}

/// Inflates an entry into `out`, refusing it past 256 MiB and checking its CRC; gives the size.
fn inflate(bytes: &[u8], e: &Entry, out: impl Write) -> Result<u64, Error> {
    let data = &bytes[e.data.clone()];
    let reader: Box<dyn Read> = match e.method {
        0 => Box::new(data),
        8 => Box::new(DeflateDecoder::new(data)),
        _ => return Err(Error::damaged("its structure can't be read")),
    };
    let mut out = CrcWriter::new(out);
    let n = io::copy(&mut reader.take(MAX_PART + 1), &mut out)
        .map_err(|_| Error::damaged("its structure can't be read"))?;
    if n > MAX_PART {
        return Err(Error::too_large(
            &format!("part {}", safe(&e.name)),
            "256 MiB unpacked",
        ));
    }
    if out.crc().sum() != e.crc {
        return Err(Error::damaged("its structure can't be read"));
    }
    Ok(n)
}

impl<'a> Package<'a> {
    pub fn open(bytes: &'a [u8]) -> Result<Self, Error> {
        let zip = zipscan::scan(bytes)?;
        // Every entry is inflated once to count it before any is kept in memory.
        let mut sizes = vec![];
        let mut total = 0;
        for e in &zip.entries {
            let n = inflate(bytes, e, io::sink())?;
            total += n;
            if total > MAX_PACKAGE {
                return Err(Error::too_large("the unpacked package", "1 GiB"));
            }
            sizes.push(n);
        }
        let index = zip
            .entries
            .iter()
            .enumerate()
            .map(|(i, e)| (key(&e.name), i))
            .collect();
        let mut pkg = Self {
            bytes,
            zip,
            sizes,
            index,
            defaults: HashMap::new(),
            overrides: HashMap::new(),
            format: Format::Docx,
            variant: "",
            main: String::new(),
            links: vec![],
            parts: vec![],
        };
        if !pkg.has("[Content_Types].xml") || !pkg.has("_rels/.rels") {
            return Err(Error::unsupported_format());
        }
        pkg.content_types()?;
        let root = pkg.rels("")?;
        let main = root
            .iter()
            .find(|r| r.kind.ends_with("/officeDocument") && !r.external);
        let main = match main {
            Some(r) if r.kind.starts_with(STRICT) => return Err(Error::strict()),
            Some(r) if r.kind == OFFICE_DOCUMENT => r,
            _ => return Err(Error::unsupported_format()),
        };
        let part = resolve("", &main.target)
            .filter(|p| pkg.has(p))
            .ok_or_else(|| {
                Error::damaged(&format!(
                    "part {} is missing",
                    safe(main.target.trim_start_matches('/'))
                ))
            })?;
        let content_type = pkg.content_type(&part).unwrap_or_default();
        if content_type == XLSB {
            return Err(Error::xlsb());
        }
        let (_, format, variant) = MAIN_TYPES
            .iter()
            .find(|(t, ..)| *t == content_type)
            .ok_or_else(Error::unsupported_format)?;
        (pkg.format, pkg.variant) = (*format, variant);
        for (e, &size) in pkg.zip.entries.iter().zip(&pkg.sizes) {
            let content_type = pkg.content_type(&e.name).unwrap_or_default();
            let xml = content_type.ends_with("xml");
            let story = (*format == Format::Docx && key(&e.name) == key(&part))
                || content_type
                    .strip_prefix(word::WML)
                    .is_some_and(|t| word::STORY_TYPES.contains(&t));
            let tree = story
                || content_type.contains("drawing")
                || content_type.ends_with("presentationml.slide+xml");
            if tree && size > MAX_TREE {
                return Err(Error::too_large(
                    &format!("part {}", safe(&e.name)),
                    "64 MiB",
                ));
            }
            if part_name(&e.name) && (xml || e.name.to_ascii_lowercase().ends_with(".rels")) {
                let bytes = pkg.read(&e.name)?;
                xml::Reader::new(&xml::decode(&bytes, &e.name)?, &e.name).validate()?;
            }
        }
        pkg.main = pkg.name(&part);
        pkg.follow()?;
        Ok(pkg)
    }

    /// The part's name in the package's own spelling.
    fn name(&self, part: &str) -> String {
        self.index
            .get(&key(part))
            .map_or(part, |&i| &self.zip.entries[i].name)
            .to_string()
    }

    /// Fills `links` and `parts` by following relationships from the package.
    fn follow(&mut self) -> Result<(), Error> {
        let mut seen = HashSet::new();
        let mut pending = std::collections::VecDeque::from([String::new()]);
        while let Some(source) = pending.pop_front() {
            for rel in self.rels(&source)? {
                let part = resolve(&source, &rel.target)
                    .filter(|t| !rel.external && self.has(t))
                    .map(|t| self.name(&t));
                if let Some(p) = &part
                    && seen.insert(key(p))
                {
                    self.parts.push(p.clone());
                    pending.push_back(p.clone());
                }
                self.links.push(Link {
                    source: source.clone(),
                    rel,
                    part,
                });
            }
        }
        Ok(())
    }

    /// The reachable parts of content type `ct`.
    pub fn typed<'s>(&'s self, ct: &'s str) -> impl Iterator<Item = &'s str> {
        self.parts
            .iter()
            .map(String::as_str)
            .filter(move |p| self.content_type(p) == Some(ct))
    }

    /// Streams the XML part `name`, handing every event but the end to `f`.
    pub fn walk(
        &self,
        name: &str,
        mut f: impl FnMut(&xml::Reader, &Event) -> Result<(), Error>,
    ) -> Result<(), Error> {
        let bytes = self.read(name)?;
        let text = xml::decode(&bytes, name)?;
        let mut r = xml::Reader::new(&text, name);
        loop {
            match r.event()? {
                Event::Eof => return Ok(()),
                ev => f(&r, &ev)?,
            }
        }
    }

    /// Whether the package has the part `name`.
    pub fn has(&self, name: &str) -> bool {
        self.index.contains_key(&key(name))
    }

    /// A part's inflated bytes.
    pub fn read(&self, name: &str) -> Result<Vec<u8>, Error> {
        let i = *self
            .index
            .get(&key(name))
            .ok_or_else(|| Error::damaged(&format!("part {} is missing", safe(name))))?;
        let mut out = Vec::with_capacity(self.sizes[i] as usize);
        inflate(self.bytes, &self.zip.entries[i], &mut out)?;
        Ok(out)
    }

    /// Reads `[Content_Types].xml`: Defaults by extension, Overrides by part name.
    fn content_types(&mut self) -> Result<(), Error> {
        let name = "[Content_Types].xml";
        let bytes = self.read(name)?;
        let text = xml::decode(&bytes, name)?;
        let mut r = xml::Reader::new(&text, name);
        loop {
            match r.event()? {
                Event::Start(e) | Event::Empty(e) => {
                    let ct = r.attr(&e, "ContentType")?.unwrap_or_default().into_owned();
                    if r.is(&e, CONTENT_TYPES, "Default") {
                        let ext = r.attr(&e, "Extension")?.unwrap_or_default();
                        self.defaults.insert(ext.to_ascii_lowercase(), ct);
                    } else if r.is(&e, CONTENT_TYPES, "Override") {
                        let part = r.attr(&e, "PartName")?.unwrap_or_default();
                        self.overrides.insert(key(part.trim_start_matches('/')), ct);
                    }
                }
                Event::Eof => return Ok(()),
                _ => {}
            }
        }
    }

    /// A part's content type: its Override, else the Default for its extension.
    pub fn content_type(&self, name: &str) -> Option<&str> {
        let k = key(name);
        let file = k.rsplit('/').next().unwrap_or_default();
        let ext = file.rsplit_once('.').map(|(_, e)| e);
        self.overrides
            .get(&k)
            .or_else(|| self.defaults.get(ext?))
            .map(String::as_str)
    }

    /// The relationships of `source` ("" for the package), or none when it has no `.rels`.
    pub fn rels(&self, source: &str) -> Result<Vec<Rel>, Error> {
        let name = rels_name(source);
        if !self.has(&name) {
            return Ok(vec![]);
        }
        let bytes = self.read(&name)?;
        let text = xml::decode(&bytes, &name)?;
        let mut r = xml::Reader::new(&text, &name);
        let mut rels = vec![];
        loop {
            match r.event()? {
                Event::Start(e) | Event::Empty(e) if r.is(&e, RELATIONSHIPS, "Relationship") => {
                    let get = |a| r.attr(&e, a).map(|v| v.unwrap_or_default().into_owned());
                    rels.push(Rel {
                        id: get("Id")?,
                        kind: get("Type")?,
                        target: get("Target")?,
                        external: get("TargetMode")? == "External",
                    });
                }
                Event::Eof => return Ok(rels),
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn part_names_compare_decoded_and_case_folded() {
        assert_eq!(key("Word/Media/Image%201.PNG"), "word/media/image 1.png");
        assert_eq!(key("a%2"), "a%2");
        assert_eq!(key("%zz"), "%zz");
    }

    #[test]
    fn relationship_parts_sit_beside_their_source() {
        assert_eq!(rels_name(""), "_rels/.rels");
        assert_eq!(
            rels_name("word/document.xml"),
            "word/_rels/document.xml.rels"
        );
        assert_eq!(rels_name("x.xml"), "_rels/x.xml.rels");
    }

    #[test]
    fn targets_resolve_from_the_source_folder() {
        assert_eq!(
            resolve("", "word/document.xml").as_deref(),
            Some("word/document.xml")
        );
        assert_eq!(
            resolve("", "/word/document.xml").as_deref(),
            Some("word/document.xml")
        );
        assert_eq!(
            resolve("word/document.xml", "media/a.png").as_deref(),
            Some("word/media/a.png")
        );
        assert_eq!(
            resolve("word/document.xml", "../customXml/item1.xml").as_deref(),
            Some("customXml/item1.xml")
        );
        assert_eq!(
            resolve("ppt/slides/slide1.xml", "/ppt/media/x.png#frag").as_deref(),
            Some("ppt/media/x.png")
        );
        assert_eq!(resolve("word/document.xml", "../../x.xml"), None);
        assert_eq!(resolve("", "../x.xml"), None);
    }
}
