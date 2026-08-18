# lievisual API 分析与重构指导

> 目标定位：`lievisual` 是一个**渲染库**（类似 Canvas + Pango 的合体劣化版），不是布局库。
> 它只负责"给定坐标，画出来"；布局 / CSS / 分页等"该放哪、跨不跨页"的决策由上层业务
> （liecharts / liemermaid / liepress）自行负责。
>
> 本文档记录对 4 个初步想法的评估、关键事实调查，以及据此给出的重构指导与落地顺序。

---

## 0. 一句话结论

四个想法方向都正确，但**当前最大的杠杆不是新功能，而是消除已经存在的重复**：
`liepress` 已经用 `parley 0.11` + `vello_cpu 0.2` 把 lievisual 的文本层几乎完整复制了一遍。
重构的核心 = **把 lievisual 的文本排版/绘制能力收口成"自闭环合同"，让 liepress 的文本层
切换过来**，并把 `parley` 从 liepress 的公共依赖中移除。

---

## 1. 项目定位（先校准，避免方向跑偏）

- `lievisual` = **渲染库**，定位为 "Canvas + Pango 的劣化版"。
- **不做 flex / box 布局树**。布局是上层业务（图表画布 / 文档引擎）自己算坐标的事。
- 渲染库职责：`Scene`/`Element` 描述"画什么"，`Renderer` 描述"怎么画"；
  文本排版给定坐标/宽度，产出可绘制字形。

### 职责分界（渲染库 vs 文档引擎）

| 属于 `lievisual`（渲染库）            | 属于 `liepress`（文档引擎）        |
|--------------------------------------|------------------------------------|
| 富文本排版（span → 多行 `TextLayout`）| CSS 计算引擎                      |
| 文本测量（给宽度 → 尺寸 / 度量）      | box / 表格 / 块级布局              |
| 字形 → outline（矢量保真）            | 分页 / 分列 / 页面                 |
| 把 `TextLayout` 画到 SVG / PNG / 矢量 | Markdown → AST → HTML → Document   |
| 图片编解码                           | 页眉页脚 / 大纲 / 书签              |

**分界线一句话**：`lievisual` 回答"单段文本在给定宽度下长什么样、以及怎么画它"；
`liepress` 决定"这段放哪个框、跨不跨页"。

---

## 2. 四个想法的逐条评估

### 想法 1：以 Scene IR 出发，渲染得到 PNG / SVG

**结论：方向正确，且已是现状。**

- 现状 `Scene`/`Element` 就是 IR，`SvgRenderer` 出矢量、`VelloPixmapRenderer` 出 PNG。
- 这是已建成的主干，无需新增；但注意现有 `Scene` 是"绘制命令列表"（描述怎么画），
  **不回答"放哪、多大"**。这不需改变——因为布局不属于渲染库职责。
- 唯一需要明确的是：**IR 与后端分离**是核心竞争力，后续所有 API 设计都不得破坏它。

### 想法 2：文本排版（同步提供测量）+ 渲染

**结论：方向正确，且测量不是"排版的拓展"，而是"排版的自然产出"。**

- 现有 `text.rs` 已做对核心：`RichSpan`（样式化片段）→ `layout_text`/`measure_text`
  共用同一条 parley 管线（保证测量与绘制尺寸一致），返回自闭环 `TextLayout`。
- 定位要纠正：**排版是核心，测量是排版结果的直接读取**，不是并列功能。
- 目标能力边界（对齐 Pango 的"劣化版"）：
  - 单段文本 + 样式 + 宽度 → 多行 `TextLayout`（现有 `max_width` 换行）。
  - **不做段落流、分页、分栏**（那是文档引擎的事）。

### 想法 3：友好 API，参考 Web Canvas API

**结论：方向正确，但"参考"要理解为"心智模型"而非"形态照搬"。**

- Canvas 是命令式（可变 `ctx` + 方法序列）；`lievisual` 的 IR 是声明式（不可变场景树）。
  **直接照搬命令式形态会破坏 IR 与后端分离的核心价值。**
- 正确姿势 = 两层 API：
  - 底层：保持声明式 `Scene`/`Element`（核心竞争力，别动）。
  - 上层：Canvas 风格的**便捷构造层（builder）**，如 `ctx.fill_text(x, y, text, style)`
    内部算好 offset 生成 `Element::Text`。builder 是可选的糖衣，不污染 IR。

### 想法 4：暴露 parley/kurbo 定义，还是自己独立实现一套

**结论：不重造一套，但要"包装而非透传"。**

- 几何：**re-export kurbo**（`Point`/`Rect`/`Vec2`/`Size`）。kurbo 极稳定、是
  vello/parley 共享基座，且已消除"两套坐标 + 缝合函数"反模式。
- 排版结果：**自研薄封装**（`TextLayout`/`TextLine`/`TextRun`/`Glyph`），作为公共合同，
  内部从 parley 提取。升级 parley 时只改提取层。
- 排版引擎：**parley 只做内部依赖**，不向公共 API 泄漏 `parley::Layout` /
  `RangedBuilder` / `StyleProperty` 等内部类型。
- 样式：自研 `TextStyle`/`RichSpan`，内部映射 parley `StyleProperty`。

**核心原则**：对外合同 = 你的类型（自闭环、可序列化、脱离 parley 可消费）；
parley/kurbo 只是实现细节。

---

## 3. 关键事实调查：liepress 现状

> 决定性发现：`liepress` 已把 lievisual 的文本层完整复制了一份，并**直接依赖 parley**。

### 3.1 liepress 的重复实现

`liepress/src/text.rs` 拥有与 lievisual 几乎同构的：
- `TextLayout` / `TextLine` / `TextRun` / `Glyph`
- `extract_lines_from_parley`（从 parley `Layout` 提取行，逻辑与 lievisual 高度重合）
- `TextStyle` / `TextAlign` / `FontSource` / `register_font`（线程本地 TLS 上下文）
- `layout_text` / `create_text_layout` / `create_text_layout_with_contexts`

`liepress/src/visual.rs` 拥有 lievisual `Scene`/`Element` 的同构副本：
- `VisualElement`（Rect / RoundedRect / Circle / Line / Polyline / Path / GradientPath /
  TextLine / Image / Group / ZGroup）
- `Transform` / `Color` / `Stroke` / `StrokeStyle` / `FillStrokeStyle` / `GradientDef`

`liepress/src/render/` 有 3 个后端：
- `pdf.rs`（krilla 0.8）、`pixmap.rs`（vello_cpu）、`svg.rs`

### 3.2 liepress 与 lievisual 的依赖对齐

| 依赖 | liepress | lievisual |
|------|----------|-----------|
| parley | 0.11（直依赖） | 0.11 |
| vello_cpu | 0.2 | 0.2 |
| 其他 | krilla 0.8、pulldown-cmark、lightningcss 等 | image |

版本完全对齐 → 存在"集中到一处"的可行性基础。

### 3.3 PDF 后端确实需要"从 run 提取 font data"

`liepress/src/render/pdf.rs:497` 的 `draw_text_run` 依赖：
- `run.font_data.data`（字体原始字节）→ 构造 krilla `Font`（`pdf.rs:530-533`）
- `run.font_data.index`（ttc/otc 集合索引）→ 构造 krilla `Font` 与缓存 key（`pdf.rs:39-47`）

这是**硬需求**：PDF 要嵌入字体，必须拿到字体字节 + index。
liepress 现在通过 `run.font_data: parley::FontData` 直接持有 parley 类型来满足。

---

## 4. 对"是否一对一暴露 parley"的最终结论

**结论：不要一对一暴露，但要满足被下游真实消费的两个数据需求。**

parley 概念是否暴露，按下游是否真用决定：

| parley 概念 | 下游（liepress）真用？ | lievisual 策略 |
|------------|----------------------|----------------|
| `FontData` / `Blob` | **用**（krilla 嵌入字体） | 用自有 `font_data: Arc<Vec<u8>>` + `font_index: u32` 替代 |
| `Layout` 原始结构 | 否 | 不暴露，经自有 `TextLayout` |
| `RangedBuilder` | 否 | 不暴露 |
| `StyleProperty` | 否 | 不暴露，用自有 `TextStyle` |
| 行 / run / glyph 结构 | 用 | 自有 `TextLine` / `TextRun` / `Glyph`（已有） |
| `Alignment` | 用 | 自有 `TextAlign`（已有） |

**唯一透出的例外 = 字体字节 + index**，因为它是真实的二进制资源（不是逻辑结构），
且是 PDF/DOCX 嵌入字体的前提。

---

## 5. 重构目标与指导

### 5.1 总体目标

让 `liepress` 用 lievisual 的文本排版做 document 排版，并移除 liepress 对 parley 的直依赖。
保留 liepress 的 CSS / box / 分页逻辑（这些不下沉到渲染库）。

### 5.2 目标链路（成为 lievisual 的验收标准）

```rust
// liepress 用法：给定宽度 → 排版 → 画到任意后端
let layout: Arc<TextLayout> = lievisual::text::layout_text(&spans, Some(paragraph_width));
for line in &layout.lines {
    for run in &line.runs {
        renderer.draw_text_run(run, origin + line.bounds.origin);
        // 或矢量后端：把 run.glyphs 转 path（经 GlyphOutline）
    }
}
```

`TextLayout` 必须满足：
1. 坐标全绝对、自包含（不依赖 parley 生命周期）——已做到。
2. 行度量（ascent / descent / line_height / baseline）在 `TextLine` 直接可读——已有 `LineMetrics`。
3. **补 `font_index`** —— PDF 字体嵌入的 ttc 索引（当前缺口）。
4. **`GlyphOutline` 查询** —— glyph id → `BezPath`（PDF/DOCX 精确矢量字形，当前缺口）。

### 5.3 指导 A：`TextRun` 收口为自闭环合同

- `font_data` 统一为 `Arc<Vec<u8>>`（共享，不复制整份字体）。
- 新增 `font_index: u32`。
- 移除对 `parley::FontData` / `parley::layout::Run` 等内部类型的公共依赖。
- 升级 parley 时，只有 lievisual 的提取层（`extract_lines_from_parley`）需要改动。

### 5.4 指导 B：新增 `GlyphOutline`（矢量文本保真）

- 暴露查询：`(font_data, font_index, glyph_id) -> BezPath`，内部调 fontique/fontations。
- 服务场景：PDF / DOCX 精确矢量字形；SVG 文本矢量保真（README 规划项 3 的前提）。
- 这是"从 run 提取数据"的第二个真实需求。

### 5.5 指导 C：`Element::Text` 预排版路径成为一等公民

- `Element::Text { layout: Option<Arc<TextLayout>> }` 已支持预排版。
- liepress 用法：排版 → 把 `Arc<TextLayout>` 塞进 `Element::Text` → 后端直接画，不重复排版。
- 这条路径要在 API 与文档上明确定位，作为 liepress 接入的入口。

### 5.6 指导 D：Builder 层（Canvas 风格，可选糖衣）

- 提供 Canvas 风格便捷构造器（`fill_text` / `measure_text` / 锚点定位），内部算好 offset。
- 不污染声明式 IR，保持两层 API 分离。

---

## 6. 落地顺序（建议）

| 步骤 | 内容 | 动机 |
|------|------|------|
| 1 | **`TextRun` 收口**：补 `font_index`、`font_data` 统一为 `Arc<Vec<u8>>`、移除 parley 公共依赖 | 后续一切的前提；改动最聚焦 |
| 2 | 补 `GlyphOutline`（glyph id → `BezPath`） | 服务 PDF/DOCX 矢量字形 |
| 3 | Builder 层（Canvas 风格便捷构造器） | 友好 API 落地，不破坏 IR |
| 4 | liepress 接入：文本层切换过来，移除 parley 直依赖 | 消除重复，验证合同 |
| 5 | 后续：GPU 后端、`Scene::bounds()` 等 README 规划项 | 长期演进 |

---

## 7. 风险与注意

- **版本对齐**：接入时需保证 lievisual 与 liepress 的 parley / vello_cpu 版本一致
  （当前都为 0.11 / 0.2，可行）。
- **能力差异**：lievisual 的 span 数组已覆盖 Pango 绝大多数字形级属性；`rise`（上下标）与
  `background`（行内背景）为 parley 硬限制，lievisual 在渲染端手工模拟（已有实现），
  liepress 现有 `baseline_shift` / `background_color` 用法需对齐。
- **行为回归**：接入时以 liepress 现有测试（`tests/`）为回归基准，确保 PDF/SVG/PNG
  输出不劣化。
