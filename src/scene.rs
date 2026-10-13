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
//! - Container vs. drawing content is modelled by [`NodeKind`]: a node either **draws** a leaf
//!   primitive ([`Element`]: geometry + style only) or is a **container** holding child nodes
//!   ([`NodeKind::Group`]). Use [`SceneNode::is_group`] / [`SceneNode::children`] /
//!   [`SceneNode::children_mut`] / [`SceneNode::element`] (and [`SceneNode::visit`] /
//!   [`SceneNode::visit_mut`]) instead of matching the kind directly, so rendering / bounds /
//!   effects / hit-testing share one recursion over the tree. [`Scene::root_lists`] /
//!   [`Scene::root_lists_mut`] likewise unify "default nodes + layers" for root-level traversals.

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

    /// Visit every node in **document order** — the default [`Scene::nodes`] first, then each
    /// [`Layer`] bottom-to-top — recursing into [`NodeKind::Group`] containers (a container before
    /// its children). `z_index` is *not* applied (this is structural order, not draw order).
    ///
    /// Layer-level attributes (`visible` / `opacity` / `transform`) are **not** applied, and no
    /// node is skipped for `visible = false`; the callback decides. Use this for hit-testing /
    /// inspection over the whole tree.
    pub fn visit_nodes(&self, f: &mut impl FnMut(&SceneNode)) {
        for node in &self.nodes {
            node.visit(f);
        }
        for layer in &self.layers {
            for node in &layer.nodes {
                node.visit(f);
            }
        }
    }

    /// Mutable variant of [`Scene::visit_nodes`] (same order).
    pub fn visit_nodes_mut(&mut self, f: &mut impl FnMut(&mut SceneNode)) {
        for node in &mut self.nodes {
            node.visit_mut(f);
        }
        for layer in &mut self.layers {
            for node in &mut layer.nodes {
                node.visit_mut(f);
            }
        }
    }

    /// The scene's **root node lists** in draw order — the default [`Scene::nodes`] first
    /// (`transform = None`), then each **visible** [`Layer`]'s nodes bottom-to-top paired with the
    /// layer's `transform`.
    ///
    /// Single source of "what are the scene's top-level lists", shared by bounds / effects /
    /// rendering so the default layer and named layers are no longer handled separately.
    pub fn root_lists(&self) -> impl Iterator<Item = (&[SceneNode], Option<Transform>)> {
        std::iter::once((self.nodes.as_slice(), None)).chain(
            self.layers
                .iter()
                .filter(|l| l.visible)
                .map(|l| (l.nodes.as_slice(), l.transform)),
        )
    }

    /// Mutable counterpart of [`Scene::root_lists`] (visible layers only), for passes that rewrite
    /// elements (e.g. the sketch pass).
    pub fn root_lists_mut(&mut self) -> impl Iterator<Item = &mut Vec<SceneNode>> {
        std::iter::once(&mut self.nodes).chain(
            self.layers
                .iter_mut()
                .filter(|l| l.visible)
                .map(|l| &mut l.nodes),
        )
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
    /// Layer-wide blend mode when compositing the layer onto the backdrop;
    /// default [`BlendMode::Normal`].
    pub blend_mode: BlendMode,
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
            blend_mode: BlendMode::Normal,
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

    /// Set the layer's blend mode (compositing onto the backdrop).
    #[must_use]
    pub fn with_blend_mode(mut self, mode: BlendMode) -> Self {
        self.blend_mode = mode;
        self
    }
}

/// The **subtree attributes** shared by a [`SceneNode`] and a [`Layer`], expressed as one
/// compositing scope.
///
/// A [`Layer`] is essentially a *named root group*, so both push exactly the same kind of scope
/// onto a backend's state stack — `transform → clip → filter → opacity → blend → mask → name`.
/// Sharing one description keeps the two render paths in sync: see
/// [`crate::render::Renderer::push_scope`].
#[derive(Debug, Clone, Copy)]
pub struct NodeScope<'a> {
    /// Transform applied to the whole subtree.
    pub transform: Option<Transform>,
    /// Clip applied to the whole subtree.
    pub clip: Option<&'a Clip>,
    /// Post-processing filter applied to the subtree's rendered content.
    pub filter: Option<&'a Filter>,
    /// Subtree opacity, 0.0–1.0; `1.0` means "no scope".
    pub opacity: f64,
    /// Blend mode used when compositing the subtree onto the backdrop.
    pub blend_mode: BlendMode,
    /// Alpha mask applied to the subtree's composited result.
    pub mask: Option<&'a Mask>,
    /// Optional scope name (the SVG backend emits it as an `id`).
    pub name: Option<&'a str>,
}

impl NodeScope<'_> {
    /// Whether this scope would do nothing at all (all attributes at their defaults).
    #[must_use]
    pub fn is_identity(&self) -> bool {
        self.transform.is_none()
            && self.clip.is_none()
            && self.filter.is_none()
            && self.opacity == 1.0
            && self.blend_mode.is_normal()
            && self.mask.is_none()
            && self.name.is_none()
    }
}

impl SceneNode {
    /// This node's compositing scope (see [`NodeScope`]).
    #[must_use]
    pub fn scope(&self) -> NodeScope<'_> {
        NodeScope {
            transform: self.transform,
            clip: self.clip.as_ref(),
            filter: self.filter.as_ref(),
            opacity: self.opacity,
            blend_mode: self.blend_mode,
            mask: self.mask.as_ref(),
            name: self.name.as_deref(),
        }
    }
}

impl Layer {
    /// This layer's compositing scope. A layer is a named root group, so it carries no
    /// clip / filter / mask.
    #[must_use]
    pub fn scope(&self) -> NodeScope<'_> {
        NodeScope {
            transform: self.transform,
            clip: None,
            filter: None,
            opacity: self.opacity,
            blend_mode: self.blend_mode,
            mask: None,
            name: (!self.name.is_empty()).then_some(self.name.as_str()),
        }
    }
}

/// What a [`SceneNode`] holds: **drawing content** (a leaf primitive) or **children** (a
/// container).
///
/// Keeping these apart means [`Element`] stays a pure "geometry + style" vocabulary — it carries
/// no `Group` variant, so the recursive tree lives entirely in [`SceneNode`]. New *container*
/// semantics (isolated layers, clip groups, …) extend this enum rather than `Element`.
#[derive(Debug, Clone)]
pub enum NodeKind {
    /// A drawable leaf primitive.
    Draw(Element),
    /// A container: children are drawn under the parent transform / clip / compositing scope.
    Group(Vec<SceneNode>),
}

/// A single node in the scene: drawing content (or children) + layer + local transform + node
/// attributes.
///
/// All fields except `kind` are "subtree-level" attributes: `transform` / `opacity` / `name` /
/// `visible` also apply to a container ([`NodeKind::Group`]) (and `opacity` / `transform` are
/// composed through recursion).
#[derive(Debug, Clone)]
pub struct SceneNode {
    /// Drawing content (a [`NodeKind::Draw`] leaf) or children (a [`NodeKind::Group`] container).
    pub kind: NodeKind,
    /// Draw order **within this node's own sibling list**: nodes are drawn in stable ascending
    /// `z_index` order (equal values keep insertion order). See [`SceneNode::ordered`].
    ///
    /// It is *not* a global z: it never reorders across a container boundary, and the default
    /// [`Scene::nodes`] list always draws **below** the named [`Layer`]s regardless of value —
    /// the layer array order wins. Use [`Scene::push_layer`] (not a large `z_index`) to lift
    /// something above everything else.
    pub z_index: i32,
    pub transform: Option<Transform>,
    /// Opacity for the node (and its subtree), 0.0–1.0; default 1.0.
    pub opacity: f64,
    /// Optional identifier: debugging, hit-testing, SVG `id`.
    pub name: Option<String>,
    /// Whether visible; default `true`. When `false` the whole subtree is skipped.
    pub visible: bool,
    /// Optional clip shape: restrict the whole subtree to its interior.
    pub clip: Option<Clip>,
    /// Blend mode when compositing this node (and its subtree) onto the backdrop;
    /// default [`BlendMode::Normal`] (no blending).
    pub blend_mode: BlendMode,
    /// Optional alpha mask applied to the subtree's composited result.
    pub mask: Option<Mask>,
    /// Optional post-processing filter applied to the subtree (blur / drop shadow).
    pub filter: Option<Filter>,
}

impl SceneNode {
    /// Node with all subtree attributes at their defaults.
    fn with_defaults(kind: NodeKind) -> Self {
        Self {
            kind,
            z_index: 0,
            transform: None,
            opacity: 1.0,
            name: None,
            visible: true,
            clip: None,
            blend_mode: BlendMode::Normal,
            mask: None,
            filter: None,
        }
    }

    /// A drawing (leaf) node from a primitive.
    pub fn new(element: Element) -> Self {
        Self::with_defaults(NodeKind::Draw(element))
    }

    /// Construct a recursive group (container) node: children are drawn under the parent
    /// transform / clip / compositing scope.
    pub fn group(children: Vec<SceneNode>) -> Self {
        Self::with_defaults(NodeKind::Group(children))
    }

    /// Yield node references in ascending `z_index` order (stable: insertion order is preserved
    /// within the same level).
    pub fn ordered(nodes: &[SceneNode]) -> impl Iterator<Item = &SceneNode> {
        let mut order: Vec<usize> = (0..nodes.len()).collect();
        // Stable sort: `sort_by_key` preserves relative order for equal `z_index`.
        order.sort_by_key(|&i| nodes[i].z_index);
        order.into_iter().map(move |i| &nodes[i])
    }

    /// Whether this node is a **container** ([`NodeKind::Group`]).
    ///
    /// Containers hold child nodes; [`NodeKind::Draw`] nodes are drawable leaves. Prefer this plus
    /// [`SceneNode::children`] / [`SceneNode::element`] over matching [`NodeKind`] directly, so
    /// every pass (rendering / bounds / effects / hit-testing) shares one notion of "the tree".
    #[must_use]
    pub fn is_group(&self) -> bool {
        matches!(self.kind, NodeKind::Group(_))
    }

    /// The leaf primitive of a drawing node, or `None` for a container.
    #[must_use]
    pub fn element(&self) -> Option<&Element> {
        match &self.kind {
            NodeKind::Draw(element) => Some(element),
            NodeKind::Group(_) => None,
        }
    }

    /// Mutable leaf primitive of a drawing node, or `None` for a container.
    pub fn element_mut(&mut self) -> Option<&mut Element> {
        match &mut self.kind {
            NodeKind::Draw(element) => Some(element),
            NodeKind::Group(_) => None,
        }
    }

    /// Consume the node, returning its leaf primitive (`None` for a container).
    ///
    /// The owned counterpart of [`SceneNode::element`]; handy when the node is a temporary (e.g. a
    /// freshly built primitive) and the element is wanted by value.
    #[must_use]
    pub fn into_element(self) -> Option<Element> {
        match self.kind {
            NodeKind::Draw(element) => Some(element),
            NodeKind::Group(_) => None,
        }
    }

    /// Child nodes of a container, or `None` for a drawing (leaf) node.
    #[must_use]
    pub fn children(&self) -> Option<&[SceneNode]> {
        match &self.kind {
            NodeKind::Group(children) => Some(children),
            NodeKind::Draw(_) => None,
        }
    }

    /// Mutable child nodes of a container, or `None` for a drawing node.
    pub fn children_mut(&mut self) -> Option<&mut Vec<SceneNode>> {
        match &mut self.kind {
            NodeKind::Group(children) => Some(children),
            NodeKind::Draw(_) => None,
        }
    }

    /// Pre-order visit over this node and its whole subtree: `self` first, then each child in
    /// stored order, recursing into nested containers (order is independent of `z_index`).
    pub fn visit(&self, f: &mut impl FnMut(&SceneNode)) {
        f(self);
        if let Some(children) = self.children() {
            for child in children {
                child.visit(f);
            }
        }
    }

    /// Mutable pre-order visit over this node and its subtree (same order as [`SceneNode::visit`]).
    ///
    /// A container's children are re-read after `f` runs, so a callback that turns a container
    /// into a leaf (or vice versa) is honoured.
    pub fn visit_mut(&mut self, f: &mut impl FnMut(&mut SceneNode)) {
        f(self);
        if let Some(children) = self.children_mut() {
            for child in children.iter_mut() {
                child.visit_mut(f);
            }
        }
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

    /// Set the blend mode used when compositing this node (and its subtree) onto the backdrop.
    #[must_use]
    pub fn with_blend_mode(mut self, mode: BlendMode) -> Self {
        self.blend_mode = mode;
        self
    }

    /// Set an alpha mask applied to the subtree's composited result.
    #[must_use]
    pub fn with_mask(mut self, mask: Mask) -> Self {
        self.mask = Some(mask);
        self
    }

    /// Set a post-processing filter (blur / drop shadow) applied to the subtree.
    #[must_use]
    pub fn with_filter(mut self, filter: Filter) -> Self {
        self.filter = Some(filter);
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

/// An alpha mask applied to a node's subtree.
///
/// Unlike [`Clip`] (which clips each primitive at draw time), a mask multiplies the alpha of the
/// subtree's **composited result**, so non-binary content is supported: the mask value is the
/// paint's alpha where `shape` covers — a translucent solid gives a uniform partial mask, a
/// gradient gives a smooth fade.
///
/// Backends: vello builds a canvas-sized mask bitmap and uses `RenderContext::push_layer`; SVG
/// emits `<mask>` (`maskContentUnits="userSpaceOnUse"`).
#[derive(Debug, Clone, PartialEq)]
pub struct Mask {
    /// Shape the mask paint fills (reuses the [`Clip`] geometry vocabulary).
    pub shape: Clip,
    /// Paint whose alpha becomes the mask value.
    pub fill: Fill,
}

impl Mask {
    /// Opaque mask covering `shape` (equal to a [`Clip`], but applied at composite time).
    #[must_use]
    pub fn new(shape: Clip) -> Self {
        Self {
            shape,
            fill: Fill::Solid(Color::WHITE),
        }
    }

    /// Set the mask paint: its alpha is the mask value (`impl Into<Fill>` accepts a [`Color`],
    /// [`LinearGradient`] or [`RadialGradient`]).
    #[must_use]
    pub fn with_fill(mut self, fill: impl Into<Fill>) -> Self {
        self.fill = fill.into();
        self
    }
}

/// A post-processing filter applied to a node's subtree.
///
/// The subtree is rendered into an offscreen layer, filtered, then composited — mirroring CSS
/// `filter` / SVG filter effects. Only effects supported by **both** backends are exposed (vello's
/// filter support is single-primitive and experimental; it also panics under multi-threaded
/// rendering, which lievisual does not enable by default).
#[derive(Debug, Clone, PartialEq)]
pub enum Filter {
    /// Gaussian blur; `std_dev` is the standard deviation σ in px (visible reach ≈ 3σ).
    Blur {
        /// Standard deviation of the Gaussian kernel (px).
        std_dev: f64,
    },
    /// Drop shadow (the source is composited on top of the shadow).
    DropShadow {
        /// Horizontal offset (px; positive = right).
        dx: f64,
        /// Vertical offset (px; positive = down).
        dy: f64,
        /// Blur standard deviation of the shadow (px).
        std_dev: f64,
        /// Shadow color (its alpha is the shadow opacity).
        color: Color,
    },
}

impl Filter {
    /// A Gaussian blur with standard deviation `std_dev` (px).
    #[must_use]
    pub fn blur(std_dev: f64) -> Self {
        Self::Blur { std_dev }
    }

    /// A drop shadow offset by `(dx, dy)`, blurred by `std_dev`, in `color`.
    #[must_use]
    pub fn drop_shadow(dx: f64, dy: f64, std_dev: f64, color: Color) -> Self {
        Self::DropShadow {
            dx,
            dy,
            std_dev,
            color,
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
    /// Sweep (conic) gradient — see [`SweepGradient`].
    ///
    /// Exact on the raster backend (`peniko::GradientKind::Sweep`). SVG has no conic paint, so
    /// [`SvgRenderer`](crate::render::SvgRenderer) approximates it by clipped wedges (see the
    /// backend docs); strokes cannot carry a gradient at all in this IR, so a fill is the only
    /// place a sweep appears.
    SweepGradient(SweepGradient),
    /// Hand-drawn ("sketch") fill: a *style hint*, not literal geometry.
    ///
    /// The IR only records "this shape wants a hand-drawn fill"; each backend decides how to
    /// express it:
    /// - SVG: `<pattern>` (a tiny seamless tile + `patternTransform`), so the output stays small;
    /// - raster (vello): explicit hachure strokes / dots, clipped to the shape.
    ///
    /// Nothing produces this variant unless [`crate::sketch`] is enabled — a scene built
    /// normally never contains it, so default rendering is bit-for-bit unchanged.
    Sketch(SketchFill),
}

/// Hand-drawn fill style (rough.js semantics: the fill is *drawn*, not painted).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SketchFillStyle {
    /// Parallel hachure lines (the rough.js default).
    #[default]
    Hachure,
    /// Two crossing hachure families (90° apart).
    CrossHatch,
    /// Zig-zag hachure.
    Zigzag,
    /// Dot grid.
    Dots,
}

/// Parameters of a [`Fill::Sketch`] fill.
///
/// `angle` / `gap` / `fill_weight` mirror rough.js' `hachureAngle` / `hachureGap` /
/// `fillWeight`; both backends read the same numbers, so the two outputs agree on line
/// direction, spacing and weight (they differ only in the rasterizer's anti-aliasing).
#[derive(Debug, Clone, PartialEq)]
pub struct SketchFill {
    /// Colour of the drawn lines / dots (rough.js draws the fill in the fill colour).
    pub color: Color,
    /// Fill style.
    pub style: SketchFillStyle,
    /// Hachure direction in **degrees**. Default `-41.0` (rough.js' `hachureAngle`).
    pub angle: f64,
    /// Spacing between two hachure lines, in scene units. Default `4.0`.
    pub gap: f64,
    /// Stroke width of a single hachure line. Default `0.5`.
    pub fill_weight: f64,
}

impl SketchFill {
    /// A hachure fill in `color`, with rough.js' default angle / gap / weight.
    #[must_use]
    pub fn new(color: Color) -> Self {
        Self {
            color,
            style: SketchFillStyle::Hachure,
            angle: -41.0,
            gap: 4.0,
            fill_weight: 0.5,
        }
    }

    /// Set the fill style.
    #[must_use]
    pub fn with_style(mut self, style: SketchFillStyle) -> Self {
        self.style = style;
        self
    }

    /// Set the hachure direction (degrees).
    #[must_use]
    pub fn with_angle(mut self, angle: f64) -> Self {
        self.angle = angle;
        self
    }

    /// Set the spacing between hachure lines.
    #[must_use]
    pub fn with_gap(mut self, gap: f64) -> Self {
        self.gap = gap;
        self
    }

    /// Set the weight (stroke width) of a hachure line.
    #[must_use]
    pub fn with_fill_weight(mut self, weight: f64) -> Self {
        self.fill_weight = weight;
        self
    }

    /// Effective gap, guarded against degenerate values (`0` / negative / NaN would make the
    /// line count explode or the tile degenerate).
    #[must_use]
    pub fn gap(&self) -> f64 {
        if self.gap.is_finite() && self.gap > 0.05 {
            self.gap
        } else {
            4.0
        }
    }

    /// Effective weight, guarded against non-positive / non-finite values.
    #[must_use]
    pub fn weight(&self) -> f64 {
        if self.fill_weight.is_finite() && self.fill_weight > 0.0 {
            self.fill_weight.min(self.gap() * 0.5)
        } else {
            0.5
        }
    }
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

impl From<SweepGradient> for Fill {
    fn from(g: SweepGradient) -> Self {
        Self::SweepGradient(g)
    }
}

/// How a gradient behaves **outside** its base range (SVG `spreadMethod`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GradientSpread {
    /// Clamp to the end colours (SVG `pad`; the default).
    #[default]
    Pad,
    /// Repeat the ramp (SVG `repeat`).
    Repeat,
    /// Mirror the ramp (SVG `reflect`).
    Reflect,
}

/// Linear gradient (shared by `GradientPath` and `Fill::LinearGradient`).
#[derive(Debug, Clone, PartialEq)]
pub struct LinearGradient {
    pub start: Point,
    pub end: Point,
    pub stops: Vec<GradientStop>,
    /// Behaviour outside `start..end`; default [`GradientSpread::Pad`].
    pub spread: GradientSpread,
}

impl LinearGradient {
    /// Gradient from `start` to `end` (`spread = Pad`).
    #[must_use]
    pub fn new(start: Point, end: Point, stops: Vec<GradientStop>) -> Self {
        Self {
            start,
            end,
            stops,
            spread: GradientSpread::Pad,
        }
    }

    /// Set the spread mode (behaviour outside `start..end`).
    #[must_use]
    pub fn with_spread(mut self, spread: GradientSpread) -> Self {
        self.spread = spread;
        self
    }
}

/// Radial gradient: spreads outward from `center` with `radius` as the outer circle radius.
#[derive(Debug, Clone, PartialEq)]
pub struct RadialGradient {
    pub center: Point,
    pub radius: f64,
    pub stops: Vec<GradientStop>,
    /// Behaviour outside `0..radius`; default [`GradientSpread::Pad`].
    pub spread: GradientSpread,
}

impl RadialGradient {
    /// Gradient centred on `center` with `radius` (`spread = Pad`).
    #[must_use]
    pub fn new(center: Point, radius: f64, stops: Vec<GradientStop>) -> Self {
        Self {
            center,
            radius,
            stops,
            spread: GradientSpread::Pad,
        }
    }

    /// Set the spread mode (behaviour outside `0..radius`).
    #[must_use]
    pub fn with_spread(mut self, spread: GradientSpread) -> Self {
        self.spread = spread;
        self
    }
}

/// Sweep (conic) gradient: the **angle** around `center` drives the ramp — the natural fit for
/// gauges, progress rings and pie sectors.
///
/// Angles are radians in the scene's y-down system, measured from the +x axis and increasing
/// **clockwise** (the same convention as [`Element::Arc`] / [`Element::Pie`]). `start_angle` maps
/// to ramp offset 0 and `end_angle` to offset 1, so a partial range expresses a progress arc; use
/// [`SweepGradient::full`] for a whole turn.
#[derive(Debug, Clone, PartialEq)]
pub struct SweepGradient {
    pub center: Point,
    /// Angle (radians) mapped to ramp offset 0.
    pub start_angle: f64,
    /// Angle (radians) mapped to ramp offset 1.
    pub end_angle: f64,
    pub stops: Vec<GradientStop>,
    /// Behaviour outside `start_angle..end_angle`; default [`GradientSpread::Pad`].
    pub spread: GradientSpread,
}

impl SweepGradient {
    /// Sweep from `start_angle` to `end_angle` (`spread = Pad`).
    #[must_use]
    pub fn new(center: Point, start_angle: f64, end_angle: f64, stops: Vec<GradientStop>) -> Self {
        Self {
            center,
            start_angle,
            end_angle,
            stops,
            spread: GradientSpread::Pad,
        }
    }

    /// A whole turn starting at `start_angle` (i.e. `start_angle .. start_angle + 2π`).
    #[must_use]
    pub fn full(center: Point, start_angle: f64, stops: Vec<GradientStop>) -> Self {
        Self::new(
            center,
            start_angle,
            start_angle + std::f64::consts::TAU,
            stops,
        )
    }

    /// Set the spread mode (behaviour outside `start_angle..end_angle`).
    #[must_use]
    pub fn with_spread(mut self, spread: GradientSpread) -> Self {
        self.spread = spread;
        self
    }

    /// Ramp position (`0..1`, or outside it for non-[`Pad`](GradientSpread::Pad) spreads) that
    /// `angle` maps to.
    #[must_use]
    pub fn ramp_t(&self, angle: f64) -> f64 {
        let span = self.end_angle - self.start_angle;
        if span.abs() < 1e-12 {
            return 0.0;
        }
        let raw = (angle - self.start_angle) / span;
        match self.spread {
            GradientSpread::Pad => raw.clamp(0.0, 1.0),
            GradientSpread::Repeat => raw.rem_euclid(1.0),
            GradientSpread::Reflect => {
                let m = raw.rem_euclid(2.0);
                if m > 1.0 { 2.0 - m } else { m }
            }
        }
    }

    /// Colour at `angle` (radians, scene convention), interpolating the stops.
    #[must_use]
    pub fn sample(&self, angle: f64) -> Color {
        sample_stops(&self.stops, self.ramp_t(angle))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct GradientStop {
    pub offset: f64,
    pub color: Color,
}

/// Sample a sorted stop ramp at `t`: clamp to the end colours outside the ramp, otherwise
/// linearly interpolate between the two surrounding stops.
///
/// Interpolation is straight (non-premultiplied) RGBA, matching how the backends blend adjacent
/// stops of a native gradient.
#[must_use]
pub fn sample_stops(stops: &[GradientStop], t: f64) -> Color {
    let Some(first) = stops.first() else {
        return Color::default();
    };
    if t <= first.offset {
        return first.color;
    }
    for w in stops.windows(2) {
        let (a, b) = (&w[0], &w[1]);
        if t >= a.offset && t <= b.offset {
            let span = b.offset - a.offset;
            let u = if span > 1e-12 {
                (t - a.offset) / span
            } else {
                1.0
            };
            return lerp_color(a.color, b.color, u);
        }
    }
    stops.last().map_or(first.color, |s| s.color)
}

/// Straight (non-premultiplied) RGBA interpolation, rounded to the nearest byte.
fn lerp_color(a: Color, b: Color, t: f64) -> Color {
    let t = t.clamp(0.0, 1.0);
    let ch = |x: u8, y: u8| {
        let v = f64::from(x) + (f64::from(y) - f64::from(x)) * t;
        v.round().clamp(0.0, 255.0) as u8
    };
    Color::rgba(ch(a.r, b.r), ch(a.g, b.g), ch(a.b, b.b), ch(a.a, b.a))
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

/// Layer blend mode when compositing a scope onto its backdrop.
///
/// Mirrors CSS `mix-blend-mode` (W3C Compositing and Blending Level 1). Backends map it to
/// `peniko::Mix` (vello) or the `mix-blend-mode` style property (SVG). `Normal` (the default)
/// performs no blending.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BlendMode {
    /// No blending: the source replaces the backdrop (per its own alpha).
    #[default]
    Normal,
    Multiply,
    Screen,
    Overlay,
    Darken,
    Lighten,
    ColorDodge,
    ColorBurn,
    HardLight,
    SoftLight,
    Difference,
    Exclusion,
    Hue,
    Saturation,
    Color,
    Luminosity,
    // ---- Porter-Duff compositing ops (no CSS `mix-blend-mode` equivalent; raster-only) ----
    /// Clear the destination (transparent where the scope covers).
    Clear,
    /// Replace the destination with the source (Porter-Duff *Src*).
    Copy,
    /// Keep only the destination.
    Dest,
    /// Destination placed over the source.
    DestOver,
    /// Source ∩ destination.
    SrcIn,
    /// Destination ∩ source.
    DestIn,
    /// Source outside the destination.
    SrcOut,
    /// Destination outside the source.
    DestOut,
    /// Source over the destination, both clipped to the source.
    SrcAtop,
    /// Destination over the source, both clipped to the destination.
    DestAtop,
    /// The non-overlapping regions of source and destination.
    Xor,
    /// Additive: source + destination (clamped).
    Plus,
    /// Additive with a lighter weight (`Plus`, non-premultiplied sum).
    PlusLighter,
}

impl BlendMode {
    /// Whether this is [`BlendMode::Normal`] (no blending).
    #[must_use]
    pub fn is_normal(self) -> bool {
        matches!(self, BlendMode::Normal)
    }

    /// The CSS `mix-blend-mode` keyword, or `None` for [`BlendMode::Normal`] (which emits no
    /// blending and can be skipped entirely).
    #[must_use]
    pub fn css_keyword(self) -> Option<&'static str> {
        Some(match self {
            BlendMode::Normal => return None,
            BlendMode::Multiply => "multiply",
            BlendMode::Screen => "screen",
            BlendMode::Overlay => "overlay",
            BlendMode::Darken => "darken",
            BlendMode::Lighten => "lighten",
            BlendMode::ColorDodge => "color-dodge",
            BlendMode::ColorBurn => "color-burn",
            BlendMode::HardLight => "hard-light",
            BlendMode::SoftLight => "soft-light",
            BlendMode::Difference => "difference",
            BlendMode::Exclusion => "exclusion",
            BlendMode::Hue => "hue",
            BlendMode::Saturation => "saturation",
            BlendMode::Color => "color",
            BlendMode::Luminosity => "luminosity",
            // Porter-Duff ops (`Clear` … `PlusLighter`) have no `mix-blend-mode` equivalent.
            _ => return None,
        })
    }

    /// Whether this is a **Porter-Duff compositing op** (`Clear` … `PlusLighter`) rather than a
    /// CSS `mix-blend-mode`.
    ///
    /// Porter-Duff ops are **raster-only**: SVG (and the browser) expose no per-element equivalent,
    /// so the SVG backend degrades them to [`BlendMode::Normal`].
    #[must_use]
    pub fn is_porter_duff(self) -> bool {
        matches!(
            self,
            BlendMode::Clear
                | BlendMode::Copy
                | BlendMode::Dest
                | BlendMode::DestOver
                | BlendMode::SrcIn
                | BlendMode::DestIn
                | BlendMode::SrcOut
                | BlendMode::DestOut
                | BlendMode::SrcAtop
                | BlendMode::DestAtop
                | BlendMode::Xor
                | BlendMode::Plus
                | BlendMode::PlusLighter
        )
    }
}

/// Declarative primitive enum. Describes geometry and style only.
///
/// Containers are **not** elements: a node holding children is a [`NodeKind::Group`], so `Element`
/// never references [`SceneNode`] and the recursive tree lives entirely in the scene graph.
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
        match node.element() {
            Some(Element::Rect { rect, .. }) => rect.min_x(),
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
            let g = RadialGradient::new(
                Point::new(0.0, 0.0),
                10.0,
                vec![
                    GradientStop {
                        offset: 0.0,
                        color: Color::BLACK,
                    },
                    GradientStop {
                        offset: 1.0,
                        color: Color::WHITE,
                    },
                ],
            );
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

        /// `z_index` 只在**同级**排序：默认层永远画在命名 layer 之下，即使它的 z 更大。
        #[test]
        fn z_index_does_not_cross_root_lists() {
            let mut scene = Scene::new(10.0, 10.0);
            scene.push_node(rect_node(1.0, 100)); // 默认层，z 很大
            scene.push_layer(Layer::new("top").with_nodes(vec![rect_node(2.0, -100)]));
            let drawn: Vec<f64> = scene
                .root_lists()
                .flat_map(|(nodes, _)| SceneNode::ordered(nodes).map(rect_x).collect::<Vec<_>>())
                .collect();
            assert_eq!(drawn, vec![1.0, 2.0], "layer 数组顺序优先于 z_index");
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
            assert_eq!(group.children().unwrap().len(), 2);
            assert!(group.element().is_none(), "容器没有 leaf element");
            let mut scene = Scene::new(10.0, 10.0);
            scene.push_group(vec![rect_node(0.0, 0)]);
            assert!(scene.nodes[0].is_group());
        }

        /// 容器访问器（`is_group` / `children` / `children_mut`）与前后序遍历。
        #[test]
        fn container_accessors_and_traversal() {
            // group( leafA, group( leafB ) )
            let outer = SceneNode::group(vec![
                rect_node(0.0, 0),
                SceneNode::group(vec![rect_node(5.0, 0)]),
            ]);
            assert!(outer.is_group());
            assert_eq!(outer.children().map(|c| c.len()), Some(2));
            assert!(!outer.children().unwrap()[0].is_group());
            assert!(outer.children().unwrap()[1].is_group());
            // A leaf has no children.
            assert!(rect_node(0.0, 0).children().is_none());

            // Pre-order: outer, leafA, inner, leafB → 4 nodes.
            let mut visited = 0usize;
            outer.visit(&mut |_| visited += 1);
            assert_eq!(visited, 4);

            // Mutable access reaches nested containers.
            let mut outer = outer;
            outer.children_mut().unwrap()[1]
                .children_mut()
                .unwrap()
                .push(rect_node(9.0, 0));
            assert_eq!(outer.children().unwrap()[1].children().unwrap().len(), 2);
        }

        /// `visit_mut` 覆盖整棵子树（根容器 + 每个子节点）。
        #[test]
        fn visit_mut_reaches_every_node() {
            let mut outer = SceneNode::group(vec![rect_node(0.0, 0)]);
            let mut seen = 0usize;
            outer.visit_mut(&mut |_| seen += 1);
            assert_eq!(seen, 2, "根容器 + 1 个子节点");
        }

        /// `Scene::visit_nodes` 覆盖默认层与各 layer 的全部节点（含 Group 递归）。
        #[test]
        fn scene_visit_nodes_covers_default_and_layers() {
            let mut scene = Scene::new(20.0, 20.0);
            scene.push(rect_node(0.0, 0)); // 默认层：叶子
            scene.push_group(vec![rect_node(0.0, 0)]); // 默认层：Group + 子节点
            scene.push_layer(Layer::new("L").with_nodes(vec![rect_node(0.0, 0)]));
            let mut seen = 0usize;
            scene.visit_nodes(&mut |_| seen += 1);
            assert_eq!(seen, 1 + 1 + 1 + 1);
        }

        /// `Scene::root_lists` = 默认层 + 可见 layer（隐藏 layer 跳过），顺序与绘制一致。
        #[test]
        fn scene_root_lists_skip_hidden_layers() {
            let mut scene = Scene::new(20.0, 20.0);
            scene.push(rect_node(0.0, 0));
            scene.push_layer(Layer::new("a").with_nodes(vec![rect_node(0.0, 0)]));
            scene.push_layer(
                Layer::new("b")
                    .with_nodes(vec![rect_node(0.0, 0), rect_node(1.0, 0)])
                    .with_visible(false),
            );
            let sizes: Vec<usize> = scene.root_lists().map(|(n, _)| n.len()).collect();
            assert_eq!(sizes, vec![1, 1], "默认层 + 可见层 a；隐藏层 b 被跳过");
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

        /// `Layer::scope()` 把 layer 属性映射成与节点一致的合成作用域（B：统一 scope 路径）。
        #[test]
        fn layer_scope_matches_its_attributes() {
            let layer = Layer::new("l").with_opacity(0.5);
            let s = layer.scope();
            assert_eq!(s.opacity, 0.5);
            assert_eq!(s.blend_mode, BlendMode::Normal);
            assert_eq!(s.name, Some("l"));
            assert!(s.clip.is_none() && s.filter.is_none() && s.mask.is_none());
            assert!(!s.is_identity());

            // 无名且全默认的 layer 不产生任何作用域。
            assert!(Layer::new("").scope().is_identity());
        }
    }

    /// 渐变：stop 采样与 sweep 的 spread 语义。
    mod gradient {
        use super::*;

        fn ramp() -> Vec<GradientStop> {
            vec![
                GradientStop {
                    offset: 0.0,
                    color: Color::rgb(0, 0, 0),
                },
                GradientStop {
                    offset: 1.0,
                    color: Color::rgb(255, 255, 255),
                },
            ]
        }

        #[test]
        fn sample_stops_interpolates_and_clamps() {
            let s = ramp();
            assert_eq!(sample_stops(&s, -1.0).r, 0);
            assert_eq!(sample_stops(&s, 0.0).r, 0);
            assert_eq!(sample_stops(&s, 0.5).r, 128);
            assert_eq!(sample_stops(&s, 2.0).r, 255);
        }

        #[test]
        fn sweep_ramp_honours_spread() {
            use std::f64::consts::{FRAC_PI_2, PI, TAU};
            let s = ramp();
            // 四分之一圈的 ramp。
            let g = SweepGradient::new(Point::ZERO, 0.0, FRAC_PI_2, s.clone());
            assert!(g.ramp_t(0.0).abs() < 1e-12);
            assert!((g.ramp_t(FRAC_PI_2) - 1.0).abs() < 1e-12);
            // Pad：范围外夹到端色。
            assert!((g.ramp_t(PI) - 1.0).abs() < 1e-12);
            assert_eq!(g.sample(0.0).r, 0);
            assert_eq!(g.sample(PI).r, 255);

            // Repeat：整周期回卷（π 恰好 2 个周期）。
            let rep = g.clone().with_spread(GradientSpread::Repeat);
            assert!(rep.ramp_t(PI).abs() < 1e-12);
            // Reflect：2.5 个周期处折回 0.5。
            let refl = g.with_spread(GradientSpread::Reflect);
            assert!((refl.ramp_t(1.25 * PI) - 0.5).abs() < 1e-12);

            // full() 覆盖整圈，且可直接作为 Fill。
            let full = SweepGradient::full(Point::ZERO, 0.0, s);
            assert!((full.end_angle - TAU).abs() < 1e-12);
            assert!(matches!(Fill::from(full), Fill::SweepGradient(_)));
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
