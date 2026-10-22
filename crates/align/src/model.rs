//! The alignment model: a [`Similarity`] about the frame's center, and the parameters the solver estimates it with.
//!
//! A frame's alignment maps a pixel of the master frame to the same point of the scene in the frame: scale by `scale`
//! and turn by `rotation_deg` about the frame's center, then shift by `(shift_x, shift_y)`, as
//! [`Affine::similarity`] does and as the synthetic generator moves its frames. Focus breathing scales about the
//! optical center, which is close to the frame's.
//!
//! The solver works with [`Params`] instead: the shift, and the scale change and the rotation as the distances they
//! move a point at the frame's corner, all in full-resolution pixels. With every term in the same unit, the normal
//! equations stay well scaled whatever the frame's size, and one tolerance in pixels says when they have converged.

use florastack_core::Affine;

use crate::settings::{AlignSettings, Term};

/// A similarity transform about the center of a frame: scale, then rotate, then shift. See the module
/// documentation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Similarity {
    /// The horizontal shift in pixels: positive moves the master's content to the right in the frame.
    pub shift_x: f64,
    /// The vertical shift in pixels: positive is down.
    pub shift_y: f64,
    /// The magnification: 1 for none, above 1 when the frame shows the scene larger than the master.
    pub scale: f64,
    /// The rotation in degrees. With y pointing down, a positive angle turns clockwise on screen.
    pub rotation_deg: f64,
}

impl Similarity {
    pub const IDENTITY: Similarity = Similarity { shift_x: 0.0, shift_y: 0.0, scale: 1.0, rotation_deg: 0.0 };

    /// The transform of a `width` x `height` frame, about its center `((width - 1) / 2, (height - 1) / 2)`.
    pub fn to_affine(&self, width: usize, height: usize) -> Affine {
        Affine::similarity(center(width, height), self.scale, self.rotation_deg.to_radians(), (self.shift_x, self.shift_y))
    }

    /// The transform that applies `self` first and `next` after it, both about the same center.
    #[must_use]
    pub fn then(&self, next: &Similarity) -> Similarity {
        // next(self(x)) = s2 R2 (s1 R1 (x - c) + t1) + c + t2 = s1 s2 R2 R1 (x - c) + c + (s2 R2 t1 + t2).
        let (sin, cos) = next.rotation_deg.to_radians().sin_cos();
        Similarity {
            shift_x: next.scale * (cos * self.shift_x - sin * self.shift_y) + next.shift_x,
            shift_y: next.scale * (sin * self.shift_x + cos * self.shift_y) + next.shift_y,
            scale: self.scale * next.scale,
            rotation_deg: self.rotation_deg + next.rotation_deg,
        }
    }

    /// Whether every term is finite.
    pub fn is_finite(&self) -> bool {
        [self.shift_x, self.shift_y, self.scale, self.rotation_deg].iter().all(|v| v.is_finite())
    }
}

impl Default for Similarity {
    fn default() -> Self {
        Similarity::IDENTITY
    }
}

/// The center of a `width` x `height` frame, in pixels.
pub(crate) fn center(width: usize, height: usize) -> (f64, f64) {
    ((width.max(1) - 1) as f64 / 2.0, (height.max(1) - 1) as f64 / 2.0)
}

/// The terms of a [`Similarity`] as the solver estimates them, indexed by [`Term`]: the shift in pixels, and the
/// scale change and rotation as the distance they move a point at the frame's corner, `(scale - 1) r` and
/// `radians r`, where `r` is the distance from the center to a corner.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Params(pub [f64; 4]);

impl Params {
    pub fn get(&self, term: Term) -> f64 {
        self.0[index(term)]
    }

    pub fn set(&mut self, term: Term, value: f64) {
        self.0[index(term)] = value;
    }
}

/// The position of a term in [`Params`].
pub(crate) const fn index(term: Term) -> usize {
    match term {
        Term::ShiftX => 0,
        Term::ShiftY => 1,
        Term::Scale => 2,
        Term::Rotation => 3,
    }
}

/// The geometry of a stack's frames: their size, the center that transforms turn about, the corner radius that
/// [`Params`] are measured at, and the limits of each term in those units.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Geometry {
    pub width: usize,
    pub height: usize,
    pub center: (f64, f64),
    /// The distance from the center to a corner, at least 1.
    pub radius: f64,
    /// The largest magnitude of each term, in [`Params`] units.
    pub limits: Params,
}

impl Geometry {
    pub fn new(width: usize, height: usize, settings: &AlignSettings) -> Geometry {
        let center = center(width, height);
        let radius = center.0.hypot(center.1).max(1.0);
        let shift = settings.max_shift / 100.0;
        let limits =
            Params([shift * width as f64, shift * height as f64, settings.max_scale / 100.0 * radius, settings.max_rotation.to_radians() * radius]);
        Geometry { width, height, center, radius, limits }
    }

    /// The similarity that `params` stand for.
    pub fn similarity(&self, params: &Params) -> Similarity {
        let [tx, ty, scale, rotation] = params.0;
        Similarity { shift_x: tx, shift_y: ty, scale: 1.0 + scale / self.radius, rotation_deg: (rotation / self.radius).to_degrees() }
    }

    /// The parameters of `similarity`.
    pub fn params(&self, similarity: &Similarity) -> Params {
        Params([similarity.shift_x, similarity.shift_y, (similarity.scale - 1.0) * self.radius, similarity.rotation_deg.to_radians() * self.radius])
    }

    /// The first term of `params` that lies beyond its limit by more than `tolerance` pixels.
    pub fn beyond_limits(&self, params: &Params, tolerance: f64) -> Option<Term> {
        Term::ALL.into_iter().find(|&t| params.get(t).abs() > self.limits.get(t) + tolerance)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: (f64, f64), b: (f64, f64)) -> bool {
        (a.0 - b.0).abs() < 1e-9 && (a.1 - b.1).abs() < 1e-9
    }

    #[test]
    fn similarities_compose_like_their_affines() {
        let (w, h) = (300, 200);
        let a = Similarity { shift_x: 1.5, shift_y: -0.75, scale: 1.02, rotation_deg: 0.4 };
        let b = Similarity { shift_x: -3.0, shift_y: 2.0, scale: 0.97, rotation_deg: -1.1 };
        let composed = a.then(&b).to_affine(w, h);
        let expected = a.to_affine(w, h).then(&b.to_affine(w, h));
        for (x, y) in [(0.0, 0.0), (299.0, 199.0), (40.5, 170.25)] {
            assert!(close(composed.apply(x, y), expected.apply(x, y)));
        }
        assert_eq!(Similarity::IDENTITY.then(&a), a);
        assert!(Similarity::default().to_affine(w, h).is_identity());
    }

    #[test]
    fn params_measure_terms_at_the_corner() {
        let settings = AlignSettings { max_shift: 10.0, max_scale: 5.0, max_rotation: 2.0, ..AlignSettings::default() };
        let g = Geometry::new(401, 301, &settings);
        assert_eq!((g.center, g.radius), ((200.0, 150.0), 250.0));
        assert!((g.limits.0[0] - 40.1).abs() < 1e-9 && (g.limits.0[1] - 30.1).abs() < 1e-9);
        let s = Similarity { shift_x: 2.0, shift_y: -1.0, scale: 1.004, rotation_deg: 0.5 };
        let p = g.params(&s);
        // A 0.4% scale change moves the corner by 1 pixel.
        assert!((p.get(Term::Scale) - 1.0).abs() < 1e-9);
        let back = g.similarity(&p);
        assert!((back.scale - s.scale).abs() < 1e-12 && (back.rotation_deg - s.rotation_deg).abs() < 1e-12);
        assert_eq!(g.beyond_limits(&p, 0.0), None);
        let wide = Params([0.0, 30.5, 0.0, 0.0]);
        assert_eq!(g.beyond_limits(&wide, 0.0), Some(Term::ShiftY));
        assert_eq!(g.beyond_limits(&wide, 0.5), None, "within the tolerance");
    }
}
