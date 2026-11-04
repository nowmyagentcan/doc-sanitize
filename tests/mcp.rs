//! Talks to `doc-sanitize mcp` over stdio the way MCP clients do.

mod common;

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use common::*;
use serde_json::{Value, json};

const BIN: &str = env!("CARGO_BIN_EXE_doc-sanitize");

const INSTRUCTIONS: &str = "Check and clean hidden data in local PDF, Word, Excel and PowerPoint files before they are shared, and verify whether a PDF redaction is real. Pass absolute file paths. Findings leave out the hidden text itself unless show_values is true; keep it off when you only need to know what is there.";

const CHECK: &str = "Check local PDF, Word (.docx), Excel (.xlsx) and PowerPoint (.pptx) files for hidden data before they are shared: document properties and author names, comments, tracked changes (including deleted text), hidden text, sheets, rows, slides and objects, speaker notes, embedded files, custom XML, macros and scripts, links to local files, picture metadata such as GPS, cropped picture areas, pivot caches, earlier PDF revisions, and fake redactions (text still under black boxes). Use it to verify whether a PDF redaction is real. Read-only. Returns every check that ran with its result; the hidden text itself is left out unless show_values is true. A file that can't be read gets its own error entry; on invalid arguments the result has isError true and one text block holding {\"error\":{\"code\",\"message\",\"hint\"}}.";

const CLEAN: &str = "Write a cleaned copy of local PDF, DOCX, XLSX or PPTX files with hidden data removed (properties, comments, tracked changes accepted, hidden content, speaker notes, embedded files, macros, picture metadata, fake redactions: pages with hidden text become pictures, and more), then re-check the copy and save it only if nothing removable is left. The original file is never changed. keep lists check ids to leave in place. Returns what was removed, side effects such as formulas replaced by their values, the output path and the re-check.";

const INITIALIZE: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"test","version":"1.0.0"}}}"#;

/// A folder holding `leaky.docx` and `blank.pdf`.
fn folder(test: &str) -> PathBuf {
    let dir = dir(&format!("mcp-{test}"));
    put(
        &dir,
        &[("leaky.docx", &leaky_docx("docx")), ("blank.pdf", PDF)],
    );
    dir
}

/// Sends each message from `dir`, closes stdin, and returns the responses by id and the exit
/// code. The server writes nothing to stderr.
fn session(dir: &Path, messages: &[Value]) -> (Vec<Value>, i32) {
    let mut child = Command::new(BIN)
        .arg("mcp")
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    writeln!(stdin, "{INITIALIZE}").unwrap();
    let initialized = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
    writeln!(stdin, "{initialized}").unwrap();
    for m in messages {
        writeln!(stdin, "{m}").unwrap();
    }
    drop(stdin);
    let out = child.wait_with_output().unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stderr), "");
    let mut responses: Vec<Value> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    responses.sort_by_key(|r| r["id"].as_u64());
    (responses, out.status.code().unwrap())
}

fn call(id: u64, tool: &str, arguments: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {"name": tool, "arguments": arguments}})
}

/// The answer to one tool call.
fn result(dir: &Path, tool: &str, arguments: Value) -> Value {
    let (responses, code) = session(dir, &[call(2, tool, arguments)]);
    assert_eq!(code, 0);
    responses[1]["result"].clone()
}

/// The CLI's `--json` output for `args`, run in `dir`.
fn cli(dir: &Path, args: &[&str]) -> Value {
    json(&run_in(dir, &[args, &["--json"]].concat()))
}

/// An error result: one text block holding the usage object and no structured content.
fn refused(result: &Value, message: &str) {
    assert_eq!(result["isError"], true, "{result}");
    assert!(result.get("structuredContent").is_none(), "{result}");
    let blocks = result["content"].as_array().unwrap();
    assert_eq!(blocks.len(), 1);
    let e: Value = serde_json::from_str(blocks[0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(e["error"]["code"], "usage");
    let got = e["error"]["message"].as_str().unwrap();
    assert!(got.contains(message), "{got}");
}

/// A result holding the report as structured content and as its one text block.
fn report(result: &Value) -> &Value {
    assert_eq!(result["isError"], false, "{result}");
    let text = result["content"][0]["text"].as_str().unwrap();
    let parsed: Value = serde_json::from_str(text).unwrap();
    assert_eq!(parsed, result["structuredContent"]);
    &result["structuredContent"]
}

#[test]
fn initializes_and_lists_both_tools_as_documented() {
    let list = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"});
    let (responses, code) = session(&folder("list"), &[list]);
    assert_eq!(code, 0);
    let init = &responses[0]["result"];
    assert_eq!(
        init["serverInfo"],
        json!({"name": "doc-sanitize", "version": "0.1.0"})
    );
    assert_eq!(init["instructions"], INSTRUCTIONS);
    let tools = responses[1]["result"]["tools"].as_array().unwrap();
    let mut tools: Vec<_> = tools.iter().collect();
    tools.sort_by_key(|t| t["name"].as_str());
    let [check, clean] = tools[..] else {
        panic!("{tools:?}")
    };
    assert_eq!(
        (&check["name"], &check["description"]),
        (&json!("check_document"), &json!(CHECK))
    );
    assert_eq!(
        (&clean["name"], &clean["description"]),
        (&json!("clean_document"), &json!(CLEAN))
    );
    assert_eq!(
        check["annotations"],
        json!({"readOnlyHint": true, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false})
    );
    assert_eq!(
        clean["annotations"],
        json!({"readOnlyHint": false, "destructiveHint": false, "idempotentHint": false, "openWorldHint": false})
    );
    let names = |s: &Value| {
        let mut n: Vec<String> = s["properties"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        n.sort();
        n.join(" ")
    };
    let (input, cleaning) = (&check["inputSchema"], &clean["inputSchema"]);
    assert_eq!(names(input), "password paths recursive show_values");
    assert_eq!(
        names(cleaning),
        "keep out_dir output overwrite password paths recursive show_values"
    );
    for schema in [input, cleaning] {
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(schema["required"], json!(["paths"]));
        assert!(!schema.to_string().contains("\"format\""), "{schema}");
        let paths = &schema["properties"]["paths"];
        assert_eq!(
            (&paths["minItems"], &paths["maxItems"]),
            (&json!(1), &json!(100))
        );
        assert_eq!(schema["properties"]["recursive"]["default"], false);
    }
    let keep = cleaning["properties"]["keep"]["items"]["enum"]
        .as_array()
        .unwrap();
    assert_eq!(keep.len(), 20);
    assert!(!keep.contains(&json!("signatures")));
    assert!(!keep.contains(&json!("old-revisions")));
    assert_eq!(
        check["outputSchema"]["required"],
        json!(["version", "command", "files", "summary"])
    );
    assert_eq!(
        clean["outputSchema"]["required"],
        json!(["version", "command", "files", "summary"])
    );
}

#[test]
fn check_equals_the_cli_and_writes_nothing() {
    let dir = folder("check");
    let before = fs::read_dir(&dir).unwrap().count();
    let args = json!({"paths": ["leaky.docx", "blank.pdf", "missing.pdf"]});
    let answer = result(&dir, "check_document", args);
    assert_eq!(
        report(&answer),
        &cli(&dir, &["leaky.docx", "blank.pdf", "missing.pdf"])
    );
    assert_eq!(fs::read_dir(&dir).unwrap().count(), before);
    assert!(secrets_in(answer.to_string().as_bytes()).is_empty());
    let keys: Vec<_> = answer["structuredContent"]
        .as_object()
        .unwrap()
        .keys()
        .collect();
    assert_eq!(keys, ["version", "command", "files", "summary"]);
    let shown = result(
        &dir,
        "check_document",
        json!({"paths": ["leaky.docx"], "show_values": true}),
    );
    assert_eq!(report(&shown), &cli(&dir, &["leaky.docx", "--show-values"]));
    assert!(!secrets_in(shown.to_string().as_bytes()).is_empty());
}

#[test]
fn a_folder_is_read_as_the_cli_reads_it() {
    let dir = folder("folder");
    put(&dir, &[("deep/inner.pdf", PDF)]);
    let abs = dir.to_str().unwrap();
    let answer = result(
        &dir,
        "check_document",
        json!({"paths": [abs], "recursive": true}),
    );
    assert_eq!(report(&answer), &cli(&dir, &[abs, "-r"]));
    assert_eq!(answer["structuredContent"]["summary"]["files"], 3);
}

#[test]
fn clean_writes_the_copy_and_gives_its_absolute_path() {
    let dir = folder("clean");
    let args = json!({"paths": ["leaky.docx"], "keep": ["comments"]});
    let answer = result(&dir, "clean_document", args);
    let file = &report(&answer)["files"][0];
    assert_eq!(file["status"], "cleaned", "{file}");
    assert_eq!(file["kept"], json!(["comments"]));
    let output = PathBuf::from(file["output"].as_str().unwrap());
    assert!(output.is_absolute(), "{output:?}");
    assert_eq!(output.file_name().unwrap(), "leaky.clean.docx");
    assert!(output.exists());
    assert!(
        !secrets_in(answer.to_string().as_bytes())
            .iter()
            .any(|_| true)
    );

    let out_dir = dir.join("outbox");
    let args = json!({"paths": ["blank.pdf"], "out_dir": out_dir});
    let answer = result(&dir, "clean_document", args);
    let file = &report(&answer)["files"][0];
    assert_eq!(file["status"], "cleaned", "{file}");
    assert!(out_dir.join("blank.clean.pdf").exists());

    let args = json!({"paths": ["blank.pdf"], "output": "named.pdf"});
    report(&result(&dir, "clean_document", args.clone()));
    assert!(dir.join("named.pdf").exists());
    let again = result(&dir, "clean_document", args);
    assert_eq!(report(&again)["files"][0]["error"]["code"], "output_exists");
    let args = json!({"paths": ["blank.pdf"], "output": "named.pdf", "overwrite": true});
    assert_eq!(
        report(&result(&dir, "clean_document", args))["files"][0]["status"],
        "cleaned"
    );
}

#[test]
fn clean_equals_the_cli_for_absolute_outputs() {
    let dir = folder("same");
    let out = dir.join("copy.docx");
    let out = out.to_str().unwrap();
    let answer = result(
        &dir,
        "clean_document",
        json!({"paths": ["leaky.docx"], "output": out}),
    );
    let mcp = report(&answer).clone();
    fs::remove_file(out).unwrap();
    assert_eq!(mcp, cli(&dir, &["clean", "leaky.docx", "-o", out]));
}

#[test]
fn invalid_or_unknown_arguments_are_refused_without_structured_content() {
    let dir = folder("bad");
    let many = vec!["blank.pdf"; 101];
    for (tool, args, message) in [
        ("check_document", json!({}), "paths"),
        ("check_document", json!({"paths": []}), "No file given."),
        ("check_document", json!({"paths": many}), "Too many paths."),
        ("check_document", json!({"paths": "a"}), "invalid type"),
        (
            "check_document",
            json!({"paths": ["blank.pdf"], "pages": 1}),
            "unknown field",
        ),
        (
            "clean_document",
            json!({"paths": ["blank.pdf"], "keep": ["nope"]}),
            "Unknown check \"nope\".",
        ),
        (
            "clean_document",
            json!({"paths": ["blank.pdf"], "keep": ["signatures"]}),
            "Signatures can't be kept",
        ),
        (
            "clean_document",
            json!({"paths": ["blank.pdf", "leaky.docx"], "output": "x.pdf"}),
            "-o works with one file; you gave 2.",
        ),
        (
            "clean_document",
            json!({"paths": ["blank.pdf"], "output": "x.pdf", "out_dir": "y"}),
            "can't go together",
        ),
        (
            "clean_document",
            json!({"paths": ["blank.pdf"], "output": "blank.pdf"}),
            "The output would replace the input file.",
        ),
    ] {
        refused(&result(&dir, tool, args), message);
    }
    assert_eq!(fs::read_dir(&dir).unwrap().count(), 2);
}

#[test]
fn exits_quietly_when_stdin_closes() {
    let mut child = Command::new(BIN)
        .arg("mcp")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdin.take());
    let out = child.wait_with_output().unwrap();
    assert_eq!(
        (out.status.code(), out.stdout.len(), out.stderr.len()),
        (Some(0), 0, 0)
    );
}

#[test]
fn exits_quietly_when_stdout_closes() {
    // The read end closes before the spawn: dropped after it, a test spawning beside this one
    // could inherit a copy and keep the pipe open.
    let (reader, writer) = std::io::pipe().unwrap();
    drop(reader);
    let mut child = Command::new(BIN)
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(writer)
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let _ = writeln!(stdin, "{INITIALIZE}");
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        assert!(start.elapsed() < Duration::from_secs(10), "still running");
        thread::sleep(Duration::from_millis(20));
    };
    drop(stdin);
    let out = child.wait_with_output().unwrap();
    assert_eq!(
        (status.code(), String::from_utf8_lossy(&out.stderr).as_ref()),
        (Some(0), "")
    );
}

#[test]
fn has_a_help_and_takes_no_arguments() {
    let out = run(&["mcp", "--help"]);
    assert_eq!(out.status.code(), Some(0));
    assert!(stdout(&out).starts_with("Run the MCP server on stdio.\n"));
    assert!(stdout(&run(&["--help"])).contains("\n  mcp    Run the MCP server on stdio\n"));
    let extra = run(&["mcp", "x.pdf"]);
    assert_eq!(extra.status.code(), Some(2));
    assert!(stderr(&extra).starts_with("✗ "), "{}", stderr(&extra));
}
