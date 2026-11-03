//! Checks that only PowerPoint presentations have.

use quick_xml::events::Event;

use super::clean::Draft;
use super::tree::{Attr, El, Node};
use super::{Ctx, common, key, ns, xml};
use crate::report::{CheckId, Error, Finding, Where, plural};

/// `hidden-slides`: slides left out of the show.
pub fn hidden_slides(ctx: &Ctx) -> Result<Vec<Finding>, Error> {
    let mut found = vec![];
    for (i, part) in ctx.slides.iter().enumerate() {
        let mut hidden = false;
        ctx.pkg.walk(part, |r, ev| {
            if let Event::Start(e) | Event::Empty(e) = ev
                && r.level() == 0
            {
                hidden = matches!(r.attr(e, "show")?.as_deref(), Some("0" | "false"));
            }
            Ok(())
        })?;
        if hidden {
            let place = Where {
                slide: Some(i + 1),
                ..Where::part(part)
            };
            let f = Finding::new(
                CheckId::HiddenSlides,
                "hidden_slide",
                place,
                1,
                "slides",
                "1 hidden slide".into(),
            );
            found.push(f.with_value(ctx.values, Some(&common::all_text(ctx.pkg, part)?)));
        }
    }
    Ok(found)
}

/// Placeholders a notes page always has, which hold no notes.
const NOTES_FURNITURE: [&str; 5] = ["sldImg", "sldNum", "hdr", "ftr", "dt"];

/// `speaker-notes`: slides whose notes page holds text or pictures.
pub fn speaker_notes(ctx: &Ctx) -> Result<Vec<Finding>, Error> {
    let pkg = ctx.pkg;
    let mut found = vec![];
    for (i, slide) in ctx.slides.iter().enumerate() {
        let notes = pkg
            .links
            .iter()
            .filter(|l| l.source == *slide && l.rel.short() == "notesSlide")
            .filter_map(|l| l.part.as_deref());
        for part in notes {
            let (mut text, mut pictures, mut furniture) = (String::new(), 0, false);
            pkg.walk(part, |r, ev| {
                match ev {
                    Event::Start(e) | Event::Empty(e) if r.is(e, ns::P, "sp") => furniture = false,
                    Event::Start(e) | Event::Empty(e) if r.is(e, ns::P, "ph") => {
                        let kind = r.attr(e, "type")?;
                        furniture = kind.is_some_and(|k| NOTES_FURNITURE.contains(&k.as_ref()));
                    }
                    Event::Start(e) | Event::Empty(e) if r.is(e, ns::P, "pic") => pictures += 1,
                    ev if !furniture && r.parent() == Some((ns::A, "t")) => {
                        text += &xml::text(ev).unwrap_or_default();
                    }
                    _ => {}
                }
                Ok(())
            })?;
            let chars = text.chars().filter(|c| !c.is_whitespace()).count();
            let detail = match (chars, pictures) {
                (0, 0) => continue,
                (0, p) => format!("speaker notes with {}", plural(p, "picture")),
                (n, _) => format!("speaker notes with {}", plural(n as u64, "character")),
            };
            let place = Where {
                slide: Some(i + 1),
                ..Where::part(part)
            };
            let f = Finding::new(CheckId::SpeakerNotes, "notes", place, 1, "notes", detail);
            found.push(f.with_value(ctx.values, Some(text.as_str()).filter(|t| !t.is_empty())));
        }
    }
    Ok(found)
}

/// Turns each OLE object frame of a slide into the picture that previews it: the frame's
/// `p:cNvPr` and position, the preview's fill. A frame with no preview goes. Gives how many
/// became pictures and how many went.
pub fn ole_to_pictures(root: &mut El) -> (u64, u64) {
    let (mut pictures, mut gone) = (0, 0);
    convert(root, &mut pictures, &mut gone);
    (pictures, gone)
}

fn convert(e: &mut El, pictures: &mut u64, gone: &mut u64) {
    let ole = |f: &El| f.is(ns::P, "graphicFrame") && f.has(&|o: &El| o.is(ns::P, "oleObj"));
    let mut kids = Vec::with_capacity(e.kids.len());
    for k in std::mem::take(&mut e.kids) {
        match k {
            Node::El(f) if ole(&f) => match picture_of(&f) {
                Some(pic) => {
                    *pictures += 1;
                    kids.push(Node::El(pic));
                }
                None => *gone += 1,
            },
            Node::El(mut c) => {
                convert(&mut c, pictures, gone);
                kids.push(Node::El(c));
            }
            k => kids.push(k),
        }
    }
    e.kids = kids;
}

/// The `p:pic` that shows an OLE frame: its preview's fill in the frame's place.
fn picture_of(frame: &El) -> Option<El> {
    let mut fill = None;
    frame.each(&mut |e| {
        if fill.is_none() && e.is(ns::P, "pic") {
            fill = e.child(ns::P, "blipFill").cloned();
        }
    });
    let fill = fill?;
    let nv = frame.child(ns::P, "nvGraphicFramePr")?;
    let xfrm = frame.child(ns::P, "xfrm")?;
    let a = xfrm.els().next()?;
    let el = |like: &El, local: &str, kids: Vec<El>| El {
        kids: kids.into_iter().map(Node::El).collect(),
        ..El::like(like, local)
    };
    let mut c_nv = nv.child(ns::P, "cNvPr")?.clone();
    c_nv.kids.clear();
    let nv_pr = nv
        .child(ns::P, "nvPr")
        .cloned()
        .unwrap_or_else(|| El::like(nv, "nvPr"));
    let mut geometry = El::like(a, "prstGeom");
    geometry.attrs.push(Attr {
        name: "prst".into(),
        ns: "".into(),
        value: "rect".into(),
    });
    geometry.kids.push(Node::El(El::like(a, "avLst")));
    let shape = el(
        frame,
        "spPr",
        vec![el(a, "xfrm", xfrm.els().cloned().collect()), geometry],
    );
    let nv_pic = el(
        frame,
        "nvPicPr",
        vec![c_nv, El::like(frame, "cNvPicPr"), nv_pr],
    );
    Some(el(frame, "pic", vec![nv_pic, fill, shape]))
}

const P14: &str = "http://schemas.microsoft.com/office/powerpoint/2010/main";

/// Deletes the slides `slides` (parts): their entries in the slide list, custom shows and
/// sections, and the links to them on other slides; the sweep takes their notes, comments,
/// charts and media.
pub fn delete_slides(d: &mut Draft, slides: &[String]) -> Result<(), Error> {
    let gone = |p: &Option<String>| {
        p.as_deref()
            .is_some_and(|p| slides.iter().any(|s| key(s) == key(p)))
    };
    let main = d.main.clone();
    let rids: Vec<String> = d
        .rels(&main)?
        .into_iter()
        .filter(|r| r.short() == "slide" && gone(&d.target(&main, &r.id).ok().flatten()))
        .map(|r| r.id)
        .collect();
    let mut numbers = vec![];
    if let Some(doc) = d.doc(&main)? {
        doc.root.each(&mut |e| {
            if e.is(ns::P, "sldId")
                && e.attr(ns::R, "id")
                    .is_some_and(|id| rids.iter().any(|r| r == id))
            {
                numbers.extend(e.attr("", "id").map(String::from));
            }
        });
    }
    d.prune_linked(&main, |e| {
        let rid = e
            .attr(ns::R, "id")
            .is_some_and(|id| rids.iter().any(|r| r == id));
        ((e.is(ns::P, "sldId") || e.is(ns::P, "sld")) && rid)
            || (e.is(P14, "sldId")
                && e.attr("", "id")
                    .is_some_and(|n| numbers.iter().any(|g| g == n)))
    })?;
    for l in d.links()? {
        if l.rel.short() == "slide" && gone(&l.part) && key(&l.source) != key(&main) {
            let id = l.rel.id.clone();
            d.prune_linked(&l.source, |e| {
                (e.is(ns::A, "hlinkClick") || e.is(ns::A, "hlinkHover"))
                    && e.attr(ns::R, "id") == Some(&id)
            })?;
            d.unlink(&l.source, &l.rel.id)?;
        }
    }
    Ok(())
}
