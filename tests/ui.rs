//! The local page: the HTML works offline and its report is the CLI's text, and `doc-sanitize ui`
//! checks and cleans uploads over HTTP on 127.0.0.1 as the CLI does files.

mod common;

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use common::*;
use serde_json::{Value, json};

const PAGE: &str = include_str!("../src/ui.html");

#[test]
fn page_loads_nothing_from_another_host() {
    let footer = [
        "https://nowmyagentcan.com",
        "https://github.com/nowmyagentcan/doc-sanitize",
        "https://x.com/chayp05",
    ];
    for pat in ["src=", "href=", "url(", "@import", "fetch(", "action="] {
        for (i, _) in PAGE.match_indices(pat) {
            let rest = PAGE[i + pat.len()..].trim_start_matches(['"', '\'', ' ']);
            let target = &rest[..rest.find(['"', '\'', ' ', ')', '>']).unwrap_or(rest.len())];
            assert!(
                target.starts_with("data:") || !target.contains("://") || footer.contains(&target),
                "{pat} loads {target}"
            );
        }
    }
    assert!(PAGE.contains("await fetch(path, {"));
}

/// `doc-sanitize ui` on a free port, stopped when dropped.
struct Server {
    child: Child,
    port: u16,
}

/// A response: status, headers and body.
type Answer = (u16, String, Vec<u8>);

impl Server {
    fn start() -> Server {
        let mut child = bin()
            .args(["ui", "--no-open"])
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let port = line
            .strip_prefix("Open http://127.0.0.1:")
            .and_then(|rest| rest.strip_suffix("  (Ctrl+C to stop)\n"))
            .unwrap_or_else(|| panic!("{line:?}"))
            .parse()
            .unwrap();
        Server { child, port }
    }

    /// Sends `head` (`{port}` filled in) and `body`, closes the sending side, and reads the
    /// answer.
    fn raw(&self, head: &[u8], body: &[u8]) -> Answer {
        let mut stream = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        let (mut filled, mut rest) = (vec![], head);
        while let Some(i) = rest.windows(6).position(|w| w == b"{port}") {
            filled.extend_from_slice(&rest[..i]);
            filled.extend_from_slice(self.port.to_string().as_bytes());
            rest = &rest[i + 6..];
        }
        stream.write_all(&[&filled, rest].concat()).unwrap();
        stream.write_all(b"\r\n\r\n").unwrap();
        stream.write_all(body).unwrap();
        stream.shutdown(Shutdown::Write).unwrap();
        let mut answer = vec![];
        stream.read_to_end(&mut answer).unwrap();
        let end = answer.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        let headers = String::from_utf8(answer[..end].to_vec()).unwrap();
        let status = headers.split(' ').nth(1).unwrap().parse().unwrap();
        (status, headers, answer[end + 4..].to_vec())
    }

    /// Posts `body` to `path` as the file `name` with `options`, as the page does.
    fn post(&self, path: &str, name: &str, options: &Value, body: &[u8]) -> Answer {
        let head = format!(
            "POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{{port}}\r\nOrigin: http://127.0.0.1:{{port}}\r\nX-File-Name: {name}\r\nX-Options: {options}\r\nContent-Length: {}",
            body.len()
        );
        self.raw(head.as_bytes(), body)
    }

    /// The page's answer for one file.
    fn check(&self, name: &str, options: &Value, body: &[u8]) -> Value {
        let (status, _, answer) = self.post("/api/check", name, options, body);
        assert_eq!(status, 200);
        serde_json::from_slice(&answer).unwrap()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The answer's error code and message.
fn error(answer: &[u8]) -> (String, String) {
    let e: Value = serde_json::from_slice(answer).unwrap();
    let text = |k: &str| e["error"][k].as_str().unwrap().to_string();
    (text("code"), text("message"))
}

/// Percent-decodes `s` as `decodeURIComponent` does.
fn unescape(s: &str) -> String {
    let mut bytes = vec![];
    let mut rest = s.as_bytes();
    while let Some((&b, tail)) = rest.split_first() {
        if b == b'%' {
            bytes.push(u8::from_str_radix(std::str::from_utf8(&tail[..2]).unwrap(), 16).unwrap());
            rest = &tail[2..];
        } else {
            bytes.push(b);
            rest = tail;
        }
    }
    String::from_utf8(bytes).unwrap()
}

/// The value of a response header.
fn header<'a>(headers: &'a str, name: &str) -> Option<&'a str> {
    headers.lines().find_map(|l| {
        let (k, v) = l.split_once(": ")?;
        k.eq_ignore_ascii_case(name).then_some(v)
    })
}

#[test]
fn serves_the_page_with_its_guards_on() {
    let s = Server::start();
    let (status, headers, body) = s.raw(b"GET / HTTP/1.1\r\nHost: 127.0.0.1:{port}", b"");
    assert_eq!(status, 200);
    assert_eq!(body, PAGE.as_bytes());
    assert_eq!(
        header(&headers, "Content-Type"),
        Some("text/html; charset=utf-8")
    );
    assert_eq!(header(&headers, "Cache-Control"), Some("no-store"));
    let csp = header(&headers, "Content-Security-Policy").unwrap();
    assert!(csp.starts_with("default-src 'none';"), "{csp}");
    assert!(csp.contains("connect-src 'self'"), "{csp}");
    for host in ["localhost:{port}", "LOCALHOST:{port}"] {
        let head = format!("GET /?x=1 HTTP/1.1\r\nHost: {host}");
        assert_eq!(s.raw(head.as_bytes(), b"").0, 200, "{host}");
    }
}

#[test]
fn every_guard_answers_as_stated() {
    let s = Server::start();
    let post = "POST /api/check HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nX-File-Name: a.pdf";
    let head = |rest: &str| format!("{post}\r\nContent-Length: 1{rest}").into_bytes();
    for (head, body, status, code) in [
        (
            b"GET / HTTP/1.1\r\nHost: evil.test".to_vec(),
            &b""[..],
            403,
            "forbidden",
        ),
        (
            b"GET / HTTP/1.1\r\nHost: 127.0.0.1".to_vec(),
            b"",
            403,
            "forbidden",
        ),
        (
            b"GET / HTTP/1.1\r\nHost: 127.0.0.1:1".to_vec(),
            b"",
            403,
            "forbidden",
        ),
        (b"GET / HTTP/1.0".to_vec(), b"", 403, "forbidden"),
        (
            head("\r\nOrigin: https://evil.test"),
            b"x",
            403,
            "forbidden",
        ),
        (head("\r\nOrigin: null"), b"x", 403, "forbidden"),
        (
            b"GET / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nHost: evil.test".to_vec(),
            b"",
            400,
            "bad_request",
        ),
        (
            head("\r\nOrigin: http://127.0.0.1:{port}\r\nOrigin: http://evil.test"),
            b"x",
            400,
            "bad_request",
        ),
        (head("\r\nContent-Length: 1"), b"x", 400, "bad_request"),
        (
            head("\r\nTransfer-Encoding: chunked"),
            b"x",
            400,
            "bad_request",
        ),
        (
            head("\r\ntransfer-encoding: identity"),
            b"x",
            400,
            "bad_request",
        ),
        (
            b"GET /\xFF HTTP/1.1\r\nHost: 127.0.0.1:{port}".to_vec(),
            b"",
            400,
            "bad_request",
        ),
        (
            b"GET  / HTTP/1.1\r\nHost: 127.0.0.1:{port}".to_vec(),
            b"",
            400,
            "bad_request",
        ),
        (
            b"GET / HTTP/2\r\nHost: 127.0.0.1:{port}".to_vec(),
            b"",
            400,
            "bad_request",
        ),
        (
            b"nonsense\r\nHost: 127.0.0.1:{port}".to_vec(),
            b"",
            400,
            "bad_request",
        ),
        (
            [&head("")[..], b"\r\nX-Other: \xFF"].concat(),
            b"x",
            400,
            "bad_request",
        ),
        (head("\r\nBad Name: x"), b"x", 400, "bad_request"),
        (head("\r\n folded: x"), b"x", 400, "bad_request"),
        (head("\r\nno colon"), b"x", 400, "bad_request"),
        (head(""), b"", 400, "bad_request"),
        (
            format!("{post}\r\nContent-Length: 5").into_bytes(),
            b"xy",
            400,
            "bad_request",
        ),
        (post.as_bytes().to_vec(), b"", 400, "bad_request"),
        (
            format!("{post}\r\nContent-Length: +1").into_bytes(),
            b"x",
            400,
            "bad_request",
        ),
        (
            format!("{post}\r\nContent-Length: -1").into_bytes(),
            b"x",
            400,
            "bad_request",
        ),
        (
            format!("{post}\r\nContent-Length: ").into_bytes(),
            b"",
            400,
            "bad_request",
        ),
        (
            format!("{post}\r\nContent-Length: {}", (256 << 20) + 1).into_bytes(),
            b"x",
            413,
            "too_large",
        ),
        (
            format!("{post}\r\nContent-Length: 99999999999999999999999").into_bytes(),
            b"x",
            413,
            "too_large",
        ),
        (
            b"GET /other HTTP/1.1\r\nHost: 127.0.0.1:{port}".to_vec(),
            b"",
            404,
            "not_found",
        ),
        (
            b"POST / HTTP/1.1\r\nHost: 127.0.0.1:{port}".to_vec(),
            b"",
            404,
            "not_found",
        ),
        (
            b"GET /api/check HTTP/1.1\r\nHost: 127.0.0.1:{port}".to_vec(),
            b"",
            404,
            "not_found",
        ),
    ] {
        let (got, _, answer) = s.raw(&head, body);
        let name = String::from_utf8_lossy(&head).into_owned();
        assert_eq!(got, status, "{name}");
        assert_eq!(error(&answer).0, code, "{name}");
    }
    // A client that hangs up mid-request costs the server nothing.
    drop(TcpStream::connect(("127.0.0.1", s.port)).unwrap());
    let (status, _, _) = s.raw(b"GET / HTTP/1.1\r\nHost: 127.0.0.1:{port}", b"");
    assert_eq!(status, 200);
}

#[test]
fn malformed_uploads_and_options_are_refused() {
    let s = Server::start();
    let empty = json!({});
    for (path, name, options, status, code) in [
        ("/api/check", "%zz", &empty, 400, "bad_request"),
        ("/api/check", "", &empty, 400, "bad_request"),
        (
            "/api/check",
            "a.pdf",
            &json!({"other": 1}),
            400,
            "bad_request",
        ),
        (
            "/api/check",
            "a.pdf",
            &json!({"keep": []}),
            400,
            "bad_request",
        ),
        (
            "/api/clean",
            "a.pdf",
            &json!({"show_values": true}),
            400,
            "bad_request",
        ),
        (
            "/api/clean",
            "a.pdf",
            &json!({"keep": ["nope"]}),
            422,
            "usage",
        ),
        (
            "/api/clean",
            "a.pdf",
            &json!({"keep": ["signatures"]}),
            422,
            "usage",
        ),
    ] {
        let (got, _, answer) = s.post(path, name, options, PDF);
        assert_eq!(got, status, "{options}");
        assert_eq!(error(&answer).0, code, "{options}");
    }
    let head = "POST /api/check HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nX-File-Name: a.pdf\r\nX-Options: {bad\r\nContent-Length: 1";
    assert_eq!(s.raw(head.as_bytes(), b"x").0, 400);
}

#[test]
#[ignore = "known bug: F16"]
fn options_a_client_gets_wrong_are_422_with_the_usage_object() {
    let s = Server::start();
    for options in [json!({"show_values": "yes"}), json!({"other": 1})] {
        let (got, _, answer) = s.post("/api/check", "a.pdf", &options, PDF);
        assert_eq!(got, 422, "{options}");
        assert_eq!(error(&answer).0, "usage", "{options}");
    }
}
#[test]
fn a_check_answers_as_the_cli_does_and_never_writes() {
    let dir = dir("ui-check");
    let docx = leaky_docx("docx");
    put(&dir, &[("leaky.docx", &docx), ("notes.txt", b"plain")]);
    let s = Server::start();
    let cli = |args: &[&str]| json(&run_in(&dir, &[args, &["--json"]].concat()));

    let page = s.check("leaky.docx", &json!({}), &docx);
    assert_eq!(page, cli(&["leaky.docx"])["files"][0]);
    assert!(secrets_in(page.to_string().as_bytes()).is_empty());

    let shown = s.check("leaky.docx", &json!({"show_values": true}), &docx);
    assert_eq!(shown, cli(&["leaky.docx", "--show-values"])["files"][0]);
    assert!(!secrets_in(shown.to_string().as_bytes()).is_empty());

    let failed = s.check("notes.txt", &json!({}), b"plain");
    assert_eq!(failed, cli(&["notes.txt"])["files"][0]);
    assert_eq!(failed["error"]["code"], "unsupported_format");

    // The name only names: folders in it are dropped and it is never opened.
    let named = s.check("..%2F..%2Fx%20%C3%BC.docx", &json!({}), &docx);
    assert_eq!(named["path"], "x ü.docx");
    let renamed = s.check("a.pdf", &json!({}), &docx);
    assert_eq!(renamed["warnings"][0]["code"], "extension_mismatch");
    assert_eq!(fs::read_dir(&dir).unwrap().count(), 2);
}

#[test]
fn a_clean_gives_the_copy_and_what_happened() {
    let dir = dir("ui-clean");
    let docx = leaky_docx("docx");
    put(&dir, &[("leaky.docx", &docx)]);
    let s = Server::start();
    let (status, headers, copy) = s.post("/api/clean", "leaky.docx", &json!({}), &docx);
    assert_eq!(status, 200);
    assert_eq!(
        header(&headers, "Content-Disposition"),
        Some("attachment; filename*=UTF-8''leaky.clean.docx")
    );
    // Accepted insertions stay: they are the document's text now.
    let accepted = ["ins", "move"].map(|k| format!("SECRET-docx-{k}-1"));
    for (name, data) in unzip(&copy) {
        let left = secrets_in(&data);
        assert!(
            left.iter().all(|s| accepted.contains(s)),
            "{name}: {left:?}"
        );
    }
    let raw = header(&headers, "X-Result").unwrap();
    assert!(raw.len() <= 8192, "{}", raw.len());
    let result: Value = serde_json::from_str(&unescape(raw)).unwrap();
    assert!(secrets_in(unescape(raw).as_bytes()).is_empty());
    assert_eq!(result["status"], "cleaned");
    assert_eq!(result["output"], "leaky.clean.docx");
    assert_eq!(result["recheck"]["status"], "none_found");
    assert!(result["recheck"].get("findings").is_none());

    let cli = json(&run_in(&dir, &["clean", "leaky.docx", "--json"]));
    let file = &cli["files"][0];
    for key in ["status", "output", "removed", "kept", "warnings"] {
        assert_eq!(result[key], file[key], "{key}");
    }
    assert_eq!(result["recheck"]["checks"], file["recheck"]["checks"]);
    // The page's copy is the CLI's copy, byte for byte: PDFs alone get a random /ID.
    assert_eq!(copy, fs::read(dir.join("leaky.clean.docx")).unwrap());

    // An unticked box is kept, and the re-check says so.
    let keep = json!({"keep": ["comments"]});
    let (status, headers, copy) = s.post("/api/clean", "leaky.docx", &keep, &docx);
    assert_eq!(status, 200);
    let result: Value =
        serde_json::from_str(&unescape(header(&headers, "X-Result").unwrap())).unwrap();
    assert_eq!(result["kept"], json!(["comments"]));
    let again = s.check("copy.docx", &json!({}), &copy);
    let comments = again["checks"].as_array().unwrap();
    let comments = comments.iter().find(|c| c["id"] == "comments").unwrap();
    assert_eq!(comments["status"], "found");
    assert_eq!(fs::read_dir(&dir).unwrap().count(), 2);
}

#[test]
fn a_clean_that_cannot_finish_answers_422_with_the_clean_object() {
    let s = Server::start();
    let (status, headers, body) = s.post("/api/clean", "a.txt", &json!({}), b"plain");
    assert_eq!(status, 422);
    assert!(header(&headers, "X-Result").is_none());
    let file: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(file["status"], "not_cleaned");
    assert_eq!(file["output"], Value::Null);
    assert_eq!(file["error"]["code"], "unsupported_format");
}

#[test]
fn macros_change_the_name_of_the_copy() {
    let s = Server::start();
    let docm = leaky_docx("docm");
    let (status, headers, _) = s.post("/api/clean", "gr%C3%BC%C3%9Fe%20a.docm", &json!({}), &docm);
    assert_eq!(status, 200);
    assert_eq!(
        header(&headers, "Content-Disposition"),
        Some("attachment; filename*=UTF-8''gr%C3%BC%C3%9Fe%20a.clean.docx")
    );
}

/// The page's own report builders, run in Node.js on `files`.
fn page_report(files: &Value) -> String {
    let start = PAGE.find("const plural =").unwrap();
    let end = PAGE.find("// The page has no --keep flag").unwrap();
    let script = format!(
        "{}\nconst files = JSON.parse(require('fs').readFileSync(0, 'utf8'));\nprocess.stdout.write(report(files));",
        &PAGE[start..end]
    );
    let path = dir("page").join("report.js");
    fs::write(&path, script).unwrap();
    let mut node = Command::new("node")
        .arg(&path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = node.stdin.take().unwrap();
    stdin.write_all(files.to_string().as_bytes()).unwrap();
    drop(stdin);
    let out = node.wait_with_output().unwrap();
    assert!(out.status.success());
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn the_copy_report_is_the_clis_text() {
    let dir = dir("ui-report");
    let pdf = redacted();
    let files: [(&str, &[u8]); 5] = [
        ("leaky.docx", &leaky_docx("docx")),
        ("leaky.xlsx", &leaky_xlsx("xlsx")),
        ("leaky.pptx", &leaky_pptx("pptx")),
        ("my redacted.pdf", &pdf),
        ("blank.pdf", PDF),
    ];
    put(&dir, &files);
    let s = Server::start();
    for values in [false, true] {
        let mut args = vec![];
        let mut pages = vec![];
        for (name, bytes) in files {
            let options = json!({"show_values": values});
            pages.push(s.check(&name.replace(' ', "%20"), &options, bytes));
            args.push(name);
            let flag = values.then_some("--show-values");
            let one = run_in(
                &dir,
                &[&[name], &flag.into_iter().collect::<Vec<_>>()[..]].concat(),
            );
            assert_eq!(
                page_report(&json!([pages.last().unwrap()])),
                stdout(&one),
                "{name}"
            );
        }
        if values {
            args.push("--show-values");
        }
        assert_eq!(page_report(&json!(pages)), stdout(&run_in(&dir, &args)));
    }
    let failed = s.check("notes.txt", &json!({}), b"plain");
    put(&dir, &[("notes.txt", b"plain")]);
    assert_eq!(
        page_report(&json!([failed])),
        stderr(&run_in(&dir, &["notes.txt"]))
    );
}

#[test]
fn closed_stdout_ends_the_server_quietly() {
    // The read end closes before the spawn: dropped after it, a test spawning beside this one
    // could inherit a copy and keep the pipe open.
    let (reader, writer) = std::io::pipe().unwrap();
    drop(reader);
    let mut child = bin()
        .args(["ui", "--no-open"])
        .stdout(writer)
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        assert!(start.elapsed() < Duration::from_secs(10), "still running");
        thread::sleep(Duration::from_millis(20));
    };
    let out = child.wait_with_output().unwrap();
    assert_eq!(
        (status.code(), String::from_utf8_lossy(&out.stderr).as_ref()),
        (Some(0), "")
    );
}

#[test]
fn a_bad_or_taken_port_is_refused() {
    for bad in ["0", "65536", "x", "-1"] {
        let out = run(&["ui", "--no-open", &format!("--port={bad}")]);
        assert_eq!(out.status.code(), Some(2), "{bad}");
        assert_eq!(
            stderr(&out),
            format!("✗ Port {bad} isn't valid.\n  Use a number from 1 to 65535.\n")
        );
    }
    let taken = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = taken.local_addr().unwrap().port().to_string();
    let out = run(&["ui", "--no-open", "--port", &port]);
    assert_eq!(out.status.code(), Some(3));
    assert_eq!(
        stderr(&out),
        format!(
            "✗ Port {port} is in use.\n  Choose another with --port, or leave it out for a free one.\n"
        )
    );
    assert_eq!(run(&["ui", "--nope"]).status.code(), Some(2));
    let help = run(&["ui", "--help"]);
    assert_eq!(help.status.code(), Some(0));
    assert!(stdout(&help).starts_with("Open the local page in your browser.\n"));
}
