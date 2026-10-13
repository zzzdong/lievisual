use crate::geometry::{Color, Rect, Size};
use crate::text::style::*;
use std::sync::Arc;

/// A single glyph (1:1 with parley's positioned glyph).
#[derive(Debug, Clone, PartialEq)]
pub struct Glyph {
    /// Glyph ID.
    pub id: u32,
    /// X coordinate (offset relative to the origin of the owning [`TextLine::bounds`]).
    pub x: f32,
    /// Y coordinate (offset relative to the origin of the owning [`TextLine::bounds`]).
    pub y: f32,
    /// Advance width.
    pub advance: f32,
    /// Byte-cluster offset (relative to this run's own `text`, i.e. `text[cluster..]`).
    pub cluster: u32,
}

/// A text Run — a contiguous sequence of glyphs sharing the same style (font, size, color).
///
/// The smallest unit of text rendering. A Run is self-contained: `text` is a local slice
/// relative to the original `full_text`, and `glyphs[].cluster` is normalized to a local
/// offset relative to `text`, so it can be drawn independently of the owning Line and the
/// original string. The font is embedded as a self-contained `Vec<u8>` (`Arc`-shared, so
/// the whole font file is not copied per run).
///
/// Extended attributes (`url` / `decoration` / `background_color` / `baseline_shift`) are
/// mapped by lievisual from each [`RichSpan`]'s style ([`TextStyle`]) onto the run according
/// to byte ranges, so downstream can consume them directly without re-annotating.
#[derive(Debug, Clone)]
pub struct TextRun {
    /// The run's text content (self-contained, a local slice relative to the original
    /// full_text).
    pub text: String,
    /// Raw font bytes (parsed from parley's FontData blob; self-contained, `Arc`-shared).
    pub font_data: Arc<Vec<u8>>,
    /// Index of the font within the font-collection file (ttc / otc). Always 0 for a
    /// single-font file.
    ///
    /// Downstream (e.g. PDF font embedding) uses this to locate the specific font instance
    /// inside `font_data`.
    pub font_index: u32,
    /// Font size.
    pub font_size: f32,
    /// Whether the font is bold (backends using system fonts, such as SVG, emit this as
    /// `font-weight`).
    pub font_weight_bold: bool,
    /// Whether the font is italic (backends emit this as `font-style`).
    pub font_style_italic: bool,
    /// Text color.
    pub color: Color,
    /// Total advance width.
    pub advance: f32,
    /// Glyph list (coordinates offset relative to [`TextLine::bounds`] origin).
    pub glyphs: Vec<Glyph>,
    /// Whether the run is right-to-left.
    pub is_rtl: bool,
    /// Baseline X coordinate of the run's first character (relative to the layout origin).
    pub baseline_x: f32,
    /// Baseline Y coordinate of the line (offset from the line top; shared by all runs in
    /// the same line).
    pub baseline_y: f32,
    /// Hyperlink URL, if any.
    pub url: Option<String>,
    /// Text decoration (underline / strikethrough).
    pub decoration: TextDecoration,
    /// Baseline shift (moves superscripts / subscripts up or down within the line).
    pub baseline_shift: f32,
    /// Inline background color (inline code / highlight; `None` means no background).
    pub background_color: Option<Color>,
}

impl TextRun {
    /// Split the run into multiple sub-runs at byte offsets (relative to `text`).
    ///
    /// - `offsets` 会被去重排序，并只保留位于 `(0, text.len())` 内、落在字符边界
    ///   且与某字形 `cluster` 对齐的切点（不对齐的切点被忽略，字形不会丢失）。
    /// - 子 run 的 `glyphs[].cluster` 重定基为相对各自 `text` 的局部偏移；
    ///   `advance` 按子 run 字形重新求和；`baseline_x` 取子 run 首字形的 `x`
    ///   （使背景矩形 / 链接热区等按 `baseline_x + advance` 定位的绘制属性
    ///   在子 run 上依然正确）。
    /// - 字体、颜色、粗斜体、修饰、背景、链接、基线偏移等属性**原样继承**；
    ///   需要按 piece 区分扩展属性（如只给中段文本上背景色 / 链接）的调用方，
    ///   在切分后按 piece 改写即可。
    ///
    /// 无有效切点时返回原 run 的克隆（原样）。
    #[must_use]
    pub fn split_at(&self, offsets: &[usize]) -> Vec<TextRun> {
        let len = self.text.len();
        let mut cuts: Vec<usize> = offsets.to_vec();
        cuts.retain(|&o| {
            o > 0
                && o < len
                && self.text.is_char_boundary(o)
                && self.glyphs.iter().any(|g| g.cluster as usize == o)
        });
        cuts.sort_unstable();
        cuts.dedup();
        if cuts.is_empty() {
            return vec![self.clone()];
        }

        let mut bounds = cuts;
        bounds.insert(0, 0);
        bounds.push(len);

        let mut out = Vec::with_capacity(bounds.len() - 1);
        for w in bounds.windows(2) {
            let (start, end) = (w[0], w[1]);
            let glyphs: Vec<Glyph> = self
                .glyphs
                .iter()
                .filter(|g| {
                    let c = g.cluster as usize;
                    c >= start && c < end
                })
                .map(|g| Glyph {
                    cluster: g.cluster - start as u32,
                    ..g.clone()
                })
                .collect();
            if glyphs.is_empty() {
                continue;
            }
            let advance: f32 = glyphs.iter().map(|g| g.advance).sum();
            let baseline_x = glyphs[0].x;
            out.push(TextRun {
                text: self.text[start..end].to_string(),
                advance,
                glyphs,
                baseline_x,
                ..self.clone()
            });
        }
        out
    }
}

/// Per-line font metrics — mirrors Parley 0.12's `LineMetrics` (block-axis coords)
/// plus the font-level `cap_height` / `x_height` pulled from parley's `FontMetrics`.
///
/// All block-axis fields are **relative to the line's top edge** (i.e.
/// `parley_line_metrics.baseline - row_top_rel`, etc.), so consumers don't need
/// to know the line's position within the layout.
///
/// Parley 0.12 dropped `ascent` / `descent` from `LineMetrics`; they are now
/// derivable as `baseline - content_block_min` and `content_block_max - baseline`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LineMetrics {
    /// The absolute line height (`line_height`).
    pub line_height: f32,

    /// Block-axis coordinate of the line's baseline, relative to the line's top.
    pub baseline: f32,

    /// Top of the **line box** (half-leading included), relative to the line's top.
    /// Comes from `block_min_coord`.
    pub block_min: f32,

    /// Bottom of the **line box**, relative to the line's top.
    /// Comes from `block_max_coord`.
    pub block_max: f32,

    /// Top of the **typographic content** (ascender edge, no leading),
    /// relative to the line's top. Comes from `content_block_min_coord`.
    pub content_block_min: f32,

    /// Bottom of the **typographic content** (descender edge, no leading),
    /// relative to the line's top. Comes from `content_block_max_coord`.
    pub content_block_max: f32,

    /// Font-level cap height (baseline → top of uppercase). `0.0` when unknown.
    /// Sourced from parley's `FontMetrics`, not `LineMetrics`.
    pub cap_height: f32,

    /// Font-level x-height (baseline → top of lowercase). `0.0` when unknown.
    pub x_height: f32,
}

impl LineMetrics {
    /// Typographic ascent (baseline → content top), derived.
    pub fn ascent(&self) -> f32 {
        self.baseline - self.content_block_min
    }

    /// Typographic descent (baseline → content bottom), derived.
    pub fn descent(&self) -> f32 {
        self.content_block_max - self.baseline
    }
}

/// A text line — holds all [`TextRun`]s within one line.
///
/// Coordinate system (relative to the layout origin):
/// - `bounds.origin`: the line's position in the layout.
/// - `runs[].glyphs[].x/y`: offsets relative to `bounds.origin`.
#[derive(Debug, Clone)]
pub struct TextLine {
    /// All runs in this line.
    pub runs: Vec<TextRun>,
    /// The line's bounding box (relative to the layout origin).
    /// This is the **line box** (equivalent to `block_min/max`, kept for compat).
    pub bounds: Rect,
    /// Inline font metrics (block-axis coords + cap / x).
    pub metrics: LineMetrics,
}

impl TextLine {
    /// **Layout bounds** for this line: parley's default `line_height` box,
    /// i.e. `row_top → row_top + line_height`.
    pub fn layout_bounds(&self) -> Rect {
        self.bounds
    }

    /// **Line box bounds** — `block_min` → `block_max` (half-leading included).
    /// Identical to [`Self::layout_bounds`] in most cases; kept separate so the
    /// two concepts stay distinct if parley changes its default.
    pub fn line_box_bounds(&self) -> Rect {
        let top = self.bounds.min_y();
        Rect::new(
            self.bounds.min_x(),
            top + self.metrics.block_min as f64,
            self.bounds.max_x(),
            top + self.metrics.block_max as f64,
        )
    }

    /// **Logical / content bounds** — `content_block_min` → `content_block_max`.
    ///
    /// This is the equivalent of Pango's `logical_rect`: typographic ascent +
    /// descent, **without** leading. Stable across text changes, but for CJK
    /// the font's ascent margin still makes it look top-heavy.
    pub fn logical_bounds(&self) -> Rect {
        let top = self.bounds.min_y();
        Rect::new(
            self.bounds.min_x(),
            top + self.metrics.content_block_min as f64,
            self.bounds.max_x(),
            top + self.metrics.content_block_max as f64,
        )
    }

    /// **Cap bounds** — `baseline - cap_height` → `baseline`.
    ///
    /// Equivalent to CSS `text-box-edge: cap alphabetic`. Recommended for
    /// visual vertical centering of digits / uppercase / CJK. Because
    /// `cap_height` is a font-level constant, this box does **not** change
    /// when the line's text changes.
    ///
    /// Falls back to [`Self::logical_bounds`] when `cap_height` is unavailable.
    pub fn cap_bounds(&self) -> Rect {
        if self.metrics.cap_height > 0.0 {
            let top = self.bounds.min_y();
            let baseline_y = top + self.metrics.baseline as f64;
            Rect::new(
                self.bounds.min_x(),
                baseline_y - self.metrics.cap_height as f64,
                self.bounds.max_x(),
                baseline_y,
            )
        } else {
            self.logical_bounds()
        }
    }
}

/// Rich-text layout result — holds the laid-out set of lines.
///
/// All coordinates are relative to the layout origin (the paragraph's
/// top-left corner); pagination and absolute positioning are the output
/// backend's responsibility.
#[derive(Debug, Clone)]
pub struct TextLayout {
    /// All laid-out lines.
    pub lines: Vec<TextLine>,
    /// Total layout width.
    pub width: f64,
    /// Total layout height (sum of `line_height`, = parley's `layout.height()`).
    pub height: f64,
}

impl TextLayout {
    /// Aggregate **layout bounds** over all lines (parley's default box).
    pub fn layout_bounds(&self) -> Rect {
        self.lines
            .iter()
            .map(TextLine::layout_bounds)
            .fold(None, union_rect)
            .unwrap_or_else(|| Rect::new(0.0, 0.0, self.width, self.height))
    }

    /// Aggregate **line box bounds** over all lines.
    pub fn line_box_bounds(&self) -> Rect {
        self.lines
            .iter()
            .map(TextLine::line_box_bounds)
            .fold(None, union_rect)
            .unwrap_or_else(|| Rect::new(0.0, 0.0, self.width, self.height))
    }

    /// Aggregate **logical / content bounds** over all lines.
    pub fn logical_bounds(&self) -> Rect {
        self.lines
            .iter()
            .map(TextLine::logical_bounds)
            .fold(None, union_rect)
            .unwrap_or_else(|| Rect::new(0.0, 0.0, self.width, self.height))
    }

    /// Aggregate **cap bounds** over all lines.
    pub fn cap_bounds(&self) -> Rect {
        self.lines
            .iter()
            .map(TextLine::cap_bounds)
            .fold(None, union_rect)
            .unwrap_or_else(|| Rect::new(0.0, 0.0, self.width, self.height))
    }
}

fn union_rect(acc: Option<Rect>, r: Rect) -> Option<Rect> {
    Some(match acc {
        None => r,
        Some(a) => a.union(r),
    })
}
/// Text measurement result (the value returned by canvas `measureText()`).
///
/// Coordinate semantics (consistent with Canvas `TextMetrics`, where y-down quantities
/// take positive values):
/// - `*ascent` / `*descent` are relative to the **baseline** (positive upward / downward);
/// - `*baseline` are relative to the **top of the text block** (positive downward);
/// - `actual_bounding_box_*` are approximate (for multiline, derived from the first line's
///   metrics).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextMetrics {
    /// Text block width (canvas `width`).
    pub width: f64,
    /// Text block height (canvas has no direct field; equals `layout.height()`).
    pub height: f64,
    /// Distance from the anchor (left edge) to the leftmost glyph boundary, ≤ 0.
    pub actual_bounding_box_left: f64,
    /// Distance from the anchor (left edge) to the rightmost glyph boundary.
    pub actual_bounding_box_right: f64,
    /// Height from the font box top to the baseline (positive, upward).
    pub font_bounding_box_ascent: f64,
    /// Height from the baseline to the font box bottom (positive, downward).
    pub font_bounding_box_descent: f64,
    /// Actual glyph top to the baseline (≈ first line ascent).
    pub actual_bounding_box_ascent: f64,
    /// Baseline to the actual glyph bottom (≈ first line descent).
    pub actual_bounding_box_descent: f64,
    /// Em-box top to the baseline (≈ first line ascent).
    pub em_height_ascent: f64,
    /// Baseline to the em-box bottom (≈ first line descent).
    pub em_height_descent: f64,
    /// Distance from the top of the text block to the hanging baseline.
    pub hanging_baseline: f64,
    /// Distance from the top of the text block to the alphabetic baseline.
    pub alphabetic_baseline: f64,
    /// Distance from the top of the text block to the ideographic baseline.
    pub ideographic_baseline: f64,
    /// Single-line line height.
    pub line_height: f64,
}

/// A measurement result: size + metrics + reusable layout (avoids re-layout when drawing).
pub struct TextMeasure {
    /// Text block size (`width` / `height`).
    pub size: Size,
    /// Detailed metrics (canvas `TextMetrics`).
    pub metrics: TextMetrics,
    /// Pre-computed layout, which can be fed to [`crate::scene::Element::Text::layout`] for
    /// exact glyph drawing.
    pub layout: Arc<TextLayout>,
}
