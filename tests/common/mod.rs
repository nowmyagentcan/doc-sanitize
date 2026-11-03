//! Running the binary on files made at test time.

#![allow(dead_code)]

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
    let dir =
        Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("{name}-{}-{n}", std::process::id()));
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

fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xEDB8_8320 & (!(crc & 1)).wrapping_add(1));
        }
    }
    !crc
}

/// A ZIP of stored entries, written by hand so tests can build the malformed ones. With
/// `zip64`, every header carries zip64 sizes and offsets and the end has zip64 records.
pub fn zip_of(entries: &[Entry], zip64: bool) -> Vec<u8> {
    let mut out = vec![];
    let mut dir = vec![];
    let mut listed = 0u16;
    for e in entries {
        let offset = out.len() as u64;
        let crc = crc32(&e.data);
        let size = e.data.len() as u64;
        let (size32, offset32) = if zip64 {
            (u32::MAX, u32::MAX)
        } else {
            (size as u32, offset as u32)
        };
        let mut local_extra = vec![];
        if zip64 {
            local_extra.extend(1u16.to_le_bytes());
            local_extra.extend(16u16.to_le_bytes());
            local_extra.extend(size.to_le_bytes());
            local_extra.extend(size.to_le_bytes());
        }
        out.extend(b"PK\x03\x04");
        out.extend(if zip64 { 45u16 } else { 20u16 }.to_le_bytes());
        out.extend(e.flags.to_le_bytes());
        out.extend([0u8; 6]); // method 0 (stored), time, date
        out.extend(crc.to_le_bytes());
        out.extend(size32.to_le_bytes());
        out.extend(size32.to_le_bytes());
        out.extend((e.name.len() as u16).to_le_bytes());
        out.extend((local_extra.len() as u16).to_le_bytes());
        out.extend(e.name.as_bytes());
        out.extend(&local_extra);
        out.extend(&e.data);
        if e.unlisted {
            continue;
        }
        listed += 1;
        let name = e.central_name.as_deref().unwrap_or(&e.name);
        let mut extra = vec![];
        if zip64 {
            extra.extend(1u16.to_le_bytes());
            extra.extend(24u16.to_le_bytes());
            extra.extend(size.to_le_bytes());
            extra.extend(size.to_le_bytes());
            extra.extend(offset.to_le_bytes());
        }
        if let Some(path) = &e.unicode_path {
            extra.extend(0x7075u16.to_le_bytes());
            extra.extend((5 + path.len() as u16).to_le_bytes());
            extra.push(1);
            extra.extend(crc32(name.as_bytes()).to_le_bytes());
            extra.extend(path.as_bytes());
        }
        dir.extend(b"PK\x01\x02");
        dir.extend(if zip64 { 45u16 } else { 20u16 }.to_le_bytes());
        dir.extend(if zip64 { 45u16 } else { 20u16 }.to_le_bytes());
        dir.extend(e.flags.to_le_bytes());
        dir.extend([0u8; 6]);
        dir.extend(crc.to_le_bytes());
        dir.extend(size32.to_le_bytes());
        dir.extend(size32.to_le_bytes());
        dir.extend((name.len() as u16).to_le_bytes());
        dir.extend((extra.len() as u16).to_le_bytes());
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
        out.extend([45, 0, 45, 0]);
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
    let (size32, offset32) = if zip64 {
        (u32::MAX, u32::MAX)
    } else {
        (cd_size as u32, cd_offset as u32)
    };
    out.extend(size32.to_le_bytes());
    out.extend(offset32.to_le_bytes());
    out.extend([0u8; 2]);
    out
}

pub fn zip(entries: &[Entry]) -> Vec<u8> {
    zip_of(entries, false)
}

/// `bytes` with the first `from` after offset `start` replaced by `to` (same length).
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
