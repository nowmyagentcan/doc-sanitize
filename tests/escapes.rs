//! Data that must not get past a clean, and copies that must not get past the rules for
//! writing them, on files shaped the way other writers and hostile authors shape them.

mod common;

use common::*;
use lopdf::{Dictionary, Document, Object, dictionary};
use serde_json::Value;
use std::fs;
use std::path::Path;
use std::sync::{Arc, Barrier};
use std::thread;

/// Cleans `bytes` as `name` and returns the file's result and the copy's bytes, if any.
fn clean(name: &str, bytes: &[u8]) -> (Value, Option<Vec<u8>>) {
    let dir = dir("escapes");
    put(&dir, &[(name, bytes)]);
    let result = json(&run_in(&dir, &["clean", name, "--json"]))["files"][0].clone();
    let copy = result["output"]
        .as_str()
        .map(|out| fs::read(dir.join(out)).unwrap());
    (result, copy)
}

fn check(name: &str, bytes: &[u8]) -> Value {
    let dir = dir("escapes-check");
    put(&dir, &[(name, bytes)]);
    json(&run_in(&dir, &[name, "--json"]))["files"][0].clone()
}

/// Every inflated entry of a package, or a PDF's bytes followed by its decoded streams.
fn plain(bytes: &[u8]) -> Vec<u8> {
    if bytes.starts_with(b"PK") {
        return unzip(bytes).into_iter().flat_map(|(_, b)| b).collect();
    }
    let mut out = bytes.to_vec();
    for object in Document::load_mem(bytes).unwrap().objects.values() {
        if let Object::Stream(s) = object {
            out.extend(
                s.decompressed_content()
                    .unwrap_or_else(|_| s.content.clone()),
            );
        }
    }
    out
}

/// A clean that says `cleaned` and leaves none of `secret` in the copy.
fn gone(name: &str, bytes: &[u8], secret: &str) {
    let (f, copy) = clean(name, bytes);
    assert_eq!(f["status"], "cleaned", "{f}");
    let plain = String::from_utf8_lossy(&plain(&copy.unwrap())).into_owned();
    assert!(!plain.contains(secret), "{secret} survived in {name}");
}

fn refused(name: &str, bytes: &[u8], code: &str) {
    assert_eq!(check(name, bytes)["error"]["code"], code);
    let (f, copy) = clean(name, bytes);
    assert_eq!(f["error"]["code"], code, "{f}");
    assert!(copy.is_none());
}

// Writing the copy.

#[test]
#[ignore = "known bug: F01"]
fn a_macro_file_named_docx_is_not_replaced_through_its_new_extension() {
    let dir = dir("escapes-input");
    let input = leaky_docx("docm");
    put(&dir, &[("a.docx", &input)]);
    let out = run_in(&dir, &["clean", "a.docx", "-o", "a.docm", "--overwrite"]);
    assert_eq!(
        fs::read(dir.join("a.docx")).unwrap(),
        input,
        "the input changed"
    );
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(stderr(&out).contains("The output would replace the input file."));
}

#[cfg(windows)]
#[test]
fn every_spelling_of_the_input_is_refused_as_the_output() {
    let dir = dir("escapes-spelling");
    let input = twin_docx();
    put(&dir, &[("a.docx", &input), ("sub/keep.txt", b"x")]);
    let absolute = dir.join("A.DOCX");
    for spelling in [
        "A.DOCX",
        ".\\a.docx",
        "sub\\..\\a.docx",
        absolute.to_str().unwrap(),
    ] {
        let out = run_in(&dir, &["clean", "a.docx", "-o", spelling, "--overwrite"]);
        assert_eq!(out.status.code(), Some(2), "{spelling}");
        assert_eq!(fs::read(dir.join("a.docx")).unwrap(), input);
    }
}

#[test]
#[ignore = "known bug: F02"]
fn same_named_files_cleaned_at_once_each_keep_their_own_copy() {
    const RUNS: usize = 16;
    let dir = dir("escapes-race");
    let mark = |i: usize| format!("PUBLIC-RACE-{i:02}");
    for i in 0..RUNS {
        let text = format!("BT /F1 12 Tf 72 700 Td ({}) Tj ET", mark(i));
        put(&dir, &[(&format!("{i}/report.pdf"), &one_page(&text))]);
    }
    // The window is short, so each round starts every run at the same moment.
    for round in 0..10 {
        let start = Arc::new(Barrier::new(RUNS));
        let runs: Vec<_> = (0..RUNS)
            .map(|i| {
                let (dir, start) = (dir.clone(), start.clone());
                thread::spawn(move || {
                    let (input, out) = (format!("{i}/report.pdf"), format!("out{round}"));
                    start.wait();
                    let args = ["clean", &input, "--out-dir", &out, "--json"];
                    json(&run_in(&dir, &args))["files"][0].clone()
                })
            })
            .collect();
        let results: Vec<_> = runs.into_iter().map(|r| r.join().unwrap()).collect();
        let mut outputs = vec![];
        for (i, f) in results.iter().enumerate() {
            assert_eq!(f["status"], "cleaned", "{f}");
            let out = f["output"].as_str().unwrap();
            let copy = plain(&fs::read(dir.join(out)).unwrap());
            assert!(String::from_utf8_lossy(&copy).contains(&mark(i)), "{out}");
            outputs.push(out);
        }
        outputs.sort();
        outputs.dedup();
        assert_eq!(outputs.len(), RUNS);
    }
}

// PDF metadata, forms and actions.

fn with_xmp(xmp: &str) -> Vec<u8> {
    let mut b = PdfBuilder::new();
    b.page("BT /F1 12 Tf 72 700 Td (Visible) Tj ET");
    let dict = dictionary! { "Type" => "Metadata", "Subtype" => "XML" };
    let id = b.stream(dict, xmp.as_bytes());
    b.catalog.set("Metadata", id);
    b.bytes()
}

const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";

#[test]
#[ignore = "known bug: F03"]
fn xmp_is_read_by_namespace_not_by_prefix() {
    let xmp = format!(
        "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><r:RDF xmlns:r=\"{RDF}\"><r:Description r:about=\"\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\"><dc:creator><r:Seq><r:li>SECRET-xmp-1</r:li></r:Seq></dc:creator></r:Description></r:RDF></x:xmpmeta>"
    );
    let pdf = with_xmp(&xmp);
    assert_eq!(check("a.pdf", &pdf)["status"], "found");
    gone("a.pdf", &pdf, "SECRET-xmp-1");
}

#[test]
#[ignore = "known bug: F03"]
fn a_prefix_that_only_looks_like_pdfa_is_not_kept() {
    let xmp = format!(
        "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"{RDF}\"><rdf:Description rdf:about=\"\" xmlns:pdfaid=\"http://example.invalid/not-pdfa/\"><pdfaid:note>SECRET-xmp-2</pdfaid:note></rdf:Description></rdf:RDF></x:xmpmeta>"
    );
    let pdf = with_xmp(&xmp);
    assert_eq!(check("a.pdf", &pdf)["status"], "found");
    gone("a.pdf", &pdf, "SECRET-xmp-2");
}

/// A form of `fields` (their dictionaries, the first of them the only root).
fn form(fields: impl FnOnce(&mut PdfBuilder) -> lopdf::ObjectId) -> Vec<u8> {
    let mut b = PdfBuilder::new();
    b.page("BT /F1 12 Tf 72 700 Td (Visible) Tj ET");
    let root = fields(&mut b);
    b.catalog
        .set("AcroForm", dictionary! { "Fields" => vec![root.into()] });
    b.bytes()
}

fn text_field(name: &str) -> Dictionary {
    dictionary! { "T" => Object::string_literal(name), "FT" => "Tx" }
}

#[test]
#[ignore = "known bug: F04"]
fn a_hidden_fields_inherited_value_is_removed_at_its_source() {
    let pdf = form(|b| {
        let parent = b.doc.new_object_id();
        let mut kid = dictionary! { "T" => Object::string_literal("Child"), "Parent" => parent };
        kid.set("F", 2);
        let kid = b.add(kid);
        let mut d = text_field("Parent");
        d.set("V", Object::string_literal("SECRET-form-1"));
        d.set("Kids", vec![kid.into()]);
        b.doc.objects.insert(parent, d.into());
        parent
    });
    gone("a.pdf", &pdf, "SECRET-form-1");
}

#[test]
#[ignore = "known bug: F04"]
fn a_hidden_fields_saved_default_is_removed_with_its_value() {
    let pdf = form(|b| {
        let mut d = text_field("Hidden");
        d.set("V", Object::string_literal("SECRET-form-2"));
        d.set("DV", Object::string_literal("SECRET-form-3"));
        b.add(d)
    });
    gone("a.pdf", &pdf, "SECRET-form-2");
    gone("a.pdf", &pdf, "SECRET-form-3");
}

#[test]
#[ignore = "known bug: F04"]
fn a_hidden_field_with_only_a_default_is_found_and_removed() {
    let pdf = form(|b| {
        let mut d = text_field("Hidden");
        d.set("DV", Object::string_literal("SECRET-form-4"));
        b.add(d)
    });
    assert_eq!(check("a.pdf", &pdf)["status"], "found");
    gone("a.pdf", &pdf, "SECRET-form-4");
}

#[test]
#[ignore = "known bug: F11"]
fn a_script_on_a_field_with_kids_is_found_and_removed() {
    let pdf = form(|b| {
        let script = dictionary! {
            "S" => "JavaScript", "JS" => Object::string_literal("SECRET-script-1"),
        };
        let script = b.add(script);
        let parent = b.doc.new_object_id();
        let kid = dictionary! { "T" => Object::string_literal("Child"), "Parent" => parent };
        let kid = b.add(kid);
        let mut d = text_field("Parent");
        d.set("Kids", vec![kid.into()]);
        d.set("AA", dictionary! { "K" => script });
        b.doc.objects.insert(parent, d.into());
        parent
    });
    assert_eq!(check("a.pdf", &pdf)["status"], "found");
    gone("a.pdf", &pdf, "SECRET-script-1");
}

#[test]
#[ignore = "known bug: F11"]
fn an_action_whose_type_is_an_indirect_object_is_found_and_removed() {
    let mut b = PdfBuilder::new();
    b.page("BT /F1 12 Tf 72 700 Td (Visible) Tj ET");
    let kind = b.add(Object::Name(b"JavaScript".to_vec()));
    let action = b.add(dictionary! {
        "S" => kind, "JS" => Object::string_literal("SECRET-script-2"),
    });
    b.catalog.set("OpenAction", action);
    let pdf = b.bytes();
    assert_eq!(check("a.pdf", &pdf)["status"], "found");
    gone("a.pdf", &pdf, "SECRET-script-2");
}

#[test]
#[ignore = "known bug: F15"]
fn a_page_timestamp_goes_even_when_nothing_else_is_metadata() {
    let mut b = PdfBuilder::new();
    let mut extra = Dictionary::new();
    extra.set("LastModified", Object::string_literal("D:20260101120000Z"));
    b.page_with("BT /F1 12 Tf 72 700 Td (Visible) Tj ET", extra);
    gone("a.pdf", &b.bytes(), "/LastModified");
}

// Excel.

fn hidden_sheet_cells(cols: &str, row: &str) -> Vec<u8> {
    let xml = format!(
        "<worksheet xmlns=\"{S}\">{cols}<sheetData><row r=\"1\"><c r=\"A1\" t=\"inlineStr\"><is><t>Visible</t></is></c>{row}</row></sheetData></worksheet>"
    );
    workbook(&[("Sheet1", "visible", &xml)]).zip()
}

const HIDDEN_B: &str = "<cols><col min=\"2\" max=\"2\" width=\"9\" hidden=\"1\"/></cols>";

#[test]
#[ignore = "known bug: F06"]
fn a_hidden_formula_with_no_saved_result_is_removed() {
    let cell = "<c r=\"B1\"><f>\"SECRET-formula-1\"</f><v></v></c>";
    let xlsx = hidden_sheet_cells(HIDDEN_B, cell);
    assert_eq!(check("a.xlsx", &xlsx)["status"], "found");
    gone("a.xlsx", &xlsx, "SECRET-formula-1");
}

#[test]
#[ignore = "known bug: F06"]
fn a_hidden_formula_that_gives_an_empty_text_is_removed() {
    // What Excel saves for =IF(A1>1,"…","") when the condition is false.
    let cell = "<c r=\"B1\" t=\"str\"><f>IF(A1&gt;1,\"SECRET-formula-2\",\"\")</f><v></v></c>";
    let xlsx = hidden_sheet_cells(HIDDEN_B, cell);
    assert_eq!(check("a.xlsx", &xlsx)["status"], "found");
    gone("a.xlsx", &xlsx, "SECRET-formula-2");
}

#[test]
fn a_cell_with_no_style_of_its_own_ignores_its_columns_style() {
    // Excel shows B1 in General: a column's or row's style is for cells that have no record.
    let styles = "<numFmts count=\"1\"><numFmt numFmtId=\"164\" formatCode=\";;;\"/></numFmts><fonts count=\"1\"><font/></fonts><fills count=\"1\"><fill/></fills><borders count=\"1\"><border/></borders><cellXfs count=\"2\"><xf numFmtId=\"0\" fontId=\"0\" fillId=\"0\"/><xf numFmtId=\"164\" fontId=\"0\" fillId=\"0\"/></cellXfs>";
    let xml = format!(
        "<worksheet xmlns=\"{S}\"><cols><col min=\"2\" max=\"2\" width=\"9\" style=\"1\"/></cols><sheetData><row r=\"1\"><c r=\"B1\" t=\"inlineStr\"><is><t>Shown</t></is></c></row></sheetData></worksheet>"
    );
    let xlsx = workbook(&[("Sheet1", "visible", &xml)])
        .part(
            "xl/workbook.xml",
            "rId9",
            "styles",
            "xl/styles.xml",
            "application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml",
            styles_xml(styles).as_bytes(),
        )
        .zip();
    let f = check("a.xlsx", &xlsx);
    assert!(findings_of(&f, "hidden-text").is_empty(), "{f}");
}

// PowerPoint.

fn hidden_in(kind: &str, root: &str, part: &str, ct: &str, secret: &str) -> Vec<u8> {
    let body = slide_xml(&shape(9, true, &xfrm(0, 0, 100, 100), secret)).replace("p:sld", root);
    presentation(&[&slide_xml("")])
        .part(
            "ppt/presentation.xml",
            "rId50",
            kind,
            part,
            ct,
            body.as_bytes(),
        )
        .zip()
}

#[test]
#[ignore = "known bug: F07"]
fn a_hidden_shape_on_a_master_is_found_and_removed() {
    let ct = "application/vnd.openxmlformats-officedocument.presentationml.slideMaster+xml";
    let part = "ppt/slideMasters/slideMaster1.xml";
    let pptx = hidden_in("slideMaster", "p:sldMaster", part, ct, "SECRET-master-1");
    assert_eq!(check("a.pptx", &pptx)["status"], "found");
    gone("a.pptx", &pptx, "SECRET-master-1");
}

#[test]
#[ignore = "known bug: F07"]
fn a_hidden_shape_on_a_layout_is_found_and_removed() {
    let ct = "application/vnd.openxmlformats-officedocument.presentationml.slideLayout+xml";
    let part = "ppt/slideLayouts/slideLayout1.xml";
    let pptx = hidden_in("slideLayout", "p:sldLayout", part, ct, "SECRET-layout-1");
    assert_eq!(check("a.pptx", &pptx)["status"], "found");
    gone("a.pptx", &pptx, "SECRET-layout-1");
}

// Word.

fn word(xml: &[u8]) -> Vec<u8> {
    zip(&with(package("docx", &[], &[]), "word/document.xml", xml))
}

fn utf16(bom: [u8; 2], encode: fn(u16) -> [u8; 2]) -> Vec<u8> {
    let run = text_run("<w:vanish/>", "SECRET-utf16-1");
    let body = doc_xml(&format!("{}{}", para(&run), para(&text_run("", "Shown"))));
    let xml = format!("<?xml version=\"1.0\" encoding=\"UTF-16\"?>{body}");
    let mut bytes = bom.to_vec();
    bytes.extend(xml.encode_utf16().flat_map(encode));
    word(&bytes)
}

/// The part as text, by its byte order mark, and refusing a declaration its bytes don't match.
fn honest_text(part: &[u8]) -> String {
    let units = |f: fn([u8; 2]) -> u16| {
        let units: Vec<u16> = part[2..].chunks(2).map(|c| f([c[0], c[1]])).collect();
        String::from_utf16(&units).unwrap()
    };
    match part {
        [0xFF, 0xFE, ..] => units(u16::from_le_bytes),
        [0xFE, 0xFF, ..] => units(u16::from_be_bytes),
        _ => {
            let text = String::from_utf8(part.to_vec()).unwrap();
            let head = &text[..text.find("?>").unwrap_or(0)];
            assert!(
                !head.contains("UTF-16"),
                "UTF-8 bytes declared UTF-16: {head}"
            );
            text
        }
    }
}

#[test]
#[ignore = "known bug: F08"]
fn a_utf16_part_stays_readable_after_an_edit() {
    for docx in [
        utf16([0xFF, 0xFE], u16::to_le_bytes),
        utf16([0xFE, 0xFF], u16::to_be_bytes),
    ] {
        let (f, copy) = clean("a.docx", &docx);
        assert_eq!(f["status"], "cleaned", "{f}");
        let parts = unzip(&copy.unwrap());
        let document = parts
            .iter()
            .find(|(n, _)| n == "word/document.xml")
            .unwrap();
        let text = honest_text(&document.1);
        assert!(text.contains("Shown") && !text.contains("SECRET"), "{text}");
    }
}

#[test]
#[ignore = "known bug: F09"]
fn names_equal_after_percent_decoding_are_two_versions_of_a_part() {
    let other = doc_xml(&para(&text_run("", "SECRET-alias-1")));
    for alias in ["word/%64ocument.xml", "word%2Fdocument.xml"] {
        let mut entries = package("docx", &[], &[]);
        entries.push(entry(alias, other.as_bytes()));
        refused("a.docx", &zip(&entries), "ambiguous_file");
    }
}

#[test]
#[ignore = "known bug: F10"]
fn tracking_ids_in_style_definitions_are_removed() {
    let styles = format!(
        "<w:styles xmlns:w=\"{W}\"><w:style w:type=\"paragraph\" w:styleId=\"Custom\"><w:name w:val=\"Custom\"/><w:rsid w:val=\"00AB12CD\"/></w:style></w:styles>"
    );
    let ct = "application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml";
    let docx = Pkg::new("docx")
        .part(
            "word/document.xml",
            "rId5",
            "styles",
            "word/styles.xml",
            ct,
            styles.as_bytes(),
        )
        .zip();
    gone("a.docx", &docx, "00AB12CD");
}

#[test]
#[ignore = "known bug: F12"]
fn xml_no_parser_accepts_is_damage() {
    let root = |rest: &str| format!("<w:document xmlns:w=\"{W}\">{rest}</w:document>");
    for xml in [
        root("<w:body><w:p><w:r><w:t>a\0b</w:t></w:r></w:p></w:body>"),
        root("<w:body><w:p a=\"1\" a=\"2\"/></w:body>"),
        "<w:document><w:body/></w:document>".to_string(),
    ] {
        refused("a.docx", &word(xml.as_bytes()), "damaged");
    }
}

#[test]
#[ignore = "known bug: F14"]
fn a_story_part_over_64_mib_is_too_large_and_one_under_is_read() {
    let story = |size: usize| {
        let unit = "<w:p/>";
        let xml = doc_xml(&unit.repeat(size / unit.len()));
        word(xml.as_bytes())
    };
    assert_eq!(check("a.docx", &story(63 << 20))["status"], "none_found");
    refused("a.docx", &story(65 << 20), "too_large");
}

// Names from the file stay out of the default output.

#[test]
#[ignore = "known bug: F13"]
fn an_entry_name_that_is_not_a_part_name_is_not_printed() {
    let mut entries = package("docx", &[], &[]);
    entries.push(entry("C:/SECRET-entry-1/draft.txt", b"x"));
    let dir = dir("escapes-names");
    put(&dir, &[("a.docx", &zip(&entries))]);
    let out = run_in(&dir, &["a.docx", "--json"]);
    assert!(!stdout(&out).contains("SECRET"), "{}", stdout(&out));
    assert!(!stderr(&out).contains("SECRET"));
}

// What the README says.

const README: &str = include_str!("../README.md");

#[test]
#[ignore = "known bug: F18"]
fn the_readme_promises_json_only_where_it_exists() {
    for command in ["ui", "mcp"] {
        assert_eq!(run(&[command, "--json"]).status.code(), Some(2));
    }
    assert!(!README.contains("`--json` on any command"));
}

#[test]
#[ignore = "known bug: F19"]
fn the_readme_credits_the_rules_it_ported() {
    for name in ["Open-XML-PowerTools", "Clippit", "x-ray"] {
        assert!(README.contains(name), "{name}");
    }
}

#[test]
fn every_readme_example_is_a_valid_command() {
    let dir = dir("escapes-readme");
    put(
        &dir,
        &[
            ("contract.docx", &leaky_docx("docx")),
            ("deck.pptx", &leaky_pptx("pptx")),
            ("redacted.pdf", &redacted()),
            ("filing.pdf", &redacted()),
            ("exports/a.docx", &twin_docx()),
        ],
    );
    let examples: Vec<&str> = README
        .lines()
        .filter(|l| l.starts_with("doc-sanitize ") && !l.contains("interactive"))
        .collect();
    assert!(examples.len() >= 8, "{examples:?}");
    for line in examples {
        let args: Vec<&str> = line
            .split('#')
            .next()
            .unwrap()
            .split_whitespace()
            .skip(1)
            .collect();
        let out = run_in(&dir, &args);
        assert!(
            matches!(out.status.code(), Some(0 | 1)),
            "{line}: {}",
            stderr(&out)
        );
    }
    assert!(Path::new(&dir.join("outbox/a.clean.docx")).exists());
}
