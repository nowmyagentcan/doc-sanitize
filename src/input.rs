//! The files to check: paths and folders expanded in order, each file read within the size
//! limit, and what it is, from its bytes and never from its name.

use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::report::Error;

/// The largest input file.
pub const MAX_FILE: u64 = 256 << 20;

/// The most files one run checks.
pub const MAX_FILES: usize = 10_000;

/// The extensions a folder is checked for; files named explicitly are read whatever their name.
const EXTENSIONS: [&str; 15] = [
    "pdf", "docx", "docm", "dotx", "dotm", "xlsx", "xlsm", "xltx", "xltm", "pptx", "pptm", "potx",
    "potm", "ppsx", "ppsm",
];

/// One file to check, or a path that gave no file to check.
#[derive(Debug)]
pub enum Input {
    File(PathBuf),
    Failed(PathBuf, Error),
}

impl Input {
    pub fn path(&self) -> &Path {
        match self {
            Input::File(p) | Input::Failed(p, _) => p,
        }
    }
}

/// The files `paths` name, in order. A folder gives its files with a known extension, sorted
/// by path (with `recursive`, from every subfolder), leaving out our own `*.clean.*` and
/// `*.clean-N.*` copies and anything that isn't a regular file; symlinks inside folders are
/// never followed. A folder with none is one failed input.
pub fn expand(paths: &[PathBuf], recursive: bool) -> Result<Vec<Input>, Error> {
    let mut inputs = vec![];
    for path in paths {
        if fs::metadata(path).is_ok_and(|m| m.is_dir()) {
            let mut files = vec![];
            folder(path, recursive, &mut files, &mut inputs);
            if files.is_empty() && !inputs.iter().any(|i| i.path() == path) {
                inputs.push(Input::Failed(path.clone(), Error::empty_folder()));
            }
            files.sort();
            inputs.extend(files.into_iter().map(Input::File));
        } else {
            inputs.push(Input::File(path.clone()));
        }
        if inputs.len() > MAX_FILES {
            return Err(Error::usage(
                format!("More than {MAX_FILES} files to check."),
                "Check fewer files or folders at a time.",
            ));
        }
    }
    Ok(inputs)
}

/// Collects the files of `dir` into `files`, walking with a stack so no tree is too deep; a
/// folder that can't be listed is a failed input.
fn folder(dir: &Path, recursive: bool, files: &mut Vec<PathBuf>, inputs: &mut Vec<Input>) {
    let mut todo = vec![dir.to_path_buf()];
    while let Some(dir) = todo.pop() {
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) => {
                inputs.push(Input::Failed(dir, Error::unreadable(e)));
                continue;
            }
        };
        for entry in entries.flatten() {
            let path = entry.path();
            match entry.file_type() {
                Ok(t) if t.is_dir() && recursive => todo.push(path),
                Ok(t) if t.is_file() && wanted(&path) => files.push(path),
                _ => {}
            }
        }
        if files.len() > MAX_FILES {
            return;
        }
    }
}

/// A known extension, and not one of our own cleaned copies.
fn wanted(path: &Path) -> bool {
    let known = path
        .extension()
        .is_some_and(|e| EXTENSIONS.iter().any(|k| e.eq_ignore_ascii_case(k)));
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    let copy = stem.rsplit_once(".clean").is_some_and(|(_, rest)| {
        rest.is_empty()
            || rest
                .strip_prefix('-')
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    });
    known && !copy
}

/// The whole file, refused before reading when it is over 256 MiB.
pub fn load(path: &Path) -> Result<Vec<u8>, Error> {
    // Looked at before opening: opening a FIFO would wait for a writer.
    let meta = fs::metadata(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound if fs::symlink_metadata(path).is_err() => Error::not_found(),
        _ => Error::unreadable(e),
    })?;
    if !meta.is_file() {
        return Err(Error::unreadable("it isn't a regular file"));
    }
    if meta.len() > MAX_FILE {
        return Err(too_big());
    }
    let file = File::open(path).map_err(Error::unreadable)?;
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    file.take(MAX_FILE + 1)
        .read_to_end(&mut bytes)
        .map_err(Error::unreadable)?;
    if bytes.len() as u64 > MAX_FILE {
        return Err(too_big());
    }
    Ok(bytes)
}

fn too_big() -> Error {
    Error::too_large("the file", "256 MiB")
}

/// What a file's bytes say it is.
#[derive(Debug, PartialEq, Eq)]
pub enum Kind {
    Pdf,
    /// A ZIP, which may be an Office package.
    Zip,
}

/// Identifies a file: `%PDF-` in the first 1,024 bytes, a CFB container (legacy or
/// password-protected Office), or a ZIP end record.
pub fn sniff(bytes: &[u8]) -> Result<Kind, Error> {
    let head = &bytes[..bytes.len().min(1024)];
    if head.windows(5).any(|w| w == b"%PDF-") {
        return Ok(Kind::Pdf);
    }
    if bytes.starts_with(&[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1]) {
        return Err(if cfb_has(bytes, "EncryptedPackage") {
            Error::office_password()
        } else {
            Error::legacy_office()
        });
    }
    if bytes.windows(4).any(|w| w == b"PK\x05\x06") {
        return Ok(Kind::Zip);
    }
    Err(Error::unsupported_format())
}

/// Whether a CFB (compound file) container has a stream called `name` in its directory.
/// Follows the directory's sector chain through the FAT, within the file's bounds.
fn cfb_has(b: &[u8], name: &str) -> bool {
    let u32_at = |at: usize| {
        b.get(at..at + 4)
            .map(|s| u32::from_le_bytes(s.try_into().unwrap()))
    };
    let shift = b.get(30).copied().unwrap_or(0);
    if !(7..=16).contains(&shift) {
        return false;
    }
    let size = 1usize << shift;
    let sector = |id: u32| {
        let start = (id as usize + 1).checked_mul(size)?;
        b.get(start..start.checked_add(size)?)
    };
    let sectors = b.len() / size;
    // The FAT's own sectors: 109 listed in the header, the rest in the DIFAT chain.
    let mut fat_sectors: Vec<u32> = (0..109).filter_map(|i| u32_at(76 + 4 * i)).collect();
    let mut difat = u32_at(68).unwrap_or(u32::MAX);
    for _ in 0..sectors {
        let Some(s) = sector(difat) else { break };
        let ids: Vec<u32> = s
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| u32::from_le_bytes(*c))
            .collect();
        fat_sectors.extend(&ids[..ids.len() - 1]);
        difat = ids[ids.len() - 1];
    }
    let per = size / 4;
    let next = |id: u32| {
        let fat = *fat_sectors.get(id as usize / per)?;
        let at = (fat as usize + 1).checked_mul(size)? + (id as usize % per) * 4;
        u32_at(at)
    };
    let want: Vec<u16> = name.encode_utf16().collect();
    let mut id = u32_at(48).unwrap_or(u32::MAX);
    for _ in 0..sectors {
        let Some(s) = sector(id) else { return false };
        for entry in s.as_chunks::<128>().0 {
            let len = usize::from(u16::from_le_bytes([entry[64], entry[65]])).min(64);
            let units: Vec<u16> = entry[..len]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| u16::from_le_bytes(*c))
                .collect();
            if entry[66] == 2 && units.strip_suffix(&[0]).unwrap_or(&units) == want {
                return true;
            }
        }
        match next(id) {
            Some(n) => id = n,
            None => return false,
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A one-sector-FAT CFB file with a root entry and one stream named `stream`.
    pub fn cfb(stream: &str) -> Vec<u8> {
        let mut b = vec![0u8; 512 * 3];
        b[..8].copy_from_slice(&[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1]);
        b[30] = 9;
        b[44..48].copy_from_slice(&1u32.to_le_bytes());
        b[48..52].copy_from_slice(&1u32.to_le_bytes());
        b[68..72].copy_from_slice(&u32::MAX.to_le_bytes());
        for i in 0..109 {
            let v = if i == 0 { 0 } else { u32::MAX };
            b[76 + 4 * i..80 + 4 * i].copy_from_slice(&v.to_le_bytes());
        }
        for (i, v) in [0xFFFF_FFFDu32, 0xFFFF_FFFE].iter().enumerate() {
            b[512 + 4 * i..516 + 4 * i].copy_from_slice(&v.to_le_bytes());
        }
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

    #[test]
    fn content_decides_never_the_name() {
        assert_eq!(sniff(b"%PDF-1.7\n"), Ok(Kind::Pdf));
        let late = [vec![b' '; 1019], b"%PDF-".to_vec()].concat();
        assert_eq!(sniff(&late), Ok(Kind::Pdf));
        let too_late = [vec![b' '; 1020], b"%PDF-".to_vec()].concat();
        assert_eq!(sniff(&too_late).unwrap_err().code, "unsupported_format");
        assert_eq!(sniff(b"").unwrap_err().code, "unsupported_format");
        assert_eq!(
            sniff(b"{\\rtf1 hello}").unwrap_err().code,
            "unsupported_format"
        );
        assert_eq!(sniff(b"x PK\x05\x06 y"), Ok(Kind::Zip));
    }

    #[test]
    fn compound_files_are_legacy_or_password_protected_office() {
        assert_eq!(sniff(&cfb("WordDocument")), Err(Error::legacy_office()));
        assert_eq!(
            sniff(&cfb("EncryptedPackage")),
            Err(Error::office_password())
        );
        // The name as body text, outside the directory, doesn't count.
        let mut doc = cfb("WordDocument");
        doc.extend("EncryptedPackage".encode_utf16().flat_map(u16::to_le_bytes));
        assert_eq!(sniff(&doc), Err(Error::legacy_office()));
        // A directory chain that loops or points past the end stops.
        let mut looped = cfb("WordDocument");
        looped[516..520].copy_from_slice(&1u32.to_le_bytes());
        assert_eq!(sniff(&looped), Err(Error::legacy_office()));
        let mut short = cfb("EncryptedPackage");
        short.truncate(600);
        assert_eq!(sniff(&short), Err(Error::legacy_office()));
    }

    #[test]
    fn folders_skip_our_copies_and_unknown_extensions() {
        for (name, want) in [
            ("a.docx", true),
            ("A.PDF", true),
            ("a.clean.docx", false),
            ("a.clean-12.xlsx", false),
            ("a.clean-.xlsx", true),
            ("a.cleaner.pptx", true),
            ("a.clean-x.pptx", true),
            ("notes.txt", false),
            ("noext", false),
        ] {
            assert_eq!(wanted(Path::new(name)), want, "{name}");
        }
    }
}
