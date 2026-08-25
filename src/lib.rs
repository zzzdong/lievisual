//! # lievisual
//!
//! 声明式视觉场景（IR）+ 可插拔渲染后端。
//!
//! ## 设计要点
//! - **IR 与后端分离**：[`scene`] 描述"画什么"（图元 + 层级 `z_index` + 变换 + 节点属性），
//!   [`render::Renderer`] 描述"怎么画"。增加后端（SVG / vello_cpu / GPU）无需改动 IR。
//! - **统一层级与变换**：`z_index` / `Transform` 提升为 [`scene::SceneNode`] 的字段；
//!   节点级 `opacity` / `name` / `visible` / `clip` 同样属于节点（Group 子树递归生效），
//!   新增图元变体时不必到处补字段。
//! - **图层**：[`scene::Scene`] 可携带一组有序 [`scene::Layer`]（从底到顶），整层控制
//!   `visible` / `opacity` / `transform` / `name`，层内节点再按 `z_index` 稳定排序；
//!   `nodes` 作为默认层（最底）保留。
//! - **图元丰富**：[`scene::Element`] 提供矩形、圆、椭圆、圆角矩形、线、折线、多边形、
//!   圆弧、扇形、路径、渐变路径、文本、组合（Group），覆盖图表/流程图/Markdown 常见需求。
//! - **填充多样**：纯色、线性渐变、径向渐变（[`scene::Fill`]）；描边支持线帽/连接、
//!   虚线（`dash_array` / `dash_offset`）与 `miter_limit`。
//! - **文本（富文本核心）**：[`scene::Element::Text`] 以样式化片段 [`text::RichSpan`] 为核心，
//!   排版 / 测量统一走 `&[RichSpan]` 入口（[`text::measure_text`] / [`text::layout_text`]）；
//!   可携带预排版 [`text::TextLayout`]。栅格后端用 layout 精确绘字形，SVG 后端回退到 `<text>`。
//!
//! ## 后端支持（默认全部启用，无 feature gate）
//! - [`render::SvgRenderer`]：输出矢量 SVG 字符串，支持场景元数据（`title` / `description`
//!   / `scale`）。
//! - [`render::VelloPixmapRenderer`]：vello_cpu 栅格化 → PNG 字节。

pub mod builder;
pub mod geometry;
pub mod glyph;
pub mod pixmap;
pub mod render;
pub mod scene;
pub mod text;

/// 暴露底层排版引擎 parley，供下游（如 liepress）从 [`text::TextLayout`] 读取
/// 逐字形 / run / 字体字节等细粒度数据（lievisual 只封装高层测量与渲染）。
pub use parley;

pub use geometry::{BezPath, Color, Point, PointExt, Rect, RectExt, Size, Transform, Vec2};
pub use pixmap::Pixmap;
pub use scene::{
    Clip, Element, Fill, FillStrokeStyle, GradientStop, Layer, LineCap, LineJoin, LinearGradient,
    ObjectFit, RadialGradient, Scene, SceneImage, SceneNode, Stroke,
};
pub use text::{
    FontSource, FontStyle, Glyph, LineMetrics, RichSpan, TextAlign, TextBaseline, TextDecoration,
    TextLayout, TextLine, TextMeasure, TextMetrics, TextRun, TextStyle, compute_text_offset,
    layout_metrics, layout_text, measure_text, parse_generic_family, register_font,
    register_font_generic,
};
