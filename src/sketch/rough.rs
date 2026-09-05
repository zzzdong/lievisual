//! Hand-drawn geometry jitter (rough.js-style).
//!
//! Every sketchable [`Element`] is turned into a **jittered open Bézier path** that is stroked
//! like the original outline. Three rules matter — drop any of them and the result stops looking
//! hand-drawn:
//!
//! 1. **Bowing**: a line is not "two jittered endpoints". Two randomised mid-points (at 50% and
//!    75% of the segment) are inserted and the stroke is a pair of cubics through them, so the
//!    line bends instead of merely wobbling.
//! 2. **Length damping**: jitter amplitude is a function of the segment length; without damping,
//!    a long straight edge of a big shape looks noisy while a short edge of a small shape stays
//!    crisp.
//! 3. **Multi-stroke**: each segment is drawn twice (the second pass uses half the amplitude and
//!    its own random phase), mimicking a pen going back and forth.
//!
//! Closed shapes additionally re-draw the first edge after closing, which produces the classic
//! "overshoot past the corner" look.

use kurbo::{ParamCurve, ParamCurveArclen, Shape};

use super::SketchOptions;
use super::rng::Rng;
use crate::geometry::{BezPath, Point, Rect, RoundedRect, Vec2};

/// rough.js' `maxRandomnessOffset`: the base amplitude (in scene units) that `roughness` scales.
const MAX_OFFSET: f64 = 2.0;

/// Length damping (rough.js `roughnessGain`): short segments use the full amplitude, very long
/// ones converge to 0.4 so a huge rectangle does not turn into noise.
pub(crate) fn dampening(len: f64) -> f64 {
    if len < 200.0 {
        1.0
    } else if len > 500.0 {
        0.4
    } else {
        -0.001_666_8 * len + 1.233_334
    }
}

/// Displace `p` by a symmetric random offset of amplitude `amp`.
fn shake(rng: &mut Rng, p: Point, amp: f64, opts: &SketchOptions, gain: f64) -> Point {
    Point::new(
        p.x + rng.offset(amp, opts.roughness, gain),
        p.y + rng.offset(amp, opts.roughness, gain),
    )
}

/// Append one jittered segment to `path`.
///
/// `first` starts a new sub-path; otherwise the segment continues from the current point (so a
/// polyline stays connected instead of falling apart at the corners). `overlay` is the second
/// (multi-stroke) pass: half amplitude, single curve, its own sub-path.
fn segment(
    rng: &mut Rng,
    path: &mut BezPath,
    a: Point,
    b: Point,
    opts: &SketchOptions,
    first: bool,
    overlay: bool,
) {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let len = dx.hypot(dy);
    let gain = dampening(len);
    // Very short segments: cap the amplitude, otherwise a 2pt edge would be scrambled.
    let mut off = MAX_OFFSET;
    if off * off * 100.0 > len * len {
        off = len / 10.0;
    }
    let amp = if overlay { off * 0.5 } else { off };

    // Bowing: perpendicular to the segment, amplitude growing with the segment length.
    let mut mx = opts.bowing * off * dy / 200.0;
    let mut my = opts.bowing * off * (-dx) / 200.0;
    mx = rng.offset(mx, opts.roughness, gain);
    my = rng.offset(my, opts.roughness, gain);

    let diverge = 0.2 + rng.next_f64() * 0.2;
    let d1 = Point::new(mx + a.x + dx * diverge, my + a.y + dy * diverge);
    let d2 = Point::new(mx + a.x + dx * diverge * 2.0, my + a.y + dy * diverge * 2.0);

    let p0 = shake(rng, a, amp, opts, gain);
    let c1 = shake(rng, a, amp, opts, gain);
    let c2 = shake(rng, d1, amp, opts, gain);
    let m1 = shake(rng, d2, amp, opts, gain);
    let c3 = shake(rng, d1, amp, opts, gain);
    let c4 = shake(rng, d2, amp, opts, gain);
    let p1 = shake(rng, b, amp, opts, gain);

    if overlay {
        path.move_to(p0);
        path.curve_to(c2, m1, p1);
    } else {
        if first {
            path.move_to(p0);
        }
        path.curve_to(c1, c2, m1);
        path.curve_to(c3, c4, p1);
    }
}

/// Jitter a point list (an open or closed polyline).
pub(crate) fn polyline(
    rng: &mut Rng,
    path: &mut BezPath,
    pts: &[Point],
    closed: bool,
    opts: &SketchOptions,
) {
    if pts.len() < 2 {
        return;
    }
    for pass in 0..if opts.multistroke { 2 } else { 1 } {
        let overlay = pass == 1;
        for (i, w) in pts.windows(2).enumerate() {
            segment(rng, path, w[0], w[1], opts, i == 0, overlay);
        }
        if closed && pts.len() > 2 {
            // Close the loop, then re-draw the first edge: the overshoot past the starting
            // corner is what makes a rectangle read as "drawn by hand".
            let last = pts[pts.len() - 1];
            segment(rng, path, last, pts[0], opts, false, overlay);
            segment(rng, path, pts[0], pts[1], opts, false, overlay);
        }
    }
}

/// Sample an (elliptical) arc in the scene's y-down convention
/// (`p = center + (rx·cos t, ry·sin t)`, positive angles clockwise) — identical to the formula
/// both backends use, so the sketch outline follows the rendered geometry exactly.
pub(crate) fn sample_arc(
    center: Point,
    radii: Vec2,
    rotation: f64,
    start: f64,
    sweep: f64,
    step: f64,
) -> Vec<Point> {
    let (rx, ry) = (radii.x.abs(), radii.y.abs());
    // Ramanujan's approximation is plenty accurate for choosing a sample count.
    let perimeter = std::f64::consts::PI
        * (3.0 * (rx + ry) - ((3.0 * rx + ry) * (rx + 3.0 * ry)).sqrt().max(0.0));
    let arc_len = perimeter * (sweep.abs() / std::f64::consts::TAU);
    let n = ((arc_len / step.max(1.0)).ceil()).clamp(8.0, 48.0) as usize;
    let (rc, rs) = (rotation.cos(), rotation.sin());
    (0..=n)
        .map(|i| {
            let t = start + sweep * (i as f64 / n as f64);
            let (x, y) = (rx * t.cos(), ry * t.sin());
            Point::new(center.x + x * rc - y * rs, center.y + x * rs + y * rc)
        })
        .collect()
}

/// A full ellipse / circle drawn in one stroke: the pen starts slightly *before* the start angle
/// and stops slightly *after* it, so the two ends overshoot and cross instead of meeting exactly.
pub(crate) fn ellipse(
    rng: &mut Rng,
    path: &mut BezPath,
    center: Point,
    radii: Vec2,
    rotation: f64,
    opts: &SketchOptions,
) {
    let tau = std::f64::consts::TAU;
    let head = rng.range(0.05, 0.15) * tau;
    let tail = rng.range(0.0, 0.10) * tau;
    let pts = sample_arc(center, radii, rotation, -head, tau + tail, opts.curve_step);
    polyline(rng, path, &pts, false, opts);
}

/// An open arc (`sweep` may be negative).
pub(crate) fn arc(
    rng: &mut Rng,
    path: &mut BezPath,
    center: Point,
    radii: Vec2,
    start_angle: f64,
    sweep_angle: f64,
    opts: &SketchOptions,
) {
    let pts = sample_arc(
        center,
        radii,
        0.0,
        start_angle,
        sweep_angle,
        opts.curve_step,
    );
    polyline(rng, path, &pts, false, opts);
}

/// Flatten a Bézier path into one point list per sub-path.
///
/// Sample count per segment comes from its true arc length, so a long curve gets more points
/// than a short one (a uniform-per-segment count would over-jitter short curves).
pub(crate) fn flatten_points(path: &BezPath, step: f64) -> Vec<Vec<Point>> {
    let mut out: Vec<Vec<Point>> = Vec::new();
    let mut cur: Vec<Point> = Vec::new();
    for seg in path.segments() {
        let p0 = seg.eval(0.0);
        // A new sub-path starts whenever the segment does not continue the previous end point.
        if cur
            .last()
            .map(|p| (p.x - p0.x).abs() > 1e-9 || (p.y - p0.y).abs() > 1e-9)
            .unwrap_or(true)
        {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            cur.push(p0);
        }
        let cubic = seg.to_cubic();
        let len = cubic.arclen(0.5);
        let n = (len / step.max(0.5)).ceil().clamp(1.0, 64.0) as usize;
        for i in 1..=n {
            cur.push(cubic.eval(i as f64 / n as f64));
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Jitter every sub-path of a Bézier path.
pub(crate) fn path(
    rng: &mut Rng,
    out: &mut BezPath,
    src: &BezPath,
    closed: bool,
    opts: &SketchOptions,
) {
    for sub in flatten_points(src, opts.curve_step) {
        polyline(rng, out, &sub, closed, opts);
    }
}

/// Axis-aligned rectangle corners (counter-clockwise in the y-down frame).
pub(crate) fn rect_points(rect: Rect) -> [Point; 4] {
    [
        Point::new(rect.min_x(), rect.min_y()),
        Point::new(rect.max_x(), rect.min_y()),
        Point::new(rect.max_x(), rect.max_y()),
        Point::new(rect.min_x(), rect.max_y()),
    ]
}

/// Rounded-rectangle outline as a point list (sampled from kurbo's path, so the corner radius is
/// honoured).
pub(crate) fn rounded_rect_points(rect: Rect, radius: f64, step: f64) -> Vec<Point> {
    let (x0, y0) = (
        rect.min_x().min(rect.max_x()),
        rect.min_y().min(rect.max_y()),
    );
    let (x1, y1) = (
        rect.max_x().max(rect.min_x()),
        rect.max_y().max(rect.min_y()),
    );
    let r = radius.abs().min((x1 - x0).min(y1 - y0) / 2.0).max(0.0);
    let shape = RoundedRect::new(x0, y0, x1, y1, r);
    flatten_points(&shape.to_path(0.1), step)
        .into_iter()
        .next()
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::{BezPath, PathEl};

    fn opts() -> SketchOptions {
        SketchOptions {
            seed: Some(1),
            ..Default::default()
        }
    }

    fn svg_of(path: &BezPath) -> String {
        path.to_svg()
    }

    #[test]
    fn dampening_is_monotonic_and_bounded() {
        assert_eq!(dampening(10.0), 1.0);
        assert_eq!(dampening(5000.0), 0.4);
        assert!(dampening(350.0) > 0.4 && dampening(350.0) < 1.0);
        assert!(dampening(200.0) >= dampening(400.0));
    }

    #[test]
    fn same_seed_produces_identical_paths() {
        let mut a = Rng::new(42);
        let mut b = Rng::new(42);
        let o = opts();
        let mut pa = BezPath::new();
        let mut pb = BezPath::new();
        polyline(
            &mut a,
            &mut pa,
            &[
                Point::new(0.0, 0.0),
                Point::new(100.0, 0.0),
                Point::new(100.0, 60.0),
                Point::new(0.0, 60.0),
            ],
            true,
            &o,
        );
        polyline(
            &mut b,
            &mut pb,
            &[
                Point::new(0.0, 0.0),
                Point::new(100.0, 0.0),
                Point::new(100.0, 60.0),
                Point::new(0.0, 60.0),
            ],
            true,
            &o,
        );
        assert_eq!(svg_of(&pa), svg_of(&pb), "同种子输出必须逐点相同");
        assert!(!pa.elements().is_empty());
    }

    #[test]
    fn different_seeds_produce_different_paths() {
        let o = opts();
        let mut pa = BezPath::new();
        let mut pb = BezPath::new();
        let pts = [Point::new(0.0, 0.0), Point::new(120.0, 40.0)];
        polyline(&mut Rng::new(1), &mut pa, &pts, false, &o);
        polyline(&mut Rng::new(2), &mut pb, &pts, false, &o);
        assert_ne!(svg_of(&pa), svg_of(&pb));
    }

    #[test]
    fn zero_roughness_reproduces_the_input_geometry() {
        let o = SketchOptions {
            roughness: 0.0,
            seed: Some(5),
            ..Default::default()
        };
        let mut p = BezPath::new();
        polyline(
            &mut Rng::new(5),
            &mut p,
            &[Point::new(0.0, 0.0), Point::new(10.0, 0.0)],
            false,
            &o,
        );
        // The stroke must stay on the y = 0 axis (bowing is scaled by roughness as well).
        for el in p.elements() {
            if let PathEl::MoveTo(q) | PathEl::LineTo(q) | PathEl::CurveTo(_, _, q) = el {
                assert!(q.y.abs() < 1e-9, "roughness=0 不应产生垂直偏移, got {q:?}");
            }
        }
    }

    #[test]
    fn multistroke_doubles_the_subpath_count() {
        let pts = [Point::new(0.0, 0.0), Point::new(50.0, 0.0)];
        let count_moves = |multistroke: bool| {
            let o = SketchOptions {
                multistroke,
                seed: Some(3),
                ..Default::default()
            };
            let mut p = BezPath::new();
            polyline(&mut Rng::new(3), &mut p, &pts, false, &o);
            p.elements()
                .iter()
                .filter(|el| matches!(el, PathEl::MoveTo(_)))
                .count()
        };
        // 单遍：整条折线是一条子路径（只有起点一个 MoveTo）。
        assert_eq!(count_moves(false), 1, "单 stroke 应是连续一条子路径");
        // 第二遍（overlay）按 rough.js 的做法另起一条半幅子路径。
        assert_eq!(count_moves(true), 2, "多 stroke 的第二遍会另起一条子路径");
    }

    #[test]
    fn closed_shapes_overshoot_the_starting_corner() {
        let o = SketchOptions {
            multistroke: false,
            seed: Some(8),
            ..Default::default()
        };
        let square = rect_points(Rect::new(0.0, 0.0, 40.0, 40.0));
        let mut closed_p = BezPath::new();
        polyline(&mut Rng::new(8), &mut closed_p, &square, true, &o);
        let mut open_p = BezPath::new();
        polyline(&mut Rng::new(8), &mut open_p, &square, false, &o);
        // closed 会额外重画第一条边 → 曲线段更多。
        assert!(
            closed_p.elements().len() > open_p.elements().len(),
            "闭合图形应多出一条过冲边"
        );
    }

    #[test]
    fn flatten_keeps_subpaths_separate() {
        let mut p = BezPath::new();
        p.move_to((0.0, 0.0));
        p.line_to((10.0, 0.0));
        p.move_to((20.0, 0.0));
        p.line_to((30.0, 0.0));
        let subs = flatten_points(&p, 4.0);
        assert_eq!(subs.len(), 2, "两条子路径应被分开");
        assert_eq!(subs[0].first().unwrap().x, 0.0);
        assert_eq!(subs[1].first().unwrap().x, 20.0);
    }

    #[test]
    fn ellipse_is_drawn_as_one_overshooting_stroke() {
        let o = opts();
        let mut p = BezPath::new();
        ellipse(
            &mut Rng::new(11),
            &mut p,
            Point::new(50.0, 50.0),
            Vec2::new(20.0, 20.0),
            0.0,
            &o,
        );
        // 采样点数在 8..48 之间，且是开放折线（不闭合）→ 起止端互相过冲。
        assert!(!p.elements().is_empty());
        assert!(matches!(p.elements()[0], PathEl::MoveTo(_)));
    }

    #[test]
    fn rounded_rect_sampling_follows_the_corner_radius() {
        let pts = rounded_rect_points(Rect::new(0.0, 0.0, 100.0, 50.0), 12.0, 12.0);
        assert!(pts.len() >= 8, "圆角应有足够的采样点, got {}", pts.len());
        let bbox = Rect::from_points(pts[0], pts[0]);
        let bbox = pts.iter().fold(bbox, |b, p| b.union_pt(*p));
        assert!((bbox.width() - 100.0).abs() < 0.5);
        assert!((bbox.height() - 50.0).abs() < 0.5);
    }
}
