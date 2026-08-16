//! 文本排版支持（参考 Web Canvas 文本接口）。
//!
//! 对齐 Web Canvas 的 `CanvasRenderingContext2D` 文本 API：
//! - [`TextStyle`] 对应 `ctx.font`（`font-style` / `font-weight` / `font-size` /
//!   `line-height` / `font-family`）+ `textAlign` + `textBaseline`。
//! - [`TextAlign`] 对应 `ctx.textAlign`：`position.x` 作为水平锚点。
//! - [`TextBaseline`] 对应 `ctx.textBaseline`：`position.y` 作为垂直锚点。
//! - [`measure_text`] 对应 `ctx.measureText()`，返回 [`TextMeasure`]（含
//!   [`TextMetrics`]，即 canvas 的 `TextMetrics` 对象）。
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

use crate::geometry::{Color, Size};
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
    /// 行高（px）。`None` 使用字体默认行高（canvas `line-height` 省略时）。
    pub line_height: Option<f64>,
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
            line_height: None,
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

pub type TextLayout = parley::Layout<Color>; // brush = lievisual 的 Color（自动满足 parley::Brush）

pub(crate) use measure::layout_text;
pub use measure::{layout_metrics, measure_text};

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
    use parley::{FontContext, LayoutContext, StyleProperty};
    use std::cell::RefCell;
    use std::sync::Arc;

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

    /// 测量一段文本（canvas `measureText`）。`style.max_width` 作为换行上限。
    ///
    /// 复用线程本地缓存的 `FontContext` / `LayoutContext`（parley 内部缓存字体、
    /// 字形与 scratch 分配），避免每次调用重建导致的系统字体扫描。
    pub fn measure_text(content: &str, style: &TextStyle, max_width: Option<f64>) -> TextMeasure {
        with_cached_context(|fc, lc| {
            let layout = build_layout(fc, lc, content, style, max_width);
            let metrics = layout_metrics(&layout);
            TextMeasure {
                size: Size::new(metrics.width, metrics.height),
                metrics,
                layout: Arc::new(layout),
            }
        })
    }

    /// 排版一段文本（返回预排版 layout，不额外计算度量）。
    ///
    /// 与 [`measure_text`] 共享同一 TLS 上下文；供渲染端等"只要 layout"的场景复用，
    /// 避免重复排版。`style.max_width` 作为换行上限。
    pub(crate) fn layout_text(
        content: &str,
        style: &TextStyle,
        max_width: Option<f64>,
    ) -> Arc<TextLayout> {
        with_cached_context(|fc, lc| Arc::new(build_layout(fc, lc, content, style, max_width)))
    }

    /// 从已排版 layout 提取度量（canvas `TextMetrics`）。
    pub fn layout_metrics(layout: &TextLayout) -> TextMetrics {
        let width = layout.width() as f64;
        let height = layout.height() as f64;
        let (ascent, descent, baseline, line_height) = match layout.get(0) {
            Some(line) => {
                let m = line.metrics();
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

    /// 构造 parley 布局（测量与渲染共享，保证尺寸一致）。
    pub(crate) fn build_layout(
        font_ctx: &mut FontContext,
        layout_ctx: &mut LayoutContext<Color>,
        content: &str,
        style: &TextStyle,
        max_width: Option<f64>,
    ) -> TextLayout {
        // scale = 1.0（显示缩放）；字号由 FontSize 属性指定，保证 layout 坐标为逻辑像素。
        let mut builder = layout_ctx.ranged_builder(font_ctx, content, 1.0, true);
        builder.push_default(StyleProperty::FontSize(style.font_size as f32));
        builder.push_default(StyleProperty::Brush(style.color));
        // font_family 可能是逗号分隔列表（CSS font-family）。用 parlance 解析为
        // 有序族列表，交给 parley 依次回退；解析失败时回退为整体当作单个族名。
        use parley::style::{FontFamily, FontFamilyName};
        let families: Vec<FontFamilyName> = FontFamilyName::parse_css_list(&style.font_family)
            .filter_map(Result::ok)
            .collect();
        builder.push_default(StyleProperty::FontFamily(if families.is_empty() {
            FontFamily::named(&style.font_family)
        } else {
            FontFamily::List(families.into())
        }));
        builder.push_default(StyleProperty::FontWeight(parley::style::FontWeight::new(
            style.font_weight,
        )));
        let pstyle = match style.font_style {
            FontStyle::Normal => parley::style::FontStyle::Normal,
            FontStyle::Italic => parley::style::FontStyle::Italic,
            FontStyle::Oblique => parley::style::FontStyle::Oblique(None),
        };
        builder.push_default(StyleProperty::FontStyle(pstyle));
        if let Some(lh) = style.line_height {
            let factor = (lh / style.font_size.max(1e-6)) as f32;
            builder.push_default(StyleProperty::LineHeight(
                parley::style::LineHeight::FontSizeRelative(factor),
            ));
        }

        let mut layout = builder.build(content);
        // 最大宽度作为换行上限；无限制时传 None。
        layout.break_all_lines(max_width.map(|w| w as f32));
        layout
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::Color;

    #[test]
    fn measure_metrics_basic() {
        let style = TextStyle::new(Color::BLACK, 20.0, "sans-serif");
        let m = measure_text("Hi", &style, None);
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
        let m = measure_text("X", &style, None).metrics;
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
        let a = measure_text("缓存复用", &style, None).metrics.width;
        let b = measure_text("缓存复用", &style, None).metrics.width;
        assert!((a - b).abs() < 1e-6);

        // 独立线程各自持有上下文，互不干扰。
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let style = style.clone();
                std::thread::spawn(move || {
                    let m = measure_text("thread", &style, None);
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
        let m = measure_text("共享", &style, None);
        let l = layout_text("共享", &style, None);
        assert_eq!(l.width() as f64, m.metrics.width);
        assert_eq!(l.height() as f64, m.metrics.height);
    }

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
