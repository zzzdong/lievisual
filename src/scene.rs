//! Declarative scene-graph IR.
//!
//! Design principles (see the crate-level docs):
//! - `z_index` and [`Transform`] are promoted to [`SceneNode`] fields rather than carried
//!   by every primitive. Likewise, `opacity` / `name` / `visible` are node (or whole-subtree)
//!   attributes.
//! - The [`Element`] enum only describes geometry and style; adding a new primitive variant
//!   does not affect layering / transform logic.
//! - Node-level `opacity` applies to the whole subtree (composed level-by-level during Group
//!   recursion); when `visible = false`, the whole subtree is skipped.
//! - Layers: a [`Scene`] may carry an ordered set of [`Layer`] (bottom to top). A layer controls
//!   `visible` / `opacity` / `transform` for the whole layer, while nodes inside are stably
//!   sorted by `z_index`; `Scene::nodes` is kept as the "default layer" (bottom-most) for
//!   backward compatibility.
//! - [`Element::Text`] expresses text via styled spans ([`crate::text::RichSpan`]) and may carry
//!   a pre-laid-out [`crate::text::TextLayout`]; plain text is derived by concatenating `spans`,
//!   keeping all backends consistent.
//! - Unified ordering helper: [`SceneNode::ordered`] iterates nodes in stable ascending `z_index`
//!   order (preserving insertion order within the same level), and [`Scene::iter_ordered`] is a
//!   convenience entry for the top-level collection; rendering and callers share the same
//!   traversal logic.

use crate::geometry::{Color, Point, Rect, Transform, Vec2};
use crate::render::Renderer;
use crate::text::{RichSpan, TextLayout, layout_text};
use kurbo::BezPath;
use std::sync::Arc;

/// Typeset `spans` for [`Element::Text`], or `None` when there is nothing to lay out.
///
/// Returns `None` (rather than an empty layout) for empty spans / empty text, so a text element
/// that draws nothing does not pay for an allocation.
fn prelayout(spans: &[RichSpan], max_width: Option<f64>) -> Option<Arc<TextLayout>> {
    if spans.is_empty() || spans.iter().all(|s| s.text.is_empty()) {
        return None;
    }
    let layout = layout_text(spans, max_width);
    (!layout.lines.is_empty()).then_some(layout)
}

/// How an image fits inside its target frame (`Element::Image.frame`, pt/px coordinates).
///
/// Mirrors the `ObjectFit` concept used by downstream crates such as liepress, but is a
/// self-contained enum in the lievisual IR so the rendering layer does not depend on a
/// document layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ObjectFit {
    /// Scale to fit while preserving aspect ratio, fully visible (may pad inside the frame).
    /// Corresponds to SVG `meet`.
    #[default]
    Contain,
    /// Scale to fill while preserving aspect ratio, cropping the overflow. Corresponds to SVG `slice`.
    Cover,
    /// Stretch to fill the frame (may distort).
    Fill,
    /// No scaling; place at the frame's top-left at original pixel size (may overflow or pad).
    None,
}

/// Image resource inside a scene (self-contained RGBA8 bitmap).
///
/// Holds an already-decoded pixel bitmap ([`crate::Pixmap`]); **lievisual does not decode** —
/// decoding is the caller's / resource-loading layer's responsibility. `frame` (given in
/// [`Element::Image`]) is the display area; `object_fit` decides how the image maps onto `frame`.
#[derive(Debug, Clone)]
pub struct SceneImage {
    /// Decoded RGBA8 bitmap.
    pub pixmap: crate::Pixmap,
    /// Fit mode.
    pub object_fit: ObjectFit,
}

impl SceneImage {
    /// Construct from an already-decoded RGBA8 bitmap (default `object_fit = Contain`).
    #[must_use]
    pub fn from_pixmap(pixmap: crate::Pixmap) -> Self {
        Self {
            pixmap,
            object_fit: ObjectFit::Contain,
        }
    }

    /// Convenience: build a bitmap from RGBA8 pixels (default `object_fit = Contain`).
    ///
    /// The length must be `4 * width * height`, otherwise `None` is returned.
    #[must_use]
    pub fn from_rgba8(
        width: u32,
        height: u32,
        pixels: impl Into<std::sync::Arc<[u8]>>,
    ) -> Option<Self> {
        crate::Pixmap::from_rgba8(width, height, pixels).map(Self::from_pixmap)
    }

    /// Convenience: decode from PNG bytes (`png` is a permanent dependency). Returns `None`
    /// for non-PNG input or decode failure.
    #[must_use]
    pub fn from_png(bytes: &[u8]) -> Option<Self> {
        crate::Pixmap::decode_png(bytes).map(Self::from_pixmap)
    }

    /// Decode from arbitrary-encoded bytes (`jpeg` / `webp` / `gif`, etc. via `image`).
    ///
    /// Only available when the `extra-image-codecs` feature is enabled. By default lievisual
    /// does not decode; images enter the IR as already-decoded RGBA8 bitmaps.
    #[cfg(feature = "extra-image-codecs")]
    #[must_use]
    pub fn from_encoded(bytes: &[u8]) -> Option<Self> {
        let dyn_img = image::load_from_memory(bytes).ok()?;
        let rgba = dyn_img.to_rgba8();
        let (w, h) = rgba.dimensions();
        Self::from_rgba8(w, h, rgba.into_raw())
    }

    /// Original pixel width.
    #[must_use]
    pub fn width(&self) -> u32 {
        self.pixmap.width()
    }

    /// Original pixel height.
    #[must_use]
    pub fn height(&self) -> u32 {
        self.pixmap.height()
    }

    /// Set the fit mode.
    #[must_use]
    pub fn with_object_fit(mut self, fit: ObjectFit) -> Self {
        self.object_fit = fit;
        self
    }

    /// The `preserveAspectRatio` fragment (without prefix) used for SVG output.
    ///
    /// `None` together with `"none"` plus the raw pixel width/height on the SVG side
    /// achieves "no scaling, top-left".
    #[must_use]
    pub fn preserve_aspect_ratio(&self) -> &'static str {
        match self.object_fit {
            ObjectFit::Contain => "xMidYMid meet",
            ObjectFit::Cover => "xMidYMid slice",
            ObjectFit::Fill => "none",
            ObjectFit::None => "none",
        }
    }
}

/// A complete, renderable scene.
#[derive(Debug, Clone)]
pub struct Scene {
    pub width: f64,
    pub height: f64,
    pub background: Color,
    /// Top-level primitive collection (the "default layer", below all `layers`). `render_scene`
    /// sorts by `z_index` before drawing.
    pub nodes: Vec<SceneNode>,
    /// Ordered layers (bottom to top). Each layer controls `visible` / `opacity` / `transform`
    /// for the whole layer, and nodes inside are drawn in stable ascending `z_index` order.
    pub layers: Vec<Layer>,
    /// Document title (emitted as `<title>` in SVG, for accessibility / export naming).
    pub title: Option<String>,
    /// Document description (emitted as `<desc>` in SVG).
    pub description: Option<String>,
    /// Output scale (pixel density). Default 1.0.
    ///
    /// **Both** backends share one contract: the size passed to a renderer is the **output**
    /// size, and `scale` divides it to obtain the user coordinate system the scene is drawn in
    /// (on the SVG side, `viewBox = 输出尺寸 / scale`). `scale` is therefore a pure density knob —
    /// it never silently enlarges the canvas. See [`crate::render::SvgRenderer::into_string`].
    pub scale: f64,
}

impl Default for Scene {
    fn default() -> Self {
        Self {
            width: 0.0,
            height: 0.0,
            background: Color::WHITE,
            nodes: Vec::new(),
            layers: Vec::new(),
            title: None,
            description: None,
            scale: 1.0,
        }
    }
}

impl Scene {
    pub fn new(width: f64, height: f64) -> Self {
        Self {
            width,
            height,
            background: Color::WHITE,
            nodes: Vec::new(),
            layers: Vec::new(),
            title: None,
            description: None,
            scale: 1.0,
        }
    }

    /// Convenience constructor: background + size.
    #[must_use]
    pub fn with_background(mut self, bg: Color) -> Self {
        self.background = bg;
        self
    }

    /// Convenience constructor: document title.
    #[must_use]
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// Convenience constructor: document description.
    #[must_use]
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Convenience constructor: output scale (pixel density).
    #[must_use]
    pub fn with_scale(mut self, scale: f64) -> Self {
        self.scale = scale;
        self
    }

    /// Append a primitive or node (default `z_index = 0`, no transform).
    /// Accepts both [`Element`] (auto-converted to [`SceneNode`] via `From<Element>`) and an
    /// already-configured [`SceneNode`].
    pub fn push(&mut self, node: impl Into<SceneNode>) {
        self.nodes.push(node.into());
    }

    /// Append a primitive with explicit layer / transform as a node.
    pub fn push_node(&mut self, node: SceneNode) {
        self.nodes.push(node);
    }

    /// Append a recursive group: children are drawn under the parent transform and layer.
    pub fn push_group(&mut self, children: Vec<SceneNode>) {
        self.nodes.push(SceneNode::group(children));
    }

    /// Append a layer (later `push` draws higher).
    pub fn push_layer(&mut self, layer: Layer) {
        self.layers.push(layer);
    }

    /// All layers (bottom to top).
    pub fn layers(&self) -> &[Layer] {
        &self.layers
    }

    /// Find a layer by name.
    pub fn layer(&self, name: &str) -> Option<&Layer> {
        self.layers.iter().find(|l| l.name == name)
    }

    /// Find a layer by name (mutable).
    pub fn layer_mut(&mut self, name: &str) -> Option<&mut Layer> {
        self.layers.iter_mut().find(|l| l.name == name)
    }

    /// Render to any backend.
    pub fn render(&self, renderer: &mut dyn Renderer) {
        renderer.render_scene(self);
    }

    /// Iterate top-level primitives in ascending `z_index` order (stable: insertion order is
    /// preserved within the same level). Covers only the default `nodes` layer; for layers see
    /// [`Layer::iter_ordered`] and [`Renderer::render_layer`].
    pub fn iter_ordered(&self) -> impl Iterator<Item = &SceneNode> {
        SceneNode::ordered(&self.nodes)
    }
}

/// A named layer: a set of nodes plus layer-wide attributes.
///
/// Layers are drawn in array order from bottom to top; when a layer's `visible = false` the
/// whole layer is skipped, and `opacity` / `transform` apply to all nodes inside it (consistent
/// with a Group's subtree semantics). Nodes inside are then drawn in stable ascending `z_index`
/// order.
#[derive(Debug, Clone)]
pub struct Layer {
    /// Layer name (emitted as `<g id>` on the SVG side; may be empty to omit).
    pub name: String,
    /// Whether visible; default `true`. When `false` the whole layer is skipped.
    pub visible: bool,
    /// Layer-wide opacity, 0.0–1.0; default 1.0.
    pub opacity: f64,
    /// Layer-wide transform (applied to all nodes inside).
    pub transform: Option<Transform>,
    /// Nodes inside the layer (drawn after stable `z_index` sorting).
    pub nodes: Vec<SceneNode>,
}

impl Layer {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            visible: true,
            opacity: 1.0,
            transform: None,
            nodes: Vec::new(),
        }
    }

    /// Layer with initial nodes.
    pub fn with_nodes(mut self, nodes: Vec<SceneNode>) -> Self {
        self.nodes = nodes;
        self
    }

    /// Append a primitive or node inside the layer.
    pub fn push(&mut self, node: impl Into<SceneNode>) {
        self.nodes.push(node.into());
    }

    /// Append a node with explicit layer / transform inside the layer.
    pub fn push_node(&mut self, node: SceneNode) {
        self.nodes.push(node);
    }

    /// Append a recursive group inside the layer.
    pub fn push_group(&mut self, children: Vec<SceneNode>) {
        self.nodes.push(SceneNode::group(children));
    }

    /// Iterate inside the layer in ascending `z_index` order (stable: insertion order is
    /// preserved within the same level).
    pub fn iter_ordered(&self) -> impl Iterator<Item = &SceneNode> {
        SceneNode::ordered(&self.nodes)
    }

    #[must_use]
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// Set the layer's visibility.
    #[must_use]
    pub fn with_visible(mut self, visible: bool) -> Self {
        self.visible = visible;
        self
    }

    /// Set the layer's opacity (0.0–1.0).
    #[must_use]
    pub fn with_opacity(mut self, opacity: f64) -> Self {
        self.opacity = opacity.clamp(0.0, 1.0);
        self
    }

    /// Set the layer's transform.
    #[must_use]
    pub fn with_transform(mut self, t: Transform) -> Self {
        self.transform = Some(t);
        self
    }
}

/// A single node in the scene: primitive + layer + local transform + node attributes.
///
/// All fields except `element` are "subtree-level" attributes: `transform` / `opacity` / `name`
/// / `visible` also apply to a `Group` (and `opacity` / `transform` are composed through
/// recursion).
#[derive(Debug, Clone)]
pub struct SceneNode {
    pub element: Element,
    pub z_index: i32,
    pub transform: Option<Transform>,
    /// Opacity for the node (and Group subtree), 0.0–1.0; default 1.0.
    pub opacity: f64,
    /// Optional identifier: debugging, hit-testing, SVG `id`.
    pub name: Option<String>,
    /// Whether visible; default `true`. When `false` the whole subtree is skipped.
    pub visible: bool,
    /// Optional clip shape: restrict the whole subtree to its interior.
    pub clip: Option<Clip>,
}

impl SceneNode {
    pub fn new(element: Element) -> Self {
        Self {
            element,
            z_index: 0,
            transform: None,
            opacity: 1.0,
            name: None,
            visible: true,
            clip: None,
        }
    }

    /// Construct a recursive group node (children drawn under the parent transform and layer).
    pub fn group(children: Vec<SceneNode>) -> Self {
        Self {
            element: Element::Group { children },
            z_index: 0,
            transform: None,
            opacity: 1.0,
            name: None,
            visible: true,
            clip: None,
        }
    }

    /// Yield node references in ascending `z_index` order (stable: insertion order is preserved
    /// within the same level).
    pub fn ordered(nodes: &[SceneNode]) -> impl Iterator<Item = &SceneNode> {
        let mut order: Vec<usize> = (0..nodes.len()).collect();
        // Stable sort: `sort_by_key` preserves relative order for equal `z_index`.
        order.sort_by_key(|&i| nodes[i].z_index);
        order.into_iter().map(move |i| &nodes[i])
    }

    #[must_use]
    pub fn with_z(mut self, z: i32) -> Self {
        self.z_index = z;
        self
    }

    #[must_use]
    pub fn with_transform(mut self, t: Transform) -> Self {
        self.transform = Some(t);
        self
    }

    /// Set the node opacity (0.0–1.0).
    #[must_use]
    pub fn with_opacity(mut self, opacity: f64) -> Self {
        self.opacity = opacity.clamp(0.0, 1.0);
        self
    }

    /// Set the node name (debugging / hit-testing / SVG `id`).
    #[must_use]
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Set visibility. When `false` the whole subtree is skipped.
    #[must_use]
    pub fn with_visible(mut self, visible: bool) -> Self {
        self.visible = visible;
        self
    }

    /// Set a clip shape: restrict the whole subtree to the interior of `clip`.
    #[must_use]
    pub fn with_clip(mut self, clip: Clip) -> Self {
        self.clip = Some(clip);
        self
    }
}

/// Construct a node from an [`Element`] (default `z_index = 0`, no transform), enabling
/// `Scene::push` etc. to accept `Element`.
impl From<Element> for SceneNode {
    fn from(element: Element) -> Self {
        SceneNode::new(element)
    }
}

/// A clip shape: restrict a node (and its whole subtree) to drawing inside the shape.
///
/// As a node-level attribute ([`SceneNode::clip`]) it acts on the subtree; the shape coordinates
/// are local user coordinates, composed with the node `transform` (transform first, then clip).
/// Reuses existing geometry / path types for cross-backend consistency:
/// - SVG: `<clipPath>` + `clip-path="url(#...)"`
/// - vello: `push_clip_path` / `pop_clip_path`
#[derive(Debug, Clone, PartialEq)]
pub enum Clip {
    /// Rectangular clip.
    Rect(Rect),
    /// Rounded-rectangle clip.
    RoundedRect { rect: Rect, radius: f64 },
    /// Circular clip.
    Circle { center: Point, radius: f64 },
    /// Elliptical clip (rotatable).
    Ellipse {
        center: Point,
        radii: Vec2,
        rotation: f64,
    },
    /// Arbitrary Bézier-path clip (supports multiple sub-paths, default nonzero fill rule).
    Path(BezPath),
}

impl Clip {
    #[must_use]
    pub fn rect(rect: Rect) -> Self {
        Self::Rect(rect)
    }

    #[must_use]
    pub fn rounded_rect(rect: Rect, radius: f64) -> Self {
        Self::RoundedRect { rect, radius }
    }

    #[must_use]
    pub fn circle(center: Point, radius: f64) -> Self {
        Self::Circle { center, radius }
    }

    #[must_use]
    pub fn ellipse(center: Point, radii: Vec2, rotation: f64) -> Self {
        Self::Ellipse {
            center,
            radii,
            rotation,
        }
    }

    #[must_use]
    pub fn path(path: BezPath) -> Self {
        Self::Path(path)
    }

    /// Convert to a kurbo Bézier path (for path-consuming backends such as vello).
    #[must_use]
    pub fn to_bezpath(&self) -> BezPath {
        use kurbo::Shape;
        match self {
            Clip::Rect(rect) => {
                let mut p = BezPath::new();
                p.move_to((rect.min_x(), rect.min_y()));
                p.line_to((rect.max_x(), rect.min_y()));
                p.line_to((rect.max_x(), rect.max_y()));
                p.line_to((rect.min_x(), rect.max_y()));
                p.close_path();
                p
            }
            Clip::RoundedRect { rect, radius } => kurbo::RoundedRect::new(
                rect.min_x(),
                rect.min_y(),
                rect.max_x(),
                rect.max_y(),
                *radius,
            )
            .to_path(0.1),
            Clip::Circle { center, radius } => {
                kurbo::Circle::new((center.x, center.y), *radius).to_path(0.1)
            }
            Clip::Ellipse {
                center,
                radii,
                rotation,
            } => kurbo::Ellipse::new((center.x, center.y), (radii.x, radii.y), *rotation)
                .to_path(0.1),
            Clip::Path(path) => path.clone(),
        }
    }
}

/// Fill + stroke style.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FillStrokeStyle {
    pub fill: Option<Fill>,
    pub stroke: Option<Stroke>,
}

impl FillStrokeStyle {
    /// Solid-fill style.
    #[must_use]
    pub fn fill(color: Color) -> Self {
        Self {
            fill: Some(Fill::Solid(color)),
            stroke: None,
        }
    }

    /// Linear-gradient fill style.
    #[must_use]
    pub fn fill_gradient(gradient: impl Into<Fill>) -> Self {
        Self {
            fill: Some(gradient.into()),
            stroke: None,
        }
    }

    /// Pure-stroke style.
    #[must_use]
    pub fn stroke(color: Color, width: f64) -> Self {
        Self {
            fill: None,
            stroke: Some(Stroke {
                color,
                width,
                ..Default::default()
            }),
        }
    }
}

/// Fill definition.
#[derive(Debug, Clone, PartialEq)]
pub enum Fill {
    Solid(Color),
    LinearGradient(LinearGradient),
    RadialGradient(RadialGradient),
}

impl From<Color> for Fill {
    fn from(c: Color) -> Self {
        Self::Solid(c)
    }
}

impl From<LinearGradient> for Fill {
    fn from(g: LinearGradient) -> Self {
        Self::LinearGradient(g)
    }
}

impl From<RadialGradient> for Fill {
    fn from(g: RadialGradient) -> Self {
        Self::RadialGradient(g)
    }
}

/// Linear gradient (shared by `GradientPath` and `Fill::LinearGradient`).
#[derive(Debug, Clone, PartialEq)]
pub struct LinearGradient {
    pub start: Point,
    pub end: Point,
    pub stops: Vec<GradientStop>,
}

/// Radial gradient: spreads outward from `center` with `radius` as the outer circle radius.
#[derive(Debug, Clone, PartialEq)]
pub struct RadialGradient {
    pub center: Point,
    pub radius: f64,
    pub stops: Vec<GradientStop>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GradientStop {
    pub offset: f64,
    pub color: Color,
}

/// Stroke definition.
#[derive(Debug, Clone, PartialEq)]
pub struct Stroke {
    pub color: Color,
    pub width: f64,
    /// Line cap: butt / round / square.
    pub line_cap: LineCap,
    /// Line join: miter / round / bevel.
    pub line_join: LineJoin,
    /// Dash array (alternating on/off lengths, same unit as `width`). Empty means solid.
    pub dash_array: Vec<f64>,
    /// Dash start offset.
    pub dash_offset: f64,
    /// Miter-join limit. Default 10.0, aligned with SVG / kurbo.
    pub miter_limit: f64,
}

impl Default for Stroke {
    fn default() -> Self {
        Self {
            color: Color::default(),
            width: 0.0,
            line_cap: LineCap::default(),
            line_join: LineJoin::default(),
            dash_array: Vec::new(),
            dash_offset: 0.0,
            miter_limit: 10.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LineCap {
    #[default]
    Butt,
    Round,
    Square,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LineJoin {
    #[default]
    Miter,
    Round,
    Bevel,
}

impl Stroke {
    /// Convenience constructor: solid-color stroke.
    #[must_use]
    pub fn new(color: Color, width: f64) -> Self {
        Self {
            color,
            width,
            ..Default::default()
        }
    }

    /// Convenience constructor: dashed stroke (`dash_array` is the alternating on/off lengths).
    #[must_use]
    pub fn dashed(color: Color, width: f64, dash_array: Vec<f64>) -> Self {
        Self {
            color,
            width,
            dash_array,
            ..Default::default()
        }
    }
}

/// Declarative primitive enum. Describes geometry and style only.
#[derive(Debug, Clone)]
pub enum Element {
    Rect {
        rect: Rect,
        style: FillStrokeStyle,
    },
    Circle {
        center: Point,
        radius: f64,
        style: FillStrokeStyle,
    },
    /// Ellipse (rotatable): `radii` are the two semi-axis lengths, `rotation` is the rotation
    /// about the center (radians).
    Ellipse {
        center: Point,
        radii: Vec2,
        rotation: f64,
        style: FillStrokeStyle,
    },
    /// Rounded rectangle: `radius` is the uniform corner radius.
    RoundedRect {
        rect: Rect,
        radius: f64,
        style: FillStrokeStyle,
    },
    Line {
        start: Point,
        end: Point,
        style: Stroke,
    },
    /// Open polyline (stroke only).
    Polyline {
        points: Vec<Point>,
        style: Stroke,
    },
    /// Closed polygon (fill + optional stroke).
    Polygon {
        points: Vec<Point>,
        style: FillStrokeStyle,
    },
    /// Arc (open, stroke only). `radii` are the two semi-axis lengths; angles are in radians.
    Arc {
        center: Point,
        radii: Vec2,
        start_angle: f64,
        sweep_angle: f64,
        style: Stroke,
    },
    /// Pie sector (closed, with lines from center to both endpoints; fill + optional stroke).
    /// Suitable for pie charts.
    Pie {
        center: Point,
        radius: f64,
        start_angle: f64,
        sweep_angle: f64,
        style: FillStrokeStyle,
    },
    /// Closed / open path, using a kurbo Bézier path.
    Path {
        path: BezPath,
        style: FillStrokeStyle,
        /// `true` means a closed path (used for filling).
        closed: bool,
    },
    /// Image: self-contained binary resource + display frame (pt/px coordinates).
    ///
    /// - `image`: image bytes, original pixel size, and fit mode (see [`SceneImage`]);
    /// - `frame`: display area; the backend maps the image onto this frame per `image.object_fit`;
    /// - `opacity`: overall opacity (0–1), default 1.0.
    ///
    /// - SVG: `<image>` tag (`data:` URI + `preserveAspectRatio`);
    /// - vello: decoded to a bitmap then drawn with an image paint (clipped by `object_fit`
    ///   and `frame`).
    Image {
        image: SceneImage,
        frame: Rect,
        opacity: f64,
    },
    /// Path with a gradient fill.
    GradientPath {
        path: BezPath,
        gradient: LinearGradient,
        stroke: Option<Stroke>,
    },
    /// Text: built around styled spans ([`RichSpan`]), with an optional pre-laid-out `layout`.
    ///
    /// - `spans`: styled spans (backends such as vello typeset glyphs from these); a single text
    ///   is one span;
    /// - `style`: block-level positioning style (`align` / `baseline` / `rotation` / `max_width`)
    ///   and the default font;
    /// - `layout`: pre-laid-out result; when present the backend should prefer it (precise glyph
    ///   positioning).
    ///
    /// Plain text content (SVG backend) is derived by concatenating `spans`, keeping all backends
    /// consistent.
    Text {
        spans: Vec<RichSpan>,
        position: Point,
        style: crate::text::TextStyle,
        layout: Option<std::sync::Arc<TextLayout>>,
    },
    /// Recursive group: children are drawn under the parent transform and layer.
    Group {
        children: Vec<SceneNode>,
    },
}

impl Element {
    /// Rectangle primitive.
    #[must_use]
    pub fn rect(rect: Rect, style: FillStrokeStyle) -> Self {
        Self::Rect { rect, style }
    }

    /// Circle primitive.
    #[must_use]
    pub fn circle(center: Point, radius: f64, style: FillStrokeStyle) -> Self {
        Self::Circle {
            center,
            radius,
            style,
        }
    }

    /// Ellipse primitive.
    #[must_use]
    pub fn ellipse(center: Point, radii: Vec2, rotation: f64, style: FillStrokeStyle) -> Self {
        Self::Ellipse {
            center,
            radii,
            rotation,
            style,
        }
    }

    /// Rounded-rectangle primitive.
    #[must_use]
    pub fn rounded_rect(rect: Rect, radius: f64, style: FillStrokeStyle) -> Self {
        Self::RoundedRect {
            rect,
            radius,
            style,
        }
    }

    /// Line segment primitive.
    #[must_use]
    pub fn line(start: Point, end: Point, style: Stroke) -> Self {
        Self::Line { start, end, style }
    }

    /// Polyline primitive (open, stroke only).
    #[must_use]
    pub fn poly(points: Vec<Point>, style: Stroke) -> Self {
        Self::Polyline { points, style }
    }

    /// Polygon primitive (closed, fill + optional stroke).
    #[must_use]
    pub fn polygon(points: Vec<Point>, style: FillStrokeStyle) -> Self {
        Self::Polygon { points, style }
    }

    /// Arc primitive (open, stroke only).
    #[must_use]
    pub fn arc(
        center: Point,
        radii: Vec2,
        start_angle: f64,
        sweep_angle: f64,
        style: Stroke,
    ) -> Self {
        Self::Arc {
            center,
            radii,
            start_angle,
            sweep_angle,
            style,
        }
    }

    /// Pie-sector primitive (closed, fill + optional stroke).
    #[must_use]
    pub fn pie(
        center: Point,
        radius: f64,
        start_angle: f64,
        sweep_angle: f64,
        style: FillStrokeStyle,
    ) -> Self {
        Self::Pie {
            center,
            radius,
            start_angle,
            sweep_angle,
            style,
        }
    }

    /// Plain-text primitive; the layout is **measured eagerly** at construction.
    ///
    /// `style` serves both as block-level positioning (`align` / `baseline` / `rotation` /
    /// `max_width`) and as the default font style; internally a single [`RichSpan`] is produced.
    ///
    /// The layout is typeset immediately (see [`Element::rich_text`] for why), so the result
    /// carries accurate block dimensions. To skip it in a performance-sensitive hot loop,
    /// build the struct variant directly with `layout: None`.
    #[must_use]
    pub fn text(
        content: impl Into<String>,
        position: Point,
        style: crate::text::TextStyle,
    ) -> Self {
        let spans = vec![RichSpan::new(content, style.clone())];
        let layout = prelayout(&spans, style.max_width);
        Self::Text {
            spans,
            position,
            style,
            layout,
        }
    }

    /// Rich-text primitive: composed of styled spans (`style` provides block-level positioning
    /// and the default font).
    ///
    /// ## Why the layout is measured here
    ///
    /// The two backends typeset text differently: the raster one via parley, the SVG one by
    /// emitting `<text>` for the **browser** to typeset. Without a shared pre-computed layout
    /// the SVG side had to *estimate* the block size from `font_size` (e.g. baseline at
    /// `0.85 × font_size`), which made the text background rectangle and the vertical anchor
    /// drift away from the raster output — and from [`crate::fit::scene_bounds`], which measures
    /// for real. Typesetting once here keeps all three consistent.
    ///
    /// Measurement is skipped (leaving `layout: None`) when there is nothing to typeset, e.g.
    /// empty spans.
    #[must_use]
    pub fn rich_text(spans: Vec<RichSpan>, position: Point, style: crate::text::TextStyle) -> Self {
        let layout = prelayout(&spans, style.max_width);
        Self::Text {
            spans,
            position,
            style,
            layout,
        }
    }

    /// Image primitive: `image` is the self-contained resource, `frame` is the display area
    /// (pt/px coordinates).
    ///
    /// `opacity` defaults to 1.0; `object_fit` is controlled by [`SceneImage::object_fit`].
    #[must_use]
    pub fn image(image: SceneImage, frame: Rect) -> Self {
        Self::Image {
            image,
            frame,
            opacity: 1.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a rectangle node with a distinguishable x coordinate and a specified z_index
    /// (used for stability assertions).
    fn rect_node(x: f64, z: i32) -> SceneNode {
        SceneNode::new(Element::rect(
            Rect::new(x, 0.0, x + 1.0, 1.0),
            FillStrokeStyle::fill(Color::BLACK),
        ))
        .with_z(z)
    }

    fn rect_x(node: &SceneNode) -> f64 {
        match &node.element {
            Element::Rect { rect, .. } => rect.min_x(),
            _ => panic!("expected rect"),
        }
    }

    /// Element constructors, node builders, and defaults.
    mod element {
        use super::*;

        #[test]
        fn push_accepts_element_and_scene_node() {
            let mut scene = Scene::new(100.0, 100.0);
            scene.push(Element::rect(
                Rect::new(0.0, 0.0, 1.0, 1.0),
                FillStrokeStyle::fill(Color::BLACK),
            ));
            scene.push(SceneNode::new(Element::rect(
                Rect::new(0.0, 0.0, 1.0, 1.0),
                FillStrokeStyle::fill(Color::BLACK),
            )));
            assert_eq!(scene.nodes.len(), 2);
        }

        #[test]
        fn builder_chaining() {
            let node = SceneNode::new(Element::circle(
                Point::new(1.0, 2.0),
                3.0,
                FillStrokeStyle::stroke(Color::BLACK, 1.0),
            ))
            .with_z(7)
            .with_transform(Transform::translate(1.0, 2.0))
            .with_opacity(0.5)
            .with_name("node-a")
            .with_visible(false);
            assert_eq!(node.z_index, 7);
            assert_eq!(node.transform, Some(Transform::translate(1.0, 2.0)));
            assert_eq!(node.opacity, 0.5);
            assert_eq!(node.name.as_deref(), Some("node-a"));
            assert!(!node.visible);
        }

        #[test]
        fn node_defaults() {
            let node = SceneNode::new(Element::rect(
                Rect::new(0.0, 0.0, 1.0, 1.0),
                FillStrokeStyle::fill(Color::BLACK),
            ));
            assert_eq!(node.opacity, 1.0);
            assert!(node.name.is_none());
            assert!(node.visible);
        }

        #[test]
        fn from_element_conversion() {
            let node: SceneNode = Element::line(
                Point::new(0.0, 0.0),
                Point::new(1.0, 1.0),
                Stroke::new(Color::BLACK, 1.0),
            )
            .into();
            assert_eq!(node.z_index, 0);
            assert!(node.transform.is_none());
        }

        #[test]
        fn element_constructors() {
            let r = Element::rect(
                Rect::new(0.0, 0.0, 10.0, 10.0),
                FillStrokeStyle::fill(Color::BLACK),
            );
            assert!(matches!(r, Element::Rect { .. }));

            let c = Element::circle(
                Point::new(1.0, 2.0),
                3.0,
                FillStrokeStyle::fill(Color::BLACK),
            );
            assert!(matches!(c, Element::Circle { .. }));

            let e = Element::ellipse(
                Point::new(1.0, 2.0),
                Vec2::new(4.0, 2.0),
                0.1,
                FillStrokeStyle::fill(Color::BLACK),
            );
            assert!(matches!(e, Element::Ellipse { .. }));

            let rr = Element::rounded_rect(
                Rect::new(0.0, 0.0, 10.0, 10.0),
                3.0,
                FillStrokeStyle::fill(Color::BLACK),
            );
            assert!(matches!(rr, Element::RoundedRect { .. }));

            let l = Element::line(
                Point::new(0.0, 0.0),
                Point::new(1.0, 1.0),
                Stroke::new(Color::BLACK, 1.0),
            );
            assert!(matches!(l, Element::Line { .. }));

            let p = Element::poly(
                vec![Point::new(0.0, 0.0), Point::new(1.0, 1.0)],
                Stroke::new(Color::BLACK, 1.0),
            );
            assert!(matches!(p, Element::Polyline { .. }));

            let pg = Element::polygon(
                vec![
                    Point::new(0.0, 0.0),
                    Point::new(1.0, 1.0),
                    Point::new(0.0, 1.0),
                ],
                FillStrokeStyle::fill(Color::BLACK),
            );
            assert!(matches!(pg, Element::Polygon { .. }));

            let a = Element::arc(
                Point::new(0.0, 0.0),
                Vec2::new(5.0, 5.0),
                0.0,
                std::f64::consts::FRAC_PI_2,
                Stroke::new(Color::BLACK, 1.0),
            );
            assert!(matches!(a, Element::Arc { .. }));

            let pie = Element::pie(
                Point::new(0.0, 0.0),
                5.0,
                0.0,
                std::f64::consts::FRAC_PI_2,
                FillStrokeStyle::fill(Color::BLACK),
            );
            assert!(matches!(pie, Element::Pie { .. }));

            let t = Element::text(
                "hi",
                Point::new(0.0, 0.0),
                crate::text::TextStyle::new(Color::BLACK, 12.0, "sans-serif"),
            );
            match t {
                Element::Text {
                    spans,
                    position,
                    style,
                    layout,
                } => {
                    assert_eq!(spans.len(), 1);
                    assert_eq!(spans[0].text, "hi");
                    assert_eq!(position, Point::new(0.0, 0.0));
                    assert_eq!(style.font_size, 12.0);
                    // 构造时即排版（无可用字体时为 None）。
                    if let Some(l) = layout {
                        assert!(!l.lines.is_empty());
                        assert!(l.width > 0.0 && l.height > 0.0);
                    }
                }
                _ => panic!("expected text"),
            }

            // 空文本不排版，layout 保持 None。
            let empty = Element::text(
                "",
                Point::new(0.0, 0.0),
                crate::text::TextStyle::new(Color::BLACK, 12.0, "sans-serif"),
            );
            match empty {
                Element::Text { layout, .. } => assert!(layout.is_none()),
                _ => panic!("expected text"),
            }
        }

        #[test]
        fn stroke_dash_and_miter_defaults() {
            let s = Stroke::new(Color::BLACK, 2.0);
            assert!(s.dash_array.is_empty());
            assert_eq!(s.dash_offset, 0.0);
            assert_eq!(s.miter_limit, 10.0);

            let d = Stroke::dashed(Color::BLACK, 2.0, vec![4.0, 2.0]);
            assert_eq!(d.dash_array, vec![4.0, 2.0]);
        }

        #[test]
        fn radial_gradient_into_fill() {
            let g = RadialGradient {
                center: Point::new(0.0, 0.0),
                radius: 10.0,
                stops: vec![
                    GradientStop {
                        offset: 0.0,
                        color: Color::BLACK,
                    },
                    GradientStop {
                        offset: 1.0,
                        color: Color::WHITE,
                    },
                ],
            };
            let style = FillStrokeStyle::fill_gradient(g);
            assert!(matches!(style.fill, Some(Fill::RadialGradient(_))));
        }

        #[test]
        fn rect_center_helper() {
            // kurbo Rect::new(x0, y0, x1, y1); origin(2,4) width 10 height 6 → (2,4)-(12,10).
            assert_eq!(
                Rect::new(2.0, 4.0, 12.0, 10.0).center(),
                Point::new(7.0, 7.0)
            );
        }

        #[test]
        fn clip_constructors_and_bezpath() {
            let c = Clip::circle(Point::new(1.0, 2.0), 5.0);
            assert!(matches!(c, Clip::Circle { radius: 5.0, .. }));
            // to_bezpath should produce a closed circle path
            let p = c.to_bezpath();
            assert!(!p.is_empty());

            let r = Clip::rect(Rect::new(0.0, 0.0, 10.0, 20.0));
            assert!(matches!(r, Clip::Rect(_)));
            assert!(!r.to_bezpath().is_empty());

            let rr = Clip::rounded_rect(Rect::new(0.0, 0.0, 10.0, 10.0), 2.0);
            assert!(matches!(rr, Clip::RoundedRect { .. }));

            let e = Clip::ellipse(Point::new(0.0, 0.0), Vec2::new(3.0, 4.0), 0.0);
            assert!(matches!(e, Clip::Ellipse { .. }));

            let mut bp = BezPath::new();
            bp.move_to((0.0, 0.0));
            bp.line_to((1.0, 0.0));
            let p2 = Clip::path(bp);
            assert!(matches!(p2, Clip::Path(_)));
            assert!(!p2.to_bezpath().is_empty());
        }

        #[test]
        fn scene_node_clip_builder() {
            let node = SceneNode::new(Element::rect(
                Rect::new(0.0, 0.0, 10.0, 10.0),
                FillStrokeStyle::fill(Color::BLACK),
            ))
            .with_clip(Clip::circle(Point::new(5.0, 5.0), 3.0));
            assert!(node.clip.is_some());
            // no clip by default
            let n = SceneNode::new(Element::rect(
                Rect::new(0.0, 0.0, 1.0, 1.0),
                FillStrokeStyle::fill(Color::BLACK),
            ));
            assert!(n.clip.is_none());
        }
    }

    /// Ordering: ascending z_index with same-z stability.
    mod ordering {
        use super::*;

        #[test]
        fn iter_ordered_sorts_z_ascending() {
            let mut scene = Scene::new(100.0, 100.0);
            scene.push_node(rect_node(0.0, 5));
            scene.push_node(rect_node(0.0, -3));
            scene.push_node(rect_node(0.0, 1));
            let zs: Vec<i32> = scene.iter_ordered().map(|n| n.z_index).collect();
            assert_eq!(zs, vec![-3, 1, 5]);
        }

        #[test]
        fn ordered_is_stable_within_same_z() {
            // three nodes with the same z_index; use distinguishable x coordinates to verify
            // insertion order is preserved.
            let nodes = vec![rect_node(10.0, 0), rect_node(20.0, 0), rect_node(30.0, 0)];
            let xs: Vec<f64> = SceneNode::ordered(&nodes).map(rect_x).collect();
            assert_eq!(xs, vec![10.0, 20.0, 30.0]);
        }

        #[test]
        fn layer_iter_ordered_sorts_within_layer() {
            let layer = Layer::new("l").with_nodes(vec![
                rect_node(0.0, 5),
                rect_node(0.0, -2),
                rect_node(0.0, 1),
            ]);
            let zs: Vec<i32> = layer.iter_ordered().map(|n| n.z_index).collect();
            assert_eq!(zs, vec![-2, 1, 5]);
        }
    }

    /// Scene metadata, grouping, layer loading, and queries.
    mod scene {
        use super::*;

        #[test]
        fn scene_metadata_defaults_and_builders() {
            let scene = Scene::new(10.0, 20.0)
                .with_title("图表")
                .with_description("示例")
                .with_scale(2.0);
            assert_eq!(scene.title.as_deref(), Some("图表"));
            assert_eq!(scene.description.as_deref(), Some("示例"));
            assert_eq!(scene.scale, 2.0);
            let d = Scene::default();
            assert_eq!(d.scale, 1.0);
        }

        #[test]
        fn scene_node_group_and_push_group() {
            let group = SceneNode::group(vec![rect_node(0.0, 0), rect_node(0.0, 1)]);
            match &group.element {
                Element::Group { children } => assert_eq!(children.len(), 2),
                _ => panic!("expected group"),
            }
            let mut scene = Scene::new(10.0, 10.0);
            scene.push_group(vec![rect_node(0.0, 0)]);
            assert!(matches!(&scene.nodes[0].element, Element::Group { .. }));
        }

        #[test]
        fn scene_push_layer_and_lookup() {
            let mut scene = Scene::new(100.0, 100.0);
            scene.push_layer(Layer::new("grid"));
            scene.push_layer(Layer::new("data").with_nodes(vec![rect_node(0.0, 0)]));
            assert_eq!(scene.layers().len(), 2);
            assert!(scene.layer("data").is_some());
            assert!(scene.layer("missing").is_none());
            scene.layer_mut("grid").unwrap().visible = false;
            assert!(!scene.layers[0].visible);
        }

        #[test]
        fn scene_default_has_empty_layers() {
            let scene = Scene::new(10.0, 10.0);
            assert!(scene.layers.is_empty());
        }
    }

    /// Layer defaults and builders.
    mod layer {
        use super::*;

        #[test]
        fn layer_defaults_and_builders() {
            let layer = Layer::new("data").with_nodes(vec![rect_node(0.0, 1)]);
            assert_eq!(layer.name, "data");
            assert!(layer.visible);
            assert_eq!(layer.opacity, 1.0);
            assert!(layer.transform.is_none());
            assert_eq!(layer.nodes.len(), 1);

            let l2 = Layer::new("x")
                .with_visible(false)
                .with_opacity(0.5)
                .with_transform(Transform::translate(1.0, 2.0));
            assert!(!l2.visible);
            assert_eq!(l2.opacity, 0.5);
            assert_eq!(l2.transform, Some(Transform::translate(1.0, 2.0)));
        }
    }

    /// Image resource: SceneImage object-fit mapping and construction.
    mod image {
        use super::*;

        fn img(w: u32, h: u32) -> SceneImage {
            let px = vec![0u8; (w * h * 4) as usize];
            SceneImage::from_rgba8(w, h, px).unwrap()
        }

        #[test]
        fn preserve_aspect_ratio_mapping() {
            let base = img(10, 10);
            assert_eq!(base.preserve_aspect_ratio(), "xMidYMid meet");
            assert_eq!(
                base.clone()
                    .with_object_fit(ObjectFit::Cover)
                    .preserve_aspect_ratio(),
                "xMidYMid slice"
            );
            assert_eq!(
                base.clone()
                    .with_object_fit(ObjectFit::Fill)
                    .preserve_aspect_ratio(),
                "none"
            );
            assert_eq!(
                base.clone()
                    .with_object_fit(ObjectFit::None)
                    .preserve_aspect_ratio(),
                "none"
            );
        }

        #[test]
        fn default_object_fit_is_contain() {
            assert_eq!(img(1, 1).object_fit, ObjectFit::Contain);
        }

        #[test]
        fn from_rgba8_validates_dimensions() {
            assert!(SceneImage::from_rgba8(4, 4, vec![0u8; 64]).is_some());
            // insufficient length → None.
            assert!(SceneImage::from_rgba8(4, 4, vec![0u8; 63]).is_none());
        }

        #[test]
        fn from_png_roundtrips() {
            let pm = crate::Pixmap::from_rgba8(2, 2, vec![255u8; 16]).unwrap();
            let png = pm.to_png();
            let img = SceneImage::from_png(&png).expect("有效 PNG 应可解码");
            assert_eq!(img.width(), 2);
            assert_eq!(img.height(), 2);
            assert_eq!(img.pixmap.pixels(), pm.pixels());
        }

        #[test]
        fn image_element_constructs() {
            let img = img(4, 4);
            let el = Element::image(img, Rect::new(0.0, 0.0, 10.0, 10.0));
            match &el {
                Element::Image {
                    image,
                    frame,
                    opacity,
                } => {
                    assert_eq!(image.width(), 4);
                    assert_eq!(image.height(), 4);
                    assert_eq!(frame.width(), 10.0);
                    assert_eq!(*opacity, 1.0);
                }
                _ => panic!("expected image"),
            }
        }
    }
}
