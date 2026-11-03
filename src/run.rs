//! Checking one file: read it, identify it and run its checks on a worker thread with a large
//! stack, where a panic becomes that file's `internal` error and the next file still runs.

use std::any::Any;
use std::backtrace::Backtrace;
use std::cell::RefCell;
use std::fs;
use std::panic::{self, AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::Once;
use std::thread;

use crate::input::{self, Kind};
use crate::office::clean::Cleaned;
use crate::office::{self, Package};
use crate::pdf;
use crate::report::{
    CheckId, CleanResult, CleanStatus, Error, FileResult, Format, Recheck, Status, Warning,
    removed, safe,
};

const WORKER: &str = "doc-sanitize-worker";
const WORKER_STACK: usize = 64 << 20;

thread_local! {
    /// Where the last panic on this thread happened, and its stack, for `--debug`.
    static PANIC_AT: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// What the person asked for that changes a check's result.
#[derive(Debug, Default)]
pub struct Options {
    /// Put each finding's text in its `value`.
    pub show_values: bool,
    /// The PDF user password to try after the empty one.
    pub password: Option<String>,
}

/// Checks the file at `path`. A caught panic comes back as `internal`, with where it happened
/// as the second value.
pub fn check(path: &Path, options: &Options) -> (FileResult, Option<String>) {
    let name = safe(&path.to_string_lossy());
    let result = on_worker(|| {
        let bytes = input::load(path)?;
        examine(&name, Some(path), &bytes, options).map(|(file, _)| file)
    });
    match result {
        Ok(Ok(file)) => (file, None),
        Ok(Err(e)) => (FileResult::failed(name, e), None),
        Err(at) => (FileResult::failed(name, Error::internal()), Some(at)),
    }
}

/// A file opened for its checks.
enum Opened<'b> {
    Pdf,
    Office(Box<Package<'b>>),
}

/// Identifies `bytes` and runs every check of its format; `path`, when given, is the name
/// whose extension must match the variant.
fn examine<'b>(
    name: &str,
    path: Option<&Path>,
    bytes: &'b [u8],
    options: &Options,
) -> Result<(FileResult, Opened<'b>), Error> {
    let mut warnings = vec![];
    let (format, variant, ran, found, opened): (_, _, &[CheckId], _, _) = match input::sniff(bytes)?
    {
        Kind::Pdf => {
            let pdf = pdf::open(bytes, options.password.as_deref())?;
            if pdf.repaired {
                warnings.push(Warning::repaired());
            }
            let found = pdf::check(&pdf, bytes, options.show_values);
            (Format::Pdf, "pdf", &pdf::CHECKS, found, Opened::Pdf)
        }
        Kind::Zip => {
            let pkg = Package::open(bytes)?;
            let findings = office::check(&pkg, options.show_values)?;
            let found = pdf::Found {
                findings,
                incomplete: vec![],
            };
            let ran = office::checks(pkg.format);
            (
                pkg.format,
                pkg.variant,
                ran,
                found,
                Opened::Office(Box::new(pkg)),
            )
        }
    };
    if let Some(w) = path.and_then(|p| mismatch(name, p, variant)) {
        warnings.insert(0, w);
    }
    let file = FileResult::checked(
        name.into(),
        format,
        variant,
        ran,
        found.findings,
        &found.incomplete,
        warnings,
    );
    Ok((file, opened))
}

/// Where clean writes a copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// `<stem>.clean.<ext>` beside the input, or `-2` … `-999` when that is taken.
    Beside,
    /// The `-o` file.
    File(PathBuf),
    /// `--out-dir`, with names as beside the input.
    Dir(PathBuf),
}

/// What clean is asked to do beyond checking.
#[derive(Debug)]
pub struct CleanOptions {
    pub check: Options,
    /// Checks to leave in place.
    pub keep: Vec<CheckId>,
    pub target: Target,
    /// Replace an existing `-o` file.
    pub overwrite: bool,
}

/// Cleans the file at `path`: checks it, removes every found check but the kept ones,
/// re-checks the copy and writes it only when nothing is left outside the kept checks.
pub fn clean(path: &Path, options: &CleanOptions) -> (CleanResult, Option<String>) {
    let name = safe(&path.to_string_lossy());
    let unread = |e: Error| {
        Box::new(CleanResult::failed(
            FileResult::failed(name.clone(), e.clone()),
            e,
        ))
    };
    let result = on_worker(|| {
        let bytes = input::load(path).map_err(unread)?;
        let (checked, opened) =
            examine(&name, Some(path), &bytes, &options.check).map_err(unread)?;
        let ran: Vec<CheckId> = checked.checks.iter().map(|c| c.id).collect();
        let keep: Vec<CheckId> = ran
            .iter()
            .copied()
            .filter(|c| options.keep.contains(c))
            .collect();
        let remove: Vec<CheckId> = ran.iter().copied().filter(|c| !keep.contains(c)).collect();
        let cleaned = match opened {
            Opened::Office(pkg) => office::clean::clean(&pkg, &checked.findings, &remove),
            Opened::Pdf => Err(Error::cannot_clean(&[(
                CheckId::Metadata,
                "this version can't clean PDF files yet".into(),
                "Check it with doc-sanitize check instead.".into(),
            )])),
        };
        let cleaned = match cleaned {
            Ok(c) => c,
            Err(e) => return Err(Box::new(CleanResult::failed(checked, e))),
        };
        Ok(conclude(path, checked, &keep, cleaned, options))
    });
    match result {
        Ok(Ok(r)) => (r, None),
        Ok(Err(r)) => (*r, None),
        Err(at) => (*unread(Error::internal()), Some(at)),
    }
}

/// Re-checks the copy of the file at `path` (whose check is `checked`) and writes it when the
/// re-check finds nothing outside the `keep` checks.
fn conclude(
    path: &Path,
    checked: FileResult,
    keep: &[CheckId],
    cleaned: Cleaned,
    options: &CleanOptions,
) -> CleanResult {
    let recheck = match examine(&checked.path, None, &cleaned.bytes, &options.check) {
        Ok((copy, _)) => Recheck::of(copy, keep),
        Err(_) => return CleanResult::failed(checked, Error::internal()),
    };
    let found = |c: &CheckId| checked.findings.iter().any(|f| f.check == *c);
    let mut result = CleanResult {
        path: checked.path.clone(),
        format: checked.format,
        variant: checked.variant,
        output: None,
        status: match recheck.status {
            Status::Incomplete => CleanStatus::CleanedIncomplete,
            _ => CleanStatus::Cleaned,
        },
        removed: removed(&checked.findings, keep),
        kept: keep.iter().copied().filter(found).collect(),
        changes: cleaned.changes,
        recheck: None,
        warnings: checked.warnings,
        error: None,
    };
    let error = match recheck.findings.first() {
        Some(f) => Some(Error::recheck_failed(f.check)),
        None => save(path, &cleaned.bytes, cleaned.variant, options)
            .map(|out| result.output = Some(out))
            .err(),
    };
    result.recheck = Some(recheck);
    if let Some(e) = error {
        result.status = CleanStatus::NotCleaned;
        result.output = None;
        result.error = Some(e.named(&result.path));
    }
    result
}

/// Writes the copy of `input` where `options` say and gives the path written, as shown.
fn save(
    input: &Path,
    bytes: &[u8],
    variant: &str,
    options: &CleanOptions,
) -> Result<String, Error> {
    let out = output(input, variant, &options.target);
    let shown = safe(&out.to_string_lossy());
    let replace = options.overwrite && matches!(options.target, Target::File(_));
    if fs::symlink_metadata(&out).is_ok() && !replace {
        return Err(Error::output_exists(&shown));
    }
    if let Target::Dir(dir) = &options.target {
        fs::create_dir_all(dir).map_err(|e| Error::write_failed(&shown, e))?;
    }
    write(&out, bytes).map_err(|e| Error::write_failed(&shown, e))?;
    Ok(shown)
}

/// Where the copy of `input` goes: the `-o` file, or `<stem>.clean.<ext>` (the variant's
/// extension) beside the input or in `--out-dir`, with `-2` … `-999` while the name is taken.
fn output(input: &Path, variant: &str, target: &Target) -> PathBuf {
    let dir = match target {
        Target::File(f) => return f.clone(),
        Target::Dir(d) => d.clone(),
        Target::Beside => input.parent().map(Path::to_path_buf).unwrap_or_default(),
    };
    let stem = input.file_stem().unwrap_or_default().to_string_lossy();
    let mut out = dir.join(format!("{stem}.clean.{variant}"));
    for n in 2..=999 {
        if fs::symlink_metadata(&out).is_err() {
            break;
        }
        out = dir.join(format!("{stem}.clean-{n}.{variant}"));
    }
    out
}

/// Writes `bytes` to `<out>.tmp-<pid>` and renames it to `out`, which replaces a link rather
/// than writing through it; the temp file is deleted on any failure.
fn write(out: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut tmp = out.as_os_str().to_owned();
    tmp.push(format!(".tmp-{}", std::process::id()));
    let tmp = PathBuf::from(tmp);
    let done = fs::write(&tmp, bytes).and_then(|()| fs::rename(&tmp, out));
    if done.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    done
}

/// `extension_mismatch` when the file's extension isn't its variant's.
fn mismatch(name: &str, path: &Path, variant: &str) -> Option<Warning> {
    let ext = path.extension()?.to_string_lossy().to_lowercase();
    (ext != variant).then(|| Warning {
        code: "extension_mismatch",
        message: format!("{name} is a .{variant} file with a .{} name.", safe(&ext)),
    })
}

/// Runs `f` on a worker thread with a 64 MiB stack. A panic there prints nothing and comes
/// back as where it happened, with its message and stack.
fn on_worker<T: Send>(f: impl FnOnce() -> T + Send) -> Result<T, String> {
    static HOOK: Once = Once::new();
    HOOK.call_once(|| {
        let default = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            if thread::current().name() == Some(WORKER) {
                let at = info.location().map(ToString::to_string).unwrap_or_default();
                let stack = Backtrace::force_capture();
                PANIC_AT.set(Some(format!("{at}\n{stack}")));
            } else {
                default(info);
            }
        }));
    });
    thread::scope(|scope| {
        let worker = thread::Builder::new()
            .name(WORKER.into())
            .stack_size(WORKER_STACK)
            .spawn_scoped(scope, || {
                catch_unwind(AssertUnwindSafe(f)).map_err(|panic| {
                    let at = PANIC_AT.take().unwrap_or_default();
                    format!("panicked: {}, at {at}", message(&*panic))
                })
            })
            .map_err(|e| format!("can't start a worker thread: {e}"))?;
        worker.join().expect("the worker catches its panics")
    })
}

fn message(payload: &(dyn Any + Send)) -> &str {
    match (
        payload.downcast_ref::<&str>(),
        payload.downcast_ref::<String>(),
    ) {
        (Some(s), _) => s,
        (_, Some(s)) => s,
        _ => "a panic without a message",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_panic_is_caught_and_the_next_job_still_runs() {
        let at = on_worker(|| -> u8 { panic!("boom") }).unwrap_err();
        assert!(at.starts_with("panicked: boom, at src"), "{at}");
        assert_eq!(on_worker(|| 7), Ok(7));
    }

    #[test]
    fn a_deep_stack_fits_on_the_worker() {
        fn depth(n: u32) -> u32 {
            let pad = std::hint::black_box([0u8; 512]);
            if n == 0 {
                u32::from(pad[0])
            } else {
                depth(n - 1) + 1 + u32::from(pad[1])
            }
        }
        assert_eq!(on_worker(|| depth(20_000)), Ok(20_000));
    }

    #[test]
    fn a_copy_that_still_holds_a_finding_is_never_written() {
        let dir = std::env::temp_dir().join(format!("doc-sanitize-recheck-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let input = dir.join("a.pdf");
        let bytes = b"%PDF-1.4
1 0 obj
<< /Type /Catalog /Pages 2 0 R >>
endobj
2 0 obj
<< /Type /Pages /Kids [] /Count 0 >>
endobj
trailer
<< /Root 1 0 R /Info << /Author (Avery Example) >> >>
%%EOF
";
        fs::write(&input, bytes).unwrap();
        let options = CleanOptions {
            check: Options::default(),
            keep: vec![],
            target: Target::Beside,
            overwrite: false,
        };
        let (checked, _) = examine("a.pdf", Some(&input), bytes, &options.check).unwrap();
        assert!(
            checked
                .findings
                .iter()
                .any(|f| f.check == CheckId::Metadata)
        );
        let held = Cleaned {
            bytes: bytes.to_vec(),
            variant: "pdf",
            changes: vec![],
        };
        let r = conclude(&input, checked, &[], held, &options);
        assert_eq!(r.status, CleanStatus::NotCleaned);
        assert_eq!(r.output, None);
        let e = r.error.unwrap();
        assert_eq!(e.code, "recheck_failed");
        assert_eq!(
            e.message,
            "The cleaned copy of a.pdf still held metadata, so it wasn't saved."
        );
        assert!(!dir.join("a.clean.pdf").exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_extension_must_match_the_variant() {
        let w = mismatch("fake.docx", Path::new("fake.docx"), "pdf").unwrap();
        assert_eq!(w.message, "fake.docx is a .pdf file with a .docx name.");
        assert!(mismatch("a.PDF", Path::new("a.PDF"), "pdf").is_none());
        assert!(mismatch("clean", Path::new("clean"), "docx").is_none());
    }
}
