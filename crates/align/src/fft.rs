//! The discrete Fourier transform of power-of-two lengths, in one and two dimensions, by the iterative radix-2
//! algorithm (J. W. Cooley and J. W. Tukey, "An algorithm for the machine calculation of complex Fourier series",
//! Mathematics of Computation 19(90), 1965): what phase correlation needs, on windows of a few hundred samples a side.
//!
//! Values are `f64`. The forward transform is `X[k] = sum over j of x[j] exp(-2 pi i j k / n)`, and the inverse
//! divides by `n`, so that it undoes the forward one. The rows, then the columns, of a 2D transform are transformed in
//! parallel; each is computed by the same arithmetic whatever the thread count.

use std::ops::{Add, Mul, Sub};

use rayon::prelude::*;

/// A complex number.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Complex {
    pub re: f64,
    pub im: f64,
}

impl Complex {
    pub const ZERO: Complex = Complex { re: 0.0, im: 0.0 };

    pub const fn new(re: f64, im: f64) -> Complex {
        Complex { re, im }
    }

    pub fn conj(self) -> Complex {
        Complex::new(self.re, -self.im)
    }

    pub fn norm(self) -> f64 {
        self.re.hypot(self.im)
    }

    pub fn scale(self, k: f64) -> Complex {
        Complex::new(self.re * k, self.im * k)
    }
}

impl Add for Complex {
    type Output = Complex;
    fn add(self, o: Complex) -> Complex {
        Complex::new(self.re + o.re, self.im + o.im)
    }
}

impl Sub for Complex {
    type Output = Complex;
    fn sub(self, o: Complex) -> Complex {
        Complex::new(self.re - o.re, self.im - o.im)
    }
}

impl Mul for Complex {
    type Output = Complex;
    fn mul(self, o: Complex) -> Complex {
        Complex::new(self.re * o.re - self.im * o.im, self.re * o.im + self.im * o.re)
    }
}

/// A transform of one power-of-two length, with its twiddle factors and bit-reversal permutation.
#[derive(Clone, Debug)]
pub(crate) struct Fft {
    n: usize,
    /// `exp(-2 pi i k / n)` for `k` below `n / 2`.
    twiddles: Vec<Complex>,
    /// Where each index goes in the bit-reversed order.
    reversed: Vec<usize>,
}

impl Fft {
    /// The transform of length `n`, or `None` unless `n` is a power of two.
    pub fn new(n: usize) -> Option<Fft> {
        if !n.is_power_of_two() {
            return None;
        }
        let twiddles = (0..n / 2)
            .map(|k| {
                let (sin, cos) = (-std::f64::consts::TAU * k as f64 / n as f64).sin_cos();
                Complex::new(cos, sin)
            })
            .collect();
        let bits = n.trailing_zeros();
        let reversed = (0..n).map(|i| if bits == 0 { 0 } else { i.reverse_bits() >> (usize::BITS - bits) }).collect();
        Some(Fft { n, twiddles, reversed })
    }

    pub fn len(&self) -> usize {
        self.n
    }

    /// Transforms `data` in place. Data of another length is left as it is.
    pub fn forward(&self, data: &mut [Complex]) {
        self.run(data, false);
    }

    /// Transforms `data` back in place, divided by the length.
    pub fn inverse(&self, data: &mut [Complex]) {
        self.run(data, true);
        let k = 1.0 / self.n as f64;
        for v in data.iter_mut() {
            *v = v.scale(k);
        }
    }

    fn run(&self, data: &mut [Complex], inverse: bool) {
        if data.len() != self.n {
            return;
        }
        for (i, &j) in self.reversed.iter().enumerate() {
            if i < j {
                data.swap(i, j);
            }
        }
        let mut len = 2;
        while len <= self.n {
            let half = len / 2;
            let step = self.n / len;
            for chunk in data.chunks_exact_mut(len) {
                let (lo, hi) = chunk.split_at_mut(half);
                for ((a, b), &w) in lo.iter_mut().zip(hi.iter_mut()).zip(self.twiddles.iter().step_by(step)) {
                    let w = if inverse { w.conj() } else { w };
                    let t = *b * w;
                    *b = *a - t;
                    *a = *a + t;
                }
            }
            len *= 2;
        }
    }
}

/// A 2D transform of `width` x `height` values in row-major order, both powers of two.
#[derive(Clone, Debug)]
pub(crate) struct Fft2 {
    rows: Fft,
    columns: Fft,
}

impl Fft2 {
    pub fn new(width: usize, height: usize) -> Option<Fft2> {
        Some(Fft2 { rows: Fft::new(width)?, columns: Fft::new(height)? })
    }

    pub fn width(&self) -> usize {
        self.rows.len()
    }

    pub fn height(&self) -> usize {
        self.columns.len()
    }

    pub fn forward(&self, data: &mut [Complex]) {
        self.run(data, false);
    }

    /// The inverse, divided by `width * height`.
    pub fn inverse(&self, data: &mut [Complex]) {
        self.run(data, true);
    }

    fn run(&self, data: &mut [Complex], inverse: bool) {
        let (w, h) = (self.width(), self.height());
        if data.len() != w * h {
            return;
        }
        let one = |fft: &Fft, line: &mut [Complex]| if inverse { fft.inverse(line) } else { fft.forward(line) };
        data.par_chunks_exact_mut(w).for_each(|row| one(&self.rows, row));
        let mut columns = transpose(data, w, h);
        columns.par_chunks_exact_mut(h).for_each(|column| one(&self.columns, column));
        data.copy_from_slice(&transpose(&columns, h, w));
    }
}

/// The `height` x `width` transpose of `width` x `height` values in row-major order.
fn transpose(data: &[Complex], width: usize, height: usize) -> Vec<Complex> {
    let mut out = vec![Complex::ZERO; data.len()];
    for (y, row) in data.chunks_exact(width).enumerate().take(height) {
        for (x, &v) in row.iter().enumerate() {
            if let Some(o) = out.get_mut(x * height + y) {
                *o = v;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The DFT by its definition.
    fn naive(data: &[Complex]) -> Vec<Complex> {
        let n = data.len();
        (0..n)
            .map(|k| {
                data.iter().enumerate().fold(Complex::ZERO, |acc, (j, &x)| {
                    let (sin, cos) = (-std::f64::consts::TAU * (j * k % n) as f64 / n as f64).sin_cos();
                    acc + x * Complex::new(cos, sin)
                })
            })
            .collect()
    }

    fn signal(n: usize, seed: u64) -> Vec<Complex> {
        let mut rng = florastack_core::testutil::Rng::new(seed);
        (0..n).map(|_| Complex::new(rng.range(-1.0, 1.0), rng.range(-1.0, 1.0))).collect()
    }

    fn close(a: &[Complex], b: &[Complex], tolerance: f64) -> bool {
        a.len() == b.len() && a.iter().zip(b).all(|(x, y)| (*x - *y).norm() < tolerance)
    }

    #[test]
    fn the_fft_matches_the_definition_and_inverts() {
        for n in [1, 2, 4, 8, 64, 256] {
            let data = signal(n, n as u64);
            let fft = Fft::new(n).unwrap();
            let mut transformed = data.clone();
            fft.forward(&mut transformed);
            assert!(close(&transformed, &naive(&data), 1e-9), "length {n}");
            fft.inverse(&mut transformed);
            assert!(close(&transformed, &data, 1e-12), "length {n}");
        }
        assert!(Fft::new(0).is_none() && Fft::new(12).is_none());
        let mut wrong = signal(5, 1);
        let before = wrong.clone();
        Fft::new(8).unwrap().forward(&mut wrong);
        assert_eq!(wrong, before, "another length is left alone");
    }

    #[test]
    fn the_2d_fft_transforms_rows_then_columns() {
        let (w, h) = (8, 4);
        let data = signal(w * h, 3);
        let fft = Fft2::new(w, h).unwrap();
        let mut transformed = data.clone();
        fft.forward(&mut transformed);
        // The definition, separably: each row, then each column.
        let rows: Vec<Complex> = data.chunks(w).flat_map(naive).collect();
        let mut expected = vec![Complex::ZERO; w * h];
        for x in 0..w {
            let column: Vec<Complex> = (0..h).map(|y| rows[y * w + x]).collect();
            for (y, v) in naive(&column).into_iter().enumerate() {
                expected[y * w + x] = v;
            }
        }
        assert!(close(&transformed, &expected, 1e-9));
        fft.inverse(&mut transformed);
        assert!(close(&transformed, &data, 1e-12));
    }
}
