//! Office packages: every variant recognised by its main part's content type, and the
//! variants this version refuses named.

mod common;

use common::*;

const VARIANTS: [&str; 14] = [
    "docx", "docm", "dotx", "dotm", "xlsx", "xlsm", "xltx", "xltm", "pptx", "pptm", "potx", "potm",
    "ppsx", "ppsm",
];

fn check_bytes(name: &str, bytes: &[u8]) -> serde_json::Value {
    let dir = dir("office");
    put(&dir, &[(name, bytes)]);
    json(&run_in(&dir, &[name, "--json"]))["files"][0].clone()
}

#[test]
fn every_variant_is_recognised_by_its_main_part() {
    for variant in VARIANTS {
        let bytes = zip(&package(variant, &[], &[]));
        let f = check_bytes(&format!("file.{variant}"), &bytes);
        let format = match &variant[..1] {
            "d" => "docx",
            "x" => "xlsx",
            _ => "pptx",
        };
        assert_eq!(f["format"], format, "{variant}: {f}");
        assert_eq!(f["variant"], variant);
        assert_eq!(f["warnings"], serde_json::json!([]), "{variant}");
        assert_eq!(f["error"], serde_json::Value::Null);
    }
}

#[test]
fn a_renamed_file_keeps_its_real_variant_with_a_warning() {
    let f = check_bytes("budget.docx", &zip(&package("xlsm", &[], &[])));
    assert_eq!(
        (f["format"].as_str(), f["variant"].as_str()),
        (Some("xlsx"), Some("xlsm"))
    );
    assert_eq!(f["warnings"][0]["code"], "extension_mismatch");
    assert_eq!(
        f["warnings"][0]["message"],
        "budget.docx is a .xlsm file with a .docx name."
    );
    let f = check_bytes("fake.docx", PDF);
    assert_eq!(f["format"], "pdf");
    assert_eq!(
        f["warnings"][0]["message"],
        "fake.docx is a .pdf file with a .docx name."
    );
}

#[test]
fn pdfs_of_every_version_are_recognised() {
    for head in [
        "%PDF-1.0\n",
        "%PDF-1.4\n",
        "%PDF-1.7\n",
        "%PDF-2.0\n",
        "junk before\r\n%PDF-1.5\n",
    ] {
        let f = check_bytes("doc.pdf", format!("{head}%%EOF\n").as_bytes());
        assert_eq!(
            (f["format"].as_str(), f["variant"].as_str()),
            (Some("pdf"), Some("pdf")),
            "{head}"
        );
    }
}

fn refused(name: &str, bytes: &[u8], code: &str, message: &str, hint: &str) {
    let f = check_bytes(name, bytes);
    assert_eq!(f["status"], "error", "{name}: {f}");
    assert_eq!(f["error"]["code"], code, "{name}");
    assert_eq!(f["error"]["message"], message, "{name}");
    assert_eq!(f["error"]["hint"], hint, "{name}");
}

const NOT_OURS: &str =
    "doc-sanitize reads .pdf, .docx, .xlsx and .pptx files, and their macro and template versions.";

#[test]
fn refused_variants_say_what_to_do() {
    let strict = package("docx", &[], &[]);
    let strict = with(
        strict,
        "_rels/.rels",
        format!("<Relationships xmlns=\"{REL}\"><Relationship Id=\"rId1\" Type=\"http://purl.oclc.org/ooxml/officeDocument/relationships/officeDocument\" Target=\"word/document.xml\"/></Relationships>").as_bytes(),
    );
    refused(
        "strict.docx",
        &zip(&strict),
        "unsupported_variant",
        "strict.docx is saved as Strict Open XML, which this version can't read.",
        "Save it in Office as a normal .docx, .xlsx or .pptx.",
    );
    refused(
        "book.xlsb",
        &zip(&package("xlsb", &[], &[])),
        "unsupported_variant",
        "book.xlsb is a binary Excel workbook (.xlsb).",
        "Save it as .xlsx in Excel.",
    );

    let odt = zip(&[
        entry("mimetype", b"application/vnd.oasis.opendocument.text"),
        entry("content.xml", b"<office:document-content/>"),
    ]);
    refused(
        "notes.odt",
        &odt,
        "unsupported_format",
        "notes.odt isn't a PDF, Word, Excel or PowerPoint file.",
        NOT_OURS,
    );
    let plain = zip(&[entry("readme.txt", b"hello")]);
    refused(
        "plain.zip",
        &plain,
        "unsupported_format",
        "plain.zip isn't a PDF, Word, Excel or PowerPoint file.",
        NOT_OURS,
    );
    let visio = with(package("docx", &[], &[]), "[Content_Types].xml", b"<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"><Default Extension=\"xml\" ContentType=\"application/vnd.ms-visio.drawing.main+xml\"/></Types>");
    refused(
        "drawing.vsdx",
        &zip(&visio),
        "unsupported_format",
        "drawing.vsdx isn't a PDF, Word, Excel or PowerPoint file.",
        NOT_OURS,
    );
}

#[test]
fn legacy_and_password_protected_office_files_are_named() {
    refused(
        "old.doc",
        &cfb("WordDocument"),
        "legacy_office",
        "old.doc is an older Office file (.doc, .xls or .ppt).",
        "Save it as .docx, .xlsx or .pptx in Office, then check the new file.",
    );
    refused(
        "cfb_encrypted.docx",
        &cfb("EncryptedPackage"),
        "password_required",
        "cfb_encrypted.docx is password-protected.",
        "Remove the password in Office (File › Info › Protect), save, and check again.",
    );
}

#[test]
fn a_missing_main_part_is_damage() {
    let entries: Vec<Entry> = package("docx", &[], &[])
        .into_iter()
        .filter(|e| e.name != "word/document.xml")
        .collect();
    refused(
        "gone.docx",
        &zip(&entries),
        "damaged",
        "gone.docx is damaged: part word/document.xml is missing.",
        "Open it in the app that made it and save a new copy.",
    );
}

#[test]
fn a_part_that_isnt_xml_is_damage() {
    let bad = with(
        package("pptx", &[], &[]),
        "ppt/presentation.xml",
        b"<p:presentation",
    );
    refused(
        "bad.pptx",
        &zip(&bad),
        "damaged",
        "bad.pptx is damaged: part ppt/presentation.xml isn't valid XML.",
        "Open it in the app that made it and save a new copy.",
    );
}

#[test]
fn utf16_parts_are_read() {
    let (_, _, body) = main_part("docx");
    let text = format!("<?xml version=\"1.0\" encoding=\"UTF-16\" standalone=\"yes\"?>\r\n{body}");
    let utf16: Vec<u8> = [0xFF, 0xFE]
        .into_iter()
        .chain(text.encode_utf16().flat_map(u16::to_le_bytes))
        .collect();
    let f = check_bytes(
        "utf16.docx",
        &zip(&with(
            package("docx", &[], &[]),
            "word/document.xml",
            &utf16,
        )),
    );
    assert_eq!(f["error"], serde_json::Value::Null, "{f}");
    assert_eq!(f["variant"], "docx");
}

#[test]
fn text_names_the_file_and_what_ran() {
    let dir = dir("office-text");
    put(&dir, &[("deck.pptx", &zip(&package("pptx", &[], &[])))]);
    let out = run_in(&dir, &["deck.pptx"]);
    assert_eq!(stdout(&out), "deck.pptx  nothing found by 0 checks\n");
}
