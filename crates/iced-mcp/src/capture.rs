//! Window screenshot → cropped, downscaled PNG.

use base64::Engine as _;
use iced::window::Screenshot as Shot;
use iced_mcp_protocol::{Rect, Screenshot, ScreenshotParams};

/// Encodes `shot` as the request asks. Runs off the UI thread: a full-size
/// Retina frame takes long enough to stutter a frame.
pub(crate) fn encode(shot: &Shot, params: &ScreenshotParams) -> Result<Screenshot, String> {
    let scale = shot.scale_factor.max(0.01);
    let full_w = shot.size.width;
    let full_h = shot.size.height;
    let logical_w = full_w as f32 / scale;
    let logical_h = full_h as f32 / scale;

    // The region in logical pixels, clamped to the window.
    let region = params.region.unwrap_or(Rect {
        x: 0.0,
        y: 0.0,
        width: logical_w,
        height: logical_h,
    });
    let x0 = region.x.clamp(0.0, logical_w);
    let y0 = region.y.clamp(0.0, logical_h);
    let x1 = (region.x + region.width).clamp(x0, logical_w);
    let y1 = (region.y + region.height).clamp(y0, logical_h);
    let region = Rect {
        x: x0,
        y: y0,
        width: x1 - x0,
        height: y1 - y0,
    };
    if region.width < 1.0 || region.height < 1.0 {
        return Err("the region is outside the window".into());
    }

    // The same region in physical pixels.
    let px = |v: f32, max: u32| ((v * scale).round() as u32).min(max);
    let (sx0, sy0) = (px(x0, full_w), px(y0, full_h));
    let (sx1, sy1) = (px(x1, full_w).max(sx0 + 1), px(y1, full_h).max(sy0 + 1));
    let (src_w, src_h) = (sx1 - sx0, sy1 - sy0);

    let max_width = params.max_width.unwrap_or(src_w).clamp(16, src_w.max(16));
    let out_w = src_w.min(max_width);
    let out_h = ((src_h as u64 * out_w as u64) / src_w as u64).max(1) as u32;

    let rgba = shot.rgba.as_ref();
    let stride = full_w as usize * 4;
    let mut out = vec![0_u8; out_w as usize * out_h as usize * 4];
    let fx = src_w as f32 / out_w as f32;
    let fy = src_h as f32 / out_h as f32;

    // Box filter: each output pixel averages the source block it covers.
    for oy in 0..out_h {
        let ya = sy0 + (oy as f32 * fy) as u32;
        let yb = (sy0 + ((oy + 1) as f32 * fy) as u32).clamp(ya + 1, sy1);
        for ox in 0..out_w {
            let xa = sx0 + (ox as f32 * fx) as u32;
            let xb = (sx0 + ((ox + 1) as f32 * fx) as u32).clamp(xa + 1, sx1);
            let mut sum = [0_u32; 4];
            let mut count = 0_u32;
            for y in ya..yb {
                let row = y as usize * stride;
                for x in xa..xb {
                    let i = row + x as usize * 4;
                    if let Some(px) = rgba.get(i..i + 4) {
                        for (s, p) in sum.iter_mut().zip(px) {
                            *s += u32::from(*p);
                        }
                        count += 1;
                    }
                }
            }
            let o = (oy as usize * out_w as usize + ox as usize) * 4;
            if let Some(dst) = out.get_mut(o..o + 4) {
                for (d, s) in dst.iter_mut().zip(sum) {
                    *d = (s / count.max(1)) as u8;
                }
            }
        }
    }

    let mut png_bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut png_bytes, out_w, out_h);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
        writer.write_image_data(&out).map_err(|e| e.to_string())?;
    }

    Ok(Screenshot {
        png_base64: base64::engine::general_purpose::STANDARD.encode(png_bytes),
        width: out_w,
        height: out_h,
        logical_per_pixel: region.width / out_w as f32,
        region,
    })
}
