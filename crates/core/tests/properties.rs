//! Property tests (proptest) of florastack-core's conversions, tiled view and tile cache: round trips that must be
//! exact for any image, whatever its size, channel count, row padding and sample type.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::HashMap;

use florastack_core::ops::{BlurMethod, Border, Interpolation, gaussian_blur_in_place, warp};
use florastack_core::{Affine, ColorSpace, Encoding, Image, MemoryStore, Sample, TileCache, TileGrid, TileKey};
use proptest::prelude::*;

/// Images with 1 to `max_side` pixels a side, 1 to 4 channels, up to 3 samples of row padding and either encoding,
/// with samples from `sample`.
fn image_of<T: Sample>(sample: impl Strategy<Value = T> + Clone + 'static, max_side: usize) -> impl Strategy<Value = Image<T>> {
    (1..=max_side, 1..=max_side, 1usize..=4, 0usize..=3, any::<bool>()).prop_flat_map(move |(w, h, c, pad, srgb)| {
        let stride = w * c + pad;
        let len = stride * (h - 1) + w * c;
        let color = if srgb { ColorSpace::SRGB } else { ColorSpace::LINEAR };
        prop::collection::vec(sample.clone(), len).prop_map(move |data| Image::from_vec_with_stride(w, h, c, stride, color.clone(), data).unwrap())
    })
}

/// A copy of `image` with packed rows (no padding), for comparing buffers.
fn samples<T: Sample>(image: &Image<T>) -> Vec<T> {
    image.rows().flatten().copied().collect()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(96))]

    #[test]
    fn u8_images_survive_linear_f32_exactly(image in image_of(any::<u8>(), 24)) {
        let encoding = image.color_space().encoding;
        let linear = image.to_linear_f32().unwrap();
        prop_assert_eq!(linear.color_space().encoding, Encoding::Linear);
        prop_assert_eq!(&linear.to_encoding::<u8>(encoding).unwrap(), &image);
        prop_assert_eq!(&image.convert::<f32>().unwrap().convert::<u8>().unwrap(), &image);
        prop_assert_eq!(&image.convert::<u16>().unwrap().convert::<u8>().unwrap(), &image);
    }

    #[test]
    fn u16_images_survive_linear_f32_exactly(image in image_of(any::<u16>(), 24)) {
        let encoding = image.color_space().encoding;
        let linear = image.to_linear_f32().unwrap();
        prop_assert_eq!(&linear.to_encoding::<u16>(encoding).unwrap(), &image);
        prop_assert_eq!(&image.convert::<f32>().unwrap().convert::<u16>().unwrap(), &image);
    }

    #[test]
    fn f32_images_survive_an_srgb_round_trip(image in image_of(-1.0f32..4.0, 24)) {
        let mut linear = image.clone();
        linear.set_color_space(ColorSpace::LINEAR);
        let back = linear.to_encoding::<f32>(Encoding::Srgb).unwrap().to_linear_f32().unwrap();
        for (a, b) in samples(&back).iter().zip(samples(&linear)) {
            prop_assert!((a - b).abs() <= 1e-6 + 2e-6 * b.abs(), "{} -> {}", b, a);
        }
        // In place, the same.
        let mut in_place = linear.clone();
        in_place.convert_encoding(Encoding::Srgb);
        in_place.convert_encoding(Encoding::Linear);
        prop_assert_eq!(samples(&in_place), samples(&back));
    }

    #[test]
    fn split_then_stitch_is_the_identity(
        image in image_of(any::<u16>(), 60),
        tile_size in 1usize..32,
        overlap_seed in 0usize..1000,
    ) {
        let overlap = overlap_seed % (tile_size / 2 + 1);
        let grid = TileGrid::for_image(&image, tile_size, overlap).unwrap();
        let tiles = grid.split(&image).unwrap();
        prop_assert_eq!(tiles.len(), grid.len());
        for (tile, pixels) in grid.tiles().zip(&tiles) {
            prop_assert_eq!((pixels.width(), pixels.height()), (tile.outer.width, tile.outer.height));
        }
        prop_assert_eq!(&grid.stitch(&tiles).unwrap(), &image);
        let streamed = grid.stitch_with(|i| Ok::<_, florastack_core::TileError>(tiles[i].clone())).unwrap();
        prop_assert_eq!(&streamed, &image);
    }

    #[test]
    fn float_tiles_stitch_back_bit_for_bit(image in image_of(-2.0f32..2.0, 50), tile_size in 2usize..24, overlap in 0usize..12) {
        let overlap = overlap.min(tile_size / 2);
        let grid = TileGrid::for_image(&image, tile_size, overlap).unwrap();
        let stitched = grid.stitch(&grid.split(&image).unwrap()).unwrap();
        let bits = |i: &Image<f32>| samples(i).iter().map(|v| v.to_bits()).collect::<Vec<_>>();
        prop_assert_eq!(bits(&stitched), bits(&image));
    }

    /// Rows and columns are blurred alike: blurring the transposed image gives the transposed blur, whatever the size,
    /// sigma and method (up to the rounding between the two passes, which run in the other order).
    #[test]
    fn blurring_commutes_with_transposition(image in image_of(0.0f32..1.0, 40), sigma in 0.0f64..20.0, passes in 0usize..=5) {
        let method = if passes == 0 { BlurMethod::Exact } else { BlurMethod::Boxes { passes } };
        let transposed = Image::from_fn(image.height(), image.width(), image.channels(), ColorSpace::LINEAR, |x, y, c| {
            image.get(y, x, c).unwrap()
        })
        .unwrap();
        let (mut a, mut b) = (image.clone(), transposed);
        gaussian_blur_in_place(&mut a, sigma, method).unwrap();
        gaussian_blur_in_place(&mut b, sigma, method).unwrap();
        for y in 0..image.height() {
            for x in 0..image.width() {
                for c in 0..image.channels() {
                    let (p, q) = (a.get(x, y, c).unwrap(), b.get(y, x, c).unwrap());
                    prop_assert!((p - q).abs() < 2e-5, "{:?} sigma {}: ({}, {}, {}) {} vs {}", method, sigma, x, y, c, p, q);
                    prop_assert!((-1e-5..=1.0 + 1e-5).contains(&p), "a blur of samples in [0, 1] stays in [0, 1]: {}", p);
                }
            }
        }
    }

    #[test]
    fn whole_pixel_moves_copy_samples_exactly(image in image_of(-1.0f32..1.0, 20), dx in -3i32..=3, dy in -3i32..=3) {
        let t = Affine::translation(f64::from(dx), f64::from(dy));
        for interpolation in [Interpolation::Bilinear, Interpolation::Bicubic] {
            let moved = warp(&image, &t, interpolation, Border::Constant(9.0)).unwrap();
            for y in 0..image.height() {
                for x in 0..image.width() {
                    let sx = x.checked_add_signed(isize::try_from(dx).unwrap()).filter(|&v| v < image.width());
                    let sy = y.checked_add_signed(isize::try_from(dy).unwrap()).filter(|&v| v < image.height());
                    let expected: Vec<f32> = match (sx, sy) {
                        (Some(sx), Some(sy)) => image.pixel(sx, sy).unwrap().to_vec(),
                        _ => vec![9.0; image.channels()],
                    };
                    prop_assert_eq!(moved.pixel(x, y).unwrap(), &expected[..]);
                }
            }
        }
    }
}

/// One step of the tile cache's model test.
#[derive(Clone, Debug)]
enum Op {
    Put(u8, u8),
    Get(u8),
    Change(u8, f32),
    Take(u8),
    Remove(u8),
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        3 => (0u8..6, 1u8..5).prop_map(|(k, side)| Op::Put(k, side)),
        3 => (0u8..6).prop_map(Op::Get),
        1 => (0u8..6, -1.0f32..1.0).prop_map(|(k, v)| Op::Change(k, v)),
        1 => (0u8..6).prop_map(Op::Take),
        1 => (0u8..6).prop_map(Op::Remove),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// Whatever the budget, the cache behaves like a map, and the tiles in memory never exceed the budget, or the last
    /// tile used when that one alone is larger.
    #[test]
    fn the_tile_cache_behaves_like_a_map(ops in prop::collection::vec(op(), 1..60), budget in 0usize..400) {
        let mut cache = TileCache::new(MemoryStore::new(), budget);
        let mut model: HashMap<u8, Image<f32>> = HashMap::new();
        let key = |k: u8| TileKey::new(1, u64::from(k));
        let mut serial = 0.0f32;
        let mut bound = budget;
        for op in ops {
            let mut last_used = None;
            match op {
                Op::Put(k, side) => {
                    serial += 1.0;
                    let s = usize::from(side);
                    let image = Image::from_fn(s, s, 1, ColorSpace::LINEAR, |x, y, _| serial + (x + 10 * y) as f32 / 100.0).unwrap();
                    cache.put(key(k), image.clone()).unwrap();
                    last_used = Some(image.as_raw().len() * 4);
                    model.insert(k, image);
                }
                Op::Get(k) => {
                    let got = cache.get::<f32>(key(k)).unwrap().cloned();
                    prop_assert_eq!(got.as_ref(), model.get(&k));
                    last_used = got.map(|g| g.as_raw().len() * 4);
                }
                Op::Change(k, v) => {
                    if let Some(tile) = cache.get_mut::<f32>(key(k)).unwrap() {
                        tile.pixel_mut(0, 0).unwrap()[0] = v;
                        last_used = Some(tile.as_raw().len() * 4);
                        model.get_mut(&k).unwrap().pixel_mut(0, 0).unwrap()[0] = v;
                    } else {
                        prop_assert!(!model.contains_key(&k));
                    }
                }
                Op::Take(k) => prop_assert_eq!(cache.take::<f32>(key(k)).unwrap(), model.remove(&k)),
                Op::Remove(k) => prop_assert_eq!(cache.remove(key(k)), model.remove(&k).is_some()),
            }
            prop_assert_eq!(cache.len(), model.len());
            if let Some(bytes) = last_used {
                bound = budget.max(bytes);
            }
            prop_assert!(cache.resident_bytes() <= bound, "{} > {}", cache.resident_bytes(), bound);
        }
        for (k, image) in &model {
            prop_assert_eq!(cache.get::<f32>(key(*k)).unwrap(), Some(image));
        }
    }
}
