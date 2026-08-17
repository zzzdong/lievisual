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
        // 背景矩形（viewBox 坐标，随 scale 缩放）
        let _ = writeln!(
            out,
            r#"<rect x="0" y="0" width="{:.2}" height="{:.2}" fill="{}"/>"#,
            self.width,
            self.height,
            self.background.to_hex()
        );
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
            rect.x, rect.y, rect.width, rect.height
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
        let mut el = String::new();
        let _ = write!(
            el,
            r#"<rect x="{:.2}" y="{:.2}" width="{:.2}" height="{:.2}" rx="{:.2}" "#,
            rect.x, rect.y, rect.width, rect.height, radius
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

    fn draw_polyline(&mut self, points: &[Point], style: &Stroke) {
        let pts: Vec<String> = points
            .iter()
            .map(|p| format!("{:.2},{:.2}", p.x, p.y))
            .collect();
        let mut el = String::new();
        let _ = write!(el, r#"<polyline points="{}" "#, pts.join(" "));
        self.apply_stroke_only(&mut el, style);
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

    fn draw_path(&mut self, path: &kurbo::BezPath, style: &FillStrokeStyle, closed: bool) {
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
        _layout: Option<&crate::text::TextLayout>,
    ) {
        // SVG 使用原生 <text>，由浏览器排版；忽略 parley layout。
        // 纯文本由 spans 拼接推导，保证与 vello 后端的字形内容一致。
        let content = spans.iter().map(|s| s.text.as_str()).collect::<String>();
        // 锚点：text-anchor（水平，对应 align）+ dominant-baseline（垂直，对应 baseline）。
        let anchor = match style.align {
            crate::text::TextAlign::Left => "start",
            crate::text::TextAlign::Center => "middle",
            crate::text::TextAlign::Right => "end",
        };
        let baseline = match style.baseline {
            crate::text::TextBaseline::Top => "top",
            crate::text::TextBaseline::Hanging => "hanging",
            crate::text::TextBaseline::Alphabetic => "alphabetic",
            crate::text::TextBaseline::Middle => "middle",
            crate::text::TextBaseline::Ideographic => "ideographic",
            crate::text::TextBaseline::Bottom => "bottom",
        };
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
        let mut el = String::new();
        let _ = write!(
            el,
            r#"<text x="{:.2}" y="{:.2}" text-anchor="{}" dominant-baseline="{}" font-family="{}" font-size="{:.2}" font-style="{}" font-weight="{:.0}" fill="{}"{}{}{}{}{}>{}</text>"#,
            position.x,
            position.y,
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
            escape_text(&content)
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

    fn push_name(&mut self, name: &str) {
        let m = format!(r#"<g id="{}">"#, escape_attr(name));
        self.buf.push_str(&m);
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
                rect.x, rect.y, rect.width, rect.height
            ),
            crate::scene::Clip::RoundedRect { rect, radius } => format!(
                r#"<rect x="{:.2}" y="{:.2}" width="{:.2}" height="{:.2}" rx="{:.2}"/>"#,
                rect.x, rect.y, rect.width, rect.height, radius
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
            Rect::new(50.0, 50.0, 40.0, 20.0),
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
                Rect::new(20.0, 0.0, 10.0, 10.0),
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
        assert!(out.contains(r#"dominant-baseline="middle""#));
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
}
