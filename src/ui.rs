//! The local page on 127.0.0.1: `GET /` serves it, `POST /api/check` and `POST /api/clean` work
//! on one uploaded file, held in memory only.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, Shutdown, TcpListener, TcpStream};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Mutex, MutexGuard};
use std::thread;
use std::time::Duration;

use doc_sanitize::input::MAX_FILE;
use doc_sanitize::report::{CleanResult, CleanStatus, Error, safe};
use doc_sanitize::run::{self, Options};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::json;

const PAGE: &str = include_str!("ui.html");
/// The most a request may send in its line and headers.
const HEAD_LIMIT: u64 = 16 * 1024;
/// The page reaches nothing but its own server.
const CSP: &str = "default-src 'none'; style-src 'unsafe-inline'; script-src 'unsafe-inline'; img-src data:; connect-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";
const JSON: &str = "application/json";
/// A change's sentence that lists more pages than `X-Result` has room for.
const LONG_DETAIL: usize = 500;

/// Requests are answered side by side, but one job runs at a time: each can take a few hundred
/// megabytes.
static JOB: Mutex<()> = Mutex::new(());

struct Reply {
    status: u16,
    kind: &'static str,
    /// Further header lines, each ending in CRLF.
    headers: String,
    body: Vec<u8>,
}

impl Reply {
    fn new(status: u16, kind: &'static str, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status,
            kind,
            headers: String::new(),
            body: body.into(),
        }
    }

    fn json(status: u16, value: &impl serde::Serialize) -> Self {
        let body = serde_json::to_vec(value).expect("results are plain data");
        Self::new(status, JSON, body)
    }

    fn error(status: u16, code: &'static str, message: &str) -> Self {
        Self::new(status, JSON, Error::request(code, message).to_json())
    }

    fn bad(message: &str) -> Self {
        Self::error(400, "bad_request", message)
    }
}

/// What the page sends in `X-Options` with `/api/check`.
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct CheckOptions {
    password: Option<String>,
    #[serde(default)]
    show_values: bool,
}

/// What the page sends in `X-Options` with `/api/clean`.
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct CleanOptions {
    password: Option<String>,
    #[serde(default)]
    keep: Vec<String>,
}

/// Serves on 127.0.0.1:`port` (0 picks a free port) until the process is stopped, and opens
/// the page in the default browser when `open` is set. `stop` says how to stop it.
pub fn serve(port: u16, open: bool, stop: &str) -> Result<(), Error> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port)).map_err(|e| {
        if e.kind() == io::ErrorKind::AddrInUse {
            Error::port_in_use(port)
        } else {
            Error::server(e)
        }
    })?;
    let port = listener.local_addr().map_or(port, |a| a.port());
    let url = format!("http://127.0.0.1:{port}");
    // With stdout closed nobody learns the address, so end quietly as other commands do.
    match writeln!(io::stdout(), "Open {url}{stop}") {
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => return Ok(()),
        Err(e) => return Err(Error::server(e)),
        Ok(()) => {}
    }
    if open {
        browse(url);
    }
    for stream in listener.incoming().flatten() {
        // A client that hangs up or stalls loses only its own connection.
        thread::spawn(move || handle(&stream, port));
    }
    Ok(())
}

/// Asks the OS to open `url`; when it can't, the printed address is enough.
fn browse(url: String) {
    let mut command = if cfg!(windows) {
        let mut c = Command::new("cmd");
        c.args(["/C", "start", ""]);
        c
    } else if cfg!(target_os = "macos") {
        Command::new("open")
    } else {
        Command::new("xdg-open")
    };
    command
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // Some openers wait for the browser to close, so they run beside the server.
    thread::spawn(move || command.status());
}

/// One request, one response, then the connection closes.
fn handle(stream: &TcpStream, port: u16) -> io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    let reply = respond(&mut BufReader::new(stream.take(HEAD_LIMIT)), port)?;
    let reason = match reply.status {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        413 => "Content Too Large",
        _ => "Unprocessable Content",
    };
    let mut out = stream;
    write!(
        out,
        "HTTP/1.1 {} {reason}\r\nContent-Type: {}\r\nContent-Length: {}\r\n{}\
         Cache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\n\
         Content-Security-Policy: {CSP}\r\nConnection: close\r\n\r\n",
        reply.status,
        reply.kind,
        reply.body.len(),
        reply.headers,
    )?;
    out.write_all(&reply.body)?;
    stream.shutdown(Shutdown::Write)?;
    // Read what the client still sends, so closing doesn't reset the connection before it
    // has read the answer.
    io::copy(&mut stream.take(1 << 20), &mut io::sink()).map(drop)
}

fn respond(reader: &mut BufReader<io::Take<&TcpStream>>, port: u16) -> io::Result<Reply> {
    let (mut request, mut line) = (Vec::new(), Vec::new());
    reader.read_until(b'\n', &mut request)?;
    let (mut host, mut origin, mut length, mut name, mut options) = (None, None, None, None, None);
    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line)? == 0 {
            return Ok(Reply::bad("The request ended early."));
        }
        if line.iter().all(|b| b"\r\n".contains(b)) {
            break;
        }
        let Some((key, value)) = header(&line) else {
            return Ok(Reply::bad("Can't read a request header."));
        };
        let field = match key.to_ascii_lowercase().as_str() {
            "host" => &mut host,
            "origin" => &mut origin,
            "content-length" => &mut length,
            "x-file-name" => &mut name,
            "x-options" => &mut options,
            // Bodies come only with a Content-Length; chunked ones aren't read.
            "transfer-encoding" => return Ok(Reply::bad("Send the body with a Content-Length.")),
            _ => continue,
        };
        // A second value could disagree with the first one checked (RFC 9112 §3.2, §6.3).
        if field.replace(value.to_owned()).is_some() {
            return Ok(Reply::bad("A request header is repeated."));
        }
    }
    // RFC 9112 §3: the request line is ASCII.
    let request = std::str::from_utf8(&request).ok().filter(|r| r.is_ascii());
    let mut parts = request
        .unwrap_or_default()
        .trim_end_matches(['\r', '\n'])
        .split(' ');
    let (Some(method), Some(target), Some("HTTP/1.1" | "HTTP/1.0"), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Ok(Reply::bad("Can't read the request line."));
    };
    let local = |scheme: &str, value: &str| {
        ["127.0.0.1", "localhost"]
            .iter()
            .any(|h| value.eq_ignore_ascii_case(&format!("{scheme}{h}:{port}")))
    };
    if !host.is_some_and(|h| local("", &h)) || origin.is_some_and(|o| !local("http://", &o)) {
        return Ok(Reply::error(
            403,
            "forbidden",
            "Only this computer's page can use doc-sanitize ui.",
        ));
    }
    let path = target.split_once('?').map_or(target, |(path, _)| path);
    let job: fn(&str, Option<&str>, &[u8]) -> Reply = match (method, path) {
        ("GET", "/") => return Ok(Reply::new(200, "text/html; charset=utf-8", PAGE)),
        ("POST", "/api/check") => check,
        ("POST", "/api/clean") => clean,
        _ => return Ok(Reply::error(404, "not_found", "There's nothing here.")),
    };
    // Digits only: Rust would also read a leading `+`. A number too long for u64 is too large.
    let length = length.filter(|l| !l.is_empty() && l.bytes().all(|b| b.is_ascii_digit()));
    let Some(length) = length.map(|l| l.parse().unwrap_or(u64::MAX)) else {
        return Ok(Reply::bad("Can't read the request length."));
    };
    if length > MAX_FILE {
        return Ok(Reply::error(
            413,
            "too_large",
            "The file is larger than 256 MiB.",
        ));
    }
    let buffered = reader.buffer().len() as u64;
    reader.get_mut().set_limit(length.saturating_sub(buffered));
    let mut body = Vec::with_capacity(length as usize);
    let read = reader.take(length).read_to_end(&mut body);
    if read.is_err() || (body.len() as u64) < length {
        return Ok(Reply::bad("The request ended early."));
    }
    let Some(name) = name.as_deref().and_then(file_name) else {
        return Ok(Reply::bad("Can't read the file name."));
    };
    Ok(job(&name, options.as_deref(), &body))
}

/// A header line's name and value. Malformed: a name that isn't a token, bytes that aren't
/// UTF-8, or a NUL or CR inside the value (RFC 9110 §5.5).
fn header(line: &[u8]) -> Option<(&str, &str)> {
    let line = std::str::from_utf8(line).ok()?;
    let line = line.strip_suffix('\n')?;
    let line = line.strip_suffix('\r').unwrap_or(line);
    let (name, value) = line.split_once(':')?;
    let token = !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b));
    (token && !value.contains(['\0', '\r'])).then_some((name, value.trim()))
}

/// The options of a request, or the 400 for ones that can't be read.
fn options<T: DeserializeOwned + Default>(options: Option<&str>) -> Result<T, Reply> {
    options.map_or(Ok(T::default()), |o| {
        serde_json::from_str(o).map_err(|e| Reply::bad(&format!("Can't read the options: {e}.")))
    })
}

fn one_at_a_time() -> MutexGuard<'static, ()> {
    JOB.lock().unwrap_or_else(|e| e.into_inner())
}

/// Checks one upload, answering with the `files[0]` object of the check JSON.
fn check(name: &str, header: Option<&str>, bytes: &[u8]) -> Reply {
    let o: CheckOptions = match options(header) {
        Ok(o) => o,
        Err(reply) => return reply,
    };
    let options = Options {
        show_values: o.show_values,
        password: o.password,
    };
    let _job = one_at_a_time();
    Reply::json(200, &run::check_bytes(name, bytes, &options))
}

/// Cleans one upload, answering with the copy and what happened in `X-Result`, or with 422.
fn clean(name: &str, header: Option<&str>, bytes: &[u8]) -> Reply {
    let o: CleanOptions = match options(header) {
        Ok(o) => o,
        Err(reply) => return reply,
    };
    let keep = match run::keep_ids(&o.keep) {
        Ok(keep) => keep,
        Err(e) => return Reply::new(422, JSON, e.to_json()),
    };
    let options = Options {
        show_values: false,
        password: o.password,
    };
    let _job = one_at_a_time();
    let (file, copy) = run::clean_bytes(name, bytes, &options, &keep);
    match (file.status, copy) {
        (CleanStatus::Cleaned | CleanStatus::CleanedIncomplete, Some(copy)) => {
            let output = file.output.as_deref().unwrap_or_default();
            let mut reply = Reply::new(200, "application/octet-stream", copy);
            reply.headers = format!(
                "Content-Disposition: attachment; filename*=UTF-8''{}\r\nX-Result: {}\r\n",
                escape(output),
                escape(&result(&file)),
            );
            reply
        }
        _ => Reply::json(422, &file),
    }
}

/// The compact JSON of `X-Result`: all the page shows of a cleaned file, findings left out.
fn result(file: &CleanResult) -> String {
    let mut changes = file.changes.clone();
    for c in &mut changes {
        c.slides = None;
        if let Some(pages) = c.pages.take()
            && c.detail.len() > LONG_DETAIL
        {
            c.detail = format!(
                "{} pages are now pictures; their text can't be selected or searched.",
                pages.len()
            );
        }
    }
    let recheck = file
        .recheck
        .as_ref()
        .map(|r| json!({"status": r.status, "checks": r.checks}));
    json!({
        "status": file.status,
        "output": file.output,
        "removed": file.removed,
        "kept": file.kept,
        "changes": changes,
        "warnings": file.warnings,
        "recheck": recheck,
    })
    .to_string()
}

/// The shown name of an `X-File-Name`: percent-decoded UTF-8, its last path component only.
fn file_name(header: &str) -> Option<String> {
    let name = unescape(header)?;
    let name = name.rsplit(['/', '\\']).next().unwrap_or_default();
    Path::new(name).file_stem()?;
    Some(safe(name))
}

/// Percent-decodes `s` as UTF-8.
fn unescape(s: &str) -> Option<String> {
    let mut bytes = Vec::with_capacity(s.len());
    let mut rest = s.as_bytes();
    while let Some((&b, tail)) = rest.split_first() {
        rest = tail;
        if b == b'%' {
            let hex = tail
                .get(..2)
                .filter(|h| h.iter().all(u8::is_ascii_hexdigit))?;
            bytes.push(u8::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok()?);
            rest = &tail[2..];
        } else {
            bytes.push(b);
        }
    }
    String::from_utf8(bytes).ok()
}

/// Percent-encodes `s` as `encodeURIComponent` does, which also suits a header's `filename*`.
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out += &format!("%{b:02X}");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names_are_percent_decoded_and_lose_their_folders() {
        let name = |s| file_name(s);
        assert_eq!(
            name("Gr%C3%BC%C3%9Fe%20a.pdf").as_deref(),
            Some("Grüße a.pdf")
        );
        assert_eq!(name("..%2F..%2Fa.pdf").as_deref(), Some("a.pdf"));
        assert_eq!(name("C%3A%5Cx%5Ca.pdf").as_deref(), Some("a.pdf"));
        assert_eq!(name("a%0D%0Ab.pdf").as_deref(), Some("a<U+000D>\nb.pdf"));
        for bad in ["", "%", "%4", "%zz", "%+1", "%C3", "x%2F"] {
            assert_eq!(name(bad), None, "{bad}");
        }
    }

    #[test]
    fn escaping_round_trips_and_leaves_no_header_breaks() {
        let s = "Grüße \"a\"\r\n%.pdf";
        let e = escape(s);
        assert!(e.is_ascii() && !e.contains(['"', '\r', '\n', ' ']), "{e}");
        assert_eq!(unescape(&e).as_deref(), Some(s));
    }

    #[test]
    fn headers_are_tokens_and_utf_8_values() {
        assert_eq!(header(b"Host: a:1\r\n"), Some(("Host", "a:1")));
        assert_eq!(header(b"X:\n"), Some(("X", "")));
        for bad in [
            &b"Host : a\r\n"[..],
            b" folded: a\r\n",
            b"no colon\r\n",
            b": a\r\n",
            b"X: \xFF\r\n",
            b"X: a\rb\r\n",
            b"X: a\0\r\n",
            b"X: a",
        ] {
            assert_eq!(header(bad), None, "{bad:?}");
        }
    }
}
