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
    assert_eq!(
        stdout(&out),
        "deck.pptx  nothing found by 1 check
  ✓ 1 check found nothing: leftover-data
"
    );
    assert_eq!(out.status.code(), Some(0));
}

/// The `leftover-data` findings of a package, as (kind, part, count, detail).
fn leftovers(name: &str, bytes: &[u8]) -> Vec<(String, String, u64, String)> {
    let f = check_bytes(name, bytes);
    assert_eq!(
        f["checks"],
        serde_json::json!([{ "id": "leftover-data", "status": if f["findings"].as_array().unwrap().is_empty() { "none_found" } else { "found" }, "count": f["findings"].as_array().unwrap().len() }]),
        "{f}"
    );
    let s = |v: &serde_json::Value| v.as_str().unwrap_or_default().to_string();
    f["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| {
            assert_eq!(x["check"], "leftover-data");
            (
                s(&x["kind"]),
                s(&x["where"]["part"]),
                x["count"].as_u64().unwrap(),
                s(&x["detail"]),
            )
        })
        .collect()
}

fn found(kind: &str, part: &str, count: u64, detail: &str) -> (String, String, u64, String) {
    (kind.into(), part.into(), count, detail.into())
}

#[test]
fn a_clean_package_has_no_leftovers() {
    for variant in ["docx", "xlsx", "pptx"] {
        assert_eq!(
            leftovers(
                &format!("twin.{variant}"),
                &zip(&package(variant, &[], &[]))
            ),
            []
        );
    }
}

#[test]
fn bytes_outside_the_package_are_found() {
    let docx = zip(&package("docx", &[], &[]));
    let prepended = [vec![b'x'; 4096], docx.clone()].concat();
    assert_eq!(
        leftovers("prepended.docx", &prepended),
        [found(
            "data_outside_package",
            "",
            4096,
            "4,096 bytes before the package"
        )]
    );
    let appended = [
        zip(&package("xlsx", &[], &[])),
        b"SECRET-appended-1".to_vec(),
    ]
    .concat();
    assert_eq!(
        leftovers("appended.xlsx", &appended),
        [found(
            "data_outside_package",
            "",
            17,
            "17 bytes after the package"
        )]
    );
}

#[test]
fn entries_with_no_content_type_are_found_and_never_extracted() {
    let names = ["../evil", "/abs", "C:/x", r"word\document.xml"];
    let traversal = package("xlsx", &[], &names.map(|n| entry(n, b"SECRET-traversal-1")));
    let dir = dir("traversal");
    put(&dir, &[("traversal.xlsx", &zip(&traversal))]);
    let out = run_in(&dir, &["traversal.xlsx", "--json"]);
    let kinds: Vec<_> = json(&out)["files"][0]["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| (f["kind"].clone(), f["where"]["part"].clone()))
        .collect();
    assert_eq!(
        kinds,
        names.map(|n| (serde_json::json!("untyped_entries"), serde_json::json!(n)))
    );
    let left: Vec<_> = std::fs::read_dir(dir.parent().unwrap())
        .unwrap()
        .flatten()
        .map(|e| e.file_name())
        .collect();
    assert!(!left.iter().any(|n| n == "evil" || n == "abs"));
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);

    let deck = package("pptx", &[], &[entry("ppt/secret.bin", b"SECRET-untyped-1")]);
    assert_eq!(
        leftovers("untyped_part.pptx", &zip(&deck)),
        [found(
            "untyped_entries",
            "ppt/secret.bin",
            1,
            "1 ZIP entry with no content type"
        )]
    );
}

#[test]
fn parts_nothing_links_to_are_found() {
    let doc = package(
        "docx",
        &[],
        &[entry(
            "word/old-draft.xml",
            b"<w:document xmlns:w=\"urn:x\">SECRET-orphan-1</w:document>",
        )],
    );
    assert_eq!(
        leftovers("orphan.docx", &zip(&doc)),
        [found(
            "orphan_parts",
            "word/old-draft.xml",
            1,
            "1 part that nothing in the file links to"
        )]
    );
    // A relationship part counts as used when its own part is reached.
    let linked = package(
        "docx",
        &[],
        &[
            entry(
                "word/_rels/document.xml.rels",
                rels(&[("rId1", "image", "media/a.png")]).as_bytes(),
            ),
            entry("word/media/a.png", b"png"),
            entry("word/_rels/old.xml.rels", rels(&[]).as_bytes()),
        ],
    );
    assert_eq!(
        leftovers("linked.docx", &zip(&linked)),
        [found(
            "orphan_parts",
            "word/_rels/old.xml.rels",
            1,
            "1 part that nothing in the file links to"
        )]
    );
}

#[test]
fn the_zip_comment_is_found_and_its_text_shown_only_on_request() {
    let mut book = zip(&package("xlsx", &[], &[]));
    let comment = b"SECRET-comment-1";
    let n = book.len();
    book[n - 2..].copy_from_slice(&(comment.len() as u16).to_le_bytes());
    book.extend(comment);
    assert_eq!(
        leftovers("zip_comment.xlsx", &book),
        [found("zip_comment", "", 16, "1 ZIP comment of 16 bytes")]
    );
    let dir = dir("comment");
    put(&dir, &[("zip_comment.xlsx", &book)]);
    let out = run_in(&dir, &["zip_comment.xlsx"]);
    assert!(!stdout(&out).contains("SECRET") && !stderr(&out).contains("SECRET"));
    assert_eq!(out.status.code(), Some(1));
    let out = run_in(&dir, &["zip_comment.xlsx", "--show-values"]);
    assert_eq!(
        stdout(&out),
        "zip_comment.xlsx  found 1 kind of hidden data\n  ✗ leftover-data  1 ZIP comment of 16 bytes\n                   SECRET-comment-1\n  Remove it: doc-sanitize clean zip_comment.xlsx\n"
    );
    let out = run_in(&dir, &["zip_comment.xlsx", "--show-values", "--json"]);
    assert_eq!(
        json(&out)["files"][0]["findings"][0]["value"],
        "SECRET-comment-1"
    );
}

#[test]
fn shared_strings_no_cell_uses_are_found() {
    let sheet = format!(
        "<worksheet xmlns=\"{S}\"><sheetData><row r=\"1\"><c r=\"A1\" t=\"s\"><v>1</v></c><c r=\"B1\"><v>0</v></c></row></sheetData></worksheet>"
    );
    let sst = format!(
        "<sst xmlns=\"{S}\" count=\"3\"><si><t>SECRET-sst-1</t></si><si><t>visible</t></si><si><r><t>SECRET</t></r><r><t>-sst-2 &amp; more</t></r></si><si/></sst>"
    );
    let ox = "application/vnd.openxmlformats-officedocument.spreadsheetml";
    let book = package(
        "xlsx",
        &[
            ("xl/worksheets/sheet1.xml", &format!("{ox}.worksheet+xml")),
            ("xl/sharedStrings.xml", &format!("{ox}.sharedStrings+xml")),
        ],
        &[
            entry(
                "xl/_rels/workbook.xml.rels",
                rels(&[
                    ("rId1", "worksheet", "worksheets/sheet1.xml"),
                    ("rId2", "sharedStrings", "sharedStrings.xml"),
                ])
                .as_bytes(),
            ),
            entry("xl/worksheets/sheet1.xml", sheet.as_bytes()),
            entry("xl/sharedStrings.xml", sst.as_bytes()),
        ],
    );
    let book = zip(&book);
    assert_eq!(
        leftovers("strings.xlsx", &book),
        [found(
            "unused_strings",
            "xl/sharedStrings.xml",
            3,
            "3 shared strings that no cell uses"
        )]
    );
    let dir = dir("strings");
    put(&dir, &[("strings.xlsx", &book)]);
    let out = run_in(&dir, &["strings.xlsx", "--show-values", "--json"]);
    assert_eq!(
        json(&out)["files"][0]["findings"][0]["value"],
        "SECRET-sst-1"
    );
}

#[test]
fn the_glossary_is_found_in_word_files() {
    let doc = package(
        "docx",
        &[(
            "word/glossary/document.xml",
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document.glossary+xml",
        )],
        &[
            entry(
                "word/_rels/document.xml.rels",
                rels(&[("rId9", "glossaryDocument", "glossary/document.xml")]).as_bytes(),
            ),
            entry(
                "word/glossary/document.xml",
                format!("<w:glossaryDocument xmlns:w=\"{W}\"/>").as_bytes(),
            ),
        ],
    );
    assert_eq!(
        leftovers("glossary.docx", &zip(&doc)),
        [found(
            "glossary",
            "word/glossary/document.xml",
            1,
            "1 glossary document (building blocks and AutoText)"
        )]
    );
}

#[test]
fn part_names_with_control_characters_print_safely() {
    let deck = package("pptx", &[], &[entry("ppt/\u{1b}[31mx\u{202e}.bin", b"x")]);
    let dir = dir("controls");
    put(&dir, &[("controls.pptx", &zip(&deck))]);
    let out = run_in(&dir, &["controls.pptx"]);
    assert!(
        stdout(&out).contains("ppt/<U+001B>[31mx<U+202E>.bin: 1 ZIP entry"),
        "{}",
        stdout(&out)
    );
    assert!(!stdout(&out).contains('\u{1b}'));
}
