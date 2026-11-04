//! Visual regression: compare a screenshot with a baseline, find where they
//! differ, and draw it.
//!
//! The method follows Argent's `screenshot-diff`: a pixel differs when its
//! RGB distance exceeds a tolerance; differing pixels are grouped into
//! regions on a coarse grid; the result says how much changed and where, and
//! comes with a diff image (brighter pixels green, darker red, regions boxed
//! in yellow over a dimmed copy of the new screenshot).

use std::io::Cursor;

/// An RGBA8 image.
pub(crate) struct Image {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) rgba: Vec<u8>,
}

/// The grid that groups changed pixels into regions, in pixels.
const CELL: u32 = 8;
/// A cell needs this many changed pixels to count (antialiasing noise).
const MIN_CELL_PIXELS: u32 = 2;
/// Regions listed in the report; the image shows all of them.
pub(crate) const MAX_REGIONS: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Status {
    Unchanged,
    Changed,
    DimensionMismatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Region {
    pub(crate) x: u32,
    pub(crate) y: u32,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) changed: u32,
}

pub(crate) struct Comparison {
    pub(crate) status: Status,
    pub(crate) changed: u64,
    pub(crate) total: u64,
    /// Largest first.
    pub(crate) regions: Vec<Region>,
    /// Full-size diff image (empty on a dimension mismatch).
    pub(crate) diff: Option<Image>,
}

impl Comparison {
    pub(crate) fn mismatch_percent(&self) -> f64 {
        if self.total == 0 {
            return 0.0;
        }
        self.changed as f64 * 100.0 / self.total as f64
    }

    /// "minor localized" … "large broad", as Argent words it.
    pub(crate) fn severity(&self) -> String {
        let pct = self.mismatch_percent();
        let size = match pct {
            p if p < 5.0 => "minor",
            p if p < 10.0 => "moderate",
            p if p < 20.0 => "significant",
            _ => "large",
        };
        let spread = if self.regions.len() <= 5 {
            "localized"
        } else {
            "broad"
        };
        format!("{size} {spread}")
    }
}

pub(crate) fn decode(png_bytes: &[u8]) -> Result<Image, String> {
    let decoder = png::Decoder::new(Cursor::new(png_bytes));
    let mut reader = decoder.read_info().map_err(|e| format!("not a PNG: {e}"))?;
    let mut buffer = vec![0; reader.output_buffer_size()];
    let info = reader
        .next_frame(&mut buffer)
        .map_err(|e| format!("cannot read the PNG: {e}"))?;
    buffer.truncate(info.buffer_size());
    let rgba = match (info.color_type, info.bit_depth) {
        (png::ColorType::Rgba, png::BitDepth::Eight) => buffer,
        (png::ColorType::Rgb, png::BitDepth::Eight) => buffer
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|&[r, g, b]| [r, g, b, 255])
            .collect(),
        other => {
            return Err(format!(
                "unsupported PNG format {other:?}: expected 8-bit RGB(A)"
            ))
        }
    };
    Ok(Image {
        width: info.width,
        height: info.height,
        rgba,
    })
}

pub(crate) fn encode(image: &Image) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, image.width, image.height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
        writer
            .write_image_data(&image.rgba)
            .map_err(|e| e.to_string())?;
    }
    Ok(out)
}

/// Compares `current` with `baseline`. `tolerance` is the fraction (0–1) of
/// the largest RGB distance below which a pixel counts as unchanged.
pub(crate) fn compare(baseline: &Image, current: &Image, tolerance: f32) -> Comparison {
    if baseline.width != current.width || baseline.height != current.height {
        return Comparison {
            status: Status::DimensionMismatch,
            changed: 0,
            total: 0,
            regions: Vec::new(),
            diff: None,
        };
    }
    let (w, h) = (current.width, current.height);
    let max_distance = 3.0 * 255.0 * 255.0;
    let limit = f64::from(tolerance.clamp(0.0, 1.0)).powi(2) * max_distance;

    let (cols, rows) = (w.div_ceil(CELL), h.div_ceil(CELL));
    let mut cells = vec![0_u32; (cols * rows) as usize];
    let mut diff = Image {
        width: w,
        height: h,
        rgba: vec![0; current.rgba.len()],
    };
    let mut changed = 0_u64;
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            let (a, b) = (&baseline.rgba[i..i + 4], &current.rgba[i..i + 4]);
            let distance: f64 = (0..3)
                .map(|c| (f64::from(a[c]) - f64::from(b[c])).powi(2))
                .sum();
            let out = &mut diff.rgba[i..i + 4];
            if distance > limit {
                changed += 1;
                cells[((y / CELL) * cols + x / CELL) as usize] += 1;
                let brighter = luma(b) >= luma(a);
                out.copy_from_slice(if brighter {
                    &[40, 220, 90, 255]
                } else {
                    &[230, 50, 50, 255]
                });
            } else {
                // A dimmed greyscale of the new screenshot, for context.
                let grey = (luma(b) * 0.35 + 150.0) as u8;
                out.copy_from_slice(&[grey, grey, grey, 255]);
            }
        }
    }

    let mut regions = group(&cells, cols, rows, w, h);
    for region in &regions {
        outline(&mut diff, region, [250, 200, 20, 255]);
    }
    regions.sort_by_key(|r| std::cmp::Reverse(r.changed));

    Comparison {
        status: if changed == 0 {
            Status::Unchanged
        } else {
            Status::Changed
        },
        changed,
        total: u64::from(w) * u64::from(h),
        regions,
        diff: Some(diff),
    }
}

fn luma(p: &[u8]) -> f32 {
    0.299 * f32::from(p[0]) + 0.587 * f32::from(p[1]) + 0.114 * f32::from(p[2])
}

/// Connected groups of changed cells (8-neighbour), as pixel rectangles.
fn group(cells: &[u32], cols: u32, rows: u32, w: u32, h: u32) -> Vec<Region> {
    let mut seen = vec![false; cells.len()];
    let mut regions = Vec::new();
    for start in 0..cells.len() {
        if seen[start] || cells[start] < MIN_CELL_PIXELS {
            continue;
        }
        let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0, 0);
        let mut count = 0;
        let mut stack = vec![start];
        seen[start] = true;
        while let Some(cell) = stack.pop() {
            let (cx, cy) = (cell as u32 % cols, cell as u32 / cols);
            count += cells[cell];
            (x0, y0, x1, y1) = (x0.min(cx), y0.min(cy), x1.max(cx), y1.max(cy));
            for dy in -1_i64..=1 {
                for dx in -1_i64..=1 {
                    let (nx, ny) = (i64::from(cx) + dx, i64::from(cy) + dy);
                    if nx < 0 || ny < 0 || nx >= i64::from(cols) || ny >= i64::from(rows) {
                        continue;
                    }
                    let n = (ny as u32 * cols + nx as u32) as usize;
                    if !seen[n] && cells[n] >= MIN_CELL_PIXELS {
                        seen[n] = true;
                        stack.push(n);
                    }
                }
            }
        }
        let (px, py) = (x0 * CELL, y0 * CELL);
        regions.push(Region {
            x: px,
            y: py,
            width: ((x1 + 1) * CELL).min(w) - px,
            height: ((y1 + 1) * CELL).min(h) - py,
            changed: count,
        });
    }
    regions
}

fn outline(image: &mut Image, r: &Region, color: [u8; 4]) {
    const THICK: u32 = 2;
    let (w, h) = (image.width, image.height);
    let mut put = |x: u32, y: u32| {
        if x < w && y < h {
            let i = ((y * w + x) * 4) as usize;
            image.rgba[i..i + 4].copy_from_slice(&color);
        }
    };
    for t in 0..THICK {
        for x in r.x..r.x + r.width {
            put(x, r.y + t);
            put(x, (r.y + r.height).saturating_sub(1 + t));
        }
        for y in r.y..r.y + r.height {
            put(r.x + t, y);
            put((r.x + r.width).saturating_sub(1 + t), y);
        }
    }
}

/// A box-filtered copy at most `max_width` wide, for showing inline.
pub(crate) fn shrink(image: &Image, max_width: u32) -> Image {
    if image.width <= max_width {
        return Image {
            width: image.width,
            height: image.height,
            rgba: image.rgba.clone(),
        };
    }
    let out_w = max_width.max(1);
    let out_h =
        ((u64::from(image.height) * u64::from(out_w)) / u64::from(image.width)).max(1) as u32;
    let fx = image.width as f32 / out_w as f32;
    let fy = image.height as f32 / out_h as f32;
    let mut rgba = vec![0; (out_w * out_h * 4) as usize];
    for oy in 0..out_h {
        let (ya, yb) = (
            (oy as f32 * fy) as u32,
            (((oy + 1) as f32 * fy) as u32).clamp((oy as f32 * fy) as u32 + 1, image.height),
        );
        for ox in 0..out_w {
            let xa = (ox as f32 * fx) as u32;
            let xb = (((ox + 1) as f32 * fx) as u32).clamp(xa + 1, image.width);
            let mut sum = [0_u32; 4];
            let mut n = 0;
            for y in ya..yb {
                for x in xa..xb {
                    let i = ((y * image.width + x) * 4) as usize;
                    for (s, v) in sum.iter_mut().zip(&image.rgba[i..i + 4]) {
                        *s += u32::from(*v);
                    }
                    n += 1;
                }
            }
            let o = ((oy * out_w + ox) * 4) as usize;
            for (d, s) in rgba[o..o + 4].iter_mut().zip(sum) {
                *d = (s / n.max(1)) as u8;
            }
        }
    }
    Image {
        width: out_w,
        height: out_h,
        rgba,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: u32, h: u32, c: [u8; 4]) -> Image {
        Image {
            width: w,
            height: h,
            rgba: c
                .iter()
                .copied()
                .cycle()
                .take((w * h * 4) as usize)
                .collect(),
        }
    }

    fn paint(image: &mut Image, x0: u32, y0: u32, x1: u32, y1: u32, c: [u8; 4]) {
        for y in y0..y1 {
            for x in x0..x1 {
                let i = ((y * image.width + x) * 4) as usize;
                image.rgba[i..i + 4].copy_from_slice(&c);
            }
        }
    }

    #[test]
    fn identical_images_are_unchanged() {
        let a = solid(40, 30, [10, 20, 30, 255]);
        let b = solid(40, 30, [10, 20, 30, 255]);
        let result = compare(&a, &b, 0.1);
        assert_eq!(result.status, Status::Unchanged);
        assert!(result.regions.is_empty());
    }

    #[test]
    fn small_noise_is_within_tolerance() {
        let a = solid(16, 16, [100, 100, 100, 255]);
        let b = solid(16, 16, [104, 103, 101, 255]);
        assert_eq!(compare(&a, &b, 0.1).status, Status::Unchanged);
    }

    #[test]
    fn separate_changes_become_separate_regions() {
        let a = solid(100, 60, [255, 255, 255, 255]);
        let mut b = solid(100, 60, [255, 255, 255, 255]);
        paint(&mut b, 4, 4, 20, 12, [0, 0, 0, 255]);
        paint(&mut b, 70, 40, 90, 56, [0, 0, 0, 255]);
        let result = compare(&a, &b, 0.1);
        assert_eq!(result.status, Status::Changed);
        assert_eq!(result.regions.len(), 2);
        assert_eq!(result.changed, 16 * 8 + 20 * 16);
        assert_eq!(result.regions[0].changed, 320, "largest first");
        assert!(result.severity().starts_with("moderate localized"));
        let png = encode(result.diff.as_ref().unwrap()).unwrap();
        let back = decode(&png).unwrap();
        assert_eq!((back.width, back.height), (100, 60));
    }

    #[test]
    fn different_sizes_are_reported_not_compared() {
        let a = solid(10, 10, [0; 4]);
        let b = solid(12, 10, [0; 4]);
        assert_eq!(compare(&a, &b, 0.1).status, Status::DimensionMismatch);
    }

    #[test]
    fn shrinking_keeps_the_aspect_ratio() {
        let small = shrink(&solid(400, 200, [9, 9, 9, 255]), 100);
        assert_eq!((small.width, small.height), (100, 50));
        assert_eq!(&small.rgba[..4], &[9, 9, 9, 255]);
    }
}
