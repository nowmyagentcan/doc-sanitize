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
fn unused_shared_strings_are_dropped_and_cells_renumbered() {
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
    let copy = unzip(&fs::read(dir.join("a.clean.xlsx")).unwrap());
    let part = |n: &str| String::from_utf8(copy.iter().find(|e| e.0 == n).unwrap().1.clone());
    assert_eq!(
        part("xl/sharedStrings.xml").unwrap(),
        format!(
            "<sst xmlns=\"{S}\" count=\"2\" uniqueCount=\"2\"><si><t>first</t></si><si><t>third</t></si></sst>"
        )
    );
    assert!(part("xl/worksheets/sheet1.xml").unwrap().contains(
        "<c r=\"A1\" t=\"s\"><v>1</v></c><c r=\"B1\" t=\"s\"><v>0</v></c><c r=\"C1\"><v>0</v></c>"
    ));
}
