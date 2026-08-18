//! 字形轮廓（glyph outline）提取。
//!
//! 提供从字体字节 + 集合索引 + 字形 ID 还原贝塞尔路径（[`kurbo::BezPath`]）的能力，
//! 供 PDF / DOCX 等需要**精确矢量字形**（而非光栅化或系统字体回退）的后端消费。
//!
//! 本模块是"从 [`text::TextRun`] 提取数据"的第二个真实需求（第一个是字体字节 +
//! [`text::TextRun::font_index`]），是 SVG 文本矢量保真（README 规划项）的前提。
//!
//! 内部经 [`skrifa`]（parley / fontique 同源的字体缩放引擎）实现：
//! 1. [`skrifa::FontRef::from_index`] 从字节 + 集合索引定位到具体字体（支持 ttc/otc 集合，
//!    也支持单字体文件）；
//! 2. `outline_glyphs().get(glyph_id)` 取回 [`skrifa::OutlineGlyph`]；
//! 3. 以 `font_size`（ppem）缩放，把轮廓绘制进 [`kurbo::BezPath`]。
//!
//! ## 坐标约定
//! 输出路径坐标为 **y-down**（与 lievisual 其余几何一致，Canvas 约定）：字形轮廓在字体
//! 坐标系中为 y-up，本模块在绘制笔内做 `y → -y` 翻转。字形原点在**基线**，路径已按
//! `font_size` 缩放到像素（`ppem`）。消费方将其叠加到文本基线位置即可。

use kurbo::BezPath;
use skrifa::outline::{DrawSettings, OutlinePen};
use skrifa::prelude::{FontRef, LocationRef, MetadataProvider, Size};

/// 字形轮廓查询。
///
/// 以字体字节 + 集合索引定位字体，再按字形 ID 取出缩放到 `font_size` 的贝塞尔路径。
/// 纯函数、无内部状态；每次调用都会重建轮廓集合（字形数较多时，消费方应自行做轮廓缓存）。
pub struct GlyphOutline;

impl GlyphOutline {
    /// 取回单个字形的轮廓路径。
    ///
    /// - `font_data`：字体文件原始字节（含 ttf/otf 单字体或 ttc/otc 集合）。
    /// - `font_index`：字体在集合中的索引（单字体恒为 0；对应 [`text::TextRun::font_index`]）。
    /// - `glyph_id`：字形 ID（对应 [`text::Glyph::id`]）。
    /// - `font_size`：缩放目标（ppem/像素；对应 [`text::TextRun::font_size`]）。
    ///
    /// 返回 `None` 表示：字体字节无法解析、集合索引越界，或该字形无轮廓
    /// （如 .notdef 或纯位图字形）。
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
}

/// 把 skrifa 的路径命令收集为 [`kurbo::BezPath`]，并翻转 y 轴为 y-down。
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

    /// 布局一个字形，取回其 font_data / font_index / id，经 `GlyphOutline::outline`
    /// 还原轮廓，断言路径非空。
    #[test]
    fn outline_extracts_glyph_path() {
        let style = TextStyle::new(Color::BLACK, 40.0, "sans-serif");
        let layout = layout_text(std::slice::from_ref(&RichSpan::new("A", style)), None);
        let run = &layout.lines[0].runs[0];
        let glyph = &run.glyphs[0];

        let path = GlyphOutline::outline(&run.font_data, run.font_index, glyph.id, run.font_size)
            .expect("sans-serif 的 'A' 应有矢量轮廓");

        // 非空路径：至少含一次 move_to 开头的子路径。
        assert!(
            path.elements().len() >= 2,
            "轮廓路径应至少含 move_to + 至少一个线段，got {} elements",
            path.elements().len()
        );
        // y-down 翻转：字形原点在基线，绝大多数字形轮廓点应位于基线下方（y 为正）。
        // 不强制所有点都正（如无下伸量字形），仅断言路径整体范围有限且非空。
        let bbox = path.bounding_box();
        assert!(
            bbox.width() > 0.0 || bbox.height() > 0.0,
            "轮廓应有非零包围盒"
        );
    }

    /// 无效字体 / 越界集合索引应返回 `None`，不 panic。
    #[test]
    fn outline_handles_invalid_input() {
        assert!(GlyphOutline::outline(&[0u8; 8], 0, 0, 12.0).is_none());
        // 合法字体但集合索引越界（单字体文件的 index 必须为 0）。
        let style = TextStyle::new(Color::BLACK, 40.0, "sans-serif");
        let layout = layout_text(std::slice::from_ref(&RichSpan::new("A", style)), None);
        let run = &layout.lines[0].runs[0];
        assert!(
            GlyphOutline::outline(&run.font_data, 99, run.glyphs[0].id, run.font_size).is_none(),
            "单字体文件的越界集合索引应返回 None"
        );
    }
}
