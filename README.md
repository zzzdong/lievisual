<div align="center">

# lievisual

**Declarative visual scene IR with pluggable rendering backends.**

[![Crates.io](https://img.shields.io/crates/v/lievisual.svg)](https://crates.io/crates/lievisual)
[![Docs.rs](https://img.shields.io/docsrs/lievisual)](https://docs.rs/lievisual)
[![License](https://img.shields.io/badge/license-MIT_OR_Apache--2.0-blue.svg)](#license)

Upper layers (charts / diagrams / documents) declare *what* to draw as a `Scene`;
backends decide *how* — SVG, CPU-rasterized PNG, or (planned) GPU.

</div>

- Pure Rust, edition 2024, no unsafe; every backend enabled by default, no feature gates
- One IR, two backends with a single sizing contract: **the size given to a renderer is the
  output size** — `scale` is a pure density knob, never a silent enlargement
- Text API mirrors Web Canvas (`textAlign` / `textBaseline` / `measureText`), so browser
  experience ports over

## Usage

```toml
[dependencies]
lievisual = "0.2"
```

```rust
use lievisual::geometry::{Color, Point, Rect};
use lievisual::render::SvgRenderer;
use lievisual::scene::{Element, FillStrokeStyle, Scene};

let mut scene = Scene::new(200.0, 100.0).with_background(Color::WHITE);
scene.push(Element::rect(
    Rect::new(20.0, 20.0, 160.0, 60.0),
    FillStrokeStyle::fill(Color::rgb(0x33, 0x99, 0xcc)),
));
scene.push(Element::text(
    "Hello, lievisual",
    Point::new(100.0, 55.0),
    lievisual::TextStyle::new(Color::WHITE, 16.0, "sans-serif")
        .with_align(lievisual::TextAlign::Center),
));

// SVG
let mut svg = SvgRenderer::new(scene.width, scene.height);
scene.render(&mut svg);
let svg_str = svg.into_string();

// PNG
let mut raster = lievisual::render::VelloPixmapRenderer::new(200, 100)
    .with_background(Color::WHITE);
let png: Vec<u8> = raster.render_png(&scene);
```

A richer tour (layers, gradients, pie, canvas-style builder, both outputs):

```sh
cargo run --example demo   # writes demo.svg + demo.png
```

## Highlights

- **Canvas-like builder** — `Ctx` + `Path2D` cover the Canvas 2D vocabulary
  (`fillRect`, `beginPath…fill()`, transform/alpha/clip) plus what Canvas lacks:
  layers, named nodes, rich-text spans, gradients as fills, dash/cap/join.
  Immutable (`with_*` chains), and every method returns inspectable IR.
- **Layers & node attributes** — `z_index` / transform / clip / opacity / `name` / `visible`
  live once on `SceneNode`; layers stack bottom-to-top with whole-layer attributes.
- **Content bounds & auto-fit** — `fit::scene_bounds` + `fit_scene(&mut scene, FitOptions)`
  hug the canvas to the content (stroke widths, node transforms and text rotation included);
  built for "content-driven canvas" exporters.
- **Rich primitives** — rects, circles, ellipses (rotatable), rounded rects, lines, polylines,
  polygons, arcs, pies, paths, gradient paths, images, groups; solid/linear/radial fills,
  dashes, caps, joins, miter limit; clips consistent across backends.
- **Cross-backend text** — glyphs typeset once (parley) into a shared `TextLayout`; the SVG
  backend emits native `<text>`, the raster backend renders the layout. `register_font` is
  process-wide, visible to all threads.

## Backends

| Backend | Output | Text |
|---|---|---|
| `SvgRenderer` | SVG string | Native `<text>` (browser typesets) |
| `VelloPixmapRenderer` | `Pixmap` → PNG | parley layout |
| GPU (`vello_hybrid`) | planned | — |

## Layout

```
src/
  geometry.rs   Point / Rect / Color / Transform (f64, kurbo-aligned)
  scene.rs      Scene / Layer / SceneNode / Element / styles / Clip
  text/         Canvas-style text API + parley-backed layout engine
  builder.rs    Ctx + Path2D (Canvas 2D superset)
  fit.rs        content bounds + fit_scene
  render/       Renderer trait, SvgRenderer, VelloPixmapRenderer
  pixmap.rs     self-contained RGBA8 bitmap + PNG encode/decode
```

## Testing

```sh
cargo test   # 153 unit tests + 4 doc tests, incl. bug regressions & backend-contract locks
```

## License

Licensed under either of

- [MIT license](LICENSE-MIT)
- [Apache License, Version 2.0](LICENSE-APACHE)

at your option.
