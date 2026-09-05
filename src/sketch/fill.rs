//! Stroke (pen) generation for [`Fill::Sketch`] — consumed by the raster backend.
//!
//! The SVG backend expresses the same fill as a `<pattern>` tile (see `render/svg.rs`); here we
//! need real geometry, because vello has no pattern concept.
//!
//! Two properties are shared with the SVG side so the two backends stay visually equivalent:
//! - **Anchoring**: the hachure lattice is anchored at the scene origin (`k · gap + gap/2` along
//!   the offset axis), exactly like `patternUnits="userSpaceOnUse"` tiles are. Phase therefore
//!   agrees between PNG and SVG.
//! - **Parameters**: direction, spacing and weight are read straight from [`SketchFill`].
//!
//! Lines are generated over the shape's bounding box and **clipped by the caller**; they
//! deliberately overshoot so a rotated family still covers the corners.

use crate::geometry::{Point, Rect, Vec2};
use crate::scene::{SketchFill, SketchFillStyle};

/// One pen stroke (a hachure line is two points, a zig-zag is many).
pub(crate) type Stroke = Vec<Point>;

/// Last-resort stroke cap for the raster backend.
///
/// The normal protection lives in the sketch pass: it floors the gap per element using
/// [`crate::sketch::SketchOptions::max_fill_strokes`] and the element's size, so the IR already
/// carries a sane gap. This constant only guards against pathological input that bypasses the
/// pass (a hand-built `Fill::Sketch` with a tiny gap on a huge shape).
pub(crate) const HARD_CAP: usize = 20_000;

/// Is this style drawn as strokes (as opposed to dots)?
pub(crate) fn is_stroke_style(sf: &SketchFill) -> bool {
    !matches!(sf.style, SketchFillStyle::Dots)
}

/// Generate the hachure / cross-hatch / zig-zag strokes covering `bbox`.
///
/// `max` is the hard cap from [`crate::sketch::SketchOptions::max_fill_strokes`]: when the
/// requested gap would produce more strokes, the gap is coarsened instead of flooding the
/// rasterizer (a 400×400 heat-map cell at gap 4 is 100 lines, ×1000 cells is not acceptable).
pub(crate) fn strokes(bbox: Rect, sf: &SketchFill, max: usize) -> Vec<Stroke> {
    let angles = match sf.style {
        SketchFillStyle::Hachure | SketchFillStyle::Zigzag => vec![sf.angle],
        SketchFillStyle::CrossHatch => vec![sf.angle, sf.angle + 90.0],
        SketchFillStyle::Dots => Vec::new(),
    };
    let mut out = Vec::new();
    for a in angles {
        out.extend(parallel_family(
            bbox,
            a.to_radians(),
            sf.gap(),
            matches!(sf.style, SketchFillStyle::Zigzag),
            max,
        ));
    }
    out
}

/// Dot grid for [`SketchFillStyle::Dots`]: lattice points plus the radius to draw them with.
pub(crate) fn dots(bbox: Rect, sf: &SketchFill, max: usize) -> (Vec<Point>, f64) {
    let a = sf.angle.to_radians();
    let (u, v) = basis(a);
    let (u_lo, u_hi, v_lo, v_hi) = projections(bbox, u, v);
    let (k0, k1, gap_u) = index_range(v_lo, v_hi, sf.gap(), max);
    let (m0, m1, gap_v) = index_range(u_lo, u_hi, sf.gap(), max);
    let mut pts = Vec::new();
    for k in k0..=k1 {
        for m in m0..=m1 {
            pts.push(
                Point::ORIGIN
                    + v * (k as f64 * gap_v + gap_v * 0.5)
                    + u * (m as f64 * gap_u + gap_u * 0.5),
            );
        }
    }
    (pts, dot_radius(sf))
}

/// Dot radius: rough.js scales the dot with `fillWeight`, clamped into the gap so dots never
/// merge into a blob.
pub(crate) fn dot_radius(sf: &SketchFill) -> f64 {
    (sf.weight() * 1.5).clamp(0.35, sf.gap() * 0.4)
}

/// Unit direction of the lines (`u`) and the perpendicular offset axis (`v`).
fn basis(angle: f64) -> (Vec2, Vec2) {
    (
        Vec2::new(angle.cos(), angle.sin()),
        Vec2::new(-angle.sin(), angle.cos()),
    )
}

/// Projections of the bbox corners onto both axes.
fn projections(bbox: Rect, u: Vec2, v: Vec2) -> (f64, f64, f64, f64) {
    let mut u_lo = f64::MAX;
    let mut u_hi = f64::MIN;
    let mut v_lo = f64::MAX;
    let mut v_hi = f64::MIN;
    for c in [
        Point::new(bbox.min_x(), bbox.min_y()),
        Point::new(bbox.max_x(), bbox.min_y()),
        Point::new(bbox.max_x(), bbox.max_y()),
        Point::new(bbox.min_x(), bbox.max_y()),
    ] {
        let c = c.to_vec2();
        u_lo = u_lo.min(c.dot(u));
        u_hi = u_hi.max(c.dot(u));
        v_lo = v_lo.min(c.dot(v));
        v_hi = v_hi.max(c.dot(v));
    }
    (u_lo, u_hi, v_lo, v_hi)
}

/// Inclusive integer index range covering `[lo, hi]` at the given spacing, plus the (possibly
/// coarsened) spacing that keeps the count within `max`.
fn index_range(lo: f64, hi: f64, gap: f64, max: usize) -> (i64, i64, f64) {
    let max = max.max(1) as f64;
    let (mut gap, mut k0, mut k1) = (gap, 0.0f64, 0.0f64);
    // Coarsen until the count fits the budget. One step is not enough: integer ceil/floor on the
    // padded range can still overshoot by one, so iterate (bounded, always converging because the
    // gap at least doubles).
    for _ in 0..16 {
        let pad = gap;
        k0 = ((lo - pad) / gap).floor();
        k1 = ((hi + pad) / gap).ceil();
        let count = (k1 - k0).max(0.0);
        if count <= max {
            break;
        }
        gap *= (count / max).ceil().max(2.0);
    }
    (k0 as i64, k1 as i64, gap)
}

/// One family of parallel lines (or zig-zags) at `angle`, covering `bbox`.
fn parallel_family(bbox: Rect, angle: f64, gap: f64, zigzag: bool, max: usize) -> Vec<Stroke> {
    let (u, v) = basis(angle);
    let (u_lo, u_hi, v_lo, v_hi) = projections(bbox, u, v);
    let (k0, k1, gap) = index_range(v_lo, v_hi, gap, max);
    // Extend along the line direction so rotation still covers the corners (the clip trims it).
    let (a_lo, a_hi) = (u_lo - gap * 2.0, u_hi + gap * 2.0);
    let mut out = Vec::new();
    for k in k0..=k1 {
        let off = v * (k as f64 * gap + gap * 0.5);
        if !zigzag {
            out.push(vec![
                Point::ORIGIN + off + u * a_lo,
                Point::ORIGIN + off + u * a_hi,
            ]);
        } else {
            // Zig-zag: walk along the line, alternating sideways by ±gap/2 every half period.
            let mut pts = Vec::new();
            let mut i = 0;
            let mut t = a_lo;
            while t <= a_hi {
                let lateral = if i % 2 == 0 { -gap * 0.5 } else { gap * 0.5 };
                pts.push(Point::ORIGIN + off + u * t + v * lateral);
                t += gap * 0.5;
                i += 1;
            }
            if pts.len() >= 2 {
                out.push(pts);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sf(style: SketchFillStyle) -> SketchFill {
        SketchFill::new(crate::geometry::Color::BLACK).with_style(style)
    }

    fn bbox() -> Rect {
        Rect::new(0.0, 0.0, 100.0, 100.0)
    }

    #[test]
    fn hachure_spacing_matches_the_gap() {
        let s = strokes(bbox(), &sf(SketchFillStyle::Hachure), 10_000);
        // 100×100 的框在 -41° 方向上的投影宽 ≈ 141，gap 4 → 约 35~40 条。
        assert!(s.len() > 30 && s.len() < 50, "got {}", s.len());
        for line in &s {
            assert_eq!(line.len(), 2);
        }
    }

    #[test]
    fn cross_hatch_has_two_families() {
        let one = strokes(bbox(), &sf(SketchFillStyle::Hachure), 10_000).len();
        let two = strokes(bbox(), &sf(SketchFillStyle::CrossHatch), 10_000).len();
        assert!(
            two >= one * 2 - 2,
            "交叉排线应为两个方向: one={one} two={two}"
        );
    }

    #[test]
    fn zigzag_lines_have_many_points() {
        let s = strokes(bbox(), &sf(SketchFillStyle::Zigzag), 10_000);
        assert!(!s.is_empty());
        assert!(s.iter().all(|l| l.len() > 4), "折线应含多个点");
    }

    #[test]
    fn dots_style_emits_no_strokes() {
        assert!(strokes(bbox(), &sf(SketchFillStyle::Dots), 10_000).is_empty());
        let (pts, r) = dots(bbox(), &sf(SketchFillStyle::Dots), 10_000);
        assert!(!pts.is_empty());
        assert!(r > 0.0 && r < 4.0, "点半径应在合理范围, got {r}");
    }

    /// 元素数保护：超过上限时放粗间距，而不是生成上万条线。
    #[test]
    fn max_strokes_cap_coarsens_the_gap() {
        let s = strokes(bbox(), &sf(SketchFillStyle::Hachure), 8);
        assert!(s.len() <= 8, "got {}", s.len());
        let big = Rect::new(0.0, 0.0, 4000.0, 4000.0);
        let s2 = strokes(big, &sf(SketchFillStyle::Hachure), 8);
        assert!(s2.len() <= 8, "超大区域也必须被限流, got {}", s2.len());
    }

    #[test]
    fn degenerate_gap_is_guarded() {
        let bad = sf(SketchFillStyle::Hachure).with_gap(0.0);
        assert_eq!(bad.gap(), 4.0);
        assert!(bad.weight() > 0.0);
    }

    /// 网格锚定在原点（与 SVG `patternUnits="userSpaceOnUse"` 同一个语义）：
    /// 不同位置的形状落在同一条网格上，相位只由 gap 决定 → 两个后端的排线相位一致。
    #[test]
    fn lattice_is_anchored_at_the_origin() {
        let sf = sf(SketchFillStyle::Hachure);
        let (_, v) = basis(sf.angle.to_radians());
        let gap = sf.gap();
        for rect in [
            Rect::new(0.0, 0.0, 40.0, 40.0),
            Rect::new(8.0, 8.0, 48.0, 48.0),
            Rect::new(-33.0, 17.0, 61.0, 90.0),
        ] {
            for line in strokes(rect, &sf, HARD_CAP) {
                // 线 i 的偏移恒为 k·gap + gap/2 → 归一化后小数部分恒为 0.5。
                let t = line[0].to_vec2().dot(v) / gap;
                assert!((t.fract().abs() - 0.5).abs() < 1e-6, "相位未锚定: {t}");
            }
        }
    }
}
