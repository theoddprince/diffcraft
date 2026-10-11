//! Picture comparison: per-pixel differences with a tolerance.

use std::path::Path;

pub use ::image::RgbaImage;
use rayon::prelude::*;

use crate::{Error, Result};

/// Decoder memory cap (a 16k × 16k RGBA image is 1 GiB).
pub const MAX_DECODE_BYTES: u64 = 1024 * 1024 * 1024;

pub const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "gif", "bmp", "webp", "tif", "tiff", "ico", "tga"];

pub fn is_image_path(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| IMAGE_EXTENSIONS.iter().any(|x| x.eq_ignore_ascii_case(e)))
}

/// Decode an image file to RGBA8.
pub fn load(path: &Path) -> Result<RgbaImage> {
    let err = |message: String| Error::Image { path: path.to_path_buf(), message };
    let mut reader = ::image::ImageReader::open(path).map_err(|e| Error::io(path, e))?.with_guessed_format().map_err(|e| Error::io(path, e))?;
    let mut limits = ::image::Limits::default();
    limits.max_alloc = Some(MAX_DECODE_BYTES);
    reader.limits(limits);
    let img = reader.decode().map_err(|e| err(e.to_string()))?;
    Ok(img.to_rgba8())
}

/// Result of [`compare`].
#[derive(Clone, Debug)]
pub struct ImageDiff {
    pub width: u32,
    pub height: u32,
    pub differing_pixels: u64,
    pub total_pixels: u64,
    /// Largest per-channel difference found.
    pub max_delta: u8,
    /// Visualisation: differing pixels in red (stronger = larger difference), the rest as a
    /// faded greyscale of the left image.
    pub mask: RgbaImage,
}

impl ImageDiff {
    pub fn is_identical(&self) -> bool {
        self.differing_pixels == 0
    }

    pub fn differing_percent(&self) -> f64 {
        if self.total_pixels == 0 { 0.0 } else { self.differing_pixels as f64 * 100.0 / self.total_pixels as f64 }
    }
}

/// Compare pixel by pixel on a canvas large enough for both. A pixel differs when any channel
/// differs by more than `tolerance`; pixels outside one image always differ.
pub fn compare(a: &RgbaImage, b: &RgbaImage, tolerance: u8) -> ImageDiff {
    let width = a.width().max(b.width());
    let height = a.height().max(b.height());
    let mut mask = RgbaImage::new(width, height);
    let row_bytes = width as usize * 4;
    let (diff, max_delta) = if row_bytes == 0 {
        (0u64, 0u8)
    } else {
        mask.par_chunks_mut(row_bytes)
            .enumerate()
            .map(|(y, row)| {
                let y = y as u32;
                let mut count = 0u64;
                let mut max = 0u8;
                let (pixels, _) = row.as_chunks_mut::<4>();
                for (x, px) in pixels.iter_mut().enumerate() {
                    let x = x as u32;
                    let pa = (x < a.width() && y < a.height()).then(|| a.get_pixel(x, y).0);
                    let pb = (x < b.width() && y < b.height()).then(|| b.get_pixel(x, y).0);
                    let (out, delta) = match (pa, pb) {
                        (Some(p), Some(q)) => {
                            let d = p.iter().zip(q.iter()).map(|(u, v)| u.abs_diff(*v)).max().unwrap_or(0);
                            if d > tolerance {
                                let s = 96u16 + (d as u16 * 159 / 255);
                                ([255, 0, 0, s.min(255) as u8], d)
                            } else {
                                let g = ((p[0] as u16 * 30 + p[1] as u16 * 59 + p[2] as u16 * 11) / 100) as u8;
                                let g = 128 + g / 2;
                                ([g, g, g, 255], d)
                            }
                        }
                        (Some(_), None) | (None, Some(_)) => ([255, 160, 0, 255], 255),
                        (None, None) => ([0, 0, 0, 0], 0),
                    };
                    *px = out;
                    max = max.max(delta);
                    if delta > tolerance {
                        count += 1;
                    }
                }
                (count, max)
            })
            .reduce(|| (0, 0), |x, y| (x.0 + y.0, x.1.max(y.1)))
    };
    ImageDiff { width, height, differing_pixels: diff, total_pixels: width as u64 * height as u64, max_delta, mask }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::image::Rgba;

    #[test]
    fn tolerance_and_sizes() {
        let a = RgbaImage::from_pixel(4, 4, Rgba([10, 10, 10, 255]));
        let mut b = a.clone();
        b.put_pixel(1, 1, Rgba([14, 10, 10, 255]));
        assert_eq!(compare(&a, &b, 0).differing_pixels, 1);
        assert!(compare(&a, &b, 4).is_identical());
        let c = RgbaImage::from_pixel(5, 4, Rgba([10, 10, 10, 255]));
        let d = compare(&a, &c, 0);
        assert_eq!((d.width, d.height, d.differing_pixels), (5, 4, 4));
        assert!((d.differing_percent() - 20.0).abs() < 1e-9);
    }

    #[test]
    fn empty_images() {
        let e = RgbaImage::new(0, 0);
        assert!(compare(&e, &e, 0).is_identical());
    }

    #[test]
    fn load_round_trip_and_errors() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.png");
        RgbaImage::from_pixel(3, 2, Rgba([1, 2, 3, 255])).save(&p).unwrap();
        assert_eq!(load(&p).unwrap().dimensions(), (3, 2));
        let bad = dir.path().join("bad.png");
        std::fs::write(&bad, b"not a png").unwrap();
        assert!(load(&bad).is_err());
        assert!(is_image_path(Path::new("X.JPG")));
    }
}
