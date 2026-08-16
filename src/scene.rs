//! 声明式场景图 IR。
//!
//! 设计原则（参见 crate 文档）：
//! - `z_index` 与 [`Transform`] 提升为 [`SceneNode`] 字段，而非每个图元重复携带。
//!   同理，`opacity` / `name` / `visible` 也属于节点（或整棵子树）属性。
//! - [`Element`] 枚举只描述几何与样式；新增图元变体时不影响层级/变换逻辑。
//! - 节点级 `opacity` 作用于整棵子树（Group 递归时逐层复合），`visible = false`
//!   时整棵子树跳过。
//! - 图层：`Scene` 可携带一组有序 [`Layer`]（从底到顶）。图层整体控制
//!   `visible` / `opacity` / `transform`，层内节点再按 `z_index` 稳定排序；
//!   `Scene::nodes` 作为"默认层"（最底）保留，向后兼容。
//! - [`Element::Text`] 双形态：纯文本 + 样式，或携带预排版的 [`crate::text::TextLayout`]。
//! - 统一排序助手：[`SceneNode::ordered`] 按 `z_index` 稳定升序遍历节点（同层保持插入次序），
//!   [`Scene::iter_ordered`] 为顶层集合的便捷入口；渲染与调用方复用同一套遍历逻辑。

use crate::geometry::{Color, Point, Rect, Transform, Vec2};
use crate::render::Renderer;
use crate::text::TextLayout;
use kurbo::BezPath;

/// 一个完整的可渲染场景。
#[derive(Debug, Clone)]
pub struct Scene {
    pub width: f64,
    pub height: f64,
    pub background: Color,
    /// 顶层图元集合（"默认层"，位于所有 `layers` 之下）。`render_scene` 会先按 `z_index` 排序再绘制。
    pub nodes: Vec<SceneNode>,
    /// 有序图层（从底到顶）。每个图层整体控制 `visible` / `opacity` / `transform`，
    /// 层内节点按 `z_index` 稳定升序绘制。
    pub layers: Vec<Layer>,
    /// 文档标题（SVG 输出为 `<title>`，供无障碍/导出名使用）。
    pub title: Option<String>,
    /// 文档描述（SVG 输出为 `<desc>`）。
    pub description: Option<String>,
    /// 输出缩放（像素密度）。默认 1.0；SVG 端同时放大 `width`/`height` 而保持
    /// `viewBox` 不变，得到更高分辨率的位图导出。
    pub scale: f64,
}

impl Default for Scene {
    fn default() -> Self {
        Self {
            width: 0.0,
            height: 0.0,
            background: Color::WHITE,
            nodes: Vec::new(),
            layers: Vec::new(),
            title: None,
            description: None,
            scale: 1.0,
        }
    }
}

impl Scene {
    pub fn new(width: f64, height: f64) -> Self {
        Self {
            width,
            height,
            background: Color::WHITE,
            nodes: Vec::new(),
            layers: Vec::new(),
            title: None,
            description: None,
            scale: 1.0,
        }
    }

    /// 便捷构造器：背景 + 尺寸。
    #[must_use]
    pub fn with_background(mut self, bg: Color) -> Self {
        self.background = bg;
        self
    }

    /// 便捷构造器：文档标题。
    #[must_use]
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// 便捷构造器：文档描述。
    #[must_use]
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// 便捷构造器：输出缩放（像素密度）。
    #[must_use]
    pub fn with_scale(mut self, scale: f64) -> Self {
        self.scale = scale;
        self
    }

    /// 追加一个图元或节点（默认 z_index = 0，无变换）。
    /// 同时接受 [`Element`]（经 `From<Element>` 自动转为 [`SceneNode`]）与已配置的 [`SceneNode`]。
    pub fn push(&mut self, node: impl Into<SceneNode>) {
        self.nodes.push(node.into());
    }

    /// 追加一个带层级/变换的图元。
    pub fn push_node(&mut self, node: SceneNode) {
        self.nodes.push(node);
    }

    /// 追加一个递归组合（Group）：子节点在父变换与层级下绘制。
    pub fn push_group(&mut self, children: Vec<SceneNode>) {
        self.nodes.push(SceneNode::group(children));
    }

    /// 追加一个图层（越后 push 越靠上）。
    pub fn push_layer(&mut self, layer: Layer) {
        self.layers.push(layer);
    }

    /// 所有图层（从底到顶）。
    pub fn layers(&self) -> &[Layer] {
        &self.layers
    }

    /// 按名称查找图层。
    pub fn layer(&self, name: &str) -> Option<&Layer> {
        self.layers.iter().find(|l| l.name == name)
    }

    /// 按名称查找图层（可变）。
    pub fn layer_mut(&mut self, name: &str) -> Option<&mut Layer> {
        self.layers.iter_mut().find(|l| l.name == name)
    }

    /// 渲染到任意后端。
    pub fn render(&self, renderer: &mut dyn Renderer) {
        renderer.render_scene(self);
    }

    /// 顶层图元按 `z_index` 升序遍历（稳定：同层保持插入次序）。
    /// 仅覆盖默认层 `nodes`；图层的遍历见 [`Layer::iter_ordered`] 与 [`Renderer::render_layer`]。
    pub fn iter_ordered(&self) -> impl Iterator<Item = &SceneNode> {
        SceneNode::ordered(&self.nodes)
    }
}

/// 一个命名图层：一组节点 + 图层级整体属性。
///
/// 图层按数组顺序从底到顶绘制；图层级 `visible = false` 时整层跳过，`opacity` /
/// `transform` 作用于层内所有节点（与 Group 的子树语义一致）。层内节点再按各自
/// `z_index` 稳定升序绘制。
#[derive(Debug, Clone)]
pub struct Layer {
    /// 图层名（SVG 端输出为 `<g id>`；可为空字符串以不输出）。
    pub name: String,
    /// 是否可见，默认 `true`。`false` 时整层跳过渲染。
    pub visible: bool,
    /// 图层整体透明度，0.0–1.0，默认 1.0。
    pub opacity: f64,
    /// 图层整体变换（应用于层内所有节点）。
    pub transform: Option<Transform>,
    /// 层内图元集合（按 `z_index` 稳定排序后绘制）。
    pub nodes: Vec<SceneNode>,
}

impl Layer {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            visible: true,
            opacity: 1.0,
            transform: None,
            nodes: Vec::new(),
        }
    }

    /// 带初始图元的图层。
    pub fn with_nodes(mut self, nodes: Vec<SceneNode>) -> Self {
        self.nodes = nodes;
        self
    }

    /// 层内追加一个图元或节点。
    pub fn push(&mut self, node: impl Into<SceneNode>) {
        self.nodes.push(node.into());
    }

    /// 层内追加一个带层级/变换的节点。
    pub fn push_node(&mut self, node: SceneNode) {
        self.nodes.push(node);
    }

    /// 层内追加一个递归组合。
    pub fn push_group(&mut self, children: Vec<SceneNode>) {
        self.nodes.push(SceneNode::group(children));
    }

    /// 层内按 `z_index` 升序遍历（稳定：同层保持插入次序）。
    pub fn iter_ordered(&self) -> impl Iterator<Item = &SceneNode> {
        SceneNode::ordered(&self.nodes)
    }

    #[must_use]
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// 设置整层可见性。
    #[must_use]
    pub fn with_visible(mut self, visible: bool) -> Self {
        self.visible = visible;
        self
    }

    /// 设置整层透明度（0.0–1.0）。
    #[must_use]
    pub fn with_opacity(mut self, opacity: f64) -> Self {
        self.opacity = opacity.clamp(0.0, 1.0);
        self
    }

    /// 设置整层变换。
    #[must_use]
    pub fn with_transform(mut self, t: Transform) -> Self {
        self.transform = Some(t);
        self
    }
}

/// 场景中的单个节点：图元 + 层级 + 局部变换 + 节点属性。
///
/// 除 `element` 外的字段都是"子树级"属性：`transform` / `opacity` / `name` /
/// `visible` 对 `Group` 同样生效（`opacity` 与 `transform` 会随递归复合）。
#[derive(Debug, Clone)]
pub struct SceneNode {
    pub element: Element,
    pub z_index: i32,
    pub transform: Option<Transform>,
    /// 节点（及 Group 子树）整体透明度，0.0–1.0，默认 1.0。
    pub opacity: f64,
    /// 可选标识：调试、命中测试、SVG `id`。
    pub name: Option<String>,
    /// 是否可见，默认 `true`。`false` 时整棵子树跳过渲染。
    pub visible: bool,
    /// 可选裁剪形状：将整棵子树限制在其内部绘制。
    pub clip: Option<Clip>,
}

impl SceneNode {
    pub fn new(element: Element) -> Self {
        Self {
            element,
            z_index: 0,
            transform: None,
            opacity: 1.0,
            name: None,
            visible: true,
            clip: None,
        }
    }

    /// 构造递归组合节点（children 在父变换与层级下绘制）。
    pub fn group(children: Vec<SceneNode>) -> Self {
        Self {
            element: Element::Group { children },
            z_index: 0,
            transform: None,
            opacity: 1.0,
            name: None,
            visible: true,
            clip: None,
        }
    }

    /// 按 `z_index` 升序产出节点引用（稳定：同层保持插入次序）。
    pub fn ordered(nodes: &[SceneNode]) -> impl Iterator<Item = &SceneNode> {
        let mut order: Vec<usize> = (0..nodes.len()).collect();
        // 稳定排序：`sort_by_key` 对相等 z_index 保持相对次序。
        order.sort_by_key(|&i| nodes[i].z_index);
        order.into_iter().map(move |i| &nodes[i])
    }

    #[must_use]
    pub fn with_z(mut self, z: i32) -> Self {
        self.z_index = z;
        self
    }

    #[must_use]
    pub fn with_transform(mut self, t: Transform) -> Self {
        self.transform = Some(t);
        self
    }

    /// 设置节点透明度（0.0–1.0）。
    #[must_use]
    pub fn with_opacity(mut self, opacity: f64) -> Self {
        self.opacity = opacity.clamp(0.0, 1.0);
        self
    }

    /// 设置节点名称（调试 / 命中测试 / SVG `id`）。
    #[must_use]
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// 设置可见性。`false` 时整棵子树跳过渲染。
    #[must_use]
    pub fn with_visible(mut self, visible: bool) -> Self {
        self.visible = visible;
        self
    }

    /// 设置裁剪形状：整棵子树限制在 `clip` 内部绘制。
    #[must_use]
    pub fn with_clip(mut self, clip: Clip) -> Self {
        self.clip = Some(clip);
        self
    }
}

/// 由 [`Element`] 构造节点（默认 z_index = 0，无变换），供 `Scene::push` 等接受 `Element`。
impl From<Element> for SceneNode {
    fn from(element: Element) -> Self {
        SceneNode::new(element)
    }
}

/// 裁剪形状：将节点（及整棵子树）限制在形状内部绘制。
///
/// 作为节点级属性（[`SceneNode::clip`]），作用于子树；形状坐标为局部用户坐标，
/// 与节点 `transform` 复合（先变换后裁剪）。复用现有几何/路径类型，跨后端一致：
/// - SVG：`<clipPath>` + `clip-path="url(#...)"`
/// - vello：`push_clip_path` / `pop_clip_path`
#[derive(Debug, Clone, PartialEq)]
pub enum Clip {
    /// 矩形裁剪。
    Rect(Rect),
    /// 圆角矩形裁剪。
    RoundedRect { rect: Rect, radius: f64 },
    /// 圆形裁剪。
    Circle { center: Point, radius: f64 },
    /// 椭圆裁剪（可旋转）。
    Ellipse {
        center: Point,
        radii: Vec2,
        rotation: f64,
    },
    /// 任意贝塞尔路径裁剪（支持多子路径，默认 nonzero 填充规则）。
    Path(BezPath),
}

impl Clip {
    #[must_use]
    pub fn rect(rect: Rect) -> Self {
        Self::Rect(rect)
    }

    #[must_use]
    pub fn rounded_rect(rect: Rect, radius: f64) -> Self {
        Self::RoundedRect { rect, radius }
    }

    #[must_use]
    pub fn circle(center: Point, radius: f64) -> Self {
        Self::Circle { center, radius }
    }

    #[must_use]
    pub fn ellipse(center: Point, radii: Vec2, rotation: f64) -> Self {
        Self::Ellipse {
            center,
            radii,
            rotation,
        }
    }

    #[must_use]
    pub fn path(path: BezPath) -> Self {
        Self::Path(path)
    }

    /// 转为 kurbo 贝塞尔路径（供需要路径的渲染后端，如 vello）。
    #[must_use]
    pub fn to_bezpath(&self) -> BezPath {
        use kurbo::Shape;
        match self {
            Clip::Rect(rect) => {
                let mut p = BezPath::new();
                p.move_to((rect.x, rect.y));
                p.line_to((rect.x + rect.width, rect.y));
                p.line_to((rect.x + rect.width, rect.y + rect.height));
                p.line_to((rect.x, rect.y + rect.height));
                p.close_path();
                p
            }
            Clip::RoundedRect { rect, radius } => kurbo::RoundedRect::new(
                rect.x,
                rect.y,
                rect.x + rect.width,
                rect.y + rect.height,
                *radius,
            )
            .to_path(0.1),
            Clip::Circle { center, radius } => {
                kurbo::Circle::new((center.x, center.y), *radius).to_path(0.1)
            }
            Clip::Ellipse {
                center,
                radii,
                rotation,
            } => kurbo::Ellipse::new((center.x, center.y), (radii.x, radii.y), *rotation)
                .to_path(0.1),
            Clip::Path(path) => path.clone(),
        }
    }
}

/// 填充 + 描边样式。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FillStrokeStyle {
    pub fill: Option<Fill>,
    pub stroke: Option<Stroke>,
}

impl FillStrokeStyle {
    /// 纯填充样式。
    #[must_use]
    pub fn fill(color: Color) -> Self {
        Self {
            fill: Some(Fill::Solid(color)),
            stroke: None,
        }
    }

    /// 线性渐变填充样式。
    #[must_use]
    pub fn fill_gradient(gradient: impl Into<Fill>) -> Self {
        Self {
            fill: Some(gradient.into()),
            stroke: None,
        }
    }

    /// 纯描边样式。
    #[must_use]
    pub fn stroke(color: Color, width: f64) -> Self {
        Self {
            fill: None,
            stroke: Some(Stroke {
                color,
                width,
                ..Default::default()
            }),
        }
    }
}

/// 填充定义。
#[derive(Debug, Clone, PartialEq)]
pub enum Fill {
    Solid(Color),
    LinearGradient(LinearGradient),
    RadialGradient(RadialGradient),
}

impl From<Color> for Fill {
    fn from(c: Color) -> Self {
        Self::Solid(c)
    }
}

impl From<LinearGradient> for Fill {
    fn from(g: LinearGradient) -> Self {
        Self::LinearGradient(g)
    }
}

impl From<RadialGradient> for Fill {
    fn from(g: RadialGradient) -> Self {
        Self::RadialGradient(g)
    }
}

/// 线性渐变（`GradientPath` 与 `Fill::LinearGradient` 共用）。
#[derive(Debug, Clone, PartialEq)]
pub struct LinearGradient {
    pub start: Point,
    pub end: Point,
    pub stops: Vec<GradientStop>,
}

/// 径向渐变：从 `center` 以 `radius` 为外圈半径向外铺色。
#[derive(Debug, Clone, PartialEq)]
pub struct RadialGradient {
    pub center: Point,
    pub radius: f64,
    pub stops: Vec<GradientStop>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GradientStop {
    pub offset: f64,
    pub color: Color,
}

/// 描边定义。
#[derive(Debug, Clone, PartialEq)]
pub struct Stroke {
    pub color: Color,
    pub width: f64,
    /// 线帽：butt / round / square。
    pub line_cap: LineCap,
    /// 连接：miter / round / bevel。
    pub line_join: LineJoin,
    /// 虚线数组（on/off 交替，单位与 `width` 相同）。空表示实线。
    pub dash_array: Vec<f64>,
    /// 虚线起点偏移。
    pub dash_offset: f64,
    /// 尖角连接上限（miter join）。默认 10.0，与 SVG / kurbo 对齐。
    pub miter_limit: f64,
}

impl Default for Stroke {
    fn default() -> Self {
        Self {
            color: Color::default(),
            width: 0.0,
            line_cap: LineCap::default(),
            line_join: LineJoin::default(),
            dash_array: Vec::new(),
            dash_offset: 0.0,
            miter_limit: 10.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LineCap {
    #[default]
    Butt,
    Round,
    Square,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LineJoin {
    #[default]
    Miter,
    Round,
    Bevel,
}

impl Stroke {
    /// 便捷构造：纯色描边。
    #[must_use]
    pub fn new(color: Color, width: f64) -> Self {
        Self {
            color,
            width,
            ..Default::default()
        }
    }

    /// 便捷构造：虚线描边（`dash_array` 为 on/off 交替长度）。
    #[must_use]
    pub fn dashed(color: Color, width: f64, dash_array: Vec<f64>) -> Self {
        Self {
            color,
            width,
            dash_array,
            ..Default::default()
        }
    }
}

/// 声明式图元枚举。仅描述几何与样式。
#[derive(Debug, Clone)]
pub enum Element {
    Rect {
        rect: Rect,
        style: FillStrokeStyle,
    },
    Circle {
        center: Point,
        radius: f64,
        style: FillStrokeStyle,
    },
    /// 椭圆（可旋转）：`radii` 为两个半轴长，`rotation` 为绕中心的旋转（弧度）。
    Ellipse {
        center: Point,
        radii: Vec2,
        rotation: f64,
        style: FillStrokeStyle,
    },
    /// 圆角矩形：`radius` 为四角统一圆角半径。
    RoundedRect {
        rect: Rect,
        radius: f64,
        style: FillStrokeStyle,
    },
    Line {
        start: Point,
        end: Point,
        style: Stroke,
    },
    /// 开放折线（仅描边）。
    Polyline {
        points: Vec<Point>,
        style: Stroke,
    },
    /// 闭合多边形（填充 + 可选描边）。
    Polygon {
        points: Vec<Point>,
        style: FillStrokeStyle,
    },
    /// 圆弧（开放，仅描边）。`radii` 为两半轴长，角度单位为弧度。
    Arc {
        center: Point,
        radii: Vec2,
        start_angle: f64,
        sweep_angle: f64,
        style: Stroke,
    },
    /// 扇形（闭合，含圆心到两端点的连线，填充 + 可选描边）。适合饼图。
    Pie {
        center: Point,
        radius: f64,
        start_angle: f64,
        sweep_angle: f64,
        style: FillStrokeStyle,
    },
    /// 闭合/开放路径，使用 kurbo 贝塞尔路径。
    Path {
        path: BezPath,
        style: FillStrokeStyle,
        /// true 表示闭合路径（用于填充）。
        closed: bool,
    },
    /// 带渐变填充的路径。
    GradientPath {
        path: BezPath,
        gradient: LinearGradient,
        stroke: Option<Stroke>,
    },
    /// 文本：纯文本 + 样式，可选预排版 layout。
    Text {
        content: String,
        position: Point,
        style: crate::text::TextStyle,
        /// 预排版结果。存在时后端应优先使用（精确字形定位）；
        /// 不存在时后端可回退到原生文本输出或自行排版。
        layout: Option<std::sync::Arc<TextLayout>>,
    },
    /// 递归组合：子节点在父变换与层级下绘制。
    Group {
        children: Vec<SceneNode>,
    },
}

impl Element {
    /// 矩形图元。
    #[must_use]
    pub fn rect(rect: Rect, style: FillStrokeStyle) -> Self {
        Self::Rect { rect, style }
    }

    /// 圆图元。
    #[must_use]
    pub fn circle(center: Point, radius: f64, style: FillStrokeStyle) -> Self {
        Self::Circle {
            center,
            radius,
            style,
        }
    }

    /// 椭圆图元。
    #[must_use]
    pub fn ellipse(center: Point, radii: Vec2, rotation: f64, style: FillStrokeStyle) -> Self {
        Self::Ellipse {
            center,
            radii,
            rotation,
            style,
        }
    }

    /// 圆角矩形图元。
    #[must_use]
    pub fn rounded_rect(rect: Rect, radius: f64, style: FillStrokeStyle) -> Self {
        Self::RoundedRect {
            rect,
            radius,
            style,
        }
    }

    /// 线段图元。
    #[must_use]
    pub fn line(start: Point, end: Point, style: Stroke) -> Self {
        Self::Line { start, end, style }
    }

    /// 折线图元（开放，仅描边）。
    #[must_use]
    pub fn poly(points: Vec<Point>, style: Stroke) -> Self {
        Self::Polyline { points, style }
    }

    /// 多边形图元（闭合，可填充 + 描边）。
    #[must_use]
    pub fn polygon(points: Vec<Point>, style: FillStrokeStyle) -> Self {
        Self::Polygon { points, style }
    }

    /// 圆弧图元（开放，仅描边）。
    #[must_use]
    pub fn arc(
        center: Point,
        radii: Vec2,
        start_angle: f64,
        sweep_angle: f64,
        style: Stroke,
    ) -> Self {
        Self::Arc {
            center,
            radii,
            start_angle,
            sweep_angle,
            style,
        }
    }

    /// 扇形图元（闭合，填充 + 可选描边）。
    #[must_use]
    pub fn pie(
        center: Point,
        radius: f64,
        start_angle: f64,
        sweep_angle: f64,
        style: FillStrokeStyle,
    ) -> Self {
        Self::Pie {
            center,
            radius,
            start_angle,
            sweep_angle,
            style,
        }
    }

    /// 纯文本图元（layout 为空，由后端自行排版）。
    #[must_use]
    pub fn text(
        content: impl Into<String>,
        position: Point,
        style: crate::text::TextStyle,
    ) -> Self {
        Self::Text {
            content: content.into(),
            position,
            style,
            layout: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造一个 x 坐标可区分、z_index 可指定的矩形节点（用于稳定性断言）。
    fn rect_node(x: f64, z: i32) -> SceneNode {
        SceneNode::new(Element::rect(
            Rect::new(x, 0.0, 1.0, 1.0),
            FillStrokeStyle::fill(Color::BLACK),
        ))
        .with_z(z)
    }

    fn rect_x(node: &SceneNode) -> f64 {
        match &node.element {
            Element::Rect { rect, .. } => rect.x,
            _ => panic!("expected rect"),
        }
    }

    #[test]
    fn push_accepts_element_and_scene_node() {
        let mut scene = Scene::new(100.0, 100.0);
        scene.push(Element::rect(
            Rect::new(0.0, 0.0, 1.0, 1.0),
            FillStrokeStyle::fill(Color::BLACK),
        ));
        scene.push(SceneNode::new(Element::rect(
            Rect::new(0.0, 0.0, 1.0, 1.0),
            FillStrokeStyle::fill(Color::BLACK),
        )));
        assert_eq!(scene.nodes.len(), 2);
    }

    #[test]
    fn iter_ordered_sorts_z_ascending() {
        let mut scene = Scene::new(100.0, 100.0);
        scene.push_node(rect_node(0.0, 5));
        scene.push_node(rect_node(0.0, -3));
        scene.push_node(rect_node(0.0, 1));
        let zs: Vec<i32> = scene.iter_ordered().map(|n| n.z_index).collect();
        assert_eq!(zs, vec![-3, 1, 5]);
    }

    #[test]
    fn ordered_is_stable_within_same_z() {
        // 同 z_index 的三个节点，用可区分的 x 坐标验证保持插入次序。
        let nodes = vec![rect_node(10.0, 0), rect_node(20.0, 0), rect_node(30.0, 0)];
        let xs: Vec<f64> = SceneNode::ordered(&nodes).map(rect_x).collect();
        assert_eq!(xs, vec![10.0, 20.0, 30.0]);
    }

    #[test]
    fn builder_chaining() {
        let node = SceneNode::new(Element::circle(
            Point::new(1.0, 2.0),
            3.0,
            FillStrokeStyle::stroke(Color::BLACK, 1.0),
        ))
        .with_z(7)
        .with_transform(Transform::translate(1.0, 2.0))
        .with_opacity(0.5)
        .with_name("node-a")
        .with_visible(false);
        assert_eq!(node.z_index, 7);
        assert_eq!(node.transform, Some(Transform::translate(1.0, 2.0)));
        assert_eq!(node.opacity, 0.5);
        assert_eq!(node.name.as_deref(), Some("node-a"));
        assert!(!node.visible);
    }

    #[test]
    fn node_defaults() {
        let node = SceneNode::new(Element::rect(
            Rect::new(0.0, 0.0, 1.0, 1.0),
            FillStrokeStyle::fill(Color::BLACK),
        ));
        assert_eq!(node.opacity, 1.0);
        assert!(node.name.is_none());
        assert!(node.visible);
    }

    #[test]
    fn scene_metadata_defaults_and_builders() {
        let scene = Scene::new(10.0, 20.0)
            .with_title("图表")
            .with_description("示例")
            .with_scale(2.0);
        assert_eq!(scene.title.as_deref(), Some("图表"));
        assert_eq!(scene.description.as_deref(), Some("示例"));
        assert_eq!(scene.scale, 2.0);
        let d = Scene::default();
        assert_eq!(d.scale, 1.0);
    }

    #[test]
    fn scene_node_group_and_push_group() {
        let group = SceneNode::group(vec![rect_node(0.0, 0), rect_node(0.0, 1)]);
        match &group.element {
            Element::Group { children } => assert_eq!(children.len(), 2),
            _ => panic!("expected group"),
        }
        let mut scene = Scene::new(10.0, 10.0);
        scene.push_group(vec![rect_node(0.0, 0)]);
        assert!(matches!(&scene.nodes[0].element, Element::Group { .. }));
    }

    #[test]
    fn from_element_conversion() {
        let node: SceneNode = Element::line(
            Point::new(0.0, 0.0),
            Point::new(1.0, 1.0),
            Stroke::new(Color::BLACK, 1.0),
        )
        .into();
        assert_eq!(node.z_index, 0);
        assert!(node.transform.is_none());
    }

    #[test]
    fn element_constructors() {
        let r = Element::rect(
            Rect::new(0.0, 0.0, 10.0, 10.0),
            FillStrokeStyle::fill(Color::BLACK),
        );
        assert!(matches!(r, Element::Rect { .. }));

        let c = Element::circle(
            Point::new(1.0, 2.0),
            3.0,
            FillStrokeStyle::fill(Color::BLACK),
        );
        assert!(matches!(c, Element::Circle { .. }));

        let e = Element::ellipse(
            Point::new(1.0, 2.0),
            Vec2::new(4.0, 2.0),
            0.1,
            FillStrokeStyle::fill(Color::BLACK),
        );
        assert!(matches!(e, Element::Ellipse { .. }));

        let rr = Element::rounded_rect(
            Rect::new(0.0, 0.0, 10.0, 10.0),
            3.0,
            FillStrokeStyle::fill(Color::BLACK),
        );
        assert!(matches!(rr, Element::RoundedRect { .. }));

        let l = Element::line(
            Point::new(0.0, 0.0),
            Point::new(1.0, 1.0),
            Stroke::new(Color::BLACK, 1.0),
        );
        assert!(matches!(l, Element::Line { .. }));

        let p = Element::poly(
            vec![Point::new(0.0, 0.0), Point::new(1.0, 1.0)],
            Stroke::new(Color::BLACK, 1.0),
        );
        assert!(matches!(p, Element::Polyline { .. }));

        let pg = Element::polygon(
            vec![
                Point::new(0.0, 0.0),
                Point::new(1.0, 1.0),
                Point::new(0.0, 1.0),
            ],
            FillStrokeStyle::fill(Color::BLACK),
        );
        assert!(matches!(pg, Element::Polygon { .. }));

        let a = Element::arc(
            Point::new(0.0, 0.0),
            Vec2::new(5.0, 5.0),
            0.0,
            std::f64::consts::FRAC_PI_2,
            Stroke::new(Color::BLACK, 1.0),
        );
        assert!(matches!(a, Element::Arc { .. }));

        let pie = Element::pie(
            Point::new(0.0, 0.0),
            5.0,
            0.0,
            std::f64::consts::FRAC_PI_2,
            FillStrokeStyle::fill(Color::BLACK),
        );
        assert!(matches!(pie, Element::Pie { .. }));

        let t = Element::text(
            "hi",
            Point::new(0.0, 0.0),
            crate::text::TextStyle::new(Color::BLACK, 12.0, "sans-serif"),
        );
        match t {
            Element::Text {
                content,
                position,
                style,
                layout,
            } => {
                assert_eq!(content, "hi");
                assert_eq!(position, Point::new(0.0, 0.0));
                assert_eq!(style.font_size, 12.0);
                assert!(layout.is_none());
            }
            _ => panic!("expected text"),
        }
    }

    #[test]
    fn stroke_dash_and_miter_defaults() {
        let s = Stroke::new(Color::BLACK, 2.0);
        assert!(s.dash_array.is_empty());
        assert_eq!(s.dash_offset, 0.0);
        assert_eq!(s.miter_limit, 10.0);

        let d = Stroke::dashed(Color::BLACK, 2.0, vec![4.0, 2.0]);
        assert_eq!(d.dash_array, vec![4.0, 2.0]);
    }

    #[test]
    fn radial_gradient_into_fill() {
        let g = RadialGradient {
            center: Point::new(0.0, 0.0),
            radius: 10.0,
            stops: vec![
                GradientStop {
                    offset: 0.0,
                    color: Color::BLACK,
                },
                GradientStop {
                    offset: 1.0,
                    color: Color::WHITE,
                },
            ],
        };
        let style = FillStrokeStyle::fill_gradient(g);
        assert!(matches!(style.fill, Some(Fill::RadialGradient(_))));
    }

    #[test]
    fn rect_center_helper() {
        assert_eq!(
            Rect::new(2.0, 4.0, 10.0, 6.0).center(),
            Point::new(7.0, 7.0)
        );
    }

    #[test]
    fn layer_defaults_and_builders() {
        let layer = Layer::new("data").with_nodes(vec![rect_node(0.0, 1)]);
        assert_eq!(layer.name, "data");
        assert!(layer.visible);
        assert_eq!(layer.opacity, 1.0);
        assert!(layer.transform.is_none());
        assert_eq!(layer.nodes.len(), 1);

        let l2 = Layer::new("x")
            .with_visible(false)
            .with_opacity(0.5)
            .with_transform(Transform::translate(1.0, 2.0));
        assert!(!l2.visible);
        assert_eq!(l2.opacity, 0.5);
        assert_eq!(l2.transform, Some(Transform::translate(1.0, 2.0)));
    }

    #[test]
    fn layer_iter_ordered_sorts_within_layer() {
        let layer = Layer::new("l").with_nodes(vec![
            rect_node(0.0, 5),
            rect_node(0.0, -2),
            rect_node(0.0, 1),
        ]);
        let zs: Vec<i32> = layer.iter_ordered().map(|n| n.z_index).collect();
        assert_eq!(zs, vec![-2, 1, 5]);
    }

    #[test]
    fn scene_push_layer_and_lookup() {
        let mut scene = Scene::new(100.0, 100.0);
        scene.push_layer(Layer::new("grid"));
        scene.push_layer(Layer::new("data").with_nodes(vec![rect_node(0.0, 0)]));
        assert_eq!(scene.layers().len(), 2);
        assert!(scene.layer("data").is_some());
        assert!(scene.layer("missing").is_none());
        scene.layer_mut("grid").unwrap().visible = false;
        assert!(!scene.layers[0].visible);
    }

    #[test]
    fn scene_default_has_empty_layers() {
        let scene = Scene::new(10.0, 10.0);
        assert!(scene.layers.is_empty());
    }

    #[test]
    fn clip_constructors_and_bezpath() {
        let c = Clip::circle(Point::new(1.0, 2.0), 5.0);
        assert!(matches!(c, Clip::Circle { radius: 5.0, .. }));
        // to_bezpath 应产出闭合的圆路径
        let p = c.to_bezpath();
        assert!(!p.is_empty());

        let r = Clip::rect(Rect::new(0.0, 0.0, 10.0, 20.0));
        assert!(matches!(r, Clip::Rect(_)));
        assert!(!r.to_bezpath().is_empty());

        let rr = Clip::rounded_rect(Rect::new(0.0, 0.0, 10.0, 10.0), 2.0);
        assert!(matches!(rr, Clip::RoundedRect { .. }));

        let e = Clip::ellipse(Point::new(0.0, 0.0), Vec2::new(3.0, 4.0), 0.0);
        assert!(matches!(e, Clip::Ellipse { .. }));

        let mut bp = BezPath::new();
        bp.move_to((0.0, 0.0));
        bp.line_to((1.0, 0.0));
        let p2 = Clip::path(bp);
        assert!(matches!(p2, Clip::Path(_)));
        assert!(!p2.to_bezpath().is_empty());
    }

    #[test]
    fn scene_node_clip_builder() {
        let node = SceneNode::new(Element::rect(
            Rect::new(0.0, 0.0, 10.0, 10.0),
            FillStrokeStyle::fill(Color::BLACK),
        ))
        .with_clip(Clip::circle(Point::new(5.0, 5.0), 3.0));
        assert!(node.clip.is_some());
        // 默认无 clip
        let n = SceneNode::new(Element::rect(
            Rect::new(0.0, 0.0, 1.0, 1.0),
            FillStrokeStyle::fill(Color::BLACK),
        ));
        assert!(n.clip.is_none());
    }
}
