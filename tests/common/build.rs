//! XML for the parts of test packages.

use super::*;

pub const CORE: &str = "http://schemas.openxmlformats.org/package/2006/metadata/core-properties";

pub const CORE_TYPE: &str = "application/vnd.openxmlformats-package.core-properties+xml";

pub const APP_NS: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/extended-properties";

pub fn core_xml(fields: &str) -> String {
    format!(
        "<cp:coreProperties xmlns:cp=\"{CORE}\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\" xmlns:dcterms=\"http://purl.org/dc/terms/\" xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\">{fields}</cp:coreProperties>"
    )
}

pub const SHEET_TYPE: &str =
    "application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml";

pub const SLIDE_TYPE: &str =
    "application/vnd.openxmlformats-officedocument.presentationml.slide+xml";

/// A workbook whose sheets (name, state, body) are linked as rId1, rId2, … from workbook.xml
/// and saved as xl/worksheets/sheetN.xml.
pub fn workbook(sheets: &[(&str, &str, &str)]) -> Pkg {
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
pub fn presentation(slides: &[&str]) -> Pkg {
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

pub fn sheet_xml(data: &str) -> String {
    format!("<worksheet xmlns=\"{S}\" xmlns:r=\"{R}\"><sheetData>{data}</sheetData></worksheet>")
}

pub fn slide_xml(shapes: &str) -> String {
    format!(
        "<p:sld xmlns:p=\"{P}\" xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" xmlns:r=\"{R}\"><p:cSld><p:spTree><p:nvGrpSpPr><p:cNvPr id=\"1\" name=\"\"/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr/>{shapes}</p:spTree></p:cSld></p:sld>"
    )
}

pub fn doc_xml(body: &str) -> String {
    format!(
        "<w:document xmlns:w=\"{W}\" xmlns:r=\"{R}\" xmlns:mc=\"http://schemas.openxmlformats.org/markup-compatibility/2006\" xmlns:wp=\"http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing\" xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" xmlns:pic=\"http://schemas.openxmlformats.org/drawingml/2006/picture\" xmlns:v=\"urn:schemas-microsoft-com:vml\" xmlns:o=\"urn:schemas-microsoft-com:office:office\"><w:body>{body}</w:body></w:document>"
    )
}

pub const BY_SAM: &str = "w:author=\"Sam Sample\" w:date=\"2026-01-01T00:00:00Z\"";

pub const BY_AVERY: &str = "w:author=\"Avery Example\"";

/// A complete theme: Office refuses a file whose theme lacks any of its schemes.
pub fn theme_xml() -> String {
    "<a:theme xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" name=\"Office\"><a:themeElements><a:clrScheme name=\"Office\"><a:dk1><a:sysClr val=\"windowText\" lastClr=\"000000\"/></a:dk1><a:lt1><a:sysClr val=\"window\" lastClr=\"FFFFFF\"/></a:lt1><a:dk2><a:srgbClr val=\"44546A\"/></a:dk2><a:lt2><a:srgbClr val=\"E7E6E6\"/></a:lt2><a:accent1><a:srgbClr val=\"4472C4\"/></a:accent1><a:accent2><a:srgbClr val=\"ED7D31\"/></a:accent2><a:accent3><a:srgbClr val=\"A5A5A5\"/></a:accent3><a:accent4><a:srgbClr val=\"FFC000\"/></a:accent4><a:accent5><a:srgbClr val=\"5B9BD5\"/></a:accent5><a:accent6><a:srgbClr val=\"70AD47\"/></a:accent6><a:hlink><a:srgbClr val=\"0563C1\"/></a:hlink><a:folHlink><a:srgbClr val=\"954F72\"/></a:folHlink></a:clrScheme><a:fontScheme name=\"Office\"><a:majorFont><a:latin typeface=\"Calibri Light\"/><a:ea typeface=\"\"/><a:cs typeface=\"\"/></a:majorFont><a:minorFont><a:latin typeface=\"Calibri\"/><a:ea typeface=\"\"/><a:cs typeface=\"\"/></a:minorFont></a:fontScheme><a:fmtScheme name=\"Office\"><a:fillStyleLst><a:solidFill><a:schemeClr val=\"phClr\"/></a:solidFill><a:solidFill><a:schemeClr val=\"phClr\"/></a:solidFill><a:solidFill><a:schemeClr val=\"phClr\"/></a:solidFill></a:fillStyleLst><a:lnStyleLst><a:ln w=\"6350\"><a:solidFill><a:schemeClr val=\"phClr\"/></a:solidFill></a:ln><a:ln w=\"12700\"><a:solidFill><a:schemeClr val=\"phClr\"/></a:solidFill></a:ln><a:ln w=\"19050\"><a:solidFill><a:schemeClr val=\"phClr\"/></a:solidFill></a:ln></a:lnStyleLst><a:effectStyleLst><a:effectStyle><a:effectLst/></a:effectStyle><a:effectStyle><a:effectLst/></a:effectStyle><a:effectStyle><a:effectLst/></a:effectStyle></a:effectStyleLst><a:bgFillStyleLst><a:solidFill><a:schemeClr val=\"phClr\"/></a:solidFill><a:solidFill><a:schemeClr val=\"phClr\"/></a:solidFill><a:solidFill><a:schemeClr val=\"phClr\"/></a:solidFill></a:bgFillStyleLst></a:fmtScheme></a:themeElements></a:theme>".into()
}

pub const THEME_TYPE: &str = "application/vnd.openxmlformats-officedocument.theme+xml";

pub fn text_run(props: &str, text: &str) -> String {
    format!("<w:r><w:rPr>{props}</w:rPr><w:t xml:space=\"preserve\">{text}</w:t></w:r>")
}

pub fn para(runs: &str) -> String {
    format!("<w:p>{runs}</w:p>")
}

pub fn styles_xml(body: &str) -> String {
    format!("<styleSheet xmlns=\"{S}\">{body}</styleSheet>")
}

pub fn shape(id: u32, hidden: bool, xfrm: &str, text: &str) -> String {
    let hidden = if hidden { " hidden=\"1\"" } else { "" };
    format!(
        "<p:sp><p:nvSpPr><p:cNvPr id=\"{id}\" name=\"Shape {id}\"{hidden}/><p:cNvSpPr/><p:nvPr/></p:nvSpPr><p:spPr>{xfrm}</p:spPr><p:txBody><a:bodyPr/><a:p><a:r><a:t>{text}</a:t></a:r></a:p></p:txBody></p:sp>"
    )
}

pub fn xfrm(x: i64, y: i64, cx: i64, cy: i64) -> String {
    format!("<a:xfrm><a:off x=\"{x}\" y=\"{y}\"/><a:ext cx=\"{cx}\" cy=\"{cy}\"/></a:xfrm>")
}

/// A JPEG whose EXIF block (none when there are no tags) holds `tags` (each a short), with optional bytes after the end.
pub fn jpeg(tags: &[u16], after: &[u8]) -> Vec<u8> {
    let mut exif = b"Exif\0\0II*\0\x08\0\0\0".to_vec();
    exif.extend((tags.len() as u16).to_le_bytes());
    for t in tags {
        exif.extend(t.to_le_bytes());
        exif.extend([3, 0, 1, 0, 0, 0, 6, 0, 0, 0]);
    }
    exif.extend([0; 4]);
    let mut b = vec![0xFF, 0xD8];
    if !tags.is_empty() {
        b.extend([0xFF, 0xE1]);
        b.extend(((exif.len() + 2) as u16).to_be_bytes());
        b.extend(exif);
    }
    b.extend([0xFF, 0xDA, 0, 2, 0x12, 0x34, 0xFF, 0xD9]);
    b.extend(after);
    b
}

/// A real `w × h` JPEG (a colour gradient) that decoders read.
pub fn photo(w: u32, h: u32) -> Vec<u8> {
    let rgb = |x: u32, y: u32| image::Rgb([(x * 255 / w) as u8, (y * 255 / h) as u8, 128]);
    let image = image::RgbImage::from_fn(w, h, rgb);
    let mut out = vec![];
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 90)
        .encode_image(&image)
        .unwrap();
    out
}

pub fn picture_doc(media: &[(&str, &[u8])], body: &str) -> Vec<u8> {
    let mut p = Pkg::new("docx").body(&doc_xml(body));
    for (i, (name, bytes)) in media.iter().enumerate() {
        p = p.part(
            "word/document.xml",
            &format!("rId{}", i + 1),
            "image",
            name,
            "",
            bytes,
        );
    }
    p.zip()
}

pub fn blip(id: &str, crop: &str) -> String {
    format!(
        "<w:p><w:r><w:drawing><wp:inline><wp:docPr id=\"1\" name=\"p\"/><a:graphic><a:graphicData><pic:pic><pic:blipFill><a:blip r:embed=\"{id}\"/>{crop}<a:stretch/></pic:blipFill></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>"
    )
}

/// The `[Content_Types].xml` of `entries` with `part`'s Override changed to `ct`.
pub fn zip_types_with(entries: &[Entry], part: &str, ct: &str) -> Vec<u8> {
    let types = String::from_utf8(entries[0].data.clone()).unwrap();
    let from = format!("<Override PartName=\"/{part}\" ContentType=\"{SHEET_TYPE}\"/>");
    assert!(types.contains(&from));
    types
        .replace(
            &from,
            &format!("<Override PartName=\"/{part}\" ContentType=\"{ct}\"/>"),
        )
        .into_bytes()
}
