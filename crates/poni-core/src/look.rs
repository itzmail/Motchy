//! How the island paints in each theme: its ground, its ink and its accents.
//!
//! Pages and activities draw in one vocabulary, the Classic island's: a black
//! ground, white content at some opacity (text, icons, washes, tracks) and a
//! few system-style accents (`geom::palette`). A [`Look`] maps that vocabulary
//! onto its own colours as the canvas paints, so every page follows the theme
//! without knowing about it, including pages written later.
//!
//! * Greys (white, black and everything between, at any opacity) are read as
//!   "white at that brightness over the black island" and repainted between
//!   the look's ground and ink: Classic's white text becomes the ink, its 8 %
//!   white wash becomes an 8 % ink wash, the black island becomes the ground.
//! * The palette's accents swap for the look's own, with a deeper shade for
//!   text so small coloured labels stay readable on a light ground.
//! * Anything else (album-art colours, Oyen's own face, `palette::ON_ACCENT`)
//!   is left alone.
//!
//! A look also says what the island is made of: Classic and Oyen paint a
//! solid ground, Glass draws liquid glass there (gfx and the glass shader)
//! and paints content in the light or dark Glass look on top. And its
//! shape: every theme hangs from the top edge like a notch, except Pill,
//! which floats below it as a capsule (`crate::settings::Theme::Pill`).

use crate::geom::{palette, Color};
use crate::settings::Theme;

/// An accent and what a look paints instead: `fill` for shapes, `text` for
/// coloured text and icons.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Swap {
    pub from: Color,
    pub fill: Color,
    pub text: Color,
}

#[derive(Debug, PartialEq)]
pub struct Look {
    /// The island's fill: what Classic paints black.
    pub ground: Color,
    /// Content: what Classic paints white.
    pub ink: Color,
    /// The theme's own accent; `None` follows the Windows accent colour.
    pub accent: Option<Color>,
    /// Hairline around the island (Classic: a faint white edge on black).
    pub border: Color,
    /// Light ground with dark content.
    pub light: bool,
    /// Embedded font family the look writes in; `None` = the system UI font.
    pub font: Option<&'static str>,
    /// What the island's body is made of.
    pub material: Material,
    /// The island's outline.
    pub shape: Shape,
    swaps: &'static [Swap],
}

/// The island's body: a solid fill in `ground`, or liquid glass (with
/// `ground` as its nominal tint, used where the glass can't see the screen).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Material {
    Solid,
    Glass,
}

/// The island's outline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    /// Hangs from the top edge: a flat top flush with the screen, concave
    /// shoulders flaring into the edge, rounded bottom corners.
    Notch,
    /// Floats a little below the top edge, every corner rounded: a capsule
    /// while it rests, a card while it's open.
    Pill,
}

pub const CLASSIC: Look = Look {
    ground: Color::black(1.0),
    ink: Color::white(1.0),
    accent: None,
    border: palette::BORDER,
    light: false,
    font: None,
    material: Material::Solid,
    shape: Shape::Notch,
    swaps: &[],
};

/// Glass over a dark backdrop (and in Windows dark mode): white ink, the
/// palette's own accents (made for dark grounds), the system UI font.
pub const GLASS_DARK: Look = Look {
    ground: Color::hex(0x1C1C1E),
    ink: Color::white(1.0),
    accent: None,
    border: Color::white(0.10),
    light: false,
    font: None,
    material: Material::Glass,
    shape: Shape::Notch,
    swaps: &[],
};

/// Glass over a light backdrop (and in Windows light mode): near-black ink,
/// and accents in their light-mode shades, deeper still for text.
pub const GLASS_LIGHT: Look = Look {
    ground: Color::hex(0xF2F2F7),
    ink: Color::hex(0x1D1D1F),
    accent: None,
    border: Color::black(0.08),
    light: true,
    font: None,
    material: Material::Glass,
    shape: Shape::Notch,
    swaps: &[
        Swap { from: palette::GREEN, fill: Color::hex(0x34C759), text: Color::hex(0x248A3D) },
        Swap { from: palette::ORANGE, fill: Color::hex(0xFF9500), text: Color::hex(0xC93400) },
        Swap { from: palette::RED, fill: Color::hex(0xFF3B30), text: Color::hex(0xD70015) },
        Swap { from: palette::BLUE, fill: Color::hex(0x007AFF), text: Color::hex(0x0040DD) },
        Swap { from: palette::CYAN, fill: Color::hex(0x32ADE6), text: Color::hex(0x0071A4) },
        Swap { from: palette::PURPLE, fill: Color::hex(0xAF52DE), text: Color::hex(0x8944AB) },
        Swap { from: palette::YELLOW, fill: Color::hex(0xFFCC00), text: Color::hex(0xA05A00) },
        Swap { from: palette::PINK, fill: Color::hex(0xFF2D55), text: Color::hex(0xD30F45) },
    ],
};

/// Pill: Classic's black and white (pages paint exactly as they ask) in a
/// floating capsule, with a faint rim (the spec's white at 0.10, a little
/// brighter than Classic's edge) so it holds its shape on a dark screen.
pub const PILL: Look = Look {
    ground: Color::black(1.0),
    ink: Color::white(1.0),
    accent: None,
    border: Color::white(0.10),
    light: false,
    font: None,
    material: Material::Solid,
    shape: Shape::Pill,
    swaps: &[],
};

/// The theme's look. Glass starts dark; the app picks [`glass`] light or
/// dark from what's behind the island and the Windows mode.
pub fn of(theme: Theme) -> &'static Look {
    match theme {
        Theme::Classic => &CLASSIC,
        Theme::Glass => &GLASS_DARK,
        Theme::Pill => &PILL,
    }
}

/// The Glass look for a light (`true`) or dark backdrop.
pub fn glass(light: bool) -> &'static Look {
    if light {
        &GLASS_LIGHT
    } else {
        &GLASS_DARK
    }
}

impl Look {
    /// The colour to paint for a shape the page asked to fill or stroke in `c`.
    pub fn paint(&self, c: Color) -> Color {
        self.map(c, false)
    }

    /// The colour to paint for text (and icons) the page asked for in `c`.
    pub fn paint_text(&self, c: Color) -> Color {
        self.map(c, true)
    }

    fn map(&self, c: Color, text: bool) -> Color {
        if self.swaps.is_empty() && self.ground == CLASSIC.ground && self.ink == CLASSIC.ink {
            return c;
        }
        if c.r == c.g && c.g == c.b {
            let base = match c.r {
                v if v <= 0.0 => self.ground,
                v if v >= 1.0 => self.ink,
                v => Color::lerp(self.ground, self.ink, v),
            };
            return base.with_alpha(c.a);
        }
        match self.swaps.iter().find(|s| same_rgb(s.from, c)) {
            Some(s) => (if text { s.text } else { s.fill }).with_alpha(c.a),
            None => c,
        }
    }
}

fn same_rgb(a: Color, b: Color) -> bool {
    (a.r - b.r).abs() < 1e-4 && (a.g - b.g).abs() < 1e-4 && (a.b - b.b).abs() < 1e-4
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classic_paints_what_it_is_given() {
        for c in [Color::white(0.62), Color::black(1.0), palette::GREEN, Color::rgba(0.16, 0.16, 0.16, 1.0), Color::hex(0x123456)] {
            assert_eq!(CLASSIC.paint(c), c);
            assert_eq!(CLASSIC.paint_text(c), c);
        }
    }

    #[test]
    fn pill_floats_and_paints_like_classic() {
        assert_eq!(PILL.shape, Shape::Pill);
        assert_eq!(*of(Theme::Pill), PILL);
        for look in [&CLASSIC, &GLASS_DARK, &GLASS_LIGHT] {
            assert_eq!(look.shape, Shape::Notch);
        }
        // Black and white like Classic: what a page asks for is what it gets.
        for c in [Color::white(0.62), Color::black(1.0), palette::GREEN, Color::hex(0x123456)] {
            assert_eq!(PILL.paint(c), c);
            assert_eq!(PILL.paint_text(c), c);
        }
        assert_eq!(PILL.material, Material::Solid);
    }

    #[test]
    fn glass_is_glass_and_the_others_are_solid() {
        assert_eq!(CLASSIC.material, Material::Solid);
        assert_eq!(GLASS_DARK.material, Material::Glass);
        assert_eq!(GLASS_LIGHT.material, Material::Glass);
        assert_eq!(*of(Theme::Glass), GLASS_DARK);
        assert_eq!(*glass(true), GLASS_LIGHT);
        assert_eq!(*glass(false), GLASS_DARK);
    }

    #[test]
    fn light_glass_writes_dark_ink_and_deepens_accent_text() {
        assert_eq!(GLASS_LIGHT.paint(Color::white(1.0)), GLASS_LIGHT.ink);
        assert_eq!(GLASS_LIGHT.paint(palette::TEXT_DIM), GLASS_LIGHT.ink.with_alpha(0.62));
        // Black content (a glyph cut out of a white disc) takes the glass's ground.
        assert_eq!(GLASS_LIGHT.paint(Color::black(1.0)), GLASS_LIGHT.ground);
        let fill = GLASS_LIGHT.paint(palette::BLUE);
        let text = GLASS_LIGHT.paint_text(palette::BLUE);
        assert!(text.luminance() < fill.luminance());
        // Dark glass keeps the palette's accents, made for dark grounds.
        assert_eq!(GLASS_DARK.paint(palette::GREEN), palette::GREEN);
        assert_eq!(GLASS_DARK.paint(Color::white(0.5)), Color::white(0.5));
        assert_eq!(GLASS_DARK.paint_text(palette::ON_ACCENT), palette::ON_ACCENT);
    }
}
