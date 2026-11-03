//! Checks that only Excel workbooks have.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

use quick_xml::events::{BytesStart, Event};

use super::colour::{self, Rgb, Theme};
use super::common::{CELL_SHEETS, targets};
use super::xml::{self, Reader};
use super::{Ctx, ns};
use crate::report::{CheckId, Error, Finding, Where, counted, plural};

/// `tracked-changes` in a workbook: the revision logs and user list of a shared workbook.
pub fn shared_workbook_log(ctx: &Ctx) -> Result<Vec<Finding>, Error> {
    let pkg = ctx.pkg;
    let mut found = vec![];
    for (short, detail) in [
        ("revisionHeaders", "1 shared-workbook revision list"),
        ("revisionLog", "1 shared-workbook revision log"),
        ("usernames", "1 shared-workbook user list"),
    ] {
        for part in targets(pkg, short) {
            let mut user = None;
            if short == "usernames" {
                pkg.walk(part, |r, ev| {
                    if let Event::Start(e) | Event::Empty(e) = ev
                        && user.is_none()
                        && r.is(e, ns::S, "userInfo")
                    {
                        user = r.attr(e, "name")?.map(|n| n.into_owned());
                    }
                    Ok(())
                })?;
            }
            let f = Finding::new(
                CheckId::TrackedChanges,
                "shared_workbook_log",
                ctx.place(part),
                1,
                "parts",
                detail.into(),
            );
            found.push(f.with_value(ctx.values, user.as_deref()));
        }
    }
    Ok(found)
}

/// A colour as a workbook's styles write it.
#[derive(Debug, Clone, Default)]
struct Spec {
    rgb: Option<Rgb>,
    theme: Option<usize>,
    indexed: Option<usize>,
    tint: f64,
    auto: bool,
}

impl Spec {
    fn read(r: &Reader, e: &BytesStart) -> Result<Self, Error> {
        let num =
            |a| -> Result<Option<usize>, Error> { Ok(r.attr(e, a)?.and_then(|v| v.parse().ok())) };
        Ok(Self {
            rgb: r.attr(e, "rgb")?.as_deref().and_then(colour::hex),
            theme: num("theme")?,
            indexed: num("indexed")?,
            tint: r
                .attr(e, "tint")?
                .and_then(|v| v.parse().ok())
                .unwrap_or(0.0),
            auto: matches!(r.attr(e, "auto")?.as_deref(), Some("1" | "true")),
        })
    }

    /// The colour, or None when it is automatic or can't be told.
    fn resolve(&self, theme: &Theme, custom: &[Rgb]) -> Option<Rgb> {
        if self.auto {
            return None;
        }
        let base = match (self.rgb, self.theme, self.indexed) {
            (Some(c), ..) => c,
            (_, Some(t), _) => theme.indexed(t)?,
            (.., Some(i)) => custom.get(i).copied().or_else(|| colour::palette(i))?,
            _ => return None,
        };
        Some(colour::tint(base, self.tint))
    }
}

/// Whether a number format shows nothing: it has sections and every one is empty (`;;;`).
fn hides(code: &str) -> bool {
    code.contains(';') && code.split(';').all(|s| s.trim().is_empty())
}

/// The cell kinds of hidden-text and fake-redactions, with their details.
const CELL_KINDS: [(CheckId, &str, &str); 3] = [
    (
        CheckId::HiddenText,
        "hidden_format",
        "with a number format that shows nothing",
    ),
    (
        CheckId::HiddenText,
        "same_colour",
        "coloured like their fill",
    ),
    (
        CheckId::FakeRedactions,
        "same_colour",
        "coloured like the dark fill behind them",
    ),
];

/// For each cell style (`cellXfs` index), the [`CELL_KINDS`] index its cells fall under.
fn cell_styles(ctx: &Ctx) -> Result<Vec<Option<usize>>, Error> {
    let pkg = ctx.pkg;
    let Some(part) = targets(pkg, "styles").first().copied() else {
        return Ok(vec![]);
    };
    let theme = Theme::read(pkg)?;
    let (mut formats, mut fonts, mut fills, mut xfs, mut custom) =
        (HashMap::new(), vec![], vec![], vec![], vec![]);
    pkg.walk(part, |r, ev| {
        let (Event::Start(e) | Event::Empty(e)) = ev else {
            return Ok(());
        };
        let parent = r.parent();
        let num = |a| -> Result<usize, Error> {
            Ok(r.attr(e, a)?.and_then(|v| v.parse().ok()).unwrap_or(0))
        };
        match (e.local_name().as_ref(), parent) {
            ("numFmt", Some((ns::S, "numFmts"))) => {
                let code = r.attr(e, "formatCode")?.unwrap_or_default().into_owned();
                formats.insert(num("numFmtId")?, code);
            }
            ("font", Some((ns::S, "fonts"))) => fonts.push(None),
            ("color", Some((ns::S, "font"))) if r.within(ns::S, "fonts") => {
                if let Some(f) = fonts.last_mut() {
                    *f = Some(Spec::read(r, e)?);
                }
            }
            ("fill", Some((ns::S, "fills"))) => fills.push((false, None)),
            ("patternFill", _) if r.within(ns::S, "fills") => {
                if let Some(f) = fills.last_mut() {
                    f.0 = r.attr(e, "patternType")?.as_deref() == Some("solid");
                }
            }
            ("fgColor", Some((ns::S, "patternFill"))) if r.within(ns::S, "fills") => {
                if let Some(f) = fills.last_mut() {
                    f.1 = Some(Spec::read(r, e)?);
                }
            }
            ("xf", Some((ns::S, "cellXfs"))) => {
                xfs.push((num("numFmtId")?, num("fontId")?, num("fillId")?))
            }
            ("rgbColor", Some((ns::S, "indexedColors"))) => {
                custom.push(
                    r.attr(e, "rgb")?
                        .as_deref()
                        .and_then(colour::hex)
                        .unwrap_or(colour::BLACK),
                );
            }
            _ => {}
        }
        Ok(())
    })?;
    let kinds = xfs
        .into_iter()
        .map(|(format, font, fill)| {
            if formats.get(&format).is_some_and(|c| hides(c)) {
                return Some(0);
            }
            // Excel's automatic text colour is black, and so is a solid fill's default.
            let fg = fonts
                .get(font)
                .cloned()
                .flatten()
                .and_then(|s| s.resolve(&theme, &custom))
                .unwrap_or(colour::BLACK);
            let bg = match fills.get(fill) {
                Some((true, spec)) => spec
                    .as_ref()
                    .map_or(Some(colour::BLACK), |s| s.resolve(&theme, &custom))
                    .unwrap_or(colour::BLACK),
                _ => colour::WHITE,
            };
            colour::same(fg, bg).then_some(if colour::dark(bg) { 2 } else { 1 })
        })
        .collect();
    Ok(kinds)
}

/// The workbook's shared strings, when values are shown.
fn shared_strings(ctx: &Ctx) -> Result<Vec<String>, Error> {
    let mut strings: Vec<String> = vec![];
    if !ctx.values {
        return Ok(strings);
    }
    for part in targets(ctx.pkg, "sharedStrings") {
        ctx.pkg.walk(part, |r, ev| {
            match ev {
                Event::Start(e) | Event::Empty(e) if r.level() == 1 && r.is(e, ns::S, "si") => {
                    strings.push(String::new());
                }
                ev if r.parent() == Some((ns::S, "t")) && !r.within(ns::S, "rPh") => {
                    if let (Some(s), Some(t)) = (strings.last_mut(), xml::text(ev)) {
                        *s += &t;
                    }
                }
                _ => {}
            }
            Ok(())
        })?;
    }
    Ok(strings)
}

/// A cell's column and row (1-based) from its reference, e.g. `C5` → (3, 5).
fn cell_ref(r: &str) -> Option<(u32, u32)> {
    let split = r.find(|c: char| c.is_ascii_digit())?;
    let (letters, digits) = r.split_at(split);
    let mut col = 0u32;
    for b in letters.bytes() {
        if !b.is_ascii_uppercase() {
            return None;
        }
        col = col.checked_mul(26)?.checked_add(u32::from(b - b'A') + 1)?;
    }
    Some((col, digits.parse().ok()?)).filter(|&(c, r)| c > 0 && r > 0)
}

/// A column's letters: 1 → `A`, 27 → `AA`.
fn column(mut col: u32) -> String {
    let mut out = vec![];
    while col > 0 {
        col -= 1;
        out.push(b'A' + (col % 26) as u8);
        col /= 26;
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_default()
}

/// Found cells: how many, the range around them and the first one's value.
#[derive(Debug, Default)]
struct Cells {
    n: usize,
    /// The distinct rows or columns they are in.
    lines: HashSet<u32>,
    /// (first column, first row, last column, last row).
    range: Option<(u32, u32, u32, u32)>,
    first: Option<String>,
}

impl Cells {
    fn add(&mut self, (col, row): (u32, u32), line: u32, text: &str) {
        self.n += 1;
        self.lines.insert(line);
        self.first.get_or_insert_with(|| text.to_string());
        let (c0, r0, c1, r1) = self.range.unwrap_or((col, row, col, row));
        self.range = Some((c0.min(col), r0.min(row), c1.max(col), r1.max(row)));
    }

    /// The A1 range: `C2:C40`, or `C2` for one cell.
    fn shown(&self) -> Option<String> {
        let (c0, r0, c1, r1) = self.range?;
        let one = format!("{}{r0}", column(c0));
        Some(if (c0, r0) == (c1, r1) {
            one
        } else {
            format!("{one}:{}{r1}", column(c1))
        })
    }
}

/// The cell kinds of hidden-rows-columns, with the noun each counts.
const LINE_KINDS: [(&str, &str, &str, &str); 2] = [
    ("hidden_rows", "rows", "hidden row", "hidden rows"),
    (
        "hidden_columns",
        "columns",
        "hidden column",
        "hidden columns",
    ),
];

fn yes(v: Option<Cow<str>>) -> bool {
    matches!(v.as_deref(), Some("1" | "true"))
}

/// `hidden-text`, `fake-redactions` and `hidden-rows-columns` in a workbook: cells with a value
/// whose number format shows nothing or whose text is coloured like their fill, and values in
/// hidden rows and columns.
pub fn cells(ctx: &Ctx) -> Result<Vec<Finding>, Error> {
    let pkg = ctx.pkg;
    let styles = cell_styles(ctx)?;
    let strings = shared_strings(ctx)?;
    let mut found = vec![];
    for sheet in &ctx.sheets {
        let Some(part) = sheet.part.as_deref().filter(|p| {
            pkg.content_type(p)
                .is_some_and(|t| CELL_SHEETS.contains(&t))
        }) else {
            continue;
        };
        let mut kinds: Vec<Cells> = (0..CELL_KINDS.len()).map(|_| Cells::default()).collect();
        let mut lines: [Cells; 2] = Default::default();
        let mut hidden_cols: Vec<(u32, u32)> = vec![];
        // The open cell: where it is, its style, whether it holds a shared string, its text.
        let mut cell: Option<((u32, u32), usize, bool, String)> = None;
        let (mut row, mut col, mut row_hidden) = (0u32, 0u32, false);
        pkg.walk(part, |r, ev| {
            match ev {
                Event::Start(e) | Event::Empty(e) if r.is(e, ns::S, "col") => {
                    let num = |a| -> Result<Option<f64>, Error> {
                        Ok(r.attr(e, a)?.and_then(|v| v.parse().ok()))
                    };
                    if yes(r.attr(e, "hidden")?) || num("width")?.is_some_and(|w| w < 0.1) {
                        let (min, max) = (num("min")?.unwrap_or(0.0), num("max")?.unwrap_or(0.0));
                        hidden_cols.push((min as u32, max as u32));
                    }
                }
                Event::Start(e) | Event::Empty(e) if r.is(e, ns::S, "row") => {
                    row = r
                        .attr(e, "r")?
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(row + 1);
                    col = 0;
                    let low = yes(r.attr(e, "customHeight")?)
                        && r.attr(e, "ht")?
                            .and_then(|v| v.parse::<f64>().ok())
                            .is_some_and(|h| h < 0.5);
                    row_hidden = yes(r.attr(e, "hidden")?) || low;
                }
                Event::Start(e) | Event::Empty(e) if r.is(e, ns::S, "c") => {
                    let at = r
                        .attr(e, "r")?
                        .as_deref()
                        .and_then(cell_ref)
                        .unwrap_or((col + 1, row));
                    (col, row) = at;
                    if matches!(ev, Event::Start(_)) {
                        let style = r.attr(e, "s")?.and_then(|v| v.parse().ok()).unwrap_or(0);
                        let shared = r.attr(e, "t")?.as_deref() == Some("s");
                        cell = Some((at, style, shared, String::new()));
                    }
                }
                Event::End(_) if r.closes(ns::S, "c") => {
                    let Some((at, style, shared, text)) = cell.take().filter(|c| !c.3.is_empty())
                    else {
                        return Ok(());
                    };
                    let text = match shared {
                        true => text
                            .trim()
                            .parse::<usize>()
                            .ok()
                            .and_then(|i| strings.get(i))
                            .cloned()
                            .unwrap_or_default(),
                        false => text,
                    };
                    if let Some(&Some(kind)) = styles.get(style) {
                        kinds[kind].add(at, at.1, &text);
                    }
                    if row_hidden {
                        lines[0].add(at, at.1, &text);
                    }
                    if hidden_cols
                        .iter()
                        .any(|&(lo, hi)| (lo..=hi).contains(&at.0))
                    {
                        lines[1].add(at, at.0, &text);
                    }
                }
                ev if matches!(r.parent(), Some((ns::S, "v" | "t"))) => {
                    if let (Some(c), Some(t)) = (&mut cell, xml::text(ev)) {
                        c.3 += &t;
                    }
                }
                _ => {}
            }
            Ok(())
        })?;
        let mut add = |check, kind, cells: &Cells, n: usize, unit, detail: String| {
            let mut place = ctx.place(part);
            place.cells = cells.shown();
            let f = Finding::new(check, kind, place, n as u64, unit, detail);
            found.push(f.with_value(ctx.values, cells.first.as_deref()));
        };
        for ((check, kind, what), cells) in CELL_KINDS.iter().zip(&kinds) {
            if cells.n > 0 {
                let detail = format!("{} {what}", plural(cells.n as u64, "cell"));
                add(*check, kind, cells, cells.n, "cells", detail);
            }
        }
        for ((kind, unit, one, many), cells) in LINE_KINDS.iter().zip(&lines) {
            let n = cells.lines.len();
            if n > 0 {
                let detail = format!(
                    "{} holding {}",
                    counted(n as u64, one, many),
                    plural(cells.n as u64, "value")
                );
                add(CheckId::HiddenRowsColumns, kind, cells, n, unit, detail);
            }
        }
    }
    Ok(found)
}

/// `hidden-sheets`: hidden and very hidden sheets of any type.
pub fn hidden_sheets(ctx: &Ctx) -> Result<Vec<Finding>, Error> {
    let mut found = vec![];
    for (i, sheet) in ctx.sheets.iter().enumerate() {
        let how = match sheet.state.as_str() {
            "hidden" => "hidden",
            "veryHidden" => "very hidden",
            _ => continue,
        };
        let ct = sheet
            .part
            .as_deref()
            .and_then(|p| ctx.pkg.content_type(p))
            .unwrap_or_default();
        let what = match ct.rsplit('.').next().unwrap_or_default() {
            "chartsheet+xml" => "chart sheet",
            "macrosheet+xml" => "macro sheet",
            "dialogsheet+xml" => "dialog sheet",
            _ => "sheet",
        };
        let mut place = sheet
            .part
            .as_deref()
            .map_or_else(Where::default, |p| ctx.place(p));
        place.sheet_index = Some(i + 1);
        let detail = format!("1 {how} {what}");
        let f = Finding::new(
            CheckId::HiddenSheets,
            "hidden_sheet",
            place,
            1,
            "sheets",
            detail,
        );
        found.push(f.with_value(ctx.values, Some(&sheet.name)));
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cell_references_and_columns_convert() {
        assert_eq!(cell_ref("C5"), Some((3, 5)));
        assert_eq!(cell_ref("AA10"), Some((27, 10)));
        assert_eq!(cell_ref("XFD1048576"), Some((16384, 1_048_576)));
        assert_eq!(cell_ref("5"), None);
        assert_eq!(cell_ref("c5"), None);
        assert_eq!(cell_ref("A0"), None);
        assert_eq!(
            (column(1), column(26), column(27), column(16384)),
            ("A".into(), "Z".into(), "AA".into(), "XFD".into())
        );
    }

    #[test]
    fn only_formats_with_every_section_empty_hide() {
        assert!(hides(";;;") && hides(";;") && hides(" ; ; "));
        assert!(!hides("") && !hides("General") && !hides("0;-0;;@"));
    }
}
