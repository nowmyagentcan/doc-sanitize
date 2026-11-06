//! Colours as Office writes them, and whether text stands out from what is behind it.

use quick_xml::events::Event;

use super::Package;
use crate::report::Error;

pub type Rgb = [u8; 3];

pub const WHITE: Rgb = [255, 255, 255];
pub const BLACK: Rgb = [0, 0, 0];

/// `RRGGBB`, or `AARRGGBB` with the alpha ignored.
pub fn hex(s: &str) -> Option<Rgb> {
    let s = s.get(s.len().checked_sub(6)?..)?;
    if s.len() != 6 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&s[i..i + 2], 16).ok();
    Some([byte(0)?, byte(2)?, byte(4)?])
}

/// WCAG relative luminance, 0 (black) to 1 (white).
pub fn luminance(c: Rgb) -> f64 {
    let channel = |v: u8| {
        let v = f64::from(v) / 255.0;
        if v <= 0.039_28 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(c[0]) + 0.7152 * channel(c[1]) + 0.0722 * channel(c[2])
}

/// Whether text in `fg` can't be told from `bg`: a WCAG contrast ratio under 1.25.
pub fn same(fg: Rgb, bg: Rgb) -> bool {
    let (a, b) = (luminance(fg), luminance(bg));
    (a.max(b) + 0.05) / (a.min(b) + 0.05) < 1.25
}

/// Whether a background is dark: relative luminance under 0.4.
pub fn dark(bg: Rgb) -> bool {
    luminance(bg) < 0.4
}

/// `c` with its HSL lightness scaled by `keep` and moved towards white by `add`
/// (`L' = L × keep + add`), as Office applies tints and shades.
pub fn lighten(c: Rgb, keep: f64, add: f64) -> Rgb {
    let [r, g, b] = c.map(|v| f64::from(v) / 255.0);
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let l = (max + min) / 2.0;
    let d = max - min;
    let s = if d == 0.0 {
        0.0
    } else {
        d / (1.0 - (2.0 * l - 1.0).abs())
    };
    let h = if d == 0.0 {
        0.0
    } else if max == r {
        ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    let l = (l * keep + add).clamp(0.0, 1.0);
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - (h.rem_euclid(2.0) - 1.0).abs());
    let (r, g, b) = match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    [r, g, b].map(|v| ((v + m) * 255.0).round() as u8)
}

/// An Excel tint from -1 (black) to 1 (white).
pub fn tint(c: Rgb, t: f64) -> Rgb {
    if t < 0.0 {
        lighten(c, 1.0 + t, 0.0)
    } else {
        lighten(c, 1.0 - t, t)
    }
}

const DRAWING: &str = "http://schemas.openxmlformats.org/drawingml/2006/main";

/// The theme's colour scheme in its file order: dk1, lt1, dk2, lt2, accent1–6, hlink,
/// folHlink.
#[derive(Debug, Default)]
pub struct Theme(pub Vec<Option<Rgb>>);

const SCHEME: [&str; 12] = [
    "dk1", "lt1", "dk2", "lt2", "accent1", "accent2", "accent3", "accent4", "accent5", "accent6",
    "hlink", "folHlink",
];

impl Theme {
    /// Reads the first theme part the package links to, if any.
    pub fn read(pkg: &Package) -> Result<Self, Error> {
        let mut theme = Self(vec![None; SCHEME.len()]);
        let Some(part) = super::common::targets(pkg, "theme").first().copied() else {
            return Ok(theme);
        };
        let mut slot = None;
        pkg.walk(part, |r, ev| {
            if let Event::Start(e) | Event::Empty(e) = ev
                && r.within(DRAWING, "clrScheme")
            {
                let local = e.local_name();
                if let Some(i) = SCHEME.iter().position(|s| *s == local.as_ref()) {
                    slot = Some(i);
                } else if let Some(i) = slot {
                    let value = match local.as_ref() {
                        "srgbClr" => r.attr(e, "val")?,
                        "sysClr" => r.attr(e, "lastClr")?,
                        _ => None,
                    };
                    if let Some(c) = value.as_deref().and_then(hex) {
                        theme.0[i].get_or_insert(c);
                    }
                }
            }
            Ok(())
        })?;
        Ok(theme)
    }

    /// A colour by the name Word gives it.
    pub fn named(&self, name: &str) -> Option<Rgb> {
        let slot = match name {
            "dark1" | "text1" | "tx1" => "dk1",
            "light1" | "background1" | "bg1" => "lt1",
            "dark2" | "text2" | "tx2" => "dk2",
            "light2" | "background2" | "bg2" => "lt2",
            "hyperlink" => "hlink",
            "followedHyperlink" => "folHlink",
            n => n,
        };
        self.0[SCHEME.iter().position(|s| *s == slot)?]
    }

    /// A colour by Excel's theme index: lt1, dk1, lt2, dk2, then the scheme's order.
    pub fn indexed(&self, i: usize) -> Option<Rgb> {
        let slot = match i {
            0 => 1,
            1 => 0,
            2 => 3,
            3 => 2,
            i => i,
        };
        *self.0.get(slot)?
    }
}

/// Word's highlight colours by name.
pub fn highlight(name: &str) -> Option<Rgb> {
    let hex = match name {
        "black" => 0x000000,
        "blue" => 0x0000FF,
        "cyan" => 0x00FFFF,
        "green" => 0x00FF00,
        "magenta" => 0xFF00FF,
        "red" => 0xFF0000,
        "yellow" => 0xFFFF00,
        "white" => 0xFFFFFF,
        "darkBlue" => 0x000080,
        "darkCyan" => 0x008080,
        "darkGreen" => 0x008000,
        "darkMagenta" => 0x800080,
        "darkRed" => 0x800000,
        "darkYellow" => 0x808000,
        "darkGray" => 0x808080,
        "lightGray" => 0xC0C0C0,
        _ => return None,
    };
    Some(rgb(hex))
}

fn rgb(hex: u32) -> Rgb {
    let [_, r, g, b] = hex.to_be_bytes();
    [r, g, b]
}

/// Excel's default indexed palette; 64 and 65 are the system text and window colours.
pub fn palette(i: usize) -> Option<Rgb> {
    const PALETTE: [u32; 66] = [
        0x000000, 0xFFFFFF, 0xFF0000, 0x00FF00, 0x0000FF, 0xFFFF00, 0xFF00FF, 0x00FFFF, 0x000000,
        0xFFFFFF, 0xFF0000, 0x00FF00, 0x0000FF, 0xFFFF00, 0xFF00FF, 0x00FFFF, 0x800000, 0x008000,
        0x000080, 0x808000, 0x800080, 0x008080, 0xC0C0C0, 0x808080, 0x9999FF, 0x993366, 0xFFFFCC,
        0xCCFFFF, 0x660066, 0xFF8080, 0x0066CC, 0xCCCCFF, 0x000080, 0xFF00FF, 0xFFFF00, 0x00FFFF,
        0x800080, 0x800000, 0x008080, 0x0000FF, 0x00CCFF, 0xCCFFFF, 0xCCFFCC, 0xFFFF99, 0x99CCFF,
        0xFF99CC, 0xCC99FF, 0xFFCC99, 0x3366FF, 0x33CCCC, 0x99CC00, 0xFFCC00, 0xFF9900, 0xFF6600,
        0x666699, 0x969696, 0x003366, 0x339966, 0x003300, 0x333300, 0x993300, 0x993366, 0x333399,
        0x333333, 0x000000, 0xFFFFFF,
    ];
    PALETTE.get(i).map(|&h| rgb(h))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_colours_read_with_or_without_alpha() {
        assert_eq!(hex("FF0080"), Some([255, 0, 128]));
        assert_eq!(hex("ffFF0080"), Some([255, 0, 128]));
        assert_eq!(hex("auto"), None);
        assert_eq!(hex("12345"), None);
    }

    #[test]
    fn contrast_follows_wcag() {
        assert!((luminance(WHITE) - 1.0).abs() < 1e-9 && luminance(BLACK) == 0.0);
        assert!(same(WHITE, WHITE) && same(BLACK, [20, 20, 20]));
        assert!(same([250, 250, 250], WHITE));
        assert!(!same(BLACK, WHITE) && !same([128, 128, 128], WHITE));
        assert!(dark(BLACK) && dark([0, 0, 128]) && !dark(WHITE) && !dark([255, 255, 0]));
    }

    #[test]
    fn tints_move_lightness_towards_white_or_black() {
        assert_eq!(tint(BLACK, 0.5), [128, 128, 128]);
        assert_eq!(tint(WHITE, -0.5), [128, 128, 128]);
        assert_eq!(tint([255, 0, 0], 0.0), [255, 0, 0]);
        assert_eq!(tint([255, 0, 0], -1.0), BLACK);
        assert_eq!(tint([68, 114, 196], 0.799_981_688_894_314), [218, 227, 243]);
    }

    #[test]
    fn palettes_and_names_resolve() {
        assert_eq!(palette(64), Some(BLACK));
        assert_eq!(palette(2), Some([255, 0, 0]));
        assert_eq!(palette(66), None);
        assert_eq!(highlight("darkBlue"), Some([0, 0, 128]));
        assert_eq!(highlight("none"), None);
        let theme = Theme(vec![Some(BLACK), Some(WHITE), None, None, Some([1, 2, 3])]);
        assert_eq!(theme.named("background1"), Some(WHITE));
        assert_eq!(theme.named("accent1"), Some([1, 2, 3]));
        assert_eq!(theme.indexed(0), Some(WHITE));
        assert_eq!(theme.indexed(1), Some(BLACK));
        assert_eq!(theme.indexed(4), Some([1, 2, 3]));
    }
}
