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
//! ## Module layout
//!
//! The implementation is split so each concern is a small, focused module (there is no
//! single monolithic `measure` layer):
//! - `style` — style / alignment / baseline enums and [`TextMetrics`].
//! - `layout` — glyph-level layout IR ([`Glyph`], [`TextRun`], [`TextLine`], [`TextLayout`],
//!   [`RichSpan`], [`TextStyle`], [`TextMeasure`]) and font-family normalization.
//! - `engine` — the parley-backed layout engine, font registry, and text-offset math.

mod engine;
pub mod glyph;
mod layout;
mod style;
#[cfg(test)]
mod tests;

pub use engine::{
    FontSource, compute_text_offset, layout_metrics, layout_text, measure_text,
    parse_generic_family, register_font, register_font_generic,
};
pub use glyph::*;
pub use layout::*;
pub use style::*;
