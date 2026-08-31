//! Rasterization backend based on `vello_cpu`.
//!
//! Renders a [`Scene`] into a `vello_cpu::Pixmap`, which can be further exported to PNG.
//! Text prefers the caller-provided [`text::TextLayout`]; if absent, it is laid out on the fly
//! via [`text::measure_text`].
//!
//! Notes:
//! - Filling uniformly goes through [`paint_path`](VelloPixmapRenderer::paint_path), supporting
//!   `Solid` / linear gradient / radial gradient; strokes support dashes (`dash_pattern`) and
//!   `miter_limit`.
//! - Transforms are maintained with an independent stack (`push_transform` composes with the
//!   parent transform, `pop_transform` restores it), supporting nested Groups.
//! - Node opacity is applied by multiplying the current `opacity` into the color's alpha.

use kurbo::{BezPath, Circle, Point as VPoint, Rect as VRect, Shape, Stroke as KurboStroke};
use vello_cpu::peniko::color::AlphaColor;
use vello_cpu::{Pixmap, RenderContext, Resources};

use crate::geometry::{Color, Point, Rect, Transform};
use crate::render::Renderer;
use crate::scene::{
    Fill, FillStrokeStyle, GradientStop, LineCap, LineJoin, LinearGradient, Scene, Stroke,
};
use crate::text::TextStyle;

/// vello_cpu rasterization renderer.
#[derive(Debug)]
pub struct VelloPixmapRenderer {
    ctx: RenderContext,
    resources: Resources,
    width: u32,
    height: u32,
    background: Option<Color>,
    /// Transform stack: top is the currently active composed transform.
    transform_stack: Vec<vello_cpu::kurbo::Affine>,
    /// Opacity stack: top is the currently active composed opacity (layers multiplied together).
    opacity_stack: Vec<f64>,
}

/// Clamp a canvas dimension (in device pixels) into vello_cpu's `1..=u16::MAX` range.
///
/// `RenderContext` / `Pixmap` both take `u16`, so an unclamped `as u16` would silently
/// truncate (e.g. 70000 → 4464) and produce a wrong-sized canvas.
fn clamp_canvas(v: f64) -> u16 {
    if !v.is_finite() || v < 1.0 {
        1
    } else {
        v.round().min(u16::MAX as f64) as u16
    }
}

/// Clamp a bitmap dimension (in pixels) into `1..=u16::MAX`.
fn clamp_px(v: f64) -> u32 {
    if !v.is_finite() || v < 1.0 {
        1
    } else {
        (v.ceil() as u32).min(u16::MAX as u32)
    }
}

impl VelloPixmapRenderer {
    /// Create a renderer with the given output canvas size (in device pixels).
    ///
    /// Sizes above `u16::MAX` are clamped (vello_cpu's canvas limit).
    pub fn new(width: u32, height: u32) -> Self {
        let w = width.min(u16::MAX as u32);
        let h = height.min(u16::MAX as u32);
        Self {
            ctx: RenderContext::new(w as u16, h as u16),
            resources: Resources::new(),
            width: w,
            height: h,
            background: None,
            transform_stack: Vec::new(),
            opacity_stack: Vec::new(),
        }
    }

    /// Explicit background color; when unset, [`Scene::background`] is used.
    ///
    /// A fully transparent color (`Color::TRANSPARENT`) keeps the canvas transparent
    /// (no background is painted).
    #[must_use]
    pub fn with_background(mut self, color: Color) -> Self {
        self.background = Some(color);
        self
    }

    /// Render the scene and return a Pixmap.
    ///
    /// Reuses the same `RenderContext` / `Resources` (glyph, atlas caches persist across frames);
    /// `reset()` clears the previous frame's scene before each render, so the same renderer can
    /// render multiple frames repeatedly.
    ///
    /// The output canvas is exactly the size given at construction (["renderer size = output
    /// size"](crate::render::SvgRenderer::into_string), same as the SVG backend). The scene is
    /// mapped onto the
    /// logical viewport `输出尺寸 / scene.scale` like SVG's default
    /// `preserveAspectRatio="xMidYMid meet"` (uniform scale plus centering), then that viewport is
    /// scaled up by `scene.scale` — i.e. `scale` is a pure *density* knob, never a silent
    /// enlargement of the canvas.
    pub fn render_scene_to_pixmap(&mut self, scene: &Scene) -> Pixmap {
        // Output canvas = the size given at construction (clamped to vello_cpu's u16 range).
        //
        // Deliberately **not** multiplied by `scene.scale`: downstream (e.g. liepress) builds the
        // scene at final pixel size and passes `scene.width` straight in while also recording
        // `scene.scale = dpi / 72` as metadata — applying it here would enlarge the canvas a
        // second time. Pixel density is the caller's choice via the constructor.
        let out_w = clamp_canvas(self.width as f64);
        let out_h = clamp_canvas(self.height as f64);
        // Clear the previous frame's scene and state (including transform/opacity stacks, to
        // avoid residue after an abnormal interruption of the previous frame).
        if self.ctx.width() != out_w || self.ctx.height() != out_h {
            self.ctx.reset_and_resize(out_w, out_h);
        } else {
            self.ctx.reset();
        }
        self.transform_stack.clear();
        self.opacity_stack.clear();

        // Background: fall back to the scene's background (the Scene is the authoritative IR);
        // skip painting an alpha-0 background so a transparent canvas stays possible.
        let bg = self.background.unwrap_or(scene.background);
        if bg.a > 0 {
            self.ctx
                .set_transform(vello_cpu::kurbo::Affine::IDENTITY);
            let rect = VRect::new(0.0, 0.0, out_w as f64, out_h as f64);
            self.ctx.set_paint(self.to_vello(&bg));
            self.ctx.fill_rect(&rect);
        }

        // Scene coordinates (scene.width × scene.height) must map to the pixel canvas
        // (out_w × out_h), with `scene.scale` acting as the pixel density — the exact contract
        // of the SVG backend, where the output size is `width/height` and the drawing coordinate
        // system is `viewBox = 输出尺寸 / scale`:
        //
        //   1. scene → viewBox (out/scale), via `preserveAspectRatio="xMidYMid meet"`
        //      (uniform scale + centering; scaling by width alone would overflow / crop the
        //      vertical axis whenever the aspect ratios differ).
        //   2. viewBox → canvas, uniformly by `scale`.
        let density = if scene.scale.is_finite() && scene.scale > 0.0 {
            scene.scale
        } else {
            1.0
        };
        let (vw, vh) = (out_w as f64 / density, out_h as f64 / density);
        let (s, tx, ty) = if scene.width > 0.0 && scene.height > 0.0 {
            let s = (vw / scene.width).min(vh / scene.height);
            (
                s,
                (vw - scene.width * s) / 2.0,
                (vh - scene.height * s) / 2.0,
            )
        } else {
            (1.0, 0.0, 0.0)
        };
        let fit = vello_cpu::kurbo::Affine::new([s, 0.0, 0.0, s, tx, ty]);
        let base = vello_cpu::kurbo::Affine::scale(density) * fit;
        self.ctx.set_transform(base);
        self.transform_stack.push(base);
        // Delegate the default traversal (sorting + Group recursion + node attributes + local
        // transform).
        self.render_scene(scene);
        if self.transform_stack.pop().is_some() {
            self.ctx.set_transform(vello_cpu::kurbo::Affine::IDENTITY);
        }

        let mut pixmap = Pixmap::new(out_w, out_h);
        self.ctx.render(&mut pixmap, &mut self.resources);
        pixmap
    }

    /// Render the scene and export PNG bytes (depends on the permanent `png` encoder).
    ///
    /// The PNG size equals the renderer's canvas size (see
    /// [`VelloPixmapRenderer::render_scene_to_pixmap`]); `scene.scale` is not applied.
    pub fn render_png(&mut self, scene: &Scene) -> Vec<u8> {
        let pixmap = self.render_scene_to_pixmap(scene);
        pixmap_to_png(&pixmap)
    }

    /// The currently active composed opacity (default 1.0).
    fn current_opacity(&self) -> f64 {
        self.opacity_stack.last().copied().unwrap_or(1.0)
    }

    /// Convert to a vello color, multiplying in the current opacity.
    fn to_vello(&self, color: &Color) -> AlphaColor<vello_cpu::color::Srgb> {
        // `color` stores exact 8-bit channels; only the alpha is modulated by opacity.
        // Round (instead of truncating) so e.g. 50% opacity maps to 128 rather than 127.
        let a = (color.a as f64 / 255.0 * self.current_opacity()).clamp(0.0, 1.0);
        AlphaColor::from_rgba8(color.r, color.g, color.b, (a * 255.0).round() as u8)
    }

    fn kurbo_stroke(stroke: &Stroke) -> KurboStroke {
        let cap = match stroke.line_cap {
            LineCap::Butt => kurbo::Cap::Butt,
            LineCap::Round => kurbo::Cap::Round,
            LineCap::Square => kurbo::Cap::Square,
        };
        let join = match stroke.line_join {
            LineJoin::Miter => kurbo::Join::Miter,
            LineJoin::Round => kurbo::Join::Round,
            LineJoin::Bevel => kurbo::Join::Bevel,
        };
        let mut k = KurboStroke {
            width: stroke.width,
            join,
            start_cap: cap,
            end_cap: cap,
            miter_limit: stroke.miter_limit,
            ..Default::default()
        };
        if !stroke.dash_array.is_empty() {
            k.dash_pattern = stroke.dash_array.iter().copied().collect();
            k.dash_offset = stroke.dash_offset;
        }
        k
    }

    /// Build peniko color stops carrying the current opacity.
    fn peniko_stops(&self, stops: &[GradientStop]) -> Vec<vello_cpu::peniko::ColorStop> {
        stops
            .iter()
            .map(|s| vello_cpu::peniko::ColorStop {
                offset: s.offset as f32,
                color: vello_cpu::peniko::color::DynamicColor::from_alpha_color(
                    self.to_vello(&s.color),
                ),
            })
            .collect()
    }

    /// Set a gradient fill (keeping user-space coordinates, cancelling node/layer transforms).
    fn paint_gradient(&mut self, kind: vello_cpu::peniko::GradientKind, stops: &[GradientStop]) {
        use vello_cpu::peniko::{
            ColorStops, Extend, Gradient, InterpolationAlphaSpace, color::ColorSpaceTag,
            color::HueDirection,
        };
        let stops_vec = self.peniko_stops(stops);
        let gradient = Gradient {
            kind,
            extend: Extend::Pad,
            interpolation_cs: ColorSpaceTag::Srgb,
            hue_direction: HueDirection::default(),
            interpolation_alpha_space: InterpolationAlphaSpace::Premultiplied,
            stops: ColorStops::from(stops_vec.as_slice()),
        };
        self.set_user_space_paint();
        self.ctx.set_paint(gradient);
    }

    /// The currently active scene (composed) transform: the top of the stack, or identity when
    /// no transform is applied.
    fn current_transform(&self) -> vello_cpu::kurbo::Affine {
        self.transform_stack
            .last()
            .copied()
            .unwrap_or(vello_cpu::kurbo::Affine::IDENTITY)
    }

    /// Set the paint transform so gradient coordinates stay in user (root) space, cancelling the
    /// `set_transform` already applied by nodes/layers (consistent with SVG
    /// `gradientUnits="userSpaceOnUse"`). Under a singular transform (det=0) the inverse is
    /// meaningless, so we fall back to the identity matrix (gradient follows the shape).
    fn set_user_space_paint(&mut self) {
        use vello_cpu::kurbo::Affine;
        let scene = self.current_transform();
        if scene.determinant().abs() > 1e-12 {
            self.ctx.set_paint_transform(scene.inverse());
        } else {
            self.ctx.set_paint_transform(Affine::IDENTITY);
        }
    }

    /// Set the current brush according to `Fill` (Solid / linear / radial gradient).
    fn set_fill(&mut self, fill: &Fill) {
        use vello_cpu::peniko::{GradientKind, LinearGradientPosition, RadialGradientPosition};
        match fill {
            Fill::Solid(c) => self.ctx.set_paint(self.to_vello(c)),
            Fill::LinearGradient(g) => {
                let kind = GradientKind::Linear(LinearGradientPosition::new(
                    VPoint::new(g.start.x, g.start.y),
                    VPoint::new(g.end.x, g.end.y),
                ));
                self.paint_gradient(kind, &g.stops);
            }
            Fill::RadialGradient(g) => {
                let kind = GradientKind::Radial(RadialGradientPosition::new(
                    VPoint::new(g.center.x, g.center.y),
                    g.radius as f32,
                ));
                self.paint_gradient(kind, &g.stops);
            }
        }
    }

    /// Unified drawing: fill (any type) + optional stroke (including dashes).
    fn paint_path(&mut self, path: &BezPath, style: &FillStrokeStyle) {
        if let Some(fill) = &style.fill {
            self.set_fill(fill);
            self.ctx.fill_path(path);
            // A gradient fill set a user-space paint transform; reset it after drawing.
            self.ctx.reset_paint_transform();
        }
        if let Some(s) = &style.stroke {
            self.ctx.set_paint(self.to_vello(&s.color));
            self.ctx.set_stroke(Self::kurbo_stroke(s));
            self.ctx.stroke_path(path);
        }
    }
}

impl Renderer for VelloPixmapRenderer {
    fn draw_rect(&mut self, rect: Rect, style: &FillStrokeStyle) {
        // 统一转路径绘制，以支持渐变填充与虚线描边。
        let mut path = BezPath::new();
        path.move_to((rect.min_x(), rect.min_y()));
        path.line_to((rect.max_x(), rect.min_y()));
        path.line_to((rect.max_x(), rect.max_y()));
        path.line_to((rect.min_x(), rect.max_y()));
        path.close_path();
        self.paint_path(&path, style);
    }

    fn draw_circle(&mut self, center: Point, radius: f64, style: &FillStrokeStyle) {
        let circle = Circle::new(VPoint::new(center.x, center.y), radius);
        let path = circle.to_path(0.1);
        self.paint_path(&path, style);
    }

    fn draw_line(&mut self, start: Point, end: Point, style: &Stroke) {
        let mut path = BezPath::new();
        path.move_to(VPoint::new(start.x, start.y));
        path.line_to(VPoint::new(end.x, end.y));
        self.ctx.set_paint(self.to_vello(&style.color));
        self.ctx.set_stroke(Self::kurbo_stroke(style));
        self.ctx.stroke_path(&path);
    }

    fn draw_polyline(&mut self, points: &[Point], style: &Stroke) {
        if points.len() < 2 {
            return;
        }
        let mut path = BezPath::new();
        path.move_to(VPoint::new(points[0].x, points[0].y));
        for p in &points[1..] {
            path.line_to(VPoint::new(p.x, p.y));
        }
        self.ctx.set_paint(self.to_vello(&style.color));
        self.ctx.set_stroke(Self::kurbo_stroke(style));
        self.ctx.stroke_path(&path);
    }

    fn draw_path(&mut self, path: &BezPath, style: &FillStrokeStyle, closed: bool) {
        let path = if closed {
            let mut p = path.clone();
            p.close_path();
            p
        } else {
            path.clone()
        };
        self.paint_path(&path, style);
    }

    fn draw_gradient_path(
        &mut self,
        path: &BezPath,
        gradient: &LinearGradient,
        stroke: Option<&Stroke>,
    ) {
        use vello_cpu::peniko::{GradientKind, LinearGradientPosition};
        let kind = GradientKind::Linear(LinearGradientPosition::new(
            VPoint::new(gradient.start.x, gradient.start.y),
            VPoint::new(gradient.end.x, gradient.end.y),
        ));
        self.paint_gradient(kind, &gradient.stops);
        self.ctx.fill_path(path);
        // 渐变 fill 设置了 user-space paint transform，绘制后复位。
        self.ctx.reset_paint_transform();

        if let Some(s) = stroke {
            self.ctx.set_paint(self.to_vello(&s.color));
            self.ctx.set_stroke(Self::kurbo_stroke(s));
            self.ctx.stroke_path(path);
        }
    }

    fn draw_image(&mut self, image: &crate::SceneImage, frame: Rect, opacity: f64) {
        let src = &image.pixmap;
        if src.width() == 0 || src.height() == 0 {
            return;
        }
        // 图片自身的 opacity 与节点 / 图层的 opacity 复合（其它图元都经 to_vello 乘入
        // 当前 opacity；此前图片只用了自身 opacity，导致节点 opacity 对图片无效）。
        let op = (opacity * self.current_opacity()).clamp(0.0, 1.0);
        if op <= 0.0 {
            return;
        }

        // frame 已处于 px 坐标（场景坐标即 px）；直接换算到设备像素。
        let fx = frame.min_x();
        let fy = frame.min_y();
        let fw = frame.width().max(1.0);
        let fh = frame.height().max(1.0);

        // 按 object_fit 计算图片在 frame 内的实际绘制矩形（局部坐标，相对 frame 原点）。
        let iw = src.width() as f64;
        let ih = src.height() as f64;
        let (dw, dh, offx, offy) = match image.object_fit {
            crate::ObjectFit::Fill => (fw, fh, 0.0, 0.0),
            crate::ObjectFit::None => (iw, ih, 0.0, 0.0),
            fit => {
                let scale = if fit == crate::ObjectFit::Cover {
                    (fw / iw).max(fh / ih)
                } else {
                    (fw / iw).min(fh / ih)
                };
                let dw = iw * scale;
                let dh = ih * scale;
                (dw, dh, (fw - dw) / 2.0, (fh - dh) / 2.0)
            }
        };

        // 画布之外的图片无需绘制：先做可见性剔除，省掉后续的大块位图分配与 blit。
        let t = self.current_transform();
        let (canvas_w, canvas_h) = (self.ctx.width() as f64, self.ctx.height() as f64);
        let corners = [
            t * VPoint::new(fx, fy),
            t * VPoint::new(fx + fw, fy),
            t * VPoint::new(fx, fy + fh),
            t * VPoint::new(fx + fw, fy + fh),
        ];
        let (min_x, max_x) = corners
            .iter()
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), p| {
                (a.min(p.x), b.max(p.x))
            });
        let (min_y, max_y) = corners
            .iter()
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), p| {
                (a.min(p.y), b.max(p.y))
            });
        if max_x <= 0.0 || min_x >= canvas_w || max_y <= 0.0 || min_y >= canvas_h {
            return;
        }

        // 中间位图的像素上限：frame / dest 以场景坐标给出，乘当前变换的实际缩放
        // （|det|^0.5）折算到设备像素。极端 frame（如 65535×65535）或"极小图 + Cover"
        // 会放大出 GB 级位图，这里统一按 1/k 降采样（k=1 时行为完全不变）。
        let mag = t.determinant().abs().sqrt().max(1e-6);
        let cap = (canvas_w * canvas_h * 4.0)
            .clamp(16.0 * 1024.0 * 1024.0, 64.0 * 1024.0 * 1024.0);
        let area = (dw * dh).max(fw * fh) * mag * mag;
        let k = if area > cap {
            (area / cap).sqrt().ceil().max(1.0)
        } else {
            1.0
        };

        // 目标帧位图（frame 大小，透明底）；cover/none 时图片可能溢出 frame，
        // 用 fill_rect 的裁剪范围（frame）裁掉溢出部分，省去显式 clip。
        let frame_w = clamp_px(fw / k);
        let frame_h = clamp_px(fh / k);
        let dest_w = clamp_px(dw / k);
        let dest_h = clamp_px(dh / k);
        // 缩放源位图（RGBA8）到目标尺寸。
        let rgba = resize_rgba8(src.pixels(), src.width(), src.height(), dest_w, dest_h);

        // 应用整体 opacity：预乘到 alpha 通道（frame 透明底，src-over 合成正确）。
        let mut dest_pixels: Vec<vello_cpu::color::PremulRgba8> = vec![
                vello_cpu::color::PremulRgba8::from_u8_array([0, 0, 0, 0]);
                (frame_w * frame_h) as usize
            ];
        let src_pixels: Vec<vello_cpu::color::PremulRgba8> = rgba
            .as_chunks::<4>()
            .0
            .iter()
            .map(|p| {
                let [r, g, b, a] = [p[0], p[1], p[2], p[3]];
                // 四舍五入而非截断，减少大批量像素上的系统性偏暗。
                let a = ((a as f64) * op).round() as u8;
                vello_cpu::color::PremulRgba8 {
                    r: premul(r, a),
                    g: premul(g, a),
                    b: premul(b, a),
                    a,
                }
            })
            .collect();

        // 把缩放后的图片 blit 到 frame 位图（考虑 offx/offy，越界部分被 frame 裁剪）。
        // 位图按 1/k 分辨率栅格化，故偏移量同样除以 k。
        let ox = (offx / k).floor() as i64;
        let oy = (offy / k).floor() as i64;
        for row in 0..(dest_h as i64) {
            let dy = row + oy;
            if dy < 0 || dy >= frame_h as i64 {
                continue;
            }
            for col in 0..(dest_w as i64) {
                let dx = col + ox;
                if dx < 0 || dx >= frame_w as i64 {
                    continue;
                }
                let sidx = (row * dest_w as i64 + col) as usize;
                let didx = (dy * frame_w as i64 + dx) as usize;
                dest_pixels[didx] = src_pixels[sidx];
            }
        }

        let dest_pixmap =
            vello_cpu::Pixmap::from_parts(dest_pixels, frame_w as u16, frame_h as u16);
        let brush = vello_cpu::Image {
            image: vello_cpu::ImageSource::Pixmap(std::sync::Arc::new(dest_pixmap)),
            sampler: vello_cpu::peniko::ImageSampler::default(),
        };
        self.ctx.set_paint(brush);
        // Image paint 以 pixmap 像素坐标 (0,0) 起始绘制；用 paint transform 把它平移到
        // frame 原点（frame 大小即为裁剪框）。位图按 1/k 分辨率栅格化，故再放大 k 倍
        // 铺回原尺寸的 frame（k=1 时等价于纯平移）。
        let rect = VRect::new(fx, fy, fx + fw, fy + fh);
        self.ctx.set_paint_transform(
            vello_cpu::kurbo::Affine::translate((fx, fy))
                * vello_cpu::kurbo::Affine::scale(k),
        );
        self.ctx.fill_rect(&rect);
        self.ctx.reset_paint_transform();
    }

    fn draw_text(
        &mut self,
        spans: &[crate::text::RichSpan],
        position: Point,
        style: &TextStyle,
        layout: Option<&crate::text::TextLayout>,
    ) {
        use vello_cpu::kurbo::Affine;

        // 优先使用调用者提供的 layout（直接借用，避免整体克隆）；否则经统一
        // TLS 上下文现场排版（layout_text）持有临时 layout。
        let owned_layout;
        let layout = match layout {
            Some(l) => l,
            None => {
                owned_layout = crate::text::layout_text(spans, style.max_width);
                &owned_layout
            }
        };

        // 局部变换：若节点带 transform，已由 push_transform 处理；这里仅处理文本自身旋转。
        // 锚点语义（canvas fillText）：把文本块在局部坐标中平移 (dx, dy)，使锚点落在
        // position。dx 由 align（水平），dy 由 baseline（垂直）决定；旋转绕锚点。
        let m = crate::text::layout_metrics(layout);
        let dx = match style.align {
            crate::text::TextAlign::Left | crate::text::TextAlign::Justify => 0.0,
            crate::text::TextAlign::Center => -m.width / 2.0,
            crate::text::TextAlign::Right => -m.width,
        };
        let dy = -style.baseline.anchor_offset(&m) - style.baseline_shift;
        let transform = Affine::translate((position.x, position.y))
            * Affine::rotate(style.rotation)
            * Affine::translate((dx, dy));

        // 文本背景（行内高亮）：在 layout 局部坐标 [0,0]×[w,h] 画矩形，经 transform
        // 映射到节点坐标，与 glyph 精确定位一致。
        if let Some(bg) = style.background_color {
            // kurbo Affine 不支持直接乘 Rect，手动变换对角点得 AABB。
            let p0 = transform * VPoint::new(0.0, 0.0);
            let p1 = transform * VPoint::new(m.width, m.height);
            let bg_rect = Rect::from_points(p0, p1);
            let style = crate::scene::FillStrokeStyle::fill(bg);
            self.draw_rect(bg_rect, &style);
        }

        for line in &layout.lines {
            for run in &line.runs {
                let glyphs: Vec<vello_cpu::Glyph> = run
                    .glyphs
                    .iter()
                    .map(|g| vello_cpu::Glyph {
                        id: g.id,
                        x: g.x,
                        y: g.y,
                    })
                    .collect();
                if glyphs.is_empty() {
                    continue;
                }

                // 从自闭环的字体字节构造 vello 字体（与 parley/vello 共享同一 peniko FontData）。
                let font_data = vello_cpu::peniko::FontData::new(
                    vello_cpu::peniko::Blob::from(run.font_data.as_ref().clone()),
                    0,
                );
                self.ctx.set_paint(self.to_vello(&run.color));
                self.ctx
                    .glyph_run(&mut self.resources, &font_data)
                    .font_size(run.font_size)
                    .glyph_transform(transform)
                    .fill_glyphs(glyphs.into_iter());
            }
        }
    }

    fn push_transform(&mut self, t: Transform) {
        use vello_cpu::kurbo::Affine;
        let a = Affine::new([t.a, t.b, t.c, t.d, t.e, t.f]);
        let combined = match self.transform_stack.last() {
            Some(parent) => *parent * a,
            None => a,
        };
        self.ctx.set_transform(combined);
        self.transform_stack.push(combined);
    }

    fn pop_transform(&mut self) {
        self.transform_stack.pop();
        match self.transform_stack.last() {
            Some(t) => self.ctx.set_transform(*t),
            None => self.ctx.set_transform(vello_cpu::kurbo::Affine::IDENTITY),
        }
    }

    fn push_opacity(&mut self, opacity: f64) {
        let cur = self.current_opacity();
        self.opacity_stack.push(cur * opacity.clamp(0.0, 1.0));
    }

    fn pop_opacity(&mut self) {
        self.opacity_stack.pop();
    }

    fn push_clip(&mut self, clip: &crate::scene::Clip) {
        // vello_cpu 的 clip 路径按当前 transform（clip_path_transform）应用；
        // 我们 push_transform 已设置 ctx transform，故 clip 与节点变换一致复合。
        let path = clip.to_bezpath();
        self.ctx.push_clip_path(&path);
    }

    fn pop_clip(&mut self) {
        self.ctx.pop_clip_path();
    }
}

/// 预乘 alpha 的分量还原为直通（非预乘）分量：`c = premul_c * 255 / a`（四舍五入）。
///
/// vello 的 `Pixmap` 保存的是 **预乘** alpha 像素，而 PNG（以及本 crate 的
/// [`crate::Pixmap`] RGBA8 约定）使用非预乘 alpha：直接把预乘值写进 PNG 会让所有
/// alpha<255 的像素（抗锯齿边缘、半透明填充、文字边缘）整体偏暗。
fn unpremultiply(c: u8, a: u8) -> u8 {
    if a == 0 {
        0
    } else if a == 255 {
        c
    } else {
        (((c as u32 * 255) + (a as u32 / 2)) / a as u32).min(255) as u8
    }
}

/// vello Pixmap → 非预乘 RGBA8 字节。
fn pixmap_to_rgba8(pixmap: &Pixmap) -> Vec<u8> {
    let mut data = Vec::with_capacity(pixmap.data().len() * 4);
    for p in pixmap.data() {
        data.extend_from_slice(&[
            unpremultiply(p.r, p.a),
            unpremultiply(p.g, p.a),
            unpremultiply(p.b, p.a),
            p.a,
        ]);
    }
    data
}

/// 将 vello Pixmap 转为 PNG 字节（预乘 RGBA → 非预乘 RGBA8 → PNG，经 `png` 常驻依赖）。
fn pixmap_to_png(pixmap: &Pixmap) -> Vec<u8> {
    let (w, h) = (pixmap.width() as u32, pixmap.height() as u32);
    let data = pixmap_to_rgba8(pixmap);
    let mut out = Vec::with_capacity(data.len() + 64);
    {
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().expect("PNG 编码：写入头失败");
        writer
            .write_image_data(&data)
            .expect("PNG 编码：写入像素失败");
    }
    out
}

/// 通道值按 alpha 预乘（`c * a / 255`，四舍五入）。
fn premul(c: u8, a: u8) -> u8 {
    ((c as u32 * a as u32 + 127) / 255).min(255) as u8
}

/// 双线性缩放 RGBA8 位图（`src` 尺寸 `sw×sh` → `dw×dh`）。
///
/// 用双线性插值替代 `image` 的 `resize_exact`（移除解码依赖后自实现，够图表场景用）。
///
/// 插值在 **预乘** 空间进行：直通 RGBA 直接插值会让半透明像素与透明像素混合出偏暗的
/// 边缘（halo），预乘后再插值、最后还原为直通值是正确做法。
fn resize_rgba8(src: &[u8], sw: u32, sh: u32, dw: u32, dh: u32) -> Vec<u8> {
    if sw == dw && sh == dh {
        return src.to_vec();
    }
    let mut out = vec![0u8; (dw as usize) * (dh as usize) * 4];
    let sx = sw as f64 / dw as f64;
    let sy = sh as f64 / dh as f64;
    for dy in 0..dh {
        let fy = (dy as f64 + 0.5) * sy - 0.5;
        let y0 = fy.floor().max(0.0) as u32;
        let y1 = (y0 + 1).min(sh - 1);
        let wy = (fy - fy.floor()).clamp(0.0, 1.0);
        for dx in 0..dw {
            let fx = (dx as f64 + 0.5) * sx - 0.5;
            let x0 = fx.floor().max(0.0) as u32;
            let x1 = (x0 + 1).min(sw - 1);
            let wx = (fx - fx.floor()).clamp(0.0, 1.0);
            // 四个采样像素（RGBA8，u32 直接读取）。
            let p00 = &src[((y0 * sw + x0) as usize) * 4..];
            let p01 = &src[((y0 * sw + x1) as usize) * 4..];
            let p10 = &src[((y1 * sw + x0) as usize) * 4..];
            let p11 = &src[((y1 * sw + x1) as usize) * 4..];
            let out_idx = ((dy * dw + dx) as usize) * 4;
            // 采样点先按各自 alpha 预乘，再对 RGB 插值。
            let premul = |px: &[u8]| -> [f64; 3] {
                let a = px[3] as f64 / 255.0;
                [
                    px[0] as f64 * a,
                    px[1] as f64 * a,
                    px[2] as f64 * a,
                ]
            };
            let (q00, q01, q10, q11) = (premul(p00), premul(p01), premul(p10), premul(p11));
            // alpha 通道单独做直通插值。
            let a_top = p00[3] as f64 * (1.0 - wx) + p01[3] as f64 * wx;
            let a_bot = p10[3] as f64 * (1.0 - wx) + p11[3] as f64 * wx;
            let a = (a_top * (1.0 - wy) + a_bot * wy).round().clamp(0.0, 255.0) as u8;
            out[out_idx + 3] = a;
            for c in 0..3 {
                let top = q00[c] * (1.0 - wx) + q01[c] * wx;
                let bot = q10[c] * (1.0 - wx) + q11[c] * wx;
                let v = (top * (1.0 - wy) + bot * wy).round().clamp(0.0, 255.0) as u8;
                // 由预乘值还原为直通值。
                out[out_idx + c] = unpremultiply(v, a);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::{Color, Point, Rect, Transform};
    use crate::scene::{Element, FillStrokeStyle, GradientStop, LinearGradient, Scene, SceneNode};

    /// 提取 pixmap 像素为 RGBA u8 数组（非预乘，用于比较）。
    fn pixels(pixmap: &Pixmap) -> Vec<u8> {
        pixmap_to_rgba8(pixmap)
    }

    /// 渲染并解码为 RGBA8（走 render_png 全链路，覆盖 PNG 编码）。
    fn render_png_pixels(renderer: &mut VelloPixmapRenderer, scene: &Scene) -> (u32, u32, Vec<u8>) {
        let png = renderer.render_png(scene);
        let pm = crate::Pixmap::decode_png(&png).expect("PNG 解码失败");
        (pm.width(), pm.height(), pm.pixels().to_vec())
    }

    /// 一个横跨矩形、左右颜色差异明显的线性渐变矩形。
    /// `rect` 为矩形坐标；`t` 为节点局部变换（None = 无）。
    fn gradient_rect_scene(rect: Rect, t: Option<Transform>) -> Scene {
        let grad = LinearGradient {
            start: Point::new(10.0, 10.0),
            end: Point::new(90.0, 10.0),
            stops: vec![
                GradientStop {
                    offset: 0.0,
                    color: Color::rgb(0xff, 0x00, 0x00),
                },
                GradientStop {
                    offset: 1.0,
                    color: Color::rgb(0x00, 0x00, 0xff),
                },
            ],
        };
        let style = FillStrokeStyle {
            fill: Some(grad.into()),
            stroke: None,
        };
        let mut scene = Scene::new(100.0, 100.0);
        let node = SceneNode::new(Element::rect(rect, style));
        scene.push_node(match t {
            Some(t) => node.with_transform(t),
            None => node,
        });
        scene
    }

    /// Bug 1 回归：渐变坐标为用户（根）空间，不受节点局部 transform 影响。
    ///
    /// 场景 A：矩形直接画在 (10,10)，无变换，渐变轴 (10,10)->(90,10)。
    /// 场景 B：矩形在 (0,0)，节点 transform=translate(10,10)，渐变轴仍 (10,10)->(90,10)。
    /// 两者在屏幕上完全一致（图形位置、渐变轴都相同）→ 像素应相同。
    /// 修复前渐变会随 transform 平移，B 的渐变轴变为相对图形偏移，像素不同。
    #[test]
    fn gradient_stays_in_user_space_under_node_transform() {
        let mut ra = VelloPixmapRenderer::new(100, 100);
        let mut rb = VelloPixmapRenderer::new(100, 100);
        let scene_a = gradient_rect_scene(Rect::new(10.0, 10.0, 90.0, 90.0), None);
        let scene_b = gradient_rect_scene(
            Rect::new(0.0, 0.0, 80.0, 80.0),
            Some(Transform::translate(10.0, 10.0)),
        );
        let a = pixels(&ra.render_scene_to_pixmap(&scene_a));
        let b = pixels(&rb.render_scene_to_pixmap(&scene_b));
        assert_eq!(a.len(), b.len());
        // 允许极少量抗锯齿差异（<0.1% 通道字节）。
        let diff = a.iter().zip(b.iter()).filter(|(x, y)| x != y).count();
        assert!(
            diff < a.len() / 1000,
            "渐变在节点 transform 下应保持用户空间，但 {} 个通道字节不同",
            diff
        );
    }

    /// Bug 2 回归：同一渲染器可复用渲染多帧，结果一致（reset 清场 + Resources 复用）。
    #[test]
    fn renderer_reusable_across_frames() {
        let mut renderer = VelloPixmapRenderer::new(60, 60).with_background(Color::WHITE);
        let scene = gradient_rect_scene(Rect::new(5.0, 5.0, 55.0, 55.0), None);

        let p1 = renderer.render_scene_to_pixmap(&scene);
        let p2 = renderer.render_scene_to_pixmap(&scene);

        let d1 = pixels(&p1);
        let d2 = pixels(&p2);
        assert_eq!(d1, d2, "多帧渲染结果应一致");
        // 非全白背景：渐变矩形应覆盖部分像素。
        let non_white = d1
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|px| px[0] < 250 || px[1] < 250 || px[2] < 250)
            .count();
        assert!(non_white > 0, "渐变矩形未渲染出来");
    }

    /// 统计 pixmap 中近似等于给定 RGB 的像素数（容差 tol）。
    fn count_color(pixmap: &Pixmap, rgb: (u8, u8, u8), tol: u8) -> usize {
        pixmap
            .data()
            .iter()
            .filter(|p| {
                p.r.abs_diff(rgb.0) <= tol
                    && p.g.abs_diff(rgb.1) <= tol
                    && p.b.abs_diff(rgb.2) <= tol
            })
            .count()
    }

    /// 读取 (x, y) 处的像素（RGBA）。越界返回 (0,0,0,0)。
    fn pixel_at(pixmap: &Pixmap, x: u32, y: u32) -> (u8, u8, u8, u8) {
        let w = pixmap.width() as u32;
        let h = pixmap.height() as u32;
        if x >= w || y >= h {
            return (0, 0, 0, 0);
        }
        let p = &pixmap.data()[(y * w + x) as usize];
        (p.r, p.g, p.b, p.a)
    }

    /// 覆盖：圆/椭圆/圆角矩形/多边形/圆弧/扇形 在 vello 端都能渲染出对应颜色。
    #[test]
    fn primitives_render() {
        use crate::geometry::Vec2;
        let mut scene = Scene::new(150.0, 100.0);
        scene.push(Element::circle(
            Point::new(20.0, 20.0),
            10.0,
            FillStrokeStyle::fill(Color::rgb(0xff, 0x00, 0x00)),
        ));
        scene.push(Element::ellipse(
            Point::new(50.0, 20.0),
            Vec2::new(15.0, 8.0),
            0.0,
            FillStrokeStyle::fill(Color::rgb(0x00, 0xff, 0x00)),
        ));
        scene.push(Element::rounded_rect(
            Rect::new(75.0, 10.0, 105.0, 30.0),
            5.0,
            FillStrokeStyle::fill(Color::rgb(0x00, 0x00, 0xff)),
        ));
        scene.push(Element::polygon(
            vec![
                Point::new(5.0, 60.0),
                Point::new(30.0, 60.0),
                Point::new(17.0, 85.0),
            ],
            FillStrokeStyle::fill(Color::rgb(0xff, 0xff, 0x00)),
        ));
        scene.push(Element::arc(
            Point::new(70.0, 70.0),
            Vec2::new(20.0, 20.0),
            0.0,
            std::f64::consts::PI,
            Stroke::new(Color::rgb(0x00, 0xff, 0xff), 3.0),
        ));
        scene.push(Element::pie(
            Point::new(110.0, 60.0),
            20.0,
            0.0,
            std::f64::consts::FRAC_PI_2,
            FillStrokeStyle::fill(Color::rgb(0xff, 0x00, 0xff)),
        ));
        let mut r = VelloPixmapRenderer::new(150, 100);
        let pix = r.render_scene_to_pixmap(&scene);
        // 每种填充色都应出现。
        assert!(count_color(&pix, (0xff, 0x00, 0x00), 20) > 0, "圆未渲染");
        assert!(count_color(&pix, (0x00, 0xff, 0x00), 20) > 0, "椭圆未渲染");
        assert!(
            count_color(&pix, (0x00, 0x00, 0xff), 20) > 0,
            "圆角矩形未渲染"
        );
        assert!(
            count_color(&pix, (0xff, 0xff, 0x00), 20) > 0,
            "多边形未渲染"
        );
        assert!(count_color(&pix, (0x00, 0xff, 0xff), 20) > 0, "圆弧未渲染");
        assert!(count_color(&pix, (0xff, 0x00, 0xff), 20) > 0, "扇形未渲染");
    }

    /// 虚线描边：实线与虚线在同一路径上，虚线段数量少于实线（分段断裂）。
    #[test]
    fn dashed_stroke_renders() {
        let mut scene_solid = Scene::new(100.0, 20.0);
        scene_solid.push(Element::line(
            Point::new(5.0, 10.0),
            Point::new(95.0, 10.0),
            Stroke::new(Color::BLACK, 4.0),
        ));
        let mut scene_dashed = Scene::new(100.0, 20.0);
        scene_dashed.push(Element::line(
            Point::new(5.0, 10.0),
            Point::new(95.0, 10.0),
            Stroke::dashed(Color::BLACK, 4.0, vec![8.0, 8.0]),
        ));
        let mut rs = VelloPixmapRenderer::new(100, 20).with_background(Color::WHITE);
        let mut rd = VelloPixmapRenderer::new(100, 20).with_background(Color::WHITE);
        let ps = rs.render_scene_to_pixmap(&scene_solid);
        let pd = rd.render_scene_to_pixmap(&scene_dashed);
        // 沿中线统计黑色不透明像素（排除白色背景）。
        let black_on_line = |pix: &Pixmap| {
            (0..100u32)
                .filter(|&x| {
                    let (r, g, b, a) = pixel_at(pix, x, 10);
                    r < 100 && g < 100 && b < 100 && a > 200
                })
                .count()
        };
        let solid = black_on_line(&ps);
        let dashed = black_on_line(&pd);
        // 实线几乎全段覆盖，虚线约一半。
        assert!(solid > 80, "实线应覆盖中线大部分, got {solid}");
        assert!(
            dashed < solid,
            "虚线覆盖应少于实线, solid={solid} dashed={dashed}"
        );
        assert!(dashed > 5, "虚线应仍有可见段, got {dashed}");
    }

    /// 节点 opacity：半透明绿覆盖红 → 中心为混合色（≈(128,128,0)）。
    #[test]
    fn opacity_blends_pixels() {
        let mut scene = Scene::new(20.0, 20.0);
        scene.push_node(
            SceneNode::new(Element::rect(
                Rect::new(0.0, 0.0, 20.0, 20.0),
                FillStrokeStyle::fill(Color::rgb(0xff, 0x00, 0x00)),
            ))
            .with_z(0),
        );
        scene.push_node(
            SceneNode::new(Element::rect(
                Rect::new(0.0, 0.0, 20.0, 20.0),
                FillStrokeStyle::fill(Color::rgb(0x00, 0xff, 0x00)),
            ))
            .with_z(1)
            .with_opacity(0.5),
        );
        let mut r = VelloPixmapRenderer::new(20, 20);
        let pix = r.render_scene_to_pixmap(&scene);
        let (r_, g, b, _) = pixel_at(&pix, 10, 10);
        // 50% 绿 over 红 → 期望 (128,128,0)；容差 40。
        assert!(r_.abs_diff(128) <= 40, "R={r_}");
        assert!(g.abs_diff(128) <= 40, "G={g}");
        assert!(b <= 40, "B={b}");
    }

    /// 节点 visible=false：整棵子树跳过，不产生像素。
    #[test]
    fn visibility_skips_pixels() {
        let mut scene = Scene::new(20.0, 20.0);
        scene.push_node(
            SceneNode::new(Element::rect(
                Rect::new(0.0, 0.0, 20.0, 20.0),
                FillStrokeStyle::fill(Color::rgb(0xff, 0x00, 0x00)),
            ))
            .with_visible(false),
        );
        let mut r = VelloPixmapRenderer::new(20, 20);
        let pix = r.render_scene_to_pixmap(&scene);
        // 背景为透明 → 红色像素数应为 0。
        assert_eq!(
            count_color(&pix, (0xff, 0x00, 0x00), 20),
            0,
            "隐藏节点不应渲染"
        );
    }

    /// 图层层序：顶层覆盖底层。交换层序后遮挡关系反转。
    #[test]
    fn layer_order_pixels() {
        use crate::scene::Layer;
        let mut scene = Scene::new(40.0, 40.0);
        let bottom = Layer::new("bottom").with_nodes(vec![SceneNode::new(Element::rect(
            Rect::new(0.0, 0.0, 40.0, 40.0),
            FillStrokeStyle::fill(Color::rgb(0xff, 0x00, 0x00)),
        ))]);
        let top = Layer::new("top").with_nodes(vec![SceneNode::new(Element::rect(
            Rect::new(10.0, 10.0, 30.0, 30.0),
            FillStrokeStyle::fill(Color::rgb(0x00, 0x00, 0xff)),
        ))]);
        scene.push_layer(bottom);
        scene.push_layer(top);
        let mut r = VelloPixmapRenderer::new(40, 40);
        let pix = r.render_scene_to_pixmap(&scene);
        // 中心 (20,20) 在顶层蓝矩形内 → 蓝色。
        let (r_, g, b, _) = pixel_at(&pix, 20, 20);
        assert!(
            b > 200 && r_ < 100 && g < 100,
            "中心应为顶层蓝色, got ({r_},{g},{b})"
        );
        // 角落 (2,2) 在底层红矩形内 → 红色。
        let (r2, g2, b2, _) = pixel_at(&pix, 2, 2);
        assert!(
            r2 > 200 && g2 < 100 && b2 < 100,
            "角落应为底层红色, got ({r2},{g2},{b2})"
        );
    }

    /// 文本字形渲染：黑字在白色背景上产生非白像素。
    #[test]
    fn text_glyphs_render() {
        let style = TextStyle::new(Color::rgb(0, 0, 0), 24.0, "sans-serif");
        let mut scene = Scene::new(200.0, 60.0);
        scene.push(Element::text("Hello", Point::new(10.0, 40.0), style));
        let mut r = VelloPixmapRenderer::new(200, 60).with_background(Color::WHITE);
        let pix = r.render_scene_to_pixmap(&scene);
        let non_white = pix
            .data()
            .iter()
            .filter(|p| p.r < 250 || p.g < 250 || p.b < 250)
            .count();
        assert!(non_white > 20, "字形应产生非白像素, got {non_white}");
    }

    /// 预排版 layout 与自动排版渲染结果一致（同一 TLS 上下文）。
    #[test]
    fn prelaid_layout_renders_same_as_auto() {
        use crate::text::RichSpan;
        use crate::text::measure_text;
        let style = TextStyle::new(Color::BLACK, 24.0, "sans-serif");
        let m = measure_text(
            std::slice::from_ref(&RichSpan::new("Hello", style.clone())),
            None,
        );
        // 自动排版
        let mut a = Scene::new(200.0, 60.0);
        a.push(Element::text(
            "Hello",
            Point::new(10.0, 40.0),
            style.clone(),
        ));
        // 预排版
        let mut b = Scene::new(200.0, 60.0);
        b.push(Element::Text {
            spans: vec![RichSpan::new("Hello", style.clone())],
            position: Point::new(10.0, 40.0),
            style,
            layout: Some(m.layout),
        });
        let mut ra = VelloPixmapRenderer::new(200, 60).with_background(Color::WHITE);
        let mut rb = VelloPixmapRenderer::new(200, 60).with_background(Color::WHITE);
        let da = pixels(&ra.render_scene_to_pixmap(&a));
        let db = pixels(&rb.render_scene_to_pixmap(&b));
        let diff = da.iter().zip(db.iter()).filter(|(x, y)| x != y).count();
        assert!(
            diff < da.len() / 1000,
            "预排版与自动排版应一致, 差异 {diff} 字节"
        );
    }

    /// clip：把大红矩形裁到中心小圆内 → 圆心是红、四角是背景（非红）。
    #[test]
    fn clip_restricts_pixels_to_shape() {
        use crate::scene::Clip;
        let mut scene = Scene::new(100.0, 100.0);
        // 全幅红色矩形，裁到中心半径 20 的圆。
        scene.push_node(
            SceneNode::new(Element::rect(
                Rect::new(0.0, 0.0, 100.0, 100.0),
                FillStrokeStyle::fill(Color::rgb(0xff, 0x00, 0x00)),
            ))
            .with_clip(Clip::circle(Point::new(50.0, 50.0), 20.0)),
        );
        let mut r = VelloPixmapRenderer::new(100, 100).with_background(Color::WHITE);
        let pix = r.render_scene_to_pixmap(&scene);
        // 圆心 (50,50) 应在圆内 → 红色。
        let (cr, cg, cb, ca) = pixel_at(&pix, 50, 50);
        assert!(
            cr > 200 && cg < 60 && cb < 60 && ca > 200,
            "圆心应为红, got ({cr},{cg},{cb},{ca})"
        );
        // 角落 (5,5) 在圆外 → 白色背景（红色不应出现）。
        let (lr, lg, lb, _) = pixel_at(&pix, 5, 5);
        assert!(
            lr > 240 && lg > 240 && lb > 240,
            "角落应为白, got ({lr},{lg},{lb})"
        );
    }

    /// Bug 回归：PNG 输出必须是 **非预乘** alpha。
    ///
    /// vello 的 Pixmap 保存预乘像素，直接写入 PNG 会让所有 alpha<255 的像素
    /// （抗锯齿边缘 / 半透明填充 / 文字边缘）整体偏暗。
    #[test]
    fn png_output_is_unpremultiplied() {
        let mut scene = Scene::new(4.0, 4.0).with_background(Color::TRANSPARENT);
        scene.push(Element::rect(
            Rect::new(0.0, 0.0, 4.0, 4.0),
            FillStrokeStyle::fill(Color::rgba(0xff, 0x00, 0x00, 128)),
        ));
        let mut r = VelloPixmapRenderer::new(4, 4);
        let (w, h, data) = render_png_pixels(&mut r, &scene);
        assert_eq!((w, h), (4, 4));
        let i = ((2 * w + 2) * 4) as usize;
        let (pr, pg, pb, pa) = (data[i], data[i + 1], data[i + 2], data[i + 3]);
        assert!(
            pr > 240 && pg < 20 && pb < 20 && pa.abs_diff(128) <= 2,
            "50% 红应输出非预乘的 (255,0,0,128), got ({pr},{pg},{pb},{pa})"
        );
    }

    /// Bug 回归：图片的最终透明度 = 图片自身 opacity × 节点 / 图层 opacity。
    #[test]
    fn image_opacity_composes_with_node_opacity() {
        // 4×4 不透明白图，节点 opacity = 0.5，画在黑底上 → 中灰。
        let img = crate::SceneImage::from_rgba8(4, 4, vec![0xffu8; 4 * 4 * 4]).unwrap();
        let mut scene = Scene::new(20.0, 20.0);
        scene.push_node(
            SceneNode::new(Element::image(img, Rect::new(0.0, 0.0, 20.0, 20.0)))
                .with_opacity(0.5),
        );
        let mut r = VelloPixmapRenderer::new(20, 20).with_background(Color::BLACK);
        let (w, _, data) = render_png_pixels(&mut r, &scene);
        let i = ((10 * w + 10) * 4) as usize;
        let (pr, pg, pb, _) = (data[i], data[i + 1], data[i + 2], data[i + 3]);
        let mid = |c: u8| (100..=160).contains(&c);
        assert!(
            mid(pr) && mid(pg) && mid(pb),
            "半透明白图叠黑底应为中灰, got ({pr},{pg},{pb})"
        );
    }

    /// 契约锁定：输出画布 = 构造时给定的尺寸，`scene.scale` **不参与**光栅画布计算。
    ///
    /// 下游（liepress）把场景按最终像素尺寸构造（`scene.width = page_w * (dpi/72)`）并
    /// 记录 `scene.scale = dpi/72` 作为元数据，再把 `scene.width` 直接传进来；若这里再乘
    /// 一次 scale，输出会被二次放大（dpi=144 时面积 ×4）。像素密度由调用方通过构造器
    /// 决定。
    #[test]
    fn renderer_size_defines_output_canvas_regardless_of_scene_scale() {
        let mut scene = Scene::new(100.0, 100.0).with_scale(2.0);
        scene.push(Element::rect(
            Rect::new(0.0, 0.0, 100.0, 100.0),
            FillStrokeStyle::fill(Color::rgb(0xff, 0x00, 0x00)),
        ));
        let mut r = VelloPixmapRenderer::new(100, 100).with_background(Color::WHITE);
        let (w, h, _) = render_png_pixels(&mut r, &scene);
        assert_eq!(
            (w, h),
            (100, 100),
            "scene.scale=2 时输出画布仍应为 100×100（scale 是密度而非放大）"
        );
    }

    /// 契约锁定（跨后端）：`scale` 是**密度**，两个后端语义完全一致。
    ///
    /// 场景坐标 = 输出尺寸 / scale。故"场景 100×100 + scale=2"分别用
    /// `new(200,200)`（vello）与 `new(200.0,200.0)`（SVG）渲染时，内容都应占满整块画布，
    /// 且输出尺寸都是 200×200 —— 而不是 100×100 或 400×400。
    #[test]
    fn scale_is_density_identical_on_both_backends() {
        let mut scene = Scene::new(100.0, 100.0).with_scale(2.0);
        scene.push(Element::rect(
            Rect::new(0.0, 0.0, 100.0, 100.0),
            FillStrokeStyle::fill(Color::rgb(0xff, 0x00, 0x00)),
        ));

        // 光栅端：输出 200×200，内容铺满（scene 100 → 视口 100 → ×2）。
        let mut r = VelloPixmapRenderer::new(200, 200).with_background(Color::WHITE);
        let (w, h, data) = render_png_pixels(&mut r, &scene);
        assert_eq!((w, h), (200, 200));
        for (x, y) in [(5u32, 5u32), (195, 195), (100, 100)] {
            let i = ((y * w + x) * 4) as usize;
            assert!(
                data[i] > 200 && data[i + 1] < 60 && data[i + 2] < 60,
                "({x},{y}) 应为红（内容铺满画布）, got {:?}",
                &data[i..i + 4]
            );
        }

        // SVG 端：同样的构造尺寸 → 同样的输出尺寸与用户坐标系。
        let mut svg = crate::render::SvgRenderer::new(200.0, 200.0);
        scene.render(&mut svg);
        let out = svg.into_string();
        assert!(
            out.contains(r#"width="200.00" height="200.00" viewBox="0 0 100.00 100.00""#),
            "SVG 端应与光栅端一致, 实际: {out}"
        );
    }

    /// 契约锁定：构造尺寸 == 场景尺寸时（下游 liepress 的用法），两个后端都是 1:1 铺满，
    /// 不受 `scene.scale` 影响（liepress 已把坐标乘过 scale 了）。
    #[test]
    fn preset_output_size_with_density_scale_maps_one_to_one() {
        // liepress 形态：场景坐标已是最终像素（= 逻辑尺寸 × scale），scale=2 仅作元数据。
        let mut scene = Scene::new(200.0, 100.0).with_scale(2.0);
        scene.push(Element::rect(
            Rect::new(0.0, 0.0, 200.0, 100.0),
            FillStrokeStyle::fill(Color::rgb(0x00, 0x00, 0xff)),
        ));
        let mut r = VelloPixmapRenderer::new(200, 100).with_background(Color::WHITE);
        let (w, h, data) = render_png_pixels(&mut r, &scene);
        assert_eq!((w, h), (200, 100), "构造尺寸即输出尺寸");
        let i = ((50 * w + 100) * 4) as usize;
        assert!(
            data[i] < 60 && data[i + 2] > 200,
            "内容应 1:1 铺满画布, got {:?}",
            &data[i..i + 4]
        );
    }

    /// Bug 回归：画布与场景长宽比不一致时，按 SVG 默认的
    /// `preserveAspectRatio="xMidYMid meet"` 等比缩放并居中（此前只按宽度缩放，
    /// 纵向会溢出/裁切，与 SVG 输出不一致）。
    #[test]
    fn aspect_mismatch_uses_meet_and_centers() {
        let mut scene = Scene::new(100.0, 100.0);
        scene.push(Element::rect(
            Rect::new(0.0, 0.0, 50.0, 50.0),
            FillStrokeStyle::fill(Color::rgb(0xff, 0x00, 0x00)),
        ));
        // 画布 200×100、场景 100×100 → meet 取 s=1，水平居中 → 方块占 x∈[50,100]。
        let mut r = VelloPixmapRenderer::new(200, 100).with_background(Color::WHITE);
        let pix = r.render_scene_to_pixmap(&scene);
        let (cr, cg, cb, _) = pixel_at(&pix, 60, 25);
        assert!(
            cr > 200 && cg < 60 && cb < 60,
            "居中后的方块内应为红, got ({cr},{cg},{cb})"
        );
        // 居中留白区（x=10）与纵向（y=75，场景外）应为白。
        let (lr, lg, lb, _) = pixel_at(&pix, 10, 25);
        assert!(
            lr > 240 && lg > 240 && lb > 240,
            "左侧留白应为白, got ({lr},{lg},{lb})"
        );
        let (br, bg, bb, _) = pixel_at(&pix, 60, 75);
        assert!(
            br > 240 && bg > 240 && bb > 240,
            "纵向未裁切区应为白, got ({br},{bg},{bb})"
        );
    }

    /// 极端 frame 不应导致超大中间位图分配（降采样后仍能画出图片）。
    #[test]
    fn huge_image_frame_is_downscaled() {
        // 1×1 的图 + Cover + 巨大 frame：放大倍数极高，必须被像素上限压住。
        let img = crate::SceneImage::from_rgba8(1, 1, vec![0xffu8; 4])
            .unwrap()
            .with_object_fit(crate::ObjectFit::Cover);
        let mut scene = Scene::new(40.0, 40.0);
        scene.push(Element::image(img, Rect::new(0.0, 0.0, 50000.0, 50000.0)));
        let mut r = VelloPixmapRenderer::new(40, 40).with_background(Color::BLACK);
        let pix = r.render_scene_to_pixmap(&scene);
        let (cr, cg, cb, _) = pixel_at(&pix, 20, 20);
        assert!(
            cr > 200 && cg > 200 && cb > 200,
            "放大后的白色图片应铺满画布, got ({cr},{cg},{cb})"
        );
    }

    /// 画布之外的图片应被直接剔除（不产生任何像素）。
    #[test]
    fn offscreen_image_is_culled() {
        let img = crate::SceneImage::from_rgba8(4, 4, vec![0xffu8; 4 * 4 * 4]).unwrap();
        let mut scene = Scene::new(100.0, 100.0);
        scene.push(Element::image(img, Rect::new(500.0, 500.0, 600.0, 600.0)));
        let mut r = VelloPixmapRenderer::new(100, 100).with_background(Color::BLACK);
        let pix = r.render_scene_to_pixmap(&scene);
        let white = pixels(&pix)
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|p| p[0] > 200 && p[1] > 200 && p[2] > 200)
            .count();
        assert_eq!(white, 0, "画布外图片不应产生像素");
    }
}
