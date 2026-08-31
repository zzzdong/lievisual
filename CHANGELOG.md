# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0] — 2026-08-31

A release focused on **cross-backend consistency** (SVG and raster must agree), **text
correctness**, and a much larger **Canvas-like construction API**.

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
[0.2.0]: https://github.com/zzzdong/lievisual/compare/v0.1.4...v0.2.0
