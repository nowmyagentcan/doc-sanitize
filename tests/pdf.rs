//! PDF files: opening them under limits, with passwords, and every PDF check on generated
//! files.

mod common;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::*;
use lopdf::encryption::crypt_filters::{Aes256CryptFilter, CryptFilter};
use lopdf::{EncryptionState, EncryptionVersion, Permissions};

/// Checks `bytes` saved as `name`, with `args`, giving the file's JSON entry. Every PDF
/// fixture must finish within 10 s.
fn check_pdf(name: &str, bytes: &[u8], args: &[&str]) -> serde_json::Value {
    let dir = dir("pdf");
    put(&dir, &[(name, bytes)]);
    let start = Instant::now();
    let out = run_in(&dir, &[&[name, "--json"], args].concat());
    assert!(
        start.elapsed() < Duration::from_secs(10),
        "{name} took too long"
    );
    json(&out)["files"][0].clone()
}

fn error_of(f: &serde_json::Value) -> (&str, &str) {
    (
        f["error"]["code"].as_str().unwrap_or_default(),
        f["error"]["message"].as_str().unwrap_or_default(),
    )
}

/// A one-page PDF encrypted with AES-256 (R6).
fn aes256(owner: &str, user: &str) -> Vec<u8> {
    let mut doc = PdfBuilder::new();
    doc.page(&text_at(72, 720, "Locked away"));
    let mut doc = doc.finish();
    let filter: Arc<dyn CryptFilter> = Arc::new(Aes256CryptFilter);
    let state = EncryptionState::try_from(EncryptionVersion::V5 {
        encrypt_metadata: true,
        crypt_filters: BTreeMap::from([(b"StdCF".to_vec(), filter)]),
        file_encryption_key: &[7; 32],
        stream_filter: b"StdCF".to_vec(),
        string_filter: b"StdCF".to_vec(),
        owner_password: owner,
        user_password: user,
        permissions: Permissions::PRINTABLE,
    })
    .unwrap();
    doc.encrypt(&state).unwrap();
    save(&mut doc)
}

#[test]
fn passwords_open_encrypted_pdfs() {
    let f = check_pdf("owner.pdf", &aes256("owner-secret", ""), &[]);
    assert_eq!(f["error"], serde_json::Value::Null, "{f}");
    assert_eq!(f["format"], "pdf");

    let locked = aes256("owner-secret", "open-sesame");
    let f = check_pdf("user.pdf", &locked, &[]);
    assert_eq!(
        error_of(&f),
        ("password_required", "user.pdf needs a password to open.")
    );
    assert_eq!(f["error"]["hint"], "Pass it with --password.");
    let f = check_pdf("user.pdf", &locked, &["--password", "open-sesame"]);
    assert_eq!(f["error"], serde_json::Value::Null, "{f}");
    let f = check_pdf("user.pdf", &locked, &["--password", "nope"]);
    assert_eq!(
        error_of(&f),
        ("wrong_password", "The password for user.pdf isn't right.")
    );
    assert_eq!(f["error"]["hint"], "Check the password and try again.");
}

#[test]
fn other_security_handlers_are_named() {
    let mut objects = raw_page("");
    objects.push((4, b"<< /Filter /MyHandler /V 2 /R 3 >>".to_vec()));
    let bytes = raw(&objects, "/Root 1 0 R /Encrypt 4 0 R /ID [<00> <00>]");
    for args in [&[][..], &["--password", "x"]] {
        let f = check_pdf("custom.pdf", &bytes, args);
        assert_eq!(
            error_of(&f),
            (
                "unsupported_encryption",
                "custom.pdf uses encryption this tool can't open (/MyHandler)."
            )
        );
        assert_eq!(
            f["error"]["hint"],
            "Save an unencrypted copy from the app that made it."
        );
    }
}

#[test]
fn broken_structure_is_damaged_without_hanging() {
    // The page tree loops back to its root.
    let objects = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (3, b"<< /Type /Pages /Kids [2 0 R] /Count 1 >>".to_vec()),
    ];
    let f = check_pdf("page_cycle.pdf", &raw(&objects, "/Root 1 0 R"), &[]);
    assert_eq!(
        error_of(&f),
        ("damaged", "page_cycle.pdf is damaged: its page tree loops.")
    );

    // 20,000 nested arrays.
    let mut objects = raw_page("");
    let deep = format!("{}{}", "[".repeat(20_000), "]".repeat(20_000));
    objects[0].1 = format!("<< /Type /Catalog /Pages 2 0 R /Deep {deep} >>").into();
    let f = check_pdf("deep_array.pdf", &raw(&objects, "/Root 1 0 R"), &[]);
    assert_eq!(f["error"]["code"], "damaged", "{f}");

    // /Prev points at the same cross-reference section.
    let objects = raw_page("");
    let bytes = String::from_utf8(raw(&objects, "/Root 1 0 R")).unwrap();
    let xref = bytes.rsplit("startxref\n").next().unwrap().lines().next();
    let looped = raw(&objects, &format!("/Root 1 0 R /Prev {}", xref.unwrap()));
    let f = check_pdf("prev_loop.pdf", &looped, &[]);
    assert_eq!(f["error"], serde_json::Value::Null, "{f}");
}

#[test]
fn object_streams_that_list_themselves_are_read_or_refused() {
    let mut out = b"%PDF-1.7\n".to_vec();
    let objstm = stream_body("/Type /ObjStm /N 2 /First 8", b"4 0 5 3 <<>> null");
    let at = write_objects(
        &mut out,
        &[
            (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] >>".to_vec(),
            ),
            (4, objstm),
        ],
    );
    let xref_at = out.len();
    use XrefEntry::*;
    let entries = [
        Free,
        At(at[0]),
        At(at[1]),
        At(at[2]),
        At(at[3]),
        In(4, 1),
        At(xref_at),
    ];
    write_objects(&mut out, &[(6, xref_stream(&entries, "/Root 1 0 R"))]);
    out.extend(format!("startxref\n{xref_at}\n%%EOF\n").as_bytes());
    let f = check_pdf("objstm_self.pdf", &out, &[]);
    assert!(
        f["error"].is_null() || f["error"]["code"] == "damaged",
        "{f}"
    );
}

#[test]
fn an_object_only_in_the_hybrid_stream_is_read() {
    let mut out = b"%PDF-1.7\n".to_vec();
    let content = stream_body("", text_at(72, 720, "Hybrid").as_bytes());
    let at = write_objects(
        &mut out,
        &[
            (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Contents 5 0 R >>".to_vec(),
            ),
            (5, content),
        ],
    );
    let stm_at = out.len();
    use XrefEntry::*;
    let entries = [Free, Free, Free, Free, Free, At(at[3]), At(stm_at)];
    write_objects(&mut out, &[(6, xref_stream(&entries, ""))]);
    let xref_at = out.len();
    out.extend(b"xref\n0 4\n0000000000 65535 f \n");
    for a in &at[..3] {
        out.extend(format!("{a:010} 00000 n \n").as_bytes());
    }
    out.extend(
        format!(
            "trailer\n<< /Size 7 /Root 1 0 R /XRefStm {stm_at} >>\nstartxref\n{xref_at}\n%%EOF\n"
        )
        .as_bytes(),
    );
    let f = check_pdf("hybrid_xref.pdf", &out, &[]);
    assert_eq!(f["error"], serde_json::Value::Null, "{f}");
}

#[test]
fn a_stream_that_inflates_past_256_mib_is_too_large() {
    let mut objects = raw_page("/Contents 4 0 R");
    let bomb = zlib_zeros(260 << 20);
    objects.push((4, stream_body("/Filter /FlateDecode", &bomb)));
    let f = check_pdf("flate_bomb.pdf", &raw(&objects, "/Root 1 0 R"), &[]);
    assert_eq!(
        error_of(&f),
        (
            "too_large",
            "flate_bomb.pdf is too large to check (a stream once decoded is over 256 MiB)."
        )
    );
}

#[test]
fn a_wrong_stream_length_is_read_and_may_be_repaired() {
    for delta in [-100i64, 100] {
        let content = text_at(72, 720, "Lengthy");
        let mut objects = raw_page("/Contents 4 0 R");
        let mut body = stream_body("", content.as_bytes());
        let real = format!("/Length {}", content.len());
        let wrong = format!("/Length {}", content.len() as i64 + delta);
        body = String::from_utf8(body)
            .unwrap()
            .replace(&real, &wrong)
            .into();
        objects.push((4, body));
        let f = check_pdf("bad_length.pdf", &raw(&objects, "/Root 1 0 R"), &[]);
        assert_eq!(f["error"], serde_json::Value::Null, "{delta}: {f}");
        for w in f["warnings"].as_array().unwrap() {
            assert_eq!(w["code"], "repaired");
        }
    }
}

/// Each finding of `check` as (kind, where, count, detail).
fn pdf_kinds(f: &serde_json::Value, check: &str) -> Vec<(String, String, u64, String)> {
    findings_of(f, check)
        .into_iter()
        .map(|x| {
            (
                x["kind"].as_str().unwrap().into(),
                x["where"].to_string(),
                x["count"].as_u64().unwrap(),
                x["detail"].as_str().unwrap().into(),
            )
        })
        .collect()
}

fn k(kind: &str, place: &str, count: u64, detail: &str) -> (String, String, u64, String) {
    (kind.into(), place.into(), count, detail.into())
}

#[test]
fn structure_leaks_are_found_and_their_values_stay_out() {
    let dir = dir("pdf-structure");
    put(
        &dir,
        &[
            ("leaky.pdf", &leaky_structure()),
            ("plain.pdf", &plain_structure()),
        ],
    );
    let out = run_in(&dir, &["leaky.pdf", "--json"]);
    assert!(!stdout(&out).contains("SECRET-") && !stderr(&out).contains("SECRET-"));
    let f = &json(&out)["files"][0];
    let xmp = f["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["kind"] == "xmp")
        .unwrap()["where"]
        .to_string();
    assert_eq!(
        pdf_kinds(f, "metadata"),
        [
            k(
                "info_dict",
                r#"{"field":"/Info"}"#,
                2,
                "2 document properties, including the author"
            ),
            k("xmp", &xmp, 1, "1 XMP metadata packet"),
            k("object_xmp", "{}", 1, "1 object with its own XMP metadata"),
            k(
                "app_data",
                "{}",
                1,
                "1 object with private application data"
            ),
            k("thumbnail", "{}", 1, "1 page thumbnail"),
        ]
    );
    assert_eq!(
        pdf_kinds(f, "comments"),
        [k(
            "annotation",
            r#"{"page":1}"#,
            1,
            "1 annotation by 1 author"
        )]
    );
    assert_eq!(
        pdf_kinds(f, "embedded-files"),
        [
            k("attachment", "{}", 1, "1 attached file"),
            k("associated_file", "{}", 1, "1 associated file"),
            k(
                "portfolio",
                "{}",
                1,
                "the file is a portfolio that shows its attachments as its content"
            ),
            k(
                "file_annotation",
                r#"{"page":1}"#,
                1,
                "1 file attached to an annotation"
            ),
        ]
    );
    assert_eq!(
        pdf_kinds(f, "macros"),
        [
            k(
                "javascript",
                "{}",
                1,
                "1 script that runs when the file is opened or used"
            ),
            k(
                "action",
                "{}",
                1,
                "1 action that opens a file, imports data or sends form data"
            ),
        ]
    );
    assert_eq!(
        pdf_kinds(f, "external-links"),
        [
            k("remote_go_to", "{}", 1, "1 link to another file"),
            k("file_uri", "{}", 1, "1 link to a local file"),
        ]
    );
    let image = pdf_kinds(f, "image-metadata");
    assert_eq!(
        (image.len(), image[0].3.as_str()),
        (1, "1 picture with EXIF and GPS")
    );
    assert_eq!(
        pdf_kinds(f, "signatures"),
        [k(
            "pdf_signature",
            "{}",
            2,
            "2 digital signatures, including usage rights"
        )]
    );
    assert_eq!(
        pdf_kinds(f, "form-data"),
        [
            k("xfa", "{}", 1, "an XFA form in 1 part"),
            k(
                "hidden_field_value",
                "{}",
                1,
                "1 form field with a value no page shows"
            ),
        ]
    );

    let shown = stdout(&run_in(&dir, &["leaky.pdf", "--show-values"]));
    for secret in [
        "/Title=SECRET-pdf-title-1",
        "xmp:CreatorTool=SECRET-pdf-tool-1; dc:creator=SECRET-pdf-xmp-1",
        "page 1: SECRET-pdf-comment-1",
        "SECRET-pdf-attached-1.txt",
        "page 1: SECRET-pdf-note-file-1.txt",
        "app.alert('SECRET-pdf-js-1')",
        "SECRET-pdf-launch-1.exe",
        r"D:\Projects\avery\SECRET-pdf-goto-1.pdf",
        "file:///home/avery/SECRET-pdf-uri-1.txt",
        "SECRET-pdf-field-1",
    ] {
        assert!(shown.contains(secret), "{secret} in {shown}");
    }

    let f = &json(&run_in(&dir, &["plain.pdf", "--json"]))["files"][0];
    assert_eq!(f["status"], "none_found", "{f}");
}
