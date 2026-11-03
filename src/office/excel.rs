//! Checks that only Excel workbooks have.

use std::borrow::Cow;
use std::collections::{BTreeSet, HashMap, HashSet};

use quick_xml::events::{BytesStart, Event};

use super::clean::Draft;
use super::colour::{self, Rgb, Theme};
use super::common::{CELL_SHEETS, targets};
use super::tree::{Attr, El, Node};
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
        "with text coloured like the cell fill",
    ),
    (
        CheckId::FakeRedactions,
        "same_colour",
        "with text coloured like the dark cell fill",
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
pub fn column(mut col: u32) -> String {
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
        // Scenarios, their saved values and the first one; custom sheet properties.
        let (mut scenarios, mut saved, mut first_saved) = (0usize, 0usize, None);
        let mut custom: Vec<String> = vec![];
        pkg.walk(part, |r, ev| {
            match ev {
                Event::Start(e) | Event::Empty(e) if r.is(e, ns::S, "scenario") => scenarios += 1,
                Event::Start(e) | Event::Empty(e) if r.is(e, ns::S, "inputCells") => {
                    saved += 1;
                    if first_saved.is_none() {
                        first_saved = r.attr(e, "val")?.map(Cow::into_owned);
                    }
                }
                Event::Start(e) | Event::Empty(e) if r.is(e, ns::S, "customPr") => {
                    custom.push(r.attr(e, "name")?.unwrap_or_default().into_owned());
                }
                Event::Start(e) | Event::Empty(e) if r.is(e, ns::S, "col") => {
                    let num = |a| -> Result<Option<f64>, Error> {
                        Ok(r.attr(e, a)?.and_then(|v| v.parse().ok()))
                    };
                    if xml::yes(r.attr(e, "hidden")?) || num("width")?.is_some_and(|w| w < 0.1) {
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
                    let low = xml::yes(r.attr(e, "customHeight")?)
                        && r.attr(e, "ht")?
                            .and_then(|v| v.parse::<f64>().ok())
                            .is_some_and(|h| h < 0.5);
                    row_hidden = xml::yes(r.attr(e, "hidden")?) || low;
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
        let mut data = |kind, n: usize, unit, detail: String, value: Option<&str>| {
            let f = Finding::new(
                CheckId::CustomData,
                kind,
                ctx.place(part),
                n as u64,
                unit,
                detail,
            );
            found.push(f.with_value(ctx.values, value));
        };
        if scenarios > 0 {
            let detail = format!(
                "{} with {}",
                plural(scenarios as u64, "scenario"),
                plural(saved as u64, "saved value")
            );
            data(
                "scenarios",
                saved.max(scenarios),
                "values",
                detail,
                first_saved.as_deref(),
            );
        }
        if !custom.is_empty() {
            let detail = counted(
                custom.len() as u64,
                "custom sheet property",
                "custom sheet properties",
            );
            data(
                "sheet_properties",
                custom.len(),
                "fields",
                detail,
                custom.first().map(String::as_str),
            );
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

/// `pivot-caches`: pivot caches with their source rows, and data model parts.
pub fn pivot_caches(ctx: &Ctx) -> Result<Vec<Finding>, Error> {
    let pkg = ctx.pkg;
    let mut found = vec![];
    for def in targets(pkg, "pivotCacheDefinition") {
        let (mut rows, mut first) = (None, None);
        pkg.walk(def, |r, ev| {
            if let Event::Start(e) | Event::Empty(e) = ev {
                if r.level() == 0 {
                    rows = r
                        .attr(e, "recordCount")?
                        .and_then(|v| v.parse::<u64>().ok());
                } else if first.is_none() && r.is(e, ns::S, "s") && r.within(ns::S, "sharedItems") {
                    first = r.attr(e, "v")?.map(Cow::into_owned);
                }
            }
            Ok(())
        })?;
        let records = pkg
            .links
            .iter()
            .find(|l| l.source == def && l.rel.short() == "pivotCacheRecords")
            .and_then(|l| l.part.as_deref());
        if let Some(part) = records {
            let mut n = 0;
            pkg.walk(part, |r, ev| {
                if let Event::Start(e) | Event::Empty(e) = ev
                    && r.level() == 1
                    && r.is(e, ns::S, "r")
                {
                    n += 1;
                }
                Ok(())
            })?;
            rows = Some(n);
        }
        let detail = format!(
            "1 pivot cache with {}",
            plural(rows.unwrap_or(0), "source row")
        );
        let f = Finding::new(
            CheckId::PivotCaches,
            "pivot_cache",
            ctx.place(records.unwrap_or(def)),
            1,
            "parts",
            detail,
        );
        found.push(f.with_value(ctx.values, first.as_deref()));
    }
    for part in pkg
        .parts
        .iter()
        .filter(|p| p.to_ascii_lowercase().starts_with("xl/model/"))
    {
        found.push(Finding::new(
            CheckId::PivotCaches,
            "data_model",
            ctx.place(part),
            1,
            "parts",
            "1 data model".into(),
        ));
    }
    Ok(found)
}

/// The cell sheets the copy still reaches (worksheets and macro sheets).
pub fn cell_sheets(d: &Draft) -> Result<Vec<String>, Error> {
    let reached = d.reachable()?;
    let mut names = d.names();
    names.retain(|n| {
        reached.contains(&super::key(n))
            && d.content_type(n)
                .is_some_and(|t| CELL_SHEETS.contains(&t.as_str()))
    });
    Ok(names)
}

/// The shared-string index a cell holds, when it holds one.
fn shared_index(c: &El) -> Option<usize> {
    (c.attr("", "t") == Some("s"))
        .then(|| c.child(ns::S, "v"))
        .flatten()
        .and_then(|v| v.text().trim().parse().ok())
}

/// Adds the shared strings a sheet's cells use to `used`.
pub fn used_in(sheet: &El, used: &mut BTreeSet<usize>) {
    sheet.each(&mut |e| {
        if e.is(ns::S, "c")
            && let Some(i) = shared_index(e)
        {
            used.insert(i);
        }
    });
}

/// The shared strings no cell of the copy uses.
pub fn unused_strings(d: &Draft) -> Result<BTreeSet<usize>, Error> {
    let mut used = BTreeSet::new();
    for s in cell_sheets(d)? {
        if let Some(doc) = d.doc(&s)? {
            used_in(&doc.root, &mut used);
        }
    }
    let mut n = 0;
    for sst in d
        .linked("sharedStrings")?
        .into_iter()
        .filter_map(|l| l.part)
    {
        if let Some(doc) = d.doc(&sst)? {
            n = n.max(doc.root.els().filter(|e| e.is(ns::S, "si")).count());
        }
    }
    Ok((0..n).filter(|i| !used.contains(i)).collect())
}

/// Empties every shared string that isn't `used` or `kept`, so cells keep their numbers and
/// no text is left behind.
pub fn blank_strings(
    d: &mut Draft,
    used: &BTreeSet<usize>,
    kept: &BTreeSet<usize>,
) -> Result<(), Error> {
    for sst in d
        .linked("sharedStrings")?
        .into_iter()
        .filter_map(|l| l.part)
    {
        d.edit(&sst, |root| {
            let mut changed = false;
            for (i, si) in root.els_mut().filter(|e| e.is(ns::S, "si")).enumerate() {
                if !used.contains(&i) && !kept.contains(&i) && !si.text().is_empty() {
                    si.kids = vec![Node::El(El::like(si, "t"))];
                    changed = true;
                }
            }
            Ok(changed)
        })?;
    }
    Ok(())
}

/// Whether a pivot cache reads its source through a data connection.
pub fn caches_use_connections(ctx: &Ctx) -> Result<bool, Error> {
    let mut used = false;
    for def in targets(ctx.pkg, "pivotCacheDefinition") {
        ctx.pkg.walk(def, |r, ev| {
            if let Event::Start(e) | Event::Empty(e) = ev
                && r.is(e, ns::S, "cacheSource")
            {
                used |= r.attr(e, "connectionId")?.is_some();
            }
            Ok(())
        })?;
    }
    Ok(used)
}

/// Which cells clean clears: those in hidden rows and columns (`lines`), and those whose
/// style falls under a [`CELL_KINDS`] entry that is picked.
pub struct Clear {
    lines: bool,
    kinds: [bool; 3],
    styles: Vec<Option<usize>>,
}

impl Clear {
    /// `hidden` picks the hidden-text kinds, `dark` the fake-redactions one.
    pub fn new(ctx: &Ctx, lines: bool, hidden: bool, dark: bool) -> Result<Self, Error> {
        Ok(Self {
            lines,
            kinds: [hidden, hidden, dark],
            styles: cell_styles(ctx)?,
        })
    }

    fn style_clears(&self, style: usize) -> bool {
        matches!(self.styles.get(style), Some(&Some(k)) if self.kinds[k])
    }
}

/// A1 range bounds: (first column, first row, last column, last row).
fn range(r: &str) -> Option<(u32, u32, u32, u32)> {
    let (a, b) = r.split_once(':').unwrap_or((r, r));
    let ((c0, r0), (c1, r1)) = (cell_ref(a)?, cell_ref(b)?);
    Some((c0.min(c1), r0.min(r1), c0.max(c1), r0.max(r1)))
}

/// Clears the cells `clear` picks in a sheet: their value, formula, inline text and type go,
/// their reference and style stay; in a merged range the top-left cell decides. Gives the
/// references of the cleared cells that held something.
pub fn clear_cells(root: &mut El, clear: &Clear) -> Vec<String> {
    let mut hidden_cols: Vec<(u32, u32)> = vec![];
    if clear.lines {
        for col in root.child(ns::S, "cols").into_iter().flat_map(El::els) {
            let num = |a| col.attr("", a).and_then(|v| v.parse::<f64>().ok());
            if xml::yes(col.attr("", "hidden").map(Cow::from))
                || num("width").is_some_and(|w| w < 0.1)
            {
                let (lo, hi) = (num("min").unwrap_or(0.0), num("max").unwrap_or(0.0));
                hidden_cols.push((lo as u32, hi as u32));
            }
        }
    }
    let merges: Vec<(u32, u32, u32, u32)> = root
        .child(ns::S, "mergeCells")
        .into_iter()
        .flat_map(El::els)
        .filter_map(|m| m.attr("", "ref").and_then(range))
        .collect();
    // Each cell's decision, then the top-left cell's for the rest of its merged range.
    let mut decided: HashMap<(u32, u32), bool> = HashMap::new();
    let Some(data) = root.child_mut(ns::S, "sheetData") else {
        return vec![];
    };
    let mut row_n = 0;
    for row in data.els_mut().filter(|r| r.is(ns::S, "row")) {
        row_n = row
            .attr("", "r")
            .and_then(|v| v.parse().ok())
            .unwrap_or(row_n + 1);
        let low = xml::yes(row.attr("", "customHeight").map(Cow::from))
            && row
                .attr("", "ht")
                .and_then(|h| h.parse::<f64>().ok())
                .is_some_and(|h| h < 0.5);
        let row_hidden = clear.lines && (xml::yes(row.attr("", "hidden").map(Cow::from)) || low);
        let mut col_n = 0;
        for c in row.els_mut().filter(|c| c.is(ns::S, "c")) {
            let at = c
                .attr("", "r")
                .and_then(cell_ref)
                .unwrap_or((col_n + 1, row_n));
            col_n = at.0;
            let style = c.attr("", "s").and_then(|v| v.parse().ok()).unwrap_or(0);
            let hidden = row_hidden
                || hidden_cols
                    .iter()
                    .any(|&(lo, hi)| (lo..=hi).contains(&at.0));
            decided.insert(at, hidden || clear.style_clears(style));
        }
    }
    let decides = |(col, row): (u32, u32)| {
        let top = merges
            .iter()
            .find(|&&(c0, r0, c1, r1)| (c0..=c1).contains(&col) && (r0..=r1).contains(&row));
        let at = top.map_or((col, row), |m| (m.0, m.1));
        decided.get(&at).copied().unwrap_or(false)
    };
    let mut cleared = vec![];
    let mut row_n = 0;
    for row in data.els_mut().filter(|r| r.is(ns::S, "row")) {
        row_n = row
            .attr("", "r")
            .and_then(|v| v.parse().ok())
            .unwrap_or(row_n + 1);
        let mut col_n = 0;
        for c in row.els_mut().filter(|c| c.is(ns::S, "c")) {
            let at = c
                .attr("", "r")
                .and_then(cell_ref)
                .unwrap_or((col_n + 1, row_n));
            col_n = at.0;
            if decides(at) && c.els().next().is_some() {
                c.kids.clear();
                c.drop_attrs(|a| a.name == "t" || a.name == "cm" || a.name == "vm");
                cleared.push(format!("{}{}", column(at.0), at.1));
            }
        }
    }
    cleared
}

/// The values rule in one sheet: every formula goes and its saved result stays (a string
/// result becomes inline text); a formula with no saved result leaves the cell empty. Gives
/// (formulas replaced, formulas without a value).
pub fn to_values(root: &mut El) -> (u64, u64) {
    let (mut replaced, mut empty) = (0, 0);
    let Some(data) = root.child_mut(ns::S, "sheetData") else {
        return (0, 0);
    };
    for row in data.els_mut() {
        for c in row.els_mut().filter(|c| c.is(ns::S, "c")) {
            if c.child(ns::S, "f").is_none() {
                continue;
            }
            c.kids
                .retain(|k| !matches!(k, Node::El(e) if e.is(ns::S, "f")));
            let Some(v) = c.child(ns::S, "v").map(El::text) else {
                empty += 1;
                c.kids.clear();
                c.drop_attrs(|a| a.name == "t");
                continue;
            };
            replaced += 1;
            if c.attr("", "t") == Some("str") {
                c.set("", "t", "inlineStr");
                let mut t = El::like(c, "t");
                if v.trim() != v {
                    t.attrs.push(Attr {
                        name: "xml:space".into(),
                        ns: "http://www.w3.org/XML/1998/namespace".into(),
                        value: "preserve".into(),
                    });
                }
                t.kids.push(Node::Text(v));
                let mut is = El::like(c, "is");
                is.kids.push(Node::El(t));
                c.kids = vec![Node::El(is)];
            }
        }
    }
    (replaced, empty)
}

const CHART: &str = "http://schemas.openxmlformats.org/drawingml/2006/chart";

/// Whether a formula names one of the sheets `names` (`Name!` or `'Name'!`, any case).
pub fn names_sheet(formula: &str, names: &[String]) -> bool {
    let f = formula.to_lowercase();
    names.iter().any(|n| {
        let n = n.to_lowercase();
        f.contains(&format!("{n}!")) || f.contains(&format!("'{}'!", n.replace('\'', "''")))
    })
}

/// A chart made literal: each cell reference becomes the values cached beside it, series
/// names their cached text, and pivot charts lose their pivot source. Gives whether it
/// changed, or the formula of a multi-level reference to a deleted sheet, which can't be
/// made literal.
pub fn chart_to_values(root: &mut El, deleted: &[String]) -> Result<bool, String> {
    let mut stuck = None;
    root.each(&mut |e| {
        if e.is(CHART, "multiLvlStrRef")
            && let Some(f) = e.child(CHART, "f").map(El::text)
            && names_sheet(&f, deleted)
        {
            stuck.get_or_insert(f);
        }
    });
    if let Some(f) = stuck {
        return Err(f);
    }
    let mut n = root.prune(&|e: &El| {
        e.is(CHART, "pivotSource") || e.is(CHART, "pivotFmts") || e.is(CHART, "fmtId")
    });
    root.each_mut(&mut |e| {
        // A series name from a cell becomes its cached text.
        if e.is(CHART, "tx")
            && let Some(r) = e.child(CHART, "strRef")
        {
            let text = r
                .child(CHART, "strCache")
                .and_then(|c| c.child(CHART, "pt"))
                .and_then(|p| p.child(CHART, "v"))
                .map(El::text)
                .unwrap_or_default();
            let mut v = El::like(r, "v");
            v.kids.push(Node::Text(text));
            e.kids = vec![Node::El(v)];
            n += 1;
        }
        for (reference, cache, literal) in [
            ("numRef", "numCache", "numLit"),
            ("strRef", "strCache", "strLit"),
        ] {
            for k in e.els_mut().filter(|k| k.is(CHART, reference)) {
                let data = k
                    .child(CHART, cache)
                    .map(|c| c.kids.clone())
                    .unwrap_or_default();
                *k = El {
                    kids: data,
                    ..El::like(k, literal)
                };
                n += 1;
            }
        }
    });
    Ok(n > 0)
}

/// The sheets `names` deleted from the workbook: their `<sheet>` entries (the sweep takes the
/// parts), defined names on or naming them (higher `localSheetId`s move down), the active and
/// first tab kept on a remaining visible sheet, and the conditional formats and validations
/// of other sheets that name them.
pub fn delete_sheets(d: &mut Draft, names: &[String]) -> Result<(), Error> {
    let book = d.main.clone();
    let mut gone_ids = vec![];
    d.edit(&book, |root| {
        let Some(sheets) = root.child(ns::S, "sheets") else {
            return Ok(false);
        };
        let list: Vec<(String, bool)> = sheets
            .els()
            .map(|s| {
                let name = s.attr("", "name").unwrap_or_default().to_string();
                let visible = s.attr("", "state").is_none_or(|v| v == "visible");
                (name, visible)
            })
            .collect();
        let gone: Vec<usize> = (0..list.len())
            .filter(|&i| names.contains(&list[i].0))
            .collect();
        // The new index of each old one, None when deleted.
        let mut next = 0;
        let new: Vec<Option<usize>> = (0..list.len())
            .map(|i| {
                (!gone.contains(&i)).then(|| {
                    next += 1;
                    next - 1
                })
            })
            .collect();
        let first_visible = (0..list.len())
            .find(|&i| list[i].1 && new[i].is_some())
            .and_then(|i| new[i])
            .unwrap_or(0);
        if let Some(sheets) = root.child_mut(ns::S, "sheets") {
            let mut i = 0;
            sheets.kids.retain(|k| {
                let Node::El(s) = k else { return true };
                i += 1;
                if gone.contains(&(i - 1)) {
                    gone_ids.extend(s.attr(ns::R, "id").map(String::from));
                    false
                } else {
                    true
                }
            });
        }
        if let Some(defined) = root.child_mut(ns::S, "definedNames") {
            defined.kids.retain_mut(|k| {
                let Node::El(n) = k else { return true };
                let local: Option<usize> = n.attr("", "localSheetId").and_then(|v| v.parse().ok());
                match local.map(|l| new.get(l).copied().flatten()) {
                    Some(None) => return false,
                    Some(Some(l)) => n.set("", "localSheetId", &l.to_string()),
                    None => {}
                }
                !names_sheet(&n.text(), names)
            });
        }
        root.prune(&|e: &El| e.is(ns::S, "definedNames") && e.els().next().is_none());
        for view in root
            .child_mut(ns::S, "bookViews")
            .into_iter()
            .flat_map(El::els_mut)
        {
            for a in ["activeTab", "firstSheet"] {
                if let Some(old) = view.attr("", a).and_then(|v| v.parse::<usize>().ok()) {
                    let at = new
                        .get(old)
                        .copied()
                        .flatten()
                        .filter(|&n| list[old].1 || n == first_visible);
                    view.set("", a, &at.unwrap_or(first_visible).to_string());
                }
            }
        }
        Ok(true)
    })?;
    for id in gone_ids {
        d.unlink(&book, &id)?;
    }
    d.delete_linked("calcChain")?;
    for sheet in cell_sheets(d)? {
        d.edit(&sheet, |root| {
            Ok(root.prune(&|e: &El| {
                (e.local() == "conditionalFormatting" || e.local() == "dataValidation")
                    && names_sheet(&e.text(), names)
            }) > 0)
        })?;
    }
    Ok(())
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
