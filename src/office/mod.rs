//! Office packages (OPC): ZIP files of XML parts tied together by relationships. Opening one
//! scans the ZIP, inflates every entry once within the limits, reads the content types and
//! relationships, decides the variant from the main part's type and checks that every XML part
//! is well-formed within the XML limits.

pub mod common;
pub mod xml;
pub mod zipscan;

use std::collections::{HashMap, HashSet};
use std::io::{self, Read, Write};

use flate2::CrcWriter;
use flate2::read::DeflateDecoder;
use quick_xml::events::Event;

use crate::report::{Error, Format, safe};
use zipscan::{Entry, Zip};

/// The most one entry may inflate to, and the whole package.
const MAX_PART: u64 = 256 << 20;
const MAX_PACKAGE: u64 = 1 << 30;

const CONTENT_TYPES: &str = "http://schemas.openxmlformats.org/package/2006/content-types";
const RELATIONSHIPS: &str = "http://schemas.openxmlformats.org/package/2006/relationships";
const OFFICE_DOCUMENT: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument";
const STRICT: &str = "http://purl.oclc.org/ooxml/";

/// The variant each main part's content type makes a package.
const MAIN_TYPES: [(&str, Format, &str); 14] = [
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
        for e in &pkg.zip.entries {
            let xml = pkg
                .content_type(&e.name)
                .is_some_and(|t| t.ends_with("xml"));
            if part_name(&e.name) && (xml || e.name.to_ascii_lowercase().ends_with(".rels")) {
                let bytes = pkg.read(&e.name)?;
                xml::Reader::new(&xml::decode(&bytes, &e.name)?, &e.name).validate()?;
            }
        }
        Ok(pkg)
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

    /// The parts reachable from the package's relationships through internal targets, as
    /// part names in the package's own spelling, in the order they are found.
    pub fn reachable(&self) -> Result<Vec<String>, Error> {
        let mut seen = HashSet::new();
        let mut found = vec![];
        let mut pending = vec![String::new()];
        while let Some(source) = pending.pop() {
            for rel in self.rels(&source)? {
                let Some(target) = resolve(&source, &rel.target).filter(|_| !rel.external) else {
                    continue;
                };
                if let Some(&i) = self.index.get(&key(&target))
                    && seen.insert(i)
                {
                    let name = self.zip.entries[i].name.clone();
                    found.push(name.clone());
                    pending.push(name);
                }
            }
        }
        Ok(found)
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
