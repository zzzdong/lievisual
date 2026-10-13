# lievisual API 设计概览

> 本文档归纳 lievisual 当前的 API 设计（截至 2026-08-18，含 `TextRun` 收口、
> Builder 层的重构）。定位：**渲染库**（类似 Canvas + Pango 的合体劣化版），不做布局。

---

## 1. 总体架构：三层 API

```
用户代码
   │
   ▼
① Builder 层（builder::Ctx）—— Canvas 风格糖衣，可选
   │  产出 Element
   ▼
② 声明式 IR 层（scene::Scene / Element / SceneNode）
   │  文本经 text:: 排版/测量
   ▼
③ 渲染后端（render::Renderer: SvgRenderer / VelloPixmapRenderer）
```

### 模块划分（`lib.rs`）

| 模块 | 职责 |
|------|------|
| `scene` | 声明式场景图 IR：`Scene` / `SceneNode` / `Element` / 样式 / `Layer` |
| `text` | 文本排版与测量：`RichSpan` / `TextStyle` / `TextLayout` / 字体注册 |
| `geometry` | 几何与颜色：re-export kurbo + 自研 `Color` / `Transform` |
| `path` | 路径运算：布尔运算 / stroke-to-path / 弧长测量 / 膨胀收缩 |
| `pixmap` | 自包含 RGBA8 位图 `Pixmap`（PNG 编解码；lievisual 不做图片解码） |
| `builder` | Canvas 风格便捷构造层（可选糖衣） |
| `render` | 渲染后端 trait + SVG / vello_cpu 实现 |

顶层 re-export：核心类型 + `pub use parley`（细粒度数据读取出入口）。

---

## 2. ① Builder 层（`builder::Ctx`）

Canvas 风格**便捷构造**层：纯函数返回 `Element`，不引入可变状态、不新增 IR 类型。

### 状态（可链式派生）

- `fill: Option<Color>`（`fillStyle`，仅作用于形状填充）
- `stroke: Option<Stroke>`（`strokeStyle` + `lineWidth`）
- `font: TextStyle`（`font` + `textAlign` + `textBaseline`）

链式方法：`with_fill` / `with_stroke` / `with_font` / `with_text_color`。

### 方法

| 方法 | Canvas 对应 | 语义 |
|------|------------|------|
| `fill_text(x, y, text)` | `fillText` | `(x,y)`=锚点；对齐/基线由 `font.align`/`font.baseline` 决定 |
| `measure_text(text)` | `measureText` | 返回 `Size` |
| `fill_rect` / `stroke_rect` | `fillRect` / `strokeRect` | 左上角 + 宽高 |
| `fill_circle` / `fill_ellipse` | `arc`+fill / `ellipse`+fill | 圆心/半径/半轴 |
| `line` / `polyline` / `polygon` | `moveTo` / `lineTo` | 线/折线/多边形 |
| `path(path, closed)` | `beginPath`+fill | 贝塞尔路径 |
| `boolean_path(a, b, op)` | — | 两路径的布尔运算结果（lievisual 扩展） |
| `outline_path(path, stroke)` | — | stroke-to-path：描边轮廓当填充画（lievisual 扩展） |

### 关键语义

- **文本色 = `font.color`，形状填充 = `fill`**（与 Canvas 解耦：Canvas 的 `fillStyle` 同时管两者）。
- **文本锚点**：`(x, y)` 直接作为锚点，渲染端内部按 `align`/`baseline` 统一算偏移，无需手动定位。

---

## 3. ② 声明式 IR 层（`scene`）

### 3.1 `Scene` / `SceneNode` / `Layer`

- `Scene::new(w, h)`：可携带有序 `Layer`（从底到顶）；层内节点按 `z_index` 稳定排序；`nodes` 为默认层（最底）。
- `SceneNode`：内容为 `kind: NodeKind`（`Draw(Element)` 叶子 / `Group(Vec<SceneNode>)` 容器），加节点属性 `z_index` / `transform` / `opacity` / `name` / `visible` / `clip` / `blend_mode` / `mask` / `filter` / `color_matrix` / `antialias`（子树递归生效）；`filter` / `opacity` / `blend_mode` / `mask` 为隔离合成（离屏层），跨后端一致。叶子用 `element()` / `element_mut()` / `into_element()` 读取，容器用 `is_group()` / `children()` / `children_mut()` / `visit` / `visit_mut`。
- `ColorMatrix`：4×5 色彩矩阵（`grayscale` / `saturate` / `hue_rotate` / `brightness` / `contrast` / `tint` 预设 + `then` 组合），经 `with_color_matrix` 挂在节点上。**烘进 paint 颜色**（两个后端同法，逐像素一致）；图片与作用域机械不受影响。
- `antialias: Option<bool>`（`with_antialias`）：`None` 沿子树继承（默认开启），`false` 给硬边（SVG `shape-rendering="crispEdges"` / vello `aliasing_threshold = 0`）。
- `CornerRadii`：圆角矩形的统一半径或四角半径；`Element::rounded_rect(rect, 3.0, style)` 仍接受标量。
- `NodeScope`：节点与 `Layer` 共用的作用域描述；`Renderer::push_scope` / `pop_scope` 为其唯一实现（`render_layer` 与 `render_node` 共用）。
- `Scene::push(node)`：`Element: From<Element> → SceneNode` 自动转换。

### 3.2 `Element` 图元枚举

矩形、圆、椭圆、圆角矩形、线、折线、多边形、圆弧、扇形、路径、渐变路径、文本、`SceneImage`（图片）。
**`Element` 只描述几何 + 样式** —— 容器不是图元，而是 `NodeKind::Group`（`Element` 不含 `Group`，也不引用 `SceneNode`）。

### 3.3 填充 / 描边

- `Fill`：纯色 / 线性渐变 / 径向渐变 / **扫掠（conic）渐变** / **图片填充**（`ImageFill`）/ 手绘（sketch）。
- 图片：`SceneImage`（`pixmap` + `object_fit` + `quality`）；`ObjectFit`：`Contain` / `Cover` / `Fill` / `None` / **`Tile`**（按原始像素尺寸平铺）；`ImageQuality`：`Nearest` / `Bilinear`（默认）/ `Bicubic`。`ImageFill { image, tile }` 把图片当画刷按网格重复（`ImageFill::natural(pixmap, at)`）。
- 渐变：`GradientStop`（offset + color）+ `GradientSpread`（`Pad`（默认）/ `Repeat` / `Reflect`）。
  构造用 `LinearGradient::new` / `RadialGradient::new` / `SweepGradient::new`（或 `SweepGradient::full`），
  再用 `with_spread` 改扩展模式。`SweepGradient::sample(angle)` 可直接取某角度的颜色（SVG 端用它生成扇区）。
- `Stroke`：颜色 + 宽度 + `LineCap` / `LineJoin` + 虚线（`dash_array` / `dash_offset`）+ `miter_limit`。
- `FillStrokeStyle`：组合填充 + 描边。

---

## 4. ② 文本子系统（`text`）

### 核心模型

- `RichSpan { text, style }`：样式化片段，排版/测量统一入口。
- `TextStyle`：`color` / `font_size` / 字体族 / `align` / `baseline` / 装饰等。
- `TextLayout → TextLine[] → TextRun[] → Glyph[]`；`TextRun` 含 `font_data: Arc<Vec<u8>>` + **`font_index: u32`**（PDF 嵌入字体用）。

### 入口函数

- `layout_text(&[RichSpan], max_width) -> TextLayout`
- `measure_text(...) -> TextMeasure`
- `layout_metrics` / `compute_text_offset`（锚点偏移）
- `parse_generic_family` / `register_font` / `register_font_generic`（字体注册）

### 顶层 `pub use parley`

暴露底层排版引擎，供下游（如 liepress）读逐字形 / run / 字体字节细粒度数据；lievisual 自身封装为自闭环 `TextLayout` 合同。

---

## 5. ② 图片 / 位图（`pixmap`）

### `Pixmap`（自包含 RGBA8 位图）

- `from_rgba8(width, height, pixels)` / `decode_png(&[u8])` / `to_png() -> Vec<u8>`。
- 内存：RGBA8 紧排（`stride = 4*width`），与后端解耦。
- **lievisual 不做图片解码**：图片以已解码位图进入 IR；PNG 解码走 `png` 库，其余格式经 `image`（feature）。

### `SceneImage`（IR 中的图片资源）

- 持有 `pixmap: Pixmap` + `object_fit`；构造 `from_pixmap` / `from_rgba8` / `from_png`。
- `Element::image(image, frame)`：`frame` 为显示区域，`object_fit` 决定映射。
- feature `extra-image-codecs` 启用后额外提供 `from_encoded(&[u8])`（jpeg/webp/gif 解码）。

---

## 6. ③ 渲染后端（`render`）

`Renderer` trait + 两个后端（默认全开，无 feature gate）：

- `SvgRenderer`：SVG 字符串（矢量），文本回退 `<text>`，支持 `title` / `description` / `scale`；图片由位图 → PNG → data-URI 内嵌。
- `VelloPixmapRenderer`：vello_cpu 栅格化 → lievisual 自有 `Pixmap`（非预乘 RGBA8）/ PNG 字节；图片直接消费 `pixmap` 像素（双线性缩放 + blit）。下游无需直接依赖 vello_cpu。

---

## 7. 几何（`geometry`）与路径运算（`path`）

re-export kurbo：`Point` / `Rect` / `Size` / `Vec2`；自研 `Color` / `Transform` / `LineCap` / `LineJoin` 等。

### 路径运算（`path`）

`use lievisual::PathOps;` 之后可在任意 `BezPath` 上：

- **布尔运算**：`boolean(other, op, tolerance)` + 便捷的 `union` / `intersect` / `difference` / `xor`（`BoolOp`）。
- **规范化**：`normalize(tolerance)` —— 解自交、合并重叠 contour（"自并"）。
- **stroke-to-path**：`outline(stroke, tolerance)` —— 描边转可填充轮廓，**保留曲线**（不展平）。
- **弧长测量**：`path_length()` / `point_at_length(d)` → `PathSample`（点 + 单位切向 + 实际弧长）/ `trim(from, to)`。
- **膨胀 / 收缩**：`offset(d, tolerance)` / `inset(d, tolerance)` —— 圆盘 Minkowski（圆角化），把路径当**填充区域**处理。

要点：

- **构建期、IR 中立**：`BezPath` 进、`BezPath` 出，不进 `scene`、不碰后端 → 结果交给 `Ctx::path` 后，SVG 与栅格**天然一致**。
- 布尔运算由 **`i_overlay`**（纯 Rust）完成（kurbo 0.13 已移除 `BooleanOps`）：曲线先按 `tolerance`（默认 `DEFAULT_TOLERANCE = 0.1`）展平成多边形，**结果也是多边形**；填充按**非零**规则（两个后端的默认），洞以反向绕的 contour 表达。
- 便捷入口：`Ctx::boolean_path(a, b, op)`、`Ctx::outline_path(path, stroke)`；`Stroke::to_kurbo()` 是描边样式的唯一定义（后端与 `outline` 共用）。

---

## 8. 设计主线

1. **IR 与后端分离**：`scene` 描述"画什么"，`render` 描述"怎么画"。
2. **文本合同自闭环**：`TextLayout` 脱离 parley 可消费，`font_index` / `font_data` 支撑 PDF 嵌入。
3. **图片位图自包含**：`Pixmap` 已解码 RGBA8 进入 IR，**lievisual 不做图片解码**（与文本"调用方排版"原则一致）。
4. **分层糖衣**：`builder`（Canvas 风格）→ `scene`（声明式）→ `render`（可插拔）。
5. **parley/kurbo 策略**：kurbo 几何 re-export；parley 作为引擎 re-export 供下游细粒度读取，但 lievisual 自身封装为 `TextLayout` 合同。
