//! The app icon, as SVG, PNG, ICO (Windows) and ICNS (macOS), drawn from a few flat shapes so that packaging needs
//! no image tools: a placeholder (six translucent petals over a dark tile) until a designed icon replaces it. The
//! SVG and the rasters come from the same shapes, so they always agree.

use std::fmt::Write as _;

/// The shapes' coordinate space: a 1024-unit square, as macOS draws its icons.
const CANVAS: f64 = 1024.0;

#[derive(Clone, Copy, Debug)]
enum Shape {
    /// A rectangle from (x0, y0) to (x1, y1) with corners of the given radius.
    RoundedRect { x0: f64, y0: f64, x1: f64, y1: f64, radius: f64 },
    /// An ellipse with semi-axes `rx` and `ry`, turned clockwise by `degrees` about its center.
    Ellipse { cx: f64, cy: f64, rx: f64, ry: f64, degrees: f64 },
}

#[derive(Clone, Copy, Debug)]
struct Layer {
    shape: Shape,
    rgb: [u8; 3],
    opacity: f64,
}

/// Bottom to top.
fn layers() -> Vec<Layer> {
    let mut layers =
        vec![Layer { shape: Shape::RoundedRect { x0: 64.0, y0: 64.0, x1: 960.0, y1: 960.0, radius: 200.0 }, rgb: [0x1d, 0x4d, 0x3f], opacity: 1.0 }];
    // Petals overlap where they meet, so that the flower reads as layers stacked on one another.
    for k in 0..6_u8 {
        let degrees = f64::from(k) * 60.0 - 90.0;
        let (sin, cos) = degrees.to_radians().sin_cos();
        layers.push(Layer {
            shape: Shape::Ellipse { cx: 512.0 + 172.0 * cos, cy: 512.0 + 172.0 * sin, rx: 172.0, ry: 92.0, degrees },
            rgb: if k % 2 == 0 { [0xf6, 0xd8, 0xe6] } else { [0xe9, 0xb8, 0xd4] },
            opacity: 0.82,
        });
    }
    layers.push(Layer { shape: Shape::Ellipse { cx: 512.0, cy: 512.0, rx: 98.0, ry: 98.0, degrees: 0.0 }, rgb: [0xff, 0xcf, 0x56], opacity: 1.0 });
    layers
}

impl Shape {
    fn contains(self, x: f64, y: f64) -> bool {
        match self {
            Shape::RoundedRect { x0, y0, x1, y1, radius } => {
                let dx = x - x.clamp(x0 + radius, x1 - radius);
                let dy = y - y.clamp(y0 + radius, y1 - radius);
                (x0..=x1).contains(&x) && (y0..=y1).contains(&y) && dx * dx + dy * dy <= radius * radius
            }
            Shape::Ellipse { cx, cy, rx, ry, degrees } => {
                let (sin, cos) = degrees.to_radians().sin_cos();
                let (dx, dy) = (x - cx, y - cy);
                // Into the ellipse's own axes: turned back by its angle.
                let (u, v) = (dx * cos + dy * sin, -dx * sin + dy * cos);
                (u / rx).powi(2) + (v / ry).powi(2) <= 1.0
            }
        }
    }
}

/// The icon as SVG, for Linux desktops (`hicolor/scalable`).
pub fn svg() -> String {
    let mut svg = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"1024\" height=\"1024\" viewBox=\"0 0 1024 1024\">\n",
    );
    for layer in layers() {
        let [r, g, b] = layer.rgb;
        let paint = format!("fill=\"#{r:02x}{g:02x}{b:02x}\" fill-opacity=\"{}\"", layer.opacity);
        let _ = match layer.shape {
            Shape::RoundedRect { x0, y0, x1, y1, radius } => {
                writeln!(svg, "  <rect x=\"{x0}\" y=\"{y0}\" width=\"{}\" height=\"{}\" rx=\"{radius}\" {paint}/>", x1 - x0, y1 - y0)
            }
            Shape::Ellipse { cx, cy, rx, ry, degrees } => writeln!(
                svg,
                "  <ellipse cx=\"{cx:.2}\" cy=\"{cy:.2}\" rx=\"{rx}\" ry=\"{ry}\" transform=\"rotate({degrees} {cx:.2} {cy:.2})\" {paint}/>"
            ),
        };
    }
    svg.push_str("</svg>\n");
    svg
}

/// The icon as `size` x `size` RGBA pixels (straight alpha), each pixel the average of a grid of samples.
pub fn rgba(size: u32) -> Vec<u8> {
    let layers = layers();
    // Enough samples for smooth edges at every size: 8 x 8 for the small icons, 2 x 2 at 1024.
    let grid = (2048 / size.max(1)).clamp(2, 8);
    let scale = CANVAS / f64::from(size);
    let samples = f64::from(grid * grid);
    let mut pixels = Vec::with_capacity(4 * (size as usize).pow(2));
    for py in 0..size {
        for px in 0..size {
            // Premultiplied red, green, blue and alpha, summed over the samples.
            let mut sum = [0.0_f64; 4];
            for sy in 0..grid {
                for sx in 0..grid {
                    let x = (f64::from(px) + (f64::from(sx) + 0.5) / f64::from(grid)) * scale;
                    let y = (f64::from(py) + (f64::from(sy) + 0.5) / f64::from(grid)) * scale;
                    let mut color = [0.0_f64; 4];
                    for layer in layers.iter().filter(|layer| layer.shape.contains(x, y)) {
                        let a = layer.opacity;
                        for (c, &channel) in color.iter_mut().zip(layer.rgb.iter()) {
                            *c = f64::from(channel) / 255.0 * a + *c * (1.0 - a);
                        }
                        color[3] = a + color[3] * (1.0 - a);
                    }
                    for (s, c) in sum.iter_mut().zip(color) {
                        *s += c;
                    }
                }
            }
            let alpha = sum[3] / samples;
            for &channel in &sum[..3] {
                let straight = if alpha > 0.0 { channel / samples / alpha } else { 0.0 };
                pixels.push(to_byte(straight));
            }
            pixels.push(to_byte(alpha));
        }
    }
    pixels
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // Clamped to [0, 255] first.
fn to_byte(value: f64) -> u8 {
    (value * 255.0).round().clamp(0.0, 255.0) as u8
}

/// The icon as a PNG file.
pub fn png(size: u32) -> Result<Vec<u8>, String> {
    let mut file = Vec::new();
    let mut encoder = png::Encoder::new(&mut file, size, size);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(|e| format!("cannot encode the icon: {e}"))?;
    writer.write_image_data(&rgba(size)).map_err(|e| format!("cannot encode the icon: {e}"))?;
    writer.finish().map_err(|e| format!("cannot encode the icon: {e}"))?;
    Ok(file)
}

/// A Windows icon file: PNG images at the sizes Explorer and the Start menu ask for (PNG entries need Vista or later).
pub fn ico() -> Result<Vec<u8>, String> {
    let images = [16_u32, 24, 32, 48, 64, 256].into_iter().map(|size| png(size).map(|png| (size, png))).collect::<Result<Vec<_>, _>>()?;
    let count = u16::try_from(images.len()).map_err(|_| "too many icon sizes")?;
    let mut file = Vec::new();
    file.extend_from_slice(&[0, 0, 1, 0]); // Reserved, then type 1: an icon.
    file.extend_from_slice(&count.to_le_bytes());
    let mut offset = 6 + 16 * images.len();
    for (size, png) in &images {
        // A size of 256 is written as 0.
        let side = u8::try_from(*size).unwrap_or(0);
        file.extend_from_slice(&[side, side, 0, 0]); // Width, height, no palette, reserved.
        file.extend_from_slice(&1_u16.to_le_bytes()); // Color planes.
        file.extend_from_slice(&32_u16.to_le_bytes()); // Bits per pixel.
        file.extend_from_slice(&u32::try_from(png.len()).map_err(|_| "icon image too large")?.to_le_bytes());
        file.extend_from_slice(&u32::try_from(offset).map_err(|_| "icon too large")?.to_le_bytes());
        offset += png.len();
    }
    for (_, png) in images {
        file.extend_from_slice(&png);
    }
    Ok(file)
}

/// A macOS icon file: PNG images under the element types for 16 to 1024 pixels.
pub fn icns() -> Result<Vec<u8>, String> {
    let mut elements = Vec::new();
    for (kind, size) in [(b"icp4", 16_u32), (b"icp5", 32), (b"icp6", 64), (b"ic07", 128), (b"ic08", 256), (b"ic09", 512), (b"ic10", 1024)] {
        let png = png(size)?;
        elements.extend_from_slice(kind);
        elements.extend_from_slice(&u32::try_from(8 + png.len()).map_err(|_| "icon image too large")?.to_be_bytes());
        elements.extend_from_slice(&png);
    }
    let mut file = b"icns".to_vec();
    file.extend_from_slice(&u32::try_from(8 + elements.len()).map_err(|_| "icon too large")?.to_be_bytes());
    file.extend_from_slice(&elements);
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(png: &[u8]) -> (u32, Vec<u8>) {
        let decoder = png::Decoder::new(std::io::Cursor::new(png));
        let mut reader = decoder.read_info().unwrap();
        let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut pixels).unwrap();
        assert_eq!((info.color_type, info.bit_depth), (png::ColorType::Rgba, png::BitDepth::Eight));
        assert_eq!(info.width, info.height);
        (info.width, pixels)
    }

    fn pixel(pixels: &[u8], size: u32, x: u32, y: u32) -> [u8; 4] {
        let i = 4 * (y * size + x) as usize;
        pixels[i..i + 4].try_into().unwrap()
    }

    #[test]
    fn the_rasters_show_the_shapes() {
        let (size, pixels) = decode(&png(64).unwrap());
        assert_eq!(size, 64);
        assert_eq!(pixel(&pixels, 64, 0, 0)[3], 0, "the corner outside the tile is transparent");
        assert_eq!(pixel(&pixels, 64, 32, 32), [0xff, 0xcf, 0x56, 255], "the opaque center");
        let tile = pixel(&pixels, 64, 7, 32);
        assert_eq!(tile, [0x1d, 0x4d, 0x3f, 255], "the tile's left edge, outside the petals");
        // The tile's rounded corner crosses this pixel: partly covered.
        let corner = pixel(&pixels, 64, 7, 7);
        assert!(corner[3] > 0 && corner[3] < 255, "{corner:?}");
    }

    #[test]
    fn the_containers_hold_their_images() {
        let ico = ico().unwrap();
        assert_eq!(&ico[..6], &[0, 0, 1, 0, 6, 0]);
        // The last entry is the 256-pixel image, written as 0 x 0; its offset and length point at a PNG.
        let entry = &ico[6 + 16 * 5..6 + 16 * 6];
        assert_eq!(&entry[..2], &[0, 0]);
        let length = u32::from_le_bytes(entry[8..12].try_into().unwrap()) as usize;
        let offset = u32::from_le_bytes(entry[12..16].try_into().unwrap()) as usize;
        assert_eq!(offset + length, ico.len());
        assert_eq!(decode(&ico[offset..]).0, 256);

        let icns = icns().unwrap();
        assert_eq!(&icns[..4], b"icns");
        assert_eq!(u32::from_be_bytes(icns[4..8].try_into().unwrap()) as usize, icns.len());
        let mut at = 8;
        let mut sizes = Vec::new();
        while at < icns.len() {
            let length = u32::from_be_bytes(icns[at + 4..at + 8].try_into().unwrap()) as usize;
            sizes.push(decode(&icns[at + 8..at + length]).0);
            at += length;
        }
        assert_eq!(at, icns.len());
        assert_eq!(sizes, [16, 32, 64, 128, 256, 512, 1024]);
    }

    #[test]
    fn the_svg_draws_every_layer() {
        let svg = svg();
        assert!(svg.starts_with("<?xml") && svg.trim_end().ends_with("</svg>"));
        assert_eq!(svg.matches("<ellipse").count() + svg.matches("<rect").count(), layers().len());
    }
}
