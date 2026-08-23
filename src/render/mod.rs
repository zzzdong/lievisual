//! 渲染后端 trait 与默认遍历逻辑。
//!
//! [`Renderer`] 定义原子绘制方法（Canvas API 风格）。默认方法 [`Renderer::render_scene`]
//! 负责：经 [`crate::scene::Scene::iter_ordered`] 按 `z_index` 稳定排序 → 应用节点属性
//! （`visible` / `opacity` / `name` / `clip`）与 `Transform` → 对 `Group` 递归（内部复用
//! [`crate::scene::SceneNode::ordered`]）。此外还会先绘制默认层 `nodes`，再按序绘制
//! 各 [`crate::scene::Layer`]（[`Renderer::render_layer`]，整体应用 `visible` / `opacity` /
//! `transform` / `name`）。后端只需实现原子方法。
//!
//! 新增图元的约定：`draw_*` 均提供默认实现（把几何转为 [`kurbo::BezPath`] 后走
//! [`Renderer::draw_path`]），因此新后端不实现也能正确渲染；后端可按需 override 以输出
//! 原生标签（如 SVG 的 `<ellipse>` / `<rect rx>`）。

use crate::geometry::{Point, Rect, Transform, Vec2};
use crate::scene::{Element, FillStrokeStyle, Layer, Scene, SceneNode, Stroke};
use kurbo::Shape;

pub use svg::SvgRenderer;
pub use vello_pixmap::VelloPixmapRenderer;

mod svg;
mod vello_pixmap;

/// 在屏幕(y-down)坐标系下构造一段圆弧路径，与 SVG 后端的 `arc_path_d` 几何完全一致。
///
/// 场景（SVG/vello 一致）采用 y-down 坐标系：原点在左上、y 向下、0 角为 +x、正角为顺时针。
/// 而 kurbo::Arc 按数学 y-up 采样：`start=(cx+r·cos a, cy+r·sin a)`、正 sweep 为逆时针。
///
/// 这里复用 kurbo 成熟的贝塞尔弧逼近（`to_path`），再做一次 y-up → y-down 的坐标映射，
/// 使采样出的弧与 SVG 后端公式（`arc_path_d`）完全对齐，避免两端画出的圆弧/扇形垂直镜像：
/// 1. 圆心 y、角度统一取负，在数学平面构造出"待翻转"的弧；
/// 2. 整体应用 y 轴翻转 `Affine(1,0,0,-1)`，把数学坐标映回屏幕坐标。
pub(crate) fn arc_path_y_down(
    center: Point,
    radii: Vec2,
    start_angle: f64,
    sweep_angle: f64,
) -> kurbo::BezPath {
    // 场景 y-down → 数学 y-up：圆心 y 与角度符号取反（0 角不变，顺/逆时针互换）。
    let arc = kurbo::Arc::new(
        (center.x, -center.y),
        (radii.x, radii.y),
        -start_angle,
        -sweep_angle,
        0.0,
    )
    .to_path(0.1);
    // 数学 y-up → 场景 y-down：y 轴整体翻转（x 不变）。
    let mut path = arc;
    path.apply_affine(kurbo::Affine::new([1.0, 0.0, 0.0, -1.0, 0.0, 0.0]));
    path
}

/// 渲染后端接口（Canvas API 风格）。
///
/// 调用者（liecharts / liemermaid / liepress）生成 [`Scene`] 后，选择一个 `Renderer`
/// 实现即可输出到目标媒介。新增后端不影响 IR。
pub trait Renderer {
    /// 绘制矩形。
    fn draw_rect(&mut self, rect: Rect, style: &FillStrokeStyle);

    /// 绘制圆。
    fn draw_circle(&mut self, center: Point, radius: f64, style: &FillStrokeStyle);

    /// 绘制椭圆（`radii` 为两半轴长，`rotation` 绕中心旋转）。
    fn draw_ellipse(&mut self, center: Point, radii: Vec2, rotation: f64, style: &FillStrokeStyle) {
        let path =
            kurbo::Ellipse::new((center.x, center.y), (radii.x, radii.y), rotation).to_path(0.1);
        self.draw_path(&path, style, true, None);
    }

    /// 绘制圆角矩形（`radius` 为四角统一圆角半径）。
    fn draw_rounded_rect(&mut self, rect: Rect, radius: f64, style: &FillStrokeStyle) {
        let path = kurbo::RoundedRect::new(
            rect.min_x(),
            rect.min_y(),
            rect.max_x(),
            rect.max_y(),
            radius,
        )
        .to_path(0.1);
        self.draw_path(&path, style, true, None);
    }

    /// 绘制线段。
    fn draw_line(&mut self, start: Point, end: Point, style: &Stroke);

    /// 绘制折线（多段线，开放，仅描边）。
    fn draw_polyline(&mut self, points: &[Point], style: &Stroke, marker_end: Option<&str>);

    /// 绘制多边形（闭合，填充 + 可选描边）。
    fn draw_polygon(&mut self, points: &[Point], style: &FillStrokeStyle) {
        let mut path = kurbo::BezPath::new();
        if let Some(p0) = points.first() {
            path.move_to((p0.x, p0.y));
            for p in &points[1..] {
                path.line_to((p.x, p.y));
            }
            path.close_path();
        }
        self.draw_path(&path, style, false, None);
    }

    /// 绘制圆弧（开放，仅描边）。
    fn draw_arc(
        &mut self,
        center: Point,
        radii: Vec2,
        start_angle: f64,
        sweep_angle: f64,
        style: &Stroke,
    ) {
        let path = crate::render::arc_path_y_down(center, radii, start_angle, sweep_angle);
        let style = FillStrokeStyle {
            fill: None,
            stroke: Some(style.clone()),
        };
        self.draw_path(&path, &style, false, None);
    }

    /// 绘制扇形（闭合，含圆心到两端点的连线，填充 + 可选描边）。
    fn draw_pie(
        &mut self,
        center: Point,
        radius: f64,
        start_angle: f64,
        sweep_angle: f64,
        style: &FillStrokeStyle,
    ) {
        let mut path = kurbo::BezPath::new();
        path.move_to((center.x, center.y));
        let arc = crate::render::arc_path_y_down(
            center,
            Vec2::new(radius, radius),
            start_angle,
            sweep_angle,
        );
        // arc 路径以 MoveTo(start) 开头，需替换为 LineTo(start)，
        // 这样整体路径为 `M center L start [arc...] end Z` —— 真正的扇形
        // （含圆心顶点和两条半径边），与 SVG 后端的 `M center L start A end Z` 一致。
        let mut els = arc.elements().iter();
        if let Some(kurbo::PathEl::MoveTo(p0)) = els.next() {
            path.line_to(*p0);
            for el in els {
                path.push(*el);
            }
        } else {
            path.extend(arc);
        }
        path.close_path();
        self.draw_path(&path, style, false, None);
    }

    /// 绘制路径（可闭合填充或描边）。
    fn draw_path(&mut self, path: &kurbo::BezPath, style: &FillStrokeStyle, closed: bool, marker_end: Option<&str>);

    /// 绘制带线性渐变填充的路径。
    fn draw_gradient_path(
        &mut self,
        path: &kurbo::BezPath,
        gradient: &crate::scene::LinearGradient,
        stroke: Option<&crate::scene::Stroke>,
    );

    /// 绘制文本。以样式化片段（`spans`）为核心；若 `layout` 提供则优先使用。
    ///
    /// 非 parley 后端（如 SVG）应从 `spans` 拼接纯文本；`style` 提供块级定位
    /// （`align` / `baseline` / `rotation` / `max_width`）。
    fn draw_text(
        &mut self,
        spans: &[crate::text::RichSpan],
        position: Point,
        style: &crate::text::TextStyle,
        layout: Option<&crate::text::TextLayout>,
    );

    /// 绘制图片：`image` 为自包含 RGBA8 位图，`frame` 为显示区域（pt/px 坐标）。
    ///
    /// 后端直接消费 `image.pixmap`（已解码，**无需再解码**）并按 `image.object_fit`
    /// 映射到 `frame`。默认实现为空（后端应 override 以真正绘制）。
    fn draw_image(&mut self, image: &crate::SceneImage, frame: Rect, opacity: f64) {
        let _ = (image, frame, opacity);
    }

    /// 进入一个变换作用域（Group 或节点局部变换）。
    /// 默认空实现；支持栈式状态的后端（vello/svg group）应 override。
    fn push_transform(&mut self, _t: Transform) {}
    /// 退出 [`Renderer::push_transform`] 建立的作用域。
    fn pop_transform(&mut self) {}

    /// 进入一个透明度作用域（`opacity < 1` 的节点/子树）。默认空实现。
    fn push_opacity(&mut self, _opacity: f64) {}
    /// 退出 [`Renderer::push_opacity`] 建立的作用域。
    fn pop_opacity(&mut self) {}

    /// 进入一个命名作用域（带 `name` 的节点；SVG 端输出 `id`，并附带 `class`）。
    /// `name` 与 `class` 均为 `Option`：`None` 表示不输出对应属性。
    fn push_name(&mut self, _name: Option<&str>, _class: Option<&str>) {}
    /// 退出 [`Renderer::push_name`] 建立的作用域。
    fn pop_name(&mut self) {}

    /// 进入一个裁剪作用域（带 `clip` 的节点/子树）。默认空实现。
    fn push_clip(&mut self, _clip: &crate::scene::Clip) {}
    /// 退出 [`Renderer::push_clip`] 建立的作用域。
    fn pop_clip(&mut self) {}

    /// 进入节点级作用域：按顺序叠加变换、裁剪、透明度、命名。
    /// 默认组合 `push_transform` / `push_clip` / `push_opacity` / `push_name`；后端一般无需 override。
    fn push_node_scope(&mut self, node: &SceneNode) {
        if let Some(t) = node.transform {
            self.push_transform(t);
        }
        if let Some(clip) = &node.clip {
            self.push_clip(clip);
        }
        if node.opacity != 1.0 {
            self.push_opacity(node.opacity);
        }
        if node.name.is_some() || node.class.is_some() {
            self.push_name(node.name.as_deref(), node.class.as_deref());
        }
    }

    /// 退出 [`Renderer::push_node_scope`] 建立的作用域（逆序 pop）。
    fn pop_node_scope(&mut self, node: &SceneNode) {
        // name 作用域的 pop 必须与 push 配对：push 在 `name.is_some() || class.is_some()`
        // 时建立，故此处用相同条件判断，否则仅含 class 的节点（name 为 None）会漏 pop，
        // 导致 SVG 的 <g> 未闭合（结构非法）。
        if node.name.is_some() || node.class.is_some() {
            self.pop_name();
        }
        if node.opacity != 1.0 {
            self.pop_opacity();
        }
        if node.clip.is_some() {
            self.pop_clip();
        }
        if node.transform.is_some() {
            self.pop_transform();
        }
    }

    /// 渲染整个场景：默认实现处理排序与递归。
    /// 后端可在 override 中捕获 [`Scene`] 元数据（title/description/scale）后，
    /// 再调用 [`Renderer::render_scene_ordered`] 复用默认遍历。
    fn render_scene(&mut self, scene: &Scene) {
        self.render_scene_ordered(scene);
    }

    /// 默认遍历：先绘制默认层 `nodes`（最底，按 `z_index` 稳定升序），
    /// 再按数组顺序从底到顶绘制各图层 [`Layer`]。
    fn render_scene_ordered(&mut self, scene: &Scene) {
        for node in scene.iter_ordered() {
            self.render_node(node);
        }
        for layer in &scene.layers {
            self.render_layer(layer);
        }
    }

    /// 渲染单个图层：应用图层级 `visible` / `opacity` / `transform` / `name` 作用域，
    /// 层内按 `z_index` 稳定升序逐节点渲染（含 Group 递归）。一般无需 override。
    fn render_layer(&mut self, layer: &Layer) {
        if !layer.visible {
            return;
        }
        if let Some(t) = layer.transform {
            self.push_transform(t);
        }
        if layer.opacity != 1.0 {
            self.push_opacity(layer.opacity);
        }
        if !layer.name.is_empty() {
            self.push_name(Some(&layer.name), None);
        }

        for node in layer.iter_ordered() {
            self.render_node(node);
        }

        if !layer.name.is_empty() {
            self.pop_name();
        }
        if layer.opacity != 1.0 {
            self.pop_opacity();
        }
        if layer.transform.is_some() {
            self.pop_transform();
        }
    }

    /// 渲染单个节点（含节点属性、局部变换与 Group 递归）。一般无需 override。
    fn render_node(&mut self, node: &SceneNode) {
        // 隐藏节点整棵子树跳过。
        if !node.visible {
            return;
        }
        // 进入节点作用域（变换 + 透明度 + 命名）。
        self.push_node_scope(node);

        match &node.element {
            Element::Group { children } => {
                // Group 内子节点按 z_index 稳定升序绘制（复用 SceneNode::ordered）。
                for child in SceneNode::ordered(children) {
                    self.render_node(child);
                }
            }
            Element::Rect { rect, style } => self.draw_rect(*rect, style),
            Element::Circle {
                center,
                radius,
                style,
            } => self.draw_circle(*center, *radius, style),
            Element::Ellipse {
                center,
                radii,
                rotation,
                style,
            } => self.draw_ellipse(*center, *radii, *rotation, style),
            Element::RoundedRect {
                rect,
                radius,
                style,
            } => self.draw_rounded_rect(*rect, *radius, style),
            Element::Line { start, end, style } => self.draw_line(*start, *end, style),
            Element::Polyline { points, style, marker_end } => {
                self.draw_polyline(points, style, marker_end.as_deref())
            }
            Element::Polygon { points, style } => self.draw_polygon(points, style),
            Element::Arc {
                center,
                radii,
                start_angle,
                sweep_angle,
                style,
            } => self.draw_arc(*center, *radii, *start_angle, *sweep_angle, style),
            Element::Pie {
                center,
                radius,
                start_angle,
                sweep_angle,
                style,
            } => self.draw_pie(*center, *radius, *start_angle, *sweep_angle, style),
            Element::Path {
                path,
                style,
                closed,
                marker_end,
            } => self.draw_path(path, style, *closed, marker_end.as_deref()),
            Element::Image {
                image,
                frame,
                opacity,
            } => self.draw_image(image, *frame, *opacity),
            Element::GradientPath {
                path,
                gradient,
                stroke,
            } => self.draw_gradient_path(path, gradient, stroke.as_ref()),
            Element::Text {
                spans,
                position,
                style,
                layout,
            } => self.draw_text(spans, *position, style, layout.as_deref()),
        }

        self.pop_node_scope(node);
    }
}

#[cfg(test)]
mod tests {
    use crate::geometry::{Point, Vec2};

    /// 与 SVG 后端 `arc_path_d` 完全一致的屏幕(y-down)坐标端点公式。
    /// start = (cx + rx·cos a, cy + ry·sin a)，end = (cx + rx·cos(a+s), cy + ry·sin(a+s))。
    fn expected_screen_points(
        center: Point,
        radii: Vec2,
        start_angle: f64,
        sweep_angle: f64,
    ) -> (Point, Point) {
        let start = Point::new(
            center.x + radii.x * start_angle.cos(),
            center.y + radii.y * start_angle.sin(),
        );
        let end_angle = start_angle + sweep_angle;
        let end = Point::new(
            center.x + radii.x * end_angle.cos(),
            center.y + radii.y * end_angle.sin(),
        );
        (start, end)
    }

    /// 校验 vello 后端 `arc_path_y_down` 生成的弧端点与 SVG `arc_path_d` 公式一致，
    /// 确保两个后端画出的饼图/圆弧形状完全对齐（曾因 kurbo 数学坐标未翻转而垂直镜像）。
    #[test]
    fn arc_y_down_matches_svg_geometry() {
        let cases = [
            (
                Point::new(300.0, 320.0),
                Vec2::new(70.0, 70.0),
                0.0f64,
                std::f64::consts::FRAC_PI_2,
            ),
            (
                Point::new(300.0, 320.0),
                Vec2::new(70.0, 70.0),
                std::f64::consts::FRAC_PI_2,
                std::f64::consts::FRAC_PI_2,
            ),
            (
                Point::new(300.0, 320.0),
                Vec2::new(70.0, 70.0),
                std::f64::consts::PI,
                -std::f64::consts::FRAC_PI_2,
            ),
            (Point::new(100.0, 100.0), Vec2::new(40.0, 20.0), 0.4, 2.7),
        ];
        for (center, radii, a, s) in cases {
            let path = crate::render::arc_path_y_down(center, radii, a, s);
            // 纯弧路径：elements 以 MoveTo(start) 开头，后续 CurveTo 终点依次追加，
            // 因此 pts[0] 是弧起点，pts[last] 是弧终点。
            let pts: Vec<Point> = path
                .elements()
                .iter()
                .filter_map(|el| match el {
                    kurbo::PathEl::MoveTo(p)
                    | kurbo::PathEl::LineTo(p)
                    | kurbo::PathEl::QuadTo(_, p)
                    | kurbo::PathEl::CurveTo(_, _, p) => Some(Point::new(p.x, p.y)),
                    kurbo::PathEl::ClosePath => None,
                })
                .collect();
            let n = pts.len();
            assert!(n >= 2, "arc path should have at least 2 points");
            let got_start = pts[0];
            let got_end = pts[n - 1];
            let (exp_start, exp_end) = expected_screen_points(center, radii, a, s);
            let eps = 1e-6;
            assert!(
                (got_start.x - exp_start.x).abs() < eps && (got_start.y - exp_start.y).abs() < eps,
                "start mismatch: got {:?} expected {:?} (center={:?} radii={:?} a={} s={})",
                got_start,
                exp_start,
                center,
                radii,
                a,
                s
            );
            assert!(
                (got_end.x - exp_end.x).abs() < eps && (got_end.y - exp_end.y).abs() < eps,
                "end mismatch: got {:?} expected {:?} (center={:?} radii={:?} a={} s={})",
                got_end,
                exp_end,
                center,
                radii,
                a,
                s
            );
        }
    }
}
