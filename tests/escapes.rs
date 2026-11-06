//! Data that must not get past a clean, and copies that must not get past the rules for
//! writing them, on files shaped the way other writers and hostile authors shape them.

mod common;

use common::*;
use lopdf::{Dictionary, Document, Object, dictionary};
use serde_json::Value;
use std::fs;
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
fn xmp_is_read_by_namespace_not_by_prefix() {
    let xmp = format!(
        "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><r:RDF xmlns:r=\"{RDF}\"><r:Description r:about=\"\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\"><dc:creator><r:Seq><r:li>SECRET-xmp-1</r:li></r:Seq></dc:creator></r:Description></r:RDF></x:xmpmeta>"
    );
    let pdf = with_xmp(&xmp);
    assert_eq!(check("a.pdf", &pdf)["status"], "found");
    gone("a.pdf", &pdf, "SECRET-xmp-1");
}

#[test]
fn a_prefix_that_only_looks_like_pdfa_is_not_kept() {
    let xmp = format!(
        "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"{RDF}\"><rdf:Description rdf:about=\"\" xmlns:pdfaid=\"http://example.invalid/not-pdfa/\"><pdfaid:note>SECRET-xmp-2</pdfaid:note></rdf:Description></rdf:RDF></x:xmpmeta>"
    );
    let pdf = with_xmp(&xmp);
    assert_eq!(check("a.pdf", &pdf)["status"], "found");
    gone("a.pdf", &pdf, "SECRET-xmp-2");
}

#[test]
fn only_a_packet_holding_nothing_but_pdfa_identification_is_left_alone() {
    let secret = format!("SECRET-{}", "xmp-3");
    let in_rdf = |inner: &str| {
        format!(
            "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"{RDF}\">{inner}</rdf:RDF></x:xmpmeta>"
        )
    };
    let id = "<rdf:Description rdf:about=\"\" xmlns:pdfaid=\"http://www.aiim.org/pdfa/ns/id/\" pdfaid:part=\"2\" pdfaid:conformance=\"B\"/>";
    assert_eq!(
        check("a.pdf", &with_xmp(&in_rdf(id)))["status"],
        "none_found"
    );
    for xmp in [
        format!("<a>{secret}"),
        format!("<a>{secret}</a>"),
        in_rdf(&format!("<!-- {secret} -->{id}")),
        in_rdf(&format!("{secret}{id}")),
        in_rdf(&id.replace("rdf:about=\"\"", &format!("rdf:about=\"{secret}\""))),
        in_rdf(&id.replace("pdfaid:part=\"2\"", &format!("pdfaid:part=\"{secret}\""))),
        in_rdf(&id.replace("pdfaid:part=\"2\"", &format!("pdfaid:note=\"{secret}\""))),
        in_rdf(id).replace("<rdf:RDF", &format!("<rdf:RDF x:tool=\"{secret}\" ")),
    ] {
        let pdf = with_xmp(&xmp);
        assert_eq!(check("a.pdf", &pdf)["status"], "found", "{xmp}");
        gone("a.pdf", &pdf, &secret);
    }
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
fn a_value_a_shown_field_inherits_stays_and_its_hidden_sibling_hides_it() {
    let mut b = PdfBuilder::new();
    let page = b.page("BT /F1 12 Tf 72 700 Td (Visible) Tj ET");
    let parent = b.doc.new_object_id();
    let kid = |name: &str, flags: i64| {
        dictionary! {
            "Type" => "Annot", "Subtype" => "Widget", "T" => Object::string_literal(name),
            "Rect" => vec![10.into(), 10.into(), 90.into(), 30.into()],
            "F" => flags, "Parent" => parent,
        }
    };
    let (shown, hidden) = (b.add(kid("Shown", 4)), b.add(kid("Hidden", 2)));
    let mut d = text_field("Parent");
    d.set("V", Object::string_literal("PUBLIC-shared"));
    d.set("Kids", vec![shown.into(), hidden.into()]);
    b.doc.objects.insert(parent, d.into());
    b.doc
        .get_dictionary_mut(page)
        .unwrap()
        .set("Annots", vec![shown.into(), hidden.into()]);
    b.catalog
        .set("AcroForm", dictionary! { "Fields" => vec![parent.into()] });
    let (f, copy) = clean("a.pdf", &b.bytes());
    assert_eq!(f["status"], "cleaned", "{f}");
    let copy = String::from_utf8_lossy(&plain(&copy.unwrap())).into_owned();
    assert!(copy.contains("PUBLIC-shared"), "the shown value went");
}

#[test]
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
fn an_action_is_found_wherever_clean_would_cut_it() {
    let secret = format!("SECRET-{}", "script-3");
    let script = |key: &str| {
        dictionary! { "S" => key, "JS" => Object::string_literal(secret.as_str()) }
    };
    // A bookmark's action.
    let mut b = PdfBuilder::new();
    b.page("BT /F1 12 Tf 72 700 Td (Visible) Tj ET");
    let action = b.add(script("JavaScript"));
    let outlines = b.doc.new_object_id();
    let item = b.add(dictionary! {
        "Title" => Object::string_literal("Start"), "Parent" => outlines, "A" => action,
    });
    let root = dictionary! { "Type" => "Outlines", "First" => item, "Last" => item, "Count" => 1 };
    b.doc.objects.insert(outlines, root.into());
    b.catalog.set("Outlines", outlines);
    let bookmark = b.bytes();
    // A script on an action of another kind.
    let mut b = PdfBuilder::new();
    let mut extra = Dictionary::new();
    extra.set("AA", dictionary! { "O" => script("Rendition") });
    b.page_with("BT /F1 12 Tf 72 700 Td (Visible) Tj ET", extra);
    for pdf in [bookmark, b.bytes()] {
        assert_eq!(check("a.pdf", &pdf)["status"], "found");
        gone("a.pdf", &pdf, &secret);
    }
}

#[test]
fn a_link_to_a_local_file_held_in_an_object_of_its_own_is_found_and_removed() {
    let mut b = PdfBuilder::new();
    b.page("BT /F1 12 Tf 72 700 Td (Visible) Tj ET");
    let secret = format!("file:///SECRET-{}", "uri-1");
    let uri = b.add(Object::string_literal(secret.as_str()));
    let action = b.add(dictionary! { "S" => "URI", "URI" => uri });
    b.catalog.set("OpenAction", action);
    let pdf = b.bytes();
    assert_eq!(check("a.pdf", &pdf)["status"], "found");
    gone("a.pdf", &pdf, &secret);
}

#[test]
fn a_page_timestamp_goes_even_when_nothing_else_is_metadata() {
    let mut b = PdfBuilder::new();
    let mut extra = Dictionary::new();
    extra.set("LastModified", Object::string_literal("D:20260101120000Z"));
    b.page_with("BT /F1 12 Tf 72 700 Td (Visible) Tj ET", extra);
    gone("a.pdf", &b.bytes(), "/LastModified");
}

#[test]
fn an_update_section_with_no_objects_is_not_an_earlier_revision() {
    // Office's PDF export ends with `xref 0 0` and a trailer pointing back at the real table.
    let mut pdf = raw(&raw_page(""), "/Root 1 0 R");
    let table = pdf.windows(5).position(|w| w == b"xref\n").unwrap();
    let update = pdf.len();
    let tail = format!(
        "xref\n0 0\ntrailer\n<< /Size 4 /Root 1 0 R /Prev {table} >>\nstartxref\n{update}\n%%EOF\n"
    );
    pdf.extend(tail.as_bytes());
    let f = check("a.pdf", &pdf);
    assert!(findings_of(&f, "old-revisions").is_empty(), "{f}");
}

// Excel.

/// A workbook whose hidden column B holds `cell`.
fn hidden_column(cell: &str) -> Vec<u8> {
    let xml = format!(
        "<worksheet xmlns=\"{S}\"><cols><col min=\"2\" max=\"2\" width=\"9\" hidden=\"1\"/></cols><sheetData><row r=\"1\"><c r=\"A1\" t=\"inlineStr\"><is><t>Visible</t></is></c>{cell}</row></sheetData></worksheet>"
    );
    workbook(&[("Sheet1", "visible", &xml)]).zip()
}

#[test]
fn a_hidden_formula_with_no_saved_result_is_removed() {
    let cell = "<c r=\"B1\"><f>\"SECRET-formula-1\"</f><v></v></c>";
    let xlsx = hidden_column(cell);
    assert_eq!(check("a.xlsx", &xlsx)["status"], "found");
    gone("a.xlsx", &xlsx, "SECRET-formula-1");
}

#[test]
fn a_hidden_formula_that_gives_an_empty_text_is_removed() {
    // What Excel saves for =IF(A1>1,"…","") when the condition is false.
    let cell = "<c r=\"B1\" t=\"str\"><f>IF(A1&gt;1,\"SECRET-formula-2\",\"\")</f><v/></c>";
    let xlsx = hidden_column(cell);
    assert_eq!(check("a.xlsx", &xlsx)["status"], "found");
    gone("a.xlsx", &xlsx, "SECRET-formula-2");
}

#[test]
fn a_hidden_formula_with_no_result_shows_its_formula_when_values_are_asked_for() {
    let dir = dir("escapes-formula");
    let cell = "<c r=\"B1\"><f>\"PUBLIC-formula\"</f></c>";
    put(&dir, &[("a.xlsx", &hidden_column(cell))]);
    let f = &json(&run_in(&dir, &["a.xlsx", "--json", "--show-values"]))["files"][0];
    let found = findings_of(f, "hidden-rows-columns");
    assert_eq!(found[0]["value"], "\"PUBLIC-formula\"", "{f}");
}

#[test]
fn a_shared_formula_with_no_text_or_result_still_counts_as_a_hidden_cell() {
    let cell = "<c r=\"B1\"><f t=\"shared\" si=\"0\"/></c>";
    let f = check("a.xlsx", &hidden_column(cell));
    assert_eq!(findings_of(&f, "hidden-rows-columns").len(), 1, "{f}");
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
fn a_hidden_shape_on_a_master_is_found_and_removed() {
    let ct = "application/vnd.openxmlformats-officedocument.presentationml.slideMaster+xml";
    let part = "ppt/slideMasters/slideMaster1.xml";
    let pptx = hidden_in("slideMaster", "p:sldMaster", part, ct, "SECRET-master-1");
    assert_eq!(check("a.pptx", &pptx)["status"], "found");
    gone("a.pptx", &pptx, "SECRET-master-1");
}

#[test]
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
fn names_equal_after_percent_decoding_are_two_versions_of_a_part() {
    let other = doc_xml(&para(&text_run("", "SECRET-alias-1")));
    for alias in ["word/%64ocument.xml", "word%2Fdocument.xml"] {
        let mut entries = package("docx", &[], &[]);
        entries.push(entry(alias, other.as_bytes()));
        refused("a.docx", &zip(&entries), "ambiguous_file");
    }
}

#[test]
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
fn a_story_part_over_64_mib_is_too_large_and_one_under_is_read() {
    let story = |size: usize| {
        let unit = "<w:p/>";
        let xml = doc_xml(&unit.repeat(size / unit.len()));
        word(xml.as_bytes())
    };
    assert_eq!(check("a.docx", &story(63 << 20))["status"], "none_found");
    refused("a.docx", &story(65 << 20), "too_large");
}

#[test]
fn a_part_clean_must_read_as_a_tree_is_bounded_and_one_it_streams_is_not() {
    let filler = |size: usize| format!("<a>{}</a>", "<b/>".repeat(size / 4)).into_bytes();
    let big = 100 << 20;
    // A tracking id, so clean reads every part it keeps.
    let docx = Pkg::new("docx")
        .body(&doc_xml(TRACKED))
        .part(
            "word/document.xml",
            "rId5",
            "settings",
            "word/settings.xml",
            "application/vnd.openxmlformats-officedocument.wordprocessingml.settings+xml",
            &filler(big),
        )
        .zip();
    assert_eq!(check("a.docx", &docx)["error"], Value::Null);
    let (f, copy) = clean("a.docx", &docx);
    assert_eq!(f["error"]["code"], "too_large", "{f}");
    assert!(copy.is_none());
    let row = "<row><c><v>1</v></c></row>";
    let sheet = format!(
        "<worksheet xmlns=\"{S}\"><sheetData>{}</sheetData></worksheet>",
        row.repeat(big / row.len())
    );
    let f = check("a.xlsx", &workbook(&[("Sheet1", "visible", &sheet)]).zip());
    assert_eq!(f["error"], Value::Null, "{f}");
}

const TRACKED: &str = "<w:p w:rsidR=\"00AB12CD\"/>";

/// A workbook whose sheet has a hidden column and a comment of `tags` tag openers: a comment
/// is one node, so a test of the limit can't take the memory the limit is there to prevent.
fn padded_sheet(tags: usize) -> Vec<u8> {
    let sheet = format!(
        "<worksheet xmlns=\"{S}\"><cols><col min=\"2\" max=\"2\" width=\"9\" hidden=\"1\"/></cols><sheetData><row r=\"1\"><c r=\"B1\"><v>7</v></c></row></sheetData><!--{}--></worksheet>",
        "<".repeat(tags)
    );
    workbook(&[("Sheet1", "visible", &sheet)]).zip()
}

#[test]
fn a_workbook_part_too_big_to_edit_is_checked_but_not_cleaned() {
    let big = padded_sheet(16_000_001);
    assert_eq!(check("a.xlsx", &big)["status"], "found");
    let (f, copy) = clean("a.xlsx", &big);
    assert_eq!(f["error"]["code"], "too_large", "{f}");
    assert!(
        f["error"]["message"]
            .as_str()
            .unwrap()
            .contains("too large to clean")
    );
    assert!(copy.is_none());
    let (f, copy) = clean("a.xlsx", &padded_sheet(15_000_000));
    assert_eq!(f["status"], "cleaned", "{f}");
    assert!(copy.is_some());
}

#[test]
fn only_a_part_clean_reads_as_a_tree_is_held_to_the_limit_on_trees() {
    let secret = format!("SECRET-{}", "custom-1");
    let docx = |filler: String| {
        let xml = format!("<a>{secret}<!--{filler}--></a>");
        Pkg::new("docx")
            .body(&doc_xml(TRACKED))
            .part(
                "word/document.xml",
                "rId5",
                "customXml",
                "customXml/item1.xml",
                "application/xml",
                xml.as_bytes(),
            )
            .zip()
    };
    // Only stories, slides, drawings and charts are held to 64 MiB; custom XML is streamed, and
    // clean deletes it unread.
    let over = docx(" ".repeat(65 << 20));
    assert_eq!(check("a.docx", &over)["status"], "found");
    gone("a.docx", &over, &secret);
    // Kept, it is read as a tree like every part that stays.
    let dir = dir("escapes-kept");
    put(&dir, &[("a.docx", &docx("<".repeat(16_000_001)))]);
    let out = run_in(
        &dir,
        &["clean", "a.docx", "--keep", "custom-data", "--json"],
    );
    assert_eq!(json(&out)["files"][0]["error"]["code"], "too_large");
}

#[test]
fn a_package_stored_inside_a_package_is_not_a_second_end_record() {
    // Word and PowerPoint store a chart's workbook without compressing it.
    let workbook = zip(&package("xlsx", &[], &[]));
    let ct = "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet";
    let docx = Pkg::new("docx")
        .part(
            "word/document.xml",
            "rId5",
            "package",
            "word/embeddings/Microsoft_Excel_Worksheet.xlsx",
            ct,
            &workbook,
        )
        .zip();
    let f = check("a.docx", &docx);
    assert_eq!(f["error"], Value::Null, "{f}");
    let (f, copy) = clean("a.docx", &docx);
    assert_eq!(f["status"], "cleaned", "{f}");
    assert!(copy.is_some());
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
    assert!(dir.join("outbox/a.clean.docx").exists());
}

// Text boxes, which Word writes twice: as DrawingML and as VML for older readers.

/// A Word text box holding `inner`, in both forms, the way Word writes it.
fn text_box(inner: &str) -> String {
    format!(
        "<w:r><mc:AlternateContent><mc:Choice Requires=\"wps\"><w:drawing><wp:anchor><wp:docPr id=\"7\" name=\"Text Box 1\"/><a:graphic><a:graphicData uri=\"wps\"><wps:wsp xmlns:wps=\"http://schemas.microsoft.com/office/word/2010/wordprocessingShape\"><wps:txbx><w:txbxContent>{inner}</w:txbxContent></wps:txbx></wps:wsp></a:graphicData></a:graphic></wp:anchor></w:drawing></mc:Choice><mc:Fallback><w:pict><v:shape id=\"b\" style=\"position:absolute\"><v:textbox><w:txbxContent>{inner}</w:txbxContent></v:textbox></v:shape></w:pict></mc:Fallback></mc:AlternateContent></w:r>"
    )
}

/// `body` cleaned as a Word document with `rels`, as the copy's document part. The box must
/// still say `KEEPBOX` in both its forms.
fn box_survives(body: &str, mut pkg: Pkg, rels: &[String]) -> String {
    for r in rels {
        pkg = pkg.link("word/document.xml", r.clone());
    }
    let (f, copy) = clean("box.docx", &pkg.body(&doc_xml(&para(body))).zip());
    assert_eq!(f["status"], "cleaned", "{f}");
    let parts = unzip(&copy.unwrap());
    let doc = &parts.iter().find(|p| p.0 == "word/document.xml").unwrap().1;
    let doc = String::from_utf8_lossy(doc).into_owned();
    assert_eq!(doc.matches("KEEPBOX").count(), 2, "{doc}");
    doc
}

#[test]
fn a_tracked_change_in_a_text_box_keeps_the_box() {
    let keep = para(&text_run("", "KEEPBOX"));
    let changed = format!(
        "{keep}<w:p><w:del w:id=\"1\" {BY_SAM}><w:r><w:delText>GONE</w:delText></w:r></w:del><w:r><w:rPr><w:b/><w:rPrChange w:id=\"2\" {BY_SAM}><w:rPr/></w:rPrChange></w:rPr><w:t>x</w:t></w:r></w:p>"
    );
    let doc = box_survives(&text_box(&changed), Pkg::new("docx"), &[]);
    assert!(!doc.contains("GONE") && !doc.contains("w:del") && !doc.contains("rPrChange"));
}

#[test]
fn a_bound_content_control_in_a_text_box_keeps_the_box() {
    let bound = format!(
        "<w:sdt><w:sdtPr><w:dataBinding w:xpath=\"/x\" w:storeItemID=\"{{1}}\"/></w:sdtPr><w:sdtContent>{}</w:sdtContent></w:sdt>",
        para(&text_run("", "KEEPBOX"))
    );
    let doc = box_survives(&text_box(&bound), Pkg::new("docx"), &[]);
    assert!(!doc.contains("dataBinding"));
}

#[test]
fn a_comment_and_a_permission_in_a_text_box_keep_the_box() {
    let marked = format!(
        "<w:p w:rsidR=\"00112233\"><w:permStart w:id=\"1\" w:edGrp=\"everyone\"/><w:commentRangeStart w:id=\"0\"/>{}<w:commentRangeEnd w:id=\"0\"/><w:r><w:commentReference w:id=\"0\"/></w:r><w:permEnd w:id=\"1\"/></w:p>",
        text_run("", "KEEPBOX")
    );
    let notes = format!("<w:comments xmlns:w=\"{W}\"><w:comment w:id=\"0\"/></w:comments>");
    let pkg = Pkg::new("docx").part(
        "word/document.xml",
        "rId8",
        "comments",
        "word/comments.xml",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.comments+xml",
        notes.as_bytes(),
    );
    let doc = box_survives(&text_box(&marked), pkg, &[]);
    assert!(!doc.contains("commentRange") && !doc.contains("permStart"));
}

#[test]
fn a_linked_picture_in_a_text_box_keeps_the_box() {
    let picture = "<w:r><w:drawing><wp:inline><wp:docPr id=\"8\" name=\"p\"/><a:graphic><a:graphicData uri=\"pic\"><pic:pic><pic:blipFill><a:blip r:link=\"rId9\"/></pic:blipFill></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r>";
    let inner = para(&format!("{}{picture}", text_run("", "KEEPBOX")));
    let doc = box_survives(
        &text_box(&inner),
        Pkg::new("docx"),
        &[external("rId9", "image", "file:///C:/private/a.png")],
    );
    assert!(!doc.contains("r:link"));
}

#[test]
fn an_object_wrapped_in_alternatives_goes_with_its_wrapper() {
    let sheet = format!(
        "<worksheet xmlns=\"{S}\" xmlns:r=\"{R}\" xmlns:mc=\"http://schemas.openxmlformats.org/markup-compatibility/2006\"><sheetData/><mc:AlternateContent><mc:Choice Requires=\"x14\"><oleObjects><oleObject progId=\"Package\" shapeId=\"1\"/></oleObjects></mc:Choice><mc:Fallback><oleObjects><oleObject progId=\"Package\" shapeId=\"1\"/></oleObjects></mc:Fallback></mc:AlternateContent></worksheet>"
    );
    let pkg = workbook(&[("One", "visible", &sheet)]).part(
        "xl/worksheets/sheet1.xml",
        "rId7",
        "package",
        "xl/embeddings/a.bin",
        "application/vnd.openxmlformats-officedocument.oleObject",
        b"data",
    );
    let (f, copy) = clean("wrapped.xlsx", &pkg.zip());
    assert_eq!(f["status"], "cleaned", "{f}");
    let plain = String::from_utf8_lossy(&plain(&copy.unwrap())).into_owned();
    assert!(!plain.contains("AlternateContent"), "{plain}");
}

// Hidden shapes, one by one.

#[test]
fn a_hidden_word_text_box_goes_in_both_its_forms() {
    let word = text_box(&para(&text_run("", "HIDDENBOX"))).replace(
        "<wp:docPr id=\"7\" name=\"Text Box 1\"/>",
        "<wp:docPr id=\"7\" name=\"Text Box 1\" hidden=\"1\"/>",
    );
    let word = word.replace("position:absolute", "position:absolute;visibility:hidden");
    let keep = para(&text_run("", "KEEPBODY"));
    let pkg = Pkg::new("docx").body(&doc_xml(&format!("{}{keep}", para(&word))));
    let (f, copy) = clean("shape.docx", &pkg.zip());
    assert_eq!(f["status"], "cleaned", "{f}");
    let plain = String::from_utf8_lossy(&plain(&copy.unwrap())).into_owned();
    assert!(!plain.contains("HIDDENBOX") && plain.contains("KEEPBODY"));
}

#[test]
fn a_hidden_picture_in_a_text_box_leaves_the_box() {
    let picture = "<w:r><w:drawing><wp:inline><wp:docPr id=\"8\" name=\"p\" hidden=\"1\" descr=\"HIDDENALT\"/></wp:inline></w:drawing></w:r>";
    let inner = para(&format!("{}{picture}", text_run("", "KEEPBOX")));
    let doc = box_survives(&text_box(&inner), Pkg::new("docx"), &[]);
    assert!(!doc.contains("HIDDENALT"));
}

/// A workbook whose first sheet has a drawing part holding `anchors`.
fn with_drawing(anchors: &str) -> Vec<u8> {
    let drawing = format!(
        "<xdr:wsDr xmlns:xdr=\"http://schemas.openxmlformats.org/drawingml/2006/spreadsheetDrawing\" xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\">{anchors}</xdr:wsDr>"
    );
    let sheet = format!(
        "<worksheet xmlns=\"{S}\" xmlns:r=\"{R}\"><sheetData/><drawing r:id=\"rId1\"/></worksheet>"
    );
    workbook(&[("One", "visible", &sheet)])
        .part(
            "xl/worksheets/sheet1.xml",
            "rId1",
            "drawing",
            "xl/drawings/drawing1.xml",
            "application/vnd.openxmlformats-officedocument.drawing+xml",
            drawing.as_bytes(),
        )
        .zip()
}

fn excel_shape(id: u32, hidden: &str, text: &str) -> String {
    format!(
        "<xdr:sp><xdr:nvSpPr><xdr:cNvPr id=\"{id}\" name=\"s\"{hidden}/><xdr:cNvSpPr/></xdr:nvSpPr><xdr:spPr/><xdr:txBody><a:p><a:r><a:t>{text}</a:t></a:r></a:p></xdr:txBody></xdr:sp>"
    )
}

fn excel_group(members: &str) -> String {
    format!(
        "<xdr:twoCellAnchor><xdr:from/><xdr:to/><xdr:grpSp><xdr:nvGrpSpPr><xdr:cNvPr id=\"20\" name=\"g\"/><xdr:cNvGrpSpPr/></xdr:nvGrpSpPr><xdr:grpSpPr/>{members}</xdr:grpSp><xdr:clientData/></xdr:twoCellAnchor>"
    )
}

#[test]
fn a_hidden_shape_in_an_excel_group_leaves_its_visible_siblings() {
    let hidden = " hidden=\"1\"";
    let members = excel_shape(2, "", "KEEPSHAPE") + &excel_shape(3, hidden, "HIDDENSHAPE");
    let (f, copy) = clean("group.xlsx", &with_drawing(&excel_group(&members)));
    assert_eq!(f["status"], "cleaned", "{f}");
    let plain = String::from_utf8_lossy(&plain(&copy.unwrap())).into_owned();
    assert!(plain.contains("KEEPSHAPE"), "the visible sibling went");
    assert!(!plain.contains("HIDDENSHAPE"));
}

#[test]
fn a_group_with_every_member_hidden_goes_with_its_anchor() {
    let hidden = " hidden=\"1\"";
    let members = excel_shape(2, hidden, "HIDDENA") + &excel_shape(3, hidden, "HIDDENB");
    let (f, copy) = clean("all.xlsx", &with_drawing(&excel_group(&members)));
    assert_eq!(f["status"], "cleaned", "{f}");
    let plain = String::from_utf8_lossy(&plain(&copy.unwrap())).into_owned();
    assert!(
        !plain.contains("twoCellAnchor") && !plain.contains("HIDDEN"),
        "{plain}"
    );
}

#[test]
fn a_hidden_shape_in_a_word_group_leaves_its_visible_siblings() {
    let member = |hidden: &str, text: &str| {
        format!(
            "<wps:wsp><wps:cNvPr id=\"3\" name=\"m\"{hidden}/><wps:spPr/><wps:txbx><w:txbxContent>{}</w:txbxContent></wps:txbx></wps:wsp>",
            para(&text_run("", text))
        )
    };
    let group = format!(
        "<w:r><w:drawing><wp:anchor><wp:docPr id=\"7\" name=\"g\"/><a:graphic><a:graphicData uri=\"wpg\"><wpg:wgp xmlns:wpg=\"http://schemas.microsoft.com/office/word/2010/wordprocessingGroup\" xmlns:wps=\"http://schemas.microsoft.com/office/word/2010/wordprocessingShape\"><wpg:cNvGrpSpPr/><wpg:grpSpPr/>{}{}</wpg:wgp></a:graphicData></a:graphic></wp:anchor></w:drawing></w:r>",
        member("", "KEEPSHAPE"),
        member(" hidden=\"1\"", "HIDDENSHAPE")
    );
    let canvas = group
        .replace("<wpg:cNvGrpSpPr/><wpg:grpSpPr/>", "")
        .replace("wpg:wgp", "wpc:wpc")
        .replace("xmlns:wpg", "xmlns:wpc");
    for drawing in [&group, &canvas] {
        let pkg = Pkg::new("docx").body(&doc_xml(&para(drawing)));
        let (f, copy) = clean("group.docx", &pkg.zip());
        assert_eq!(f["status"], "cleaned", "{f}");
        let plain = String::from_utf8_lossy(&plain(&copy.unwrap())).into_owned();
        assert!(plain.contains("KEEPSHAPE"), "the visible sibling went");
        assert!(!plain.contains("HIDDENSHAPE"));
    }
}

// White text is only hidden on what shows white.

const WHITE: &str = "<w:color w:val=\"FFFFFF\"/>";

/// A box of `inner` painted with `fill` (DrawingML) and `vml` (attributes of its VML twin).
fn painted_box(fill: &str, vml: &str, inner: &str) -> String {
    text_box(inner)
        .replace(
            "<wps:txbx>",
            &format!("<wps:spPr>{fill}</wps:spPr><wps:txbx>"),
        )
        .replace(
            "style=\"position:absolute\"",
            &format!("style=\"position:absolute\" {vml}"),
        )
}

fn hidden_text_count(f: &Value) -> usize {
    findings_of(f, "hidden-text").len()
}

#[test]
fn white_text_on_a_painted_text_box_stays() {
    let solid = "<a:solidFill xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\"><a:srgbClr val=\"1F497D\"/></a:solidFill>";
    let changed = solid.replace(
        "<a:srgbClr val=\"1F497D\"/>",
        "<a:schemeClr val=\"accent1\"><a:lumMod val=\"75000\"/></a:schemeClr>",
    );
    for (fill, vml) in [
        (solid.to_string(), "fillcolor=\"#1f497d [3215]\""),
        (changed, "fillcolor=\"#365f91 [2404]\""),
    ] {
        let inner = para(&text_run(WHITE, "KEEPBOX"));
        let body = doc_xml(&para(&painted_box(&fill, vml, &inner)));
        let pkg = Pkg::new("docx").body(&body).zip();
        assert_eq!(hidden_text_count(&check("painted.docx", &pkg)), 0);
    }
}

#[test]
fn white_text_on_a_box_with_no_fill_stays_since_a_shape_may_lie_under_it() {
    let none = "<a:noFill xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\"/>";
    let inner = para(&text_run(WHITE, "KEEPBOX"));
    let body = doc_xml(&para(&painted_box(none, "filled=\"f\"", &inner)));
    let doc = Pkg::new("docx").body(&body).zip();
    let f = check("empty.docx", &doc);
    assert_eq!(hidden_text_count(&f), 0);
    assert_eq!(f["warnings"][0]["code"], "unjudged_text", "{f}");
    let (f, _) = clean("empty.docx", &doc);
    assert_eq!(f["warnings"][0]["code"], "unjudged_text", "{f}");
}

#[test]
fn white_text_with_a_text_fill_of_its_own_stays() {
    let filled = format!(
        "<w:p><w:r><w:rPr>{WHITE}<w14:textFill xmlns:w14=\"http://schemas.microsoft.com/office/word/2010/wordml\"/></w:rPr><w:t>KEEPFILL</w:t></w:r></w:p>"
    );
    let doc = Pkg::new("docx").body(&doc_xml(&filled)).zip();
    assert_eq!(hidden_text_count(&check("fill.docx", &doc)), 0);
}

#[test]
fn white_text_on_a_white_text_box_is_hidden() {
    let inner = para(&format!(
        "{}{}",
        text_run(WHITE, "HIDDENWHITE"),
        text_run("", "KEEPBOX")
    ));
    let white = "<a:solidFill xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\"><a:srgbClr val=\"FFFFFF\"/></a:solidFill>";
    let body = painted_box(white, "fillcolor=\"white\"", &inner);
    let doc = box_survives(&body, Pkg::new("docx"), &[]);
    assert!(!doc.contains("HIDDENWHITE"));
}

fn with_styles(body: &str, styles: &str) -> Vec<u8> {
    let styles = format!("<w:styles xmlns:w=\"{W}\">{styles}</w:styles>");
    Pkg::new("docx")
        .body(&doc_xml(body))
        .part(
            "word/document.xml",
            "rId1",
            "styles",
            "word/styles.xml",
            "application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml",
            styles.as_bytes(),
        )
        .zip()
}

#[test]
fn white_text_on_a_paragraph_style_with_shading_stays() {
    let styles = "<w:style w:type=\"paragraph\" w:styleId=\"Base\"><w:pPr><w:shd w:val=\"clear\" w:color=\"auto\" w:fill=\"1F497D\"/></w:pPr></w:style><w:style w:type=\"paragraph\" w:styleId=\"Band\"><w:basedOn w:val=\"Base\"/><w:rPr><w:color w:val=\"FFFFFF\"/></w:rPr></w:style>";
    let body = format!(
        "<w:p><w:pPr><w:pStyle w:val=\"Band\"/></w:pPr><w:r><w:t>KEEPBAND</w:t></w:r></w:p>"
    );
    assert_eq!(
        hidden_text_count(&check("band.docx", &with_styles(&body, styles))),
        0
    );
    // A paragraph's own "no shading" shows the page again.
    let bare = body.replace(
        "<w:pStyle w:val=\"Band\"/>",
        "<w:pStyle w:val=\"Band\"/><w:shd w:val=\"clear\" w:color=\"auto\" w:fill=\"auto\"/>",
    );
    assert_eq!(
        hidden_text_count(&check("bare.docx", &with_styles(&bare, styles))),
        1
    );
}

#[test]
fn white_text_in_a_table_whose_style_paints_cells_stays() {
    let cell = |style: &str| {
        format!(
            "<w:tbl><w:tblPr><w:tblStyle w:val=\"{style}\"/></w:tblPr><w:tr><w:tc>{}</w:tc></w:tr></w:tbl>",
            para(&text_run(WHITE, "KEEPCELL"))
        )
    };
    let styles = "<w:style w:type=\"table\" w:styleId=\"Painted\"><w:tblStylePr w:type=\"firstRow\"><w:tcPr><w:shd w:val=\"clear\" w:color=\"auto\" w:fill=\"1F497D\"/></w:tcPr></w:tblStylePr></w:style><w:style w:type=\"table\" w:styleId=\"Plain\"/>";
    let painted = with_styles(&cell("Painted"), styles);
    assert_eq!(hidden_text_count(&check("painted.docx", &painted)), 0);
    let plain = with_styles(&cell("Plain"), styles);
    assert_eq!(hidden_text_count(&check("plain.docx", &plain)), 1);
}

// Structures a clean refuses rather than get wrong.

fn cannot(name: &str, bytes: &[u8]) {
    let (f, copy) = clean(name, bytes);
    assert_eq!(f["error"]["code"], "cannot_clean", "{f}");
    assert!(!f["error"]["hint"].as_str().unwrap().contains("bug"), "{f}");
    assert!(copy.is_none());
}

fn inline(at: &str, text: &str) -> String {
    format!("<c r=\"{at}\" t=\"inlineStr\"><is><t>{text}</t></is></c>")
}

const HIDDEN_C: &str = "<cols><col min=\"3\" max=\"3\" width=\"9\" hidden=\"1\"/></cols>";

fn sheet_with(cols: &str, rows: &str, tail: &str) -> String {
    format!(
        "<worksheet xmlns=\"{S}\" xmlns:r=\"{R}\">{cols}<sheetData>{rows}</sheetData>{tail}</worksheet>"
    )
}

#[test]
fn a_table_with_a_hidden_column_is_refused() {
    let rows = format!(
        "<row r=\"1\">{}{}{}</row><row r=\"2\">{}<c r=\"B2\"><v>2</v></c><c r=\"C2\"><v>7</v></c></row>",
        inline("A1", "Name"),
        inline("B1", "Val"),
        inline("C1", "HIDDENHEADER"),
        inline("A2", "a"),
    );
    let tail = "<tableParts count=\"1\"><tablePart r:id=\"rId1\"/></tableParts>";
    let table = format!(
        "<table xmlns=\"{S}\" id=\"1\" name=\"T\" displayName=\"T\" ref=\"A1:C2\"><tableColumns count=\"3\"><tableColumn id=\"1\" name=\"Name\"/><tableColumn id=\"2\" name=\"Val\"/><tableColumn id=\"3\" name=\"HIDDENHEADER\"/></tableColumns></table>"
    );
    let doc = workbook(&[("One", "visible", &sheet_with(HIDDEN_C, &rows, tail))])
        .part(
            "xl/worksheets/sheet1.xml",
            "rId1",
            "table",
            "xl/tables/table1.xml",
            "application/vnd.openxmlformats-officedocument.spreadsheetml.table+xml",
            table.as_bytes(),
        )
        .zip();
    cannot("table.xlsx", &doc);
}

/// A visible sheet ending in `tail`, and the hidden sheet it reads.
fn reads_hidden(tail: &str) -> Vec<u8> {
    let body = sheet_with(
        "",
        &format!("<row r=\"1\">{}</row>", inline("A1", "x")),
        tail,
    );
    workbook(&[
        ("One", "visible", &body),
        ("Gone Sheet", "hidden", &sheet_xml("")),
    ])
    .zip()
}

#[test]
fn a_link_or_sparkline_on_a_hidden_sheet_is_refused() {
    cannot(
        "link.xlsx",
        &reads_hidden(
            "<hyperlinks><hyperlink ref=\"A1\" location=\"'Gone Sheet'!A1\" display=\"x\"/></hyperlinks>",
        ),
    );
    cannot(
        "spark.xlsx",
        &reads_hidden(
            "<extLst><ext uri=\"{05C60535-1F16-4fd2-B633-F4F36F0B64E0}\"><f>'Gone Sheet'!A1:A2</f></ext></extLst>",
        ),
    );
}

#[test]
fn a_sheet_whose_name_ends_another_leaves_its_uses() {
    let body = sheet_with(
        "",
        "",
        "<hyperlinks><hyperlink ref=\"A1\" location=\"Data!A1\"/></hyperlinks>",
    );
    let doc = workbook(&[
        ("One", "visible", &body),
        ("Data", "visible", &sheet_xml("")),
        ("ta", "hidden", &sheet_xml("")),
    ])
    .zip();
    let (f, _) = clean("near.xlsx", &doc);
    assert_eq!(f["status"], "cleaned", "{f}");
}

#[test]
fn a_chart_reading_a_hidden_column_is_refused() {
    let chart = "<c:chartSpace xmlns:c=\"http://schemas.openxmlformats.org/drawingml/2006/chart\"><c:chart><c:plotArea><c:barChart><c:ser><c:val><c:numRef><c:f>One!$C$2:$C$3</c:f></c:numRef></c:val></c:ser></c:barChart></c:plotArea></c:chart></c:chartSpace>";
    let rows = format!("<row r=\"1\">{}</row>", inline("C1", "HIDDENSERIES"));
    let drawing = "<xdr:wsDr xmlns:xdr=\"http://schemas.openxmlformats.org/drawingml/2006/spreadsheetDrawing\"/>";
    let doc = workbook(&[(
        "One",
        "visible",
        &sheet_with(HIDDEN_C, &rows, "<drawing r:id=\"rId1\"/>"),
    )])
    .part(
        "xl/worksheets/sheet1.xml",
        "rId1",
        "drawing",
        "xl/drawings/drawing1.xml",
        "application/vnd.openxmlformats-officedocument.drawing+xml",
        drawing.as_bytes(),
    )
    .part(
        "xl/drawings/drawing1.xml",
        "rId1",
        "chart",
        "xl/charts/chart1.xml",
        "application/vnd.openxmlformats-officedocument.drawingml.chart+xml",
        chart.as_bytes(),
    )
    .zip();
    cannot("chart.xlsx", &doc);
}

fn hidden_run(inner: &str) -> String {
    format!("<w:r><w:rPr><w:vanish/></w:rPr>{inner}</w:r>")
}

#[test]
fn a_hidden_field_or_note_reference_is_found_and_refused() {
    for inner in [
        "<w:instrText> IF 1 = 2 \"HIDDENFIELD\" \"\" </w:instrText>",
        "<w:footnoteReference w:id=\"1\"/>",
    ] {
        let doc = Pkg::new("docx")
            .body(&doc_xml(&para(&hidden_run(inner))))
            .zip();
        assert_eq!(hidden_text_count(&check("field.docx", &doc)), 1);
        cannot("field.docx", &doc);
    }
}

#[test]
fn a_hidden_link_is_refused() {
    let link = format!(
        "<w:hyperlink r:id=\"rId5\">{}</w:hyperlink>",
        hidden_run("<w:t>x</w:t>")
    );
    let doc = Pkg::new("docx")
        .body(&doc_xml(&para(&link)))
        .link(
            "word/document.xml",
            external("rId5", "hyperlink", "https://example.com/HIDDENTARGET"),
        )
        .zip();
    cannot("link.docx", &doc);
}

#[test]
fn a_picture_in_a_white_run_stays_while_its_text_goes() {
    let pic =
        "<w:drawing><wp:inline><wp:docPr id=\"8\" name=\"KEEPPICTURE\"/></wp:inline></w:drawing>";
    let white = format!("<w:r><w:rPr>{WHITE}</w:rPr><w:t>HIDDENWHITE</w:t>{pic}</w:r>");
    let doc = Pkg::new("docx").body(&doc_xml(&para(&white))).zip();
    let (f, copy) = clean("white.docx", &doc);
    assert_eq!(f["status"], "cleaned", "{f}");
    let plain = String::from_utf8_lossy(&plain(&copy.unwrap())).into_owned();
    assert!(plain.contains("KEEPPICTURE") && !plain.contains("HIDDENWHITE"));
}

#[test]
fn a_hidden_shape_on_the_notes_or_handout_master_is_refused() {
    let tree = slide_xml(&shape(3, true, "", "HIDDENMASTER"));
    let ox = "application/vnd.openxmlformats-officedocument.presentationml.";
    for (short, name, kind, root) in [
        (
            "notesMaster",
            "ppt/notesMasters/notesMaster1.xml",
            "notesMaster+xml",
            "notesMaster",
        ),
        (
            "handoutMaster",
            "ppt/handoutMasters/handoutMaster1.xml",
            "handoutMaster+xml",
            "handoutMaster",
        ),
    ] {
        let body = tree.replace("p:sld", &format!("p:{root}"));
        let doc = presentation(&[&slide_xml("")])
            .part(
                "ppt/presentation.xml",
                "rId9",
                short,
                name,
                &format!("{ox}{kind}"),
                body.as_bytes(),
            )
            .zip();
        cannot("masters.pptx", &doc);
    }
}

#[test]
fn an_object_on_a_master_becomes_its_picture() {
    let frame = "<p:graphicFrame><p:nvGraphicFramePr><p:cNvPr id=\"9\" name=\"o\"/><p:cNvGraphicFramePr/><p:nvPr/></p:nvGraphicFramePr><p:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"9\" cy=\"9\"/></p:xfrm><a:graphic><a:graphicData uri=\"ole\"><p:oleObj r:id=\"rId1\" progId=\"Package\"><p:embed/><p:pic><p:nvPicPr><p:cNvPr id=\"0\" name=\"\"/><p:cNvPicPr/><p:nvPr/></p:nvPicPr><p:blipFill><a:blip/></p:blipFill><p:spPr/></p:pic></p:oleObj></a:graphicData></a:graphic></p:graphicFrame>";
    let body = slide_xml(frame).replace("p:sld", "p:sldMaster");
    let ox = "application/vnd.openxmlformats-officedocument.";
    let doc = presentation(&[&slide_xml("")])
        .part(
            "ppt/presentation.xml",
            "rId9",
            "slideMaster",
            "ppt/slideMasters/slideMaster1.xml",
            &format!("{ox}presentationml.slideMaster+xml"),
            body.as_bytes(),
        )
        .part(
            "ppt/slideMasters/slideMaster1.xml",
            "rId1",
            "package",
            "ppt/embeddings/a.bin",
            &format!("{ox}oleObject"),
            b"HIDDENOBJECT",
        )
        .zip();
    gone("master-object.pptx", &doc, "HIDDENOBJECT");
}
