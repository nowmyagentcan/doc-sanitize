//! The command line: `doc-sanitize [check] <PATH>... [OPTIONS]`.

mod interactive;
mod text;

use std::ffi::OsString;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use clap::Parser;
use clap::error::{ContextKind, ContextValue, ErrorKind};
use doc_sanitize::input::{self, Input};
use doc_sanitize::report::{
    CHECKS, CheckId, CleanReport, CleanResult, Error, FileResult, Report, Status, safe,
};
use doc_sanitize::run;
use text::{Look, Out};

#[derive(Parser, Default)]
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
    #[arg(long, value_name = "LIST", value_delimiter = ',')]
    keep: Vec<String>,
    #[arg(short = 'o', value_name = "FILE", conflicts_with = "out_dir")]
    out: Option<PathBuf>,
    #[arg(long, value_name = "DIR")]
    out_dir: Option<PathBuf>,
    #[arg(long)]
    overwrite: bool,
    #[arg(short, long)]
    help: bool,
    #[arg(short = 'V', long)]
    version: bool,
}

pub const ABOUT: &str = "Find and remove hidden data in PDF, Word, Excel and PowerPoint files.";

const USAGE: &str = "\
Usage: doc-sanitize [check] <PATH>... [OPTIONS]
       doc-sanitize clean <PATH>... [OPTIONS]

Commands:
  check  Report hidden data (the default when the first argument is a path)
  clean  Write a copy without it, check the copy again, and keep it only if nothing is left

Options:
  -r                 Look in subfolders too
      --password PW  PDF user password
      --show-values  Also print the hidden text itself (off: only places and counts)
      --json         Print one JSON object
      --keep LIST    clean: checks to leave in place, comma-separated
  -o FILE            clean, one file: where to write the copy
      --out-dir DIR  clean: folder for the copies
      --overwrite    clean: replace an existing -o file
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
  doc-sanitize clean contract.docx
  doc-sanitize clean deck.pptx --keep speaker-notes -o deck-to-send.pptx
  doc-sanitize clean exports/ -r --out-dir outbox/
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
    let bare = args.len() == 1;
    // Only an exact first argument is a command: `./check` is a file.
    let clean = args.get(1).is_some_and(|a| a == "clean");
    if clean || args.get(1).is_some_and(|a| a == "check") {
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
            format!(
                "doc-sanitize {}
",
                env!("CARGO_PKG_VERSION")
            )
        };
        let _ = io::stdout().write_all(text.as_bytes());
        return ExitCode::SUCCESS;
    }
    let interactive = bare && io::stdin().is_terminal() && io::stdout().is_terminal();
    if cli.paths.is_empty() && !interactive {
        let e = Error::usage("No file given.", "Try: doc-sanitize contract.docx");
        return fail(&e, cli.json);
    }
    if !clean && let Some(flag) = clean_only(&cli) {
        let e = Error::usage(
            format!("{flag} works only with clean."),
            "See doc-sanitize --help.",
        );
        return fail(&e, cli.json);
    }
    let mut out = Out::new(io::stdout().lock(), Look::stdout());
    let mut err = Out::new(io::stderr(), Look::stderr());
    let mut code = 0;
    let done = if interactive {
        interactive::run(io::stdin().lock(), &mut out, &mut err, ask)
    } else {
        run(&cli, clean, &mut out, &mut err, &mut code)
    };
    match done {
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

/// One line of interactive mode: the paths with no options, whatever exit code they earn.
fn ask(
    paths: &[PathBuf],
    clean: bool,
    out: &mut Out<impl Write>,
    err: &mut Out<impl Write>,
) -> io::Result<()> {
    let cli = Cli {
        paths: paths.to_vec(),
        ..Cli::default()
    };
    run(&cli, clean, out, err, &mut 0)
}

/// Checks or cleans `cli.paths`; `code` follows the highest exit code earned. An error that
/// stops the whole run prints like any other report.
fn run(
    cli: &Cli,
    clean: bool,
    out: &mut Out<impl Write>,
    err: &mut Out<impl Write>,
    code: &mut u8,
) -> io::Result<()> {
    let prepared = input::expand(&cli.paths, cli.recursive).and_then(|inputs| {
        let options = clean.then(|| clean_options(cli, &inputs)).transpose()?;
        Ok((inputs, options))
    });
    match prepared {
        Ok((inputs, Some(options))) => clean_all(cli, &options, &inputs, out, err, code),
        Ok((inputs, None)) => check(cli, &inputs, out, err, code),
        Err(e) => {
            *code = e.exit_code();
            if cli.json {
                writeln!(out.w, "{}", e.to_json())
            } else {
                text::error(err, &e.message, &e.hint)
            }
        }
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
                let label = progress("Checking", err.look, path, i, inputs.len());
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
    } else {
        if files.len() > 1 {
            text::summary(out, &files)?;
        }
        if files.iter().any(|f| f.status == Status::Found) {
            text::clean_prompt(out)?;
        }
    }
    out.w.flush()
}

/// The first clean-only option given.
fn clean_only(cli: &Cli) -> Option<&'static str> {
    [
        (!cli.keep.is_empty(), "--keep"),
        (cli.out.is_some(), "-o"),
        (cli.out_dir.is_some(), "--out-dir"),
        (cli.overwrite, "--overwrite"),
    ]
    .into_iter()
    .find_map(|(given, flag)| given.then_some(flag))
}

/// The clean options, refusing a `--keep` id that is unknown or can't be kept, `-o` with
/// several files and an `-o` that is the input.
fn clean_options(cli: &Cli, inputs: &[Input]) -> Result<run::CleanOptions, Error> {
    let mut keep = vec![];
    for id in cli.keep.iter().map(|k| k.trim()).filter(|k| !k.is_empty()) {
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
    let target = match (&cli.out, &cli.out_dir) {
        (Some(file), _) => {
            if inputs.len() != 1 {
                return Err(Error::usage(
                    format!("-o works with one file; you gave {}.", inputs.len()),
                    "Use --out-dir for several files.",
                ));
            }
            if same_file(file, inputs[0].path()) {
                return Err(Error::usage(
                    "The output would replace the input file.",
                    "Choose another name with -o.",
                ));
            }
            run::Target::File(file.clone())
        }
        (None, Some(dir)) => run::Target::Dir(dir.clone()),
        (None, None) => run::Target::Beside,
    };
    Ok(run::CleanOptions {
        check: run::Options {
            show_values: cli.show_values,
            password: cli.password.clone(),
        },
        keep,
        target,
        overwrite: cli.overwrite,
    })
}

/// Whether two paths name one existing file.
fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Cleans every input in order, printing text per file as it finishes or one JSON object at
/// the end; `code` follows the highest exit code earned.
fn clean_all(
    cli: &Cli,
    options: &run::CleanOptions,
    inputs: &[Input],
    out: &mut Out<impl Write>,
    err: &mut Out<impl Write>,
    code: &mut u8,
) -> io::Result<()> {
    let mut files: Vec<CleanResult> = vec![];
    for (i, input) in inputs.iter().enumerate() {
        let start = Instant::now();
        let (file, panic) = match input {
            Input::Failed(path, e) => {
                let name = safe(&path.to_string_lossy());
                let failed = FileResult::failed(name, e.clone());
                (CleanResult::failed(failed, e.clone()), None)
            }
            Input::File(path) if cli.json => run::clean(path, options),
            Input::File(path) => {
                let label = progress("Cleaning", err.look, path, i, inputs.len());
                text::spin(err, &label, || run::clean(path, options))
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
            text::cleaned(out, err, &file, i == 0)?;
        }
        files.push(file);
    }
    if cli.json {
        serde_json::to_writer_pretty(&mut out.w, &CleanReport::new(&files))?;
        writeln!(out.w)?;
    } else if files.len() > 1 {
        text::clean_summary(out, &files)?;
    }
    out.w.flush()
}

/// The spinner's label: `Checking report.pdf`, or `Checking 2 of 5 · report.pdf`.
fn progress(verb: &str, look: Look, path: &Path, i: usize, n: usize) -> String {
    let name = safe(&path.file_name().unwrap_or_default().to_string_lossy());
    match n {
        1 => format!("{verb} {name}"),
        n => format!("{verb} {} of {n} {} {name}", i + 1, look.pick("·", "-")),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_checks_quoted_paths_and_cleans_them_beside_the_file() {
        let dir = std::env::temp_dir().join(format!("doc-sanitize-session-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a b.pdf");
        let pdf = b"%PDF-1.4
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
        std::fs::write(&file, pdf).unwrap();
        let typed = format!(
            "\"{}\"
/clean
/quit
",
            file.display()
        );
        let (mut out, mut err) = (
            Out::new(vec![], Look::default()),
            Out::new(vec![], Look::default()),
        );
        interactive::run(typed.as_bytes(), &mut out, &mut err, ask).unwrap();
        let out = String::from_utf8(out.w).unwrap();
        let name = file.display();
        assert!(
            out.contains(&format!(
                "›   {name}  found 1 kind of hidden data
"
            )),
            "{out}"
        );
        assert!(
            out.contains(
                "
  Remove it: /clean

"
            ),
            "{out}"
        );
        assert!(!out.contains("doc-sanitize clean"), "{out}");
        assert!(
            out.contains(
                "  ✓ re-checked: 13 checks, nothing left
"
            ),
            "{out}"
        );
        assert!(dir.join("a b.clean.pdf").exists());
        assert!(err.w.is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
