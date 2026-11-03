//! Pages turned into pictures: rendered at up to 200 DPI with their annotations, /Redact
//! marks painted in, and encoded as an image that replaces the page's content.

use std::io::Write;

use flate2::Compression;
use flate2::write::ZlibEncoder;
use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_syntax::Pdf as Pages;
use hayro::hayro_syntax::page::{Page, Rotation};
use hayro::kurbo::{Affine, Rect};
use hayro::vello_cpu::color::palette::css::WHITE;
use hayro::{PixmapSettings, RenderCache, RenderSettings};
use image::codecs::jpeg::JpegEncoder;

use super::Ctx;
use super::visible::mark_boxes;
use hayro::hayro_interpret::TransformExt;

const DPI: f64 = 200.0;
const MIN_DPI: f64 = 72.0;
const MAX_PIXELS: f64 = 40e6;
const MAX_SIDE: f64 = 65_535.0;
/// Past this size a Flate picture is compared with a JPEG one.
const FLATE_LIMIT: usize = 1_500_000;

/// One page as a picture.
pub struct Drawn {
    pub width: u32,
    pub height: u32,
    /// The image stream's data and filter name.
    pub data: Vec<u8>,
    pub filter: &'static str,
    /// The page's box in default user space, as shown (rotation applied to its size).
    pub bounds: [f64; 4],
    pub rotated: bool,
    /// /Redact marks painted, and whether each was black.
    pub redactions: Vec<bool>,
}

/// Draws one page of `pages` (the document as saved for drawing, without its marks), with
/// the /Redact marks of `dict` (the page in the document being cleaned) painted in.
/// None when it can't reach 72 DPI within the pixel limits.
pub fn draw<'a>(
    c: &Ctx,
    pages: &'a [Page<'a>],
    cache: &RenderCache<'a>,
    number: usize,
    dict: &lopdf::Dictionary,
) -> Option<Drawn> {
    let page = pages.get(number - 1)?;
    let (w, h) = page.render_dimensions();
    let (w, h) = (f64::from(w), f64::from(h));
    let scale = (DPI / 72.0)
        .min((MAX_PIXELS / (w * h)).sqrt())
        .min(MAX_SIDE / w)
        .min(MAX_SIDE / h);
    if scale * 72.0 < MIN_DPI {
        return None;
    }
    let pixmap = PixmapSettings {
        x_scale: scale as f32,
        y_scale: scale as f32,
        bg_color: WHITE,
    };
    let drawn = hayro::render(
        page,
        cache,
        &InterpreterSettings::default(),
        &RenderSettings::default(),
        &pixmap,
    );
    let (width, height) = (usize::from(drawn.width()), usize::from(drawn.height()));
    let mut rgb: Vec<u8> = drawn.data().iter().flat_map(|p| [p.r, p.g, p.b]).collect();
    drop(drawn);
    let map = Affine::scale(scale) * page.initial_transform(true).to_kurbo();
    let mut redactions = vec![];
    for (_, a) in c.annots(dict) {
        if super::subtype(a) != b"Redact" {
            continue;
        }
        let colour = mark_colour(c, a);
        for mark in mark_boxes(c, a) {
            fill(
                &mut rgb,
                width,
                height,
                map.transform_rect_bbox(mark),
                colour,
            );
        }
        redactions.push(colour == [0; 3]);
    }
    let (data, filter) = encode(&rgb, width as u32, height as u32);
    let crop = page.intersected_crop_box();
    Some(Drawn {
        width: width as u32,
        height: height as u32,
        data,
        filter,
        bounds: [crop.x0, crop.y0, crop.x0 + w, crop.y0 + h],
        rotated: !matches!(page.rotation(), Rotation::None),
        redactions,
    })
}

/// The pages of the document saved as `bytes`.
pub fn open(bytes: Vec<u8>) -> Option<Pages> {
    Pages::new(bytes).ok()
}

/// A /Redact mark's /IC colour; black when it has none.
fn mark_colour(c: &Ctx, a: &lopdf::Dictionary) -> [u8; 3] {
    let n: Vec<f32> = c
        .array(a, b"IC")
        .iter()
        .filter_map(|n| n.as_float().ok())
        .collect();
    let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    match n[..] {
        [g] => [byte(g); 3],
        [r, g, b] => [byte(r), byte(g), byte(b)],
        [c, m, y, k] => [c, m, y].map(|v| byte((1.0 - v) * (1.0 - k))),
        _ => [0; 3],
    }
}

/// Fills the pixels `r` touches.
fn fill(rgb: &mut [u8], width: usize, height: usize, r: Rect, colour: [u8; 3]) {
    let clamp = |v: f64, max: usize| (v.max(0.0) as usize).min(max);
    let (x0, x1) = (clamp(r.x0.floor(), width), clamp(r.x1.ceil(), width));
    let (y0, y1) = (clamp(r.y0.floor(), height), clamp(r.y1.ceil(), height));
    for y in y0..y1 {
        for px in rgb[(y * width + x0) * 3..(y * width + x1) * 3]
            .as_chunks_mut::<3>()
            .0
        {
            px.copy_from_slice(&colour);
        }
    }
}

/// Flate-compressed RGB; past 1.5 MB, a JPEG at quality 90 when that is smaller.
fn encode(rgb: &[u8], width: u32, height: u32) -> (Vec<u8>, &'static str) {
    let mut z = ZlibEncoder::new(vec![], Compression::new(6));
    let flate = z
        .write_all(rgb)
        .and_then(|()| z.finish())
        .unwrap_or_default();
    if flate.len() > FLATE_LIMIT {
        let mut jpeg = vec![];
        let done = JpegEncoder::new_with_quality(&mut jpeg, 90).encode(
            rgb,
            width,
            height,
            image::ExtendedColorType::Rgb8,
        );
        if done.is_ok() && jpeg.len() < flate.len() {
            return (jpeg, "DCTDecode");
        }
    }
    (flate, "FlateDecode")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marks_fill_the_pixels_they_touch_and_stay_on_the_page() {
        let mut rgb = vec![255; 4 * 3 * 3];
        fill(&mut rgb, 4, 3, Rect::new(0.5, 0.5, 2.5, 1.5), [1, 2, 3]);
        let at = |x: usize, y: usize| rgb[(y * 4 + x) * 3];
        assert_eq!((at(0, 0), at(2, 1), at(3, 1), at(0, 2)), (1, 1, 255, 255));
        fill(&mut rgb, 4, 3, Rect::new(-9.0, -9.0, 99.0, 99.0), [7; 3]);
        assert!(rgb.iter().all(|&b| b == 7));
    }

    #[test]
    fn busy_pictures_become_jpegs_and_flat_ones_stay_flate() {
        let flat = vec![255; 600 * 600 * 3];
        assert_eq!(encode(&flat, 600, 600).1, "FlateDecode");
        let mut x: u32 = 1;
        let busy: Vec<u8> = (0..1000 * 1000 * 3)
            .map(|_| {
                x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (x >> 24) as u8
            })
            .collect();
        assert_eq!(encode(&busy, 1000, 1000).1, "DCTDecode");
    }
}
