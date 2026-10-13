//! Path operations: boolean combinations, stroke-to-path, arc-length measurement and
//! offset / inset.
//!
//! Every operation here is **build-time** and **IR-neutral**: it takes a [`BezPath`] and returns a
//! new [`BezPath`], never touching the scene graph ([`crate::scene`]) or a backend. Feed the result
//! to [`crate::builder::Ctx::path`] and the SVG and the raster backend render it identically —
//! there is nothing per-backend to keep in sync.
//!
//! ```
//! use lievisual::{BoolOp, PathOps, Rect, Shape};
//!
//! let a = Rect::new(0.0, 0.0, 10.0, 10.0).to_path(0.01);
//! let b = Rect::new(5.0, 5.0, 15.0, 15.0).to_path(0.01);
//! assert!((a.union(&b).area() - 175.0).abs() < 0.5);
//!
//! // Subtract an inner rect → a ring with a real hole (two contours).
//! let ring = a.difference(&Rect::new(3.0, 3.0, 7.0, 7.0).to_path(0.01));
//! assert!((ring.area() - 84.0).abs() < 0.5);
//! ```
//!
//! ## Boolean ops need a polygon engine
//!
//! kurbo 0.13 no longer ships `BooleanOps`, so [`PathOps::boolean`] delegates to
//! [`i_overlay`](https://docs.rs/i_overlay) (pure Rust, no `unsafe`, no threading by default).
//! Curves are **flattened** into polygons first (see `tolerance` / [`DEFAULT_TOLERANCE`]) and the
//! result is **polygonal**: a union of two circles comes back as a dense polyline, not as arcs.
//! Raise the tolerance to emit fewer, coarser nodes; lower it for smoother output.
//!
//! Filling uses the **non-zero** rule, which is also the default of both backends, so contours
//! that come back as holes (opposite winding) render as holes everywhere.
//!
//! ## Offset / inset
//!
//! [`PathOps::offset`] and [`PathOps::inset`] are the morphological **dilation** and **erosion**
//! by a disk (Minkowski sum / difference), built from stroke-to-path plus a boolean op. They treat
//! the path as a **filled region** (an open path is closed first) and they **round convex corners**,
//! exactly as a disk-shaped structuring element does. That is the usual "grow / shrink a shape"
//! semantic; for an exact constant-width band around a *curve* use [`PathOps::outline`] instead.

use crate::geometry::{BezPath, PathEl, PathSeg, Point, Vec2};
use crate::scene::Stroke;
use i_overlay::core::fill_rule::FillRule;
use i_overlay::core::overlay_rule::OverlayRule;
use i_overlay::float::simplify::SimplifyShape;
use i_overlay::float::single::SingleFloatOverlay;
use kurbo::{ParamCurve, ParamCurveArclen, ParamCurveDeriv};

/// Default flattening / stroking tolerance in user units, used by the convenience
/// [`PathOps::union`] and friends, and by the [`crate::builder`] path helpers.
///
/// `0.1` keeps the polygonal error well below a device pixel for typical (chart-sized) scenes
/// while staying cheap.
pub const DEFAULT_TOLERANCE: f64 = 0.1;

/// Absolute arc-length accuracy in user units (kurbo's `arclen` error bound). Not exposed: it is
/// far below any rendering-relevant resolution.
const ARCLEN_ACCURACY: f64 = 1e-6;

/// Which boolean combination [`PathOps::boolean`] should compute.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BoolOp {
    /// Everything covered by either path.
    #[default]
    Union,
    /// Only the area covered by **both** paths.
    Intersect,
    /// `self` **minus** `other`.
    Difference,
    /// The area covered by exactly one of the two paths (symmetric difference).
    Xor,
}

impl BoolOp {
    fn to_overlay_rule(self) -> OverlayRule {
        match self {
            BoolOp::Union => OverlayRule::Union,
            BoolOp::Intersect => OverlayRule::Intersect,
            BoolOp::Difference => OverlayRule::Difference,
            BoolOp::Xor => OverlayRule::Xor,
        }
    }
}

/// A position sampled along a path by [`PathOps::point_at_length`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PathSample {
    /// Position at the requested arc length (clamped to the path's extent).
    pub point: Point,
    /// **Unit** tangent — the direction of travel — at `point`.
    pub tangent: Vec2,
    /// The arc length actually sampled: `requested.clamp(0.0, path_length())`.
    pub distance: f64,
}

/// Path algorithms on any [`BezPath`]: boolean combinations, stroke-to-path, measurement, and
/// offset / inset.
///
/// Import the trait to use the methods (`use lievisual::PathOps;`).
pub trait PathOps {
    /// Boolean combination of `self` and `other` (see [`BoolOp`]).
    ///
    /// Curves are flattened at `tolerance`; the result is a polygonal, closed path using the
    /// non-zero fill rule.
    #[must_use]
    fn boolean(&self, other: &BezPath, op: BoolOp, tolerance: f64) -> BezPath;

    /// Union of `self` and `other` (flattened at [`DEFAULT_TOLERANCE`]).
    #[must_use]
    fn union(&self, other: &BezPath) -> BezPath {
        self.boolean(other, BoolOp::Union, DEFAULT_TOLERANCE)
    }

    /// Intersection of `self` and `other` (flattened at [`DEFAULT_TOLERANCE`]).
    #[must_use]
    fn intersect(&self, other: &BezPath) -> BezPath {
        self.boolean(other, BoolOp::Intersect, DEFAULT_TOLERANCE)
    }

    /// `self` minus `other` (flattened at [`DEFAULT_TOLERANCE`]) — the usual way to punch a hole
    /// or clip one shape by another.
    #[must_use]
    fn difference(&self, other: &BezPath) -> BezPath {
        self.boolean(other, BoolOp::Difference, DEFAULT_TOLERANCE)
    }

    /// Symmetric difference of `self` and `other` (flattened at [`DEFAULT_TOLERANCE`]).
    #[must_use]
    fn xor(&self, other: &BezPath) -> BezPath {
        self.boolean(other, BoolOp::Xor, DEFAULT_TOLERANCE)
    }

    /// Clean up `self`: resolve self-intersections and merge overlapping contours into a canonical
    /// non-zero filled region (the "self-union" of a messy path).
    #[must_use]
    fn normalize(&self, tolerance: f64) -> BezPath;

    /// **Stroke-to-path**: the filled outline of `self` stroked with `stroke`.
    ///
    /// Honours width, caps, joins, miter limit and dashes. The result is a closed region (fill it
    /// instead of stroking), which is how "hit the same geometry as the stroke" — hit-testing,
    /// clipping, boolean ops on a stroked shape — is expressed.
    ///
    /// Unlike [`PathOps::boolean`] this is not flattening-bound: kurbo emits real curves, so an
    /// outline stays smooth at any zoom.
    #[must_use]
    fn outline(&self, stroke: &Stroke, tolerance: f64) -> BezPath;

    /// Total arc length of the path (all sub-paths, in user units).
    #[must_use]
    fn path_length(&self) -> f64;

    /// Sample the path at arc length `distance` from its start.
    ///
    /// Returns `None` when the path is empty or `distance` is negative / not finite; otherwise the
    /// distance is **clamped** to the path's extent, so "past the end" yields the end point.
    #[must_use]
    fn point_at_length(&self, distance: f64) -> Option<PathSample>;

    /// The sub-path covering the arc-length range `from..to` (clamped, no wrap-around). `from` and
    /// `to` are measured from the start of the path; an empty range yields an empty path.
    ///
    /// Typical use: draw a progress arc, or dash a curve by arc length instead of by segment.
    #[must_use]
    fn trim(&self, from: f64, to: f64) -> BezPath;

    /// Dilate (grow) the filled region by `distance`, rounding convex corners.
    #[must_use]
    fn offset(&self, distance: f64, tolerance: f64) -> BezPath;

    /// Erode (shrink) the filled region by `distance`, rounding concave corners.
    #[must_use]
    fn inset(&self, distance: f64, tolerance: f64) -> BezPath;
}

impl PathOps for BezPath {
    fn boolean(&self, other: &BezPath, op: BoolOp, tolerance: f64) -> BezPath {
        let tolerance = sane_tolerance(tolerance);
        let subject = flatten_contours(self, tolerance);
        let clip = flatten_contours(other, tolerance);
        // Degenerate operands are answered directly: i_overlay would give the same result, but
        // spelling it out documents the identity elements (`∅` is the identity of union / xor).
        if subject.is_empty() {
            return match op {
                BoolOp::Union | BoolOp::Xor => other.clone(),
                BoolOp::Intersect | BoolOp::Difference => BezPath::new(),
            };
        }
        if clip.is_empty() {
            return match op {
                BoolOp::Intersect => BezPath::new(),
                BoolOp::Union | BoolOp::Difference | BoolOp::Xor => self.clone(),
            };
        }
        let shapes = subject.overlay(&clip, op.to_overlay_rule(), FillRule::NonZero);
        shapes_to_path(&shapes)
    }

    fn normalize(&self, tolerance: f64) -> BezPath {
        let contours = flatten_contours(self, sane_tolerance(tolerance));
        if contours.is_empty() {
            return BezPath::new();
        }
        shapes_to_path(&contours.simplify_shape(FillRule::NonZero))
    }

    fn outline(&self, stroke: &Stroke, tolerance: f64) -> BezPath {
        kurbo::stroke(
            self.iter(),
            &stroke.to_kurbo(),
            &kurbo::StrokeOpts::default(),
            sane_tolerance(tolerance),
        )
    }

    fn path_length(&self) -> f64 {
        self.segments().map(|seg| seg.arclen(ARCLEN_ACCURACY)).sum()
    }

    fn point_at_length(&self, distance: f64) -> Option<PathSample> {
        if !distance.is_finite() || distance < 0.0 {
            return None;
        }
        let total = self.path_length();
        if total <= 0.0 {
            return None;
        }
        let target = distance.min(total);

        let mut travelled = 0.0;
        let mut segments = self.segments().peekable();
        while let Some(seg) = segments.next() {
            let len = seg.arclen(ARCLEN_ACCURACY);
            let is_last = segments.peek().is_none();
            // Take the segment holding `target` — or the last one when `target` sits exactly at
            // the end (so the sample is the true end point, not the previous joint).
            if target <= travelled + len || is_last {
                let local = (target - travelled).clamp(0.0, len);
                let t = if len > 0.0 {
                    seg.inv_arclen(local, ARCLEN_ACCURACY).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                return Some(PathSample {
                    point: seg.eval(t),
                    tangent: unit_tangent(&seg, t),
                    distance: target,
                });
            }
            travelled += len;
        }
        None
    }

    fn trim(&self, from: f64, to: f64) -> BezPath {
        if !from.is_finite() || !to.is_finite() {
            return BezPath::new();
        }
        let total = self.path_length();
        if total <= 0.0 {
            return BezPath::new();
        }
        let start = from.max(0.0).min(total);
        let end = to.max(0.0).min(total);
        if end <= start {
            return BezPath::new();
        }
        if start <= 0.0 && end >= total {
            return self.clone();
        }

        let mut out = BezPath::new();
        let mut travelled = 0.0;
        let mut open = false;
        for seg in self.segments() {
            let len = seg.arclen(ARCLEN_ACCURACY);
            let (seg_start, seg_end) = (travelled, travelled + len);
            travelled = seg_end;
            if seg_end <= start || seg_start >= end {
                continue;
            }
            let (t0, t1) = if len > 0.0 {
                (
                    seg.inv_arclen((start - seg_start).clamp(0.0, len), ARCLEN_ACCURACY),
                    seg.inv_arclen((end - seg_start).clamp(0.0, len), ARCLEN_ACCURACY),
                )
            } else {
                (0.0, 1.0)
            };
            let (t0, t1) = (t0.clamp(0.0, 1.0), t1.clamp(0.0, 1.0));
            if t1 <= t0 {
                continue;
            }
            push_segment(&mut out, &seg.subsegment(t0..t1), &mut open);
        }
        out
    }

    fn offset(&self, distance: f64, tolerance: f64) -> BezPath {
        if !distance.is_finite() || distance <= 0.0 {
            return self.clone();
        }
        let tolerance = sane_tolerance(tolerance);
        let band = dilation_band(self, distance, tolerance);
        self.boolean(&band, BoolOp::Union, tolerance)
    }

    fn inset(&self, distance: f64, tolerance: f64) -> BezPath {
        if !distance.is_finite() || distance <= 0.0 {
            return self.clone();
        }
        let tolerance = sane_tolerance(tolerance);
        let band = dilation_band(self, distance, tolerance);
        self.boolean(&band, BoolOp::Difference, tolerance)
    }
}

/// Guard a caller-supplied tolerance: non-finite or non-positive falls back to the default (a
/// negative tolerance would make kurbo's flattening non-terminating).
fn sane_tolerance(tolerance: f64) -> f64 {
    if tolerance.is_finite() && tolerance > 0.0 {
        tolerance
    } else {
        DEFAULT_TOLERANCE
    }
}

/// Unit tangent at parameter `t`, with fallbacks for degenerate (zero-derivative) segments.
///
/// `PathSeg` itself is not `ParamCurveDeriv` (the derivative of a cubic is a quadratic, so the
/// type changes), hence the per-variant match.
fn unit_tangent(seg: &PathSeg, t: f64) -> Vec2 {
    let d = match seg {
        PathSeg::Line(line) => line.p1 - line.p0,
        PathSeg::Quad(quad) => quad.deriv().eval(t).to_vec2(),
        PathSeg::Cubic(cubic) => cubic.deriv().eval(t).to_vec2(),
    };
    if d.hypot() > 1e-12 {
        return d.normalize();
    }
    let chord = seg.end() - seg.start();
    if chord.hypot() > 1e-12 {
        return chord.normalize();
    }
    Vec2::new(1.0, 0.0)
}

/// Append one (sub-)segment to `out`, emitting `MoveTo` for the first piece only.
fn push_segment(out: &mut BezPath, seg: &PathSeg, open: &mut bool) {
    match seg {
        PathSeg::Line(line) => {
            if !*open {
                out.move_to(line.p0);
                *open = true;
            }
            out.line_to(line.p1);
        }
        PathSeg::Quad(quad) => {
            if !*open {
                out.move_to(quad.p0);
                *open = true;
            }
            out.quad_to(quad.p1, quad.p2);
        }
        PathSeg::Cubic(cubic) => {
            if !*open {
                out.move_to(cubic.p0);
                *open = true;
            }
            out.curve_to(cubic.p1, cubic.p2, cubic.p3);
        }
    }
}

/// The Minkowski band of `path` with a disk of radius `distance`: the stroke outline of `path`
/// with width `2 * distance` and round caps / joins.
///
/// Dilating a region by a disk is exactly `region ∪ band`; eroding it is `region ∖ band`. Round
/// caps / joins are what make the band the *disk* (rather than a mitered or butt-ended) sum.
fn dilation_band(path: &BezPath, distance: f64, tolerance: f64) -> BezPath {
    let mut closed = path.clone();
    let already_closed = matches!(closed.elements().last(), Some(PathEl::ClosePath));
    if !closed.is_empty() && !already_closed {
        closed.close_path();
    }
    let style = kurbo::Stroke::new(2.0 * distance)
        .with_caps(kurbo::Cap::Round)
        .with_join(kurbo::Join::Round);
    kurbo::stroke(
        closed.iter(),
        &style,
        &kurbo::StrokeOpts::default(),
        tolerance,
    )
}

/// Flatten `path` into polygonal contours (dropping degenerate ones and non-finite points).
fn flatten_contours(path: &BezPath, tolerance: f64) -> Vec<Vec<[f64; 2]>> {
    let mut contours: Vec<Vec<[f64; 2]>> = Vec::new();
    let mut current: Vec<[f64; 2]> = Vec::new();
    kurbo::flatten(path.iter(), tolerance, |el| match el {
        PathEl::MoveTo(p) => {
            flush_contour(&mut current, &mut contours);
            if p.is_finite() {
                current.push([p.x, p.y]);
            }
        }
        PathEl::LineTo(p) => {
            if p.is_finite() {
                current.push([p.x, p.y]);
            }
        }
        PathEl::ClosePath => flush_contour(&mut current, &mut contours),
        // `flatten` never emits curves; ignore defensively.
        PathEl::QuadTo(..) | PathEl::CurveTo(..) => {}
    });
    flush_contour(&mut current, &mut contours);
    contours
}

/// End the current contour: keep it when it is a real polygon (≥ 3 points), then reuse the buffer.
fn flush_contour(current: &mut Vec<[f64; 2]>, contours: &mut Vec<Vec<[f64; 2]>>) {
    if current.len() >= 3 {
        contours.push(std::mem::take(current));
    } else {
        current.clear();
    }
}

/// Turn i_overlay's shapes (shape → contours → points, holes wound the other way) into a
/// [`BezPath`] with one closed sub-path per contour.
fn shapes_to_path(shapes: &[Vec<Vec<[f64; 2]>>]) -> BezPath {
    let mut path = BezPath::new();
    for shape in shapes {
        for contour in shape {
            if contour.len() < 3 {
                continue;
            }
            path.move_to((contour[0][0], contour[0][1]));
            for p in &contour[1..] {
                path.line_to((p[0], p[1]));
            }
            path.close_path();
        }
    }
    path
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::{Circle, Color, Rect, Shape};
    use crate::scene::{LineCap, LineJoin};

    fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> BezPath {
        Rect::new(x0, y0, x1, y1).to_path(0.01)
    }

    fn contour_count(path: &BezPath) -> usize {
        path.elements()
            .iter()
            .filter(|el| matches!(el, PathEl::MoveTo(_)))
            .count()
    }

    fn stroke(width: f64, cap: LineCap) -> Stroke {
        Stroke {
            color: Color::BLACK,
            width,
            line_cap: cap,
            line_join: LineJoin::Miter,
            ..Default::default()
        }
    }

    /// 圆环：轮廓无角、无自交，`Shape::area()` 就是渲染面积。
    #[test]
    fn outline_of_a_circle_is_an_annulus() {
        let circle = Circle::new(Point::new(0.0, 0.0), 5.0).to_path(0.01);
        let outline = circle.outline(&stroke(2.0, LineCap::Butt), DEFAULT_TOLERANCE);
        assert_eq!(contour_count(&outline), 2, "外圈 + 内圈");
        // 圆环：π(6² - 4²) —— 无角、无自交，shoelace 就是渲染面积。
        let expected = std::f64::consts::PI * (36.0 - 16.0);
        assert!(
            (outline.area().abs() - expected).abs() < 0.5,
            "面积 {} 期望 {expected}",
            outline.area().abs()
        );
    }

    // ——— 布尔运算 ———

    #[test]
    fn boolean_ops_on_overlapping_rects() {
        let a = rect(0.0, 0.0, 10.0, 10.0); // 100
        let b = rect(5.0, 5.0, 15.0, 15.0); // 100, 重叠 25

        assert!(
            (a.union(&b).area() - 175.0).abs() < 0.5,
            "{:?}",
            a.union(&b).area()
        );
        assert!((a.intersect(&b).area() - 25.0).abs() < 0.5);
        assert!((a.difference(&b).area() - 75.0).abs() < 0.5);
        assert!((a.xor(&b).area() - 150.0).abs() < 0.5);
    }

    /// 差集挖洞：结果必须有**两条**子路径（外圈 + 反向绕的洞）。
    ///
    /// `Shape::area` 走格林公式、对各 contour 的**有向**面积求和，所以 `84 = 100 - 16` 本身就
    /// 证明了洞是反向绕的（若同向绕会得到 `116`），而非零规则据此把它渲染成洞。
    #[test]
    fn difference_punches_a_real_hole() {
        let ring = rect(0.0, 0.0, 10.0, 10.0).difference(&rect(3.0, 3.0, 7.0, 7.0));
        assert_eq!(contour_count(&ring), 2, "外圈 + 洞");
        assert!((ring.area() - 84.0).abs() < 0.5, "面积 {}", ring.area());
    }

    #[test]
    fn boolean_on_disjoint_shapes() {
        let a = rect(0.0, 0.0, 10.0, 10.0);
        let b = rect(20.0, 0.0, 30.0, 10.0);
        assert!((a.union(&b).area() - 200.0).abs() < 0.5);
        assert_eq!(contour_count(&a.union(&b)), 2);
        assert!(a.intersect(&b).is_empty());
        assert!((a.difference(&b).area() - 100.0).abs() < 0.5);
    }

    #[test]
    fn degenerate_operands_degrade_predictably() {
        let a = rect(0.0, 0.0, 10.0, 10.0);
        let empty = BezPath::new();

        assert!((a.union(&empty).area() - 100.0).abs() < 0.5);
        assert!((a.difference(&empty).area() - 100.0).abs() < 0.5);
        assert!((a.xor(&empty).area() - 100.0).abs() < 0.5);
        assert!(a.intersect(&empty).is_empty());

        assert!((empty.union(&a).area() - 100.0).abs() < 0.5);
        assert!(empty.intersect(&a).is_empty());
        assert!(empty.difference(&a).is_empty());
    }

    #[test]
    fn normalize_merges_overlapping_contours() {
        // 同一路径里两个互相重叠的圈（各自独立 contour）——非零自并应合并成一个。
        let mut messy = BezPath::new();
        for r in [rect(0.0, 0.0, 10.0, 10.0), rect(5.0, 5.0, 15.0, 15.0)] {
            messy.extend(r.iter());
        }
        assert_eq!(contour_count(&messy), 2);

        let clean = messy.normalize(DEFAULT_TOLERANCE);
        assert_eq!(contour_count(&clean), 1, "重叠圈应合并");
        assert!((clean.area() - 175.0).abs() < 0.5, "面积 {}", clean.area());

        // 空路径 → 空。
        assert!(BezPath::new().normalize(DEFAULT_TOLERANCE).is_empty());
    }

    // ——— stroke-to-path ———

    #[test]
    fn outline_of_a_line_is_a_stadium() {
        let mut line = BezPath::new();
        line.move_to((0.0, 0.0));
        line.line_to((10.0, 0.0));

        let outline = line.outline(&stroke(4.0, LineCap::Round), DEFAULT_TOLERANCE);
        let bb = outline.bounding_box();
        assert!((bb.min_x() + 2.0).abs() < 1e-6, "{bb:?}");
        assert!((bb.max_x() - 12.0).abs() < 1e-6, "{bb:?}");
        assert!((bb.min_y() + 2.0).abs() < 1e-6, "{bb:?}");
        assert!((bb.max_y() - 2.0).abs() < 1e-6, "{bb:?}");

        // 胶囊 = 10×4 的矩形 + 两个半径 2 的半圆。
        let expected = 10.0 * 4.0 + std::f64::consts::PI * 4.0;
        assert!(
            (outline.area() - expected).abs() < 0.5,
            "面积 {} 期望 {expected}",
            outline.area()
        );
    }

    /// 方框描边（10×10、宽 2）：外沿 0..12、内沿 2..10，环带 = 144 - 64 = 80。
    ///
    /// 注意这里**不能**用 `Shape::area()` 断言：kurbo 对闭合路径给出的是"未裁剪的偏移"轮廓，
    /// 内圈在四个直角处会留下靠**非零绕数**抵消的回折（自交），shoelace 面积会偏大（84）。
    /// 真正渲染出来的面积由栅格后端给出 80 —— 这也正是 `offset` / `inset` 走布尔运算（同样按
    /// 非零规则解绕数）能得到正确结果的原因。
    #[test]
    fn outline_of_a_closed_rect_fills_the_band() {
        use crate::render::VelloPixmapRenderer;
        use crate::{Element, FillStrokeStyle, Scene};

        let outline =
            rect(1.0, 1.0, 11.0, 11.0).outline(&stroke(2.0, LineCap::Butt), DEFAULT_TOLERANCE);
        assert_eq!(contour_count(&outline), 2, "环形带是外圈 + 内洞");
        let bb = outline.bounding_box();
        assert!(
            (bb.min_x()).abs() < 1e-6 && (bb.max_x() - 12.0).abs() < 1e-6,
            "{bb:?}"
        );

        let mut scene = Scene::new(14.0, 14.0);
        scene.push(Element::Path {
            path: outline,
            style: FillStrokeStyle::fill(Color::RED),
            closed: true,
        });
        let mut renderer = VelloPixmapRenderer::new(14, 14).with_background(Color::WHITE);
        let pixmap = renderer.render_scene_to_pixmap(&scene);
        let red = pixmap
            .pixels()
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|p| p[0] > 200 && p[1] < 60)
            .count();
        assert!(
            red.abs_diff(80) <= 4,
            "环带渲染面积应约 80 像素，实得 {red}"
        );
    }

    #[test]
    fn outline_keeps_curves_smooth() {
        // stroke-to-path 不经多边形化：结果里仍有曲线段。
        let circle = Circle::new(Point::new(0.0, 0.0), 5.0).to_path(0.01);
        let outline = circle.outline(&stroke(2.0, LineCap::Butt), DEFAULT_TOLERANCE);
        assert!(
            outline
                .segments()
                .any(|seg| matches!(seg, PathSeg::Cubic(_) | PathSeg::Quad(_))),
            "描边轮廓应保留曲线段"
        );
    }

    // ——— 测量 ———

    #[test]
    fn measure_straight_line() {
        let mut p = BezPath::new();
        p.move_to((0.0, 0.0));
        p.line_to((3.0, 4.0));

        assert!((p.path_length() - 5.0).abs() < 1e-9);

        let s = p.point_at_length(2.5).unwrap();
        assert!(
            (s.point.x - 1.5).abs() < 1e-6 && (s.point.y - 2.0).abs() < 1e-6,
            "{:?}",
            s.point
        );
        assert!(
            (s.tangent.x - 0.6).abs() < 1e-6 && (s.tangent.y - 0.8).abs() < 1e-6,
            "{:?}",
            s.tangent
        );
        assert!((s.distance - 2.5).abs() < 1e-9);
    }

    #[test]
    fn measure_circle_and_clamping() {
        let circle = Circle::new(Point::new(0.0, 0.0), 10.0).to_path(1e-6);
        let expected = std::f64::consts::TAU * 10.0;
        assert!(
            (circle.path_length() - expected).abs() < 0.05,
            "周长 {} 期望 {expected}",
            circle.path_length()
        );

        // 超出末端 → 夹到终点。
        let end = circle.point_at_length(expected * 2.0).unwrap();
        assert!((end.point.to_vec2().length() - 10.0).abs() < 1e-3);
        assert!((end.distance - circle.path_length()).abs() < 1e-9);
        // 切向是单位向量。
        assert!((end.tangent.hypot() - 1.0).abs() < 1e-9);

        // 非法输入。
        assert!(circle.point_at_length(-1.0).is_none());
        assert!(circle.point_at_length(f64::NAN).is_none());
        assert!(BezPath::new().point_at_length(1.0).is_none());
    }

    #[test]
    fn measure_is_multi_subpath() {
        let mut p = BezPath::new();
        p.move_to((0.0, 0.0));
        p.line_to((10.0, 0.0));
        p.move_to((100.0, 0.0));
        p.line_to((100.0, 10.0));

        assert!((p.path_length() - 20.0).abs() < 1e-9);
        // 第二段的起点在累计弧长 10 处。
        let s = p.point_at_length(15.0).unwrap();
        assert!(
            (s.point.x - 100.0).abs() < 1e-6 && (s.point.y - 5.0).abs() < 1e-6,
            "{:?}",
            s.point
        );
    }

    #[test]
    fn trim_extracts_a_sub_arc() {
        let mut p = BezPath::new();
        p.move_to((0.0, 0.0));
        p.line_to((10.0, 0.0));

        let mid = p.trim(2.5, 7.5);
        assert!(
            (mid.path_length() - 5.0).abs() < 1e-6,
            "{}",
            mid.path_length()
        );
        assert_eq!(mid.elements()[0], PathEl::MoveTo(Point::new(2.5, 0.0)));

        // 全长 → 原样；空区间 / 反向区间 → 空。
        assert_eq!(p.trim(0.0, 10.0).elements().len(), p.elements().len());
        assert!(p.trim(5.0, 5.0).is_empty());
        assert!(p.trim(8.0, 3.0).is_empty());
        assert!(p.trim(f64::NAN, 1.0).is_empty());
        assert!(BezPath::new().trim(0.0, 1.0).is_empty());
    }

    #[test]
    fn trim_across_a_curve_keeps_curve_type() {
        let mut p = BezPath::new();
        p.move_to((0.0, 0.0));
        p.curve_to((0.0, 10.0), (10.0, 10.0), (10.0, 0.0));
        let sub = p.trim(0.25 * p.path_length(), 0.75 * p.path_length());
        assert!(
            sub.segments().all(|seg| matches!(seg, PathSeg::Cubic(_))),
            "裁剪曲线段应仍是三次曲线"
        );
        assert!((sub.path_length() - 0.5 * p.path_length()).abs() < 1e-3);
    }

    // ——— offset / inset ———

    #[test]
    fn offset_dilates_and_rounds_corners() {
        let grown = rect(0.0, 0.0, 10.0, 10.0).offset(1.0, DEFAULT_TOLERANCE);
        let bb = grown.bounding_box();
        assert!(
            (bb.min_x() + 1.0).abs() < 1e-4 && (bb.max_x() - 11.0).abs() < 1e-4,
            "{bb:?}"
        );

        // 膨胀 = 原方形 + 四条 10×1 的边带 + 四个 1/4 圆角（半径 1 的整圆）。
        let expected = 100.0 + 4.0 * 10.0 + std::f64::consts::PI;
        assert!(
            (grown.area() - expected).abs() < 1.0,
            "面积 {} 期望 {expected}",
            grown.area()
        );
    }

    #[test]
    fn inset_erodes_to_the_inner_square() {
        let shrunk = rect(0.0, 0.0, 10.0, 10.0).inset(1.0, DEFAULT_TOLERANCE);
        let bb = shrunk.bounding_box();
        assert!(
            (bb.min_x() - 1.0).abs() < 1e-4 && (bb.max_x() - 9.0).abs() < 1e-4,
            "{bb:?}"
        );
        // 方形的内缩对角是"两条半边界的交"，仍是尖角 → 8×8。
        assert!((shrunk.area() - 64.0).abs() < 1.0, "面积 {}", shrunk.area());
    }

    #[test]
    fn over_inset_vanishes() {
        assert!(
            rect(0.0, 0.0, 10.0, 10.0)
                .inset(6.0, DEFAULT_TOLERANCE)
                .is_empty(),
            "内缩超过半宽应消失"
        );
    }

    #[test]
    fn non_positive_offset_is_identity() {
        let r = rect(0.0, 0.0, 10.0, 10.0);
        assert!((r.offset(0.0, DEFAULT_TOLERANCE).area() - 100.0).abs() < 1e-9);
        assert!((r.offset(-3.0, DEFAULT_TOLERANCE).area() - 100.0).abs() < 1e-9);
        assert!((r.inset(-3.0, DEFAULT_TOLERANCE).area() - 100.0).abs() < 1e-9);
    }

    /// 坏容差应回落到默认值，而不是让 flatten 不收敛。
    #[test]
    fn bad_tolerance_falls_back() {
        let r = rect(0.0, 0.0, 10.0, 10.0);
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert!((r.union(&r).area() - 100.0).abs() < 0.5, "tolerance {bad}");
        }
    }

    // ——— 端到端：两后端一致 ———

    /// 布尔运算结果是 IR 里的普通路径 → 两个后端渲染一致，洞在两个后端都是洞。
    #[test]
    fn boolean_result_renders_on_both_backends() {
        use crate::render::{SvgRenderer, VelloPixmapRenderer};
        use crate::{Element, FillStrokeStyle, Scene};

        let ring = rect(0.0, 0.0, 20.0, 20.0).difference(&rect(6.0, 6.0, 14.0, 14.0));
        let mut scene = Scene::new(20.0, 20.0);
        scene.push(Element::Path {
            path: ring,
            style: FillStrokeStyle::fill(Color::RED),
            closed: true,
        });

        // SVG：一条 <path>，d 里两段闭合子路径（外圈 + 洞）。
        let mut svg = SvgRenderer::new(20.0, 20.0);
        scene.render(&mut svg);
        let svg = svg.into_string();
        let d = svg
            .split("d=\"")
            .nth(1)
            .and_then(|s| s.split('"').next())
            .expect("SVG 应含 d 属性");
        assert_eq!(d.matches('M').count(), 2, "外圈 + 洞: {d}");
        assert!(d.ends_with('Z'), "每条子路径都闭合: {d}");

        // 栅格：环上红、洞中透出背景。
        let mut renderer = VelloPixmapRenderer::new(20, 20).with_background(Color::WHITE);
        let pixmap = renderer.render_scene_to_pixmap(&scene);
        let at = |x: u32, y: u32| {
            let i = ((y * pixmap.width() + x) as usize) * 4;
            let p = &pixmap.pixels()[i..i + 4];
            (p[0], p[1], p[2])
        };
        let (r, g, b) = at(2, 2);
        assert!(r > 200 && g < 60 && b < 60, "环上应为红: {:?}", (r, g, b));
        let (r, g, b) = at(10, 10);
        assert!(
            r > 240 && g > 240 && b > 240,
            "洞中应为背景: {:?}",
            (r, g, b)
        );
    }

    /// builder 便捷入口：布尔运算沿用 ctx 样式，stroke-to-path 用描边色填充。
    #[test]
    fn builder_helpers() {
        use crate::Element;
        use crate::builder::Ctx;

        let node = Ctx::new().with_fill(Color::BLUE).boolean_path(
            &rect(0.0, 0.0, 10.0, 10.0),
            &rect(5.0, 0.0, 15.0, 10.0),
            BoolOp::Intersect,
        );
        match node.element() {
            Some(Element::Path {
                path,
                style,
                closed,
            }) => {
                assert!(*closed, "布尔结果应闭合");
                assert!((path.area() - 50.0).abs() < 0.5);
                assert_eq!(style.fill, Some(crate::scene::Fill::Solid(Color::BLUE)));
            }
            other => panic!("期望 Path，得到 {other:?}"),
        }

        let mut line = BezPath::new();
        line.move_to((0.0, 0.0));
        line.line_to((10.0, 0.0));
        let outline = Ctx::new().outline_path(&line, &stroke(4.0, LineCap::Round));
        match outline.element() {
            Some(Element::Path { path, style, .. }) => {
                assert!(path.area() > 40.0, "轮廓应有面积: {}", path.area());
                assert_eq!(style.fill, Some(crate::scene::Fill::Solid(Color::BLACK)));
                assert!(style.stroke.is_none(), "轮廓本身用填充画");
            }
            other => panic!("期望 Path，得到 {other:?}"),
        }
    }
}
