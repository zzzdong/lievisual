use super::*;
use crate::geometry::Color;
use std::sync::Arc;

/// Convenience: measure a plain string (the single-span case).
fn measure_one(text: &str, style: &TextStyle, max_width: Option<f64>) -> TextMeasure {
    measure_text(
        std::slice::from_ref(&RichSpan::new(text, style.clone())),
        max_width,
    )
}

/// FontWeight keyword / numeric parsing.
mod font_weight_parse {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn parses_keywords() {
        assert_eq!(FontWeight::parse("bold"), Some(FontWeight::Bold));
        assert_eq!(FontWeight::parse("normal"), Some(FontWeight::Normal));
        assert_eq!(FontWeight::parse("Thin"), Some(FontWeight::Thin));
        assert_eq!(FontWeight::parse("EXTRA-BOLD"), Some(FontWeight::ExtraBold));
        assert_eq!(FontWeight::parse("semi_bold"), Some(FontWeight::SemiBold));
        assert_eq!(FontWeight::parse("black"), Some(FontWeight::Black));
    }

    #[test]
    fn parses_numeric() {
        assert_eq!(FontWeight::parse("100"), Some(FontWeight::Thin));
        assert_eq!(FontWeight::parse("400"), Some(FontWeight::Normal));
        assert_eq!(FontWeight::parse("700"), Some(FontWeight::Bold));
        assert_eq!(FontWeight::parse("750"), Some(FontWeight::ExtraBold));
        assert_eq!(FontWeight::parse(" 900 "), Some(FontWeight::Black));
    }

    #[test]
    fn rejects_invalid() {
        assert_eq!(FontWeight::parse("xxx"), None);
        assert_eq!(FontWeight::parse(""), None);
        assert_eq!(FontWeight::parse("1000"), None);
        assert!(FontWeight::from_str("nope").is_err());
        assert_eq!(FontWeight::from_str("bold").unwrap(), FontWeight::Bold);
    }
}

/// Convenience: lay out a plain string.
fn layout_one(text: &str, style: &TextStyle, max_width: Option<f64>) -> Arc<TextLayout> {
    layout_text(
        std::slice::from_ref(&RichSpan::new(text, style.clone())),
        max_width,
    )
}

/// Measurement and metrics: size, baseline offset, and TLS-context reuse.
mod measure {
    use super::*;

    #[test]
    fn measure_metrics_basic() {
        let style = TextStyle::new(Color::BLACK, 20.0, "sans-serif");
        let m = measure_one("Hi", &style, None);
        assert!(m.metrics.width > 0.0);
        assert!(m.metrics.height > 0.0);
        assert!(m.metrics.font_bounding_box_ascent > 0.0);
        assert!(m.metrics.font_bounding_box_descent >= 0.0);
        assert!(m.metrics.alphabetic_baseline > 0.0);
        assert_eq!(m.size.width, m.metrics.width);
        assert_eq!(m.size.height, m.metrics.height);
    }

    #[test]
    fn baseline_anchor_offsets_monotonic() {
        let style = TextStyle::new(Color::BLACK, 20.0, "sans-serif");
        let m = measure_one("X", &style, None).metrics;
        let top = TextBaseline::Top.anchor_offset(&m);
        let alpha = TextBaseline::Alphabetic.anchor_offset(&m);
        let bottom = TextBaseline::Bottom.anchor_offset(&m);
        assert_eq!(top, 0.0);
        assert!(top < alpha && alpha < bottom);
        assert!(bottom <= m.height + 1e-6);
    }

    #[test]
    fn measure_text_reuses_thread_local_context() {
        // Multiple measurements on the same thread go through the thread-local cache:
        // no panic and stable results.
        let style = TextStyle::new(Color::BLACK, 16.0, "sans-serif");
        let a = measure_one("缓存复用", &style, None).metrics.width;
        let b = measure_one("缓存复用", &style, None).metrics.width;
        assert!((a - b).abs() < 1e-6);

        // Independent threads each hold their own context, without interference.
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let style = style.clone();
                std::thread::spawn(move || {
                    let m = measure_one("thread", &style, None);
                    assert!(m.metrics.width > 0.0);
                })
            })
            .collect();
        for h in handles {
            h.join().expect("thread panicked");
        }
    }

    #[test]
    fn layout_text_uses_same_tls_context() {
        // The renderer's layout entry point and measurement share the same TLS context, so
        // results must agree.
        let style = TextStyle::new(Color::BLACK, 16.0, "sans-serif");
        let m = measure_one("共享", &style, None);
        let l = layout_one("共享", &style, None);
        assert_eq!(l.width, m.metrics.width);
        assert_eq!(l.height, m.metrics.height);
    }
}

/// Alignment offsets: horizontal alignment and vertical baseline anchors.
mod offset {
    use super::*;

    #[test]
    fn compute_text_offset_horizontal_alignment() {
        let style = TextStyle::new(Color::BLACK, 20.0, "sans-serif");
        let layout = layout_one("Hello", &style, None);
        let (x_left, _) = compute_text_offset(&layout, TextAlign::Left, TextBaseline::Top);
        let (x_center, _) = compute_text_offset(&layout, TextAlign::Center, TextBaseline::Top);
        let (x_right, _) = compute_text_offset(&layout, TextAlign::Right, TextBaseline::Top);
        assert_eq!(x_left, 0.0);
        // Center / right align shift symmetrically by the layout width.
        assert!((x_center + layout.width / 2.0).abs() < 1e-6);
        assert!((x_right + layout.width).abs() < 1e-6);
    }

    #[test]
    fn compute_text_offset_baselines_monotonic() {
        let style = TextStyle::new(Color::BLACK, 20.0, "sans-serif");
        let layout = layout_one("Yg", &style, None);
        // Vertical anchors from top to bottom: Top > Middle > Hanging > Alphabetic >
        // Ideographic > Bottom. In y-down, lower anchors give more negative offsets, so
        // the sequence is strictly decreasing.
        let top = compute_text_offset(&layout, TextAlign::Left, TextBaseline::Top).1;
        let middle = compute_text_offset(&layout, TextAlign::Left, TextBaseline::Middle).1;
        let hanging = compute_text_offset(&layout, TextAlign::Left, TextBaseline::Hanging).1;
        let alpha = compute_text_offset(&layout, TextAlign::Left, TextBaseline::Alphabetic).1;
        let ideo = compute_text_offset(&layout, TextAlign::Left, TextBaseline::Ideographic).1;
        let bottom = compute_text_offset(&layout, TextAlign::Left, TextBaseline::Bottom).1;
        assert_eq!(top, 0.0);
        assert!(top > middle, "top({top}) > middle({middle})");
        assert!(middle > hanging, "middle({middle}) > hanging({hanging})");
        assert!(hanging > alpha, "hanging({hanging}) > alpha({alpha})");
        assert!(alpha > ideo, "alpha({alpha}) > ideo({ideo})");
        assert!(ideo > bottom, "ideo({ideo}) > bottom({bottom})");
    }

    #[test]
    fn justify_offsets_like_left_and_layouts_multiline() {
        // Anchor offset: Justify equals Left (0), because justification is done by parley
        // during layout.
        let style = TextStyle::new(Color::BLACK, 20.0, "sans-serif");
        let layout = layout_one("Hello", &style, None);
        let (xj, _) = compute_text_offset(&layout, TextAlign::Justify, TextBaseline::Top);
        let (xl, _) = compute_text_offset(&layout, TextAlign::Left, TextBaseline::Top);
        assert_eq!(xj, xl);

        // Multi-line Justify layout must not panic and must produce multiple lines (visual
        // justification is guaranteed by parley).
        let jstyle =
            TextStyle::new(Color::BLACK, 16.0, "sans-serif").with_align(TextAlign::Justify);
        let text = "The quick brown fox jumps over the lazy dog while padding enough to wrap.";
        let jl = layout_text(
            std::slice::from_ref(&RichSpan::new(text, jstyle.clone())),
            Some(160.0),
        );
        let lines: Vec<_> = jl.lines.iter().collect();
        assert!(lines.len() >= 2, "should produce multiple lines");
        let single = layout_text(std::slice::from_ref(&RichSpan::new("a", jstyle)), None);
        assert!(jl.height > single.height);
    }
}

/// `font-family` CSS normalization.
mod font_css {
    use super::*;

    #[test]
    fn font_family_css_normalizes_list() {
        let style = TextStyle::new(Color::BLACK, 12.0, "Helvetica Neue, Arial, sans-serif");
        assert_eq!(
            style.font_family_css(),
            "'Helvetica Neue', Arial, sans-serif"
        );
    }

    #[test]
    fn font_family_css_generic_stays_unquoted() {
        for g in [
            "sans-serif",
            "serif",
            "monospace",
            "cursive",
            "system-ui",
            "ui-monospace",
        ] {
            let style = TextStyle::new(Color::BLACK, 12.0, g);
            assert_eq!(
                style.font_family_css(),
                g,
                "generic family {g} should stay unquoted"
            );
        }
    }

    #[test]
    fn font_family_css_quotes_spaces_and_non_ascii() {
        // Contains spaces → single quotes
        let s = TextStyle::new(Color::BLACK, 12.0, "Times New Roman");
        assert_eq!(s.font_family_css(), "'Times New Roman'");
        // Non-ASCII → single quotes
        let s = TextStyle::new(Color::BLACK, 12.0, "微软雅黑");
        assert_eq!(s.font_family_css(), "'微软雅黑'");
        // Single quote → CSS string escaping
        let s = TextStyle::new(Color::BLACK, 12.0, "Rock'n'Roll");
        assert_eq!(s.font_family_css(), "'Rock\\'n\\'Roll'");
        // Backslash → escaping
        let s = TextStyle::new(Color::BLACK, 12.0, r"a\b");
        assert_eq!(s.font_family_css(), r"'a\\b'");
    }

    #[test]
    fn font_family_css_strips_user_quotes_and_keywords() {
        // User-supplied quotes → stripped then rebuilt per the rules
        let s = TextStyle::new(Color::BLACK, 12.0, "\"Helvetica Neue\"");
        assert_eq!(s.font_family_css(), "'Helvetica Neue'");
        // Global keyword → quoted to avoid being read as a keyword
        let s = TextStyle::new(Color::BLACK, 12.0, "inherit");
        assert_eq!(s.font_family_css(), "'inherit'");
        // Idempotent: already-normalized input is unchanged
        let s = TextStyle::new(Color::BLACK, 12.0, "'Helvetica Neue', Arial, sans-serif");
        assert_eq!(s.font_family_css(), "'Helvetica Neue', Arial, sans-serif");
        // Empty items are filtered out
        let s = TextStyle::new(Color::BLACK, 12.0, "Arial, , serif");
        assert_eq!(s.font_family_css(), "Arial, serif");
    }
}

/// Rich-text layout / measurement, plus the caller-side measurement pattern.
mod rich_text {
    use super::*;

    #[test]
    fn rich_text_measures_and_layouts() {
        let spans = vec![
            RichSpan::new("Hello", TextStyle::new(Color::BLACK, 20.0, "sans-serif")),
            RichSpan::new(" World", TextStyle::new(Color::BLACK, 20.0, "sans-serif")),
        ];
        let m = measure_text(&spans, None);
        assert!(m.metrics.width > 0.0);
        assert!(m.metrics.height > 0.0);
        assert_eq!(m.size.width, m.metrics.width);
        // Consistent with measuring a single span (same TLS context).
        let plain = measure_one("Hello World", &spans[0].style, None);
        assert!((m.metrics.width - plain.metrics.width).abs() < 1e-6);
    }

    #[test]
    fn rich_text_mixed_styles_apply_ranges() {
        // The larger font size should make the overall line height taller.
        let spans = vec![
            RichSpan::new("big", TextStyle::new(Color::BLACK, 40.0, "sans-serif")),
            RichSpan::new("small", TextStyle::new(Color::BLACK, 12.0, "sans-serif")),
        ];
        let m = measure_text(&spans, None);
        assert!(m.metrics.width > 0.0);
        // The 40px span dominates the line height (first-line metrics come from the whole
        // mixed row's font box).
        assert!(m.metrics.font_bounding_box_ascent > 20.0);
        assert!(m.metrics.font_bounding_box_descent >= 0.0);
        // 12px-only control: the larger span should raise the overall line height.
        let small_only = measure_text(
            std::slice::from_ref(&RichSpan::new("small", spans[1].style.clone())),
            None,
        );
        assert!(
            m.metrics.height > small_only.metrics.height,
            "mixed larger font should be taller: {} vs {}",
            m.metrics.height,
            small_only.metrics.height
        );
    }

    #[test]
    fn rich_text_empty_and_newline() {
        // Empty list: returns an empty layout without panicking.
        let empty = measure_text(&[], None);
        assert!(empty.metrics.height >= 0.0);
        // A newline yields a multi-line layout that is taller than a single line.
        let single = measure_text(
            &[RichSpan::new(
                "ab",
                TextStyle::new(Color::BLACK, 16.0, "sans-serif"),
            )],
            None,
        );
        let multi = measure_text(
            &[RichSpan::new(
                "a\nb",
                TextStyle::new(Color::BLACK, 16.0, "sans-serif"),
            )],
            None,
        );
        assert!(
            multi.metrics.height > single.metrics.height,
            "rich text with newline should be taller: {} vs {}",
            multi.metrics.height,
            single.metrics.height
        );
    }

    #[test]
    fn rich_text_max_width_wraps() {
        let span = RichSpan::new(
            "aaaaaaaa aaaaaaaa",
            TextStyle::new(Color::BLACK, 16.0, "sans-serif"),
        );
        let narrow = measure_text(std::slice::from_ref(&span), Some(50.0));
        let wide = measure_text(std::slice::from_ref(&span), None);
        // A narrow width wraps, so the height grows (words break into multiple lines at 50px).
        assert!(
            narrow.metrics.height > wide.metrics.height,
            "max_width should wrap and grow height: {} vs {}",
            narrow.metrics.height,
            wide.metrics.height
        );
    }

    /// Simulate the caller-side measurement pattern (e.g. liemermaid's
    /// builder/layout/measure.rs): `layout_text(spans, None)` → read
    /// `layout.width()/height()` → position via `compute_text_offset`, and finally feed
    /// the pre-computed layout to `Element::Text`.
    ///
    /// This guarantees the APIs keep consistent signatures and unified semantics, and
    /// that the `Arc<TextLayout>` returned by rich-text measurement is directly reusable.
    #[test]
    fn caller_pattern_create_layout_then_offset() {
        use crate::scene::Element;

        // A centered + vertically-centered node label, measured via layout_text.
        let style = TextStyle::new(Color::BLACK, 13.0, "sans-serif")
            .with_align(TextAlign::Center)
            .with_baseline(TextBaseline::Middle);
        let text = "Hello Node";
        let layout = layout_one(text, &style, None);
        let (x_off, y_off) = compute_text_offset(&layout, TextAlign::Center, TextBaseline::Middle);
        // Anchor (cx, cy) + offset = the text block's top-left corner.
        let cx = 100.0;
        let cy = 50.0;
        let top_left_x = cx + x_off;
        let top_left_y = cy + y_off;
        // Centered: top-left x = cx - half width (horizontal uses the line box width).
        assert!((top_left_x - (cx - layout.width / 2.0)).abs() < 1e-6);
        // Vertically centered on the *ink* (visible glyphs): top-left y = cy - (ink_min_y +
        // ink_height/2). This is what `compute_text_offset` returns for `Middle`, so the
        // text is centered by its visual height rather than its em box (which would push
        // glyphs up by descent/leading). Recompute the expected value from the layout to
        // avoid hard-coding the ink metrics.
        let ib = layout.ink_bounds();
        let ink_vh = (ib.max_y() - ib.min_y()).max(0.0);
        let expected_y = cy - (ib.min_y() + ink_vh / 2.0);
        assert!((top_left_y - expected_y).abs() < 1e-6);

        // Rich-text measurement matches the single-span layout width, and Arc<TextLayout>
        // can serve as Element::Text.layout.
        let rich = measure_text(
            std::slice::from_ref(&RichSpan::new(text, style.clone())),
            None,
        );
        assert!((rich.layout.width - layout.width).abs() < 1e-6);
        let node = Element::Text {
            spans: vec![RichSpan::new(text, style.clone())],
            position: crate::geometry::Point::new(top_left_x, top_left_y),
            style,
            layout: Some(rich.layout),
        };
        match node {
            Element::Text { layout, .. } => assert!(layout.is_some()),
            _ => unreachable!(),
        }
    }
}

/// Decoration and spacing: underline / strikethrough / letter spacing / font width.
mod decoration {
    use super::*;

    #[test]
    fn letter_spacing_widens_measure() {
        let base = TextStyle::new(Color::BLACK, 20.0, "sans-serif");
        let spaced = base.clone().with_letter_spacing(8.0);
        let a = measure_one("Hello", &base, None).metrics.width;
        let b = measure_one("Hello", &spaced, None).metrics.width;
        // Adding letter spacing should clearly increase the overall width.
        assert!(
            b > a + 1.0,
            "letter_spacing should widen: base={a}, spaced={b}"
        );
    }

    // Temporarily skipped: this test depends on "a font with a `wdth` variation axis"
    // being present on the system, which parley/fontique's `FontWidth` uses to pick a
    // narrower variant. The current test environment (840 system fonts) has no Latin
    // font with a `wdth` axis (only `Noto Sans Sinhala * Condensed`, which is Sinhala-only),
    // so fontconfig falls back to normal width and normal and condensed end up the same
    // width. The code logic is correct; re-enable once the environment provides a
    // variable font (e.g. Inter / Roboto Flex with a wdth axis) or a condensed font file
    // is explicitly injected.
    #[test]
    #[ignore = "environment lacks a font with a wdth variation axis; FontWidth cannot pick a narrower font"]
    fn condensed_font_width_is_narrower() {
        let normal = TextStyle::new(Color::BLACK, 20.0, "sans-serif");
        let condensed = normal.clone().with_font_width(FontWidth::Condensed);
        let a = measure_one("MMMM", &normal, None).metrics.width;
        let b = measure_one("MMMM", &condensed, None).metrics.width;
        // A condensed width should make the glyphs narrower.
        assert!(
            b < a,
            "condensed(0.75) should be narrower: normal={a}, condensed={b}"
        );
    }

    #[test]
    fn decorations_do_not_change_size_but_render_rule() {
        // Underline / strikethrough do not change glyph layout size, only rendering.
        let plain = TextStyle::new(Color::BLACK, 20.0, "sans-serif");
        let underlined = plain
            .clone()
            .with_underline(true)
            .with_underline_color(Some(Color::rgb(0, 0, 0xff)));
        let struck = plain.clone().with_strikethrough(true);
        let w_plain = measure_one("Hi", &plain, None).metrics.width;
        let w_ul = measure_one("Hi", &underlined, None).metrics.width;
        let w_st = measure_one("Hi", &struck, None).metrics.width;
        assert!((w_plain - w_ul).abs() < 1e-6);
        assert!((w_plain - w_st).abs() < 1e-6);
    }

    #[test]
    fn builder_defaults() {
        let s = TextStyle::new(Color::BLACK, 12.0, "sans-serif");
        assert_eq!(s.font_width, FontWidth::Normal);
        assert_eq!(s.letter_spacing, 0.0);
        assert!(!s.underline);
        assert!(!s.strikethrough);
        assert_eq!(s.underline_color, None);
        assert_eq!(s.strikethrough_color, None);
        assert_eq!(s.baseline_shift, 0.0);
        assert_eq!(s.background_color, None);
    }

    #[test]
    fn baseline_shift_and_background_builders() {
        let s = TextStyle::new(Color::BLACK, 12.0, "sans-serif")
            .with_baseline_shift(6.0)
            .with_background_color(Some(Color::rgb(0xf0, 0xf0, 0xf0)));
        assert_eq!(s.baseline_shift, 6.0);
        assert_eq!(s.background_color, Some(Color::rgb(0xf0, 0xf0, 0xf0)));
        // Negative shift = move the subscript down.
        let sub = TextStyle::new(Color::BLACK, 12.0, "sans-serif").with_baseline_shift(-4.0);
        assert_eq!(sub.baseline_shift, -4.0);
    }

    #[test]
    fn rich_text_mixed_decorations_apply_per_span() {
        // In rich text, only some spans add letter spacing → the overall width falls
        // between "all spaced" and "none spaced", proving span-level differences are
        // correctly applied per range via push_style_diff.
        let plain = TextStyle::new(Color::BLACK, 20.0, "sans-serif");
        let spaced = plain.clone().with_letter_spacing(6.0);

        let all_plain = measure_one("AB", &plain, None).metrics.width;
        let all_spaced = measure_one("AB", &spaced, None).metrics.width;
        assert!(all_spaced > all_plain, "all-spaced should be wider");

        let mixed = measure_text(
            &[
                RichSpan::new("A", plain.clone()),
                RichSpan::new("B", spaced),
            ],
            None,
        )
        .metrics
        .width;
        assert!(
            mixed > all_plain && mixed < all_spaced,
            "mixed width should be in between: all_plain={all_plain}, mixed={mixed}, all_spaced={all_spaced}"
        );
    }

    /// 回归：同一字体的一个 parley 字体级 Run 可横跨多个「仅背景色不同」的 span
    /// （背景色不是 parley 样式属性，不产生 GlyphRun 边界）。提取时必须在 span
    /// 边界处切分 piece，且 piece 的文本范围须从 cluster 区间推导——否则
    /// 「普通 + 高亮 + 普通」会整段涂上高亮背景、且 run.text 变成整段全文。
    #[test]
    fn background_only_span_does_not_leak_to_neighbours() {
        let plain = TextStyle::new(Color::BLACK, 12.0, "sans-serif");
        let marked = plain
            .clone()
            .with_background_color(Some(Color::rgb(0xff, 0xff, 0xcc)));

        let layout = layout_text(
            &[
                RichSpan::new("pre ", plain.clone()),
                RichSpan::new("marked", marked.clone()),
                RichSpan::new(" post", plain.clone()),
            ],
            None,
        );

        assert_eq!(layout.lines.len(), 1);
        let line = &layout.lines[0];

        // 同字体同色不拆 style，但提取层必须按 span 切分：至少 3 个 run。
        assert!(
            line.runs.len() >= 3,
            "应按 span 边界切分为至少 3 个 run，实际 {}",
            line.runs.len()
        );

        // 背景色只应出现在中间的 span 上，且 run.text 应为各自的切片而非全文。
        let bg_runs: Vec<_> = line
            .runs
            .iter()
            .filter(|r| r.background_color.is_some())
            .collect();
        assert_eq!(bg_runs.len(), 1, "只有中间 span 的 run 携带背景色");
        assert_eq!(bg_runs[0].text, "marked");
        assert!(
            line.runs.iter().all(|r| r.text != "pre marked post"),
            "任何 run 的 text 都不应是拼接全文（Run::text_range 误用）"
        );

        // 字形总数守恒：4+6+5 = 15。
        let total_glyphs: usize = line.runs.iter().map(|r| r.glyphs.len()).sum();
        assert_eq!(total_glyphs, 15);
    }

    /// 回归：语法高亮场景（同一 span 内多次颜色变化）下，各 run 的 text 应为
    /// 各自的字节区间切片，且 cluster 是相对 run.text 的局部偏移。
    #[test]
    fn colored_ranges_keep_per_piece_text() {
        let base = TextStyle::new(Color::BLACK, 12.0, "sans-serif");
        let mut red = base.clone();
        red.color = Color::rgb(0xff, 0, 0);
        let mut blue = base.clone();
        blue.color = Color::rgb(0, 0, 0xff);

        let layout = layout_text(
            &[
                RichSpan::new("fn ", red),
                RichSpan::new("name", base.clone()),
                RichSpan::new("() {}", blue),
            ],
            None,
        );

        let line = &layout.lines[0];
        let texts: Vec<&str> = line.runs.iter().map(|r| r.text.as_str()).collect();
        assert!(
            texts.contains(&"fn "),
            "应存在 text == \"fn \" 的 run，实际 {texts:?}"
        );
        assert!(
            texts.contains(&"name") && texts.contains(&"() {}"),
            "各 run 应为各自区间切片，实际 {texts:?}"
        );
        let total_glyphs: usize = line.runs.iter().map(|r| r.glyphs.len()).sum();
        assert_eq!(total_glyphs, 12, "字形总数应守恒");
    }

    /// `TextRun::split_at`：按 cluster 对齐的字节切点切分，字形不丢、
    /// cluster 重定基、advance 重算、baseline_x 取各 piece 首字形。
    #[test]
    fn split_at_partitions_glyphs_and_recomputes_metrics() {
        let style = TextStyle::new(Color::BLACK, 12.0, "sans-serif");
        let layout = layout_text(std::slice::from_ref(&RichSpan::new("abcdef", style)), None);
        let run = layout.lines[0].runs[0].clone();
        assert_eq!(run.glyphs.len(), 6);

        let pieces = run.split_at(&[2, 5]);
        assert_eq!(pieces.len(), 3);
        let texts: Vec<&str> = pieces.iter().map(|p| p.text.as_str()).collect();
        assert_eq!(texts, vec!["ab", "cde", "f"]);

        // 字形总数守恒，cluster 各自归零。
        let total: usize = pieces.iter().map(|p| p.glyphs.len()).sum();
        assert_eq!(total, 6);
        for p in &pieces {
            assert_eq!(p.glyphs[0].cluster, 0, "piece 首字形 cluster 应归零");
            let expect: f32 = p.glyphs.iter().map(|g| g.advance).sum();
            assert!((p.advance - expect).abs() < 1e-4, "advance 应按 piece 重算");
        }
        // baseline_x 依次取各 piece 首字形 x（供背景矩形 / 链接热区定位）。
        assert!(pieces[0].baseline_x <= pieces[1].baseline_x);
        assert!(pieces[1].baseline_x <= pieces[2].baseline_x);
        // 原 run 不受影响。
        assert_eq!(run.glyphs.len(), 6);
        assert_eq!(run.text, "abcdef");
    }

    /// `TextRun::split_at`：无效切点（越界 / 非字符边界 / 未对齐 cluster）
    /// 被忽略；无有效切点时原样返回。
    #[test]
    fn split_at_ignores_invalid_offsets() {
        let style = TextStyle::new(Color::BLACK, 12.0, "sans-serif");
        let layout = layout_text(std::slice::from_ref(&RichSpan::new("中文", style)), None);
        // CJK 走回退字体，与 ASCII 部分不在同一 run；本测试只需 runs[0]（"中文"，
        // 每字一个字形，cluster 为字节偏移 0 / 3）。
        let run = layout.lines[0].runs[0].clone();
        assert_eq!(run.text, "中文");
        assert_eq!(run.glyphs.len(), 2);

        // 切点 1 落在「中」的簇中间（非 cluster 对齐）→ 忽略；切点 3 有效；
        // 切点 100 越界 → 忽略。
        let pieces = run.split_at(&[1, 3, 100]);
        let texts: Vec<&str> = pieces.iter().map(|p| p.text.as_str()).collect();
        assert_eq!(texts, vec!["中", "文"]);

        // 无有效切点：原样返回。
        let same = run.split_at(&[0, 100]);
        assert_eq!(same.len(), 1);
        assert_eq!(same[0].text, run.text);
        assert_eq!(same[0].glyphs.len(), run.glyphs.len());
    }
}
