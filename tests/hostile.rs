//! Hostile and tricky inputs: each gives exactly its stated result, quickly, with no panic.

mod common;

use std::time::{Duration, Instant};

use common::*;

/// Checks `bytes` saved as `name` and gives the file's JSON entry, within 10 s.
fn check(name: &str, bytes: &[u8]) -> serde_json::Value {
    let dir = dir(name.split('.').next().unwrap());
    put(&dir, &[(name, bytes)]);
    let start = Instant::now();
    let out = run_in(&dir, &[name, "--json"]);
    assert!(
        start.elapsed() < Duration::from_secs(10),
        "{name} took {:?}",
        start.elapsed()
    );
    assert!(!stderr(&out).contains("panicked"), "{}", stderr(&out));
    json(&out)["files"][0].clone()
}

fn refused(name: &str, bytes: &[u8], code: &str, message: &str) {
    let f = check(name, bytes);
    assert_eq!(f["status"], "error", "{name}: {f}");
    assert_eq!(f["error"]["code"], code, "{name}: {f}");
    assert_eq!(f["error"]["message"], message, "{name}");
}

const AMBIGUOUS_HINT: &str =
    "This file may have been altered to hide content. Ask for a fresh copy.";

fn word() -> Vec<Entry> {
    vec![
        entry("[Content_Types].xml", b"<Types/>"),
        entry("_rels/.rels", b"<Relationships/>"),
        entry("word/document.xml", b"<w:document/>"),
    ]
}

#[test]
fn overlapping_entries_are_refused_before_any_inflate() {
    // Fifield-style: every entry's data runs on through the next entries' headers to one
    // shared kernel at the end.
    let kernel = vec![0u8; 1 << 20];
    let names: Vec<String> = (0..100).map(|i| format!("word/part{i:03}.xml")).collect();
    let mut out = vec![];
    let mut offsets = vec![];
    for name in &names {
        offsets.push(out.len());
        out.extend(b"PK\x03\x04\x14\x00\x00\x00\x00\x00\x00\x00\x00\x00");
        out.extend([0u8; 12]);
        out.extend((name.len() as u16).to_le_bytes());
        out.extend([0u8; 2]);
        out.extend(name.as_bytes());
    }
    out.extend(&kernel);
    let end = out.len();
    let mut dir = vec![];
    for (name, &offset) in names.iter().zip(&offsets) {
        let size = (end - offset - 30 - name.len()) as u32;
        dir.extend(b"PK\x01\x02\x14\x00\x14\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00");
        dir.extend(size.to_le_bytes());
        dir.extend(size.to_le_bytes());
        dir.extend((name.len() as u16).to_le_bytes());
        dir.extend([0u8; 12]);
        dir.extend((offset as u32).to_le_bytes());
        dir.extend(name.as_bytes());
    }
    let cd = out.len() as u32;
    out.extend(&dir);
    out.extend(b"PK\x05\x06\x00\x00\x00\x00");
    out.extend(100u16.to_le_bytes());
    out.extend(100u16.to_le_bytes());
    out.extend((dir.len() as u32).to_le_bytes());
    out.extend(cd.to_le_bytes());
    out.extend([0u8; 2]);
    // The local headers' sizes are 0, so they disagree with the directory first; with the
    // sizes made equal, the overlap itself is what refuses it.
    refused(
        "overlap_bomb.docx",
        &out,
        "ambiguous_file",
        "overlap_bomb.docx is ambiguous: its file index doesn't match its contents.",
    );
    let mut same = out.clone();
    for (name, &offset) in names.iter().zip(&offsets) {
        let size = (end - offset - 30 - name.len()) as u32;
        same[offset + 18..offset + 22].copy_from_slice(&size.to_le_bytes());
        same[offset + 22..offset + 26].copy_from_slice(&size.to_le_bytes());
    }
    let f = check("overlap_bomb.docx", &same);
    assert_eq!(
        f["error"]["message"],
        "overlap_bomb.docx is ambiguous: its parts overlap."
    );
    assert_eq!(f["error"]["hint"], AMBIGUOUS_HINT);
}

#[test]
fn two_versions_of_a_part_are_refused() {
    let mut dup = word();
    dup.push(entry(
        "word/documenX.xml",
        b"<w:document>SECRET-dup-1</w:document>",
    ));
    let bytes = zip(&dup);
    let at = central_start(&bytes);
    let bytes = patch(
        patch(bytes, 0, b"documenX", b"document"),
        at,
        b"documenX",
        b"document",
    );
    refused(
        "dup_name.docx",
        &bytes,
        "ambiguous_file",
        "dup_name.docx holds two different versions of word/document.xml, so apps may show either one.",
    );

    let mut case = word();
    case.push(entry("Word/Document.xml", b"<w:document/>"));
    refused(
        "case_dup.docx",
        &zip(&case),
        "ambiguous_file",
        "case_dup.docx holds two different versions of Word/Document.xml, so apps may show either one.",
    );
}

#[test]
fn headers_that_disagree_are_refused() {
    let index =
        |name: &str| format!("{name} is ambiguous: its file index doesn't match its contents.");
    let mut cloak = word();
    cloak[2].central_name = Some("word/documenx.xml".into());
    refused(
        "cloak.docx",
        &zip(&cloak),
        "ambiguous_file",
        &index("cloak.docx"),
    );

    let mut hidden = word();
    hidden.insert(
        2,
        Entry {
            unlisted: true,
            ..entry("word/hidden.xml", b"<x>SECRET-lfh-1</x>")
        },
    );
    refused(
        "hidden_lfh.docx",
        &zip(&hidden),
        "ambiguous_file",
        &index("hidden_lfh.docx"),
    );

    let mut unicode = word();
    unicode[2].unicode_path = Some("word/other.xml".into());
    refused(
        "unicode_path.docx",
        &zip(&unicode),
        "ambiguous_file",
        &index("unicode_path.docx"),
    );
    let mut agrees = word();
    agrees[2].unicode_path = Some("word/document.xml".into());
    assert_ne!(
        check("unicode_ok.docx", &zip(&agrees))["error"]["code"],
        "ambiguous_file"
    );

    // A second, older end record, as an appended update leaves.
    let old = zip(&word());
    let cd = central_start(&old);
    let mut two = old.clone();
    let new_cd = two.len() as u32;
    two.extend_from_slice(&old[cd..old.len() - 22]);
    two.extend_from_slice(&old[old.len() - 22..old.len() - 6]);
    two.extend(new_cd.to_le_bytes());
    two.extend([0u8; 2]);
    refused(
        "two_eocd.docx",
        &two,
        "ambiguous_file",
        &index("two_eocd.docx"),
    );
}

#[test]
fn an_encrypted_entry_is_damage() {
    let mut crypt = word();
    crypt[2].flags = 1;
    refused(
        "zipcrypto.docx",
        &zip(&crypt),
        "damaged",
        "zipcrypto.docx is damaged: it has an encrypted part, which Office files never have.",
    );
}

#[test]
fn too_many_entries_are_refused() {
    let many: Vec<Entry> = (0..10_001).map(|i| entry(&format!("p{i}"), b"")).collect();
    refused(
        "many.docx",
        &zip(&many),
        "too_large",
        "many.docx is too large to check (its entry count is over 10,000).",
    );
}

#[test]
fn a_cut_short_zip_is_damaged() {
    let bytes = zip(&word());
    let cut = &bytes[..bytes.len() - 40];
    refused(
        "cut.docx",
        cut,
        "damaged",
        "cut.docx is damaged: its structure can't be read.",
    );
}

/// A Word package whose document part is `xml`.
fn document(xml: &[u8]) -> Vec<u8> {
    zip(&with(package("docx", &[], &[]), "word/document.xml", xml))
}

#[test]
fn a_ratio_bomb_stops_at_256_mib() {
    let bomb = with(package("docx", &[], &[]), "word/document.xml", b"");
    let bomb: Vec<Entry> = bomb
        .into_iter()
        .map(|e| {
            if e.name == "word/document.xml" {
                zeros(&e.name, 1 << 30)
            } else {
                e
            }
        })
        .collect();
    refused(
        "ratio_bomb.docx",
        &zip(&bomb),
        "too_large",
        "ratio_bomb.docx is too large to check (part word/document.xml is over 256 MiB unpacked).",
    );
}

#[test]
fn declared_sizes_are_never_trusted() {
    let bytes = zip(&package("xlsx", &[], &[]));
    let cd = central_start(&bytes);
    // The workbook's uncompressed size in the directory, 24 bytes into its record.
    let record = cd
        + bytes[cd..]
            .windows(15)
            .position(|w| w == b"xl/workbook.xml")
            .unwrap()
        - 46;
    let mut huge = bytes.clone();
    huge[record + 24..record + 28].copy_from_slice(&u32::MAX.to_le_bytes());
    let f = check("declared_huge.xlsx", &huge);
    assert_eq!(
        (f["variant"].as_str(), &f["error"]),
        (Some("xlsx"), &serde_json::Value::Null),
        "{f}"
    );

    let f = check("zip64.pptx", &zip_of(&package("pptx", &[], &[]), true));
    assert_eq!(
        (f["variant"].as_str(), &f["error"]),
        (Some("pptx"), &serde_json::Value::Null),
        "{f}"
    );
}

#[test]
fn a_doctype_anywhere_is_damage() {
    let doctype = format!(
        "<?xml version=\"1.0\"?><!DOCTYPE w:document SYSTEM \"file:///etc/passwd\"><w:document xmlns:w=\"{W}\"/>"
    );
    let message = "is damaged: it contains a DOCTYPE declaration, which Office files never use.";
    refused(
        "doctype.docx",
        &document(doctype.as_bytes()),
        "damaged",
        &format!("doctype.docx {message}"),
    );

    let laughs: String = (1..10)
        .map(|i| {
            format!(
                "<!ENTITY lol{i} \"{}\">",
                format!("&lol{};", i - 1).repeat(10)
            )
        })
        .collect();
    let sst = format!(
        "<!DOCTYPE sst [<!ENTITY lol0 \"lol\">{laughs}]><sst xmlns=\"{S}\"><si><t>&lol9;</t></si></sst>"
    );
    let book = package(
        "xlsx",
        &[(
            "xl/sharedStrings.xml",
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sharedStrings+xml",
        )],
        &[entry("xl/sharedStrings.xml", sst.as_bytes())],
    );
    refused(
        "billion_laughs.xlsx",
        &zip(&book),
        "damaged",
        &format!("billion_laughs.xlsx {message}"),
    );
}

#[test]
fn xml_limits_are_too_large_without_running_out_of_stack() {
    let deep = format!(
        "<w:document xmlns:w=\"{W}\">{}{}</w:document>",
        "<w:p>".repeat(100_000),
        "</w:p>".repeat(100_000)
    );
    refused(
        "deep_nest.docx",
        &document(deep.as_bytes()),
        "too_large",
        "deep_nest.docx is too large to check (element nesting in part word/document.xml is over 256 levels).",
    );

    let attr = format!(
        "<w:document xmlns:w=\"{W}\" w:x=\"{}\"/>",
        "a".repeat(40 << 20)
    );
    refused(
        "huge_attr.docx",
        &document(attr.as_bytes()),
        "too_large",
        "huge_attr.docx is too large to check (an attribute value in part word/document.xml is over 32 MiB).",
    );

    let xmlns: String = (0..100_000)
        .map(|i| format!(" xmlns:p{i}=\"urn:x:{i}\""))
        .collect();
    let many = format!("<w:document xmlns:w=\"{W}\"{xmlns}/>");
    let start = Instant::now();
    refused(
        "many_xmlns.docx",
        &document(many.as_bytes()),
        "too_large",
        "many_xmlns.docx is too large to check (the namespace declaration count of one element in part word/document.xml is over 64).",
    );
    assert!(
        start.elapsed() < Duration::from_secs(1),
        "{:?}",
        start.elapsed()
    );
}
