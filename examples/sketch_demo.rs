//! 手绘（sketch）风格演示。
//!
//! ```text
//! cargo run --example sketch_demo
//! ```
//!
//! 产出（写在工作目录）：
//! - `sketch_normal.svg`    —— 不开手绘：与改造前完全一致
//! - `sketch_all.svg/.png`  —— 全量手绘 + 排线填充
//! - `sketch_selected.svg`  —— **选择性**手绘：节点/图元手绘，网格线与连线保持笔直
//! - `sketch_fills.svg`     —— 四种填充样式（分四次按名字选择不同的图元）
//!
//! 两个要点：
//! 1. 手绘是**额外开启**的：不调用任何 `roughen_*`，场景与输出完全不变。
//! 2. 开启后仍可**选择**范围：`SketchOptions::kinds` 按图元类型，`roughen_scene_if` 按谓词。

use lievisual::render::{SvgRenderer, VelloPixmapRenderer};
use lievisual::scene::{Element, FillStrokeStyle, Layer, Scene, SceneNode, Stroke};
use lievisual::sketch::{
    SketchFillStyle, SketchKind, SketchKinds, SketchOptions, roughen_scene, roughen_scene_fit,
    roughen_scene_if,
};
use lievisual::text::{TextAlign, TextStyle};
use lievisual::{Color, Point, Rect, Vec2};

const W: f64 = 560.0;
const H: f64 = 250.0;

fn node_fill() -> Color {
    Color::rgb(0xEC, 0xEC, 0xFF)
}
fn ink() -> Color {
    Color::rgb(0x33, 0x33, 0x33)
}
fn grid_color() -> Color {
    Color::rgb(0xDD, 0xDD, 0xDD)
}

fn node_style(fill: Color) -> FillStrokeStyle {
    FillStrokeStyle {
        fill: Some(fill.into()),
        stroke: Some(Stroke::new(ink(), 1.5)),
    }
}

fn label(text: &str, at: Point) -> Element {
    Element::text(
        text,
        at,
        TextStyle::new(ink(), 16.0, "sans-serif").with_align(TextAlign::Center),
    )
}

/// 一个"流程图 + 若干图元 + 网格"的场景。
fn build_scene() -> Scene {
    let mut scene = Scene::new(W, H);

    // 网格（独立图层，用来演示"网格不参与手绘"）。
    let mut grid = Layer::new("grid");
    for i in 0..=7 {
        let x = i as f64 * 80.0;
        grid.push(Element::line(
            Point::new(x, 0.0),
            Point::new(x, H),
            Stroke::new(grid_color(), 1.0),
        ));
    }
    for j in 0..=3 {
        let y = j as f64 * 80.0;
        grid.push(Element::line(
            Point::new(0.0, y),
            Point::new(W, y),
            Stroke::new(grid_color(), 1.0),
        ));
    }
    scene.push_layer(grid);

    // 两个圆角矩形节点 + 连线。
    scene.push(Element::rounded_rect(
        Rect::new(40.0, 40.0, 190.0, 100.0),
        10.0,
        node_style(node_fill()),
    ));
    scene.push(label("开始", Point::new(115.0, 76.0)));
    scene.push(Element::rounded_rect(
        Rect::new(330.0, 40.0, 480.0, 100.0),
        10.0,
        node_style(node_fill()),
    ));
    scene.push(label("结束", Point::new(405.0, 76.0)));
    scene.push(Element::poly(
        vec![
            Point::new(190.0, 70.0),
            Point::new(260.0, 70.0),
            Point::new(260.0, 160.0),
            Point::new(330.0, 160.0),
        ],
        Stroke::new(ink(), 2.0),
    ));

    // 一排几何图元，覆盖 circle / ellipse / polygon / pie。
    scene.push(Element::circle(
        Point::new(80.0, 180.0),
        26.0,
        node_style(Color::rgb(0xFF, 0xE0, 0xE0)),
    ));
    scene.push(Element::ellipse(
        Point::new(200.0, 180.0),
        Vec2::new(44.0, 22.0),
        0.0,
        node_style(Color::rgb(0xE0, 0xFF, 0xE0)),
    ));
    scene.push(Element::polygon(
        vec![
            Point::new(320.0, 150.0),
            Point::new(360.0, 180.0),
            Point::new(320.0, 210.0),
            Point::new(280.0, 180.0),
        ],
        node_style(Color::rgb(0xFF, 0xF3, 0xD6)),
    ));
    scene.push(Element::pie(
        Point::new(460.0, 180.0),
        34.0,
        0.35,
        4.6,
        node_style(Color::rgb(0xEF, 0xE0, 0xFF)),
    ));

    scene
}

fn write(name: &str, svg: &str) {
    std::fs::write(name, svg).expect("写文件失败");
    println!("  {name:<22} {:>7} 字节", svg.len());
}

fn render_svg(scene: &Scene) -> String {
    let mut r = SvgRenderer::new(scene.width, scene.height).with_background(scene.background);
    scene.render(&mut r);
    r.into_string()
}

fn main() {
    // 1) 基线：不调用任何 roughen_* —— 与改造前逐字节一致。
    let baseline = build_scene();
    write("sketch_normal.svg", &render_svg(&baseline));

    // 2) 全量手绘 + 排线填充，并重新贴合画布（抖动可能顶到边缘）。
    let mut all = build_scene();
    roughen_scene_fit(
        &mut all,
        &SketchOptions::new()
            .with_seed(42)
            .with_fill(SketchFillStyle::Hachure),
        8.0,
    );
    write("sketch_all.svg", &render_svg(&all));
    let mut png = VelloPixmapRenderer::new(all.width.round() as u32, all.height.round() as u32)
        .with_background(Color::WHITE);
    let bytes = png.render_png(&all);
    std::fs::write("sketch_all.png", &bytes).expect("写 PNG 失败");
    println!(
        "  {:<22} {:>7} 字节 (画布 {}×{})",
        "sketch_all.png",
        bytes.len(),
        all.width.round(),
        all.height.round()
    );

    // 3) 选择性：图元手绘，网格线与连线保持笔直（kinds 排除 Line / Polyline）。
    let mut selected = build_scene();
    roughen_scene(
        &mut selected,
        &SketchOptions::new().with_seed(7).with_kinds(
            SketchKinds::ALL
                .without(SketchKind::Line)
                .without(SketchKind::Polyline),
        ),
    );
    write("sketch_selected.svg", &render_svg(&selected));

    // 4) 四种填充样式：同一个场景里按 name 选择，分四次各自开启一种样式。
    let mut fills = Scene::new(520.0, 140.0);
    for (i, name) in ["hachure", "cross", "zigzag", "dots"]
        .into_iter()
        .enumerate()
    {
        let x = 20.0 + i as f64 * 125.0;
        fills.push(
            SceneNode::new(Element::rect(
                Rect::new(x, 20.0, x + 100.0, 110.0),
                node_style(Color::rgb(0xD6, 0xE4, 0xF7)),
            ))
            .with_name(name),
        );
    }
    for (name, style) in [
        ("hachure", SketchFillStyle::Hachure),
        ("cross", SketchFillStyle::CrossHatch),
        ("zigzag", SketchFillStyle::Zigzag),
        ("dots", SketchFillStyle::Dots),
    ] {
        let want = name.to_string();
        roughen_scene_if(
            &mut fills,
            &SketchOptions::new().with_seed(3).with_fill(style),
            &move |node| node.name.as_deref() == Some(want.as_str()),
        );
    }
    write("sketch_fills.svg", &render_svg(&fills));

    println!("\n说明：手绘默认关闭；开启后可用 kinds / 谓词选择范围，文本与图片永不参与。");
}
