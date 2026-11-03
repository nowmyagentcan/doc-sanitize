//! Text output for people: each file's report on stdout, errors on stderr. The plain text is
//! exactly what the page's Copy report gives; colour only on a terminal without `NO_COLOR`,
//! ASCII marks on a Windows console whose output code page isn't UTF-8.

use std::env;
use std::io::{self, IsTerminal, Write};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::Duration;

use doc_sanitize::report::{
    CleanResult, CleanStatus, FileResult, Finding, Status, plural, quantity,
};

const BOLD: &str = "1";
const DIM: &str = "2";
const RED: &str = "31";
const GREEN: &str = "32";
const YELLOW: &str = "33";

/// How one stream shows text.
#[derive(Debug, Clone, Copy, Default)]
pub struct Look {
    /// A terminal that takes ANSI sequences.
    pub ansi: bool,
    pub colour: bool,
    pub ascii: bool,
}

impl Look {
    pub fn stdout() -> Self {
        Self::new(io::stdout().is_terminal(), console::STDOUT)
    }

    pub fn stderr() -> Self {
        Self::new(io::stderr().is_terminal(), console::STDERR)
    }

    fn new(terminal: bool, stream: console::Stream) -> Self {
        let ansi = terminal && console::enable_ansi(stream);
        Self {
            ansi,
            colour: ansi && env::var_os("NO_COLOR").is_none_or(|v| v.is_empty()),
            ascii: terminal && !console::utf8(),
        }
    }

    /// `s` in the SGR style `sgr` when colour is on.
    fn paint(self, sgr: &str, s: &str) -> String {
        if self.colour {
            format!("\x1b[{sgr}m{s}\x1b[0m")
        } else {
            s.into()
        }
    }

    /// `unicode`, or `ascii` where the console can't show it.
    pub fn pick(self, unicode: &'static str, ascii: &'static str) -> &'static str {
        if self.ascii { ascii } else { unicode }
    }
}

#[cfg(windows)]
mod console {
    use windows_sys::Win32::System::Console::{
        ENABLE_VIRTUAL_TERMINAL_PROCESSING, GetConsoleMode, GetConsoleOutputCP, GetStdHandle,
        STD_ERROR_HANDLE, STD_HANDLE, STD_OUTPUT_HANDLE, SetConsoleMode,
    };

    pub type Stream = STD_HANDLE;
    pub const STDOUT: Stream = STD_OUTPUT_HANDLE;
    pub const STDERR: Stream = STD_ERROR_HANDLE;

    /// Turns on ANSI sequences for the console behind `stream`.
    pub fn enable_ansi(stream: Stream) -> bool {
        // SAFETY: plain console calls on a standard handle; `mode` outlives the call.
        unsafe {
            let handle = GetStdHandle(stream);
            let mut mode = 0;
            GetConsoleMode(handle, &mut mode) != 0
                && SetConsoleMode(handle, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING) != 0
        }
    }

    pub fn utf8() -> bool {
        // SAFETY: no arguments; returns 0 without a console.
        unsafe { GetConsoleOutputCP() == 65001 }
    }
}

#[cfg(not(windows))]
mod console {
    pub type Stream = ();
    pub const STDOUT: Stream = ();
    pub const STDERR: Stream = ();

    pub fn enable_ansi(_: Stream) -> bool {
        true
    }

    pub fn utf8() -> bool {
        true
    }
}

/// A stream and how it shows text.
pub struct Out<W> {
    pub w: W,
    pub look: Look,
}

impl<W: Write> Out<W> {
    pub fn new(w: W, look: Look) -> Self {
        Self { w, look }
    }

    fn line(&mut self, s: &str) -> io::Result<()> {
        writeln!(self.w, "{s}")
    }
}

/// One file's report: its line, a line per check that found something, the checks that found
/// nothing, its warnings and the clean hint; or its error, on stderr. `first` leaves out the
/// blank line that separates files.
pub fn file(
    out: &mut Out<impl Write>,
    err: &mut Out<impl Write>,
    f: &FileResult,
    first: bool,
) -> io::Result<()> {
    if let Some(e) = &f.error {
        if !first {
            err.line("")?;
        }
        return error(err, &e.message, &e.hint);
    }
    if !first {
        out.line("")?;
    }
    let look = out.look;
    let with = |s| f.checks.iter().filter(move |c| c.status == s);
    let found: Vec<_> = with(Status::Found).collect();
    let incomplete: Vec<_> = with(Status::Incomplete).collect();
    let none: Vec<_> = with(Status::NoneFound).collect();
    let width = found
        .iter()
        .chain(&incomplete)
        .map(|c| c.id.id().len())
        .max()
        .unwrap_or(0)
        + 2;
    let status = match found.len() {
        0 if !incomplete.is_empty() => {
            format!("{} checks, {} incomplete", f.checks.len(), incomplete.len())
        }
        0 => format!(
            "nothing found by {}",
            plural(f.checks.len() as u64, "check")
        ),
        1 => look.paint(RED, "found 1 kind of hidden data"),
        n => look.paint(RED, &format!("found {n} kinds of hidden data")),
    };
    out.line(&format!("{}  {status}", look.paint(BOLD, &f.path)))?;
    let shown = |x: &Finding| match x.place.shown() {
        p if p.is_empty() => p,
        p => format!("{}: ", look.paint(DIM, &p)),
    };
    for c in &found {
        let fs: Vec<_> = f.findings.iter().filter(|x| x.check == c.id).collect();
        let details: Vec<_> = fs.iter().map(|x| shown(x) + &x.detail).collect();
        let mark = look.paint(RED, look.pick("✗", "x"));
        out.line(&format!(
            "  {mark} {:width$}{}",
            c.id.id(),
            details.join("; ")
        ))?;
        for x in fs {
            if let Some(v) = &x.value {
                out.line(&format!("    {:width$}{}{v}", "", shown(x)))?;
            }
        }
    }
    for c in &incomplete {
        let mark = look.paint(YELLOW, "!");
        let note = c.note.as_deref().unwrap_or_default();
        out.line(&format!("  {mark} {:width$}{note}", c.id.id()))?;
    }
    if !none.is_empty() {
        let ids: Vec<_> = none.iter().map(|c| c.id.id()).collect();
        let mark = look.paint(GREEN, look.pick("✓", "ok"));
        let ids = look.paint(DIM, &ids.join(", "));
        let n = plural(none.len() as u64, "check");
        out.line(&format!("  {mark} {n} found nothing: {ids}"))?;
    }
    for w in &f.warnings {
        out.line(&format!("  {} {}", look.paint(YELLOW, "!"), w.message))?;
    }
    if !found.is_empty() {
        let hint = format!("Remove it: doc-sanitize clean {}", quoted(&f.path));
        out.line(&format!("  {}", look.paint(DIM, &hint)))?;
    }
    Ok(())
}

/// A path as a shell argument: in double quotes when it holds spaces, quotes or characters a
/// shell would read, with those escaped. On Windows a backslash is a path separator, not one.
fn quoted(path: &str) -> String {
    let special = |c: char| "\"'$`".contains(c) || (cfg!(not(windows)) && c == '\\');
    if !path.chars().any(|c| c.is_whitespace() || special(c)) {
        return path.into();
    }
    let mut out = String::from('"');
    for c in path.chars() {
        if c == '"' || (c != '\'' && special(c)) {
            out.push('\\');
        }
        out.push(c);
    }
    out + "\""
}

/// `{f} of {n} files have hidden data`, with ` · {e} failed` when any failed, after a blank
/// line.
pub fn summary(out: &mut Out<impl Write>, files: &[FileResult]) -> io::Result<()> {
    let found = files.iter().filter(|f| f.status == Status::Found).count();
    let failed = files.iter().filter(|f| f.error.is_some()).count();
    let mut line = format!("{found} of {} files have hidden data", files.len());
    if failed > 0 {
        let dot = out.look.paint(DIM, out.look.pick("·", "-"));
        line += &format!(" {dot} {failed} failed");
    }
    out.line("")?;
    out.line(&line)
}

/// `✗ message` and the hint indented on the next line; a message of several lines with as many
/// hints gives each line its hint.
pub fn error(err: &mut Out<impl Write>, message: &str, hint: &str) -> io::Result<()> {
    let mark = err.look.paint(RED, err.look.pick("✗", "x"));
    let (lines, hints): (Vec<_>, Vec<_>) = (message.lines().collect(), hint.lines().collect());
    if lines.len() > 1 && lines.len() == hints.len() {
        for (m, h) in lines.iter().zip(hints) {
            err.line(&format!("{mark} {m}"))?;
            err.line(&format!("  {h}"))?;
        }
        return Ok(());
    }
    err.line(&format!("{mark} {message}"))?;
    err.line(&format!("  {hint}"))
}

/// One cleaned file: `{path} → {output}`, what was removed and kept, the changes, warnings and
/// the re-check; or why it wasn't cleaned, on stderr. `first` leaves out the blank line that
/// separates files.
pub fn cleaned(
    out: &mut Out<impl Write>,
    err: &mut Out<impl Write>,
    f: &CleanResult,
    first: bool,
) -> io::Result<()> {
    let (Some(output), Some(recheck)) = (&f.output, &f.recheck) else {
        if !first {
            err.line("")?;
        }
        let e = f.error.as_ref();
        return error(
            err,
            e.map_or("", |e| e.message.as_str()),
            e.map_or("", |e| e.hint.as_str()),
        );
    };
    if !first {
        out.line("")?;
    }
    let look = out.look;
    let arrow = look.pick("→", "->");
    let label = |s: &str| look.paint(DIM, s);
    out.line(&format!(
        "{} {arrow} {}",
        look.paint(BOLD, &f.path),
        look.paint(BOLD, output)
    ))?;
    let mut groups: Vec<(&str, Vec<String>)> = vec![];
    for r in &f.removed {
        let q = quantity(r.count, r.unit);
        match groups.iter_mut().find(|g| g.0 == r.check.id()) {
            Some(g) => g.1.push(q),
            None => groups.push((r.check.id(), vec![q])),
        }
    }
    if !groups.is_empty() {
        let shown: Vec<_> = groups
            .iter()
            .map(|(id, qs)| format!("{id} ({})", qs.join(", ")))
            .collect();
        out.line(&format!("  {}  {}", label("removed"), shown.join(", ")))?;
    }
    if !f.kept.is_empty() {
        let ids: Vec<_> = f.kept.iter().map(|c| c.id()).collect();
        out.line(&format!("  {}     {}", label("kept"), ids.join(", ")))?;
    }
    for (i, c) in f.changes.iter().enumerate() {
        match i {
            0 => out.line(&format!("  {}  {}", label("changed"), c.detail))?,
            _ => out.line(&format!("           {}", c.detail))?,
        }
    }
    for w in &f.warnings {
        out.line(&format!("  {} {}", look.paint(YELLOW, "!"), w.message))?;
    }
    let incomplete: Vec<_> = recheck
        .checks
        .iter()
        .filter(|c| c.status == Status::Incomplete)
        .collect();
    let n = plural(recheck.checks.len() as u64, "check");
    let mut line = match incomplete.len() {
        0 => format!(
            "{} re-checked: {n}, nothing left",
            look.paint(GREEN, look.pick("✓", "ok"))
        ),
        i => format!(
            "{} re-checked: {n}, {i} incomplete",
            look.paint(YELLOW, "!")
        ),
    };
    if !f.kept.is_empty() {
        line += &format!(" ({} kept)", f.kept.len());
    }
    out.line(&format!("  {line}"))?;
    for c in incomplete {
        let note = c.note.as_deref().unwrap_or_default();
        out.line(&format!(
            "    {} {}  {note}",
            look.paint(YELLOW, "!"),
            c.id.id()
        ))?;
    }
    Ok(())
}

/// `{c} of {n} cleaned`, with ` · {e} not cleaned` when any weren't, after a blank line.
pub fn clean_summary(out: &mut Out<impl Write>, files: &[CleanResult]) -> io::Result<()> {
    let count = |s| files.iter().filter(|f| f.status == s).count();
    let mut line = format!("{} of {} cleaned", count(CleanStatus::Cleaned), files.len());
    let not = count(CleanStatus::NotCleaned);
    if not > 0 {
        let dot = out.look.paint(DIM, out.look.pick("·", "-"));
        line += &format!(" {dot} {not} not cleaned");
    }
    out.line("")?;
    out.line(&line)
}

/// How long work runs before the spinner shows, and how long each frame stays.
const SPIN_AFTER: Duration = Duration::from_millis(300);
const FRAME: Duration = Duration::from_millis(80);

/// Runs `work`; when it takes a while and stderr is a terminal, `⠋ {label}` spins on stderr
/// meanwhile and is erased before this returns.
pub fn spin<T>(err: &Out<impl Write>, label: &str, work: impl FnOnce() -> T) -> T {
    if !err.look.ansi {
        return work();
    }
    let (stop, stopped) = mpsc::channel();
    let look = err.look;
    thread::scope(|s| {
        s.spawn(|| spinner(io::stderr(), look, &format!("{{}} {label}"), stopped));
        let result = work();
        drop(stop);
        result
    })
}

/// Draws `line` (its `{}` the frame) until `stopped` ends, then erases it.
fn spinner(mut w: impl Write, look: Look, line: &str, stopped: Receiver<()>) {
    let frames: &[&str] = if look.ascii {
        &["|", "/", "-", "\\"]
    } else {
        &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]
    };
    let mut drawn = false;
    for (i, frame) in frames.iter().cycle().enumerate() {
        let wait = if i == 0 { SPIN_AFTER } else { FRAME };
        if stopped.recv_timeout(wait) != Err(RecvTimeoutError::Timeout) {
            break;
        }
        let text = look.paint(DIM, &line.replacen("{}", frame, 1));
        if write!(w, "\r{text}").and_then(|()| w.flush()).is_err() {
            return;
        }
        drawn = true;
    }
    if drawn {
        let _ = write!(w, "\r\x1b[K");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use doc_sanitize::report::{CheckId, Error, Format, Warning, Where};

    const COLOUR: Look = Look {
        ansi: true,
        colour: true,
        ascii: false,
    };
    const ASCII: Look = Look {
        ansi: true,
        colour: false,
        ascii: true,
    };

    fn sample() -> Vec<FileResult> {
        let finding = |part: Option<&str>, detail: &str, value: Option<&str>| Finding {
            check: CheckId::LeftoverData,
            kind: "untyped_entries",
            place: Where {
                part: part.map(Into::into),
                ..Where::default()
            },
            count: 1,
            unit: "parts",
            detail: detail.into(),
            value: value.map(Into::into),
        };
        vec![
            FileResult::checked(
                "Q3 report.docx".into(),
                Format::Docx,
                "docx",
                &[CheckId::LeftoverData],
                vec![
                    finding(Some("word/x.bin"), "1 ZIP entry with no content type", None),
                    finding(None, "1 ZIP comment of 4 bytes", Some("note")),
                ],
                &[],
                vec![Warning {
                    code: "extension_mismatch",
                    message: "Q3 report.docx is a .docm file with a .docx name.".into(),
                }],
            ),
            FileResult::failed("gone.pdf".into(), Error::not_found()),
            FileResult::checked(
                "scan.pdf".into(),
                Format::Pdf,
                "pdf",
                &[CheckId::Metadata, CheckId::HiddenText],
                vec![],
                &[(
                    CheckId::HiddenText,
                    "page 4: a font couldn't be read".into(),
                )],
                vec![],
            ),
            FileResult::checked(
                "deck.pptx".into(),
                Format::Pptx,
                "pptx",
                &[CheckId::LeftoverData],
                vec![],
                &[],
                vec![],
            ),
        ]
    }

    fn printed(look: Look) -> (String, String) {
        let files = sample();
        let (mut out, mut err) = (Out::new(vec![], look), Out::new(vec![], look));
        for (i, f) in files.iter().enumerate() {
            file(&mut out, &mut err, f, i == 0).unwrap();
        }
        summary(&mut out, &files).unwrap();
        let text = |w: Vec<u8>| String::from_utf8(w).unwrap();
        (text(out.w), text(err.w))
    }

    #[test]
    fn plain_text_is_the_pages_copy_report() {
        let (out, err) = printed(Look::default());
        assert_eq!(
            out,
            "Q3 report.docx  found 1 kind of hidden data\n\
             \x20 ✗ leftover-data  word/x.bin: 1 ZIP entry with no content type; 1 ZIP comment of 4 bytes\n\
             \x20                  note\n\
             \x20 ! Q3 report.docx is a .docm file with a .docx name.\n\
             \x20 Remove it: doc-sanitize clean \"Q3 report.docx\"\n\
             \n\
             scan.pdf  2 checks, 1 incomplete\n\
             \x20 ! hidden-text  page 4: a font couldn't be read\n\
             \x20 ✓ 1 check found nothing: metadata\n\
             \n\
             deck.pptx  nothing found by 1 check\n\
             \x20 ✓ 1 check found nothing: leftover-data\n\
             \n\
             1 of 4 files have hidden data · 1 failed\n"
        );
        assert_eq!(err, "\n✗ Can't find gone.pdf.\n  Check the path.\n");
    }

    #[test]
    fn colour_marks_names_findings_and_places() {
        let (out, err) = printed(COLOUR);
        assert!(out.starts_with(
            "\x1b[1mQ3 report.docx\x1b[0m  \x1b[31mfound 1 kind of hidden data\x1b[0m\n  \
             \x1b[31m✗\x1b[0m leftover-data  \x1b[2mword/x.bin\x1b[0m: 1 ZIP entry"
        ));
        assert!(
            out.contains("  \x1b[32m✓\x1b[0m 1 check found nothing: \x1b[2mleftover-data\x1b[0m\n")
        );
        assert!(out.ends_with("files have hidden data \x1b[2m·\x1b[0m 1 failed\n"));
        assert_eq!(
            err,
            "\n\x1b[31m✗\x1b[0m Can't find gone.pdf.\n  Check the path.\n"
        );
    }

    #[test]
    fn ascii_consoles_get_ascii_marks() {
        let (out, err) = printed(ASCII);
        assert!(out.contains("  x leftover-data  word/x.bin"));
        assert!(out.contains("  ok 1 check found nothing"));
        assert!(out.ends_with("1 of 4 files have hidden data - 1 failed\n"));
        assert!(err.starts_with("\nx Can't find"));
        assert!(!out.contains('\x1b'));
    }

    #[test]
    fn paths_are_quoted_for_the_shell_only_when_needed() {
        assert_eq!(quoted("a.docx"), "a.docx");
        assert_eq!(quoted("Q3 filing.pdf"), "\"Q3 filing.pdf\"");
        assert_eq!(quoted("it's $5.pdf"), "\"it's \\$5.pdf\"");
        assert_eq!(quoted("say \"hi\".pdf"), "\"say \\\"hi\\\".pdf\"");
        let dir = if cfg!(windows) {
            "a\\b.pdf"
        } else {
            "\"a\\\\b.pdf\""
        };
        assert_eq!(quoted("a\\b.pdf"), dir);
    }

    fn cleaned_sample() -> Vec<CleanResult> {
        use doc_sanitize::report::{Change, Recheck, Removed};
        let copy = FileResult::checked(
            "b.clean.xlsx".into(),
            Format::Xlsx,
            "xlsx",
            &[CheckId::Comments, CheckId::HiddenSheets],
            vec![],
            &[],
            vec![],
        );
        let removed = |check, count, unit| Removed {
            check,
            count,
            unit,
            detail: String::new(),
        };
        vec![
            CleanResult {
                path: "b.xlsm".into(),
                format: Some(Format::Xlsx),
                variant: Some("xlsm"),
                output: Some("b.clean.xlsx".into()),
                status: CleanStatus::Cleaned,
                removed: vec![
                    removed(CheckId::Metadata, 3, "fields"),
                    removed(CheckId::Metadata, 1, "ids"),
                    removed(CheckId::HiddenSheets, 1, "sheets"),
                    removed(CheckId::Comments, 1, "people"),
                ],
                kept: vec![CheckId::Comments],
                changes: vec![
                    Change::counted(
                        "formulas_to_values",
                        40,
                        "40 formulas were replaced by their saved results.",
                    ),
                    Change::new(
                        "extension_changed",
                        "Macros were removed, so the copy is .xlsx.",
                    ),
                ],
                recheck: Some(Recheck::of(copy, &[CheckId::Comments])),
                warnings: vec![],
                error: None,
            },
            CleanResult::failed(
                FileResult::failed("p.xlsx".into(), Error::not_found()),
                Error::cannot_clean(&[(
                    CheckId::PivotCaches,
                    "its pivot table has slicers".into(),
                    "Remove the slicers in Excel, or run clean again with --keep pivot-caches."
                        .into(),
                )]),
            ),
        ]
    }

    fn printed_clean(look: Look) -> (String, String) {
        let files = cleaned_sample();
        let (mut out, mut err) = (Out::new(vec![], look), Out::new(vec![], look));
        for (i, f) in files.iter().enumerate() {
            cleaned(&mut out, &mut err, f, i == 0).unwrap();
        }
        clean_summary(&mut out, &files).unwrap();
        let text = |w: Vec<u8>| String::from_utf8(w).unwrap();
        (text(out.w), text(err.w))
    }

    #[test]
    fn clean_text_follows_the_design() {
        let (out, err) = printed_clean(Look::default());
        assert_eq!(
            out,
            "b.xlsm → b.clean.xlsx\n\
             \x20 removed  metadata (3 fields, 1 id), hidden-sheets (1 sheet), comments (1 person)\n\
             \x20 kept     comments\n\
             \x20 changed  40 formulas were replaced by their saved results.\n\
             \x20          Macros were removed, so the copy is .xlsx.\n\
             \x20 ✓ re-checked: 2 checks, nothing left (1 kept)\n\
             \n\
             1 of 2 cleaned · 1 not cleaned\n"
        );
        assert_eq!(
            err,
            "\n✗ Can't remove pivot-caches from p.xlsx: its pivot table has slicers.\n  Remove the slicers in Excel, or run clean again with --keep pivot-caches.\n"
        );
        let (out, _) = printed_clean(COLOUR);
        assert!(out.starts_with(
            "\x1b[1mb.xlsm\x1b[0m → \x1b[1mb.clean.xlsx\x1b[0m\n  \x1b[2mremoved\x1b[0m  metadata"
        ));
        assert!(out.contains("  \x1b[32m✓\x1b[0m re-checked"));
        let (out, err) = printed_clean(ASCII);
        assert!(out.starts_with("b.xlsm -> b.clean.xlsx\n"));
        assert!(out.contains("  ok re-checked") && out.ends_with("cleaned - 1 not cleaned\n"));
        assert!(err.starts_with("\nx Can't remove"));
    }

    #[test]
    fn the_spinner_shows_only_after_a_while_and_erases_itself() {
        let (stop, stopped) = mpsc::channel::<()>();
        drop(stop);
        let mut quick = vec![];
        spinner(&mut quick, COLOUR, "{} Checking a.docx", stopped);
        assert!(quick.is_empty());

        let (stop, stopped) = mpsc::channel::<()>();
        let slow = thread::spawn(move || {
            let mut w = vec![];
            spinner(&mut w, ASCII, "{} Checking 2 of 5 · scan.pdf", stopped);
            String::from_utf8(w).unwrap()
        });
        thread::sleep(Duration::from_secs(1));
        drop(stop);
        let slow = slow.join().unwrap();
        assert!(slow.starts_with("\r| Checking 2 of 5 · scan.pdf\r/ Checking"));
        assert!(slow.ends_with("\r\x1b[K"));
    }
}
