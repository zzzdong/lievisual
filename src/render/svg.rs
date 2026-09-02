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
    /// `None` → fall back to [`Scene::background`] (the Scene is the authoritative IR).
    background: Option<Color>,
    gradient_id: usize,
    clip_id: usize,
    title: Option<String>,
    description: Option<String>,
    /// `None` → fall back to [`Scene::scale`]; an explicit renderer value wins.
    scale: Option<f64>,
}

impl SvgRenderer {
    /// Create a renderer for an output canvas of `width × height` (scene units / px).
    ///
    /// A non-positive size means "unset": the value from the rendered [`Scene`] is then used,
    /// so `SvgRenderer::new(0.0, 0.0)` still produces a correctly sized document.
    pub fn new(width: f64, height: f64) -> Self {
        Self {
            buf: String::new(),
            width,
            height,
            background: None,
            gradient_id: 0,
            clip_id: 0,
            title: None,
            description: None,
            scale: None,
        }
    }

    /// Explicit background color; when unset, [`Scene::background`] is used.
    #[must_use]
    pub fn with_background(mut self, bg: Color) -> Self {
        self.background = Some(bg);
        self
    }

    /// Output scale (pixel density): enlarges `width`/`height` while keeping `viewBox` unchanged.
    ///
    /// Takes precedence over [`Scene::scale`]; when unset, the scene's scale is used.
    #[must_use]
    pub fn with_scale(mut self, scale: f64) -> Self {
        self.scale = Some(scale);
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
    ///
    /// Sizing contract (same as the raster backend): **the size given to
    /// [`SvgRenderer::new`] is the output size** — it lands verbatim in the root
    /// `width`/`height` attributes, and `viewBox` is that size divided by `scale`:
    ///
    /// ```text
    /// width/height = 输出尺寸            (SvgRenderer::new 传入)
    /// viewBox      = 0 0 (输出尺寸/scale) (绘制使用的用户坐标系)
    /// ```
    ///
    /// `scale` is therefore a *density* knob, applied symmetrically on both backends, and it
    /// never silently enlarges the output. To export a scene at 2× density, pass
    /// `new(w * 2.0, h * 2.0)` (the `new(0.0, 0.0)` fallback does this for you, treating
    /// `scene.width`/`scene.height` as logical size).
    pub fn into_string(self) -> String {
        let mut out = String::new();
        let scale = self.scale.unwrap_or(1.0);
        // viewBox 用"用户坐标系"：输出尺寸 / scale。scale=1 时与输出尺寸相同。
        let vw = self.width / scale;
        let vh = self.height / scale;
        let _ = writeln!(
            out,
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="{:.2}" height="{:.2}" viewBox="0 0 {:.2} {:.2}">"#,
            self.width, self.height, vw, vh
        );
        if let Some(t) = &self.title {
            let _ = writeln!(out, "<title>{}</title>", escape_text(t));
        }
        if let Some(d) = &self.description {
            let _ = writeln!(out, "<desc>{}</desc>", escape_text(d));
        }
        // Background rectangle (viewBox / user coordinates, so it always covers exactly the
        // logical canvas). Always output the full-canvas background so the base color is correct
        // (a transparent background is also an overlay layer, matching common-library semantics).
        let _ = write!(
            out,
            r#"<rect x="0" y="0" width="{:.2}" height="{:.2}""#,
            vw, vh
        );
        out.push_str(&color_attr_str(
            "fill",
            self.background.unwrap_or(Color::WHITE),
        ));
        let _ = writeln!(out, "/>");
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
        // Capture scene metadata, then reuse the default traversal. Values explicitly set on
        // the renderer take precedence; the rest falls back to the scene (the authoritative IR),
        // so `SvgRenderer::new(0.0, 0.0)` still yields a correctly sized document.
        self.title = scene.title.clone();
        self.description = scene.description.clone();
        // scale: an explicit renderer value wins, otherwise the scene's.
        let scale = self
            .scale
            .unwrap_or(if scene.scale.is_finite() && scene.scale > 0.0 {
                scene.scale
            } else {
                1.0
            });
        self.scale = Some(scale);
        if self.background.is_none() {
            self.background = Some(scene.background);
        }
        // Size fallback: treat `scene.width/height` as the **logical** size and multiply by
        // `scale` to get the output size — i.e. `new(0.0, 0.0)` behaves exactly like the
        // pre-existing "scale enlarges the export" behaviour, while an explicit size is honoured
        // verbatim as the output size.
        if self.width <= 0.0 {
            self.width = scene.width * scale;
        }
        if self.height <= 0.0 {
            self.height = scene.height * scale;
        }
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
        // A pre-laid-out layout provides the text block's exact dimensions.
        //
        // `Element::text` / `Element::rich_text` typeset eagerly, so the normal path always has
        // one. The `None` branch is only a fallback for a hand-built `Element::Text { layout: None
        // }` and is a **coarse `font_size`-based approximation** — it can overshoot the block
        // width by ~20%, so do not rely on it for background rectangles.
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
            let mut bg_el = String::new();
            let _ = write!(
                bg_el,
                r#"<rect x="{:.2}" y="{:.2}" width="{:.2}" height="{:.2}""#,
                bx,
                ry - style.baseline_shift,
                w,
                h
            );
            bg_el.push_str(&color_attr_str("fill", bg));
            bg_el.push_str("/>\n");
            self.buf.push_str(&bg_el);
        }
        let mut el = String::new();
        let _ = write!(
            el,
            r#"<text x="{:.2}" y="{:.2}" text-anchor="{}" dominant-baseline="{}" font-family="{}" font-size="{:.2}" font-style="{}" font-weight="{:.0}""#,
            position.x,
            alphabetic_y,
            anchor,
            baseline,
            escape_attr(&style.font_family_css()),
            style.font_size,
            fstyle,
            style.font_weight.value(),
        );
        el.push_str(&color_attr_str("fill", style.color));
        el.push_str(&fwidth);
        el.push_str(&letter);
        el.push_str(&deco);
        el.push_str(&lh);
        el.push_str(&rot);
        el.push_str(&bshift);
        el.push('>');
        if spans.len() > 1 || content.contains('\n') {
            // Rich text: emit one line-`<tspan>` per physical line and one style-`<tspan>`
            // per span fragment, so per-span color / size / weight / family and explicit
            // `\n` breaks survive (a single flat `<text>` cannot express them).
            el.push_str(&self.rich_text_tspans(spans, position.x, style, metrics.as_ref()));
        } else {
            // Single span without line breaks: keep the flat text output (no tspan).
            el.push_str(&escape_text(&content));
        }
        el.push_str("</text>\n");
        self.buf.push_str(&el);
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
            Some(Fill::Solid(c)) => el.push_str(&color_attr_str("fill", *c)),
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
        el.push_str(&color_attr_str("stroke", s.color));
        let _ = write!(
            el,
            r#" stroke-width="{:.2}" stroke-miterlimit="{:.2}""#,
            s.width, s.miter_limit
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

    /// Rich-text `<tspan>` body: one line-`<tspan>` per physical line, plus one style-`<tspan>`
    /// per span fragment inside each line.
    ///
    /// Multi-span colors / sizes / weights / families and explicit `\n` breaks are preserved,
    /// which a single flat `<text>` cannot express. Fragments that match the block style carry
    /// no attributes and inherit the outer `<text>`. Returns an empty string when there is no
    /// text to lay out.
    fn rich_text_tspans(
        &self,
        spans: &[crate::text::RichSpan],
        x: f64,
        base: &TextStyle,
        metrics: Option<&crate::text::TextMetrics>,
    ) -> String {
        // 行距：显式 line_height > 布局首行行高 > 1.2×font-size。
        let line_h = base.line_height.unwrap_or_else(|| {
            metrics
                .filter(|m| m.line_height > 0.0)
                .map_or(base.font_size * 1.2, |m| m.line_height)
        });
        // 切行：`\n` 是换行；同一行内的片段保持各自的样式。空行保留以维持行距。
        let mut lines: Vec<Vec<(&str, &TextStyle)>> = vec![Vec::new()];
        for span in spans {
            for (i, seg) in span.text.split('\n').enumerate() {
                if i > 0 {
                    lines.push(Vec::new());
                }
                lines
                    .last_mut()
                    .expect("lines starts with one empty line")
                    .push((seg, &span.style));
            }
        }
        // 文本末尾的换行不产生额外空行（与 parley 一致）。
        while lines.len() > 1 && lines.last().is_some_and(|l| l.is_empty()) {
            lines.pop();
        }
        let mut out = String::new();
        for (i, segs) in lines.iter().enumerate() {
            let dy = if i == 0 { 0.0 } else { line_h };
            let _ = write!(out, r#"<tspan x="{:.2}" dy="{:.2}">"#, x, dy);
            if segs.is_empty() {
                // 显式空行：零宽占位符保住行高（空 tspan 会被浏览器折叠）。
                out.push_str("&#8203;");
            } else {
                for (text, span_style) in segs {
                    let _ = write!(out, "<tspan{}>", self.rich_span_attrs(span_style, base));
                    out.push_str(&escape_text(text));
                    out.push_str("</tspan>");
                }
            }
            out.push_str("</tspan>");
        }
        out
    }

    /// Per-span typographic attribute overrides relative to the block style. Empty when the
    /// span matches the block (it then inherits the outer `<text>` attributes).
    fn rich_span_attrs(&self, span: &TextStyle, base: &TextStyle) -> String {
        let mut attrs = String::new();
        if span.font_family != base.font_family {
            let _ = write!(
                attrs,
                r#" font-family="{}""#,
                escape_attr(&span.font_family_css())
            );
        }
        if (span.font_size - base.font_size).abs() > f64::EPSILON {
            let _ = write!(attrs, r#" font-size="{:.2}""#, span.font_size);
        }
        if span.color != base.color {
            attrs.push_str(&color_attr_str("fill", span.color));
        }
        if span.font_weight != base.font_weight {
            let _ = write!(attrs, r#" font-weight="{:.0}""#, span.font_weight.value());
        }
        if span.font_style != base.font_style {
            let f = match span.font_style {
                crate::text::FontStyle::Normal => "normal",
                crate::text::FontStyle::Italic => "italic",
                crate::text::FontStyle::Oblique => "oblique",
            };
            let _ = write!(attrs, r#" font-style="{f}""#);
        }
        if span.font_width != base.font_width {
            let _ = write!(
                attrs,
                r#" font-stretch="{:.0}%""#,
                span.font_width.ratio() * 100.0
            );
        }
        if span.letter_spacing != base.letter_spacing {
            let _ = write!(attrs, r#" letter-spacing="{:.2}""#, span.letter_spacing);
        }
        // text-decoration 可继承；只有与块级默认不同才输出（含显式取消）。
        if span.underline != base.underline || span.strikethrough != base.strikethrough {
            let _ = write!(
                attrs,
                r#" text-decoration="{}""#,
                deco_keyword(span.underline, span.strikethrough)
            );
        }
        attrs
    }
}

/// SVG color attribute fragment: ` prefix="#rrggbb"`, with a translucent color also emitting
/// ` prefix-opacity="…"`.
///
/// 8-digit hex (`#rrggbbaa`) is CSS Color 4 / SVG 2 only and is rejected by many
/// SVG → PDF / SVG 1.1 consumers; the alpha channel is therefore always split into a separate
/// `fill-opacity` / `stroke-opacity` attribute (consistent with how gradient stops use
/// `stop-opacity`). An opaque color produces exactly the old ` prefix="#rrggbb"` form.
fn color_attr_str(prefix: &str, c: Color) -> String {
    // 注意：必须用 r## —— 字面量 `"#`（引号后的 # 属于 hex 前缀）会提前终止 r#" 字符串。
    let mut s = format!(r##" {prefix}="#{:02x}{:02x}{:02x}""##, c.r, c.g, c.b);
    if c.a != 255 {
        s.push_str(&format!(r#" {prefix}-opacity="{:.3}""#, c.a as f64 / 255.0));
    }
    s
}

/// text-decoration keyword for an underline / strikethrough pair (used per span and for the
/// block-level default).
fn deco_keyword(underline: bool, strikethrough: bool) -> &'static str {
    match (underline, strikethrough) {
        (false, false) => "none",
        (true, false) => "underline",
        (false, true) => "line-through",
        (true, true) => "underline line-through",
    }
}

/// A gradient stop: `stop-color` always uses the 6-digit form; a translucent stop carries its
/// alpha through `stop-opacity`. 8-digit hex is CSS Color 4 / SVG 2 only and is rejected by
/// many SVG -> PDF / SVG 1.1 consumers.
fn stop_svg(s: &GradientStop) -> String {
    let opacity = if s.color.a == 255 {
        String::new()
    } else {
        format!(r#" stop-opacity="{:.3}""#, s.color.a as f64 / 255.0)
    };
    format!(
        r##"<stop offset="{:.3}" stop-color="#{:02x}{:02x}{:02x}"{} />"##,
        s.offset, s.color.r, s.color.g, s.color.b, opacity
    )
}

/// Build the SVG path `d` for an arc / pie sector (using the native arc command `A` rather than
/// a Bézier approximation).
///
/// A full turn (|sweep| ≥ 360°) is emitted as **two** 180° arcs: per the SVG spec an `A`
/// segment whose start and end points coincide is dropped entirely, which would make a
/// 100%-slice pie (or a full circle stroke) render nothing on the SVG backend while the
/// raster backend draws a full circle.
fn arc_path_d(
    center: Point,
    radii: Vec2,
    start_angle: f64,
    sweep_angle: f64,
    sector: bool,
) -> String {
    let point_at = |angle: f64| -> (f64, f64) {
        (
            center.x + radii.x * angle.cos(),
            center.y + radii.y * angle.sin(),
        )
    };
    // 整圆（|sweep| ≥ 360°）拆成两段 180°：SVG 规范规定起终点重合的 A 段被整体忽略。
    let segments: Vec<(f64, f64)> = if sweep_angle.abs() >= std::f64::consts::TAU - 1e-9 {
        let half = sweep_angle / 2.0;
        vec![(start_angle, half), (start_angle + half, half)]
    } else {
        vec![(start_angle, sweep_angle)]
    };

    let mut d = String::new();
    let start = point_at(start_angle);
    if sector {
        // 扇形：M center L start [A ...]+ Z（与栅格端的 "M center L start [弧] end Z" 一致）。
        let _ = write!(
            d,
            "M {:.2} {:.2} L {:.2} {:.2}",
            center.x, center.y, start.0, start.1
        );
    } else {
        let _ = write!(d, "M {:.2} {:.2}", start.0, start.1);
    }
    for (a, s) in segments {
        // 大弧标志按单段弧度判断（拆分后每段恒为 180°，取 0 更稳妥）。
        let large_arc = if s.abs() > std::f64::consts::PI { 1 } else { 0 };
        // y-down 坐标系：正向 sweep = 顺时针 = SVG sweep-flag 1。
        let sweep_flag = if s >= 0.0 { 1 } else { 0 };
        let end = point_at(a + s);
        let _ = write!(
            d,
            " A {:.2} {:.2} 0 {} {} {:.2} {:.2}",
            radii.x, radii.y, large_arc, sweep_flag, end.0, end.1
        );
    }
    if sector {
        d.push_str(" Z");
    }
    d
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
        // 尺寸收敛后：渲染器尺寸 = 输出尺寸；viewBox = 输出尺寸 / scale。
        // 这里显式传 scene 尺寸 → 输出 100×50，用户坐标系 50×25。
        let out = render(&scene);
        assert!(out.contains("<title>T</title>"));
        assert!(out.contains("<desc>D</desc>"));
        assert!(out.contains(r#"width="100.00" height="50.00" viewBox="0 0 50.00 25.00""#));
    }

    /// 未显式给出尺寸时回落为"scene 尺寸 × scale"（保持"scale 放大导出"的旧行为）。
    #[test]
    fn size_fallback_multiplies_scene_by_scale() {
        let scene = Scene::new(100.0, 50.0).with_scale(2.0);
        let mut svg = SvgRenderer::new(0.0, 0.0);
        scene.render(&mut svg);
        let out = svg.into_string();
        assert!(
            out.contains(r#"width="200.00" height="100.00" viewBox="0 0 100.00 50.00""#),
            "回落应把 scene 尺寸 × scale 作为输出尺寸，实际: {out}"
        );
        // 背景矩形应覆盖整个用户坐标系（viewBox 尺寸），而非输出尺寸。
        assert!(out.contains(r##"width="100.00" height="50.00" fill="#ffffff""##));
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

    /// 缺陷回归：文本背景矩形必须用**真实测量**的尺寸，而非 `font_size` 估算。
    ///
    /// 此前 `Element::text()` 不排版（`layout: None`），SVG 端回落到估算
    /// （宽约 `len * font_size * 0.6`、高约 `font_size * 1.3`），实测对 "Hello World"@20px
    /// 给出 132×26，而真实排版是 109.74×27.24 —— **宽度误差约 20%**，背景框明显偏大，
    /// 且与 [`crate::fit::scene_bounds`]（用真实测量）不一致。
    ///
    /// 现在 `Element::text()` 构造时即排版，两个后端共享同一份布局。
    #[test]
    fn text_background_uses_real_measurement() {
        use crate::text::TextStyle;

        let style = TextStyle::new(Color::BLACK, 20.0, "sans-serif")
            .with_background_color(Some(Color::rgb(0xff, 0xff, 0x00)));
        let el = Element::text("Hello World", Point::new(10.0, 40.0), style);
        // 无可用字体时排版为空，跳过（沙箱 / 精简镜像中可能没有字体）。
        let measured_w = match &el {
            Element::Text {
                layout: Some(l), ..
            } => l.width,
            _ => return,
        };

        let mut scene = Scene::new(300.0, 80.0);
        scene.push(el);
        let out = render(&scene);

        // 文本背景矩形（黄色）的宽度应等于排版宽度。
        let bg = out
            .lines()
            .find(|l| l.contains("#ffff00"))
            .expect("应输出文本背景矩形");
        let w: f64 = bg
            .split("width=\"")
            .nth(1)
            .and_then(|s| s.split('"').next())
            .and_then(|s| s.parse().ok())
            .expect("背景矩形应有 width");
        assert!(
            (w - measured_w).abs() < 0.5,
            "背景矩形宽度应等于排版宽度 {measured_w:.2}，实际 {w:.2}"
        );
    }

    /// `Element::text()` 构造时即排版，两个后端与 [`crate::fit`] 共享同一份布局。
    #[test]
    fn element_text_prelayouts() {
        use crate::text::TextStyle;

        // 有内容 → 排版。
        let el = Element::text(
            "Hello",
            Point::new(0.0, 0.0),
            TextStyle::new(Color::BLACK, 16.0, "sans-serif"),
        );
        match &el {
            Element::Text { layout, .. } => {
                // 无字体环境下排版为空，此时不应 panic 而应降级为 None。
                if let Some(l) = layout {
                    assert!(!l.lines.is_empty());
                    assert!(l.width > 0.0);
                }
            }
            _ => panic!("expected Text"),
        }

        // 空文本 → 不排版（避免无谓分配）。
        let empty = Element::text(
            "",
            Point::new(0.0, 0.0),
            TextStyle::new(Color::BLACK, 16.0, "sans-serif"),
        );
        match &empty {
            Element::Text { layout, .. } => assert!(layout.is_none(), "空文本不应排版"),
            _ => panic!("expected Text"),
        }

        // rich_text 同理。
        let rich = Element::rich_text(
            vec![crate::text::RichSpan::new(
                "ab",
                TextStyle::new(Color::BLACK, 16.0, "sans-serif"),
            )],
            Point::new(0.0, 0.0),
            TextStyle::new(Color::BLACK, 16.0, "sans-serif"),
        );
        match &rich {
            Element::Text { layout, .. } => {
                if let Some(l) = layout {
                    assert!(!l.lines.is_empty());
                }
            }
            _ => panic!("expected Text"),
        }
        let empty_rich = Element::rich_text(
            vec![],
            Point::new(0.0, 0.0),
            TextStyle::new(Color::BLACK, 16.0, "sans-serif"),
        );
        match &empty_rich {
            Element::Text { layout, .. } => assert!(layout.is_none()),
            _ => panic!("expected Text"),
        }
    }

    /// Bug 回归：渲染器未显式设置尺寸 / 背景时，回落到 [`Scene`] 的元数据
    /// （此前 `SvgRenderer::new(0.0, 0.0)` 会输出 `width="0.00" height="0.00"`，
    /// 且 `Scene::background` 被完全忽略）。
    #[test]
    fn falls_back_to_scene_size_and_background() {
        let mut scene = Scene::new(300.0, 200.0).with_background(Color::rgb(0x00, 0x00, 0xff));
        scene.push(Element::rect(
            Rect::new(0.0, 0.0, 10.0, 10.0),
            FillStrokeStyle::fill(Color::RED),
        ));
        let mut svg = SvgRenderer::new(0.0, 0.0);
        scene.render(&mut svg);
        let out = svg.into_string();
        assert!(
            out.contains(r#"width="300.00" height="200.00" viewBox="0 0 300.00 200.00""#),
            "应回落到场景尺寸，实际输出: {out}"
        );
        assert!(
            out.contains(r##"fill="#0000ff""##),
            "应回落到场景背景色，实际输出: {out}"
        );
    }

    /// 渲染器显式设置的值优先于场景元数据（尺寸原样作为输出尺寸，scale 只影响 viewBox）。
    #[test]
    fn explicit_renderer_settings_win() {
        let scene = Scene::new(300.0, 200.0).with_scale(3.0);
        let mut svg = SvgRenderer::new(50.0, 40.0)
            .with_scale(2.0)
            .with_background(Color::GREEN);
        scene.render(&mut svg);
        let out = svg.into_string();
        assert!(out.contains(r#"width="50.00" height="40.00" viewBox="0 0 25.00 20.00""#));
        assert!(out.contains(r##"fill="#00ff00""##));
        // 显式 scale=2 应压过 scene 的 scale=3。
        assert!(
            !out.contains(r#"viewBox="0 0 16.67"#),
            "渲染器 scale 应优先: {out}"
        );
    }

    /// Bug 回归：整圆（|sweep| = 360°）在 SVG 中必须拆成两段 180° 弧。
    ///
    /// SVG 规范规定起终点重合的 `A` 段被整体忽略，单段写法会让 100% 扇形（整圆饼图）
    /// 在 SVG 端什么都不画，而栅格端画出整圆。
    #[test]
    fn full_circle_arc_is_split_in_two() {
        let d = arc_path_d(
            Point::new(50.0, 50.0),
            Vec2::new(30.0, 30.0),
            0.0,
            std::f64::consts::TAU,
            true,
        );
        assert_eq!(
            d.matches(" A ").count(),
            2,
            "整圆应拆成两段弧，实际 d = {d}"
        );
        assert!(d.starts_with("M 50.00 50.00 L "), "扇形应含中心点: {d}");
        assert!(d.ends_with(" Z"), "扇形应闭合: {d}");
        // 两段的起终点不能重合（否则又被忽略）。
        let open_pie = arc_path_d(
            Point::new(50.0, 50.0),
            Vec2::new(30.0, 30.0),
            0.0,
            std::f64::consts::TAU,
            false,
        );
        assert_eq!(open_pie.matches(" A ").count(), 2, "整圆弧: {open_pie}");
        // 非整圆仍是单段。
        let normal = arc_path_d(
            Point::new(50.0, 50.0),
            Vec2::new(30.0, 30.0),
            0.0,
            std::f64::consts::FRAC_PI_2,
            true,
        );
        assert_eq!(normal.matches(" A ").count(), 1, "普通扇形: {normal}");
    }

    /// 半透明渐变 stop：用 `stop-opacity` 表达 alpha（8 位 hex 在 SVG 1.1 / 部分
    /// SVG→PDF 工具链中不被识别）。
    #[test]
    fn gradient_stop_alpha_uses_stop_opacity() {
        let mut scene = Scene::new(100.0, 100.0);
        let grad = LinearGradient {
            start: Point::new(0.0, 0.0),
            end: Point::new(100.0, 0.0),
            stops: vec![
                GradientStop {
                    offset: 0.0,
                    color: Color::rgba(0xff, 0x00, 0x00, 128),
                },
                GradientStop {
                    offset: 1.0,
                    color: Color::rgb(0x00, 0x00, 0xff),
                },
            ],
        };
        scene.push(Element::rect(
            Rect::new(0.0, 0.0, 100.0, 100.0),
            FillStrokeStyle {
                fill: Some(grad.into()),
                stroke: None,
            },
        ));
        let out = render(&scene);
        assert!(
            out.contains(r##"stop-color="#ff0000" stop-opacity="0.502""##),
            "半透明 stop 应输出 stop-opacity: {out}"
        );
        assert!(
            out.contains(r##"stop-color="#0000ff" />"##),
            "不透明 stop 不应带 stop-opacity: {out}"
        );
    }

    /// 缺陷回归：富文本的每个 span 必须输出自己的样式（此前把 spans 拼成纯文本，
    /// 颜色/字号/字重全部退化为块级单一样式，SVG 与 vello 不一致）。
    #[test]
    fn rich_text_spans_emit_tspan_with_own_style() {
        use crate::text::RichSpan;
        let mut scene = Scene::new(300.0, 60.0);
        scene.push(Element::rich_text(
            vec![
                RichSpan::new(
                    "red ",
                    TextStyle::new(Color::rgb(0xff, 0x00, 0x00), 20.0, "sans-serif")
                        .with_weight(FontWeight::Bold),
                ),
                RichSpan::new(
                    "blue",
                    TextStyle::new(Color::rgb(0x00, 0x00, 0xff), 12.0, "sans-serif"),
                ),
            ],
            Point::new(10.0, 30.0),
            TextStyle::new(Color::BLACK, 12.0, "sans-serif"),
        ));
        let out = render(&scene);
        assert!(out.contains("<tspan"), "富文本应输出 tspan: {out}");
        assert!(
            out.contains(r##"fill="#ff0000""##)
                && out.contains(r##"font-size="20.00""##)
                && out.contains(r##"font-weight="700""##),
            "红色加粗 span 的样式应保留: {out}"
        );
        assert!(
            out.contains(r##"fill="#0000ff""##),
            "蓝色 span 的样式应保留: {out}"
        );
        assert!(out.contains(">red </tspan>"), "span 文本应完整: {out}");
        assert!(out.contains(">blue</tspan>"), "span 文本应完整: {out}");

        // 单 span、无换行的热路径保持纯文本输出（无 tspan）。
        let mut plain = Scene::new(100.0, 30.0);
        plain.push(Element::text(
            "plain",
            Point::new(1.0, 2.0),
            TextStyle::new(Color::BLACK, 12.0, "sans-serif"),
        ));
        let out_plain = render(&plain);
        assert!(
            !out_plain.contains("<tspan"),
            "纯文本不应输出 tspan: {out_plain}"
        );
    }

    /// 缺陷回归：文本内含 `\n` 时必须逐行输出 tspan（SVG `<text>` 本身不换行，
    /// 此前整串输出会退化为一行，与 vello 的多行布局不一致）。
    #[test]
    fn multiline_text_emits_line_tspans() {
        let mut scene = Scene::new(100.0, 100.0);
        scene.push(Element::text(
            "line1\nline2",
            Point::new(5.0, 10.0),
            TextStyle::new(Color::BLACK, 12.0, "sans-serif"),
        ));
        let out = render(&scene);
        assert!(out.contains(">line1</tspan>"), "第一行内容缺失: {out}");
        assert!(out.contains(">line2</tspan>"), "第二行内容缺失: {out}");
        // 每行一个行级 tspan，且首行 dy=0、后续行按行距下移。
        let line_tspans: Vec<&str> = out.matches(r#"<tspan x="5.00" dy="#).collect();
        assert!(line_tspans.len() >= 2, "应有两个行级 tspan: {out}");
    }

    /// 缺陷回归：半透明 fill/stroke 必须拆成 `#rrggbb` + `*-opacity`，不能输出
    /// SVG 1.1 / SVG→PDF 不支持的 8 位 hex。
    #[test]
    fn translucent_color_uses_fill_and_stroke_opacity() {
        let mut scene = Scene::new(40.0, 40.0);
        scene.push(Element::rect(
            Rect::new(0.0, 0.0, 40.0, 40.0),
            FillStrokeStyle {
                fill: Some(Color::rgba(0xff, 0x00, 0x00, 128).into()),
                stroke: Some(Stroke::new(Color::rgba(0x00, 0x00, 0xff, 128), 2.0)),
            },
        ));
        let out = render(&scene);
        assert!(
            out.contains(r##"fill="#ff0000" fill-opacity="0.502""##),
            "半透明 fill 应带 fill-opacity: {out}"
        );
        assert!(
            out.contains(r##"stroke="#0000ff" stroke-opacity="0.502""##),
            "半透明 stroke 应带 stroke-opacity: {out}"
        );
        assert!(
            !out.contains("#ff000080") && !out.contains("#0000ff80"),
            "不应输出 8 位 hex: {out}"
        );
    }
}
