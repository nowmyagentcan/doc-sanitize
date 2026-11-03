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
