//! Geometric and color base types.
//!
//! Coordinate types ([`Point`] / [`Rect`] / [`Vec2`] / [`Size`]) and the Bézier path [`BezPath`]
//! are **re-exported directly from kurbo**, giving zero-cost interoperability with the geometry
//! types of rendering backends (vello / parley) and downstream crates (e.g. liemermaid), and
//! eliminating the `to_lie_*` shim functions that "two coordinate systems" would otherwise need.
//! Layout methods (`min_x` / `max_x` / `inflate` / `from_points`, etc.) are provided directly by
//! kurbo; [`RectExt::union`] is a bounding-box union method added by this crate on top of kurbo's
//! `Rect`.
//!
//! ## Geometry pass-through (controlled allow-list)
//!
//! kurbo's "geometry / path / shape" public types are **explicitly enumerated and re-exported**
//! here (not a `pub use kurbo;` glob). Downstream can usually take all geometric vocabulary types
//! via `lievisual::geometry::*` (or the corresponding re-exports at the crate root) without
//! per-item allow-list maintenance; only kurbo `Stroke` is not promoted to the crate root because
//! it collides with the [`crate::scene::Stroke`] primitive stroke — take it from
//! `lievisual::geometry::Stroke` instead. The exposure is limited to geometric vocabulary types
//! (the stable kurbo 0.13 set), deliberately excluding the `ParamCurve` trait family and the
//! `common` / `offset` / `simplify` algorithm modules to avoid leaking internal concepts.
//!
//! [`Color`] / [`Transform`] remain custom (kurbo has no equivalent, or business semantics are
//! needed). [`PointExt`] / [`RectExt`] are convenience extension methods missing from kurbo,
//! layered on top of the geometric types. The custom [`Transform`] performs backend matrix
//! composition through [`Affine`] (i.e. kurbo's `Affine`).

// ——— kurbo geometric vocabulary types: explicit enumeration (stable kurbo 0.13 set) ———
pub use kurbo::{
    Affine, Arc, ArcAppendIter, Axis, BezPath, Circle, CirclePathIter, CircleSegment, CubicBez,
    CubicBezIter, CuspType, Diagonal2, Ellipse, Insets, Line, LineIntersection, LinePathIter,
    MinDistance, Moments, PathEl, PathSeg, PathSegIter, Point, QuadBez, QuadBezIter, QuadSpline,
    Rect, RectPathIter, RoundedRect, RoundedRectPathIter, RoundedRectRadii, Segments, Shape, Size,
    Stroke, SvgArc, TranslateScale, Triangle, TrianglePathIter, Vec2,
};

/// Convenience extension methods for [`Point`] (helpers missing from kurbo).
///
/// Re-exported from the crate root (`use lievisual::PointExt`; also
/// `use lievisual::geometry::PointExt`).
pub trait PointExt {
    /// Midpoint.
    #[must_use]
    fn midpoint(self, other: Self) -> Self;
    /// Whether both components are finite (not NaN / infinity).
    #[must_use]
    fn is_finite(&self) -> bool;
    /// Component-wise minimum.
    #[must_use]
    fn min(self, other: Self) -> Self;
    /// Component-wise maximum.
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

/// Convenience extension methods for [`Rect`] (helpers missing from kurbo 0.13).
///
/// Re-exported from the crate root (`use lievisual::RectExt`; also
/// `use lievisual::geometry::RectExt`).
///
/// Note: for outward expansion use kurbo's own `Rect::inflate(width, height)`
/// (expands x / y by the respective amounts).
pub trait RectExt {
    /// Bounding box that contains another rectangle (union).
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

/// Color (RGBA, components in the range 0.0–1.0, consistent with vello / CSS).
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

    /// Opaque 8-bit RGB.
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self {
            r: r as f64 / 255.0,
            g: g as f64 / 255.0,
            b: b as f64 / 255.0,
            a: 1.0,
        }
    }

    /// 8-bit RGBA.
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self {
            r: r as f64 / 255.0,
            g: g as f64 / 255.0,
            b: b as f64 / 255.0,
            a: a as f64 / 255.0,
        }
    }

    pub fn r(&self) -> u8 {
        (self.r.clamp(0.0, 1.0) * 255.0).round() as u8
    }

    pub fn g(&self) -> u8 {
        (self.g.clamp(0.0, 1.0) * 255.0).round() as u8
    }

    pub fn b(&self) -> u8 {
        (self.b.clamp(0.0, 1.0) * 255.0).round() as u8
    }

    pub const BLACK: Color = Color::new(0.0, 0.0, 0.0, 1.0);
    pub const WHITE: Color = Color::new(1.0, 1.0, 1.0, 1.0);
    pub const TRANSPARENT: Color = Color::new(0.0, 0.0, 0.0, 0.0);
    pub const RED: Color = Color::new(1.0, 0.0, 0.0, 1.0);
    pub const GREEN: Color = Color::new(0.0, 1.0, 0.0, 1.0);
    pub const BLUE: Color = Color::new(0.0, 0.0, 1.0, 1.0);

    /// CSS hex string, e.g. `#0066cc`.
    /// Emits `#rrggbb` when opaque, or `#rrggbbaa` when an alpha component is present.
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

/// 2D affine transform, used for the local coordinate system of [`crate::scene::SceneNode`].
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

    /// Translation.
    pub fn translate(x: f64, y: f64) -> Self {
        Self::new(1.0, 0.0, 0.0, 1.0, x, y)
    }

    /// Uniform scaling.
    pub fn scale(s: f64) -> Self {
        Self::new(s, 0.0, 0.0, s, 0.0, 0.0)
    }

    /// Rotation about the origin (radians).
    pub fn rotate(angle: f64) -> Self {
        let (s, c) = angle.sin_cos();
        Self::new(c, s, -s, c, 0.0, 0.0)
    }

    /// Compose: apply `self` first, then `rhs` (equivalent to `rhs * self`).
    pub fn then(&self, rhs: &Transform) -> Transform {
        let m = self.to_kurbo() * rhs.to_kurbo();
        Self::from_kurbo(m)
    }

    /// Convert to a kurbo affine (for backend computation and matrix composition).
    pub fn to_kurbo(&self) -> Affine {
        Affine::new([self.a, self.b, self.c, self.d, self.e, self.f])
    }

    /// Construct from a kurbo affine.
    pub fn from_kurbo(a: Affine) -> Self {
        let c = a.as_coeffs();
        Self::new(c[0], c[1], c[2], c[3], c[4], c[5])
    }

    /// Transform a point (standard 2D affine: x' = a*x + c*y + e, y' = b*x + d*y + f).
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
            // kurbo semantics: Point + Vec2 = Point; Point - Point = Vec2.
            assert_eq!(a + b.to_vec2(), Point::new(5.0, 8.0));
            assert_eq!(b - a, Vec2::new(3.0, 4.0));
            assert_eq!(a.to_vec2() * 2.0, Vec2::new(2.0, 4.0));
            assert!((a.distance(b) - 5.0).abs() < 1e-9); // 3-4-5 triangle
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
            // kurbo Rect::new(x0, y0, x1, y1) -- min/max semantics.
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
            // kurbo's own inflate(width, height) -- expands x / y respectively.
            let i = a.inflate(2.0, 2.0);
            assert_eq!(i, Rect::new(-2.0, -2.0, 12.0, 12.0));
        }

        #[test]
        fn contains_point() {
            // kurbo contains uses a half-open interval [x0,x1) x [y0,y1).
            let r = Rect::new(0.0, 0.0, 10.0, 10.0);
            assert!(r.contains(Point::new(5.0, 5.0)));
            assert!(r.contains(Point::new(0.0, 0.0)));
            assert!(!r.contains(Point::new(10.0, 10.0))); // right/bottom edges excluded
            assert!(!r.contains(Point::new(10.1, 5.0)));
            assert!(!r.contains(Point::new(5.0, -1.0)));
        }
    }
}
