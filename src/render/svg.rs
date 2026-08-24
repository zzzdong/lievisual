//! SVG 矢量输出后端。
//!
//! 纯声明式，不依赖 vello / parley。将每个 [`Element`] 映射为原生 SVG 标签：
//! - Rect/Circle/Ellipse/RoundedRect/Line/Polyline/Polygon → 对应 SVG 图形元素。
//! - Arc/Pie → `<path d="...">`（SVG 原生圆弧指令 `A`）。
//! - Path/GradientPath → 用 kurbo 的 SVG path 序列化（`d` 属性）。
//! - Text → `<text>` 元素（不依赖 parley；`layout` 字段被忽略，由浏览器排版）。
//!
//! 变换通过 `<g transform="...">` 实现，与后端状态栈解耦；节点 `opacity` / `name`
//! 分别输出 `<g opacity>` / `<g id>`；场景 `title` / `description` / `scale` 在
//! [`Renderer::render_scene`] 时捕获。
//!
//! 渐变统一使用 `gradientUnits="userSpaceOnUse"`（用户坐标，与 IR 的绝对坐标一致）。

use crate::geometry::{Color, Point, Rect, Transform, Vec2};
use crate::render::Renderer;
use crate::scene::{
    Fill, FillStrokeStyle, GradientStop, LineCap, LineJoin, LinearGradient, Scene, Stroke,
};
use crate::text::TextStyle;
use std::fmt::Write;

/// SVG 渲染器：累积 SVG 字符串。
pub struct SvgRenderer {
    buf: String,
    width: f64,
    height: f64,
    background: Color,
    gradient_id: usize,
    clip_id: usize,
    title: Option<String>,
    description: Option<String>,
    scale: f64,
    /// 延迟累积的 `<marker>` 定义：仅当有边实际请求 `marker_end` 时才填充，
    /// 避免每个文档都带上未使用的箭头 `<defs>`。
    marker_defs: String,
}

impl SvgRenderer {
    pub fn new(width: f64, height: f64) -> Self {
        Self {
            buf: String::new(),
            width,
            height,
            background: Color::WHITE,
            gradient_id: 0,
            clip_id: 0,
            title: None,
            description: None,
            scale: 1.0,
            marker_defs: String::new(),
        }
    }

    pub fn with_background(mut self, bg: Color) -> Self {
        self.background = bg;
        self
    }

    /// 输出缩放（像素密度）：放大 `width`/`height` 而保持 `viewBox` 不变。
    #[must_use]
    pub fn with_scale(mut self, scale: f64) -> Self {
        self.scale = scale;
        self
    }

    /// 文档标题。
    #[must_use]
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// 文档描述。
    #[must_use]
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// 取出完整 SVG 文档（含 `<svg>` 根与背景）。
    pub fn into_string(self) -> String {
        let mut out = String::new();
        // scale 只放大输出尺寸，viewBox 保持场景坐标，实现更高分辨率导出。
        let sw = self.width * self.scale;
        let sh = self.height * self.scale;
        let _ = writeln!(
            out,
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="{:.2}" height="{:.2}" viewBox="0 0 {:.2} {:.2}">"#,
            sw, sh, self.width, self.height
        );
        if let Some(t) = &self.title {
            let _ = writeln!(out, "<title>{}</title>", escape_text(t));
        }
        if let Some(d) = &self.description {
            let _ = writeln!(out, "<desc>{}</desc>", escape_text(d));
        }
        // 背景矩形（viewBox 坐标，随 scale 缩放）。背景完全透明时跳过，
        // 避免多一层无意义的整画布矩形（mermaid 的默认 SVG 即不含背景 <rect>，
        // 其底色由 CSS 的 background-color 决定）。
        if self.background.a > 0.0 {
            let _ = writeln!(
                out,
                r#"<rect x="0" y="0" width="{:.2}" height="{:.2}" fill="{}"/>"#,
                self.width,
                self.height,
                self.background.to_hex()
            );
        }
        // 共享箭头 marker：边通过 marker-end="url(#arrow)" 复用，避免每条边独立绘制箭头。
        // 延迟输出：仅当场景中实际出现 marker_end 引用时才写入 <defs>。
        if !self.marker_defs.is_empty() {
            let _ = writeln!(out, "<defs>{}</defs>", self.marker_defs);
        }
        // 内容
        out.push_str(&self.buf);
        out.push_str("</svg>\n");
        out
    }

    fn next_gradient_id(&mut self) -> usize {
        let id = self.gradient_id;
        self.gradient_id += 1;
        id
    }

    /// 延迟注册箭头 marker：仅当某条边实际请求 `marker_end` 时才输出 `<defs>`，
    /// 避免每个文档都带上未使用的箭头定义。按 id 去重，同 id 只输出一次。
    fn ensure_marker(&mut self, id: &str) {
        let needle = format!("id=\"{}\"", id);
        if self.marker_defs.contains(&needle) {
            return;
        }
        let _ = write!(
            self.marker_defs,
            r#"<marker id="{id}" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="8" markerHeight="8" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10" fill="none" stroke="context-stroke" stroke-width="1.5"/></marker>"#,
        );
    }
}

impl Renderer for SvgRenderer {
    fn render_scene(&mut self, scene: &Scene) {
        // 捕获场景元数据，再复用默认遍历。
        self.title = scene.title.clone();
        self.description = scene.description.clone();
        self.scale = scene.scale;
        self.render_scene_ordered(scene);
    }

    fn draw_rect(&mut self, rect: Rect, style: &FillStrokeStyle) {
        let mut el = String::new();
        let _ = write!(
            el,
            r#"<rect x="{:.2}" y="{:.2}" width="{:.2}" height="{:.2}" "#,
            rect.min_x(),
            rect.min_y(),
            rect.width(),
            rect.height()
        );
        self.apply_fill_stroke(&mut el, style);
        let _ = writeln!(el, "/>");
        self.buf.push_str(&el);
    }

    fn draw_circle(&mut self, center: Point, radius: f64, style: &FillStrokeStyle) {
        let mut el = String::new();
        let _ = write!(
            el,
            r#"<circle cx="{:.2}" cy="{:.2}" r="{:.2}" "#,
            center.x, center.y, radius
        );
        self.apply_fill_stroke(&mut el, style);
        let _ = writeln!(el, "/>");
        self.buf.push_str(&el);
    }

    fn draw_ellipse(&mut self, center: Point, radii: Vec2, rotation: f64, style: &FillStrokeStyle) {
        let mut el = String::new();
        let _ = write!(
            el,
            r#"<ellipse cx="{:.2}" cy="{:.2}" rx="{:.2}" ry="{:.2}" "#,
            center.x, center.y, radii.x, radii.y
        );
        if rotation != 0.0 {
            let _ = write!(
                el,
                r#"transform="rotate({:.2} {:.2} {:.2})" "#,
                rotation.to_degrees(),
                center.x,
                center.y
            );
        }
        self.apply_fill_stroke(&mut el, style);
        let _ = writeln!(el, "/>");
        self.buf.push_str(&el);
    }

    fn draw_rounded_rect(&mut self, rect: Rect, radius: f64, style: &FillStrokeStyle) {
        // 与 PNG 后端一致：用几何边界得到非负宽高（Rect 可能 min > max）。
        let (x, y) = (
            rect.min_x().min(rect.max_x()),
            rect.min_y().min(rect.max_y()),
        );
        let w = (rect.max_x() - rect.min_x()).abs();
        let h = (rect.max_y() - rect.min_y()).abs();
        let mut el = String::new();
        let _ = write!(
            el,
            r#"<rect x="{:.2}" y="{:.2}" width="{:.2}" height="{:.2}" rx="{:.2}" "#,
            x, y, w, h, radius
        );
        self.apply_fill_stroke(&mut el, style);
        let _ = writeln!(el, "/>");
        self.buf.push_str(&el);
    }

    fn draw_line(&mut self, start: Point, end: Point, style: &Stroke) {
        let mut el = String::new();
        let _ = write!(
            el,
            r#"<line x1="{:.2}" y1="{:.2}" x2="{:.2}" y2="{:.2}" "#,
            start.x, start.y, end.x, end.y
        );
        self.apply_stroke_only(&mut el, style);
        let _ = writeln!(el, "/>");
        self.buf.push_str(&el);
    }

    fn draw_polyline(&mut self, points: &[Point], style: &Stroke, marker_end: Option<&str>) {
        let pts: Vec<String> = points
            .iter()
            .map(|p| format!("{:.2},{:.2}", p.x, p.y))
            .collect();
        let mut el = String::new();
        let _ = write!(el, r#"<polyline points="{}" "#, pts.join(" "));
        self.apply_stroke_only(&mut el, style);
        if let Some(id) = marker_end {
            self.ensure_marker(id);
            let _ = write!(el, r#" marker-end="url(#{})""#, id);
        }
        let _ = writeln!(el, "/>");
        self.buf.push_str(&el);
    }

    fn draw_polygon(&mut self, points: &[Point], style: &FillStrokeStyle) {
        let pts: Vec<String> = points
            .iter()
            .map(|p| format!("{:.2},{:.2}", p.x, p.y))
            .collect();
        let mut el = String::new();
        let _ = write!(el, r#"<polygon points="{}" "#, pts.join(" "));
        self.apply_fill_stroke(&mut el, style);
        let _ = writeln!(el, "/>");
        self.buf.push_str(&el);
    }

    fn draw_arc(
        &mut self,
        center: Point,
        radii: Vec2,
        start_angle: f64,
        sweep_angle: f64,
        style: &Stroke,
    ) {
        let d = arc_path_d(center, radii, start_angle, sweep_angle, false);
        let mut el = String::new();
        let _ = write!(el, r#"<path d="{}" "#, d);
        self.apply_stroke_only(&mut el, style);
        let _ = writeln!(el, "/>");
        self.buf.push_str(&el);
    }

    fn draw_pie(
        &mut self,
        center: Point,
        radius: f64,
        start_angle: f64,
        sweep_angle: f64,
        style: &FillStrokeStyle,
    ) {
        let d = arc_path_d(
            center,
            Vec2::new(radius, radius),
            start_angle,
            sweep_angle,
            true,
        );
        let mut el = String::new();
        let _ = write!(el, r#"<path d="{}" "#, d);
        self.apply_fill_stroke(&mut el, style);
        let _ = writeln!(el, "/>");
        self.buf.push_str(&el);
    }

    fn draw_path(&mut self, path: &kurbo::BezPath, style: &FillStrokeStyle, closed: bool, marker_end: Option<&str>) {
        let d = if closed {
            let mut p = path.clone();
            p.close_path();
            p.to_svg()
        } else {
            path.to_svg()
        };
        let mut el = String::new();
        let _ = write!(el, r#"<path d="{}" "#, d);
        self.apply_fill_stroke(&mut el, style);
        if let Some(id) = marker_end {
            self.ensure_marker(id);
            let _ = write!(el, r#" marker-end="url(#{})""#, id);
        }
        let _ = writeln!(el, "/>");
        self.buf.push_str(&el);
    }

    fn draw_gradient_path(
        &mut self,
        path: &kurbo::BezPath,
        gradient: &LinearGradient,
        stroke: Option<&Stroke>,
    ) {
        let mut p = path.clone();
        p.close_path();
        let d = p.to_svg();
        let gid = self.next_gradient_id();
        let defs = format!(
            r#"<defs><linearGradient id="grad{}" gradientUnits="userSpaceOnUse" x1="{:.2}" y1="{:.2}" x2="{:.2}" y2="{:.2}">{}</linearGradient></defs>"#,
            gid,
            gradient.start.x,
            gradient.start.y,
            gradient.end.x,
            gradient.end.y,
            gradient.stops.iter().map(stop_svg).collect::<String>()
        );
        let fill = format!("url(#grad{})", gid);
        let mut el = String::new();
        let _ = write!(el, r#"<path d="{}" fill="{}" "#, d, fill);
        if let Some(s) = stroke {
            self.append_stroke(&mut el, s);
        }
        let _ = writeln!(el, "/>");
        self.buf.push_str(&defs);
        self.buf.push_str(&el);
    }

    fn draw_text(
        &mut self,
        spans: &[crate::text::RichSpan],
        position: Point,
        style: &TextStyle,
        layout: Option<&crate::text::TextLayout>,
    ) {
        // SVG 使用原生 <text>，由浏览器排版；layout 仅用于背景矩形的精确尺寸。
        // 纯文本由 spans 拼接推导，保证与 vello 后端的字形内容一致。
        let content = spans.iter().map(|s| s.text.as_str()).collect::<String>();
        // 预排版 layout 可提供文本块精确尺寸；无则回退到近似。
        let metrics = layout.map(crate::text::layout_metrics);
        // 锚点：text-anchor（水平，对应 align）+ dominant-baseline（垂直，对应 baseline）。
        let anchor = match style.align {
            crate::text::TextAlign::Left | crate::text::TextAlign::Justify => "start",
            crate::text::TextAlign::Center => "middle",
            crate::text::TextAlign::Right => "end",
        };
        // SVG 的 dominant-baseline 没有 "top"/"bottom" 等值；统一转成 alphabetic
        // baseline 坐标，让浏览器排版与 vello/parley 后端的 canvas fillText 语义一致。
        // position.y 是锚点（由 baseline 决定含义），先算文本块顶部，再推到 alphabetic baseline。
        let (anchor_offset, alphabetic_baseline) = match &metrics {
            Some(m) => (style.baseline.anchor_offset(m), m.alphabetic_baseline),
            None => {
                let ab = style.font_size * 0.85;
                let off = match style.baseline {
                    crate::text::TextBaseline::Top => 0.0,
                    crate::text::TextBaseline::Hanging => style.font_size * 0.75,
                    crate::text::TextBaseline::Middle => style.font_size * 0.5,
                    crate::text::TextBaseline::Alphabetic => ab,
                    crate::text::TextBaseline::Ideographic => style.font_size * 0.9,
                    crate::text::TextBaseline::Bottom => style.font_size * 1.3,
                };
                (off, ab)
            }
        };
        let block_top_y = position.y - anchor_offset;
        let alphabetic_y = block_top_y + alphabetic_baseline;
        let baseline = "alphabetic";
        let fstyle = match style.font_style {
            crate::text::FontStyle::Normal => "normal",
            crate::text::FontStyle::Italic => "italic",
            crate::text::FontStyle::Oblique => "oblique",
        };
        let rot = if style.rotation != 0.0 {
            format!(
                r#" transform="rotate({:.2} {:.2} {:.2})""#,
                style.rotation.to_degrees(),
                position.x,
                position.y
            )
        } else {
            String::new()
        };
        let lh = match style.line_height {
            Some(h) => format!(r#" style="line-height:{:.2}""#, h),
            None => String::new(),
        };
        // 可选排版属性：字宽（font-stretch）、字间距（letter-spacing）、
        // 文本装饰（下划线 / 删除线，按 style 块级默认值输出）。
        let fwidth = match style.font_width {
            Some(w) => format!(r#" font-stretch="{:.0}%""#, w * 100.0),
            None => String::new(),
        };
        let letter = if style.letter_spacing != 0.0 {
            format!(r#" letter-spacing="{:.2}""#, style.letter_spacing)
        } else {
            String::new()
        };
        let decoration: Vec<&str> = match (style.underline, style.strikethrough) {
            (true, true) => vec!["underline", "line-through"],
            (true, false) => vec!["underline"],
            (false, true) => vec!["line-through"],
            (false, false) => vec![],
        };
        let deco = if decoration.is_empty() {
            String::new()
        } else {
            format!(r#" text-decoration="{}""#, decoration.join(" "))
        };
        // 基线偏移（上下标）：baseline-shift 正数=上移（上标），负数=下移（下标）。
        let bshift = if style.baseline_shift != 0.0 {
            format!(r#" baseline-shift="{:.2}px""#, -style.baseline_shift)
        } else {
            String::new()
        };
        // 文本背景（行内高亮）：优先用预排版 layout 的精确宽高；无 layout 时回退近似。
        // 宽 = metrics.width（有 layout）或 字符数×0.6×font-size；高 = metrics.height 或 font-size×1.3。
        if let Some(bg) = style.background_color {
            let (w, h) = match &metrics {
                Some(m) => (m.width, m.height),
                None => {
                    let avg_w = style.font_size * 0.6;
                    (
                        (content.chars().count() as f64 * avg_w).max(avg_w),
                        style.font_size * 1.3,
                    )
                }
            };
            // 背景矩形与文本块顶部对齐，和 vello 后端一致。
            let rx = position.x;
            let ry = block_top_y;
            // 右对齐时背景向左扩展。
            let bx = match style.align {
                crate::text::TextAlign::Right => rx - w,
                crate::text::TextAlign::Center => rx - w / 2.0,
                crate::text::TextAlign::Left | crate::text::TextAlign::Justify => rx,
            };
            self.buf.push_str(&format!(
                r#"<rect x="{:.2}" y="{:.2}" width="{:.2}" height="{:.2}" fill="{}"/>"#,
                bx,
                ry - style.baseline_shift,
                w,
                h,
                bg.to_hex()
            ));
            self.buf.push('\n');
        }
        let mut el = String::new();
        let _ = write!(
            el,
            r#"<text x="{:.2}" y="{:.2}" text-anchor="{}" dominant-baseline="{}" font-family="{}" font-size="{:.2}" font-style="{}" font-weight="{:.0}" fill="{}"{}{}{}{}{}{}>{}</text>"#,
            position.x,
            alphabetic_y,
            anchor,
            baseline,
            escape_attr(&style.font_family_css()),
            style.font_size,
            fstyle,
            style.font_weight,
            style.color.to_hex(),
            fwidth,
            letter,
            deco,
            lh,
            rot,
            bshift,
            escape_text(&content)
        );
        self.buf.push_str(&el);
        self.buf.push('\n');
    }

    fn draw_image(&mut self, image: &crate::SceneImage, frame: Rect, opacity: f64) {
        // 位图 → PNG 编码 → data: URI（base64）并输出 <image>，由浏览器负责解码与 object-fit。
        let png = image.pixmap.to_png();
        let b64 = base64_encode(&png);
        let data_uri = format!("data:image/png;base64,{b64}");
        let op = if (opacity - 1.0).abs() > 1e-6 {
            format!(r#" opacity="{:.3}""#, opacity)
        } else {
            String::new()
        };
        // preserveAspectRatio 控制 object-fit；x/y/width/height 为显示框。
        // None：不缩放，用原始像素尺寸放框左上角。
        let (w, h) = match image.object_fit {
            crate::ObjectFit::None => (image.pixmap.width() as f64, image.pixmap.height() as f64),
            _ => (frame.width(), frame.height()),
        };
        let el = format!(
            r#"<image x="{:.2}" y="{:.2}" width="{:.2}" height="{:.2}" href="{}" preserveAspectRatio="{}"{}/>"#,
            frame.min_x(),
            frame.min_y(),
            w,
            h,
            data_uri,
            image.preserve_aspect_ratio(),
            op
        );
        self.buf.push_str(&el);
        self.buf.push('\n');
    }

    fn push_transform(&mut self, t: Transform) {
        let m = format!(
            r#"<g transform="matrix({:.6} {:.6} {:.6} {:.6} {:.6} {:.6})">"#,
            t.a, t.b, t.c, t.d, t.e, t.f
        );
        self.buf.push_str(&m);
    }

    fn pop_transform(&mut self) {
        self.buf.push_str("</g>");
    }

    fn push_opacity(&mut self, opacity: f64) {
        let m = format!(r#"<g opacity="{:.3}">"#, opacity);
        self.buf.push_str(&m);
    }

    fn pop_opacity(&mut self) {
        self.buf.push_str("</g>");
    }

    fn push_name(&mut self, name: Option<&str>, class: Option<&str>) {
        let id = name.unwrap_or("");
        let cls = class.unwrap_or("");
        match (id.is_empty(), cls.is_empty()) {
            (false, false) => self
                .buf
                .push_str(&format!(r#"<g id="{}" class="{}">"#, escape_attr(id), escape_attr(cls))),
            (false, true) => self.buf.push_str(&format!(r#"<g id="{}">"#, escape_attr(id))),
            (true, false) => self
                .buf
                .push_str(&format!(r#"<g class="{}">"#, escape_attr(cls))),
            (true, true) => self.buf.push_str("<g>"),
        }
    }

    fn pop_name(&mut self) {
        self.buf.push_str("</g>");
    }

    fn push_clip(&mut self, clip: &crate::scene::Clip) {
        let id = self.clip_id;
        self.clip_id += 1;
        // 将 Clip 映射为 <clipPath> 内的原生 SVG 元素。
        let inner = match clip {
            crate::scene::Clip::Rect(rect) => format!(
                r#"<rect x="{:.2}" y="{:.2}" width="{:.2}" height="{:.2}"/>"#,
                rect.min_x(),
                rect.min_y(),
                rect.width(),
                rect.height()
            ),
            crate::scene::Clip::RoundedRect { rect, radius } => format!(
                r#"<rect x="{:.2}" y="{:.2}" width="{:.2}" height="{:.2}" rx="{:.2}"/>"#,
                rect.min_x(),
                rect.min_y(),
                rect.width(),
                rect.height(),
                radius
            ),
            crate::scene::Clip::Circle { center, radius } => format!(
                r#"<circle cx="{:.2}" cy="{:.2}" r="{:.2}"/>"#,
                center.x, center.y, radius
            ),
            crate::scene::Clip::Ellipse {
                center,
                radii,
                rotation,
            } => {
                let rot = if *rotation != 0.0 {
                    format!(
                        r#" transform="rotate({:.2} {:.2} {:.2})""#,
                        rotation.to_degrees(),
                        center.x,
                        center.y
                    )
                } else {
                    String::new()
                };
                format!(
                    r#"<ellipse cx="{:.2}" cy="{:.2}" rx="{:.2}" ry="{:.2}"{} />"#,
                    center.x, center.y, radii.x, radii.y, rot
                )
            }
            crate::scene::Clip::Path(path) => {
                format!(r#"<path d="{}"/>"#, path.to_svg())
            }
        };
        let defs = format!(
            r#"<defs><clipPath id="clip{}">{}</clipPath></defs>"#,
            id, inner
        );
        self.buf.push_str(&defs);
        let m = format!(r#"<g clip-path="url(#clip{})">"#, id);
        self.buf.push_str(&m);
    }

    fn pop_clip(&mut self) {
        self.buf.push_str("</g>");
    }
}

impl SvgRenderer {
    fn apply_fill_stroke(&mut self, el: &mut String, style: &FillStrokeStyle) {
        match &style.fill {
            Some(Fill::Solid(c)) => write!(el, r#" fill="{}""#, c.to_hex()).unwrap(),
            Some(Fill::LinearGradient(g)) => {
                let gid = self.next_gradient_id();
                let defs = format!(
                    r#"<defs><linearGradient id="grad{}" gradientUnits="userSpaceOnUse" x1="{:.2}" y1="{:.2}" x2="{:.2}" y2="{:.2}">{}</linearGradient></defs>"#,
                    gid,
                    g.start.x,
                    g.start.y,
                    g.end.x,
                    g.end.y,
                    g.stops.iter().map(stop_svg).collect::<String>()
                );
                self.buf.push_str(&defs);
                write!(el, r#" fill="url(#grad{})""#, gid).unwrap();
            }
            Some(Fill::RadialGradient(g)) => {
                let gid = self.next_gradient_id();
                let defs = format!(
                    r#"<defs><radialGradient id="grad{}" gradientUnits="userSpaceOnUse" cx="{:.2}" cy="{:.2}" r="{:.2}">{}</radialGradient></defs>"#,
                    gid,
                    g.center.x,
                    g.center.y,
                    g.radius,
                    g.stops.iter().map(stop_svg).collect::<String>()
                );
                self.buf.push_str(&defs);
                write!(el, r#" fill="url(#grad{})""#, gid).unwrap();
            }
            None => write!(el, r#" fill="none""#).unwrap(),
        }
        if let Some(s) = &style.stroke {
            self.append_stroke(el, s);
        }
    }

    fn apply_stroke_only(&self, el: &mut String, style: &Stroke) {
        // SVG 形状的 fill 默认是 black；只描边的元素必须显式关闭填充，
        // 否则浏览器会把 polyline/arc 等区域填充成黑色。
        let _ = write!(el, r#" fill="none""#);
        self.append_stroke(el, style);
    }

    fn append_stroke(&self, el: &mut String, s: &Stroke) {
        let _ = write!(
            el,
            r#" stroke="{}" stroke-width="{:.2}" stroke-miterlimit="{:.2}""#,
            s.color.to_hex(),
            s.width,
            s.miter_limit
        );
        let cap = match s.line_cap {
            LineCap::Butt => "butt",
            LineCap::Round => "round",
            LineCap::Square => "square",
        };
        let join = match s.line_join {
            LineJoin::Miter => "miter",
            LineJoin::Round => "round",
            LineJoin::Bevel => "bevel",
        };
        let _ = write!(
            el,
            r#" stroke-linecap="{}" stroke-linejoin="{}""#,
            cap, join
        );
        if !s.dash_array.is_empty() {
            let da = s
                .dash_array
                .iter()
                .map(|d| format!("{:.2}", d))
                .collect::<Vec<_>>()
                .join(",");
            let _ = write!(el, r#" stroke-dasharray="{}""#, da);
            if s.dash_offset != 0.0 {
                let _ = write!(el, r#" stroke-dashoffset="{:.2}""#, s.dash_offset);
            }
        }
    }
}

fn stop_svg(s: &GradientStop) -> String {
    format!(
        r#"<stop offset="{:.3}" stop-color="{}"/>"#,
        s.offset,
        s.color.to_hex()
    )
}

/// 构造圆弧 / 扇形的 SVG path `d`（使用原生圆弧指令 `A`，而非贝塞尔近似）。
fn arc_path_d(
    center: Point,
    radii: Vec2,
    start_angle: f64,
    sweep_angle: f64,
    sector: bool,
) -> String {
    let start = (
        center.x + radii.x * start_angle.cos(),
        center.y + radii.y * start_angle.sin(),
    );
    let end_angle = start_angle + sweep_angle;
    let end = (
        center.x + radii.x * end_angle.cos(),
        center.y + radii.y * end_angle.sin(),
    );
    let large_arc = if sweep_angle.abs() > std::f64::consts::PI {
        1
    } else {
        0
    };
    // y-down 坐标系：正 sweep = 顺时针 = SVG sweep-flag 1。
    let sweep_flag = if sweep_angle >= 0.0 { 1 } else { 0 };
    if sector {
        format!(
            "M {:.2} {:.2} L {:.2} {:.2} A {:.2} {:.2} 0 {} {} {:.2} {:.2} Z",
            center.x,
            center.y,
            start.0,
            start.1,
            radii.x,
            radii.y,
            large_arc,
            sweep_flag,
            end.0,
            end.1
        )
    } else {
        format!(
            "M {:.2} {:.2} A {:.2} {:.2} 0 {} {} {:.2} {:.2}",
            start.0, start.1, radii.x, radii.y, large_arc, sweep_flag, end.0, end.1
        )
    }
}

/// XML 文本转义（用于 `<text>` 内容）。
fn escape_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
    out
}

fn escape_attr(s: &str) -> String {
    escape_text(s).replace('"', "&quot;")
}

/// 最小化 base64 编码（用于 SVG `data:` URI），避免引入额外依赖。
fn base64_encode(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[((n >> 18) & 0x3f) as usize] as char);
        out.push(ALPHABET[((n >> 12) & 0x3f) as usize] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[((n >> 6) & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[(n & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::Vec2;
    use crate::scene::{Element, Scene, SceneNode};
    use crate::text::{FontStyle, TextAlign, TextBaseline, TextStyle};

    fn render(scene: &Scene) -> String {
        let mut svg = SvgRenderer::new(scene.width, scene.height).with_background(scene.background);
        scene.render(&mut svg);
        svg.into_string()
    }

    #[test]
    fn scene_metadata_title_desc_scale() {
        let scene = Scene::new(100.0, 50.0)
            .with_title("T")
            .with_description("D")
            .with_scale(2.0);
        let out = render(&scene);
        assert!(out.contains("<title>T</title>"));
        assert!(out.contains("<desc>D</desc>"));
        // scale 放大输出尺寸，viewBox 保持场景坐标。
        assert!(out.contains(r#"width="200.00" height="100.00" viewBox="0 0 100.00 50.00""#));
    }

    #[test]
    fn ellipse_rounded_rect_polygon_native_tags() {
        let mut scene = Scene::new(200.0, 200.0);
        scene.push(Element::ellipse(
            Point::new(10.0, 10.0),
            Vec2::new(30.0, 15.0),
            0.0,
            FillStrokeStyle::fill(Color::BLACK),
        ));
        scene.push(Element::rounded_rect(
            Rect::new(50.0, 50.0, 90.0, 70.0),
            5.0,
            FillStrokeStyle::fill(Color::BLACK),
        ));
        scene.push(Element::polygon(
            vec![
                Point::new(0.0, 0.0),
                Point::new(10.0, 0.0),
                Point::new(5.0, 10.0),
            ],
            FillStrokeStyle::fill(Color::BLACK),
        ));
        let out = render(&scene);
        assert!(out.contains("<ellipse cx=\"10.00\" cy=\"10.00\" rx=\"30.00\" ry=\"15.00\""));
        assert!(
            out.contains(r#"<rect x="50.00" y="50.00" width="40.00" height="20.00" rx="5.00""#)
        );
        assert!(out.contains("<polygon points=\"0.00,0.00 10.00,0.00 5.00,10.00\""));
    }

    #[test]
    fn arc_and_pie_native_path() {
        let mut scene = Scene::new(100.0, 100.0);
        scene.push(Element::arc(
            Point::new(50.0, 50.0),
            Vec2::new(40.0, 30.0),
            0.0,
            std::f64::consts::FRAC_PI_2,
            Stroke::new(Color::BLACK, 2.0),
        ));
        scene.push(Element::pie(
            Point::new(50.0, 50.0),
            30.0,
            0.0,
            std::f64::consts::FRAC_PI_2,
            FillStrokeStyle::fill(Color::BLACK),
        ));
        let out = render(&scene);
        // 圆弧用 SVG 原生 A 指令；扇形含圆心起点与 Z 闭合。
        assert!(out.contains("A 40.00 30.00"));
        assert!(out.contains("M 50.00 50.00 L "));
        assert!(out.contains(" Z"));
    }

    #[test]
    fn radial_gradient_and_dash() {
        let mut scene = Scene::new(100.0, 100.0);
        let radial = crate::scene::RadialGradient {
            center: Point::new(50.0, 50.0),
            radius: 20.0,
            stops: vec![
                GradientStop {
                    offset: 0.0,
                    color: Color::WHITE,
                },
                GradientStop {
                    offset: 1.0,
                    color: Color::BLACK,
                },
            ],
        };
        scene.push(Element::circle(
            Point::new(50.0, 50.0),
            20.0,
            FillStrokeStyle {
                fill: Some(radial.into()),
                stroke: Some(Stroke::dashed(Color::BLACK, 1.0, vec![3.0, 2.0])),
            },
        ));
        let out = render(&scene);
        assert!(out.contains("<radialGradient"));
        assert!(out.contains("gradientUnits=\"userSpaceOnUse\""));
        assert!(out.contains("stroke-dasharray=\"3.00,2.00\""));
        assert!(out.contains("stroke-miterlimit=\"10.00\""));
    }

    #[test]
    fn opacity_name_visibility() {
        let mut scene = Scene::new(100.0, 100.0);
        scene.push_node(
            SceneNode::new(Element::rect(
                Rect::new(0.0, 0.0, 10.0, 10.0),
                FillStrokeStyle::fill(Color::BLACK),
            ))
            .with_opacity(0.4)
            .with_name("a"),
        );
        scene.push_node(
            SceneNode::new(Element::rect(
                Rect::new(20.0, 0.0, 30.0, 10.0),
                FillStrokeStyle::fill(Color::BLACK),
            ))
            .with_visible(false),
        );
        let out = render(&scene);
        assert!(out.contains(r#"<g id="a">"#));
        assert!(out.contains(r#"<g opacity="0.400">"#));
        // 隐藏节点不应输出其矩形。
        assert!(!out.contains(r#"x="20.00""#));
    }

    #[test]
    fn text_escaping() {
        let mut scene = Scene::new(100.0, 100.0);
        scene.push(Element::text(
            "a <b> & \"c\"",
            Point::new(1.0, 2.0),
            TextStyle::new(Color::BLACK, 12.0, "sans-serif"),
        ));
        let out = render(&scene);
        assert!(out.contains("a &lt;b&gt; &amp; \"c\""));
    }

    #[test]
    fn text_align_anchor() {
        let mut scene = Scene::new(200.0, 100.0);
        let style = |align| TextStyle::new(Color::BLACK, 12.0, "sans-serif").with_align(align);
        scene.push(Element::text(
            "L",
            Point::new(10.0, 10.0),
            style(TextAlign::Left),
        ));
        scene.push(Element::text(
            "C",
            Point::new(100.0, 10.0),
            style(TextAlign::Center),
        ));
        scene.push(Element::text(
            "R",
            Point::new(190.0, 10.0),
            style(TextAlign::Right),
        ));
        let out = render(&scene);
        assert!(out.contains(r#"text-anchor="start""#));
        assert!(out.contains(r#"text-anchor="middle""#));
        assert!(out.contains(r#"text-anchor="end""#));
    }

    #[test]
    fn text_baseline_and_font_attrs() {
        let mut scene = Scene::new(200.0, 100.0);
        scene.push(Element::text(
            "b",
            Point::new(10.0, 10.0),
            TextStyle::new(Color::BLACK, 12.0, "sans-serif")
                .with_baseline(TextBaseline::Middle)
                .with_weight(700.0)
                .with_style(FontStyle::Italic)
                .with_line_height(18.0),
        ));
        let out = render(&scene);
        assert!(out.contains(r#"dominant-baseline="alphabetic""#));
        assert!(out.contains(r#"font-weight="700""#));
        assert!(out.contains(r#"font-style="italic""#));
        assert!(out.contains(r#"style="line-height:18.00""#));
    }

    #[test]
    fn node_and_layer_transform_matrix() {
        use crate::geometry::Transform;
        use crate::scene::Layer;
        let mut scene = Scene::new(100.0, 100.0);
        // 节点变换 → <g transform="matrix(...)">
        scene.push_node(
            SceneNode::new(Element::rect(
                Rect::new(0.0, 0.0, 10.0, 10.0),
                FillStrokeStyle::fill(Color::BLACK),
            ))
            .with_transform(Transform::translate(5.0, 7.0)),
        );
        // 图层变换
        scene.push_layer(Layer::new("l").with_transform(Transform::translate(1.0, 2.0)));
        let out = render(&scene);
        assert!(out.contains(r#"matrix(1.000000 0.000000 0.000000 1.000000 5.000000 7.000000)"#));
        assert!(out.contains(r#"matrix(1.000000 0.000000 0.000000 1.000000 1.000000 2.000000)"#));
    }

    #[test]
    fn layer_render_order_bottom_to_top() {
        use crate::scene::Layer;
        let mut scene = Scene::new(100.0, 100.0);
        // 默认层 nodes（最底）
        scene.push(Element::rect(
            Rect::new(0.0, 0.0, 10.0, 10.0),
            FillStrokeStyle::fill(Color::BLACK),
        ));
        scene.push_layer(
            Layer::new("bottom").with_nodes(vec![SceneNode::new(Element::rect(
                Rect::new(0.0, 0.0, 5.0, 5.0),
                FillStrokeStyle::fill(Color::BLACK),
            ))]),
        );
        scene.push_layer(
            Layer::new("top").with_nodes(vec![SceneNode::new(Element::circle(
                Point::new(0.0, 0.0),
                1.0,
                FillStrokeStyle::fill(Color::BLACK),
            ))]),
        );
        let out = render(&scene);
        let bg_idx = out.find("width=\"10.00\"").expect("默认层矩形");
        let bottom_idx = out.find(r#"<g id="bottom">"#).expect("bottom 图层");
        let top_idx = out.find(r#"<g id="top">"#).expect("top 图层");
        assert!(
            bg_idx < bottom_idx && bottom_idx < top_idx,
            "渲染顺序应为 默认层 < bottom < top"
        );
    }

    #[test]
    fn path_closed_vs_open() {
        use kurbo::BezPath;
        let mut scene = Scene::new(100.0, 100.0);
        // 开放路径
        let mut open = BezPath::new();
        open.move_to((0.0, 0.0));
        open.line_to((10.0, 0.0));
        scene.push(Element::Path {
            path: open,
            style: FillStrokeStyle::stroke(Color::BLACK, 1.0),
            closed: false,
            marker_end: None,
        });
        // 闭合路径
        let mut closed = BezPath::new();
        closed.move_to((20.0, 0.0));
        closed.line_to((30.0, 0.0));
        closed.line_to((25.0, 10.0));
        scene.push(Element::Path {
            path: closed,
            style: FillStrokeStyle::stroke(Color::BLACK, 1.0),
            closed: true,
            marker_end: None,
        });
        let out = render(&scene);
        // kurbo to_svg 输出格式：M0,0 L10,0（无空格、无小数补零）。
        // 开放路径 d 无 Z 闭合；闭合路径 d 以 Z 结尾。
        assert!(
            out.contains(r#"d="M0,0 L10,0""#),
            "开放路径 d 应为 M0,0 L10,0: {}",
            out
        );
        assert!(
            out.contains(r#"d="M20,0 L30,0 L25,10 Z""#),
            "闭合路径 d 应以 Z 结尾: {}",
            out
        );
    }

    #[test]
    fn marker_defs_emitted_only_when_used() {
        // 无 marker_end：文本/普通折线场景不应出现 marker（回归保护：箭头 marker 惰性输出）。
        let mut scene = Scene::new(100.0, 100.0);
        scene.push(Element::poly(
            vec![Point::new(0.0, 0.0), Point::new(10.0, 10.0)],
            Stroke::new(Color::BLACK, 1.0),
        ));
        let out = render(&scene);
        assert!(!out.contains("marker"), "无 marker_end 时不应输出 marker: {}", out);

        // 有 marker_end：惰性输出一次 <defs><marker id="arrow">，折线引用 url(#arrow)。
        let mut scene = Scene::new(100.0, 100.0);
        let mut el = Element::poly(
            vec![Point::new(0.0, 0.0), Point::new(10.0, 10.0)],
            Stroke::new(Color::BLACK, 1.0),
        );
        if let Element::Polyline { marker_end, .. } = &mut el {
            *marker_end = Some("arrow".to_string());
        }
        scene.push(el);
        let out = render(&scene);
        assert!(
            out.contains(r#"<marker id="arrow""#),
            "应输出箭头 marker: {}",
            out
        );
        assert!(
            out.contains(r#"marker-end="url(#arrow)""#),
            "折线应引用 marker: {}",
            out
        );
        // 同一 id 只输出一个 marker 定义。
        assert_eq!(out.matches(r#"<marker id="arrow""#).count(), 1);
    }

    #[test]
    fn prelaid_text_ignores_layout_in_svg() {
        use crate::text::RichSpan;
        use crate::text::measure_text;
        let mut scene = Scene::new(100.0, 100.0);
        let style = TextStyle::new(Color::BLACK, 12.0, "sans-serif");
        let m = measure_text(
            std::slice::from_ref(&RichSpan::new("hello", style.clone())),
            None,
        );
        scene.push(Element::Text {
            spans: vec![RichSpan::new("hello", style.clone())],
            position: Point::new(1.0, 2.0),
            style,
            layout: Some(m.layout),
        });
        let out = render(&scene);
        // SVG 从 spans 拼接纯文本输出 <text>，与预排版 layout 无关。
        assert!(out.contains(">hello</text>"));
        assert!(!out.contains("path"));
    }

    #[test]
    fn font_family_attr_is_css_normalized_and_xml_escaped() {
        use crate::text::TextStyle;
        let mut scene = Scene::new(100.0, 100.0);
        // 含空格族名 + 通用族 + 特殊字符，验证双层转义：
        // CSS 层（font_family_css）：加引号；XML 层（escape_attr）：& → &amp;
        let style = TextStyle::new(
            Color::BLACK,
            12.0,
            "Helvetica Neue, Rock & Roll, sans-serif",
        );
        scene.push(Element::text("x", Point::new(1.0, 2.0), style));
        let out = render(&scene);
        // 期望属性值：'Helvetica Neue', 'Rock &amp; Roll', sans-serif
        // （含空格 → 单引号；& 被 XML 转义；sans-serif 通用族不加引号）
        assert!(out.contains(r#"font-family="'Helvetica Neue', 'Rock &amp; Roll', sans-serif""#));
        // 单引号族名不应被 XML 转义（单引号在双引号属性内安全）
        assert!(!out.contains("&apos;"));
    }

    #[test]
    fn text_decoration_spacing_stretch_emitted() {
        use crate::text::TextStyle;
        let mut scene = Scene::new(100.0, 100.0);
        let style = TextStyle::new(Color::BLACK, 12.0, "sans-serif")
            .with_underline(true)
            .with_strikethrough(true)
            .with_letter_spacing(2.0)
            .with_font_width(0.75);
        scene.push(Element::text("x", Point::new(1.0, 2.0), style));
        let out = render(&scene);
        // 装饰 / 间距 / 字宽作为 SVG 属性输出。
        assert!(out.contains(r#"text-decoration="underline line-through""#));
        assert!(out.contains(r#"letter-spacing="2.00""#));
        assert!(out.contains(r#"font-stretch="75%""#));
    }

    #[test]
    fn clip_emits_clip_path_and_wraps_subtree() {
        use crate::scene::Clip;
        let mut scene = Scene::new(100.0, 100.0);
        // 圆角矩形裁剪一个红色矩形
        scene.push_node(
            SceneNode::new(Element::rect(
                Rect::new(0.0, 0.0, 50.0, 50.0),
                FillStrokeStyle::fill(Color::rgb(0xff, 0x00, 0x00)),
            ))
            .with_clip(Clip::circle(Point::new(25.0, 25.0), 10.0)),
        );
        let out = render(&scene);
        // 应有 <clipPath id="clip0"> 内为 <circle>
        assert!(out.contains(
            r#"<clipPath id="clip0"><circle cx="25.00" cy="25.00" r="10.00"/></clipPath>"#
        ));
        // 应有 <g clip-path="url(#clip0)"> 包裹
        assert!(out.contains(r#"<g clip-path="url(#clip0)">"#));
    }

    #[test]
    fn base64_encode_standard_vectors() {
        // RFC 4648 测试向量。
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    /// 构造一个实心单色 RGBA8 pixmap 图片。
    fn test_image(w: u32, h: u32, fill: u8) -> crate::SceneImage {
        let pixels = vec![fill; (w * h * 4) as usize];
        crate::SceneImage::from_rgba8(w, h, pixels).unwrap()
    }

    #[test]
    fn image_emits_data_uri_and_contain_meet() {
        let mut scene = Scene::new(100.0, 100.0);
        scene.push(Element::image(
            test_image(4, 4, 255),
            Rect::new(0.0, 0.0, 10.0, 20.0),
        ));
        let out = render(&scene);
        // 位图 → PNG → data:image/png;base64,<real PNG>
        assert!(out.contains(r#"href="data:image/png;base64,"#));
        assert!(out.contains(r#"width="10.00" height="20.00""#));
        assert!(out.contains(r#"preserveAspectRatio="xMidYMid meet""#));
    }

    #[test]
    fn image_object_fit_none_uses_raw_pixel_size() {
        let mut scene = Scene::new(100.0, 100.0);
        scene.push(Element::image(
            test_image(30, 40, 255).with_object_fit(crate::ObjectFit::None),
            Rect::new(5.0, 6.0, 105.0, 106.0),
        ));
        let out = render(&scene);
        // None：preserveAspectRatio="none" 且 width/height 用原始像素尺寸（30x40）。
        assert!(out.contains(r#"x="5.00" y="6.00" width="30.00" height="40.00""#));
        assert!(out.contains(r#"preserveAspectRatio="none""#));
    }

    #[test]
    fn image_object_fit_fill_maps_to_none() {
        let mut scene = Scene::new(100.0, 100.0);
        scene.push(Element::image(
            test_image(10, 10, 255).with_object_fit(crate::ObjectFit::Fill),
            Rect::new(0.0, 0.0, 10.0, 10.0),
        ));
        let out = render(&scene);
        // Fill：用 frame 尺寸 + preserveAspectRatio="none"。
        assert!(out.contains(r#"width="10.00" height="10.00""#));
        assert!(out.contains(r#"preserveAspectRatio="none""#));
    }

    #[test]
    fn image_respects_opacity() {
        let mut scene = Scene::new(100.0, 100.0);
        scene.push_node(
            SceneNode::from(Element::image(
                test_image(10, 10, 255),
                Rect::new(0.0, 0.0, 10.0, 10.0),
            ))
            .with_opacity(0.5),
        );
        let out = render(&scene);
        assert!(out.contains(r#"opacity="0.500""#));
    }

    #[test]
    fn text_baseline_shift_and_background_emitted() {
        use crate::text::TextStyle;
        let mut scene = Scene::new(200.0, 100.0);
        // 上标：baseline_shift 正 → baseline-shift 负（上移）。
        let sup = TextStyle::new(Color::BLACK, 12.0, "sans-serif").with_baseline_shift(6.0);
        scene.push(Element::text("x²", Point::new(10.0, 50.0), sup));
        // 行内背景：输出背景 <rect>。
        let code = TextStyle::new(Color::BLACK, 12.0, "sans-serif")
            .with_background_color(Some(Color::rgb(0xf0, 0xf0, 0xf0)));
        scene.push(Element::text("code", Point::new(30.0, 50.0), code));
        let out = render(&scene);
        assert!(out.contains("baseline-shift=\"-6.00px\""));
        assert!(out.contains("<rect "));
        assert!(out.contains("fill=\"#f0f0f0\""));
    }

    #[test]
    fn text_background_uses_layout_exact_size() {
        use crate::text::{RichSpan, TextStyle, measure_text};
        let style = TextStyle::new(Color::BLACK, 12.0, "sans-serif")
            .with_background_color(Some(Color::rgb(0xf0, 0xf0, 0xf0)));
        let m = measure_text(
            std::slice::from_ref(&RichSpan::new("code", style.clone())),
            None,
        );
        // 提供预排版 layout → 背景 <rect> 应使用精确的 layout 宽度。
        let mut scene = Scene::new(200.0, 100.0);
        scene.push(Element::Text {
            spans: vec![RichSpan::new("code", style.clone())],
            position: Point::new(30.0, 50.0),
            style,
            layout: Some(m.layout),
        });
        let out = render(&scene);
        let expect_w = format!("{:.2}", m.metrics.width);
        let expect_h = format!("{:.2}", m.metrics.height);
        assert!(
            out.contains(&format!("width=\"{}\" height=\"{}\"", expect_w, expect_h)),
            "背景矩形应使用 layout 精确尺寸，但输出含: {}",
            out
        );
    }

    #[test]
    fn text_no_background_when_none() {
        use crate::text::TextStyle;
        let mut scene = Scene::new(100.0, 100.0);
        scene.push(Element::text(
            "plain",
            Point::new(10.0, 50.0),
            TextStyle::new(Color::BLACK, 12.0, "sans-serif"),
        ));
        let out = render(&scene);
        // 无 background_color 时只有画布背景 1 个 <rect>（无文本背景矩形）。
        assert_eq!(out.matches("<rect ").count(), 1);
    }
}
