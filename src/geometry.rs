//! 几何与颜色基础类型。
//!
//! 坐标与尺寸统一使用 `f64`，以匹配 kurbo / parley / vello 的几何精度；
//! 渲染后端在落盘时再按各自需要向 `f32`（`vello`）或字符串（`svg`）转换。

use kurbo::{Affine, Rect as KurboRect};

/// 二维点。
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

impl Point {
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
}

impl From<(f64, f64)> for Point {
    fn from((x, y): (f64, f64)) -> Self {
        Self { x, y }
    }
}

/// 二维向量（与 `Point` 区分语义，避免混用）。
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Vec2 {
    pub x: f64,
    pub y: f64,
}

impl Vec2 {
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
}

/// 尺寸。
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Size {
    pub width: f64,
    pub height: f64,
}

impl Size {
    pub const fn new(width: f64, height: f64) -> Self {
        Self { width, height }
    }
}

/// 轴对齐矩形（左上角 + 尺寸）。
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Rect {
    pub fn new(x: f64, y: f64, width: f64, height: f64) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// 几何中心。
    #[must_use]
    pub fn center(&self) -> Point {
        Point::new(self.x + self.width / 2.0, self.y + self.height / 2.0)
    }

    /// 转为 kurbo 矩形（用于后端几何计算）。
    pub fn to_kurbo(&self) -> KurboRect {
        KurboRect::new(self.x, self.y, self.x + self.width, self.y + self.height)
    }

    /// 从 kurbo 矩形构造（左上角 + 宽高）。
    pub fn from_kurbo(r: KurboRect) -> Self {
        Self::new(r.min_x(), r.min_y(), r.width(), r.height())
    }
}

/// 颜色（RGBA，分量范围 0.0–1.0，与 vello / CSS 一致）。
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Color {
    pub r: f64,
    pub g: f64,
    pub b: f64,
    pub a: f64,
}

impl Color {
    pub const fn new(r: f64, g: f64, b: f64, a: f64) -> Self {
        Self { r, g, b, a }
    }

    /// 不透明的 8 位 RGB。
    pub fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self {
            r: r as f64 / 255.0,
            g: g as f64 / 255.0,
            b: b as f64 / 255.0,
            a: 1.0,
        }
    }

    /// 8 位 RGBA。
    pub fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self {
            r: r as f64 / 255.0,
            g: g as f64 / 255.0,
            b: b as f64 / 255.0,
            a: a as f64 / 255.0,
        }
    }

    pub const BLACK: Color = Color::new(0.0, 0.0, 0.0, 1.0);
    pub const WHITE: Color = Color::new(1.0, 1.0, 1.0, 1.0);
    pub const TRANSPARENT: Color = Color::new(0.0, 0.0, 0.0, 0.0);

    /// CSS 十六进制字符串，如 `#0066cc`。
    /// 不透明时输出 `#rrggbb`，含 alpha 时输出 `#rrggbbaa`。
    pub fn to_hex(&self) -> String {
        let r = (self.r.clamp(0.0, 1.0) * 255.0).round() as u8;
        let g = (self.g.clamp(0.0, 1.0) * 255.0).round() as u8;
        let b = (self.b.clamp(0.0, 1.0) * 255.0).round() as u8;
        let a = (self.a.clamp(0.0, 1.0) * 255.0).round() as u8;
        if a == 255 {
            format!("#{:02x}{:02x}{:02x}", r, g, b)
        } else {
            format!("#{:02x}{:02x}{:02x}{:02x}", r, g, b, a)
        }
    }
}

/// 2D 仿射变换，用于 [`crate::scene::SceneNode`] 的局部坐标系。
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Transform {
    pub a: f64,
    pub b: f64,
    pub c: f64,
    pub d: f64,
    pub e: f64,
    pub f: f64,
}

impl Transform {
    pub const IDENTITY: Transform = Transform {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        e: 0.0,
        f: 0.0,
    };

    pub const fn new(a: f64, b: f64, c: f64, d: f64, e: f64, f: f64) -> Self {
        Self { a, b, c, d, e, f }
    }

    /// 平移。
    pub fn translate(x: f64, y: f64) -> Self {
        Self::new(1.0, 0.0, 0.0, 1.0, x, y)
    }

    /// 均匀缩放。
    pub fn scale(s: f64) -> Self {
        Self::new(s, 0.0, 0.0, s, 0.0, 0.0)
    }

    /// 绕原点旋转（弧度）。
    pub fn rotate(angle: f64) -> Self {
        let (s, c) = angle.sin_cos();
        Self::new(c, s, -s, c, 0.0, 0.0)
    }

    /// 复合：先应用 `self`，再应用 `rhs`（等价于 `rhs * self`）。
    pub fn then(&self, rhs: &Transform) -> Transform {
        let m = self.to_kurbo() * rhs.to_kurbo();
        Self::from_kurbo(m)
    }

    /// 转 kurbo 仿射（用于后端计算与矩阵拼接）。
    pub fn to_kurbo(&self) -> Affine {
        Affine::new([self.a, self.b, self.c, self.d, self.e, self.f])
    }

    /// 从 kurbo 仿射构造。
    pub fn from_kurbo(a: Affine) -> Self {
        let c = a.as_coeffs();
        Self::new(c[0], c[1], c[2], c[3], c[4], c[5])
    }

    /// 变换一个点（标准 2D 仿射：x' = a*x + c*y + e, y' = b*x + d*y + f）。
    pub fn apply_point(&self, p: Point) -> Point {
        Point::new(
            self.a * p.x + self.c * p.y + self.e,
            self.b * p.x + self.d * p.y + self.f,
        )
    }
}
