//! Text layout support (modeled after the Web Canvas text API).
//!
//! Maps onto the Web Canvas `CanvasRenderingContext2D` text API:
//! - [`TextStyle`] corresponds to `ctx.font` (`font-style` / `font-weight` / `font-size` /
//!   `line-height` / `font-family`) plus `textAlign` and `textBaseline`.
//! - [`TextAlign`] corresponds to `ctx.textAlign`: `position.x` is the horizontal anchor.
//! - [`TextBaseline`] corresponds to `ctx.textBaseline`: `position.y` is the vertical anchor.
//! - [`measure_text`] corresponds to `ctx.measureText()`, returning [`TextMeasure`]
//!   (which embeds [`TextMetrics`], i.e. the canvas `TextMetrics` object).
//!
//! Text content is uniformly expressed as **styled spans** ([`RichSpan`] = text + style):
//! a plain string is just a single span, and rich text is several spans concatenated in
//! order. Layout and measurement both flow through the **span-list** entry points
//! [`measure_text`] / [`layout_text`]; each span can independently set its font family /
//! size / weight / style / color / line height.
//! - [`crate::scene::Element::Text`]'s `position` has the same semantics as canvas
//!   `fillText(x, y)`: `x` is driven by `align` and `y` by `baseline`
//!   (default `Alphabetic`, i.e. `y` is the baseline position).
//!
//! ## Anchor semantics
//! Horizontal anchor ([`TextAlign`]):
//! - [`TextAlign::Left`]: `position.x` sits at the left edge of the text block;
//! - [`TextAlign::Center`]: `position.x` sits at the horizontal center of the text block;
//! - [`TextAlign::Right`]: `position.x` sits at the right edge of the text block.
//!
//! Vertical anchor ([`TextBaseline`], canvas semantics):
//! - [`TextBaseline::Top`]: `position.y` sits at the top of the topmost line;
//! - [`TextBaseline::Hanging`]: the hanging baseline (~0.8 × ascent);
//! - [`TextBaseline::Middle`]: the vertical center of the em box;
//! - [`TextBaseline::Alphabetic`]: the alphabetic baseline (default, `position.y` is the baseline);
//! - [`TextBaseline::Ideographic`]: the CJK baseline (~0.1 × descent below the baseline);
//! - [`TextBaseline::Bottom`]: the bottom of the bottommost line.
//!
//! Metrics and baseline values are all `f64` (consistent with the IR geometry) and are
//! provided by [`TextMetrics`].
//!
//! ## Layout context caching
//! parley's `FontContext` / `LayoutContext` are designed to be "one per application /
//! per thread": the former caches the font library and source data (LRU), the latter
//! reuses scratch allocations and glyph caches. **Every parley-related operation** in this
//! crate (measurement via [`measure_text`] and internal layout) holds these two contexts in
//! a **thread-local** manner: lazily initialized on first use per thread, then the same
//! instance is reused for all layout within that thread. This avoids repeated system-font
//! scanning and allocation, and each thread holds its own instance so no synchronization
//! is needed.

use crate::geometry::{Color, Rect, Size};
use std::sync::Arc;

/// Horizontal text alignment (`position.x` is the anchor). Maps to Canvas `textAlign`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TextAlign {
    /// Anchor at the left edge of the text block (default).
    #[default]
    Left,
    /// Anchor at the horizontal center of the text block.
    Center,
    /// Anchor at the right edge of the text block.
    Right,
    /// Justify (last line is left-aligned). Implemented by parley by adjusting letter
    /// spacing during layout.
    Justify,
}

/// Vertical text anchor (`position.y` is the anchor). Maps to Canvas `textBaseline`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TextBaseline {
    /// Anchor at the top of the topmost line.
    Top,
    /// Anchor at the hanging baseline (~0.8 × ascent).
    Hanging,
    /// Anchor at the alphabetic baseline (default, `position.y` is the baseline).
    #[default]
    Alphabetic,
    /// Anchor at the vertical center of the em box.
    Middle,
    /// Anchor at the CJK baseline (~0.1 × descent below the baseline).
    Ideographic,
    /// Anchor at the bottom of the bottommost line.
    Bottom,
}

impl TextBaseline {
    /// Offset from the top of the text block to the anchor (positive downward, y-down
    /// coordinates).
    pub(crate) fn anchor_offset(&self, m: &TextMetrics) -> f64 {
        match self {
            TextBaseline::Top => 0.0,
            TextBaseline::Hanging => m.hanging_baseline,
            TextBaseline::Alphabetic => m.alphabetic_baseline,
            TextBaseline::Middle => m.alphabetic_baseline - m.em_height_ascent * 0.5,
            TextBaseline::Ideographic => m.ideographic_baseline,
            TextBaseline::Bottom => m.height,
        }
    }
}

/// Font style. Maps to Canvas `ctx.font`'s `font-style`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FontStyle {
    /// Normal style (default).
    #[default]
    Normal,
    /// Italic style.
    Italic,
    /// Oblique style.
    Oblique,
}

/// Text style. Maps to Canvas `ctx.font` + `textAlign` + `textBaseline`.
#[derive(Debug, Clone, PartialEq)]
pub struct TextStyle {
    /// Text color (in canvas this is usually set via `fillStyle`; folded into the style here).
    pub color: Color,
    /// Font family, e.g. `"sans-serif"` or `"Helvetica, Arial, sans-serif"` (canvas
    /// `font-family`).
    pub font_family: String,
    /// Font size in px (canvas `font-size`).
    pub font_size: f64,
    /// Font weight, 100–900 (canvas `font-weight`), default 400.
    pub font_weight: f32,
    /// Font style (canvas `font-style`).
    pub font_style: FontStyle,
    /// Font stretch as a ratio (1.0 = normal; 0.75 = condensed; 1.25 = expanded).
    /// `None` uses the font's default (1.0).
    pub font_width: Option<f32>,
    /// Line height in px. `None` uses the font's default line height (as when canvas
    /// `line-height` is omitted).
    pub line_height: Option<f64>,
    /// Letter spacing in px (`letter-spacing`). `0.0` means no extra spacing.
    pub letter_spacing: f64,
    /// Whether to draw an underline (`text-decoration: underline`).
    pub underline: bool,
    /// Underline color. `None` uses the foreground color.
    pub underline_color: Option<Color>,
    /// Whether to draw a strikethrough (`text-decoration: line-through`).
    pub strikethrough: bool,
    /// Strikethrough color. `None` uses the foreground color.
    pub strikethrough_color: Option<Color>,
    /// Baseline shift in px (positive = up / superscript, negative = down / subscript).
    ///
    /// parley has no rise attribute; lievisual implements this by translating the whole
    /// text block along y at render time.
    pub baseline_shift: f64,
    /// Text background color (inline highlight / inline-code gray background). `None` means
    /// no background.
    ///
    /// parley has no background attribute; lievisual draws a text-block rectangle at render
    /// time.
    pub background_color: Option<Color>,
    /// Hyperlink URL (`None` means no link). A lievisual extension (analogous to pango's
    /// `link` attribute).
    pub url: Option<String>,
    /// Rotation in radians (around the text anchor). A lievisual extension; canvas would
    /// need `ctx.translate/rotate`.
    pub rotation: f64,
    /// Maximum width in px: when `Some`, the text wraps past this width (an approximation
    /// of canvas `fillText`'s `maxWidth`, but canvas compresses text whereas here it wraps).
    pub max_width: Option<f64>,
    /// Horizontal anchor (canvas `textAlign`).
    pub align: TextAlign,
    /// Vertical anchor (canvas `textBaseline`).
    pub baseline: TextBaseline,
}

impl TextStyle {
    /// Convenience constructor: color + font size + font family, with everything else at
    /// their defaults (weight 400, normal style, alphabetic baseline, left align).
    #[must_use]
    pub fn new(color: Color, font_size: f64, font_family: impl Into<String>) -> Self {
        Self {
            color,
            font_family: font_family.into(),
            font_size,
            font_weight: 400.0,
            font_style: FontStyle::Normal,
            font_width: None,
            line_height: None,
            letter_spacing: 0.0,
            underline: false,
            underline_color: None,
            strikethrough: false,
            strikethrough_color: None,
            baseline_shift: 0.0,
            background_color: None,
            url: None,
            rotation: 0.0,
            max_width: None,
            align: TextAlign::Left,
            baseline: TextBaseline::Alphabetic,
        }
    }

    #[must_use]
    pub fn with_align(mut self, align: TextAlign) -> Self {
        self.align = align;
        self
    }

    #[must_use]
    pub fn with_baseline(mut self, baseline: TextBaseline) -> Self {
        self.baseline = baseline;
        self
    }

    /// Font weight (100–900).
    #[must_use]
    pub fn with_weight(mut self, weight: f32) -> Self {
        self.font_weight = weight;
        self
    }

    #[must_use]
    pub fn with_style(mut self, style: FontStyle) -> Self {
        self.font_style = style;
        self
    }

    /// Font stretch as a ratio (1.0 = normal, 0.75 = condensed, 1.25 = expanded).
    #[must_use]
    pub fn with_font_width(mut self, ratio: f32) -> Self {
        self.font_width = Some(ratio);
        self
    }

    /// Letter spacing in px (`letter-spacing`).
    #[must_use]
    pub fn with_letter_spacing(mut self, spacing: f64) -> Self {
        self.letter_spacing = spacing;
        self
    }

    /// Whether to draw an underline.
    #[must_use]
    pub fn with_underline(mut self, underline: bool) -> Self {
        self.underline = underline;
        self
    }

    /// Underline color (`None` uses the foreground color).
    #[must_use]
    pub fn with_underline_color(mut self, color: Option<Color>) -> Self {
        self.underline_color = color;
        self
    }

    /// Whether to draw a strikethrough.
    #[must_use]
    pub fn with_strikethrough(mut self, strikethrough: bool) -> Self {
        self.strikethrough = strikethrough;
        self
    }

    /// Strikethrough color (`None` uses the foreground color).
    #[must_use]
    pub fn with_strikethrough_color(mut self, color: Option<Color>) -> Self {
        self.strikethrough_color = color;
        self
    }

    /// Baseline shift in px (positive = up / superscript, negative = down / subscript).
    #[must_use]
    pub fn with_baseline_shift(mut self, shift: f64) -> Self {
        self.baseline_shift = shift;
        self
    }

    /// Text background color (inline highlight / inline-code gray background). `None` means
    /// no background.
    #[must_use]
    pub fn with_background_color(mut self, color: Option<Color>) -> Self {
        self.background_color = color;
        self
    }

    /// Hyperlink URL (`None` means no link).
    #[must_use]
    pub fn with_url(mut self, url: Option<String>) -> Self {
        self.url = url;
        self
    }

    /// Line height in px.
    #[must_use]
    pub fn with_line_height(mut self, height: f64) -> Self {
        self.line_height = Some(height);
        self
    }

    #[must_use]
    pub fn with_rotation(mut self, rotation: f64) -> Self {
        self.rotation = rotation;
        self
    }

    #[must_use]
    pub fn with_max_width(mut self, width: f64) -> Self {
        self.max_width = Some(width);
        self
    }

    /// Returns a value conforming to CSS / SVG `font-family` syntax (a comma-separated list).
    ///
    /// The `font_family` field is a human-readable raw form (it may contain spaces, quotes,
    /// and generic families). This method normalizes each family name: generic families
    /// (such as `sans-serif`) and valid CSS identifiers are left unquoted; names containing
    /// spaces / non-ASCII / special characters / global keywords are wrapped in single
    /// quotes with CSS string escaping applied.
    ///
    /// The output carries **no XML escaping** (the SVG backend applies `escape_attr`
    /// later). Responsibilities are layered:
    /// - CSS layer (this method): decides whether to quote, and applies `\'` / `\\` escaping;
    /// - XML layer (`escape_attr`): turns `&` into `&amp;`, `"` into `&quot;`, etc.
    ///
    /// Example: `"Helvetica Neue, Arial, sans-serif"` → `'Helvetica Neue', Arial, sans-serif`
    #[must_use]
    pub fn font_family_css(&self) -> String {
        normalize_font_family_list(&self.font_family)
    }
}

/// A span of rich text (a styled text fragment).
///
/// Rich text is formed by concatenating [`RichSpan`]s in order, where each span may carry
/// its own style: font family / size / weight / style / stretch / color / line height /
/// letter spacing / underline / strikethrough (corresponding to pango's
/// family / size / weight / style / stretch / foreground / underline /
/// strikethrough / letter_spacing). A `\n` inside a span's text is treated as a line break
/// by parley (consistent with the layout semantics of [`TextStyle`]).
///
/// Usage pattern (layout / measurement both go through the span-list entry point):
/// ```
/// use lievisual::geometry::Color;
/// use lievisual::text::{RichSpan, TextStyle, measure_text};
/// let spans = vec![
///     RichSpan::new("Hello ", TextStyle::new(Color::BLACK, 20.0, "sans-serif")),
///     // mixed styling: bold + underline + strikethrough + letter spacing
///     RichSpan::new("World", TextStyle::new(Color::rgb(0, 0, 0xff), 20.0, "sans-serif")
///         .with_weight(700.0)
///         .with_underline(true)
///         .with_strikethrough(true)
///         .with_letter_spacing(2.0)),
/// ];
/// let m = measure_text(&spans, None);
/// assert!(m.size.width > 0.0);
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct RichSpan {
    /// The span's text (may contain `\n`).
    pub text: String,
    /// The style applied to this span.
    pub style: TextStyle,
}

impl RichSpan {
    /// Construct a styled span.
    #[must_use]
    pub fn new(text: impl Into<String>, style: TextStyle) -> Self {
        Self {
            text: text.into(),
            style,
        }
    }
}

/// Normalize a `font-family` list (comma-separated → each item normalized independently).
fn normalize_font_family_list(raw: &str) -> String {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(normalize_family)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Normalize a single family name: return the correct form (quoted or unquoted), without
/// any XML escaping.
fn normalize_family(raw: &str) -> String {
    let name = strip_family_quotes(raw);
    if is_generic_family(name) || is_css_ident(name) {
        name.to_string()
    } else {
        // CSS string: wrap in single quotes and escape backslashes and single quotes.
        format!("'{}'", name.replace('\\', "\\\\").replace('\'', "\\'"))
    }
}

/// Strip user-supplied single / double quotes (when paired).
fn strip_family_quotes(s: &str) -> &str {
    if (s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\'')) {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

/// CSS generic-family keywords (quoting them would disable them, so they must be emitted
/// verbatim).
fn is_generic_family(s: &str) -> bool {
    matches!(
        s.to_ascii_lowercase().as_str(),
        "serif"
            | "sans-serif"
            | "monospace"
            | "cursive"
            | "fantasy"
            | "system-ui"
            | "ui-serif"
            | "ui-sans-serif"
            | "ui-monospace"
            | "ui-rounded"
            | "emoji"
            | "math"
            | "fangsong"
    )
}

/// Whether the name is a CSS custom identifier (`<custom-ident>`) that can be emitted
/// unquoted. CSS global keywords are excluded so they are not interpreted as keywords
/// instead of family names.
fn is_css_ident(s: &str) -> bool {
    !s.is_empty()
        && !matches!(
            s,
            "inherit" | "initial" | "unset" | "revert" | "revert-layer" | "default"
        )
        && s.chars().enumerate().all(|(i, c)| {
            if i == 0 {
                c.is_ascii_alphabetic() || c == '_' || c == '-'
            } else {
                c.is_ascii_alphanumeric() || c == '_' || c == '-'
            }
        })
}

// ---------------------------------------------------------------------------
// Glyph-level layout results (self-contained, consumable independently of parley)
// ---------------------------------------------------------------------------

/// Text decoration (underline / strikethrough). Maps to pango's `underline` /
/// `strikethrough` attributes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TextDecoration {
    /// No decoration.
    #[default]
    None,
    /// Underline.
    Underline,
    /// Strikethrough.
    LineThrough,
}

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
/// Produced by [`layout_text`] / [`measure_text`], it is **self-contained**: it does not
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
    /// Computed during layout in [`layout_text`] / [`measure_text`] from per-glyph outlines
    /// via skrifa. It is a closer fit to the true visual boundary than parley's
    /// `LineMetrics` (which carry ascent / descent design margins), and is used for text
    /// vertical centering (e.g. `visual_height`).
    pub fn ink_bounds(&self) -> Rect {
        self.ink_bounds
    }
}

pub use measure::{
    FontSource, compute_text_offset, layout_metrics, layout_text, measure_text,
    parse_generic_family, register_font, register_font_generic,
};

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

mod measure {
    use super::*;
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

    // One cached layout context per thread: lazily initialized, reused by every
    // measurement / layout within the thread. parley's official guidance designs
    // FontContext (font library + LRU source cache) and LayoutContext (scratch + glyph
    // cache) as "one per application / per thread"; thread_local realizes that semantics and
    // avoids the system-font scanning and allocation of rebuilding each time. Every
    // parley-related operation goes through with_cached_context to reuse this same pair of
    // instances; threads are independent (separate instances, no synchronization needed).
    thread_local! {
        static FONT_CONTEXT: RefCell<FontContext> = RefCell::new(FontContext::new());
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
            style.font_weight,
        )));
        builder.push_default(StyleProperty::FontStyle(parley_font_style(
            style.font_style,
        )));
        if let Some(w) = style.font_width {
            builder.push_default(StyleProperty::FontWidth(
                parley::style::FontWidth::from_ratio(w),
            ));
        }
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
        if (s.font_weight - base.font_weight).abs() > f32::EPSILON {
            builder.push(
                StyleProperty::FontWeight(parley::style::FontWeight::new(s.font_weight)),
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
            if let Some(w) = s.font_width {
                builder.push(
                    StyleProperty::FontWidth(parley::style::FontWidth::from_ratio(w)),
                    range.clone(),
                );
            } else {
                // Explicitly fall back to the default normal width.
                builder.push(
                    StyleProperty::FontWidth(parley::style::FontWidth::from_ratio(1.0)),
                    range.clone(),
                );
            }
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
            hanging_baseline: ascent * 0.8,
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
                        let bbox = crate::glyph::GlyphOutline::ink_box(
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

    /// Register a custom font into the global font context.
    ///
    /// Once loaded, the font can be referenced by its `font_family` name in text styles.
    /// Because all layout goes through this crate's thread-local context, registering once
    /// is shared by both measurement and rendering.
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

        with_cached_context(|font_cx, _| {
            let registered = font_cx.collection.register_fonts(data, override_info);
            if let Some(generic) = generic_family {
                let ids: Vec<_> = registered.into_iter().map(|(id, _)| id).collect();
                font_cx
                    .collection
                    .append_generic_families(generic, ids.into_iter());
            }
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
                let first_line = layout.lines.first();
                if let Some(line) = first_line {
                    -line.metrics.ascent as f64
                } else {
                    -layout_height * 0.8
                }
            }
            // Hanging / Ideographic rely on the first line's font metrics (in y-down, the
            // anchor sits below the baseline, hence the negative sign). This matches the
            // convention of [`TextBaseline::anchor_offset`] / [`layout_metrics`]:
            // Hanging ≈ 0.8 × ascent; Ideographic ≈ baseline + 0.1 × descent.
            TextBaseline::Hanging => {
                let first_line = layout.lines.first();
                if let Some(line) = first_line {
                    -(line.metrics.ascent * 0.8) as f64
                } else {
                    -layout_height * 0.8
                }
            }
            TextBaseline::Ideographic => {
                let first_line = layout.lines.first();
                if let Some(line) = first_line {
                    let m = line.metrics;
                    -(m.baseline + m.descent * 0.1) as f64
                } else {
                    -layout_height * 0.8
                }
            }
        };

        (x_offset, y_offset)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::Color;

    /// Convenience: measure a plain string (the single-span case).
    fn measure_one(text: &str, style: &TextStyle, max_width: Option<f64>) -> TextMeasure {
        measure_text(
            std::slice::from_ref(&RichSpan::new(text, style.clone())),
            max_width,
        )
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
            let (x_off, y_off) =
                compute_text_offset(&layout, TextAlign::Center, TextBaseline::Middle);
            // Anchor (cx, cy) + offset = the text block's top-left corner.
            let cx = 100.0;
            let cy = 50.0;
            let top_left_x = cx + x_off;
            let top_left_y = cy + y_off;
            // Centered: top-left x = cx - half width; vertically centered: top-left y = cy - half
            // height.
            assert!((top_left_x - (cx - layout.width / 2.0)).abs() < 1e-6);
            assert!((top_left_y - (cy - layout.height / 2.0)).abs() < 1e-6);

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
            let condensed = normal.clone().with_font_width(0.75);
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
            assert_eq!(s.font_width, None);
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
    }
}
