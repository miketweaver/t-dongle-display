//! Baseline (sequential, 8-bit, Huffman) JPEG decoder with EXIF orientation.
//! Progressive/arithmetic/lossless/CMYK JPEGs return an error; the caller can
//! fall back to an external converter for those.

use super::Rgba;

const ZIGZAG: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20, 13, 6, 7, 14, 21,
    28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59, 52, 45, 38, 31, 39, 46, 53, 60, 61,
    54, 47, 55, 62, 63,
];

#[derive(Clone, Default)]
struct Huff {
    maxcode: [i32; 18],
    valptr: [i32; 17],
    mincode: [i32; 17],
    vals: Vec<u8>,
}

impl Huff {
    fn new(counts: &[u8; 16], vals: Vec<u8>) -> Huff {
        let mut h = Huff { vals, ..Default::default() };
        let (mut code, mut k) = (0i32, 0i32);
        for l in 1..=16 {
            let n = counts[l - 1] as i32;
            h.valptr[l] = k;
            h.mincode[l] = code;
            code += n;
            k += n;
            h.maxcode[l] = if n > 0 { code - 1 } else { -1 };
            code <<= 1;
        }
        h.maxcode[17] = i32::MAX;
        h
    }
}

struct Bits<'a> {
    d: &'a [u8],
    pos: usize,
    acc: u32,
    n: u32,
}

impl Bits<'_> {
    fn bit(&mut self) -> u32 {
        if self.n == 0 {
            let mut b = 0u8;
            if self.pos < self.d.len() {
                b = self.d[self.pos];
                if b == 0xFF {
                    let next = self.d.get(self.pos + 1).copied().unwrap_or(0);
                    if next == 0 {
                        self.pos += 2;
                    } else {
                        b = 0; // a marker: feed zeros, leave the marker in place
                    }
                } else {
                    self.pos += 1;
                }
            }
            self.acc = b as u32;
            self.n = 8;
        }
        self.n -= 1;
        (self.acc >> self.n) & 1
    }
    fn bits(&mut self, n: u32) -> u32 {
        let mut v = 0;
        for _ in 0..n {
            v = (v << 1) | self.bit();
        }
        v
    }
    fn decode(&mut self, h: &Huff) -> Result<u8, String> {
        let mut code = self.bit() as i32;
        let mut l = 1;
        while code > h.maxcode[l] {
            code = (code << 1) | self.bit() as i32;
            l += 1;
            if l > 16 {
                return Err("JPEG: bad Huffman code".into());
            }
        }
        h.vals.get((h.valptr[l] + code - h.mincode[l]) as usize).copied().ok_or_else(|| "JPEG: bad Huffman code".into())
    }
    fn receive_extend(&mut self, s: u8) -> i32 {
        if s == 0 {
            return 0;
        }
        let v = self.bits(s as u32) as i32;
        if v < (1 << (s - 1)) {
            v - (1 << s) + 1
        } else {
            v
        }
    }
    /// After a restart interval: drop leftover bits and skip the RSTn marker.
    fn restart(&mut self) {
        self.n = 0;
        while self.pos + 1 < self.d.len() {
            if self.d[self.pos] == 0xFF && (0xD0..=0xD7).contains(&self.d[self.pos + 1]) {
                self.pos += 2;
                return;
            }
            self.pos += 1;
        }
    }
}

#[derive(Clone, Default)]
struct Comp {
    id: u8,
    h: usize,
    v: usize,
    tq: usize,
    td: usize,
    ta: usize,
    pred: i32,
    plane: Vec<u8>,
    pw: usize, // plane width in pixels
}

fn idct_block(coef: &[f32; 64], out: &mut [u8], stride: usize, cos: &[[f32; 8]; 8]) {
    let mut tmp = [0f32; 64];
    // rows: tmp[y][x] = sum_u C(u) F[y][u] cos(..x..u)
    for y in 0..8 {
        for x in 0..8 {
            let mut s = 0.0;
            for u in 0..8 {
                s += coef[y * 8 + u] * cos[x][u];
            }
            tmp[y * 8 + x] = s;
        }
    }
    for x in 0..8 {
        for y in 0..8 {
            let mut s = 0.0;
            for v in 0..8 {
                s += tmp[v * 8 + x] * cos[y][v];
            }
            let p = (s / 4.0 + 128.0).round().clamp(0.0, 255.0);
            out[y * stride + x] = p as u8;
        }
    }
}

fn exif_orientation(d: &[u8]) -> Option<u8> {
    if d.len() < 14 || &d[..6] != b"Exif\0\0" {
        return None;
    }
    let t = &d[6..];
    let le = match &t[..2] {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let u16_at = |o: usize| -> Option<u16> {
        let b = t.get(o..o + 2)?;
        Some(if le { u16::from_le_bytes([b[0], b[1]]) } else { u16::from_be_bytes([b[0], b[1]]) })
    };
    let u32_at = |o: usize| -> Option<u32> {
        let b = t.get(o..o + 4)?;
        let a = [b[0], b[1], b[2], b[3]];
        Some(if le { u32::from_le_bytes(a) } else { u32::from_be_bytes(a) })
    };
    let ifd = u32_at(4)? as usize;
    let n = u16_at(ifd)? as usize;
    for i in 0..n {
        let e = ifd + 2 + i * 12;
        if u16_at(e)? == 0x0112 {
            return Some(u16_at(e + 8)? as u8);
        }
    }
    None
}

/// Returns the image and its EXIF orientation (1 = as stored).
pub fn decode(d: &[u8]) -> Result<(Rgba, u8), String> {
    if d.len() < 4 || d[0] != 0xFF || d[1] != 0xD8 {
        return Err("not a JPEG".into());
    }
    let mut qt = [[0f32; 64]; 4];
    let mut dc: [Huff; 4] = Default::default();
    let mut ac: [Huff; 4] = Default::default();
    let mut comps: Vec<Comp> = Vec::new();
    let (mut w, mut h, mut hmax, mut vmax) = (0usize, 0usize, 1usize, 1usize);
    let mut restart = 0usize;
    let mut orient = 1u8;
    let mut adobe_transform: Option<u8> = None;
    let mut cos = [[0f32; 8]; 8];
    for (x, row) in cos.iter_mut().enumerate() {
        for (u, c) in row.iter_mut().enumerate() {
            let cu = if u == 0 { std::f32::consts::FRAC_1_SQRT_2 } else { 1.0 };
            *c = cu * (((2 * x + 1) * u) as f32 * std::f32::consts::PI / 16.0).cos();
        }
    }

    let mut pos = 2;
    let mut frame_done = false;
    while pos + 4 <= d.len() {
        if d[pos] != 0xFF {
            pos += 1;
            continue;
        }
        let m = d[pos + 1];
        if m == 0xFF || m == 0x00 || (0xD0..=0xD7).contains(&m) || m == 0x01 {
            pos += if m == 0xFF { 1 } else { 2 };
            continue;
        }
        if m == 0xD9 {
            break;
        }
        let len = u16::from_be_bytes([d[pos + 2], d[pos + 3]]) as usize;
        let seg = d.get(pos + 4..pos + 2 + len).ok_or("JPEG: truncated segment")?;
        pos += 2 + len;
        match m {
            0xE1 => {
                if let Some(o) = exif_orientation(seg) {
                    orient = o;
                }
            }
            0xEE if seg.len() >= 12 && &seg[..5] == b"Adobe" => adobe_transform = Some(seg[11]),
            0xDB => {
                let mut s = seg;
                while !s.is_empty() {
                    let (pq, tq) = ((s[0] >> 4) as usize, (s[0] & 15) as usize);
                    if tq > 3 {
                        return Err("JPEG: bad quantization table".into());
                    }
                    let n = if pq == 0 { 64 } else { 128 };
                    let body = s.get(1..1 + n).ok_or("JPEG: truncated DQT")?;
                    for k in 0..64 {
                        qt[tq][k] = if pq == 0 { body[k] as f32 } else { u16::from_be_bytes([body[2 * k], body[2 * k + 1]]) as f32 };
                    }
                    s = &s[1 + n..];
                }
            }
            0xC4 => {
                let mut s = seg;
                while s.len() >= 17 {
                    let (tc, th) = (s[0] >> 4, (s[0] & 15) as usize);
                    if th > 3 {
                        return Err("JPEG: bad Huffman table".into());
                    }
                    let mut counts = [0u8; 16];
                    counts.copy_from_slice(&s[1..17]);
                    let total: usize = counts.iter().map(|&c| c as usize).sum();
                    let vals = s.get(17..17 + total).ok_or("JPEG: truncated DHT")?.to_vec();
                    let t = Huff::new(&counts, vals);
                    if tc == 0 {
                        dc[th] = t
                    } else {
                        ac[th] = t
                    }
                    s = &s[17 + total..];
                }
            }
            0xDD if seg.len() >= 2 => restart = u16::from_be_bytes([seg[0], seg[1]]) as usize,
            0xC0 | 0xC1 => {
                if seg.len() < 6 || seg.len() < 6 + 3 * seg[5] as usize {
                    return Err("JPEG: truncated frame header".into());
                }
                if !comps.is_empty() {
                    return Err("JPEG: multiple frames".into());
                }
                if seg[0] != 8 {
                    return Err("JPEG: only 8-bit precision is supported".into());
                }
                h = u16::from_be_bytes([seg[1], seg[2]]) as usize;
                w = u16::from_be_bytes([seg[3], seg[4]]) as usize;
                let n = seg[5] as usize;
                if w == 0 || h == 0 || (n != 1 && n != 3) {
                    return Err(format!("JPEG: unsupported layout ({n} components, {w}x{h})"));
                }
                if w * h > 64_000_000 {
                    return Err(format!("JPEG: {w}x{h} is too large"));
                }
                for i in 0..n {
                    let c = &seg[6 + i * 3..9 + i * 3];
                    comps.push(Comp {
                        id: c[0],
                        h: (c[1] >> 4).max(1) as usize,
                        v: (c[1] & 15).max(1) as usize,
                        tq: (c[2] & 3) as usize,
                        ..Default::default()
                    });
                }
                hmax = comps.iter().map(|c| c.h).max().unwrap();
                vmax = comps.iter().map(|c| c.v).max().unwrap();
                let (mcux, mcuy) = (w.div_ceil(8 * hmax), h.div_ceil(8 * vmax));
                for c in comps.iter_mut() {
                    c.pw = mcux * c.h * 8;
                    c.plane = vec![0u8; c.pw * mcuy * c.v * 8];
                }
            }
            0xC2 | 0xC6 | 0xCA | 0xCE => return Err("JPEG: progressive JPEG is not supported".into()),
            0xC3 | 0xC5 | 0xC7 | 0xC9 | 0xCB | 0xCD | 0xCF => {
                return Err("JPEG: lossless/arithmetic-coded JPEG is not supported".into())
            }
            0xDA => {
                if comps.is_empty() {
                    return Err("JPEG: scan before frame header".into());
                }
                let ns = *seg.first().ok_or("JPEG: empty scan header")? as usize;
                if ns == 0 || seg.len() < 1 + 2 * ns {
                    return Err("JPEG: truncated scan header".into());
                }
                let mut scan = Vec::new();
                for i in 0..ns {
                    let (cid, t) = (seg[1 + i * 2], seg[2 + i * 2]);
                    let ci = comps.iter().position(|c| c.id == cid).ok_or("JPEG: scan references unknown component")?;
                    comps[ci].td = (t >> 4) as usize & 3;
                    comps[ci].ta = (t & 15) as usize & 3;
                    comps[ci].pred = 0;
                    scan.push(ci);
                }
                let mut bits = Bits { d, pos, acc: 0, n: 0 };
                let (mcux, mcuy) = (w.div_ceil(8 * hmax), h.div_ceil(8 * vmax));
                // Non-interleaved scans walk one component's blocks; interleaved walk MCUs.
                let units: Vec<(usize, usize)> = if ns == 1 {
                    let c = &comps[scan[0]];
                    let bw = (w * c.h).div_ceil(hmax).div_ceil(8);
                    let bh = (h * c.v).div_ceil(vmax).div_ceil(8);
                    (0..bh).flat_map(|y| (0..bw).map(move |x| (x, y))).collect()
                } else {
                    (0..mcuy).flat_map(|y| (0..mcux).map(move |x| (x, y))).collect()
                };
                let mut coef = [0f32; 64];
                for (ui, &(ux, uy)) in units.iter().enumerate() {
                    if restart > 0 && ui > 0 && ui % restart == 0 {
                        bits.restart();
                        for &ci in &scan {
                            comps[ci].pred = 0;
                        }
                    }
                    for &ci in &scan {
                        let (bh_, bv_) = if ns == 1 { (1, 1) } else { (comps[ci].h, comps[ci].v) };
                        for by in 0..bv_ {
                            for bx in 0..bh_ {
                                coef.fill(0.0);
                                let c = &mut comps[ci];
                                let q = &qt[c.tq];
                                let t = bits.decode(&dc[c.td])?;
                                c.pred += bits.receive_extend(t);
                                coef[0] = c.pred as f32 * q[0];
                                let mut k = 1;
                                while k < 64 {
                                    let rs = bits.decode(&ac[c.ta])?;
                                    let (r, s) = ((rs >> 4) as usize, rs & 15);
                                    if s == 0 {
                                        if r == 15 {
                                            k += 16;
                                            continue;
                                        }
                                        break;
                                    }
                                    k += r;
                                    if k > 63 {
                                        return Err("JPEG: corrupt AC coefficients".into());
                                    }
                                    coef[ZIGZAG[k]] = bits.receive_extend(s) as f32 * q[k];
                                    k += 1;
                                }
                                let (px, py) = if ns == 1 { (ux * 8, uy * 8) } else { ((ux * c.h + bx) * 8, (uy * c.v + by) * 8) };
                                let pw = c.pw;
                                if (py + 7) * pw + px + 7 < c.plane.len() {
                                    idct_block(&coef, &mut c.plane[py * pw + px..], pw, &cos);
                                }
                            }
                        }
                    }
                }
                pos = bits.pos;
                frame_done = true;
            }
            _ => {}
        }
    }
    if !frame_done {
        return Err("JPEG: no image data".into());
    }

    let mut out = Rgba { w: w as u32, h: h as u32, px: vec![255u8; w * h * 4] };
    let ycc = comps.len() == 3 && adobe_transform != Some(0);
    for y in 0..h {
        for x in 0..w {
            let s = |c: &Comp| c.plane[(y * c.v / vmax) * c.pw + x * c.h / hmax] as f32;
            let o = (y * w + x) * 4;
            if comps.len() == 1 {
                let g = s(&comps[0]) as u8;
                out.px[o..o + 3].fill(g);
            } else if ycc {
                let (yy, cb, cr) = (s(&comps[0]), s(&comps[1]) - 128.0, s(&comps[2]) - 128.0);
                out.px[o] = (yy + 1.402 * cr).round().clamp(0.0, 255.0) as u8;
                out.px[o + 1] = (yy - 0.344136 * cb - 0.714136 * cr).round().clamp(0.0, 255.0) as u8;
                out.px[o + 2] = (yy + 1.772 * cb).round().clamp(0.0, 255.0) as u8;
            } else {
                out.px[o] = s(&comps[0]) as u8;
                out.px[o + 1] = s(&comps[1]) as u8;
                out.px[o + 2] = s(&comps[2]) as u8;
            }
        }
    }
    Ok((out, orient))
}
