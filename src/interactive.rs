//! Interactive mode: a banner, then a prompt that checks the paths typed or dropped on each line
//! and cleans them on `/clean`.

use std::io::{self, BufRead, Write};
use std::mem;
use std::path::{Path, PathBuf};

use crate::ABOUT;
use crate::text::{self, BOLD, DIM, Out};

const HELP: [&str; 4] = [
    "Type or drop one or more paths to check them (quote a path that has spaces).",
    "/clean   write a cleaned copy of each file you just checked, beside it",
    "/help    show this",
    "/quit    leave (Ctrl+C works too)",
];

/// Shows the banner, then hands each line's paths to `ask` with `false`, and on `/clean` the
/// last line's paths with `true`, until `/quit` or the end of `input`.
pub fn run<O: Write, E: Write>(
    mut input: impl BufRead,
    out: &mut Out<O>,
    err: &mut Out<E>,
    mut ask: impl FnMut(&[PathBuf], bool, &mut Out<O>, &mut Out<E>) -> io::Result<()>,
) -> io::Result<()> {
    banner(out)?;
    let prompt = out.look.paint(BOLD, out.look.pick("›", ">"));
    (out.prompted, err.prompted) = (true, true);
    let (mut last, mut line): (Vec<PathBuf>, _) = (vec![], vec![]);
    loop {
        write!(out.w, "{prompt} ")?;
        out.w.flush()?;
        line.clear();
        // Bytes, not read_line: a path that isn't UTF-8 is a file not found, never a stop.
        if input.read_until(b'\n', &mut line)? == 0 {
            return writeln!(out.w);
        }
        let words = split(&String::from_utf8_lossy(&line), cfg!(not(windows)));
        match words.as_slice() {
            [] => continue,
            [w] if w == "/quit" => return Ok(()),
            [w] if w == "/help" => {
                for help in HELP {
                    out.line(help)?;
                }
            }
            [w] if w == "/clean" && last.is_empty() => text::error(
                err,
                "Nothing checked yet.",
                "Type a path first, then /clean.",
            )?,
            [w] if w == "/clean" => ask(&last, true, out, err)?,
            [w] if is_command(w) => {
                text::error(err, &format!("Unknown command '{w}'."), "Try /help.")?
            }
            _ => {
                last = words.iter().map(PathBuf::from).collect();
                ask(&last, false, out, err)?;
            }
        }
        writeln!(out.w)?;
    }
}

/// A word like `/x` that isn't an existing path.
fn is_command(word: &str) -> bool {
    word.strip_prefix('/')
        .is_some_and(|name| name.chars().all(|c| c.is_ascii_alphabetic()))
        && !Path::new(word).exists()
}

fn banner(out: &mut Out<impl Write>) -> io::Result<()> {
    let look = out.look;
    let version = env!("CARGO_PKG_VERSION");
    let name = format!("{} doc-sanitize", look.pick("✻", "*"));
    let rows = [
        (
            format!("{name} {version}"),
            format!("{} {version}", look.paint(BOLD, &name)),
        ),
        (format!("  {ABOUT}"), format!("  {ABOUT}")),
    ];
    let width = rows.iter().map(|(plain, _)| plain.chars().count()).max();
    let width = width.unwrap_or_default();
    let [top_left, top_right, bottom_left, bottom_right, across, side] = if look.ascii {
        ["+", "+", "+", "+", "-", "|"]
    } else {
        ["╭", "╮", "╰", "╯", "─", "│"]
    };
    let rule = across.repeat(width + 2);
    let side = look.paint(DIM, side);
    out.line(&look.paint(DIM, &format!("{top_left}{rule}{top_right}")))?;
    for (plain, styled) in rows {
        let pad = " ".repeat(width - plain.chars().count());
        out.line(&format!("{side} {styled}{pad} {side}"))?;
    }
    out.line(&look.paint(DIM, &format!("{bottom_left}{rule}{bottom_right}")))?;
    let dot = look.pick("·", "-");
    let hint = format!("  Drop files here or type paths {dot} /clean {dot} /help {dot} /quit");
    out.line(&look.paint(DIM, &hint))?;
    out.line("")
}

/// The words of a line, split on spaces outside quotes. With `posix` as a POSIX shell reads a
/// dropped file (backslash escapes, single and double quotes), otherwise as Windows quotes
/// paths (double quotes only, backslashes kept).
fn split(line: &str, posix: bool) -> Vec<String> {
    let (mut words, mut word, mut quote, mut started) = (vec![], String::new(), None, false);
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (None, c) if c.is_ascii_whitespace() => {
                if mem::take(&mut started) {
                    words.push(mem::take(&mut word));
                }
                continue;
            }
            (None, '\\') if posix => word.extend(chars.next()),
            (None, '"' | '\'') if posix || c == '"' => quote = Some(c),
            (Some(q), c) if c == q => quote = None,
            (Some('"'), '\\') if posix => match chars.next() {
                Some(escaped @ ('"' | '\\' | '$' | '`')) => word.push(escaped),
                other => {
                    word.push('\\');
                    word.extend(other);
                }
            },
            _ => word.push(c),
        }
        started = true;
    }
    if started {
        words.push(word);
    }
    words
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::Look;

    #[test]
    fn posix_lines_split_like_a_shell() {
        let split = |line| split(line, true);
        assert_eq!(split(r"/tmp/a\ b.pdf  c.pdf"), ["/tmp/a b.pdf", "c.pdf"]);
        assert_eq!(split("'/x y/it''s.pdf'"), ["/x y/its.pdf"]);
        assert_eq!(split(r#""a \"b\" \\c\d" e"#), [r#"a "b" \c\d"#, "e"]);
        assert_eq!(split(r#"'it'\''s' "#), ["it's"]);
        assert!(split(" \t\n").is_empty());
    }

    #[test]
    fn windows_lines_split_on_spaces_outside_double_quotes() {
        let split = |line| split(line, false);
        assert_eq!(
            split("\"C:\\a b\\c.pdf\" d.pdf\r\n"),
            [r"C:\a b\c.pdf", "d.pdf"]
        );
        assert_eq!(split(r"C:\x\it's.pdf"), [r"C:\x\it's.pdf"]);
        assert_eq!(split(r#"dir\"a b"\c.pdf"#), [r"dir\a b\c.pdf"]);
    }

    /// Runs interactive mode on `typed`, recording what `ask` was given.
    fn session(typed: &[u8], look: Look) -> (String, String) {
        let (mut out, mut err) = (Out::new(vec![], look), Out::new(vec![], look));
        run(typed, &mut out, &mut err, |paths, clean, out, _| {
            out.line(&format!(
                "{} {paths:?}",
                if clean { "clean" } else { "check" }
            ))
        })
        .unwrap();
        let text = |w: Vec<u8>| String::from_utf8(w).unwrap();
        (text(out.w), text(err.w))
    }

    #[test]
    fn a_session_checks_paths_and_cleans_the_last_line_until_quit() {
        let typed = "/clean\n\"a b.pdf\" c.pdf\nd.pdf\n\n/clean\n/help\n/x\n/quit\nlater.pdf\n";
        let (out, err) = session(typed.as_bytes(), Look::default());
        let screen = [
            "╭─────────────────────────────────────────────────────────────────────────╮",
            "│ ✻ doc-sanitize 0.1.0                                                    │",
            "│   Find and remove hidden data in PDF, Word, Excel and PowerPoint files. │",
            "╰─────────────────────────────────────────────────────────────────────────╯",
            "  Drop files here or type paths · /clean · /help · /quit",
            "",
            "› ",
            "›   check [\"a b.pdf\", \"c.pdf\"]",
            "",
            "›   check [\"d.pdf\"]",
            "",
            "› ›   clean [\"d.pdf\"]",
            "",
            "›   Type or drop one or more paths to check them (quote a path that has spaces).",
            "  /clean   write a cleaned copy of each file you just checked, beside it",
            "  /help    show this",
            "  /quit    leave (Ctrl+C works too)",
            "",
            "› ",
            "› ",
        ];
        assert_eq!(out, screen.join("\n"));
        assert_eq!(
            err,
            "  ✗ Nothing checked yet.\n    Type a path first, then /clean.\n  ✗ Unknown command '/x'.\n    Try /help.\n"
        );
    }

    #[test]
    fn the_end_of_input_leaves_and_an_ascii_console_gets_ascii() {
        let ascii = Look {
            ascii: true,
            ..Look::default()
        };
        let (out, _) = session(b"", ascii);
        assert!(out.starts_with("+------"));
        assert!(out.contains("| * doc-sanitize 0.1.0 "));
        assert!(out.ends_with("Drop files here or type paths - /clean - /help - /quit\n\n> \n"));
    }

    #[test]
    fn a_line_that_is_not_utf_8_is_a_path_and_no_stop() {
        let (out, _) = session(b"caf\xE9.pdf\n/quit\n", Look::default());
        assert!(out.contains("check [\"caf\u{FFFD}.pdf\"]"), "{out}");
    }
}
