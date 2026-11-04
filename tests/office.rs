//! Office packages: every variant recognised by its main part's content type, and the
//! variants this version refuses named.

mod common;

use common::*;

/// The checks each format lists, in order.
const DOCX_CHECKS: &[&str] = &[
    "metadata",
    "comments",
    "tracked-changes",
    "hidden-text",
    "fake-redactions",
    "hidden-objects",
    "embedded-files",
    "custom-data",
    "macros",
    "external-links",
    "image-metadata",
    "cropped-images",
    "leftover-data",
    "signatures",
];
const XLSX_CHECKS: &[&str] = &[
    "metadata",
    "comments",
    "tracked-changes",
    "hidden-text",
    "fake-redactions",
    "hidden-sheets",
    "hidden-rows-columns",
    "hidden-objects",
    "embedded-files",
    "custom-data",
    "macros",
    "external-links",
    "image-metadata",
    "cropped-images",
    "pivot-caches",
    "leftover-data",
    "signatures",
];
const PPTX_CHECKS: &[&str] = &[
    "metadata",
    "comments",
    "hidden-slides",
    "speaker-notes",
    "hidden-objects",
    "embedded-files",
    "custom-data",
    "macros",
    "external-links",
    "image-metadata",
    "cropped-images",
    "leftover-data",
    "signatures",
];

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
        let mut bytes = head.as_bytes().to_vec();
        bytes.extend(&PDF[9..]);
        let f = check_bytes("doc.pdf", &bytes);
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
    // The empty fourth string holds nothing, so it isn't counted.
    assert_eq!(
        leftovers("strings.xlsx", &book),
        [found(
            "unused_strings",
            "xl/sharedStrings.xml",
            2,
            "2 shared strings that no cell uses"
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
    let text = stdout(&run_in(&dir, &["h.xlsx"]));
    assert!(
        text.contains("sheet 1: 1 note\n") && !text.contains("Salaries"),
        "{text}"
    );
}

#[test]
fn tracked_changes_are_found_with_the_deleted_text() {
    let body = format!(
        "<w:p><w:pPr><w:rPr><w:del w:id=\"1\" {BY_SAM}/></w:rPr></w:pPr><w:ins w:id=\"2\" {BY_SAM}><w:r><w:t>SECRET-change-1</w:t></w:r></w:ins><w:del w:id=\"3\" {BY_SAM}><w:r><w:delText>SECRET-change-2</w:delText></w:r></w:del></w:p>\
         <w:p><w:moveFrom w:id=\"4\" {BY_SAM}><w:r><w:t>SECRET-change-3</w:t></w:r></w:moveFrom><w:moveTo w:id=\"5\" {BY_SAM}><w:r><w:t>SECRET-change-3</w:t></w:r></w:moveTo>\
         <w:r><w:rPr><w:b/><w:rPrChange w:id=\"6\" {BY_AVERY}><w:rPr/></w:rPrChange></w:rPr><w:t>bold</w:t></w:r></w:p>\
         <w:tbl><w:tr><w:trPr><w:del w:id=\"7\" {BY_SAM}/></w:trPr><w:tc><w:tcPr><w:cellDel w:id=\"8\" {BY_SAM}/></w:tcPr><w:p><w:r><w:t>row</w:t></w:r></w:p></w:tc></w:tr></w:tbl>\
         <w:p><w:del w:id=\"9\" {BY_AVERY}><w:r><w:delText>second</w:delText></w:r></w:del></w:p>"
    );
    let doc = Pkg::new("docx").body(&doc_xml(&body)).zip();
    assert_eq!(
        kinds(&check_bytes("tc.docx", &doc), "tracked-changes"),
        [
            found(
                "insertion",
                "word/document.xml",
                1,
                "1 insertion by 1 author"
            ),
            found(
                "deletion",
                "word/document.xml",
                3,
                "3 deletions by 2 authors, with the deleted text"
            ),
            found("move", "word/document.xml", 2, "2 moves by 1 author"),
            found(
                "format_change",
                "word/document.xml",
                1,
                "1 formatting change by 1 author"
            ),
            found(
                "table_change",
                "word/document.xml",
                2,
                "2 table changes by 1 author"
            ),
        ]
    );
    assert_eq!(
        values("tc.docx", &doc),
        [
            "SECRET-change-1",
            "SECRET-change-2",
            "SECRET-change-3",
            "",
            ""
        ]
    );
}

#[test]
fn changes_are_found_by_namespace_and_in_every_branch() {
    // Fixture 23: the WordprocessingML namespace bound to another prefix.
    let rebound = format!(
        "<x:document xmlns:x=\"{W}\"><x:body><x:p><x:del x:id=\"1\" x:author=\"Sam Sample\"><x:r><x:delText>SECRET-rebind-1</x:delText></x:r></x:del></x:p></x:body></x:document>"
    );
    let f = check_bytes("prefix_rebind.docx", &Pkg::new("docx").body(&rebound).zip());
    assert_eq!(
        kinds(&f, "tracked-changes"),
        [found(
            "deletion",
            "word/document.xml",
            1,
            "1 deletion by 1 author, with the deleted text"
        )]
    );
    // A `w` prefix bound elsewhere is not WordprocessingML.
    let foreign = "<w:document xmlns:w=\"urn:other\"><w:body><w:del><w:delText>x</w:delText></w:del></w:body></w:document>";
    let f = check_bytes("foreign.docx", &Pkg::new("docx").body(foreign).zip());
    assert_eq!(kinds(&f, "tracked-changes"), []);
    // Fixture 24: the secret only in mc:Fallback.
    let fallback = doc_xml(
        "<w:p><mc:AlternateContent><mc:Choice Requires=\"w14\"><w:r><w:t>x</w:t></w:r></mc:Choice><mc:Fallback><w:del w:id=\"1\" w:author=\"Sam Sample\"><w:r><w:delText>SECRET-fallback-1</w:delText></w:r></w:del></mc:Fallback></mc:AlternateContent></w:p>",
    );
    let doc = Pkg::new("docx").body(&fallback).zip();
    assert_eq!(
        kinds(&check_bytes("mc_fallback.docx", &doc), "tracked-changes").len(),
        1
    );
    assert_eq!(values("mc_fallback.docx", &doc), ["SECRET-fallback-1"]);
}

#[test]
fn changes_in_headers_and_styles_are_found() {
    let header = format!(
        "<w:hdr xmlns:w=\"{W}\"><w:p><w:ins w:id=\"1\"><w:r><w:t>SECRET-header-1</w:t></w:r></w:ins></w:p></w:hdr>"
    );
    let styles = format!(
        "<w:styles xmlns:w=\"{W}\"><w:style w:type=\"paragraph\" w:styleId=\"Normal\"><w:pPr><w:jc w:val=\"left\"/><w:pPrChange w:id=\"2\" w:author=\"Sam Sample\"><w:pPr/></w:pPrChange></w:pPr></w:style></w:styles>"
    );
    let doc = Pkg::new("docx")
        .part(
            "word/document.xml",
            "rId1",
            "header",
            "word/header1.xml",
            &format!("{OX}.wordprocessingml.header+xml"),
            header.as_bytes(),
        )
        .part(
            "word/document.xml",
            "rId2",
            "styles",
            "word/styles.xml",
            &format!("{OX}.wordprocessingml.styles+xml"),
            styles.as_bytes(),
        )
        .zip();
    assert_eq!(
        kinds(&check_bytes("hs.docx", &doc), "tracked-changes"),
        [
            found("insertion", "word/header1.xml", 1, "1 insertion"),
            found(
                "format_change",
                "word/styles.xml",
                1,
                "1 formatting change by 1 author"
            ),
        ]
    );
}

#[test]
fn shared_workbook_logs_are_found() {
    let ss = "application/vnd.openxmlformats-officedocument.spreadsheetml";
    let book = workbook(&[("Sheet1", "visible", &sheet_xml(""))])
        .part("xl/workbook.xml", "rId8", "revisionHeaders", "xl/revisions/revisionHeaders.xml", &format!("{ss}.revisionHeaders+xml"), format!("<headers xmlns=\"{S}\" xmlns:r=\"{R}\" guid=\"{{1}}\"><header guid=\"{{2}}\" dateTime=\"2026-01-01T00:00:00\" userName=\"Sam Sample\" r:id=\"rId1\"/></headers>").as_bytes())
        .part("xl/revisions/revisionHeaders.xml", "rId1", "revisionLog", "xl/revisions/revisionLog1.xml", &format!("{ss}.revisionLog+xml"), format!("<revisions xmlns=\"{S}\"><rcc rId=\"1\" sId=\"1\"><nc r=\"A1\" t=\"inlineStr\"><is><t>SECRET-log-1</t></is></nc></rcc></revisions>").as_bytes())
        .part("xl/workbook.xml", "rId9", "usernames", "xl/revisions/userNames.xml", &format!("{ss}.userNames+xml"), format!("<users xmlns=\"{S}\" count=\"1\"><userInfo guid=\"{{3}}\" name=\"SECRET-log-2\" id=\"1\" dateTime=\"2026-01-01T00:00:00\"/></users>").as_bytes())
        .zip();
    assert_eq!(
        kinds(&check_bytes("shared.xlsx", &book), "tracked-changes"),
        [
            found(
                "shared_workbook_log",
                "xl/revisions/revisionHeaders.xml",
                1,
                "1 shared-workbook revision list"
            ),
            found(
                "shared_workbook_log",
                "xl/revisions/revisionLog1.xml",
                1,
                "1 shared-workbook revision log"
            ),
            found(
                "shared_workbook_log",
                "xl/revisions/userNames.xml",
                1,
                "1 shared-workbook user list"
            ),
        ]
    );
    assert_eq!(values("shared.xlsx", &book), ["", "", "SECRET-log-2"]);
}

/// A document with text hidden every way Word allows, and text that only looks like it.
fn hidden_docx() -> Vec<u8> {
    let styles = format!(
        "<w:styles xmlns:w=\"{W}\"><w:docDefaults><w:rPrDefault><w:rPr><w:sz w:val=\"22\"/></w:rPr></w:rPrDefault></w:docDefaults>\
         <w:style w:type=\"paragraph\" w:default=\"1\" w:styleId=\"Normal\"><w:name w:val=\"Normal\"/></w:style>\
         <w:style w:type=\"character\" w:styleId=\"Secret\"><w:rPr><w:vanish/></w:rPr></w:style>\
         <w:style w:type=\"character\" w:styleId=\"Child\"><w:basedOn w:val=\"Secret\"/><w:rPr><w:b/></w:rPr></w:style></w:styles>"
    );
    let body = [
        para(&text_run("", "Visible text")),
        para(&(text_run("<w:vanish/>", "SECRET-hidden-1") + &text_run("", "shown"))),
        para(&text_run("<w:rStyle w:val=\"Child\"/>", "SECRET-hidden-2")),
        para(&text_run("<w:vanish w:val=\"0\"/>", "not hidden")),
        "<w:p><w:pPr><w:rPr><w:vanish/></w:rPr></w:pPr></w:p>".into(),
        para(&text_run("<w:sz w:val=\"1\"/>", "SECRET-tiny-1")),
        para(&text_run("<w:color w:val=\"FFFFFF\"/>", "SECRET-white-1")),
        para(&text_run("<w:color w:val=\"000000\" w:themeColor=\"background1\"/>", "SECRET-white-2")),
        para(&text_run("<w:color w:val=\"000000\"/><w:highlight w:val=\"black\"/>", "SECRET-black-1")),
        format!("<w:tbl><w:tr><w:tc><w:tcPr><w:shd w:val=\"clear\" w:color=\"auto\" w:fill=\"000000\"/></w:tcPr>{}</w:tc></w:tr></w:tbl>", para(&text_run("<w:color w:val=\"111111\"/>", "SECRET-black-2"))),
        para(&text_run("<w:color w:val=\"auto\"/><w:highlight w:val=\"black\"/>", "auto on black")),
        para(&text_run("<w:color w:val=\"FFFFFF\"/><w:highlight w:val=\"darkBlue\"/>", "white on blue")),
        para(&text_run("<w:color w:val=\"FFFFFF\"/><w:shd w:val=\"clear\" w:fill=\"auto\"/><w:highlight w:val=\"none\"/>", "SECRET-white-3")),
    ]
    .concat();
    let header = format!(
        "<w:hdr xmlns:w=\"{W}\">{}</w:hdr>",
        para(&text_run("", "Header text"))
    );
    let wml = |t: &str| format!("{OX}.wordprocessingml.{t}+xml");
    Pkg::new("docx")
        .body(&doc_xml(&body))
        .part(
            "word/document.xml",
            "rId1",
            "styles",
            "word/styles.xml",
            &wml("styles"),
            styles.as_bytes(),
        )
        .part(
            "word/document.xml",
            "rId2",
            "theme",
            "word/theme/theme1.xml",
            THEME_TYPE,
            theme_xml().as_bytes(),
        )
        .part(
            "word/document.xml",
            "rId3",
            "header",
            "word/header1.xml",
            &wml("header"),
            header.as_bytes(),
        )
        .zip()
}

#[test]
fn hidden_and_same_colour_text_is_found_in_documents() {
    let f = check_bytes("hidden.docx", &hidden_docx());
    assert_eq!(
        kinds(&f, "hidden-text"),
        [
            found(
                "hidden_formatting",
                "word/document.xml",
                30,
                "30 characters with hidden formatting"
            ),
            found(
                "tiny",
                "word/document.xml",
                13,
                "13 characters smaller than 1 pt"
            ),
            found(
                "same_colour",
                "word/document.xml",
                42,
                "42 characters coloured like the background"
            ),
        ]
    );
    assert_eq!(
        kinds(&f, "fake-redactions"),
        [found(
            "same_colour",
            "word/document.xml",
            28,
            "28 characters coloured like the dark background"
        )]
    );
    assert_eq!(
        values("hidden.docx", &hidden_docx()),
        [
            "SECRET-hidden-1",
            "SECRET-tiny-1",
            "SECRET-white-1",
            "SECRET-black-1"
        ]
    );
}

#[test]
fn hidden_and_same_colour_cells_are_found() {
    let styles = styles_xml(
        "<numFmts count=\"1\"><numFmt numFmtId=\"164\" formatCode=\";;;\"/></numFmts>\
         <fonts><font><sz val=\"11\"/></font><font><color rgb=\"FFFFFFFF\"/></font><font><color rgb=\"FF000000\"/></font><font><color theme=\"0\"/></font><font><color theme=\"1\" tint=\"0.5\"/></font></fonts>\
         <fills><fill><patternFill patternType=\"none\"/></fill><fill><patternFill patternType=\"gray125\"/></fill><fill><patternFill patternType=\"solid\"><fgColor rgb=\"FF000000\"/></patternFill></fill></fills>\
         <cellStyleXfs><xf numFmtId=\"164\" fontId=\"1\"/></cellStyleXfs>\
         <cellXfs><xf numFmtId=\"0\" fontId=\"0\" fillId=\"0\"/><xf numFmtId=\"164\" fontId=\"0\" fillId=\"0\"/><xf numFmtId=\"0\" fontId=\"1\" fillId=\"0\"/><xf numFmtId=\"0\" fontId=\"2\" fillId=\"2\"/><xf numFmtId=\"0\" fontId=\"0\" fillId=\"2\"/><xf numFmtId=\"0\" fontId=\"3\" fillId=\"1\"/><xf numFmtId=\"0\" fontId=\"1\" fillId=\"2\"/><xf numFmtId=\"0\" fontId=\"4\" fillId=\"0\"/></cellXfs>",
    );
    let sheet = sheet_xml(
        "<row r=\"1\"><c r=\"A1\" t=\"inlineStr\"><is><t>visible</t></is></c></row>\
         <row r=\"2\"><c r=\"B2\" s=\"1\" t=\"inlineStr\"><is><t>SECRET-cell-1</t></is></c></row>\
         <row r=\"3\"><c r=\"C3\" s=\"2\" t=\"s\"><v>0</v></c></row>\
         <row r=\"4\"><c r=\"C4\" s=\"5\"><v>7</v></c></row>\
         <row r=\"5\"><c r=\"D5\" s=\"3\"><v>42</v></c></row>\
         <row r=\"6\"><c r=\"D6\" s=\"4\"><v>43</v></c><c s=\"6\"><v>1</v></c><c r=\"G6\" s=\"7\"><v>2</v></c></row>\
         <row r=\"8\"><c r=\"F8\" s=\"2\"/></row>",
    );
    let sst = format!("<sst xmlns=\"{S}\"><si><t>SECRET-cell-2</t></si></sst>");
    let ss = |t: &str| format!("{OX}.spreadsheetml.{t}+xml");
    let book = workbook(&[("Data", "visible", &sheet)])
        .part(
            "xl/workbook.xml",
            "rId7",
            "styles",
            "xl/styles.xml",
            &ss("styles"),
            styles.as_bytes(),
        )
        .part(
            "xl/workbook.xml",
            "rId8",
            "theme",
            "xl/theme/theme1.xml",
            THEME_TYPE,
            theme_xml().as_bytes(),
        )
        .part(
            "xl/workbook.xml",
            "rId9",
            "sharedStrings",
            "xl/sharedStrings.xml",
            &ss("sharedStrings"),
            sst.as_bytes(),
        )
        .zip();
    let f = check_bytes("cells.xlsx", &book);
    assert_eq!(
        kinds(&f, "hidden-text"),
        [
            found(
                "hidden_format",
                "xl/worksheets/sheet1.xml",
                1,
                "1 cell with a number format that shows nothing"
            ),
            found(
                "same_colour",
                "xl/worksheets/sheet1.xml",
                2,
                "2 cells with text coloured like the cell fill"
            ),
        ]
    );
    assert_eq!(
        kinds(&f, "fake-redactions"),
        [found(
            "same_colour",
            "xl/worksheets/sheet1.xml",
            2,
            "2 cells with text coloured like the dark cell fill"
        )]
    );
    let places: Vec<_> = f["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["where"].clone())
        .collect();
    assert_eq!(
        places,
        [
            serde_json::json!({ "part": "xl/worksheets/sheet1.xml", "sheet_index": 1, "sheet": "Data", "cells": "B2" }),
            serde_json::json!({ "part": "xl/worksheets/sheet1.xml", "sheet_index": 1, "sheet": "Data", "cells": "C3:C4" }),
            serde_json::json!({ "part": "xl/worksheets/sheet1.xml", "sheet_index": 1, "sheet": "Data", "cells": "D5:D6" }),
        ]
    );
    assert_eq!(
        values("cells.xlsx", &book),
        ["SECRET-cell-1", "SECRET-cell-2", "42"]
    );
    let dir = dir("cells-text");
    put(&dir, &[("cells.xlsx", &book)]);
    assert!(stdout(&run_in(&dir, &["cells.xlsx"])).contains(
        "  ✗ fake-redactions  sheet Data, D5:D6: 2 cells with text coloured like the dark cell fill\n"
    ));
}

#[test]
fn hidden_sheets_rows_and_columns_are_found() {
    let summary = format!(
        "<worksheet xmlns=\"{S}\"><cols><col min=\"2\" max=\"2\" width=\"0.05\" customWidth=\"1\"/><col min=\"6\" max=\"7\" width=\"9\" hidden=\"1\"/></cols><sheetData>\
         <row r=\"1\"><c r=\"A1\"><v>1</v></c><c r=\"F1\"><v>11</v></c></row>\
         <row r=\"2\"><c r=\"B2\" t=\"inlineStr\"><is><t>SECRET-col-1</t></is></c><c r=\"G2\"><v>12</v></c></row>\
         <row r=\"3\" hidden=\"1\"><c r=\"A3\" t=\"inlineStr\"><is><t>SECRET-row-1</t></is></c><c r=\"C3\"><v>3</v></c></row>\
         <row r=\"4\" customHeight=\"1\" ht=\"0.2\"><c r=\"A4\"><v>4</v></c></row>\
         <row r=\"5\" hidden=\"1\"><c r=\"A5\"/></row>\
         <row r=\"6\" customHeight=\"1\" ht=\"30\"><c r=\"A6\"><v>6</v></c></row>\
         </sheetData></worksheet>"
    );
    let book = workbook(&[
        ("Summary", "visible", &summary),
        ("Salaries", "veryHidden", &sheet_xml("")),
        ("Draft", "hidden", &sheet_xml("")),
    ])
    .zip();
    let f = check_bytes("hidden.xlsx", &book);
    assert_eq!(
        kinds(&f, "hidden-sheets"),
        [
            found(
                "hidden_sheet",
                "xl/worksheets/sheet2.xml",
                1,
                "1 very hidden sheet"
            ),
            found(
                "hidden_sheet",
                "xl/worksheets/sheet3.xml",
                1,
                "1 hidden sheet"
            ),
        ]
    );
    assert_eq!(
        findings_of(&f, "hidden-sheets")[0]["where"],
        serde_json::json!({ "part": "xl/worksheets/sheet2.xml", "sheet_index": 2 })
    );
    assert_eq!(
        kinds(&f, "hidden-rows-columns"),
        [
            found(
                "hidden_rows",
                "xl/worksheets/sheet1.xml",
                2,
                "2 hidden rows holding 3 values"
            ),
            found(
                "hidden_columns",
                "xl/worksheets/sheet1.xml",
                3,
                "3 hidden columns holding 3 values"
            ),
        ]
    );
    let cells: Vec<_> = findings_of(&f, "hidden-rows-columns")
        .iter()
        .map(|x| x["where"]["cells"].clone())
        .collect();
    assert_eq!(cells, ["A3:C4", "B1:G2"]);
    assert_eq!(
        values("hidden.xlsx", &book),
        ["Salaries", "Draft", "SECRET-row-1", "11"]
    );
}

#[test]
fn hidden_slides_notes_and_shapes_are_found() {
    let group = format!(
        "<p:grpSp><p:nvGrpSpPr><p:cNvPr id=\"10\" name=\"Group\"/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr><a:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"100\" cy=\"100\"/><a:chOff x=\"0\" y=\"0\"/><a:chExt cx=\"10\" cy=\"10\"/></a:xfrm></p:grpSpPr>{}{}</p:grpSp>",
        shape(11, false, &xfrm(2_000_000, 0, 10, 10), "SECRET-offslide-1"),
        shape(12, false, &xfrm(5, 5, 1, 1), "inside"),
    );
    let slide1 = slide_xml(
        &[
            shape(2, true, &xfrm(0, 0, 100, 100), "SECRET-shape-1"),
            shape(3, false, "", "placeholder"),
            shape(4, false, &xfrm(-500_000, 100, 1_000_000, 100), "partly on"),
            shape(
                5,
                false,
                &xfrm(-5_000_000, 100, 1_000_000, 100),
                "left of it",
            ),
        ]
        .concat(),
    );
    let slide2 = slide_xml(&shape(2, false, &xfrm(0, 0, 10, 10), "SECRET-slide-1"))
        .replace("<p:sld ", "<p:sld show=\"0\" ");
    let slide3 = slide_xml(&group);
    let notes = |body: &str| {
        format!(
            "<p:notes xmlns:p=\"{P}\" xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\"><p:cSld><p:spTree>{body}</p:spTree></p:cSld></p:notes>"
        )
    };
    let ph = |kind: &str, text: &str| {
        format!(
            "<p:sp><p:nvSpPr><p:cNvPr id=\"2\" name=\"x\"/><p:cNvSpPr/><p:nvPr><p:ph type=\"{kind}\"/></p:nvPr></p:nvSpPr><p:spPr/><p:txBody><a:bodyPr/><a:p><a:r><a:t>{text}</a:t></a:r></a:p></p:txBody></p:sp>"
        )
    };
    let notes1 = notes(&(ph("sldImg", "") + &ph("body", "SECRET-notes-1") + &ph("sldNum", "1")));
    let notes3 = notes(&(ph("sldImg", "") + &ph("body", " ") + &ph("sldNum", "3")));
    let pml = |t: &str| format!("{OX}.presentationml.{t}+xml");
    let deck = presentation(&[&slide1, &slide2, &slide3])
        .part(
            "ppt/slides/slide1.xml",
            "rId9",
            "notesSlide",
            "ppt/notesSlides/notesSlide1.xml",
            &pml("notesSlide"),
            notes1.as_bytes(),
        )
        .part(
            "ppt/slides/slide3.xml",
            "rId9",
            "notesSlide",
            "ppt/notesSlides/notesSlide3.xml",
            &pml("notesSlide"),
            notes3.as_bytes(),
        )
        .zip();
    let f = check_bytes("deck.pptx", &deck);
    assert_eq!(
        kinds(&f, "hidden-slides"),
        [found(
            "hidden_slide",
            "ppt/slides/slide2.xml",
            1,
            "1 hidden slide"
        )]
    );
    assert_eq!(
        kinds(&f, "speaker-notes"),
        [found(
            "notes",
            "ppt/notesSlides/notesSlide1.xml",
            1,
            "speaker notes with 14 characters"
        )]
    );
    assert_eq!(
        kinds(&f, "hidden-objects"),
        [
            found(
                "hidden_object",
                "ppt/slides/slide1.xml",
                1,
                "1 hidden object"
            ),
            found(
                "off_slide_object",
                "ppt/slides/slide1.xml",
                1,
                "1 object off the slide"
            ),
            found(
                "off_slide_object",
                "ppt/slides/slide3.xml",
                1,
                "1 object off the slide"
            ),
        ]
    );
    let slides: Vec<_> = f["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["where"]["slide"].clone())
        .collect();
    assert_eq!(slides, [2, 1, 1, 1, 3]);
    assert_eq!(
        values("deck.pptx", &deck),
        [
            "SECRET-slide-1",
            "SECRET-notes-1",
            "SECRET-shape-1",
            "left of it",
            "SECRET-offslide-1"
        ]
    );
}

#[test]
fn hidden_shapes_are_found_in_documents_and_sheets() {
    let body = "".to_string()
        + "<w:p><w:r><w:drawing><wp:inline><wp:extent cx=\"10\" cy=\"10\"/><wp:docPr id=\"1\" name=\"Picture 1\" hidden=\"1\"/><a:graphic><a:graphicData><pic:pic><pic:nvPicPr><pic:cNvPr id=\"0\" name=\"x\"/><pic:cNvPicPr/></pic:nvPicPr></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r>\
         <w:r><w:pict><v:shape id=\"t1\" style=\"position:absolute;visibility: hidden\"><v:textbox><w:txbxContent><w:p><w:r><w:t>SECRET-vml-1</w:t></w:r></w:p></w:txbxContent></v:textbox></v:shape></w:pict></w:r>\
         <w:r><w:drawing><wp:inline><wp:docPr id=\"2\" name=\"Shown\"/></wp:inline></w:drawing></w:r></w:p>";
    let doc = Pkg::new("docx").body(&doc_xml(&body)).zip();
    assert_eq!(
        kinds(&check_bytes("shapes.docx", &doc), "hidden-objects"),
        [found(
            "hidden_object",
            "word/document.xml",
            2,
            "2 hidden objects"
        )]
    );
    let drawing = "<xdr:wsDr xmlns:xdr=\"http://schemas.openxmlformats.org/drawingml/2006/spreadsheetDrawing\" xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\"><xdr:twoCellAnchor><xdr:sp><xdr:nvSpPr><xdr:cNvPr id=\"2\" name=\"Box\" hidden=\"1\"/><xdr:cNvSpPr/></xdr:nvSpPr><xdr:txBody><a:p><a:r><a:t>SECRET-xdr-1</a:t></a:r></a:p></xdr:txBody></xdr:sp></xdr:twoCellAnchor></xdr:wsDr>";
    let book = workbook(&[("Data", "visible", &sheet_xml(""))])
        .part(
            "xl/worksheets/sheet1.xml",
            "rId1",
            "drawing",
            "xl/drawings/drawing1.xml",
            &format!("{OX}.drawing+xml"),
            drawing.as_bytes(),
        )
        .zip();
    let f = check_bytes("shapes.xlsx", &book);
    assert_eq!(
        kinds(&f, "hidden-objects"),
        [found(
            "hidden_object",
            "xl/drawings/drawing1.xml",
            1,
            "1 hidden object"
        )]
    );
    assert_eq!(
        findings_of(&f, "hidden-objects")[0]["where"]["sheet"],
        "Data"
    );
    assert_eq!(values("shapes.xlsx", &book), ["SECRET-xdr-1"]);
}

#[test]
fn embedded_files_custom_data_macros_and_signatures_are_found() {
    let body = concat!(
        "<w:altChunk r:id=\"rId20\"/>",
        "<w:sdt><w:sdtPr><w:dataBinding w:xpath=\"/root/client\" w:storeItemID=\"{1}\"/></w:sdtPr><w:sdtContent><w:p><w:r><w:t>Bound</w:t></w:r></w:p></w:sdtContent></w:sdt>",
        "<w:p><w:r><w:fldChar w:fldCharType=\"begin\"/></w:r><w:r><w:instrText xml:space=\"preserve\"> DDEAUTO excel </w:instrText></w:r><w:r><w:instrText>\"D:\\\\Projects\\\\SECRET-dde-1.xlsx\" A1</w:instrText></w:r><w:r><w:fldChar w:fldCharType=\"separate\"/></w:r><w:r><w:t>42</w:t></w:r><w:r><w:fldChar w:fldCharType=\"end\"/></w:r>",
        "<w:fldSimple w:instr=\" PAGE \"><w:r><w:t>1</w:t></w:r></w:fldSimple><w:fldSimple w:instr=\"dde excel x\"/></w:p>",
    );
    let settings = format!(
        "<w:settings xmlns:w=\"{W}\"><w:docVars><w:docVar w:name=\"Client\" w:val=\"SECRET-var-1\"/><w:docVar w:name=\"Matter\" w:val=\"42\"/></w:docVars></w:settings>"
    );
    let ms = "http://schemas.microsoft.com/office";
    let doc = Pkg::new("docm")
        .body(&doc_xml(body))
        .part("word/document.xml", "rId1", "settings", "word/settings.xml", &format!("{OX}.wordprocessingml.settings+xml"), settings.as_bytes())
        .part("word/document.xml", "rId2", "oleObject", "word/embeddings/oleObject1.bin", "", b"\xD0\xCF\x11\xE0SECRET-ole-1")
        .part("word/document.xml", "rId3", "package", "word/embeddings/Book1.xlsx", &format!("{OX}.spreadsheetml.sheet"), b"PK\x03\x04SECRET-pkg-1")
        .part("word/document.xml", "rId20", "aFChunk", "word/afchunk.htm", "text/html", b"<p>SECRET-chunk-1</p>")
        .part("word/document.xml", "rId4", "customXml", "customXml/item1.xml", "", b"<root><client>SECRET-xml-1</client><matter> 7 </matter></root>")
        .part("word/document.xml", "rId5", &format!("{ms}/2006/relationships/vbaProject"), "word/vbaProject.bin", "application/vnd.ms-office.vbaProject", b"\xCC\x61SECRET-vba-1")
        .part("word/document.xml", "rId6", "control", "word/activeX/activeX1.xml", "application/vnd.ms-office.activeX+xml", b"<ax:ocx xmlns:ax=\"http://schemas.microsoft.com/office/2006/activeX\" ax:classid=\"{8BD21D40}\"/>")
        .part("", "rId7", &format!("{ms}/2007/relationships/ui/extensibility"), "customUI/customUI14.xml", "", b"<customUI xmlns=\"http://schemas.microsoft.com/office/2009/07/customui\"/>")
        .part("", "rId8", &format!("{PKG_RELS}/digital-signature/origin"), "_xmlsignatures/origin.sigs", "application/vnd.openxmlformats-package.digital-signature-origin", b"")
        .part("_xmlsignatures/origin.sigs", "rId1", &format!("{PKG_RELS}/digital-signature/signature"), "_xmlsignatures/sig1.xml", "application/vnd.openxmlformats-package.digital-signature-xmlsignature+xml", b"<Signature xmlns=\"http://www.w3.org/2000/09/xmldsig#\"/>")
        .zip();
    let f = check_bytes("parts.docm", &doc);
    assert_eq!(
        kinds(&f, "embedded-files"),
        [
            found(
                "imported_chunk",
                "word/document.xml",
                1,
                "1 imported document"
            ),
            found(
                "embedded_object",
                "word/embeddings/oleObject1.bin",
                1,
                "1 embedded OLE object"
            ),
            found(
                "embedded_object",
                "word/embeddings/Book1.xlsx",
                1,
                "1 embedded file"
            ),
        ]
    );
    assert_eq!(
        kinds(&f, "custom-data"),
        [
            found(
                "data_binding",
                "word/document.xml",
                1,
                "1 data-bound content control"
            ),
            found(
                "document_variables",
                "word/settings.xml",
                2,
                "2 document variables"
            ),
            found("custom_xml", "customXml/item1.xml", 1, "1 custom XML part"),
        ]
    );
    assert_eq!(
        kinds(&f, "macros"),
        [
            found("dde_field", "word/document.xml", 2, "2 DDE fields"),
            found(
                "vba_project",
                "word/vbaProject.bin",
                1,
                "1 VBA macro project"
            ),
            found(
                "activex",
                "word/activeX/activeX1.xml",
                1,
                "1 ActiveX control"
            ),
            found(
                "ribbon_customization",
                "customUI/customUI14.xml",
                1,
                "1 custom ribbon"
            ),
        ]
    );
    assert_eq!(
        kinds(&f, "signatures"),
        [found(
            "package_signature",
            "_xmlsignatures/sig1.xml",
            1,
            "1 digital signature"
        )]
    );
    let values = values("parts.docm", &doc);
    let shown: Vec<_> = values
        .iter()
        .filter(|v| !v.is_empty())
        .map(String::as_str)
        .collect();
    assert_eq!(
        shown,
        [
            "Client=SECRET-var-1; Matter=42",
            "SECRET-xml-1 7",
            "DDEAUTO excel \"D:\\\\Projects\\\\SECRET-dde-1.xlsx\" A1",
        ]
    );
}

#[test]
fn workbook_and_deck_data_parts_are_found() {
    let sheet = format!(
        "<worksheet xmlns=\"{S}\" xmlns:r=\"{R}\"><sheetData/><scenarios><scenario name=\"Best\" count=\"2\"><inputCells r=\"A1\" val=\"SECRET-scenario-1\"/><inputCells r=\"A2\" val=\"2\"/></scenario></scenarios><customProperties><customPr name=\"SECRET-custom-1\" r:id=\"rId1\"/></customProperties></worksheet>"
    );
    let chart = "<c:chartSpace xmlns:c=\"http://schemas.openxmlformats.org/drawingml/2006/chart\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"><c:externalData r:id=\"rId1\"/></c:chartSpace>";
    let macro_sheet = format!(
        "<xm:macrosheet xmlns:xm=\"http://schemas.microsoft.com/office/excel/2006/main\" xmlns=\"{S}\"><sheetData/></xm:macrosheet>"
    );
    let book = workbook(&[("Data", "visible", &sheet), ("Auto_Open", "hidden", &macro_sheet)])
        .part("xl/worksheets/sheet1.xml", "rId5", "drawing", "xl/drawings/drawing1.xml", &format!("{OX}.drawing+xml"), b"<xdr:wsDr xmlns:xdr=\"http://schemas.openxmlformats.org/drawingml/2006/spreadsheetDrawing\"/>")
        .part("xl/drawings/drawing1.xml", "rId1", "chart", "xl/charts/chart1.xml", &format!("{OX}.drawingml.chart+xml"), chart.as_bytes())
        .part("xl/charts/chart1.xml", "rId1", "package", "xl/embeddings/Book2.xlsx", "", b"PK")
        .entries();
    let types = zip_types_with(
        &book,
        "xl/worksheets/sheet2.xml",
        "application/vnd.ms-excel.macrosheet+xml",
    );
    let book = zip(&with(book, "[Content_Types].xml", &types));
    let f = check_bytes("data.xlsx", &book);
    assert_eq!(
        kinds(&f, "embedded-files"),
        [found(
            "chart_workbook",
            "xl/embeddings/Book2.xlsx",
            1,
            "1 workbook embedded in a chart"
        )]
    );
    assert_eq!(
        kinds(&f, "custom-data"),
        [
            found(
                "scenarios",
                "xl/worksheets/sheet1.xml",
                2,
                "1 scenario with 2 saved values"
            ),
            found(
                "sheet_properties",
                "xl/worksheets/sheet1.xml",
                1,
                "1 custom sheet property"
            ),
        ]
    );
    assert_eq!(
        kinds(&f, "macros"),
        [found(
            "macro_sheet",
            "xl/worksheets/sheet2.xml",
            1,
            "1 Excel 4.0 macro sheet"
        )]
    );
    let deck = presentation(&[&slide_xml("")])
        .part("ppt/slides/slide1.xml", "rId5", "tags", "ppt/tags/tag1.xml", &format!("{OX}.presentationml.tags+xml"), format!("<p:tagLst xmlns:p=\"{P}\"><p:tag name=\"CLIENT\" val=\"SECRET-tag-1\"/></p:tagLst>").as_bytes())
        .part("", "rId7", "http://schemas.microsoft.com/office/2011/relationships/webextension", "ppt/webextensions/webextension1.xml", "application/vnd.ms-office.webextension+xml", b"<we:webextension xmlns:we=\"http://schemas.microsoft.com/office/webextensions/webextension/2010/11\" id=\"{1}\"><we:properties><we:property name=\"token\" value=\"SECRET-addin-1\"/></we:properties></we:webextension>")
        .zip();
    let f = check_bytes("data.pptx", &deck);
    assert_eq!(
        kinds(&f, "custom-data"),
        [
            found(
                "add_in_data",
                "ppt/webextensions/webextension1.xml",
                1,
                "1 add-in data part"
            ),
            found("tags", "ppt/tags/tag1.xml", 1, "1 tags part"),
        ]
    );
    assert_eq!(
        values("data.pptx", &deck),
        ["token=SECRET-addin-1", "CLIENT=SECRET-tag-1"]
    );
}

#[test]
fn links_to_local_files_are_found_and_web_links_are_not() {
    let settings = format!(
        "<w:settings xmlns:w=\"{W}\" xmlns:r=\"{R}\"><w:attachedTemplate r:id=\"rId1\"/><w:mailMerge><w:mainDocumentType w:val=\"formLetters\"/><w:connectString w:val=\"Provider=x\"/><w:dataSource r:id=\"rId2\"/></w:mailMerge></w:settings>"
    );
    let body = "<w:p><w:fldSimple w:instr=\" INCLUDEPICTURE &quot;\\\\\\\\fileserver\\\\share\\\\SECRET-field-1.png&quot; \\* MERGEFORMAT \"/><w:fldSimple w:instr=\"INCLUDEPICTURE &quot;https://example.com/a.png&quot;\"/><w:fldSimple w:instr=\"LINK Excel.Sheet.12 D:\\\\Budget.xlsx A1\"/></w:p>\
         <w:p><w:r><w:drawing><wp:inline><wp:docPr id=\"1\" name=\"Picture 1\" descr=\"D:\\Projects\\avery\\SECRET-alt-1.png\"/></wp:inline></w:drawing></w:r>\
         <w:r><w:drawing><wp:inline><wp:docPr id=\"2\" name=\"Picture 2\" descr=\"A photo of a dog\" title=\"/home/avery/SECRET-alt-2.jpg\"/></wp:inline></w:drawing></w:r></w:p>";
    let doc = Pkg::new("docx")
        .body(&doc_xml(body))
        .part(
            "word/document.xml",
            "rId1",
            "settings",
            "word/settings.xml",
            &format!("{OX}.wordprocessingml.settings+xml"),
            settings.as_bytes(),
        )
        .link(
            "word/settings.xml",
            external(
                "rId1",
                "attachedTemplate",
                "file:///D:/Projects/avery/SECRET-template-1.dotx",
            ),
        )
        .link(
            "word/settings.xml",
            external(
                "rId2",
                "mailMergeSource",
                "file:///\\\\fileserver\\share\\SECRET-merge-1.xlsx",
            ),
        )
        .link(
            "word/document.xml",
            external("rId7", "hyperlink", "../Budget/SECRET-link-1.xlsx"),
        )
        .link(
            "word/document.xml",
            external("rId8", "hyperlink", "https://example.com/report"),
        )
        .link(
            "word/document.xml",
            external("rId9", "hyperlink", "mailto:avery@example.com"),
        )
        .link(
            "word/document.xml",
            external("rId10", "image", "\\\\fileserver\\share\\logo.png"),
        )
        .zip();
    let f = check_bytes("links.docx", &doc);
    assert_eq!(
        kinds(&f, "external-links"),
        [
            found(
                "field_path",
                "word/document.xml",
                2,
                "2 fields that read files"
            ),
            found(
                "path_in_alt_text",
                "word/document.xml",
                2,
                "2 alt texts that hold a file path"
            ),
            found(
                "template_path",
                "word/settings.xml",
                1,
                "1 path to the document's template"
            ),
            found(
                "mail_merge_source",
                "word/settings.xml",
                1,
                "1 mail merge data source"
            ),
            found(
                "linked_file",
                "word/document.xml",
                2,
                "2 links to local or network files"
            ),
        ]
    );
    let shown = values("links.docx", &doc);
    assert_eq!(
        shown,
        [
            "INCLUDEPICTURE \"\\\\\\\\fileserver\\\\share\\\\SECRET-field-1.png\" \\* MERGEFORMAT",
            "D:\\Projects\\avery\\SECRET-alt-1.png",
            "file:///D:/Projects/avery/SECRET-template-1.dotx",
            "file:///\\\\fileserver\\share\\SECRET-merge-1.xlsx",
            "../Budget/SECRET-link-1.xlsx",
        ]
    );
}

#[test]
fn workbook_links_connections_and_paths_are_found() {
    let ss = |t: &str| format!("{OX}.spreadsheetml.{t}+xml");
    let link = format!(
        "<externalLink xmlns=\"{S}\" xmlns:r=\"{R}\"><externalBook r:id=\"rId1\"><sheetNames><sheetName val=\"Rates\"/></sheetNames><sheetDataSet><sheetData sheetId=\"0\"><row r=\"1\"><cell r=\"A1\"><v>SECRET-cache-1</v></cell><cell r=\"B1\"><v>2</v></cell></row></sheetData></sheetDataSet></externalBook></externalLink>"
    );
    let connections = format!(
        "<connections xmlns=\"{S}\"><connection id=\"1\" name=\"Query - Clients\" type=\"5\"><dbPr connection=\"Provider=SQLOLEDB;Data Source=SECRET-server-1\" command=\"x\"/></connection><connection id=\"2\" name=\"web\" type=\"4\"><webPr url=\"https://example.com\"/></connection></connections>"
    );
    let book = workbook(&[("Data", "visible", &sheet_xml(""))])
        .body(&format!("<workbook xmlns=\"{S}\" xmlns:r=\"{R}\" xmlns:mc=\"http://schemas.openxmlformats.org/markup-compatibility/2006\" xmlns:x15ac=\"http://schemas.microsoft.com/office/spreadsheetml/2010/11/ac\"><mc:AlternateContent><mc:Choice Requires=\"x15\"><x15ac:absPath url=\"D:\\Projects\\avery\\SECRET-folder-1\\\"/></mc:Choice></mc:AlternateContent><sheets><sheet name=\"Data\" sheetId=\"1\" r:id=\"rId1\"/></sheets><externalReferences><externalReference r:id=\"rId8\"/></externalReferences></workbook>"))
        .part("xl/workbook.xml", "rId8", "externalLink", "xl/externalLinks/externalLink1.xml", &ss("externalLink"), link.as_bytes())
        .link("xl/externalLinks/externalLink1.xml", external("rId1", "externalLinkPath", "file:///D:/Projects/SECRET-book-1.xlsx"))
        .part("xl/workbook.xml", "rId9", "connections", "xl/connections.xml", &ss("connections"), connections.as_bytes())
        .part("xl/worksheets/sheet1.xml", "rId1", "queryTable", "xl/queryTables/queryTable1.xml", &ss("queryTable"), format!("<queryTable xmlns=\"{S}\" name=\"q\" connectionId=\"1\"/>").as_bytes())
        .zip();
    let f = check_bytes("links.xlsx", &book);
    assert_eq!(
        kinds(&f, "external-links"),
        [
            found(
                "external_workbook",
                "xl/externalLinks/externalLink1.xml",
                1,
                "1 linked workbook with 2 cached values"
            ),
            found(
                "data_connection",
                "xl/connections.xml",
                2,
                "2 data connections"
            ),
            found(
                "folder_path",
                "xl/workbook.xml",
                1,
                "1 folder path the workbook was saved in"
            ),
            found(
                "data_connection",
                "xl/queryTables/queryTable1.xml",
                1,
                "1 query table"
            ),
        ]
    );
    let shown: Vec<_> = values("links.xlsx", &book)
        .into_iter()
        .filter(|v| !v.is_empty())
        .collect();
    assert_eq!(
        shown,
        [
            "file:///D:/Projects/SECRET-book-1.xlsx",
            "Provider=SQLOLEDB;Data Source=SECRET-server-1",
            "D:\\Projects\\avery\\SECRET-folder-1\\",
        ]
    );
}

#[test]
fn the_path_rule_tells_files_from_web_links() {
    for path in [
        "file:///D:/a.docx",
        "FILE://server/x",
        "\\\\fileserver\\share\\a.xlsx",
        "//fileserver/share/a",
        "D:\\Projects\\a.png",
        "c:/x",
        "/Users/avery/a",
        "/home/avery/a",
        "/Volumes/Disk/a",
    ] {
        assert!(doc_sanitize::office::common::is_path(path, false), "{path}");
    }
    for not in [
        "https://example.com/a",
        "mailto:a@example.com",
        "ftp://x",
        "tel:123",
        "#bookmark",
        "Budget.xlsx",
        "a photo",
        "",
        "///x",
    ] {
        assert!(!doc_sanitize::office::common::is_path(not, false), "{not}");
    }
    assert!(doc_sanitize::office::common::is_path(
        "../Budget.xlsx",
        true
    ));
    assert!(doc_sanitize::office::common::is_path("Budget.xlsx", true));
    assert!(!doc_sanitize::office::common::is_path(
        "https://example.com",
        true
    ));
    assert!(!doc_sanitize::office::common::is_path("#Sheet2!A1", true));
}

#[test]
fn picture_metadata_and_crops_are_found() {
    let body = blip(
        "rId1",
        "<a:srcRect l=\"10000\" r=\"20000\" t=\"0\" b=\"20000\"/>",
    ) + &blip("rId2", "<a:srcRect l=\"500\"/>")
        + &blip("rId3", "<a:srcRect l=\"-5000\" t=\"40000\"/>")
        + &blip("rId2", "");
    let doc = picture_doc(
        &[
            ("word/media/image1.jpeg", &jpeg(&[0x0112, 0x8825], b"")),
            ("word/media/image2.jpeg", &jpeg(&[0x0112], b"")),
            (
                "word/media/image3.jpeg",
                &jpeg(&[0x0112], b"SECRET-trailing-1"),
            ),
        ],
        &body,
    );
    let f = check_bytes("pictures.docx", &doc);
    assert_eq!(
        kinds(&f, "image-metadata"),
        [
            found(
                "picture_metadata",
                "word/media/image1.jpeg",
                1,
                "1 picture with EXIF and GPS"
            ),
            found(
                "picture_metadata",
                "word/media/image3.jpeg",
                1,
                "1 picture with trailing bytes"
            ),
        ]
    );
    assert_eq!(
        kinds(&f, "cropped-images"),
        [found(
            "cropped_picture",
            "word/document.xml",
            2,
            "2 pictures with up to 44% cut away by cropping"
        )]
    );
    let cut = jpeg(&[0x0112], b"");
    let broken = picture_doc(&[("word/media/image1.jpeg", &cut[..cut.len() - 2])], "");
    let f = check_bytes("broken.docx", &broken);
    assert_eq!(f["error"]["code"], "damaged");
    assert_eq!(
        f["error"]["message"],
        "broken.docx is damaged: picture word/media/image1.jpeg is cut short."
    );
}

#[test]
fn pivot_caches_and_data_models_are_found() {
    let ss = |t: &str| format!("{OX}.spreadsheetml.{t}+xml");
    let def = format!(
        "<pivotCacheDefinition xmlns=\"{S}\" xmlns:r=\"{R}\" r:id=\"rId1\" recordCount=\"9\"><cacheSource type=\"worksheet\"><worksheetSource ref=\"A1:B3\" sheet=\"Deleted\"/></cacheSource><cacheFields count=\"1\"><cacheField name=\"Name\"><sharedItems><s v=\"SECRET-pivot-1\"/><s v=\"Sam\"/></sharedItems></cacheField></cacheFields></pivotCacheDefinition>"
    );
    let records = format!(
        "<pivotCacheRecords xmlns=\"{S}\" count=\"2\"><r><x v=\"0\"/></r><r><x v=\"1\"/></r></pivotCacheRecords>"
    );
    let book = workbook(&[("Data", "visible", &sheet_xml(""))])
        .part(
            "xl/workbook.xml",
            "rId8",
            "pivotCacheDefinition",
            "xl/pivotCache/pivotCacheDefinition1.xml",
            &ss("pivotCacheDefinition"),
            def.as_bytes(),
        )
        .part(
            "xl/pivotCache/pivotCacheDefinition1.xml",
            "rId1",
            "pivotCacheRecords",
            "xl/pivotCache/pivotCacheRecords1.xml",
            &ss("pivotCacheRecords"),
            records.as_bytes(),
        )
        .part(
            "xl/workbook.xml",
            "rId9",
            "http://schemas.microsoft.com/office/2007/relationships/powerPivotData",
            "xl/model/item.data",
            "application/vnd.openxmlformats-officedocument.model+data",
            b"\0SECRET-model-1",
        )
        .zip();
    let f = check_bytes("pivot.xlsx", &book);
    assert_eq!(
        kinds(&f, "pivot-caches"),
        [
            found(
                "pivot_cache",
                "xl/pivotCache/pivotCacheRecords1.xml",
                1,
                "1 pivot cache with 2 source rows"
            ),
            found("data_model", "xl/model/item.data", 1, "1 data model"),
        ]
    );
    assert_eq!(values("pivot.xlsx", &book), ["SECRET-pivot-1", ""]);
}

/// Each (check, kind) a file's findings name, once.
fn found_kinds(f: &serde_json::Value) -> std::collections::BTreeSet<(String, String)> {
    let s = |v: &serde_json::Value| v.as_str().unwrap_or_default().to_string();
    f["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| (s(&x["check"]), s(&x["kind"])))
        .collect()
}

fn pairs(list: &[(&str, &str)]) -> std::collections::BTreeSet<(String, String)> {
    list.iter()
        .map(|(c, k)| (c.to_string(), k.to_string()))
        .collect()
}

const DOCX_LEAKS: [(&str, &str); 25] = [
    ("metadata", "core_properties"),
    ("metadata", "app_properties"),
    ("metadata", "custom_properties"),
    ("metadata", "thumbnail"),
    ("metadata", "tracking_ids"),
    ("metadata", "editor_names"),
    ("comments", "comment"),
    ("comments", "people"),
    ("tracked-changes", "insertion"),
    ("tracked-changes", "deletion"),
    ("tracked-changes", "move"),
    ("tracked-changes", "table_change"),
    ("hidden-text", "hidden_formatting"),
    ("hidden-text", "same_colour"),
    ("fake-redactions", "same_colour"),
    ("embedded-files", "embedded_object"),
    ("custom-data", "data_binding"),
    ("custom-data", "document_variables"),
    ("custom-data", "custom_xml"),
    ("external-links", "template_path"),
    ("external-links", "mail_merge_source"),
    ("external-links", "field_path"),
    ("image-metadata", "picture_metadata"),
    ("cropped-images", "cropped_picture"),
    ("leftover-data", "glossary"),
];

const XLSX_LEAKS: [(&str, &str); 20] = [
    ("metadata", "core_properties"),
    ("metadata", "app_properties"),
    ("metadata", "printer_settings"),
    ("metadata", "editor_names"),
    ("comments", "note"),
    ("comments", "threaded_comment"),
    ("comments", "people"),
    ("hidden-text", "hidden_format"),
    ("hidden-text", "same_colour"),
    ("fake-redactions", "same_colour"),
    ("hidden-sheets", "hidden_sheet"),
    ("hidden-rows-columns", "hidden_rows"),
    ("hidden-rows-columns", "hidden_columns"),
    ("custom-data", "scenarios"),
    ("macros", "macro_sheet"),
    ("external-links", "external_workbook"),
    ("external-links", "data_connection"),
    ("external-links", "folder_path"),
    ("pivot-caches", "pivot_cache"),
    ("leftover-data", "unused_strings"),
];

const PPTX_LEAKS: [(&str, &str); 14] = [
    ("metadata", "core_properties"),
    ("metadata", "app_properties"),
    ("metadata", "tracking_ids"),
    ("comments", "comment"),
    ("comments", "people"),
    ("hidden-slides", "hidden_slide"),
    ("speaker-notes", "notes"),
    ("hidden-objects", "hidden_object"),
    ("hidden-objects", "off_slide_object"),
    ("embedded-files", "embedded_object"),
    ("embedded-files", "chart_workbook"),
    ("custom-data", "tags"),
    ("custom-data", "add_in_data"),
    ("cropped-images", "cropped_picture"),
];

#[test]
fn the_leak_fixtures_show_every_planted_leak() {
    let docx = check_bytes("leaky.docx", &leaky_docx("docx"));
    assert_eq!(found_kinds(&docx), pairs(&DOCX_LEAKS), "{docx}");
    let changes = kinds(&docx, "tracked-changes");
    let counts: Vec<_> = changes.iter().map(|c| (c.0.as_str(), c.2)).collect();
    assert_eq!(
        counts,
        [
            ("insertion", 1),
            ("deletion", 2),
            ("move", 2),
            ("table_change", 2)
        ]
    );
    assert!(
        changes.iter().all(|c| c.3.contains("by 1 author")),
        "{changes:?}"
    );
    assert_eq!(kinds(&docx, "hidden-text")[0].2, 40);

    let xlsx = check_bytes("leaky.xlsx", &leaky_xlsx("xlsx"));
    assert_eq!(found_kinds(&xlsx), pairs(&XLSX_LEAKS), "{xlsx}");
    let sheets: Vec<_> = findings_of(&xlsx, "hidden-sheets")
        .iter()
        .map(|x| x["where"]["sheet_index"].clone())
        .collect();
    assert_eq!(sheets, [2, 3, 4]);
    let rows = findings_of(&xlsx, "hidden-rows-columns");
    assert_eq!(
        rows[0]["where"],
        serde_json::json!({ "part": "xl/worksheets/sheet1.xml", "sheet_index": 1, "sheet": "Summary", "cells": "A5:A6" })
    );
    assert_eq!(
        (rows[0]["count"].clone(), rows[1]["count"].clone()),
        (2.into(), 1.into())
    );

    let pptx = check_bytes("leaky.pptx", &leaky_pptx("pptx"));
    assert_eq!(found_kinds(&pptx), pairs(&PPTX_LEAKS), "{pptx}");
    let slides: Vec<_> = ["hidden-slides", "speaker-notes", "hidden-objects"]
        .iter()
        .flat_map(|c| findings_of(&pptx, c))
        .map(|x| x["where"]["slide"].clone())
        .collect();
    assert_eq!(slides, [2, 1, 1, 1]);
}

#[test]
fn the_twins_find_nothing() {
    for (name, bytes) in [
        ("twin.docx", twin_docx()),
        ("twin.xlsx", twin_xlsx()),
        ("twin.pptx", twin_pptx()),
    ] {
        let f = check_bytes(name, &bytes);
        assert_eq!(f["status"], "none_found", "{name}: {f}");
        assert!(
            f["checks"]
                .as_array()
                .unwrap()
                .iter()
                .all(|c| c["status"] == "none_found")
        );
    }
}

#[test]
fn planted_values_show_only_when_asked() {
    for (name, bytes, secrets) in [
        (
            "leaky.docx",
            leaky_docx("docx"),
            &[
                "docx-comment-1",
                "docx-ins-1",
                "docx-del-1",
                "docx-move-1",
                "docx-vanish-1",
                "docx-white-1",
                "docx-black-1",
                "docx-var-1",
                "docx-xml-1",
                "docx-template-1",
                "docx-merge-1",
                "docx-field-1",
                "docx-ed-1",
                "docx-company-1",
                "docx-custom-1",
            ][..],
        ),
        (
            "leaky.xlsx",
            leaky_xlsx("xlsx"),
            &[
                "xlsx-note-1",
                "xlsx-thread-1",
                "xlsx-format-1",
                "xlsx-white-1",
                "xlsx-black-1",
                "xlsx-row-1",
                "xlsx-col-1",
                "xlsx-scenario-1",
                "xlsx-book-1",
                "xlsx-dsn-1",
                "xlsx-folder-1",
                "xlsx-pivot-1",
                "xlsx-unused-1",
                "xlsx-user-1",
            ][..],
        ),
        (
            "leaky.pptx",
            leaky_pptx("pptx"),
            &[
                "pptx-slide-1",
                "pptx-notes-1",
                "pptx-comment-1",
                "pptx-comment-2",
                "pptx-shape-1",
                "pptx-offslide-1",
                "pptx-tag-1",
                "pptx-addin-1",
            ][..],
        ),
    ] {
        let shown = values(name, &bytes).join("\n");
        for s in secrets {
            assert!(
                shown.contains(&format!("SECRET-{s}")),
                "{name}: SECRET-{s} not in\n{shown}"
            );
        }
    }
}

#[test]
fn macro_variants_report_their_vba_project() {
    for (name, bytes, part) in [
        ("macros.docm", leaky_docx("docm"), "word/vbaProject.bin"),
        ("macros.xlsm", leaky_xlsx("xlsm"), "xl/vbaProject.bin"),
        ("macros.pptm", leaky_pptx("pptm"), "ppt/vbaProject.bin"),
    ] {
        let f = check_bytes(name, &bytes);
        let vba: Vec<_> = kinds(&f, "macros")
            .into_iter()
            .filter(|k| k.0 == "vba_project")
            .collect();
        assert_eq!(
            vba,
            [found("vba_project", part, 1, "1 VBA macro project")],
            "{name}"
        );
        assert_eq!(f["warnings"], serde_json::json!([]), "{name}");
    }
}

/// `ledger.xlsx` (spec §9): 1,000,000 cells over 4 sheets, a tenth of them shared strings,
/// with formulas and one hidden column.
fn ledger() -> Vec<u8> {
    use std::fmt::Write;
    let mut sheets = vec![];
    for s in 0..4 {
        let mut data = String::with_capacity(8 << 20);
        for row in 1..=25_000u32 {
            write!(data, "<row r=\"{row}\">").unwrap();
            for (c, col) in "ABCDEFGHIJ".chars().enumerate() {
                match c {
                    0 => write!(
                        data,
                        "<c r=\"{col}{row}\" t=\"s\"><v>{}</v></c>",
                        row % 1000
                    ),
                    9 => write!(
                        data,
                        "<c r=\"{col}{row}\"><f>SUM(B{row}:I{row})</f><v>{}</v></c>",
                        row * 8
                    ),
                    _ => write!(data, "<c r=\"{col}{row}\"><v>{}</v></c>", row + s),
                }
                .unwrap();
            }
            data += "</row>";
        }
        let cols =
            "<cols><col min=\"5\" max=\"5\" width=\"9\" hidden=\"1\" customWidth=\"1\"/></cols>";
        sheets.push(format!(
            "<worksheet xmlns=\"{S}\">{cols}<sheetData>{data}</sheetData></worksheet>"
        ));
    }
    let named: Vec<_> = sheets
        .iter()
        .enumerate()
        .map(|(i, b)| (format!("Sheet{}", i + 1), b))
        .collect();
    let list: Vec<_> = named
        .iter()
        .map(|(n, b)| (n.as_str(), "visible", b.as_str()))
        .collect();
    let sst: String = (0..1000)
        .map(|i| format!("<si><t>Account {i}</t></si>"))
        .collect();
    workbook(&list)
        .part(
            "xl/workbook.xml",
            "rId9",
            "sharedStrings",
            "xl/sharedStrings.xml",
            &format!("{OX}.spreadsheetml.sharedStrings+xml"),
            format!("<sst xmlns=\"{S}\">{sst}</sst>").as_bytes(),
        )
        .zip()
}

/// A real `side` × `side` JPEG of noise with EXIF and GPS: about a byte per pixel, like a photo.
fn noisy_photo(seed: u32, side: u32) -> Vec<u8> {
    let mut x = seed.wrapping_mul(2_654_435_761) | 1;
    let image = image::RgbImage::from_fn(side, side, |_, _| {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        let [r, g, b, _] = x.to_le_bytes();
        image::Rgb([r, g, b])
    });
    let mut out = vec![];
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 60)
        .encode_image(&image)
        .unwrap();
    let exif = jpeg(&[0x0112, 0x8825], b"");
    [&out[..2], &exif[2..exif.len() - 8], &out[2..]].concat()
}

/// `memo.docx` (spec §9): about 60 KB of text.
fn memo() -> Vec<u8> {
    let words = [
        "fee", "term", "party", "notice", "clause", "payment", "date", "scope",
    ];
    let mut x = 7u32;
    let mut body = String::new();
    for _ in 0..180 {
        let mut text = String::new();
        for _ in 0..24 {
            x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            text += words[(x >> 28) as usize % 8];
            text += &format!(" {} ", x >> 20);
        }
        body += &para(&text_run("", &text));
    }
    Pkg::new("docx").body(&doc_xml(&body)).zip()
}

/// `photos.docx` (spec §9): 12 MB, 20 JPEGs with EXIF.
fn photos() -> Vec<u8> {
    let media: Vec<_> = (0..20)
        .map(|i| (format!("image{i}.jpeg"), noisy_photo(i + 1, 800)))
        .collect();
    let list: Vec<_> = media.iter().map(|(n, b)| (n.as_str(), &b[..])).collect();
    let body: String = (1..=20)
        .map(|i| format!("<w:p>{}</w:p>", drawing(&format!("rId{i}"), "", "Photo")))
        .collect();
    picture_doc(&list, &body)
}

/// `deck.pptx` (spec §9): 200 slides, 60 MB of pictures.
fn deck() -> Vec<u8> {
    let slide = slide_xml(&format!(
        "<p:pic><p:nvPicPr><p:cNvPr id=\"2\" name=\"Picture\"/><p:cNvPicPr/><p:nvPr/></p:nvPicPr><p:blipFill><a:blip r:embed=\"rId1\"/><a:stretch><a:fillRect/></a:stretch></p:blipFill><p:spPr>{}<a:prstGeom prst=\"rect\"/></p:spPr></p:pic>",
        xfrm(0, 0, 3_000_000, 3_000_000)
    ));
    let mut p = presentation(&vec![slide.as_str(); 200]);
    for i in 1..=200 {
        p = p.part(
            &format!("ppt/slides/slide{i}.xml"),
            "rId1",
            "image",
            &format!("ppt/media/image{i}.jpeg"),
            "",
            &noisy_photo(i, 560),
        );
    }
    p.zip()
}

/// Writes the leak fixtures, their twins and the spec §9 inputs to `$DOC_SANITIZE_FIXTURES` (or
/// a folder under the test target), for timing and for opening in Office.
#[test]
#[ignore = "writes files for measuring by hand"]
fn write_fixtures() {
    let out = std::env::var_os("DOC_SANITIZE_FIXTURES")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| dir("fixtures"));
    std::fs::create_dir_all(&out).unwrap();
    for (name, bytes) in [
        ("leaky.docx", leaky_docx("docx")),
        ("leaky.xlsx", leaky_xlsx("xlsx")),
        ("leaky.pptx", leaky_pptx("pptx")),
        ("macros.docm", leaky_docx("docm")),
        ("macros.xlsm", leaky_xlsx("xlsm")),
        ("macros.pptm", leaky_pptx("pptm")),
        ("twin.docx", twin_docx()),
        ("twin.xlsx", twin_xlsx()),
        ("twin.pptx", twin_pptx()),
        ("ledger.xlsx", ledger()),
        ("memo.docx", memo()),
        ("photos.docx", photos()),
        ("deck.pptx", deck()),
    ] {
        std::fs::write(out.join(name), bytes).unwrap();
    }
    println!("{}", out.display());
}
