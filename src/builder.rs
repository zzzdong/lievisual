//! Canvas-style convenience construction layer (builder / sugar):
//! **an immutable superset of the Web Canvas 2D drawing vocabulary**.
//!
//! This module is optional sugar on top of the lower-level declarative IR ([`crate::scene`] /
//! [`crate::text`]). It follows the mental model of the Web Canvas 2D API (`ctx.fillStyle` /
//! `ctx.font` / `ctx.fillText`) so that "migrating from the browser" or "quickly building a
//! diagram" feels familiar, while exposing the parts of lievisual's IR that Canvas has no
//! equivalent for.
//!
//! ## Scope and boundaries
//! - **Pure construction**: every drawing method returns a [`SceneNode`], performing no immediate
//!   drawing; fully compatible with the declarative IR, and the result can be pushed directly via
//!   `scene.push(...)` or `layer.push(...)`.
//! - **No mutable state**: Canvas's `ctx` is mutable, imperative state, which this layer
//!   deliberately does not simulate. `with_*` returns a new `Ctx` (builder pattern); to use
//!   "multiple styles on one canvas", copy / chain-derive so they don't interfere.
//! - **No IR breakage**: all convenience methods eventually land on `Element::*` / `RichSpan`,
//!   introducing no new IR types.
//!
//! ## What is a superset (Canvas has no equivalent)
//!
//! | Capability | API | Why |
//! |---|---|---|
//! | Node transform | [`Ctx::with_transform`] / [`Ctx::translate`] / [`Ctx::rotate`] / [`Ctx::scale`] | Canvas folds it into `save`/`restore` on a mutable ctx |
//! | Global alpha | [`Ctx::with_alpha`] | Same |
//! | Clipping | [`Ctx::with_clip`] | Same |
//! | Named / z-ordered nodes | [`Ctx::with_name`] / [`Ctx::with_z`] | Canvas has no object model |
//! | Layers / groups | [`Ctx::group`], push into [`Layer`] | Canvas has no layer model |
//! | Rich text spans | [`Ctx::fill_rich_text`] | Canvas `fillText` is single-style |
//! | Gradients as fills | [`Ctx::with_fill_gradient`] | Canvas needs a separate `createLinearGradient` object |
//! | Dash / cap / join / miter | [`Ctx::with_line_width`] and friends | Canvas sets them via `ctx.lineWidth = …` |
//! | Full text metrics | [`Ctx::measure_text`] → [`TextMeasure`] | Canvas `TextMetrics`, plus the layout it derived |
//!
//! ## Text anchors (aligned with Canvas `fillText`)
//! The `(x, y)` of [`Ctx::fill_text`] is an **anchor**: [`TextStyle`]'s `align` / `baseline`
//! decide where the anchor lands within the text block (default `Left` + `Alphabetic`, i.e.
//! `(x,y)` is on the first-line baseline). The renderer computes the offset internally via
//! [`crate::text::compute_text_offset`], so the caller needn't position manually.
//!
//! Usage pattern:
//! ```
//! use lievisual::builder::{Ctx, Path2D};
//! use lievisual::geometry::{Color, Point};
//! use lievisual::scene::{Layer, Scene};
//! use lievisual::text::TextStyle;
//!
//! let mut scene = Scene::new(200.0, 100.0);
//! let ctx = Ctx::new()
//!     .with_fill(Color::rgb(0x33, 0x66, 0x99))
//!     .with_font(TextStyle::new(Color::rgb(0x33, 0x66, 0x99), 24.0, "sans-serif"));
//! scene.push(ctx.fill_rect(10.0, 10.0, 80.0, 40.0));  // filled rect (uses with_fill color)
//! scene.push(ctx.fill_text(50.0, 50.0, "Hi"));        // text (font color; anchor = baseline)
//!
//! // Path2D: accumulate, then terminate with fill / stroke / both.
//! let mut p = Path2D::new();
//! p.move_to(0.0, 0.0);
//! p.line_to(20.0, 0.0);
//! p.arc(20.0, 10.0, 10.0, -90f64.to_radians(), 90f64.to_radians());
//! p.close_path();
//! scene.push(ctx.stroke(&p));                         // stroke-only (Canvas: ctx.stroke())
//!
//! // Layers: any node the Ctx produces can be pushed straight into a layer.
//! let mut grid = Layer::new("grid");
//! grid.push(ctx.with_alpha(0.3).line(0.0, 0.0, 200.0, 0.0));
//! scene.push_layer(grid);
//! ```

use crate::geometry::{Color, Point, Rect, Size, Transform, Vec2};
use crate::scene::{Clip, Element, Fill, FillStrokeStyle, Layer, SceneImage, SceneNode, Stroke};
use crate::text::{RichSpan, TextMeasure, TextStyle, measure_text};
use kurbo::{BezPath, Shape};

/// Canvas-style construction context: default fill / stroke / font plus the node-level
/// attributes that every produced node carries.
///
/// Immutable and chain-derivable (`with_*` returns a new instance); carries no drawing target.
///
/// Every drawing method returns a [`SceneNode`] that has the current
/// `transform` / `alpha` / `clip` / `name` / `z_index` already applied — the declarative
/// equivalent of a Canvas `save()` … `restore()` block.
#[derive(Debug, Clone)]
pub struct Ctx {
    /// Default fill (Canvas `fillStyle`). Used only for **shape** fills; text color is decided by
    /// [`Self::font`]'s `color` (see [`Ctx::fill_text`]). When `None`, shape methods skip filling
    /// and strokes fall back to the font's `color`.
    ///
    /// A superset of Canvas: accepts a gradient as well as a solid color.
    pub fill: Option<Fill>,
    /// Default stroke (Canvas `strokeStyle` + `lineWidth`). When `None`, shape methods skip
    /// stroking and line methods fall back to 1px.
    pub stroke: Option<Stroke>,
    /// Default text style (Canvas `font` + `textAlign` + `textBaseline`); text color is its
    /// `color`.
    pub font: TextStyle,
    /// Transform applied to every node produced (`translate` / `rotate` / `scale` compose onto
    /// it). Superset of Canvas, where this lives in the mutable ctx state.
    pub transform: Option<Transform>,
    /// Global alpha (0.0–1.0), multiplied into every produced node's `opacity`.
    pub alpha: f64,
    /// Clip shape applied to every produced node's subtree.
    pub clip: Option<Clip>,
    /// Optional name / id attached to every produced node (debugging, hit-testing, SVG `id`).
    pub name: Option<String>,
    /// z-order attached to every produced node.
    pub z_index: i32,
}

impl Default for Ctx {
    fn default() -> Self {
        Self {
            fill: Some(Fill::Solid(Color::BLACK)),
            stroke: None,
            font: TextStyle::new(Color::BLACK, 12.0, "sans-serif"),
            transform: None,
            alpha: 1.0,
            clip: None,
            name: None,
            z_index: 0,
        }
    }
}

impl Ctx {
    /// Construct the default context (black fill, no stroke, 12px sans-serif text, no transform,
    /// full opacity).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    // ---- style: fill ----

    /// Set the default fill color (`fillStyle`).
    #[must_use]
    pub fn with_fill(mut self, color: Color) -> Self {
        self.fill = Some(Fill::Solid(color));
        self
    }

    /// Set the default fill to a **gradient** (linear or radial).
    ///
    /// Superset of Canvas, which needs a separate `createLinearGradient` / `addColorStop` object.
    #[must_use]
    pub fn with_fill_gradient(mut self, gradient: impl Into<Fill>) -> Self {
        self.fill = Some(gradient.into());
        self
    }

    /// Remove the fill: shapes are then stroke-only / unfilled.
    #[must_use]
    pub fn without_fill(mut self) -> Self {
        self.fill = None;
        self
    }

    /// Set the default stroke (`strokeStyle` + `lineWidth`).
    #[must_use]
    pub fn with_stroke(mut self, stroke: Stroke) -> Self {
        self.stroke = Some(stroke);
        self
    }

    /// Convenience: stroke color + width, keeping the other stroke attributes.
    #[must_use]
    pub fn with_line(mut self, color: Color, width: f64) -> Self {
        self.stroke = Some(match self.stroke {
            Some(mut s) => {
                s.color = color;
                s.width = width;
                s
            }
            None => Stroke::new(color, width),
        });
        self
    }

    /// Remove the stroke.
    #[must_use]
    pub fn without_stroke(mut self) -> Self {
        self.stroke = None;
        self
    }

    // ---- style: fine-grained stroke (Canvas `lineWidth` / `lineCap` / … ) ----

    /// `ctx.lineWidth`: stroke width only. Creates a default stroke if none is set.
    #[must_use]
    pub fn with_line_width(mut self, width: f64) -> Self {
        self.stroke_mut().width = width;
        self
    }

    /// `ctx.lineCap`: butt / round / square.
    #[must_use]
    pub fn with_line_cap(mut self, cap: crate::scene::LineCap) -> Self {
        self.stroke_mut().line_cap = cap;
        self
    }

    /// `ctx.lineJoin`: miter / round / bevel.
    #[must_use]
    pub fn with_line_join(mut self, join: crate::scene::LineJoin) -> Self {
        self.stroke_mut().line_join = join;
        self
    }

    /// `ctx.miterLimit`.
    #[must_use]
    pub fn with_miter_limit(mut self, limit: f64) -> Self {
        self.stroke_mut().miter_limit = limit;
        self
    }

    /// `ctx.setLineDash` + `ctx.lineDashOffset`. An empty `pattern` means solid.
    #[must_use]
    pub fn with_dash(mut self, pattern: Vec<f64>, offset: f64) -> Self {
        let s = self.stroke_mut();
        s.dash_array = pattern;
        s.dash_offset = offset;
        self
    }

    /// Mutable handle to the stroke, creating one from the current fill color if absent.
    fn stroke_mut(&mut self) -> &mut Stroke {
        // The fallback color is computed **before** taking the mutable borrow: the closure would
        // otherwise borrow `self` immutably while `stroke` is already mutably borrowed.
        let fallback = match &self.stroke {
            Some(_) => self.font.color,
            None => self.shape_fill(),
        };
        self.stroke
            .get_or_insert_with(|| Stroke::new(fallback, 1.0))
    }

    // ---- style: font / text ----

    /// Set the default text style (`font`).
    #[must_use]
    pub fn with_font(mut self, font: TextStyle) -> Self {
        self.font = font;
        self
    }

    /// Convenience: set only the text color (writes `self.font.color`; does not affect shape
    /// fill [`Self::fill`]).
    #[must_use]
    pub fn with_text_color(mut self, color: Color) -> Self {
        self.font.color = color;
        self
    }

    /// `ctx.textAlign`.
    #[must_use]
    pub fn with_text_align(mut self, align: crate::text::TextAlign) -> Self {
        self.font.align = align;
        self
    }

    /// `ctx.textBaseline`.
    #[must_use]
    pub fn with_text_baseline(mut self, baseline: crate::text::TextBaseline) -> Self {
        self.font.baseline = baseline;
        self
    }

    /// Text wrapping width (`ctx.fillText`'s `maxWidth`, applied uniformly to every line).
    #[must_use]
    pub fn with_max_width(mut self, max_width: f64) -> Self {
        self.font.max_width = Some(max_width);
        self
    }

    /// Remove the text wrapping width.
    #[must_use]
    pub fn without_max_width(mut self) -> Self {
        self.font.max_width = None;
        self
    }

    // ---- node attributes (superset of Canvas) ----

    /// Apply an absolute transform to every produced node.
    #[must_use]
    pub fn with_transform(mut self, t: Transform) -> Self {
        self.transform = Some(t);
        self
    }

    /// Compose a translation onto the current transform (`ctx.translate`).
    #[must_use]
    pub fn translate(mut self, x: f64, y: f64) -> Self {
        let t = Transform::translate(x, y);
        self.transform = Some(match self.transform {
            Some(cur) => cur.then(&t),
            None => t,
        });
        self
    }

    /// Compose a rotation about the origin (`ctx.rotate`, radians).
    #[must_use]
    pub fn rotate(mut self, angle: f64) -> Self {
        let t = Transform::rotate(angle);
        self.transform = Some(match self.transform {
            Some(cur) => cur.then(&t),
            None => t,
        });
        self
    }

    /// Compose a uniform scale (`ctx.scale`).
    #[must_use]
    pub fn scale(mut self, s: f64) -> Self {
        let t = Transform::scale(s);
        self.transform = Some(match self.transform {
            Some(cur) => cur.then(&t),
            None => t,
        });
        self
    }

    /// `ctx.globalAlpha`: multiplied into every produced node's `opacity`.
    #[must_use]
    pub fn with_alpha(mut self, alpha: f64) -> Self {
        self.alpha = alpha.clamp(0.0, 1.0);
        self
    }

    /// `ctx.clip()`: restrict every produced node to the interior of `clip`.
    #[must_use]
    pub fn with_clip(mut self, clip: Clip) -> Self {
        self.clip = Some(clip);
        self
    }

    /// Attach a name / id to every produced node (debugging, hit-testing, SVG `id`).
    #[must_use]
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Attach a z-order to every produced node.
    #[must_use]
    pub fn with_z(mut self, z: i32) -> Self {
        self.z_index = z;
        self
    }

    // ---- derived helpers ----

    /// Text color = the font's own color (`TextStyle::color`; `with_text_color` edits this
    /// field).
    ///
    /// Unlike Canvas where `fillStyle` decides text color, lievisual folds text color into
    /// `TextStyle.color`, so text color is decoupled from the shape fill ([`Self::shape_fill`]),
    /// avoiding `with_fill` accidentally overriding text.
    fn text_color(&self) -> Color {
        self.font.color
    }

    /// Shape fill color: default fill > font's own color. A gradient fill has no single color, so
    /// its first stop stands in (used only as a fallback for strokes / text).
    fn shape_fill(&self) -> Color {
        let from_gradient = match &self.fill {
            Some(Fill::Solid(c)) => return *c,
            Some(Fill::LinearGradient(g)) => first_stop_color(&g.stops),
            Some(Fill::RadialGradient(g)) => first_stop_color(&g.stops),
            None => None,
        };
        from_gradient.unwrap_or(self.font.color)
    }

    /// Build a `FillStrokeStyle` from the default fill + stroke.
    fn fill_stroke(&self) -> FillStrokeStyle {
        FillStrokeStyle {
            fill: self.fill.clone(),
            stroke: self.stroke.clone(),
        }
    }

    /// Fallback stroke: the current one, else a 1px solid in the fill color.
    fn stroke_or_default(&self) -> Stroke {
        self.stroke
            .clone()
            .unwrap_or_else(|| Stroke::new(self.shape_fill(), 1.0))
    }

    /// Wrap an element into a node carrying this context's node-level attributes.
    fn node(&self, element: Element) -> SceneNode {
        let mut n = SceneNode::new(element);
        if let Some(t) = self.transform {
            n = n.with_transform(t);
        }
        if (self.alpha - 1.0).abs() > f64::EPSILON {
            n = n.with_opacity(self.alpha);
        }
        if let Some(c) = &self.clip {
            n = n.with_clip(c.clone());
        }
        if let Some(name) = &self.name {
            n = n.with_name(name.clone());
        }
        if self.z_index != 0 {
            n = n.with_z(self.z_index);
        }
        n
    }

    // ---- text ----

    /// Draw text (`ctx.fillText`). `(x, y)` is the anchor, decided by `self.font`'s
    /// `align` / `baseline` (default: first-line baseline).
    #[must_use]
    pub fn fill_text(&self, x: f64, y: f64, text: impl Into<String>) -> SceneNode {
        let mut style = self.font.clone();
        style.color = self.text_color();
        self.node(Element::text(text, Point::new(x, y), style))
    }

    /// Draw text clipped to `max_width` (`ctx.fillText`'s fourth argument).
    #[must_use]
    pub fn fill_text_max_width(
        &self,
        x: f64,
        y: f64,
        text: impl Into<String>,
        max_width: f64,
    ) -> SceneNode {
        self.clone().with_max_width(max_width).fill_text(x, y, text)
    }

    /// Draw **rich text** from styled spans.
    ///
    /// Superset of Canvas, whose `fillText` handles a single style. The block-level
    /// `align` / `baseline` / `rotation` / `max_width` come from `self.font`; per-span styling
    /// (color / size / family / weight / decoration) comes from each [`RichSpan`].
    #[must_use]
    pub fn fill_rich_text(&self, x: f64, y: f64, spans: Vec<RichSpan>) -> SceneNode {
        let mut style = self.font.clone();
        // The block style carries positioning only; `color` is a per-span concern here, so keep
        // the context color as the base for spans that inherit it.
        style.color = self.text_color();
        // `rich_text` typesets eagerly, so SVG and raster agree on the block size and anchor.
        self.node(Element::rich_text(spans, Point::new(x, y), style))
    }

    /// Measure text (`ctx.measureText`), typeset with `self.font`.
    ///
    /// Returns the full [`TextMeasure`]: `.size` is the block size, `.metrics` the canvas-style
    /// metrics, and `.layout` a reusable pre-computed layout that can be fed back into
    /// [`Element::Text`] to skip a second typeset pass.
    #[must_use]
    pub fn measure_text(&self, text: impl AsRef<str>) -> TextMeasure {
        self.measure_rich_text(std::slice::from_ref(&RichSpan::new(
            text.as_ref(),
            self.font.clone(),
        )))
    }

    /// Measure rich text (styled spans), honouring `self.font.max_width`.
    #[must_use]
    pub fn measure_rich_text(&self, spans: &[RichSpan]) -> TextMeasure {
        measure_text(spans, self.font.max_width)
    }

    /// Convenience: block width / height only (the common `measureText().width` case).
    #[must_use]
    pub fn measure_size(&self, text: impl AsRef<str>) -> Size {
        self.measure_text(text).size
    }

    // ---- images ----

    /// Draw an image scaled into the target rect (`ctx.drawImage` with dw/dh).
    #[must_use]
    pub fn draw_image(&self, image: SceneImage, x: f64, y: f64, w: f64, h: f64) -> SceneNode {
        self.node(Element::Image {
            image,
            frame: Rect::new(x, y, x + w, y + h),
            opacity: self.alpha,
        })
    }

    /// Draw an image at its **original pixel size**, top-left at `(x, y)`.
    #[must_use]
    pub fn draw_image_at_size(&self, image: SceneImage, x: f64, y: f64) -> SceneNode {
        let (w, h) = (image.width() as f64, image.height() as f64);
        self.draw_image(image, x, y, w, h)
    }

    // ---- shapes: rectangle ----

    /// Filled rectangle (`fillRect`). `(x, y)` is the top-left corner, `w`/`h` are width/height.
    #[must_use]
    pub fn fill_rect(&self, x: f64, y: f64, w: f64, h: f64) -> SceneNode {
        self.node(Element::rect(
            Rect::new(x, y, x + w, y + h),
            FillStrokeStyle {
                fill: self.fill.clone(),
                stroke: None,
            },
        ))
    }

    /// Stroked rectangle (`strokeRect`). The stroke is centered on the edge (Canvas semantics).
    #[must_use]
    pub fn stroke_rect(&self, x: f64, y: f64, w: f64, h: f64) -> SceneNode {
        self.node(Element::rect(
            Rect::new(x, y, x + w, y + h),
            FillStrokeStyle {
                fill: None,
                stroke: Some(self.stroke_or_default()),
            },
        ))
    }

    /// Filled **and** stroked rectangle (Canvas `fillRect` + `strokeRect`).
    #[must_use]
    pub fn fill_stroke_rect(&self, x: f64, y: f64, w: f64, h: f64) -> SceneNode {
        self.node(Element::rect(
            Rect::new(x, y, x + w, y + h),
            self.fill_stroke(),
        ))
    }

    /// Rounded rectangle (fill + optional stroke). `radius` is the uniform corner radius.
    #[must_use]
    pub fn round_rect(&self, x: f64, y: f64, w: f64, h: f64, radius: f64) -> SceneNode {
        self.node(Element::RoundedRect {
            rect: Rect::new(x, y, x + w, y + h),
            radius,
            style: self.fill_stroke(),
        })
    }

    // ---- shapes: circle / ellipse ----

    /// Filled circle (`arc` full turn + `fill`). `(x, y)` is the center, `r` is the radius.
    #[must_use]
    pub fn fill_circle(&self, x: f64, y: f64, r: f64) -> SceneNode {
        self.node(Element::circle(
            Point::new(x, y),
            r,
            FillStrokeStyle {
                fill: self.fill.clone(),
                stroke: None,
            },
        ))
    }

    /// Stroked circle (`arc` full turn + `stroke`).
    #[must_use]
    pub fn stroke_circle(&self, x: f64, y: f64, r: f64) -> SceneNode {
        self.node(Element::circle(
            Point::new(x, y),
            r,
            FillStrokeStyle {
                fill: None,
                stroke: Some(self.stroke_or_default()),
            },
        ))
    }

    /// Filled ellipse. `(x, y)` is the center, `rx`/`ry` are the two semi-axes, `rotation` is
    /// the rotation about the center (radians).
    #[must_use]
    pub fn fill_ellipse(&self, x: f64, y: f64, rx: f64, ry: f64, rotation: f64) -> SceneNode {
        self.node(Element::ellipse(
            Point::new(x, y),
            Vec2::new(rx, ry),
            rotation,
            self.fill_stroke(),
        ))
    }

    /// Pie sector: `start_angle` / `sweep_angle` in radians, positive = clockwise (y-down).
    ///
    /// Superset of Canvas, where a pie is `moveTo(center) + arc + closePath`.
    #[must_use]
    pub fn pie(&self, cx: f64, cy: f64, r: f64, start_angle: f64, sweep_angle: f64) -> SceneNode {
        self.node(Element::Pie {
            center: Point::new(cx, cy),
            radius: r,
            start_angle,
            sweep_angle,
            style: self.fill_stroke(),
        })
    }

    /// Open arc (stroke only), same angle convention as [`Ctx::pie`].
    #[must_use]
    pub fn arc(
        &self,
        cx: f64,
        cy: f64,
        rx: f64,
        ry: f64,
        start_angle: f64,
        sweep_angle: f64,
    ) -> SceneNode {
        self.node(Element::Arc {
            center: Point::new(cx, cy),
            radii: Vec2::new(rx, ry),
            start_angle,
            sweep_angle,
            style: self.stroke_or_default(),
        })
    }

    // ---- shapes: line / polyline / polygon ----

    /// Line segment (`beginPath + moveTo + lineTo + stroke`).
    #[must_use]
    pub fn line(&self, x0: f64, y0: f64, x1: f64, y1: f64) -> SceneNode {
        self.node(Element::line(
            Point::new(x0, y0),
            Point::new(x1, y1),
            self.stroke_or_default(),
        ))
    }

    /// Polyline (`polyline`, stroke only). `pts` is the `(x, y)` sequence.
    #[must_use]
    pub fn polyline(&self, pts: &[(f64, f64)]) -> SceneNode {
        self.node(Element::poly(to_points(pts), self.stroke_or_default()))
    }

    /// Closed polygon (fill + optional stroke).
    #[must_use]
    pub fn polygon(&self, pts: &[(f64, f64)]) -> SceneNode {
        self.node(Element::polygon(to_points(pts), self.fill_stroke()))
    }

    // ---- shapes: Path2D ----

    /// Fill a [`Path2D`] (`beginPath + … + fill`).
    ///
    /// The path is filled regardless of whether it is closed (Canvas non-zero winding).
    #[must_use]
    pub fn fill(&self, path: &Path2D) -> SceneNode {
        self.node(Element::Path {
            path: path.path.clone(),
            style: FillStrokeStyle {
                fill: self.fill.clone(),
                stroke: None,
            },
            closed: true,
        })
    }

    /// Stroke a [`Path2D`] (`beginPath + … + stroke`).
    #[must_use]
    pub fn stroke(&self, path: &Path2D) -> SceneNode {
        self.node(Element::Path {
            path: path.path.clone(),
            style: FillStrokeStyle {
                fill: None,
                stroke: Some(self.stroke_or_default()),
            },
            closed: path.closed,
        })
    }

    /// Fill **and** stroke a [`Path2D`].
    #[must_use]
    pub fn fill_stroke_path(&self, path: &Path2D) -> SceneNode {
        self.node(Element::Path {
            path: path.path.clone(),
            style: self.fill_stroke(),
            closed: path.closed,
        })
    }

    /// Bézier path from an existing [`BezPath`]. `closed` decides whether to close before fill.
    #[must_use]
    pub fn path(&self, path: BezPath, closed: bool) -> SceneNode {
        self.node(Element::Path {
            path,
            style: self.fill_stroke(),
            closed,
        })
    }

    /// Path with a gradient fill.
    #[must_use]
    pub fn gradient_path(
        &self,
        path: BezPath,
        gradient: crate::scene::LinearGradient,
    ) -> SceneNode {
        self.node(Element::GradientPath {
            path,
            gradient,
            stroke: self.stroke.clone(),
        })
    }

    // ---- composition ----

    /// Group already-built nodes into one node, carrying this context's node attributes.
    ///
    /// The declarative equivalent of a Canvas `save()` … `restore()` block: everything inside
    /// shares one transform / alpha / clip.
    #[must_use]
    pub fn group(&self, children: Vec<SceneNode>) -> SceneNode {
        self.node(Element::Group { children })
    }

    /// Build a named [`Layer`] carrying this context's transform and alpha, then push nodes via
    /// [`Layer::push`] (which accepts anything the `Ctx` produces).
    #[must_use]
    pub fn layer(&self, name: impl Into<String>) -> Layer {
        let mut l = Layer::new(name);
        if let Some(t) = self.transform {
            l = l.with_transform(t);
        }
        if (self.alpha - 1.0).abs() > f64::EPSILON {
            l = l.with_opacity(self.alpha);
        }
        l
    }
}

/// First stop color of a gradient, used as a stand-in "solid" color where one is required.
fn first_stop_color(stops: &[crate::scene::GradientStop]) -> Option<Color> {
    stops.iter().map(|s| s.color).next()
}

fn to_points(pts: &[(f64, f64)]) -> Vec<Point> {
    pts.iter().map(|&(x, y)| Point::new(x, y)).collect()
}

/// A mutable path accumulator, mirroring Canvas's `Path2D` / `beginPath` … `fill()` workflow.
///
/// Unlike the rest of this module, `Path2D` is **mutable** — accumulation is its whole purpose,
/// and an immutable chain would be awkward. It is consumed (not mutated) by
/// [`Ctx::fill`] / [`Ctx::stroke`] / [`Ctx::fill_stroke_path`].
///
/// ```
/// use lievisual::builder::{Ctx, Path2D};
/// let mut p = Path2D::new();
/// p.rect(0.0, 0.0, 10.0, 10.0);
/// let node = Ctx::new().stroke(&p);
/// ```
#[derive(Debug, Clone, Default)]
pub struct Path2D {
    path: BezPath,
    started: bool,
    closed: bool,
}

impl Path2D {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Build from an existing Bézier path.
    #[must_use]
    pub fn from_bezpath(path: BezPath) -> Self {
        // A path ending in ClosePath is already closed; mirror that in `is_closed()` so
        // `Ctx::fill`/`fill_stroke_path` propagate the right `closed` flag.
        let closed = path
            .elements()
            .last()
            .is_some_and(|el| matches!(el, kurbo::PathEl::ClosePath));
        Self {
            started: !path.elements().is_empty(),
            path,
            closed,
        }
    }

    /// The accumulated path.
    #[must_use]
    pub fn as_bezpath(&self) -> &BezPath {
        &self.path
    }

    #[must_use]
    pub fn into_bezpath(self) -> BezPath {
        self.path
    }

    /// Whether the path was closed by [`Path2D::close_path`].
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.closed
    }

    /// Start a new sub-path at `(x, y)`.
    pub fn move_to(&mut self, x: f64, y: f64) -> &mut Self {
        self.path.move_to((x, y));
        self.started = true;
        self.closed = false;
        self
    }

    /// Straight line to `(x, y)`. On an empty path this behaves like `moveTo` (Canvas semantics).
    pub fn line_to(&mut self, x: f64, y: f64) -> &mut Self {
        if self.started {
            self.path.line_to((x, y));
        } else {
            self.path.move_to((x, y));
            self.started = true;
        }
        self
    }

    /// Horizontal line to `x` (`h`-style shortcut).
    pub fn hline_to(&mut self, x: f64) -> &mut Self {
        let y = self.current_point().map(|p| p.y).unwrap_or(0.0);
        self.line_to(x, y)
    }

    /// Vertical line to `y`.
    pub fn vline_to(&mut self, y: f64) -> &mut Self {
        let x = self.current_point().map(|p| p.x).unwrap_or(0.0);
        self.line_to(x, y)
    }

    /// Quadratic Bézier to `(x, y)` with control point `(cx, cy)`.
    pub fn quad_to(&mut self, cx: f64, cy: f64, x: f64, y: f64) -> &mut Self {
        if !self.started {
            self.path.move_to((cx, cy));
            self.started = true;
        }
        self.path.quad_to((cx, cy), (x, y));
        self
    }

    /// Cubic Bézier to `(x, y)` with control points `(c1x, c1y)` and `(c2x, c2y)`.
    pub fn curve_to(
        &mut self,
        c1x: f64,
        c1y: f64,
        c2x: f64,
        c2y: f64,
        x: f64,
        y: f64,
    ) -> &mut Self {
        if !self.started {
            self.path.move_to((c1x, c1y));
            self.started = true;
        }
        self.path.curve_to((c1x, c1y), (c2x, c2y), (x, y));
        self
    }

    /// Canvas alias of [`Path2D::curve_to`] (`bezierCurveTo`).
    pub fn bezier_curve_to(
        &mut self,
        c1x: f64,
        c1y: f64,
        c2x: f64,
        c2y: f64,
        x: f64,
        y: f64,
    ) -> &mut Self {
        self.curve_to(c1x, c1y, c2x, c2y, x, y)
    }

    /// Circular arc: center `(cx, cy)`, `radius`, from `start_angle` to `end_angle` (radians),
    /// sweeping **clockwise** in the y-down coordinate system.
    ///
    /// A connecting line is emitted first when the path already has a current point (Canvas
    /// semantics). Use [`Path2D::arc_ccw`] for the opposite direction.
    pub fn arc(
        &mut self,
        cx: f64,
        cy: f64,
        radius: f64,
        start_angle: f64,
        end_angle: f64,
    ) -> &mut Self {
        self.ellipse(cx, cy, radius, radius, 0.0, start_angle, end_angle)
    }

    /// Counter-clockwise variant of [`Path2D::arc`].
    pub fn arc_ccw(
        &mut self,
        cx: f64,
        cy: f64,
        radius: f64,
        start_angle: f64,
        end_angle: f64,
    ) -> &mut Self {
        // Sweeping the other way round covers the complementary arc.
        self.ellipse(
            cx,
            cy,
            radius,
            radius,
            0.0,
            start_angle,
            end_angle - std::f64::consts::TAU,
        )
    }

    /// Elliptical arc: center `(cx, cy)`, semi-axes `rx`/`ry`, `rotation` (radians), from
    /// `start_angle` to `end_angle`.
    ///
    /// The arc is approximated with cubic Béziers — at most one per 90°, which keeps the error
    /// below ~0.03% of the radius while producing far fewer segments than a polyline.
    ///
    /// The arity mirrors Canvas's `ctx.ellipse(x, y, radiusX, radiusY, rotation, startAngle,
    /// endAngle)` on purpose, so browser code ports over without reshaping arguments.
    #[allow(clippy::too_many_arguments)]
    pub fn ellipse(
        &mut self,
        cx: f64,
        cy: f64,
        rx: f64,
        ry: f64,
        rotation: f64,
        start_angle: f64,
        end_angle: f64,
    ) -> &mut Self {
        let sweep = end_angle - start_angle;
        if !sweep.is_finite() || sweep == 0.0 || !rx.is_finite() || !ry.is_finite() {
            return self;
        }
        let (rx, ry) = (rx.abs(), ry.abs());
        let n = ((sweep.abs() / std::f64::consts::FRAC_PI_2).ceil() as usize).max(1);
        let step = sweep / n as f64;
        // Standard 90°-max cubic approximation factor.
        let alpha = (4.0 / 3.0) * (step / 4.0).tan();
        let (sin_r, cos_r) = rotation.sin_cos();
        let center = Point::new(cx, cy);
        let at = |theta: f64| -> Point {
            let (s, c) = theta.sin_cos();
            Point::new(
                center.x + rx * c * cos_r - ry * s * sin_r,
                center.y + rx * c * sin_r + ry * s * cos_r,
            )
        };
        let tangent = |theta: f64| -> Point {
            let (s, c) = theta.sin_cos();
            Point::new(
                -rx * s * cos_r - ry * c * sin_r,
                -rx * s * sin_r + ry * c * cos_r,
            )
        };

        // Canvas emits a connecting line from the current point to the arc's start.
        let first = at(start_angle);
        if self.started {
            self.path.line_to(first);
        } else {
            self.path.move_to(first);
            self.started = true;
        }
        let mut t = start_angle;
        for _ in 0..n {
            let p0 = at(t);
            let p3 = at(t + step);
            let d0 = tangent(t);
            let d3 = tangent(t + step);
            let c1 = Point::new(p0.x + alpha * d0.x, p0.y + alpha * d0.y);
            let c2 = Point::new(p3.x - alpha * d3.x, p3.y - alpha * d3.y);
            self.path.curve_to(c1, c2, p3);
            t += step;
        }
        self
    }

    /// Add a rectangle as a closed sub-path (independent of the current point).
    pub fn rect(&mut self, x: f64, y: f64, w: f64, h: f64) -> &mut Self {
        self.path.move_to((x, y));
        self.path.line_to((x + w, y));
        self.path.line_to((x + w, y + h));
        self.path.line_to((x, y + h));
        self.path.close_path();
        self.started = true;
        self
    }

    /// Add a rounded rectangle as a closed sub-path.
    pub fn round_rect(&mut self, x: f64, y: f64, w: f64, h: f64, radius: f64) -> &mut Self {
        let r = kurbo::RoundedRect::new(x, y, x + w, y + h, radius);
        self.path.extend(r.to_path(0.1));
        self.started = true;
        self
    }

    /// Add a full circle as a closed sub-path.
    pub fn circle(&mut self, cx: f64, cy: f64, r: f64) -> &mut Self {
        self.path
            .extend(kurbo::Circle::new((cx, cy), r).to_path(0.1));
        self.started = true;
        self
    }

    /// Append another path, optionally transformed.
    pub fn add_path(&mut self, other: &Path2D, transform: Option<Transform>) -> &mut Self {
        let els: Vec<_> = other.path.elements().to_vec();
        let mut buf = BezPath::from_vec(els);
        if let Some(t) = transform {
            buf.apply_affine(t.to_kurbo());
        }
        self.path.extend(buf);
        self.started = self.started || other.started;
        self
    }

    /// Close the current sub-path.
    pub fn close_path(&mut self) -> &mut Self {
        self.path.close_path();
        self.closed = true;
        self
    }

    /// The last point of the current sub-path, if any.
    fn current_point(&self) -> Option<Point> {
        // The final element of a finished sub-path carries the end point.
        self.path.elements().iter().rev().find_map(|el| match el {
            kurbo::PathEl::MoveTo(p)
            | kurbo::PathEl::LineTo(p)
            | kurbo::PathEl::QuadTo(_, p)
            | kurbo::PathEl::CurveTo(_, _, p) => Some(*p),
            kurbo::PathEl::ClosePath => None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::{LinearGradient, ObjectFit, Scene, Stroke};

    fn default_ctx() -> Ctx {
        Ctx::new().with_fill(Color::rgb(0x33, 0x66, 0x99))
    }

    /// Text anchor: default Left/Alphabetic, `position` is the anchor; color uses the font's
    /// own color (not `with_fill`).
    #[test]
    fn fill_text_anchors_and_color() {
        let ctx =
            default_ctx().with_font(TextStyle::new(Color::rgb(0xff, 0, 0), 24.0, "sans-serif"));
        match ctx.fill_text(50.0, 50.0, "Hi").element {
            Element::Text {
                spans,
                position,
                style,
                layout,
            } => {
                assert_eq!(spans.len(), 1);
                assert_eq!(spans[0].text, "Hi");
                assert_eq!(spans[0].style.color, Color::rgb(0xff, 0, 0));
                assert_eq!(position, Point::new(50.0, 50.0));
                assert_eq!(style.font_size, 24.0);
                // 构造时即排版：两个后端共享同一份布局，避免 SVG 端用 font_size 估算。
                if let Some(l) = layout {
                    assert!(!l.lines.is_empty());
                    assert!(l.width > 0.0 && l.height > 0.0);
                }
            }
            _ => panic!("expected Text"),
        }
    }

    /// `with_text_color` changes the font color (text color) without affecting the shape fill color.
    #[test]
    fn text_color_independent_of_shape_fill() {
        let ctx = default_ctx().with_text_color(Color::rgb(0xff, 0, 0));
        match ctx.fill_text(0.0, 0.0, "x").element {
            Element::Text { spans, .. } => assert_eq!(spans[0].style.color, Color::rgb(0xff, 0, 0)),
            _ => panic!("expected Text"),
        }
        // shape fill still uses the default fill color.
        match ctx.fill_rect(0.0, 0.0, 1.0, 1.0).element {
            Element::Rect { style, .. } => {
                assert_eq!(style.fill, Some(Fill::Solid(Color::rgb(0x33, 0x66, 0x99))));
            }
            _ => panic!("expected Rect"),
        }
    }

    /// Rectangle: `(x, y)` is the top-left corner, `w`/`h` are dimensions.
    #[test]
    fn rect_builders() {
        let ctx = default_ctx();
        match ctx.fill_rect(10.0, 20.0, 30.0, 40.0).element {
            Element::Rect { rect, style } => {
                assert_eq!(rect, Rect::new(10.0, 20.0, 40.0, 60.0));
                assert!(style.fill.is_some() && style.stroke.is_none());
            }
            _ => panic!("expected Rect"),
        }
        let ctx = Ctx::new().with_stroke(Stroke::new(Color::BLACK, 2.0));
        match ctx.stroke_rect(0.0, 0.0, 5.0, 5.0).element {
            Element::Rect { rect, style } => {
                assert_eq!(rect, Rect::new(0.0, 0.0, 5.0, 5.0));
                assert_eq!(style.stroke.as_ref().unwrap().width, 2.0);
                assert!(style.fill.is_none(), "stroke_rect 不应填充");
            }
            _ => panic!("expected Rect"),
        }
    }

    /// Line / polyline / polygon / circle / ellipse.
    #[test]
    fn shape_builders() {
        let ctx = Ctx::new()
            .with_fill(Color::BLACK)
            .with_stroke(Stroke::new(Color::BLACK, 1.0));
        assert!(matches!(
            ctx.line(0.0, 0.0, 1.0, 1.0).element,
            Element::Line { .. }
        ));
        assert!(matches!(
            ctx.polyline(&[(0.0, 0.0), (1.0, 1.0), (2.0, 0.0)]).element,
            Element::Polyline { .. }
        ));
        assert!(matches!(
            ctx.polygon(&[(0.0, 0.0), (1.0, 1.0), (0.0, 1.0)]).element,
            Element::Polygon { .. }
        ));
        assert!(matches!(
            ctx.fill_circle(0.0, 0.0, 5.0).element,
            Element::Circle { .. }
        ));
        assert!(matches!(
            ctx.fill_ellipse(0.0, 0.0, 4.0, 2.0, 0.0).element,
            Element::Ellipse { .. }
        ));
    }

    /// Path: `closed` is forwarded, style takes fill+stroke.
    #[test]
    fn path_builder() {
        let ctx = Ctx::new()
            .with_fill(Color::BLACK)
            .with_stroke(Stroke::new(Color::BLACK, 1.0));
        let mut p = Path2D::new();
        p.move_to(0.0, 0.0);
        p.line_to(1.0, 0.0);
        p.line_to(1.0, 1.0);
        p.close_path();
        match ctx.path(p.into_bezpath(), true).element {
            Element::Path {
                style,
                closed,
                path,
            } => {
                assert!(closed);
                assert!(style.fill.is_some() && style.stroke.is_some());
                assert!(!path.is_empty());
            }
            _ => panic!("expected Path"),
        }
    }

    /// Measurement: width / height should be > 0.
    #[test]
    fn measure_returns_size() {
        let ctx = default_ctx().with_font(TextStyle::new(Color::BLACK, 20.0, "sans-serif"));
        let m = ctx.measure_text("Hello");
        assert!(m.size.width > 0.0 && m.size.height > 0.0);
        // Canvas-style metrics must be populated too.
        assert!(m.metrics.width > 0.0);
    }

    /// Builder output can be pushed into a Scene (SceneNode → push).
    #[test]
    fn elements_are_pushable() {
        let ctx = default_ctx();
        let mut scene = Scene::new(100.0, 100.0);
        scene.push(ctx.fill_rect(0.0, 0.0, 10.0, 10.0));
        scene.push(ctx.fill_text(5.0, 5.0, "ok"));
        assert_eq!(scene.nodes.len(), 2);
    }

    // ---- P0 新增能力 ----

    /// Path2D 终结方法：fill / stroke 必须能单独使用（此前 `path()` 强制 fill+stroke）。
    #[test]
    fn path2d_fill_and_stroke_are_independent() {
        let ctx = Ctx::new()
            .with_fill(Color::RED)
            .with_stroke(Stroke::new(Color::BLACK, 2.0));
        let mut p = Path2D::new();
        p.move_to(0.0, 0.0);
        p.line_to(10.0, 0.0);
        p.line_to(10.0, 10.0);

        let filled = ctx.fill(&p).element;
        match &filled {
            Element::Path { style, .. } => {
                assert!(style.fill.is_some(), "fill 应带填充");
                assert!(style.stroke.is_none(), "fill 不应描边");
            }
            _ => panic!("expected Path"),
        }
        let stroked = ctx.stroke(&p).element;
        match &stroked {
            Element::Path { style, .. } => {
                assert!(style.fill.is_none(), "stroke 不应填充");
                assert_eq!(style.stroke.as_ref().unwrap().width, 2.0);
            }
            _ => panic!("expected Path"),
        }
        let both = ctx.fill_stroke_path(&p).element;
        match &both {
            Element::Path { style, .. } => {
                assert!(style.fill.is_some() && style.stroke.is_some());
            }
            _ => panic!("expected Path"),
        }
    }

    /// Path2D 累积：move_to / line_to / curve_to / arc / close_path。
    #[test]
    fn path2d_accumulates_segments() {
        let mut p = Path2D::new();
        p.move_to(0.0, 0.0);
        p.line_to(10.0, 0.0);
        p.quad_to(15.0, 5.0, 10.0, 10.0);
        p.curve_to(5.0, 15.0, 0.0, 15.0, 0.0, 10.0);
        p.close_path();
        assert!(p.is_closed());
        let els = p.as_bezpath().elements();
        assert!(
            els.iter()
                .any(|e| matches!(e, kurbo::PathEl::CurveTo(_, _, _))),
            "应包含三次贝塞尔段"
        );
        assert!(
            els.iter().any(|e| matches!(e, kurbo::PathEl::QuadTo(_, _))),
            "应包含二次贝塞尔段"
        );
        assert!(els.iter().any(|e| matches!(e, kurbo::PathEl::ClosePath)));
    }

    /// 空 Path2D 上的 line_to 等价于 move_to（Canvas 语义），不应 panic。
    #[test]
    fn path2d_line_to_on_empty_path_starts_subpath() {
        let mut p = Path2D::new();
        p.line_to(5.0, 5.0);
        assert!(!p.as_bezpath().elements().is_empty());
        // 后续 hline/vline 应基于当前点。
        p.hline_to(10.0);
        p.vline_to(0.0);
        let els = p.as_bezpath().elements();
        assert_eq!(els.len(), 3, "got {els:?}");
    }

    /// `arc` 用贝塞尔近似：四分之一圆应产生少量曲线段且端点正确。
    #[test]
    fn path2d_arc_generates_bezier_segments() {
        let mut p = Path2D::new();
        p.arc(0.0, 0.0, 10.0, 0.0, std::f64::consts::FRAC_PI_2);
        let els = p.as_bezpath().elements().to_vec();
        // 起点 (10,0) 由 move_to/line_to 给出，随后 1 段曲线（90° 以内一段）。
        assert!(
            els.iter()
                .any(|e| matches!(e, kurbo::PathEl::CurveTo(_, _, _))),
            "应生成贝塞尔段: {els:?}"
        );
        // 终点应接近 (0, 10)（y-down）。
        let last = els.iter().rev().find_map(|e| match e {
            kurbo::PathEl::CurveTo(_, _, p) | kurbo::PathEl::LineTo(p) => Some(*p),
            _ => None,
        });
        assert!(last.is_some());
        let last = last.unwrap();
        assert!(
            (last.x - 0.0).abs() < 1e-9 && (last.y - 10.0).abs() < 1e-9,
            "got {last:?}"
        );
    }

    /// 节点级属性：transform / alpha / clip / name / z 应落到产出的 node 上。
    #[test]
    fn node_attributes_are_applied() {
        let ctx = Ctx::new()
            .with_fill(Color::BLACK)
            .with_alpha(0.5)
            .with_name("box")
            .with_z(3)
            .with_clip(Clip::rect(Rect::new(0.0, 0.0, 10.0, 10.0)));
        let n = ctx.fill_rect(0.0, 0.0, 5.0, 5.0);
        assert_eq!(n.opacity, 0.5);
        assert_eq!(n.name.as_deref(), Some("box"));
        assert_eq!(n.z_index, 3);
        assert!(n.clip.is_some());
        assert!(n.transform.is_none());

        // transform 也应带上。
        let n = ctx
            .clone()
            .translate(10.0, 20.0)
            .fill_rect(0.0, 0.0, 5.0, 5.0);
        let t = n.transform.unwrap();
        let p = t.apply_point(Point::new(1.0, 1.0));
        assert_eq!(p, Point::new(11.0, 21.0));
    }

    /// translate / rotate / scale 应**复合**（而非互相覆盖）。
    #[test]
    fn transforms_compose() {
        let ctx = Ctx::new().translate(10.0, 0.0).scale(2.0);
        let t = ctx.fill_rect(0.0, 0.0, 1.0, 1.0).transform.unwrap();
        let p = t.apply_point(Point::new(1.0, 1.0));
        assert_eq!(p, Point::new(12.0, 2.0), "先平移再缩放 → (1+10)*2");
    }

    /// 细粒度描边设置：只改其中一项不应重置其它项。
    #[test]
    fn fine_grained_stroke_settings() {
        let ctx = Ctx::new()
            .with_stroke(Stroke::new(Color::RED, 3.0))
            .with_line_cap(crate::scene::LineCap::Round)
            .with_line_join(crate::scene::LineJoin::Bevel)
            .with_miter_limit(4.0)
            .with_dash(vec![2.0, 1.0], 0.5);

        let s = ctx.stroke.clone().unwrap();
        assert_eq!(s.color, Color::RED);
        assert_eq!(s.width, 3.0);
        assert_eq!(s.line_cap, crate::scene::LineCap::Round);
        assert_eq!(s.line_join, crate::scene::LineJoin::Bevel);
        assert_eq!(s.miter_limit, 4.0);
        assert_eq!(s.dash_array, vec![2.0, 1.0]);
        assert_eq!(s.dash_offset, 0.5);

        // 只改宽度：其它项保留。
        let s2 = ctx.with_line_width(7.0).stroke.unwrap();
        assert_eq!(s2.width, 7.0);
        assert_eq!(s2.color, Color::RED);
        assert_eq!(s2.line_cap, crate::scene::LineCap::Round);

        // 无 stroke 时 with_line_width 应基于填充色创建默认 stroke。
        let s3 = Ctx::new()
            .with_fill(Color::BLUE)
            .with_line_width(2.0)
            .stroke
            .unwrap();
        assert_eq!(s3.color, Color::BLUE);
        assert_eq!(s3.width, 2.0);
    }

    /// 渐变可作为填充（Canvas 需要额外的 gradient 对象，这里是直接 Fill）。
    #[test]
    fn gradient_fill() {
        let g = LinearGradient {
            start: Point::new(0.0, 0.0),
            end: Point::new(100.0, 0.0),
            stops: vec![crate::scene::GradientStop {
                offset: 0.0,
                color: Color::RED,
            }],
        };
        let ctx = Ctx::new().with_fill_gradient(g.clone());
        match ctx.fill_rect(0.0, 0.0, 10.0, 10.0).element {
            Element::Rect { style, .. } => {
                assert!(matches!(style.fill, Some(Fill::LinearGradient(_))));
            }
            _ => panic!("expected Rect"),
        }
        // 渐变无"单一颜色"时，回退用首个 stop 作为描边色。
        let s = Ctx::new()
            .with_fill_gradient(g)
            .with_line_width(1.0)
            .stroke
            .unwrap();
        assert_eq!(s.color, Color::RED);
    }

    /// 富文本：每个 span 带自己的样式。
    #[test]
    fn rich_text_spans_keep_their_styles() {
        let ctx = Ctx::new().with_font(TextStyle::new(Color::BLACK, 12.0, "sans-serif"));
        let spans = vec![
            RichSpan::new("bold ", TextStyle::new(Color::RED, 20.0, "sans-serif")),
            RichSpan::new("plain", TextStyle::new(Color::BLUE, 12.0, "sans-serif")),
        ];
        match ctx.fill_rich_text(0.0, 0.0, spans).element {
            Element::Text { spans, .. } => {
                assert_eq!(spans.len(), 2);
                assert_eq!(spans[0].style.color, Color::RED);
                assert_eq!(spans[0].style.font_size, 20.0);
                assert_eq!(spans[1].style.color, Color::BLUE);
            }
            _ => panic!("expected Text"),
        }
        // 富文本测量应能跑通。
        let spans = vec![RichSpan::new(
            "hello",
            TextStyle::new(Color::BLACK, 12.0, "sans-serif"),
        )];
        assert!(ctx.measure_rich_text(&spans).size.width >= 0.0);
    }

    /// 图片：draw_image 缩放，draw_image_at_size 用原始像素尺寸。
    #[test]
    fn draw_image_builders() {
        let img = SceneImage::from_rgba8(4, 2, vec![0u8; 4 * 2 * 4]).unwrap();
        let ctx = Ctx::new();
        match ctx.draw_image(img.clone(), 1.0, 2.0, 100.0, 50.0).element {
            Element::Image { frame, .. } => {
                assert_eq!(frame, Rect::new(1.0, 2.0, 101.0, 52.0));
            }
            _ => panic!("expected Image"),
        }
        match ctx.draw_image_at_size(img, 1.0, 2.0).element {
            Element::Image { frame, .. } => {
                assert_eq!(frame, Rect::new(1.0, 2.0, 5.0, 4.0), "应为原始像素尺寸");
            }
            _ => panic!("expected Image"),
        }
    }

    /// 图片透明度跟随 `with_alpha`。
    #[test]
    fn image_opacity_follows_alpha() {
        let img = SceneImage::from_rgba8(1, 1, vec![0u8; 4]).unwrap();
        let node = Ctx::new().with_alpha(0.4).draw_image_at_size(img, 0.0, 0.0);
        assert_eq!(node.opacity, 0.4);
        match node.element {
            Element::Image { opacity, .. } => assert!((opacity - 0.4).abs() < 1e-9),
            _ => panic!("expected Image"),
        }
    }

    /// group：子节点共享 ctx 的 transform / alpha / clip。
    #[test]
    fn group_carries_context_attributes() {
        let ctx = Ctx::new().with_alpha(0.5).translate(1.0, 1.0);
        let child = ctx.fill_rect(0.0, 0.0, 1.0, 1.0);
        let g = ctx.group(vec![child]);
        assert_eq!(g.opacity, 0.5);
        assert!(g.transform.is_some());
        match g.element {
            Element::Group { children } => assert_eq!(children.len(), 1),
            _ => panic!("expected Group"),
        }
    }

    /// layer：Ctx 产出的节点可直接 push 进 layer。
    #[test]
    fn layer_accepts_ctx_output() {
        let ctx = Ctx::new().with_fill(Color::BLACK);
        let mut layer = ctx.clone().with_alpha(0.5).layer("grid");
        layer.push(ctx.fill_rect(0.0, 0.0, 1.0, 1.0));
        assert_eq!(layer.nodes.len(), 1);
        assert_eq!(layer.name, "grid");
        // Ctx 的 alpha 已作为 layer 级 opacity。
        assert_eq!(layer.opacity, 0.5);
    }

    /// Pie / Arc 助手（Canvas 需 moveTo+arc+closePath 拼装）。
    #[test]
    fn pie_and_arc_helpers() {
        let ctx = Ctx::new()
            .with_fill(Color::RED)
            .with_stroke(Stroke::new(Color::BLACK, 1.0));
        match ctx
            .pie(5.0, 5.0, 10.0, 0.0, std::f64::consts::FRAC_PI_2)
            .element
        {
            Element::Pie { center, radius, .. } => {
                assert_eq!(center, Point::new(5.0, 5.0));
                assert_eq!(radius, 10.0);
            }
            _ => panic!("expected Pie"),
        }
        match ctx
            .arc(5.0, 5.0, 10.0, 20.0, 0.0, std::f64::consts::FRAC_PI_2)
            .element
        {
            Element::Arc { center, radii, .. } => {
                assert_eq!(center, Point::new(5.0, 5.0));
                assert_eq!(radii, Vec2::new(10.0, 20.0));
            }
            _ => panic!("expected Arc"),
        }
    }

    /// round_rect 助手。
    #[test]
    fn round_rect_helper() {
        let ctx = Ctx::new().with_fill(Color::BLACK);
        match ctx.round_rect(1.0, 2.0, 3.0, 4.0, 0.5).element {
            Element::RoundedRect { rect, radius, .. } => {
                assert_eq!(rect, Rect::new(1.0, 2.0, 4.0, 6.0));
                assert_eq!(radius, 0.5);
            }
            _ => panic!("expected RoundedRect"),
        }
    }

    /// `without_fill` / `without_stroke` 能关掉对应部分。
    #[test]
    fn without_fill_or_stroke() {
        let ctx = Ctx::new()
            .with_fill(Color::RED)
            .with_stroke(Stroke::new(Color::BLUE, 2.0));
        match ctx
            .clone()
            .without_fill()
            .fill_stroke_rect(0.0, 0.0, 1.0, 1.0)
            .element
        {
            Element::Rect { style, .. } => {
                assert!(style.fill.is_none() && style.stroke.is_some())
            }
            _ => panic!("expected Rect"),
        }
        match ctx
            .without_stroke()
            .fill_stroke_rect(0.0, 0.0, 1.0, 1.0)
            .element
        {
            Element::Rect { style, .. } => {
                assert!(style.fill.is_some() && style.stroke.is_none())
            }
            _ => panic!("expected Rect"),
        }
    }

    /// object_fit 可通过 SceneImage 自身设置（与 Ctx 正交）。
    #[test]
    fn object_fit_is_orthogonal() {
        let img = SceneImage::from_rgba8(1, 1, vec![0u8; 4])
            .unwrap()
            .with_object_fit(ObjectFit::Cover);
        match Ctx::new().draw_image_at_size(img, 0.0, 0.0).element {
            Element::Image { image, .. } => assert_eq!(image.object_fit, ObjectFit::Cover),
            _ => panic!("expected Image"),
        }
    }
}
