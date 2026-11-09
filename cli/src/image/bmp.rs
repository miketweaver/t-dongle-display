//! Uncompressed BMP: 1/4/8-bit palette, 24-bit, 32-bit (BI_RGB / BI_BITFIELDS).

use super::Rgba;

fn le16(d: &[u8], o: usize) -> Result<u32, String> {
    d.get(o..o + 2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]) as u32)
        .ok_or_else(|| "BMP: truncated".into())
}
fn le32(d: &[u8], o: usize) -> Result<u32, String> {
    d.get(o..o + 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .ok_or_else(|| "BMP: truncated".into())
}

pub fn decode(d: &[u8]) -> Result<Rgba, String> {
    let data_off = le32(d, 10)? as usize;
    let hdr = le32(d, 14)? as usize;
    if hdr < 40 {
        return Err("BMP: OS/2 bitmaps are not supported".into());
    }
    let w = le32(d, 18)? as i32;
    let hraw = le32(d, 22)? as i32;
    let bpp = le16(d, 28)?;
    let comp = le32(d, 30)?;
    let (h, top_down) = (hraw.unsigned_abs(), hraw < 0);
    if w <= 0 || h == 0 || (w as u64) * (h as u64) > 64_000_000 {
        return Err("BMP: bad dimensions".into());
    }
    let w = w as u32;
    if !(comp == 0 || (comp == 3 && bpp == 32)) {
        return Err("BMP: compressed bitmaps are not supported".into());
    }
    if ![1, 4, 8, 24, 32].contains(&bpp) {
        return Err(format!("BMP: {bpp}-bit bitmaps are not supported"));
    }
    let mut palette = Vec::new();
    if bpp <= 8 {
        let n = match le32(d, 46)? {
            0 => 1 << bpp,
            n => n.min(256),
        } as usize;
        let p0 = 14 + hdr;
        for i in 0..n {
            let e = d
                .get(p0 + i * 4..p0 + i * 4 + 3)
                .ok_or("BMP: truncated palette")?;
            palette.push([e[2], e[1], e[0], 255]);
        }
    }
    // 32-bit: honour an alpha channel only if the file actually uses it.
    let stride = (w as usize * bpp as usize).div_ceil(32) * 4;
    let mut out = Rgba {
        w,
        h,
        px: vec![255u8; w as usize * h as usize * 4],
    };
    let mut any_alpha = false;
    for row in 0..h as usize {
        let y = if top_down { row } else { h as usize - 1 - row };
        let src = d
            .get(data_off + row * stride..data_off + (row + 1) * stride)
            .ok_or("BMP: pixel data truncated")?;
        for x in 0..w as usize {
            let p: [u8; 4] = match bpp {
                32 => {
                    let a = src[x * 4 + 3];
                    any_alpha |= a != 0;
                    [src[x * 4 + 2], src[x * 4 + 1], src[x * 4], a]
                }
                24 => [src[x * 3 + 2], src[x * 3 + 1], src[x * 3], 255],
                _ => {
                    let bit = x * bpp as usize;
                    let idx =
                        (src[bit / 8] >> (8 - bpp as usize - bit % 8)) & ((1 << bpp) - 1) as u8;
                    *palette.get(idx as usize).unwrap_or(&[0, 0, 0, 255])
                }
            };
            let o = (y * w as usize + x) * 4;
            out.px[o..o + 4].copy_from_slice(&p);
        }
    }
    if bpp == 32 && !any_alpha {
        out.px
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .for_each(|p| p[3] = 255);
    }
    Ok(out)
}
