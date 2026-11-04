//! Checking one file: read it, identify it and run its checks on a worker thread with a large
//! stack, where a panic becomes that file's `internal` error and the next file still runs.

use std::any::Any;
use std::backtrace::Backtrace;
use std::cell::RefCell;
use std::fs;
use std::io::ErrorKind;
use std::panic::{self, AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::Once;
use std::thread;

use crate::input::{self, Input, Kind};
use crate::office::{self, Package};
use crate::pdf;
use crate::report::{
    CHECKS, CheckId, CleanResult, CleanStatus, Cleaned, Error, FileResult, Format, Recheck, Status,
    Warning, removed, safe,
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

/// Checks one input; a path that gave no file is that file's error.
pub fn check_input(input: &Input, options: &Options) -> (FileResult, Option<String>) {
    match input {
        Input::File(path) => check(path, options),
        Input::Failed(path, e) => (
            FileResult::failed(safe(&path.to_string_lossy()), e.clone()),
            None,
        ),
    }
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
    Pdf(Box<pdf::Pdf>),
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
            let opened = Opened::Pdf(Box::new(pdf));
            (Format::Pdf, "pdf", &pdf::CHECKS, found, opened)
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

impl CleanOptions {
    /// The options of a clean run over `inputs`, refusing a `keep` id that is unknown or can't
    /// be kept, `output` with `dir` or with several files, and an `output` that is the input.
    pub fn new(
        check: Options,
        keep: &[String],
        output: Option<PathBuf>,
        dir: Option<PathBuf>,
        overwrite: bool,
        inputs: &[Input],
    ) -> Result<Self, Error> {
        let target = match (output, dir) {
            (Some(_), Some(_)) => {
                return Err(Error::usage(
                    "-o and --out-dir can't go together.",
                    "Use one of them.",
                ));
            }
            (Some(file), None) => {
                if inputs.len() != 1 {
                    return Err(Error::usage(
                        format!("-o works with one file; you gave {}.", inputs.len()),
                        "Use --out-dir for several files.",
                    ));
                }
                if same_file(&file, inputs[0].path()) {
                    return Err(replaces_input());
                }
                Target::File(file)
            }
            (None, Some(dir)) => Target::Dir(dir),
            (None, None) => Target::Beside,
        };
        Ok(Self {
            check,
            keep: keep_ids(keep)?,
            target,
            overwrite,
        })
    }
}

fn replaces_input() -> Error {
    Error::usage(
        "The output would replace the input file.",
        "Choose another name with -o.",
    )
}

/// The checks `ids` name, refusing an unknown id and the two checks that can't be kept.
pub fn keep_ids(ids: &[String]) -> Result<Vec<CheckId>, Error> {
    let mut keep = vec![];
    for id in ids.iter().map(|k| k.trim()).filter(|k| !k.is_empty()) {
        match CheckId::parse(id) {
            Some(CheckId::Signatures) => {
                return Err(Error::usage(
                    "Signatures can't be kept: any change to a signed file breaks its signature.",
                    "Leave signatures out of --keep.",
                ));
            }
            Some(CheckId::OldRevisions) => {
                return Err(Error::usage(
                    "Old revisions can't be kept: a cleaned PDF is always written as one revision.",
                    "Leave old-revisions out of --keep.",
                ));
            }
            Some(c) => keep.push(c),
            None => {
                let ids: Vec<_> = CHECKS.iter().map(|c| c.0).collect();
                return Err(Error::usage(
                    format!("Unknown check \"{}\".", safe(id)),
                    &format!("Checks: {}.", ids.join(", ")),
                ));
            }
        }
    }
    Ok(keep)
}

/// Whether two paths name one existing file.
fn same_file(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Cleans one input; a path that gave no file is that file's error.
pub fn clean_input(input: &Input, options: &CleanOptions) -> (CleanResult, Option<String>) {
    match input {
        Input::File(path) => clean(path, options),
        Input::Failed(path, e) => {
            let failed = FileResult::failed(safe(&path.to_string_lossy()), e.clone());
            (CleanResult::failed(failed, e.clone()), None)
        }
    }
}

/// Cleans the file at `path`: checks it, removes every found check but the kept ones,
/// re-checks the copy and writes it only when nothing is left outside the kept checks.
pub fn clean(path: &Path, options: &CleanOptions) -> (CleanResult, Option<String>) {
    let name = safe(&path.to_string_lossy());
    let result = on_worker(|| {
        let bytes = input::load(path).map_err(|e| unread(&name, e))?;
        let store = |copy: Vec<u8>, variants: Variants| save(path, &copy, variants, options);
        remove(&name, path, &bytes, &options.check, &options.keep, store)
    });
    match result {
        Ok(Ok(r)) => (r, None),
        Ok(Err(r)) => (*r, None),
        Err(at) => (*unread(&name, Error::internal()), Some(at)),
    }
}

/// Checks `bytes`, an upload called `name` that stays in memory.
pub fn check_bytes(name: &str, bytes: &[u8], options: &Options) -> FileResult {
    let result = on_worker(|| examine(name, Some(Path::new(name)), bytes, options));
    match result {
        Ok(Ok((file, _))) => file,
        Ok(Err(e)) => FileResult::failed(name.into(), e),
        Err(_) => FileResult::failed(name.into(), Error::internal()),
    }
}

/// Cleans `bytes`, an upload called `name`, as [`clean`] does, giving the copy back instead of
/// writing it; its `output` is the name to save it under.
pub fn clean_bytes(
    name: &str,
    bytes: &[u8],
    options: &Options,
    keep: &[CheckId],
) -> (CleanResult, Option<Vec<u8>>) {
    let path = Path::new(name);
    let mut kept = None;
    let result = on_worker(|| {
        let store = |copy, (_, variant): Variants| {
            kept = Some(copy);
            let stem = path.file_stem().unwrap_or_default().to_string_lossy();
            Ok(format!("{stem}.clean.{variant}"))
        };
        remove(name, path, bytes, options, keep, store)
    });
    let file = match result {
        Ok(Ok(r)) => r,
        Ok(Err(r)) => *r,
        Err(_) => *unread(name, Error::internal()),
    };
    (file, kept)
}

/// The input's variant, when known, and the copy's.
type Variants<'a> = (Option<&'a str>, &'a str);

fn unread(name: &str, e: Error) -> Box<CleanResult> {
    let failed = FileResult::failed(name.into(), e.clone());
    Box::new(CleanResult::failed(failed, e))
}

/// Checks `bytes`, makes the copy without every found check but the `keep` ones, and hands it
/// to `store` when the re-check allows.
fn remove(
    name: &str,
    path: &Path,
    bytes: &[u8],
    options: &Options,
    keep: &[CheckId],
    store: impl FnOnce(Vec<u8>, Variants) -> Result<String, Error>,
) -> Result<CleanResult, Box<CleanResult>> {
    let (checked, opened) =
        examine(name, Some(path), bytes, options).map_err(|e| unread(name, e))?;
    let ran: Vec<CheckId> = checked.checks.iter().map(|c| c.id).collect();
    let keep: Vec<CheckId> = ran.iter().copied().filter(|c| keep.contains(c)).collect();
    let remove: Vec<CheckId> = ran.iter().copied().filter(|c| !keep.contains(c)).collect();
    let cleaned = match opened {
        Opened::Office(pkg) => office::clean::clean(&pkg, &checked.findings, &remove),
        Opened::Pdf(pdf) => pdf::clean::clean(&pdf, &checked.findings, &remove),
    };
    match cleaned {
        Ok(c) => Ok(conclude(checked, &keep, c, options, store)),
        Err(e) => Err(Box::new(CleanResult::failed(checked, e))),
    }
}

/// Re-checks the copy of a file whose check is `checked` and stores it when the re-check finds
/// nothing outside the `keep` checks.
fn conclude(
    checked: FileResult,
    keep: &[CheckId],
    mut cleaned: Cleaned,
    options: &Options,
    store: impl FnOnce(Vec<u8>, Variants) -> Result<String, Error>,
) -> CleanResult {
    let keep = &[keep, &cleaned.kept].concat();
    let recheck = match examine(&checked.path, None, &cleaned.bytes, options) {
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
        kept: checked
            .checks
            .iter()
            .map(|c| c.id)
            .filter(|c| keep.contains(c) && found(c))
            .collect(),
        changes: cleaned.changes,
        recheck: None,
        warnings: [checked.warnings, std::mem::take(&mut cleaned.warnings)].concat(),
        error: None,
    };
    let error = match recheck.findings.first() {
        Some(f) => Some(Error::recheck_failed(f.check)),
        None => store(cleaned.bytes, (checked.variant, cleaned.variant))
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

/// Writes the copy of `input` (its variant and the copy's) where `options` say and gives the
/// path written, as shown.
fn save(
    input: &Path,
    bytes: &[u8],
    variants: Variants,
    options: &CleanOptions,
) -> Result<String, Error> {
    let names = outputs(input, variants, &options.target);
    let shown = |p: &Path| safe(&p.to_string_lossy());
    let file = matches!(options.target, Target::File(_));
    // The extension can change after the run began, so only now is the final name known.
    if file && same_file(&names[0], input) {
        return Err(replaces_input());
    }
    if let Target::Dir(dir) = &options.target {
        fs::create_dir_all(dir).map_err(|e| Error::write_failed(&shown(&names[0]), e))?;
    }
    let replace = file && options.overwrite;
    let out = match replace {
        true => &names[0],
        false => claim(&names)?,
    };
    let written = write(out, bytes);
    if written.is_err() && !replace {
        let _ = fs::remove_file(out);
    }
    written.map_err(|e| Error::write_failed(&shown(out), e))?;
    Ok(shown(out))
}

/// Takes the first free name by creating it empty: finding it free and taking it are one step,
/// so runs at the same moment can't end up with one name.
fn claim(names: &[PathBuf]) -> Result<&PathBuf, Error> {
    for out in names {
        match fs::File::create_new(out) {
            Ok(_) => return Ok(out),
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
            Err(e) => return Err(Error::write_failed(&out.to_string_lossy(), e)),
        }
    }
    Err(Error::output_exists(
        &names[names.len() - 1].to_string_lossy(),
    ))
}

/// The names the copy of `input` may take, in order: the `-o` file, or `<stem>.clean.<ext>`
/// (the copy's variant) then `-2` … `-999` beside the input or in `--out-dir`.
fn outputs(input: &Path, (was, variant): Variants, target: &Target) -> Vec<PathBuf> {
    let dir = match target {
        // An -o name with the macro extension the copy no longer has takes the new one.
        Target::File(f) => {
            return vec![match f.extension() {
                Some(e) if was.is_some_and(|w| w != variant && e.eq_ignore_ascii_case(w)) => {
                    f.with_extension(variant)
                }
                _ => f.clone(),
            }];
        }
        Target::Dir(d) => d.clone(),
        Target::Beside => input.parent().map(Path::to_path_buf).unwrap_or_default(),
    };
    let stem = input.file_stem().unwrap_or_default().to_string_lossy();
    let numbered = (2..=999).map(|n| format!("{stem}.clean-{n}.{variant}"));
    std::iter::once(format!("{stem}.clean.{variant}"))
        .chain(numbered)
        .map(|name| dir.join(name))
        .collect()
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
            ..Cleaned::default()
        };
        let store = |_: Vec<u8>, _: Variants| unreachable!("the copy still holds a finding");
        let r = conclude(checked, &[], held, &options.check, store);
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
