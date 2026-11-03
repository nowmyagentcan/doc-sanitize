//! Checks that work the same in Word, Excel and PowerPoint packages.

use std::collections::HashSet;

use quick_xml::events::Event;

use super::{Ctx, Package, key, ns, part_name, xml};
use crate::report::{CheckId, Error, Finding, Format, Where, counted, plural, safe, value};

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
            ..Where::default()
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
    let reachable = &pkg.parts;
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

/// The distinct parts that relationships of type `short` point at.
pub fn targets<'p>(pkg: &'p Package, short: &str) -> Vec<&'p str> {
    let mut seen = HashSet::new();
    pkg.links
        .iter()
        .filter(|l| l.rel.short() == short)
        .filter_map(|l| l.part.as_deref())
        .filter(|p| seen.insert(key(p)))
        .collect()
}

/// `name=value` pairs joined by `; `.
fn pairs(fields: &[(String, String)]) -> String {
    let shown: Vec<_> = fields.iter().map(|(n, v)| format!("{n}={v}")).collect();
    shown.join("; ")
}

/// The root's children that hold text or elements, as (namespace, local name, text); the text
/// of separate elements inside one child is joined by `, `.
fn fields(pkg: &Package, part: &str) -> Result<Vec<(String, String, String)>, Error> {
    let mut out = vec![];
    let mut open: Option<(String, String, String, bool)> = None;
    let mut fresh = true;
    pkg.walk(part, |r, ev| {
        match ev {
            Event::Start(e) if r.level() == 1 => {
                open = Some((r.ns(e).into(), xml::local(e), String::new(), false));
                fresh = true;
            }
            Event::End(_) if r.level() == 1 => {
                if let Some((ns, local, text, child)) = open.take()
                    && (child || !text.trim().is_empty())
                {
                    out.push((ns, local, text));
                }
            }
            Event::Start(_) | Event::Empty(_) | Event::End(_) if r.level() >= 2 => {
                if let Some(o) = &mut open {
                    o.3 |= !matches!(ev, Event::End(_));
                }
                fresh = true;
            }
            ev => {
                if let (Some(o), Some(t)) = (&mut open, xml::text(ev)) {
                    if fresh && !o.2.is_empty() {
                        o.2 += ", ";
                    }
                    o.2 += &t;
                    fresh = false;
                }
            }
        }
        Ok(())
    })?;
    Ok(out)
}

/// app.xml fields that say nothing about the people or places behind a file.
const PLAIN_APP_FIELDS: [&str; 14] = [
    "DocSecurity",
    "ScaleCrop",
    "LinksUpToDate",
    "SharedDoc",
    "HyperlinksChanged",
    "Pages",
    "Words",
    "Characters",
    "CharactersWithSpaces",
    "Lines",
    "Paragraphs",
    "Slides",
    "Notes",
    "HiddenSlides",
];

/// `metadata`: document, application and custom properties, sensitivity labels, thumbnails,
/// printer settings, tracking ids and editor names.
pub fn metadata(ctx: &Ctx) -> Result<Vec<Finding>, Error> {
    let (pkg, values) = (ctx.pkg, ctx.values);
    let mut found = vec![];
    let mut add = |kind, part: &str, n: usize, unit, detail: String, text: Option<String>| {
        let f = Finding::new(
            CheckId::Metadata,
            kind,
            Where::part(part),
            n as u64,
            unit,
            detail,
        );
        found.push(f.with_value(values, text.as_deref()));
    };
    let props = |n| if n == 1 { "property" } else { "properties" };
    for part in targets(pkg, "core-properties") {
        let set: Vec<_> = fields(pkg, part)?
            .into_iter()
            .map(|(ns, local, text)| {
                let prefix = match ns.as_str() {
                    ns::DC => "dc:",
                    ns::DCTERMS => "dcterms:",
                    ns::CP => "cp:",
                    _ => "",
                };
                (format!("{prefix}{local}"), text)
            })
            .collect();
        if !set.is_empty() {
            let author = set
                .iter()
                .any(|(n, _)| n == "dc:creator" || n == "cp:lastModifiedBy");
            let including = if author { ", including the author" } else { "" };
            let detail = format!("{} document {}{including}", set.len(), props(set.len()));
            add(
                "core_properties",
                part,
                set.len(),
                "fields",
                detail,
                Some(pairs(&set)),
            );
        }
    }
    for part in targets(pkg, "extended-properties") {
        let set: Vec<_> = fields(pkg, part)?
            .into_iter()
            .filter(|(ns, local, _)| ns == ns::APP && !PLAIN_APP_FIELDS.contains(&local.as_str()))
            .map(|(_, local, text)| (local, text))
            .collect();
        if !set.is_empty() {
            let detail = format!("{} application {}", set.len(), props(set.len()));
            add(
                "app_properties",
                part,
                set.len(),
                "fields",
                detail,
                Some(pairs(&set)),
            );
        }
    }
    for part in targets(pkg, "custom-properties") {
        let (labels, custom): (Vec<_>, Vec<_>) = custom_properties(pkg, part)?
            .into_iter()
            .partition(|(name, _)| name.starts_with("MSIP_Label_"));
        if !custom.is_empty() {
            let detail = format!("{} custom {}", custom.len(), props(custom.len()));
            add(
                "custom_properties",
                part,
                custom.len(),
                "fields",
                detail,
                Some(pairs(&custom)),
            );
        }
        if !labels.is_empty() {
            let detail = plural(labels.len() as u64, "sensitivity label field");
            add(
                "sensitivity_label",
                part,
                labels.len(),
                "fields",
                detail,
                Some(pairs(&labels)),
            );
        }
    }
    for part in targets(pkg, "classificationlabels") {
        let mut labels = vec![];
        pkg.walk(part, |r, ev| {
            if let Event::Start(e) | Event::Empty(e) = ev
                && e.local_name().as_ref() == "label"
            {
                labels.push(pairs(&[(
                    "id".into(),
                    r.attr(e, "id")?.unwrap_or_default().into(),
                )]));
            }
            Ok(())
        })?;
        let n = labels.len().max(1);
        let detail = plural(n as u64, "sensitivity label");
        add(
            "sensitivity_label",
            part,
            n,
            "fields",
            detail,
            labels.into_iter().next(),
        );
    }
    for (short, kind, detail) in [
        (
            "thumbnail",
            "thumbnail",
            "1 thumbnail picture of the first page",
        ),
        (
            "printerSettings",
            "printer_settings",
            "1 printer settings part",
        ),
    ] {
        for part in targets(pkg, short) {
            add(kind, part, 1, "parts", detail.into(), None);
        }
    }
    let revision_info = targets(pkg, "revisionInfo");
    for part in &pkg.parts {
        let xml = pkg.content_type(part).is_some_and(|t| t.ends_with("+xml"));
        if !xml {
            continue;
        }
        let revision = revision_info.contains(&part.as_str());
        let (mut ids, names) = tracking_ids(pkg, part, revision)?;
        if revision {
            ids = ids.max(1);
        }
        if ids > 0 {
            let what = match pkg.format {
                Format::Docx => "editing-session id",
                _ => "tracking id",
            };
            add(
                "tracking_ids",
                part,
                ids,
                "ids",
                plural(ids as u64, what),
                None,
            );
        }
        if !names.is_empty() {
            let detail = plural(names.len() as u64, "editor name");
            add(
                "editor_names",
                part,
                names.len(),
                "names",
                detail,
                names.first().cloned(),
            );
        }
    }
    Ok(found)
}

/// Comment and author parts: the format, the relationship type, the kind, the element each
/// item is, the unit and the detail's noun.
const COMMENT_PARTS: [(Format, &str, &str, &str, &str, &str); 8] = [
    (
        Format::Docx,
        "comments",
        "comment",
        "comment",
        "comments",
        "comment",
    ),
    (
        Format::Docx,
        "people",
        "people",
        "person",
        "people",
        "comment author",
    ),
    (
        Format::Xlsx,
        "comments",
        "note",
        "comment",
        "comments",
        "note",
    ),
    (
        Format::Xlsx,
        "threadedComment",
        "threaded_comment",
        "threadedComment",
        "comments",
        "threaded comment",
    ),
    (
        Format::Xlsx,
        "person",
        "people",
        "person",
        "people",
        "comment author",
    ),
    (
        Format::Pptx,
        "comments",
        "comment",
        "cm",
        "comments",
        "comment",
    ),
    (
        Format::Pptx,
        "commentAuthors",
        "people",
        "cmAuthor",
        "people",
        "comment author",
    ),
    (
        Format::Pptx,
        "authors",
        "people",
        "author",
        "people",
        "comment author",
    ),
];

/// `comments`: comments and notes with their authors, and the parts that list the authors.
pub fn comments(ctx: &Ctx) -> Result<Vec<Finding>, Error> {
    let pkg = ctx.pkg;
    let mut found = vec![];
    for (format, short, kind, item, unit, noun) in COMMENT_PARTS {
        if format != pkg.format {
            continue;
        }
        for part in targets(pkg, short) {
            let (mut n, mut authors, mut first) = (0, HashSet::new(), None::<String>);
            let mut inside = None;
            pkg.walk(part, |r, ev| {
                match ev {
                    Event::Start(e) | Event::Empty(e) if e.local_name().as_ref() == item => {
                        n += 1;
                        for (_, name, v) in r.attrs(e)? {
                            let name = name.as_str();
                            if kind == "people" {
                                if first.is_none()
                                    && ["author", "displayName", "name"].contains(&name)
                                {
                                    first = Some(v.into());
                                }
                            } else if ["author", "authorId", "personId"].contains(&name) {
                                authors.insert(v.into_owned());
                            }
                        }
                        if matches!(ev, Event::Start(_)) && n == 1 {
                            inside = Some(r.level());
                        }
                    }
                    Event::End(_) if inside == Some(r.level()) => inside = None,
                    ev if inside.is_some() && kind != "people" => {
                        *first.get_or_insert_default() += &xml::text(ev).unwrap_or_default();
                    }
                    _ => {}
                }
                Ok(())
            })?;
            if n == 0 {
                continue;
            }
            let mut detail = plural(n as u64, noun);
            if kind != "people" && !authors.is_empty() {
                let by = counted(authors.len() as u64, "person", "people");
                detail += &format!(" by {by}");
            }
            let f = Finding::new(
                CheckId::Comments,
                kind,
                ctx.place(part),
                n as u64,
                unit,
                detail,
            );
            found.push(f.with_value(ctx.values, first.as_deref()));
        }
    }
    Ok(found)
}

/// The custom properties of a custom.xml part, as (name, value).
fn custom_properties(pkg: &Package, part: &str) -> Result<Vec<(String, String)>, Error> {
    let mut out: Vec<(String, String)> = vec![];
    pkg.walk(part, |r, ev| {
        match ev {
            Event::Start(e) | Event::Empty(e)
                if r.level() == 1 && r.is(e, ns::CUSTOM, "property") =>
            {
                out.push((r.attr(e, "name")?.unwrap_or_default().into(), String::new()));
            }
            ev if r.level() >= 2 => {
                if let (Some(p), Some(t)) = (out.last_mut(), xml::text(ev)) {
                    p.1 += &t;
                }
            }
            _ => {}
        }
        Ok(())
    })?;
    Ok(out)
}

/// The tracking ids in one part and the editor names it holds.
fn tracking_ids(pkg: &Package, part: &str, revision: bool) -> Result<(usize, Vec<String>), Error> {
    let (mut ids, mut names) = (0, vec![]);
    let format = pkg.format;
    pkg.walk(part, |r, ev| {
        let (Event::Start(e) | Event::Empty(e)) = ev else {
            return Ok(());
        };
        let raw = e.attributes_raw();
        match format {
            Format::Docx => {
                if raw.contains("rsid") {
                    let attrs = r.attrs(e)?;
                    ids += attrs
                        .iter()
                        .filter(|(n, l, _)| n == ns::W && l.starts_with("rsid"))
                        .count();
                }
                let id = r.is(e, ns::W, "rsid") || r.is(e, ns::W, "rsidRoot");
                if id || r.is(e, ns::W14, "docId") || r.is(e, ns::W15, "docId") {
                    ids += 1;
                }
                if r.is(e, ns::W, "permStart")
                    && let Some(ed) = r.attr_ns(e, ns::W, "ed")?
                {
                    names.push(ed.into_owned());
                }
            }
            Format::Xlsx => {
                if raw.contains("uid") && r.attr_ns(e, ns::XR, "uid")?.is_some() {
                    ids += 1;
                }
                if r.is(e, ns::XR, "revisionPtr") {
                    ids += 1;
                }
                if r.is(e, ns::S, "fileSharing")
                    && let Some(user) = r.attr(e, "userName")?.filter(|u| !u.is_empty())
                {
                    names.push(user.into_owned());
                }
            }
            Format::Pptx => {
                if r.is(e, ns::P14, "creationId")
                    || (revision && e.local_name().as_ref() == "client")
                {
                    ids += 1;
                }
            }
            Format::Pdf => {}
        }
        Ok(())
    })?;
    Ok((ids, names))
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
