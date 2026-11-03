//! PDF files: opening them under limits, with passwords, and every PDF check on generated
//! files.

mod common;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::*;
use doc_sanitize::report::thousands;
use lopdf::encryption::crypt_filters::{Aes256CryptFilter, CryptFilter};
use lopdf::{EncryptionState, EncryptionVersion, Permissions, dictionary};

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

/// A cross-reference section whose /Prev points at itself.
fn prev_loop() -> Vec<u8> {
    let objects = raw_page("");
    let bytes = String::from_utf8(raw(&objects, "/Root 1 0 R")).unwrap();
    let xref = bytes.rsplit("startxref\n").next().unwrap().lines().next();
    raw(&objects, &format!("/Root 1 0 R /Prev {}", xref.unwrap()))
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
    let f = check_pdf("deep_array.pdf", &deep_array(), &[]);
    assert_eq!(f["error"]["code"], "damaged", "{f}");

    let f = check_pdf("prev_loop.pdf", &prev_loop(), &[]);
    assert_eq!(f["error"], serde_json::Value::Null, "{f}");
}

/// An object stream that lists itself.
fn objstm_self() -> Vec<u8> {
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
    out
}

#[test]
fn object_streams_that_list_themselves_are_read_or_refused() {
    let f = check_pdf("objstm_self.pdf", &objstm_self(), &[]);
    assert!(
        f["error"].is_null() || f["error"]["code"] == "damaged",
        "{f}"
    );
}

/// A page's content listed only in the /XRefStm of a hybrid file.
fn hybrid_xref() -> Vec<u8> {
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
    out
}

#[test]
fn an_object_only_in_the_hybrid_stream_is_read() {
    let f = check_pdf("hybrid_xref.pdf", &hybrid_xref(), &[]);
    assert_eq!(f["error"], serde_json::Value::Null, "{f}");
}

#[test]
fn a_stream_that_inflates_past_256_mib_is_too_large() {
    let f = check_pdf("flate_bomb.pdf", &flate_bomb(), &[]);
    assert_eq!(
        error_of(&f),
        (
            "too_large",
            "flate_bomb.pdf is too large to check (a stream once decoded is over 256 MiB)."
        )
    );
}

/// A page stream with a /Length off by `delta`.
fn bad_length(delta: i64) -> Vec<u8> {
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
    raw(&objects, "/Root 1 0 R")
}

#[test]
fn a_wrong_stream_length_is_read_and_may_be_repaired() {
    for delta in [-100i64, 100] {
        let f = check_pdf("bad_length.pdf", &bad_length(delta), &[]);
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
            found(
                "info_dict",
                r#"{"field":"/Info"}"#,
                2,
                "2 document properties, including the author"
            ),
            found("xmp", &xmp, 1, "1 XMP metadata packet"),
            found("object_xmp", "{}", 1, "1 object with its own XMP metadata"),
            found(
                "app_data",
                "{}",
                1,
                "1 object with private application data"
            ),
            found("thumbnail", "{}", 1, "1 page thumbnail"),
        ]
    );
    assert_eq!(
        pdf_kinds(f, "comments"),
        [found(
            "annotation",
            r#"{"page":1}"#,
            1,
            "1 annotation by 1 author"
        )]
    );
    assert_eq!(
        pdf_kinds(f, "embedded-files"),
        [
            found("attachment", "{}", 1, "1 attached file"),
            found("associated_file", "{}", 1, "1 associated file"),
            found(
                "portfolio",
                "{}",
                1,
                "the file is a portfolio that shows its attachments as its content"
            ),
            found(
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
            found(
                "javascript",
                "{}",
                1,
                "1 script that runs when the file is opened or used"
            ),
            found(
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
            found("remote_go_to", "{}", 1, "1 link to another file"),
            found("file_uri", "{}", 1, "1 link to a local file"),
        ]
    );
    let image = pdf_kinds(f, "image-metadata");
    assert_eq!(
        (image.len(), image[0].3.as_str()),
        (1, "1 picture with EXIF and GPS")
    );
    assert_eq!(
        pdf_kinds(f, "signatures"),
        [found(
            "pdf_signature",
            "{}",
            2,
            "2 digital signatures, including usage rights"
        )]
    );
    assert_eq!(
        pdf_kinds(f, "form-data"),
        [
            found("xfa", "{}", 1, "an XFA form in 1 part"),
            found(
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

/// A PDF whose second revision removes a sentence from the page and the author from its
/// properties.
fn incremental() -> Vec<u8> {
    let mut b = PdfBuilder::new();
    let page = b.page(&format!(
        "{}{}",
        text_at(72, 720, "Kept sentence."),
        text_at(72, 700, "SECRET-pdf-removed-1")
    ));
    let info = b.add(dictionary! {
        "Author" => lopdf::Object::string_literal("SECRET-pdf-author-1"),
        "Title" => lopdf::Object::string_literal("Draft"),
    });
    let mut doc = b.finish();
    doc.trailer.set("Info", info);
    let first = save(&mut doc);
    let old = lopdf::Document::load_mem(&first).unwrap();
    let contents = old.get_page_contents(page)[0];
    let mut update = lopdf::IncrementalDocument::create_from(first, old);
    let new = &mut update.new_document;
    let content = text_at(72, 720, "Kept sentence.").into_bytes();
    new.set_object(
        contents,
        lopdf::Stream::new(lopdf::Dictionary::new(), content),
    );
    new.set_object(
        info,
        dictionary! { "Title" => lopdf::Object::string_literal("Draft") },
    );
    let mut out = vec![];
    update.save_to(&mut out).unwrap();
    out
}

#[test]
fn earlier_revisions_name_what_changed_after_them() {
    let f = check_pdf("incremental.pdf", &incremental(), &[]);
    assert_eq!(
        pdf_kinds(&f, "old-revisions"),
        [found(
            "earlier_revision",
            r#"{"revision":1}"#,
            1,
            "1 earlier revision; 2 objects were changed or removed after it, including page content and document properties"
        )]
    );
    assert_eq!(pdf_kinds(&f, "leftover-data"), []);

    // Made from a one-page PDF with `qpdf --linearize --object-streams=disable
    // --deterministic-id`: its first-page section isn't a revision.
    let f = check_pdf(
        "linearized.pdf",
        include_bytes!("fixtures/linearized.pdf"),
        &[],
    );
    assert_eq!(pdf_kinds(&f, "old-revisions"), []);
    assert_eq!(pdf_kinds(&f, "leftover-data"), []);
}

/// A one-page PDF with a ZIP after its end, and the ZIP.
fn polyglot() -> (Vec<u8>, Vec<u8>) {
    let mut polyglot = one_page(&text_at(72, 720, "Front"));
    let zip = zip(&package("docx", &[], &[]));
    polyglot.extend(&zip);
    polyglot.extend(b"\n\0\n");
    (polyglot, zip)
}

/// An object body in the bytes that no cross-reference table lists, an object nothing uses,
/// and an image the page never draws.
fn unlisted() -> Vec<u8> {
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut objects = raw_page("/Resources << /XObject << /Im1 5 0 R >> >> /Contents 6 0 R");
    objects.push((5, stream_body("/Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8", b"\0")));
    objects.push((6, stream_body("", b"q Q")));
    objects.push((7, b"(SECRET-pdf-unused-1)".to_vec()));
    let at = write_objects(&mut out, &objects);
    write_objects(&mut out, &[(9, b"(SECRET-pdf-unlisted-1)".to_vec())]);
    let xref = out.len();
    out.extend(b"xref\n0 8\n0000000000 65535 f \n");
    for n in 1..8u32 {
        match objects.iter().position(|o| o.0 == n) {
            Some(i) => out.extend(format!("{:010} 00000 n \n", at[i]).as_bytes()),
            None => out.extend(b"0000000000 65535 f \n"),
        }
    }
    out.extend(
        format!("trailer\n<< /Size 8 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
    );
    out
}

#[test]
fn leftovers_are_found_in_the_bytes_and_the_objects() {
    let (polyglot, zip) = polyglot();
    let f = check_pdf("polyglot.pdf", &polyglot, &[]);
    // The ZIP ends in zeros (an empty comment length), which aren't counted.
    let size = zip.iter().rposition(|&b| b != 0).unwrap() + 1;
    let after = format!("{} bytes after the PDF ends", thousands(size as u64));
    assert_eq!(
        pdf_kinds(&f, "leftover-data"),
        [found("data_outside_pdf", "{}", size as u64, &after)]
    );
    let mut before = b"%!PS junk\n".to_vec();
    before.extend(one_page(""));
    let f = check_pdf("before.pdf", &before, &[]);
    assert_eq!(
        pdf_kinds(&f, "leftover-data"),
        [found(
            "data_outside_pdf",
            "{}",
            10,
            "10 bytes before the PDF starts"
        )]
    );

    let f = check_pdf("unlisted.pdf", &unlisted(), &[]);
    assert_eq!(
        pdf_kinds(&f, "leftover-data"),
        [
            found(
                "unused_objects",
                "{}",
                1,
                "1 object that nothing in the file uses"
            ),
            found(
                "unlisted_objects",
                "{}",
                1,
                "1 object body that no cross-reference table lists"
            ),
            found(
                "unused_resources",
                r#"{"page":1}"#,
                1,
                "1 picture or drawing that nothing on the page shows"
            ),
        ]
    );
}

/// Three pages with content in layer A (switched off unless `on`) through marked content, a
/// form and a membership dictionary, and layer B (on) everywhere.
fn layered(on: bool) -> Vec<u8> {
    let mut b = PdfBuilder::new();
    let a = b.add(dictionary! { "Type" => "OCG", "Name" => lopdf::Object::string_literal("SECRET-pdf-layer-1") });
    let lb = b.add(
        dictionary! { "Type" => "OCG", "Name" => lopdf::Object::string_literal("Visible layer") },
    );
    let both = b
        .add(dictionary! { "Type" => "OCMD", "OCGs" => vec![a.into(), lb.into()], "P" => "AllOn" });
    let form = b.stream(
        dictionary! { "Type" => "XObject", "Subtype" => "Form", "BBox" => vec![0.into(), 0.into(), 100.into(), 100.into()], "OC" => a },
        b"0 0 10 10 re f",
    );
    let props = dictionary! { "Properties" => dictionary! { "a" => a, "b" => lb, "ab" => both } };
    let marked = |tag: &str, text: &str| format!("/OC /{tag} BDC\n{}EMC\n", text_at(72, 700, text));
    let mut with = props.clone();
    with.set("XObject", dictionary! { "Fm1" => form });
    b.page_with(
        &(marked("a", "Draft wording") + &marked("b", "Shown")),
        dictionary! { "Resources" => props.clone() },
    );
    b.page_with("/Fm1 Do\n", dictionary! { "Resources" => with });
    b.page_with(
        &marked("ab", "Both layers"),
        dictionary! { "Resources" => props },
    );
    let off: Vec<lopdf::Object> = if on { vec![] } else { vec![a.into()] };
    b.catalog = dictionary! {
        "OCProperties" => dictionary! {
            "OCGs" => vec![a.into(), lb.into()],
            "D" => dictionary! { "ON" => vec![lb.into()], "OFF" => off },
        },
    };
    b.bytes()
}

#[test]
fn content_in_layers_that_are_off_is_found_with_its_pages() {
    let f = check_pdf("layers.pdf", &layered(false), &[]);
    assert_eq!(
        pdf_kinds(&f, "hidden-layers"),
        [found(
            "hidden_layer",
            r#"{"page":1}"#,
            1,
            "1 layer that is switched off, with content on pages 1, 2 and 3"
        )]
    );
    let f = check_pdf("layers.pdf", &layered(false), &["--show-values"]);
    assert_eq!(f["findings"][0]["value"], "SECRET-pdf-layer-1");
    let f = check_pdf("layers.pdf", &layered(true), &[]);
    assert_eq!(pdf_kinds(&f, "hidden-layers"), []);
}

/// The kinds the rendered and layer checks found on each page, by page.
fn hidden_kinds(f: &serde_json::Value) -> Vec<(u64, String)> {
    let mut kinds: Vec<_> = f["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|x| {
            ["hidden-text", "fake-redactions", "hidden-layers"]
                .contains(&x["check"].as_str().unwrap())
        })
        .map(|x| {
            (
                x["where"]["page"].as_u64().unwrap(),
                x["kind"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    kinds.sort();
    kinds
}

#[test]
fn each_redaction_case_gives_exactly_its_kind() {
    let f = check_pdf("redacted.pdf", &redacted(), &[]);
    let expected: Vec<_> = REDACTED_CASES
        .iter()
        .enumerate()
        .filter(|(_, (_, kind))| !kind.is_empty())
        .map(|(i, (_, kind))| ((i + 1) as u64, kind.to_string()))
        .collect();
    assert_eq!(hidden_kinds(&f), expected, "{f}");
    let first = &f["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["kind"] == "text_under_box")
        .unwrap();
    assert_eq!(first["detail"], "17 characters under a black box");
    let bbox = first["where"]["bbox"].as_array().unwrap();
    assert!(
        bbox[0].as_f64().unwrap() >= 72.0 && bbox[3].as_f64().unwrap() <= 715.0,
        "{bbox:?}"
    );
    assert!(!f.to_string().contains("SECRET-"));
    let shown = check_pdf("redacted.pdf", &redacted(), &["--show-values"]);
    let values: Vec<_> = shown["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|x| x["value"].as_str())
        .collect();
    for page in [1, 6, 7, 8, 9, 10, 11, 12, 13] {
        assert!(
            values.contains(&format!("SECRET-pdf-case-{page}").as_str()),
            "{page}: {values:?}"
        );
    }
}

/// A scanned page: a full-page picture with its text drawn before it, visible (as ABBYY's
/// "text under image") or invisible (as OCRmyPDF's layer).
fn scanned(invisible: bool, picture: &[u8], size: (i64, i64)) -> Vec<u8> {
    let mut b = PdfBuilder::new();
    let mut z = flate2::write::ZlibEncoder::new(vec![], flate2::Compression::fast());
    std::io::Write::write_all(&mut z, picture).unwrap();
    let scan = b.stream(
        dictionary! {
            "Type" => "XObject", "Subtype" => "Image", "Width" => size.0, "Height" => size.1,
            "ColorSpace" => "DeviceRGB", "BitsPerComponent" => 8, "Filter" => "FlateDecode",
        },
        &z.finish().unwrap(),
    );
    let mode = if invisible { 3 } else { 0 };
    let text: String = (0..5)
        .map(|i| {
            format!(
                "BT {mode} Tr /F1 12 Tf 72 {} Td (Scanned line {i} of the letter) Tj ET\n",
                700 - 20 * i
            )
        })
        .collect();
    b.page_with(
        &format!("{text}q 595 0 0 842 0 0 cm /Scan Do Q\n"),
        dictionary! { "Resources" => dictionary! { "XObject" => dictionary! { "Scan" => scan } } },
    );
    b.bytes()
}

#[test]
fn visible_text_under_a_page_picture_is_hidden_text() {
    let f = check_pdf("abbyy_style.pdf", &scanned(false, &[200; 3], (1, 1)), &[]);
    let kinds = hidden_kinds(&f);
    assert_eq!(kinds, [(1, "text_under_page_image".to_string())], "{f}");
    // Five lines in a row under one picture make one group.
    assert_eq!(
        findings_of(&f, "hidden-text")[0]["detail"],
        "115 characters under a picture that covers the page"
    );
}

/// The page `scanned` draws, as an RGB picture: its text visible, drawn by the renderer.
fn scan_of_text() -> (Vec<u8>, (i64, i64)) {
    let text: String = (0..5)
        .map(|i| text_at(72, 700 - 20 * i, &format!("Scanned line {i} of the letter")))
        .collect();
    let pdf = hayro::hayro_syntax::Pdf::new(one_page(&text)).unwrap();
    let page = &pdf.pages()[0];
    let settings = hayro::PixmapSettings {
        x_scale: 1.0,
        y_scale: 1.0,
        bg_color: hayro::vello_cpu::color::palette::css::WHITE,
    };
    let drawn = hayro::render(
        page,
        &hayro::RenderCache::new(),
        &Default::default(),
        &Default::default(),
        &settings,
    );
    let rgb = drawn
        .data_as_u8_slice()
        .chunks(4)
        .flat_map(|p| [p[0], p[1], p[2]])
        .collect();
    (rgb, (i64::from(drawn.width()), i64::from(drawn.height())))
}

#[test]
fn an_ocr_layer_drawn_before_its_scan_is_not_hidden() {
    let (rgb, size) = scan_of_text();
    let f = check_pdf("ocr_scan.pdf", &scanned(true, &rgb, size), &[]);
    assert_eq!(hidden_kinds(&f), [], "{f}");
    assert_eq!(f["status"], "none_found", "{f}");
}

#[test]
fn pages_that_cant_be_drawn_leave_the_rendered_checks_incomplete() {
    let f = check_pdf("huge_image.pdf", &huge_image(), &[]);
    let note =
        "page 1: a picture on it is over 100 megapixels, so its text wasn't drawn and checked";
    for id in ["hidden-text", "fake-redactions"] {
        let check = f["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"] == id)
            .unwrap();
        assert_eq!(
            (check["status"].as_str(), check["note"].as_str()),
            (Some("incomplete"), Some(note)),
            "{f}"
        );
    }
    assert_eq!(f["status"], "incomplete");

    let dir = dir("unknown-filter");
    put(&dir, &[("unknown_filter.pdf", &unknown_filter())]);
    let out = run_in(&dir, &["unknown_filter.pdf"]);
    assert_eq!(out.status.code(), Some(1));
    let note = "page 1: its content couldn't be decoded, so its text wasn't checked";
    assert!(
        stdout(&out).starts_with(&format!(
            "unknown_filter.pdf  13 checks, 2 incomplete\n  ! hidden-text      {note}\n  ! fake-redactions  {note}\n"
        )),
        "{}",
        stdout(&out)
    );
}

#[test]
fn a_font_that_cant_be_read_leaves_the_pixel_test_out() {
    let mut objects = raw_page("/Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >>");
    objects.push((
        4,
        stream_body("", b"1 g BT /F1 12 Tf 72 700 Td (White words here) Tj ET"),
    ));
    objects.push((
        5,
        b"<< /Type /Font /Subtype /Type0 /BaseFont /Broken /Encoding /Identity-H >>".to_vec(),
    ));
    let f = check_pdf("bad_font.pdf", &raw(&objects, "/Root 1 0 R"), &[]);
    // hayro draws with a stand-in font, so flat pixels there prove nothing.
    assert_eq!(hidden_kinds(&f), [], "{f}");
    let note = "page 1: a font couldn't be read, so some text wasn't checked";
    assert_eq!(f["checks"][2]["note"], note, "{f}");
    assert_eq!(f["status"], "incomplete");
}

/// Prints how long the debug binary takes on book-300.pdf, and writes the file to
/// `DOC_SANITIZE_BOOK` when set, for timing release builds.
#[test]
#[ignore = "timing: run with --ignored --nocapture"]
fn book_300_timing() {
    let book = book(300, 10);
    if let Some(path) = std::env::var_os("DOC_SANITIZE_BOOK") {
        std::fs::write(path, &book).unwrap();
    }
    let dir = dir("book");
    put(&dir, &[("book-300.pdf", &book)]);
    let start = Instant::now();
    let out = run_in(&dir, &["book-300.pdf", "--json"]);
    println!("book-300.pdf check: {:.1} s", start.elapsed().as_secs_f64());
    let f = &json(&out)["files"][0];
    assert_eq!(hidden_kinds(f).len(), 10, "{f}");
    let start = Instant::now();
    let out = run_in(&dir, &["clean", "book-300.pdf", "--json"]);
    println!("book-300.pdf clean: {:.1} s", start.elapsed().as_secs_f64());
    let f = &json(&out)["files"][0];
    assert_eq!(f["status"], "cleaned", "{f}");
    let copy = std::fs::metadata(dir.join("book-300.clean.pdf"))
        .unwrap()
        .len();
    println!("{} bytes in, {copy} bytes out", book.len());
}

const WORDS: &str = "Some words to check";

/// A page stream inflating past 256 MiB.
fn flate_bomb() -> Vec<u8> {
    let mut objects = raw_page("/Contents 4 0 R");
    objects.push((
        4,
        stream_body("/Filter /FlateDecode", &zlib_zeros(260 << 20)),
    ));
    raw(&objects, "/Root 1 0 R")
}

/// 20,000 nested arrays in the catalog.
fn deep_array() -> Vec<u8> {
    let mut objects = raw_page("");
    let deep = format!("{}{}", "[".repeat(20_000), "]".repeat(20_000));
    objects[0].1 = format!("<< /Type /Catalog /Pages 2 0 R /Deep {deep} >>").into();
    raw(&objects, "/Root 1 0 R")
}

/// Text beside a picture declaring 100,000 × 100,000 pixels.
fn huge_image() -> Vec<u8> {
    let text = text_at(72, 700, WORDS);
    let huge = stream_body(
        "/Type /XObject /Subtype /Image /Width 100000 /Height 100000 /ColorSpace /DeviceGray /BitsPerComponent 8",
        b"\0",
    );
    let mut objects = raw_page(
        "/Contents 4 0 R /Resources << /Font << /F1 6 0 R >> /XObject << /Huge 5 0 R >> >>",
    );
    let content = format!("{text}q 10 0 0 10 300 300 cm /Huge Do Q");
    objects.push((4, stream_body("", content.as_bytes())));
    objects.push((5, huge));
    objects.push((
        6,
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
    ));
    raw(&objects, "/Root 1 0 R")
}

/// A page stream in a filter nobody knows.
fn unknown_filter() -> Vec<u8> {
    let mut objects = raw_page("/Contents 4 0 R");
    objects.push((
        4,
        stream_body("/Filter /FooDecode", text_at(72, 700, WORDS).as_bytes()),
    ));
    raw(&objects, "/Root 1 0 R")
}

/// Writes the PDF fixtures, and each readable one's cleaned copy as `clean-<name>`, into
/// `DOC_SANITIZE_OUT`, for measuring and for validators outside the tests.
#[test]
#[ignore = "writes files: run with --ignored and DOC_SANITIZE_OUT set"]
fn write_pdf_fixtures() {
    let out = std::path::PathBuf::from(std::env::var_os("DOC_SANITIZE_OUT").unwrap());
    std::fs::create_dir_all(&out).unwrap();
    for (name, bytes) in readable_pdfs() {
        let (_, copy) = clean_pdf(name, &bytes, &[]);
        std::fs::write(out.join(format!("clean-{name}")), copy).unwrap();
        std::fs::write(out.join(name), bytes).unwrap();
    }
    for (name, bytes) in [
        ("flate_bomb.pdf", flate_bomb()),
        ("deep_array.pdf", deep_array()),
    ] {
        std::fs::write(out.join(name), bytes).unwrap();
    }
}

/// Cleans `bytes` saved as `name`, with `args`: the file's JSON entry and the copy's bytes.
fn clean_pdf(name: &str, bytes: &[u8], args: &[&str]) -> (serde_json::Value, Vec<u8>) {
    let dir = dir("pdf-clean");
    put(&dir, &[(name, bytes)]);
    let out = run_in(&dir, &[&["clean", name, "--json"], args].concat());
    let f = json(&out)["files"][0].clone();
    let copy = f["output"]
        .as_str()
        .map_or(vec![], |o| std::fs::read(dir.join(o)).unwrap());
    (f, copy)
}

/// What every cleaned PDF is: one revision, no /Info unless `info`, no /Encrypt, a new /ID.
fn assert_rewritten(copy: &[u8], input: &[u8], info: bool) -> lopdf::Document {
    assert_eq!(copy.windows(9).filter(|w| w == b"startxref").count(), 1);
    assert_eq!(copy.windows(5).filter(|w| w == b"%%EOF").count(), 1);
    let doc = lopdf::Document::load_mem(copy).unwrap();
    assert_eq!(doc.trailer.has(b"Info"), info);
    assert!(!doc.trailer.has(b"Encrypt") && !doc.trailer.has(b"Prev"));
    let id = doc.trailer.get(b"ID").unwrap().as_array().unwrap();
    assert_eq!(id.len(), 2);
    let was = lopdf::Document::load_mem(input).ok();
    assert_ne!(
        Some(id),
        was.as_ref()
            .and_then(|d| d.trailer.get(b"ID").ok()?.as_array().ok())
    );
    doc
}

/// The planted secrets in a PDF's bytes and in every stream decoded.
fn pdf_secrets(copy: &[u8]) -> Vec<String> {
    let mut found = secrets_in(copy);
    let doc = lopdf::Document::load_mem(copy).unwrap();
    for o in doc.objects.values() {
        if let lopdf::Object::Stream(s) = o
            && let Ok(plain) = s.decompressed_content()
        {
            found.extend(secrets_in(&plain));
        }
    }
    found
}

/// The ids of the checks that found something.
fn found_ids(f: &serde_json::Value) -> Vec<String> {
    f["checks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["status"] == "found")
        .map(|c| c["id"].as_str().unwrap().into())
        .collect()
}

#[test]
fn structure_leaks_go_and_what_shows_stays() {
    let input = leaky_structure();
    let (f, copy) = clean_pdf("leaky.pdf", &input, &[]);
    assert_eq!(f["status"], "cleaned", "{f}");
    assert_eq!(f["recheck"]["status"], "none_found", "{f}");
    let codes: Vec<_> = f["changes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| &c["code"])
        .collect();
    assert_eq!(codes, ["signatures_removed"]);
    let doc = assert_rewritten(&copy, &input, false);
    assert_eq!(pdf_secrets(&copy), Vec::<String>::new());
    let page = doc.page_iter().next().unwrap();
    let text = doc.extract_text(&[1]).unwrap();
    assert!(text.contains("Visible text"), "{text}");
    // The web link stays, with the widget of the field whose value went.
    let dict = doc.get_dictionary(page).unwrap();
    let annots = dict.get(b"Annots").unwrap().as_array().unwrap();
    let links = annots
        .iter()
        .filter_map(|a| {
            a.as_reference()
                .ok()
                .map_or(a.as_dict().ok(), |id| doc.get_dictionary(id).ok())
        })
        .filter(|a| a.get(b"Subtype").and_then(lopdf::Object::as_name).ok() == Some(b"Link"))
        .count();
    assert_eq!(links, 1, "{annots:?}");
    assert!(!dict.has(b"PieceInfo") && !dict.has(b"Thumb"));
    let catalog = doc.catalog().unwrap();
    for key in [
        b"OpenAction".as_slice(),
        b"Perms",
        b"AF",
        b"Collection",
        b"Metadata",
    ] {
        assert!(!catalog.has(key));
    }
}

#[test]
fn the_jpeg_in_a_pdf_keeps_its_scan_and_length() {
    let mut b = PdfBuilder::new();
    let image = b.stream(
        dictionary! {
            "Type" => "XObject", "Subtype" => "Image", "Width" => 1, "Height" => 1,
            "ColorSpace" => "DeviceGray", "BitsPerComponent" => 8, "Filter" => "DCTDecode",
        },
        &jpeg(&[0x010F, 0x8825], b""),
    );
    b.page_with(
        "q 10 0 0 10 50 50 cm /Im1 Do Q
",
        dictionary! { "Resources" => dictionary! { "XObject" => dictionary! { "Im1" => image } } },
    );
    let input = b.bytes();
    let (f, copy) = clean_pdf("photo.pdf", &input, &["--keep", "image-metadata"]);
    assert_eq!(f["kept"], serde_json::json!(["image-metadata"]), "{f}");
    assert!(copy.windows(4).any(|w| w == b"Exif"));
    let (f, copy) = clean_pdf("photo.pdf", &input, &[]);
    assert_eq!(f["status"], "cleaned", "{f}");
    let doc = lopdf::Document::load_mem(&copy).unwrap();
    let image = doc
        .objects
        .values()
        .find_map(|o| o.as_stream().ok().filter(|s| s.dict.has_type(b"XObject")))
        .unwrap();
    assert_eq!(
        image.dict.get(b"Length").unwrap().as_i64().unwrap() as usize,
        image.content.len()
    );
    assert_eq!(
        doc_sanitize::pictures::metadata(&image.content),
        Some(vec![])
    );
    assert!(
        image
            .content
            .ends_with(&[0xFF, 0xDA, 0, 2, 0x12, 0x34, 0xFF, 0xD9])
    );
}

#[test]
fn every_kept_pdf_check_survives() {
    let input = leaky_structure();
    let ids = found_ids(&check_pdf("leaky.pdf", &input, &[]));
    assert!(ids.len() >= 8, "{ids:?}");
    // The fixture's picture is drawn nowhere, so it goes as a leftover whatever is kept.
    let not_keepable = ["signatures", "old-revisions", "image-metadata"];
    for id in ids.iter().filter(|i| !not_keepable.contains(&i.as_str())) {
        let (f, copy) = clean_pdf("leaky.pdf", &input, &["--keep", id]);
        assert_eq!(f["status"], "cleaned", "{id}: {f}");
        assert_eq!(f["kept"], serde_json::json!([id]), "{id}");
        let left = found_ids(&check_pdf("copy.pdf", &copy, &[]));
        assert_eq!(left, [id.as_str()], "{id}");
    }
}

#[test]
fn old_revisions_and_outside_bytes_never_survive() {
    let input = incremental();
    let (f, copy) = clean_pdf("incremental.pdf", &input, &[]);
    assert_eq!(f["status"], "cleaned", "{f}");
    assert_rewritten(&copy, &input, false);
    assert_eq!(pdf_secrets(&copy), Vec::<String>::new());

    let (input, _) = polyglot();
    let (f, copy) = clean_pdf("polyglot.pdf", &input, &[]);
    assert_eq!(f["status"], "cleaned", "{f}");
    assert!(copy.ends_with(b"%%EOF"));
    assert_rewritten(&copy, &input, false);

    let input = unlisted();
    let (f, copy) = clean_pdf("unlisted.pdf", &input, &[]);
    assert_eq!(f["status"], "cleaned", "{f}");
    assert_eq!(pdf_secrets(&copy), Vec::<String>::new());
    let doc = assert_rewritten(&copy, &input, false);
    let image = |o: &lopdf::Object| o.as_stream().is_ok_and(|s| s.dict.has_type(b"XObject"));
    assert!(!doc.objects.values().any(image));

    // Kept leftovers: what nothing used stays; the unlisted body can't survive a rewrite.
    let (f, copy) = clean_pdf("unlisted.pdf", &input, &["--keep", "leftover-data"]);
    assert_eq!(f["status"], "cleaned", "{f}");
    assert_eq!(pdf_secrets(&copy), ["SECRET-pdf-unused-1"]);
    assert_eq!(f["kept"], serde_json::json!(["leftover-data"]));
}

#[test]
fn encrypted_files_lose_their_password_with_a_warning() {
    let input = aes256("owner-secret", "");
    let (f, copy) = clean_pdf("owner.pdf", &input, &[]);
    assert_eq!(f["status"], "cleaned", "{f}");
    let w = &f["warnings"][0];
    assert_eq!(w["code"], "encryption_removed");
    assert_eq!(
        w["message"],
        "The copy has no password or permission settings; the original had them."
    );
    assert_rewritten(&copy, &input, false);
    let input = aes256("o", "open-sesame");
    let (f, copy) = clean_pdf("user.pdf", &input, &["--password", "open-sesame"]);
    assert_eq!(f["status"], "cleaned", "{f}");
    assert_eq!(f["warnings"][0]["code"], "encryption_removed");
    let doc = lopdf::Document::load_mem(&copy).unwrap();
    assert!(doc.extract_text(&[1]).unwrap().contains("Locked away"));
}

/// A PDF/A file (with PDF/UA when `ua`) whose XMP also names its author.
fn pdfa(ua: bool) -> Vec<u8> {
    let mut b = PdfBuilder::new();
    b.page(&text_at(72, 720, "Archived"));
    let ua = if ua { " pdfuaid:part=\"1\"" } else { "" };
    let xmp = format!(
        "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"><rdf:Description xmlns:dc=\"http://purl.org/dc/elements/1.1/\" xmlns:pdfaid=\"http://www.aiim.org/pdfa/ns/id/\" xmlns:pdfuaid=\"http://www.aiim.org/pdfua/ns/id/\" pdfaid:part=\"2\" pdfaid:conformance=\"B\"{ua}><dc:creator><rdf:Seq><rdf:li>SECRET-pdf-pdfa-1</rdf:li></rdf:Seq></dc:creator></rdf:Description></rdf:RDF></x:xmpmeta>"
    );
    let meta = b.stream(
        dictionary! { "Type" => "Metadata", "Subtype" => "XML" },
        xmp.as_bytes(),
    );
    b.catalog = dictionary! { "Metadata" => meta };
    b.bytes()
}

#[test]
fn a_pdfa_file_keeps_only_its_identification() {
    let (f, copy) = clean_pdf("a.pdf", &pdfa(false), &[]);
    assert_eq!(f["status"], "cleaned", "{f}");
    assert_eq!(f["warnings"], serde_json::json!([]));
    assert_eq!(pdf_secrets(&copy), Vec::<String>::new());
    let doc = lopdf::Document::load_mem(&copy).unwrap();
    let meta = doc.catalog().unwrap().get(b"Metadata").unwrap();
    let stream = doc
        .get_object(meta.as_reference().unwrap())
        .unwrap()
        .as_stream()
        .unwrap();
    assert!(!stream.dict.has(b"Filter"));
    let xmp = String::from_utf8(stream.content.clone()).unwrap();
    assert!(
        xmp.contains("pdfaid:part=\"2\"") && xmp.contains("pdfaid:conformance=\"B\""),
        "{xmp}"
    );
    let (f, _) = clean_pdf("ua.pdf", &pdfa(true), &[]);
    assert_eq!(f["warnings"][0]["code"], "pdfa_title_removed");
}

#[test]
fn a_dynamic_xfa_form_leaves_no_file() {
    let mut b = PdfBuilder::new();
    b.page(&text_at(72, 720, "Please wait"));
    b.catalog = dictionary! {
        "NeedsRendering" => true,
        "AcroForm" => dictionary! {
            "Fields" => Vec::<lopdf::Object>::new(),
            "XFA" => lopdf::Object::string_literal("<xdp/>"),
        },
    };
    let input = b.bytes();
    let (f, _) = clean_pdf("xfa.pdf", &input, &[]);
    assert_eq!(f["status"], "not_cleaned", "{f}");
    assert_eq!(f["output"], serde_json::Value::Null);
    assert_eq!(f["error"]["code"], "cannot_clean");
    assert_eq!(
        f["error"]["message"],
        "Can't remove form-data from xfa.pdf: it is a dynamic XFA form."
    );
    let (f, _) = clean_pdf("xfa.pdf", &input, &["--keep", "form-data"]);
    assert_eq!(f["status"], "cleaned", "{f}");
}

#[test]
fn a_comment_over_text_a_kept_check_found_stays() {
    let square = dictionary! {
        "Type" => "Annot", "Subtype" => "Square",
        "Rect" => vec![60.into(), 700.into(), 300.into(), 740.into()],
        "T" => lopdf::Object::string_literal("Sam Sample"),
    };
    let elsewhere = dictionary! {
        "Type" => "Annot", "Subtype" => "Text",
        "Rect" => vec![400.into(), 100.into(), 420.into(), 120.into()],
        "Contents" => lopdf::Object::string_literal("SECRET-pdf-note-1"),
    };
    let mut b = PdfBuilder::new();
    b.page_with(
        &format!(
            "{}0 g 60 700 240 40 re f\n",
            text_at(72, 720, "Under the box")
        ),
        dictionary! { "Annots" => vec![square.into(), elsewhere.into()] },
    );
    let input = b.bytes();
    let (f, copy) = clean_pdf("a.pdf", &input, &["--keep", "fake-redactions"]);
    assert_eq!(f["status"], "cleaned", "{f}");
    assert_eq!(
        f["kept"],
        serde_json::json!(["comments", "fake-redactions"]),
        "{f}"
    );
    assert_eq!(pdf_secrets(&copy), Vec::<String>::new());
    let doc = lopdf::Document::load_mem(&copy).unwrap();
    let page = doc.page_iter().next().unwrap();
    let annots = doc.get_dictionary(page).unwrap().get(b"Annots").unwrap();
    assert_eq!(annots.as_array().unwrap().len(), 1);
}

/// A page of `bytes` drawn at 200 DPI on white: its size and RGB pixels.
fn drawn(bytes: &[u8], number: usize) -> (usize, usize, Vec<u8>) {
    use hayro::hayro_interpret::InterpreterSettings;
    use hayro::{PixmapSettings, RenderCache, RenderSettings};
    let pdf = hayro::hayro_syntax::Pdf::new(bytes.to_vec()).unwrap();
    let pages = pdf.pages();
    let scale = 200.0 / 72.0;
    let pixmap = hayro::render(
        &pages[number - 1],
        &RenderCache::new(),
        &InterpreterSettings::default(),
        &RenderSettings::default(),
        &PixmapSettings {
            x_scale: scale,
            y_scale: scale,
            bg_color: hayro::vello_cpu::color::palette::css::WHITE,
        },
    );
    let rgb = pixmap.data().iter().flat_map(|p| [p.r, p.g, p.b]).collect();
    (pixmap.width().into(), pixmap.height().into(), rgb)
}

/// The share of pixels that differ by more than anti-aliasing can between two drawings of
/// one page, `skip` being a box of pixels left out.
fn changed(a: &(usize, usize, Vec<u8>), b: &(usize, usize, Vec<u8>), skip: [usize; 4]) -> f64 {
    assert_eq!((a.0, a.1), (b.0, b.1));
    let mut different = 0;
    for y in 0..a.1 {
        for x in 0..a.0 {
            let inside = (skip[0]..skip[2]).contains(&x) && (skip[1]..skip[3]).contains(&y);
            let i = (y * a.0 + x) * 3;
            let near = (0..3).all(|k| a.2[i + k].abs_diff(b.2[i + k]) < 64);
            different += usize::from(!inside && !near);
        }
    }
    different as f64 / (a.0 * a.1) as f64
}

#[test]
fn hidden_text_turns_its_pages_into_pictures() {
    let input = redacted();
    let (f, copy) = clean_pdf("redacted.pdf", &input, &[]);
    assert_eq!(f["status"], "cleaned", "{f}");
    assert_eq!(f["recheck"]["status"], "none_found", "{f}");
    let change = |code: &str| {
        f["changes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["code"] == code)
            .cloned()
            .unwrap_or_else(|| panic!("no {code}: {f}"))
    };
    let pages = change("pages_rasterised");
    assert_eq!(
        pages["pages"],
        serde_json::json!((1..=14).collect::<Vec<_>>())
    );
    assert_eq!(
        pages["detail"],
        "Pages 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13 and 14 are now pictures; their text can't be selected or searched."
    );
    assert_eq!(change("redactions_applied")["count"], 1);
    assert_eq!(
        change("redactions_applied")["detail"],
        "1 redaction mark was applied: its area is now filled in black."
    );
    assert_eq!(change("annotations_flattened")["count"], 1);
    let doc = assert_rewritten(&copy, &input, false);
    assert_eq!(pdf_secrets(&copy), Vec::<String>::new());
    assert!(!doc.catalog().unwrap().has(b"OCProperties"));
    for (i, id) in doc.page_iter().enumerate() {
        let page = doc.get_dictionary(id).unwrap();
        let content = doc.get_page_content(id);
        let text = String::from_utf8_lossy(&content);
        assert_eq!(text.contains("Tj"), i == 14, "page {}: {text}", i + 1);
        assert!(!page.has(b"Annots"), "page {}", i + 1);
        if i < 14 {
            let bounds = page.get(b"MediaBox").unwrap();
            assert_eq!(page.get(b"CropBox").unwrap(), bounds);
            assert!(text.starts_with("q 595 0 0 842 0 0 cm /Im0 Do Q"), "{text}");
        }
    }
    // Kept leftovers are what nothing used in the input, never what clean took out.
    let (f, kept) = clean_pdf("redacted.pdf", &input, &["--keep", "leftover-data"]);
    assert_eq!(f["status"], "cleaned", "{f}");
    assert_eq!(pdf_secrets(&kept), Vec::<String>::new());
    // Every page looks as it did, but for the mark now painted over its area.
    let [x0, y0, x1, y1] = [70.0, 842.0 - 715.0, 300.0, 842.0 - 695.0].map(|v| v * 200.0 / 72.0);
    let mark = [x0 as usize, y0 as usize, x1 as usize + 1, y1 as usize + 1];
    for page in 1..=15 {
        let (a, b) = (drawn(&input, page), drawn(&copy, page));
        let skip = if page == 13 { mark } else { [0; 4] };
        let share = changed(&a, &b, skip);
        assert!(share < 0.001, "page {page}: {share}");
        if page == 13 {
            let middle = (mark[1] + 20) * b.0 + (mark[0] + mark[2]) / 2;
            assert_eq!(&b.2[middle * 3..middle * 3 + 3], [0, 0, 0]);
        }
    }
}

#[test]
fn layers_that_are_off_leave_with_their_pages() {
    let input = layered(false);
    let (f, copy) = clean_pdf("layers.pdf", &input, &[]);
    assert_eq!(f["status"], "cleaned", "{f}");
    let changes = f["changes"].as_array().unwrap();
    assert_eq!(changes[0]["pages"], serde_json::json!([1, 2, 3]), "{f}");
    let doc = assert_rewritten(&copy, &input, false);
    assert!(!doc.catalog().unwrap().has(b"OCProperties"));
    assert_eq!(pdf_secrets(&copy), Vec::<String>::new());
    let raw = doc.get_page_content(doc.page_iter().next().unwrap());
    assert!(!String::from_utf8_lossy(&raw).contains("BDC"));
    // Kept, nothing changes on the pages.
    let (f, copy) = clean_pdf("layers.pdf", &input, &["--keep", "hidden-layers"]);
    assert_eq!(f["kept"], serde_json::json!(["hidden-layers"]));
    assert_eq!(f["changes"], serde_json::json!([]));
    assert!(
        lopdf::Document::load_mem(&copy)
            .unwrap()
            .catalog()
            .unwrap()
            .has(b"OCProperties")
    );
}

#[test]
fn only_hidden_text_turns_a_scan_into_a_picture() {
    let (rgb, size) = scan_of_text();
    let input = scanned(true, &rgb, size);
    let (f, copy) = clean_pdf("ocr_scan.pdf", &input, &[]);
    assert_eq!(f["status"], "cleaned", "{f}");
    assert_eq!(f["changes"], serde_json::json!([]));
    let doc = lopdf::Document::load_mem(&copy).unwrap();
    assert!(doc.extract_text(&[1]).unwrap().contains("Scanned line"));
    let input = scanned(false, &[200; 3], (1, 1));
    let (f, copy) = clean_pdf("abbyy_style.pdf", &input, &[]);
    assert_eq!(f["status"], "cleaned", "{f}");
    assert_eq!(
        f["changes"][0]["detail"],
        "Page 1 is now a picture; its text can't be selected or searched."
    );
    let doc = lopdf::Document::load_mem(&copy).unwrap();
    let content = doc.get_page_content(doc.page_iter().next().unwrap());
    assert!(!String::from_utf8_lossy(&content).contains("Tj"));
}

#[test]
fn widgets_links_and_rotation_on_a_page_turned_into_a_picture() {
    let mut b = PdfBuilder::new();
    let rect = vec![100.into(), 500.into(), 300.into(), 520.into()];
    let appearance = b.stream(
        dictionary! { "Type" => "XObject", "Subtype" => "Form", "BBox" => vec![0.into(), 0.into(), 200.into(), 20.into()] },
        b"0.8 g 0 0 200 20 re f",
    );
    let widget = b.add(dictionary! {
        "Type" => "Annot", "Subtype" => "Widget", "Rect" => rect.clone(), "FT" => "Tx",
        "T" => lopdf::Object::string_literal("Name"), "V" => lopdf::Object::string_literal("Shown"),
        "AP" => dictionary! { "N" => appearance },
    });
    let other = b.add(dictionary! {
        "Type" => "Annot", "Subtype" => "Widget", "Rect" => rect.clone(), "FT" => "Tx",
        "T" => lopdf::Object::string_literal("Other"),
    });
    let link = b.add(dictionary! {
        "Type" => "Annot", "Subtype" => "Link", "Rect" => rect.clone(),
        "A" => dictionary! { "S" => "URI", "URI" => lopdf::Object::string_literal("https://example.com/") },
    });
    let hidden = format!("1 g {}0 g\n", text_at(72, 700, "Hidden words here"));
    b.page_with(
        &hidden,
        dictionary! { "Annots" => vec![widget.into(), link.into()] },
    );
    b.page_with(
        &hidden,
        dictionary! { "Annots" => vec![link.into()], "Rotate" => 90 },
    );
    b.page_with(
        &text_at(72, 700, "Plain"),
        dictionary! { "Annots" => vec![other.into()] },
    );
    b.catalog = dictionary! {
        "AcroForm" => dictionary! { "Fields" => vec![widget.into(), other.into()] },
    };
    let input = b.bytes();
    let (f, copy) = clean_pdf("form.pdf", &input, &[]);
    assert_eq!(f["status"], "cleaned", "{f}");
    let doc = assert_rewritten(&copy, &input, false);
    let pages: Vec<_> = doc.page_iter().collect();
    let page = |i: usize| doc.get_dictionary(pages[i]).unwrap();
    let annots = |i: usize| page(i).get(b"Annots").unwrap().as_array().unwrap().len();
    assert_eq!(annots(0), 1, "the link stays, the widget is in the picture");
    assert!(!page(1).has(b"Annots"), "a link can't follow a turned page");
    assert!(!page(1).has(b"Rotate"));
    let size: Vec<f32> = page(1)
        .get(b"MediaBox")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n.as_float().unwrap())
        .collect();
    assert_eq!(size, [0.0, 0.0, 842.0, 595.0]);
    let form = doc
        .catalog()
        .unwrap()
        .get(b"AcroForm")
        .unwrap()
        .as_dict()
        .unwrap();
    assert_eq!(form.get(b"Fields").unwrap().as_array().unwrap().len(), 1);
    assert_eq!(
        f["changes"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|c| c["code"] == "annotations_flattened")
            .count(),
        1
    );
}

#[test]
fn a_page_too_big_to_draw_leaves_no_file() {
    let mut b = PdfBuilder::new();
    let hidden = format!("1 g {}0 g\n", text_at(72, 700, "Hidden words here"));
    let page = b.page(&hidden);
    let mut doc = b.finish();
    let box_: Vec<lopdf::Object> = [0, 0, 14_400, 14_400].map(lopdf::Object::from).to_vec();
    doc.get_dictionary_mut(page).unwrap().set("MediaBox", box_);
    let (f, _) = clean_pdf("huge.pdf", &save(&mut doc), &[]);
    assert_eq!(f["status"], "not_cleaned", "{f}");
    assert_eq!(
        f["error"]["message"],
        "Can't remove hidden-text from huge.pdf: page 1 can't be drawn as a picture of at least 72 DPI within 40 megapixels."
    );
    assert_eq!(
        f["error"]["hint"],
        "Run clean again with --keep hidden-text."
    );
}

/// The PDFs of the hostile set (with the leak fixtures) that can be read.
fn readable_pdfs() -> Vec<(&'static str, Vec<u8>)> {
    let (rgb, size) = scan_of_text();
    vec![
        ("redacted.pdf", redacted()),
        ("leaky.pdf", leaky_structure()),
        ("plain.pdf", plain_structure()),
        ("incremental.pdf", incremental()),
        ("layers.pdf", layered(false)),
        ("abbyy_style.pdf", scanned(false, &[200; 3], (1, 1))),
        ("ocr_scan.pdf", scanned(true, &rgb, size)),
        ("huge_image.pdf", huge_image()),
        ("unknown_filter.pdf", unknown_filter()),
        ("prev_loop.pdf", prev_loop()),
        ("hybrid_xref.pdf", hybrid_xref()),
        ("short_length.pdf", bad_length(-100)),
        ("long_length.pdf", bad_length(100)),
        ("owner_only.pdf", aes256("owner-secret", "")),
        (
            "linearized.pdf",
            include_bytes!("fixtures/linearized.pdf").to_vec(),
        ),
        ("polyglot.pdf", polyglot().0),
        ("unlisted.pdf", unlisted()),
        ("pdfa.pdf", pdfa(false)),
    ]
}

#[test]
fn every_readable_hostile_pdf_cleans() {
    for (name, input) in readable_pdfs() {
        let (f, copy) = clean_pdf(name, &input, &[]);
        let incomplete = ["huge_image.pdf", "unknown_filter.pdf"].contains(&name);
        let status = if incomplete {
            "cleaned_incomplete"
        } else {
            "cleaned"
        };
        assert_eq!(f["status"], status, "{name}: {f}");
        assert_rewritten(&copy, &input, false);
    }
}
