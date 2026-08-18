//! Canvas 风格便捷构造层（builder / sugar）。
//!
//! 本模块在底层声明式 IR（[`crate::scene`] / [`crate::text`]）之上提供一层可选糖衣，
//! 参考 Web Canvas 2D API 的心智模型（`ctx.fillStyle` / `ctx.font` / `ctx.fillText`），
//! 让"从浏览器迁移"或"快速搭一个图"更顺手。
//!
//! ## 定位与边界
//! - **纯构造**：所有方法都**消费不可变的 [`Ctx`] 并返回 [`Element`]**，不做任何即时绘制；
//!   与声明式 IR 完全兼容，产出可直接 `scene.push(...)`（经 `From<Element>` 自动转 `SceneNode`）。
//! - **不引入可变状态**：Canvas 的 `ctx` 是可变命令式状态，本层刻意不模拟。`with_*` 返回
//!   新的 `Ctx`（构建者模式），如需"同画布多种样式"就复制/链式派生，互不污染。
//! - **不破坏 IR**：所有便捷方法最终落到 `Element::*` / `RichSpan`，未新增任何 IR 类型。
//!   核心 value：文本锚点语义（`fillText` 的 `textAlign`/`textBaseline` 定位）由渲染端
//!   内部统一计算，本层直接转发，无需调用方手动算偏移。
//!
//! ## 文本锚点（对齐 Canvas `fillText`）
//! [`Ctx::fill_text`] 的 `(x, y)` 是**锚点**：由 [`TextStyle`] 的 `align` / `baseline`
//! 决定锚点落在文本块的哪个位置（默认 `Left` + `Alphabetic`，即 `(x,y)` 在首行基线）。
//! 渲染端内部按 [`crate::text::compute_text_offset`] 统一算偏移，调用方无需手动定位。
//!
//! 使用范式：
//! ```
//! use lievisual::builder::Ctx;
//! use lievisual::geometry::{Color, Point};
//! use lievisual::scene::{Scene, Element};
//! use lievisual::text::TextStyle;
//!
//! let mut scene = Scene::new(200.0, 100.0);
//! let ctx = Ctx::new()
//!     .with_fill(Color::rgb(0x33, 0x66, 0x99))
//!     .with_font(TextStyle::new(Color::rgb(0x33, 0x66, 0x99), 24.0, "sans-serif"));
//! scene.push(ctx.fill_rect(10.0, 10.0, 80.0, 40.0));   // 填充矩形（用 with_fill 的色）
//! scene.push(ctx.fill_text(50.0, 50.0, "Hi"));          // 文本（用字体色，锚点=首行基线）
//! ```

use crate::geometry::{Color, Point, Rect, Size, Vec2};
use crate::scene::{Element, Fill, FillStrokeStyle, Stroke};
use crate::text::{TextStyle, measure_text};
use kurbo::BezPath;

/// Canvas 风格构造上下文：持有默认填充色 / 描边 / 字体，供便捷方法使用。
///
/// 不可变、可链式派生（`with_*` 返回新实例）；不携带任何绘制目标。
#[derive(Debug, Clone)]
pub struct Ctx {
    /// 默认填充色（Canvas `fillStyle`）。仅用于**形状**（矩形/圆/多边形/路径）的填充；
    /// 文本颜色由 [`Self::font`] 的 `color` 决定（见 [`Ctx::fill_text`]）。`None` 时
    /// 形状方法不填充、描边回退用字体的 `color`。
    pub fill: Option<Color>,
    /// 默认描边（Canvas `strokeStyle` + `lineWidth`）。`None` 时形状方法不描边、线类方法回退 1px。
    pub stroke: Option<Stroke>,
    /// 默认文本样式（Canvas `font` + `textAlign` + `textBaseline`）；文本色取其 `color`。
    pub font: TextStyle,
}

impl Default for Ctx {
    fn default() -> Self {
        Self {
            fill: Some(Color::BLACK),
            stroke: None,
            font: TextStyle::new(Color::BLACK, 12.0, "sans-serif"),
        }
    }
}

impl Ctx {
    /// 构造默认上下文（填充黑、无描边、12px sans-serif 文本）。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 设置默认填充色（`fillStyle`）。
    #[must_use]
    pub fn with_fill(mut self, color: Color) -> Self {
        self.fill = Some(color);
        self
    }

    /// 设置默认描边（`strokeStyle` + `lineWidth`）。
    #[must_use]
    pub fn with_stroke(mut self, stroke: Stroke) -> Self {
        self.stroke = Some(stroke);
        self
    }

    /// 设置默认文本样式（`font`）。
    #[must_use]
    pub fn with_font(mut self, font: TextStyle) -> Self {
        self.font = font;
        self
    }

    /// 便捷：仅设置文本颜色（写入 `self.font.color`；不影响形状填充 [`Self::fill`]）。
    #[must_use]
    pub fn with_text_color(mut self, color: Color) -> Self {
        self.font.color = color;
        self
    }

    /// 文本颜色 = 字体自带色（`TextStyle::color`；`with_text_color` 即改此字段）。
    ///
    /// 与 Canvas `fillStyle` 决定文本色不同，lievisual 把文本色并入 `TextStyle.color`，
    /// 故文本色与形状填充（[`Self::shape_fill`]）解耦，避免 `with_fill` 意外覆盖文本。
    fn text_color(&self) -> Color {
        self.font.color
    }

    /// 形状填充色：默认填充色 > 字体自带色。
    fn shape_fill(&self) -> Color {
        self.fill.unwrap_or(self.font.color)
    }

    /// 用默认填充 + 描边构造 `FillStrokeStyle`。
    fn fill_stroke(&self) -> FillStrokeStyle {
        FillStrokeStyle {
            fill: self.fill.map(Fill::Solid),
            stroke: self.stroke.clone(),
        }
    }

    // ---- 文本 ----

    /// 绘制文本（`ctx.fillText`）。`(x, y)` 为锚点，由 `self.font` 的
    /// `align` / `baseline` 决定（默认：首行基线）。
    #[must_use]
    pub fn fill_text(&self, x: f64, y: f64, text: impl Into<String>) -> Element {
        let mut style = self.font.clone();
        style.color = self.text_color();
        Element::text(text, Point::new(x, y), style)
    }

    /// 测量文本（`ctx.measureText`），返回宽/高（由 `self.font` 排版）。
    #[must_use]
    pub fn measure_text(&self, text: impl AsRef<str>) -> Size {
        let style = self.font.clone();
        measure_text(
            std::slice::from_ref(&crate::text::RichSpan::new(text.as_ref(), style)),
            None,
        )
        .size
    }

    // ---- 形状：矩形 ----

    /// 填充矩形（`fillRect`）。`(x, y)` 为左上角，`w`/`h` 为宽高。
    #[must_use]
    pub fn fill_rect(&self, x: f64, y: f64, w: f64, h: f64) -> Element {
        Element::rect(
            Rect::new(x, y, x + w, y + h),
            FillStrokeStyle::fill(self.shape_fill()),
        )
    }

    /// 描边矩形（`strokeRect`）。描边居中于边线（Canvas 语义）。
    #[must_use]
    pub fn stroke_rect(&self, x: f64, y: f64, w: f64, h: f64) -> Element {
        Element::rect(
            Rect::new(x, y, x + w, y + h),
            FillStrokeStyle::stroke(
                self.stroke
                    .as_ref()
                    .map(|s| s.color)
                    .unwrap_or(self.shape_fill()),
                self.stroke.as_ref().map(|s| s.width).unwrap_or(1.0),
            ),
        )
    }

    // ---- 形状：圆 / 椭圆 ----

    /// 填充圆（`arc` 全周 + `fill`）。`(x, y)` 为圆心，`r` 为半径。
    #[must_use]
    pub fn fill_circle(&self, x: f64, y: f64, r: f64) -> Element {
        Element::circle(
            Point::new(x, y),
            r,
            FillStrokeStyle::fill(self.shape_fill()),
        )
    }

    /// 填充椭圆。`(x, y)` 为圆心，`rx`/`ry` 为两半轴，`rotation` 为绕心旋转（弧度）。
    #[must_use]
    pub fn fill_ellipse(&self, x: f64, y: f64, rx: f64, ry: f64, rotation: f64) -> Element {
        Element::ellipse(
            Point::new(x, y),
            Vec2::new(rx, ry),
            rotation,
            FillStrokeStyle::fill(self.shape_fill()),
        )
    }

    // ---- 形状：线 / 折线 / 多边形 ----

    /// 线段（`beginPath + moveTo + lineTo + stroke`）。
    #[must_use]
    pub fn line(&self, x0: f64, y0: f64, x1: f64, y1: f64) -> Element {
        Element::line(
            Point::new(x0, y0),
            Point::new(x1, y1),
            self.stroke
                .clone()
                .unwrap_or(Stroke::new(self.shape_fill(), 1.0)),
        )
    }

    /// 折线（`polyline`，仅描边）。`pts` 为 `(x, y)` 序列。
    #[must_use]
    pub fn polyline(&self, pts: &[(f64, f64)]) -> Element {
        Element::poly(
            pts.iter().map(|&(x, y)| Point::new(x, y)).collect(),
            self.stroke
                .clone()
                .unwrap_or(Stroke::new(self.shape_fill(), 1.0)),
        )
    }

    /// 闭合多边形（可填充 + 描边）。
    #[must_use]
    pub fn polygon(&self, pts: &[(f64, f64)]) -> Element {
        Element::polygon(
            pts.iter().map(|&(x, y)| Point::new(x, y)).collect(),
            self.fill_stroke(),
        )
    }

    // ---- 形状：路径 ----

    /// 贝塞尔路径（`beginPath + 路径 + fill`）。`closed` 决定是否闭合后再填充。
    #[must_use]
    pub fn path(&self, path: BezPath, closed: bool) -> Element {
        Element::Path {
            path,
            style: self.fill_stroke(),
            closed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_ctx() -> Ctx {
        Ctx::new().with_fill(Color::rgb(0x33, 0x66, 0x99))
    }

    /// 文本锚点：默认 Left/Alphabetic，`position` 即锚点；颜色取字体自带色（非 `with_fill`）。
    #[test]
    fn fill_text_anchors_and_color() {
        let ctx =
            default_ctx().with_font(TextStyle::new(Color::rgb(0xff, 0, 0), 24.0, "sans-serif"));
        let el = ctx.fill_text(50.0, 50.0, "Hi");
        match el {
            Element::Text {
                spans,
                position,
                style,
                layout,
            } => {
                assert_eq!(spans.len(), 1);
                assert_eq!(spans[0].text, "Hi");
                assert_eq!(spans[0].style.color, Color::rgb(0xff, 0, 0));
                assert_eq!(position, Point::new(50.0, 50.0));
                assert_eq!(style.font_size, 24.0);
                assert!(layout.is_none());
            }
            _ => panic!("expected Text"),
        }
    }

    /// `with_text_color` 修改字体色（文本色），不影响形状填充色。
    #[test]
    fn text_color_independent_of_shape_fill() {
        let ctx = default_ctx().with_text_color(Color::rgb(0xff, 0, 0));
        match ctx.fill_text(0.0, 0.0, "x") {
            Element::Text { spans, .. } => assert_eq!(spans[0].style.color, Color::rgb(0xff, 0, 0)),
            _ => panic!("expected Text"),
        }
        // 形状填充仍用默认填充色。
        match ctx.fill_rect(0.0, 0.0, 1.0, 1.0) {
            Element::Rect { style, .. } => {
                assert_eq!(style.fill, Some(Fill::Solid(Color::rgb(0x33, 0x66, 0x99))));
            }
            _ => panic!("expected Rect"),
        }
    }

    /// 矩形：(x, y) 为左上角，w/h 为尺寸。
    #[test]
    fn rect_builders() {
        let ctx = default_ctx();
        match ctx.fill_rect(10.0, 20.0, 30.0, 40.0) {
            Element::Rect { rect, style } => {
                assert_eq!(rect, Rect::new(10.0, 20.0, 40.0, 60.0));
                assert!(style.fill.is_some() && style.stroke.is_none());
            }
            _ => panic!("expected Rect"),
        }
        let ctx = Ctx::new().with_stroke(Stroke::new(Color::BLACK, 2.0));
        match ctx.stroke_rect(0.0, 0.0, 5.0, 5.0) {
            Element::Rect { rect, style } => {
                assert_eq!(rect, Rect::new(0.0, 0.0, 5.0, 5.0));
                assert_eq!(style.stroke.as_ref().unwrap().width, 2.0);
            }
            _ => panic!("expected Rect"),
        }
    }

    /// 线 / 折线 / 多边形 / 圆。
    #[test]
    fn shape_builders() {
        let ctx = Ctx::new()
            .with_fill(Color::BLACK)
            .with_stroke(Stroke::new(Color::BLACK, 1.0));
        assert!(matches!(ctx.line(0.0, 0.0, 1.0, 1.0), Element::Line { .. }));
        assert!(matches!(
            ctx.polyline(&[(0.0, 0.0), (1.0, 1.0), (2.0, 0.0)]),
            Element::Polyline { .. }
        ));
        assert!(matches!(
            ctx.polygon(&[(0.0, 0.0), (1.0, 1.0), (0.0, 1.0)]),
            Element::Polygon { .. }
        ));
        assert!(matches!(
            ctx.fill_circle(0.0, 0.0, 5.0),
            Element::Circle { .. }
        ));
        assert!(matches!(
            ctx.fill_ellipse(0.0, 0.0, 4.0, 2.0, 0.0),
            Element::Ellipse { .. }
        ));
    }

    /// 路径：`closed` 透传，样式取 fill+stroke。
    #[test]
    fn path_builder() {
        let ctx = Ctx::new()
            .with_fill(Color::BLACK)
            .with_stroke(Stroke::new(Color::BLACK, 1.0));
        let mut p = BezPath::new();
        p.move_to((0.0, 0.0));
        p.line_to((1.0, 0.0));
        p.line_to((1.0, 1.0));
        p.close_path();
        match ctx.path(p, true) {
            Element::Path {
                style,
                closed,
                path,
            } => {
                assert!(closed);
                assert!(style.fill.is_some() && style.stroke.is_some());
                assert!(!path.is_empty());
            }
            _ => panic!("expected Path"),
        }
    }

    /// 测量：宽高应 > 0。
    #[test]
    fn measure_returns_size() {
        let ctx = default_ctx().with_font(TextStyle::new(Color::BLACK, 20.0, "sans-serif"));
        let size = ctx.measure_text("Hello");
        assert!(size.width > 0.0 && size.height > 0.0);
    }

    /// builder 产出可 push 进 Scene（Element → SceneNode 自动转换）。
    #[test]
    fn elements_are_pushable() {
        use crate::scene::Scene;
        let ctx = default_ctx();
        let mut scene = Scene::new(100.0, 100.0);
        scene.push(ctx.fill_rect(0.0, 0.0, 10.0, 10.0));
        scene.push(ctx.fill_text(5.0, 5.0, "ok"));
        assert_eq!(scene.nodes.len(), 2);
    }
}
