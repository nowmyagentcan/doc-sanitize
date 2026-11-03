//! Checks that work the same in Word, Excel and PowerPoint packages.

use std::collections::HashSet;

use quick_xml::events::Event;

use super::{Package, key, part_name, xml};
use crate::report::{CheckId, Error, Finding, Format, Where, plural, safe, value};

const SHARED_STRINGS: &str =
    "application/vnd.openxmlformats-officedocument.spreadsheetml.sharedStrings+xml";
const CELL_SHEETS: [&str; 2] = [
    "application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml",
    "application/vnd.ms-excel.macrosheet+xml",
];
const GLOSSARY: &str =
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document.glossary+xml";
const SPREADSHEET: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";

fn finding(
    kind: &'static str,
    part: Option<&str>,
    count: u64,
    unit: &'static str,
    detail: String,
) -> Finding {
    Finding {
        check: CheckId::LeftoverData,
        kind,
        place: Where {
            part: part.map(safe),
        },
        count,
        unit,
        detail,
        value: None,
    }
}

/// The part a relationship part belongs to ("" for the package), from its name.
fn rels_source(name: &str) -> Option<String> {
    let (dir, file) = name.rsplit_once('/')?;
    let file = file.strip_suffix(".rels")?;
    let parent = dir.strip_suffix("_rels")?;
    Some(format!("{parent}{file}"))
}

/// `leftover-data`: parts nothing links to, entries with no content type, unused shared
/// strings, the glossary, the ZIP comment and bytes outside the package.
pub fn leftover_data(pkg: &Package, values: bool) -> Result<Vec<Finding>, Error> {
    let reachable = pkg.reachable()?;
    let reached: HashSet<String> = reachable.iter().map(|n| key(n)).collect();
    let mut orphans = vec![];
    let mut untyped = vec![];
    for e in &pkg.zip.entries {
        let name = e.name.as_str();
        let folder = name.ends_with('/') && e.data.is_empty();
        if name.eq_ignore_ascii_case("[Content_Types].xml") || folder {
            continue;
        }
        let rels = name.to_ascii_lowercase().ends_with(".rels");
        if !part_name(name) || (pkg.content_type(name).is_none() && !rels) {
            untyped.push(name);
        } else if pkg.content_type(name).is_some() {
            let used = match rels_source(name) {
                Some(source) => source.is_empty() || reached.contains(&key(&source)),
                None => reached.contains(&key(name)),
            };
            if !used {
                orphans.push(name);
            }
        }
    }
    let mut found: Vec<Finding> = orphans
        .into_iter()
        .map(|n| {
            finding(
                "orphan_parts",
                Some(n),
                1,
                "parts",
                "1 part that nothing in the file links to".into(),
            )
        })
        .collect();
    found.extend(untyped.into_iter().map(|n| {
        finding(
            "untyped_entries",
            Some(n),
            1,
            "parts",
            "1 ZIP entry with no content type".into(),
        )
    }));
    let typed = |t: &'static str| {
        reachable
            .iter()
            .filter(move |n| pkg.content_type(n) == Some(t))
    };
    if pkg.format == Format::Xlsx {
        for sst in typed(SHARED_STRINGS) {
            let sheets = CELL_SHEETS.iter().flat_map(|t| typed(t));
            if let Some(f) = unused_strings(pkg, sst, sheets, values)? {
                found.push(f);
            }
        }
    }
    if pkg.format == Format::Docx {
        for g in typed(GLOSSARY) {
            let detail = "1 glossary document (building blocks and AutoText)".into();
            found.push(finding("glossary", Some(g), 1, "parts", detail));
        }
    }
    let zip = &pkg.zip;
    if !zip.comment.is_empty() {
        let n = zip.comment.len() as u64;
        let mut f = finding(
            "zip_comment",
            None,
            n,
            "bytes",
            format!("1 ZIP comment of {}", plural(n, "byte")),
        );
        if values {
            f.value = Some(value(&String::from_utf8_lossy(
                &pkg.bytes[zip.comment.clone()],
            )));
        }
        found.push(f);
    }
    for (n, place) in [
        (zip.before, "before the package"),
        (zip.between, "between the package's entries"),
        (zip.after, "after the package"),
    ] {
        if n > 0 {
            let n = n as u64;
            found.push(finding(
                "data_outside_package",
                None,
                n,
                "bytes",
                format!("{} {place}", plural(n, "byte")),
            ));
        }
    }
    Ok(found)
}

/// Shared strings in `sst` that no cell of `sheets` uses, as one finding; with `values`, the
/// first unused string's text.
fn unused_strings<'n>(
    pkg: &Package,
    sst: &str,
    sheets: impl Iterator<Item = &'n String>,
    values: bool,
) -> Result<Option<Finding>, Error> {
    let mut used = HashSet::new();
    for sheet in sheets {
        let bytes = pkg.read(sheet)?;
        let text = xml::decode(&bytes, sheet)?;
        let mut r = xml::Reader::new(&text, sheet);
        let (mut shared, mut cell_value, mut index) = (false, false, String::new());
        loop {
            match r.event()? {
                Event::Start(e) if r.is(&e, SPREADSHEET, "c") => {
                    shared = r.attr(&e, "t")?.as_deref() == Some("s");
                }
                Event::Start(e) if shared && r.is(&e, SPREADSHEET, "v") => cell_value = true,
                Event::End(e) if cell_value && e.local_name().as_ref() == "v" => {
                    if let Ok(i) = index.trim().parse::<usize>() {
                        used.insert(i);
                    }
                    (cell_value, shared) = (false, false);
                    index.clear();
                }
                ev if cell_value => index += &xml::text(&ev).unwrap_or_default(),
                Event::Eof => break,
                _ => {}
            }
        }
    }
    let bytes = pkg.read(sst)?;
    let text = xml::decode(&bytes, sst)?;
    let mut r = xml::Reader::new(&text, sst);
    let (mut count, mut unused, mut depth, mut first) = (0usize, 0u64, 0usize, None::<String>);
    let mut current = String::new();
    loop {
        let ev = r.event()?;
        match &ev {
            Event::Start(e) if depth == 1 && r.is(e, SPREADSHEET, "si") => {
                current.clear();
            }
            Event::Empty(e) if depth == 1 && r.is(e, SPREADSHEET, "si") => {
                if !used.contains(&count) {
                    unused += 1;
                    first.get_or_insert_default();
                }
                count += 1;
            }
            Event::End(e) if depth == 2 && e.local_name().as_ref() == "si" => {
                if !used.contains(&count) {
                    unused += 1;
                    if values && first.is_none() {
                        first = Some(current.clone());
                    }
                }
                count += 1;
            }
            Event::Eof => break,
            _ if values && depth >= 2 => current += &xml::text(&ev).unwrap_or_default(),
            _ => {}
        }
        match ev {
            Event::Start(_) => depth += 1,
            Event::End(_) => depth -= 1,
            _ => {}
        }
    }
    if unused == 0 {
        return Ok(None);
    }
    let detail = format!("{} that no cell uses", plural(unused, "shared string"));
    let mut f = finding("unused_strings", Some(sst), unused, "strings", detail);
    f.value = first.filter(|_| values).map(|t| value(&t));
    Ok(Some(f))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn part_names_follow_opc() {
        for good in ["word/document.xml", "[trash]/0001.dat", "C:/x"] {
            assert!(part_name(good), "{good}");
        }
        for bad in [
            "../evil",
            "/abs",
            "word\\document.xml",
            "a//b",
            "a/./b",
            "a.",
            "word/",
        ] {
            assert!(!part_name(bad), "{bad}");
        }
    }

    #[test]
    fn relationship_parts_name_their_source() {
        assert_eq!(rels_source("_rels/.rels").as_deref(), Some(""));
        assert_eq!(
            rels_source("word/_rels/document.xml.rels").as_deref(),
            Some("word/document.xml")
        );
        assert_eq!(rels_source("word/document.xml"), None);
        assert_eq!(rels_source("word/x.rels"), None);
    }
}
