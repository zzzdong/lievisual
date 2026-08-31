//! Content-bounds computation and "fit the canvas to the content" helpers.
//!
//! Two complementary pieces:
//!
//! - **Bounds**: [`element_bounds`] / [`node_bounds`] / [`nodes_bounds`] / [`scene_bounds`]
//!   compute the geometric bounding box of IR data. These belong in lievisual because the
//!   traversal must know every [`Element`] variant, how [`Transform`] composes, and how a
//!   [`TextLayout`](crate::text::TextLayout) is measured — knowledge that only exists here.
//! - **Fit**: [`fit_scene`] shrinks / moves an already-built [`Scene`] so the canvas hugs its
//!   content, optionally constrained by a maximum size. This is what "content-driven canvas"
//!   renderers (diagram / chart exporters) need, and it used to be re-implemented downstream.
//!
//! # What counts as content
//!
//! - Strokes: bounds are inflated by half the stroke width, so a thick outline is not clipped.
//! - Node / layer transforms: applied when walking the tree, so the result is in the same
//!   coordinate space the backends draw in.
//! - `visible = false` subtrees are skipped (they draw nothing, so they should not reserve space).
//! - Clips are **not** applied: a clip only *removes* pixels, it never adds any, so ignoring it
//!   keeps the box conservative (never too small).
//!
//! # Example
//!
//! ```
//! use lievisual::{Color, Point, Rect, Scene};
//! use lievisual::fit::{fit_scene, FitOptions};
//! use lievisual::scene::{Element, FillStrokeStyle};
//!
//! let mut scene = Scene::new(0.0, 0.0);
//! scene.push(Element::rect(
//!     Rect::new(20.0, 30.0, 120.0, 80.0),
//!     FillStrokeStyle::fill(Color::BLACK),
//! ));
//! // Hug the content with a 8pt margin, never upscaling.
//! fit_scene(&mut scene, FitOptions::new().with_margin(8.0));
//! assert_eq!(scene.width, 116.0);
//! assert_eq!(scene.height, 66.0);
//! ```

use kurbo::Shape;

use crate::geometry::{Point, Rect, Size, Transform, Vec2};
use crate::scene::{Element, Scene, SceneNode, Stroke};

/// Bounding box of a single primitive, in the element's own coordinate space.
///
/// Returns `None` for primitives that occupy no area (an empty polyline / path, a degenerate
/// rect, ...). Node-level attributes (`transform` / `clip` / `visible`) are **not** considered
/// here; use [`node_bounds`] for that.
pub fn element_bounds(element: &Element) -> Option<Rect> {
    let box_ = match element {
        Element::Rect { rect, style } => {
            let mut r = normalized(*rect);
            if let Some(pad) = stroke_inset(style.stroke.as_ref()) {
                r = r.inflate(pad, pad);
            }
            Some(r)
        }
        Element::RoundedRect {
            rect,
            radius: _,
            style,
        } => {
            let mut r = normalized(*rect);
            if let Some(pad) = stroke_inset(style.stroke.as_ref()) {
                r = r.inflate(pad, pad);
            }
            Some(r)
        }
        Element::Circle {
            center,
            radius,
            style,
        } => circle_box(*center, *radius, style.stroke.as_ref()),
        Element::Ellipse {
            center,
            radii,
            rotation,
            style,
        } => {
            // kurbo's bounding_box handles the rotation exactly (via the derivative), so a
            // rotated ellipse does not get the over-large "axis-aligned radii" box.
            let e = kurbo::Ellipse::new(
                (center.x, center.y),
                (radii.x.abs(), radii.y.abs()),
                *rotation,
            );
            inflate_opt(e.bounding_box(), style.stroke.as_ref())
        }
        Element::Line { start, end, style } => {
            let r = Rect::from_points(*start, *end);
            inflate_opt(r, Some(style))
        }
        Element::Polyline { points, style } => points_box(points).map(|r| inflate(r, Some(style))),
        Element::Polygon { points, style } => {
            points_box(points).map(|r| inflate(r, style.stroke.as_ref()))
        }
        Element::Arc {
            center,
            radii,
            start_angle,
            sweep_angle,
            style,
        } => inflate_opt(
            arc_box(*center, *radii, *start_angle, *sweep_angle),
            Some(style),
        ),
        Element::Pie {
            center,
            radius,
            start_angle: _,
            sweep_angle: _,
            style,
        } => circle_box(*center, *radius, style.stroke.as_ref()),
        Element::Path { path, style, .. } => {
            inflate_opt(path.bounding_box(), style.stroke.as_ref())
        }
        Element::GradientPath {
            path,
            gradient: _,
            stroke,
        } => inflate_opt(path.bounding_box(), stroke.as_ref()),
        Element::Image {
            image,
            frame,
            opacity: _,
        } => {
            // `Contain` / `Cover` / `Fill` all stay inside `frame` (`Cover` is clipped to it);
            // `None` places the bitmap at its **original pixel size** from the frame's top-left,
            // which may overflow the frame — union both to stay conservative.
            let mut r = normalized(*frame);
            if image.object_fit == crate::scene::ObjectFit::None {
                r = r.union(Rect::from_origin_size(
                    Point::new(r.min_x(), r.min_y()),
                    Size::new(image.width() as f64, image.height() as f64),
                ));
            }
            Some(r)
        }
        Element::Text {
            spans,
            position,
            style,
            layout,
        } => text_box(spans, *position, style, layout.as_deref()),
        Element::Group { children } => nodes_bounds(children),
    };
    // Drop degenerate / non-finite boxes so a single pathological primitive cannot poison the
    // whole canvas transform.
    box_.filter(|r| r.is_finite() && r.width() >= 0.0 && r.height() >= 0.0)
}

fn inflate_opt(r: Rect, stroke: Option<&Stroke>) -> Option<Rect> {
    Some(inflate(r, stroke))
}

/// Inflate by half the stroke width (strokes are centred on the geometry).
fn inflate(r: Rect, stroke: Option<&Stroke>) -> Rect {
    match stroke_inset(stroke) {
        Some(pad) => r.inflate(pad, pad),
        None => r,
    }
}

/// Half the stroke width, or `None` when there is nothing to pad for.
fn stroke_inset(stroke: Option<&Stroke>) -> Option<f64> {
    let w = stroke.map(|s| s.width).unwrap_or(0.0);
    if w.is_finite() && w > 0.0 {
        Some(w / 2.0)
    } else {
        None
    }
}

/// A `Rect` may be given with `min > max`; normalize so the box is never empty.
fn normalized(r: Rect) -> Rect {
    Rect::new(
        r.min_x().min(r.max_x()),
        r.min_y().min(r.max_y()),
        r.max_x().max(r.min_x()),
        r.max_y().max(r.min_y()),
    )
}

fn circle_box(center: Point, radius: f64, stroke: Option<&Stroke>) -> Option<Rect> {
    let r = radius.abs();
    if !r.is_finite() {
        return None;
    }
    inflate_opt(
        Rect::new(center.x - r, center.y - r, center.x + r, center.y + r),
        stroke,
    )
}

/// Union of an iterator of boxes; `None` when empty.
fn union_all(boxes: impl IntoIterator<Item = Rect>) -> Option<Rect> {
    boxes.into_iter().reduce(|a, b| a.union(b))
}

fn points_box(points: &[Point]) -> Option<Rect> {
    union_all(
        points
            .iter()
            .filter(|p| p.is_finite())
            .map(|p| Rect::from_points(*p, *p)),
    )
}

/// Union of a single point expressed as a zero-size box.
fn pt_box(p: Point) -> Rect {
    Rect::from_points(p, p)
}

/// Exact bounding box of an (elliptical) arc: endpoints plus any on-curve extrema.
///
/// The conservative "full ellipse" box is correct but wasteful — for a pie chart's 30° slice it
/// would reserve the whole circle and leave a huge empty canvas. `x` extrema sit where
/// `cos θ = ±1` (θ = 0 / π) and `y` extrema where `sin θ = ±1` (θ = ±π/2), so it suffices to
/// collect the two endpoints plus whichever of those four angles fall inside the swept range.
fn arc_box(center: Point, radii: Vec2, start_angle: f64, sweep_angle: f64) -> Rect {
    let rx = radii.x.abs();
    let ry = radii.y.abs();
    let at = |theta: f64| Point::new(center.x + rx * theta.cos(), center.y + ry * theta.sin());
    let (lo, hi) = if sweep_angle >= 0.0 {
        (start_angle, start_angle + sweep_angle)
    } else {
        (start_angle + sweep_angle, start_angle)
    };
    let mut pts = vec![at(start_angle), at(start_angle + sweep_angle)];
    if lo.is_finite() && hi.is_finite() {
        for k in [
            0.0,
            std::f64::consts::PI,
            std::f64::consts::FRAC_PI_2,
            -std::f64::consts::FRAC_PI_2,
        ] {
            // Smallest θ ≡ k (mod 2π) that is ≥ lo; keep it only if it is still ≤ hi.
            let n = ((lo - k) / std::f64::consts::TAU).ceil();
            let theta = k + n * std::f64::consts::TAU;
            if theta <= hi {
                pts.push(at(theta));
            }
        }
    }
    union_all(pts.into_iter().filter(|p| p.is_finite()).map(pt_box))
        .unwrap_or_else(|| pt_box(center))
}

/// Bounding box of a text element, honouring `align` / `baseline` / `rotation` / `baseline_shift`.
fn text_box(
    spans: &[crate::text::RichSpan],
    position: Point,
    style: &crate::text::TextStyle,
    layout: Option<&crate::text::TextLayout>,
) -> Option<Rect> {
    // Prefer a pre-laid-out result; otherwise measure (only when fitting, so the cost is paid
    // once per export rather than once per frame).
    let (w, h) = match layout {
        Some(l) => (l.width, l.height),
        None => {
            let m = crate::text::measure_text(spans, style.max_width);
            (m.size.width, m.size.height)
        }
    };
    if !w.is_finite() || !h.is_finite() || w <= 0.0 || h <= 0.0 {
        return None;
    }
    let (x0, x1) = match style.align {
        crate::text::TextAlign::Left | crate::text::TextAlign::Justify => {
            (position.x, position.x + w)
        }
        crate::text::TextAlign::Center => (position.x - w / 2.0, position.x + w / 2.0),
        crate::text::TextAlign::Right => (position.x - w, position.x),
    };
    let shift = style.baseline_shift;
    let (y0, y1) = match style.baseline {
        crate::text::TextBaseline::Top => (position.y, position.y + h),
        crate::text::TextBaseline::Middle => (position.y - h / 2.0, position.y + h / 2.0),
        crate::text::TextBaseline::Bottom => (position.y - h, position.y),
        // The alphabetic baseline sits ~80% down the block (ascent-dominant in most fonts).
        _ => (position.y - h * 0.8, position.y + h * 0.2),
    };
    let mut r = Rect::new(x0, y0 + shift, x1, y1 + shift);
    // Rotation is about `position`: rotate the four corners and re-fit an axis-aligned box.
    if style.rotation != 0.0 && style.rotation.is_finite() {
        let rot = Transform::rotate(style.rotation);
        r = transform_box_about(&rot, r, position)?;
    }
    Some(r)
}

/// Bounding box of a node, with its own `transform` applied.
///
/// `visible = false` yields `None` (nothing is drawn). `clip` is ignored on purpose — it can only
/// shrink the visible area, so skipping it keeps the result conservative.
pub fn node_bounds(node: &SceneNode) -> Option<Rect> {
    if !node.visible {
        return None;
    }
    let local = element_bounds(&node.element)?;
    Some(match node.transform.as_ref() {
        Some(t) => transform_box(t, local),
        None => local,
    })
}

/// Bounding box of a node slice (all boxes unioned).
pub fn nodes_bounds(nodes: &[SceneNode]) -> Option<Rect> {
    union_all(nodes.iter().filter_map(node_bounds))
}

/// Bounding box of a whole scene: the default `nodes` layer **and** every named layer.
pub fn scene_bounds(scene: &Scene) -> Option<Rect> {
    let layers = scene.layers.iter().filter(|l| l.visible).filter_map(|l| {
        let b = nodes_bounds(&l.nodes)?;
        Some(match l.transform.as_ref() {
            Some(t) => transform_box(t, b),
            None => b,
        })
    });
    union_all(nodes_bounds(&scene.nodes).into_iter().chain(layers))
}

/// Map an axis-aligned box through an affine transform: transform the four corners, then re-fit.
fn transform_box(t: &Transform, r: Rect) -> Rect {
    // `apply_point` can yield NaN for a degenerate matrix; fall back to the untouched box so one
    // pathological node cannot poison the whole fit.
    transform_box_about(t, r, Point::ZERO).unwrap_or(r)
}

/// Same as [`transform_box`], but the transform is applied about `pivot` rather than the origin.
fn transform_box_about(t: &Transform, r: Rect, pivot: Point) -> Option<Rect> {
    let corners = [
        Point::new(r.min_x(), r.min_y()),
        Point::new(r.max_x(), r.min_y()),
        Point::new(r.max_x(), r.max_y()),
        Point::new(r.min_x(), r.max_y()),
    ]
    .into_iter()
    .map(|c| {
        let p = t.apply_point(Point::new(c.x - pivot.x, c.y - pivot.y));
        Point::new(p.x + pivot.x, p.y + pivot.y)
    });
    union_all(corners.filter(|p| p.is_finite()).map(pt_box))
}

/// Options for [`fit_scene`].
#[derive(Debug, Clone)]
pub struct FitOptions {
    /// Maximum canvas width, **including** the margin. `None` = unconstrained on this axis.
    pub max_width: Option<f64>,
    /// Maximum canvas height, **including** the margin. `None` = unconstrained on this axis.
    pub max_height: Option<f64>,
    /// Padding kept on all four sides of the content.
    pub margin: f64,
    /// When `false` (default) the constraints are an **upper bound** and content is never
    /// enlarged — matching mermaid's "canvas hugs the content" output. Set `true` when the
    /// target is a bitmap that needs a minimum pixel size.
    pub upscale: bool,
    /// Canvas size used when the scene has no measurable content (empty, or all coordinates
    /// non-finite). `None` leaves `scene.width` / `scene.height` untouched.
    pub empty_size: Option<Size>,
}

impl Default for FitOptions {
    fn default() -> Self {
        Self {
            max_width: None,
            max_height: None,
            margin: 0.0,
            upscale: false,
            empty_size: None,
        }
    }
}

impl FitOptions {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Maximum canvas width (including margin).
    #[must_use]
    pub fn with_max_width(mut self, w: f64) -> Self {
        self.max_width = Some(w);
        self
    }

    /// Maximum canvas height (including margin).
    #[must_use]
    pub fn with_max_height(mut self, h: f64) -> Self {
        self.max_height = Some(h);
        self
    }

    /// Padding on all four sides.
    #[must_use]
    pub fn with_margin(mut self, margin: f64) -> Self {
        self.margin = margin;
        self
    }

    /// Allow enlarging the content to fill the constraint.
    #[must_use]
    pub fn with_upscale(mut self, upscale: bool) -> Self {
        self.upscale = upscale;
        self
    }

    /// Canvas size for an empty scene.
    #[must_use]
    pub fn with_empty_size(mut self, size: Size) -> Self {
        self.empty_size = Some(size);
        self
    }
}

/// Resize a scene so its canvas hugs the content, then move the content to `(margin, margin)`.
///
/// The content is scaled **uniformly** by `min(avail_w / content_w, avail_h / content_h)` and
/// clamped to `1.0` unless [`FitOptions::upscale`] is set, so the canvas is
/// `content * scale + 2 * margin` on each axis.
///
/// Returns `true` when the scene was adjusted. When the resulting transform is (near) the
/// identity it is skipped entirely, leaving the node tree untouched — only `width` / `height`
/// change.
pub fn fit_scene(scene: &mut Scene, opts: FitOptions) -> bool {
    // A negative / non-finite margin has no geometric meaning; clamp to 0.
    let margin = if opts.margin.is_finite() && opts.margin > 0.0 {
        opts.margin
    } else {
        0.0
    };

    let Some(bbox) = scene_bounds(scene).filter(|r| r.is_finite()) else {
        if let Some(size) = opts.empty_size {
            scene.width = size.width;
            scene.height = size.height;
            return true;
        }
        return false;
    };

    // Guard against zero-area content: a null box would make `scale` infinite.
    let content_w = (bbox.width()).max(1.0);
    let content_h = (bbox.height()).max(1.0);

    // Available area, at least 1pt — a max size smaller than 2×margin must not produce a
    // negative or zero scale (which previously emitted `viewBox="0 0 0 -20.41"`).
    let avail_w =
        sanitize_max(opts.max_width).map_or(f64::INFINITY, |w| (w - 2.0 * margin).max(1.0));
    let avail_h =
        sanitize_max(opts.max_height).map_or(f64::INFINITY, |h| (h - 2.0 * margin).max(1.0));

    // Unconstrained on both axes → no meaningful upscale target, so keep the natural size.
    let natural = (avail_w / content_w).min(avail_h / content_h);
    let natural = if natural.is_finite() { natural } else { 1.0 };
    let max_scale = if opts.upscale { f64::MAX } else { 1.0 };
    let scale = natural.clamp(f64::MIN_POSITIVE, max_scale);

    scene.width = content_w * scale + 2.0 * margin;
    scene.height = content_h * scale + 2.0 * margin;

    let offset_x = margin - bbox.min_x() * scale;
    let offset_y = margin - bbox.min_y() * scale;

    // Sub-pixel shift and unit scale: wrapping in a Group would only add indirection.
    if offset_x.abs() < 0.5 && offset_y.abs() < 0.5 && (scale - 1.0).abs() < 1e-3 {
        return true;
    }

    let fit = Transform::translate(offset_x, offset_y).then(&Transform::scale(scale));

    // Wrap the default layer; each named layer gets the transform composed onto its own
    // (layer-local transform first, then the fit), so layers and default nodes stay aligned.
    if !scene.nodes.is_empty() {
        let children = std::mem::take(&mut scene.nodes);
        scene
            .nodes
            .push(SceneNode::group(children).with_transform(fit));
    }
    for layer in &mut scene.layers {
        layer.transform = Some(match layer.transform.take() {
            Some(t) => t.then(&fit),
            None => fit,
        });
    }
    true
}

/// Drop `None` / non-finite / non-positive max sizes.
fn sanitize_max(v: Option<f64>) -> Option<f64> {
    v.filter(|v| v.is_finite() && *v > 0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::Color;
    use crate::scene::{Element, FillStrokeStyle, Layer, ObjectFit, SceneImage};
    use crate::text::TextStyle;

    fn rect_node(x0: f64, y0: f64, x1: f64, y1: f64) -> SceneNode {
        SceneNode::new(Element::rect(
            Rect::new(x0, y0, x1, y1),
            FillStrokeStyle::default(),
        ))
    }

    fn scene_with(nodes: Vec<SceneNode>) -> Scene {
        let mut s = Scene::new(0.0, 0.0);
        s.nodes = nodes;
        s
    }

    // ——— bounds ———

    #[test]
    fn element_bounds_includes_stroke_half_width() {
        // 100×100 rect, 10pt stroke → the outline extends 5pt beyond the geometry.
        let el = Element::rect(
            Rect::new(0.0, 0.0, 100.0, 100.0),
            FillStrokeStyle {
                fill: None,
                stroke: Some(Stroke::new(Color::BLACK, 10.0)),
            },
        );
        let b = element_bounds(&el).unwrap();
        assert_eq!((b.min_x(), b.min_y()), (-5.0, -5.0));
        assert_eq!((b.max_x(), b.max_y()), (105.0, 105.0));
    }

    /// 缺陷回归：下游实现完全忽略 stroke 宽度，粗描边图形会被画布裁掉半个线宽。
    #[test]
    fn line_bounds_includes_stroke_half_width() {
        let el = Element::Line {
            start: Point::new(0.0, 0.0),
            end: Point::new(100.0, 0.0),
            style: Stroke::new(Color::BLACK, 20.0),
        };
        let b = element_bounds(&el).unwrap();
        assert_eq!(b.min_y(), -10.0, "水平线应上下各扩展半个线宽");
        assert_eq!(b.max_y(), 10.0);
    }

    /// 缺陷回归：下游实现忽略节点 transform，算出的是未变换坐标。
    #[test]
    fn node_bounds_applies_transform() {
        let node =
            rect_node(0.0, 0.0, 10.0, 10.0).with_transform(Transform::translate(100.0, 50.0));
        let b = node_bounds(&node).unwrap();
        assert_eq!((b.min_x(), b.min_y()), (100.0, 50.0));
        assert_eq!((b.max_x(), b.max_y()), (110.0, 60.0));
    }

    #[test]
    fn node_bounds_applies_rotation() {
        // 正方形绕原点旋转 90°：应落在 x∈[-10,0], y∈[0,10]。
        let node = rect_node(0.0, 0.0, 10.0, 10.0)
            .with_transform(Transform::rotate(std::f64::consts::FRAC_PI_2));
        let b = node_bounds(&node).unwrap();
        assert!((b.min_x() + 10.0).abs() < 1e-9, "got {b:?}");
        assert!((b.max_x()).abs() < 1e-9, "got {b:?}");
        assert!((b.min_y()).abs() < 1e-9, "got {b:?}");
        assert!((b.max_y() - 10.0).abs() < 1e-9, "got {b:?}");
    }

    #[test]
    fn invisible_nodes_are_skipped() {
        let node = rect_node(0.0, 0.0, 1000.0, 1000.0).with_visible(false);
        assert_eq!(node_bounds(&node), None);
        assert_eq!(nodes_bounds(&[node]), None);
    }

    #[test]
    fn group_bounds_unions_children() {
        let g = SceneNode::new(Element::Group {
            children: vec![
                rect_node(0.0, 0.0, 10.0, 10.0),
                rect_node(90.0, 40.0, 100.0, 50.0),
            ],
        });
        let b = node_bounds(&g).unwrap();
        assert_eq!(
            (b.min_x(), b.min_y(), b.max_x(), b.max_y()),
            (0.0, 0.0, 100.0, 50.0)
        );
    }

    /// 弧的包围盒应只覆盖扫过的部分（用极值点精确求解），而非整个椭圆。
    #[test]
    fn arc_bounds_covers_only_the_swept_range() {
        // 第一象限的四分之一圆弧：应只到 (10,10)，不覆盖整个圆。
        let b = arc_box(
            Point::new(0.0, 0.0),
            Vec2::new(10.0, 10.0),
            0.0,
            std::f64::consts::FRAC_PI_2,
        );
        assert!((b.min_x() - 0.0).abs() < 1e-9, "got {b:?}");
        assert!((b.min_y() - 0.0).abs() < 1e-9, "got {b:?}");
        assert!((b.max_x() - 10.0).abs() < 1e-9, "got {b:?}");
        assert!((b.max_y() - 10.0).abs() < 1e-9, "got {b:?}");

        // 下半圆（0 → π，y 恒 ≥ 0 在 y-down 坐标系下）不应含 y = -10 的极值点。
        let half = arc_box(
            Point::new(0.0, 0.0),
            Vec2::new(10.0, 10.0),
            0.0,
            std::f64::consts::PI,
        );
        assert!((half.min_y() - 0.0).abs() < 1e-9, "got {half:?}");
        assert!((half.max_y() - 10.0).abs() < 1e-9, "got {half:?}");
    }

    /// `ObjectFit::None` 下图片按原始像素尺寸摆放，可能溢出 frame。
    #[test]
    fn image_none_fit_may_overflow_frame() {
        let img = SceneImage::from_rgba8(50, 20, vec![0u8; 50 * 20 * 4])
            .unwrap()
            .with_object_fit(ObjectFit::None);
        let el = Element::Image {
            image: img,
            frame: Rect::new(0.0, 0.0, 10.0, 10.0),
            opacity: 1.0,
        };
        let b = element_bounds(&el).unwrap();
        assert_eq!(
            (b.max_x(), b.max_y()),
            (50.0, 20.0),
            "应按原始像素尺寸溢出 frame"
        );
    }

    #[test]
    fn text_bounds_uses_real_measurement() {
        let style = TextStyle::new(Color::BLACK, 16.0, "sans-serif");
        let el = Element::Text {
            spans: vec![crate::text::RichSpan::new("Hello", style.clone())],
            position: Point::new(0.0, 0.0),
            style,
            layout: None,
        };
        let b = element_bounds(&el);
        // 无可用字体时测量可能为 0 → 返回 None；有字体时宽高应为正。
        if let Some(b) = b {
            assert!(b.width() > 0.0 && b.height() > 0.0, "got {b:?}");
        }
    }

    #[test]
    fn scene_bounds_covers_layers() {
        let mut s = scene_with(vec![rect_node(0.0, 0.0, 10.0, 10.0)]);
        let mut layer = Layer::new("overlay");
        layer.push(rect_node(100.0, 100.0, 120.0, 120.0));
        s.push_layer(layer);
        let b = scene_bounds(&s).unwrap();
        assert_eq!(
            (b.min_x(), b.min_y(), b.max_x(), b.max_y()),
            (0.0, 0.0, 120.0, 120.0)
        );
    }

    #[test]
    fn empty_scene_has_no_bounds() {
        assert_eq!(scene_bounds(&scene_with(vec![])), None);
    }

    // ——— fit ———

    #[test]
    fn fit_hugs_content_with_margin() {
        let mut s = scene_with(vec![rect_node(20.0, 30.0, 120.0, 80.0)]);
        assert!(fit_scene(&mut s, FitOptions::new().with_margin(8.0)));
        // 内容 100×50 + 两侧 8pt 边距。
        assert_eq!(s.width, 116.0);
        assert_eq!(s.height, 66.0);
        // 内容被平移到 (8, 8)。
        let b = scene_bounds(&s).unwrap();
        assert!((b.min_x() - 8.0).abs() < 1e-9, "got {b:?}");
        assert!((b.min_y() - 8.0).abs() < 1e-9, "got {b:?}");
    }

    #[test]
    fn fit_never_upscales_by_default() {
        let mut s = scene_with(vec![rect_node(0.0, 0.0, 100.0, 100.0)]);
        fit_scene(
            &mut s,
            FitOptions::new()
                .with_max_width(800.0)
                .with_max_height(600.0)
                .with_margin(8.0),
        );
        assert!(
            s.width < 200.0 && s.height < 200.0,
            "got {}x{}",
            s.width,
            s.height
        );
    }

    #[test]
    fn fit_upscales_when_requested() {
        let mut s = scene_with(vec![rect_node(0.0, 0.0, 100.0, 100.0)]);
        fit_scene(
            &mut s,
            FitOptions::new()
                .with_max_width(800.0)
                .with_max_height(600.0)
                .with_margin(8.0)
                .with_upscale(true),
        );
        assert!(
            s.width > 500.0 && s.height > 500.0,
            "got {}x{}",
            s.width,
            s.height
        );
    }

    /// 只约束一个维度时，另一维度按内容比例同步缩放（不被隐式限制）。
    #[test]
    fn fit_single_axis_constraint_keeps_aspect() {
        let mut s = scene_with(vec![rect_node(0.0, 0.0, 100.0, 100.0)]);
        fit_scene(
            &mut s,
            FitOptions::new()
                .with_max_width(500.0)
                .with_margin(8.0)
                .with_upscale(true),
        );
        assert!((s.width - 500.0).abs() < 1.0, "got {}", s.width);
        assert!((s.height - 500.0).abs() < 1.0, "got {}", s.height);

        let mut s = scene_with(vec![rect_node(0.0, 0.0, 100.0, 100.0)]);
        fit_scene(
            &mut s,
            FitOptions::new()
                .with_max_height(500.0)
                .with_margin(8.0)
                .with_upscale(true),
        );
        assert!((s.height - 500.0).abs() < 1.0, "got {}", s.height);
        assert!((s.width - 500.0).abs() < 1.0, "got {}", s.width);
    }

    /// 内容宽高比与约束不同时，取较小的比例（等比缩放，不拉伸）。
    #[test]
    fn fit_uses_uniform_scale() {
        let mut s = scene_with(vec![rect_node(0.0, 0.0, 200.0, 100.0)]);
        fit_scene(
            &mut s,
            FitOptions::new()
                .with_max_width(1000.0)
                .with_max_height(300.0)
                .with_margin(0.0)
                .with_upscale(true),
        );
        // avail = (1000, 300) → natural = min(5, 3) = 3 → 600×300。
        assert!((s.width - 600.0).abs() < 1e-6, "got {}", s.width);
        assert!((s.height - 300.0).abs() < 1e-6, "got {}", s.height);
    }

    /// 约束小于 2×边距时必须保证 scale 为正，画布尺寸不能为负。
    #[test]
    fn fit_tiny_constraint_keeps_positive_canvas() {
        let mut s = scene_with(vec![rect_node(0.0, 0.0, 100.0, 100.0)]);
        fit_scene(
            &mut s,
            FitOptions::new()
                .with_max_width(4.0)
                .with_max_height(4.0)
                .with_margin(8.0),
        );
        assert!(
            s.width > 0.0 && s.height > 0.0,
            "got {}x{}",
            s.width,
            s.height
        );
        assert!(s.width.is_finite() && s.height.is_finite());
    }

    #[test]
    fn fit_empty_scene_uses_empty_size() {
        let mut s = scene_with(vec![]);
        assert!(fit_scene(
            &mut s,
            FitOptions::new().with_empty_size(Size::new(800.0, 600.0))
        ));
        assert_eq!((s.width, s.height), (800.0, 600.0));

        // 未给 empty_size 时保持原状。
        let mut s = Scene::new(123.0, 456.0);
        assert!(!fit_scene(&mut s, FitOptions::new()));
        assert_eq!((s.width, s.height), (123.0, 456.0));
    }

    /// 内容已在原点附近时跳过 Group 包裹（避免无谓的树变换）。
    #[test]
    fn fit_skips_wrapping_when_already_placed() {
        let mut s = scene_with(vec![rect_node(0.0, 0.0, 100.0, 50.0)]);
        fit_scene(&mut s, FitOptions::new());
        assert_eq!(s.nodes.len(), 1, "恒等变换不应包裹 Group");
        assert!(matches!(s.nodes[0].element, Element::Rect { .. }));
        assert_eq!((s.width, s.height), (100.0, 50.0));
    }

    /// 变换后 layers 与默认节点必须一起被移动，否则两层会错位。
    #[test]
    fn fit_moves_layers_together_with_nodes() {
        let mut s = scene_with(vec![rect_node(1000.0, 1000.0, 1100.0, 1050.0)]);
        let mut layer = Layer::new("overlay");
        layer.push(rect_node(1000.0, 1000.0, 1100.0, 1050.0));
        s.push_layer(layer);

        fit_scene(&mut s, FitOptions::new().with_margin(5.0));
        // 两层都从 (1000,1000) 平移到 (5,5)。
        let b = scene_bounds(&s).unwrap();
        assert!((b.min_x() - 5.0).abs() < 1e-6, "got {b:?}");
        assert!((b.min_y() - 5.0).abs() < 1e-6, "got {b:?}");
        assert!((b.width() - 100.0).abs() < 1e-6, "got {b:?}");
        assert!((b.height() - 50.0).abs() < 1e-6, "got {b:?}");
    }

    /// 图层自带 transform 时，fit 变换应叠加在其**之后**（先图层自身，再 fit）。
    #[test]
    fn fit_composes_onto_existing_layer_transform() {
        let mut s = Scene::new(0.0, 0.0);
        let mut layer = Layer::new("shifted").with_transform(Transform::translate(1000.0, 0.0));
        layer.push(rect_node(0.0, 0.0, 100.0, 50.0));
        s.push_layer(layer);

        fit_scene(&mut s, FitOptions::new().with_margin(5.0));
        let b = scene_bounds(&s).unwrap();
        assert!((b.min_x() - 5.0).abs() < 1e-6, "got {b:?}");
        assert!((b.min_y() - 5.0).abs() < 1e-6, "got {b:?}");
    }
}
