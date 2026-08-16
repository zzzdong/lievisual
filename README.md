# lievisual

[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

**声明式视觉场景 IR + 可插拔渲染后端。**

`lievisual` 是一个 Rust 图形库，定位为 liecharts / liemermaid / liepress 共享的"绘图层"：
上层业务（图表 / 流程图 / Markdown）只产出声明式的 `Scene`（描述"画什么"），再由所选
后端输出 SVG 矢量、vello_cpu 栅格 PNG 或（规划中的）GPU 加速画面（描述"怎么画"）。

- 纯 Rust，`edition 2024`，无 unsafe 业务代码
- 无 feature gate：SVG / vello / 文本排版默认全部启用
- 文本 API 对齐 Web Canvas `CanvasRenderingContext2D`，方便从浏览器习惯迁移

## 特性

- **IR 与后端分离**：`Scene` / `Element` 描述画什么，`Renderer` trait 描述怎么画。
  新增后端（如 GPU）不影响 IR；新增图元变体只需在 `Renderer` 各实现里加一个
  `match` 分支，且 `draw_*` 均有默认实现（几何转 `BezPath` 后走 `draw_path`），
  新后端不实现也能正确渲染。
- **统一层级与节点属性**：`z_index` / `Transform` / `clip` / `opacity` / `name` /
  `visible` 提升为 `SceneNode` 的字段。`transform` 与 `opacity` 对 `Group` 子树递归
  复合，`clip` 限制整棵子树，`visible = false` 整棵子树跳过。
- **图层（Layer）**：`Scene` 可携带一组有序图层（从底到顶），整层控制 `name` /
  `visible` / `opacity` / `transform`；层内节点再按 `z_index` 稳定排序。
- **图元丰富**：矩形、圆、椭圆（可旋转）、圆角矩形、线、折线、多边形、圆弧（开放描边）、
  扇形（闭合，适合饼图）、路径、渐变路径、文本（双形态）、组合 Group。
- **填充多样**：纯色 / 线性渐变 / 径向渐变（坐标一律为绝对用户坐标）；描边支持
  线帽 / 连接 / 虚线（`dash_array` / `dash_offset`）/ `miter_limit`。
- **裁剪**：矩形 / 圆角矩形 / 圆 / 椭圆 / 任意贝塞尔路径，跨后端一致
  （SVG `<clipPath>` / vello `push_clip_path`）。
- **场景元数据**：`title` / `description`（SVG 输出 `<title>` / `<desc>`）与 `scale`
  （像素密度，SVG 放大输出尺寸而保持 `viewBox`）。
- **文本（Canvas 风格）**：`font-family` / `font-size` / `font-weight` / `font-style` /
  `line-height` + `textAlign` + `textBaseline`；`measure_text` 对应 `ctx.measureText()`，
  返回 `TextMetrics`。排版上下文按线程缓存（thread-local），避免反复系统字体扫描。

## 快速开始

`Cargo.toml`：

```toml
[dependencies]
lievisual = "0.1"
```

构建一个场景并输出 SVG：

```rust
use lievisual::geometry::{Color, Point, Rect};
use lievisual::render::SvgRenderer;
use lievisual::scene::{Element, FillStrokeStyle, Scene};

let mut scene = Scene::new(200.0, 100.0).with_background(Color::WHITE);

// 矩形 + 文本
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

// 输出 SVG
let mut svg = SvgRenderer::new(scene.width, scene.height);
scene.render(&mut svg);
let svg_str = svg.into_string();
```

输出 PNG（`VelloPixmapRenderer`）：

```rust
use lievisual::render::VelloPixmapRenderer;

let mut renderer = VelloPixmapRenderer::new(200, 100).with_background(Color::WHITE);
let png: Vec<u8> = renderer.render_png(&scene);
```

运行仓库自带的丰富演示（同时输出 `demo.svg` 与 `demo.png`）：

```sh
cargo run --example demo
```

## 后端

| 后端 | 输出 | 文本处理 |
|------|------|----------|
| `SvgRenderer` | 矢量 SVG 字符串 | 原生 `<text>`（`text-anchor` / `dominant-baseline` / `font-weight` / `font-style`），不依赖 parley |
| `VelloPixmapRenderer` | `Pixmap` → PNG 字节 | parley 排版（统一 TLS 上下文），优先用调用者提供的 `TextLayout` |
| GPU（`vello_hybrid`） | GPU 加速（规划中） | 同 vello，仅 Scene 提交方式不同 |

所有后端默认启用，无需 feature 开关。图层在 SVG 端输出为 `<g id="layer-name">`
（含整层 `opacity` / `transform`），栅格端并入透明度 / 变换栈。

## 模块结构

```
src/
  geometry.rs   点 / 向量 / 尺寸 / 矩形 / 颜色 / 仿射变换（f64 精度，与 kurbo / parley / vello 对齐）
  scene.rs      Scene（元数据 + 图层 layers）/ Layer / SceneNode（层级 + 节点属性）/
                Element（图元枚举）/ 样式（FillStrokeStyle / Fill / Stroke 含虚线）/ Clip
  text.rs       Canvas 风格文本 API：TextStyle / TextAlign / TextBaseline / TextMetrics /
                measure_text（thread-local 排版上下文缓存）
  render/
    mod.rs      Renderer trait + 默认 render_scene_ordered（排序 + 节点属性 + 图层 + 递归 Group / 变换）
    svg.rs      SvgRenderer（原生标签 + 用户坐标渐变 + title/desc/scale + 图层 <g>）
    vello_pixmap.rs  VelloPixmapRenderer（vello_cpu 0.2 → PNG；统一渐变填充、虚线描边、变换栈与透明度栈）
```

## 设计原则

1. **IR 与后端分离**。`Scene` / `Element` 描述"画什么"，`Renderer` trait 描述"怎么画"。
   新增后端不影响 IR；新增图元只需在 `Renderer` 各实现里加 `match` 分支。
2. **层级与节点属性提升**。`z_index` / `Transform` / `clip` / `opacity` / `name` /
   `visible` 是 `SceneNode` 的字段，而非每个图元重复携带。
3. **图层（Layer）**。`Scene` 可携带一组有序 `Layer`，整层控制 `name` / `visible` /
   `opacity` / `transform`；`Scene::nodes` 作为默认层（最底）保留，向后兼容。
4. **填充多样**。纯色 / 线性渐变 / 径向渐变，坐标一律为绝对用户坐标（SVG 端以
   `gradientUnits="userSpaceOnUse"` 保证与 IR 一致）。
5. **文本（Canvas 风格）**。对齐 Web Canvas，方便上层从浏览器习惯迁移；排版上下文
   按线程缓存（thread-local）。
6. **无 feature gate**。SVG / vello / text 默认全部启用。

## 颜色约定

- `geometry::Color`：RGBA，分量 0.0–1.0，贯穿整个 IR。
- 作为 parley 的 brush 类型：`Color` 自动满足 `parley::Brush` marker trait，无需转换。
- 栅格化时由后端 `to_vello` 转为 `AlphaColor<Srgb>`，并乘入当前节点透明度。

## 依赖版本对齐

- `kurbo = "0.13"`：与 `vello_cpu 0.2` 内部 kurbo 0.13 对齐，避免双版本类型不兼容。
- `parley = "0.11"`、`vello_cpu = "0.2"`：参考 liemermaid 已验证可用的组合。
- `peniko 0.6`（随 vello_cpu）：支持 Linear / Radial 两种渐变。
- `image = "0.25"`（仅 PNG 特性）：PNG 编码。

## 测试

```sh
cargo test
```

50 个单元测试覆盖：场景构建、图层排序 / 迭代、元素构造器、渐变、裁剪、变换、SVG 输出
（标签 / 路径 / 转义 / 锚点 / 元数据）、vello 像素渲染（层序 / 透明度 / 裁剪 / 虚线 /
字形 / 渲染器复用）、文本测量与 TLS 上下文复用，以及 Bug 回归用例。

## 后续规划

1. **GPU 后端**：新增 `VelloGpuRenderer`（基于 linebender/vello 的 `vello_hybrid` sparse_strips）。
2. **`Scene::bounds()` 几何求交**：为布局 / 命中测试提供图元包围盒计算。
3. **SVG 文本矢量保真**（可选）：将 `TextLayout` 的字形转为 `<path>`，替代 `<text>`。
4. **接入 liecharts / liemermaid**：收敛各自的 IR 到 lievisual 的 `Scene`，删除重复定义。
5. **接入 liepress**：四端（PDF/PNG/SVG/DOCX）统一走 `Scene` 中间表示。

## License

Apache-2.0
