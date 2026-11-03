//! Checking one file: read it, identify it and run its checks on a worker thread with a large
//! stack, where a panic becomes that file's `internal` error and the next file still runs.

use std::any::Any;
use std::backtrace::Backtrace;
use std::cell::RefCell;
use std::panic::{self, AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::sync::Once;
use std::thread;

use crate::input::{self, Kind};
use crate::office::Package;
use crate::report::{CheckId, Error, FileResult, Format, Warning, safe};

const WORKER: &str = "doc-sanitize-worker";
const WORKER_STACK: usize = 64 << 20;

thread_local! {
    /// Where the last panic on this thread happened, and its stack, for `--debug`.
    static PANIC_AT: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Checks the file at `path`. A caught panic comes back as `internal`, with where it happened
/// as the second value.
pub fn check(path: &Path) -> (FileResult, Option<String>) {
    let name = safe(&path.to_string_lossy());
    let result = on_worker(|| {
        let bytes = input::load(path)?;
        let (format, variant, ran, findings): (_, _, Vec<CheckId>, _) = match input::sniff(&bytes)?
        {
            Kind::Pdf => (Format::Pdf, "pdf", vec![], vec![]),
            Kind::Zip => {
                let pkg = Package::open(&bytes)?;
                (pkg.format, pkg.variant, vec![], vec![])
            }
        };
        let warnings = mismatch(&name, path, variant).into_iter().collect();
        Ok(FileResult::checked(
            name.clone(),
            format,
            variant,
            &ran,
            findings,
            warnings,
        ))
    });
    match result {
        Ok(Ok(file)) => (file, None),
        Ok(Err(e)) => (FileResult::failed(name, e), None),
        Err(at) => (FileResult::failed(name, Error::internal()), Some(at)),
    }
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
    fn the_extension_must_match_the_variant() {
        let w = mismatch("fake.docx", Path::new("fake.docx"), "pdf").unwrap();
        assert_eq!(w.message, "fake.docx is a .pdf file with a .docx name.");
        assert!(mismatch("a.PDF", Path::new("a.PDF"), "pdf").is_none());
        assert!(mismatch("clean", Path::new("clean"), "docx").is_none());
    }
}
