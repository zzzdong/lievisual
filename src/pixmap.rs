//! 自包含的 RGBA8 位图（pixmap）。
//!
//! lievisual 的图片统一以**已解码的 RGBA8 位图**进入 IR，渲染库**不做解码**——
//! 与文本层"调用方排版、渲染端消费自闭环 [`TextLayout`]"的原则一致。解码（PNG 解析或
//! 其它格式）是调用方 / 资源加载层的职责；lievisual 只负责持有并绘制就绪的像素。
//!
//! 本类型与具体渲染后端（vello / SVG / …）**解耦**，自有 `Arc<[u8]>` + 宽高，
//! 各后端按需消费：
//! - 栅格后端（vello）直接取 RGBA8 像素做缩放 / blit；
//! - SVG 经 [`Pixmap::to_png`] 编码为 PNG 后内嵌。
//!
//! ## PNG 编解码
//! - [`Pixmap::to_png`]：RGBA8 → PNG 字节（供 SVG 内嵌、`render_png` 导出）。
//! - [`Pixmap::decode_png`]：PNG 字节 → RGBA8（便捷入口，默认可用）。
//! - 其它格式（jpeg / webp / gif …）解码需启用 `extra-image-codecs` feature（依赖 `image`）。
//!
//! ## 坐标 / 内存约定
//! - RGBA8 紧排：`stride = 4 * width`，行优先，从左上角开始。
//! - 无颜色空间/伽马处理；透明为常规 alpha（未预乘）。

use std::sync::Arc;

/// RGBA8 位图。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pixmap {
    width: u32,
    height: u32,
    /// RGBA8 像素，长度恒为 `4 * width * height`。
    pixels: Arc<[u8]>,
}

impl Pixmap {
    /// 由已解码的 RGBA8 像素构造。长度必须为 `4 * width * height`，否则返回 `None`。
    #[must_use]
    pub fn from_rgba8(width: u32, height: u32, pixels: impl Into<Arc<[u8]>>) -> Option<Self> {
        let pixels = pixels.into();
        let expect = (width as usize)
            .checked_mul(height as usize)?
            .checked_mul(4)?;
        if pixels.len() != expect {
            return None;
        }
        Some(Self {
            width,
            height,
            pixels,
        })
    }

    /// 像素宽。
    #[must_use]
    pub fn width(&self) -> u32 {
        self.width
    }

    /// 像素高。
    #[must_use]
    pub fn height(&self) -> u32 {
        self.height
    }

    /// RGBA8 像素切片（`len = 4 * width * height`）。
    #[must_use]
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// 编码为 PNG 字节（`png` 常驻依赖，RGBA8 → PNG）。
    #[must_use]
    pub fn to_png(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.pixels.len() + 64);
        {
            let mut enc = png::Encoder::new(&mut out, self.width, self.height);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            let mut writer = enc
                .write_header()
                .expect("[lievisual] PNG 编码：写入头失败");
            writer
                .write_image_data(&self.pixels)
                .expect("[lievisual] PNG 编码：写入像素失败");
        }
        out
    }

    /// 从 PNG 字节解码为 RGBA8。非 PNG / 解码失败返回 `None`。
    #[must_use]
    pub fn decode_png(bytes: &[u8]) -> Option<Self> {
        let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
        let mut reader = decoder.read_info().ok()?;
        let mut buf = vec![0u8; reader.output_buffer_size()?];
        let info = reader.next_frame(&mut buf).ok()?;
        buf.truncate(info.buffer_size());
        // 归一化为 RGBA8：expand 到 RGBA / 剥 16bit。
        let (color, depth) = reader.output_color_type();
        let (w, h) = (info.width, info.height);
        let rgba = normalize_to_rgba8(buf, color, depth, w, h)?;
        Self::from_rgba8(w, h, rgba)
    }
}

/// 把解码原始缓冲归一化为 RGBA8（处理灰度/调色板/16bit/缺 alpha 等组合）。
fn normalize_to_rgba8(
    buf: Vec<u8>,
    color: png::ColorType,
    depth: png::BitDepth,
    width: u32,
    height: u32,
) -> Option<Vec<u8>> {
    let n = (width as usize) * (height as usize);
    let bpp = match color {
        png::ColorType::Grayscale => 1usize,
        png::ColorType::Rgb => 3,
        png::ColorType::Indexed => 1,
        png::ColorType::GrayscaleAlpha => 2,
        png::ColorType::Rgba => 4,
    };
    // 8bit（normalize 后应恒为 8bit）每像素字节数。
    let chan = if depth == png::BitDepth::Eight { 1 } else { 2 };
    if buf.len() < n * bpp * chan {
        return None;
    }
    let mut out = Vec::with_capacity(n * 4);
    for i in 0..n {
        let base = i * bpp * chan;
        // 8bit 取该通道字节；16bit 取高字节（MSB 在前）。
        let byte = |idx: usize| buf[base + idx * chan];
        let (r, g, b, a) = match color {
            png::ColorType::Grayscale => {
                let v = byte(0);
                (v, v, v, 255)
            }
            png::ColorType::GrayscaleAlpha => {
                let v = byte(0);
                (v, v, v, byte(1))
            }
            png::ColorType::Rgb => (byte(0), byte(1), byte(2), 255),
            png::ColorType::Rgba => (byte(0), byte(1), byte(2), byte(3)),
            png::ColorType::Indexed => {
                // `normalize_to_color8` 的 EXPAND 应已展开调色板；理论上不到这里。
                // 保守：视为灰度（避免错误读索引）。
                let v = byte(0);
                (v, v, v, 255)
            }
        };
        out.extend_from_slice(&[r, g, b, a]);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_rgba8_validates_length() {
        assert!(Pixmap::from_rgba8(2, 2, vec![0u8; 16]).is_some());
        assert!(Pixmap::from_rgba8(2, 2, vec![0u8; 15]).is_none());
        assert!(Pixmap::from_rgba8(0, 0, vec![] as Vec<u8>).is_some());
    }

    #[test]
    fn png_roundtrip() {
        let pixels: Vec<u8> = (0..4u32 * 3 * 3).map(|i| (i % 251) as u8).collect();
        let pm = Pixmap::from_rgba8(3, 3, pixels).unwrap();
        let png = pm.to_png();
        let back = Pixmap::decode_png(&png).unwrap();
        assert_eq!(back.width(), 3);
        assert_eq!(back.height(), 3);
        assert_eq!(back.pixels(), pm.pixels());
    }

    #[test]
    fn decode_png_rejects_garbage() {
        assert!(Pixmap::decode_png(b"not a png").is_none());
    }
}
