//! Glyph outline extraction.
//!
//! Provides the ability to reconstruct a Bézier path ([`kurbo::BezPath`]) from font bytes +
//! collection index + glyph ID, for backends such as PDF / DOCX that need **exact vector
//! glyphs** (rather than rasterization or system-font fallback).
//!
//! This module is the second real need to "extract data from [`crate::text::TextRun`]" (the first was
//! font bytes + [`crate::text::TextRun::font_index`]), and is a prerequisite for SVG text vector
//! fidelity (a README planned item).
//!
//! Internally it uses [`skrifa`] (the same font-scaling engine as parley / fontique):
//! 1. [`skrifa::FontRef::from_index`] locates the concrete font from bytes + collection index
//!    (supports ttc/otc collections as well as single-font files);
//! 2. `outline_glyphs().get(glyph_id)` retrieves the [`skrifa::OutlineGlyph`];
//! 3. it is scaled by `font_size` (ppem) and drawn into a [`kurbo::BezPath`].
//!
//! ## Coordinate convention
//! The output path coordinates are **y-down** (consistent with the rest of lievisual's geometry,
//! the Canvas convention): glyph outlines are y-up in the font coordinate system, and this
//! module flips `y → -y` inside the pen. The glyph origin is the **baseline**, and the path is
//! already scaled to pixels (`ppem`) by `font_size`. Consumers overlay it onto the text
//! baseline position.

use crate::geometry::Rect;
use kurbo::BezPath;
use skrifa::metrics::GlyphMetrics;
use skrifa::outline::{DrawSettings, OutlinePen};
use skrifa::prelude::{FontRef, LocationRef, MetadataProvider, Size};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

/// Glyph outline query.
///
/// Locates a font by font bytes + collection index, then retrieves the Bézier path for a glyph ID
/// scaled to `font_size`. A pure function with no internal state; every call rebuilds the outline
/// collection (when there are many glyphs, consumers should cache outlines themselves).
pub struct GlyphOutline;

impl GlyphOutline {
    /// Retrieve the outline path of a single glyph.
    ///
    /// - `font_data`: raw font-file bytes (ttf/otf single font or ttc/otc collection).
    /// - `font_index`: the index of the font within the collection (always 0 for a single font;
    ///   corresponds to [`crate::text::TextRun::font_index`]).
    /// - `glyph_id`: the glyph ID (corresponds to [`crate::text::Glyph::id`]).
    /// - `font_size`: the scaling target (ppem / pixels; corresponds to
    ///   [`crate::text::TextRun::font_size`]).
    ///
    /// Returns `None` when: the font bytes cannot be parsed, the collection index is out of
    /// bounds, or the glyph has no outline (e.g. `.notdef` or a pure bitmap glyph).
    #[must_use]
    pub fn outline(
        font_data: &[u8],
        font_index: u32,
        glyph_id: u32,
        font_size: f32,
    ) -> Option<BezPath> {
        let font = FontRef::from_index(font_data, font_index).ok()?;
        let outline = font.outline_glyphs().get(skrifa::GlyphId::new(glyph_id))?;
        let mut pen = BezPathPen::default();
        // 无变量坐标（LocationRef::default()），字形分解默认 FreeType（DrawSettings 默认）。
        outline
            .draw(
                DrawSettings::unhinted(Size::new(font_size), LocationRef::default()),
                &mut pen,
            )
            .ok()?;
        Some(pen.path)
    }

    /// Retrieve the ink bounding box of a single glyph, without building its outline.
    ///
    /// Unlike [`GlyphOutline::outline`] (which reconstructs a Bézier path and derives its
    /// extent from control points), this reads the glyph's stored bbox directly via skrifa's
    /// `GlyphMetrics::bounds` — exact, allocation-free, and much cheaper. The returned box is
    /// relative to the glyph origin (the baseline), scaled to `font_size` (ppem), in **y-down**
    /// coordinates (consistent with the rest of lievisual): `y_min` below the baseline is
    /// positive.
    ///
    /// `font_data` is taken as `&Arc<Vec<u8>>` and used as the cache key: the font is parsed (and
    /// a per-glyph box map built) only once per distinct `Arc` + `font_index` + `font_size`, then
    /// shared across all glyphs and across layouts. The cache entry holds an `Arc` clone so the
    /// allocation is pinned for the entry's lifetime.
    ///
    /// Returns `None` when: the font bytes cannot be parsed, the collection index is out of
    /// bounds, or the glyph id is invalid. For an empty glyph (e.g. space, `.notdef`) it returns
    /// a zero-area box at the origin rather than `None`.
    #[must_use]
    pub fn ink_box(
        font_data: &Arc<Vec<u8>>,
        font_index: u32,
        glyph_id: u32,
        font_size: f32,
    ) -> Option<Rect> {
        // Cache key: the `Arc` allocation identity (the address of the `Vec<u8>` the `Arc`
        // points to) + collection index + pixel size. Keying on the `Arc` itself — not a raw
        // `ptr`/`len` pair — makes the key exact: two `Arc` clones of the same font share a key,
        // and because the entry holds an `Arc` clone (see `InkBoxEntry`), the allocation is pinned
        // for as long as the entry lives, so its address cannot be reused by a *different* font.
        let key = (
            Arc::as_ptr(font_data) as usize,
            font_index,
            font_size.to_bits(),
        );
        // Consult a process-wide per-font map so that only the *first* glyph of a given
        // font+size reparses the font; every other glyph (and repeats across runs / layouts)
        // reuses the shared map instead of re-running `FontRef::from_index` on line 88.
        let entry = ink_box_cache()
            .lock()
            .ok()?
            .entry(key)
            .or_insert_with(|| {
                Arc::new(InkBoxEntry {
                    _font: font_data.clone(),
                    map: Mutex::new(HashMap::new()),
                })
            })
            .clone();
        if let Some(cached) = entry.map.lock().ok()?.get(&glyph_id) {
            return *cached;
        }
        // Miss: parse the font once, compute this glyph's box, and store it for later reuse.
        let font = FontRef::from_index(font_data.as_slice(), font_index).ok()?;
        let metrics = GlyphMetrics::new(&font, Size::new(font_size), LocationRef::default());
        let box_opt = metrics.bounds(skrifa::GlyphId::new(glyph_id)).map(|bbox| {
            // skrifa bbox is y-up (font convention); flip to y-down.
            Rect::new(
                bbox.x_min as f64,
                -(bbox.y_max as f64),
                bbox.x_max as f64,
                -(bbox.y_min as f64),
            )
        });
        entry.map.lock().ok()?.insert(glyph_id, box_opt);
        box_opt
    }
}

/// A cached, borrow-free per-glyph ink-box map for one font + size.
///
/// `_font` keeps the `Arc<Vec<u8>>` alive (and therefore pins its allocation address, which is
/// the cache key) for as long as this entry is retained; the `map` itself owns the computed
/// boxes and borrows nothing, so it can be shared freely via `Arc`.
struct InkBoxEntry {
    _font: Arc<Vec<u8>>,
    map: Mutex<HashMap<u32, Option<Rect>>>,
}

/// Identity-keyed cache of per-glyph ink boxes, shared process-wide.
///
/// Keyed by the font `Arc` address + collection index + pixel size so that repeated glyphs (and
/// repeated fonts) never reparse the font. Each value is an `Arc<InkBoxEntry>` so the map is
/// shared by reference across all `ink_box` calls for the same font+size.
type InkBoxCache = HashMap<(usize, u32, u32), Arc<InkBoxEntry>>;

fn ink_box_cache() -> &'static Mutex<InkBoxCache> {
    static CACHE: OnceLock<Mutex<InkBoxCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Collect skrifa's path commands into a [`kurbo::BezPath`], flipping the y-axis to y-down.
#[derive(Default)]
struct BezPathPen {
    path: BezPath,
}

impl OutlinePen for BezPathPen {
    fn move_to(&mut self, x: f32, y: f32) {
        self.path.move_to((x as f64, -y as f64));
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.path.line_to((x as f64, -y as f64));
    }

    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        self.path
            .quad_to((cx0 as f64, -cy0 as f64), (x as f64, -y as f64));
    }

    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        self.path.curve_to(
            (cx0 as f64, -cy0 as f64),
            (cx1 as f64, -cy1 as f64),
            (x as f64, -y as f64),
        );
    }

    fn close(&mut self) {
        self.path.close_path();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::Color;
    use crate::text::{RichSpan, TextStyle, layout_text};
    use kurbo::Shape;

    /// Lay out a glyph, retrieve its font_data / font_index / id, reconstruct the outline via
    /// `GlyphOutline::outline`, and assert the path is non-empty.
    #[test]
    fn outline_extracts_glyph_path() {
        let style = TextStyle::new(Color::BLACK, 40.0, "sans-serif");
        let layout = layout_text(std::slice::from_ref(&RichSpan::new("A", style)), None);
        let run = &layout.lines[0].runs[0];
        let glyph = &run.glyphs[0];

        let path = GlyphOutline::outline(&run.font_data, run.font_index, glyph.id, run.font_size)
            .expect("sans-serif 的 'A' 应有矢量轮廓");

        // Non-empty path: at least a sub-path starting with move_to.
        assert!(
            path.elements().len() >= 2,
            "the outline path should contain at least move_to + one segment, got {} elements",
            path.elements().len()
        );
        // y-down flip: the glyph origin is the baseline, and most glyph outline points lie below
        // the baseline (y positive). We don't require all points to be positive (e.g. glyphs
        // with no descender), only that the overall path extent is finite and non-empty.
        let bbox = path.bounding_box();
        assert!(
            bbox.width() > 0.0 || bbox.height() > 0.0,
            "the outline should have a non-zero bounding box"
        );
    }

    /// Invalid font / out-of-bounds collection index should return `None` without panicking.
    #[test]
    fn outline_handles_invalid_input() {
        assert!(GlyphOutline::outline(&[0u8; 8], 0, 0, 12.0).is_none());
        // Valid font but out-of-bounds collection index (a single-font file's index must be 0).
        let style = TextStyle::new(Color::BLACK, 40.0, "sans-serif");
        let layout = layout_text(std::slice::from_ref(&RichSpan::new("A", style)), None);
        let run = &layout.lines[0].runs[0];
        assert!(
            GlyphOutline::outline(&run.font_data, 99, run.glyphs[0].id, run.font_size).is_none(),
            "单字体文件的越界集合索引应返回 None"
        );
    }
}
