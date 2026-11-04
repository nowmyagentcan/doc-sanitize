//! The leak fixtures (spec §8.4 44–47) and their no-leak twins. Every planted secret holds
//! `SECRET-`, so a search for it finds any that got out.

use super::*;

const WML: &str = "application/vnd.openxmlformats-officedocument.wordprocessingml";
const SML: &str = "application/vnd.openxmlformats-officedocument.spreadsheetml";
const PML: &str = "application/vnd.openxmlformats-officedocument.presentationml";
const MS: &str = "http://schemas.microsoft.com/office";

/// A bar chart with one series (`series`: its values) and what follows the chart (`after`).
fn bar_chart(series: &str, after: &str) -> String {
    let axis = |kind: &str, id: u32, cross: u32, pos: &str| {
        format!(
            "<c:{kind}><c:axId val=\"{id}\"/><c:scaling><c:orientation val=\"minMax\"/></c:scaling><c:delete val=\"0\"/><c:axPos val=\"{pos}\"/><c:crossAx val=\"{cross}\"/></c:{kind}>"
        )
    };
    format!(
        "<c:chartSpace xmlns:c=\"http://schemas.openxmlformats.org/drawingml/2006/chart\" xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"><c:chart><c:plotArea><c:layout/><c:barChart><c:barDir val=\"col\"/><c:grouping val=\"clustered\"/><c:varyColors val=\"0\"/><c:ser><c:idx val=\"0\"/><c:order val=\"0\"/>{series}</c:ser><c:axId val=\"1\"/><c:axId val=\"2\"/></c:barChart>{}{}</c:plotArea></c:chart>{after}</c:chartSpace>",
        axis("catAx", 1, 2, "b"),
        axis("valAx", 2, 1, "l")
    )
}

/// A deck's slide master, layout and theme, and the notes master when `notes`, linked as
/// PowerPoint links them; `slides` and `notes_slides` get their layout and master.
fn masters(mut p: Pkg, slides: &[&str], notes_slides: &[&str]) -> Pkg {
    let tree = "<p:cSld><p:spTree><p:nvGrpSpPr><p:cNvPr id=\"1\" name=\"\"/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr/></p:spTree></p:cSld>";
    let map = "<p:clrMap bg1=\"lt1\" tx1=\"dk1\" bg2=\"lt2\" tx2=\"dk2\" accent1=\"accent1\" accent2=\"accent2\" accent3=\"accent3\" accent4=\"accent4\" accent5=\"accent5\" accent6=\"accent6\" hlink=\"hlink\" folHlink=\"folHlink\"/>";
    let ns = format!(
        "xmlns:p=\"{P}\" xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" xmlns:r=\"{R}\""
    );
    let master = format!(
        "<p:sldMaster {ns}>{tree}{map}<p:sldLayoutIdLst><p:sldLayoutId id=\"2147483649\" r:id=\"rId1\"/></p:sldLayoutIdLst></p:sldMaster>"
    );
    let layout = format!(
        "<p:sldLayout {ns}>{tree}<p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:sldLayout>"
    );
    let notes_master = format!("<p:notesMaster {ns}>{tree}{map}</p:notesMaster>");
    let pr = "ppt/presentation.xml";
    let m = "ppt/slideMasters/slideMaster1.xml";
    let l = "ppt/slideLayouts/slideLayout1.xml";
    p = p
        .part(
            pr,
            "rId90",
            "slideMaster",
            m,
            &pml("slideMaster"),
            master.as_bytes(),
        )
        .part(
            m,
            "rId1",
            "slideLayout",
            l,
            &pml("slideLayout"),
            layout.as_bytes(),
        )
        .part(
            m,
            "rId2",
            "theme",
            "ppt/theme/theme1.xml",
            THEME_TYPE,
            theme_xml().as_bytes(),
        )
        .link(
            l,
            rel("rId1", "slideMaster", "../slideMasters/slideMaster1.xml"),
        )
        .link(pr, rel("rId92", "theme", "theme/theme1.xml"));
    for s in slides {
        p = p.link(
            s,
            rel("rId99", "slideLayout", "../slideLayouts/slideLayout1.xml"),
        );
    }
    if !notes_slides.is_empty() {
        let n = "ppt/notesMasters/notesMaster1.xml";
        p = p
            .part(
                pr,
                "rId91",
                "notesMaster",
                n,
                &pml("notesMaster"),
                notes_master.as_bytes(),
            )
            .part(
                n,
                "rId1",
                "theme",
                "ppt/theme/theme2.xml",
                THEME_TYPE,
                theme_xml().as_bytes(),
            );
        for s in notes_slides {
            p = p.link(
                s,
                rel("rId98", "notesMaster", "../notesMasters/notesMaster1.xml"),
            );
        }
    }
    p
}

/// The master lists a presentation needs, put first in its XML.
fn with_masters(presentation: &str, notes: bool) -> String {
    let at = presentation.find('>').unwrap() + 1;
    let notes = match notes {
        true => "<p:notesMasterIdLst><p:notesMasterId r:id=\"rId91\"/></p:notesMasterIdLst>",
        false => "",
    };
    format!(
        "{}<p:sldMasterIdLst><p:sldMasterId id=\"2147483648\" r:id=\"rId90\"/></p:sldMasterIdLst>{notes}{}",
        &presentation[..at],
        &presentation[at..]
    )
}

/// A `w × h` RGB PNG of one colour.
pub fn png(w: u32, h: u32) -> Vec<u8> {
    use std::io::Write;
    let chunk = |kind: &[u8; 4], data: &[u8]| {
        let mut crc = flate2::Crc::new();
        crc.update(kind);
        crc.update(data);
        [
            (data.len() as u32).to_be_bytes().to_vec(),
            kind.to_vec(),
            data.to_vec(),
            crc.sum().to_be_bytes().to_vec(),
        ]
        .concat()
    };
    let mut ihdr = w.to_be_bytes().to_vec();
    ihdr.extend(h.to_be_bytes());
    ihdr.extend([8, 2, 0, 0, 0]);
    let row: Vec<u8> = [0]
        .into_iter()
        .chain((0..w).flat_map(|_| [40, 120, 200]))
        .collect();
    let mut z = flate2::write::ZlibEncoder::new(vec![], flate2::Compression::default());
    for _ in 0..h {
        z.write_all(&row).unwrap();
    }
    [
        b"\x89PNG\r\n\x1a\n".to_vec(),
        chunk(b"IHDR", &ihdr),
        chunk(b"IDAT", &z.finish().unwrap()),
        chunk(b"IEND", b""),
    ]
    .concat()
}

fn wml(t: &str) -> String {
    format!("{WML}.{t}+xml")
}

fn sml(t: &str) -> String {
    format!("{SML}.{t}+xml")
}

fn pml(t: &str) -> String {
    format!("{PML}.{t}+xml")
}

fn props(p: Pkg, creator: &str, company: &str) -> Pkg {
    let core = core_xml(&format!(
        "<dc:creator>{creator}</dc:creator><cp:lastModifiedBy>Sam Sample</cp:lastModifiedBy>"
    ));
    let app = format!(
        "<Properties xmlns=\"{APP_NS}\"><Application>Microsoft Office</Application><Company>{company}</Company></Properties>"
    );
    p.part(
        "",
        "rId20",
        &format!("{PKG_RELS}/metadata/core-properties"),
        "docProps/core.xml",
        CORE_TYPE,
        core.as_bytes(),
    )
    .part(
        "",
        "rId21",
        "extended-properties",
        "docProps/app.xml",
        &format!("{OX}.extended-properties+xml"),
        app.as_bytes(),
    )
}

pub fn drawing(id: &str, crop: &str, name: &str) -> String {
    format!(
        "<w:r><w:drawing><wp:inline><wp:extent cx=\"914400\" cy=\"914400\"/><wp:docPr id=\"9\" name=\"{name}\"/><a:graphic><a:graphicData uri=\"http://schemas.openxmlformats.org/drawingml/2006/picture\"><pic:pic><pic:nvPicPr><pic:cNvPr id=\"0\" name=\"{name}\"/><pic:cNvPicPr/></pic:nvPicPr><pic:blipFill><a:blip r:embed=\"{id}\"/>{crop}<a:stretch><a:fillRect/></a:stretch></pic:blipFill><pic:spPr><a:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"914400\" cy=\"914400\"/></a:xfrm><a:prstGeom prst=\"rect\"/></pic:spPr></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r>"
    )
}

const WORD_STYLES: &str = "<w:styles xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"><w:docDefaults><w:rPrDefault><w:rPr><w:sz w:val=\"22\"/></w:rPr></w:rPrDefault></w:docDefaults><w:style w:type=\"paragraph\" w:default=\"1\" w:styleId=\"Normal\"><w:name w:val=\"Normal\"/></w:style><w:style w:type=\"character\" w:styleId=\"Quiet\"><w:name w:val=\"Quiet\"/><w:rPr><w:vanish/></w:rPr></w:style></w:styles>";

/// Fixture 44: a Word document with every Word leak the checks know.
pub fn leaky_docx(variant: &str) -> Vec<u8> {
    let sam = "w:author=\"Sam Sample\" w:date=\"2026-01-01T00:00:00Z\"";
    let body = [
        format!("<w:p w:rsidR=\"00A1B2C3\" w:rsidRDefault=\"00A1B2C3\"><w:permStart w:id=\"1\" w:ed=\"SECRET-docx-ed-1\"/>{}<w:permEnd w:id=\"1\"/></w:p>", text_run("", "Agreement between the parties.")),
        format!("<w:p><w:commentRangeStart w:id=\"0\"/>{}<w:commentRangeEnd w:id=\"0\"/><w:r><w:commentReference w:id=\"0\"/></w:r></w:p>", text_run("", "The fee is due monthly.")),
        format!("<w:p><w:pPr><w:rPr><w:del w:id=\"10\" {sam}/></w:rPr></w:pPr>{}<w:ins w:id=\"11\" {sam}><w:r><w:t>SECRET-docx-ins-1</w:t></w:r></w:ins><w:del w:id=\"12\" {sam}><w:r><w:delText>SECRET-docx-del-1</w:delText></w:r></w:del></w:p>", text_run("", "Payment terms")),
        format!("<w:p><w:moveFrom w:id=\"13\" {sam}><w:r><w:t>SECRET-docx-move-1</w:t></w:r></w:moveFrom>{}<w:moveTo w:id=\"14\" {sam}><w:r><w:t>SECRET-docx-move-1</w:t></w:r></w:moveTo></w:p>", text_run("", "Moved")),
        para(&(text_run("", "Visible ") + &text_run("<w:vanish/>", "SECRET-docx-vanish-1"))),
        para(&text_run("<w:rStyle w:val=\"Quiet\"/>", "SECRET-docx-vanish-2")),
        para(&text_run("<w:color w:val=\"FFFFFF\"/>", "SECRET-docx-white-1")),
        para(&text_run("<w:color w:val=\"000000\"/><w:highlight w:val=\"black\"/>", "SECRET-docx-black-1")),
        format!("<w:tbl><w:tblPr><w:tblW w:w=\"0\" w:type=\"auto\"/></w:tblPr><w:tblGrid><w:gridCol w:w=\"2000\"/><w:gridCol w:w=\"2000\"/></w:tblGrid><w:tr><w:tc>{}</w:tc><w:tc><w:tcPr><w:cellDel w:id=\"15\" {sam}/></w:tcPr>{}</w:tc></w:tr><w:tr><w:trPr><w:del w:id=\"16\" {sam}/></w:trPr><w:tc>{}</w:tc><w:tc>{}</w:tc></w:tr></w:tbl>",
            para(&text_run("", "Kept")), para(&text_run("", "Cell")), para(&text_run("", "Row")), para(&text_run("", "Row"))),
        format!("<w:sdt><w:sdtPr><w:dataBinding w:xpath=\"/matter/client\" w:storeItemID=\"{{6E3B1D1C-0000-0000-0000-000000000001}}\"/></w:sdtPr><w:sdtContent>{}</w:sdtContent></w:sdt>", para(&text_run("", "Bound client"))),
        "<w:p><w:fldSimple w:instr=\" INCLUDEPICTURE &quot;D:\\\\Projects\\\\avery\\\\SECRET-docx-field-1.png&quot; \\* MERGEFORMAT \"><w:r><w:t>[picture]</w:t></w:r></w:fldSimple></w:p>".into(),
        format!("<w:p>{}{}</w:p>", drawing("rId30", "<a:srcRect l=\"10000\" t=\"20000\"/>", "Cropped"), drawing("rId31", "", "Photo")),
        "<w:p><w:r><w:object w:dxaOrig=\"1440\" w:dyaOrig=\"1440\"><v:shape id=\"_x0000_i1025\" type=\"#_x0000_t75\" style=\"width:72pt;height:72pt\"><v:imagedata r:id=\"rId30\" o:title=\"\"/></v:shape><o:OLEObject Type=\"Embed\" ProgID=\"Package\" ShapeID=\"_x0000_i1025\" DrawAspect=\"Icon\" ObjectID=\"_1\" r:id=\"rId32\"/></w:object></w:r></w:p>".into(),
        "<w:sectPr><w:pgSz w:w=\"12240\" w:h=\"15840\"/></w:sectPr>".into(),
    ]
    .concat();
    let settings = format!(
        "<w:settings xmlns:w=\"{W}\" xmlns:r=\"{R}\" xmlns:w14=\"http://schemas.microsoft.com/office/word/2010/wordml\"><w:attachedTemplate r:id=\"rId1\"/><w:mailMerge><w:mainDocumentType w:val=\"formLetters\"/><w:dataType w:val=\"native\"/><w:connectString w:val=\"\"/><w:query w:val=\"SELECT * FROM x\"/><w:dataSource r:id=\"rId2\"/></w:mailMerge><w:rsids><w:rsidRoot w:val=\"00A1B2C3\"/><w:rsid w:val=\"00A1B2C3\"/></w:rsids><w:docVars><w:docVar w:name=\"MatterId\" w:val=\"SECRET-docx-var-1\"/></w:docVars><w14:docId w14:val=\"1A2B3C4D\"/></w:settings>"
    );
    let comments = format!(
        "<w:comments xmlns:w=\"{W}\"><w:comment w:id=\"0\" w:author=\"Avery Example\" w:initials=\"AE\"><w:p><w:r><w:t>SECRET-docx-comment-1</w:t></w:r></w:p></w:comment></w:comments>"
    );
    let people = "<w15:people xmlns:w15=\"http://schemas.microsoft.com/office/word/2012/wordml\"><w15:person w15:author=\"Avery Example\"><w15:presenceInfo w15:providerId=\"None\" w15:userId=\"Avery Example\"/></w15:person></w15:people>";
    let custom = "<Properties xmlns=\"http://schemas.openxmlformats.org/officeDocument/2006/custom-properties\" xmlns:vt=\"http://schemas.openxmlformats.org/officeDocument/2006/docPropsVTypes\"><property fmtid=\"{D5CDD505-2E9C-101B-9397-08002B2CF9AE}\" pid=\"2\" name=\"Client\"><vt:lpwstr>SECRET-docx-custom-1</vt:lpwstr></property></Properties>";
    let mut p = Pkg::new(variant);
    p = props(
        p.body(&doc_xml(&body)),
        "Avery Example",
        "SECRET-docx-company-1",
    )
    .part(
        "",
        "rId22",
        "custom-properties",
        "docProps/custom.xml",
        &format!("{OX}.custom-properties+xml"),
        custom.as_bytes(),
    )
    .part(
        "",
        "rId23",
        &format!("{PKG_RELS}/metadata/thumbnail"),
        "docProps/thumbnail.jpeg",
        "",
        &jpeg(&[], b""),
    );
    let main = p.main();
    p = p
        .part(
            main,
            "rId10",
            "styles",
            "word/styles.xml",
            &wml("styles"),
            WORD_STYLES.as_bytes(),
        )
        .part(
            main,
            "rId11",
            "settings",
            "word/settings.xml",
            &wml("settings"),
            settings.as_bytes(),
        )
        .part(
            main,
            "rId12",
            "comments",
            "word/comments.xml",
            &wml("comments"),
            comments.as_bytes(),
        )
        .part(
            main,
            "rId13",
            &format!("{MS}/2011/relationships/people"),
            "word/people.xml",
            &wml("people"),
            people.as_bytes(),
        )
        .part(
            main,
            "rId14",
            "theme",
            "word/theme/theme1.xml",
            THEME_TYPE,
            theme_xml().as_bytes(),
        )
        .part(
            main,
            "rId15",
            "glossaryDocument",
            "word/glossary/document.xml",
            &wml("document.glossary"),
            format!("<w:glossaryDocument xmlns:w=\"{W}\"><w:docParts/></w:glossaryDocument>")
                .as_bytes(),
        )
        .part(
            main,
            "rId16",
            "customXml",
            "customXml/item1.xml",
            "",
            b"<matter><client>SECRET-docx-xml-1</client></matter>",
        )
        .part(
            main,
            "rId30",
            "image",
            "word/media/image1.png",
            "",
            &png(8, 8),
        )
        .part(
            main,
            "rId31",
            "image",
            "word/media/image2.jpeg",
            "",
            &jpeg(&[0x0112, 0x8825], b""),
        )
        .part(
            main,
            "rId32",
            "oleObject",
            "word/embeddings/oleObject1.bin",
            "",
            b"\xD0\xCF\x11\xE0\xA1\xB1\x1A\xE1SECRET-docx-ole-1",
        )
        .link(
            "word/settings.xml",
            external(
                "rId1",
                "attachedTemplate",
                "file:///D:/Projects/avery/SECRET-docx-template-1.dotx",
            ),
        )
        .link(
            "word/settings.xml",
            external(
                "rId2",
                "mailMergeSource",
                "file:///\\\\fileserver\\share\\SECRET-docx-merge-1.xlsx",
            ),
        );
    if variant.ends_with('m') {
        p = p.part(
            main,
            "rId17",
            &format!("{MS}/2006/relationships/vbaProject"),
            "word/vbaProject.bin",
            "application/vnd.ms-office.vbaProject",
            b"\xCC\x61SECRET-docm-vba-1",
        );
    }
    p.zip()
}

/// The no-leak twin of fixture 44: visible text, headers and footers, web and mail links,
/// automatic text on a dark highlight, contrasting colours and plain pictures.
pub fn twin_docx() -> Vec<u8> {
    let body = [
        para(&text_run("", "Agreement between the parties.")),
        "<w:p><w:hyperlink r:id=\"rId40\"><w:r><w:t>example.com</w:t></w:r></w:hyperlink><w:hyperlink r:id=\"rId41\"><w:r><w:t>mail us</w:t></w:r></w:hyperlink><w:hyperlink w:anchor=\"top\"><w:r><w:t>top</w:t></w:r></w:hyperlink></w:p>".into(),
        para(&text_run("<w:color w:val=\"auto\"/><w:highlight w:val=\"black\"/>", "auto on black")),
        para(&text_run("<w:color w:val=\"FFFFFF\"/><w:highlight w:val=\"darkBlue\"/>", "white on blue")),
        para(&text_run("<w:color w:val=\"C00000\"/>", "red on white")),
        para(&text_run("<w:vanish w:val=\"0\"/>", "not hidden")),
        "<w:p><w:fldSimple w:instr=\" PAGE \"><w:r><w:t>1</w:t></w:r></w:fldSimple><w:fldSimple w:instr=\"INCLUDEPICTURE &quot;https://example.com/logo.png&quot;\"/></w:p>".into(),
        format!("<w:p>{}{}</w:p>", drawing("rId30", "<a:srcRect l=\"500\"/>", "Logo"), drawing("rId31", "", "Photo")),
        "<w:sectPr><w:headerReference w:type=\"default\" r:id=\"rId42\"/><w:footerReference w:type=\"default\" r:id=\"rId43\"/></w:sectPr>".into(),
    ]
    .concat();
    let header = format!(
        "<w:hdr xmlns:w=\"{W}\">{}</w:hdr>",
        para(&text_run("", "Confidential draft"))
    );
    let footer = format!(
        "<w:ftr xmlns:w=\"{W}\">{}</w:ftr>",
        para(&text_run("", "Page footer"))
    );
    let p = Pkg::new("docx").body(&doc_xml(&body));
    let main = p.main();
    p.part(
        main,
        "rId10",
        "styles",
        "word/styles.xml",
        &wml("styles"),
        WORD_STYLES.as_bytes(),
    )
    .part(
        main,
        "rId14",
        "theme",
        "word/theme/theme1.xml",
        THEME_TYPE,
        theme_xml().as_bytes(),
    )
    .part(
        main,
        "rId30",
        "image",
        "word/media/image1.png",
        "",
        &png(8, 8),
    )
    .part(
        main,
        "rId31",
        "image",
        "word/media/image2.jpeg",
        "",
        &jpeg(&[0x0112], b""),
    )
    .part(
        main,
        "rId42",
        "header",
        "word/header1.xml",
        &wml("header"),
        header.as_bytes(),
    )
    .part(
        main,
        "rId43",
        "footer",
        "word/footer1.xml",
        &wml("footer"),
        footer.as_bytes(),
    )
    .link(main, external("rId40", "hyperlink", "https://example.com/"))
    .link(
        main,
        external("rId41", "hyperlink", "mailto:office@example.com"),
    )
    .zip()
}

const XLSX_STYLES: &str = "<styleSheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\"><numFmts count=\"1\"><numFmt numFmtId=\"164\" formatCode=\";;;\"/></numFmts><fonts count=\"3\"><font><sz val=\"11\"/></font><font><color rgb=\"FFFFFFFF\"/><sz val=\"11\"/></font><font><color theme=\"1\"/><sz val=\"11\"/></font></fonts><fills count=\"3\"><fill><patternFill patternType=\"none\"/></fill><fill><patternFill patternType=\"gray125\"/></fill><fill><patternFill patternType=\"solid\"><fgColor rgb=\"FF000000\"/><bgColor indexed=\"64\"/></patternFill></fill></fills><borders count=\"1\"><border/></borders><cellStyleXfs count=\"1\"><xf numFmtId=\"0\" fontId=\"0\" fillId=\"0\" borderId=\"0\"/></cellStyleXfs><cellXfs count=\"5\"><xf numFmtId=\"0\" fontId=\"0\" fillId=\"0\" borderId=\"0\"/><xf numFmtId=\"164\" fontId=\"0\" fillId=\"0\" borderId=\"0\" applyNumberFormat=\"1\"/><xf numFmtId=\"0\" fontId=\"1\" fillId=\"0\" borderId=\"0\" applyFont=\"1\"/><xf numFmtId=\"0\" fontId=\"2\" fillId=\"2\" borderId=\"0\" applyFont=\"1\" applyFill=\"1\"/><xf numFmtId=\"0\" fontId=\"1\" fillId=\"2\" borderId=\"0\" applyFont=\"1\" applyFill=\"1\"/></cellXfs></styleSheet>";

/// Fixture 45: a workbook with every Excel leak the checks know.
pub fn leaky_xlsx(variant: &str) -> Vec<u8> {
    let summary = format!(
        "<worksheet xmlns=\"{S}\" xmlns:r=\"{R}\"><cols><col min=\"4\" max=\"4\" width=\"9\" hidden=\"1\" customWidth=\"1\"/></cols><sheetData>\
         <row r=\"1\"><c r=\"A1\" t=\"s\"><v>0</v></c><c r=\"B1\"><f>SUM(Data!A1:A3)</f><v>6</v></c><c r=\"D1\" t=\"s\"><v>2</v></c></row>\
         <row r=\"2\"><c r=\"A2\" s=\"1\" t=\"s\"><v>3</v></c><c r=\"B2\" s=\"2\" t=\"s\"><v>4</v></c><c r=\"C2\" s=\"3\" t=\"s\"><v>5</v></c><c r=\"E2\" s=\"4\" t=\"s\"><v>0</v></c></row>\
         <row r=\"5\" hidden=\"1\"><c r=\"A5\" t=\"s\"><v>6</v></c></row>\
         <row r=\"6\" hidden=\"1\"><c r=\"A6\"><v>7</v></c></row>\
         </sheetData><autoFilter ref=\"A5:A6\"/><scenarios current=\"0\"><scenario name=\"Best case\" count=\"1\" user=\"Avery Example\"><inputCells r=\"B1\" val=\"SECRET-xlsx-scenario-1\"/></scenario></scenarios><legacyDrawing r:id=\"rId3\"/></worksheet>"
    );
    let data = sheet_xml(
        "<row r=\"1\"><c r=\"A1\"><v>1</v></c></row><row r=\"2\"><c r=\"A2\"><v>2</v></c></row><row r=\"3\"><c r=\"A3\"><v>3</v></c></row>",
    );
    let secret = sheet_xml("<row r=\"1\"><c r=\"A1\" t=\"s\"><v>7</v></c></row>");
    let strings = [
        "Region",
        "SECRET-xlsx-unused-1",
        "SECRET-xlsx-col-1",
        "SECRET-xlsx-format-1",
        "SECRET-xlsx-white-1",
        "SECRET-xlsx-black-1",
        "SECRET-xlsx-row-1",
        "SECRET-xlsx-salary-1",
    ];
    let sst: String = strings
        .iter()
        .map(|s| format!("<si><t>{s}</t></si>"))
        .collect();
    let sst = format!("<sst xmlns=\"{S}\" count=\"8\" uniqueCount=\"8\">{sst}</sst>");
    let notes = format!(
        "<comments xmlns=\"{S}\"><authors><author>Avery Example</author></authors><commentList><comment ref=\"A1\" authorId=\"0\"><text><r><t>SECRET-xlsx-note-1</t></r></text></comment></commentList></comments>"
    );
    let vml = "<xml xmlns:v=\"urn:schemas-microsoft-com:vml\" xmlns:o=\"urn:schemas-microsoft-com:office:office\" xmlns:x=\"urn:schemas-microsoft-com:office:excel\"><v:shape id=\"_x0000_s1025\" type=\"#_x0000_t202\" style=\"position:absolute;visibility:hidden\"><x:ClientData ObjectType=\"Note\"><x:Row>0</x:Row><x:Column>0</x:Column></x:ClientData></v:shape></xml>";
    let threaded = "<ThreadedComments xmlns=\"http://schemas.microsoft.com/office/spreadsheetml/2018/threadedcomments\"><threadedComment ref=\"A1\" dT=\"2026-01-01T00:00:00.00\" personId=\"{11111111-1111-1111-1111-111111111111}\" id=\"{22222222-2222-2222-2222-222222222222}\"><text>SECRET-xlsx-thread-1</text></threadedComment></ThreadedComments>";
    let persons = "<personList xmlns=\"http://schemas.microsoft.com/office/spreadsheetml/2018/threadedcomments\"><person displayName=\"Sam Sample\" id=\"{11111111-1111-1111-1111-111111111111}\" userId=\"sam\" providerId=\"None\"/></personList>";
    let pivot = format!(
        "<pivotCacheDefinition xmlns=\"{S}\" xmlns:r=\"{R}\" r:id=\"rId1\" recordCount=\"2\"><cacheSource type=\"worksheet\"><worksheetSource ref=\"A1:A3\" sheet=\"Deleted sheet\"/></cacheSource><cacheFields count=\"1\"><cacheField name=\"Name\" numFmtId=\"0\"><sharedItems><s v=\"SECRET-xlsx-pivot-1\"/><s v=\"Sam\"/></sharedItems></cacheField></cacheFields></pivotCacheDefinition>"
    );
    let records = format!(
        "<pivotCacheRecords xmlns=\"{S}\" count=\"2\"><r><x v=\"0\"/></r><r><x v=\"1\"/></r></pivotCacheRecords>"
    );
    let link = format!(
        "<externalLink xmlns=\"{S}\" xmlns:r=\"{R}\"><externalBook r:id=\"rId1\"><sheetNames><sheetName val=\"Rates\"/></sheetNames><sheetDataSet><sheetData sheetId=\"0\"><row r=\"1\"><cell r=\"A1\" t=\"str\"><v>SECRET-xlsx-cache-1</v></cell></row></sheetData></sheetDataSet></externalBook></externalLink>"
    );
    let connections = format!(
        "<connections xmlns=\"{S}\"><connection id=\"1\" name=\"Clients\" type=\"1\" refreshedVersion=\"8\"><dbPr connection=\"DSN=SECRET-xlsx-dsn-1\" command=\"SELECT 1\"/></connection></connections>"
    );
    let chart = bar_chart(
        "<c:val><c:numRef><c:f>Data!$A$1:$A$3</c:f><c:numCache><c:ptCount val=\"3\"/><c:pt idx=\"0\"><c:v>1</c:v></c:pt><c:pt idx=\"1\"><c:v>2</c:v></c:pt><c:pt idx=\"2\"><c:v>3</c:v></c:pt></c:numCache></c:numRef></c:val>",
        "",
    );
    let drawing = "<xdr:wsDr xmlns:xdr=\"http://schemas.openxmlformats.org/drawingml/2006/spreadsheetDrawing\" xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\" xmlns:c=\"http://schemas.openxmlformats.org/drawingml/2006/chart\"><xdr:twoCellAnchor><xdr:from><xdr:col>5</xdr:col><xdr:colOff>0</xdr:colOff><xdr:row>1</xdr:row><xdr:rowOff>0</xdr:rowOff></xdr:from><xdr:to><xdr:col>10</xdr:col><xdr:colOff>0</xdr:colOff><xdr:row>15</xdr:row><xdr:rowOff>0</xdr:rowOff></xdr:to><xdr:graphicFrame macro=\"\"><xdr:nvGraphicFramePr><xdr:cNvPr id=\"2\" name=\"Chart 1\"/><xdr:cNvGraphicFramePr/></xdr:nvGraphicFramePr><xdr:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"0\" cy=\"0\"/></xdr:xfrm><a:graphic><a:graphicData uri=\"http://schemas.openxmlformats.org/drawingml/2006/chart\"><c:chart r:id=\"rId1\"/></a:graphicData></a:graphic></xdr:graphicFrame><xdr:clientData/></xdr:twoCellAnchor></xdr:wsDr>";
    let macro_sheet = format!(
        "<xm:macrosheet xmlns=\"{S}\" xmlns:xm=\"http://schemas.microsoft.com/office/excel/2006/main\"><sheetData><row r=\"1\"><c r=\"A1\" t=\"str\"><f>EXEC(\"calc\")</f><v>0</v></c></row></sheetData></xm:macrosheet>"
    );
    let book = format!(
        "<workbook xmlns=\"{S}\" xmlns:r=\"{R}\" xmlns:mc=\"http://schemas.openxmlformats.org/markup-compatibility/2006\" xmlns:x15ac=\"http://schemas.microsoft.com/office/spreadsheetml/2010/11/ac\" mc:Ignorable=\"x15ac\"><fileSharing userName=\"SECRET-xlsx-user-1\"/><mc:AlternateContent><mc:Choice Requires=\"x15\"><x15ac:absPath url=\"D:\\Projects\\avery\\SECRET-xlsx-folder-1\\\"/></mc:Choice></mc:AlternateContent><bookViews><workbookView activeTab=\"0\"/></bookViews><sheets><sheet name=\"Summary\" sheetId=\"1\" r:id=\"rId1\"/><sheet name=\"Data\" sheetId=\"2\" state=\"hidden\" r:id=\"rId2\"/><sheet name=\"Salaries\" sheetId=\"3\" state=\"veryHidden\" r:id=\"rId3\"/><sheet name=\"Auto\" sheetId=\"4\" state=\"hidden\" r:id=\"rId4\"/></sheets><externalReferences><externalReference r:id=\"rId25\"/></externalReferences><pivotCaches><pivotCache cacheId=\"1\" r:id=\"rId26\"/></pivotCaches></workbook>"
    );
    let w = "xl/workbook.xml";
    let sheet1 = "xl/worksheets/sheet1.xml";
    let mut p = props(Pkg::new(variant), "Avery Example", "Example Corp").body(&book);
    p = p
        .part(
            w,
            "rId1",
            "worksheet",
            sheet1,
            SHEET_TYPE,
            summary.as_bytes(),
        )
        .part(
            w,
            "rId2",
            "worksheet",
            "xl/worksheets/sheet2.xml",
            SHEET_TYPE,
            data.as_bytes(),
        )
        .part(
            w,
            "rId3",
            "worksheet",
            "xl/worksheets/sheet3.xml",
            SHEET_TYPE,
            secret.as_bytes(),
        )
        .part(
            w,
            "rId4",
            "http://schemas.microsoft.com/office/2006/relationships/xlMacrosheet",
            "xl/macrosheets/sheet1.xml",
            "application/vnd.ms-excel.macrosheet+xml",
            macro_sheet.as_bytes(),
        )
        .part(
            w,
            "rId22",
            "styles",
            "xl/styles.xml",
            &sml("styles"),
            XLSX_STYLES.as_bytes(),
        )
        .part(
            w,
            "rId23",
            "theme",
            "xl/theme/theme1.xml",
            THEME_TYPE,
            theme_xml().as_bytes(),
        )
        .part(
            w,
            "rId24",
            "sharedStrings",
            "xl/sharedStrings.xml",
            &sml("sharedStrings"),
            sst.as_bytes(),
        )
        .part(
            w,
            "rId25",
            "externalLink",
            "xl/externalLinks/externalLink1.xml",
            &sml("externalLink"),
            link.as_bytes(),
        )
        .link(
            "xl/externalLinks/externalLink1.xml",
            external(
                "rId1",
                "externalLinkPath",
                "file:///D:/Projects/avery/SECRET-xlsx-book-1.xlsx",
            ),
        )
        .part(
            w,
            "rId26",
            "pivotCacheDefinition",
            "xl/pivotCache/pivotCacheDefinition1.xml",
            &sml("pivotCacheDefinition"),
            pivot.as_bytes(),
        )
        .part(
            "xl/pivotCache/pivotCacheDefinition1.xml",
            "rId1",
            "pivotCacheRecords",
            "xl/pivotCache/pivotCacheRecords1.xml",
            &sml("pivotCacheRecords"),
            records.as_bytes(),
        )
        .part(
            w,
            "rId27",
            "connections",
            "xl/connections.xml",
            &sml("connections"),
            connections.as_bytes(),
        )
        .part(
            w,
            "rId28",
            &format!("{MS}/2017/10/relationships/person"),
            "xl/persons/person.xml",
            "application/vnd.ms-excel.person+xml",
            persons.as_bytes(),
        )
        .part(
            sheet1,
            "rId1",
            "comments",
            "xl/comments1.xml",
            &sml("comments"),
            notes.as_bytes(),
        )
        .part(
            sheet1,
            "rId2",
            &format!("{MS}/2017/10/relationships/threadedComment"),
            "xl/threadedComments/threadedComment1.xml",
            "application/vnd.ms-excel.threadedcomments+xml",
            threaded.as_bytes(),
        )
        .part(
            sheet1,
            "rId3",
            "vmlDrawing",
            "xl/drawings/vmlDrawing1.vml",
            &format!("{OX}.vmlDrawing"),
            vml.as_bytes(),
        )
        .part(
            sheet1,
            "rId4",
            "printerSettings",
            "xl/printerSettings/printerSettings1.bin",
            &format!("{SML}.printerSettings"),
            b"\0\0SECRET-xlsx-printer-1",
        )
        .part(
            sheet1,
            "rId5",
            "drawing",
            "xl/drawings/drawing1.xml",
            &format!("{OX}.drawing+xml"),
            drawing.as_bytes(),
        )
        .part(
            "xl/drawings/drawing1.xml",
            "rId1",
            "chart",
            "xl/charts/chart1.xml",
            &format!("{OX}.drawingml.chart+xml"),
            chart.as_bytes(),
        );
    if variant.ends_with('m') {
        p = p.part(
            w,
            "rId29",
            &format!("{MS}/2006/relationships/vbaProject"),
            "xl/vbaProject.bin",
            "application/vnd.ms-office.vbaProject",
            b"\xCC\x61SECRET-xlsm-vba-1",
        );
    }
    p.zip()
}

/// The no-leak twin of fixture 45: visible sheets, rows and columns, contrasting colours, a
/// web link and every shared string in use.
pub fn twin_xlsx() -> Vec<u8> {
    let sheet = format!(
        "<worksheet xmlns=\"{S}\" xmlns:r=\"{R}\"><cols><col min=\"1\" max=\"2\" width=\"12\" customWidth=\"1\"/></cols><sheetData><row r=\"1\" customHeight=\"1\" ht=\"30\"><c r=\"A1\" t=\"s\"><v>0</v></c><c r=\"B1\"><f>1+1</f><v>2</v></c></row><row r=\"2\"><c r=\"A2\" s=\"0\"><v>5</v></c><c r=\"B2\"/></row></sheetData><hyperlinks><hyperlink ref=\"A1\" r:id=\"rId9\"/></hyperlinks></worksheet>"
    );
    let sst =
        format!("<sst xmlns=\"{S}\" count=\"1\" uniqueCount=\"1\"><si><t>Region</t></si></sst>");
    let w = "xl/workbook.xml";
    workbook(&[("Summary", "visible", &sheet)])
        .part(
            w,
            "rId22",
            "styles",
            "xl/styles.xml",
            &sml("styles"),
            XLSX_STYLES.as_bytes(),
        )
        .part(
            w,
            "rId23",
            "theme",
            "xl/theme/theme1.xml",
            THEME_TYPE,
            theme_xml().as_bytes(),
        )
        .part(
            w,
            "rId24",
            "sharedStrings",
            "xl/sharedStrings.xml",
            &sml("sharedStrings"),
            sst.as_bytes(),
        )
        .link(
            "xl/worksheets/sheet1.xml",
            external("rId9", "hyperlink", "https://example.com/"),
        )
        .zip()
}

fn group(children: &str) -> String {
    format!(
        "<p:grpSp><p:nvGrpSpPr><p:cNvPr id=\"20\" name=\"Group\"/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr><a:xfrm><a:off x=\"1000000\" y=\"1000000\"/><a:ext cx=\"2000000\" cy=\"2000000\"/><a:chOff x=\"0\" y=\"0\"/><a:chExt cx=\"1000000\" cy=\"1000000\"/></a:xfrm></p:grpSpPr>{children}</p:grpSp>"
    )
}

fn picture(id: u32, embed: &str, crop: &str) -> String {
    format!(
        "<p:pic><p:nvPicPr><p:cNvPr id=\"{id}\" name=\"Picture {id}\"/><p:cNvPicPr/><p:nvPr/></p:nvPicPr><p:blipFill><a:blip r:embed=\"{embed}\"/>{crop}<a:stretch><a:fillRect/></a:stretch></p:blipFill><p:spPr>{}<a:prstGeom prst=\"rect\"/></p:spPr></p:pic>",
        xfrm(100, 100, 1000, 1000)
    )
}

/// Fixture 46: a presentation with every PowerPoint leak the checks know.
pub fn leaky_pptx(variant: &str) -> Vec<u8> {
    let ext = "<p:extLst><p:ext uri=\"{BB962C8B-B14F-4D97-AF65-F5344CB8AC3E}\"><p14:creationId xmlns:p14=\"http://schemas.microsoft.com/office/powerpoint/2010/main\" val=\"1234567\"/></p:ext></p:extLst>";
    let link = "<p:sp><p:nvSpPr><p:cNvPr id=\"30\" name=\"Link\"><a:hlinkClick r:id=\"rId7\" action=\"ppaction://hlinksldjump\"/></p:cNvPr><p:cNvSpPr/><p:nvPr/></p:nvSpPr><p:spPr/><p:txBody><a:bodyPr/><a:p><a:r><a:t>See appendix</a:t></a:r></a:p></p:txBody></p:sp>";
    let ole = "<p:graphicFrame><p:nvGraphicFramePr><p:cNvPr id=\"40\" name=\"Object 1\"/><p:cNvGraphicFramePr/><p:nvPr/></p:nvGraphicFramePr><p:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"100\" cy=\"100\"/></p:xfrm><a:graphic><a:graphicData uri=\"http://schemas.openxmlformats.org/presentationml/2006/ole\"><p:oleObj name=\"Worksheet\" r:id=\"rId5\" imgW=\"100\" imgH=\"100\" progId=\"Excel.Sheet.12\"><p:embed/><p:pic><p:nvPicPr><p:cNvPr id=\"0\" name=\"\"/><p:cNvPicPr/><p:nvPr/></p:nvPicPr><p:blipFill><a:blip r:embed=\"rId6\"/><a:stretch><a:fillRect/></a:stretch></p:blipFill><p:spPr/></p:pic></p:oleObj></a:graphicData></a:graphic></p:graphicFrame>";
    let chart = "<p:graphicFrame><p:nvGraphicFramePr><p:cNvPr id=\"41\" name=\"Chart 1\"/><p:cNvGraphicFramePr/><p:nvPr/></p:nvGraphicFramePr><p:xfrm><a:off x=\"200\" y=\"200\"/><a:ext cx=\"100\" cy=\"100\"/></p:xfrm><a:graphic><a:graphicData uri=\"http://schemas.openxmlformats.org/drawingml/2006/chart\"><c:chart xmlns:c=\"http://schemas.openxmlformats.org/drawingml/2006/chart\" r:id=\"rId8\"/></a:graphicData></a:graphic></p:graphicFrame>";
    let shapes = [
        link.to_string(),
        group(
            &(shape(21, true, &xfrm(0, 0, 100, 100), "SECRET-pptx-shape-1")
                + &shape(
                    22,
                    false,
                    &xfrm(9_000_000, 0, 100, 100),
                    "SECRET-pptx-offslide-1",
                )),
        ),
        picture(23, "rId4", "<a:srcRect l=\"25000\" r=\"25000\"/>"),
        ole.into(),
        chart.into(),
    ]
    .concat();
    let timing = "<p:timing><p:tnLst><p:par><p:cTn id=\"1\" dur=\"indefinite\" restart=\"never\" nodeType=\"tmRoot\"><p:childTnLst><p:par><p:cTn id=\"2\"><p:childTnLst><p:animEffect><p:cBhvr><p:cTn id=\"3\"/><p:tgtEl><p:spTgt spid=\"22\"/></p:tgtEl></p:cBhvr></p:animEffect></p:childTnLst></p:cTn></p:par></p:childTnLst></p:cTn></p:par></p:tnLst></p:timing>";
    let slide1 = slide_xml(&shapes).replace("</p:cSld>", &format!("</p:cSld>{timing}{ext}"));
    let slide2 = slide_xml(&shape(
        2,
        false,
        &xfrm(0, 0, 100, 100),
        "SECRET-pptx-slide-1",
    ))
    .replace("<p:sld ", "<p:sld show=\"0\" ");
    let slide3 = slide_xml(&shape(2, false, &xfrm(0, 0, 100, 100), "Thanks"));
    let ph = |kind: &str, text: &str| {
        format!(
            "<p:sp><p:nvSpPr><p:cNvPr id=\"2\" name=\"{kind}\"/><p:cNvSpPr/><p:nvPr><p:ph type=\"{kind}\"/></p:nvPr></p:nvSpPr><p:spPr/><p:txBody><a:bodyPr/><a:p><a:r><a:t>{text}</a:t></a:r></a:p></p:txBody></p:sp>"
        )
    };
    let notes = format!(
        "<p:notes xmlns:p=\"{P}\" xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" xmlns:r=\"{R}\"><p:cSld><p:spTree><p:nvGrpSpPr><p:cNvPr id=\"1\" name=\"\"/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr/>{}{}</p:spTree></p:cSld></p:notes>",
        ph("sldImg", ""),
        ph("body", "SECRET-pptx-notes-1")
    );
    let legacy = format!(
        "<p:cmLst xmlns:p=\"{P}\"><p:cm authorId=\"0\" dt=\"2026-01-01T00:00:00\" idx=\"1\"><p:pos x=\"10\" y=\"10\"/><p:text>SECRET-pptx-comment-1</p:text></p:cm></p:cmLst>"
    );
    let authors = format!(
        "<p:cmAuthorLst xmlns:p=\"{P}\"><p:cmAuthor id=\"0\" name=\"Avery Example\" initials=\"AE\" lastIdx=\"1\" clrIdx=\"0\"/></p:cmAuthorLst>"
    );
    let modern = "<p188:cmLst xmlns:p188=\"http://schemas.microsoft.com/office/powerpoint/2018/8/main\" xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\"><p188:cm id=\"{33333333-3333-3333-3333-333333333333}\" authorId=\"{44444444-4444-4444-4444-444444444444}\" created=\"2026-01-01T00:00:00.000\"><p188:txBody><a:bodyPr/><a:p><a:r><a:t>SECRET-pptx-comment-2</a:t></a:r></a:p></p188:txBody></p188:cm></p188:cmLst>";
    let modern_authors = "<p188:authorLst xmlns:p188=\"http://schemas.microsoft.com/office/powerpoint/2018/8/main\"><p188:author id=\"{44444444-4444-4444-4444-444444444444}\" name=\"Sam Sample\" initials=\"SS\" userId=\"sam\" providerId=\"None\"/></p188:authorLst>";
    let chart_part = bar_chart(
        "<c:val><c:numLit><c:ptCount val=\"1\"/><c:pt idx=\"0\"><c:v>5</c:v></c:pt></c:numLit></c:val>",
        "<c:externalData r:id=\"rId1\"><c:autoUpdate val=\"0\"/></c:externalData>",
    );
    let sections = "<p:extLst><p:ext uri=\"{521415D9-36F7-43E2-AB2F-B90AF26B5E84}\"><p14:sectionLst xmlns:p14=\"http://schemas.microsoft.com/office/powerpoint/2010/main\"><p14:section name=\"Main\" id=\"{55555555-5555-5555-5555-555555555555}\"><p14:sldIdLst><p14:sldId id=\"256\"/><p14:sldId id=\"257\"/><p14:sldId id=\"258\"/></p14:sldIdLst></p14:section></p14:sectionLst></p:ext></p:extLst>";
    let pres = format!(
        "<p:presentation xmlns:p=\"{P}\" xmlns:r=\"{R}\"><p:sldIdLst><p:sldId id=\"256\" r:id=\"rId1\"/><p:sldId id=\"257\" r:id=\"rId2\"/><p:sldId id=\"258\" r:id=\"rId3\"/></p:sldIdLst><p:sldSz cx=\"9144000\" cy=\"6858000\"/><p:notesSz cx=\"6858000\" cy=\"9144000\"/><p:custShowLst><p:custShow name=\"Short\" id=\"0\"><p:sldLst><p:sld r:id=\"rId2\"/></p:sldLst></p:custShow></p:custShowLst>{sections}</p:presentation>"
    );
    let pr = "ppt/presentation.xml";
    let s1 = "ppt/slides/slide1.xml";
    let mut p =
        props(Pkg::new(variant), "Avery Example", "Example Corp").body(&with_masters(&pres, true));
    p = p
        .part(pr, "rId1", "slide", s1, SLIDE_TYPE, slide1.as_bytes())
        .part(pr, "rId2", "slide", "ppt/slides/slide2.xml", SLIDE_TYPE, slide2.as_bytes())
        .part(pr, "rId3", "slide", "ppt/slides/slide3.xml", SLIDE_TYPE, slide3.as_bytes())
        .part(pr, "rId10", "commentAuthors", "ppt/commentAuthors.xml", &pml("commentAuthors"), authors.as_bytes())
        .part(pr, "rId11", &format!("{MS}/2018/10/relationships/authors"), "ppt/authors.xml", "application/vnd.ms-powerpoint.authors+xml", modern_authors.as_bytes())
        .part(pr, "rId12", &format!("{MS}/2011/relationships/webextension"), "ppt/webextensions/webextension1.xml", "application/vnd.ms-office.webextension+xml", b"<we:webextension xmlns:we=\"http://schemas.microsoft.com/office/webextensions/webextension/2010/11\" id=\"{66666666-6666-6666-6666-666666666666}\"><we:reference id=\"wa1\" version=\"1.0\" store=\"en-US\" storeType=\"OMEX\"/><we:properties><we:property name=\"matter\" value=\"&quot;SECRET-pptx-addin-1&quot;\"/></we:properties></we:webextension>")
        .part(s1, "rId2", "notesSlide", "ppt/notesSlides/notesSlide1.xml", &pml("notesSlide"), notes.as_bytes())
        .part(s1, "rId3", "comments", "ppt/comments/comment1.xml", &pml("comments"), legacy.as_bytes())
        .part(s1, "rId4", "image", "ppt/media/image1.jpeg", "", &photo(40, 20))
        .part(s1, "rId5", "package", "ppt/embeddings/Microsoft_Excel_Worksheet.xlsx", &format!("{SML}.sheet"), b"PK\x03\x04SECRET-pptx-ole-1")
        .part(s1, "rId6", "image", "ppt/media/image2.png", "", &png(4, 4))
        .link(s1, rel("rId7", "slide", "slide2.xml"))
        .part(s1, "rId8", "chart", "ppt/charts/chart1.xml", &format!("{OX}.drawingml.chart+xml"), chart_part.as_bytes())
        .part("ppt/charts/chart1.xml", "rId1", "package", "ppt/embeddings/Microsoft_Excel_Worksheet1.xlsx", &format!("{SML}.sheet"), b"PK\x03\x04SECRET-pptx-chart-1")
        .part(s1, "rId9", "tags", "ppt/tags/tag1.xml", &pml("tags"), format!("<p:tagLst xmlns:p=\"{P}\"><p:tag name=\"MATTER\" val=\"SECRET-pptx-tag-1\"/></p:tagLst>").as_bytes())
        .part("ppt/slides/slide3.xml", "rId1", &format!("{MS}/2018/10/relationships/comments"), "ppt/comments/modernComment_1.xml", "application/vnd.ms-powerpoint.comments+xml", modern.as_bytes())
        .link("ppt/notesSlides/notesSlide1.xml", rel("rId1", "slide", "../slides/slide1.xml"));
    p = masters(
        p,
        &[s1, "ppt/slides/slide2.xml", "ppt/slides/slide3.xml"],
        &["ppt/notesSlides/notesSlide1.xml"],
    );
    if variant.ends_with('m') {
        p = p.part(
            pr,
            "rId13",
            &format!("{MS}/2006/relationships/vbaProject"),
            "ppt/vbaProject.bin",
            "application/vnd.ms-office.vbaProject",
            b"\xCC\x61SECRET-pptm-vba-1",
        );
    }
    p.zip()
}

/// The no-leak twin of fixture 46: visible slides and shapes, a notes page with only its
/// placeholders, an uncropped picture and a web link.
pub fn twin_pptx() -> Vec<u8> {
    let link = "<p:sp><p:nvSpPr><p:cNvPr id=\"30\" name=\"Link\"><a:hlinkClick r:id=\"rId7\"/></p:cNvPr><p:cNvSpPr/><p:nvPr/></p:nvSpPr><p:spPr/><p:txBody><a:bodyPr/><a:p><a:r><a:t>example.com</a:t></a:r></a:p></p:txBody></p:sp>";
    let shapes = [
        link.to_string(),
        group(
            &(shape(21, false, &xfrm(0, 0, 100, 100), "Inside")
                + &shape(22, false, &xfrm(3_000_000, 0, 100, 100), "Edge")),
        ),
        picture(23, "rId4", "<a:srcRect/>"),
        shape(24, false, "", "Title placeholder"),
    ]
    .concat();
    let notes = format!(
        "<p:notes xmlns:p=\"{P}\" xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\"><p:cSld><p:spTree><p:nvGrpSpPr><p:cNvPr id=\"1\" name=\"\"/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr/><p:sp><p:nvSpPr><p:cNvPr id=\"2\" name=\"n\"/><p:cNvSpPr/><p:nvPr><p:ph type=\"sldNum\"/></p:nvPr></p:nvSpPr><p:spPr/><p:txBody><a:bodyPr/><a:p><a:fld id=\"{{B6F15528-21DE-4FAA-801E-634DDDAF4B2B}}\" type=\"slidenum\"><a:t>1</a:t></a:fld></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:notes>"
    );
    let s1 = "ppt/slides/slide1.xml";
    let pres = format!(
        "<p:presentation xmlns:p=\"{P}\" xmlns:r=\"{R}\"><p:sldIdLst><p:sldId id=\"256\" r:id=\"rId1\"/><p:sldId id=\"257\" r:id=\"rId2\"/></p:sldIdLst><p:sldSz cx=\"12192000\" cy=\"6858000\"/><p:notesSz cx=\"6858000\" cy=\"9144000\"/></p:presentation>"
    );
    let notes_slide = "ppt/notesSlides/notesSlide1.xml";
    let deck = presentation(&[&slide_xml(&shapes), &slide_xml("")])
        .body(&with_masters(&pres, true))
        .link(notes_slide, rel("rId1", "slide", "../slides/slide1.xml"));
    masters(deck, &[s1, "ppt/slides/slide2.xml"], &[notes_slide])
        .part(
            s1,
            "rId2",
            "notesSlide",
            "ppt/notesSlides/notesSlide1.xml",
            &pml("notesSlide"),
            notes.as_bytes(),
        )
        .part(
            s1,
            "rId4",
            "image",
            "ppt/media/image1.jpeg",
            "",
            &jpeg(&[], b""),
        )
        .link(s1, external("rId7", "hyperlink", "https://example.com/"))
        .zip()
}
