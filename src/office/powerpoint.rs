//! Checks that only PowerPoint presentations have.

use quick_xml::events::Event;

use super::{Ctx, common, ns, xml};
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
