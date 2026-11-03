//! Accepting every tracked change in a Word part, with the rules of Open-XML-PowerTools'
//! RevisionProcessor (MIT; the rules, not the code): moves land at their destination,
//! insertions stay, deletions go, deleted paragraph marks join their paragraph to the next,
//! and property and table changes keep their new state.

use super::ns::W;
use super::tree::{Attr, El, Node};

const M: &str = "http://schemas.openxmlformats.org/officeDocument/2006/math";

/// Property changes, whose old properties are dropped (rule 6).
const PROPERTY_CHANGES: [&str; 10] = [
    "pPrChange",
    "rPrChange",
    "tblPrChange",
    "tblPrExChange",
    "tblGridChange",
    "trPrChange",
    "tcPrChange",
    "sectPrChange",
    "numberingChange",
    "cellIns",
];

/// Range markers of moves and custom XML changes (rule 5).
const RANGES: [&str; 12] = [
    "moveFromRangeStart",
    "moveFromRangeEnd",
    "moveToRangeStart",
    "moveToRangeEnd",
    "customXmlInsRangeStart",
    "customXmlInsRangeEnd",
    "customXmlDelRangeStart",
    "customXmlDelRangeEnd",
    "customXmlMoveFromRangeStart",
    "customXmlMoveFromRangeEnd",
    "customXmlMoveToRangeStart",
    "customXmlMoveToRangeEnd",
];

/// Every element that records a tracked change.
const TRACKED: [&str; 8] = [
    "ins",
    "del",
    "moveFrom",
    "moveTo",
    "cellDel",
    "cellMerge",
    "delText",
    "delInstrText",
];

fn w(e: &El, local: &str) -> bool {
    e.is(W, local)
}

/// Whether a paragraph's mark was deleted (or moved away).
fn mark_deleted(p: &El) -> bool {
    p.child(W, "pPr")
        .and_then(|pr| pr.child(W, "rPr"))
        .is_some_and(|r| r.child(W, "del").is_some() || r.child(W, "moveFrom").is_some())
}

/// Accepts every tracked change in a story part (document, header, footer, notes, comments,
/// glossary): rules 1 to 12, in order.
pub fn accept(root: &mut El) {
    // 1. Moves: the destination stays, the source goes.
    root.unwrap(&|e| w(e, "moveTo"));
    root.prune(&|e| w(e, "moveFrom") && !marker(e));
    // 2. Content controls whose content was all deleted or moved away.
    root.prune(&|e| w(e, "sdt") && all_deleted(e));
    // 3. Deleted paragraph marks join their paragraph to the next one.
    merge_marks(root);
    // 4. Insertions stay; marks of inserted properties go.
    root.unwrap(&|e| w(e, "ins") && !marker(e));
    // 5, 6. Range markers and old properties.
    root.prune(&|e| e.ns.as_ref() == W && RANGES.contains(&e.local()));
    root.prune(&|e| e.ns.as_ref() == W && PROPERTY_CHANGES.contains(&e.local()));
    // 7, 8. Deleted text, runs, rows and math.
    root.prune(&|e| {
        w(e, "del")
            || (w(e, "r")
                && e.child(W, "rPr")
                    .is_some_and(|r| r.child(W, "del").is_some()))
            || (w(e, "tr")
                && e.child(W, "trPr")
                    .is_some_and(|r| r.child(W, "del").is_some()))
            || (e.is(M, "f")
                && e.child(M, "fPr")
                    .and_then(|p| p.child(M, "ctrlPr"))
                    .is_some_and(|c| c.child(W, "del").is_some()))
    });
    // The marks left in properties: inserted, moved or deleted (last) paragraph marks.
    root.prune(&|e| {
        ["ins", "del", "moveFrom", "moveTo"].contains(&e.local()) && e.ns.as_ref() == W
    });
    // 9, 10, 11. Cell merges, deleted cells and emptied tables.
    tables(root);
    // 12. Tables that now touch, with the same style, become one.
    join_tables(root);
}

/// Accepts the property changes in styles or numbering: only rule 6 applies there.
pub fn accept_properties(root: &mut El) -> usize {
    root.prune(&|e| e.ns.as_ref() == W && PROPERTY_CHANGES.contains(&e.local()))
}

/// The first tracked-change element left in a part, as `w:<name>`.
pub fn leftover(root: &El) -> Option<String> {
    let mut found = None;
    root.each(&mut |e| {
        let tracked = TRACKED.contains(&e.local())
            || RANGES.contains(&e.local())
            || PROPERTY_CHANGES.contains(&e.local());
        if found.is_none() && e.ns.as_ref() == W && tracked {
            found = Some(format!("w:{}", e.local()));
        }
    });
    found
}

/// Whether an element is a mark inside properties (`w:rPr/w:ins`, `w:trPr/w:ins`, …) rather
/// than a wrapper around content: it holds no elements.
fn marker(e: &El) -> bool {
    e.kids.iter().all(|k| matches!(k, Node::Text(_)))
}

/// Whether every run in a content control was deleted or moved away (and it has one).
fn all_deleted(sdt: &El) -> bool {
    let Some(content) = sdt.child(W, "sdtContent") else {
        return false;
    };
    let (mut runs, mut gone) = (0, 0);
    count_runs(content, false, &mut runs, &mut gone);
    runs > 0 && runs == gone
}

fn count_runs(e: &El, deleted: bool, runs: &mut usize, gone: &mut usize) {
    for k in e.els() {
        let deleted = deleted || w(k, "del") || w(k, "moveFrom");
        if w(k, "r") {
            *runs += 1;
            *gone += usize::from(deleted);
        } else {
            count_runs(k, deleted, runs, gone);
        }
    }
}

/// Rule 3, in every element that holds paragraphs: each run of paragraphs whose mark was
/// deleted joins the paragraph after it, which keeps its own properties. With no paragraph
/// after it (the end, or a table), the run joins its own last paragraph, which only loses the
/// mark. Bookmarks and other range marks between them stay where they are.
fn merge_marks(root: &mut El) {
    for e in root.els_mut() {
        merge_marks(e);
    }
    if !root.els().any(|p| w(p, "p") && mark_deleted(p)) {
        return;
    }
    let content = |p: El| {
        p.kids
            .into_iter()
            .filter(|k| !matches!(k, Node::El(e) if w(e, "pPr")))
    };
    let flush = |pending: &mut Vec<El>, kids: &mut Vec<Node>| {
        if let Some(mut last) = pending.pop() {
            let before: Vec<Node> = pending.drain(..).flat_map(content).collect();
            let at = usize::from(last.child(W, "pPr").is_some());
            last.kids.splice(at..at, before);
            kids.push(Node::El(last));
        }
    };
    let mut kids: Vec<Node> = Vec::with_capacity(root.kids.len());
    let mut pending: Vec<El> = vec![];
    for k in std::mem::take(&mut root.kids) {
        match k {
            Node::El(p) if w(&p, "p") && mark_deleted(&p) => pending.push(p),
            Node::El(mut p) if w(&p, "p") => {
                let before: Vec<Node> = pending.drain(..).flat_map(content).collect();
                let at = usize::from(p.child(W, "pPr").is_some());
                p.kids.splice(at..at, before);
                kids.push(Node::El(p));
            }
            Node::El(e) if !pending.is_empty() && !range_mark(&e) => {
                flush(&mut pending, &mut kids);
                kids.push(Node::El(e));
            }
            k => kids.push(k),
        }
    }
    flush(&mut pending, &mut kids);
    root.kids = kids;
}

/// Whether an element only marks a place (a bookmark, permission or range edge).
fn range_mark(e: &El) -> bool {
    let l = e.local();
    e.ns.as_ref() == W
        && (l.ends_with("RangeStart")
            || l.ends_with("RangeEnd")
            || matches!(
                l,
                "bookmarkStart" | "bookmarkEnd" | "permStart" | "permEnd" | "proofErr"
            ))
}

/// Rules 9 to 11: `w:cellMerge` becomes `w:vMerge`, a deleted cell gives its width to the
/// cell before it, a cell left with no paragraph gets an empty one, and a table left with no
/// row goes.
fn tables(root: &mut El) {
    for e in root.els_mut() {
        tables(e);
    }
    if w(root, "tr") {
        let mut kids: Vec<Node> = vec![];
        for k in std::mem::take(&mut root.kids) {
            match k {
                Node::El(tc) if w(&tc, "tc") && cell_deleted(&tc) => {
                    let span = grid_span(&tc);
                    if let Some(prev) = kids.iter_mut().rev().find_map(|n| match n {
                        Node::El(e) if w(e, "tc") => Some(e),
                        _ => None,
                    }) {
                        let total = grid_span(prev) + span;
                        set_grid_span(prev, total);
                    }
                }
                k => kids.push(k),
            }
        }
        root.kids = kids;
    }
    if w(root, "tcPr") {
        for k in &mut root.kids {
            if let Node::El(e) = k
                && w(e, "cellMerge")
            {
                let v = match e.attr(W, "vMerge") {
                    Some("rest") => "restart",
                    _ => "continue",
                };
                let mut merge = El::like(e, "vMerge");
                merge.attrs.push(Attr {
                    name: El::like(e, "val").name,
                    ns: W.into(),
                    value: v.into(),
                });
                *e = merge;
            }
        }
    }
    if w(root, "tc") && root.child(W, "p").is_none() {
        let p = El::like(root, "p");
        root.kids.push(Node::El(p));
    }
    root.prune(&|e| w(e, "tbl") && e.child(W, "tr").is_none());
}

fn cell_deleted(tc: &El) -> bool {
    tc.child(W, "tcPr")
        .is_some_and(|p| p.child(W, "cellDel").is_some())
}

fn grid_span(tc: &El) -> u32 {
    tc.child(W, "tcPr")
        .and_then(|p| p.child(W, "gridSpan"))
        .and_then(|g| g.attr(W, "val"))
        .and_then(|v| v.parse().ok())
        .unwrap_or(1)
}

fn set_grid_span(tc: &mut El, n: u32) {
    if tc.child(W, "tcPr").is_none() {
        let pr = El::like(tc, "tcPr");
        tc.kids.insert(0, Node::El(pr));
    }
    let Some(pr) = tc.child_mut(W, "tcPr") else {
        return;
    };
    if pr.child(W, "gridSpan").is_none() {
        let mut g = El::like(pr, "gridSpan");
        g.attrs.push(Attr {
            name: El::like(pr, "val").name,
            ns: W.into(),
            value: String::new(),
        });
        // gridSpan comes after cnfStyle and tcW in a cell's properties.
        let at = pr
            .kids
            .iter()
            .position(|k| matches!(k, Node::El(e) if !w(e, "cnfStyle") && !w(e, "tcW")))
            .unwrap_or(pr.kids.len());
        pr.kids.insert(at, Node::El(g));
    }
    if let Some(g) = pr.child_mut(W, "gridSpan") {
        g.set(W, "val", &n.to_string());
    }
}

/// The style a table names, if any.
fn table_style(t: &El) -> Option<&str> {
    t.child(W, "tblPr")?.child(W, "tblStyle")?.attr(W, "val")
}

/// Rule 12: two tables with nothing between them and the same style become one.
fn join_tables(root: &mut El) {
    for e in root.els_mut() {
        join_tables(e);
    }
    let mut kids: Vec<Node> = vec![];
    for k in std::mem::take(&mut root.kids) {
        match (kids.last_mut(), k) {
            (Some(Node::El(prev)), Node::El(t))
                if w(prev, "tbl") && w(&t, "tbl") && table_style(prev) == table_style(&t) =>
            {
                prev.kids.extend(
                    t.kids
                        .into_iter()
                        .filter(|n| matches!(n, Node::El(e) if w(e, "tr"))),
                );
            }
            (_, k) => kids.push(k),
        }
    }
    root.kids = kids;
}

#[cfg(test)]
mod tests {
    use super::super::tree::Doc;
    use super::*;

    fn accepted(body: &str) -> String {
        let xml = format!("<w:body xmlns:w=\"{W}\">{body}</w:body>");
        let mut d = Doc::parse(xml.as_bytes(), "p").unwrap();
        accept(&mut d.root);
        assert_eq!(leftover(&d.root), None);
        let out = String::from_utf8(d.write()).unwrap();
        out[out.find('>').unwrap() + 1..out.len() - "</w:body>".len()].to_string()
    }

    const SAM: &str = "w:id=\"1\" w:author=\"Sam Sample\"";

    #[test]
    fn insertions_stay_and_deletions_go() {
        assert_eq!(
            accepted(&format!(
                "<w:p><w:r><w:t>a</w:t></w:r><w:ins {SAM}><w:r><w:t>b</w:t></w:r></w:ins><w:del {SAM}><w:r><w:delText>c</w:delText></w:r></w:del></w:p>"
            )),
            "<w:p><w:r><w:t>a</w:t></w:r><w:r><w:t>b</w:t></w:r></w:p>"
        );
    }

    #[test]
    fn moves_land_at_their_destination() {
        assert_eq!(
            accepted(&format!(
                "<w:p><w:moveFromRangeStart w:id=\"5\" w:name=\"m\"/><w:moveFrom {SAM}><w:r><w:t>x</w:t></w:r></w:moveFrom><w:moveFromRangeEnd w:id=\"5\"/><w:r><w:t>y</w:t></w:r><w:moveToRangeStart w:id=\"6\" w:name=\"m\"/><w:moveTo {SAM}><w:r><w:t>x</w:t></w:r></w:moveTo><w:moveToRangeEnd w:id=\"6\"/></w:p>"
            )),
            "<w:p><w:r><w:t>y</w:t></w:r><w:r><w:t>x</w:t></w:r></w:p>"
        );
    }

    #[test]
    fn a_deleted_mark_joins_the_next_paragraph_which_keeps_its_properties() {
        let del = format!("<w:pPr><w:jc w:val=\"left\"/><w:rPr><w:del {SAM}/></w:rPr></w:pPr>");
        assert_eq!(
            accepted(&format!(
                "<w:p>{del}<w:r><w:t>a</w:t></w:r></w:p><w:p>{del}<w:r><w:t>b</w:t></w:r></w:p><w:p><w:pPr><w:jc w:val=\"right\"/></w:pPr><w:r><w:t>c</w:t></w:r></w:p>"
            )),
            "<w:p><w:pPr><w:jc w:val=\"right\"/></w:pPr><w:r><w:t>a</w:t></w:r><w:r><w:t>b</w:t></w:r><w:r><w:t>c</w:t></w:r></w:p>"
        );
        // The last paragraph only loses its mark.
        assert_eq!(
            accepted(&format!(
                "<w:p><w:r><w:t>a</w:t></w:r></w:p><w:p>{del}<w:r><w:t>b</w:t></w:r></w:p>"
            )),
            "<w:p><w:r><w:t>a</w:t></w:r></w:p><w:p><w:pPr><w:jc w:val=\"left\"/><w:rPr/></w:pPr><w:r><w:t>b</w:t></w:r></w:p>"
        );
    }

    #[test]
    fn property_changes_keep_the_new_properties() {
        assert_eq!(
            accepted(&format!(
                "<w:p><w:pPr><w:rPr><w:ins {SAM}/></w:rPr><w:pPrChange {SAM}><w:pPr/></w:pPrChange></w:pPr><w:r><w:rPr><w:b/><w:rPrChange {SAM}><w:rPr/></w:rPrChange></w:rPr><w:t>a</w:t></w:r></w:p>"
            )),
            "<w:p><w:pPr><w:rPr/></w:pPr><w:r><w:rPr><w:b/></w:rPr><w:t>a</w:t></w:r></w:p>"
        );
    }

    #[test]
    fn deleted_rows_and_cells_go_and_spans_follow() {
        let p = "<w:p><w:r><w:t>x</w:t></w:r></w:p>";
        assert_eq!(
            accepted(&format!(
                "<w:tbl><w:tr><w:tc>{p}</w:tc><w:tc><w:tcPr><w:cellDel {SAM}/></w:tcPr>{p}</w:tc></w:tr><w:tr><w:trPr><w:del {SAM}/></w:trPr><w:tc>{p}</w:tc></w:tr><w:tr><w:tc><w:tcPr><w:cellMerge {SAM} w:vMerge=\"rest\"/></w:tcPr><w:del {SAM}>{p}</w:del></w:tc></w:tr></w:tbl>"
            )),
            format!(
                "<w:tbl><w:tr><w:tc><w:tcPr><w:gridSpan w:val=\"2\"/></w:tcPr>{p}</w:tc></w:tr><w:tr><w:tc><w:tcPr><w:vMerge w:val=\"restart\"/></w:tcPr><w:p/></w:tc></w:tr></w:tbl>"
            )
        );
        assert_eq!(
            accepted(&format!(
                "<w:tbl><w:tr><w:trPr><w:del {SAM}/></w:trPr><w:tc>{p}</w:tc></w:tr></w:tbl>{p}"
            )),
            p
        );
    }

    #[test]
    fn deleted_content_controls_and_touching_tables() {
        let p = "<w:p><w:r><w:t>x</w:t></w:r></w:p>";
        let t = |s: &str| {
            format!(
                "<w:tbl><w:tblPr><w:tblStyle w:val=\"{s}\"/></w:tblPr><w:tr><w:tc>{p}</w:tc></w:tr></w:tbl>"
            )
        };
        let del = format!("<w:p><w:del {SAM}><w:r><w:delText>y</w:delText></w:r></w:del></w:p>");
        let sdt = format!("<w:sdt><w:sdtContent>{del}</w:sdtContent></w:sdt>");
        let joined = accepted(&format!("{}{sdt}{}{}", t("A"), t("A"), t("B")));
        assert_eq!(
            joined,
            format!(
                "<w:tbl><w:tblPr><w:tblStyle w:val=\"A\"/></w:tblPr><w:tr><w:tc>{p}</w:tc></w:tr><w:tr><w:tc>{p}</w:tc></w:tr></w:tbl>{}",
                t("B")
            )
        );
    }
}
