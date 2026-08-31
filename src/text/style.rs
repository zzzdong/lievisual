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

use crate::geometry::Color;
use crate::text::layout::TextMetrics;

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

/// Fraction of the font ascent at which the hanging baseline sits (Canvas / typographic
/// convention ≈ 0.8). Shared by [`TextMetrics::hanging_baseline`] and [`compute_text_offset`]
/// so the two entry points never drift apart.
pub(crate) const HANGING_ASCENT_RATIO: f64 = 0.8;

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

impl FontStyle {
    /// CSS `font-style` keyword for this variant (`normal`, `italic`, `oblique`).
    pub fn as_str(self) -> &'static str {
        match self {
            FontStyle::Normal => "normal",
            FontStyle::Italic => "italic",
            FontStyle::Oblique => "oblique",
        }
    }

    /// Parse a [`FontStyle`] from a string, accepting the CSS keywords `normal`,
    /// `italic`, `oblique` (case-insensitive, `-`/`_` optional). Returns `None`
    /// if the string is not recognized.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        let t = s.trim().to_ascii_lowercase();
        let t = t.replace(['-', '_'], "");
        match t.as_str() {
            "normal" | "n" => Some(FontStyle::Normal),
            "italic" | "i" => Some(FontStyle::Italic),
            "oblique" | "o" => Some(FontStyle::Oblique),
            _ => None,
        }
    }
}

/// Font weight, analogous to the CSS `font-weight` keyword values.
///
/// Each variant maps to the corresponding numeric weight (100–900) accepted by
/// the canvas `font-weight` property; use [`FontWeight::value`] to obtain the
/// `f32` and [`FontWeight::from_value`] to recover the nearest keyword.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FontWeight {
    /// Thin (100).
    Thin,
    /// Extra light (200).
    ExtraLight,
    /// Light (300).
    Light,
    /// Normal / regular (400, default).
    #[default]
    Normal,
    /// Medium (500).
    Medium,
    /// Semi bold (600).
    SemiBold,
    /// Bold (700).
    Bold,
    /// Extra bold (800).
    ExtraBold,
    /// Black / heavy (900).
    Black,
}

impl FontWeight {
    /// Numeric weight (100–900) as used by the canvas `font-weight` property.
    pub fn value(self) -> f32 {
        match self {
            FontWeight::Thin => 100.0,
            FontWeight::ExtraLight => 200.0,
            FontWeight::Light => 300.0,
            FontWeight::Normal => 400.0,
            FontWeight::Medium => 500.0,
            FontWeight::SemiBold => 600.0,
            FontWeight::Bold => 700.0,
            FontWeight::ExtraBold => 800.0,
            FontWeight::Black => 900.0,
        }
    }

    /// Recover the nearest [`FontWeight`] keyword for a numeric weight (clamped
    /// to the 100–900 range and snapped to the nearest 100-step keyword).
    pub fn from_value(weight: f32) -> Self {
        let step = (weight.clamp(100.0, 900.0) / 100.0).round() as i32 * 100;
        match step {
            100 => FontWeight::Thin,
            200 => FontWeight::ExtraLight,
            300 => FontWeight::Light,
            400 => FontWeight::Normal,
            500 => FontWeight::Medium,
            600 => FontWeight::SemiBold,
            700 => FontWeight::Bold,
            800 => FontWeight::ExtraBold,
            900 => FontWeight::Black,
            _ => FontWeight::Normal,
        }
    }

    /// Parse a [`FontWeight`] from a string, accepting either:
    /// - a keyword: `thin`, `extralight`/`extra-light`, `light`, `normal`,
    ///   `medium`, `semibold`/`semi-bold`, `bold`, `extrabold`/`extra-bold`,
    ///   `black` (case-insensitive, `-`/`_` optional); or
    /// - a numeric weight in the 100–900 range (`100`, `400`, `700`, …).
    ///
    /// Returns `None` if the string is not recognized or the numeric weight is
    /// outside the 100–900 range.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        let t = s.trim().to_ascii_lowercase();
        let t = t.replace(['-', '_'], "");
        match t.as_str() {
            "thin" => Some(FontWeight::Thin),
            "extralight" => Some(FontWeight::ExtraLight),
            "light" => Some(FontWeight::Light),
            "normal" => Some(FontWeight::Normal),
            "medium" => Some(FontWeight::Medium),
            "semibold" => Some(FontWeight::SemiBold),
            "bold" => Some(FontWeight::Bold),
            "extrabold" => Some(FontWeight::ExtraBold),
            "black" => Some(FontWeight::Black),
            _ => {
                let w = t.parse::<f32>().ok()?;
                if !(100.0..=900.0).contains(&w) {
                    return None;
                }
                Some(FontWeight::from_value(w))
            }
        }
    }
}

impl std::str::FromStr for FontWeight {
    type Err = FontWeightParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        FontWeight::parse(s).ok_or(FontWeightParseError)
    }
}

/// Error returned when a string cannot be parsed into a [`FontWeight`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FontWeightParseError;

impl std::fmt::Display for FontWeightParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("invalid font weight")
    }
}

impl std::error::Error for FontWeightParseError {}

/// Font width (stretch), analogous to the CSS `font-stretch` keyword values.
///
/// Each variant maps to a width ratio relative to the normal (unstretched) face,
/// where `1.0` is normal. Use [`FontWidth::ratio`] to obtain the `f32` ratio and
/// [`FontWidth::from_ratio`] to recover the nearest keyword.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FontWidth {
    /// Ultra condensed (0.5×).
    UltraCondensed,
    /// Extra condensed (0.625×).
    ExtraCondensed,
    /// Condensed (0.75×).
    Condensed,
    /// Semi condensed (0.875×).
    SemiCondensed,
    /// Normal / regular width (1.0×, default).
    #[default]
    Normal,
    /// Semi expanded (1.125×).
    SemiExpanded,
    /// Expanded (1.25×).
    Expanded,
    /// Extra expanded (1.5×).
    ExtraExpanded,
    /// Ultra expanded (2.0×).
    UltraExpanded,
}

impl FontWidth {
    /// Width ratio relative to the normal face (`1.0` = normal).
    pub fn ratio(self) -> f32 {
        match self {
            FontWidth::UltraCondensed => 0.5,
            FontWidth::ExtraCondensed => 0.625,
            FontWidth::Condensed => 0.75,
            FontWidth::SemiCondensed => 0.875,
            FontWidth::Normal => 1.0,
            FontWidth::SemiExpanded => 1.125,
            FontWidth::Expanded => 1.25,
            FontWidth::ExtraExpanded => 1.5,
            FontWidth::UltraExpanded => 2.0,
        }
    }

    /// Recover the nearest [`FontWidth`] keyword for a width ratio (clamped to
    /// the 0.5–2.0 range and rounded to the nearest keyword on the log-ish
    /// scale above).
    pub fn from_ratio(ratio: f32) -> Self {
        let r = ratio.clamp(0.5, 2.0);
        if r < 0.5625 {
            FontWidth::UltraCondensed
        } else if r < 0.6875 {
            FontWidth::ExtraCondensed
        } else if r < 0.8125 {
            FontWidth::Condensed
        } else if r < 0.9375 {
            FontWidth::SemiCondensed
        } else if r < 1.0625 {
            FontWidth::Normal
        } else if r < 1.1875 {
            FontWidth::SemiExpanded
        } else if r < 1.375 {
            FontWidth::Expanded
        } else if r < 1.75 {
            FontWidth::ExtraExpanded
        } else {
            FontWidth::UltraExpanded
        }
    }
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
    pub font_weight: FontWeight,
    /// Font style (canvas `font-style`).
    pub font_style: FontStyle,
    /// Font stretch as a ratio (1.0 = normal; 0.75 = condensed; 1.25 = expanded).
    /// `None` uses the font's default (1.0).
    pub font_width: FontWidth,
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
            font_weight: FontWeight::Normal,
            font_style: FontStyle::Normal,
            font_width: FontWidth::Normal,
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

    #[must_use]
    pub fn with_weight(mut self, weight: FontWeight) -> Self {
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
    pub fn with_font_width(mut self, width: FontWidth) -> Self {
        self.font_width = width;
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
/// use lievisual::text::{FontWeight, RichSpan, TextStyle, measure_text};
/// let spans = vec![
///     RichSpan::new("Hello ", TextStyle::new(Color::BLACK, 20.0, "sans-serif")),
///     // mixed styling: bold + underline + strikethrough + letter spacing
///     RichSpan::new("World", TextStyle::new(Color::rgb(0, 0, 0xff), 20.0, "sans-serif")
///         .with_weight(FontWeight::Bold)
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
