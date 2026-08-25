//! 几何与颜色基础类型。
//!
//! 坐标类型（[`Point`] / [`Rect`] / [`Vec2`] / [`Size`]）与贝塞尔路径 [`BezPath`]
//! **直接 re-export kurbo**：与渲染后端（vello / parley）及下游（如 liemermaid）的
//! 几何类型零转换互通，消除"两套坐标"导致的 `to_lie_*` 缝合函数。布局方法
//! （`min_x`/`max_x`/`union`/`inflate`/`from_points`/`center` 等）由 kurbo 直接提供。
//!
//! [`Color`] / [`Transform`] 保留自定义（kurbo 无对应或需业务语义）。

pub use kurbo::{BezPath, Point, Rect, Size, Vec2};

use kurbo::Affine;

/// [`Point`] 的便捷扩展方法（kurbo 缺失的辅助）。
///
/// 已随 crate 根 re-export（`use lievisual::PointExt`；也可 `use lievisual::geometry::PointExt`）。
pub trait PointExt {
    /// 中点。
    #[must_use]
    fn midpoint(self, other: Self) -> Self;
    /// 两个分量是否都是有限值（非 NaN / 无穷）。
    #[must_use]
    fn is_finite(&self) -> bool;
    /// 分量最小值。
    #[must_use]
    fn min(self, other: Self) -> Self;
    /// 分量最大值。
    #[must_use]
    fn max(self, other: Self) -> Self;
}

impl PointExt for Point {
    #[inline]
    fn midpoint(self, other: Self) -> Self {
        Point::new((self.x + other.x) / 2.0, (self.y + other.y) / 2.0)
    }

    #[inline]
    fn is_finite(&self) -> bool {
        self.x.is_finite() && self.y.is_finite()
    }

    #[inline]
    fn min(self, other: Self) -> Self {
        Point::new(self.x.min(other.x), self.y.min(other.y))
    }

    #[inline]
    fn max(self, other: Self) -> Self {
        Point::new(self.x.max(other.x), self.y.max(other.y))
    }
}

/// [`Rect`] 的便捷扩展方法（kurbo 0.13 缺失的辅助）。
///
/// 已随 crate 根 re-export（`use lievisual::RectExt`；也可 `use lievisual::geometry::RectExt`）。
///
/// 注：外扩用 kurbo 自带 `Rect::inflate(width, height)`（x/y 各自外扩量）。
pub trait RectExt {
    /// 包含另一个矩形的包围盒（并集）。
    #[must_use]
    fn union(&self, other: Self) -> Rect;
}

impl RectExt for Rect {
    #[inline]
    fn union(&self, other: Self) -> Rect {
        Rect::new(
            self.min_x().min(other.min_x()),
            self.min_y().min(other.min_y()),
            self.max_x().max(other.max_x()),
            self.max_y().max(other.max_y()),
        )
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
    pub const RED: Color = Color::new(1.0, 0.0, 0.0, 1.0);
    pub const GREEN: Color = Color::new(0.0, 1.0, 0.0, 1.0);
    pub const BLUE: Color = Color::new(0.0, 0.0, 1.0, 1.0);

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

#[cfg(test)]
mod tests {
    use super::*;

    mod point {
        use super::{PointExt, *};

        #[test]
        fn arithmetic_and_distance() {
            let a = Point::new(1.0, 2.0);
            let b = Point::new(4.0, 6.0);
            // kurbo 语义：Point + Vec2 = Point；Point - Point = Vec2。
            assert_eq!(a + b.to_vec2(), Point::new(5.0, 8.0));
            assert_eq!(b - a, Vec2::new(3.0, 4.0));
            assert_eq!(a.to_vec2() * 2.0, Vec2::new(2.0, 4.0));
            assert!((a.distance(b) - 5.0).abs() < 1e-9); // 3-4-5
        }

        #[test]
        fn lerp_midpoint_min_max() {
            let a = Point::new(0.0, 0.0);
            let b = Point::new(10.0, 20.0);
            assert_eq!(a.lerp(b, 0.5), Point::new(5.0, 10.0));
            assert_eq!(a.midpoint(b), Point::new(5.0, 10.0));
            assert_eq!(a.min(b), Point::new(0.0, 0.0));
            assert_eq!(a.max(b), Point::new(10.0, 20.0));
        }
    }

    mod vec2 {
        use super::*;

        #[test]
        fn length_dot_and_ops() {
            let a = Vec2::new(3.0, 4.0);
            assert!((a.length() - 5.0).abs() < 1e-9);
            assert!((a.dot(Vec2::new(1.0, 0.0)) - 3.0).abs() < 1e-9);
            assert_eq!(a + Vec2::new(1.0, 1.0), Vec2::new(4.0, 5.0));
            assert_eq!(a * 2.0, Vec2::new(6.0, 8.0));
        }
    }

    mod rect {
        use super::*;

        #[test]
        fn accessors_and_bounds() {
            // kurbo Rect::new(x0, y0, x1, y1) —— min/max 语义。
            let r = Rect::new(2.0, 4.0, 12.0, 10.0);
            assert_eq!(r.min_x(), 2.0);
            assert_eq!(r.max_x(), 12.0);
            assert_eq!(r.min_y(), 4.0);
            assert_eq!(r.max_y(), 10.0);
            assert_eq!(r.width(), 10.0);
            assert_eq!(r.height(), 6.0);
        }

        #[test]
        fn from_points_normalizes_opposite_corners() {
            let r = Rect::from_points(Point::new(5.0, 7.0), Point::new(1.0, 3.0));
            assert_eq!(r, Rect::new(1.0, 3.0, 5.0, 7.0));
        }

        #[test]
        fn union_and_inflate() {
            let a = Rect::new(0.0, 0.0, 10.0, 10.0);
            let b = Rect::new(5.0, 5.0, 15.0, 15.0);
            let u = a.union(b);
            assert_eq!(u, Rect::new(0.0, 0.0, 15.0, 15.0));
            // kurbo 自带 inflate(width, height) —— x/y 各自外扩。
            let i = a.inflate(2.0, 2.0);
            assert_eq!(i, Rect::new(-2.0, -2.0, 12.0, 12.0));
        }

        #[test]
        fn contains_point() {
            // kurbo contains 是半开区间 [x0,x1) × [y0,y1)。
            let r = Rect::new(0.0, 0.0, 10.0, 10.0);
            assert!(r.contains(Point::new(5.0, 5.0)));
            assert!(r.contains(Point::new(0.0, 0.0)));
            assert!(!r.contains(Point::new(10.0, 10.0))); // 右/下边界不含
            assert!(!r.contains(Point::new(10.1, 5.0)));
            assert!(!r.contains(Point::new(5.0, -1.0)));
        }
    }
}
