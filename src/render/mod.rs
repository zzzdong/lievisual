//! Rendering backend trait and default traversal logic.
//!
//! [`Renderer`] defines the atomic drawing methods (Canvas API style). The default
//! method [`Renderer::render_scene`] is responsible for: sorting nodes stably by
//! `z_index` via [`crate::scene::Scene::iter_ordered`] → applying node attributes
//! (`visible` / `opacity` / `name` / `clip`) and `Transform` → recursing into `Group`s
//! (internally reusing [`crate::scene::SceneNode::ordered`]). It also draws the default
//! `nodes` layer first, then each [`crate::scene::Layer`] in order
//! ([`Renderer::render_layer`], applying `visible` / `opacity` / `transform` / `name`
//! to the whole layer). A backend only needs to implement the atomic methods.
//!
//! Convention for new primitives: every `draw_*` has a default implementation that
//! converts the geometry into a [`kurbo::BezPath`] and delegates to [`Renderer::draw_path`],
//! so a new backend renders correctly even without overriding them. Backends may override
//! these to emit native markup (e.g. SVG `<ellipse>` / `<rect rx>`).

use crate::geometry::{Point, Rect, Transform, Vec2};
use crate::scene::{Element, FillStrokeStyle, Layer, Scene, SceneNode, Stroke};
use kurbo::Shape;

pub use svg::{SvgRenderer, SvgSizing};
pub use vello_pixmap::VelloPixmapRenderer;

mod svg;
mod vello_pixmap;

/// Build an arc path in the screen (y-down) coordinate system, geometrically identical to the
/// SVG backend's `arc_path_d`.
///
/// The scene (consistent across SVG / vello) uses a y-down coordinate system: origin at the
/// top-left, y downward, 0 angle along +x, and positive angles clockwise. kurbo::Arc, however,
/// samples mathematically in y-up: `start=(cx+r·cos a, cy+r·sin a)` with a positive sweep being
/// counterclockwise.
///
/// Here we reuse kurbo's mature Bézier arc approximation (`to_path`) and then apply a
/// y-up → y-down coordinate mapping so the sampled arc aligns exactly with the SVG backend's
/// `arc_path_d` formula, avoiding a vertical mirror between the two backends' arcs / pies:
/// 1. Negate the center y and angles to construct the "to-be-flipped" arc in the math plane;
/// 2. Apply a whole y-axis flip `Affine(1,0,0,-1)` to map math coordinates back to screen.
pub(crate) fn arc_path_y_down(
    center: Point,
    radii: Vec2,
    start_angle: f64,
    sweep_angle: f64,
) -> kurbo::BezPath {
    // Scene y-down → math y-up: negate the center y and angle signs (0 angle unchanged,
    // clockwise/counterclockwise swap).
    let arc = kurbo::Arc::new(
        (center.x, -center.y),
        (radii.x, radii.y),
        -start_angle,
        -sweep_angle,
        0.0,
    )
    .to_path(0.1);
    // Math y-up → scene y-down: flip the whole y-axis (x unchanged).
    let mut path = arc;
    path.apply_affine(kurbo::Affine::new([1.0, 0.0, 0.0, -1.0, 0.0, 0.0]));
    path
}

/// Rendering backend interface (Canvas API style).
///
/// Callers (liecharts / liemermaid / liepress) build a [`Scene`] and then pick a
/// `Renderer` implementation to emit the target medium. Adding a backend does not affect
/// the intermediate representation.
pub trait Renderer {
    /// Draw an axis-aligned rectangle.
    fn draw_rect(&mut self, rect: Rect, style: &FillStrokeStyle);

    /// Draw a circle.
    fn draw_circle(&mut self, center: Point, radius: f64, style: &FillStrokeStyle);

    /// Draw an ellipse (`radii` are the two semi-axis lengths, `rotation` is the angle
    /// around the center).
    fn draw_ellipse(&mut self, center: Point, radii: Vec2, rotation: f64, style: &FillStrokeStyle) {
        let path =
            kurbo::Ellipse::new((center.x, center.y), (radii.x, radii.y), rotation).to_path(0.1);
        self.draw_path(&path, style, true);
    }

    /// Draw a rounded rectangle (`radius` is the uniform corner radius).
    fn draw_rounded_rect(&mut self, rect: Rect, radius: f64, style: &FillStrokeStyle) {
        let path = kurbo::RoundedRect::new(
            rect.min_x(),
            rect.min_y(),
            rect.max_x(),
            rect.max_y(),
            radius,
        )
        .to_path(0.1);
        self.draw_path(&path, style, true);
    }

    /// Draw a line segment.
    fn draw_line(&mut self, start: Point, end: Point, style: &Stroke);

    /// Draw a polyline (multiple connected segments, open, stroke only).
    fn draw_polyline(&mut self, points: &[Point], style: &Stroke);

    /// Draw a polygon (closed, filled + optional stroke).
    fn draw_polygon(&mut self, points: &[Point], style: &FillStrokeStyle) {
        let mut path = kurbo::BezPath::new();
        if let Some(p0) = points.first() {
            path.move_to((p0.x, p0.y));
            for p in &points[1..] {
                path.line_to((p.x, p.y));
            }
            path.close_path();
        }
        self.draw_path(&path, style, false);
    }

    /// Draw an arc (open, stroke only).
    fn draw_arc(
        &mut self,
        center: Point,
        radii: Vec2,
        start_angle: f64,
        sweep_angle: f64,
        style: &Stroke,
    ) {
        let path = crate::render::arc_path_y_down(center, radii, start_angle, sweep_angle);
        let style = FillStrokeStyle {
            fill: None,
            stroke: Some(style.clone()),
        };
        self.draw_path(&path, &style, false);
    }

    /// Draw a pie sector (closed, including the two radius edges from the center to the
    /// arc endpoints, filled + optional stroke).
    fn draw_pie(
        &mut self,
        center: Point,
        radius: f64,
        start_angle: f64,
        sweep_angle: f64,
        style: &FillStrokeStyle,
    ) {
        let mut path = kurbo::BezPath::new();
        path.move_to((center.x, center.y));
        let arc = crate::render::arc_path_y_down(
            center,
            Vec2::new(radius, radius),
            start_angle,
            sweep_angle,
        );
        // The arc path starts with MoveTo(start); replace it with LineTo(start) so the whole
        // path becomes `M center L start [arc...] end Z` -- a true pie sector (with the center
        // vertex and the two radius edges), matching the SVG backend's `M center L start A end Z`.
        let mut els = arc.elements().iter();
        if let Some(kurbo::PathEl::MoveTo(p0)) = els.next() {
            path.line_to(*p0);
            for el in els {
                path.push(*el);
            }
        } else {
            path.extend(arc);
        }
        path.close_path();
        self.draw_path(&path, style, false);
    }

    /// Draw a path (optionally closed for fill or stroke).
    fn draw_path(&mut self, path: &kurbo::BezPath, style: &FillStrokeStyle, closed: bool);

    /// Draw a path with a linear-gradient fill.
    fn draw_gradient_path(
        &mut self,
        path: &kurbo::BezPath,
        gradient: &crate::scene::LinearGradient,
        stroke: Option<&crate::scene::Stroke>,
    );

    /// Draw text. The styled `spans` are the core content; when `layout` is provided it
    /// takes precedence.
    ///
    /// Non-parley backends (e.g. SVG) should concatenate the `spans` into plain text;
    /// `style` provides block-level placement (`align` / `baseline` / `rotation` /
    /// `max_width`).
    fn draw_text(
        &mut self,
        spans: &[crate::text::RichSpan],
        position: Point,
        style: &crate::text::TextStyle,
        layout: Option<&crate::text::TextLayout>,
    );

    /// Draw an image: `image` is a self-contained RGBA8 bitmap and `frame` is the display
    /// rectangle (pt/px coordinates).
    ///
    /// The backend consumes `image.pixmap` directly (already decoded, **no further
    /// decoding needed**) and maps it onto `frame` according to `image.object_fit`.
    /// The default implementation is empty (backends should override it to actually draw).
    fn draw_image(&mut self, image: &crate::SceneImage, frame: Rect, opacity: f64) {
        let _ = (image, frame, opacity);
    }

    /// Enter a transform scope (for a Group or node-local transform).
    /// Empty by default; backends with stack-based state (vello / svg group) should
    /// override.
    fn push_transform(&mut self, _t: Transform) {}
    /// Exit the scope established by [`Renderer::push_transform`].
    fn pop_transform(&mut self) {}

    /// Enter an opacity scope (for a node / subtree with `opacity < 1`). Empty by default.
    fn push_opacity(&mut self, _opacity: f64) {}
    /// Exit the scope established by [`Renderer::push_opacity`].
    fn pop_opacity(&mut self) {}

    /// Enter a named scope (for a node with `name`; emits `id` on the SVG side).
    /// `name` is an `Option`: `None` means no `id` attribute is emitted.
    fn push_name(&mut self, _name: Option<&str>) {}
    /// Exit the scope established by [`Renderer::push_name`].
    fn pop_name(&mut self) {}

    /// Enter a clip scope (for a node / subtree with `clip`). Empty by default.
    fn push_clip(&mut self, _clip: &crate::scene::Clip) {}
    /// Exit the scope established by [`Renderer::push_clip`].
    fn pop_clip(&mut self) {}

    /// Enter the node-level scope: stacks transform, clip, opacity, and name in order.
    /// The default combines `push_transform` / `push_clip` / `push_opacity` /
    /// `push_name`; backends rarely need to override this.
    fn push_node_scope(&mut self, node: &SceneNode) {
        if let Some(t) = node.transform {
            self.push_transform(t);
        }
        if let Some(clip) = &node.clip {
            self.push_clip(clip);
        }
        if node.opacity != 1.0 {
            self.push_opacity(node.opacity);
        }
        if node.name.is_some() {
            self.push_name(node.name.as_deref());
        }
    }

    /// Exit the scope established by [`Renderer::push_node_scope`] (pops in reverse order).
    fn pop_node_scope(&mut self, node: &SceneNode) {
        if node.name.is_some() {
            self.pop_name();
        }
        if node.opacity != 1.0 {
            self.pop_opacity();
        }
        if node.clip.is_some() {
            self.pop_clip();
        }
        if node.transform.is_some() {
            self.pop_transform();
        }
    }

    /// Render the whole scene: the default implementation handles sorting and recursion.
    /// A backend may override this to capture [`Scene`] metadata (title / description /
    /// scale), then call [`Renderer::render_scene_ordered`] to reuse the default traversal.
    fn render_scene(&mut self, scene: &Scene) {
        self.render_scene_ordered(scene);
    }

    /// Default traversal: draws the default `nodes` layer first (bottommost, sorted
    /// stably by ascending `z_index`), then each [`Layer`] in array order from bottom to
    /// top.
    fn render_scene_ordered(&mut self, scene: &Scene) {
        for node in scene.iter_ordered() {
            self.render_node(node);
        }
        for layer in &scene.layers {
            self.render_layer(layer);
        }
    }

    /// Render a single layer: applies the layer-level `visible` / `opacity` /
    /// `transform` / `name` scope, then renders each node inside the layer stably by
    /// ascending `z_index` (including Group recursion). Rarely needs to be overridden.
    fn render_layer(&mut self, layer: &Layer) {
        if !layer.visible {
            return;
        }
        if let Some(t) = layer.transform {
            self.push_transform(t);
        }
        if layer.opacity != 1.0 {
            self.push_opacity(layer.opacity);
        }
        if !layer.name.is_empty() {
            self.push_name(Some(&layer.name));
        }

        for node in layer.iter_ordered() {
            self.render_node(node);
        }

        if !layer.name.is_empty() {
            self.pop_name();
        }
        if layer.opacity != 1.0 {
            self.pop_opacity();
        }
        if layer.transform.is_some() {
            self.pop_transform();
        }
    }

    /// Render a single node (node attributes, local transform, and Group recursion
    /// included). Rarely needs to be overridden.
    fn render_node(&mut self, node: &SceneNode) {
        // Skip the whole subtree for a hidden node.
        if !node.visible {
            return;
        }
        // Enter the node scope (transform + opacity + name).
        self.push_node_scope(node);

        match &node.element {
            Element::Group { children } => {
                // Children inside a Group are drawn stably by ascending z_index
                // (reusing SceneNode::ordered).
                for child in SceneNode::ordered(children) {
                    self.render_node(child);
                }
            }
            Element::Rect { rect, style } => self.draw_rect(*rect, style),
            Element::Circle {
                center,
                radius,
                style,
            } => self.draw_circle(*center, *radius, style),
            Element::Ellipse {
                center,
                radii,
                rotation,
                style,
            } => self.draw_ellipse(*center, *radii, *rotation, style),
            Element::RoundedRect {
                rect,
                radius,
                style,
            } => self.draw_rounded_rect(*rect, *radius, style),
            Element::Line { start, end, style } => self.draw_line(*start, *end, style),
            Element::Polyline { points, style } => self.draw_polyline(points, style),
            Element::Polygon { points, style } => self.draw_polygon(points, style),
            Element::Arc {
                center,
                radii,
                start_angle,
                sweep_angle,
                style,
            } => self.draw_arc(*center, *radii, *start_angle, *sweep_angle, style),
            Element::Pie {
                center,
                radius,
                start_angle,
                sweep_angle,
                style,
            } => self.draw_pie(*center, *radius, *start_angle, *sweep_angle, style),
            Element::Path {
                path,
                style,
                closed,
            } => self.draw_path(path, style, *closed),
            Element::Image {
                image,
                frame,
                opacity,
            } => self.draw_image(image, *frame, *opacity),
            Element::GradientPath {
                path,
                gradient,
                stroke,
            } => self.draw_gradient_path(path, gradient, stroke.as_ref()),
            Element::Text {
                spans,
                position,
                style,
                layout,
            } => self.draw_text(spans, *position, style, layout.as_deref()),
        }

        self.pop_node_scope(node);
    }
}

#[cfg(test)]
mod tests {
    use crate::geometry::{Point, Vec2};

    /// The screen (y-down) endpoint formula, identical to the SVG backend's `arc_path_d`.
    /// start = (cx + rx·cos a, cy + ry·sin a), end = (cx + rx·cos(a+s), cy + ry·sin(a+s)).
    fn expected_screen_points(
        center: Point,
        radii: Vec2,
        start_angle: f64,
        sweep_angle: f64,
    ) -> (Point, Point) {
        let start = Point::new(
            center.x + radii.x * start_angle.cos(),
            center.y + radii.y * start_angle.sin(),
        );
        let end_angle = start_angle + sweep_angle;
        let end = Point::new(
            center.x + radii.x * end_angle.cos(),
            center.y + radii.y * end_angle.sin(),
        );
        (start, end)
    }

    /// Verify that the arc endpoints generated by the vello backend's `arc_path_y_down` match
    /// the SVG `arc_path_d` formula, ensuring the pie/arc shapes drawn by both backends align
    /// exactly (they were once vertically mirrored because kurbo's math coordinates were not
    /// flipped).
    #[test]
    fn arc_y_down_matches_svg_geometry() {
        let cases = [
            (
                Point::new(300.0, 320.0),
                Vec2::new(70.0, 70.0),
                0.0f64,
                std::f64::consts::FRAC_PI_2,
            ),
            (
                Point::new(300.0, 320.0),
                Vec2::new(70.0, 70.0),
                std::f64::consts::FRAC_PI_2,
                std::f64::consts::FRAC_PI_2,
            ),
            (
                Point::new(300.0, 320.0),
                Vec2::new(70.0, 70.0),
                std::f64::consts::PI,
                -std::f64::consts::FRAC_PI_2,
            ),
            (Point::new(100.0, 100.0), Vec2::new(40.0, 20.0), 0.4, 2.7),
        ];
        for (center, radii, a, s) in cases {
            let path = crate::render::arc_path_y_down(center, radii, a, s);
            // Pure arc path: elements start with MoveTo(start), then CurveTo endpoints are
            // appended in order, so pts[0] is the arc start and pts[last] is the arc end.
            let pts: Vec<Point> = path
                .elements()
                .iter()
                .filter_map(|el| match el {
                    kurbo::PathEl::MoveTo(p)
                    | kurbo::PathEl::LineTo(p)
                    | kurbo::PathEl::QuadTo(_, p)
                    | kurbo::PathEl::CurveTo(_, _, p) => Some(Point::new(p.x, p.y)),
                    kurbo::PathEl::ClosePath => None,
                })
                .collect();
            let n = pts.len();
            assert!(n >= 2, "arc path should have at least 2 points");
            let got_start = pts[0];
            let got_end = pts[n - 1];
            let (exp_start, exp_end) = expected_screen_points(center, radii, a, s);
            let eps = 1e-6;
            assert!(
                (got_start.x - exp_start.x).abs() < eps && (got_start.y - exp_start.y).abs() < eps,
                "start mismatch: got {:?} expected {:?} (center={:?} radii={:?} a={} s={})",
                got_start,
                exp_start,
                center,
                radii,
                a,
                s
            );
            assert!(
                (got_end.x - exp_end.x).abs() < eps && (got_end.y - exp_end.y).abs() < eps,
                "end mismatch: got {:?} expected {:?} (center={:?} radii={:?} a={} s={})",
                got_end,
                exp_end,
                center,
                radii,
                a,
                s
            );
        }
    }
}
