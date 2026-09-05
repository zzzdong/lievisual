//! Scene traversal: turn selected elements into their hand-drawn counterparts.
//!
//! ## Selection (this is the whole point of the module)
//!
//! A sketch pass is **explicitly requested**, and it is *selective* on two levels:
//! 1. [`SketchOptions::kinds`] — a set of element kinds (rect / circle / path / …). A caller that
//!    only wants node boxes sketched but leaves sub-graph containers, grid lines and images
//!    untouched passes `SketchKinds::only(&[SketchKind::RoundedRect])`.
//! 2. `roughen_scene_if(..., predicate)` — an arbitrary `Fn(&SceneNode) -> bool`. A node for which
//!    the predicate returns `false` is skipped **together with its whole sub-tree**, so one can
//!    select by layer, by `name`, by z-range, …
//!
//! Nothing happens by default: a scene that never goes through one of these functions keeps its
//! exact geometry and its exact rendering.
//!
//! ## What a single element becomes
//!
//! ```text
//! Rect { fill: Solid(c), stroke: Some(s) }
//!   → Group[ Rect { fill: Sketch(c), stroke: None },      // 填充区：原几何，未抖动
//!            Path { jittered outline, stroke: s } ]       // 描边：抖动后的贝塞尔
//! ```
//!
//! Fill and outline are deliberately separated: the fill must stay inside the *original* outline
//! (a pattern fill clipped by the shape), while the outline is allowed to overshoot it — that
//! overshoot is most of the "drawn by hand" character.

use super::rough;
use super::{Rng, SketchKind, SketchOptions};
use crate::fit::{FitOptions, element_bounds, fit_scene};
use crate::geometry::{BezPath, Color, Vec2};
use crate::scene::{Element, Fill, FillStrokeStyle, Scene, SceneNode, SketchFill, Stroke};

/// Apply the sketch pass to the whole scene (subject to `opts.kinds`).
pub fn roughen_scene(scene: &mut Scene, opts: &SketchOptions) {
    roughen_scene_where(scene, opts, None);
}

/// Apply the sketch pass to every node accepted by `filter`.
///
/// A node rejected by the filter is skipped **with its whole sub-tree** (a Group's children are
/// not visited either), so a single predicate can exclude a whole layer or a named sub-graph.
pub fn roughen_scene_if(
    scene: &mut Scene,
    opts: &SketchOptions,
    filter: &dyn Fn(&SceneNode) -> bool,
) {
    roughen_scene_where(scene, opts, Some(filter));
}

/// [`roughen_scene`] followed by a refit — the recommended entry point for
/// content-driven canvases (diagram / chart exporters).
///
/// Jitter can push the outline a couple of units past the canvas edge; refitting with `margin`
/// pulls it back in. The refit is idempotent in the sense that an already-hugging scene only gets
/// shifted by the (tiny) overshoot — see the tests.
pub fn roughen_scene_fit(scene: &mut Scene, opts: &SketchOptions, margin: f64) {
    roughen_scene_fit_where(scene, opts, margin, None);
}

/// [`roughen_scene_fit`] restricted by a predicate (see [`roughen_scene_if`]).
pub fn roughen_scene_fit_if(
    scene: &mut Scene,
    opts: &SketchOptions,
    margin: f64,
    filter: &dyn Fn(&SceneNode) -> bool,
) {
    roughen_scene_fit_where(scene, opts, margin, Some(filter));
}

fn roughen_scene_fit_where(
    scene: &mut Scene,
    opts: &SketchOptions,
    margin: f64,
    filter: Option<&dyn Fn(&SceneNode) -> bool>,
) {
    roughen_scene_where(scene, opts, filter);
    fit_scene(
        scene,
        FitOptions::new().with_margin(if margin > 0.0 { margin } else { 0.0 }),
    );
}

fn roughen_scene_where(
    scene: &mut Scene,
    opts: &SketchOptions,
    filter: Option<&dyn Fn(&SceneNode) -> bool>,
) {
    let mut rng = match opts.seed {
        Some(seed) => Rng::new(seed),
        None => Rng::from_entropy(),
    };
    // Document order (default nodes, then layers bottom-to-top) so the same seed always yields
    // the same randomness per element.
    walk(&mut scene.nodes, &mut rng, opts, filter);
    for layer in &mut scene.layers {
        if !layer.visible {
            continue;
        }
        walk(&mut layer.nodes, &mut rng, opts, filter);
    }
}

fn walk(
    nodes: &mut [SceneNode],
    rng: &mut Rng,
    opts: &SketchOptions,
    filter: Option<&dyn Fn(&SceneNode) -> bool>,
) {
    for node in nodes.iter_mut() {
        if !node.visible {
            continue;
        }
        if let Some(f) = filter
            && !f(node)
        {
            continue;
        }
        if matches!(node.element, Element::Group { .. }) {
            if let Element::Group { children } = &mut node.element {
                walk(children, rng, opts, filter);
            }
            continue;
        }
        let Some(kind) = SketchKind::of(&node.element) else {
            continue;
        };
        if !opts.kinds.contains(kind) {
            continue;
        }
        let replacement = sketch_element(&node.element, rng, opts);
        if let Some(element) = replacement {
            node.element = element;
        }
    }
}

/// Sketch a single element: returns the replacement (fill first, outline second) or `None` when
/// there is nothing to draw by hand (text, image, an element without fill *and* stroke, …).
///
/// Exposed for callers that build their own pipeline; the result is a `Vec` because a filled and
/// stroked shape becomes two elements.
pub fn roughen_element(el: &Element, rng: &mut Rng, opts: &SketchOptions) -> Vec<Element> {
    let mut out = Vec::with_capacity(2);
    let (fill, outline) = sketch_parts(el, rng, opts);
    if let Some(fill) = fill {
        out.push(fill);
    }
    if let Some(outline) = outline {
        out.push(outline);
    }
    out
}

fn sketch_element(el: &Element, rng: &mut Rng, opts: &SketchOptions) -> Option<Element> {
    let (fill, outline) = sketch_parts(el, rng, opts);
    match (fill, outline) {
        (None, None) => None,
        (Some(f), None) => Some(f),
        (None, Some(o)) => Some(o),
        // Fill under outline — same as how a single filled+stroked shape is painted.
        (Some(f), Some(o)) => Some(Element::Group {
            children: vec![SceneNode::new(f), SceneNode::new(o)],
        }),
    }
}

/// Fill element + outline element for one input element.
///
/// The outline only drops the fill when the fill moved into its own (sketch) layer; otherwise the
/// original fill is carried over onto the outline path — otherwise "sketch outlines only"
/// (`opts.fill = None`) would silently delete every fill in the scene.
fn sketch_parts(
    el: &Element,
    rng: &mut Rng,
    opts: &SketchOptions,
) -> (Option<Element>, Option<Element>) {
    let fill = fill_element(el, opts);
    let outline = outline_element(el, fill.is_some(), rng, opts);
    (fill, outline)
}

/// The fill region: the **original** geometry with the fill swapped for [`Fill::Sketch`].
///
/// `None` when there is nothing to convert: no style requested, the element has no fill, the fill
/// is already a sketch fill, or the fill region is smaller than
/// [`SketchOptions::min_fill_area`] (tiny filled shapes — arrowheads, label backgrounds — read
/// much better with their solid ink preserved than with a 3-line hatch).
///
/// Gradients have no single colour, so they degrade deterministically to their *last* stop (the
/// outer ring of a radial gradient, the end of a linear one).
fn fill_element(el: &Element, opts: &SketchOptions) -> Option<Element> {
    let style = opts.fill?;
    if let Some(min) = opts.min_fill_area
        && let Some(b) = element_bounds(el)
        && b.width() * b.height() < min
    {
        return None;
    }
    let color = fill_color(el)?;
    let sf = SketchFill::new(color)
        .with_style(style)
        .with_angle(opts.fill_angle)
        .with_gap(floored_gap(el, opts))
        .with_fill_weight(opts.fill_weight);
    match el {
        // `GradientPath` carries a gradient instead of a `FillStrokeStyle`.
        Element::GradientPath { path, .. } => Some(Element::Path {
            path: path.clone(),
            style: FillStrokeStyle {
                fill: Some(Fill::Sketch(sf)),
                stroke: None,
            },
            closed: true,
        }),
        _ => {
            let mut out = el.clone();
            let st = style_mut(&mut out)?;
            st.fill = Some(Fill::Sketch(sf));
            st.stroke = None;
            Some(out)
        }
    }
}

/// The jittered outline, stroked with the element's own stroke (or a synthesised one).
///
/// `fill_taken` says the fill moved into its own sketch layer; otherwise the outline path
/// inherits the element's original fill (solid or gradient), so an element whose fill was *not*
/// converted stays filled.
fn outline_element(
    el: &Element,
    fill_taken: bool,
    rng: &mut Rng,
    opts: &SketchOptions,
) -> Option<Element> {
    let stroke = match stroke_of(el) {
        Some(s) => Some(s),
        // A fill-only shape (bars, pie sectors, …) would otherwise keep crisp edges under a
        // hand-drawn fill — synthesise a thin outline in the fill colour, like a pen.
        None if opts.outline_when_unstroked => {
            Some(Stroke::new(fill_color(el)?, opts.outline_width))
        }
        None => None,
    }?;
    let path = outline_path(el, rng, opts)?;
    Some(Element::Path {
        path,
        style: FillStrokeStyle {
            fill: if fill_taken { None } else { original_fill(el) },
            stroke: Some(stroke),
        },
        closed: false,
    })
}

/// The element's current fill, for carrying over onto the outline path.
fn original_fill(el: &Element) -> Option<Fill> {
    match el {
        Element::GradientPath { gradient, .. } => Some(Fill::LinearGradient(gradient.clone())),
        _ => style_of(el)?.fill.clone(),
    }
}

fn outline_path(el: &Element, rng: &mut Rng, opts: &SketchOptions) -> Option<BezPath> {
    let mut p = BezPath::new();
    match el {
        Element::Rect { rect, .. } => {
            rough::polyline(rng, &mut p, &rough::rect_points(*rect), true, opts)
        }
        Element::RoundedRect { rect, radius, .. } => {
            let pts = rough::rounded_rect_points(*rect, *radius, opts.curve_step);
            rough::polyline(rng, &mut p, &pts, true, opts);
        }
        Element::Circle { center, radius, .. } => rough::ellipse(
            rng,
            &mut p,
            *center,
            Vec2::new(radius.abs(), radius.abs()),
            0.0,
            opts,
        ),
        Element::Ellipse {
            center,
            radii,
            rotation,
            ..
        } => rough::ellipse(rng, &mut p, *center, *radii, *rotation, opts),
        Element::Line { start, end, .. } => {
            rough::polyline(rng, &mut p, &[*start, *end], false, opts)
        }
        Element::Polyline { points, .. } => rough::polyline(rng, &mut p, points, false, opts),
        Element::Polygon { points, .. } => rough::polyline(rng, &mut p, points, true, opts),
        Element::Arc {
            center,
            radii,
            start_angle,
            sweep_angle,
            ..
        } => rough::arc(
            rng,
            &mut p,
            *center,
            *radii,
            *start_angle,
            *sweep_angle,
            opts,
        ),
        Element::Pie {
            center,
            radius,
            start_angle,
            sweep_angle,
            ..
        } => {
            let mut pts = vec![*center];
            pts.extend(rough::sample_arc(
                *center,
                Vec2::new(radius.abs(), radius.abs()),
                0.0,
                *start_angle,
                *sweep_angle,
                opts.curve_step,
            ));
            rough::polyline(rng, &mut p, &pts, true, opts);
        }
        Element::Path { path, closed, .. } => rough::path(rng, &mut p, path, *closed, opts),
        Element::GradientPath { path, .. } => rough::path(rng, &mut p, path, true, opts),
        // Text / Image / Group are never sketched (see module docs).
        _ => return None,
    }
    (!p.elements().is_empty()).then_some(p)
}

/// `&FillStrokeStyle` of every variant that owns one.
fn style_of(el: &Element) -> Option<&FillStrokeStyle> {
    match el {
        Element::Rect { style, .. }
        | Element::Circle { style, .. }
        | Element::Ellipse { style, .. }
        | Element::RoundedRect { style, .. }
        | Element::Polygon { style, .. }
        | Element::Pie { style, .. }
        | Element::Path { style, .. } => Some(style),
        _ => None,
    }
}

fn style_mut(el: &mut Element) -> Option<&mut FillStrokeStyle> {
    match el {
        Element::Rect { style, .. }
        | Element::Circle { style, .. }
        | Element::Ellipse { style, .. }
        | Element::RoundedRect { style, .. }
        | Element::Polygon { style, .. }
        | Element::Pie { style, .. }
        | Element::Path { style, .. } => Some(style),
        _ => None,
    }
}

/// The stroke an element is drawn with (stroke-only variants carry one directly).
fn stroke_of(el: &Element) -> Option<Stroke> {
    match el {
        Element::Line { style, .. }
        | Element::Polyline { style, .. }
        | Element::Arc { style, .. } => Some(style.clone()),
        Element::GradientPath { stroke, .. } => stroke.clone(),
        _ => style_of(el)?.stroke.clone(),
    }
}

/// The dominant colour of an element's fill; `None` when there is no fill (or it is already a
/// sketch fill, which must not be re-sketched).
fn fill_color(el: &Element) -> Option<Color> {
    match el {
        Element::GradientPath { gradient, .. } => last_stop(&gradient.stops),
        _ => match style_of(el)?.fill.as_ref()? {
            Fill::Solid(c) => Some(*c),
            Fill::LinearGradient(g) => last_stop(&g.stops),
            Fill::RadialGradient(g) => last_stop(&g.stops),
            Fill::Sketch(_) => None,
        },
    }
}

/// Floor the hachure gap so a single element can never ask for more strokes than
/// [`SketchOptions::max_fill_strokes`] — regardless of how large the shape is.
///
/// The protection is baked into the IR here (rather than left to the backends, or to every
/// caller remembering to switch hachure off for a heat map): the element knows its own size, and
/// the resulting `SketchFill.gap` is what both backends then honour.
fn floored_gap(el: &Element, opts: &SketchOptions) -> f64 {
    let gap = if opts.fill_gap.is_finite() && opts.fill_gap > 0.05 {
        opts.fill_gap
    } else {
        4.0
    };
    let Some(bbox) = element_bounds(el) else {
        return gap;
    };
    let t = opts.fill_angle.to_radians();
    let along = (bbox.width() * t.sin()).abs() + (bbox.height() * t.cos()).abs();
    // Cross-hatch adds an orthogonal family, so size for the worst case.
    let across = (bbox.width() * t.cos()).abs() + (bbox.height() * t.sin()).abs();
    gap.max(along.max(across) / opts.max_fill_strokes.max(1) as f64)
}

/// Last stop of a gradient (highest offset), used when a gradient degrades to a sketch fill.
fn last_stop(stops: &[crate::scene::GradientStop]) -> Option<Color> {
    stops
        .iter()
        .max_by(|a, b| {
            a.offset
                .partial_cmp(&b.offset)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|s| s.color)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::{Point, Rect, Vec2};
    use crate::scene::{Clip, SketchFillStyle};
    use crate::sketch::SketchKinds;

    fn colored_rect() -> Element {
        Element::rect(
            Rect::new(0.0, 0.0, 100.0, 50.0),
            FillStrokeStyle {
                fill: Some(Fill::Solid(Color::rgb(0x33, 0x66, 0x99))),
                stroke: Some(Stroke::new(Color::BLACK, 2.0)),
            },
        )
    }

    fn scene_with(nodes: Vec<SceneNode>) -> Scene {
        let mut s = Scene::new(400.0, 300.0);
        s.nodes = nodes;
        s
    }

    #[test]
    fn filled_and_stroked_shape_becomes_fill_plus_outline() {
        let els = roughen_element(
            &colored_rect(),
            &mut Rng::new(1),
            &SketchOptions {
                fill: Some(SketchFillStyle::Hachure),
                ..Default::default()
            },
        );
        assert_eq!(els.len(), 2, "填充 + 描边应拆成两个图元");
        match &els[0] {
            Element::Rect { style, .. } => {
                assert!(matches!(style.fill, Some(Fill::Sketch(_))));
                assert!(style.stroke.is_none(), "填充层不应再描边");
            }
            other => panic!("期望保留原几何做填充, got {other:?}"),
        }
        match &els[1] {
            Element::Path { style, closed, .. } => {
                assert!(style.fill.is_none());
                assert!(style.stroke.is_some());
                assert!(!closed, "描边是开放路径");
            }
            other => panic!("期望抖动后的描边路径, got {other:?}"),
        }
    }

    #[test]
    fn fill_only_shape_gets_a_synthesised_outline() {
        let el = Element::rect(
            Rect::new(0.0, 0.0, 10.0, 10.0),
            FillStrokeStyle::fill(Color::rgb(0xff, 0x00, 0x00)),
        );
        let opts = SketchOptions {
            fill: Some(SketchFillStyle::Hachure),
            ..Default::default()
        };
        let els = roughen_element(&el, &mut Rng::new(2), &opts);
        assert_eq!(els.len(), 2);
        if let Element::Path { style, .. } = &els[1] {
            assert_eq!(
                style.stroke.as_ref().unwrap().color,
                Color::rgb(0xff, 0x00, 0x00),
                "合成描边应取填充色"
            );
        } else {
            panic!("期望合成描边");
        }
        // 关闭合成后只保留填充层。
        let els = roughen_element(
            &el,
            &mut Rng::new(2),
            &SketchOptions {
                outline_when_unstroked: false,
                ..opts
            },
        );
        assert_eq!(els.len(), 1);
    }

    /// `opts.fill = None`：只抖描边，但原填充必须被描边层继承（不能凭空消失）。
    #[test]
    fn fill_is_untouched_when_no_style_is_requested() {
        let els = roughen_element(&colored_rect(), &mut Rng::new(3), &SketchOptions::default());
        assert_eq!(els.len(), 1, "未开启手绘填充时只输出描边");
        match &els[0] {
            Element::Path { style, .. } => {
                assert_eq!(
                    style.fill,
                    Some(Fill::Solid(Color::rgb(0x33, 0x66, 0x99))),
                    "原填充应保留在描边路径上"
                );
                assert!(style.stroke.is_some());
            }
            other => panic!("期望抖动后的 Path, got {other:?}"),
        }
    }

    /// 小面积填充保护：低于 `min_fill_area` 的填充保持实底（继承到描边层），不排线。
    #[test]
    fn small_fills_keep_their_solid_ink() {
        let opts = SketchOptions {
            fill: Some(SketchFillStyle::Hachure),
            min_fill_area: Some(1000.0),
            ..Default::default()
        };
        // 箭头级别的小三角形（~60px²）→ 实底。
        let tiny = Element::polygon(
            vec![
                Point::new(0.0, 0.0),
                Point::new(10.0, 5.0),
                Point::new(0.0, 10.0),
            ],
            FillStrokeStyle::fill(Color::rgb(0x33, 0x33, 0x33)),
        );
        let els = roughen_element(&tiny, &mut Rng::new(6), &opts);
        assert_eq!(els.len(), 1, "小填充不拆层");
        if let Element::Path { style, .. } = &els[0] {
            assert_eq!(
                style.fill,
                Some(Fill::Solid(Color::rgb(0x33, 0x33, 0x33))),
                "实底应被继承"
            );
        } else {
            panic!("期望带实底的抖动路径");
        }
        // 大矩形（10_000px²）→ 仍拆成排线填充层 + 描边层。
        let big = Element::rect(
            Rect::new(0.0, 0.0, 100.0, 100.0),
            FillStrokeStyle::fill(Color::rgb(0x33, 0x66, 0x99)),
        );
        let els = roughen_element(&big, &mut Rng::new(6), &opts);
        assert_eq!(els.len(), 2, "大填充仍应拆层");
        assert!(matches!(els[0], Element::Rect { .. }));
    }

    /// `min_fill_area = Some(0.0)` 显式关闭保护（一切填充都排线）。
    #[test]
    fn zero_min_fill_area_disables_the_guard() {
        let opts = SketchOptions {
            fill: Some(SketchFillStyle::Hachure),
            min_fill_area: Some(0.0),
            ..Default::default()
        };
        let tiny = Element::polygon(
            vec![
                Point::new(0.0, 0.0),
                Point::new(10.0, 5.0),
                Point::new(0.0, 10.0),
            ],
            FillStrokeStyle::fill(Color::rgb(0x33, 0x33, 0x33)),
        );
        let els = roughen_element(&tiny, &mut Rng::new(6), &opts);
        assert_eq!(els.len(), 2, "关闭保护后小填充也拆层");
    }

    #[test]
    fn text_and_image_are_never_sketched() {
        let opts = SketchOptions {
            fill: Some(SketchFillStyle::Hachure),
            ..Default::default()
        };
        let text = Element::text(
            "hi",
            Point::new(0.0, 0.0),
            crate::text::TextStyle::new(Color::BLACK, 12.0, "sans-serif"),
        );
        assert!(roughen_element(&text, &mut Rng::new(4), &opts).is_empty());
    }

    /// 选择性：kinds 决定哪些图元参与。
    #[test]
    fn kinds_select_which_elements_are_sketched() {
        let mut scene = scene_with(vec![
            SceneNode::new(Element::rect(
                Rect::new(0.0, 0.0, 10.0, 10.0),
                FillStrokeStyle::stroke(Color::BLACK, 1.0),
            )),
            SceneNode::new(Element::circle(
                Point::new(20.0, 20.0),
                5.0,
                FillStrokeStyle::stroke(Color::BLACK, 1.0),
            )),
        ]);
        roughen_scene(
            &mut scene,
            &SketchOptions {
                seed: Some(1),
                kinds: SketchKinds::only(&[SketchKind::Circle]),
                ..Default::default()
            },
        );
        assert!(
            matches!(scene.nodes[0].element, Element::Rect { .. }),
            "矩形被排除"
        );
        assert!(
            matches!(scene.nodes[1].element, Element::Path { .. }),
            "圆被手绘化"
        );
    }

    /// 选择性：谓词为 false 时整棵子树跳过。
    #[test]
    fn predicate_skips_whole_subtrees() {
        let child = SceneNode::new(Element::rect(
            Rect::new(0.0, 0.0, 10.0, 10.0),
            FillStrokeStyle::stroke(Color::BLACK, 1.0),
        ))
        .with_name("keep");
        let mut scene = scene_with(vec![SceneNode::group(vec![
            child,
            SceneNode::new(Element::rect(
                Rect::new(0.0, 0.0, 10.0, 10.0),
                FillStrokeStyle::stroke(Color::BLACK, 1.0),
            ))
            .with_name("drop"),
        ])]);
        roughen_scene_if(
            &mut scene,
            &SketchOptions {
                seed: Some(1),
                ..Default::default()
            },
            &|n| n.name.as_deref() != Some("drop"),
        );
        let Element::Group { children } = &scene.nodes[0].element else {
            panic!("期望 Group");
        };
        assert!(
            matches!(children[0].element, Element::Path { .. }),
            "keep 应被手绘化"
        );
        assert!(
            matches!(children[1].element, Element::Rect { .. }),
            "drop 应被跳过"
        );
    }

    #[test]
    fn hidden_nodes_are_left_alone() {
        let mut scene = scene_with(vec![
            SceneNode::new(Element::rect(
                Rect::new(0.0, 0.0, 10.0, 10.0),
                FillStrokeStyle::stroke(Color::BLACK, 1.0),
            ))
            .with_visible(false),
        ]);
        roughen_scene(
            &mut scene,
            &SketchOptions {
                seed: Some(1),
                ..Default::default()
            },
        );
        assert!(matches!(scene.nodes[0].element, Element::Rect { .. }));
    }

    #[test]
    fn layers_are_visited_too() {
        let mut scene = scene_with(vec![]);
        let mut layer = crate::scene::Layer::new("grid");
        layer.push(Element::rect(
            Rect::new(0.0, 0.0, 10.0, 10.0),
            FillStrokeStyle::stroke(Color::BLACK, 1.0),
        ));
        scene.push_layer(layer);
        roughen_scene(
            &mut scene,
            &SketchOptions {
                seed: Some(1),
                ..Default::default()
            },
        );
        assert!(matches!(
            scene.layers[0].nodes[0].element,
            Element::Path { .. }
        ));
    }

    /// 节点级属性（transform / clip / opacity / name）必须留在原位，不被手绘化吞掉。
    #[test]
    fn node_attributes_survive_the_pass() {
        let mut scene = scene_with(vec![
            SceneNode::new(Element::rect(
                Rect::new(0.0, 0.0, 10.0, 10.0),
                FillStrokeStyle {
                    fill: Some(Fill::Solid(Color::BLACK)),
                    stroke: Some(Stroke::new(Color::BLACK, 1.0)),
                },
            ))
            .with_transform(crate::geometry::Transform::translate(5.0, 5.0))
            .with_opacity(0.5)
            .with_name("node")
            .with_clip(Clip::rect(Rect::new(0.0, 0.0, 40.0, 40.0))),
        ]);
        roughen_scene(
            &mut scene,
            &SketchOptions {
                seed: Some(1),
                fill: Some(SketchFillStyle::Hachure),
                ..Default::default()
            },
        );
        let n = &scene.nodes[0];
        assert!(n.transform.is_some() && n.clip.is_some());
        assert_eq!(n.opacity, 0.5);
        assert_eq!(n.name.as_deref(), Some("node"));
        assert!(matches!(n.element, Element::Group { .. }));
    }

    /// 端到端确定性：同种子两次处理同场景 → 完全相同的路径。
    #[test]
    fn whole_scene_is_deterministic() {
        let build = || {
            scene_with(vec![
                SceneNode::new(Element::rect(
                    Rect::new(10.0, 10.0, 110.0, 60.0),
                    FillStrokeStyle {
                        fill: Some(Fill::Solid(Color::rgb(0xee, 0xee, 0xff))),
                        stroke: Some(Stroke::new(Color::BLACK, 1.5)),
                    },
                )),
                SceneNode::new(Element::circle(
                    Point::new(200.0, 40.0),
                    18.0,
                    FillStrokeStyle::stroke(Color::BLACK, 1.0),
                )),
            ])
        };
        let opts = SketchOptions {
            seed: Some(2024),
            fill: Some(SketchFillStyle::Hachure),
            ..Default::default()
        };
        let mut a = build();
        let mut b = build();
        roughen_scene(&mut a, &opts);
        roughen_scene(&mut b, &opts);
        assert_eq!(format!("{:?}", a.nodes), format!("{:?}", b.nodes));
    }

    #[test]
    fn different_seeds_produce_different_scenes() {
        let build = || {
            scene_with(vec![SceneNode::new(Element::rect(
                Rect::new(0.0, 0.0, 100.0, 50.0),
                FillStrokeStyle::stroke(Color::BLACK, 1.0),
            ))])
        };
        let mut a = build();
        let mut b = build();
        roughen_scene(
            &mut a,
            &SketchOptions {
                seed: Some(1),
                ..Default::default()
            },
        );
        roughen_scene(
            &mut b,
            &SketchOptions {
                seed: Some(2),
                ..Default::default()
            },
        );
        assert_ne!(format!("{:?}", a.nodes), format!("{:?}", b.nodes));
    }

    /// 抖动后的包围盒只会略微变大，不会跑飞。
    #[test]
    fn roughened_bounds_stay_close_to_the_original() {
        let mut scene = scene_with(vec![SceneNode::new(Element::rect(
            Rect::new(10.0, 10.0, 110.0, 60.0),
            FillStrokeStyle::stroke(Color::BLACK, 2.0),
        ))]);
        let before = crate::fit::scene_bounds(&scene).unwrap();
        roughen_scene(
            &mut scene,
            &SketchOptions {
                seed: Some(7),
                roughness: 2.0,
                ..Default::default()
            },
        );
        let after = crate::fit::scene_bounds(&scene).unwrap();
        assert!(after.min_x() < before.min_x() && after.max_x() > before.max_x());
        assert!(
            after.width() - before.width() < 12.0,
            "抖动外扩应有界, got {}",
            after.width() - before.width()
        );
    }

    /// roughen_scene_fit：抖动溢出后被重新贴合，且二次调用是幂等的（尺寸不再变化）。
    #[test]
    fn refit_is_idempotent() {
        let mut scene = scene_with(vec![SceneNode::new(Element::rect(
            Rect::new(0.0, 0.0, 100.0, 50.0),
            FillStrokeStyle {
                fill: Some(Fill::Solid(Color::BLACK)),
                stroke: Some(Stroke::new(Color::BLACK, 3.0)),
            },
        ))]);
        let opts = SketchOptions {
            seed: Some(11),
            roughness: 2.5,
            ..Default::default()
        };
        roughen_scene_fit(&mut scene, &opts, 8.0);
        let (w, h) = (scene.width, scene.height);
        let b1 = crate::fit::scene_bounds(&scene).unwrap();
        // 再 fit 一次：内容已贴合 → 只有极小的平移，画布尺寸不放大。
        crate::fit::fit_scene(&mut scene, FitOptions::new().with_margin(8.0));
        assert!((scene.width - w).abs() < 1.0 && (scene.height - h).abs() < 1.0);
        let b2 = crate::fit::scene_bounds(&scene).unwrap();
        assert!((b1.width() - b2.width()).abs() < 1.0);
    }

    #[test]
    fn gradient_degrades_to_its_last_stop() {
        let g = crate::scene::LinearGradient {
            start: Point::new(0.0, 0.0),
            end: Point::new(10.0, 0.0),
            stops: vec![
                crate::scene::GradientStop {
                    offset: 0.0,
                    color: Color::WHITE,
                },
                crate::scene::GradientStop {
                    offset: 1.0,
                    color: Color::rgb(0x12, 0x34, 0x56),
                },
            ],
        };
        let el = Element::GradientPath {
            path: BezPath::new(),
            gradient: g,
            stroke: None,
        };
        let els = roughen_element(
            &el,
            &mut Rng::new(1),
            &SketchOptions {
                fill: Some(SketchFillStyle::Hachure),
                ..Default::default()
            },
        );
        match &els[0] {
            Element::Path { style, .. } => match &style.fill {
                Some(Fill::Sketch(sf)) => {
                    assert_eq!(sf.color, Color::rgb(0x12, 0x34, 0x56), "取末端色");
                }
                other => panic!("期望 Sketch 填充, got {other:?}"),
            },
            other => panic!("期望 Path, got {other:?}"),
        }
    }

    /// 已经手绘化的填充不应被二次处理（重入保护）。
    #[test]
    fn already_sketched_fill_is_not_reprocessed() {
        let sf = SketchFill::new(Color::BLACK);
        let el = Element::rect(
            Rect::new(0.0, 0.0, 10.0, 10.0),
            FillStrokeStyle {
                fill: Some(Fill::Sketch(sf)),
                stroke: Some(Stroke::new(Color::BLACK, 1.0)),
            },
        );
        let els = roughen_element(
            &el,
            &mut Rng::new(1),
            &SketchOptions {
                fill: Some(SketchFillStyle::Hachure),
                ..Default::default()
            },
        );
        assert_eq!(els.len(), 1, "只有描边被重画，填充保持原样");
        assert!(matches!(els[0], Element::Path { .. }));
    }

    #[test]
    fn ellipse_and_pie_and_arc_are_all_sketchable() {
        let opts = SketchOptions {
            seed: Some(5),
            ..Default::default()
        };
        for el in [
            Element::ellipse(
                Point::new(10.0, 10.0),
                Vec2::new(20.0, 10.0),
                0.3,
                FillStrokeStyle::stroke(Color::BLACK, 1.0),
            ),
            Element::pie(
                Point::new(10.0, 10.0),
                20.0,
                0.0,
                1.0,
                FillStrokeStyle::stroke(Color::BLACK, 1.0),
            ),
            Element::arc(
                Point::new(10.0, 10.0),
                Vec2::new(20.0, 20.0),
                0.0,
                1.0,
                Stroke::new(Color::BLACK, 1.0),
            ),
            Element::poly(
                vec![
                    Point::new(0.0, 0.0),
                    Point::new(10.0, 5.0),
                    Point::new(4.0, 12.0),
                ],
                Stroke::new(Color::BLACK, 1.0),
            ),
        ] {
            let els = roughen_element(&el, &mut Rng::new(5), &opts);
            assert_eq!(els.len(), 1, "{el:?} 应被手绘化");
            assert!(matches!(els[0], Element::Path { .. }), "{el:?}");
        }
    }
}
