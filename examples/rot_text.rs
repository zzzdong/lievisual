//! 最小复现：同一 Scene 分别走 SVG / Pixmap 后端，对比旋转文本表现。
//!
//! 运行：cargo run --example rot_text

use lievisual::geometry::{Color, Point};
use lievisual::render::{Renderer as _, SvgRenderer, VelloPixmapRenderer};
use lievisual::scene::{Element, Scene};
use lievisual::text::{TextAlign, TextBaseline, TextStyle};

fn main() {
    let mut scene = Scene::new(400.0, 300.0).with_background(Color::WHITE);

    let style = TextStyle::new(Color::BLACK, 20.0, "sans-serif")
        .with_align(TextAlign::Left)
        .with_baseline(TextBaseline::Top)
        .with_rotation(45_f64.to_radians());

    scene.push(Element::text(
        "2026-10-01",
        Point::new(50.0, 80.0),
        style.clone(),
    ));
    scene.push(Element::text(
        "不旋转的对照",
        Point::new(50.0, 220.0),
        style.with_rotation(0.0),
    ));

    // SVG
    let mut r = SvgRenderer::new(400.0, 300.0);
    r.render_scene(&scene);
    std::fs::write("rot_text.svg", r.into_string()).unwrap();
    println!("wrote rot_text.svg");

    // PNG
    let mut r = VelloPixmapRenderer::new(400, 300);
    let pixmap = r.render_scene_to_pixmap(&scene);
    std::fs::write("rot_text.png", pixmap.to_png()).unwrap();
    println!("wrote rot_text.png");
}
