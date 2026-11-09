//! PNG decoder: all color types and bit depths, Adam7 interlace, tRNS.

use super::{inflate, Rgba};

fn be32(d: &[u8]) -> u32 {
    u32::from_be_bytes([d[0], d[1], d[2], d[3]])
}

pub fn decode(data: &[u8]) -> Result<Rgba, String> {
    if data.len() < 8 || &data[..8] != b"\x89PNG\r\n\x1a\n" {
        return Err("not a PNG".into());
    }
    let mut pos = 8;
    let (mut w, mut h, mut depth, mut ctype, mut interlace) = (0u32, 0u32, 0u8, 0u8, 0u8);
    let mut idat = Vec::new();
    let mut palette: Vec<[u8; 4]> = Vec::new();
    let mut trns: Option<Vec<u8>> = None;
    while pos + 8 <= data.len() {
        let len = be32(&data[pos..]) as usize;
        let kind = &data[pos + 4..pos + 8];
        let body = data
            .get(pos + 8..pos + 8 + len)
            .ok_or("PNG: truncated chunk")?;
        match kind {
            b"IHDR" => {
                if len < 13 {
                    return Err("PNG: bad IHDR".into());
                }
                w = be32(body);
                h = be32(&body[4..]);
                depth = body[8];
                ctype = body[9];
                interlace = body[12];
            }
            b"PLTE" => {
                palette = body
                    .as_chunks::<3>()
                    .0
                    .iter()
                    .map(|c| [c[0], c[1], c[2], 255])
                    .collect()
            }
            b"tRNS" => trns = Some(body.to_vec()),
            b"IDAT" => idat.extend_from_slice(body),
            b"IEND" => break,
            _ => {}
        }
        pos += 12 + len;
    }
    if w == 0 || h == 0 {
        return Err("PNG: missing IHDR".into());
    }
    if (w as u64) * (h as u64) > 64_000_000 {
        return Err(format!("PNG: {w}x{h} is too large"));
    }
    let channels = match ctype {
        0 => 1,
        2 => 3,
        3 => 1,
        4 => 2,
        6 => 4,
        _ => return Err(format!("PNG: bad color type {ctype}")),
    };
    let valid_depth = match ctype {
        0 => [1, 2, 4, 8, 16].contains(&depth),
        3 => [1, 2, 4, 8].contains(&depth),
        _ => [8, 16].contains(&depth),
    };
    if !valid_depth {
        return Err(format!("PNG: bad bit depth {depth} for color type {ctype}"));
    }
    if let Some(t) = &trns {
        if ctype == 3 {
            for (i, a) in t.iter().enumerate() {
                if let Some(p) = palette.get_mut(i) {
                    p[3] = *a;
                }
            }
        }
    }
    let bpp_bits = channels * depth as usize; // bits per pixel
    let bpp = bpp_bits.div_ceil(8).max(1); // filter byte distance
    let row_bytes = |pw: usize| (pw * bpp_bits).div_ceil(8);

    // Upper bound on decompressed size (interlaced adds a few filter bytes per pass).
    let limit = (h as usize + 64) * (row_bytes(w as usize) + 1) + 1024;
    let raw = inflate::zlib_decompress(&idat, limit)?;

    let mut out = Rgba {
        w,
        h,
        px: vec![0u8; w as usize * h as usize * 4],
    };
    let max = (1u32 << depth) - 1;
    let trans_key: Option<[u16; 3]> = match (&trns, ctype) {
        (Some(t), 0) if t.len() >= 2 => Some([u16::from_be_bytes([t[0], t[1]]); 3]),
        (Some(t), 2) if t.len() >= 6 => Some([
            u16::from_be_bytes([t[0], t[1]]),
            u16::from_be_bytes([t[2], t[3]]),
            u16::from_be_bytes([t[4], t[5]]),
        ]),
        _ => None,
    };

    // (x0, y0, dx, dy) per pass
    let passes: &[(usize, usize, usize, usize)] = if interlace == 1 {
        &[
            (0, 0, 8, 8),
            (4, 0, 8, 8),
            (0, 4, 4, 8),
            (2, 0, 4, 4),
            (0, 2, 2, 4),
            (1, 0, 2, 2),
            (0, 1, 1, 2),
        ]
    } else {
        &[(0, 0, 1, 1)]
    };
    let mut off = 0;
    for &(x0, y0, dx, dy) in passes {
        let pw = (w as usize).saturating_sub(x0).div_ceil(dx);
        let ph = (h as usize).saturating_sub(y0).div_ceil(dy);
        if pw == 0 || ph == 0 {
            continue;
        }
        let rb = row_bytes(pw);
        let mut prev = vec![0u8; rb];
        let mut cur = vec![0u8; rb];
        for py in 0..ph {
            let ft = *raw.get(off).ok_or("PNG: image data truncated")?;
            let src = raw
                .get(off + 1..off + 1 + rb)
                .ok_or("PNG: image data truncated")?;
            off += 1 + rb;
            for i in 0..rb {
                let a = if i >= bpp { cur[i - bpp] as i16 } else { 0 };
                let b = prev[i] as i16;
                let c = if i >= bpp { prev[i - bpp] as i16 } else { 0 };
                let pred = match ft {
                    0 => 0,
                    1 => a,
                    2 => b,
                    3 => (a + b) / 2,
                    4 => {
                        let p = a + b - c;
                        let (pa, pb, pc) = ((p - a).abs(), (p - b).abs(), (p - c).abs());
                        if pa <= pb && pa <= pc {
                            a
                        } else if pb <= pc {
                            b
                        } else {
                            c
                        }
                    }
                    _ => return Err(format!("PNG: bad filter type {ft}")),
                };
                cur[i] = src[i].wrapping_add(pred as u8);
            }
            let y = y0 + py * dy;
            for px in 0..pw {
                let x = x0 + px * dx;
                // sample n of this pixel, scaled to 0..=255 (and raw value for tRNS keys)
                let sample = |n: usize| -> (u8, u16) {
                    match depth {
                        16 => {
                            let i = (px * channels + n) * 2;
                            let v = u16::from_be_bytes([cur[i], cur[i + 1]]);
                            ((v >> 8) as u8, v)
                        }
                        8 => {
                            let v = cur[px * channels + n];
                            (v, v as u16)
                        }
                        d => {
                            let bit = px * d as usize;
                            let v = (cur[bit / 8] >> (8 - d as usize - bit % 8)) as u32 & max;
                            ((v * 255 / max) as u8, v as u16)
                        }
                    }
                };
                let rgba: [u8; 4] = match ctype {
                    0 => {
                        let (g, raw) = sample(0);
                        let a = if trans_key.is_some_and(|k| k[0] == raw) {
                            0
                        } else {
                            255
                        };
                        [g, g, g, a]
                    }
                    2 => {
                        let (r, rr) = sample(0);
                        let (g, rg) = sample(1);
                        let (b, rb2) = sample(2);
                        let a = if trans_key == Some([rr, rg, rb2]) {
                            0
                        } else {
                            255
                        };
                        [r, g, b, a]
                    }
                    3 => {
                        let (_, idx) = sample(0);
                        *palette.get(idx as usize).unwrap_or(&[0, 0, 0, 255])
                    }
                    4 => {
                        let (g, _) = sample(0);
                        [g, g, g, sample(1).0]
                    }
                    _ => [sample(0).0, sample(1).0, sample(2).0, sample(3).0],
                };
                let o = (y * w as usize + x) * 4;
                out.px[o..o + 4].copy_from_slice(&rgba);
            }
            std::mem::swap(&mut prev, &mut cur);
        }
    }
    Ok(out)
}
