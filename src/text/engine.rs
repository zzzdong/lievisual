    use super::*;
    use crate::geometry::{Color, Rect, Size};
    use crate::text::style::HANGING_ASCENT_RATIO;
    use parley::fontique::{Collection, CollectionOptions, SourceCache};
    use parley::style::FontFamily;
    use parley::{FontContext, LayoutContext, StyleProperty};
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex, OnceLock};

    // A font-byte cache shared across layouts: the key is the stable `id()` of parley's
    // `Blob` (unique per font). This avoids every Run `to_vec`-ing a whole font file
    // (potentially tens of MB), which would make memory blow up with text volume. Each
    // unique font is copied only once per process, and the rest of the Runs share it via
    // `Arc`.
    static FONT_BYTE_CACHE: OnceLock<Mutex<HashMap<u64, Arc<Vec<u8>>>>> = OnceLock::new();

    // Process-wide font **collection** (`shared: true` makes every clone observe later
    // registrations). Each thread's `FontContext` is built from a clone of it, so a font
    // registered on one thread is visible on all threads (including ones started later) —
    // with a plain per-thread `FontContext::new()` the registration stayed thread-local and
    // worker threads silently fell back to a default font.
    static GLOBAL_FONT_COLLECTION: OnceLock<Mutex<Collection>> = OnceLock::new();

    /// The process-wide font collection (system fonts + everything ever registered).
    fn global_font_collection() -> &'static Mutex<Collection> {
        GLOBAL_FONT_COLLECTION.get_or_init(|| {
            Mutex::new(Collection::new(CollectionOptions {
                shared: true,
                ..Default::default()
            }))
        })
    }

    /// Build a fresh `FontContext` sharing the global collection.
    fn new_font_context() -> FontContext {
        let collection = global_font_collection()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        FontContext {
            collection,
            source_cache: SourceCache::default(),
        }
    }

    // One cached layout context per thread: lazily initialized, reused by every
    // measurement / layout within the thread. parley's official guidance designs
    // FontContext (font library + LRU source cache) and LayoutContext (scratch + glyph
    // cache) as "one per application / per thread"; thread_local realizes that semantics and
    // avoids the system-font scanning and allocation of rebuilding each time. Every
    // parley-related operation goes through with_cached_context to reuse this same pair of
    // instances; the font collection behind them is shared process-wide (see
    // `global_font_collection`).
    thread_local! {
        static FONT_CONTEXT: RefCell<FontContext> = RefCell::new(new_font_context());
        static LAYOUT_CONTEXT: RefCell<LayoutContext<Color>> = RefCell::new(LayoutContext::new());
    }

    /// Borrow the thread-local cached contexts to perform one layout (the single entry
    /// point for all parley operations).
    fn with_cached_context<R>(
        f: impl FnOnce(&mut FontContext, &mut LayoutContext<Color>) -> R,
    ) -> R {
        FONT_CONTEXT.with(|fc| {
            LAYOUT_CONTEXT.with(|lc| {
                let mut fc = fc.borrow_mut();
                let mut lc = lc.borrow_mut();
                f(&mut fc, &mut lc)
            })
        })
    }

    /// Measure rich text (a [`RichSpan`] list, canvas `measureText`) and return
    /// size / metrics / reusable layout.
    ///
    /// Reuses the thread-local cached `FontContext` / `LayoutContext` (parley internally
    /// caches fonts, glyphs, and scratch allocations), avoiding the system-font scan that
    /// rebuilding each call would cause. `max_width` acts as the wrap limit for the whole
    /// paragraph. A plain string is the special case of a single [`RichSpan`].
    pub fn measure_text(spans: &[RichSpan], max_width: Option<f64>) -> TextMeasure {
        with_cached_context(|fc, lc| {
            let layout = build_rich_layout(spans, max_width, fc, lc);
            let metrics = layout_metrics(&layout);
            TextMeasure {
                size: Size::new(metrics.width, metrics.height),
                metrics,
                layout: Arc::new(layout),
            }
        })
    }

    /// Lay out rich text (a [`RichSpan`] list) into a single [`TextLayout`], returning an
    /// `Arc` (which can be fed directly as the pre-computed `layout` of
    /// [`crate::scene::Element::Text`] to the renderer).
    ///
    /// Shares the same TLS context with [`measure_text`]; reuse it for "layout only"
    /// scenarios to avoid re-layout.
    pub fn layout_text(spans: &[RichSpan], max_width: Option<f64>) -> Arc<TextLayout> {
        with_cached_context(|fc, lc| Arc::new(build_rich_layout(spans, max_width, fc, lc)))
    }

    /// Build a rich-text layout with a `RangedBuilder` (shared by measurement and
    /// rendering so sizes stay consistent) and extract it into a self-contained
    /// [`TextLayout`] (with glyph-level runs / glyphs and extended attributes).
    fn build_rich_layout(
        spans: &[RichSpan],
        max_width: Option<f64>,
        font_cx: &mut FontContext,
        layout_cx: &mut LayoutContext<Color>,
    ) -> TextLayout {
        if spans.is_empty() {
            let empty = TextStyle::new(Color::BLACK, 12.0, "sans-serif");
            return build_layout(font_cx, layout_cx, "", &empty, max_width);
        }

        // Concatenate all text and record the byte range of each span.
        let mut combined = String::new();
        let mut ranges: Vec<(usize, usize)> = Vec::with_capacity(spans.len());
        for span in spans {
            let start = combined.len();
            combined.push_str(&span.text);
            ranges.push((start, combined.len()));
        }

        let mut builder = layout_cx.ranged_builder(font_cx, &combined, 1.0, true);

        // Use the first span's style as the default; override differing properties per span.
        let base = &spans[0].style;
        push_text_style_defaults(&mut builder, base);

        for (i, span) in spans.iter().enumerate().skip(1) {
            let (start, end) = ranges[i];
            if start >= end {
                continue;
            }
            push_style_diff(&mut builder, &span.style, base, start..end);
        }

        let mut layout = builder.build(&combined);
        layout.break_all_lines(max_width.map(|w| w as f32));
        // Apply parley alignment according to the first span's align: Center/Right align
        // multi-line within the block, Justify adjusts letter spacing during layout. The
        // renderer's dx only positions the whole block relative to the anchor — the two are
        // orthogonal.
        apply_parley_align(&mut layout, spans[0].style.align);

        // Extract into a self-contained structure; extended attributes
        // (url/decoration/background/baseline_shift) are mapped onto each run by span byte
        // range.
        extract_lines_from_parley(&layout, &combined, spans)
    }

    /// Parse a CSS `font-family` list into parley's `FontFamily` (consistent with
    /// [`build_layout`]).
    fn font_family_prop(raw: &str) -> FontFamily<'_> {
        use parley::style::FontFamilyName;
        let families: Vec<FontFamilyName> = FontFamilyName::parse_css_list(raw)
            .filter_map(Result::ok)
            .collect();
        if families.is_empty() {
            FontFamily::named(raw)
        } else {
            FontFamily::List(families.into())
        }
    }

    fn parley_font_style(style: FontStyle) -> parley::style::FontStyle {
        match style {
            FontStyle::Normal => parley::style::FontStyle::Normal,
            FontStyle::Italic => parley::style::FontStyle::Italic,
            FontStyle::Oblique => parley::style::FontStyle::Oblique(None),
        }
    }

    fn parley_line_height(lh: f64, font_size: f64) -> parley::style::LineHeight {
        let factor = (lh / font_size.max(1e-6)) as f32;
        parley::style::LineHeight::FontSizeRelative(factor)
    }

    /// Apply parley paragraph alignment according to alignment (Center/Right align lines
    /// within the block, Justify adjusts letter spacing).
    ///
    /// This is orthogonal to the renderer's anchor offset: parley handles alignment of lines
    /// **within the block**, while the renderer's dx positions the **whole block** relative
    /// to the anchor.
    fn apply_parley_align(layout: &mut parley::Layout<Color>, align: TextAlign) {
        let p = match align {
            TextAlign::Left => parley::Alignment::Start,
            TextAlign::Center => parley::Alignment::Center,
            TextAlign::Right => parley::Alignment::End,
            TextAlign::Justify => parley::Alignment::Justify,
        };
        layout.align(p, parley::AlignmentOptions::default());
    }

    /// Push the `TextStyle`'s typographic properties as the builder's defaults (shared by
    /// `build_layout` and `build_rich_layout`, so plain text and rich text map consistently).
    ///
    /// Covers: font family / size / width / weight / style / color / line height / letter
    /// spacing / underline / strikethrough. When `line_height` is `None` it is left unset
    /// (the font's default is used).
    fn push_text_style_defaults(builder: &mut parley::RangedBuilder<'_, Color>, style: &TextStyle) {
        builder.push_default(StyleProperty::FontFamily(font_family_prop(
            &style.font_family,
        )));
        builder.push_default(StyleProperty::FontSize(style.font_size as f32));
        builder.push_default(StyleProperty::Brush(style.color));
        builder.push_default(StyleProperty::FontWeight(parley::style::FontWeight::new(
            style.font_weight.value(),
        )));
        builder.push_default(StyleProperty::FontStyle(parley_font_style(
            style.font_style,
        )));

        builder.push_default(StyleProperty::FontWidth(
            parley::style::FontWidth::from_ratio(style.font_width.ratio()),
        ));

        if let Some(lh) = style.line_height {
            builder.push_default(StyleProperty::LineHeight(parley_line_height(
                lh,
                style.font_size,
            )));
        }
        if style.letter_spacing != 0.0 {
            builder.push_default(StyleProperty::LetterSpacing(style.letter_spacing as f32));
        }
        builder.push_default(StyleProperty::Underline(style.underline));
        builder.push_default(StyleProperty::UnderlineBrush(style.underline_color));
        builder.push_default(StyleProperty::Strikethrough(style.strikethrough));
        builder.push_default(StyleProperty::StrikethroughBrush(style.strikethrough_color));
    }

    /// Push only the style properties of `TextStyle` that differ from `base`, over the
    /// given range (per-span for rich text).
    fn push_style_diff(
        builder: &mut parley::RangedBuilder<'_, Color>,
        s: &TextStyle,
        base: &TextStyle,
        range: std::ops::Range<usize>,
    ) {
        if s.font_family != base.font_family {
            builder.push(
                StyleProperty::FontFamily(font_family_prop(&s.font_family)),
                range.clone(),
            );
        }
        if (s.font_size - base.font_size).abs() > f64::EPSILON {
            builder.push(StyleProperty::FontSize(s.font_size as f32), range.clone());
        }
        if s.color != base.color {
            builder.push(StyleProperty::Brush(s.color), range.clone());
        }
        if s.font_weight != base.font_weight {
            builder.push(
                StyleProperty::FontWeight(parley::style::FontWeight::new(s.font_weight.value())),
                range.clone(),
            );
        }
        if s.font_style != base.font_style {
            builder.push(
                StyleProperty::FontStyle(parley_font_style(s.font_style)),
                range.clone(),
            );
        }
        if s.font_width != base.font_width {
            builder.push(
                StyleProperty::FontWidth(parley::style::FontWidth::from_ratio(
                    s.font_width.ratio(),
                )),
                range.clone(),
            );
        }
        if s.line_height != base.line_height {
            builder.push(
                StyleProperty::LineHeight(parley_line_height(
                    s.line_height.unwrap_or(s.font_size),
                    s.font_size,
                )),
                range.clone(),
            );
        }
        if (s.letter_spacing - base.letter_spacing).abs() > f64::EPSILON {
            builder.push(
                StyleProperty::LetterSpacing(s.letter_spacing as f32),
                range.clone(),
            );
        }
        if s.underline != base.underline {
            builder.push(StyleProperty::Underline(s.underline), range.clone());
        }
        if s.underline_color != base.underline_color {
            builder.push(
                StyleProperty::UnderlineBrush(s.underline_color),
                range.clone(),
            );
        }
        if s.strikethrough != base.strikethrough {
            builder.push(StyleProperty::Strikethrough(s.strikethrough), range.clone());
        }
        if s.strikethrough_color != base.strikethrough_color {
            builder.push(
                StyleProperty::StrikethroughBrush(s.strikethrough_color),
                range.clone(),
            );
        }
    }

    /// Extract metrics from a laid-out layout (canvas `TextMetrics`).
    pub fn layout_metrics(layout: &TextLayout) -> TextMetrics {
        let width = layout.width;
        let height = layout.height;
        let (ascent, descent, baseline, line_height) = match layout.lines.first() {
            Some(line) => {
                let m = line.metrics;
                (
                    m.ascent as f64,
                    m.descent as f64,
                    m.baseline as f64,
                    m.line_height as f64,
                )
            }
            None => (0.0, 0.0, 0.0, 0.0),
        };
        TextMetrics {
            width,
            height,
            actual_bounding_box_left: 0.0,
            actual_bounding_box_right: width,
            font_bounding_box_ascent: ascent,
            font_bounding_box_descent: descent,
            actual_bounding_box_ascent: ascent,
            actual_bounding_box_descent: descent,
            em_height_ascent: ascent,
            em_height_descent: descent,
            hanging_baseline: ascent * HANGING_ASCENT_RATIO,
            alphabetic_baseline: baseline,
            ideographic_baseline: baseline + descent * 0.1,
            line_height,
        }
    }

    /// Build a parley layout (shared by measurement and rendering so sizes stay
    /// consistent) and extract it into a self-contained [`TextLayout`].
    pub(crate) fn build_layout(
        font_ctx: &mut FontContext,
        layout_ctx: &mut LayoutContext<Color>,
        content: &str,
        style: &TextStyle,
        max_width: Option<f64>,
    ) -> TextLayout {
        // scale = 1.0 (display scaling); font size is set via the FontSize property, so
        // layout coordinates remain logical pixels.
        let mut builder = layout_ctx.ranged_builder(font_ctx, content, 1.0, true);
        push_text_style_defaults(&mut builder, style);

        let mut layout = builder.build(content);
        // Max width is the wrap limit; pass None when unlimited.
        layout.break_all_lines(max_width.map(|w| w as f32));
        // Apply parley alignment according to align: Center/Right align multi-line within
        // the block, Justify adjusts letter spacing during layout. The renderer's dx only
        // positions the whole block relative to the anchor — the two are orthogonal.
        apply_parley_align(&mut layout, style.align);

        // Treat a plain string as a single span and extract into a self-contained structure.
        let span = RichSpan::new(content.to_string(), style.clone());
        let spans = std::slice::from_ref(&span);
        extract_lines_from_parley(&layout, content, spans)
    }

    /// Raw glyph data (coordinates relative to the layout origin); used only during
    /// extraction.
    struct GlyphRaw {
        id: u32,
        x: f32,
        y: f32,
        advance: f32,
        cluster: u32,
    }

    /// Extract a self-contained [`TextLayout`] from a parley Layout, mapping extended
    /// attributes (`url` / `decoration` / `background_color` / `baseline_shift`) onto each
    /// run by span byte range.
    ///
    /// Each [`TextLine::bounds`] gives the line's position within the layout: `x` is the
    /// leftmost glyph's offset from the layout's left, `y` is the cumulative offset of the
    /// line top from the layout top. A run's `glyphs[].x/y` are offsets relative to the
    /// line origin (self-contained, drawable independently of the layout / full_text).
    ///
    /// Font bytes are deduplicated process-wide by `Blob::id()`; each unique font is
    /// `to_vec`-ed only once into an `Arc`, and the rest of the runs share it, avoiding
    /// per-run copies of a whole font file that would blow up memory.
    fn extract_lines_from_parley(
        layout: &parley::Layout<Color>,
        full_text: &str,
        spans: &[RichSpan],
    ) -> TextLayout {
        // Build the span byte-range table, used to look up extended attributes
        // (url/decoration/background/baseline_shift).
        let mut span_ranges: Vec<(usize, usize, &TextStyle)> = Vec::with_capacity(spans.len());
        let mut acc = 0usize;
        for span in spans {
            let end = acc + span.text.len();
            span_ranges.push((acc, end, &span.style));
            acc = end;
        }

        let mut lines = Vec::new();
        let mut row_top_rel = 0.0_f32;

        for line in layout.lines() {
            let m = line.metrics();
            let line_metrics = LineMetrics {
                ascent: m.ascent,
                descent: m.descent,
                baseline: m.baseline,
                line_height: m.line_height,
            };
            let line_height = m.line_height;
            let baseline_y = m.baseline - row_top_rel;

            let mut glyph_data: Vec<(GlyphRaw, usize)> = Vec::new();
            #[allow(clippy::type_complexity)]
            let mut run_infos: Vec<(
                Color,
                Arc<Vec<u8>>,
                u32,
                f32,
                parley::layout::Run<'_, Color>,
                f32,
            )> = Vec::new();

            let mut next_run_idx = 0;
            for item in line.items() {
                if let parley::layout::PositionedLayoutItem::GlyphRun(glyph_run) = item {
                    let run = glyph_run.run();
                    let color = glyph_run.style().brush;
                    // Font bytes + collection index: bytes are deduplicated in cache by Blob::id().
                    let font = run.font();
                    let blob = &font.data;
                    let font_id = blob.id();
                    let font_index = font.index;
                    let font_arc = {
                        let mut cache = FONT_BYTE_CACHE
                            .get_or_init(|| Mutex::new(HashMap::new()))
                            .lock()
                            .expect("font cache poisoned");
                        cache
                            .entry(font_id)
                            .or_insert_with(|| Arc::new(blob.data().to_vec()))
                            .clone()
                    };
                    let font_size = run.font_size();

                    let first_glyph_x = glyph_run
                        .positioned_glyphs()
                        .next()
                        .map(|g| g.x)
                        .unwrap_or(0.0);

                    run_infos.push((color, font_arc, font_index, font_size, *run, first_glyph_x));

                    // Collect positioned glyphs; the cluster is provided via the visual_clusters'
                    // text_range.
                    let positioned: Vec<_> = glyph_run.positioned_glyphs().collect();
                    let mut gi = 0usize;
                    for cluster in glyph_run.run().visual_clusters() {
                        let cluster_byte = cluster.text_range().start as u32;
                        let cluster_glyphs: Vec<_> = cluster.glyphs().collect();
                        for _cg in &cluster_glyphs {
                            if gi < positioned.len() {
                                let g = &positioned[gi];
                                glyph_data.push((
                                    GlyphRaw {
                                        id: g.id,
                                        x: g.x,
                                        y: g.y,
                                        advance: g.advance,
                                        cluster: cluster_byte,
                                    },
                                    next_run_idx,
                                ));
                                gi += 1;
                            }
                        }
                    }
                    next_run_idx += 1;
                }
            }

            if glyph_data.is_empty() {
                row_top_rel += line_height;
                continue;
            }

            let min_x = glyph_data
                .iter()
                .map(|(g, _)| g.x)
                .fold(f32::INFINITY, f32::min);
            let max_x = glyph_data
                .iter()
                .map(|(g, _)| g.x)
                .fold(f32::NEG_INFINITY, f32::max);
            let width = max_x - min_x;

            let mut runs = Vec::new();

            // Split glyphs by run index.
            let mut run_glyph_ranges: Vec<(usize, usize)> = Vec::new();
            let mut current_start = 0;
            let mut last_run_idx = glyph_data[0].1;
            for (i, (_, run_idx)) in glyph_data.iter().enumerate() {
                if *run_idx != last_run_idx {
                    run_glyph_ranges.push((current_start, i));
                    current_start = i;
                    last_run_idx = *run_idx;
                }
            }
            run_glyph_ranges.push((current_start, glyph_data.len()));

            for (start_idx, end_idx) in run_glyph_ranges.iter() {
                let run_idx = glyph_data[*start_idx].1;
                let (color, font_data, font_index, font_size, run, first_glyph_x) =
                    &run_infos[run_idx];

                let run_text_range = run.text_range();
                let text_start = run_text_range.start;
                let text_end = run_text_range.end;

                let run_text = full_text
                    .get(text_start..text_end)
                    .map(str::to_string)
                    .unwrap_or_default();

                // Normalize the cluster into a local offset relative to run_text (self-contained).
                let relative_glyphs: Vec<Glyph> = glyph_data[*start_idx..*end_idx]
                    .iter()
                    .map(|(g, _)| Glyph {
                        id: g.id,
                        x: g.x - min_x,
                        y: g.y - row_top_rel,
                        advance: g.advance,
                        cluster: g.cluster.saturating_sub(text_start as u32),
                    })
                    .collect();

                let baseline_x = *first_glyph_x - min_x;

                // Extended attributes: look up by byte range across spans (pass the run's
                // [start, end) so a merged run spanning multiple spans still matches the span
                // carrying url/background).
                let (url, decoration, background_color, baseline_shift) =
                    lookup_extended_props(text_start, text_end, &span_ranges);

                // Apply the baseline shift to the glyph y: shift>0 (superscript) → y grows →
                // visually moves up (y-down).
                let adjusted_glyphs: Vec<Glyph> = relative_glyphs
                    .iter()
                    .map(|g| Glyph {
                        id: g.id,
                        x: g.x,
                        y: g.y + baseline_shift,
                        advance: g.advance,
                        cluster: g.cluster,
                    })
                    .collect();
                let adjusted_baseline_y = baseline_y - baseline_shift;
                let advance = adjusted_glyphs.iter().map(|g| g.advance).sum();

                // Font weight / italic (needed by backends using system fonts such as SVG).
                let font_attrs = run.font_attrs();
                let font_weight_bold = font_attrs.weight >= parley::fontique::FontWeight::BOLD;
                let font_style_italic = font_attrs.style != parley::fontique::FontStyle::Normal;

                runs.push(TextRun {
                    text: run_text,
                    font_data: font_data.clone(),
                    font_index: *font_index,
                    font_size: *font_size,
                    font_weight_bold,
                    font_style_italic,
                    color: *color,
                    advance,
                    glyphs: adjusted_glyphs,
                    is_rtl: false,
                    baseline_x,
                    baseline_y: adjusted_baseline_y,
                    url,
                    decoration,
                    baseline_shift,
                    background_color,
                });
            }

            let bounds = Rect::new(
                min_x as f64,
                row_top_rel as f64,
                (min_x + width) as f64,
                (row_top_rel + line_height) as f64,
            );

            // Line-level visual ink bounds (relative to the layout origin).
            //
            // Derived from the actual ink bounding box of every glyph on the line, not from the
            // line `baseline` (which sits *above* descenders such as 'g'/'y', so using it as the
            // bottom would crop any descenders). For each glyph we read its stored bbox directly
            // via skrifa's `GlyphMetrics` (`GlyphOutline::ink_box`) — exact and allocation-free,
            // scaled to `font_size` and already in y-down convention (origin = baseline, same as
            // `glyph.rs`). We overlay that box at the glyph's `(x, y)` (which are relative to the
            // line bounds origin) and union all of them. Glyphs with no ink box (e.g. space,
            // `.notdef`) are skipped so they don't distort the union.
            let ink_bounds = runs
                .iter()
                .flat_map(|run| {
                    run.glyphs.iter().filter_map(move |g| {
                        let bbox = crate::text::glyph::GlyphOutline::ink_box(
                            &run.font_data,
                            run.font_index,
                            g.id,
                            run.font_size,
                        )?;
                        if bbox.width() <= 0.0 && bbox.height() <= 0.0 {
                            return None;
                        }
                        // Glyph bbox origin = baseline; overlay at the glyph's (x, y), which are
                        // already relative to the line bounds origin (y-down).
                        Some(Rect::new(
                            (bounds.min_x() + g.x as f64 + bbox.min_x()).max(bounds.min_x()),
                            (bounds.min_y() + g.y as f64 + bbox.min_y()).max(bounds.min_y()),
                            (bounds.min_x() + g.x as f64 + bbox.max_x()).min(bounds.max_x()),
                            (bounds.min_y() + g.y as f64 + bbox.max_y()).min(bounds.max_y()),
                        ))
                    })
                })
                .fold(None, |acc: Option<Rect>, r| match acc {
                    None => Some(r),
                    Some(a) => Some(a.union(r)),
                })
                .unwrap_or_else(|| {
                    // No glyph produced an ink box (e.g. whitespace-only line): fall back to the
                    // line ascent/descent box so the ink bounds stay non-degenerate.
                    let ascent = line_metrics.ascent as f64;
                    let descent = line_metrics.descent as f64;
                    let baseline = line_metrics.baseline as f64;
                    Rect::new(
                        bounds.min_x(),
                        bounds.min_y() + baseline - ascent,
                        bounds.max_x(),
                        bounds.min_y() + baseline + descent,
                    )
                });

            lines.push(TextLine {
                runs,
                bounds,
                metrics: line_metrics,
                ink_bounds,
            });

            row_top_rel += line_height;
        }

        let width = layout.width() as f64;
        let height = layout.height() as f64;
        // Aggregate the ink bounds of all lines (relative to the layout origin).
        let ink_bounds = lines
            .iter()
            .map(|l| l.ink_bounds)
            .fold(None, |acc: Option<Rect>, r| match acc {
                None => Some(r),
                Some(a) => Some(Rect::new(
                    a.min_x().min(r.min_x()),
                    a.min_y().min(r.min_y()),
                    a.max_x().max(r.max_x()),
                    a.max_y().max(r.max_y()),
                )),
            })
            .unwrap_or_else(|| Rect::new(0.0, 0.0, width, height));
        TextLayout {
            lines,
            width,
            height,
            ink_bounds,
        }
    }

    /// Look up the extended attributes (url / decoration / background / baseline_shift) for
    /// the byte range `[start, end)`.
    ///
    /// A run may span multiple spans (after CJK prioritization, parley often merges a whole
    /// line into one run). We therefore match by **range overlap**: prefer a span that
    /// overlaps `[start, end)` and carries a url / background; otherwise fall back to the
    /// span containing the start point. This way, when "plain text + inline link" is merged
    /// into a single run, the link span's url still hits, not just the span at the run's
    /// start.
    fn lookup_extended_props(
        start: usize,
        end: usize,
        span_ranges: &[(usize, usize, &TextStyle)],
    ) -> (Option<String>, TextDecoration, Option<Color>, f32) {
        let matched = span_ranges
            .iter()
            .find(|(s, e, st)| {
                *s < end && *e > start && (st.url.is_some() || st.background_color.is_some())
            })
            .or_else(|| {
                span_ranges
                    .iter()
                    .find(|(s, e, _)| *s <= start && start < *e)
            });
        match matched {
            Some((_, _, st)) => {
                let decoration = if st.underline && st.strikethrough {
                    // On conflict treat as none (the caller applies span-level styles itself).
                    TextDecoration::None
                } else if st.strikethrough {
                    TextDecoration::LineThrough
                } else if st.underline {
                    TextDecoration::Underline
                } else {
                    TextDecoration::None
                };
                (
                    st.url.clone(),
                    decoration,
                    st.background_color,
                    st.baseline_shift as f32,
                )
            }
            None => (None, TextDecoration::None, None, 0.0),
        }
    }

    /// Font source.
    #[derive(Debug, Clone)]
    pub enum FontSource {
        /// Load from a file path.
        Path(std::path::PathBuf),
        /// Load from in-memory data.
        Memory(Vec<u8>),
    }

    /// Register a custom font into the process-wide font collection.
    ///
    /// Once loaded, the font can be referenced by its `font_family` name in text styles.
    /// Registration is visible to **every** thread (each thread's context is a clone of the
    /// shared collection), so it covers measurement and rendering alike, including threads
    /// started after the registration.
    pub fn register_font(
        source: FontSource,
        family_name_override: Option<&str>,
    ) -> Result<(), String> {
        register_font_generic(source, family_name_override, None)
    }

    /// Register a custom font and optionally associate it with a **generic font family**
    /// (`GenericFamily`).
    ///
    /// Same as [`register_font`], with the extra ability to attach the font to a generic
    /// family such as `sans-serif` / `monospace` / `serif`, so that `font-family: monospace`
    /// uses the registered font instead of the system default. With `generic_family` set to
    /// `None` it is equivalent to [`register_font`].
    ///
    /// The generic-family type [`parley::fontique::GenericFamily`] is re-exported via
    /// [`crate::parley`].
    pub fn register_font_generic(
        source: FontSource,
        family_name_override: Option<&str>,
        generic_family: Option<parley::fontique::GenericFamily>,
    ) -> Result<(), String> {
        use parley::fontique::Blob;
        use std::sync::Arc;

        let data = match source {
            FontSource::Path(path) => {
                let bytes =
                    std::fs::read(&path).map_err(|e| format!("Failed to read font file: {e}"))?;
                Blob::new(Arc::new(bytes))
            }
            FontSource::Memory(bytes) => Blob::new(Arc::new(bytes)),
        };

        let override_info = family_name_override.map(|name| parley::fontique::FontInfoOverride {
            family_name: Some(name),
            ..Default::default()
        });

        // Register into the process-wide (shared) collection: every clone — including the
        // current thread's and those created by threads started later — observes it.
        let mut collection = global_font_collection()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let registered = collection.register_fonts(data, override_info);
        if let Some(generic) = generic_family {
            let ids: Vec<_> = registered.into_iter().map(|(id, _)| id).collect();
            collection.append_generic_families(generic, ids.into_iter());
        }
        drop(collection);
        // Touch the thread-local context once so it pulls the new shared state (fontique
        // syncs lazily on the next query; this makes it deterministic for the caller).
        with_cached_context(|font_cx, _| {
            let _ = font_cx.collection.family_names().count();
        });
        Ok(())
    }

    /// Parse a generic family name (`"monospace"` / `"sans-serif"`, etc.) into parley's
    /// [`parley::fontique::GenericFamily`].
    ///
    /// Used to associate a registered font with a generic family (see
    /// [`register_font_generic`]) and to normalize generic family names inside `font-family`.
    /// Returns `None` when the name is not recognized.
    #[must_use]
    pub fn parse_generic_family(name: &str) -> Option<parley::fontique::GenericFamily> {
        use parley::fontique::GenericFamily;
        match name.to_lowercase().as_str() {
            "serif" => Some(GenericFamily::Serif),
            "sans-serif" | "sansserif" => Some(GenericFamily::SansSerif),
            "monospace" => Some(GenericFamily::Monospace),
            "cursive" => Some(GenericFamily::Cursive),
            "fantasy" => Some(GenericFamily::Fantasy),
            "system-ui" | "systemui" => Some(GenericFamily::SystemUi),
            "ui-serif" | "uiserif" => Some(GenericFamily::UiSerif),
            "ui-sans-serif" | "uisansserif" => Some(GenericFamily::UiSansSerif),
            "ui-monospace" | "uimonospace" => Some(GenericFamily::UiMonospace),
            "ui-rounded" | "uirounded" => Some(GenericFamily::UiRounded),
            "emoji" => Some(GenericFamily::Emoji),
            "math" => Some(GenericFamily::Math),
            "fangsong" => Some(GenericFamily::FangSong),
            _ => None,
        }
    }

    /// Offset from the anchor to the text block's top-left corner.
    ///
    /// Computes the offset from the anchor coordinates to the text block's top-left corner
    /// according to the desired alignment / baseline. Usage pattern for callers: first get a
    /// layout via [`layout_text`], then compute the offset via [`compute_text_offset`]; the
    /// anchor plus the offset gives the text block's top-left corner (draw TextRuns with
    /// Left/Top).
    pub fn compute_text_offset(
        layout: &TextLayout,
        align: TextAlign,
        baseline: TextBaseline,
    ) -> (f64, f64) {
        let layout_width = layout.width;
        let layout_height = layout.height;

        let x_offset = match align {
            TextAlign::Left | TextAlign::Justify => 0.0,
            TextAlign::Center => -layout_width / 2.0,
            TextAlign::Right => -layout_width,
        };

        let y_offset = match baseline {
            TextBaseline::Top => 0.0,
            // Use the ink_bounds' true visual height for centering, to avoid the text being
            // pushed up by the descent/leading margins inside parley's layout.height.
            // ink_bounds.min_y is relative to the layout origin (i.e. the position when
            // rendered with Top).
            // Middle: vertically centered on the *ink* (visible glyphs), so the text block is
            // centered on the anchor by its actual visual height. Using ink_bounds (like
            // `Bottom`) avoids the glyphs being pushed up by descent/leading margins, which is
            // what callers (e.g. liemermaid node labels) expect when they pass a box center.
            TextBaseline::Middle => {
                let ib = layout.ink_bounds();
                let vh = (ib.max_y() - ib.min_y()).max(0.0);
                if vh > 0.0 {
                    -(ib.min_y() + vh / 2.0)
                } else {
                    -layout_height / 2.0
                }
            }
            TextBaseline::Bottom => {
                let ib = layout.ink_bounds();
                let vh = (ib.max_y() - ib.min_y()).max(0.0);
                if vh > 0.0 {
                    -(ib.min_y() + vh)
                } else {
                    -layout_height
                }
            }
            TextBaseline::Alphabetic => {
                // Reuse `layout_metrics().alphabetic_baseline` so this path stays identical to
                // `anchor_offset` (no duplicated ascent/descent arithmetic).
                -layout_metrics(layout).alphabetic_baseline
            }
            // Hanging / Ideographic reuse the same `TextMetrics` fields as `anchor_offset`,
            // in y-down the anchor sits below the top, so we negate. When there is no first
            // line the layout is empty and we fall back to the hanging-ratio estimate.
            TextBaseline::Hanging => {
                let m = layout_metrics(layout);
                if layout.lines.is_empty() {
                    -(layout_height * HANGING_ASCENT_RATIO)
                } else {
                    -m.hanging_baseline
                }
            }
            TextBaseline::Ideographic => {
                let m = layout_metrics(layout);
                if layout.lines.is_empty() {
                    -(layout_height * HANGING_ASCENT_RATIO)
                } else {
                    -m.ideographic_baseline
                }
            }
        };

        (x_offset, y_offset)
    }
