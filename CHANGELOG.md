# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **First-class container / traversal API on the scene tree** (pure refactor, no behaviour change):
  - `SceneNode::{is_group, children, children_mut}` — inspect a `Group` container without matching
    `Element::Group` directly.
  - `SceneNode::{visit, visit_mut}` — pre-order traversal of a node and its whole subtree.
  - `Scene::{visit_nodes, visit_nodes_mut}` — document-order traversal over the default node list
    and all layers.
  - `Scene::{root_lists, root_lists_mut}` — the scene's top-level node lists (the default list plus
    each visible layer with its `transform`), shared by bounds / effects / rendering.
  - `Renderer::render_children` — the single recursion used by the default list, layers and groups.
  Internally this collapses the ad-hoc `match Element::Group` in the sketch / fit / render passes
  onto the shared accessors.
- **`BlendMode`** (CSS `mix-blend-mode` / W3C Compositing Level 1) as a first-class compositing
  attribute:
  - `SceneNode::blend_mode` + `SceneNode::with_blend_mode`, and `Layer::blend_mode` +
    `Layer::with_blend_mode`; `builder::Ctx::with_blend` applies it to every produced node.
  - `Renderer::push_blend` / `pop_blend` (default no-op, so backends without blend support degrade
    to `Normal`). The default node/layer scope pushes blend **outside** opacity, so a scope is first
    scaled by opacity and then blended with the backdrop.
  - vello maps to `peniko::BlendMode`; SVG emits `<g style="mix-blend-mode:…">`.
- **`Mask`** — an alpha mask applied to a node's subtree at **compositing** time (not a
  per-primitive clip), so non-binary masks work (a translucent solid or a gradient gives a smooth
  fade): `Mask { shape: Clip, fill: Fill }` (`Mask::new` / `with_fill`), `SceneNode::mask` +
  `SceneNode::with_mask`, `builder::Ctx::with_mask`, and `Renderer::push_mask` / `pop_mask`
  (default no-op). The mask is the **outermost** compositing scope. vello rasterizes it into a
  canvas-sized `vello_cpu::Mask` and uses `push_layer(mask)`; SVG emits
  `<mask maskContentUnits="userSpaceOnUse">` + `mask="url(#…)"`.
- **`Filter`** — a post-processing filter applied to a node's subtree (rendered offscreen,
  filtered, then composited): `Filter::blur(std_dev)` and
  `Filter::drop_shadow(dx, dy, std_dev, color)`; `SceneNode::filter` + `SceneNode::with_filter`,
  `builder::Ctx::with_filter`, and `Renderer::push_filter` / `pop_filter` (default no-op). Scope
  order is `clip → filter → opacity → blend → mask` (mirrors CSS `filter` → opacity →
  `mix-blend-mode`). vello maps to `filter_effects::{FilterFunction::Blur,
  FilterPrimitive::DropShadow}`; SVG emits `<filter>` with `feGaussianBlur` / `feDropShadow`.
  Note: vello's filter support is single-primitive and panics under multi-threaded rendering
  (which lievisual does not enable).
- **Porter-Duff compositing ops** on `BlendMode` — `Clear`, `Copy`, `Dest`, `DestOver`, `SrcIn`,
  `DestIn`, `SrcOut`, `DestOut`, `SrcAtop`, `DestAtop`, `Xor`, `Plus`, `PlusLighter` — mapped to
  peniko's `Compose` (with the identity mix). `BlendMode::is_porter_duff()` distinguishes them.
  They are **raster-only**: SVG (and the browser) expose no per-element equivalent, so the SVG
  backend degrades them to `Normal` (rendered without a `mix-blend-mode` group).
- **Sweep (conic) gradients** — `SweepGradient` / `Fill::SweepGradient`, the natural fit for
  gauges, progress rings and pie sectors: the **angle** around a centre drives the ramp.
  `SweepGradient::new(center, start_angle, end_angle, stops)` (radians, y-down, clockwise — the
  same convention as `Element::Arc` / `Element::Pie`) and `SweepGradient::full(center, start, stops)`
  for a whole turn; a partial range expresses a progress arc. Exact on the raster backend
  (`peniko::GradientKind::Sweep`). SVG has no conic paint, so the SVG backend **approximates** it by
  using the shape itself as a `<clipPath>` and painting `SWEEP_WEDGES` (120) solid wedges sampled
  from the ramp (a whole turn is painted; `spread` decides what the angles outside the range look
  like). Inside `<mask>` content a sweep degrades to the colour at the middle of its ramp.
- **Gradient spread** — `GradientSpread::{Pad, Repeat, Reflect}` (SVG `spreadMethod`) on
  `LinearGradient` / `RadialGradient` via `with_spread`, mapped to peniko `Extend` (raster) and
  `spreadMethod` (SVG; `Pad` is the default, so it stays implicit and the common output is
  byte-identical). New constructors `LinearGradient::new(start, end, stops)` /
  `RadialGradient::new(center, radius, stops)`.
- `scene::sample_stops` (ramp sampling: clamp outside, linear interpolation inside),
  `SweepGradient::ramp_t` / `SweepGradient::sample` — the shared sampling used by the SVG wedge
  approximation.

### Changed

- **BREAKING:** `LinearGradient` / `RadialGradient` gained a `spread` field, so struct literals no
  longer compile — construct with `LinearGradient::new(...)` / `RadialGradient::new(...)` (then
  `.with_spread(...)`). `Fill` gained a `SweepGradient` variant, so exhaustive matches need a new
  arm. Downstream adaptations: `liecharts` 1 literal, `liemermaid` 1 literal.


- **BREAKING — container vs. drawing content is now modelled by `NodeKind`.** `SceneNode.element`
  became `SceneNode.kind`: `NodeKind::Draw(Element)` (a drawable leaf) or
  `NodeKind::Group(Vec<SceneNode>)` (a container). `Element::Group` has been **removed** — `Element`
  is now purely "geometry + style" and no longer references `SceneNode`, so future container
  semantics (isolated layers, clip groups, …) extend `NodeKind` instead of polluting `Element`.
  Migration:
  - construct with `SceneNode::new(element)` / `SceneNode::group(children)` (unchanged signatures);
  - read a leaf via `SceneNode::element()` / `element_mut()` (or `into_element()` to take it by
    value) instead of matching `node.element` directly;
  - containers are still reached through `is_group()` / `children()` / `children_mut()` / `visit` /
    `visit_mut`, so traversal code needs no change;
  - replace `Element::Group { children }` matches with `children()` / `is_group()`.
  `Renderer::draw_element(element)` is the new single place that knows every `Element` variant.
  Downstream adaptations: `liemermaid` (1 construction site), `liecharts` (17 `.element` matches),
  `liepress` (1 struct literal, now `SceneNode::new(...).with_z(...)`).
- **`Layer` and `SceneNode` now share one compositing scope.** New `NodeScope` (transform / clip /
  filter / opacity / blend / mask / name) with `SceneNode::scope()` and `Layer::scope()`, plus
  `Renderer::push_scope` / `pop_scope` as the single implementation — `render_layer` no longer
  duplicates the push / pop sequence. Non-breaking: `Layer` semantics are unchanged (a layer is a
  named root group, so it still carries no clip / filter / mask).
- `SceneNode::z_index` is documented as **sibling-local ordering only** — it never reorders across a
  container boundary, and the default `Scene::nodes` list always draws below the named `Layer`s
  regardless of value. Regression test: `z_index_does_not_cross_root_lists`.

### Fixed

- **Node / layer opacity is now isolated compositing.** The vello backend previously multiplied the
  opacity into each primitive's color alpha, so a `Group`/`Layer` with **overlapping** sub-shapes
  composited differently from the SVG backend's `<g opacity="…">` (overlaps came out darker). Opacity
  is now applied as a `vello_cpu` layer (`RenderContext::push_layer`), i.e. to the *composited
  result* of the scope — matching SVG and fixing the cross-backend inconsistency (roadmap P0-1).
  Regression test: `render::vello_pixmap::tests::group_opacity_is_isolated`.

## [0.3.0] — 2026-10-13

Breaking release aligned with `vello_cpu` 0.3 / `parley` 0.12. Downstream can now depend on
lievisual alone for rasterization and text layout — no direct `vello_cpu` / `parley` dependency.

### Changed

- **`VelloPixmapRenderer::render_scene_to_pixmap` now returns [`crate::Pixmap`]** (lievisual's own
  non-premultiplied RGBA8 bitmap) instead of `vello_cpu::Pixmap`. This removes the last public API
  that leaked a `vello_cpu` type, so downstream no longer needs `vello_cpu` in its dependency tree.
  `render_png` is unchanged (`-> Vec<u8>`).
- Text metrics migrated to parley 0.12: `LineMetrics` drops the `ascent` / `descent` fields (now the
  methods `ascent()` / `descent()`) and gains `block_min` / `block_max` / `content_block_min` /
  `content_block_max` / `cap_height` / `x_height`. `TextLayout` / `TextLine` drop the per-glyph
  `ink_bounds` field and `TextLayout::ink_bounds()`; use the font-level boxes `layout_bounds` /
  `line_box_bounds` / `logical_bounds` / `cap_bounds` instead.
- `parley` re-export bumped to 0.12.

### Removed

- **`GlyphOutline` / the `skrifa` dependency.** The glyph-outline extractor
  (`GlyphOutline::outline` / `GlyphOutline::ink_box`, glyph id → `kurbo::BezPath`) and its direct
  `skrifa` dependency have been removed; no backend or internal caller used it, and the visual text
  box is now derived from font-level metrics (`TextLine::logical_bounds` / `cap_bounds`). This is a
  **breaking** change for downstream code that imported `GlyphOutline` (e.g. for PDF/DOCX vector
  glyphs).

## [0.2.0-beta.2] — 2026-08-31

Second pre-release of 0.2.0: **dual licensing** and a rewritten English README. No code changes.

### Changed

- **Dual-licensed under MIT OR Apache-2.0** (previously Apache-2.0 only). The Apache text lives in
  `LICENSE-APACHE` (copyright placeholder filled in), MIT in `LICENSE-MIT`; `Cargo.toml` now
  declares `license = "MIT OR Apache-2.0"`.
- README rewritten in English and slimmed down (~163 → ~110 lines).

## [0.2.0-beta.1] — 2026-08-31

首个 0.2 预发布，供下游（liemermaid / liepress / liecharts）实机验证破坏性变更。
验证通过后内容原样转为 0.2.0 正式版。

A pre-release of 0.2.0, focused on **cross-backend consistency** (SVG and raster must agree),
**text correctness**, and a much larger **Canvas-like construction API**.

### Added

- **`fit` module** — content-bounds and canvas-fitting helpers that previously had to be
  re-implemented downstream:
  - `element_bounds` / `node_bounds` / `nodes_bounds` / `scene_bounds`
  - `FitOptions` + `fit_scene(&mut Scene, opts)`
  - Bounds account for stroke half-width, node/layer transforms, text rotation and
    `baseline_shift`; `visible = false` subtrees are skipped; elliptical arcs use exact extrema
    instead of the full-ellipse box.
- **`Path2D`** — a mutable path accumulator mirroring Canvas's `beginPath` … `fill()` workflow:
  `move_to` / `line_to` / `hline_to` / `vline_to` / `quad_to` / `curve_to` / `arc` / `arc_ccw` /
  `ellipse` / `rect` / `round_rect` / `circle` / `add_path` / `close_path`.
  Arcs are approximated with at most one cubic Bézier per 90°.
- **`Ctx` gains the rest of the Canvas vocabulary plus lievisual's superset**:
  - Path terminators: `fill(&path)` / `stroke(&path)` / `fill_stroke_path(&path)`
  - Node attributes: `with_transform` / `translate` / `rotate` / `scale` / `with_alpha` /
    `with_clip` / `with_name` / `with_z`
  - Fine-grained stroke: `with_line_width` / `with_line_cap` / `with_line_join` /
    `with_miter_limit` / `with_dash`
  - Fill: `with_fill_gradient` / `without_fill` / `without_stroke`
  - Shapes: `fill_stroke_rect` / `round_rect` / `stroke_circle` / `pie` / `arc`
  - Images: `draw_image` / `draw_image_at_size`
  - Text: `fill_rich_text` / `fill_text_max_width` / `with_text_align` / `with_text_baseline` /
    `with_max_width` / `with_text_color` / `measure_rich_text` / `measure_size`
  - Composition: `group(children)` / `layer(name)`

### Changed

- **Unified sizing contract across both backends.** The size passed to a renderer is the
  **output** size; `scale` is a pure *density* knob that divides it to obtain the user coordinate
  system (`viewBox = 输出尺寸 / scale` on the SVG side). Previously the SVG backend multiplied the
  size by `scale`, which silently double-scaled output for callers that had already applied the
  density themselves.
  - `SvgRenderer::new(0.0, 0.0)` still falls back to `scene 尺寸 × scale`, so size-omitting callers
    keep the old behaviour.
- **`Element::text()` / `Element::rich_text()` now typeset eagerly**, carrying a pre-computed
  `layout`. The SVG backend typesets via the browser while the raster one uses parley, so without
  a shared layout the SVG side had to *estimate* block dimensions from `font_size` — measured at
  ~20% overshoot on the text background rectangle, and inconsistent with `fit::scene_bounds`.
  - Empty spans / empty text skip typesetting and keep `layout: None`.
- **The raster backend now honours `Scene::background`** when no explicit `with_background` is set,
  matching the SVG backend. Callers that relied on a transparent canvas should pass
  `Color::TRANSPARENT` explicitly (an alpha-0 background is not painted).
- `Scene::scale` documentation now states the shared contract instead of the SVG-specific one.
- `register_font` is now visible **process-wide** (previously thread-local): each thread's context
  is a clone of a shared `fontique::Collection`, so fonts registered on one thread are seen by all
  threads, including ones started later.

### Fixed

- **PNG output was premultiplied.** vello's `Pixmap` stores premultiplied alpha, but PNG (and this
  crate's `Pixmap` RGBA8) use straight alpha — writing premultiplied values made every translucent
  pixel (anti-aliased edges, translucent fills, text edges) systematically darker.
- **Images ignored node/layer opacity.** `Element::Image` now composes its own opacity with
  `SceneNode::opacity`, matching all other primitives.
- **Aspect-ratio mismatch cropped content.** The raster backend scaled by width only; it now uses
  `min(sx, sy)` plus centering, matching SVG's default `preserveAspectRatio="xMidYMid meet"`.
- **Full-circle arcs rendered nothing on SVG.** SVG drops an `A` segment whose start and end points
  coincide, so a 100% pie slice produced no output; full turns are now split into two 180° arcs.
- **Gradient stops used 8-digit hex** (`#rrggbbaa`), which SVG 1.1 and most SVG→PDF toolchains do
  not understand; translucent stops now carry `stop-opacity`.
- **`SvgRenderer::new(0.0, 0.0)` ignored scene metadata**, emitting `width="0.00"` and dropping
  `Scene::background`; it now falls back to the scene's size, background and scale.
- **Huge image frames could exhaust memory.** A large frame combined with `ObjectFit::Cover` scaled
  the source up to gigabytes; intermediate bitmaps are now downsampled against a canvas-area budget,
  and fully offscreen images are culled before allocation.
- **Image scaling used straight-alpha interpolation**, producing dark halos around translucent
  edges; it now interpolates in premultiplied space.
- Canvas dimensions above `u16::MAX` were silently truncated by `as u16`; they are now clamped.
- Alpha conversions now round instead of truncating (50% opacity maps to 128, not 127).
- **README rewritten in English** and slimmed down (~163 → ~110 lines).

### Removed

- **`RectExt`** — its only member, `union`, is provided natively by kurbo ≥ 0.13 (as a `const fn`).
  Call `a.union(b)` directly; no trait import is needed.

### Breaking changes

| Change | Migration |
|---|---|
| `Ctx` drawing methods return `SceneNode` instead of `Element` | `scene.push(...)` / `layer.push(...)` are source-compatible (`From<Element> for SceneNode` still exists). Only code that names the type explicitly, e.g. `let e: Element = ctx.fill_rect(..)`, needs updating. |
| `Ctx::fill` is `Option<Fill>` instead of `Option<Color>` | `with_fill(Color)` is unchanged; read `.fill` as a `Fill`. |
| `Ctx::measure_text` returns `TextMeasure` instead of `Size` | Use `.size`, or call `measure_size()` to get the old `Size`. |
| SVG `width`/`height` no longer multiplied by `scale` | Callers that want a density-scaled canvas should pass the final size to the renderer (e.g. `new(w * scale, h * scale)`). Callers using `new(0.0, 0.0)` are unaffected. |
| Raster backend falls back to `Scene::background` | Pass `.with_background(Color::TRANSPARENT)` to keep a transparent canvas. |
| `RectExt` removed | Use kurbo's native `Rect::union`; drop the `RectExt` import. |

### Migration notes for downstream (liemermaid / liepress / liecharts)

- `lievisual = "0.2"`.
- **liemermaid**: `builder::fit_to_canvas` + `compute_bbox` (~200 lines) can be replaced by
  `fit::fit_scene(&mut scene, FitOptions::new().with_margin(8.0))`. The lievisual version additionally
  accounts for stroke width and node transforms, which the downstream match arms missed.
- **liecharts**: PNG output now defaults to an opaque white background (`Scene::background`).
  Pass `Color::TRANSPARENT` if a transparent canvas is required.
- **liepress**: unaffected — it already builds the scene at final pixel size and passes that size
  to the renderer; `scale` is metadata only, which is exactly the new contract. Its text path calls
  `layout_text` explicitly and builds `Element::Text` via struct literal, so eager typesetting does
  not apply.
- **liecharts**: unaffected by eager typesetting — `text_el` builds `Element::Text` via struct
  literal with `layout: None`, so the existing `compute_text_layouts` post-pass (which also swaps in
  `DEFAULT_FONT_STACK`) keeps working. The only visible change is the PNG background (above).
- **liemermaid**: three call sites use the `Element::text()` / `Element::rich_text()` constructors
  (`src/vir.rs:276`, `src/builder/paint/mod.rs:67` and `:124`) and will now typeset at construction
  time. The result should match the `measure` stage's own `layout_text`, but verify text rendering
  there. Note this also makes `compute_bbox` see a non-`None` layout for those nodes, which is
  strictly more accurate than the fallback it used before.

### Recommended adoption for downstream (not a rewrite)

Measured across the three crates: 57 `Element::*` struct literals, 30 constructor calls, and 32
`FillStrokeStyle` literals — all behind hand-rolled helper layers (`liemermaid::vir`, 386 lines;
`liecharts::pipeline::builder`) that already mirror most of `Ctx`'s surface (7–8 primitive
constructors plus 3 `FillStrokeStyle` shorthands, every one taking a `z: i32`).

Those helpers take **style as an argument**, whereas `Ctx` carries **style on itself**. For code
where every primitive has its own colour, mechanically switching to `Ctx` means a `clone()` per
primitive and reads worse — so a wholesale rewrite is *not* recommended. Instead:

1. **Adopt `fit` immediately** — replaces `liemermaid`'s `compute_bbox` + `fit_to_canvas` (~200
   lines) and fixes the stroke-width / transform bugs in the hand-written version.
2. **Adopt `Ctx` / `Path2D` for new code** — especially hand-built geometry (arrowheads, smooth
   paths), where `Path2D` reads far better than manual `BezPath` assembly.
3. **Keep the existing helper layers** — they are not wrong, and the paradigm mismatch makes
   mechanical migration a net loss.

[Unreleased]: https://github.com/zzzdong/lievisual/compare/v0.2.0...HEAD
[0.2.0-beta.2]: https://github.com/zzzdong/lievisual/compare/v0.2.0-beta.1...v0.2.0-beta.2
[0.2.0-beta.1]: https://github.com/zzzdong/lievisual/compare/v0.1.4...v0.2.0-beta.1
[0.2.0]: https://github.com/zzzdong/lievisual/compare/v0.1.4...v0.2.0
