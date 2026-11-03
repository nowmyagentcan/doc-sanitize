//! Office packages: every variant recognised by its main part's content type, and the
//! variants this version refuses named.

mod common;

use common::*;

/// The checks each format lists, in order.
const DOCX_CHECKS: &[&str] = &["metadata", "comments", "leftover-data"];
const XLSX_CHECKS: &[&str] = &["metadata", "comments", "leftover-data"];
const PPTX_CHECKS: &[&str] = &["metadata", "comments", "leftover-data"];

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
        let ids = match format {
            "docx" => DOCX_CHECKS,
            "xlsx" => XLSX_CHECKS,
            _ => PPTX_CHECKS,
        };
        let listed: Vec<_> = ids
            .iter()
            .map(|id| serde_json::json!({ "id": id, "status": "none_found", "count": 0 }))
            .collect();
        assert_eq!(f["checks"], serde_json::Value::from(listed), "{variant}");
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
    let n = PPTX_CHECKS.len();
    assert_eq!(
        stdout(&out),
        format!(
            "deck.pptx  nothing found by {n} checks
  ✓ {n} checks found nothing: {}
",
            PPTX_CHECKS.join(", ")
        )
    );
    assert_eq!(out.status.code(), Some(0));
}

/// The `leftover-data` findings of a package, as (kind, part, count, detail).
fn leftovers(name: &str, bytes: &[u8]) -> Vec<(String, String, u64, String)> {
    kinds(&check_bytes(name, bytes), "leftover-data")
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
    let shown: Vec<_> = stdout(&out)
        .lines()
        .filter(|l| !l.contains("found nothing"))
        .map(String::from)
        .collect();
    assert_eq!(
        shown,
        [
            "zip_comment.xlsx  found 1 kind of hidden data",
            "  ✗ leftover-data  1 ZIP comment of 16 bytes",
            "                   SECRET-comment-1",
            "  Remove it: doc-sanitize clean zip_comment.xlsx",
        ]
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

const CORE: &str = "http://schemas.openxmlformats.org/package/2006/metadata/core-properties";
const CORE_TYPE: &str = "application/vnd.openxmlformats-package.core-properties+xml";
const APP_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/extended-properties";

fn core_xml(fields: &str) -> String {
    format!(
        "<cp:coreProperties xmlns:cp=\"{CORE}\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\" xmlns:dcterms=\"http://purl.org/dc/terms/\" xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\">{fields}</cp:coreProperties>"
    )
}

/// A document with every kind of metadata, each field holding a planted secret.
fn metadata_docx() -> Vec<u8> {
    let core = core_xml(
        "<dc:title></dc:title><dc:creator>Avery Example SECRET-meta-1</dc:creator><cp:lastModifiedBy>Sam Sample</cp:lastModifiedBy><dc:description>SECRET-meta-2</dc:description><dcterms:created xsi:type=\"dcterms:W3CDTF\">2026-01-02T03:04:05Z</dcterms:created>",
    );
    let app = format!(
        "<Properties xmlns=\"{APP_NS}\" xmlns:vt=\"urn:vt\"><Template>D:\\Projects\\avery\\SECRET-meta-3.dotx</Template><Pages>3</Pages><Words>40</Words><Company>SECRET-meta-4</Company><DocSecurity>0</DocSecurity><TitlesOfParts><vt:vector size=\"2\" baseType=\"lpstr\"><vt:lpstr>One</vt:lpstr><vt:lpstr>SECRET-meta-5</vt:lpstr></vt:vector></TitlesOfParts><Manager/></Properties>"
    );
    let custom = "<Properties xmlns=\"http://schemas.openxmlformats.org/officeDocument/2006/custom-properties\" xmlns:vt=\"urn:vt\"><property pid=\"2\" name=\"Client\"><vt:lpwstr>SECRET-meta-6</vt:lpwstr></property><property pid=\"3\" name=\"MSIP_Label_1234_Name\"><vt:lpwstr>SECRET-meta-7</vt:lpwstr></property></Properties>";
    let body = format!(
        "<w:document xmlns:w=\"{W}\"><w:body><w:p w:rsidR=\"00A1\" w:rsidRDefault=\"00A1\"><w:permStart w:id=\"1\" w:ed=\"SECRET-meta-8\"/><w:r w:rsidRPr=\"00B2\"><w:t>Hello</w:t></w:r><w:permEnd w:id=\"1\"/></w:p></w:body></w:document>"
    );
    let settings = format!(
        "<w:settings xmlns:w=\"{W}\" xmlns:w14=\"http://schemas.microsoft.com/office/word/2010/wordml\" xmlns:w15=\"http://schemas.microsoft.com/office/word/2012/wordml\"><w:rsids><w:rsidRoot w:val=\"00A1\"/><w:rsid w:val=\"00A1\"/><w:rsid w:val=\"00B2\"/></w:rsids><w14:docId w14:val=\"1A2B\"/><w15:docId w15:val=\"{{0000}}\"/></w:settings>"
    );
    let labels = "<clbl:labelList xmlns:clbl=\"http://schemas.microsoft.com/office/2020/mipLabelMetadata\"><clbl:label id=\"{SECRET-meta-9}\" enabled=\"1\" method=\"Standard\"/></clbl:labelList>";
    Pkg::new("docx")
        .body(&body)
        .part(
            "",
            "rId2",
            &format!("{PKG_RELS}/metadata/core-properties"),
            "docProps/core.xml",
            CORE_TYPE,
            core.as_bytes(),
        )
        .part(
            "",
            "rId3",
            "extended-properties",
            "docProps/app.xml",
            &format!("{OX}.extended-properties+xml"),
            app.as_bytes(),
        )
        .part(
            "",
            "rId4",
            "custom-properties",
            "docProps/custom.xml",
            &format!("{OX}.custom-properties+xml"),
            custom.as_bytes(),
        )
        .part(
            "",
            "rId5",
            &format!("{PKG_RELS}/metadata/thumbnail"),
            "docProps/thumbnail.jpeg",
            "",
            b"\xFF\xD8\xFF\xD9",
        )
        .part(
            "",
            "rId6",
            "http://schemas.microsoft.com/office/2020/02/relationships/classificationlabels",
            "docMetadata/LabelInfo.xml",
            "application/vnd.ms-office.classificationlabels+xml",
            labels.as_bytes(),
        )
        .part(
            "word/document.xml",
            "rId1",
            "settings",
            "word/settings.xml",
            &format!("{OX}.wordprocessingml.settings+xml"),
            settings.as_bytes(),
        )
        .zip()
}

/// Every finding's value (empty when it has none) with `--show-values`, and checks that
/// none is printed without it.
fn values(name: &str, bytes: &[u8]) -> Vec<String> {
    let dir = dir("values");
    put(&dir, &[(name, bytes)]);
    for args in [&[name][..], &[name, "--json"]] {
        let plain = run_in(&dir, args);
        assert!(!stdout(&plain).contains("SECRET"), "{}", stdout(&plain));
        assert!(!stderr(&plain).contains("SECRET"), "{}", stderr(&plain));
    }
    let out = run_in(&dir, &[name, "--show-values", "--json"]);
    json(&out)["files"][0]["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["value"].as_str().unwrap_or_default().to_string())
        .collect()
}

#[test]
fn every_kind_of_office_metadata_is_found() {
    let f = check_bytes("meta.docx", &metadata_docx());
    assert_eq!(
        kinds(&f, "metadata"),
        [
            found(
                "core_properties",
                "docProps/core.xml",
                4,
                "4 document properties, including the author"
            ),
            found(
                "app_properties",
                "docProps/app.xml",
                3,
                "3 application properties"
            ),
            found(
                "custom_properties",
                "docProps/custom.xml",
                1,
                "1 custom property"
            ),
            found(
                "sensitivity_label",
                "docProps/custom.xml",
                1,
                "1 sensitivity label field"
            ),
            found(
                "sensitivity_label",
                "docMetadata/LabelInfo.xml",
                1,
                "1 sensitivity label"
            ),
            found(
                "thumbnail",
                "docProps/thumbnail.jpeg",
                1,
                "1 thumbnail picture of the first page"
            ),
            found(
                "tracking_ids",
                "word/document.xml",
                3,
                "3 editing-session ids"
            ),
            found("editor_names", "word/document.xml", 1, "1 editor name"),
            found(
                "tracking_ids",
                "word/settings.xml",
                5,
                "5 editing-session ids"
            ),
        ]
    );
    let values = values("meta.docx", &metadata_docx());
    assert_eq!(
        values,
        [
            "dc:creator=Avery Example SECRET-meta-1; cp:lastModifiedBy=Sam Sample; dc:description=SECRET-meta-2; dcterms:created=2026-01-02T03:04:05Z",
            "Template=D:\\Projects\\avery\\SECRET-meta-3.dotx; Company=SECRET-meta-4; TitlesOfParts=One, SECRET-meta-5",
            "Client=SECRET-meta-6",
            "MSIP_Label_1234_Name=SECRET-meta-7",
            "id={SECRET-meta-9}",
            "",
            "",
            "SECRET-meta-8",
            "",
        ]
    );
}

#[test]
fn workbook_and_deck_ids_and_printer_settings_are_found() {
    let book = Pkg::new("xlsx")
        .body(&format!("<workbook xmlns=\"{S}\" xmlns:xr=\"http://schemas.microsoft.com/office/spreadsheetml/2014/revision\"><fileSharing userName=\"SECRET-meta-10\"/><xr:revisionPtr revIDLastSave=\"0\" documentId=\"8_{{1}}\"/><sheets/><extLst><ext xr:uid=\"{{AB}}\"/></extLst></workbook>"))
        .part(
            "xl/workbook.xml",
            "rId9",
            "printerSettings",
            "xl/printerSettings/printerSettings1.bin",
            &format!("{OX}.spreadsheetml.printerSettings"),
            b"\0\0SECRET-meta-11",
        )
        .zip();
    assert_eq!(
        kinds(&check_bytes("ids.xlsx", &book), "metadata"),
        [
            found(
                "printer_settings",
                "xl/printerSettings/printerSettings1.bin",
                1,
                "1 printer settings part"
            ),
            found("tracking_ids", "xl/workbook.xml", 2, "2 tracking ids"),
            found("editor_names", "xl/workbook.xml", 1, "1 editor name"),
        ]
    );
    assert_eq!(values("ids.xlsx", &book), ["", "", "SECRET-meta-10"]);
    let deck = Pkg::new("pptx")
        .body(&format!("<p:presentation xmlns:p=\"{P}\"><p:extLst><p:ext><p14:creationId xmlns:p14=\"http://schemas.microsoft.com/office/powerpoint/2010/main\" val=\"123\"/></p:ext></p:extLst></p:presentation>"))
        .part(
            "ppt/presentation.xml",
            "rId9",
            "http://schemas.microsoft.com/office/2015/10/relationships/revisionInfo",
            "ppt/revisionInfo.xml",
            "application/vnd.ms-powerpoint.revisioninfo+xml",
            b"<p1510:revInfo xmlns:p1510=\"urn:p1510\"><p1510:revLst><p1510:client id=\"{A}\" v=\"1\"/><p1510:client id=\"{B}\" v=\"2\"/></p1510:revLst></p1510:revInfo>",
        )
        .zip();
    assert_eq!(
        kinds(&check_bytes("ids.pptx", &deck), "metadata"),
        [
            found("tracking_ids", "ppt/presentation.xml", 1, "1 tracking id"),
            found("tracking_ids", "ppt/revisionInfo.xml", 2, "2 tracking ids"),
        ]
    );
}

#[test]
fn empty_properties_are_not_metadata() {
    let doc = Pkg::new("docx")
        .part(
            "",
            "rId2",
            &format!("{PKG_RELS}/metadata/core-properties"),
            "docProps/core.xml",
            CORE_TYPE,
            core_xml("<dc:title/><dc:creator> </dc:creator>").as_bytes(),
        )
        .part(
            "",
            "rId3",
            "extended-properties",
            "docProps/app.xml",
            &format!("{OX}.extended-properties+xml"),
            format!("<Properties xmlns=\"{APP_NS}\"><Pages>1</Pages><Words>2</Words><ScaleCrop>false</ScaleCrop><SharedDoc>false</SharedDoc></Properties>").as_bytes(),
        )
        .zip();
    assert_eq!(kinds(&check_bytes("plain.docx", &doc), "metadata"), []);
}

const SHEET_TYPE: &str =
    "application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml";
const SLIDE_TYPE: &str = "application/vnd.openxmlformats-officedocument.presentationml.slide+xml";

/// A workbook whose sheets (name, state, body) are linked as rId1, rId2, … from workbook.xml
/// and saved as xl/worksheets/sheetN.xml.
fn workbook(sheets: &[(&str, &str, &str)]) -> Pkg {
    let list: String = sheets
        .iter()
        .enumerate()
        .map(|(i, (name, state, _))| {
            format!(
                "<sheet name=\"{name}\" sheetId=\"{}\" state=\"{state}\" r:id=\"rId{}\"/>",
                i + 1,
                i + 1
            )
        })
        .collect();
    let mut p = Pkg::new("xlsx").body(&format!(
        "<workbook xmlns=\"{S}\" xmlns:r=\"{R}\"><sheets>{list}</sheets></workbook>"
    ));
    for (i, (_, _, body)) in sheets.iter().enumerate() {
        p = p.part(
            "xl/workbook.xml",
            &format!("rId{}", i + 1),
            "worksheet",
            &format!("xl/worksheets/sheet{}.xml", i + 1),
            SHEET_TYPE,
            body.as_bytes(),
        );
    }
    p
}

/// A presentation whose slides (bodies) are linked as rId1, rId2, … and saved as
/// ppt/slides/slideN.xml.
fn presentation(slides: &[&str]) -> Pkg {
    let list: String = (1..=slides.len())
        .map(|i| format!("<p:sldId id=\"{}\" r:id=\"rId{i}\"/>", 255 + i))
        .collect();
    let mut p = Pkg::new("pptx").body(&format!(
        "<p:presentation xmlns:p=\"{P}\" xmlns:r=\"{R}\"><p:sldIdLst>{list}</p:sldIdLst><p:sldSz cx=\"12192000\" cy=\"6858000\"/></p:presentation>"
    ));
    for (i, body) in slides.iter().enumerate() {
        p = p.part(
            "ppt/presentation.xml",
            &format!("rId{}", i + 1),
            "slide",
            &format!("ppt/slides/slide{}.xml", i + 1),
            SLIDE_TYPE,
            body.as_bytes(),
        );
    }
    p
}

fn sheet_xml(data: &str) -> String {
    format!("<worksheet xmlns=\"{S}\" xmlns:r=\"{R}\"><sheetData>{data}</sheetData></worksheet>")
}

fn slide_xml(shapes: &str) -> String {
    format!(
        "<p:sld xmlns:p=\"{P}\" xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" xmlns:r=\"{R}\"><p:cSld><p:spTree><p:nvGrpSpPr><p:cNvPr id=\"1\" name=\"\"/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr/>{shapes}</p:spTree></p:cSld></p:sld>"
    )
}

#[test]
fn comments_and_their_authors_are_found() {
    let comments = format!(
        "<w:comments xmlns:w=\"{W}\"><w:comment w:id=\"0\" w:author=\"Avery Example\" w:initials=\"AE\"><w:p><w:r><w:t>SECRET-comment-1</w:t></w:r><w:r><w:t xml:space=\"preserve\"> and more</w:t></w:r></w:p></w:comment><w:comment w:id=\"1\" w:author=\"Sam Sample\"><w:p><w:r><w:t>SECRET-comment-2</w:t></w:r></w:p></w:comment></w:comments>"
    );
    let people = "<w15:people xmlns:w15=\"http://schemas.microsoft.com/office/word/2012/wordml\"><w15:person w15:author=\"SECRET-comment-3\"><w15:presenceInfo w15:providerId=\"None\" w15:userId=\"x\"/></w15:person><w15:person w15:author=\"Sam Sample\"/></w15:people>";
    let doc = Pkg::new("docx")
        .part(
            "word/document.xml",
            "rId1",
            "comments",
            "word/comments.xml",
            &format!("{OX}.wordprocessingml.comments+xml"),
            comments.as_bytes(),
        )
        .part(
            "word/document.xml",
            "rId2",
            "http://schemas.microsoft.com/office/2011/relationships/people",
            "word/people.xml",
            &format!("{OX}.wordprocessingml.people+xml"),
            people.as_bytes(),
        )
        .zip();
    assert_eq!(
        kinds(&check_bytes("c.docx", &doc), "comments"),
        [
            found("comment", "word/comments.xml", 2, "2 comments by 2 people"),
            found("people", "word/people.xml", 2, "2 comment authors"),
        ]
    );
    assert_eq!(
        values("c.docx", &doc),
        ["SECRET-comment-1 and more", "SECRET-comment-3"]
    );

    let notes = format!(
        "<comments xmlns=\"{S}\"><authors><author>Avery Example</author></authors><commentList><comment ref=\"B2\" authorId=\"0\"><text><r><t>SECRET-comment-4</t></r></text></comment></commentList></comments>"
    );
    let threaded = "<ThreadedComments xmlns=\"http://schemas.microsoft.com/office/spreadsheetml/2018/threadedcomments\"><threadedComment ref=\"B2\" dT=\"2026-01-01T00:00:00.00\" personId=\"{P1}\" id=\"{C1}\"><text>SECRET-comment-5</text></threadedComment><threadedComment ref=\"B2\" personId=\"{P2}\" id=\"{C2}\" parentId=\"{C1}\"><text>reply</text></threadedComment></ThreadedComments>";
    let persons = "<personList xmlns=\"http://schemas.microsoft.com/office/spreadsheetml/2018/threadedcomments\"><person displayName=\"SECRET-comment-6\" id=\"{P1}\" userId=\"avery\" providerId=\"None\"/></personList>";
    let book = workbook(&[("Summary", "visible", &sheet_xml(""))])
        .part(
            "xl/worksheets/sheet1.xml",
            "rId1",
            "comments",
            "xl/comments1.xml",
            &format!("{OX}.spreadsheetml.comments+xml"),
            notes.as_bytes(),
        )
        .part(
            "xl/worksheets/sheet1.xml",
            "rId2",
            "http://schemas.microsoft.com/office/2017/10/relationships/threadedComment",
            "xl/threadedComments/threadedComment1.xml",
            "application/vnd.ms-excel.threadedcomments+xml",
            threaded.as_bytes(),
        )
        .part(
            "xl/workbook.xml",
            "rId9",
            "http://schemas.microsoft.com/office/2017/10/relationships/person",
            "xl/persons/person.xml",
            "application/vnd.ms-excel.person+xml",
            persons.as_bytes(),
        )
        .zip();
    let f = check_bytes("c.xlsx", &book);
    assert_eq!(
        kinds(&f, "comments"),
        [
            found("people", "xl/persons/person.xml", 1, "1 comment author"),
            found("note", "xl/comments1.xml", 1, "1 note by 1 person"),
            found(
                "threaded_comment",
                "xl/threadedComments/threadedComment1.xml",
                2,
                "2 threaded comments by 2 people"
            ),
        ]
    );
    assert_eq!(f["findings"][1]["where"]["sheet_index"], 1);
    assert_eq!(f["findings"][1]["where"]["sheet"], "Summary");
    assert_eq!(
        values("c.xlsx", &book),
        ["SECRET-comment-6", "SECRET-comment-4", "SECRET-comment-5"]
    );

    let legacy = format!(
        "<p:cmLst xmlns:p=\"{P}\"><p:cm authorId=\"0\" idx=\"1\"><p:pos x=\"10\" y=\"10\"/><p:text>SECRET-comment-7</p:text></p:cm></p:cmLst>"
    );
    let modern = "<p188:cmLst xmlns:p188=\"http://schemas.microsoft.com/office/powerpoint/2018/8/main\" xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\"><p188:cm id=\"{1}\" authorId=\"{A1}\"><p188:txBody><a:p><a:r><a:t>SECRET-comment-8</a:t></a:r></a:p></p188:txBody></p188:cm></p188:cmLst>";
    let authors = format!(
        "<p:cmAuthorLst xmlns:p=\"{P}\"><p:cmAuthor id=\"0\" name=\"SECRET-comment-9\" initials=\"AE\" lastIdx=\"1\" clrIdx=\"0\"/></p:cmAuthorLst>"
    );
    let deck = presentation(&[&slide_xml(""), &slide_xml("")])
        .part(
            "ppt/slides/slide2.xml",
            "rId1",
            "comments",
            "ppt/comments/comment1.xml",
            &format!("{OX}.presentationml.comments+xml"),
            legacy.as_bytes(),
        )
        .part(
            "ppt/slides/slide2.xml",
            "rId2",
            "http://schemas.microsoft.com/office/2018/10/relationships/comments",
            "ppt/comments/modernComment_1.xml",
            "application/vnd.ms-powerpoint.comments+xml",
            modern.as_bytes(),
        )
        .part(
            "ppt/presentation.xml",
            "rId9",
            "commentAuthors",
            "ppt/commentAuthors.xml",
            &format!("{OX}.presentationml.commentAuthors+xml"),
            authors.as_bytes(),
        )
        .zip();
    let f = check_bytes("c.pptx", &deck);
    assert_eq!(
        kinds(&f, "comments"),
        [
            found("people", "ppt/commentAuthors.xml", 1, "1 comment author"),
            found(
                "comment",
                "ppt/comments/comment1.xml",
                1,
                "1 comment by 1 person"
            ),
            found(
                "comment",
                "ppt/comments/modernComment_1.xml",
                1,
                "1 comment by 1 person"
            ),
        ]
    );
    assert_eq!(f["findings"][1]["where"]["slide"], 2);
    assert_eq!(
        values("c.pptx", &deck),
        ["SECRET-comment-9", "SECRET-comment-7", "SECRET-comment-8"]
    );
}

#[test]
fn a_hidden_sheet_is_named_only_with_values() {
    let book = workbook(&[("Salaries", "veryHidden", &sheet_xml(""))])
        .part(
            "xl/worksheets/sheet1.xml",
            "rId1",
            "comments",
            "xl/comments1.xml",
            &format!("{OX}.spreadsheetml.comments+xml"),
            format!("<comments xmlns=\"{S}\"><commentList><comment ref=\"A1\"><text><t>x</t></text></comment></commentList></comments>").as_bytes(),
        )
        .zip();
    let dir = dir("hidden-name");
    put(&dir, &[("h.xlsx", &book)]);
    let at = |args: &[&str]| json(&run_in(&dir, args))["files"][0]["findings"][0]["where"].clone();
    assert_eq!(
        at(&["h.xlsx", "--json"]),
        serde_json::json!({ "part": "xl/comments1.xml", "sheet_index": 1 })
    );
    assert_eq!(
        at(&["h.xlsx", "--json", "--show-values"])["sheet"],
        "Salaries"
    );
    assert!(stdout(&run_in(&dir, &["h.xlsx"])).contains("  ✗ comments  sheet 1: 1 note\n"));
}
