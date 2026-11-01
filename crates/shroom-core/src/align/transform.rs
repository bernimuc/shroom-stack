/// 2D affine transform `p' = M·(x, y, 1)` in pixel coordinates (pixel centres at
/// integer positions).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Affine {
    pub m: [[f64; 3]; 2],
}

impl Affine {
    pub const IDENTITY: Affine = Affine { m: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0]] };

    pub fn translation(tx: f64, ty: f64) -> Self {
        Affine { m: [[1.0, 0.0, tx], [0.0, 1.0, ty]] }
    }

    pub fn scale(s: f64) -> Self {
        Affine { m: [[s, 0.0, 0.0], [0.0, s, 0.0]] }
    }

    /// Similarity about `centre`: scale `s`, rotation `angle` (radians), then shift.
    pub fn similarity(centre: (f64, f64), s: f64, angle: f64, shift: (f64, f64)) -> Self {
        let (a, b) = (s * angle.cos(), s * angle.sin());
        Self::from_params(centre, a - 1.0, b, shift.0, shift.1)
    }

    /// `p' = c + [[1+a, −b], [b, 1+a]]·(p − c) + t`, the parametrisation used by the
    /// estimator.
    pub(crate) fn from_params(c: (f64, f64), a: f64, b: f64, tx: f64, ty: f64) -> Self {
        let (s, r) = (1.0 + a, b);
        Affine { m: [[s, -r, c.0 - s * c.0 + r * c.1 + tx], [r, s, c.1 - r * c.0 - s * c.1 + ty]] }
    }

    pub fn apply(&self, x: f64, y: f64) -> (f64, f64) {
        let m = &self.m;
        (m[0][0] * x + m[0][1] * y + m[0][2], m[1][0] * x + m[1][1] * y + m[1][2])
    }

    /// `self ∘ other`: applies `other` first.
    pub fn then_after(&self, other: &Affine) -> Affine {
        let (a, b) = (&self.m, &other.m);
        let mut m = [[0.0; 3]; 2];
        for (i, row) in m.iter_mut().enumerate() {
            row[0] = a[i][0] * b[0][0] + a[i][1] * b[1][0];
            row[1] = a[i][0] * b[0][1] + a[i][1] * b[1][1];
            row[2] = a[i][0] * b[0][2] + a[i][1] * b[1][2] + a[i][2];
        }
        Affine { m }
    }

    pub fn inverse(&self) -> Option<Affine> {
        let m = &self.m;
        let det = m[0][0] * m[1][1] - m[0][1] * m[1][0];
        if det.abs() < 1e-12 {
            return None;
        }
        let (a, b, c, d) = (m[1][1] / det, -m[0][1] / det, -m[1][0] / det, m[0][0] / det);
        Some(Affine { m: [[a, b, -(a * m[0][2] + b * m[1][2])], [c, d, -(c * m[0][2] + d * m[1][2])]] })
    }

    /// Mean scale factor (square root of the determinant).
    pub fn scale_factor(&self) -> f64 {
        (self.m[0][0] * self.m[1][1] - self.m[0][1] * self.m[1][0]).abs().sqrt()
    }

    pub fn is_identity(&self, eps: f64) -> bool {
        self.m.iter().flatten().zip(Self::IDENTITY.m.iter().flatten()).all(|(a, b)| (a - b).abs() < eps)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: (f64, f64), b: (f64, f64)) -> bool {
        (a.0 - b.0).abs() < 1e-9 && (a.1 - b.1).abs() < 1e-9
    }

    #[test]
    fn similarity_keeps_centre_fixed_without_shift() {
        let t = Affine::similarity((50.0, 30.0), 1.1, 0.2, (0.0, 0.0));
        assert!(close(t.apply(50.0, 30.0), (50.0, 30.0)));
        let (x, y) = t.apply(60.0, 30.0);
        assert!(((x - 50.0).hypot(y - 30.0) - 11.0).abs() < 1e-9);
        assert!((t.scale_factor() - 1.1).abs() < 1e-12);
    }

    #[test]
    fn compose_and_inverse() {
        let a = Affine::similarity((10.0, 20.0), 1.03, -0.01, (2.0, -1.5));
        let b = Affine::translation(5.0, 7.0).then_after(&Affine::scale(2.0));
        let p = (3.0, 4.0);
        let (bx, by) = b.apply(p.0, p.1);
        assert!(close(a.then_after(&b).apply(p.0, p.1), a.apply(bx, by)));
        assert!(a.then_after(&a.inverse().unwrap()).is_identity(1e-12));
        assert!(Affine::scale(0.0).inverse().is_none());
    }
}
