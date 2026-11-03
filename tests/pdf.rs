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
        [k(
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

#[test]
fn leftovers_are_found_in_the_bytes_and_the_objects() {
    let mut polyglot = one_page(&text_at(72, 720, "Front"));
    let zip = zip(&package("docx", &[], &[]));
    polyglot.extend(&zip);
    polyglot.extend(b"\n\0\n");
    let f = check_pdf("polyglot.pdf", &polyglot, &[]);
    // The ZIP ends in zeros (an empty comment length), which aren't counted.
    let size = zip.iter().rposition(|&b| b != 0).unwrap() + 1;
    let after = format!("{} bytes after the PDF ends", thousands(size as u64));
    assert_eq!(
        pdf_kinds(&f, "leftover-data"),
        [k("data_outside_pdf", "{}", size as u64, &after)]
    );
    let mut before = b"%!PS junk\n".to_vec();
    before.extend(one_page(""));
    let f = check_pdf("before.pdf", &before, &[]);
    assert_eq!(
        pdf_kinds(&f, "leftover-data"),
        [k(
            "data_outside_pdf",
            "{}",
            10,
            "10 bytes before the PDF starts"
        )]
    );

    // An object body in the bytes that no cross-reference table lists, an object nothing
    // uses, and an image the page never draws.
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
    let f = check_pdf("unlisted.pdf", &out, &[]);
    assert_eq!(
        pdf_kinds(&f, "leftover-data"),
        [
            k(
                "unused_objects",
                "{}",
                1,
                "1 object that nothing in the file uses"
            ),
            k(
                "unlisted_objects",
                "{}",
                1,
                "1 object body that no cross-reference table lists"
            ),
            k(
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
        [k(
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
