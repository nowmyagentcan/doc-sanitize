//! The command line: `doc-sanitize [check] <PATH>... [OPTIONS]`.

mod text;

use std::ffi::OsString;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use clap::Parser;
use clap::error::{ContextKind, ContextValue, ErrorKind};
use doc_sanitize::input::{self, Input};
use doc_sanitize::report::{CHECKS, Error, FileResult, Report, safe};
use doc_sanitize::run;
use text::{Look, Out};

#[derive(Parser)]
#[command(
    name = "doc-sanitize",
    disable_help_flag = true,
    disable_version_flag = true
)]
struct Cli {
    paths: Vec<PathBuf>,
    #[arg(short = 'r')]
    recursive: bool,
    #[arg(long, value_name = "PW")]
    password: Option<String>,
    #[arg(long)]
    show_values: bool,
    #[arg(long)]
    json: bool,
    #[arg(long)]
    debug: bool,
    #[arg(short, long)]
    help: bool,
    #[arg(short = 'V', long)]
    version: bool,
}

const ABOUT: &str = "Find and remove hidden data in PDF, Word, Excel and PowerPoint files.";

const USAGE: &str = "\
Usage: doc-sanitize [check] <PATH>... [OPTIONS]

Commands:
  check  Report hidden data (the default when the first argument is a path)

Options:
  -r                 Look in subfolders too
      --password PW  PDF user password
      --show-values  Also print the hidden text itself (off: only places and counts)
      --json         Print one JSON object
      --debug        Show stack traces and timings
  -h, --help         Print help
  -V, --version      Print version";

const AFTER: &str = "\
Not checked: legacy .doc/.xls/.ppt, .xlsb, Strict Open XML, ODF, RTF and password-protected
Office files; files inside embedded files; covered or same-colour text in PowerPoint; floating
shapes over text in Word; cropped or covered pictures in PDFs; glyph-width leaks around real
redactions; bookmarks; protection password hashes; hidden defined names; EMF/WMF descriptions,
JPEG 2000 metadata, Office ink and embedded fonts.

Examples:
  doc-sanitize contract.docx
  doc-sanitize redacted.pdf --json
  doc-sanitize check filing.pdf --show-values";

fn help() -> String {
    let checks: String = CHECKS
        .iter()
        .map(|(id, line)| format!("  {id:21}{line}\n"))
        .collect();
    format!("{ABOUT}\n\n{USAGE}\n\nChecks:\n{checks}\n{AFTER}\n")
}

fn main() -> ExitCode {
    let mut args: Vec<OsString> = std::env::args_os().collect();
    // Only an exact first argument is a command: `./check` is a file.
    if args.get(1).is_some_and(|a| a == "check") {
        args.remove(1);
    }
    let json = args
        .iter()
        .skip(1)
        .take_while(|a| *a != "--")
        .any(|a| a == "--json");
    let cli = match Cli::try_parse_from(&args) {
        Ok(cli) => cli,
        Err(e) => return fail(&usage(&e), json),
    };
    if cli.help || cli.version {
        let text = if cli.help {
            help()
        } else {
            format!("doc-sanitize {}\n", env!("CARGO_PKG_VERSION"))
        };
        let _ = io::stdout().write_all(text.as_bytes());
        return ExitCode::SUCCESS;
    }
    if cli.paths.is_empty() {
        let e = Error::usage("No file given.", "Try: doc-sanitize contract.docx");
        return fail(&e, cli.json);
    }
    let inputs = match input::expand(&cli.paths, cli.recursive) {
        Ok(inputs) => inputs,
        Err(e) => return fail(&e, cli.json),
    };
    let mut out = Out::new(io::stdout().lock(), Look::stdout());
    let mut err = Out::new(io::stderr(), Look::stderr());
    let mut code = 0;
    match check(&cli, &inputs, &mut out, &mut err, &mut code) {
        Err(e) if e.kind() != io::ErrorKind::BrokenPipe => {
            let _ = text::error(
                &mut err,
                &format!("Can't write the report: {}.", safe(&e.to_string())),
                "Check that the output can be written to.",
            );
            ExitCode::from(3)
        }
        // A reader that hung up ends the run quietly with the code earned so far.
        _ => ExitCode::from(code),
    }
}

/// Checks every input in order, printing text per file as it finishes or one JSON object at
/// the end; `code` follows the highest exit code earned.
fn check(
    cli: &Cli,
    inputs: &[Input],
    out: &mut Out<impl Write>,
    err: &mut Out<impl Write>,
    code: &mut u8,
) -> io::Result<()> {
    let mut files: Vec<FileResult> = vec![];
    let options = run::Options {
        show_values: cli.show_values,
        password: cli.password.clone(),
    };
    for (i, input) in inputs.iter().enumerate() {
        let start = Instant::now();
        let (file, panic) = match input {
            Input::Failed(path, e) => {
                let name = safe(&path.to_string_lossy());
                (FileResult::failed(name, e.clone()), None)
            }
            Input::File(path) if cli.json => run::check(path, &options),
            Input::File(path) => {
                let name = safe(&path.file_name().unwrap_or_default().to_string_lossy());
                let label = match inputs.len() {
                    1 => format!("Checking {name}"),
                    n => {
                        let dot = err.look.pick("·", "-");
                        format!("Checking {} of {n} {dot} {name}", i + 1)
                    }
                };
                text::spin(err, &label, || run::check(path, &options))
            }
        };
        *code = (*code).max(file.exit_code());
        if cli.debug {
            let what = file.variant.unwrap_or("not identified");
            let ms = start.elapsed().as_secs_f64() * 1000.0;
            writeln!(err.w, "debug: {}: {what}, {ms:.1} ms", file.path)?;
            if let Some(at) = panic {
                writeln!(err.w, "debug: {}: {at}", file.path)?;
            }
        }
        if !cli.json {
            text::file(out, err, &file, i == 0)?;
        }
        files.push(file);
    }
    if cli.json {
        serde_json::to_writer_pretty(&mut out.w, &Report::check(&files))?;
        writeln!(out.w)?;
    } else if files.len() > 1 {
        text::summary(out, &files)?;
    }
    out.w.flush()
}

/// A clap error as our usage error: `Unknown option --x.` or clap's own sentence.
fn usage(e: &clap::Error) -> Error {
    let message = match (e.kind(), e.get(ContextKind::InvalidArg)) {
        (ErrorKind::UnknownArgument, Some(ContextValue::String(arg))) => {
            format!("Unknown option {}.", safe(arg))
        }
        _ => {
            let text = e.render().to_string();
            let line = text.lines().next().unwrap_or_default();
            let line = line.strip_prefix("error: ").unwrap_or(line);
            let mut chars = line.chars();
            let first = chars.next().map(|c| c.to_uppercase().to_string());
            format!("{}{}.", first.unwrap_or_default(), safe(chars.as_str()))
        }
    };
    Error::usage(message, "See doc-sanitize --help.")
}

/// Reports an error that stops the run, as two lines on stderr or as JSON on stdout.
fn fail(e: &Error, json: bool) -> ExitCode {
    let _ = if json {
        writeln!(io::stdout(), "{}", e.to_json())
    } else {
        text::error(
            &mut Out::new(io::stderr(), Look::stderr()),
            &e.message,
            &e.hint,
        )
    };
    ExitCode::from(e.exit_code())
}
