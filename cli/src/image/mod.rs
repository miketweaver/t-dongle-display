//! Image loading (PNG, baseline JPEG, BMP, PNM natively; anything else via
//! ImageMagick/ffmpeg if installed), scaling and RGB565 conversion.

mod bmp;
mod inflate;
mod jpeg;
mod png;
mod pnm;

use std::fs;
use std::io::Read;
use std::process::{Command, Stdio};

pub struct Rgba {
    pub w: u32,
    pub h: u32,
    pub px: Vec<u8>, // RGBA8, row-major
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Fit {
    Contain, // whole image visible, letterboxed
    Cover,   // fill the screen, crop the overflow
    Stretch, // fill the screen, ignore aspect ratio
}

pub fn read_input(path: &str) -> Result<Vec<u8>, String> {
    if path == "-" {
        let mut v = Vec::new();
        std::io::stdin()
            .read_to_end(&mut v)
            .map_err(|e| format!("stdin: {e}"))?;
        Ok(v)
    } else {
        fs::read(path).map_err(|e| format!("{path}: {e}"))
    }
}

/// Decodes `data` (from `path`, used for the external fallback), applying EXIF orientation.
pub fn load(path: &str, data: &[u8]) -> Result<Rgba, String> {
    let native: Result<Rgba, String> = if data.starts_with(b"\x89PNG") {
        png::decode(data)
    } else if data.starts_with(&[0xFF, 0xD8]) {
        jpeg::decode(data).map(|(img, o)| orient(img, o))
    } else if data.starts_with(b"BM") {
        bmp::decode(data)
    } else if data.len() > 2 && data[0] == b'P' && (b'1'..=b'6').contains(&data[1]) {
        pnm::decode(data)
    } else {
        Err("unrecognized format (supported natively: PNG, JPEG, BMP, PPM/PGM)".into())
    };
    match native {
        Ok(img) => Ok(img),
        Err(e) => external(path, data).map_err(|x| format!("{e}; {x}")),
    }
}

/// Converts with ImageMagick or ffmpeg (whichever is installed) to PNG, then decodes that.
fn external(path: &str, data: &[u8]) -> Result<Rgba, String> {
    let tmp;
    let file = if path == "-" {
        tmp = std::env::temp_dir().join(format!("tdongle-{}.img", std::process::id()));
        fs::write(&tmp, data).map_err(|e| e.to_string())?;
        tmp.to_string_lossy().to_string()
    } else {
        path.to_string()
    };
    let first_frame = format!("{file}[0]");
    let tries: [(&str, Vec<&str>); 3] = [
        (
            "magick",
            vec![first_frame.as_str(), "-auto-orient", "png:-"],
        ),
        (
            "convert",
            vec![first_frame.as_str(), "-auto-orient", "png:-"],
        ),
        (
            "ffmpeg",
            vec![
                "-v",
                "error",
                "-i",
                file.as_str(),
                "-frames:v",
                "1",
                "-f",
                "image2pipe",
                "-vcodec",
                "png",
                "-",
            ],
        ),
    ];
    let mut result = Err("install ImageMagick or ffmpeg to open this file".to_string());
    for (bin, args) in tries.iter() {
        match Command::new(bin)
            .args(args)
            .stdin(Stdio::null())
            .stderr(Stdio::piped())
            .output()
        {
            Ok(out) if out.status.success() && out.stdout.starts_with(b"\x89PNG") => {
                result = png::decode(&out.stdout);
                break;
            }
            Ok(out) => {
                let err = String::from_utf8_lossy(&out.stderr);
                result = Err(format!(
                    "{bin}: {}",
                    err.trim().lines().last().unwrap_or("conversion failed")
                ));
                break;
            }
            Err(_) => continue, // not installed
        }
    }
    if path == "-" {
        let _ = fs::remove_file(&file);
    }
    result
}

/// Applies an EXIF orientation (1..=8) so the image is upright.
pub fn orient(img: Rgba, o: u8) -> Rgba {
    if !(2..=8).contains(&o) {
        return img;
    }
    let (w, h) = (img.w as usize, img.h as usize);
    let (ow, oh) = if o >= 5 { (h, w) } else { (w, h) };
    let mut px = vec![0u8; w * h * 4];
    for y in 0..oh {
        for x in 0..ow {
            let (sx, sy) = match o {
                2 => (w - 1 - x, y),
                3 => (w - 1 - x, h - 1 - y),
                4 => (x, h - 1 - y),
                5 => (y, x),
                6 => (y, h - 1 - x),
                7 => (w - 1 - y, h - 1 - x),
                _ => (w - 1 - y, x),
            };
            let (s, d) = ((sy * w + sx) * 4, (y * ow + x) * 4);
            px[d..d + 4].copy_from_slice(&img.px[s..s + 4]);
        }
    }
    Rgba {
        w: ow as u32,
        h: oh as u32,
        px,
    }
}

/// Scales `img` onto a `sw`x`sh` canvas over `bg`. Returns linear RGB floats (0..255).
pub fn render(img: &Rgba, sw: usize, sh: usize, fit: Fit, bg: [u8; 3]) -> Vec<[f32; 3]> {
    let (iw, ih) = (img.w as f64, img.h as f64);
    let (dw, dh) = match fit {
        Fit::Stretch => (sw as f64, sh as f64),
        _ => {
            let s = if fit == Fit::Contain {
                (sw as f64 / iw).min(sh as f64 / ih)
            } else {
                (sw as f64 / iw).max(sh as f64 / ih)
            };
            ((iw * s).round().max(1.0), (ih * s).round().max(1.0))
        }
    };
    let (ox, oy) = (
        ((sw as f64 - dw) / 2.0).round(),
        ((sh as f64 - dh) / 2.0).round(),
    );
    let bgf = [bg[0] as f32, bg[1] as f32, bg[2] as f32];
    let mut out = vec![bgf; sw * sh];
    for y in 0..sh {
        let fy0 = (y as f64 - oy) * ih / dh;
        let fy1 = (y as f64 + 1.0 - oy) * ih / dh;
        if fy1 <= 0.0 || fy0 >= ih {
            continue;
        }
        let y0 = (fy0.max(0.0).floor() as usize).min(img.h as usize - 1);
        let y1 = (fy1.min(ih).ceil() as usize).max(y0 + 1);
        for x in 0..sw {
            let fx0 = (x as f64 - ox) * iw / dw;
            let fx1 = (x as f64 + 1.0 - ox) * iw / dw;
            if fx1 <= 0.0 || fx0 >= iw {
                continue;
            }
            let x0 = (fx0.max(0.0).floor() as usize).min(img.w as usize - 1);
            let x1 = (fx1.min(iw).ceil() as usize).max(x0 + 1);
            // Box filter with premultiplied alpha.
            let (mut r, mut g, mut b, mut a, mut n) = (0f32, 0f32, 0f32, 0f32, 0f32);
            for sy in y0..y1 {
                for sx in x0..x1 {
                    let p = &img.px[(sy * img.w as usize + sx) * 4..][..4];
                    let al = p[3] as f32 / 255.0;
                    r += p[0] as f32 * al;
                    g += p[1] as f32 * al;
                    b += p[2] as f32 * al;
                    a += al;
                    n += 1.0;
                }
            }
            let (a, inv) = (a / n, 1.0 / n);
            out[y * sw + x] = [
                r * inv + bgf[0] * (1.0 - a),
                g * inv + bgf[1] * (1.0 - a),
                b * inv + bgf[2] * (1.0 - a),
            ];
        }
    }
    out
}

/// Quantizes to RGB565 (big-endian bytes), optionally with Floyd–Steinberg dithering.
pub fn to_rgb565_be(mut px: Vec<[f32; 3]>, w: usize, h: usize, dither: bool) -> Vec<u8> {
    const LEVELS: [f32; 3] = [31.0, 63.0, 31.0];
    let mut out = Vec::with_capacity(w * h * 2);
    for y in 0..h {
        for x in 0..w {
            let p = px[y * w + x];
            let mut q = [0u16; 3];
            let mut err = [0f32; 3];
            for c in 0..3 {
                let v = p[c].clamp(0.0, 255.0);
                q[c] = (v * LEVELS[c] / 255.0).round() as u16;
                err[c] = v - q[c] as f32 * 255.0 / LEVELS[c];
            }
            let v = (q[0] << 11) | (q[1] << 5) | q[2];
            out.extend_from_slice(&v.to_be_bytes());
            if dither {
                let mut spread = |dx: isize, dy: usize, f: f32| {
                    let nx = x as isize + dx;
                    if nx >= 0 && (nx as usize) < w && y + dy < h {
                        let t = &mut px[(y + dy) * w + nx as usize];
                        for c in 0..3 {
                            t[c] += err[c] * f;
                        }
                    }
                };
                spread(1, 0, 7.0 / 16.0);
                spread(-1, 1, 3.0 / 16.0);
                spread(0, 1, 5.0 / 16.0);
                spread(1, 1, 1.0 / 16.0);
            }
        }
    }
    out
}

/// Writes RGB565 big-endian pixels back out as a binary PPM (for --save previews).
pub fn rgb565_to_ppm(data: &[u8], w: usize, h: usize) -> Vec<u8> {
    let mut out = format!("P6\n{w} {h}\n255\n").into_bytes();
    for c in data.as_chunks::<2>().0 {
        let v = u16::from_be_bytes([c[0], c[1]]);
        let (r, g, b) = ((v >> 11) & 31, (v >> 5) & 63, v & 31);
        out.extend_from_slice(&[
            (r * 255 / 31) as u8,
            (g * 255 / 63) as u8,
            (b * 255 / 31) as u8,
        ]);
    }
    out
}
