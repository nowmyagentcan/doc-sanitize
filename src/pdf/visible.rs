//! Text a reader can't see. hayro replays each page into a recording device that keeps every
//! glyph, opaque box and picture in paint order; ordered rules then find glyphs outside the
//! page, too small, clipped away, under a redaction mark, or under a box or picture painted
//! later.

use std::collections::HashMap;

use hayro::hayro_interpret::font::{Glyph, GlyphIdentifier, GlyphRun};
use hayro::hayro_interpret::hayro_cmap::BfString;
use hayro::hayro_interpret::{
    BlendMode, ClipPath, Context, Device, DrawMode, DrawProps, Image, ImageDrawProps,
    InterpreterCache, InterpreterSettings, Paint, RectExt, SoftMask, interpret_page,
};
use hayro::hayro_syntax::Pdf as Pages;
use hayro::kurbo::{Affine, BezPath, Rect, Shape};

use super::{Ctx, Found, n, pages_text, subtype};
use crate::report::{CheckId, Where};

/// Rendered checks cover this many pages.
const MAX_PAGES: usize = 2_000;

/// Both checks this module reports for.
const CHECKS: [CheckId; 2] = [CheckId::HiddenText, CheckId::FakeRedactions];

pub(super) fn check(c: &Ctx, bytes: &[u8], found: &mut Found) {
    let mut notes: Vec<(String, Vec<usize>)> = vec![];
    let mut note = |reason: &str, page: usize| match notes.iter_mut().find(|n| n.0 == reason) {
        Some(n) => n.1.push(page),
        None => notes.push((reason.into(), vec![page])),
    };
    let Ok(pdf) = Pages::new_with_password(bytes.to_vec(), &c.pdf.password) else {
        incomplete(
            found,
            "its pages couldn't be read for drawing, so their text wasn't checked",
        );
        return;
    };
    let pages = pdf.pages();
    let cache = InterpreterCache::new();
    let settings = InterpreterSettings::default();
    let total = c.pdf.pages.len();
    for (i, ((number, dict), page)) in c.pages().zip(pages.iter()).enumerate() {
        if i == MAX_PAGES {
            let note = format!(
                "pages {}–{} weren't checked: only the first {} are",
                crate::report::thousands(MAX_PAGES as u64 + 1),
                crate::report::thousands(total as u64),
                crate::report::thousands(MAX_PAGES as u64)
            );
            incomplete(found, &note);
            break;
        }
        if super::page_content(c, dict).is_none() {
            note(
                "its content couldn't be decoded, so its text wasn't checked",
                number,
            );
            continue;
        }
        let crop = page.intersected_crop_box().to_kurbo();
        let mut rec = Recorder::default();
        let mut ctx = Context::new(
            Affine::IDENTITY,
            crop,
            &cache,
            page.xref(),
            settings.clone(),
        );
        interpret_page(page, &mut ctx, &mut rec);
        let redactions = redactions(c, dict);
        let outcomes: Vec<_> = rec
            .glyphs
            .iter()
            .map(|g| classify(g, &rec, crop, &redactions))
            .collect();
        report(c, number, &rec, &outcomes, found);
    }
    if total > pages.len() && total <= MAX_PAGES {
        let first = pages.len() + 1;
        let reason = "the renderer couldn't find these pages, so their text wasn't checked";
        for page in first..=total {
            note(reason, page);
        }
    }
    for (reason, pages) in notes {
        let note = format!("{}: {reason}", pages_text(&pages));
        incomplete(found, &note);
    }
}

/// Both checks are incomplete for `note`.
fn incomplete(found: &mut Found, note: &str) {
    for check in CHECKS {
        found.incomplete.push((check, note.into()));
    }
}

/// A glyph as painted: its ink box in default user space, its text and how it was drawn.
struct Painted {
    ink: Rect,
    /// Its Unicode text; U+FFFD when the font doesn't say.
    text: String,
    /// Drawn in an invisible mode or with no alpha.
    invisible: bool,
    /// The clip in force, when there is one.
    clip: Option<Rect>,
    /// Its size in points.
    size: f64,
    /// Its place in the paint order.
    order: usize,
    /// No outline: nothing to see, nothing to hide.
    blank: bool,
}

/// An opaque axis-aligned box or an image, with its place in the paint order.
struct Cover {
    area: Rect,
    order: usize,
    /// Relative luminance of a box; None for a picture.
    luminance: Option<f64>,
    /// A picture that is opaque.
    opaque: bool,
}

/// Records glyphs, covers and pictures in paint order, in default user space.
#[derive(Default)]
struct Recorder {
    glyphs: Vec<Painted>,
    covers: Vec<Cover>,
    clips: Vec<Rect>,
    /// Whether each transparency group in force is opaque.
    groups: Vec<bool>,
    order: usize,
    outlines: HashMap<GlyphIdentifier, Rect>,
    /// The size and first transform of the last filled run, so the stroke of the same run
    /// (fill-and-stroke text) isn't recorded twice.
    last_fill: Option<(usize, Affine)>,
    /// The most pixels any image on the page declares.
    image_pixels: u64,
}

impl Recorder {
    fn clip(&self) -> Option<Rect> {
        self.clips.last().copied()
    }

    fn opaque(&self, props: &DrawProps) -> Option<f64> {
        let Paint::Color(colour) = &props.paint else {
            return None;
        };
        let [r, g, b, a] = colour.to_rgba().components();
        let plain = props.blend_mode == BlendMode::Normal && props.soft_mask.is_none();
        (a >= 1.0 && plain && self.groups.iter().all(|&g| g)).then(|| luminance([r, g, b]))
    }

    fn next(&mut self) -> usize {
        self.order += 1;
        self.order
    }
}

/// The relative luminance of an sRGB colour with components in 0..1.
fn luminance(rgb: [f32; 3]) -> f64 {
    let linear = |c: f32| {
        let c = f64::from(c);
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linear(rgb[0]) + 0.7152 * linear(rgb[1]) + 0.0722 * linear(rgb[2])
}

fn axis_aligned(t: Affine) -> bool {
    let [a, b, c, d, _, _] = t.as_coeffs();
    (b == 0.0 && c == 0.0) || (a == 0.0 && d == 0.0)
}

impl<'a> Device<'a> for Recorder {
    fn draw_path(&mut self, _: &BezPath, _: DrawProps<'a>, _: &DrawMode) {
        self.next();
    }

    fn draw_rect(&mut self, rect: &Rect, props: DrawProps<'a>, mode: &DrawMode) {
        let order = self.next();
        let luminance = self.opaque(&props);
        if matches!(mode, DrawMode::Fill(_) | DrawMode::FillAndStroke(..))
            && axis_aligned(props.transform)
            && luminance.is_some()
        {
            let area = props.transform.transform_rect_bbox(*rect);
            let area = self.clip().map_or(area, |c| area.intersect(c));
            self.covers.push(Cover {
                area,
                order,
                luminance,
                opaque: true,
            });
        }
    }

    fn push_clip_path(&mut self, clip: &ClipPath) {
        let b = clip.path.bounding_box();
        let b = self.clip().map_or(b, |c| b.intersect(c));
        self.clips.push(b);
    }

    fn push_transparency_group(
        &mut self,
        opacity: f32,
        mask: Option<SoftMask<'a>>,
        mode: BlendMode,
    ) {
        self.groups
            .push(opacity >= 1.0 && mask.is_none() && mode == BlendMode::Normal);
    }

    fn draw_glyph_run(&mut self, run: &GlyphRun<'_, 'a>, props: DrawProps<'a>, mode: &DrawMode) {
        let order = self.next();
        let glyphs = run.glyphs();
        let first = glyphs.first().map(|g| g.transform());
        match mode {
            DrawMode::Stroke(_)
                if self
                    .last_fill
                    .is_some_and(|f| Some(f) == first.map(|t| (glyphs.len(), t))) =>
            {
                return;
            }
            DrawMode::Fill(_) | DrawMode::FillAndStroke(..) => {
                self.last_fill = first.map(|t| (glyphs.len(), t));
            }
            _ => self.last_fill = None,
        }
        let alpha = match &props.paint {
            Paint::Color(c) => c.to_rgba().components()[3],
            Paint::Pattern(_) => 1.0,
        };
        let invisible = matches!(mode, DrawMode::Invisible) || alpha <= 0.0;
        for g in glyphs {
            let t = props.transform * g.transform();
            let outline = match &**g {
                Glyph::Outline(o) => *self
                    .outlines
                    .entry(o.identifier())
                    .or_insert_with(|| o.outline().bounding_box()),
                Glyph::Type3(t3) => Rect::new(
                    0.0,
                    0.0,
                    f64::from(t3.advance_width().unwrap_or(0.0)),
                    700.0,
                ),
            };
            let text = match g.as_unicode() {
                Some(BfString::Char(ch)) => ch.to_string(),
                Some(BfString::String(s)) => s,
                None => '\u{FFFD}'.into(),
            };
            self.glyphs.push(Painted {
                ink: t.transform_rect_bbox(outline),
                text,
                invisible,
                clip: self.clip(),
                size: t.determinant().abs().sqrt() * 1000.0,
                order,
                blank: outline.area() == 0.0,
            });
        }
    }

    fn draw_image(&mut self, image: Image<'a, '_>, props: ImageDrawProps<'a>) {
        let order = self.next();
        let (w, h) = (image.width(), image.height());
        self.image_pixels = self.image_pixels.max(u64::from(w) * u64::from(h));
        let opaque = match &image {
            Image::Raster(r) => {
                let d = r.stream().dict();
                !d.contains_key(b"SMask") && !d.contains_key(b"Mask")
            }
            Image::Stencil(_) => false,
        } && props.soft_mask.is_none()
            && props.blend_mode == BlendMode::Normal
            && self.groups.iter().all(|&g| g);
        let area =
            props
                .transform
                .transform_rect_bbox(Rect::new(0.0, 0.0, f64::from(w), f64::from(h)));
        let area = self.clip().map_or(area, |c| area.intersect(c));
        self.covers.push(Cover {
            area,
            order,
            luminance: None,
            opaque,
        });
    }

    fn pop_clip(&mut self) {
        self.clips.pop();
    }

    fn pop_transparency_group(&mut self) {
        self.groups.pop();
    }
}

/// What the rules make of one glyph.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Outcome {
    /// Whitespace or no outline: neither shown nor hidden.
    Blank,
    /// Hidden as `kind`; covers and pictures carry which one, so one cover makes one group.
    Hidden(Kind, Option<usize>),
    /// The geometry says nothing; the pixels decide.
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    OutsidePage,
    Tiny,
    Clipped,
    UnappliedRedaction,
    UnderBox,
    UnderImage,
    UnderPageImage,
}

impl Kind {
    fn check(self) -> CheckId {
        match self {
            Kind::UnappliedRedaction | Kind::UnderBox | Kind::UnderImage => CheckId::FakeRedactions,
            _ => CheckId::HiddenText,
        }
    }

    fn id(self) -> &'static str {
        match self {
            Kind::OutsidePage => "outside_page",
            Kind::Tiny => "tiny",
            Kind::Clipped => "clipped",
            Kind::UnappliedRedaction => "unapplied_redaction",
            Kind::UnderBox => "text_under_box",
            Kind::UnderImage => "text_under_image",
            Kind::UnderPageImage => "text_under_page_image",
        }
    }
}

/// The share of `a` that `b` covers.
fn covered(a: Rect, b: Rect) -> f64 {
    let i = a.intersect(b);
    if a.area() <= 0.0 || i.width() <= 0.0 || i.height() <= 0.0 {
        return 0.0;
    }
    i.area() / a.area()
}

fn overlaps(a: Rect, b: Rect) -> bool {
    a.x0 < b.x1 && b.x0 < a.x1 && a.y0 < b.y1 && b.y0 < a.y1
}

/// The first rule that holds for `g`, in the spec's order.
fn classify(g: &Painted, rec: &Recorder, crop: Rect, redactions: &[Rect]) -> Outcome {
    if g.blank || g.text.chars().all(char::is_whitespace) {
        return Outcome::Blank;
    }
    if !overlaps(g.ink, crop) {
        return Outcome::Hidden(Kind::OutsidePage, None);
    }
    if g.size < 1.0 {
        return Outcome::Hidden(Kind::Tiny, None);
    }
    if g.clip.is_some_and(|c| !overlaps(g.ink, c)) {
        return Outcome::Hidden(Kind::Clipped, None);
    }
    if redactions.iter().any(|r| covered(g.ink, *r) >= 0.5) {
        return Outcome::Hidden(Kind::UnappliedRedaction, None);
    }
    let later = rec
        .covers
        .iter()
        .enumerate()
        .filter(|(_, c)| c.order > g.order && c.opaque && covered(g.ink, c.area) >= 0.8);
    for (i, cover) in later {
        match cover.luminance {
            Some(_) => return Outcome::Hidden(Kind::UnderBox, Some(i)),
            None if g.invisible => {}
            None if covered(crop, cover.area) >= 0.85 => {
                return Outcome::Hidden(Kind::UnderPageImage, Some(i));
            }
            None => return Outcome::Hidden(Kind::UnderImage, Some(i)),
        }
    }
    Outcome::Unknown
}

/// The /Redact marks of a page, as boxes in default user space.
fn redactions(c: &Ctx, page: &lopdf::Dictionary) -> Vec<Rect> {
    let numbers = |o: &[lopdf::Object]| -> Vec<f64> {
        o.iter()
            .filter_map(|n| n.as_float().ok().map(f64::from))
            .collect()
    };
    let mut marks = vec![];
    for (_, a) in c.annots(page) {
        if subtype(a) != b"Redact" {
            continue;
        }
        let quads = numbers(c.array(a, b"QuadPoints"));
        let rect = numbers(c.array(a, b"Rect"));
        let boxes: Vec<_> = quads
            .as_chunks::<8>()
            .0
            .iter()
            .map(|q| bounds(&q[..]))
            .collect();
        match (boxes.is_empty(), rect.as_slice()) {
            (false, _) => marks.extend(boxes),
            (true, [_, _, _, _]) => marks.push(bounds(&rect)),
            _ => {}
        }
    }
    marks
}

/// The box around points given as x, y pairs.
fn bounds(xy: &[f64]) -> Rect {
    let xs = xy.iter().step_by(2);
    let ys = xy.iter().skip(1).step_by(2);
    let (x0, x1) = xs.fold((f64::MAX, f64::MIN), |(a, b), &x| (a.min(x), b.max(x)));
    let (y0, y1) = ys.fold((f64::MAX, f64::MIN), |(a, b), &y| (a.min(y), b.max(y)));
    Rect::new(x0, y0, x1, y1)
}

/// Groups consecutive glyphs of one hidden kind (and one cover) into findings.
fn report(c: &Ctx, page: usize, rec: &Recorder, outcomes: &[Outcome], found: &mut Found) {
    let (glyphs, covers) = (&rec.glyphs, &rec.covers);
    let mut i = 0;
    while i < glyphs.len() {
        let Outcome::Hidden(kind, cover) = outcomes[i] else {
            i += 1;
            continue;
        };
        let mut end = i + 1;
        let mut last = i;
        while end < glyphs.len() {
            match outcomes[end] {
                Outcome::Blank => {}
                o if o == outcomes[i] => last = end,
                _ => break,
            }
            end += 1;
        }
        let group = &glyphs[i..=last];
        let hidden: Vec<_> = group
            .iter()
            .zip(&outcomes[i..=last])
            .filter(|(_, o)| **o != Outcome::Blank)
            .map(|(g, _)| g)
            .collect();
        let count: usize = hidden
            .iter()
            .map(|g| g.text.chars().filter(|c| !c.is_whitespace()).count())
            .sum();
        let letters = hidden
            .iter()
            .flat_map(|g| g.text.chars())
            .filter(|c| c.is_alphanumeric())
            .count();
        if letters >= 1 {
            let bbox = hidden
                .iter()
                .map(|g| g.ink)
                .reduce(|a, b| a.union(b))
                .unwrap_or_default();
            let detail = match kind {
                Kind::OutsidePage => n(
                    count,
                    "character outside the visible page",
                    "characters outside the visible page",
                ),
                Kind::Tiny => n(
                    count,
                    "character smaller than 1 point",
                    "characters smaller than 1 point",
                ),
                Kind::Clipped => n(
                    count,
                    "character hidden by a clipping path",
                    "characters hidden by a clipping path",
                ),
                Kind::UnappliedRedaction => format!(
                    "{} under 1 redaction mark that was never applied",
                    n(count, "character", "characters")
                ),
                Kind::UnderBox => {
                    let what = match cover.and_then(|i| covers[i].luminance) {
                        Some(l) if l < 0.1 => "a black box",
                        Some(l) if l > 0.9 => "a white box",
                        _ => "a box",
                    };
                    format!("{} under {what}", n(count, "character", "characters"))
                }
                Kind::UnderImage => {
                    format!("{} under a picture", n(count, "character", "characters"))
                }
                Kind::UnderPageImage => format!(
                    "{} under a picture that covers the page",
                    n(count, "character", "characters")
                ),
            };
            let text: String = group.iter().map(|g| g.text.as_str()).collect();
            found.findings.push(c.finding(
                (kind.check(), kind.id()),
                Where {
                    bbox: Some(rounded(bbox)),
                    ..Where::page(page)
                },
                (count, "characters"),
                detail,
                Some(text.trim().to_string()),
            ));
        }
        i = end;
    }
}

fn rounded(r: Rect) -> [f64; 4] {
    [r.x0, r.y0, r.x1, r.y1].map(|v| (v * 10.0).round() / 10.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shares_and_luminance() {
        let unit = Rect::new(0.0, 0.0, 10.0, 10.0);
        assert_eq!(covered(unit, Rect::new(0.0, 0.0, 10.0, 8.0)), 0.8);
        assert_eq!(covered(unit, Rect::new(10.0, 0.0, 20.0, 10.0)), 0.0);
        assert_eq!(covered(Rect::ZERO, unit), 0.0);
        assert!(luminance([0.0; 3]) < 0.001 && luminance([1.0; 3]) > 0.999);
        assert!((luminance([0.5; 3]) - 0.214).abs() < 0.001);
        assert_eq!(
            bounds(&[3.0, 9.0, 1.0, 2.0, 5.0, 4.0]),
            Rect::new(1.0, 2.0, 5.0, 9.0)
        );
    }
}
