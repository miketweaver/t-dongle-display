//! Netpbm: P1-P6 (PBM/PGM/PPM, ASCII and binary, up to 16-bit).

use super::Rgba;

pub fn decode(d: &[u8]) -> Result<Rgba, String> {
    let kind = d[1];
    let mut pos = 2;
    let header = |pos: &mut usize| -> Result<u32, String> {
        loop {
            match d.get(*pos) {
                Some(b'#') => {
                    while d.get(*pos).is_some_and(|&c| c != b'\n') {
                        *pos += 1
                    }
                }
                Some(c) if c.is_ascii_whitespace() => *pos += 1,
                Some(c) if c.is_ascii_digit() => break,
                _ => return Err("PNM: bad header".into()),
            }
        }
        let start = *pos;
        while d.get(*pos).is_some_and(|c| c.is_ascii_digit()) {
            *pos += 1
        }
        std::str::from_utf8(&d[start..*pos])
            .unwrap()
            .parse()
            .map_err(|_| "PNM: bad number".into())
    };
    let w = header(&mut pos)?;
    let h = header(&mut pos)?;
    let max = if kind == b'1' || kind == b'4' {
        1
    } else {
        header(&mut pos)?
    };
    if w == 0 || h == 0 || max == 0 || max > 65535 || (w as u64) * (h as u64) > 64_000_000 {
        return Err("PNM: bad dimensions".into());
    }
    pos += 1; // single whitespace after the header
    let ch = if kind == b'3' || kind == b'6' { 3 } else { 1 };
    let n = (w * h) as usize * ch;
    let mut vals: Vec<u32> = Vec::with_capacity(n);
    match kind {
        b'1' | b'2' | b'3' => {
            pos -= 1;
            for _ in 0..n {
                vals.push(header(&mut pos)?);
            }
        }
        b'4' => {
            let stride = (w as usize).div_ceil(8);
            for y in 0..h as usize {
                for x in 0..w as usize {
                    let b = *d.get(pos + y * stride + x / 8).ok_or("PNM: truncated")?;
                    vals.push(((b >> (7 - x % 8)) & 1) as u32);
                }
            }
        }
        _ => {
            let bytes = if max > 255 { 2 } else { 1 };
            let raw = d.get(pos..pos + n * bytes).ok_or("PNM: truncated")?;
            if bytes == 2 {
                vals.extend(
                    raw.as_chunks::<2>()
                        .0
                        .iter()
                        .map(|c| u16::from_be_bytes([c[0], c[1]]) as u32),
                );
            } else {
                vals.extend(raw.iter().map(|&b| b as u32));
            }
        }
    }
    let scale = |v: u32| (v.min(max) * 255 / max) as u8;
    let mut px = Vec::with_capacity((w * h) as usize * 4);
    for i in 0..(w * h) as usize {
        if ch == 3 {
            px.extend_from_slice(&[
                scale(vals[i * 3]),
                scale(vals[i * 3 + 1]),
                scale(vals[i * 3 + 2]),
                255,
            ]);
        } else {
            // PBM: 1 = black
            let g = if kind == b'1' || kind == b'4' {
                if vals[i] == 1 {
                    0
                } else {
                    255
                }
            } else {
                scale(vals[i])
            };
            px.extend_from_slice(&[g, g, g, 255]);
        }
    }
    Ok(Rgba { w, h, px })
}
