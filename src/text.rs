//! 文本排版支持（参考 Web Canvas 文本接口）。
//!
//! 对齐 Web Canvas 的 `CanvasRenderingContext2D` 文本 API：
//! - [`TextStyle`] 对应 `ctx.font`（`font-style` / `font-weight` / `font-size` /
//!   `line-height` / `font-family`）+ `textAlign` + `textBaseline`。
//! - [`TextAlign`] 对应 `ctx.textAlign`：`position.x` 作为水平锚点。
//! - [`TextBaseline`] 对应 `ctx.textBaseline`：`position.y` 作为垂直锚点。
//! - [`measure_text`] 对应 `ctx.measureText()`，返回 [`TextMeasure`]（含
//!   [`TextMetrics`]，即 canvas 的 `TextMetrics` 对象）。
//!
//! 文本内容统一以**样式化片段**（[`RichSpan`]，文本 + 样式）为核心表达：
//! 单文本是一个片段的特例，富文本是多个片段按序拼接。排版 / 测量统一经
//! [`measure_text`] / [`layout_text`] 走**span 列表**入口；每段可独立设置
//! 字体族 / 字号 / 字重 / 风格 / 颜色 / 行高。
//! - [`crate::scene::Element::Text`] 的 `position` 语义 = canvas 的 `fillText(x, y)`：`x` 由
//!   `align` 决定，`y` 由 `baseline` 决定（默认 `Alphabetic`，即 y 是基线位置）。
//!
//! ## 锚点语义
//! 水平锚点（[`TextAlign`]）：
//! - [`TextAlign::Left`]：`position.x` 在文本块左边缘；
//! - [`TextAlign::Center`]：`position.x` 在文本块水平中心；
//! - [`TextAlign::Right`]：`position.x` 在文本块右边缘。
//!
//! 垂直锚点（[`TextBaseline`]，canvas 语义）：
//! - [`TextBaseline::Top`]：`position.y` 在最上行顶；
//! - [`TextBaseline::Hanging`]：hanging baseline（约 0.8 × 上肩高）；
//! - [`TextBaseline::Middle`]：em 盒垂直中心；
//! - [`TextBaseline::Alphabetic`]：字母基线（默认，`position.y` 是基线）；
//! - [`TextBaseline::Ideographic`]：CJK 基线（约基线下方 0.1 × 下伸量）；
//! - [`TextBaseline::Bottom`]：最下行底。
//!
//! 度量与基线值均为 `f64`（与 IR 几何一致），由 [`TextMetrics`] 提供。
//!
//! ## 排版上下文缓存
//! parley 的 `FontContext` / `LayoutContext` 被设计为"每应用/每线程一个"：
//! 前者缓存字体库与源数据（LRU），后者复用 scratch 分配与字形缓存。
//! 本 crate **所有 parley 相关操作**（测量 [`measure_text`]、内部排版）统一通过
//! **thread-local** 持有这两个上下文：首次调用按线程懒初始化，之后线程内所有排版
//! 复用同一实例，避免反复的系统字体扫描与分配；多线程各持独立实例，无需同步。

use crate::geometry::{Color, Rect, Size};
use std::sync::Arc;

/// 文本水平对齐方式（`position.x` 作为锚点）。对应 Canvas `textAlign`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TextAlign {
    /// 锚点在文本块左边缘（默认）。
    #[default]
    Left,
    /// 锚点在文本块水平中心。
    Center,
    /// 锚点在文本块右边缘。
    Right,
    /// 两端对齐（最后一行左对齐）。parley 在排版时调整字距实现。
    Justify,
}

/// 文本垂直锚点（`position.y` 作为锚点）。对应 Canvas `textBaseline`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TextBaseline {
    /// 锚点在最上行顶。
    Top,
    /// 锚点在 hanging baseline（约 0.8 × 上肩高）。
    Hanging,
    /// 锚点在字母基线（默认，`position.y` 即基线）。
    #[default]
    Alphabetic,
    /// 锚点在 em 盒垂直中心。
    Middle,
    /// 锚点在 CJK 基线（约基线下方 0.1 × 下伸量）。
    Ideographic,
    /// 锚点在最下行底。
    Bottom,
}

impl TextBaseline {
    /// 从文本块顶部到锚点的偏移（向下为正，y-down 坐标）。
    pub(crate) fn anchor_offset(&self, m: &TextMetrics) -> f64 {
        match self {
            TextBaseline::Top => 0.0,
            TextBaseline::Hanging => m.hanging_baseline,
            TextBaseline::Alphabetic => m.alphabetic_baseline,
            TextBaseline::Middle => m.alphabetic_baseline - m.em_height_ascent * 0.5,
            TextBaseline::Ideographic => m.ideographic_baseline,
            TextBaseline::Bottom => m.height,
        }
    }
}

/// 字体风格。对应 Canvas `ctx.font` 的 `font-style`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FontStyle {
    /// 正体（默认）。
    #[default]
    Normal,
    /// 斜体。
    Italic,
    /// 倾斜。
    Oblique,
}

/// 文本样式。对应 Canvas `ctx.font` + `textAlign` + `textBaseline`。
#[derive(Debug, Clone, PartialEq)]
pub struct TextStyle {
    /// 文本颜色（canvas 中通常由 `fillStyle` 决定；此处并入样式）。
    pub color: Color,
    /// 字体族，如 `"sans-serif"`、`"Helvetica, Arial, sans-serif"`（canvas `font-family`）。
    pub font_family: String,
    /// 字号（px，canvas `font-size`）。
    pub font_size: f64,
    /// 字重，100–900（canvas `font-weight`），默认 400。
    pub font_weight: f32,
    /// 字体风格（canvas `font-style`）。
    pub font_style: FontStyle,
    /// 字宽（font-stretch），比例值（1.0 = 常规；0.75 = condensed，1.25 = expanded）。
    /// `None` 使用字体默认（1.0）。
    pub font_width: Option<f32>,
    /// 行高（px）。`None` 使用字体默认行高（canvas `line-height` 省略时）。
    pub line_height: Option<f64>,
    /// 字间距（px，`letter-spacing`）。`0.0` = 不额外加距。
    pub letter_spacing: f64,
    /// 是否绘制下划线（`text-decoration: underline`）。
    pub underline: bool,
    /// 下划线颜色。`None` 使用前景色。
    pub underline_color: Option<Color>,
    /// 是否绘制删除线（`text-decoration: line-through`）。
    pub strikethrough: bool,
    /// 删除线颜色。`None` 使用前景色。
    pub strikethrough_color: Option<Color>,
    /// 基线偏移（px，正数=上移/上标，负数=下移/下标）。
    ///
    /// parley 无 rise 属性，lievisual 在渲染端对整个文本块做 y 平移实现。
    pub baseline_shift: f64,
    /// 文本背景色（行内高亮/行内代码灰底）。`None` 不画背景。
    ///
    /// parley 无 background 属性，lievisual 在渲染端画文本块矩形实现。
    pub background_color: Option<Color>,
    /// 超链接 URL（`None` 无链接）。lievisual 扩展（pango 的 `link` 属性）。
    pub url: Option<String>,
    /// 旋转弧度（绕文本锚点；lievisual 扩展，canvas 需 `ctx.translate/rotate`）。
    pub rotation: f64,
    /// 最大宽度（px）：`Some` 时在超过处换行（canvas `fillText` 的 `maxWidth` 近似，
    /// 但 canvas 是压缩文本、这里是换行）。
    pub max_width: Option<f64>,
    /// 水平锚点（canvas `textAlign`）。
    pub align: TextAlign,
    /// 垂直锚点（canvas `textBaseline`）。
    pub baseline: TextBaseline,
}

impl TextStyle {
    /// 便捷构造：纯色 + 字号 + 字体族，其余取默认（weight 400、normal、基线 alphabetic、左对齐）。
    #[must_use]
    pub fn new(color: Color, font_size: f64, font_family: impl Into<String>) -> Self {
        Self {
            color,
            font_family: font_family.into(),
            font_size,
            font_weight: 400.0,
            font_style: FontStyle::Normal,
            font_width: None,
            line_height: None,
            letter_spacing: 0.0,
            underline: false,
            underline_color: None,
            strikethrough: false,
            strikethrough_color: None,
            baseline_shift: 0.0,
            background_color: None,
            url: None,
            rotation: 0.0,
            max_width: None,
            align: TextAlign::Left,
            baseline: TextBaseline::Alphabetic,
        }
    }

    #[must_use]
    pub fn with_align(mut self, align: TextAlign) -> Self {
        self.align = align;
        self
    }

    #[must_use]
    pub fn with_baseline(mut self, baseline: TextBaseline) -> Self {
        self.baseline = baseline;
        self
    }

    /// 字重（100–900）。
    #[must_use]
    pub fn with_weight(mut self, weight: f32) -> Self {
        self.font_weight = weight;
        self
    }

    #[must_use]
    pub fn with_style(mut self, style: FontStyle) -> Self {
        self.font_style = style;
        self
    }

    /// 字宽（font-stretch），比例值（1.0 = 常规，0.75 = condensed，1.25 = expanded）。
    #[must_use]
    pub fn with_font_width(mut self, ratio: f32) -> Self {
        self.font_width = Some(ratio);
        self
    }

    /// 字间距（px，`letter-spacing`）。
    #[must_use]
    pub fn with_letter_spacing(mut self, spacing: f64) -> Self {
        self.letter_spacing = spacing;
        self
    }

    /// 是否绘制下划线，可选指定颜色。
    #[must_use]
    pub fn with_underline(mut self, underline: bool) -> Self {
        self.underline = underline;
        self
    }

    /// 下划线颜色（`None` 使用前景色）。
    #[must_use]
    pub fn with_underline_color(mut self, color: Option<Color>) -> Self {
        self.underline_color = color;
        self
    }

    /// 是否绘制删除线，可选指定颜色。
    #[must_use]
    pub fn with_strikethrough(mut self, strikethrough: bool) -> Self {
        self.strikethrough = strikethrough;
        self
    }

    /// 删除线颜色（`None` 使用前景色）。
    #[must_use]
    pub fn with_strikethrough_color(mut self, color: Option<Color>) -> Self {
        self.strikethrough_color = color;
        self
    }

    /// 基线偏移（px，正数=上移/上标，负数=下移/下标）。
    #[must_use]
    pub fn with_baseline_shift(mut self, shift: f64) -> Self {
        self.baseline_shift = shift;
        self
    }

    /// 文本背景色（行内高亮 / 行内代码灰底）。`None` 不画背景。
    #[must_use]
    pub fn with_background_color(mut self, color: Option<Color>) -> Self {
        self.background_color = color;
        self
    }

    /// 超链接 URL（`None` 无链接）。
    #[must_use]
    pub fn with_url(mut self, url: Option<String>) -> Self {
        self.url = url;
        self
    }

    /// 行高（px）。
    #[must_use]
    pub fn with_line_height(mut self, height: f64) -> Self {
        self.line_height = Some(height);
        self
    }

    #[must_use]
    pub fn with_rotation(mut self, rotation: f64) -> Self {
        self.rotation = rotation;
        self
    }

    #[must_use]
    pub fn with_max_width(mut self, width: f64) -> Self {
        self.max_width = Some(width);
        self
    }

    /// 输出符合 CSS / SVG `font-family` 语法的值（逗号分隔列表）。
    ///
    /// `font_family` 字段是用户可读的原始写法（可含空格、引号、通用族），本方法
    /// 规范化每个族名：通用族（`sans-serif` 等）与合法 CSS 标识符不加引号；
    /// 含空格 / 非 ASCII / 特殊字符 / 全局关键字的族名用单引号包裹并做 CSS 字符串转义。
    ///
    /// 输出**不含 XML 转义**（由 SVG 后端再经 `escape_attr` 处理），职责分层：
    /// - CSS 层（本方法）：决定加不加引号、`\'` `\\` 转义
    /// - XML 层（`escape_attr`）：`&`→`&amp;`、`"`→`&quot;` 等
    ///
    /// 例：`"Helvetica Neue, Arial, sans-serif"` → `'Helvetica Neue', Arial, sans-serif`
    #[must_use]
    pub fn font_family_css(&self) -> String {
        normalize_font_family_list(&self.font_family)
    }
}

/// 富文本的一段（样式化文本片段）。
///
/// 富文本由一段段 [`RichSpan`] 顺序拼接而成，每段可有独立样式：字体族 / 字号 /
/// 字重 / 风格 / 字宽 / 颜色 / 行高 / 字间距 / 下划线 / 删除线（对应 Pango 的
/// family/size/weight/style/stretch/foreground/underline/strikethrough/letter_spacing）。
/// 片段文本内的 `\n` 会被 parley 视为换行（与 [`TextStyle`] 的排版语义一致）。
///
/// 使用范式（排版 / 测量统一走 span 列表入口）：
/// ```
/// use lievisual::geometry::Color;
/// use lievisual::text::{RichSpan, TextStyle, measure_text};
/// let spans = vec![
///     RichSpan::new("Hello ", TextStyle::new(Color::BLACK, 20.0, "sans-serif")),
///     // 混排：加粗 + 下划线 + 删除线 + 字间距
///     RichSpan::new("World", TextStyle::new(Color::rgb(0, 0, 0xff), 20.0, "sans-serif")
///         .with_weight(700.0)
///         .with_underline(true)
///         .with_strikethrough(true)
///         .with_letter_spacing(2.0)),
/// ];
/// let m = measure_text(&spans, None);
/// assert!(m.size.width > 0.0);
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct RichSpan {
    /// 片段文本（可含 `\n`）。
    pub text: String,
    /// 该片段应用的样式。
    pub style: TextStyle,
}

impl RichSpan {
    /// 构造一个样式化片段。
    #[must_use]
    pub fn new(text: impl Into<String>, style: TextStyle) -> Self {
        Self {
            text: text.into(),
            style,
        }
    }
}

/// 规范化 `font-family` 列表（逗号分隔 → 每项独立规范化）。
fn normalize_font_family_list(raw: &str) -> String {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(normalize_family)
        .collect::<Vec<_>>()
        .join(", ")
}

/// 规范化单个族名：返回"加引号或不加"的正确形式（不含 XML 转义）。
fn normalize_family(raw: &str) -> String {
    let name = strip_family_quotes(raw);
    if is_generic_family(name) || is_css_ident(name) {
        name.to_string()
    } else {
        // CSS 字符串：单引号包裹 + 转义反斜杠与单引号。
        format!("'{}'", name.replace('\\', "\\\\").replace('\'', "\\'"))
    }
}

/// 剥离用户已加的单/双引号（若成对）。
fn strip_family_quotes(s: &str) -> &str {
    if (s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\'')) {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

/// CSS 通用族关键字（加引号会失效，必须原样输出）。
fn is_generic_family(s: &str) -> bool {
    matches!(
        s.to_ascii_lowercase().as_str(),
        "serif"
            | "sans-serif"
            | "monospace"
            | "cursive"
            | "fantasy"
            | "system-ui"
            | "ui-serif"
            | "ui-sans-serif"
            | "ui-monospace"
            | "ui-rounded"
            | "emoji"
            | "math"
            | "fangsong"
    )
}

/// 是否为可无引号输出的 CSS 自定义标识符（`<custom-ident>`）。
/// 排除 CSS 全局关键字，避免被解释为关键字而非族名。
fn is_css_ident(s: &str) -> bool {
    !s.is_empty()
        && !matches!(
            s,
            "inherit" | "initial" | "unset" | "revert" | "revert-layer" | "default"
        )
        && s.chars().enumerate().all(|(i, c)| {
            if i == 0 {
                c.is_ascii_alphabetic() || c == '_' || c == '-'
            } else {
                c.is_ascii_alphanumeric() || c == '_' || c == '-'
            }
        })
}

// ---------------------------------------------------------------------------
// 字形级排版结果（自闭环，可脱离 parley 独立消费）
// ---------------------------------------------------------------------------

/// 文本修饰（下划线 / 删除线）。pango 的 `underline` / `strikethrough` 属性。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TextDecoration {
    /// 无修饰。
    #[default]
    None,
    /// 下划线。
    Underline,
    /// 删除线。
    LineThrough,
}

/// 单个字形（与 parley 的 positioned glyph 一一对应）。
#[derive(Debug, Clone, PartialEq)]
pub struct Glyph {
    /// 字形 ID。
    pub id: u32,
    /// X 坐标（相对所属 [`TextLine::bounds`] 原点的偏移）。
    pub x: f32,
    /// Y 坐标（相对所属 [`TextLine::bounds`] 原点的偏移）。
    pub y: f32,
    /// 前进宽度。
    pub advance: f32,
    /// 字节簇偏移（相对本 run 自身的 `text`，即 `text[cluster..]`）。
    pub cluster: u32,
}

/// 文本 Run —— 一段具有相同样式（字体、字号、颜色）的连续字形序列。
///
/// 文本渲染的最小单元。Run 自闭环：`text` 为相对原始 `full_text` 的局部切片，
/// `glyphs[].cluster` 已归一化为相对 `text` 的局部偏移，脱离所属 Line 与原始
/// 字符串即可独立绘制。字体以 `Vec<u8>` 自包含（`Arc` 共享，不复制整份字体文件）。
///
/// 扩展属性（`url` / `decoration` / `background_color` / `baseline_shift`）由
/// lievisual 从 [`RichSpan`] 的样式（[`TextStyle`]）按字节区间映射到 run 携带，
/// 下游可直接消费，无需二次标注。
#[derive(Debug, Clone)]
pub struct TextRun {
    /// 该 Run 的文本内容（自包含，相对原始 full_text 的局部切片）。
    pub text: String,
    /// 字体原始字节（解析自 parley FontData 的 blob，自包含，`Arc` 共享）。
    pub font_data: Arc<Vec<u8>>,
    /// 字体在字体文件集合（ttc / otc）中的索引。单字体文件恒为 0。
    ///
    /// 下游（如 PDF 嵌入字体）用它从 `font_data` 定位到具体的字体实例。
    pub font_index: u32,
    /// 字体大小。
    pub font_size: f32,
    /// 字体是否粗体（SVG 等用系统字体的后端据此输出 font-weight）。
    pub font_weight_bold: bool,
    /// 字体是否斜体（SVG 等后端据此输出 font-style）。
    pub font_style_italic: bool,
    /// 文本颜色。
    pub color: Color,
    /// 总前进宽度。
    pub advance: f32,
    /// 字形列表（坐标相对 [`TextLine::bounds`] 原点偏移）。
    pub glyphs: Vec<Glyph>,
    /// 是否从右到左。
    pub is_rtl: bool,
    /// 该 Run 第一个字符的基线 X 坐标（相对 layout 原点）。
    pub baseline_x: f32,
    /// 该行的基线 Y 坐标（相对行顶的偏移；同一行内所有 run 共享此值）。
    pub baseline_y: f32,
    /// 超链接 URL（如有）。
    pub url: Option<String>,
    /// 文本修饰（下划线 / 删除线）。
    pub decoration: TextDecoration,
    /// 基线偏移（使上下标相对行内位置上下移动）。
    pub baseline_shift: f32,
    /// 行内背景色（行内代码 / 高亮，`None` 无背景）。
    pub background_color: Option<Color>,
}

/// 行内字体度量（来自 parley `LineMetrics`）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LineMetrics {
    /// 上肩高（baseline 向上）。
    pub ascent: f32,
    /// 下伸量（baseline 向下）。
    pub descent: f32,
    /// 基线距行顶的距离。
    pub baseline: f32,
    /// 行高。
    pub line_height: f32,
}

/// 文本行 —— 包含一行中的所有 [`TextRun`]。
///
/// 坐标系统（相对 layout 原点）：
/// - `bounds.origin`: 行在 layout 中的位置。
/// - `runs[].glyphs[].x/y`: 相对 `bounds.origin` 的偏移。
///
/// 绝对定位时：glyph 坐标 = 页面偏移 + bounds.origin + glyph。
#[derive(Debug, Clone)]
pub struct TextLine {
    /// 该行的所有 Run。
    pub runs: Vec<TextRun>,
    /// 行的边界框（相对 layout 原点）。
    pub bounds: Rect,
    /// 行内字体度量（ascent / descent / baseline / line_height）。
    pub metrics: LineMetrics,
    /// 该行实际字形的 ink 边界（轮廓 bbox，相对 layout 原点），用于精准垂直居中。
    pub ink_bounds: Rect,
}

/// 富文本排版结果 —— 包含排版后的行集合。
///
/// 由 [`layout_text`] / [`measure_text`] 生成，**自闭环**：不依赖 parley 的
/// 生命周期，`font_data` 内嵌字体字节、`glyphs[].cluster` 为相对 run 文本的
/// 局部偏移。下游（如 liepress 的 PDF / SVG 后端）可直接遍历 `lines` 绘制，
/// 无需再触达 parley。
///
/// 坐标全部相对 layout 原点（段落左上角）；分页与绝对定位由输出后端负责。
#[derive(Debug, Clone)]
pub struct TextLayout {
    /// 排版后的所有行。
    pub lines: Vec<TextLine>,
    /// 布局总宽度。
    pub width: f64,
    /// 布局总高度。
    pub height: f64,
    /// 全部行的实际字形 ink 边界（轮廓 bbox，相对 layout 原点），用于精准垂直居中。
    pub ink_bounds: Rect,
}

impl TextLayout {
    /// 实际字形的 ink 边界（轮廓 bbox，相对 layout 原点）。
    ///
    /// 由排版时在 [`layout_text`] / [`measure_text`] 中基于 skrifa 逐字形轮廓计算，
    /// 比 parley 的 `LineMetrics`（含 ascent/descent 设计余量）更贴近真实视觉边界，
    /// 用于文本垂直居中（`visual_height` 等）。
    pub fn ink_bounds(&self) -> Rect {
        self.ink_bounds
    }
}

pub use measure::{
    FontSource, compute_text_offset, layout_metrics, layout_text, measure_text,
    parse_generic_family, register_font, register_font_generic,
};

/// 文本测量结果（canvas `measureText()` 返回值）。
///
/// 坐标语义（与 Canvas `TextMetrics` 一致，y-down 下量取正值）：
/// - `*ascent` / `*descent` 相对**基线**（基线向上为正、向下为正）；
/// - `*baseline` 相对**文本块顶部**（向下为正）；
/// - `actual_bounding_box_*` 为近似值（多行时以首行度量近似）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextMetrics {
    /// 文本块宽度（canvas `width`）。
    pub width: f64,
    /// 文本块高度（canvas 无直接字段；等于 `layout.height()`）。
    pub height: f64,
    /// 从锚点（左边缘）到最左字形边界的距离，≤ 0。
    pub actual_bounding_box_left: f64,
    /// 从锚点（左边缘）到最右字形边界的距离。
    pub actual_bounding_box_right: f64,
    /// 字体盒上边界到基线的高度（正，向上）。
    pub font_bounding_box_ascent: f64,
    /// 基线到字体盒下边界的高度（正，向下）。
    pub font_bounding_box_descent: f64,
    /// 实际字形上边界到基线（近似 = 首行 ascent）。
    pub actual_bounding_box_ascent: f64,
    /// 基线到实际字形下边界（近似 = 首行 descent）。
    pub actual_bounding_box_descent: f64,
    /// em 盒上边界到基线（= 首行 ascent）。
    pub em_height_ascent: f64,
    /// 基线到 em 盒下边界（= 首行 descent）。
    pub em_height_descent: f64,
    /// 文本块顶部到 hanging baseline。
    pub hanging_baseline: f64,
    /// 文本块顶部到 alphabetic 基线。
    pub alphabetic_baseline: f64,
    /// 文本块顶部到 ideographic 基线。
    pub ideographic_baseline: f64,
    /// 单行行高。
    pub line_height: f64,
}

/// 测量结果：尺寸 + 度量 + 可复用的排版（避免绘制时重复排版）。
pub struct TextMeasure {
    /// 文本块尺寸（`width` / `height`）。
    pub size: Size,
    /// 详细度量（canvas `TextMetrics`）。
    pub metrics: TextMetrics,
    /// 预排版结果，可喂给 [`crate::scene::Element::Text::layout`] 精确绘字形。
    pub layout: Arc<TextLayout>,
}

mod measure {
    use super::*;
    use parley::style::FontFamily;
    use parley::{FontContext, LayoutContext, StyleProperty};
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex, OnceLock};

    // 跨排版共享的字体字节缓存：key 为 parley `Blob` 的稳定 `id()`（每种字体唯一）。
    // 避免每个 Run 都 `to_vec` 整份字体文件（可能数十 MB）导致内存随文本规模爆炸。
    // 每种唯一字体在整个进程生命周期内只复制一次字节，其余 Run 通过 `Arc` 共享。
    static FONT_BYTE_CACHE: OnceLock<Mutex<HashMap<u64, Arc<Vec<u8>>>>> = OnceLock::new();

    // 每个线程一个缓存的排版上下文：懒初始化，线程内所有测量/排版统一复用。
    // parley 官方将 FontContext（字体库 + LRU 源缓存）与 LayoutContext（scratch + 字形缓存）
    // 设计为"每应用/每线程一个"；thread_local 落地该语义，避免每次新建导致的系统字体
    // 扫描与分配。所有 parley 相关操作都经 with_cached_context 走这同一对实例；
    // 线程间互不干扰（各线程独立实例，无需同步）。
    thread_local! {
        static FONT_CONTEXT: RefCell<FontContext> = RefCell::new(FontContext::new());
        static LAYOUT_CONTEXT: RefCell<LayoutContext<Color>> = RefCell::new(LayoutContext::new());
    }

    /// 借出线程本地缓存上下文执行一次排版（所有 parley 操作的统一入口）。
    fn with_cached_context<R>(
        f: impl FnOnce(&mut FontContext, &mut LayoutContext<Color>) -> R,
    ) -> R {
        FONT_CONTEXT.with(|fc| {
            LAYOUT_CONTEXT.with(|lc| {
                let mut fc = fc.borrow_mut();
                let mut lc = lc.borrow_mut();
                f(&mut fc, &mut lc)
            })
        })
    }

    /// 测量富文本（[`RichSpan`] 列表，canvas `measureText`），返回尺寸/度量/可复用排版。
    ///
    /// 复用线程本地缓存的 `FontContext` / `LayoutContext`（parley 内部缓存字体、
    /// 字形与 scratch 分配），避免每次调用重建导致的系统字体扫描。`max_width` 作为
    /// 整段换行上限。单文本是单个 [`RichSpan`] 的特例。
    pub fn measure_text(spans: &[RichSpan], max_width: Option<f64>) -> TextMeasure {
        with_cached_context(|fc, lc| {
            let layout = build_rich_layout(spans, max_width, fc, lc);
            let metrics = layout_metrics(&layout);
            TextMeasure {
                size: Size::new(metrics.width, metrics.height),
                metrics,
                layout: Arc::new(layout),
            }
        })
    }

    /// 排版富文本（[`RichSpan`] 列表）为单一 [`TextLayout`]，返回 `Arc`（可作
    /// [`crate::scene::Element::Text`] 的预排版 `layout` 直接喂给渲染端）。
    ///
    /// 与 [`measure_text`] 共享同一 TLS 上下文；供"只要 layout"的场景复用，避免重复排版。
    pub fn layout_text(spans: &[RichSpan], max_width: Option<f64>) -> Arc<TextLayout> {
        with_cached_context(|fc, lc| Arc::new(build_rich_layout(spans, max_width, fc, lc)))
    }

    /// 用 `RangedBuilder` 构造富文本布局（测量与渲染共享，保证尺寸一致），
    /// 并提取为自闭环 [`TextLayout`]（含字形级 run / glyph 与扩展属性）。
    fn build_rich_layout(
        spans: &[RichSpan],
        max_width: Option<f64>,
        font_cx: &mut FontContext,
        layout_cx: &mut LayoutContext<Color>,
    ) -> TextLayout {
        if spans.is_empty() {
            let empty = TextStyle::new(Color::BLACK, 12.0, "sans-serif");
            return build_layout(font_cx, layout_cx, "", &empty, max_width);
        }

        // 拼接所有文本并记录每段字节区间。
        let mut combined = String::new();
        let mut ranges: Vec<(usize, usize)> = Vec::with_capacity(spans.len());
        for span in spans {
            let start = combined.len();
            combined.push_str(&span.text);
            ranges.push((start, combined.len()));
        }

        let mut builder = layout_cx.ranged_builder(font_cx, &combined, 1.0, true);

        // 以首段样式为默认值，其余段按需覆盖不同属性。
        let base = &spans[0].style;
        push_text_style_defaults(&mut builder, base);

        for (i, span) in spans.iter().enumerate().skip(1) {
            let (start, end) = ranges[i];
            if start >= end {
                continue;
            }
            push_style_diff(&mut builder, &span.style, base, start..end);
        }

        let mut layout = builder.build(&combined);
        layout.break_all_lines(max_width.map(|w| w as f32));
        // 按首段 align 应用 parley 对齐：Center/Right 让多行在块内对齐，
        // Justify 排版时调整字距。渲染端 dx 只负责块整体相对锚点定位，两者正交。
        apply_parley_align(&mut layout, spans[0].style.align);

        // 提取为自闭环结构，扩展属性（url/decoration/background/baseline_shift）
        // 按 span 字节区间映射到每个 run。
        extract_lines_from_parley(&layout, &combined, spans)
    }

    /// 解析 CSS `font-family` 列表为 parley 的 `FontFamily`（与 [`build_layout`] 一致）。
    fn font_family_prop(raw: &str) -> FontFamily<'_> {
        use parley::style::FontFamilyName;
        let families: Vec<FontFamilyName> = FontFamilyName::parse_css_list(raw)
            .filter_map(Result::ok)
            .collect();
        if families.is_empty() {
            FontFamily::named(raw)
        } else {
            FontFamily::List(families.into())
        }
    }

    fn parley_font_style(style: FontStyle) -> parley::style::FontStyle {
        match style {
            FontStyle::Normal => parley::style::FontStyle::Normal,
            FontStyle::Italic => parley::style::FontStyle::Italic,
            FontStyle::Oblique => parley::style::FontStyle::Oblique(None),
        }
    }

    fn parley_line_height(lh: f64, font_size: f64) -> parley::style::LineHeight {
        let factor = (lh / font_size.max(1e-6)) as f32;
        parley::style::LineHeight::FontSizeRelative(factor)
    }

    /// 按对齐应用 parley 段落对齐（Center/Right 让多行在块内对齐，Justify 调整字距）。
    ///
    /// 与渲染端锚点偏移正交：parley 负责**块内**各行对齐，渲染端 dx 负责**块整体**
    /// 相对锚点定位。
    fn apply_parley_align(layout: &mut parley::Layout<Color>, align: TextAlign) {
        let p = match align {
            TextAlign::Left => parley::Alignment::Start,
            TextAlign::Center => parley::Alignment::Center,
            TextAlign::Right => parley::Alignment::End,
            TextAlign::Justify => parley::Alignment::Justify,
        };
        layout.align(p, parley::AlignmentOptions::default());
    }

    /// 将 `TextStyle` 的排版属性 push 为 builder 的默认值（`build_layout` 与
    /// `build_rich_layout` 共享，保证单文本与富文本映射一致）。
    ///
    /// 覆盖：字体族 / 字号 / 字宽 / 字重 / 风格 / 颜色 / 行高 / 字间距 / 下划线 /
    /// 删除线。`line_height` 为 `None` 时不设置（用字体默认）。
    fn push_text_style_defaults(builder: &mut parley::RangedBuilder<'_, Color>, style: &TextStyle) {
        builder.push_default(StyleProperty::FontFamily(font_family_prop(
            &style.font_family,
        )));
        builder.push_default(StyleProperty::FontSize(style.font_size as f32));
        builder.push_default(StyleProperty::Brush(style.color));
        builder.push_default(StyleProperty::FontWeight(parley::style::FontWeight::new(
            style.font_weight,
        )));
        builder.push_default(StyleProperty::FontStyle(parley_font_style(
            style.font_style,
        )));
        if let Some(w) = style.font_width {
            builder.push_default(StyleProperty::FontWidth(
                parley::style::FontWidth::from_ratio(w),
            ));
        }
        if let Some(lh) = style.line_height {
            builder.push_default(StyleProperty::LineHeight(parley_line_height(
                lh,
                style.font_size,
            )));
        }
        if style.letter_spacing != 0.0 {
            builder.push_default(StyleProperty::LetterSpacing(style.letter_spacing as f32));
        }
        builder.push_default(StyleProperty::Underline(style.underline));
        builder.push_default(StyleProperty::UnderlineBrush(style.underline_color));
        builder.push_default(StyleProperty::Strikethrough(style.strikethrough));
        builder.push_default(StyleProperty::StrikethroughBrush(style.strikethrough_color));
    }

    /// 将 `TextStyle` 中与 `base` 不同的排版属性按区间覆盖 push（富文本逐 span）。
    fn push_style_diff(
        builder: &mut parley::RangedBuilder<'_, Color>,
        s: &TextStyle,
        base: &TextStyle,
        range: std::ops::Range<usize>,
    ) {
        if s.font_family != base.font_family {
            builder.push(
                StyleProperty::FontFamily(font_family_prop(&s.font_family)),
                range.clone(),
            );
        }
        if (s.font_size - base.font_size).abs() > f64::EPSILON {
            builder.push(StyleProperty::FontSize(s.font_size as f32), range.clone());
        }
        if s.color != base.color {
            builder.push(StyleProperty::Brush(s.color), range.clone());
        }
        if (s.font_weight - base.font_weight).abs() > f32::EPSILON {
            builder.push(
                StyleProperty::FontWeight(parley::style::FontWeight::new(s.font_weight)),
                range.clone(),
            );
        }
        if s.font_style != base.font_style {
            builder.push(
                StyleProperty::FontStyle(parley_font_style(s.font_style)),
                range.clone(),
            );
        }
        if s.font_width != base.font_width {
            if let Some(w) = s.font_width {
                builder.push(
                    StyleProperty::FontWidth(parley::style::FontWidth::from_ratio(w)),
                    range.clone(),
                );
            } else {
                // 显式回退到默认常规字宽。
                builder.push(
                    StyleProperty::FontWidth(parley::style::FontWidth::from_ratio(1.0)),
                    range.clone(),
                );
            }
        }
        if s.line_height != base.line_height {
            builder.push(
                StyleProperty::LineHeight(parley_line_height(
                    s.line_height.unwrap_or(s.font_size),
                    s.font_size,
                )),
                range.clone(),
            );
        }
        if (s.letter_spacing - base.letter_spacing).abs() > f64::EPSILON {
            builder.push(
                StyleProperty::LetterSpacing(s.letter_spacing as f32),
                range.clone(),
            );
        }
        if s.underline != base.underline {
            builder.push(StyleProperty::Underline(s.underline), range.clone());
        }
        if s.underline_color != base.underline_color {
            builder.push(
                StyleProperty::UnderlineBrush(s.underline_color),
                range.clone(),
            );
        }
        if s.strikethrough != base.strikethrough {
            builder.push(StyleProperty::Strikethrough(s.strikethrough), range.clone());
        }
        if s.strikethrough_color != base.strikethrough_color {
            builder.push(
                StyleProperty::StrikethroughBrush(s.strikethrough_color),
                range.clone(),
            );
        }
    }

    /// 从已排版 layout 提取度量（canvas `TextMetrics`）。
    pub fn layout_metrics(layout: &TextLayout) -> TextMetrics {
        let width = layout.width;
        let height = layout.height;
        let (ascent, descent, baseline, line_height) = match layout.lines.first() {
            Some(line) => {
                let m = line.metrics;
                (
                    m.ascent as f64,
                    m.descent as f64,
                    m.baseline as f64,
                    m.line_height as f64,
                )
            }
            None => (0.0, 0.0, 0.0, 0.0),
        };
        TextMetrics {
            width,
            height,
            actual_bounding_box_left: 0.0,
            actual_bounding_box_right: width,
            font_bounding_box_ascent: ascent,
            font_bounding_box_descent: descent,
            actual_bounding_box_ascent: ascent,
            actual_bounding_box_descent: descent,
            em_height_ascent: ascent,
            em_height_descent: descent,
            hanging_baseline: ascent * 0.8,
            alphabetic_baseline: baseline,
            ideographic_baseline: baseline + descent * 0.1,
            line_height,
        }
    }

    /// 构造 parley 布局（测量与渲染共享，保证尺寸一致），提取为自闭环 [`TextLayout`]。
    pub(crate) fn build_layout(
        font_ctx: &mut FontContext,
        layout_ctx: &mut LayoutContext<Color>,
        content: &str,
        style: &TextStyle,
        max_width: Option<f64>,
    ) -> TextLayout {
        // scale = 1.0（显示缩放）；字号由 FontSize 属性指定，保证 layout 坐标为逻辑像素。
        let mut builder = layout_ctx.ranged_builder(font_ctx, content, 1.0, true);
        push_text_style_defaults(&mut builder, style);

        let mut layout = builder.build(content);
        // 最大宽度作为换行上限；无限制时传 None。
        layout.break_all_lines(max_width.map(|w| w as f32));
        // 按 align 应用 parley 对齐：Center/Right 让多行在块内对齐，
        // Justify 排版时调整字距。渲染端 dx 只负责块整体相对锚点定位，两者正交。
        apply_parley_align(&mut layout, style.align);

        // 单文本视为单个 span，提取为自闭环结构。
        let span = RichSpan::new(content.to_string(), style.clone());
        let spans = std::slice::from_ref(&span);
        extract_lines_from_parley(&layout, content, spans)
    }

    /// 字形原始数据（相对 layout 原点的坐标），仅在提取过程中使用。
    struct GlyphRaw {
        id: u32,
        x: f32,
        y: f32,
        advance: f32,
        cluster: u32,
    }

    /// 从 parley Layout 提取自闭环 [`TextLayout`]，扩展属性（`url` / `decoration` /
    /// `background_color` / `baseline_shift`）按 span 字节区间映射到每个 run。
    ///
    /// 每个 [`TextLine::bounds`] 表示该行在 layout 中的位置：`x` = 最左字形相对
    /// layout 左侧偏移，`y` = 行顶相对 layout 顶部的累积偏移。run 的 `glyphs[].x/y`
    /// 为相对行原点的偏移（自闭环，可脱离 layout / full_text 独立绘制）。
    ///
    /// 字体字节经进程级缓存按 `Blob::id()` 去重，每种唯一字体只 `to_vec` 一次存入
    /// `Arc`，其余 run 共享，避免每个 run 复制整份字体文件导致内存爆炸。
    fn extract_lines_from_parley(
        layout: &parley::Layout<Color>,
        full_text: &str,
        spans: &[RichSpan],
    ) -> TextLayout {
        // 构建 span 字节区间表，用于扩展属性（url/decoration/background/baseline_shift）查找。
        let mut span_ranges: Vec<(usize, usize, &TextStyle)> = Vec::with_capacity(spans.len());
        let mut acc = 0usize;
        for span in spans {
            let end = acc + span.text.len();
            span_ranges.push((acc, end, &span.style));
            acc = end;
        }

        let mut lines = Vec::new();
        let mut row_top_rel = 0.0_f32;

        for line in layout.lines() {
            let m = line.metrics();
            let line_metrics = LineMetrics {
                ascent: m.ascent,
                descent: m.descent,
                baseline: m.baseline,
                line_height: m.line_height,
            };
            let line_height = m.line_height;
            let baseline_y = m.baseline - row_top_rel;

            let mut glyph_data: Vec<(GlyphRaw, usize)> = Vec::new();
            #[allow(clippy::type_complexity)]
            let mut run_infos: Vec<(
                Color,
                Arc<Vec<u8>>,
                u32,
                f32,
                parley::layout::Run<'_, Color>,
                f32,
            )> = Vec::new();

            let mut next_run_idx = 0;
            for item in line.items() {
                if let parley::layout::PositionedLayoutItem::GlyphRun(glyph_run) = item {
                    let run = glyph_run.run();
                    let color = glyph_run.style().brush;
                    // 字体字节 + 集合索引：字节按 Blob::id() 去重缓存。
                    let font = run.font();
                    let blob = &font.data;
                    let font_id = blob.id();
                    let font_index = font.index;
                    let font_arc = {
                        let mut cache = FONT_BYTE_CACHE
                            .get_or_init(|| Mutex::new(HashMap::new()))
                            .lock()
                            .expect("font cache poisoned");
                        cache
                            .entry(font_id)
                            .or_insert_with(|| Arc::new(blob.data().to_vec()))
                            .clone()
                    };
                    let font_size = run.font_size();

                    let first_glyph_x = glyph_run
                        .positioned_glyphs()
                        .next()
                        .map(|g| g.x)
                        .unwrap_or(0.0);

                    run_infos.push((color, font_arc, font_index, font_size, *run, first_glyph_x));

                    // 收集已定位字形，cluster 经 visual_clusters 的 text_range 提供。
                    let positioned: Vec<_> = glyph_run.positioned_glyphs().collect();
                    let mut gi = 0usize;
                    for cluster in glyph_run.run().visual_clusters() {
                        let cluster_byte = cluster.text_range().start as u32;
                        let cluster_glyphs: Vec<_> = cluster.glyphs().collect();
                        for _cg in &cluster_glyphs {
                            if gi < positioned.len() {
                                let g = &positioned[gi];
                                glyph_data.push((
                                    GlyphRaw {
                                        id: g.id,
                                        x: g.x,
                                        y: g.y,
                                        advance: g.advance,
                                        cluster: cluster_byte,
                                    },
                                    next_run_idx,
                                ));
                                gi += 1;
                            }
                        }
                    }
                    next_run_idx += 1;
                }
            }

            if glyph_data.is_empty() {
                row_top_rel += line_height;
                continue;
            }

            let min_x = glyph_data
                .iter()
                .map(|(g, _)| g.x)
                .fold(f32::INFINITY, f32::min);
            let max_x = glyph_data
                .iter()
                .map(|(g, _)| g.x)
                .fold(f32::NEG_INFINITY, f32::max);
            let width = max_x - min_x;

            let mut runs = Vec::new();

            // 按 run 索引切分字形。
            let mut run_glyph_ranges: Vec<(usize, usize)> = Vec::new();
            let mut current_start = 0;
            let mut last_run_idx = glyph_data[0].1;
            for (i, (_, run_idx)) in glyph_data.iter().enumerate() {
                if *run_idx != last_run_idx {
                    run_glyph_ranges.push((current_start, i));
                    current_start = i;
                    last_run_idx = *run_idx;
                }
            }
            run_glyph_ranges.push((current_start, glyph_data.len()));

            for (start_idx, end_idx) in run_glyph_ranges.iter() {
                let run_idx = glyph_data[*start_idx].1;
                let (color, font_data, font_index, font_size, run, first_glyph_x) =
                    &run_infos[run_idx];

                let run_text_range = run.text_range();
                let text_start = run_text_range.start;
                let text_end = run_text_range.end;

                let run_text = full_text
                    .get(text_start..text_end)
                    .map(str::to_string)
                    .unwrap_or_default();

                // 归一化 cluster 为相对 run_text 的局部偏移（自闭环）。
                let relative_glyphs: Vec<Glyph> = glyph_data[*start_idx..*end_idx]
                    .iter()
                    .map(|(g, _)| Glyph {
                        id: g.id,
                        x: g.x - min_x,
                        y: g.y - row_top_rel,
                        advance: g.advance,
                        cluster: g.cluster.saturating_sub(text_start as u32),
                    })
                    .collect();

                let baseline_x = *first_glyph_x - min_x;

                // 扩展属性：从 spans 按字节区间查找（传 run 的 [start, end)，
                // 以命中横跨多个 span 的合并 run 中带 url/背景的 span）。
                let (url, decoration, background_color, baseline_shift) =
                    lookup_extended_props(text_start, text_end, &span_ranges);

                // 基线偏移调整 glyph y：shift>0（上标）→ y 增大 → 视觉上移（y-down）。
                let adjusted_glyphs: Vec<Glyph> = relative_glyphs
                    .iter()
                    .map(|g| Glyph {
                        id: g.id,
                        x: g.x,
                        y: g.y + baseline_shift,
                        advance: g.advance,
                        cluster: g.cluster,
                    })
                    .collect();
                let adjusted_baseline_y = baseline_y - baseline_shift;
                let advance = adjusted_glyphs.iter().map(|g| g.advance).sum();

                // 字体粗细 / 斜体（SVG 等用系统字体后端需要）。
                let font_attrs = run.font_attrs();
                let font_weight_bold = font_attrs.weight >= parley::fontique::FontWeight::BOLD;
                let font_style_italic = font_attrs.style != parley::fontique::FontStyle::Normal;

                runs.push(TextRun {
                    text: run_text,
                    font_data: font_data.clone(),
                    font_index: *font_index,
                    font_size: *font_size,
                    font_weight_bold,
                    font_style_italic,
                    color: *color,
                    advance,
                    glyphs: adjusted_glyphs,
                    is_rtl: false,
                    baseline_x,
                    baseline_y: adjusted_baseline_y,
                    url,
                    decoration,
                    baseline_shift,
                    background_color,
                });
            }

            let bounds = Rect::new(
                min_x as f64,
                row_top_rel as f64,
                (min_x + width) as f64,
                (row_top_rel + line_height) as f64,
            );

            // 行级视觉 ink 边界（相对 layout 原点）。
            // 顶 = 基线 - ascent；底 = 基线（忽略 descent 设计余量，节点文字无下伸部时最准）。
            // 横向取整行 bounds（含字间距与对齐），与红框宽度一致。
            let ascent = line_metrics.ascent as f64;
            let baseline = line_metrics.baseline as f64;
            let ink_bounds = Rect::new(
                bounds.min_x(),
                bounds.min_y() + baseline - ascent,
                bounds.max_x(),
                bounds.min_y() + baseline,
            );

            lines.push(TextLine {
                runs,
                bounds,
                metrics: line_metrics,
                ink_bounds,
            });

            row_top_rel += line_height;
        }

        let width = layout.width() as f64;
        let height = layout.height() as f64;
        // 聚合所有行的 ink 边界（相对 layout 原点）。
        let ink_bounds = lines
            .iter()
            .map(|l| l.ink_bounds)
            .fold(None, |acc: Option<Rect>, r| match acc {
                None => Some(r),
                Some(a) => Some(Rect::new(
                    a.min_x().min(r.min_x()),
                    a.min_y().min(r.min_y()),
                    a.max_x().max(r.max_x()),
                    a.max_y().max(r.max_y()),
                )),
            })
            .unwrap_or_else(|| Rect::new(0.0, 0.0, width, height));
        TextLayout {
            lines,
            width,
            height,
            ink_bounds,
        }
    }

    /// 查找字节区间 `[start, end)` 对应的扩展属性（url / decoration / background /
    /// baseline_shift）。
    ///
    /// 一个 run 可能横跨多个 span（CJK 优先后 parley 常把整行合并为一个 run），因此
    /// 用**区间重叠**匹配：优先取与 `[start, end)` 有重叠且带 url / 背景的 span，
    /// 否则回退到起点所在 span。这样「普通文本 + 行内链接」合并成单个 run 时，
    /// 链接 span 的 url 也能命中，而不只是 run 起点所在的 span。
    fn lookup_extended_props(
        start: usize,
        end: usize,
        span_ranges: &[(usize, usize, &TextStyle)],
    ) -> (Option<String>, TextDecoration, Option<Color>, f32) {
        let matched = span_ranges
            .iter()
            .find(|(s, e, st)| {
                *s < end && *e > start && (st.url.is_some() || st.background_color.is_some())
            })
            .or_else(|| {
                span_ranges
                    .iter()
                    .find(|(s, e, _)| *s <= start && start < *e)
            });
        match matched {
            Some((_, _, st)) => {
                let decoration = if st.underline && st.strikethrough {
                    TextDecoration::None // 冲突时按无处理（调用方自行用 span 级样式）
                } else if st.strikethrough {
                    TextDecoration::LineThrough
                } else if st.underline {
                    TextDecoration::Underline
                } else {
                    TextDecoration::None
                };
                (
                    st.url.clone(),
                    decoration,
                    st.background_color,
                    st.baseline_shift as f32,
                )
            }
            None => (None, TextDecoration::None, None, 0.0),
        }
    }

    /// 字体来源。
    #[derive(Debug, Clone)]
    pub enum FontSource {
        /// 从文件路径加载。
        Path(std::path::PathBuf),
        /// 从内存数据加载。
        Memory(Vec<u8>),
    }

    /// 注册自定义字体到全局字体上下文。
    ///
    /// 加载后的字体可通过 `font_family` 名称在文本样式中使用。
    /// 由于所有排版统一走本 crate 的线程本地上下文，注册一次即可被测量与渲染共享。
    pub fn register_font(
        source: FontSource,
        family_name_override: Option<&str>,
    ) -> Result<(), String> {
        register_font_generic(source, family_name_override, None)
    }

    /// 注册自定义字体，并可选关联到**通用字体族**（`GenericFamily`）。
    ///
    /// 与 [`register_font`] 相同，额外支持把字体挂到 `sans-serif` / `monospace` /
    /// `serif` 等通用族，使 `font-family: monospace` 时使用注册字体而非系统默认。
    /// `generic_family` 为 `None` 时与 [`register_font`] 等价。
    ///
    /// 通用族类型 [`parley::fontique::GenericFamily`] 经 [`crate::parley`] re-export 可见。
    pub fn register_font_generic(
        source: FontSource,
        family_name_override: Option<&str>,
        generic_family: Option<parley::fontique::GenericFamily>,
    ) -> Result<(), String> {
        use parley::fontique::Blob;
        use std::sync::Arc;

        let data = match source {
            FontSource::Path(path) => {
                let bytes =
                    std::fs::read(&path).map_err(|e| format!("Failed to read font file: {e}"))?;
                Blob::new(Arc::new(bytes))
            }
            FontSource::Memory(bytes) => Blob::new(Arc::new(bytes)),
        };

        let override_info = family_name_override.map(|name| parley::fontique::FontInfoOverride {
            family_name: Some(name),
            ..Default::default()
        });

        with_cached_context(|font_cx, _| {
            let registered = font_cx.collection.register_fonts(data, override_info);
            if let Some(generic) = generic_family {
                let ids: Vec<_> = registered.into_iter().map(|(id, _)| id).collect();
                font_cx
                    .collection
                    .append_generic_families(generic, ids.into_iter());
            }
        });
        Ok(())
    }

    /// 解析通用字体族名称（`"monospace"` / `"sans-serif"` 等）为 parley 的
    /// [`parley::fontique::GenericFamily`]。
    ///
    /// 供注册字体关联到通用族（见 [`register_font_generic`]）、以及把 `font-family`
    /// 中的通用族名归一化使用。无法识别时返回 `None`。
    #[must_use]
    pub fn parse_generic_family(name: &str) -> Option<parley::fontique::GenericFamily> {
        use parley::fontique::GenericFamily;
        match name.to_lowercase().as_str() {
            "serif" => Some(GenericFamily::Serif),
            "sans-serif" | "sansserif" => Some(GenericFamily::SansSerif),
            "monospace" => Some(GenericFamily::Monospace),
            "cursive" => Some(GenericFamily::Cursive),
            "fantasy" => Some(GenericFamily::Fantasy),
            "system-ui" | "systemui" => Some(GenericFamily::SystemUi),
            "ui-serif" | "uiserif" => Some(GenericFamily::UiSerif),
            "ui-sans-serif" | "uisansserif" => Some(GenericFamily::UiSansSerif),
            "ui-monospace" | "uimonospace" => Some(GenericFamily::UiMonospace),
            "ui-rounded" | "uirounded" => Some(GenericFamily::UiRounded),
            "emoji" => Some(GenericFamily::Emoji),
            "math" => Some(GenericFamily::Math),
            "fangsong" => Some(GenericFamily::FangSong),
            _ => None,
        }
    }

    /// 从锚点到文本块左上角的偏移量。
    ///
    /// 根据期望的对齐/基线方式，计算从锚点坐标到文本块左上角的偏移。
    /// 组件使用范式：先 [`layout_text`] 拿到 layout，再 [`compute_text_offset`]
    /// 计算偏移，将锚点加上偏移即为文本块左上角（绘制时 TextRun 用 Left/Top）。
    pub fn compute_text_offset(
        layout: &TextLayout,
        align: TextAlign,
        baseline: TextBaseline,
    ) -> (f64, f64) {
        let layout_width = layout.width;
        let layout_height = layout.height;

        let x_offset = match align {
            TextAlign::Left | TextAlign::Justify => 0.0,
            TextAlign::Center => -layout_width / 2.0,
            TextAlign::Right => -layout_width,
        };

        let y_offset = match baseline {
            TextBaseline::Top => 0.0,
            // 用 ink_bounds 的真实视觉高度居中，避免 parley layout.height 含 descent/leading
            // 余量导致的文字偏上。ink_bounds.min_y 相对 layout 原点（即 Top 渲染时的 position）。
            TextBaseline::Middle => {
                let ib = layout.ink_bounds();
                let vh = (ib.max_y() - ib.min_y()).max(0.0);
                if vh > 0.0 {
                    -(ib.min_y() + vh / 2.0)
                } else {
                    -layout_height / 2.0
                }
            }
            TextBaseline::Bottom => {
                let ib = layout.ink_bounds();
                let vh = (ib.max_y() - ib.min_y()).max(0.0);
                if vh > 0.0 {
                    -(ib.min_y() + vh)
                } else {
                    -layout_height
                }
            }
            TextBaseline::Alphabetic => {
                let first_line = layout.lines.first();
                if let Some(line) = first_line {
                    -line.metrics.ascent as f64
                } else {
                    -layout_height * 0.8
                }
            }
            // Hanging / Ideographic 需基于首行字体度量（y-down 下锚点在基线下方，故为负）。
            // 与 [`TextBaseline::anchor_offset`] / [`layout_metrics`] 约定保持一致：
            // Hanging ≈ 0.8 × 上肩高；Ideographic ≈ 基线下 0.1 × 下伸量。
            TextBaseline::Hanging => {
                let first_line = layout.lines.first();
                if let Some(line) = first_line {
                    -(line.metrics.ascent * 0.8) as f64
                } else {
                    -layout_height * 0.8
                }
            }
            TextBaseline::Ideographic => {
                let first_line = layout.lines.first();
                if let Some(line) = first_line {
                    let m = line.metrics;
                    -(m.baseline + m.descent * 0.1) as f64
                } else {
                    -layout_height * 0.8
                }
            }
        };

        (x_offset, y_offset)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::Color;

    /// 便捷：单文本测量（一个 span 的特例）。
    fn measure_one(text: &str, style: &TextStyle, max_width: Option<f64>) -> TextMeasure {
        measure_text(
            std::slice::from_ref(&RichSpan::new(text, style.clone())),
            max_width,
        )
    }

    /// 便捷：单文本排版。
    fn layout_one(text: &str, style: &TextStyle, max_width: Option<f64>) -> Arc<TextLayout> {
        layout_text(
            std::slice::from_ref(&RichSpan::new(text, style.clone())),
            max_width,
        )
    }

    /// 测量与度量：尺寸、基线偏移、TLS 上下文复用。
    mod measure {
        use super::*;

        #[test]
        fn measure_metrics_basic() {
            let style = TextStyle::new(Color::BLACK, 20.0, "sans-serif");
            let m = measure_one("Hi", &style, None);
            assert!(m.metrics.width > 0.0);
            assert!(m.metrics.height > 0.0);
            assert!(m.metrics.font_bounding_box_ascent > 0.0);
            assert!(m.metrics.font_bounding_box_descent >= 0.0);
            assert!(m.metrics.alphabetic_baseline > 0.0);
            assert_eq!(m.size.width, m.metrics.width);
            assert_eq!(m.size.height, m.metrics.height);
        }

        #[test]
        fn baseline_anchor_offsets_monotonic() {
            let style = TextStyle::new(Color::BLACK, 20.0, "sans-serif");
            let m = measure_one("X", &style, None).metrics;
            let top = TextBaseline::Top.anchor_offset(&m);
            let alpha = TextBaseline::Alphabetic.anchor_offset(&m);
            let bottom = TextBaseline::Bottom.anchor_offset(&m);
            assert_eq!(top, 0.0);
            assert!(top < alpha && alpha < bottom);
            assert!(bottom <= m.height + 1e-6);
        }

        #[test]
        fn measure_text_reuses_thread_local_context() {
            // 同一线程多次测量：走 thread-local 缓存，不 panic、结果稳定。
            let style = TextStyle::new(Color::BLACK, 16.0, "sans-serif");
            let a = measure_one("缓存复用", &style, None).metrics.width;
            let b = measure_one("缓存复用", &style, None).metrics.width;
            assert!((a - b).abs() < 1e-6);

            // 独立线程各自持有上下文，互不干扰。
            let handles: Vec<_> = (0..4)
                .map(|_| {
                    let style = style.clone();
                    std::thread::spawn(move || {
                        let m = measure_one("thread", &style, None);
                        assert!(m.metrics.width > 0.0);
                    })
                })
                .collect();
            for h in handles {
                h.join().expect("thread panicked");
            }
        }

        #[test]
        fn layout_text_uses_same_tls_context() {
            // 渲染端排版入口与测量共享同一 TLS 上下文：结果应一致。
            let style = TextStyle::new(Color::BLACK, 16.0, "sans-serif");
            let m = measure_one("共享", &style, None);
            let l = layout_one("共享", &style, None);
            assert_eq!(l.width, m.metrics.width);
            assert_eq!(l.height, m.metrics.height);
        }
    }

    /// 对齐偏移：水平对齐与垂直基线锚点。
    mod offset {
        use super::*;

        #[test]
        fn compute_text_offset_horizontal_alignment() {
            let style = TextStyle::new(Color::BLACK, 20.0, "sans-serif");
            let layout = layout_one("Hello", &style, None);
            let (x_left, _) = compute_text_offset(&layout, TextAlign::Left, TextBaseline::Top);
            let (x_center, _) = compute_text_offset(&layout, TextAlign::Center, TextBaseline::Top);
            let (x_right, _) = compute_text_offset(&layout, TextAlign::Right, TextBaseline::Top);
            assert_eq!(x_left, 0.0);
            // 居中 / 右对齐按 layout 宽度对称偏移。
            assert!((x_center + layout.width / 2.0).abs() < 1e-6);
            assert!((x_right + layout.width).abs() < 1e-6);
        }

        #[test]
        fn compute_text_offset_baselines_monotonic() {
            let style = TextStyle::new(Color::BLACK, 20.0, "sans-serif");
            let layout = layout_one("Yg", &style, None);
            // 垂直锚点自上而下：Top > Middle > Hanging > Alphabetic > Ideographic > Bottom。
            // y-down 下偏移取负，锚点越靠下偏移越小（越负），单调递减。
            let top = compute_text_offset(&layout, TextAlign::Left, TextBaseline::Top).1;
            let middle = compute_text_offset(&layout, TextAlign::Left, TextBaseline::Middle).1;
            let hanging = compute_text_offset(&layout, TextAlign::Left, TextBaseline::Hanging).1;
            let alpha = compute_text_offset(&layout, TextAlign::Left, TextBaseline::Alphabetic).1;
            let ideo = compute_text_offset(&layout, TextAlign::Left, TextBaseline::Ideographic).1;
            let bottom = compute_text_offset(&layout, TextAlign::Left, TextBaseline::Bottom).1;
            assert_eq!(top, 0.0);
            assert!(top > middle, "top({top}) > middle({middle})");
            assert!(middle > hanging, "middle({middle}) > hanging({hanging})");
            assert!(hanging > alpha, "hanging({hanging}) > alpha({alpha})");
            assert!(alpha > ideo, "alpha({alpha}) > ideo({ideo})");
            assert!(ideo > bottom, "ideo({ideo}) > bottom({bottom})");
        }

        #[test]
        fn justify_offsets_like_left_and_layouts_multiline() {
            // 锚点偏移：Justify 与 Left 相同（0，因为两端对齐由 parley 排版实现）。
            let style = TextStyle::new(Color::BLACK, 20.0, "sans-serif");
            let layout = layout_one("Hello", &style, None);
            let (xj, _) = compute_text_offset(&layout, TextAlign::Justify, TextBaseline::Top);
            let (xl, _) = compute_text_offset(&layout, TextAlign::Left, TextBaseline::Top);
            assert_eq!(xj, xl);

            // 多行文本 Justify 排版不 panic、产生多行（视觉两端对齐由 parley 保证）。
            let jstyle =
                TextStyle::new(Color::BLACK, 16.0, "sans-serif").with_align(TextAlign::Justify);
            let text = "The quick brown fox jumps over the lazy dog while padding enough to wrap.";
            let jl = layout_text(
                std::slice::from_ref(&RichSpan::new(text, jstyle.clone())),
                Some(160.0),
            );
            let lines: Vec<_> = jl.lines.iter().collect();
            assert!(lines.len() >= 2, "应产生多行");
            let single = layout_text(std::slice::from_ref(&RichSpan::new("a", jstyle)), None);
            assert!(jl.height > single.height);
        }
    }

    /// font-family CSS 规范化。
    mod font_css {
        use super::*;

        #[test]
        fn font_family_css_normalizes_list() {
            let style = TextStyle::new(Color::BLACK, 12.0, "Helvetica Neue, Arial, sans-serif");
            assert_eq!(
                style.font_family_css(),
                "'Helvetica Neue', Arial, sans-serif"
            );
        }

        #[test]
        fn font_family_css_generic_stays_unquoted() {
            for g in [
                "sans-serif",
                "serif",
                "monospace",
                "cursive",
                "system-ui",
                "ui-monospace",
            ] {
                let style = TextStyle::new(Color::BLACK, 12.0, g);
                assert_eq!(style.font_family_css(), g, "通用族 {g} 不应加引号");
            }
        }

        #[test]
        fn font_family_css_quotes_spaces_and_non_ascii() {
            // 含空格 → 单引号
            let s = TextStyle::new(Color::BLACK, 12.0, "Times New Roman");
            assert_eq!(s.font_family_css(), "'Times New Roman'");
            // 非 ASCII → 单引号
            let s = TextStyle::new(Color::BLACK, 12.0, "微软雅黑");
            assert_eq!(s.font_family_css(), "'微软雅黑'");
            // 单引号 → CSS 字符串转义
            let s = TextStyle::new(Color::BLACK, 12.0, "Rock'n'Roll");
            assert_eq!(s.font_family_css(), "'Rock\\'n\\'Roll'");
            // 反斜杠 → 转义
            let s = TextStyle::new(Color::BLACK, 12.0, r"a\b");
            assert_eq!(s.font_family_css(), r"'a\\b'");
        }

        #[test]
        fn font_family_css_strips_user_quotes_and_keywords() {
            // 用户已带引号 → 剥离后按规则重建
            let s = TextStyle::new(Color::BLACK, 12.0, "\"Helvetica Neue\"");
            assert_eq!(s.font_family_css(), "'Helvetica Neue'");
            // 全局关键字 → 加引号避免被当关键字
            let s = TextStyle::new(Color::BLACK, 12.0, "inherit");
            assert_eq!(s.font_family_css(), "'inherit'");
            // 幂等：已规范化的输入结果不变
            let s = TextStyle::new(Color::BLACK, 12.0, "'Helvetica Neue', Arial, sans-serif");
            assert_eq!(s.font_family_css(), "'Helvetica Neue', Arial, sans-serif");
            // 空项被过滤
            let s = TextStyle::new(Color::BLACK, 12.0, "Arial, , serif");
            assert_eq!(s.font_family_css(), "Arial, serif");
        }
    }

    /// 富文本排版 / 测量，以及调用方测量范式。
    mod rich_text {
        use super::*;

        #[test]
        fn rich_text_measures_and_layouts() {
            let spans = vec![
                RichSpan::new("Hello", TextStyle::new(Color::BLACK, 20.0, "sans-serif")),
                RichSpan::new(" World", TextStyle::new(Color::BLACK, 20.0, "sans-serif")),
            ];
            let m = measure_text(&spans, None);
            assert!(m.metrics.width > 0.0);
            assert!(m.metrics.height > 0.0);
            assert_eq!(m.size.width, m.metrics.width);
            // 与单个 span 测量尺寸一致（同 TLS 上下文）。
            let plain = measure_one("Hello World", &spans[0].style, None);
            assert!((m.metrics.width - plain.metrics.width).abs() < 1e-6);
        }

        #[test]
        fn rich_text_mixed_styles_apply_ranges() {
            // 大字号片段应使整体行高更高。
            let spans = vec![
                RichSpan::new("big", TextStyle::new(Color::BLACK, 40.0, "sans-serif")),
                RichSpan::new("small", TextStyle::new(Color::BLACK, 12.0, "sans-serif")),
            ];
            let m = measure_text(&spans, None);
            assert!(m.metrics.width > 0.0);
            // 40px 片段主导行高（首行度量取自混排后的整行字体盒）。
            assert!(m.metrics.font_bounding_box_ascent > 20.0);
            assert!(m.metrics.font_bounding_box_descent >= 0.0);
            // 单段 12px 对照：大字号片段应在整体上体现更大的行高。
            let small_only = measure_text(
                std::slice::from_ref(&RichSpan::new("small", spans[1].style.clone())),
                None,
            );
            assert!(
                m.metrics.height > small_only.metrics.height,
                "混合大字号行高应更高: {} vs {}",
                m.metrics.height,
                small_only.metrics.height
            );
        }

        #[test]
        fn rich_text_empty_and_newline() {
            // 空列表：返回空布局，不 panic。
            let empty = measure_text(&[], None);
            assert!(empty.metrics.height >= 0.0);
            // 含换行 → 多行布局高度大于单行。
            let single = measure_text(
                &[RichSpan::new(
                    "ab",
                    TextStyle::new(Color::BLACK, 16.0, "sans-serif"),
                )],
                None,
            );
            let multi = measure_text(
                &[RichSpan::new(
                    "a\nb",
                    TextStyle::new(Color::BLACK, 16.0, "sans-serif"),
                )],
                None,
            );
            assert!(
                multi.metrics.height > single.metrics.height,
                "含换行的富文本应更高: {} vs {}",
                multi.metrics.height,
                single.metrics.height
            );
        }

        #[test]
        fn rich_text_max_width_wraps() {
            let span = RichSpan::new(
                "aaaaaaaa aaaaaaaa",
                TextStyle::new(Color::BLACK, 16.0, "sans-serif"),
            );
            let narrow = measure_text(std::slice::from_ref(&span), Some(50.0));
            let wide = measure_text(std::slice::from_ref(&span), None);
            // 窄宽换行 → 高度增加（单词在 50px 处折断为多行）。
            assert!(
                narrow.metrics.height > wide.metrics.height,
                "max_width 换行后高度应更高: {} vs {}",
                narrow.metrics.height,
                wide.metrics.height
            );
        }

        /// 模拟调用方（如 liemermaid 的 builder/layout/measure.rs）的测量范式：
        /// `layout_text(spans, None)` → 读 `layout.width()/height()` →
        /// 经 `compute_text_offset` 定位，最终作为预排版 layout 喂给 `Element::Text`。
        ///
        /// 保证：这些 API 签名一致、语义统一；富文本测量返回的 `Arc<TextLayout>` 可直接复用。
        #[test]
        fn caller_pattern_create_layout_then_offset() {
            use crate::scene::Element;

            // 居中 + 垂直居中的节点标签，用 layout_text 测量。
            let style = TextStyle::new(Color::BLACK, 13.0, "sans-serif")
                .with_align(TextAlign::Center)
                .with_baseline(TextBaseline::Middle);
            let text = "Hello Node";
            let layout = layout_one(text, &style, None);
            let (x_off, y_off) =
                compute_text_offset(&layout, TextAlign::Center, TextBaseline::Middle);
            // 锚点 (cx, cy) + offset = 文本块左上角。
            let cx = 100.0;
            let cy = 50.0;
            let top_left_x = cx + x_off;
            let top_left_y = cy + y_off;
            // 居中：左上角 x = cx - 半宽；垂直居中：左上角 y = cy - 半高。
            assert!((top_left_x - (cx - layout.width / 2.0)).abs() < 1e-6);
            assert!((top_left_y - (cy - layout.height / 2.0)).abs() < 1e-6);

            // 富文本测量结果与单段同样式 layout 宽度一致，且 Arc<TextLayout> 可作 Element::Text.layout。
            let rich = measure_text(
                std::slice::from_ref(&RichSpan::new(text, style.clone())),
                None,
            );
            assert!((rich.layout.width - layout.width).abs() < 1e-6);
            let node = Element::Text {
                spans: vec![RichSpan::new(text, style.clone())],
                position: crate::geometry::Point::new(top_left_x, top_left_y),
                style,
                layout: Some(rich.layout),
            };
            match node {
                Element::Text { layout, .. } => assert!(layout.is_some()),
                _ => unreachable!(),
            }
        }
    }

    /// 装饰与间距：下划线 / 删除线 / 字间距 / 字宽。
    mod decoration {
        use super::*;

        #[test]
        fn letter_spacing_widens_measure() {
            let base = TextStyle::new(Color::BLACK, 20.0, "sans-serif");
            let spaced = base.clone().with_letter_spacing(8.0);
            let a = measure_one("Hello", &base, None).metrics.width;
            let b = measure_one("Hello", &spaced, None).metrics.width;
            // 加字间距后整体宽度应明显增大。
            assert!(
                b > a + 1.0,
                "letter_spacing 应增大宽度: base={a}, spaced={b}"
            );
        }

        // 临时跳过：该测试依赖于"系统存在带 `wdth` 变体轴的字体"，由 parley/fontique
        // 的 `FontWidth` 选择更窄的字体变体实现。当前测试环境（840 个系统字体）中
        // 没有任何覆盖拉丁字符、带 `wdth` 轴的字体（仅 `Noto Sans Sinhala * Condensed`
        // 为僧伽罗文专用），fontconfig 回退到普通 width，导致 normal 与 condensed 同宽。
        // 代码逻辑正确，待环境提供 variable font（如 Inter / Roboto Flex 含 wdth 轴）
        // 或显式注入 condensed 字体文件后再启用。
        #[test]
        #[ignore = "环境缺少带 wdth 变体轴的字体，FontWidth 无法选择更窄字体"]
        fn condensed_font_width_is_narrower() {
            let normal = TextStyle::new(Color::BLACK, 20.0, "sans-serif");
            let condensed = normal.clone().with_font_width(0.75);
            let a = measure_one("MMMM", &normal, None).metrics.width;
            let b = measure_one("MMMM", &condensed, None).metrics.width;
            // 窄字宽（condensed）应使字形更窄。
            assert!(b < a, "condensed(0.75) 应更窄: normal={a}, condensed={b}");
        }

        #[test]
        fn decorations_do_not_change_size_but_render_rule() {
            // 下划线 / 删除线不改变字形布局尺寸，只影响绘制。
            let plain = TextStyle::new(Color::BLACK, 20.0, "sans-serif");
            let underlined = plain
                .clone()
                .with_underline(true)
                .with_underline_color(Some(Color::rgb(0, 0, 0xff)));
            let struck = plain.clone().with_strikethrough(true);
            let w_plain = measure_one("Hi", &plain, None).metrics.width;
            let w_ul = measure_one("Hi", &underlined, None).metrics.width;
            let w_st = measure_one("Hi", &struck, None).metrics.width;
            assert!((w_plain - w_ul).abs() < 1e-6);
            assert!((w_plain - w_st).abs() < 1e-6);
        }

        #[test]
        fn builder_defaults() {
            let s = TextStyle::new(Color::BLACK, 12.0, "sans-serif");
            assert_eq!(s.font_width, None);
            assert_eq!(s.letter_spacing, 0.0);
            assert!(!s.underline);
            assert!(!s.strikethrough);
            assert_eq!(s.underline_color, None);
            assert_eq!(s.strikethrough_color, None);
            assert_eq!(s.baseline_shift, 0.0);
            assert_eq!(s.background_color, None);
        }

        #[test]
        fn baseline_shift_and_background_builders() {
            let s = TextStyle::new(Color::BLACK, 12.0, "sans-serif")
                .with_baseline_shift(6.0)
                .with_background_color(Some(Color::rgb(0xf0, 0xf0, 0xf0)));
            assert_eq!(s.baseline_shift, 6.0);
            assert_eq!(s.background_color, Some(Color::rgb(0xf0, 0xf0, 0xf0)));
            // 负偏移 = 下标下移。
            let sub = TextStyle::new(Color::BLACK, 12.0, "sans-serif").with_baseline_shift(-4.0);
            assert_eq!(sub.baseline_shift, -4.0);
        }

        #[test]
        fn rich_text_mixed_decorations_apply_per_span() {
            // 富文本中：仅部分 span 加字间距 → 整体宽度介于"全加"与"全不加"之间，
            // 证明 span 级差异经 push_style_diff 按区间正确覆盖。
            let plain = TextStyle::new(Color::BLACK, 20.0, "sans-serif");
            let spaced = plain.clone().with_letter_spacing(6.0);

            let all_plain = measure_one("AB", &plain, None).metrics.width;
            let all_spaced = measure_one("AB", &spaced, None).metrics.width;
            assert!(all_spaced > all_plain, "全加间距应更宽");

            let mixed = measure_text(
                &[
                    RichSpan::new("A", plain.clone()),
                    RichSpan::new("B", spaced),
                ],
                None,
            )
            .metrics
            .width;
            assert!(
                mixed > all_plain && mixed < all_spaced,
                "混排宽度应介于之间: all_plain={all_plain}, mixed={mixed}, all_spaced={all_spaced}"
            );
        }
    }
}
