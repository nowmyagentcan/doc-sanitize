//! Clean: copies written beside the input or where asked, never over the input, re-checked
//! before they are kept, and leftovers swept.

mod common;

use common::*;
use std::fs;

fn twin() -> Vec<u8> {
    zip(&package("docx", &[], &[]))
}

#[test]
fn a_copy_is_written_beside_the_input_and_the_input_is_left_alone() {
    let dir = dir("clean-beside");
    put(&dir, &[("a.docx", &twin())]);
    let input = dir.join("a.docx");
    let stamp = |p: &std::path::Path| {
        (
            fs::read(p).unwrap(),
            fs::metadata(p).unwrap().modified().unwrap(),
        )
    };
    let before = stamp(&input);
    let out = run_in(&dir, &["clean", "a.docx"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(
        stdout(&out),
        "a.docx → a.clean.docx\n  ✓ re-checked: 14 checks, nothing left\n"
    );
    let out = run_in(&dir, &["clean", "a.docx", "--json"]);
    let j = json(&out);
    assert_eq!(j["command"], "clean");
    let f = &j["files"][0];
    let text = stdout(&out);
    let at = |k: &str| {
        text.find(&format!(
            "
      \"{k}\": "
        ))
        .unwrap()
    };
    let keys = [
        "path", "format", "variant", "output", "status", "removed", "kept", "changes", "recheck",
        "warnings", "error",
    ];
    assert!(keys.windows(2).all(|k| at(k[0]) < at(k[1])), "{text}");
    assert_eq!(f["output"], "a.clean-2.docx");
    assert_eq!(f["status"], "cleaned");
    assert_eq!(f["recheck"]["status"], "none_found");
    assert_eq!(f["recheck"]["checks"].as_array().unwrap().len(), 14);
    assert_eq!(
        j["summary"],
        serde_json::json!({ "files": 1, "cleaned": 1, "cleaned_incomplete": 0, "not_cleaned": 0 })
    );
    assert!(stamp(&input) == before, "the input changed");
    // Copies are never picked up as inputs.
    let out = run_in(&dir, &["clean", ".", "--json"]);
    assert_eq!(json(&out)["files"].as_array().unwrap().len(), 1);
    assert!(dir.join("a.clean-3.docx").exists());
}

#[test]
fn the_copy_is_a_fresh_zip_with_fixed_times() {
    let dir = dir("clean-zip");
    let mut parts = package("docx", &[], &[]);
    parts.rotate_left(1);
    put(&dir, &[("a.docx", &zip(&parts))]);
    let out = run_in(&dir, &["clean", "a.docx", "-o", "b.docx"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let copy = fs::read(dir.join("b.docx")).unwrap();
    assert_eq!(unzip(&copy)[0].0, "[Content_Types].xml");
    // Each local header: method 8, time 0, date 1980-01-01 and no extra field.
    let (mut at, mut n) = (0, 0);
    while copy[at..].starts_with(b"PK\x03\x04") {
        let u16_at = |i: usize| u16::from_le_bytes([copy[at + i], copy[at + i + 1]]);
        let size = u32::from_le_bytes(copy[at + 18..at + 22].try_into().unwrap()) as usize;
        assert_eq!((u16_at(8), u16_at(10), u16_at(12)), (8, 0, 0x21));
        assert_eq!(u16_at(28), 0, "extra field");
        at += 30 + usize::from(u16_at(26)) + size;
        n += 1;
    }
    assert_eq!(n, unzip(&copy).len());
    assert_eq!(&copy[copy.len() - 2..], [0, 0], "archive comment");
}

#[test]
fn outputs_go_where_asked_and_never_over_a_file_unasked() {
    let dir = dir("clean-where");
    put(
        &dir,
        &[
            ("a.docx", &twin()),
            ("b.docx", &twin()),
            ("taken.docx", b"x"),
            ("sub/a.docx", &twin()),
        ],
    );
    let out = run_in(&dir, &["clean", "a.docx", "-o", "taken.docx", "--json"]);
    assert_eq!(out.status.code(), Some(1));
    let f = &json(&out)["files"][0];
    assert_eq!(f["status"], "not_cleaned");
    assert_eq!(f["output"], serde_json::Value::Null);
    assert_eq!(f["error"]["code"], "output_exists");
    assert_eq!(f["error"]["message"], "taken.docx already exists.");
    assert_eq!(fs::read(dir.join("taken.docx")).unwrap(), b"x");
    let out = run_in(
        &dir,
        &["clean", "a.docx", "-o", "taken.docx", "--overwrite"],
    );
    assert_eq!(out.status.code(), Some(0));
    assert!(fs::read(dir.join("taken.docx")).unwrap().starts_with(b"PK"));
    let args = [
        "clean",
        "a.docx",
        "sub/a.docx",
        "--out-dir",
        "outbox",
        "--json",
    ];
    let out = run_in(&dir, &args);
    let outputs: Vec<_> = json(&out)["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["output"].clone())
        .collect();
    let sep = std::path::MAIN_SEPARATOR;
    assert_eq!(
        outputs,
        [
            format!("outbox{sep}a.clean.docx"),
            format!("outbox{sep}a.clean-2.docx")
        ]
    );
    let text = stdout(&run_in(
        &dir,
        &["clean", "a.docx", "b.docx", "--out-dir", "outbox"],
    ));
    assert!(text.ends_with("\n\n2 of 2 cleaned\n"), "{text}");
    let out = run_in(&dir, &["clean", "a.docx", "missing.docx"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stdout(&out).ends_with("\n\n1 of 2 cleaned · 1 not cleaned\n"));
    assert_eq!(
        stderr(&out),
        "\n✗ Can't find missing.docx.\n  Check the path.\n"
    );
}

#[test]
fn bad_clean_options_are_usage_errors() {
    let dir = dir("clean-usage");
    put(&dir, &[("a.docx", &twin()), ("b.docx", &twin())]);
    let ids = "Checks: metadata, comments, tracked-changes, hidden-text, fake-redactions, hidden-layers, hidden-sheets, hidden-rows-columns, hidden-slides, speaker-notes, hidden-objects, embedded-files, custom-data, macros, external-links, image-metadata, cropped-images, pivot-caches, old-revisions, leftover-data, signatures, form-data.";
    let cases: [(&[&str], &str, &str); 8] = [
        (
            &["clean", "a.docx", "--keep", "comments,nope"],
            "Unknown check \"nope\".",
            ids,
        ),
        (
            &["clean", "a.docx", "--keep", "signatures"],
            "Signatures can't be kept: any change to a signed file breaks its signature.",
            "Leave signatures out of --keep.",
        ),
        (
            &[
                "clean",
                "a.docx",
                "--keep",
                "comments",
                "--keep",
                "old-revisions",
            ],
            "Old revisions can't be kept: a cleaned PDF is always written as one revision.",
            "Leave old-revisions out of --keep.",
        ),
        (
            &["clean", "a.docx", "b.docx", "-o", "c.docx"],
            "-o works with one file; you gave 2.",
            "Use --out-dir for several files.",
        ),
        (
            &["clean", "a.docx", "-o", "a.docx"],
            "The output would replace the input file.",
            "Choose another name with -o.",
        ),
        (
            &["clean", "a.docx", "-o", "./a.docx"],
            "The output would replace the input file.",
            "Choose another name with -o.",
        ),
        (
            &["a.docx", "--keep", "comments"],
            "--keep works only with clean.",
            "See doc-sanitize --help.",
        ),
        (
            &["check", "a.docx", "-o", "c.docx"],
            "-o works only with clean.",
            "See doc-sanitize --help.",
        ),
    ];
    for (args, message, hint) in cases {
        let out = run_in(&dir, &[args, &["--json"]].concat());
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        let e = &json(&out)["error"];
        let got = (e["message"].as_str(), e["hint"].as_str());
        assert_eq!(got, (Some(message), Some(hint)), "{args:?}");
    }
    let out = run_in(&dir, &["clean", "a.docx", "-o", "c.docx", "--out-dir", "d"]);
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(fs::read_dir(&dir).unwrap().count(), 2);
}

/// A Word file with every Office leftover: an orphan part, an untyped entry, a glossary, a
/// ZIP comment and bytes before the package.
fn leftovers_docx() -> Vec<u8> {
    let glossary =
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document.glossary+xml";
    let body = format!(
        "<w:glossaryDocument xmlns:w=\"{W}\"><w:docParts><w:docPart><w:docPartBody><w:p><w:r><w:t>SECRET-left-glossary-1</w:t></w:r></w:p></w:docPartBody></w:docPart></w:docParts></w:glossaryDocument>"
    );
    let parts = package(
        "docx",
        &[
            ("word/glossary/document.xml", glossary),
            ("word/old.xml", "application/xml"),
        ],
        &[
            entry(
                "word/_rels/document.xml.rels",
                rels(&[("rId9", "glossaryDocument", "glossary/document.xml")]).as_bytes(),
            ),
            entry("word/glossary/document.xml", body.as_bytes()),
            entry("word/old.xml", b"<old>SECRET-left-orphan-1</old>"),
            entry("word/notes.bin", b"SECRET-left-untyped-1"),
        ],
    );
    let mut bytes = zip(&parts);
    let comment = b"SECRET-left-comment-1";
    let n = bytes.len();
    bytes[n - 2..].copy_from_slice(&(comment.len() as u16).to_le_bytes());
    bytes.extend(comment);
    [b"SECRET-left-before-1".to_vec(), bytes].concat()
}

#[test]
fn leftovers_are_swept_unless_kept() {
    let dir = dir("clean-leftovers");
    let input = leftovers_docx();
    assert_eq!(secrets_in(&input).len(), 5);
    put(&dir, &[("a.docx", &input)]);
    let out = run_in(&dir, &["clean", "a.docx", "--json"]);
    assert_eq!(out.status.code(), Some(0), "{}", stdout(&out));
    let f = &json(&out)["files"][0];
    assert_eq!(
        f["removed"],
        serde_json::json!([
            { "check": "leftover-data", "count": 3, "unit": "parts", "detail": "3 parts removed" },
            { "check": "leftover-data", "count": 41, "unit": "bytes", "detail": "41 bytes removed" }
        ])
    );
    let copy = fs::read(dir.join("a.clean.docx")).unwrap();
    let names: Vec<_> = unzip(&copy).into_iter().map(|e| e.0).collect();
    assert_eq!(
        names,
        [
            "[Content_Types].xml",
            "_rels/.rels",
            "word/document.xml",
            "word/_rels/document.xml.rels"
        ]
    );
    assert_eq!(secrets_in(&copy), Vec::<String>::new());
    for (_, data) in unzip(&copy) {
        assert_eq!(secrets_in(&data), Vec::<String>::new());
    }
    let check = json(&run_in(&dir, &["a.clean.docx", "--json"]));
    assert_eq!(check["files"][0]["status"], "none_found");
    // Kept, the parts stay; outside bytes and the comment can't survive a new ZIP.
    let out = run_in(
        &dir,
        &["clean", "a.docx", "--keep", "leftover-data", "--json"],
    );
    let f = &json(&out)["files"][0];
    assert_eq!(f["kept"], serde_json::json!(["leftover-data"]));
    assert_eq!(f["removed"], serde_json::json!([]));
    let copy = fs::read(dir.join("a.clean-2.docx")).unwrap();
    let inside: Vec<_> = unzip(&copy)
        .into_iter()
        .flat_map(|e| secrets_in(&e.1))
        .collect();
    assert_eq!(
        inside,
        [
            "SECRET-left-glossary-1",
            "SECRET-left-orphan-1",
            "SECRET-left-untyped-1"
        ]
    );
    let kept: Vec<_> = f["recheck"]["checks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["status"] == "kept")
        .map(|c| c["id"].clone())
        .collect();
    assert_eq!(kept, ["leftover-data"]);
    let text = stdout(&run_in(
        &dir,
        &["clean", "a.docx", "--keep", "leftover-data"],
    ));
    assert!(
        text.contains(
            "\n  kept     leftover-data\n  ✓ re-checked: 14 checks, nothing left (1 kept)\n"
        ),
        "{text}"
    );
}

#[test]
fn unused_shared_strings_are_emptied() {
    let sheet = format!(
        "<worksheet xmlns=\"{S}\"><sheetData><row r=\"1\"><c r=\"A1\" t=\"s\"><v>2</v></c><c r=\"B1\" t=\"s\"><v>0</v></c><c r=\"C1\"><v>0</v></c></row></sheetData></worksheet>"
    );
    let sst = format!(
        "<sst xmlns=\"{S}\" count=\"2\" uniqueCount=\"3\"><si><t>first</t></si><si><t>SECRET-sst-unused-1</t></si><si><t>third</t></si></sst>"
    );
    let ct = "application/vnd.openxmlformats-officedocument.spreadsheetml.sharedStrings+xml";
    let book = workbook(&[("Sheet1", "visible", &sheet)])
        .part(
            "xl/workbook.xml",
            "rId9",
            "sharedStrings",
            "xl/sharedStrings.xml",
            ct,
            sst.as_bytes(),
        )
        .zip();
    let dir = dir("clean-strings");
    put(&dir, &[("a.xlsx", &book)]);
    let out = run_in(&dir, &["clean", "a.xlsx"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(stdout(&out).contains("  removed  leftover-data (1 string)\n"));
    assert_eq!(
        String::from_utf8(part_of(&dir.join("a.clean.xlsx"), "xl/sharedStrings.xml")).unwrap(),
        format!(
            "<sst xmlns=\"{S}\" count=\"2\" uniqueCount=\"3\"><si><t>first</t></si><si><t/></si><si><t>third</t></si></sst>"
        )
    );
    // The cells keep their numbers, so no sheet is rewritten.
    let sheet = String::from_utf8(part_of(
        &dir.join("a.clean.xlsx"),
        "xl/worksheets/sheet1.xml",
    ));
    assert!(
        sheet
            .unwrap()
            .contains("<c r=\"A1\" t=\"s\"><v>2</v></c><c r=\"B1\" t=\"s\"><v>0</v></c>")
    );
}

/// The planted secrets left in a copy: in its bytes and in every inflated entry.
fn copy_secrets(path: &std::path::Path) -> std::collections::BTreeSet<String> {
    let bytes = fs::read(path).unwrap();
    let mut out: std::collections::BTreeSet<_> = secrets_in(&bytes).into_iter().collect();
    for (_, data) in unzip(&bytes) {
        out.extend(secrets_in(&data));
    }
    out
}

#[test]
fn properties_comments_custom_data_and_macros_are_removed() {
    let docx_keep = "tracked-changes,hidden-text,fake-redactions,embedded-files,external-links";
    let pptx_keep = "hidden-slides,speaker-notes,hidden-objects,embedded-files,external-links";
    let cases = [
        (
            "leaky.docm",
            leaky_docx("docm"),
            docx_keep,
            &[
                "docx-comment-1",
                "docx-ed-1",
                "docx-company-1",
                "docx-custom-1",
                "docx-var-1",
                "docx-xml-1",
                "docm-vba-1",
            ][..],
            "docx",
        ),
        (
            "leaky.pptm",
            leaky_pptx("pptm"),
            pptx_keep,
            &[
                "pptx-comment-1",
                "pptx-comment-2",
                "pptx-tag-1",
                "pptx-addin-1",
                "pptm-vba-1",
            ][..],
            "pptx",
        ),
    ];
    for (name, bytes, keep, gone, ext) in cases {
        let dir = dir("clean-parts");
        put(&dir, &[(name, &bytes)]);
        let out = run_in(&dir, &["clean", name, "--keep", keep, "--json"]);
        assert_eq!(out.status.code(), Some(0), "{name}: {}", stdout(&out));
        let f = &json(&out)["files"][0];
        let stem = name.split('.').next().unwrap();
        assert_eq!(f["output"], format!("{stem}.clean.{ext}"));
        assert_eq!(f["changes"][0]["code"], "extension_changed");
        let copy = dir.join(format!("{stem}.clean.{ext}"));
        let left = copy_secrets(&copy);
        for s in gone {
            assert!(
                !left.contains(&format!("SECRET-{s}")),
                "{name}: SECRET-{s} left"
            );
        }
        let check = json(&run_in(&dir, &[copy.to_str().unwrap(), "--json"]));
        assert_eq!(check["files"][0]["variant"], ext, "{name}");
        assert_eq!(check["files"][0]["warnings"], serde_json::json!([]));
        for id in [
            "metadata",
            "comments",
            "custom-data",
            "macros",
            "leftover-data",
            "signatures",
        ] {
            let c = check["files"][0]["checks"]
                .as_array()
                .unwrap()
                .iter()
                .find(|c| c["id"] == id)
                .unwrap();
            assert_eq!(c["status"], "none_found", "{name}: {id}");
        }
    }
}

/// A macro document with a VBA project, an ActiveX control in the text, a custom ribbon, a
/// package signature and document variables.
fn macro_docm() -> Vec<u8> {
    let body = "<w:p><w:r><w:object><w:control r:id=\"rId6\" w:name=\"CheckBox1\"/></w:object></w:r></w:p>";
    let settings = format!(
        "<w:settings xmlns:w=\"{W}\"><w:docVars><w:docVar w:name=\"Client\" w:val=\"SECRET-mac-var-1\"/></w:docVars></w:settings>"
    );
    let ms = "http://schemas.microsoft.com/office";
    Pkg::new("docm")
        .body(&doc_xml(body))
        .part("word/document.xml", "rId1", "settings", "word/settings.xml", &format!("{OX}.wordprocessingml.settings+xml"), settings.as_bytes())
        .part("word/document.xml", "rId5", &format!("{ms}/2006/relationships/vbaProject"), "word/vbaProject.bin", "application/vnd.ms-office.vbaProject", b"\xCC\x61SECRET-mac-vba-1")
        .part("word/vbaProject.bin", "rId1", &format!("{ms}/2006/relationships/wordVbaData"), "word/vbaData.xml", "application/vnd.ms-word.vbaData+xml", b"<wne:vbaSuppData xmlns:wne=\"http://schemas.microsoft.com/office/word/2006/wordml\"><wne:mcds><wne:mcd wne:macroName=\"SECRET-mac-name-1\"/></wne:mcds></wne:vbaSuppData>")
        .part("word/document.xml", "rId6", "control", "word/activeX/activeX1.xml", "application/vnd.ms-office.activeX+xml", b"<ax:ocx xmlns:ax=\"http://schemas.microsoft.com/office/2006/activeX\" ax:classid=\"{8BD21D40}\" ax:name=\"SECRET-mac-ax-1\"/>")
        .part("", "rId7", &format!("{ms}/2007/relationships/ui/extensibility"), "customUI/customUI14.xml", "", b"<customUI xmlns=\"http://schemas.microsoft.com/office/2009/07/customui\" onLoad=\"SECRET-mac-ui-1\"/>")
        .part("", "rId8", &format!("{PKG_RELS}/digital-signature/origin"), "_xmlsignatures/origin.sigs", "application/vnd.openxmlformats-package.digital-signature-origin", b"")
        .part("_xmlsignatures/origin.sigs", "rId1", &format!("{PKG_RELS}/digital-signature/signature"), "_xmlsignatures/sig1.xml", "application/vnd.openxmlformats-package.digital-signature-xmlsignature+xml", b"<Signature xmlns=\"http://www.w3.org/2000/09/xmldsig#\">SECRET-mac-sig-1</Signature>")
        .zip()
}

#[test]
fn macros_go_with_the_macro_type_unless_kept() {
    let dir = dir("clean-macros");
    put(&dir, &[("m.docm", &macro_docm())]);
    assert_eq!(
        copy_secrets(&dir.join("m.docm")).len(),
        6,
        "{:?}",
        copy_secrets(&dir.join("m.docm"))
    );
    let out = run_in(&dir, &["clean", "m.docm"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(
        stdout(&out),
        "m.docm → m.clean.docx\n  removed  custom-data (1 field), macros (2 parts, 1 object), signatures (1 signature)\n  changed  Macros were removed, so the copy is .docx.\n           The digital signature was removed; no signature survives a change.\n  ✓ re-checked: 14 checks, nothing left\n"
    );
    assert_eq!(copy_secrets(&dir.join("m.clean.docx")).len(), 0);
    let copy = unzip(&fs::read(dir.join("m.clean.docx")).unwrap());
    let doc = String::from_utf8(
        copy.iter()
            .find(|e| e.0 == "word/document.xml")
            .unwrap()
            .1
            .clone(),
    )
    .unwrap();
    assert!(doc.contains("<w:object/>"), "{doc}");
    let types = String::from_utf8(copy[0].1.clone()).unwrap();
    assert!(
        types.contains("wordprocessingml.document.main+xml"),
        "{types}"
    );
    let out = run_in(&dir, &["clean", "m.docm", "--keep", "macros", "--json"]);
    let f = &json(&out)["files"][0];
    assert_eq!(f["output"], "m.clean.docm");
    assert_eq!(f["kept"], serde_json::json!(["macros"]));
    let left = copy_secrets(&dir.join("m.clean.docm"));
    assert_eq!(
        left.into_iter().collect::<Vec<_>>(),
        [
            "SECRET-mac-ax-1",
            "SECRET-mac-name-1",
            "SECRET-mac-ui-1",
            "SECRET-mac-vba-1"
        ]
    );
}

/// The text of each paragraph of a Word part, in order.
fn paragraphs(xml: &[u8]) -> Vec<String> {
    use quick_xml::events::Event;
    let mut r = quick_xml::Reader::from_reader(xml);
    let (mut out, mut text) = (vec![], false);
    loop {
        match r.read_event().unwrap() {
            Event::Start(e) | Event::Empty(e) if e.name().as_ref() == "w:p" => {
                out.push(String::new())
            }
            Event::Start(e) if e.name().as_ref() == "w:t" => text = true,
            Event::End(e) if e.name().as_ref() == "w:t" => text = false,
            Event::Text(t) if text => *out.last_mut().unwrap() += &t.xml10_content(),
            Event::Eof => return out,
            _ => {}
        }
    }
}

/// A part of the copy at `path`.
fn part_of(path: &std::path::Path, name: &str) -> Vec<u8> {
    let entries = unzip(&fs::read(path).unwrap());
    entries.into_iter().find(|e| e.0 == name).unwrap().1
}

#[test]
fn tracked_changes_are_accepted_as_word_does() {
    let dir = dir("clean-tracked");
    put(&dir, &[("a.docx", &leaky_docx("docx"))]);
    let keep = "hidden-text,fake-redactions,embedded-files,external-links";
    let out = run_in(&dir, &["clean", "a.docx", "--keep", keep]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(
        stdout(&out).contains("tracked-changes (7 changes)"),
        "{}",
        stdout(&out)
    );
    let copy = dir.join("a.clean.docx");
    assert_eq!(
        paragraphs(&part_of(&copy, "word/document.xml")),
        [
            "Agreement between the parties.",
            "The fee is due monthly.",
            "Payment termsSECRET-docx-ins-1MovedSECRET-docx-move-1",
            "Visible SECRET-docx-vanish-1",
            "SECRET-docx-vanish-2",
            "SECRET-docx-white-1",
            "SECRET-docx-black-1",
            "Kept",
            "Bound client",
            "[picture]",
            "",
            "",
        ]
    );
    let doc = String::from_utf8(part_of(&copy, "word/document.xml")).unwrap();
    assert!(
        doc.contains("<w:tcPr><w:gridSpan w:val=\"2\"/></w:tcPr>"),
        "{doc}"
    );
    assert!(!doc.contains("SECRET-docx-del-1"));
}

#[test]
fn shared_workbook_logs_are_deleted() {
    let ss = "application/vnd.openxmlformats-officedocument.spreadsheetml";
    let book = workbook(&[("Sheet1", "visible", &sheet_xml(""))])
        .part("xl/workbook.xml", "rId8", "revisionHeaders", "xl/revisions/revisionHeaders.xml", &format!("{ss}.revisionHeaders+xml"), format!("<headers xmlns=\"{S}\" xmlns:r=\"{R}\" guid=\"{{1}}\"><header guid=\"{{2}}\" dateTime=\"2026-01-01T00:00:00\" userName=\"Sam Sample\" r:id=\"rId1\"/></headers>").as_bytes())
        .part("xl/revisions/revisionHeaders.xml", "rId1", "revisionLog", "xl/revisions/revisionLog1.xml", &format!("{ss}.revisionLog+xml"), format!("<revisions xmlns=\"{S}\"><rcc rId=\"1\" sId=\"1\"><nc r=\"A1\" t=\"inlineStr\"><is><t>SECRET-log-1</t></is></nc></rcc></revisions>").as_bytes())
        .part("xl/workbook.xml", "rId9", "usernames", "xl/revisions/userNames.xml", &format!("{ss}.userNames+xml"), format!("<users xmlns=\"{S}\" count=\"1\"><userInfo guid=\"{{3}}\" name=\"SECRET-log-2\" id=\"1\" dateTime=\"2026-01-01T00:00:00\"/></users>").as_bytes())
        .zip();
    let dir = dir("clean-shared");
    put(&dir, &[("a.xlsx", &book)]);
    let out = run_in(&dir, &["clean", "a.xlsx"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(stdout(&out).contains("removed  tracked-changes (3 parts)"));
    assert!(copy_secrets(&dir.join("a.clean.xlsx")).is_empty());
}

#[test]
fn a_change_that_cant_be_accepted_leaves_no_file() {
    let body = format!(
        "<w:p><w:ins w:id=\"1\" {BY_SAM}><w:r><w:t>new</w:t></w:r></w:ins><w:r><w:delText>stray</w:delText></w:r></w:p>"
    );
    let dir = dir("clean-stray");
    put(
        &dir,
        &[("a.docx", &Pkg::new("docx").body(&doc_xml(&body)).zip())],
    );
    let out = run_in(&dir, &["clean", "a.docx"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        stderr(&out),
        "✗ Can't remove tracked-changes from a.docx: a kind of change this version can't accept (w:delText in word/document.xml).\n  Accept all changes in Word (Review › Accept › Accept All Changes), save, and run clean again, or use --keep tracked-changes.\n"
    );
    assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
}

#[test]
fn hidden_text_goes_and_dark_text_becomes_bars() {
    let dir = dir("clean-hidden");
    put(&dir, &[("a.docx", &leaky_docx("docx"))]);
    let out = run_in(&dir, &["clean", "a.docx"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let copy = dir.join("a.clean.docx");
    assert_eq!(
        paragraphs(&part_of(&copy, "word/document.xml")),
        [
            "Agreement between the parties.",
            "The fee is due monthly.",
            "Payment termsSECRET-docx-ins-1MovedSECRET-docx-move-1",
            "Visible ",
            "",
            "",
            "\u{2588}".repeat(19).as_str(),
            "Kept",
            "Bound client",
            "[picture]",
            "",
            "",
        ]
    );
    // The accepted insertion and move are the document's text now; nothing else is left.
    let left: Vec<_> = copy_secrets(&copy).into_iter().collect();
    assert_eq!(left, ["ins", "move"].map(|k| format!("SECRET-docx-{k}-1")));
    let doc = String::from_utf8(part_of(&copy, "word/document.xml")).unwrap();
    assert!(
        doc.contains("<v:shape id=\"_x0000_i1025\""),
        "the object keeps its picture"
    );
    assert!(!doc.contains("OLEObject"));
}

/// A document with fields that read files, a DDE field, links and a linked picture to local
/// files, and alt text that holds a path.
fn links_docx() -> Vec<u8> {
    let field = |instr: &str, result: &str| {
        format!(
            "<w:r><w:fldChar w:fldCharType=\"begin\"/></w:r><w:r><w:instrText xml:space=\"preserve\">{instr}</w:instrText></w:r><w:r><w:fldChar w:fldCharType=\"separate\"/></w:r><w:r><w:t>{result}</w:t></w:r><w:r><w:fldChar w:fldCharType=\"end\"/></w:r>"
        )
    };
    let body = [
        format!("<w:p>{}</w:p>", field(" INCLUDETEXT \"D:\\\\Projects\\\\SECRET-lnk-field-1.docx\" ", "included")),
        format!("<w:p>{}{}</w:p>", field(" DDEAUTO excel \"\\\\\\\\fileserver\\\\SECRET-lnk-dde-1.xlsx\" A1 ", "42"), field(" PAGE ", "1")),
        "<w:p><w:hyperlink r:id=\"rId40\"><w:r><w:t>the budget</w:t></w:r></w:hyperlink><w:hyperlink r:id=\"rId41\"><w:r><w:t>web</w:t></w:r></w:hyperlink></w:p>".into(),
        "<w:p><w:r><w:drawing><wp:inline><wp:extent cx=\"9\" cy=\"9\"/><wp:docPr id=\"1\" name=\"P\" descr=\"/home/avery/SECRET-lnk-alt-1.png\"/><a:graphic><a:graphicData uri=\"http://schemas.openxmlformats.org/drawingml/2006/picture\"><pic:pic><pic:nvPicPr><pic:cNvPr id=\"0\" name=\"P\"/><pic:cNvPicPr/></pic:nvPicPr><pic:blipFill><a:blip r:link=\"rId42\"/></pic:blipFill><pic:spPr/></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r><w:r><w:t>after</w:t></w:r></w:p>".into(),
    ]
    .concat();
    Pkg::new("docx")
        .body(&doc_xml(&body))
        .link(
            "word/document.xml",
            external(
                "rId40",
                "hyperlink",
                "file:///D:/Projects/SECRET-lnk-link-1.xlsx",
            ),
        )
        .link(
            "word/document.xml",
            external("rId41", "hyperlink", "https://example.com/"),
        )
        .link(
            "word/document.xml",
            external(
                "rId42",
                "image",
                "\\\\fileserver\\share\\SECRET-lnk-pic-1.png",
            ),
        )
        .zip()
}

#[test]
fn fields_links_and_pictures_stop_reading_local_files() {
    let dir = dir("clean-links");
    put(&dir, &[("a.docx", &links_docx())]);
    let out = run_in(&dir, &["clean", "a.docx", "--json"]);
    assert_eq!(out.status.code(), Some(0), "{}", stdout(&out));
    let copy = dir.join("a.clean.docx");
    assert!(copy_secrets(&copy).is_empty(), "{:?}", copy_secrets(&copy));
    assert_eq!(
        paragraphs(&part_of(&copy, "word/document.xml")),
        ["included", "421", "the budgetweb", "after"]
    );
    let rels = String::from_utf8(part_of(&copy, "word/_rels/document.xml.rels")).unwrap();
    assert!(
        rels.contains("https://example.com/") && !rels.contains("rId40"),
        "{rels}"
    );
    let doc = String::from_utf8(part_of(&copy, "word/document.xml")).unwrap();
    assert!(
        doc.contains("<w:hyperlink r:id=\"rId41\">"),
        "web links stay"
    );
    assert!(doc.contains("PAGE"), "other fields stay");
}

#[test]
fn an_imported_document_leaves_no_file() {
    let body = "<w:altChunk r:id=\"rId20\"/><w:p><w:r><w:t>x</w:t></w:r></w:p>";
    let doc = Pkg::new("docx")
        .body(&doc_xml(body))
        .part(
            "word/document.xml",
            "rId20",
            "aFChunk",
            "word/afchunk.htm",
            "text/html",
            b"<p>imported</p>",
        )
        .zip();
    let dir = dir("clean-chunk");
    put(&dir, &[("a.docx", &doc)]);
    let out = run_in(&dir, &["clean", "a.docx", "--json"]);
    assert_eq!(out.status.code(), Some(1));
    let f = &json(&out)["files"][0];
    assert_eq!(f["status"], "not_cleaned");
    assert_eq!(f["output"], serde_json::Value::Null);
    assert_eq!(f["error"]["code"], "cannot_clean");
    assert_eq!(
        f["error"]["message"],
        "Can't remove embedded-files from a.docx: it holds an imported document (w:altChunk)."
    );
    assert!(
        f["error"]["hint"]
            .as_str()
            .unwrap()
            .ends_with("or use --keep embedded-files.")
    );
    assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
    let out = run_in(&dir, &["clean", "a.docx", "--keep", "embedded-files"]);
    assert_eq!(out.status.code(), Some(0));
}

#[test]
fn slide_objects_become_their_pictures_and_charts_lose_workbooks() {
    let dir = dir("clean-ole");
    put(&dir, &[("a.pptx", &leaky_pptx("pptx"))]);
    let keep = "hidden-slides,speaker-notes,hidden-objects";
    let out = run_in(&dir, &["clean", "a.pptx", "--keep", keep]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(stdout(&out).contains("  changed  1 embedded object is now a picture.\n"));
    let copy = dir.join("a.clean.pptx");
    let left = copy_secrets(&copy);
    for s in ["ole", "chart", "tag", "addin", "comment"] {
        assert!(
            !left.iter().any(|l| l.contains(&format!("-{s}-"))),
            "{s}: {left:?}"
        );
    }
    let slide = String::from_utf8(part_of(&copy, "ppt/slides/slide1.xml")).unwrap();
    assert!(
        slide.contains("<p:pic><p:nvPicPr><p:cNvPr id=\"40\" name=\"Object 1\"/><p:cNvPicPr/><p:nvPr/></p:nvPicPr><p:blipFill><a:blip r:embed=\"rId6\"/><a:stretch><a:fillRect/></a:stretch></p:blipFill><p:spPr><a:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"100\" cy=\"100\"/></a:xfrm><a:prstGeom prst=\"rect\"><a:avLst/></a:prstGeom></p:spPr></p:pic>"),
        "{slide}"
    );
    let chart = String::from_utf8(part_of(&copy, "ppt/charts/chart1.xml")).unwrap();
    assert!(!chart.contains("externalData"), "{chart}");
}

#[test]
fn hidden_sheets_and_cells_go_and_the_workbook_keeps_its_values() {
    for (name, ext) in [("a.xlsx", "xlsx"), ("a.xlsm", "xlsx")] {
        let dir = dir("clean-book");
        put(&dir, &[(name, &leaky_xlsx(&name[2..]))]);
        let out = run_in(&dir, &["clean", name, "--json"]);
        assert_eq!(out.status.code(), Some(0), "{}", stdout(&out));
        let f = &json(&out)["files"][0];
        let codes: Vec<_> = f["changes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["code"].clone())
            .collect();
        let mut want = vec!["formulas_to_values", "charts_to_values"];
        if name.ends_with('m') {
            want.insert(0, "extension_changed");
        }
        assert_eq!(codes, want, "{name}");
        let copy = dir.join(format!("a.clean.{ext}"));
        assert!(
            copy_secrets(&copy).is_empty(),
            "{name}: {:?}",
            copy_secrets(&copy)
        );
        let book = String::from_utf8(part_of(&copy, "xl/workbook.xml")).unwrap();
        assert!(
            book.contains("<sheets><sheet name=\"Summary\" sheetId=\"1\" r:id=\"rId1\"/></sheets>"),
            "{book}"
        );
        let sheet = String::from_utf8(part_of(&copy, "xl/worksheets/sheet1.xml")).unwrap();
        assert!(
            sheet.contains("<row r=\"1\"><c r=\"A1\" t=\"s\"><v>0</v></c><c r=\"B1\"><v>6</v></c><c r=\"D1\"/></row><row r=\"2\"><c r=\"A2\" s=\"1\"/><c r=\"B2\" s=\"2\"/><c r=\"C2\" s=\"3\"/>"),
            "{sheet}"
        );
        assert!(
            sheet.contains("<row r=\"5\" hidden=\"1\"><c r=\"A5\"/></row>"),
            "rows stay hidden"
        );
        let chart = String::from_utf8(part_of(&copy, "xl/charts/chart1.xml")).unwrap();
        assert!(
            chart.contains("<c:val><c:numLit><c:ptCount val=\"3\"/>") && !chart.contains("c:f"),
            "{chart}"
        );
    }
}

/// A workbook with a visible and a hidden sheet, names, views and formats that point at the
/// hidden one, a formula with no saved value, a string result and a merged range.
fn names_xlsx(chart_ref: &str) -> Vec<u8> {
    let visible = format!(
        "<worksheet xmlns=\"{S}\"><sheetData><row r=\"1\"><c r=\"A1\"><f>Secret!A1*2</f></c><c r=\"B1\" t=\"str\"><f>\"x \"&amp;Secret!B1</f><v>x  y</v></c><c r=\"C1\"><v>3</v></c><c r=\"D1\"><v>4</v></c></row></sheetData><mergeCells count=\"1\"><mergeCell ref=\"C1:D1\"/></mergeCells><conditionalFormatting sqref=\"A1\"><cfRule type=\"expression\" priority=\"1\"><formula>Secret!A1&gt;1</formula></cfRule></conditionalFormatting><conditionalFormatting sqref=\"C1\"><cfRule type=\"expression\" priority=\"2\"><formula>C1&gt;1</formula></cfRule></conditionalFormatting></worksheet>"
    );
    let book = format!(
        "<workbook xmlns=\"{S}\" xmlns:r=\"{R}\"><bookViews><workbookView firstSheet=\"1\" activeTab=\"1\"/></bookViews><sheets><sheet name=\"Secret\" sheetId=\"1\" state=\"hidden\" r:id=\"rId1\"/><sheet name=\"Shown\" sheetId=\"2\" r:id=\"rId2\"/></sheets><definedNames><definedName name=\"a\" localSheetId=\"0\">Secret!$A$1</definedName><definedName name=\"b\">'secret'!$A$1</definedName><definedName name=\"c\" localSheetId=\"1\">Shown!$C$1</definedName></definedNames></workbook>"
    );
    let chart = format!(
        "<c:chartSpace xmlns:c=\"http://schemas.openxmlformats.org/drawingml/2006/chart\"><c:chart><c:plotArea><c:barChart><c:ser><c:tx><c:strRef><c:f>Shown!$A$1</c:f><c:strCache><c:pt idx=\"0\"><c:v>Total</c:v></c:pt></c:strCache></c:strRef></c:tx><c:cat>{chart_ref}</c:cat></c:ser></c:barChart></c:plotArea></c:chart></c:chartSpace>"
    );
    let drawing = "<xdr:wsDr xmlns:xdr=\"http://schemas.openxmlformats.org/drawingml/2006/spreadsheetDrawing\" xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"><xdr:absoluteAnchor><xdr:pos x=\"0\" y=\"0\"/><xdr:ext cx=\"9\" cy=\"9\"/><xdr:graphicFrame macro=\"\"><xdr:nvGraphicFramePr><xdr:cNvPr id=\"2\" name=\"Chart\"/><xdr:cNvGraphicFramePr/></xdr:nvGraphicFramePr><xdr:xfrm/><a:graphic><a:graphicData uri=\"http://schemas.openxmlformats.org/drawingml/2006/chart\"><c:chart xmlns:c=\"http://schemas.openxmlformats.org/drawingml/2006/chart\" r:id=\"rId1\"/></a:graphicData></a:graphic></xdr:graphicFrame><xdr:clientData/></xdr:absoluteAnchor></xdr:wsDr>";
    let sml = "application/vnd.openxmlformats-officedocument";
    Pkg::new("xlsx")
        .body(&book)
        .part(
            "xl/workbook.xml",
            "rId1",
            "worksheet",
            "xl/worksheets/sheet1.xml",
            SHEET_TYPE,
            sheet_xml("<row r=\"1\"><c r=\"A1\"><v>SECRET-names-1</v></c></row>").as_bytes(),
        )
        .part(
            "xl/workbook.xml",
            "rId2",
            "worksheet",
            "xl/worksheets/sheet2.xml",
            SHEET_TYPE,
            visible.as_bytes(),
        )
        .part(
            "xl/worksheets/sheet2.xml",
            "rId1",
            "drawing",
            "xl/drawings/drawing1.xml",
            &format!("{sml}.drawing+xml"),
            drawing.as_bytes(),
        )
        .part(
            "xl/drawings/drawing1.xml",
            "rId1",
            "chart",
            "xl/charts/chart1.xml",
            &format!("{sml}.drawingml.chart+xml"),
            chart.as_bytes(),
        )
        .zip()
}

#[test]
fn deleting_a_sheet_fixes_names_views_formats_and_formulas() {
    let dir = dir("clean-names");
    put(
        &dir,
        &[(
            "a.xlsx",
            &names_xlsx(
                "<c:numRef><c:f>Shown!$C$1</c:f><c:numCache><c:pt idx=\"0\"><c:v>3</c:v></c:pt></c:numCache></c:numRef>",
            ),
        )],
    );
    let out = run_in(&dir, &["clean", "a.xlsx"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(
        stdout(&out),
        "a.xlsx → a.clean.xlsx\n  removed  hidden-sheets (1 sheet)\n  changed  1 formula was replaced by its saved result.\n           1 formula had no saved result and is now empty.\n           1 chart now shows its saved values instead of linked cells.\n  ✓ re-checked: 17 checks, nothing left\n"
    );
    let copy = dir.join("a.clean.xlsx");
    assert!(copy_secrets(&copy).is_empty());
    let book = String::from_utf8(part_of(&copy, "xl/workbook.xml")).unwrap();
    assert!(
        book.contains("<workbookView firstSheet=\"0\" activeTab=\"0\"/>"),
        "{book}"
    );
    assert!(book.contains("<definedNames><definedName name=\"c\" localSheetId=\"0\">Shown!$C$1</definedName></definedNames>"), "{book}");
    let sheet = String::from_utf8(part_of(&copy, "xl/worksheets/sheet2.xml")).unwrap();
    assert!(
        sheet.contains("<row r=\"1\"><c r=\"A1\"/><c r=\"B1\" t=\"inlineStr\"><is><t>x  y</t></is></c><c r=\"C1\"><v>3</v></c>"),
        "{sheet}"
    );
    assert!(
        !sheet.contains("Secret!") && sheet.contains("C1&gt;1"),
        "{sheet}"
    );
    let chart = String::from_utf8(part_of(&copy, "xl/charts/chart1.xml")).unwrap();
    assert!(
        chart.contains("<c:tx><c:v>Total</c:v></c:tx><c:cat><c:numLit><c:pt idx=\"0\"><c:v>3</c:v></c:pt></c:numLit></c:cat>"),
        "{chart}"
    );
}

#[test]
fn a_multi_level_label_from_a_deleted_sheet_leaves_no_file() {
    let dir = dir("clean-multi");
    put(
        &dir,
        &[(
            "a.xlsx",
            &names_xlsx("<c:multiLvlStrRef><c:f>Secret!$A$1:$B$2</c:f></c:multiLvlStrRef>"),
        )],
    );
    let out = run_in(&dir, &["clean", "a.xlsx"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        stderr(&out),
        "✗ Can't remove hidden-sheets from a.xlsx: chart xl/charts/chart1.xml has multi-level labels from a deleted sheet.\n  Change the chart's labels to one level in Excel, or run clean again with --keep hidden-sheets.\n"
    );
    assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
}

#[test]
fn slicers_stop_the_pivot_cache_clean() {
    let ss = "application/vnd.openxmlformats-officedocument.spreadsheetml";
    let ms = "http://schemas.microsoft.com/office";
    let book = format!(
        "<workbook xmlns=\"{S}\" xmlns:r=\"{R}\"><sheets><sheet name=\"Sheet1\" sheetId=\"1\" r:id=\"rId1\"/></sheets><pivotCaches><pivotCache cacheId=\"1\" r:id=\"rId2\"/></pivotCaches></workbook>"
    );
    let bytes = Pkg::new("xlsx")
        .body(&book)
        .part("xl/workbook.xml", "rId1", "worksheet", "xl/worksheets/sheet1.xml", SHEET_TYPE, sheet_xml("").as_bytes())
        .part("xl/workbook.xml", "rId2", "pivotCacheDefinition", "xl/pivotCache/pivotCacheDefinition1.xml", &format!("{ss}.pivotCacheDefinition+xml"), format!("<pivotCacheDefinition xmlns=\"{S}\" recordCount=\"0\"/>").as_bytes())
        .part("xl/workbook.xml", "rId3", &format!("{ms}/2007/relationships/slicerCache"), "xl/slicerCaches/slicerCache1.xml", "application/vnd.ms-excel.slicerCache+xml", b"<slicerCacheDefinition xmlns=\"http://schemas.microsoft.com/office/spreadsheetml/2009/9/main\" name=\"S\" sourceName=\"N\"/>")
        .zip();
    let dir = dir("clean-slicers");
    put(&dir, &[("a.xlsx", &bytes)]);
    let out = run_in(&dir, &["clean", "a.xlsx"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        stderr(&out),
        "✗ Can't remove pivot-caches from a.xlsx: its pivot table has slicers.\n  Remove the slicers in Excel, or run clean again with --keep pivot-caches.\n"
    );
    assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
    let out = run_in(&dir, &["clean", "a.xlsx", "--keep", "pivot-caches"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
}

#[test]
fn kept_comments_on_cleared_cells_go_with_them() {
    let dir = dir("clean-kept-notes");
    put(&dir, &[("a.xlsx", &leaky_xlsx("xlsx"))]);
    let keep = "comments";
    let out = run_in(&dir, &["clean", "a.xlsx", "--keep", keep]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    // The note on A1 stays: A1 keeps its value.
    let left: Vec<_> = copy_secrets(&dir.join("a.clean.xlsx"))
        .into_iter()
        .collect();
    assert_eq!(left, ["SECRET-xlsx-note-1", "SECRET-xlsx-thread-1"]);
}

#[test]
fn hidden_slides_notes_and_shapes_are_deleted() {
    let dir = dir("clean-deck");
    put(&dir, &[("a.pptx", &leaky_pptx("pptx"))]);
    let out = run_in(&dir, &["clean", "a.pptx"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(
        text.contains("  changed  Animations were removed from slide 1.\n           1 embedded object is now a picture.\n"),
        "{text}"
    );
    let copy = dir.join("a.clean.pptx");
    assert!(copy_secrets(&copy).is_empty(), "{:?}", copy_secrets(&copy));
    let names: Vec<_> = unzip(&fs::read(&copy).unwrap())
        .into_iter()
        .map(|e| e.0)
        .collect();
    assert!(
        !names
            .iter()
            .any(|n| n.contains("slide2") || n.contains("notesSlide")),
        "{names:?}"
    );
    let pres = String::from_utf8(part_of(&copy, "ppt/presentation.xml")).unwrap();
    assert!(pres.contains("<p:sldIdLst><p:sldId id=\"256\" r:id=\"rId1\"/><p:sldId id=\"258\" r:id=\"rId3\"/></p:sldIdLst>"), "{pres}");
    assert!(
        pres.contains("<p:sldLst/>")
            && pres.contains(
                "<p14:sldIdLst><p14:sldId id=\"256\"/><p14:sldId id=\"258\"/></p14:sldIdLst>"
            ),
        "{pres}"
    );
    let slide = String::from_utf8(part_of(&copy, "ppt/slides/slide1.xml")).unwrap();
    assert!(
        slide.contains("<p:cNvPr id=\"30\" name=\"Link\"/>"),
        "the link to the hidden slide goes: {slide}"
    );
    assert!(
        !slide.contains("p:timing") && !slide.contains("id=\"21\"") && !slide.contains("id=\"22\""),
        "{slide}"
    );
    assert!(
        slide.contains("<p:cNvPr id=\"20\" name=\"Group\"/>"),
        "the group stays"
    );
}

#[test]
fn hidden_shapes_go_from_documents_and_sheets() {
    let body = "<w:p><w:r><w:drawing><wp:inline><wp:extent cx=\"10\" cy=\"10\"/><wp:docPr id=\"1\" name=\"Picture 1\" hidden=\"1\" descr=\"SECRET-hid-alt-1\"/><a:graphic><a:graphicData><pic:pic><pic:nvPicPr><pic:cNvPr id=\"0\" name=\"x\"/><pic:cNvPicPr/></pic:nvPicPr></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r><w:r><w:pict><v:shape id=\"t1\" style=\"position:absolute;visibility: hidden\"><v:textbox><w:txbxContent><w:p><w:r><w:t>SECRET-hid-vml-1</w:t></w:r></w:p></w:txbxContent></v:textbox></v:shape></w:pict></w:r><w:r><w:t>shown</w:t></w:r></w:p>";
    let doc = Pkg::new("docx").body(&doc_xml(body)).zip();
    let drawing = "<xdr:wsDr xmlns:xdr=\"http://schemas.openxmlformats.org/drawingml/2006/spreadsheetDrawing\" xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\"><xdr:twoCellAnchor><xdr:sp><xdr:nvSpPr><xdr:cNvPr id=\"2\" name=\"Box\" hidden=\"1\"/><xdr:cNvSpPr/></xdr:nvSpPr><xdr:txBody><a:p><a:r><a:t>SECRET-hid-xdr-1</a:t></a:r></a:p></xdr:txBody></xdr:sp><xdr:clientData/></xdr:twoCellAnchor><xdr:twoCellAnchor><xdr:sp><xdr:nvSpPr><xdr:cNvPr id=\"3\" name=\"Shown\"/><xdr:cNvSpPr/></xdr:nvSpPr></xdr:sp><xdr:clientData/></xdr:twoCellAnchor></xdr:wsDr>";
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
    let dir = dir("clean-shapes");
    put(&dir, &[("a.docx", &doc), ("a.xlsx", &book)]);
    let out = run_in(&dir, &["clean", "a.docx", "a.xlsx"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(copy_secrets(&dir.join("a.clean.docx")).is_empty());
    assert!(copy_secrets(&dir.join("a.clean.xlsx")).is_empty());
    assert_eq!(
        paragraphs(&part_of(&dir.join("a.clean.docx"), "word/document.xml")),
        ["shown"]
    );
    let d = String::from_utf8(part_of(
        &dir.join("a.clean.xlsx"),
        "xl/drawings/drawing1.xml",
    ))
    .unwrap();
    assert!(
        d.contains("name=\"Shown\"") && d.matches("<xdr:twoCellAnchor>").count() == 1,
        "{d}"
    );
}

/// The checks a file's check found, from its JSON.
fn found_checks(f: &serde_json::Value) -> Vec<String> {
    f["checks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["status"] == "found")
        .map(|c| c["id"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn every_kept_check_survives_and_shows_as_kept() {
    for (name, bytes) in [
        ("a.docx", leaky_docx("docx")),
        ("a.xlsx", leaky_xlsx("xlsx")),
        ("a.pptx", leaky_pptx("pptx")),
    ] {
        let dir = dir("clean-keep");
        put(&dir, &[(name, &bytes)]);
        let checked = json(&run_in(&dir, &[name, "--json"]));
        for id in found_checks(&checked["files"][0]) {
            if id == "signatures" {
                continue;
            }
            // A hidden macro sheet is a hidden sheet too: removing hidden sheets removes it.
            let also = if id == "macros" && name.ends_with("xlsx") {
                ",hidden-sheets"
            } else {
                ""
            };
            let keep = format!("{id}{also}");
            let out = run_in(
                &dir,
                &[
                    "clean",
                    name,
                    "--keep",
                    &keep,
                    "-o",
                    "out",
                    "--overwrite",
                    "--json",
                ],
            );
            let f = &json(&out)["files"][0];
            assert_eq!(f["status"], "cleaned", "{name} --keep {id}: {f}");
            assert!(
                f["kept"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|k| k == id.as_str()),
                "{name} {id}"
            );
            let rechecked = f["recheck"]["checks"].as_array().unwrap();
            assert_eq!(
                rechecked.iter().find(|c| c["id"] == id.as_str()).unwrap()["status"],
                "kept"
            );
            fs::rename(dir.join("out"), dir.join(format!("copy.{}", &name[2..]))).unwrap();
            let copy = json(&run_in(&dir, &[&format!("copy.{}", &name[2..]), "--json"]));
            let still = found_checks(&copy["files"][0]);
            assert!(
                still.contains(&id),
                "{name}: {id} didn't survive: {still:?}"
            );
            let others: Vec<_> = still
                .iter()
                .filter(|c| **c != id && !keep.contains(c.as_str()))
                .collect();
            assert!(others.is_empty(), "{name} --keep {id}: {others:?} left");
        }
    }
}

#[test]
fn the_leak_fixtures_clean_to_nothing() {
    for (name, bytes) in [
        ("a.docx", leaky_docx("docx")),
        ("a.xlsx", leaky_xlsx("xlsx")),
        ("a.pptx", leaky_pptx("pptx")),
        ("a.docm", leaky_docx("docm")),
        ("a.xlsm", leaky_xlsx("xlsm")),
        ("a.pptm", leaky_pptx("pptm")),
    ] {
        let dir = dir("clean-leaks");
        put(&dir, &[(name, &bytes)]);
        let out = run_in(&dir, &["clean", name, "--json"]);
        let f = &json(&out)["files"][0];
        assert_eq!(f["status"], "cleaned", "{name}: {f}");
        let output = f["output"].as_str().unwrap();
        assert!(!output.ends_with('m'), "{name} -> {output}");
        let copy = json(&run_in(&dir, &[output, "--json"]));
        let left = found_checks(&copy["files"][0]);
        assert!(left.is_empty(), "{name}: {left:?}");
        let secrets = copy_secrets(&dir.join(output));
        let accepted = ["ins", "move"].map(|k| format!("SECRET-docx-{k}-1"));
        assert!(
            secrets.iter().all(|s| accepted.contains(s)),
            "{name}: {secrets:?}"
        );
    }
}

/// Writes the leak fixtures, their twins and their cleaned copies to `DOC_SANITIZE_OUT` (or a
/// test folder) for checks outside the tests: Office opening each copy, other validators.
#[test]
#[ignore = "writes fixtures for outside checks"]
fn write_cleaned_fixtures() {
    let out = std::env::var_os("DOC_SANITIZE_OUT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| dir("cleaned"));
    fs::create_dir_all(&out).unwrap();
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
    ] {
        fs::write(out.join(name), bytes).unwrap();
        let o = run_in(
            &out,
            &["clean", name, "--overwrite", "-o", &format!("clean-{name}")],
        );
        assert_eq!(o.status.code(), Some(0), "{name}: {}", stderr(&o));
    }
    println!("{}", out.display());
}

#[test]
fn picture_metadata_goes_and_pixels_stay() {
    use doc_sanitize::pictures::{chunk, metadata, orientation};
    let photo = jpeg(&[0x0112, 0x8825], b"SECRET-picture-tail-1");
    let plain = png(8, 8);
    let texted = [
        &plain[..33],
        &chunk(b"tEXt", b"Author\0SECRET-picture-text-1"),
        &plain[33..],
    ]
    .concat();
    let svg = "<svg xmlns=\"http://www.w3.org/2000/svg\"><metadata>SECRET-picture-svg-1</metadata><title>Logo</title></svg>";
    let doc = Pkg::new("docx")
        .body(&doc_xml(
            &(blip("rId1", "") + &blip("rId2", "") + &blip("rId3", "")),
        ))
        .part(
            "word/document.xml",
            "rId1",
            "image",
            "word/media/image1.jpeg",
            "",
            &photo,
        )
        .part(
            "word/document.xml",
            "rId2",
            "image",
            "word/media/image2.png",
            "",
            &texted,
        )
        .part(
            "word/document.xml",
            "rId3",
            "image",
            "word/media/image3.svg",
            "image/svg+xml",
            svg.as_bytes(),
        )
        .zip();
    let dir = dir("clean-pictures");
    put(&dir, &[("a.docx", &doc)]);
    let out = run_in(&dir, &["clean", "a.docx", "--json"]);
    let f = &json(&out)["files"][0];
    assert_eq!(f["status"], "cleaned", "{f}");
    assert_eq!(f["removed"][0]["check"], "image-metadata", "{f}");
    let copy = dir.join("a.clean.docx");
    let jpeg_copy = part_of(&copy, "word/media/image1.jpeg");
    assert_eq!(metadata(&jpeg_copy), Some(vec![]));
    assert_eq!(orientation(&jpeg_copy), Some(6));
    assert!(jpeg_copy.ends_with(&[0xFF, 0xDA, 0, 2, 0x12, 0x34, 0xFF, 0xD9]));
    assert_eq!(part_of(&copy, "word/media/image2.png"), plain);
    let svg_copy = String::from_utf8(part_of(&copy, "word/media/image3.svg")).unwrap();
    assert!(svg_copy.contains("<title>Logo</title>"), "{svg_copy}");
    assert!(copy_secrets(&copy).is_empty());
}

/// Fixture 49: a TIFF with metadata can't be stripped without re-encoding it.
#[test]
fn a_tiff_with_metadata_leaves_no_file() {
    let mut tiff = b"II*\0\x08\0\0\0\x02\0".to_vec();
    for tag in [256u16, 0x8825] {
        tiff.extend(tag.to_le_bytes());
        tiff.extend([3, 0, 1, 0, 0, 0, 1, 0, 0, 0]);
    }
    tiff.extend([0; 4]);
    let doc = Pkg::new("docx")
        .body(&doc_xml(&blip("rId1", "")))
        .part(
            "word/document.xml",
            "rId1",
            "image",
            "word/media/image1.tiff",
            "image/tiff",
            &tiff,
        )
        .zip();
    let dir = dir("clean-tiff");
    put(&dir, &[("tiff_exif.docx", &doc)]);
    let out = run_in(&dir, &["clean", "tiff_exif.docx"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        stderr(&out),
        "✗ Can't remove image-metadata from tiff_exif.docx: picture word/media/image1.tiff is a TIFF.\n  Replace the TIFF picture with a PNG or JPEG copy, or use --keep image-metadata.\n"
    );
    assert!(!dir.join("tiff_exif.clean.docx").exists());
    let out = run_in(
        &dir,
        &["clean", "tiff_exif.docx", "--keep", "image-metadata"],
    );
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
}

#[test]
fn cropped_pictures_keep_only_what_shows() {
    let size = |b: &[u8]| {
        let image = image::load_from_memory(b).unwrap();
        (image.width(), image.height())
    };
    let body = blip("rId1", "<a:srcRect l=\"25000\" r=\"25000\" t=\"10000\"/>")
        + &blip("rId1", "")
        + &blip("rId2", "<a:srcRect b=\"50000\"/>")
        + &blip("rId2", "<a:srcRect b=\"50000\"/>");
    let doc = picture_doc(
        &[
            ("word/media/image1.jpeg", &photo(40, 20)),
            ("word/media/image2.png", &png(10, 10)),
        ],
        &body,
    );
    let dir = dir("clean-crops");
    put(&dir, &[("a.docx", &doc)]);
    let out = run_in(&dir, &["clean", "a.docx", "--json"]);
    let f = &json(&out)["files"][0];
    assert_eq!(f["status"], "cleaned", "{f}");
    let copy = dir.join("a.clean.docx");
    let names: Vec<String> = unzip(&fs::read(&copy).unwrap())
        .into_iter()
        .map(|e| e.0)
        .filter(|n| n.contains("media"))
        .collect();
    // image1 still shows whole elsewhere; image2 showed only cropped, so it goes.
    assert_eq!(
        names,
        [
            "word/media/image1.jpeg",
            "word/media/image1-crop1.jpeg",
            "word/media/image2-crop1.png"
        ]
    );
    assert_eq!(size(&part_of(&copy, "word/media/image1.jpeg")), (40, 20));
    assert_eq!(
        size(&part_of(&copy, "word/media/image1-crop1.jpeg")),
        (20, 18)
    );
    assert_eq!(
        size(&part_of(&copy, "word/media/image2-crop1.png")),
        (10, 5)
    );
    // The gradient's red starts a quarter of the way in, where the crop now starts.
    let shown = image::load_from_memory(&part_of(&copy, "word/media/image1-crop1.jpeg")).unwrap();
    let red = shown.to_rgb8().get_pixel(0, 9).0[0];
    assert!((55..=75).contains(&red), "{red}");
    let xml = String::from_utf8(part_of(&copy, "word/document.xml")).unwrap();
    assert_eq!(xml.matches("<a:srcRect/>").count(), 3, "{xml}");
}

#[test]
fn pictures_that_cant_be_cropped_leave_no_file() {
    let mut cmyk = photo(8, 8);
    let sof = cmyk.windows(2).position(|w| w == [0xFF, 0xC0]).unwrap();
    cmyk[sof + 9] = 4;
    for (picture, why) in [
        (cmyk, "is a CMYK JPEG"),
        (jpeg(&[0x0112], b""), "is turned by its EXIF orientation"),
    ] {
        let doc = picture_doc(
            &[("word/media/image1.jpeg", &picture)],
            &blip("rId1", "<a:srcRect l=\"20000\"/>"),
        );
        let dir = dir("clean-uncroppable");
        put(&dir, &[("a.docx", &doc)]);
        let out = run_in(&dir, &["clean", "a.docx"]);
        assert_eq!(out.status.code(), Some(1));
        assert_eq!(
            stderr(&out),
            format!(
                "✗ Can't remove cropped-images from a.docx: picture word/media/image1.jpeg {why}.\n  Run clean again with --keep cropped-images.\n"
            )
        );
        assert!(!dir.join("a.clean.docx").exists());
    }
}
