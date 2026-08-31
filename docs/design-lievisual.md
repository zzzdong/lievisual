# lievisual 设计文档

声明式视觉场景 IR + 可插拔渲染后端。定位为 liecharts / liemermaid / liepress 共享的
"绘图层"：上层业务（图表/流程图/Markdown）只产出 `Scene`，再由所选后端输出到
SVG / vello_cpu 栅格 / GPU（规划中）/ PDF·DOCX（经 liepress）。

## 核心原则

1. **IR 与后端分离**。`Scene`/`Element` 描述"画什么"，`Renderer` trait 描述"怎么画"。
   新增后端（如 GPU）不影响 IR；新增图元变体只需在 `Renderer` 各实现里加一个 `match` 分支，
   且 `draw_*` 均有默认实现（几何转 `BezPath` 后走 `draw_path`），新后端不实现也能正确渲染。
2. **层级与节点属性提升**。`z_index` / `Transform` / `clip` / `opacity` / `name` / `visible` 是
   `SceneNode` 的字段，而非每个图元重复携带。其中 `transform` 与 `opacity` 对 `Group`
   子树递归复合（`opacity` 逐层相乘），`clip` 将整棵子树限制在形状内（SVG `<clipPath>` /
   vello `push_clip_path`），`visible = false` 整棵子树跳过。后端在默认遍历
   `render_scene_ordered` 里统一处理排序、节点属性、`Group` 递归与局部变换。
3. **图元丰富**。`Element` 覆盖：矩形、圆、椭圆（可旋转）、圆角矩形、线、折线、多边形、
   圆弧（开放描边）、扇形（闭合、适合饼图）、路径、渐变路径、文本（双形态）、组合 Group。
4. **图层（Layer）**。`Scene` 可携带一组有序 [`Layer`]（从底到顶），整层控制
   `name` / `visible` / `opacity` / `transform`；层内节点再按 `z_index` 稳定排序，
   互不影响其它层。`Scene::nodes` 作为"默认层"（最底）保留，向后兼容。
5. **填充多样**。`Fill`：纯色 / 线性渐变 / 径向渐变（坐标一律为绝对用户坐标，
   SVG 端以 `gradientUnits="userSpaceOnUse"` 保证与 IR 一致）。描边 `Stroke` 支持
   线帽 / 连接 / 虚线（`dash_array` / `dash_offset`）/ `miter_limit`。
6. **场景元数据**。`Scene` 携带 `title` / `description`（SVG 输出 `<title>`/`<desc>`）与
   `scale`（像素密度，SVG 放大 `width`/`height` 而保持 `viewBox`）。
7. **文本（Rich Span 为核心）**。`Element::Text` 以样式化片段 [`RichSpan`] 为核心
   （单文本 = 单 span），可携带预排版 `TextLayout`（可选）。`TextStyle` 对齐 Web Canvas
   与 Pango：`font-family` / `font-size` / `font-weight` / `font-style` / `font-width` /
   `line-height` / `letter-spacing` / `underline(+color)` / `strikethrough(+color)` /
   `baseline-shift`（上下标，渲染端 y 平移）/ `background-color`（行内背景） +
   `textAlign`（[`TextAlign`]：Left/Center/Right/**Justify 两端对齐**）+ `textBaseline`
   （[`TextBaseline`]）；`measure_text` 对应 `ctx.measureText()`，返回含 `TextMetrics`。
   `Justify` 经 parley `Alignment::Justify` 在排版时调整字距（vello 精确），渲染端锚点
   同 Left。SVG 后端用原生 `<text>`（含 `text-decoration` / `letter-spacing` /
   `font-stretch` / `baseline-shift`，背景为近似 `<rect>`），不依赖 parley。注：
   parley 无 rise/background 属性，`baseline_shift`/`background_color` 由 lievisual
   渲染端手工实现（vello 精确）。
8. **图片（Image）**。`Element::Image` 携带自包含 [`SceneImage`]（二进制 + 像素尺寸 +
   [`ObjectFit`]）。`ObjectFit`：Contain/Cover/Fill/None。SVG 输出 data:URI + `<image>`
   `preserveAspectRatio`；vello 解码 → 缩放 → 预乘 opacity → blit 到 frame。
9. **无 feature gate**。`svg` / `vello`（vello_cpu 栅格化）/ `text`（parley 排版）全部默认
   启用，无需开启任何 feature 即可使用全部后端与文本能力。

## 模块结构

```
src/
  geometry.rs   [Point]/[Rect]/[Vec2]/[Size] 直接 re-export kurbo 0.13（零转换互通，
                消除下游 to_lie_* 缝合）；补充 PointExt（midpoint/is_finite/min/max）
                与 PointExt（midpoint/is_finite/min/max）便捷扩展；布局方法（union/inflate/
                from_points）直接用 kurbo 原生；Color/Transform 保留自定义（仿射桥接 kurbo）
  scene.rs      Scene（元数据 + 图层 layers）/ Layer / SceneNode（层级+节点属性）/
                Element（图元枚举）/ 样式（FillStrokeStyle/Fill:Solid·Linear·Radial/Stroke 含虚线）
  text.rs       Canvas 风格文本 API：TextStyle/TextAlign/TextBaseline/TextMetrics/
                measure_text（thread-local 排版上下文缓存）
  render/
    mod.rs      Renderer trait + 默认 render_scene_ordered（排序 + 节点属性 + 图层 + 递归 Group/变换）
    svg.rs      SvgRenderer（原生标签 + 用户坐标渐变 + title/desc/scale + 图层 <g>）
    vello_pixmap.rs  VelloPixmapRenderer（vello_cpu 0.2 → PNG；
                统一渐变填充、虚线描边、变换栈与透明度栈）
```

## 后端清单

| 后端 | 输出 | 文本处理 |
|------|------|----------|
| `SvgRenderer` | 矢量 SVG 字符串 | 原生 `<text>`（`text-anchor`/`dominant-baseline`/`font-weight`/`font-style`），不依赖 parley |
| `VelloPixmapRenderer` | `Pixmap` → PNG 字节 | parley 排版（统一 TLS 上下文），优先用调用者提供的 `TextLayout` |
| GPU（`vello_hybrid`） | GPU 加速（规划） | 同 vello，仅 Scene 提交方式不同 |

所有后端默认启用，无需 feature 开关（vello 栅格化依赖 parley 排版，作为常驻依赖）。
图层在 SVG 端输出为 `<g id="layer-name">`（含整层 `opacity` / `transform`），
栅格端则并入透明度 / 变换栈。

## 文本排版（Canvas API）

文本 API 对齐 Web Canvas `CanvasRenderingContext2D`，方便上层（图表/流程图）从浏览器
习惯迁移：

| Canvas | lievisual |
|--------|-----------|
| `ctx.font` | `TextStyle`（`font_family`/`font_size`/`font_weight`/`font_style`/`font_width`/`line_height`） |
| `ctx.textAlign` | [`TextAlign`]（Left/Center/Right） |
| `ctx.textBaseline` | [`TextBaseline`]（Top/Hanging/Middle/Alphabetic/Ideographic/Bottom） |
| `ctx.measureText()` | [`measure_text`] → [`TextMeasure`]（含 [`TextMetrics`]） |
| `ctx.fillText(x, y)` | `Element::Text { position, .. }`：`x` 由 `align` 决定，`y` 由 `baseline` 决定 |

**富文本（Rich Text，核心表达）**：文本内容统一以样式化片段 [`RichSpan`]（文本 + 样式）为
核心，单文本是单个片段的特例，富文本是多个片段按序拼接。排版 / 测量统一走 span 列表入口：
- [`measure_text`] / [`layout_text`]：接受 `&[RichSpan]`，经统一 TLS 上下文将多段不同样式的
  文本合并为一个 `TextLayout`；`TextStyle` 提供**对齐 Pango 的能力**，每段可独立设置：
  - 字体族 / 字号 / 字重 / 风格 / **字宽（font-stretch）**
  - 前景色（`color`）
  - **下划线（`underline` + `underline_color`）** / **删除线（`strikethrough` + `strikethrough_color`）**
  - **字间距（`letter_spacing`）** / 行高（`line_height`）
- [`crate::scene::Element::Text`] 携带 `spans`（样式化片段）与块级 `style`（定位/默认字体），
  纯文本由 `spans` 拼接推导（SVG 后端），保证与 vello 后端字形一致；
- 富文本布局可喂给渲染端精确绘字形，也可与 [`compute_text_offset`] 配合实现任意对齐；
- 依赖 parley（同为 Pango 的 Rust 移植）→ 上述能力经 parley `StyleProperty` 映射到同一排版引擎。

**与 Pango 的能力边界**：lievisual 的 span 数组已覆盖 Pango markup 的绝大多数字形级属性
（family/size/style/weight/stretch/foreground/underline/strikethrough/letter_spacing）。
Pango 独有而 parley 0.11 尚未提供的：`rise`（上标/下标）与文本背景色 `background` —— 这两项
为硬限制，需在 lievisual 侧手工模拟（如装饰线条 / 上升偏移），不在本模块直接支持。

**锚点语义**：水平锚点由 `align` 决定（左/中/右），垂直锚点由 `baseline` 决定
（默认 `Alphabetic`，即 `position.y` 是基线——与 canvas 默认一致）。两端后端同步实现：
- SVG：`text-anchor` + `dominant-baseline`；
- vello：`translate(position) * rotate * translate(dx, dy)`，其中 `dx`/`dy` 由度量推导，
  旋转绕锚点。

**排版上下文缓存（thread-local）**：parley 的 `FontContext`（字体库 + LRU 源缓存）与
`LayoutContext`（scratch + 字形缓存）被官方设计为"每应用/每线程一个"。lievisual 通过
`thread_local` 持有这两个上下文，**所有 parley 相关操作**（[`measure_text`] 测量、
渲染端现场排版 `layout_text`）统一经 `with_cached_context` 复用同一对实例：首次调用按
线程懒初始化，之后线程内复用，避免反复的系统字体扫描与分配；多线程各持独立实例，
无需同步。

## 颜色约定

- `geometry::Color`：RGBA，分量 0.0–1.0，贯穿整个 IR。
- 作为 parley 的 brush 类型：`Color` 自动满足 `parley::Brush` marker trait，无需转换。
- 栅格化时由后端 `to_vello` 转为 `vello_cpu::peniko::color::AlphaColor<Srgb>`，并乘入当前
  节点透明度。

## 依赖版本对齐

- `kurbo = "0.13"`：与 `vello_cpu 0.2` 内部 kurbo 0.13 对齐，避免双版本类型不兼容。
- `parley = "0.11"`、`vello_cpu = "0.2"`：参考 liemermaid 已验证可用的组合。
- `peniko 0.6`（随 vello_cpu）：支持 Linear / Radial / Sweep 三种渐变。

## 后续规划

1. **GPU 后端**：新增 `VelloGpuRenderer`（基于 linebender/vello 的 `vello_hybrid` sparse_strips）。
   因 `Scene` 已是纯几何 + 样式，直接喂给 GPU 渲染上下文即可，上层业务零改动。
2. **`Scene::bounds()` 几何求交**：为布局/命中测试提供图元包围盒计算（依赖 `Shape::bounding_box`）。
3. **SVG 文本矢量保真**（可选）：将 `TextLayout` 的字形转为 `<path>`，替代 `<text>`，
   以获得跨渲染器完全一致的字形。
4. **接入 liecharts / liemermaid**：二者现有 `VisualElement` + `Renderer` 模式与 lievisual
   同构，可将各自的 IR 收敛到 lievisual 的 `Scene`，删除重复定义。
5. **接入 liepress**：liepress 四端（PDF/PNG/SVG/DOCX）分别选用 `VelloPixmapRenderer`、
   `SvgRenderer` 或内嵌 SVG，统一走 `Scene` 中间表示。
