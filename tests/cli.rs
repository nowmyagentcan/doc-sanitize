//! The command line: commands and paths, folders, text and JSON output, exit codes and closed
//! streams.

mod common;

use std::process::Stdio;

use common::*;

#[test]
fn version_prints_the_name_and_version() {
    for flag in ["--version", "-V"] {
        let out = run(&[flag]);
        assert!(out.status.success());
        assert_eq!(stdout(&out), "doc-sanitize 0.1.0\n");
    }
}

#[test]
fn help_lists_every_check_what_isnt_checked_and_examples() {
    let out = run(&["--help"]);
    assert!(out.status.success());
    let help = stdout(&out);
    assert!(help.starts_with(
        "Find and remove hidden data in PDF, Word, Excel and PowerPoint files.\n\nUsage: doc-sanitize [check] <PATH>..."
    ));
    let checks = help
        .split(
            "Checks:
",
        )
        .nth(1)
        .unwrap()
        .split(
            "

",
        )
        .next()
        .unwrap();
    assert_eq!(checks.lines().count(), 22);
    for line in [
        "  hidden-rows-columns  hidden rows and columns that hold values",
        "  form-data            XFA forms and values of hidden form fields",
        "Not checked: legacy .doc/.xls/.ppt, .xlsb, Strict Open XML, ODF, RTF and password-protected",
        "  doc-sanitize redacted.pdf --json",
    ] {
        assert!(help.contains(&format!("{line}\n")), "{line}");
    }
    assert_eq!(stdout(&run(&["-h"])), help);
}

#[test]
fn usage_errors_exit_2_as_text_or_json() {
    let out = run(&[]);
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(
        stderr(&out),
        "✗ No file given.\n  Try: doc-sanitize contract.docx\n"
    );

    let out = run(&["a.pdf", "--x"]);
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(
        stderr(&out),
        "✗ Unknown option --x.\n  See doc-sanitize --help.\n"
    );

    let out = run(&["--x", "--json"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stderr.is_empty());
    let e = json(&out);
    assert_eq!(e["error"]["code"], "usage");
    assert_eq!(e["error"]["message"], "Unknown option --x.");
    assert_eq!(e["error"]["hint"], "See doc-sanitize --help.");

    let out = run(&["--json"]);
    assert_eq!(json(&out)["error"]["message"], "No file given.");
}

#[test]
fn only_an_exact_first_argument_is_a_command() {
    let dir = dir("command");
    put(&dir, &[("check", PDF), ("clean", PDF), ("x.pdf", PDF)]);
    let out = run_in(&dir, &["check", "x.pdf", "--json"]);
    assert_eq!(json(&out)["files"].as_array().unwrap().len(), 1);
    let sep = std::path::MAIN_SEPARATOR;
    let out = run_in(&dir, &[&format!(".{sep}check"), "--json"]);
    assert_eq!(json(&out)["files"][0]["format"], "pdf");
    assert_eq!(json(&out)["files"][0]["path"], format!(".{sep}check"));
    let out = run_in(&dir, &["check", "check", "--json"]);
    assert_eq!(json(&out)["files"][0]["path"], "check");
    // Options go anywhere; after `--` everything is a path.
    let out = run_in(&dir, &["--json", "x.pdf", "--", "--json"]);
    let files = json(&out)["files"].clone();
    assert_eq!(files[1]["path"], "--json");
    assert_eq!(files[1]["error"]["code"], "not_found");
}

#[test]
fn json_has_the_report_envelope() {
    let dir = dir("envelope");
    put(&dir, &[("fake.docx", PDF)]);
    let out = run_in(&dir, &["fake.docx", "--json"]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        stdout(&out),
        r#"{
  "version": "0.1.0",
  "command": "check",
  "files": [
    {
      "path": "fake.docx",
      "format": "pdf",
      "variant": "pdf",
      "status": "none_found",
      "checks": [],
      "findings": [],
      "warnings": [
        {
          "code": "extension_mismatch",
          "message": "fake.docx is a .pdf file with a .docx name."
        }
      ],
      "error": null
    }
  ],
  "summary": {
    "files": 1,
    "with_findings": 0,
    "incomplete": 0,
    "failed": 0
  }
}
"#
    );
}

#[test]
fn files_that_cant_be_checked_say_why() {
    let dir = dir("refused");
    put(
        &dir,
        &[
            ("empty.pdf", b""),
            ("notes.rtf", b"{\\rtf1 hello}"),
            ("page.html", b"<!doctype html><p>hi"),
        ],
    );
    let out = run_in(
        &dir,
        &["empty.pdf", "notes.rtf", "page.html", "gone.docx", "--json"],
    );
    assert_eq!(out.status.code(), Some(1));
    let report = json(&out);
    let files = report["files"].as_array().unwrap();
    for (f, (code, message)) in files.iter().zip([
        (
            "unsupported_format",
            "empty.pdf isn't a PDF, Word, Excel or PowerPoint file.",
        ),
        (
            "unsupported_format",
            "notes.rtf isn't a PDF, Word, Excel or PowerPoint file.",
        ),
        (
            "unsupported_format",
            "page.html isn't a PDF, Word, Excel or PowerPoint file.",
        ),
        ("not_found", "Can't find gone.docx."),
    ]) {
        assert_eq!(f["status"], "error");
        assert_eq!(f["format"], serde_json::Value::Null);
        assert_eq!(f["error"]["code"], code);
        assert_eq!(f["error"]["message"], message);
        assert_eq!(f["checks"], serde_json::json!([]));
    }
    assert_eq!(
        files[0]["error"]["hint"],
        "doc-sanitize reads .pdf, .docx, .xlsx and .pptx files, and their macro and template versions."
    );
    assert_eq!(files[3]["error"]["hint"], "Check the path.");
    assert_eq!(report["summary"]["failed"], 4);
}

#[test]
fn a_device_or_locked_file_is_unreadable() {
    let device = if cfg!(windows) { "NUL" } else { "/dev/null" };
    let out = run(&[device, "--json"]);
    let f = &json(&out)["files"][0];
    assert_eq!(f["error"]["code"], "unreadable", "{f}");
    assert_eq!(f["error"]["hint"], "Check that you can open the file.");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let dir = dir("locked");
        put(&dir, &[("locked.pdf", PDF)]);
        let path = dir.join("locked.pdf");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::read(&path).is_err() {
            let out = run_in(&dir, &["locked.pdf"]);
            assert_eq!(out.status.code(), Some(1));
            assert!(
                stderr(&out)
                    .starts_with("✗ Can't read locked.pdf: Permission denied (os error 13).\n"),
                "{}",
                stderr(&out)
            );
        }
    }
}

#[test]
fn folders_expand_sorted_and_skip_cleaned_copies() {
    let dir = dir("folders");
    put(
        &dir,
        &[
            ("in/b.pdf", PDF),
            ("in/A.PDF", PDF),
            ("in/a.clean.pdf", PDF),
            ("in/a.clean-2.pdf", PDF),
            ("in/notes.txt", b"x"),
            ("in/sub/c.pdf", PDF),
            ("in/sub/deeper/d.pdf", PDF),
        ],
    );
    let paths = |args: &[&str]| -> Vec<String> {
        let report = json(&run_in(&dir, args));
        let files = report["files"].as_array().unwrap().clone();
        files
            .iter()
            .map(|f| f["path"].as_str().unwrap().replace('\\', "/"))
            .collect()
    };
    assert_eq!(paths(&["in", "--json"]), ["in/A.PDF", "in/b.pdf"]);
    assert_eq!(
        paths(&["in", "-r", "--json"]),
        [
            "in/A.PDF",
            "in/b.pdf",
            "in/sub/c.pdf",
            "in/sub/deeper/d.pdf"
        ]
    );
    // Named files are read whatever their name.
    assert_eq!(paths(&["in/a.clean.pdf", "--json"]), ["in/a.clean.pdf"]);

    put(&dir, &[("none/notes.txt", b"x"), ("none/sub/e.pdf", PDF)]);
    let out = run_in(&dir, &["none"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        stderr(&out),
        "✗ No PDF, Word, Excel or PowerPoint files in none.\n  Pass files directly, or use -r for subfolders.\n"
    );
    assert_eq!(paths(&["none", "-r", "--json"]), ["none/sub/e.pdf"]);
}

#[test]
fn text_output_has_a_line_per_file_and_a_summary() {
    let dir = dir("text");
    put(&dir, &[("a.pdf", PDF), ("Q3 filing.pdf", PDF)]);
    let out = run_in(&dir, &["a.pdf", "gone.pdf", "Q3 filing.pdf"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        stdout(&out),
        "a.pdf  nothing found by 0 checks\n\nQ3 filing.pdf  nothing found by 0 checks\n\n0 of 3 files have hidden data · 1 failed\n"
    );
    assert_eq!(
        stderr(&out),
        "\n✗ Can't find gone.pdf.\n  Check the path.\n"
    );
    // Colour only on a terminal: never on a pipe, so NO_COLOR changes nothing here.
    let plain = bin()
        .current_dir(&dir)
        .args(["a.pdf"])
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    assert_eq!(stdout(&plain), "a.pdf  nothing found by 0 checks\n");
    assert!(!stdout(&out).contains('\x1b'));
}

#[test]
fn exit_codes_follow_the_worst_file() {
    let dir = dir("exit");
    put(&dir, &[("a.pdf", PDF), ("b.pdf", PDF)]);
    assert_eq!(run_in(&dir, &["a.pdf", "b.pdf"]).status.code(), Some(0));
    assert_eq!(
        run_in(&dir, &["a.pdf", "gone.pdf", "b.pdf"]).status.code(),
        Some(1)
    );
}

#[test]
fn debug_prints_timings_on_stderr() {
    let dir = dir("debug");
    put(&dir, &[("a.pdf", PDF)]);
    let out = run_in(&dir, &["a.pdf", "--debug", "--json"]);
    let err = stderr(&out);
    assert!(
        err.starts_with("debug: a.pdf: pdf, ") && err.ends_with(" ms\n"),
        "{err}"
    );
    assert_eq!(json(&out)["files"][0]["status"], "none_found");
}

/// Runs with stdout or stderr a pipe whose reading end is already closed.
fn closed(dir: &std::path::Path, args: &[&str], stdout: bool) -> std::process::Output {
    let (reader, writer) = std::io::pipe().unwrap();
    drop(reader);
    let mut c = bin();
    c.current_dir(dir).args(args);
    if stdout {
        c.stdout(writer).stderr(Stdio::piped());
    } else {
        c.stderr(writer).stdout(Stdio::piped());
    }
    c.output().unwrap()
}

#[test]
fn a_closed_stream_ends_quietly_with_the_code_so_far() {
    let dir = dir("closed");
    let many: Vec<(String, &[u8])> = (0..50).map(|i| (format!("f{i:02}.pdf"), PDF)).collect();
    let many: Vec<(&str, &[u8])> = many.iter().map(|(n, b)| (n.as_str(), *b)).collect();
    put(&dir, &many);
    for debug in [false, true] {
        let mut args = vec!["gone.pdf", "."];
        if debug {
            args.push("--debug");
        }
        let out = closed(&dir, &args, true);
        assert_eq!(out.status.code(), Some(1), "stdout closed, debug {debug}");
        assert!(!stderr(&out).contains("panicked"), "{}", stderr(&out));
        let out = closed(&dir, &args, false);
        assert_eq!(out.status.code(), Some(1), "stderr closed, debug {debug}");
        let mut args = vec!["--help"];
        if debug {
            args.push("--debug");
        }
        assert_eq!(closed(&dir, &args, true).status.code(), Some(0));
    }
}

#[test]
fn a_file_named_clean_is_a_path() {
    let dir = dir("named-clean");
    put(&dir, &[("clean", PDF)]);
    let sep = std::path::MAIN_SEPARATOR;
    let out = run_in(&dir, &[&format!(".{sep}clean"), "--json"]);
    assert_eq!(json(&out)["files"][0]["format"], "pdf");
}

#[test]
fn a_run_checks_at_most_10000_files() {
    let dir = dir("many");
    std::fs::create_dir(dir.join("in")).unwrap();
    for i in 0..10_001 {
        std::fs::write(dir.join(format!("in/{i:05}.pdf")), PDF).unwrap();
    }
    let out = run_in(&dir, &["in", "--json"]);
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(
        json(&out)["error"]["message"],
        "More than 10,000 files to check."
    );
}
