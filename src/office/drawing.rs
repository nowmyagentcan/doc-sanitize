//! Shapes and pictures in DrawingML and VML, wherever a format puts them.

use quick_xml::events::Event;

use super::tree::{El, MC, Node};
use super::{Ctx, common, ns, word, xml};
use crate::report::{CheckId, Error, Finding, Format, counted, plural};

const VML: &str = "urn:schemas-microsoft-com:vml";
const DRAWING_PART: &str = "application/vnd.openxmlformats-officedocument.drawing+xml";

/// Elements that are one object on the page.
const OBJECTS: [&str; 10] = [
    "sp",
    "pic",
    "graphicFrame",
    "cxnSp",
    "grpSp",
    "contentPart",
    "inline",
    "anchor",
    "wsp",
    "wgp",
];

/// The parts whose drawings are checked: Word's text parts, Excel's drawings, the slides.
pub fn drawing_parts<'c>(ctx: &'c Ctx) -> Vec<&'c str> {
    let pkg = ctx.pkg;
    match pkg.format {
        Format::Docx => word::pages(ctx).collect(),
        Format::Xlsx => pkg.typed(DRAWING_PART).collect(),
        _ => ctx.slides.iter().map(String::as_str).collect(),
    }
}

/// An object being read: its level, whether it is hidden, its text, and its box with the
/// group mapping as (off, ext, child off, child ext) in EMU.
#[derive(Default)]
struct Object {
    level: usize,
    leaf: bool,
    hidden: bool,
    text: String,
    off: Option<(i64, i64)>,
    ext: Option<(i64, i64)>,
    ch_off: Option<(i64, i64)>,
    ch_ext: Option<(i64, i64)>,
}

/// `hidden-objects`: objects marked hidden, VML shapes styled hidden, and PowerPoint shapes
/// that sit entirely off the slide once their groups are applied.
pub fn hidden_objects(ctx: &Ctx) -> Result<Vec<Finding>, Error> {
    let pkg = ctx.pkg;
    let mut found = vec![];
    for part in drawing_parts(ctx) {
        let (mut hidden, mut off) = ((0, None::<String>), (0, None::<String>));
        let mut open: Vec<Object> = vec![];
        pkg.walk(part, |r, ev| {
            match ev {
                Event::Start(e) | Event::Empty(e) => {
                    let local = e.local_name();
                    let local = local.as_ref();
                    let start = matches!(ev, Event::Start(_));
                    let inside_hidden = open.iter().any(|o| o.hidden);
                    if r.ns(e) == VML
                        && r.attr(e, "style")?
                            .is_some_and(|s| s.replace(' ', "").contains("visibility:hidden"))
                    {
                        if start {
                            open.push(Object {
                                level: r.level(),
                                hidden: true,
                                ..Object::default()
                            });
                        } else if !inside_hidden {
                            hidden.0 += 1;
                        }
                    } else if start && r.ns(e) != ns::W && OBJECTS.contains(&local) {
                        let leaf = !matches!(local, "grpSp" | "wgp");
                        open.push(Object {
                            level: r.level(),
                            leaf,
                            ..Object::default()
                        });
                    } else if (local == "cNvPr" || local == "docPr")
                        && xml::yes(r.attr(e, "hidden")?)
                        && !inside_hidden
                    {
                        if let Some(o) = open.last_mut() {
                            o.hidden = true;
                        }
                    } else if matches!(r.parent(), Some((_, "xfrm")))
                        && let Some(o) = open.last_mut()
                    {
                        let num = |a| -> Result<i64, Error> {
                            Ok(r.attr(e, a)?.and_then(|v| v.parse().ok()).unwrap_or(0))
                        };
                        let slot = match local {
                            "off" => &mut o.off,
                            "ext" => &mut o.ext,
                            "chOff" => &mut o.ch_off,
                            "chExt" => &mut o.ch_ext,
                            _ => return Ok(()),
                        };
                        let pair = if local.ends_with("ff") {
                            (num("x")?, num("y")?)
                        } else {
                            (num("cx")?, num("cy")?)
                        };
                        slot.get_or_insert(pair);
                    }
                }
                Event::End(_) if open.last().is_some_and(|o| o.level == r.level()) => {
                    let o = open.pop().unwrap_or_default();
                    let around_hidden = open.iter().any(|g| g.hidden);
                    if o.hidden && !around_hidden {
                        hidden.0 += 1;
                        hidden.1.get_or_insert(o.text.clone());
                    } else if o.leaf
                        && !around_hidden
                        && pkg.format == Format::Pptx
                        && ctx.slide_size.is_some_and(|s| off_slide(s, &o, &open))
                    {
                        off.0 += 1;
                        off.1.get_or_insert(o.text.clone());
                    }
                    if let Some(parent) = open.last_mut() {
                        parent.text += &o.text;
                    }
                }
                ev if matches!(r.parent(), Some((_, "t"))) => {
                    if let (Some(o), Some(t)) = (open.last_mut(), xml::text(ev)) {
                        o.text += &t;
                    }
                }
                _ => {}
            }
            Ok(())
        })?;
        for ((n, text), kind, what) in [
            (hidden, "hidden_object", "hidden object"),
            (off, "off_slide_object", "object off the slide"),
        ] {
            if n > 0 {
                let detail = match kind {
                    "hidden_object" => plural(n, what),
                    _ => format!("{} off the slide", plural(n, "object")),
                };
                let f = Finding::new(
                    CheckId::HiddenObjects,
                    kind,
                    ctx.place(part),
                    n,
                    "objects",
                    detail,
                );
                found.push(f.with_value(ctx.values, text.as_deref().filter(|t| !t.is_empty())));
            }
        }
    }
    Ok(found)
}

/// Whether a shape's box, mapped through the groups around it, misses the slide. A shape
/// with no box of its own (a placeholder) is on the slide.
fn off_slide((cx, cy): (i64, i64), o: &Object, groups: &[Object]) -> bool {
    let (Some((mut x, mut y)), Some((mut w, mut h))) = (o.off, o.ext) else {
        return false;
    };
    for g in groups.iter().rev() {
        let (Some(off), Some(ext), Some(ch_off), Some(ch_ext)) = (g.off, g.ext, g.ch_off, g.ch_ext)
        else {
            continue;
        };
        let scale = |v: i64, num: i64, den: i64| {
            if den == 0 {
                v
            } else {
                (v as i128 * num as i128 / den as i128) as i64
            }
        };
        x = off.0 + scale(x - ch_off.0, ext.0, ch_ext.0);
        y = off.1 + scale(y - ch_off.1, ext.1, ch_ext.1);
        w = scale(w, ext.0, ch_ext.0);
        h = scale(h, ext.1, ch_ext.1);
    }
    x + w < 0 || y + h < 0 || x > cx || y > cy
}

/// `cropped-images` and `external-links` in drawings: pictures whose crop hides at least 1% of
/// the image, and `descr` or `title` alt text that holds a path.
pub fn crops_and_alt_text(ctx: &Ctx) -> Result<Vec<Finding>, Error> {
    let pkg = ctx.pkg;
    let mut found = vec![];
    for part in drawing_parts(ctx) {
        let (mut crops, mut most, mut paths) = (0u64, 0.0f64, vec![]);
        pkg.walk(part, |r, ev| {
            let (Event::Start(e) | Event::Empty(e)) = ev else {
                return Ok(());
            };
            if r.is(e, ns::A, "srcRect") {
                let side = |a| -> Result<f64, Error> { Ok(share(r.attr(e, a)?.as_deref())) };
                let hidden = hidden([side("l")?, side("t")?, side("r")?, side("b")?]);
                if hidden >= 0.01 {
                    crops += 1;
                    most = most.max(hidden);
                }
            }
            for (ns, name, v) in r.attrs(e)? {
                if ns.is_empty()
                    && (name == "descr" || name == "title")
                    && common::is_path(&v, false)
                {
                    paths.push(v.into_owned());
                }
            }
            Ok(())
        })?;
        if crops > 0 {
            let share = format!("{}% cut away by cropping", (most * 100.0).round());
            let detail = match crops {
                1 => format!("1 picture with {share}"),
                n => format!("{} with up to {share}", plural(n, "picture")),
            };
            found.push(Finding::new(
                CheckId::CroppedImages,
                "cropped_picture",
                ctx.place(part),
                crops,
                "pictures",
                detail,
            ));
        }
        if !paths.is_empty() {
            let detail = counted(
                paths.len() as u64,
                "alt text that holds a file path",
                "alt texts that hold a file path",
            );
            let f = Finding::new(
                CheckId::ExternalLinks,
                "path_in_alt_text",
                ctx.place(part),
                paths.len() as u64,
                "links",
                detail,
            );
            found.push(f.with_value(ctx.values, paths.first().map(String::as_str)));
        }
    }
    Ok(found)
}

/// One side of an `a:srcRect` (1/1000 of a percent) as a share of the picture; a negative
/// value cuts nothing.
fn share(v: Option<&str>) -> f64 {
    let v: f64 = v.and_then(|v| v.parse().ok()).unwrap_or(0.0);
    (v.max(0.0) / 100_000.0).min(1.0)
}

/// The share of a picture hidden by cutting `[l, t, r, b]` from its sides.
fn hidden([l, t, r, b]: [f64; 4]) -> f64 {
    1.0 - (1.0 - l - r).max(0.0) * (1.0 - t - b).max(0.0)
}

/// The pictures in a part whose `a:srcRect` hides at least 1% of them: the relationship id of
/// the stored picture ("" when none is stored) and the shares cut from the left, top, right
/// and bottom.
pub fn crops(root: &El) -> Vec<(String, [f64; 4])> {
    let mut out = vec![];
    root.each(&mut |e| out.extend(crop_of(e)));
    out
}

/// The crop of a picture fill (an element holding `a:blip` and `a:srcRect`), as [`crops`]
/// gives it.
pub fn crop_of(e: &El) -> Option<(String, [f64; 4])> {
    let rect = e.child(ns::A, "srcRect")?;
    let cut = ["l", "t", "r", "b"].map(|a| share(rect.attr("", a)));
    let blip = e.child(ns::A, "blip").and_then(|b| b.attr(ns::R, "embed"));
    (hidden(cut) >= 0.01).then(|| (blip.unwrap_or_default().to_string(), cut))
}

/// An object element and what makes it hidden: its own `cNvPr`/`docPr` and its box (both
/// read without looking into the objects inside it).
fn object_of(e: &El) -> Object {
    let mut o = Object {
        leaf: !matches!(e.local(), "grpSp" | "wgp"),
        ..Object::default()
    };
    fn scan(e: &El, o: &mut Object, top: bool) {
        for c in e.els() {
            let object = &*c.ns != ns::W && OBJECTS.contains(&c.local());
            if object && !top {
                continue;
            }
            if (c.local() == "cNvPr" || c.local() == "docPr")
                && c.attr("", "hidden")
                    .is_some_and(|h| h == "1" || h == "true")
            {
                o.hidden = true;
            }
            if e.local() == "xfrm" {
                let num = |a| c.attr("", a).and_then(|v| v.parse().ok()).unwrap_or(0);
                let slot = match c.local() {
                    "off" => &mut o.off,
                    "ext" => &mut o.ext,
                    "chOff" => &mut o.ch_off,
                    "chExt" => &mut o.ch_ext,
                    _ => continue,
                };
                let pair = if c.local().ends_with("ff") {
                    (num("x"), num("y"))
                } else {
                    (num("cx"), num("cy"))
                };
                slot.get_or_insert(pair);
            }
            if !object {
                scan(c, o, false);
            }
        }
    }
    scan(e, &mut o, true);
    o
}

/// Whether a VML element is styled hidden.
fn vml_hidden(e: &El) -> bool {
    &*e.ns == VML
        && e.attr("", "style")
            .is_some_and(|s| s.replace(' ', "").contains("visibility:hidden"))
}

/// Removes hidden objects (and, on slides, objects off the slide of size `slide`) from a
/// drawing part: Word removes the `w:drawing` or `w:pict` around them, Excel the anchor,
/// PowerPoint the shape itself. Gives the `cNvPr` ids of everything removed.
pub fn remove_hidden(root: &mut El, format: Format, slide: Option<(i64, i64)>) -> Vec<String> {
    let mut ids = vec![];
    let mut groups = vec![];
    walk(root, format, slide, &mut groups, &mut ids);
    ids
}

fn walk(
    e: &mut El,
    format: Format,
    slide: Option<(i64, i64)>,
    groups: &mut Vec<Object>,
    ids: &mut Vec<String>,
) {
    let mut kids = Vec::with_capacity(e.kids.len());
    for k in std::mem::take(&mut e.kids) {
        let Node::El(mut c) = k else {
            kids.push(k);
            continue;
        };
        let hidden_inside = |c: &El| {
            c.has(&|x: &El| {
                vml_hidden(x)
                    || (&*x.ns != ns::W && OBJECTS.contains(&x.local()) && object_of(x).hidden)
            })
        };
        let removable = |c: &El| match format {
            Format::Docx => {
                (c.is(ns::W, "drawing") || c.is(ns::W, "pict") || c.is(ns::W, "object"))
                    && hidden_inside(c)
            }
            Format::Xlsx => c.local().ends_with("Anchor") && hidden_inside(c),
            _ => {
                OBJECTS.contains(&c.local()) && &*c.ns != ns::W && {
                    let o = object_of(c);
                    o.hidden || (o.leaf && slide.is_some_and(|s| off_slide(s, &o, groups)))
                }
            }
        };
        // One removed inside an alternative takes the whole alternative with it.
        let whole = removable(&c) || (c.is(MC, "AlternateContent") && c.has(&removable));
        if whole {
            c.each(&mut |x| {
                if x.local() == "cNvPr" {
                    ids.extend(x.attr("", "id").map(String::from));
                }
            });
            continue;
        }
        let group = format == Format::Pptx && c.local() == "grpSp";
        if group {
            groups.push(object_of(&c));
        }
        walk(&mut c, format, slide, groups, ids);
        if group {
            groups.pop();
        }
        kids.push(Node::El(c));
    }
    e.kids = kids;
}
