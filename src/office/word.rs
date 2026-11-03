//! Checks that only Word documents have.

use std::collections::HashSet;

use quick_xml::events::Event;

use super::{Ctx, ns};
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
                    let text = super::xml::text(ev).unwrap_or_default();
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
