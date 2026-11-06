//! The app's logo: the head on a near-black tile, as the settings window's
//! sidebar and header show it. Drawn from the outlines in
//! `poni_core::brand`; a placeholder until Motchy's own mascot.

use crate::gfx::Canvas;
use poni_core::brand;
use poni_core::geom::{Color, Point, Rect};
use poni_core::svgpath::{self, Seg};

const ORANGE: Color = Color::hex(brand::ORANGE);
const FOLD: Color = Color::hex(brand::FOLD_ORANGE);
const INK: Color = Color::hex(brand::INK);
const ICON_TILE: Color = Color::hex(0x171717);
/// The logo fits a 104-unit-wide box whose middle is 12 units above its origin.
const MARK_W: f32 = 104.0;

/// The logo's `data`, mapped so its (0, 0) sits at `origin` with `k` DIPs per unit.
fn segs(data: &str, origin: Point, k: f32) -> Vec<Seg> {
    let m = |x: f32, y: f32| (origin.x + x * k, origin.y + y * k);
    svgpath::parse(data)
        .unwrap_or_default()
        .into_iter()
        .map(|s| match s {
            Seg::MoveTo(x, y) => {
                let (x, y) = m(x, y);
                Seg::MoveTo(x, y)
            }
            Seg::LineTo(x, y) => {
                let (x, y) = m(x, y);
                Seg::LineTo(x, y)
            }
            Seg::QuadTo(a, b, x, y) => {
                let ((a, b), (x, y)) = (m(a, b), m(x, y));
                Seg::QuadTo(a, b, x, y)
            }
            Seg::CubicTo(a, b, c, d, x, y) => {
                let ((a, b), (c, d), (x, y)) = (m(a, b), m(c, d), m(x, y));
                Seg::CubicTo(a, b, c, d, x, y)
            }
            Seg::ArcTo(rx, ry, rot, large, sweep, x, y) => {
                let (x, y) = m(x, y);
                Seg::ArcTo(rx * k, ry * k, rot, large, sweep, x, y)
            }
            Seg::Close => Seg::Close,
        })
        .collect()
}

/// The app icon, `size` DIPs square and centred on `center`: the head on the
/// near-black tile with phone-style rounded corners. Its own colours in every theme.
pub fn app_icon(c: &mut Canvas, center: Point, size: f32) {
    let tile = Rect::new(center.x - size / 2.0, center.y - size / 2.0, size, size);
    let w = 0.617 * size;
    let k = w / MARK_W;
    let origin = Point::new(center.x, center.y + 0.013 * size + 12.0 * k);
    // The small cut's eyes at 32 px and below, as the logo does.
    let eyes = if w * c.scale() <= 32.0 { brand::EYES_SMALL } else { brand::EYES };
    c.unthemed(|c| {
        c.fill_rrect(tile, size * 0.2237, ICON_TILE);
        c.path(&segs(brand::HEAD, origin, k), Some(ORANGE), None);
        c.path(&segs(brand::FOLD, origin, k), Some(FOLD), None);
        c.path(&segs(eyes, origin, k), Some(INK), None);
    });
}
