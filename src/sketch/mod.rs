//! Hand-drawn ("sketch") rendering — an **opt-in** post-pass over a [`Scene`].
//!
//! Nothing here runs unless you call one of the `roughen_*` functions: scenes built and rendered
//! the normal way are byte-for-byte unaffected (no new element variant is produced, no backend
//! branch is taken). Enabling the look is therefore a pure caller decision, expressed by
//! [`SketchOptions`].
//!
//! ## How it works
//!
//! ```text
//! border → IR pass rewrites geometry      (Rect/Circle/… → jittered BezPath, stroked)
//! fill   → only a *hint* is recorded      (Fill::Sketch), each backend interprets it
//! ```
//!
//! The split is deliberate: the outline is geometry, so it belongs in the IR (one pass, both
//! backends, `fit` stays correct for free); the fill is a *style*, and the two backends have
//! completely different native ways to express it — SVG has `<pattern>` (a tiny seamless tile,
//! near-zero output cost), vello has none (so it draws real hachure strokes under a clip).
//! Trying to force one representation on both would either bloat the SVG or fake the raster.
//!
//! ## Selective sketching
//!
//! Real diagrams almost never want *everything* sketched — node boxes yes, sub-graph containers
//! and grid lines no. Two mechanisms, combinable:
//!
//! ```
//! use lievisual::{Color, Point, Rect, Scene};
//! use lievisual::scene::{Element, FillStrokeStyle, Stroke};
//! use lievisual::sketch::{roughen_scene_if, SketchKind, SketchKinds, SketchOptions};
//!
//! let mut scene = Scene::new(200.0, 120.0);
//! scene.push(Element::rect(
//!     Rect::new(10.0, 10.0, 90.0, 60.0),
//!     FillStrokeStyle::stroke(Color::BLACK, 1.5),
//! ));
//! scene.push(Element::line(
//!     Point::new(0.0, 100.0),
//!     Point::new(200.0, 100.0),
//!     Stroke::new(Color::BLACK, 1.0),
//! ));
//!
//! // 只手绘矩形，连线保持笔直。
//! roughen_scene_if(
//!     &mut scene,
//!     &SketchOptions { seed: Some(42), ..Default::default() },
//!     &|node| SketchKind::of(&node.element) == Some(SketchKind::Rect),
//! );
//! ```
//!
//! [`SketchOptions::kinds`] selects by element kind; `roughen_scene_if` selects with a predicate
//! (a rejected node is skipped **with its whole sub-tree**). Text and images are never sketched
//! (see [`SketchKind`]).
//!
//! ## Reproducibility
//!
//! [`SketchOptions::seed`] drives a self-contained PRNG (mulberry32) threaded through the tree in
//! document order: same seed + same scene ⇒ point-identical geometry. Tests must always set it.

pub(crate) mod fill;
mod pass;
pub mod rng;
mod rough;

pub use pass::{
    roughen_element, roughen_scene, roughen_scene_fit, roughen_scene_fit_if, roughen_scene_if,
};
pub use rng::Rng;

// `Fill::Sketch` lives in the IR (the backends consume it), so its payload types are re-exported
// here for a single obvious import site.
pub use crate::scene::{Fill, SketchFill, SketchFillStyle};

/// Which element kinds may be sketched.
///
/// Deliberately excludes [`Element::Text`] (a hand-drawn font is an asset decision for the
/// caller's `register_font`, not something this module should invent) and [`Element::Image`]
/// (jittering a bitmap's frame would misrepresent the image; add a `Rect` next to it and sketch
/// that instead).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SketchKind {
    Rect,
    RoundedRect,
    Circle,
    Ellipse,
    Line,
    Polyline,
    Polygon,
    Arc,
    Pie,
    Path,
    /// A path with a gradient fill: the fill degrades to a hachure of the gradient's last stop.
    GradientPath,
}

impl SketchKind {
    /// The kind of `el`, or `None` when the element is never sketched.
    #[must_use]
    pub fn of(el: &crate::scene::Element) -> Option<Self> {
        use crate::scene::Element as E;
        Some(match el {
            E::Rect { .. } => Self::Rect,
            E::RoundedRect { .. } => Self::RoundedRect,
            E::Circle { .. } => Self::Circle,
            E::Ellipse { .. } => Self::Ellipse,
            E::Line { .. } => Self::Line,
            E::Polyline { .. } => Self::Polyline,
            E::Polygon { .. } => Self::Polygon,
            E::Arc { .. } => Self::Arc,
            E::Pie { .. } => Self::Pie,
            E::Path { .. } => Self::Path,
            E::GradientPath { .. } => Self::GradientPath,
            E::Text { .. } | E::Image { .. } | E::Group { .. } => return None,
        })
    }

    const fn bit(self) -> u16 {
        1 << (self as u16)
    }
}

/// A set of [`SketchKind`]s (a bitmask, so it is `Copy` and can sit inside options).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SketchKinds(u16);

impl SketchKinds {
    /// Nothing is sketched (makes a `roughen_*` call a no-op).
    pub const NONE: Self = Self(0);
    /// Every sketchable kind (the default).
    pub const ALL: Self = Self(0b0000_0111_1111_1111);

    /// A set containing exactly `kinds`.
    #[must_use]
    pub fn only(kinds: &[SketchKind]) -> Self {
        let mut s = Self::NONE;
        for k in kinds {
            s = s.with(*k);
        }
        s
    }

    /// Add a kind.
    #[must_use]
    pub fn with(self, kind: SketchKind) -> Self {
        Self(self.0 | kind.bit())
    }

    /// Remove a kind.
    #[must_use]
    pub fn without(self, kind: SketchKind) -> Self {
        Self(self.0 & !kind.bit())
    }

    /// Is `kind` in the set?
    #[must_use]
    pub fn contains(self, kind: SketchKind) -> bool {
        self.0 & kind.bit() != 0
    }

    /// Is the set empty?
    #[must_use]
    pub fn is_empty(self) -> bool {
        self.0 == 0
    }
}

impl Default for SketchKinds {
    fn default() -> Self {
        Self::ALL
    }
}

/// Options for the sketch pass.
///
/// Defaults mirror rough.js (`roughness 0.7`, `bowing 0.5`, hachure at `-41°`, gap 4, weight
/// 0.5, multi-stroke on).
#[derive(Debug, Clone)]
pub struct SketchOptions {
    /// Jitter amplitude scaling. `0.0` disables jitter entirely (crisp geometry, sketch fill).
    /// Default `0.7`.
    pub roughness: f64,
    /// How much a line *bends* (the bowing displacement perpendicular to the segment).
    /// Default `0.5`.
    pub bowing: f64,
    /// PRNG seed. `None` seeds from the wall clock (two renders differ); tests must pass `Some`.
    pub seed: Option<u64>,
    /// Draw every segment twice (the second pass at half amplitude). Default `true`.
    pub multistroke: bool,
    /// Curve sampling density: target arc length between two samples when a curve / ellipse is
    /// turned into a polyline (smaller = smoother but more jitter points). Default `12.0`.
    pub curve_step: f64,
    /// Which element kinds participate. Default [`SketchKinds::ALL`].
    pub kinds: SketchKinds,
    /// A filled shape without a stroke gets a synthesised outline in its own fill colour (so
    /// bars / sectors do not stay crisp under a hand-drawn fill). Default `true`.
    pub outline_when_unstroked: bool,
    /// Width of that synthesised outline. Default `1.0`.
    pub outline_width: f64,
    /// Turn fills into hand-drawn fills. `None` keeps the original fills (only outlines are
    /// sketched). Default `None`.
    pub fill: Option<SketchFillStyle>,
    /// Fill regions smaller than this area (in scene units²) keep their **solid** fill instead of
    /// being hatched — the jittered outline still gets sketched.
    ///
    /// Tiny filled shapes (arrowheads, label backgrounds, legend chips) turn into an
    /// illegible 2–3 line smudge when hatched; real content areas are far bigger. `None`
    /// (default) hatches every requested fill; hosts with diagram semantics should opt in
    /// (e.g. `Some(1000.0)` keeps sub-32×32 decorations solid). `Some(0.0)` disables the guard.
    pub min_fill_area: Option<f64>,
    /// Hachure direction in degrees. Default `-41.0`.
    pub fill_angle: f64,
    /// Spacing between hachure lines. Default `4.0`.
    pub fill_gap: f64,
    /// Weight of a single hachure line. Default `0.5`.
    pub fill_weight: f64,
    /// Safety cap on the number of fill strokes generated **per element** by the raster backend;
    /// beyond it the gap is coarsened instead of flooding the rasterizer. Default `2000`.
    pub max_fill_strokes: usize,
}

impl Default for SketchOptions {
    fn default() -> Self {
        Self {
            roughness: 0.7,
            bowing: 0.5,
            seed: None,
            multistroke: true,
            curve_step: 12.0,
            kinds: SketchKinds::ALL,
            outline_when_unstroked: true,
            outline_width: 1.0,
            fill: None,
            min_fill_area: None,
            fill_angle: -41.0,
            fill_gap: 4.0,
            fill_weight: 0.5,
            max_fill_strokes: 2000,
        }
    }
}

impl SketchOptions {
    /// Defaults (see each field).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Jitter amplitude.
    #[must_use]
    pub fn with_roughness(mut self, roughness: f64) -> Self {
        self.roughness = roughness.max(0.0);
        self
    }

    /// Line bowing.
    #[must_use]
    pub fn with_bowing(mut self, bowing: f64) -> Self {
        self.bowing = bowing;
        self
    }

    /// PRNG seed (always set it in tests).
    #[must_use]
    pub fn with_seed(mut self, seed: u64) -> Self {
        self.seed = Some(seed);
        self
    }

    /// Draw each segment twice.
    #[must_use]
    pub fn with_multistroke(mut self, multistroke: bool) -> Self {
        self.multistroke = multistroke;
        self
    }

    /// Curve sampling density.
    #[must_use]
    pub fn with_curve_step(mut self, step: f64) -> Self {
        self.curve_step = step.max(0.5);
        self
    }

    /// Restrict to a set of element kinds.
    #[must_use]
    pub fn with_kinds(mut self, kinds: SketchKinds) -> Self {
        self.kinds = kinds;
        self
    }

    /// Synthesise outlines for fill-only shapes.
    #[must_use]
    pub fn with_outline_when_unstroked(mut self, on: bool) -> Self {
        self.outline_when_unstroked = on;
        self
    }

    /// Enable hand-drawn fills with the given style.
    #[must_use]
    pub fn with_fill(mut self, style: SketchFillStyle) -> Self {
        self.fill = Some(style);
        self
    }

    /// Keep solid fills for regions smaller than `area` (scene units²); only their outlines are
    /// sketched. See [`SketchOptions::min_fill_area`].
    #[must_use]
    pub fn with_min_fill_area(mut self, area: f64) -> Self {
        self.min_fill_area = Some(area.max(0.0));
        self
    }

    /// Hachure direction (degrees).
    #[must_use]
    pub fn with_fill_angle(mut self, angle: f64) -> Self {
        self.fill_angle = angle;
        self
    }

    /// Hachure spacing.
    #[must_use]
    pub fn with_fill_gap(mut self, gap: f64) -> Self {
        self.fill_gap = gap;
        self
    }

    /// Hachure line weight.
    #[must_use]
    pub fn with_fill_weight(mut self, weight: f64) -> Self {
        self.fill_weight = weight;
        self
    }

    /// Per-element fill-stroke cap (raster backend).
    #[must_use]
    pub fn with_max_fill_strokes(mut self, max: usize) -> Self {
        self.max_fill_strokes = max.max(1);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::{Color, Point, Rect};
    use crate::render::SvgRenderer;
    use crate::scene::{Element, FillStrokeStyle, Scene};

    #[test]
    fn defaults_are_stable() {
        let o = SketchOptions::default();
        assert_eq!(o.roughness, 0.7);
        assert_eq!(o.bowing, 0.5);
        assert!(o.multistroke);
        assert!(o.fill.is_none(), "默认不改动填充");
        assert_eq!(o.kinds, SketchKinds::ALL);
        assert!(o.seed.is_none());
    }

    #[test]
    fn kinds_bitmask_roundtrips() {
        let k = SketchKinds::NONE
            .with(SketchKind::Rect)
            .with(SketchKind::Circle)
            .without(SketchKind::Rect);
        assert!(!k.contains(SketchKind::Rect));
        assert!(k.contains(SketchKind::Circle));
        assert!(!k.is_empty());
        assert!(SketchKinds::NONE.is_empty());
        assert_eq!(
            SketchKinds::only(&[SketchKind::Pie]),
            SketchKinds::NONE.with(SketchKind::Pie)
        );
    }

    #[test]
    fn text_and_image_have_no_kind() {
        let text = Element::text(
            "x",
            Point::new(0.0, 0.0),
            crate::text::TextStyle::new(Color::BLACK, 12.0, "sans-serif"),
        );
        assert_eq!(SketchKind::of(&text), None);
    }

    /// 端到端：默认渲染与未开启手绘时完全一致（开关的"默认关闭"语义）。
    #[test]
    fn untouched_scene_renders_identically() {
        let build = || {
            let mut s = Scene::new(120.0, 80.0);
            s.push(Element::rect(
                Rect::new(10.0, 10.0, 100.0, 60.0),
                FillStrokeStyle {
                    fill: Some(Fill::Solid(Color::rgb(0xee, 0xee, 0xff))),
                    stroke: Some(crate::scene::Stroke::new(Color::BLACK, 1.5)),
                },
            ));
            s
        };
        let mut a = build();
        let b = build();
        // kinds = NONE → 手绘被关闭，输出与不调用时一致。
        roughen_scene(&mut a, &SketchOptions::new().with_kinds(SketchKinds::NONE));
        let ra = {
            let mut r = SvgRenderer::new(120.0, 80.0);
            crate::render::Renderer::render_scene(&mut r, &a);
            r.into_string()
        };
        let rb = {
            let mut r = SvgRenderer::new(120.0, 80.0);
            crate::render::Renderer::render_scene(&mut r, &b);
            r.into_string()
        };
        assert_eq!(ra, rb);
    }
}
