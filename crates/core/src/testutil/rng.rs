//! A small seeded pseudorandom generator for tests: xoshiro256** seeded through `SplitMix64`, with uniform and normal
//! draws. Reproducible across platforms and runs, which `std` alone doesn't offer.
//!
//! - xoshiro256**: D. Blackman and S. Vigna, "Scrambled linear pseudorandom number generators", ACM Transactions on
//!   Mathematical Software 47(4), 2021.
//! - `SplitMix64`: G. L. Steele, D. Lea and C. H. Flood, "Fast splittable pseudorandom number generators", OOPSLA 2014.
//! - Normal draws: G. E. P. Box and M. E. Muller, "A note on the generation of random normal deviates", Annals of
//!   Mathematical Statistics 29(2), 1958.

/// The generator. Not for cryptography.
#[derive(Clone, Debug)]
pub struct Rng {
    s: [u64; 4],
}

impl Rng {
    /// A generator whose sequence depends only on `seed`.
    pub fn new(seed: u64) -> Rng {
        let mut state = seed;
        let mut next = || {
            state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = state;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            z ^ (z >> 31)
        };
        Rng { s: [next(), next(), next(), next()] }
    }

    pub fn next_u64(&mut self) -> u64 {
        let s = &mut self.s;
        let result = s[1].wrapping_mul(5).rotate_left(7).wrapping_mul(9);
        let t = s[1] << 17;
        s[2] ^= s[0];
        s[3] ^= s[1];
        s[1] ^= s[2];
        s[0] ^= s[3];
        s[2] ^= t;
        s[3] = s[3].rotate_left(45);
        result
    }

    /// Uniform in `[0, 1)`, from the top 53 bits.
    pub fn uniform(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Uniform in `[lo, hi)`.
    pub fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.uniform()
    }

    /// A standard normal draw (mean 0, standard deviation 1).
    pub fn normal(&mut self) -> f64 {
        // 1 - uniform is in (0, 1], so the logarithm is finite.
        let radius = (-2.0 * (1.0 - self.uniform()).ln()).sqrt();
        radius * (std::f64::consts::TAU * self.uniform()).cos()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sequence_depends_only_on_the_seed() {
        let a: Vec<u64> = (0..8).scan(Rng::new(7), |rng, _| Some(rng.next_u64())).collect();
        let b: Vec<u64> = (0..8).scan(Rng::new(7), |rng, _| Some(rng.next_u64())).collect();
        let c: Vec<u64> = (0..8).scan(Rng::new(8), |rng, _| Some(rng.next_u64())).collect();
        assert_eq!(a, b);
        assert_ne!(a, c);
        // SplitMix64 seeding: even seed 0 gives a non-zero state.
        assert_ne!(Rng::new(0).next_u64(), 0);
    }

    #[test]
    fn uniform_and_normal_draws_have_the_right_moments() {
        let mut rng = Rng::new(42);
        let n = 100_000;
        let u: Vec<f64> = (0..n).map(|_| rng.uniform()).collect();
        assert!(u.iter().all(|v| (0.0..1.0).contains(v)));
        let mean = u.iter().sum::<f64>() / f64::from(n);
        assert!((mean - 0.5).abs() < 0.01, "{mean}");
        let z: Vec<f64> = (0..n).map(|_| rng.normal()).collect();
        let mean = z.iter().sum::<f64>() / f64::from(n);
        let var = z.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / f64::from(n);
        assert!(mean.abs() < 0.02 && (var - 1.0).abs() < 0.02, "{mean} {var}");
        assert!((0..1000).map(|_| rng.range(-2.0, 3.0)).all(|v| (-2.0..3.0).contains(&v)));
    }
}
