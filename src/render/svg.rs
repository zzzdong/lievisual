//! SVG vector output backend.
//!
//! Purely declarative, without depending on vello / parley. Maps each [`Element`] to native
//! SVG tags:
//! - Rect/Circle/Ellipse/RoundedRect/Line/Polyline/Polygon → the corresponding SVG shape element.
//! - Arc/Pie → `<path d="...">` (SVG's native arc command `A`).
//! - Path/GradientPath → kurbo's SVG path serialization (`d` attribute).
//! - Text → `<text>` element (no parley dependency; the `layout` field is ignored, the browser
//!   does the typesetting).
//!
//! Transforms are realized via `<g transform="...">`, decoupled from a backend state stack; a
//! node's `opacity` / `name` are emitted as `<g opacity>` / `<g id>`; the scene's `title` /
//! `description` / `scale` are captured at [`Renderer::render_scene`].
//!
//! Gradients uniformly use `gradientUnits="userSpaceOnUse"` (user coordinates, consistent with
//! the IR's absolute coordinates).

use crate::geometry::{Color, Point, Rect, Transform, Vec2};
use crate::render::Renderer;
use crate::scene::{
    Fill, FillStrokeStyle, GradientStop, LineCap, LineJoin, LinearGradient, Scene, Stroke,
};
use crate::text::TextStyle;
use std::fmt::Write;

/// SVG renderer: accumulates the SVG string.
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

    /// Output scale (pixel density): enlarges `width`/`height` while keeping `viewBox` unchanged.
    #[must_use]
    pub fn with_scale(mut self, scale: f64) -> Self {
        self.scale = scale;
        self
    }

    /// Document title.
    #[must_use]
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// Document description.
    #[must_use]
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Take the complete SVG document (including the `<svg>` root and background).
    pub fn into_string(self) -> String {
        let mut out = String::new();
        // scale only enlarges the output size; viewBox keeps the scene coordinates, achieving
        // higher-resolution export.
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
        // Background rectangle (viewBox coordinates, scaled with scale). Always output the
        // full-canvas background so the base color is correct (a transparent background is also
        // an overlay layer, matching common-library semantics).
        let _ = writeln!(
            out,
            r#"<rect x="0" y="0" width="{:.2}" height="{:.2}" fill="{}"/>"#,
            self.width,
            self.height,
            self.background.to_hex()
        );
        // content
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
        // Capture scene metadata, then reuse the default traversal.
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
        // Consistent with the PNG backend: derive non-negative width/height from the geometry
        // bounds (a Rect may have min > max).
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
        layout: Option<&crate::text::TextLayout>,
    ) {
        // SVG uses a native <text> element, typeset by the browser; layout is used only for the
        // background rectangle's exact dimensions. Plain text is derived by concatenating spans,
        // keeping glyph content consistent with the vello backend.
        let content = spans.iter().map(|s| s.text.as_str()).collect::<String>();
        // A pre-laid-out layout provides the text block's exact dimensions; fall back to an
        // approximation when absent.
        let metrics = layout.map(crate::text::layout_metrics);
        // Anchor: text-anchor (horizontal, maps to align) + dominant-baseline (vertical, maps to
        // baseline).
        let anchor = match style.align {
            crate::text::TextAlign::Left | crate::text::TextAlign::Justify => "start",
            crate::text::TextAlign::Center => "middle",
            crate::text::TextAlign::Right => "end",
        };
        // SVG's dominant-baseline has no "top"/"bottom" values; we uniformly convert to
        // alphabetic-baseline coordinates so browser typesetting matches the canvas fillText
        // semantics of the vello/parley backend. position.y is the anchor (its meaning decided by
        // baseline); first compute the text-block top, then derive the alphabetic baseline.
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
        // Optional typographic attributes: font width (font-stretch), letter spacing
        // (letter-spacing), text decoration (underline / line-through, output per the style's
        // block-level defaults).
        let fwidth = if style.font_width != crate::text::FontWidth::Normal {
            format!(
                r#" font-stretch="{:.0}%""#,
                style.font_width.ratio() * 100.0
            )
        } else {
            String::new()
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
        // Baseline shift (superscript / subscript): positive baseline-shift = up (superscript),
        // negative = down (subscript).
        let bshift = if style.baseline_shift != 0.0 {
            format!(r#" baseline-shift="{:.2}px""#, -style.baseline_shift)
        } else {
            String::new()
        };
        // Text background (inline highlight): prefer the pre-laid-out layout's exact width/height;
        // fall back to an approximation without layout.
        // width = metrics.width (with layout) or char_count*0.6*font-size; height = metrics.height
        // or font-size*1.3.
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
            // The background rectangle aligns with the text-block top, consistent with the vello
            // backend.
            let rx = position.x;
            let ry = block_top_y;
            // For right alignment, the background extends leftward.
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
            style.font_weight.value(),
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
        // Bitmap → PNG encode → data: URI (base64), output as <image>, the browser decodes it
        // and applies object-fit.
        let png = image.pixmap.to_png();
        let b64 = base64_encode(&png);
        let data_uri = format!("data:image/png;base64,{b64}");
        let op = if (opacity - 1.0).abs() > 1e-6 {
            format!(r#" opacity="{:.3}""#, opacity)
        } else {
            String::new()
        };
        // preserveAspectRatio controls object-fit; x/y/width/height are the display frame.
        // None: no scaling, place at the frame's top-left at original pixel size.
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

    fn push_name(&mut self, name: Option<&str>) {
        match name {
            Some(id) => self
                .buf
                .push_str(&format!(r#"<g id="{}">"#, escape_attr(id))),
            None => self.buf.push_str("<g>"),
        }
    }

    fn pop_name(&mut self) {
        self.buf.push_str("</g>");
    }

    fn push_clip(&mut self, clip: &crate::scene::Clip) {
        let id = self.clip_id;
        self.clip_id += 1;
        // Map a Clip to a native SVG element inside <clipPath>.
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
        // SVG shapes default to a black fill; stroke-only elements must explicitly turn off the
        // fill, otherwise the browser would fill the polyline/arc area black.
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

/// Build the SVG path `d` for an arc / pie sector (using the native arc command `A` rather than
/// a Bézier approximation).
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
    // y-down coordinate system: positive sweep = clockwise = SVG sweep-flag 1.
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

/// XML text escaping (for `<text>` content).
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

/// Minimal base64 encoding (for SVG `data:` URIs), avoiding an extra dependency.
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
    use crate::text::{FontStyle, FontWeight, FontWidth, TextAlign, TextBaseline, TextStyle};

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
        // scale enlarges the output size; viewBox keeps the scene coordinates.
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
        // Arcs use the SVG native A command; a pie sector includes the center start point and a
        // Z close.
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
        // Hidden nodes should not output their rectangle.
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
                .with_weight(FontWeight::Bold)
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
        // Node transform → <g transform="matrix(...)">
        scene.push_node(
            SceneNode::new(Element::rect(
                Rect::new(0.0, 0.0, 10.0, 10.0),
                FillStrokeStyle::fill(Color::BLACK),
            ))
            .with_transform(Transform::translate(5.0, 7.0)),
        );
        // Layer transform
        scene.push_layer(Layer::new("l").with_transform(Transform::translate(1.0, 2.0)));
        let out = render(&scene);
        assert!(out.contains(r#"matrix(1.000000 0.000000 0.000000 1.000000 5.000000 7.000000)"#));
        assert!(out.contains(r#"matrix(1.000000 0.000000 0.000000 1.000000 1.000000 2.000000)"#));
    }

    #[test]
    fn layer_render_order_bottom_to_top() {
        use crate::scene::Layer;
        let mut scene = Scene::new(100.0, 100.0);
        // Default layer nodes (bottom-most)
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
        // open path
        let mut open = BezPath::new();
        open.move_to((0.0, 0.0));
        open.line_to((10.0, 0.0));
        scene.push(Element::Path {
            path: open,
            style: FillStrokeStyle::stroke(Color::BLACK, 1.0),
            closed: false,
        });
        // closed path
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
        // kurbo to_svg output format: M0,0 L10,0 (no spaces, no trailing-zero decimals).
        // Open-path d has no Z close; closed-path d ends with Z.
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
        // SVG outputs <text> from concatenated spans, independent of the pre-laid-out layout.
        assert!(out.contains(">hello</text>"));
        assert!(!out.contains("path"));
    }

    #[test]
    fn font_family_attr_is_css_normalized_and_xml_escaped() {
        use crate::text::TextStyle;
        let mut scene = Scene::new(100.0, 100.0);
        // Space-containing family name + generic family + special characters, verifying the
        // two-layer escaping:
        // CSS layer (font_family_css): quoted; XML layer (escape_attr): & → &amp;
        let style = TextStyle::new(
            Color::BLACK,
            12.0,
            "Helvetica Neue, Rock & Roll, sans-serif",
        );
        scene.push(Element::text("x", Point::new(1.0, 2.0), style));
        let out = render(&scene);
        // Expected attribute value: 'Helvetica Neue', 'Rock &amp; Roll', sans-serif
        // (space → single quotes; & XML-escaped; sans-serif generic family unquoted)
        assert!(out.contains(r#"font-family="'Helvetica Neue', 'Rock &amp; Roll', sans-serif""#));
        // Single-quoted family names should not be XML-escaped (single quotes are safe inside
        // double-quoted attributes)
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
            .with_font_width(FontWidth::Condensed);
        scene.push(Element::text("x", Point::new(1.0, 2.0), style));
        let out = render(&scene);
        // Decoration / spacing / font width are output as SVG attributes.
        assert!(out.contains(r#"text-decoration="underline line-through""#));
        assert!(out.contains(r#"letter-spacing="2.00""#));
        assert!(out.contains(r#"font-stretch="75%""#));
    }

    #[test]
    fn clip_emits_clip_path_and_wraps_subtree() {
        use crate::scene::Clip;
        let mut scene = Scene::new(100.0, 100.0);
        // A rounded rectangle clipping a red rectangle
        scene.push_node(
            SceneNode::new(Element::rect(
                Rect::new(0.0, 0.0, 50.0, 50.0),
                FillStrokeStyle::fill(Color::rgb(0xff, 0x00, 0x00)),
            ))
            .with_clip(Clip::circle(Point::new(25.0, 25.0), 10.0)),
        );
        let out = render(&scene);
        // Should contain <clipPath id="clip0"> with a <circle> inside
        assert!(out.contains(
            r#"<clipPath id="clip0"><circle cx="25.00" cy="25.00" r="10.00"/></clipPath>"#
        ));
        // Should be wrapped in <g clip-path="url(#clip0)">
        assert!(out.contains(r#"<g clip-path="url(#clip0)">"#));
    }

    #[test]
    fn base64_encode_standard_vectors() {
        // RFC 4648 test vectors.
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    /// Build a solid single-color RGBA8 pixmap image.
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
        // bitmap → PNG → data:image/png;base64,<real PNG>
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
        // None: preserveAspectRatio="none" and width/height use the original pixel size (30x40).
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
        // Fill: use frame size + preserveAspectRatio="none".
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
        // Superscript: positive baseline_shift → negative baseline-shift (move up).
        let sup = TextStyle::new(Color::BLACK, 12.0, "sans-serif").with_baseline_shift(6.0);
        scene.push(Element::text("x²", Point::new(10.0, 50.0), sup));
        // Inline background: outputs a background <rect>.
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
        // Providing a pre-laid-out layout → the background <rect> should use the exact layout width.
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
        // Without background_color, only the canvas background <rect> exists (no text background rect).
        assert_eq!(out.matches("<rect ").count(), 1);
    }
}
