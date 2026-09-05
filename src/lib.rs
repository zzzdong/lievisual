//! # lievisual
//!
//! Declarative visual scene (IR) + pluggable rendering backends.
//!
//! ## Design overview
//! - **IR / backend separation**: [`scene`] describes *what* to draw (primitives + layered
//!   `z_index` + transforms + node attributes), while [`render::Renderer`] describes *how*.
//!   Adding a backend (SVG / vello_cpu / GPU) requires no changes to the IR.
//! - **Unified layering and transforms**: `z_index` / `Transform` are promoted to fields of
//!   [`scene::SceneNode`]; node-level `opacity` / `name` / `visible` / `clip` also belong to the
//!   node (recursively applied to a Group subtree), so new primitive variants need no field churn.
//! - **Layers**: [`scene::Scene`] may carry an ordered set of [`scene::Layer`] (bottom to top)
//!   that controls `visible` / `opacity` / `transform` / `name` for the whole layer; nodes inside
//!   are then stably sorted by `z_index`. `nodes` is kept as the default (bottom-most) layer.
//! - **Rich primitives**: [`scene::Element`] provides rectangles, circles, ellipses, rounded
//!   rectangles, lines, polylines, polygons, arcs, sectors, paths, gradient paths, text, and
//!   groups — covering common chart / flowchart / Markdown needs.
//! - **Varied fills**: solid color, linear gradient, radial gradient ([`scene::Fill`]); strokes
//!   support line caps / joins, dashes (`dash_array` / `dash_offset`), and `miter_limit`.
//! - **Hand-drawn style (opt-in)**: [`sketch`] is a post-pass that turns selected elements into
//!   rough, pen-drawn outlines plus [`scene::Fill::Sketch`] fills. It never runs on its own: call
//!   [`sketch::roughen_scene`] (or the selective [`sketch::roughen_scene_if`]) to enable it, and
//!   pick which element kinds participate with [`sketch::SketchOptions::kinds`].
//! - **Text (rich-text core)**: [`scene::Element::Text`] is built around styled spans
//!   ([`text::RichSpan`]); typesetting / measurement go through the `&[RichSpan]` entry points
//!   ([`text::measure_text`] / [`text::layout_text`]); it can carry a pre-laid-out
//!   [`text::TextLayout`]. Raster backends draw glyphs precisely via layout, while the SVG backend
//!   falls back to `<text>`.
//!
//! ## Backend support (all rendering backends enabled by default, no feature gate)
//! - [`render::SvgRenderer`]: emits a vector SVG string, with scene metadata (`title` /
//!   `description` / `scale`).
//! - [`render::VelloPixmapRenderer`]: vello_cpu rasterization → PNG bytes.
//!
//! ## Geometry and coordinates
//! Geometric base types ([`Point`] / [`Rect`] / [`BezPath`] / [`Affine`], etc.) are re-exported
//! directly from kurbo (see [`geometry`]), giving zero-cost interoperability with the internal
//! geometry types of vello / parley; [`Color`] / [`Transform`] are defined by lievisual itself.

pub mod builder;
pub mod fit;
pub mod geometry;
pub mod pixmap;
pub mod render;
pub mod scene;
pub mod sketch;
pub mod text;

// Re-expose the glyph-outline extractor at the crate root (it lives under `text` now).
pub use text::glyph;

// re-export parley
pub use parley;
// re-export kurbo
pub use kurbo;

pub use fit::{FitOptions, fit_scene, scene_bounds};
pub use geometry::{
    Affine, Arc, ArcAppendIter, Axis, BezPath, Circle, CirclePathIter, CircleSegment, Color,
    CubicBez, CubicBezIter, CuspType, Diagonal2, Ellipse, Insets, Line, LineIntersection,
    LinePathIter, MinDistance, Moments, PathEl, PathSeg, PathSegIter, Point, PointExt, QuadBez,
    QuadBezIter, QuadSpline, Rect, RectPathIter, RoundedRect, RoundedRectPathIter,
    RoundedRectRadii, Segments, Shape, Size, Transform, TranslateScale, Triangle, TrianglePathIter,
    Vec2,
};
pub use pixmap::Pixmap;
pub use scene::{
    Clip, Element, Fill, FillStrokeStyle, GradientStop, Layer, LineCap, LineJoin, LinearGradient,
    ObjectFit, RadialGradient, Scene, SceneImage, SceneNode, SketchFill, SketchFillStyle, Stroke,
};
pub use sketch::{
    SketchKind, SketchKinds, SketchOptions, roughen_element, roughen_scene, roughen_scene_fit,
    roughen_scene_fit_if, roughen_scene_if,
};
pub use text::{
    FontSource, FontStyle, FontWeight, FontWidth, Glyph, LineMetrics, RichSpan, TextAlign,
    TextBaseline, TextDecoration, TextLayout, TextLine, TextMeasure, TextMetrics, TextRun,
    TextStyle, compute_text_offset, layout_metrics, layout_text, measure_text,
    parse_generic_family, register_font, register_font_generic,
};
