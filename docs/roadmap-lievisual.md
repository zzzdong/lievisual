# lievisual 能力缺口与演进路线（对标"简易 Skia"）

> 定位：lievisual 是 liecharts / liemermaid / liepress 共享的**辅助渲染库** —— 做"基础
> 渲染 + 文本排版"，等价于一个**简化版 Skia**（Canvas 语义的绘制 + Pango 语义的排版）。
>
> 本文档记录：① 相对 Skia 的能力缺口与优先级；② Scene 是否需要树形层次的设计讨论。
> 与 `design-lievisual.md`（设计原则）、`api-overview.md`（API 清单）互补。

---

## 0. 范围与边界（先校准）

**这是辅助库，不是 Skia 的替代品。** 明确"不做"以保持体量：

| 属于 lievisual | 明确不做（保持轻量） |
|---|---|
| 图元绘制 / 填充描边 / 渐变 / 图片 / 文本排版渲染 | SVG 滤镜图（filter graph）、复杂 path effect |
| `Scene` IR 与多后端（SVG / vello_cpu 栅格） | 动画时间轴 / 场景图动画 |
| 文本测量与字形级布局（parley） | 布局引擎（flex/box）—— 上层负责 |
| 基础合成（opacity / **blend / mask / blur**，见 P0/P1） | 完整 shader 体系 / ICC 色彩管理 |
| 内容包围盒 / 自适应画布（`fit`） | 分页 / 分列（liepress 负责） |

一句话：**"给定坐标，画出来"** + **"单段文本在给定宽度下长什么样、怎么画"**。

---

## 1. 现状能力盘点

- **IR**：`Scene { nodes, layers, ... }` → `SceneNode { element, z_index, transform, opacity, name, visible, clip }`；
  `Element::Group { children }` 可递归嵌套；`Layer` 是带 `visible/opacity/transform/name` 的节点集合。
- **图元**：Rect / Circle / Ellipse（可旋转）/ RoundedRect / Line / Polyline / Polygon / Arc / Pie / Path / GradientPath / Image / Text / Group。
- **填充**：Solid / LinearGradient / RadialGradient / Sketch（手绘排线）。
- **描边**：Cap / Join / Dash（`dash_array` / `dash_offset`）/ `miter_limit`。
- **文本**：`RichSpan` + `TextStyle` → `layout_text` / `measure_text`（parley），字体注册（进程级），
  自闭环 `TextLayout`（`TextLine` / `TextRun` / `Glyph`）。
- **后端**：`SvgRenderer`（矢量）、`VelloPixmapRenderer`（栅格 → 自有 `Pixmap`）。
- **其它**：`fit`（内容包围盒 / 自适应）、`sketch`（手绘 pass）、`Pixmap`（PNG 编解码）、clip、opacity、transform。

---

## 2. 能力缺口与优先级

### P0 — 正确性 / 跨后端一致性（优先修）

**P0-1｜Group/Layer 的 opacity 不是隔离合成**

- SVG 端 `Renderer::push_opacity` 输出 `<g opacity="...">`：**先合成子节点、再统一压 alpha**（正确，见 `src/render/svg.rs`）。
- vello 端 `to_vello` 把 opacity **逐图元乘进颜色 alpha**（见 `src/render/vello_pixmap.rs`）。
- 单个图元时二者等价；但 **Group/Layer 内多个图元重叠**时结果不一致 —— 这是"跨后端一致"契约的破口。
- 修复：改用 `vello_cpu` 0.3 的 `RenderContext::push_layer(clip_path, blend_mode, opacity, mask, filter)`
  做真正的隐式离屏层；SVG 端保持 `<g opacity>`（已是隔离语义）。

  > **状态：已修复（2026-10-13）** —— vello 端 `push_opacity` 改为 `push_layer(opacity)`，不再逐图元乘
  > alpha；新增回归测试 `group_opacity_is_isolated`。跨后端一致。

**P0-2｜混合模式（BlendMode）缺失**

- 无法表达 multiply / screen / overlay / darken / lighten / Porter-Duff 等。
- 后端已具备：vello_cpu `push_layer(..., blend_mode, ...)`；SVG 可输出 `mix-blend-mode` + `isolation: isolate`。

  > **状态：已实现（2026-10-13）** —— 新增 `BlendMode`：16 种 mix 模式（CSS `mix-blend-mode`）+ 13 种
  > Porter-Duff 合成算子（`peniko::Compose`）。挂在 `SceneNode` / `Layer` / `builder::Ctx`，经
  > `Renderer::push_blend` 由 vello / SVG 落地。**局限**：Porter-Duff 为 **raster-only**（SVG 无逐元素
  > 等价物，退化为 `Normal`），可用 `BlendMode::is_porter_duff()` 判断。

> P0-1 与 P0-2 共用"隔离层（layer/group compositing）"这一抽象，建议一并设计：
> 给节点引入 `compositing` 语义（见 §3 Step 2）。

### P1 — Skia 有、这里没有（高频）

| 能力 | 说明 | 落地难度 |
|---|---|---|
| 图像滤镜：**drop-shadow / blur / inner-shadow** | vello `push_layer(filter)`；SVG `<filter>`。注意 vello_cpu 多线程模式下 filter 会 panic | 中 |
| **Mask**（alpha / luminance 遮罩） | vello_cpu `push_layer(mask)` 已支持，IR 未暴露；SVG `<mask>` | 低 |
| **Sweep / Conic 渐变** | peniko `GradientKind::Sweep`；仪表盘/进度环刚需 | 低 |
| 渐变 **spread**（Pad/Reflect/Repeat） | 目前 `paint_gradient` 硬编码 `Extend::Pad`、`ColorSpaceTag::Srgb` | 低 |
| **ColorFilter**（灰度 / 色调 / 色彩矩阵） | 图表主题化常用 | 中 |

> **状态（2026-10-13）**：**Mask 已实现** —— `Mask { shape: Clip, fill: Fill }`（`SceneNode.mask` /
> `builder::Ctx::with_mask` / `Renderer::push_mask`），支持实心（含部分 alpha）与**渐变遮罩**；
> vello 光栅化为画布尺寸 `vello_cpu::Mask` 后 `push_layer(mask)`，SVG 输出 `<mask>`。
> **图像滤镜（blur / drop-shadow）已实现** —— `Filter::{blur, drop_shadow}`（`SceneNode.filter` /
> `Renderer::push_filter`）；vello 用 `filter_effects` 单原语，SVG 用 `<filter>`（feGaussianBlur /
> feDropShadow）。注意 vello filter 在**多线程**渲染下会 panic（lievisual 默认单线程，不受影响）。

### P2 — 能力补齐

- 路径运算：bool ops（union/intersect/difference/xor）、path measure、offset/inset、**stroke-to-path**。
- **每角圆角**：kurbo `RoundedRectRadii` 已就绪，`Element::RoundedRect` 仅暴露单一半径。
- **描边对齐**（inside / center / outside）：目前仅 center。
- 文本：**多行裁剪 + 省略号**、文字沿路径、竖排 CJK、text-decoration 的位置/厚度/线型。
- 图片：双线性之外的 **bicubic**、平铺、**图片作为填充 shader**。
- **抗锯齿开关**（per-node）：vello 有 `set_aliasing_threshold`。
- **色彩空间 / gamma**：目前 sRGB 空间混合（Skia 默认线性空间混合）；宽色域无。

### P3 — 工程化 / 生态

- `Scene` 的 **serde 序列化**（缓存 / 传输 / 快照测试）。
- 显示列表录制/回放；**hit-testing**（`SceneNode::name` 已具备基础）。
- GPU 后端（README 已列 planned）。
- 字形 / 文本缓存对外 API（vello 内部已有 glyph cache）。

---

## 3. Scene 是否需要树形层次设计

### 3.1 现状：已经是树

- `SceneNode` 的 `element` 可以是 `Element::Group { children: Vec<SceneNode> }`，可任意深度嵌套；
  `transform` / `opacity` / `clip` / `visible` / `name` 递归作用于子树；同级按 `z_index` 稳定排序。
- 因此**问题不是"要不要树"，而是"容器的表达方式是否干净"**。

### 3.2 现存设计问题

1. **容器与可绘制混在 `Element` 枚举里**：`Group` 是唯一的"非叶子"变体，导致所有遍历都要特判
   （`src/render/mod.rs`、`src/fit.rs`、`src/sketch/pass.rs`、`src/builder.rs` 各自 `match Element::Group`）。
   将来再加"隔离层 / 遮罩组"会继续污染 `Element`。
2. **`Layer` 与 `Group` 语义重叠**：`Layer` 本质是"命名的根 Group"（`visible/opacity/transform/name` + nodes），
   却与 `Group` 分成两套代码路径（`render_layer` vs `render_node`）。
3. **`Scene` 不是单一根**：`nodes` 与 `layers` 并列，`scene_bounds` 等要分别 union 两条集合。
4. **属性模型混了两类语义**：
   - 可**传递**属性：`transform` / `clip` / `visible`；
   - 需**隔离**属性：`opacity` / `blend` / `mask`（必须离屏合成）。
   现在把 opacity 当传递属性逐图元乘 alpha，直接引发 P0-1。
5. **`z_index` 是同级排序键**，不跨层比较（注释有说明，但易被误当全局 z）。

### 3.3 建议（按性价比，不必大重写）

- **Step 1 · 收敛容器（低风险，零行为变化，建议先做）**
  保留 `SceneNode.element` 兼容，引入统一的子节点访问器，例如
  `SceneNode::children() / children_mut() / is_group()`，让 fit / sketch / render / hit-test **共用一套递归**，
  消除各处的 `match Element::Group`；并让 `Layer` 复用 group 渲染路径，`Scene` 逐步收敛为"根 group 列表"。

  > **状态：已实现（2026-10-13，lievisual 0.3.0）** —— 新增 `SceneNode::{is_group, children,
  > children_mut, visit, visit_mut}`、`Scene::{visit_nodes, visit_nodes_mut, root_lists,
  > root_lists_mut}` 与 `Renderer::render_children`；sketch / fit / render 的 `Element::Group`
  > 特判已收敛到访问器与统一递归（纯重构，零行为变化）。

- **Step 2 · 隔离合成（正确性，关键）**
  给节点加 `compositing`（`opacity` / `blend_mode` / `mask` 作为**离屏层**语义）：
  - vello：`RenderContext::push_layer(clip, blend, opacity, mask, filter)`；
  - SVG：`<g opacity="…" style="mix-blend-mode:…; isolation:isolate" mask="url(#…)">`。
  这一步**比"改成树"重要得多** —— 同时修复 P0-1 的跨后端不一致，并解锁 blend / mask。

  > **状态：opacity + blend 已完成（2026-10-13）**；**mask 未做** —— vello 的 `Mask` 是
  > **与画布等尺寸的位图**（非路径），需先设计"路径 → mask 位图"的渲染与缓存，属后续单独增量。

- **Step 3 · 遍历 API（可选）**
  提供 `visit` / `visit_mut`（深度优先 + 层级路径），供 fit / sketch / hit-test 复用；
  `z_index` 保持"同级稳定排序"。

### 3.4 结论

**不必须改树**（已经是树）。真正需要的是：
**(a) 容器与可绘制解耦**（可维护性，Step 1）；
**(b) 隔离合成语义**（正确性 + 跨后端一致，Step 2）。

### 3.5 兼容性提示

`Element` 是 `pub enum`，新增变体 / 改结构对下游的穷举 `match` 属 **semver 破坏性**变更
（现下游多用 `_ =>` / `if let` 兜底，影响可控）。若采用 Step 1 的"访问器"路线，可做到**零破坏**。

---

## 4. 落地顺序建议

1. **Step 1（容器收敛）** —— 纯重构、零行为变化，为后续铺路。
2. **P0-1 + P0-2（隔离合成 + BlendMode）** —— 合并为同一个 `compositing` 设计；加跨后端一致性回归测试。
3. **P1 低成本项**：Mask、Sweep 渐变、渐变 spread、ColorFilter。
4. **P1 中成本项**：drop-shadow / blur 滤镜。
5. **P2 / P3** 按下游实际需求驱动，避免为"对齐 Skia"而过度设计。
