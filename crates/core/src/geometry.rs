//! Geometry: [`Affine`], the 2x3 transform that frame alignment estimates and warps apply.
//!
//! Coordinates are in pixels with pixel `(x, y)` at the point `(x, y)`: integer coordinates are pixel centers, so the
//! identity maps every pixel onto itself exactly, and the center of a `w`x`h` image is `((w - 1) / 2, (h - 1) / 2)`.

/// A 2D affine transform, `x' = a x + b y + c` and `y' = d x + e y + f`, stored as the rows `[[a, b, c], [d, e, f]]`.
///
/// `f64`, so that composing the transforms of a long stack doesn't drift.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Affine {
    pub m: [[f64; 3]; 2],
}

impl Affine {
    pub const IDENTITY: Affine = Affine { m: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0]] };

    pub const fn translation(tx: f64, ty: f64) -> Affine {
        Affine { m: [[1.0, 0.0, tx], [0.0, 1.0, ty]] }
    }

    /// Scaling by `s` about the origin.
    pub const fn scaling(s: f64) -> Affine {
        Affine { m: [[s, 0.0, 0.0], [0.0, s, 0.0]] }
    }

    /// Rotation by `radians` about the origin. With y pointing down, a positive angle turns clockwise on screen.
    pub fn rotation(radians: f64) -> Affine {
        let (sin, cos) = radians.sin_cos();
        Affine { m: [[cos, -sin, 0.0], [sin, cos, 0.0]] }
    }

    /// A similarity transform about `center`: scale by `scale`, rotate by `radians`, then shift by `shift`. This is
    /// how a frame moves relative to another: focus breathing scales about the optical center, and the camera or
    /// subject shifts and turns.
    pub fn similarity(center: (f64, f64), scale: f64, radians: f64, shift: (f64, f64)) -> Affine {
        Affine::translation(-center.0, -center.1)
            .then(&Affine::scaling(scale))
            .then(&Affine::rotation(radians))
            .then(&Affine::translation(center.0 + shift.0, center.1 + shift.1))
    }

    /// The transform that applies `self` first and `next` after it.
    #[must_use]
    pub fn then(&self, next: &Affine) -> Affine {
        let [[a, b, c], [d, e, f]] = self.m;
        let [[na, nb, nc], [nd, ne, nf]] = next.m;
        Affine { m: [[na * a + nb * d, na * b + nb * e, na * c + nb * f + nc], [nd * a + ne * d, nd * b + ne * e, nd * c + ne * f + nf]] }
    }

    /// The inverse transform, or `None` when this one is singular or not finite.
    pub fn inverse(&self) -> Option<Affine> {
        let [[a, b, c], [d, e, f]] = self.m;
        let det = a * e - b * d;
        if !det.is_finite() || det == 0.0 {
            return None;
        }
        let inverse = Affine { m: [[e / det, -b / det, (b * f - c * e) / det], [-d / det, a / det, (c * d - a * f) / det]] };
        inverse.m.iter().flatten().all(|v| v.is_finite()).then_some(inverse)
    }

    /// The image of the point `(x, y)`.
    pub fn apply(&self, x: f64, y: f64) -> (f64, f64) {
        let [[a, b, c], [d, e, f]] = self.m;
        (a * x + b * y + c, d * x + e * y + f)
    }

    /// Whether this is exactly the identity.
    pub fn is_identity(&self) -> bool {
        *self == Affine::IDENTITY
    }
}

impl Default for Affine {
    fn default() -> Self {
        Affine::IDENTITY
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: (f64, f64), b: (f64, f64)) -> bool {
        (a.0 - b.0).abs() < 1e-9 && (a.1 - b.1).abs() < 1e-9
    }

    #[test]
    fn the_identity_maps_pixels_onto_themselves_exactly() {
        assert_eq!(Affine::IDENTITY.apply(17.0, -3.0), (17.0, -3.0));
        assert!(Affine::default().is_identity());
        assert!(Affine::similarity((10.0, 20.0), 1.0, 0.0, (0.0, 0.0)).is_identity());
    }

    #[test]
    fn then_applies_the_first_transform_first() {
        let shift = Affine::translation(5.0, 0.0);
        let double = Affine::scaling(2.0);
        assert_eq!(shift.then(&double).apply(1.0, 1.0), (12.0, 2.0));
        assert_eq!(double.then(&shift).apply(1.0, 1.0), (7.0, 2.0));
    }

    #[test]
    fn a_similarity_keeps_its_center_up_to_the_shift() {
        let t = Affine::similarity((50.0, 30.0), 1.05, 0.1, (0.25, -0.5));
        assert!(close(t.apply(50.0, 30.0), (50.25, 29.5)));
        // A quarter turn clockwise on screen (y down): right of the center goes below it.
        let quarter = Affine::similarity((0.0, 0.0), 1.0, std::f64::consts::FRAC_PI_2, (0.0, 0.0));
        assert!(close(quarter.apply(1.0, 0.0), (0.0, 1.0)));
    }

    #[test]
    fn inverse_undoes_the_transform() {
        let t = Affine::similarity((12.0, 7.0), 0.97, -0.3, (1.5, 2.25));
        let inverse = t.inverse().unwrap();
        for (x, y) in [(0.0, 0.0), (100.0, -40.0), (3.5, 8.25)] {
            assert!(close(inverse.apply(t.apply(x, y).0, t.apply(x, y).1), (x, y)));
            assert!(close(t.then(&inverse).apply(x, y), (x, y)));
        }
        assert_eq!(Affine::scaling(0.0).inverse(), None);
        assert_eq!(Affine::scaling(f64::NAN).inverse(), None);
    }
}
