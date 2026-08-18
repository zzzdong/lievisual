# lievisual 重构日志

> 记录本次重构（`docs/refactor-lievisual.md` 的落地）的变更点、临时调整，以及与初始计划的差异。
> 每次重构会话追加一节；条目按时间倒序排列（最新在上）。

---

## 会话 3（2026-08-18）：Image 集成重构——pixmap 化、不做解码、依赖 feature gate

### 背景与计划

按讨论的四点规划重做图片集成：
1. **自有 pixmap**：`SceneImage` 改持已解码 RGBA8 位图（自有类型，与后端解耦）。
2. **lievisual 不做解码**：移除 `image::load_from_memory` 解码路径。
3. **引入 `png` 库做 PNG 编解码**（常驻）。
4. **feature gate 引入 `image`** 做额外编解码（默认不启用）。

### 变更点

| 文件 | 变更 |
|------|------|
| `Cargo.toml` | 新增常驻 `png = "0.18"`；`image` 改 `optional` + 新 feature `extra-image-codecs = ["dep:image"]`（默认关闭） |
| `src/pixmap.rs` | **新增**自包含 RGBA8 位图类型 `Pixmap`（见下） |
| `src/scene.rs` | `SceneImage` 从"编码字节"改持 `pixmap: Pixmap`；新增 `from_pixmap` / `from_rgba8` / `from_png`；删除 `data`/`format`/`mime()`；`from_encoded` 置于 feature 内 |
| `src/lib.rs` | 注册 `pixmap` 模块；re-export `Pixmap` |
| `src/render/vello_pixmap.rs` | `draw_image` 移除解码，直接消费 `image.pixmap`；新增 `resize_rgba8` 双线性缩放替代 `image::resize_exact`；`pixmap_to_png` 改用 `png` crate 编码 |
| `src/render/svg.rs` | `draw_image` 从 base64 原始字节改为 `pixmap.to_png()` → `data:image/png` |
| `src/render/mod.rs` | `Renderer::draw_image` 文档更新（不再解码） |

### `Pixmap` 设计（`src/pixmap.rs`）

- 自有类型：`width` / `height` / `pixels: Arc<[u8]>`（RGBA8 紧排，`stride = 4*width`）。
- 构造：`from_rgba8(width, height, pixels)`（长度校验）、`decode_png`。
- 编码：`to_png()`（RGBA8 → PNG，供 SVG 内嵌 / `render_png`）。
- 解耦：不依赖 vello/SVG，各后端按需消费。
- 解码约定：lievisual 只做 PNG 解码（`png` 库）；jpeg/webp/gif 等经 `image`（feature）。

### `SceneImage` API 变化

- 删除：`data` / `format` / `mime()`（编码字节与格式标签概念移除）。
- 新增：`from_pixmap(Pixmap)`、`from_rgba8(w,h,Arc<[u8]>)`、`from_png(&[u8])`、`width()` / `height()`。
- 保留：`object_fit` / `with_object_fit` / `preserve_aspect_ratio` / `Element::image`。
- feature 内：`from_encoded(&[u8])`（jpeg/webp/gif 解码）。

### 验证

- `cargo build`（默认 + `--features extra-image-codecs`）均通过。
- `cargo clippy`：无新增告警。
- `cargo test`：96 passed / 1 failed（唯一失败仍是既有 `condensed_font_width_is_narrower`，与本次无关）。
- 新增测试：`Pixmap` 的构造校验 / PNG 往返 / 解码拒绝垃圾；`SceneImage` 的尺寸校验 / from_png 往返 / object-fit 映射 / 元素构造；SVG 图片 data-uri 断言更新。

### 临时调整 / 与计划不符处

1. **`png` 版本未显式对齐**：`png 0.18.1` 已在依赖树（`image` 传递引入），直接锁定 `png = "0.18"` 复用同一版本，避免重复。
2. **SVG 内嵌改为重编码**：由于 pixmap 化后 `SceneImage` 不再持有原始编码字节，SVG 只能把 RGBA8 重编码为 PNG 再内嵌。对 PNG 源无损；对 JPEG 源（feature 开启时）为"有损→无损"，格式统一为 PNG。**记录为预期行为差异**。
3. **`resize_rgba8` 自实现**：移除 `image` 解码依赖后，vello 的 object-fit 缩放需自实现。用双线性插值替代 `image::resize_exact(Triangle)`，够图表场景用；若需更高质量可后续换用其它缩放库。
4. **`image` 仅用于 decode 的 feature**：`extra-image-codecs` 仅暴露 `SceneImage::from_encoded`。PNG 编码走 `png` 库，`image` 不再承担编码职责。
5. **测试 base64 断言放宽**：SVG 图片测试原断言 `data:image/png;base64,AQID`（假字节），现改为断言 `data:image/png;base64,` 前缀 + 尺寸，因真实 PNG 编码字节不可预写。

### 下一步（未做，属后续会话）

- 计划 Step 4：liepress 接入（文本层切换、移除 parley 直依赖）——由用户独立进行。
- 计划 Step 5：GPU 后端、`Scene::bounds()` 等长期项。

---

## 会话 1（2026-08-18）：`TextRun` 收口 + `GlyphOutline`

### 背景与计划

按 `docs/refactor-lievisual.md` 落地顺序的第 1、2 步：

1. **Step 1 `TextRun` 收口**：补 `font_index`、`font_data` 统一为 `Arc<Vec<u8>>`、移除 parley 公共依赖。
2. **Step 2 `GlyphOutline`**：glyph id → `BezPath`，服务 PDF/DOCX 矢量字形。

### 变更点

| 文件 | 变更 |
|------|------|
| `Cargo.toml` | 新增依赖 `skrifa = "0.44"`（与依赖树中 parley 同源的版本对齐） |
| `src/lib.rs` | 注册 `pub mod glyph;` |
| `src/text.rs` | `TextRun` 新增 `font_index: u32` 字段；提取层 `extract_lines_from_parley` 从 `run.font().index` 捕获集合索引并透传到 `TextRun` |
| `src/glyph.rs` | **新增**字形轮廓模块（见下） |

### Step 1 细节（`src/text.rs`）

- `TextRun` 新增公开字段：
  ```rust
  /// 字体在字体文件集合（ttc / otc）中的索引。单字体文件恒为 0。
  pub font_index: u32,
  ```
- `extract_lines_from_parley` 内部 `run_infos` 元组从
  `(Color, Arc<Vec<u8>>, f32, parley::layout::Run, f32)` 扩为
  `(Color, Arc<Vec<u8>>, u32, f32, parley::layout::Run, f32)`（插入 `font_index`）。
- 捕获点：`let font = run.font(); let font_index = font.index;`（`font.index` 即
  ttc 集合索引，liepress `pdf.rs` 经 `run.font_data.index` 消费的正是同一字段）。
- 消费方 `vello_pixmap.rs` 读 `run.font_data`（不变），无需改动。

### Step 2 细节（`src/glyph.rs`，新增）

- 公开 API：`GlyphOutline::outline(font_data: &[u8], font_index: u32, glyph_id: u32, font_size: f32) -> Option<BezPath>`。
- 内部实现（skrifa）：
  1. `FontRef::from_index(&data, index)` —— 同时支持单字体文件与 ttc/otc 集合，索引越界返回 `None`；
  2. `font.outline_glyphs().get(glyph_id)` —— 取回 `OutlineGlyph`；
  3. `outline.draw(DrawSettings::unhinted(Size::new(font_size), LocationRef::default()), &mut pen)` ——
     以 ppem 缩放、无变量坐标；
  4. 自定义 `BezPathPen` 实现 skrifa `OutlinePen`，把 move/line/quad/curve/close 收集为 `kurbo::BezPath`。
- 坐标约定：输出 **y-down**（与 lievisual 其余几何 / Canvas 一致）；字形原点在基线；已按 `font_size` 缩放。
- 纯函数、无内部状态；未做轮廓缓存（见"与计划不符"）。
- 测试：`outline_extracts_glyph_path`（sans-serif 的 'A' 有非空矢量轮廓 + 非零包围盒）、
  `outline_handles_invalid_input`（坏字节 / 越界集合索引返回 `None`）。

### 验证

- `cargo build` 通过。
- `cargo test`：**85 passed / 1 failed**。新增 2 个 glyph 测试通过；唯一失败为
  `text::tests::decoration::condensed_font_width_is_narrower`。
- **该失败经 `git stash` 验证为既有问题**（原始代码同样失败，与本次改动无关），故不计为回归。
- `cargo clippy` 对新增代码无告警。

### 临时调整 / 与计划不符处

1. **`PathStyle` 不可用（计划偏离）**：初始想显式 `DrawSettings::with_path_style(PathStyle::FreeType)`，
   但 skrifa 0.44 的 `PathStyle` 在 `outline` 模块内是**私有**的（`use pen::PathStyle`，未 `pub use`），
   无法从外部 import。实际 `DrawSettings::unhinted` 默认即 `PathStyle::FreeType`，故移除显式设置、
   去掉 `PathStyle` 导入即可。**文档/API 均不受影响**。
2. **`LocationRef` / `Size` 仅经 `skrifa::prelude` 导出**：二者不在 skrifa crate 根，
   必须从 `skrifa::prelude` 导入；`FontRef` / `GlyphId` 在根。已据此调整 import。
3. **`bounding_box()` 需显式导入 `kurbo::Shape` trait**：测试中断言轮廓包围盒时，
   `BezPath::bounding_box()` 由 `Shape` trait 提供，测试内补 `use kurbo::Shape;`。
4. **轮廓缓存延后**：计划文档第 6 节未提缓存；当前实现"每次调用重建轮廓集合"，
   未做跨字形缓存。设计上保留"纯函数"语义，由消费方（如 liepress）按需做轮廓缓存。
   若后续字形吞吐成为瓶颈，再引入进程级缓存（类似 `text.rs` 的 `FONT_BYTE_CACHE`）。

### 下一步（未做，属后续会话）

- 计划 Step 4：liepress 接入（文本层切换、移除 parley 直依赖）——由用户独立进行。
- 计划 Step 5：GPU 后端、`Scene::bounds()` 等长期项。

---

## 会话 2（2026-08-18）：Builder 层（Canvas 风格便捷构造器）

### 背景与计划

完成 `docs/refactor-lievisual.md` 落地顺序的 **Step 3**：提供 Canvas 风格便捷构造层
（builder），在底层声明式 IR 之上加糖衣，产出 [`Element`]，不破坏 IR 与后端分离。

### 变更点

| 文件 | 变更 |
|------|------|
| `src/lib.rs` | 注册 `pub mod builder;` |
| `src/builder.rs` | **新增** Canvas 风格构造层（见下） |

### 设计

`Ctx`（不可变、可链式 `with_*` 派生）持有 Canvas 风格默认状态：
- `fill: Option<Color>`（`fillStyle`，仅作用于**形状**填充）
- `stroke: Option<Stroke>`（`strokeStyle` + `lineWidth`）
- `font: TextStyle`（`font` + `textAlign` + `textBaseline`）

方法均为纯函数返回 [`Element`]，可经 `From<Element>` 自动转 `SceneNode` 后 `scene.push(...)`：

| 方法 | Canvas 对应 | 说明 |
|------|------------|------|
| `fill_text(x, y, text)` | `fillText` | `(x,y)` 为锚点；对齐/基线由 `font.align`/`font.baseline` 决定 |
| `measure_text(text)` | `measureText` | 返回 `Size` |
| `fill_rect / stroke_rect` | `fillRect/strokeRect` | `(x,y)` 左上角 + `w/h` |
| `fill_circle / fill_ellipse` | `arc`+fill / `ellipse`+fill | 圆心/半径/半轴 |
| `line / polyline / polygon` | `moveTo/lineTo/…` | |
| `path(path, closed)` | `beginPath`+fill | 透传 `BezPath` + closed |

**文本锚点关键点**：渲染端（vello/SVG）**内部**已按 `style.align`/`style.baseline`
计算偏移（`vello_pixmap.rs:417-427` 的 `dx`/`dy`），所以 builder 的 `fill_text` 无需
手动算偏移，`(x,y)` 直接作为锚点即可——与 Canvas `fillText` 语义天然一致。

测试：7 个（锚点/颜色/矩形/形状/路径/测量/push 集成）。

### 验证

- `cargo build`、`cargo test`（92 passed / 1 failed）、`cargo clippy` 均通过。
- 唯一失败仍为**既有** `condensed_font_width_is_narrower`，与本次无关。
- builder 文档测试（doctest）通过。

### 临时调整 / 与计划不符处

1. **文本色与形状填充解耦（设计澄清）**：Canvas 里 `fillStyle` 同时决定形状与文本颜色；
   但 lievisual 把文本色并入 `TextStyle.color`。初始实现让 `fill` 优先覆盖文本色，导致
   `with_text_color` 被默认 `fill` 覆盖而"失效"。改为：**文本色 = `font.color`**（`with_text_color`
   写入该字段），**形状填充 = `fill`**。两者解耦，`with_fill` 不意外覆盖文本。已在模块文档与
   字段注释中说明这一与 Canvas 的差异。
2. **未实现 `strokeText`**：Canvas 有 `strokeText`（描边字形轮廓），但 lievisual 的
   `Element::Text` 仅支持字形填充，无字形描边。为避免误导性 API，builder 暂不提供
   `stroke_text`（记录为"暂不支持"，后续若需字形描边再扩展 IR）。
3. **`fill_stroke` 初稿冗余**：初始用 `self.fill.map(FillStrokeStyle::fill).and_then(|s| s.fill)`
   提取填充色，逻辑冗余；改为直接 `self.fill.map(Fill::Solid)`，并新增 `shape_fill()`
   （`fill` 回退到 `font.color`）统一形状填充色。
4. **未加 `setTransform`/`rotate` 等变换糖衣**：Canvas 的 `ctx.translate/rotate/scale` 在
   lievisual 中是 `SceneNode::with_transform`（节点级）。builder 层暂不包装变换，避免重复；
   用户可直接 `SceneNode::new(el).with_transform(...)`。记录为后续可选增强。

### 下一步（未做，属后续会话）

- 计划 Step 4：liepress 接入（文本层切换、移除 parley 直依赖）——由用户独立进行。
- 计划 Step 5：GPU 后端、`Scene::bounds()` 等长期项。
