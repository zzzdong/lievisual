//! lievisual 丰富版演示：构建一个覆盖新图元/样式/节点属性/图层（Layer）的场景，输出 SVG 与 PNG。
//!
//! 运行：
//!   cargo run --example demo（同时输出 demo.svg 与 demo.png）
//!
//! 场景按图层组织（从底到顶）：
//! - 默认层 `nodes`：网格虚线 + 底部说明文本
//! - `shapes` 层：椭圆 / 圆角矩形 / 多边形 / 圆弧 / 组合（演示 layer 级透明度与变换）
//! - `pie` 层：饼图（层内节点透明度分级）
//! - `labels` 层：顶部标题文本

use lievisual::geometry::{Color, Point, Transform, Vec2};
use lievisual::render::SvgRenderer;
use lievisual::scene::{
    Element, FillStrokeStyle, GradientStop, Layer, LinearGradient, RadialGradient, Scene,
    SceneNode, Stroke,
};
use lievisual::text::TextStyle;

fn build_scene() -> Scene {
    let mut scene = Scene::new(560.0, 400.0)
        .with_background(Color::WHITE)
        .with_title("lievisual 丰富 IR 演示")
        .with_description("展示图层 + 椭圆/圆角矩形/多边形/圆弧/扇形、径向渐变、虚线、透明度与命名")
        .with_scale(2.0);

    // ---------- 默认层（nodes）：网格虚线 ----------
    let dashed = Stroke::dashed(Color::rgb(0xbb, 0xbb, 0xbb), 1.0, vec![6.0, 4.0]);
    scene.push(Element::poly(
        vec![
            Point::new(20.0, 260.0),
            Point::new(120.0, 220.0),
            Point::new(220.0, 260.0),
            Point::new(320.0, 220.0),
        ],
        dashed,
    ));
    scene.push(Element::line(
        Point::new(20.0, 300.0),
        Point::new(540.0, 300.0),
        Stroke::new(Color::rgb(0xdd, 0xdd, 0xdd), 1.0),
    ));

    // ---------- shapes 层 ----------
    let linear = LinearGradient {
        start: Point::new(0.0, 0.0),
        end: Point::new(200.0, 200.0),
        stops: vec![
            GradientStop {
                offset: 0.0,
                color: Color::rgb(0x00, 0x66, 0xcc),
            },
            GradientStop {
                offset: 1.0,
                color: Color::rgb(0xcc, 0x00, 0x66),
            },
        ],
    };
    let mut shapes = Layer::new("shapes").with_opacity(0.95);

    // 圆（描边）
    shapes.push(
        SceneNode::new(Element::circle(
            Point::new(100.0, 100.0),
            60.0,
            FillStrokeStyle::stroke(Color::BLACK, 2.0),
        ))
        .with_name("circle-outline"),
    );
    // 椭圆（线性渐变填充 + 旋转）
    shapes.push(
        SceneNode::new(Element::ellipse(
            Point::new(180.0, 80.0),
            Vec2::new(55.0, 28.0),
            0.5,
            FillStrokeStyle {
                fill: Some(linear.clone().into()),
                stroke: Some(Stroke::new(Color::rgb(0x22, 0x22, 0x22), 1.5)),
            },
        ))
        .with_name("ellipse-rotated"),
    );
    // 圆角矩形（径向渐变填充）
    let radial = RadialGradient {
        center: Point::new(300.0, 70.0),
        radius: 60.0,
        stops: vec![
            GradientStop {
                offset: 0.0,
                color: Color::rgb(0xff, 0xff, 0xff),
            },
            GradientStop {
                offset: 1.0,
                color: Color::rgb(0xee, 0x99, 0x00),
            },
        ],
    };
    shapes.push(
        SceneNode::new(Element::rounded_rect(
            lievisual::Rect::new(270.0, 30.0, 120.0, 70.0),
            14.0,
            FillStrokeStyle {
                fill: Some(radial.into()),
                stroke: Some(Stroke::new(Color::rgb(0x33, 0x33, 0x33), 1.0)),
            },
        ))
        .with_name("rounded-card"),
    );
    // 多边形
    shapes.push(SceneNode::new(Element::polygon(
        vec![
            Point::new(400.0, 150.0),
            Point::new(450.0, 90.0),
            Point::new(520.0, 120.0),
            Point::new(500.0, 200.0),
            Point::new(420.0, 200.0),
        ],
        FillStrokeStyle {
            fill: Some(Color::rgb(0x66, 0xcc, 0x66).into()),
            stroke: Some(Stroke::new(Color::rgb(0x22, 0x66, 0x22), 1.5)),
        },
    )));
    // 圆弧
    shapes.push(SceneNode::new(Element::arc(
        Point::new(120.0, 320.0),
        Vec2::new(50.0, 40.0),
        0.3,
        4.2,
        Stroke::new(Color::rgb(0x00, 0x88, 0x88), 3.0),
    )));
    // 裁剪：把超出圆形的矩形部分裁掉（clip 演示）
    shapes.push(
        SceneNode::new(Element::rect(
            lievisual::Rect::new(20.0, 20.0, 80.0, 80.0),
            FillStrokeStyle::fill(Color::rgb(0x88, 0x44, 0xcc)),
        ))
        .with_transform(Transform::translate(360.0, 60.0))
        .with_clip(lievisual::Clip::circle(Point::new(40.0, 40.0), 30.0)),
    );
    // 组合 + 变换 + 透明度
    let box_children = vec![
        SceneNode::new(Element::rounded_rect(
            lievisual::Rect::new(0.0, 0.0, 90.0, 50.0),
            8.0,
            FillStrokeStyle::fill(Color::rgb(0xff, 0xcc, 0x00)),
        )),
        SceneNode::new(Element::text(
            "group",
            Point::new(12.0, 30.0),
            TextStyle::new(Color::rgb(0x33, 0x33, 0x33), 14.0, "sans-serif"),
        )),
    ];
    shapes.push(
        SceneNode::group(box_children)
            .with_transform(Transform::translate(430.0, 260.0).then(&Transform::rotate(0.25)))
            .with_name("demo-group"),
    );

    // ---------- pie 层（饼图 + 层内节点透明度分级） ----------
    let pie_style = |c: Color| FillStrokeStyle {
        fill: Some(c.into()),
        stroke: Some(Stroke::new(Color::WHITE, 1.5)),
    };
    let mut pie = Layer::new("pie");
    pie.push_node(SceneNode::new(Element::pie(
        Point::new(300.0, 320.0),
        70.0,
        0.0,
        1.2,
        pie_style(Color::rgb(0xe6, 0x19, 0x4b)),
    )));
    pie.push_node(
        SceneNode::new(Element::pie(
            Point::new(300.0, 320.0),
            70.0,
            1.2,
            1.6,
            pie_style(Color::rgb(0xf0, 0x82, 0x0c)),
        ))
        .with_opacity(0.75),
    );
    pie.push_node(
        SceneNode::new(Element::pie(
            Point::new(300.0, 320.0),
            70.0,
            2.8,
            1.4,
            pie_style(Color::rgb(0x9a, 0xC7, 0x00)),
        ))
        .with_opacity(0.5),
    );
    pie.push_node(
        SceneNode::new(Element::pie(
            Point::new(300.0, 320.0),
            70.0,
            4.2,
            2.08,
            pie_style(Color::rgb(0x42, 0x62, 0xac)),
        ))
        .with_opacity(0.25),
    );

    // ---------- labels 层（顶部文本；演示 baseline/weight/align 与 measure_text） ----------
    let title_style = TextStyle::new(Color::rgb(0x11, 0x11, 0x11), 20.0, "sans-serif")
        .with_weight(700.0)
        .with_align(lievisual::TextAlign::Left);
    // 先测量再做水平居中（canvas 风格：measureText → 用 width 定位锚点）。
    // 文本以样式化片段表达：单个 RichSpan 即单文本。
    let m = lievisual::measure_text(
        std::slice::from_ref(&lievisual::RichSpan::new(
            "lievisual · 丰富 IR · Layer",
            title_style.clone(),
        )),
        None,
    );
    let title_x = 280.0 - m.size.width / 2.0; // 画布中心 280 居中
    let labels = Layer::new("labels").with_nodes(vec![SceneNode::new(Element::text(
        "lievisual · 丰富 IR · Layer",
        Point::new(title_x, 380.0),
        title_style,
    ))]);

    // 图层顺序（从底到顶）：默认层 nodes → shapes → pie → labels
    scene.push_layer(shapes);
    scene.push_layer(pie);
    scene.push_layer(labels);

    scene
}

fn main() {
    let scene = build_scene();

    // SVG 输出（render_scene 会自动捕获 scene 的 title/description/scale 与各图层）
    let mut svg = SvgRenderer::new(scene.width, scene.height).with_background(scene.background);
    scene.render(&mut svg);
    let svg_str = svg.into_string();
    std::fs::write("demo.svg", &svg_str).expect("写 SVG 失败");
    println!("SVG 已写入 demo.svg ({} 字节)", svg_str.len());
    println!("  图层数: {}", scene.layers().len());
    for l in scene.layers() {
        println!(
            "  - {} ({} 节点, visible={})",
            l.name,
            l.nodes.len(),
            l.visible
        );
    }
    // 演示 measure_text / TextMetrics（canvas 风格）
    let demo_style = lievisual::TextStyle::new(Color::BLACK, 20.0, "sans-serif");
    let tm = lievisual::measure_text(
        std::slice::from_ref(&lievisual::RichSpan::new(
            "Measure 示例",
            demo_style.clone(),
        )),
        None,
    );
    println!(
        "  测量 \"Measure 示例\": width={:.2} height={:.2} alphabetic_baseline={:.2} ascent={:.2} descent={:.2}",
        tm.metrics.width,
        tm.metrics.height,
        tm.metrics.alphabetic_baseline,
        tm.metrics.font_bounding_box_ascent,
        tm.metrics.font_bounding_box_descent
    );

    // PNG 输出
    {
        use lievisual::render::VelloPixmapRenderer;
        // scale 提升栅格分辨率。
        let mut renderer = VelloPixmapRenderer::new(
            (scene.width * scene.scale) as u32,
            (scene.height * scene.scale) as u32,
        )
        .with_background(scene.background);
        let png = renderer.render_png(&scene);
        std::fs::write("demo.png", &png).expect("写 PNG 失败");
        println!("PNG 已写入 demo.png ({} 字节)", png.len());
    }
}
