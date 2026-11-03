//! Running the binary on files made at test time, and builders for those files.

#![allow(dead_code)]

pub mod pkg;
#[allow(unused_imports)]
pub use pkg::*;

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

/// A minimal PDF: only its header matters until PDFs are parsed.
pub const PDF: &[u8] =
    b"%PDF-1.7\n1 0 obj << /Type /Catalog >> endobj\ntrailer << /Root 1 0 R >>\n%%EOF\n";

/// A fresh empty folder for one test.
pub fn dir(name: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("{name}-{n}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Writes `files` (name, bytes) into `dir`, creating subfolders.
pub fn put(dir: &Path, files: &[(&str, &[u8])]) {
    for (name, bytes) in files {
        let path = dir.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }
}

pub fn bin() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_doc-sanitize"));
    c.env_remove("NO_COLOR").stdin(Stdio::null());
    c
}

/// Runs the binary in `dir` with `args`.
pub fn run_in(dir: &Path, args: &[&str]) -> Output {
    bin().current_dir(dir).args(args).output().unwrap()
}

pub fn run(args: &[&str]) -> Output {
    bin().args(args).output().unwrap()
}

pub fn stdout(o: &Output) -> String {
    String::from_utf8(o.stdout.clone()).unwrap()
}

pub fn stderr(o: &Output) -> String {
    String::from_utf8(o.stderr.clone()).unwrap()
}

pub fn json(o: &Output) -> serde_json::Value {
    serde_json::from_slice(&o.stdout).unwrap_or_else(|e| panic!("{e}: {}", stdout(o)))
}

/// One ZIP entry for [`zip`]: stored, listed in the directory under its own name, unless a
/// field says otherwise.
#[derive(Clone, Default)]
pub struct Entry {
    pub name: String,
    pub data: Vec<u8>,
    /// Deflated data with its CRC and inflated size, written in place of `data`.
    pub deflated: Option<(Vec<u8>, u32, u64)>,
    /// The name the central directory gives it, when that differs from the local header's.
    pub central_name: Option<String>,
    /// A local header and data with no directory record.
    pub unlisted: bool,
    /// General-purpose flags in both headers (1 = encrypted).
    pub flags: u16,
    /// An Info-ZIP Unicode-path field in the directory record.
    pub unicode_path: Option<String>,
}

pub fn entry(name: &str, data: &[u8]) -> Entry {
    Entry {
        name: name.into(),
        data: data.into(),
        ..Entry::default()
    }
}

/// An entry whose data is `size` zero bytes, deflated as it is made.
pub fn zeros(name: &str, size: u64) -> Entry {
    use std::io::Write;
    let mut z = flate2::write::DeflateEncoder::new(vec![], flate2::Compression::best());
    let mut crc = flate2::Crc::new();
    let chunk = vec![0u8; 1 << 20];
    let mut left = size;
    while left > 0 {
        let n = left.min(chunk.len() as u64) as usize;
        z.write_all(&chunk[..n]).unwrap();
        crc.update(&chunk[..n]);
        left -= n as u64;
    }
    Entry {
        name: name.into(),
        deflated: Some((z.finish().unwrap(), crc.sum(), size)),
        ..Entry::default()
    }
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = flate2::Crc::new();
    crc.update(data);
    crc.sum()
}

/// A ZIP written by hand so tests can build the malformed ones. With `zip64`, every header
/// carries zip64 sizes and offsets and the end has zip64 records.
pub fn zip_of(entries: &[Entry], zip64: bool) -> Vec<u8> {
    let mut out = vec![];
    let mut dir = vec![];
    let mut listed = 0u16;
    let version = if zip64 { 45u16 } else { 20 }.to_le_bytes();
    for e in entries {
        let offset = out.len() as u64;
        let (body, method, crc, size) = match &e.deflated {
            Some((body, crc, size)) => (body.as_slice(), 8u16, *crc, *size),
            None => (e.data.as_slice(), 0, crc32(&e.data), e.data.len() as u64),
        };
        let csize = body.len() as u64;
        let [csize32, size32, offset32] = if zip64 {
            [u32::MAX; 3]
        } else {
            [csize as u32, size as u32, offset as u32]
        };
        let mut local_extra = vec![];
        let mut extra = vec![];
        if zip64 {
            local_extra.extend([1, 0, 16, 0]);
            local_extra.extend(size.to_le_bytes());
            local_extra.extend(csize.to_le_bytes());
            extra.extend([1, 0, 24, 0]);
            extra.extend(size.to_le_bytes());
            extra.extend(csize.to_le_bytes());
            extra.extend(offset.to_le_bytes());
        }
        let name = e.central_name.as_deref().unwrap_or(&e.name);
        if let Some(path) = &e.unicode_path {
            extra.extend(0x7075u16.to_le_bytes());
            extra.extend((5 + path.len() as u16).to_le_bytes());
            extra.push(1);
            extra.extend(crc32(name.as_bytes()).to_le_bytes());
            extra.extend(path.as_bytes());
        }
        // Both headers: flags, method, time and date (0), CRC, sizes, name and extra lengths.
        let fields = |extra: &[u8], name: &str| {
            let mut f = e.flags.to_le_bytes().to_vec();
            f.extend(method.to_le_bytes());
            f.extend([0u8; 4]);
            f.extend(crc.to_le_bytes());
            f.extend(csize32.to_le_bytes());
            f.extend(size32.to_le_bytes());
            f.extend((name.len() as u16).to_le_bytes());
            f.extend((extra.len() as u16).to_le_bytes());
            f
        };
        out.extend(b"PK\x03\x04");
        out.extend(version);
        out.extend(fields(&local_extra, &e.name));
        out.extend(e.name.as_bytes());
        out.extend(&local_extra);
        out.extend(body);
        if e.unlisted {
            continue;
        }
        listed += 1;
        dir.extend(b"PK\x01\x02");
        dir.extend(version);
        dir.extend(version);
        dir.extend(fields(&extra, name));
        dir.extend([0u8; 10]); // comment length, disk, attributes
        dir.extend(offset32.to_le_bytes());
        dir.extend(name.as_bytes());
        dir.extend(&extra);
    }
    let cd_offset = out.len() as u64;
    let cd_size = dir.len() as u64;
    out.extend(&dir);
    if zip64 {
        let record = out.len() as u64;
        out.extend(b"PK\x06\x06");
        out.extend(44u64.to_le_bytes());
        out.extend(version);
        out.extend(version);
        out.extend([0u8; 8]);
        out.extend(u64::from(listed).to_le_bytes());
        out.extend(u64::from(listed).to_le_bytes());
        out.extend(cd_size.to_le_bytes());
        out.extend(cd_offset.to_le_bytes());
        out.extend(b"PK\x06\x07");
        out.extend([0u8; 4]);
        out.extend(record.to_le_bytes());
        out.extend(1u32.to_le_bytes());
    }
    out.extend(b"PK\x05\x06");
    out.extend([0u8; 4]);
    let count = if zip64 { u16::MAX } else { listed };
    out.extend(count.to_le_bytes());
    out.extend(count.to_le_bytes());
    let [size32, offset32] = if zip64 {
        [u32::MAX; 2]
    } else {
        [cd_size as u32, cd_offset as u32]
    };
    out.extend(size32.to_le_bytes());
    out.extend(offset32.to_le_bytes());
    out.extend([0u8; 2]);
    out
}

pub fn zip(entries: &[Entry]) -> Vec<u8> {
    zip_of(entries, false)
}

/// `bytes` with the first `from` at or after offset `start` replaced by `to` (same length).
pub fn patch(mut bytes: Vec<u8>, start: usize, from: &[u8], to: &[u8]) -> Vec<u8> {
    let at = start
        + bytes[start..]
            .windows(from.len())
            .position(|w| w == from)
            .expect("found");
    bytes[at..at + to.len()].copy_from_slice(to);
    bytes
}

/// Where the central directory starts in a ZIP made by [`zip`].
pub fn central_start(bytes: &[u8]) -> usize {
    let end = bytes.len() - 22;
    u32::from_le_bytes(bytes[end + 16..end + 20].try_into().unwrap()) as usize
}

pub const W: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
pub const S: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
pub const P: &str = "http://schemas.openxmlformats.org/presentationml/2006/main";
pub const R: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
pub const REL: &str = "http://schemas.openxmlformats.org/package/2006/relationships";

/// Each variant's main part name, content type and a minimal body.
pub fn main_part(variant: &str) -> (&'static str, String, String) {
    let ms = "application/vnd.ms-";
    let ox = "application/vnd.openxmlformats-officedocument.";
    let (name, ct) = match variant {
        "docx" => (
            "word/document.xml",
            format!("{ox}wordprocessingml.document.main+xml"),
        ),
        "docm" => (
            "word/document.xml",
            format!("{ms}word.document.macroEnabled.main+xml"),
        ),
        "dotx" => (
            "word/document.xml",
            format!("{ox}wordprocessingml.template.main+xml"),
        ),
        "dotm" => (
            "word/document.xml",
            format!("{ms}word.template.macroEnabledTemplate.main+xml"),
        ),
        "xlsx" => (
            "xl/workbook.xml",
            format!("{ox}spreadsheetml.sheet.main+xml"),
        ),
        "xlsm" => (
            "xl/workbook.xml",
            format!("{ms}excel.sheet.macroEnabled.main+xml"),
        ),
        "xltx" => (
            "xl/workbook.xml",
            format!("{ox}spreadsheetml.template.main+xml"),
        ),
        "xltm" => (
            "xl/workbook.xml",
            format!("{ms}excel.template.macroEnabled.main+xml"),
        ),
        "xlsb" => (
            "xl/workbook.bin",
            format!("{ms}excel.sheet.binary.macroEnabled.main"),
        ),
        "pptx" => (
            "ppt/presentation.xml",
            format!("{ox}presentationml.presentation.main+xml"),
        ),
        "pptm" => (
            "ppt/presentation.xml",
            format!("{ms}powerpoint.presentation.macroEnabled.main+xml"),
        ),
        "potx" => (
            "ppt/presentation.xml",
            format!("{ox}presentationml.template.main+xml"),
        ),
        "potm" => (
            "ppt/presentation.xml",
            format!("{ms}powerpoint.template.macroEnabled.main+xml"),
        ),
        "ppsx" => (
            "ppt/presentation.xml",
            format!("{ox}presentationml.slideshow.main+xml"),
        ),
        "ppsm" => (
            "ppt/presentation.xml",
            format!("{ms}powerpoint.slideshow.macroEnabled.main+xml"),
        ),
        v => panic!("no variant {v}"),
    };
    let body = match &name[..2] {
        "wo" => format!(
            "<w:document xmlns:w=\"{W}\"><w:body><w:p><w:r><w:t>Hello</w:t></w:r></w:p></w:body></w:document>"
        ),
        "xl" => format!("<workbook xmlns=\"{S}\"><sheets/></workbook>"),
        _ => format!("<p:presentation xmlns:p=\"{P}\"/>"),
    };
    (name, ct, body)
}

/// The entries of a minimal package of `variant`: content types (Defaults for rels, xml, png
/// and jpeg; an Override for the main part and each of `types`), the package relationships,
/// the main part, then `parts`.
pub fn package(variant: &str, types: &[(&str, &str)], parts: &[Entry]) -> Vec<Entry> {
    let (main, ct, body) = main_part(variant);
    let overrides: String = [(main, ct.as_str())]
        .iter()
        .chain(types)
        .map(|(p, t)| format!("<Override PartName=\"/{p}\" ContentType=\"{t}\"/>"))
        .collect();
    let types = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
         <Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\
         <Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\
         <Default Extension=\"xml\" ContentType=\"application/xml\"/>\
         <Default Extension=\"png\" ContentType=\"image/png\"/>\
         <Default Extension=\"jpeg\" ContentType=\"image/jpeg\"/>{overrides}</Types>"
    );
    let mut entries = vec![
        entry("[Content_Types].xml", types.as_bytes()),
        entry(
            "_rels/.rels",
            rels(&[("rId1", "officeDocument", main)]).as_bytes(),
        ),
        entry(main, body.as_bytes()),
    ];
    entries.extend(parts.iter().cloned());
    entries
}

/// A `.rels` part with one relationship per (id, type under the officeDocument namespace,
/// target).
pub fn rels(rels: &[(&str, &str, &str)]) -> String {
    let body: Vec<_> = rels
        .iter()
        .map(|&(id, kind, target)| rel(id, kind, target))
        .collect();
    rels_of(&body)
}

/// A `.rels` part holding `rels` made by [`rel`] or [`external`].
pub fn rels_of(rels: &[String]) -> String {
    format!(
        "<Relationships xmlns=\"{REL}\">{}</Relationships>",
        rels.concat()
    )
}

/// One relationship; a `kind` with no scheme is a type under the officeDocument namespace.
pub fn rel(id: &str, kind: &str, target: &str) -> String {
    let kind = if kind.contains("://") {
        kind.to_string()
    } else {
        format!("{R}/{kind}")
    };
    format!("<Relationship Id=\"{id}\" Type=\"{kind}\" Target=\"{target}\"/>")
}

/// One relationship to a target outside the package.
pub fn external(id: &str, kind: &str, target: &str) -> String {
    rel(id, kind, target).replace("/>", " TargetMode=\"External\"/>")
}

/// `entries` with the part `name` replaced by `data`, or added.
pub fn with(mut entries: Vec<Entry>, name: &str, data: &[u8]) -> Vec<Entry> {
    match entries.iter_mut().find(|e| e.name == name) {
        Some(e) => e.data = data.into(),
        None => entries.push(entry(name, data)),
    }
    entries
}

/// A one-sector-FAT compound file (the legacy Office container) with a root entry and one
/// stream named `stream`.
pub fn cfb(stream: &str) -> Vec<u8> {
    let mut b = vec![0u8; 512 * 3];
    b[..8].copy_from_slice(&[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1]);
    b[30] = 9;
    b[44..48].copy_from_slice(&1u32.to_le_bytes());
    b[48..52].copy_from_slice(&1u32.to_le_bytes());
    b[68..72].copy_from_slice(&u32::MAX.to_le_bytes());
    for i in 1..109 {
        b[76 + 4 * i..80 + 4 * i].copy_from_slice(&u32::MAX.to_le_bytes());
    }
    b[512..520].copy_from_slice(&[0xFD, 0xFF, 0xFF, 0xFF, 0xFE, 0xFF, 0xFF, 0xFF]);
    for (slot, (name, kind)) in [("Root Entry", 5u8), (stream, 2)].iter().enumerate() {
        let e = 1024 + 128 * slot;
        let units: Vec<u8> = name
            .encode_utf16()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect();
        b[e..e + units.len()].copy_from_slice(&units);
        b[e + 64..e + 66].copy_from_slice(&(units.len() as u16).to_le_bytes());
        b[e + 66] = *kind;
    }
    b
}

/// The findings of `check` in a file's JSON entry, after checking that its `checks` entry
/// agrees with them.
pub fn findings_of<'v>(f: &'v serde_json::Value, check: &str) -> Vec<&'v serde_json::Value> {
    let found: Vec<_> = f["findings"]
        .as_array()
        .unwrap_or_else(|| panic!("no findings: {f}"))
        .iter()
        .filter(|x| x["check"] == check)
        .collect();
    let entry = f["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == check)
        .unwrap_or_else(|| panic!("{check} didn't run: {f}"));
    let status = if found.is_empty() {
        "none_found"
    } else {
        "found"
    };
    assert_eq!(entry["status"], status, "{check}: {f}");
    assert_eq!(entry["count"], found.len(), "{check}: {f}");
    found
}

/// Each finding of `check` as (kind, part, count, detail).
pub fn kinds(f: &serde_json::Value, check: &str) -> Vec<(String, String, u64, String)> {
    let s = |v: &serde_json::Value| v.as_str().unwrap_or_default().to_string();
    findings_of(f, check)
        .into_iter()
        .map(|x| {
            let count = x["count"].as_u64().unwrap();
            (
                s(&x["kind"]),
                s(&x["where"]["part"]),
                count,
                s(&x["detail"]),
            )
        })
        .collect()
}

pub fn found(kind: &str, part: &str, count: u64, detail: &str) -> (String, String, u64, String) {
    (kind.into(), part.into(), count, detail.into())
}
