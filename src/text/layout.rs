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

/// Per-line font metrics (from parley's `LineMetrics`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LineMetrics {
    /// Ascent (baseline upward).
    pub ascent: f32,
    /// Descent (baseline downward).
    pub descent: f32,
    /// Distance from the baseline to the line top.
    pub baseline: f32,
    /// Line height.
    pub line_height: f32,
}

/// A text line — holds all [`TextRun`]s within one line.
///
/// Coordinate system (relative to the layout origin):
/// - `bounds.origin`: the line's position in the layout.
/// - `runs[].glyphs[].x/y`: offsets relative to `bounds.origin`.
///
/// For absolute positioning: glyph coordinate = page offset + bounds.origin + glyph.
#[derive(Debug, Clone)]
pub struct TextLine {
    /// All runs in this line.
    pub runs: Vec<TextRun>,
    /// The line's bounding box (relative to the layout origin).
    pub bounds: Rect,
    /// Inline font metrics (ascent / descent / baseline / line_height).
    pub metrics: LineMetrics,
    /// The ink bounding box of the line's actual glyphs (outline bbox, relative to the
    /// layout origin), used for precise vertical centering.
    pub ink_bounds: Rect,
}

/// Rich-text layout result — holds the laid-out set of lines.
///
/// Produced by [`crate::layout_text`] / [`crate::measure_text`], it is **self-contained**: it does not
/// depend on parley's lifetime because `font_data` embeds the font bytes and
/// `glyphs[].cluster` is a local offset relative to the run text. Downstream (e.g. the PDF
/// / SVG backends of liepress) can simply iterate `lines` to draw, without touching parley
/// again.
///
/// All coordinates are relative to the layout origin (the paragraph's top-left corner);
/// pagination and absolute positioning are the output backend's responsibility.
#[derive(Debug, Clone)]
pub struct TextLayout {
    /// All laid-out lines.
    pub lines: Vec<TextLine>,
    /// Total layout width.
    pub width: f64,
    /// Total layout height.
    pub height: f64,
    /// The ink bounding box of every line's actual glyphs (outline bbox, relative to the
    /// layout origin), used for precise vertical centering.
    pub ink_bounds: Rect,
}

impl TextLayout {
    /// The ink bounding box of the actual glyphs (outline bbox, relative to the layout
    /// origin).
    ///
    /// Computed during layout in [`crate::layout_text`] / [`crate::measure_text`] from per-glyph outlines
    /// via skrifa. It is a closer fit to the true visual boundary than parley's
    /// `LineMetrics` (which carry ascent / descent design margins), and is used for text
    /// vertical centering (e.g. `visual_height`).
    pub fn ink_bounds(&self) -> Rect {
        self.ink_bounds
    }
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
