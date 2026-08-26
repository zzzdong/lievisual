//! Canvas-style convenience construction layer (builder / sugar).
//!
//! This module provides an optional layer of sugar on top of the lower-level declarative IR
//! ([`crate::scene`] / [`crate::text`]), following the mental model of the Web Canvas 2D API
//! (`ctx.fillStyle` / `ctx.font` / `ctx.fillText`), so that "migrating from the browser" or
//! "quickly building a diagram" feels familiar.
//!
//! ## Scope and boundaries
//! - **Pure construction**: every method **consumes an immutable [`Ctx`] and returns an
//!   [`Element`]**, performing no immediate drawing; fully compatible with the declarative IR,
//!   and the result can be pushed directly via `scene.push(...)` (auto-converted to `SceneNode`
//!   through `From<Element>`).
//! - **No mutable state**: Canvas's `ctx` is mutable, imperative state, which this layer
//!   deliberately does not simulate. `with_*` returns a new `Ctx` (builder pattern); to use
//!   "multiple styles on one canvas", copy / chain-derive so they don't interfere.
//! - **No IR breakage**: all convenience methods eventually land on `Element::*` / `RichSpan`,
//!   introducing no new IR types. Its core value: text-anchor semantics (`fillText`'s
//!   `textAlign`/`textBaseline` positioning) are computed uniformly inside the renderer, and this
//!   layer just forwards, so the caller needn't compute offsets by hand.
//!
//! ## Text anchors (aligned with Canvas `fillText`)
//! The `(x, y)` of [`Ctx::fill_text`] is an **anchor**: [`TextStyle`]'s `align` / `baseline`
//! decide where the anchor lands within the text block (default `Left` + `Alphabetic`, i.e.
//! `(x,y)` is on the first-line baseline). The renderer computes the offset internally via
//! [`crate::text::compute_text_offset`], so the caller needn't position manually.
//!
//! Usage pattern:
//! ```
//! use lievisual::builder::Ctx;
//! use lievisual::geometry::{Color, Point};
//! use lievisual::scene::{Scene, Element};
//! use lievisual::text::TextStyle;
//!
//! let mut scene = Scene::new(200.0, 100.0);
//! let ctx = Ctx::new()
//!     .with_fill(Color::rgb(0x33, 0x66, 0x99))
//!     .with_font(TextStyle::new(Color::rgb(0x33, 0x66, 0x99), 24.0, "sans-serif"));
//! scene.push(ctx.fill_rect(10.0, 10.0, 80.0, 40.0));   // filled rect (uses with_fill color)
//! scene.push(ctx.fill_text(50.0, 50.0, "Hi"));          // text (uses font color, anchor = first-line baseline)
//! ```

use crate::geometry::{Color, Point, Rect, Size, Vec2};
use crate::scene::{Element, Fill, FillStrokeStyle, Stroke};
use crate::text::{TextStyle, measure_text};
use kurbo::BezPath;

/// Canvas-style construction context: holds the default fill color / stroke / font for the
/// convenience methods.
///
/// Immutable and chain-derivable (`with_*` returns a new instance); carries no drawing target.
#[derive(Debug, Clone)]
pub struct Ctx {
    /// Default fill color (Canvas `fillStyle`). Used only for **shape** (rect / circle / polygon
    /// / path) fills; text color is decided by [`Self::font`]'s `color` (see
    /// [`Ctx::fill_text`]). When `None`, shape methods skip filling and strokes fall back to the
    /// font's `color`.
    pub fill: Option<Color>,
    /// Default stroke (Canvas `strokeStyle` + `lineWidth`). When `None`, shape methods skip
    /// stroking and line methods fall back to 1px.
    pub stroke: Option<Stroke>,
    /// Default text style (Canvas `font` + `textAlign` + `textBaseline`); text color is its
    /// `color`.
    pub font: TextStyle,
}

impl Default for Ctx {
    fn default() -> Self {
        Self {
            fill: Some(Color::BLACK),
            stroke: None,
            font: TextStyle::new(Color::BLACK, 12.0, "sans-serif"),
        }
    }
}

impl Ctx {
    /// Construct the default context (black fill, no stroke, 12px sans-serif text).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the default fill color (`fillStyle`).
    #[must_use]
    pub fn with_fill(mut self, color: Color) -> Self {
        self.fill = Some(color);
        self
    }

    /// Set the default stroke (`strokeStyle` + `lineWidth`).
    #[must_use]
    pub fn with_stroke(mut self, stroke: Stroke) -> Self {
        self.stroke = Some(stroke);
        self
    }

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

    /// Text color = the font's own color (`TextStyle::color`; `with_text_color` edits this
    /// field).
    ///
    /// Unlike Canvas where `fillStyle` decides text color, lievisual folds text color into
    /// `TextStyle.color`, so text color is decoupled from the shape fill ([`Self::shape_fill`]),
    /// avoiding `with_fill` accidentally overriding text.
    fn text_color(&self) -> Color {
        self.font.color
    }

    /// Shape fill color: default fill > font's own color.
    fn shape_fill(&self) -> Color {
        self.fill.unwrap_or(self.font.color)
    }

    /// Build a `FillStrokeStyle` from the default fill + stroke.
    fn fill_stroke(&self) -> FillStrokeStyle {
        FillStrokeStyle {
            fill: self.fill.map(Fill::Solid),
            stroke: self.stroke.clone(),
        }
    }

    // ---- text ----

    /// Draw text (`ctx.fillText`). `(x, y)` is the anchor, decided by `self.font`'s
    /// `align` / `baseline` (default: first-line baseline).
    #[must_use]
    pub fn fill_text(&self, x: f64, y: f64, text: impl Into<String>) -> Element {
        let mut style = self.font.clone();
        style.color = self.text_color();
        Element::text(text, Point::new(x, y), style)
    }

    /// Measure text (`ctx.measureText`), returning width / height (typeset with `self.font`).
    #[must_use]
    pub fn measure_text(&self, text: impl AsRef<str>) -> Size {
        let style = self.font.clone();
        measure_text(
            std::slice::from_ref(&crate::text::RichSpan::new(text.as_ref(), style)),
            None,
        )
        .size
    }

    // ---- shapes: rectangle ----

    /// Filled rectangle (`fillRect`). `(x, y)` is the top-left corner, `w`/`h` are width/height.
    #[must_use]
    pub fn fill_rect(&self, x: f64, y: f64, w: f64, h: f64) -> Element {
        Element::rect(
            Rect::new(x, y, x + w, y + h),
            FillStrokeStyle::fill(self.shape_fill()),
        )
    }

    /// Stroked rectangle (`strokeRect`). The stroke is centered on the edge (Canvas semantics).
    #[must_use]
    pub fn stroke_rect(&self, x: f64, y: f64, w: f64, h: f64) -> Element {
        Element::rect(
            Rect::new(x, y, x + w, y + h),
            FillStrokeStyle::stroke(
                self.stroke
                    .as_ref()
                    .map(|s| s.color)
                    .unwrap_or(self.shape_fill()),
                self.stroke.as_ref().map(|s| s.width).unwrap_or(1.0),
            ),
        )
    }

    // ---- shapes: circle / ellipse ----

    /// Filled circle (`arc` full turn + `fill`). `(x, y)` is the center, `r` is the radius.
    #[must_use]
    pub fn fill_circle(&self, x: f64, y: f64, r: f64) -> Element {
        Element::circle(
            Point::new(x, y),
            r,
            FillStrokeStyle::fill(self.shape_fill()),
        )
    }

    /// Filled ellipse. `(x, y)` is the center, `rx`/`ry` are the two semi-axes, `rotation` is
    /// the rotation about the center (radians).
    #[must_use]
    pub fn fill_ellipse(&self, x: f64, y: f64, rx: f64, ry: f64, rotation: f64) -> Element {
        Element::ellipse(
            Point::new(x, y),
            Vec2::new(rx, ry),
            rotation,
            FillStrokeStyle::fill(self.shape_fill()),
        )
    }

    // ---- shapes: line / polyline / polygon ----

    /// Line segment (`beginPath + moveTo + lineTo + stroke`).
    #[must_use]
    pub fn line(&self, x0: f64, y0: f64, x1: f64, y1: f64) -> Element {
        Element::line(
            Point::new(x0, y0),
            Point::new(x1, y1),
            self.stroke
                .clone()
                .unwrap_or(Stroke::new(self.shape_fill(), 1.0)),
        )
    }

    /// Polyline (`polyline`, stroke only). `pts` is the `(x, y)` sequence.
    #[must_use]
    pub fn polyline(&self, pts: &[(f64, f64)]) -> Element {
        Element::poly(
            pts.iter().map(|&(x, y)| Point::new(x, y)).collect(),
            self.stroke
                .clone()
                .unwrap_or(Stroke::new(self.shape_fill(), 1.0)),
        )
    }

    /// Closed polygon (fill + optional stroke).
    #[must_use]
    pub fn polygon(&self, pts: &[(f64, f64)]) -> Element {
        Element::polygon(
            pts.iter().map(|&(x, y)| Point::new(x, y)).collect(),
            self.fill_stroke(),
        )
    }

    // ---- shapes: path ----

    /// Bézier path (`beginPath + path + fill`). `closed` decides whether to close before fill.
    #[must_use]
    pub fn path(&self, path: BezPath, closed: bool) -> Element {
        Element::Path {
            path,
            style: self.fill_stroke(),
            closed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_ctx() -> Ctx {
        Ctx::new().with_fill(Color::rgb(0x33, 0x66, 0x99))
    }

    /// Text anchor: default Left/Alphabetic, `position` is the anchor; color uses the font's
    /// own color (not `with_fill`).
    #[test]
    fn fill_text_anchors_and_color() {
        let ctx =
            default_ctx().with_font(TextStyle::new(Color::rgb(0xff, 0, 0), 24.0, "sans-serif"));
        let el = ctx.fill_text(50.0, 50.0, "Hi");
        match el {
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
                assert!(layout.is_none());
            }
            _ => panic!("expected Text"),
        }
    }

    /// `with_text_color` changes the font color (text color) without affecting the shape fill color.
    #[test]
    fn text_color_independent_of_shape_fill() {
        let ctx = default_ctx().with_text_color(Color::rgb(0xff, 0, 0));
        match ctx.fill_text(0.0, 0.0, "x") {
            Element::Text { spans, .. } => assert_eq!(spans[0].style.color, Color::rgb(0xff, 0, 0)),
            _ => panic!("expected Text"),
        }
        // shape fill still uses the default fill color.
        match ctx.fill_rect(0.0, 0.0, 1.0, 1.0) {
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
        match ctx.fill_rect(10.0, 20.0, 30.0, 40.0) {
            Element::Rect { rect, style } => {
                assert_eq!(rect, Rect::new(10.0, 20.0, 40.0, 60.0));
                assert!(style.fill.is_some() && style.stroke.is_none());
            }
            _ => panic!("expected Rect"),
        }
        let ctx = Ctx::new().with_stroke(Stroke::new(Color::BLACK, 2.0));
        match ctx.stroke_rect(0.0, 0.0, 5.0, 5.0) {
            Element::Rect { rect, style } => {
                assert_eq!(rect, Rect::new(0.0, 0.0, 5.0, 5.0));
                assert_eq!(style.stroke.as_ref().unwrap().width, 2.0);
            }
            _ => panic!("expected Rect"),
        }
    }

    /// Line / polyline / polygon / circle.
    #[test]
    fn shape_builders() {
        let ctx = Ctx::new()
            .with_fill(Color::BLACK)
            .with_stroke(Stroke::new(Color::BLACK, 1.0));
        assert!(matches!(ctx.line(0.0, 0.0, 1.0, 1.0), Element::Line { .. }));
        assert!(matches!(
            ctx.polyline(&[(0.0, 0.0), (1.0, 1.0), (2.0, 0.0)]),
            Element::Polyline { .. }
        ));
        assert!(matches!(
            ctx.polygon(&[(0.0, 0.0), (1.0, 1.0), (0.0, 1.0)]),
            Element::Polygon { .. }
        ));
        assert!(matches!(
            ctx.fill_circle(0.0, 0.0, 5.0),
            Element::Circle { .. }
        ));
        assert!(matches!(
            ctx.fill_ellipse(0.0, 0.0, 4.0, 2.0, 0.0),
            Element::Ellipse { .. }
        ));
    }

    /// Path: `closed` is forwarded, style takes fill+stroke.
    #[test]
    fn path_builder() {
        let ctx = Ctx::new()
            .with_fill(Color::BLACK)
            .with_stroke(Stroke::new(Color::BLACK, 1.0));
        let mut p = BezPath::new();
        p.move_to((0.0, 0.0));
        p.line_to((1.0, 0.0));
        p.line_to((1.0, 1.0));
        p.close_path();
        match ctx.path(p, true) {
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
        let size = ctx.measure_text("Hello");
        assert!(size.width > 0.0 && size.height > 0.0);
    }

    /// Builder output can be pushed into a Scene (Element → SceneNode auto-conversion).
    #[test]
    fn elements_are_pushable() {
        use crate::scene::Scene;
        let ctx = default_ctx();
        let mut scene = Scene::new(100.0, 100.0);
        scene.push(ctx.fill_rect(0.0, 0.0, 10.0, 10.0));
        scene.push(ctx.fill_text(5.0, 5.0, "ok"));
        assert_eq!(scene.nodes.len(), 2);
    }
}
