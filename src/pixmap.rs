//! Self-contained RGBA8 bitmap (pixmap).
//!
//! Images in lievisual enter the IR as **already-decoded RGBA8 bitmaps**; the rendering
//! library **does not decode** — consistent with the text layer's principle of "the caller
//! lays out, the renderer consumes a self-contained [`crate::text::TextLayout`]". Decoding (PNG
//! parsing or
//! other formats) is the caller's / resource-loading layer's responsibility; lievisual only
//! holds and draws ready pixels.
//!
//! This type is **decoupled** from any specific rendering backend (vello / SVG / …), owning its
//! own `Arc<[u8]>` plus width/height, and each backend consumes it as needed:
//! - Raster backends (vello) take the RGBA8 pixels directly for scaling / blit;
//! - SVG encodes them to PNG via [`Pixmap::to_png`] and embeds the result.
//!
//! ## PNG codec
//! - [`Pixmap::to_png`]: RGBA8 → PNG bytes (for SVG embedding or raster export paths).
//! - [`Pixmap::decode_png`]: PNG bytes → RGBA8 (convenience entry, available by default).
//! - Decoding other formats (jpeg / webp / gif …) requires the `extra-image-codecs`
//!   feature (depends on `image`).
//!
//! ## Coordinate / memory conventions
//! - RGBA8 is tightly packed: `stride = 4 * width`, row-major, starting from the top-left.
//! - No color-space / gamma handling; transparency is plain (non-premultiplied) alpha.

use std::sync::Arc;

/// RGBA8 bitmap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pixmap {
    width: u32,
    height: u32,
    /// RGBA8 pixels; length is always `4 * width * height`.
    pixels: Arc<[u8]>,
}

impl Pixmap {
    /// Construct from already-decoded RGBA8 pixels. The length must be
    /// `4 * width * height`, otherwise `None` is returned.
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

    /// Pixel width.
    #[must_use]
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Pixel height.
    #[must_use]
    pub fn height(&self) -> u32 {
        self.height
    }

    /// RGBA8 pixel slice (`len = 4 * width * height`).
    #[must_use]
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// Encode to PNG bytes (`png` is a permanent dependency: RGBA8 → PNG).
    #[must_use]
    pub fn to_png(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.pixels.len() + 64);
        {
            let mut enc = png::Encoder::new(&mut out, self.width, self.height);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            let mut writer = enc
                .write_header()
                .expect("[lievisual] PNG encoding: write header failed");
            writer
                .write_image_data(&self.pixels)
                .expect("[lievisual] PNG encoding: write image data failed");
        }
        out
    }

    /// Decode PNG bytes into RGBA8. Returns `None` for non-PNG input or decode failure.
    #[must_use]
    pub fn decode_png(bytes: &[u8]) -> Option<Self> {
        // `png` 默认 Transformations::IDENTITY：palette/indexed 图每像素只给一个调色板
        // 索引、低深度灰度仍是 packed bits，`normalize_to_rgba8` 无法还原成真彩色。
        // 显式 EXPAND + STRIP_16 让解码器完成调色板展开、tRNS→alpha、<8bit→8bit、
        // 16bit→8bit，输出恒为 Grayscale(A)/Rgb(A)。
        let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
        decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
        let mut reader = decoder.read_info().ok()?;
        let mut buf = vec![0u8; reader.output_buffer_size()?];
        let info = reader.next_frame(&mut buf).ok()?;
        buf.truncate(info.buffer_size());
        // Normalize to RGBA8: expand to RGBA / strip 16-bit.
        let (color, depth) = reader.output_color_type();
        let (w, h) = (info.width, info.height);
        let rgba = normalize_to_rgba8(buf, color, depth, w, h)?;
        Self::from_rgba8(w, h, rgba)
    }
}

/// Normalize a decoded raw buffer to RGBA8 (handles grayscale / palette / 16-bit /
/// missing-alpha combinations).
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
    // Bytes per pixel after normalization (should always be 8-bit).
    let chan = if depth == png::BitDepth::Eight { 1 } else { 2 };
    if buf.len() < n * bpp * chan {
        return None;
    }
    let mut out = Vec::with_capacity(n * 4);
    for i in 0..n {
        let base = i * bpp * chan;
        // 8-bit: take that channel byte; 16-bit: take the high byte (MSB first).
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
            // decode_png 设置了 EXPAND | STRIP_16，palette 会在解码层展开为 Rgb/Rgba，
            // 此处不可达；保守回退为灰度以免把残留索引当真彩色。
            png::ColorType::Indexed => {
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

    /// 缺陷回归：palette/indexed PNG 必须展开为真彩色，而非把索引当灰度。
    ///
    /// 修复前 `png` 解码器默认 Transformations::IDENTITY，2×2 调色板图解码后
    /// 每个字节是调色板索引，normalize 将其当作灰度值（红→灰），颜色完全错乱；
    /// tRNS（调色板透明度）也一并丢失。修复后在解码层 EXPAND + STRIP_16。
    #[test]
    fn decode_png_expands_palette_and_trns() {
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, 2, 2);
            enc.set_color(png::ColorType::Indexed);
            enc.set_depth(png::BitDepth::Eight);
            // PLTE：entry0 = 红，entry1 = 绿。
            enc.set_palette(vec![0xff, 0x00, 0x00, 0x00, 0xff, 0x00]);
            // tRNS：entry0 不透明，entry1 半透明（128）。
            enc.set_trns(vec![0xff, 0x80]);
            let mut writer = enc.write_header().expect("编码头失败");
            // 每像素一个索引：0,1,1,0。
            writer
                .write_image_data(&[0u8, 1, 1, 0])
                .expect("写入像素失败");
        }
        let pm = Pixmap::decode_png(&out).expect("palette PNG 应能解码");
        let px = pm.pixels();
        assert_eq!(&px[0..4], &[0xff, 0x00, 0x00, 0xff], "索引0 应为不透明红");
        assert_eq!(&px[4..8], &[0x00, 0xff, 0x00, 0x80], "索引1 应为半透明绿");
        assert_eq!(&px[8..12], &[0x00, 0xff, 0x00, 0x80]);
        assert_eq!(&px[12..16], &[0xff, 0x00, 0x00, 0xff]);
    }
}
