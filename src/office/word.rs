//! Checks that only Word documents have.

use std::collections::{HashMap, HashSet};

use quick_xml::events::{BytesStart, Event};

use super::colour::{self, Rgb, Theme};
use super::xml::{self, Reader};
use super::{Ctx, common, ns};
use crate::report::{CheckId, Error, Finding, counted, plural};

const WML: &str = "application/vnd.openxmlformats-officedocument.wordprocessingml.";

/// The parts that hold text: the main document, headers, footers, footnotes, endnotes,
/// comments and the glossary.
pub fn stories<'p>(ctx: &Ctx<'p>) -> Vec<&'p str> {
    let pkg = ctx.pkg;
    let kinds = [
        "header+xml",
        "footer+xml",
        "footnotes+xml",
        "endnotes+xml",
        "comments+xml",
        "document.glossary+xml",
    ];
    let mut parts = vec![pkg.main.as_str()];
    parts.extend(pkg.parts.iter().map(String::as_str).filter(|p| {
        pkg.content_type(p)
            .and_then(|t| t.strip_prefix(WML))
            .is_some_and(|t| kinds.contains(&t))
    }));
    parts
}

/// The kind of tracked change an element is, from its name and the element around it.
fn change(local: &str, parent: Option<(&str, &str)>) -> Option<&'static str> {
    let in_row = parent == Some((ns::W, "trPr"));
    Some(match local {
        "ins" | "del" if in_row => "table_change",
        "ins" => "insertion",
        "del" => "deletion",
        "moveFrom" | "moveTo" => "move",
        "cellIns" | "cellDel" | "cellMerge" => "table_change",
        "rPrChange" | "pPrChange" | "sectPrChange" | "tblPrChange" | "tblPrExChange"
        | "tblGridChange" | "trPrChange" | "tcPrChange" | "numberingChange" => "format_change",
        _ => return None,
    })
}

/// The kinds in report order, with the noun each detail counts.
const CHANGES: [(&str, &str, &str); 5] = [
    ("insertion", "insertion", "insertions"),
    ("deletion", "deletion", "deletions"),
    ("move", "move", "moves"),
    ("format_change", "formatting change", "formatting changes"),
    ("table_change", "table change", "table changes"),
];

/// `tracked-changes` in a Word document: insertions, deletions (with their text), moves and
/// formatting and table changes, in every part that holds text, styles and numbering.
pub fn tracked_changes(ctx: &Ctx) -> Result<Vec<Finding>, Error> {
    let pkg = ctx.pkg;
    let mut parts = stories(ctx);
    parts.extend(pkg.parts.iter().map(String::as_str).filter(|p| {
        let t = pkg.content_type(p).and_then(|t| t.strip_prefix(WML));
        matches!(t, Some("styles+xml" | "numbering+xml"))
    }));
    let mut found = vec![];
    for part in parts {
        // Per kind: count, authors, the first change's text and its level while it is open.
        let mut seen: Vec<(usize, HashSet<String>, String, Option<usize>)> =
            vec![(0, HashSet::new(), String::new(), None); CHANGES.len()];
        pkg.walk(part, |r, ev| {
            match ev {
                Event::Start(e) | Event::Empty(e) if r.ns(e) == ns::W => {
                    let local = e.local_name();
                    if let Some(kind) = change(local.as_ref(), r.parent()) {
                        let i = CHANGES.iter().position(|c| c.0 == kind).unwrap_or_default();
                        let s = &mut seen[i];
                        s.0 += 1;
                        if let Some(a) = r.attr_ns(e, ns::W, "author")? {
                            s.1.insert(a.into_owned());
                        }
                        if s.2.is_empty() && s.3.is_none() && matches!(ev, Event::Start(_)) {
                            s.3 = Some(r.level());
                        }
                    }
                }
                Event::End(_) => {
                    for s in &mut seen {
                        if s.3 == Some(r.level()) {
                            s.3 = None;
                        }
                    }
                }
                ev if matches!(r.parent(), Some((ns::W, "t" | "delText"))) => {
                    let text = xml::text(ev).unwrap_or_default();
                    for s in seen.iter_mut().filter(|s| s.3.is_some()) {
                        s.2 += &text;
                    }
                }
                _ => {}
            }
            Ok(())
        })?;
        for ((kind, one, many), (n, authors, text, _)) in CHANGES.iter().zip(seen) {
            if n == 0 {
                continue;
            }
            let mut detail = counted(n as u64, one, many);
            if !authors.is_empty() {
                detail += &format!(" by {}", plural(authors.len() as u64, "author"));
            }
            if *kind == "deletion" && !text.is_empty() {
                detail += ", with the deleted text";
            }
            let text = (!text.is_empty()).then_some(text.as_str());
            let f = Finding::new(
                CheckId::TrackedChanges,
                kind,
                ctx.place(part),
                n as u64,
                "changes",
                detail,
            );
            found.push(f.with_value(ctx.values, text));
        }
    }
    Ok(found)
}

/// Run properties that decide whether text shows; `None` where a level doesn't set them.
#[derive(Debug, Clone, Default)]
struct Props {
    hidden: Option<bool>,
    half_points: Option<u32>,
    /// `Some(None)` is automatic.
    colour: Option<Option<Rgb>>,
    /// `Some(None)` is `none` or `auto`: nothing behind the text at this level.
    highlight: Option<Option<Rgb>>,
    shading: Option<Option<Rgb>>,
}

impl Props {
    /// These properties, with `o`'s where these don't set one.
    fn or(&self, o: &Props) -> Props {
        Props {
            hidden: self.hidden.or(o.hidden),
            half_points: self.half_points.or(o.half_points),
            colour: self.colour.or(o.colour),
            highlight: self.highlight.or(o.highlight),
            shading: self.shading.or(o.shading),
        }
    }

    /// Reads one child element of a `w:rPr`.
    fn set(&mut self, r: &Reader, e: &BytesStart, theme: &Theme) -> Result<(), Error> {
        let val = r.attr_ns(e, ns::W, "val")?;
        match e.local_name().as_ref() {
            "vanish" | "specVanish" => {
                let off = matches!(val.as_deref(), Some("0" | "false" | "off"));
                self.hidden = Some(!off);
            }
            "sz" => self.half_points = val.and_then(|v| v.parse().ok()),
            "color" => {
                let byte = |a| -> Result<Option<f64>, Error> {
                    let v = r.attr_ns(e, ns::W, a)?;
                    let b = v.and_then(|v| u8::from_str_radix(&v, 16).ok());
                    Ok(b.map(|b| f64::from(b) / 255.0))
                };
                let colour = match r.attr_ns(e, ns::W, "themeColor")? {
                    Some(name) => {
                        match (theme.named(&name), byte("themeTint")?, byte("themeShade")?) {
                            (Some(c), Some(t), _) => Some(colour::lighten(c, t, 1.0 - t)),
                            (Some(c), _, Some(s)) => Some(colour::lighten(c, s, 0.0)),
                            (c, ..) => c,
                        }
                    }
                    None => val.as_deref().and_then(colour::hex),
                };
                self.colour = Some(colour);
            }
            "highlight" => self.highlight = Some(val.as_deref().and_then(colour::highlight)),
            "shd" => self.shading = Some(shading(r, e)?),
            _ => {}
        }
        Ok(())
    }
}

/// The colour a `w:shd` paints: its fill, or its pattern colour when the pattern is solid.
fn shading(r: &Reader, e: &BytesStart) -> Result<Option<Rgb>, Error> {
    let attr = |a| r.attr_ns(e, ns::W, a);
    let colour = match attr("val")?.as_deref() {
        Some("solid") => attr("color")?,
        Some("nil") => None,
        _ => attr("fill")?,
    };
    Ok(colour.as_deref().and_then(colour::hex))
}

/// Character and paragraph styles with their `basedOn` and what they set, the document
/// defaults and the default paragraph style.
#[derive(Default)]
struct Styles {
    styles: HashMap<String, (Option<String>, Props)>,
    defaults: Props,
    paragraph: Option<String>,
}

impl Styles {
    fn read(ctx: &Ctx, theme: &Theme) -> Result<Self, Error> {
        let mut s = Self::default();
        let pkg = ctx.pkg;
        let Some(part) = common::targets(pkg, "styles").first().copied() else {
            return Ok(s);
        };
        let mut current: Option<String> = None;
        pkg.walk(part, |r, ev| {
            let (Event::Start(e) | Event::Empty(e)) = ev else {
                return Ok(());
            };
            if r.ns(e) != ns::W {
                return Ok(());
            }
            let val = || {
                r.attr_ns(e, ns::W, "val")
                    .map(|v| v.map(|v| v.into_owned()))
            };
            match e.local_name().as_ref() {
                "style" => {
                    let id = r
                        .attr_ns(e, ns::W, "styleId")?
                        .unwrap_or_default()
                        .into_owned();
                    let paragraph = r.attr_ns(e, ns::W, "type")?.as_deref() == Some("paragraph");
                    let default = r.attr_ns(e, ns::W, "default")?;
                    if paragraph && matches!(default.as_deref(), Some("1" | "true")) {
                        s.paragraph = Some(id.clone());
                    }
                    s.styles.insert(id.clone(), (None, Props::default()));
                    current = Some(id);
                }
                "basedOn" if r.parent() == Some((ns::W, "style")) => {
                    if let Some(st) = current.as_ref().and_then(|c| s.styles.get_mut(c)) {
                        st.0 = val()?;
                    }
                }
                _ if r.parent() == Some((ns::W, "rPr")) && !r.within(ns::W, "tblStylePr") => {
                    if r.within(ns::W, "rPrDefault") {
                        s.defaults.set(r, e, theme)?;
                    } else if r.within(ns::W, "style")
                        && let Some(st) = current.as_ref().and_then(|c| s.styles.get_mut(c))
                    {
                        st.1.set(r, e, theme)?;
                    }
                }
                _ => {}
            }
            Ok(())
        })?;
        Ok(s)
    }

    /// What a style sets through its `basedOn` chain.
    fn chain(&self, id: Option<&str>) -> Props {
        let mut props = Props::default();
        let mut id = id.map(String::from);
        for _ in 0..32 {
            let Some((based, own)) = id.as_ref().and_then(|i| self.styles.get(i)) else {
                break;
            };
            props = props.or(own);
            id = based.clone();
        }
        props
    }
}

/// A run being read: its own properties, its character style and its text.
#[derive(Default)]
struct Run {
    props: Props,
    style: Option<String>,
    text: String,
}

/// A paragraph being read: its style and shading.
#[derive(Default)]
struct Para {
    style: Option<String>,
    shading: Option<Rgb>,
}

/// The hidden-text and fake-redactions kinds of Word text, with their details.
const HIDDEN_RUNS: [(CheckId, &str, &str); 4] = [
    (
        CheckId::HiddenText,
        "hidden_formatting",
        "with hidden formatting",
    ),
    (CheckId::HiddenText, "tiny", "smaller than 1 pt"),
    (
        CheckId::HiddenText,
        "same_colour",
        "coloured like the background",
    ),
    (
        CheckId::FakeRedactions,
        "same_colour",
        "coloured like the dark background",
    ),
];

/// `hidden-text` and `fake-redactions` in a Word document: runs hidden by formatting (also
/// through styles), smaller than 1 pt, or coloured like what is behind them.
pub fn hidden_text(ctx: &Ctx) -> Result<Vec<Finding>, Error> {
    let pkg = ctx.pkg;
    let theme = Theme::read(pkg)?;
    let styles = Styles::read(ctx, &theme)?;
    let page = page_colour(ctx)?;
    let mut found = vec![];
    for part in stories(ctx) {
        let ct = pkg.content_type(part).unwrap_or_default();
        if ct.ends_with("comments+xml") || ct.ends_with("glossary+xml") {
            continue;
        }
        // Per kind: characters, the first group's text and whether that group has ended.
        let mut seen = vec![(0usize, String::new(), false); HIDDEN_RUNS.len()];
        let (mut paras, mut runs, mut behind) = (vec![Para::default()], vec![], vec![]);
        pkg.walk(part, |r, ev| {
            match ev {
                Event::Start(e) | Event::Empty(e) if r.ns(e) == ns::W => {
                    let start = matches!(ev, Event::Start(_));
                    let val = || {
                        r.attr_ns(e, ns::W, "val")
                            .map(|v| v.map(|v| v.into_owned()))
                    };
                    match (e.local_name().as_ref(), r.parent()) {
                        ("tbl" | "tc", _) if start => behind.push(None),
                        ("p", _) if start => paras.push(Para::default()),
                        ("r", _) if start => runs.push(Run::default()),
                        ("shd", Some((ns::W, "tcPr" | "tblPr"))) => {
                            if let Some(b) = behind.last_mut() {
                                *b = shading(r, e)?;
                            }
                        }
                        ("shd", Some((ns::W, "pPr"))) => {
                            if let Some(p) = paras.last_mut() {
                                p.shading = shading(r, e)?;
                            }
                        }
                        ("pStyle", Some((ns::W, "pPr"))) => {
                            if let Some(p) = paras.last_mut() {
                                p.style = val()?;
                            }
                        }
                        (local, Some((ns::W, "rPr"))) if r.within(ns::W, "r") => {
                            if let Some(run) = runs.last_mut() {
                                if local == "rStyle" {
                                    run.style = val()?;
                                } else {
                                    run.props.set(r, e, &theme)?;
                                }
                            }
                        }
                        _ => {}
                    }
                }
                Event::End(_) if r.closes(ns::W, "tbl") || r.closes(ns::W, "tc") => {
                    behind.pop();
                }
                Event::End(_) if r.closes(ns::W, "p") => {
                    paras.pop();
                    for s in &mut seen {
                        s.2 |= s.0 > 0;
                    }
                }
                Event::End(_) if r.closes(ns::W, "r") => {
                    let (Some(run), Some(para)) = (runs.pop(), paras.last()) else {
                        return Ok(());
                    };
                    let n = run.text.chars().filter(|c| !c.is_whitespace()).count();
                    let paragraph = para.style.as_deref().or(styles.paragraph.as_deref());
                    let props = run
                        .props
                        .or(&styles.chain(run.style.as_deref()))
                        .or(&styles.chain(paragraph))
                        .or(&styles.defaults);
                    let kind = if n == 0 {
                        None
                    } else if props.hidden == Some(true) {
                        Some(0)
                    } else if props.half_points.is_some_and(|h| h < 2) {
                        Some(1)
                    } else if let Some(Some(fg)) = props.colour {
                        let bg = props
                            .highlight
                            .flatten()
                            .or(props.shading.flatten())
                            .or(para.shading)
                            .or(behind.iter().rev().flatten().next().copied())
                            .or(page)
                            .unwrap_or(colour::WHITE);
                        colour::same(fg, bg).then_some(if colour::dark(bg) { 3 } else { 2 })
                    } else {
                        None
                    };
                    for (i, s) in seen.iter_mut().enumerate() {
                        if Some(i) == kind {
                            s.0 += n;
                            if !s.2 {
                                s.1 += &run.text;
                            }
                        } else {
                            s.2 |= s.0 > 0;
                        }
                    }
                }
                ev if r.parent() == Some((ns::W, "t")) => {
                    if let (Some(run), Some(t)) = (runs.last_mut(), xml::text(ev)) {
                        run.text += &t;
                    }
                }
                _ => {}
            }
            Ok(())
        })?;
        for ((check, kind, what), (n, text, _)) in HIDDEN_RUNS.iter().zip(seen) {
            if n > 0 {
                let detail = format!("{} {what}", plural(n as u64, "character"));
                let f = Finding::new(
                    *check,
                    kind,
                    ctx.place(part),
                    n as u64,
                    "characters",
                    detail,
                );
                found.push(f.with_value(ctx.values, Some(&text)));
            }
        }
    }
    Ok(found)
}

/// The page colour Word shows behind the text: `w:background`, when the settings turn it on.
fn page_colour(ctx: &Ctx) -> Result<Option<Rgb>, Error> {
    let pkg = ctx.pkg;
    let mut shown = false;
    for part in common::targets(pkg, "settings") {
        pkg.walk(part, |r, ev| {
            if let Event::Start(e) | Event::Empty(e) = ev {
                shown |= r.is(e, ns::W, "displayBackgroundShape");
            }
            Ok(())
        })?;
    }
    let mut page = None;
    if shown {
        pkg.walk(&pkg.main, |r, ev| {
            if let Event::Start(e) | Event::Empty(e) = ev
                && r.is(e, ns::W, "background")
            {
                page = r
                    .attr_ns(e, ns::W, "color")?
                    .as_deref()
                    .and_then(colour::hex);
            }
            Ok(())
        })?;
    }
    Ok(page)
}

/// A Word part's field instructions (`w:fldSimple` and complex fields, nested ones apart),
/// and its `w:altChunk` and `w:dataBinding` counts.
#[derive(Default)]
struct Story {
    fields: Vec<String>,
    chunks: usize,
    bindings: usize,
}

fn story(ctx: &Ctx, part: &str) -> Result<Story, Error> {
    let mut s = Story::default();
    let mut open: Vec<String> = vec![];
    ctx.pkg.walk(part, |r, ev| {
        match ev {
            Event::Start(e) | Event::Empty(e) if r.ns(e) == ns::W => {
                match e.local_name().as_ref() {
                    "altChunk" => s.chunks += 1,
                    "dataBinding" => s.bindings += 1,
                    "fldSimple" => s
                        .fields
                        .extend(r.attr_ns(e, ns::W, "instr")?.map(|i| i.into_owned())),
                    "fldChar" => match r.attr_ns(e, ns::W, "fldCharType")?.as_deref() {
                        Some("begin") => open.push(String::new()),
                        Some("end") => s.fields.extend(open.pop()),
                        _ => {}
                    },
                    _ => {}
                }
            }
            ev if r.parent() == Some((ns::W, "instrText")) => {
                if let (Some(f), Some(t)) = (open.last_mut(), xml::text(ev)) {
                    *f += &t;
                }
            }
            _ => {}
        }
        Ok(())
    })?;
    Ok(s)
}

/// A field instruction's words after its name; quoted ones whole, without the quotes.
fn arguments(instr: &str) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    let (mut word, mut quoted) = (String::new(), false);
    for c in instr.chars().chain([' ']) {
        match c {
            '"' => quoted = !quoted,
            c if c.is_whitespace() && !quoted => {
                out.extend((!word.is_empty()).then(|| std::mem::take(&mut word)))
            }
            c => word.push(c),
        }
    }
    out.into_iter().skip(1).collect()
}

/// A field instruction's name: `INCLUDEPICTURE`, `DDEAUTO`, …
fn field_name(instr: &str) -> String {
    instr
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase()
}

/// What Word parts hold besides text: imported chunks (`embedded-files`), data bindings and
/// document variables (`custom-data`), DDE fields (`macros`) and fields that read files
/// (`external-links`).
pub fn story_items(ctx: &Ctx) -> Result<Vec<Finding>, Error> {
    let mut found = vec![];
    for part in stories(ctx) {
        let s = story(ctx, part)?;
        let dde: Vec<_> = s
            .fields
            .iter()
            .filter(|f| matches!(field_name(f).as_str(), "DDE" | "DDEAUTO"))
            .collect();
        let paths: Vec<_> = s
            .fields
            .iter()
            .filter(|f| {
                matches!(
                    field_name(f).as_str(),
                    "INCLUDETEXT" | "INCLUDEPICTURE" | "LINK"
                )
            })
            .filter(|f| arguments(f).iter().any(|a| common::is_path(a, false)))
            .collect();
        for (check, kind, n, unit, detail, value) in [
            (
                CheckId::EmbeddedFiles,
                "imported_chunk",
                s.chunks,
                "files",
                plural(s.chunks as u64, "imported document"),
                None,
            ),
            (
                CheckId::CustomData,
                "data_binding",
                s.bindings,
                "fields",
                plural(s.bindings as u64, "data-bound content control"),
                None,
            ),
            (
                CheckId::Macros,
                "dde_field",
                dde.len(),
                "links",
                plural(dde.len() as u64, "DDE field"),
                dde.first().map(|f| f.trim()),
            ),
            (
                CheckId::ExternalLinks,
                "field_path",
                paths.len(),
                "links",
                counted(
                    paths.len() as u64,
                    "field that reads a file",
                    "fields that read files",
                ),
                paths.first().map(|f| f.trim()),
            ),
        ] {
            if n > 0 {
                let f = Finding::new(check, kind, ctx.place(part), n as u64, unit, detail);
                found.push(f.with_value(ctx.values, value));
            }
        }
    }
    for part in common::targets(ctx.pkg, "settings") {
        let mut vars = vec![];
        ctx.pkg.walk(part, |r, ev| {
            if let Event::Start(e) | Event::Empty(e) = ev
                && r.is(e, ns::W, "docVar")
            {
                let get = |a| {
                    r.attr_ns(e, ns::W, a)
                        .map(|v| v.unwrap_or_default().into_owned())
                };
                vars.push(format!("{}={}", get("name")?, get("val")?));
            }
            Ok(())
        })?;
        if !vars.is_empty() {
            let detail = plural(vars.len() as u64, "document variable");
            let f = Finding::new(
                CheckId::CustomData,
                "document_variables",
                ctx.place(part),
                vars.len() as u64,
                "fields",
                detail,
            );
            found.push(f.with_value(ctx.values, Some(&vars.join("; "))));
        }
    }
    Ok(found)
}
