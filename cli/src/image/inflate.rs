//! Minimal zlib/DEFLATE decoder (RFC 1950/1951), enough for PNG.

struct Bits<'a> {
    d: &'a [u8],
    pos: usize,
    bit: u32,
    nbits: u32,
}

impl<'a> Bits<'a> {
    fn new(d: &'a [u8]) -> Self {
        Bits {
            d,
            pos: 0,
            bit: 0,
            nbits: 0,
        }
    }
    fn need(&mut self, n: u32) -> Result<(), String> {
        while self.nbits < n {
            let b = *self
                .d
                .get(self.pos)
                .ok_or("deflate: unexpected end of data")?;
            self.pos += 1;
            self.bit |= (b as u32) << self.nbits;
            self.nbits += 8;
        }
        Ok(())
    }
    fn get(&mut self, n: u32) -> Result<u32, String> {
        if n == 0 {
            return Ok(0);
        }
        self.need(n)?;
        let v = self.bit & ((1u32 << n) - 1);
        self.bit >>= n;
        self.nbits -= n;
        Ok(v)
    }
    fn align_byte(&mut self) {
        let r = self.nbits % 8;
        self.bit >>= r;
        self.nbits -= r;
    }
}

/// Canonical Huffman table: counts per length + symbols sorted by code.
struct Huff {
    counts: [u16; 16],
    syms: Vec<u16>,
}

impl Huff {
    fn new(lengths: &[u8]) -> Result<Huff, String> {
        let mut counts = [0u16; 16];
        for &l in lengths {
            counts[l as usize] += 1;
        }
        counts[0] = 0;
        let mut offs = [0u16; 16];
        for i in 1..16 {
            offs[i] = offs[i - 1] + counts[i - 1];
        }
        let mut syms = vec![0u16; lengths.len()];
        for (s, &l) in lengths.iter().enumerate() {
            if l != 0 {
                syms[offs[l as usize] as usize] = s as u16;
                offs[l as usize] += 1;
            }
        }
        Ok(Huff { counts, syms })
    }
    fn decode(&self, b: &mut Bits) -> Result<u16, String> {
        let (mut code, mut first, mut index) = (0i32, 0i32, 0i32);
        for len in 1..16 {
            code |= b.get(1)? as i32;
            let count = self.counts[len] as i32;
            if code - count < first {
                return Ok(self.syms[(index + (code - first)) as usize]);
            }
            index += count;
            first += count;
            first <<= 1;
            code <<= 1;
        }
        Err("deflate: bad Huffman code".into())
    }
}

const LEN_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LEN_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DIST_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];

fn inflate_block(
    b: &mut Bits,
    out: &mut Vec<u8>,
    lit: &Huff,
    dist: &Huff,
    limit: usize,
) -> Result<(), String> {
    loop {
        let sym = lit.decode(b)?;
        if sym < 256 {
            out.push(sym as u8);
        } else if sym == 256 {
            return Ok(());
        } else {
            let i = (sym - 257) as usize;
            if i >= 29 {
                return Err("deflate: bad length symbol".into());
            }
            let len = LEN_BASE[i] as usize + b.get(LEN_EXTRA[i] as u32)? as usize;
            let di = dist.decode(b)? as usize;
            if di >= 30 {
                return Err("deflate: bad distance symbol".into());
            }
            let d = DIST_BASE[di] as usize + b.get(DIST_EXTRA[di] as u32)? as usize;
            if d > out.len() {
                return Err("deflate: distance too far back".into());
            }
            let start = out.len() - d;
            for k in 0..len {
                let v = out[start + k];
                out.push(v);
            }
        }
        if out.len() > limit {
            return Err("deflate: output larger than expected".into());
        }
    }
}

/// Decodes a zlib stream. `limit` caps the output size (guards against bombs).
pub fn zlib_decompress(data: &[u8], limit: usize) -> Result<Vec<u8>, String> {
    if data.len() < 2
        || data[0] & 0x0f != 8
        || !((data[0] as u16) << 8 | data[1] as u16).is_multiple_of(31)
    {
        return Err("zlib: bad header".into());
    }
    if data[1] & 0x20 != 0 {
        return Err("zlib: preset dictionary not supported".into());
    }
    let mut b = Bits::new(&data[2..]);
    let mut out = Vec::new();
    loop {
        let last = b.get(1)?;
        match b.get(2)? {
            0 => {
                b.align_byte();
                let len = b.get(16)? as usize;
                let nlen = b.get(16)? as usize;
                if len != !nlen & 0xffff {
                    return Err("deflate: stored block length mismatch".into());
                }
                // Bit buffer is byte aligned and empty now (we consumed exactly 32 bits).
                let end = b.pos + len;
                let chunk =
                    b.d.get(b.pos..end)
                        .ok_or("deflate: stored block truncated")?;
                out.extend_from_slice(chunk);
                b.pos = end;
            }
            1 => {
                let mut l = [0u8; 288];
                l[..144].fill(8);
                l[144..256].fill(9);
                l[256..280].fill(7);
                l[280..].fill(8);
                let lit = Huff::new(&l)?;
                let dist = Huff::new(&[5u8; 30])?;
                inflate_block(&mut b, &mut out, &lit, &dist, limit)?;
            }
            2 => {
                let hlit = b.get(5)? as usize + 257;
                let hdist = b.get(5)? as usize + 1;
                let hclen = b.get(4)? as usize + 4;
                const ORDER: [usize; 19] = [
                    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
                ];
                let mut cl = [0u8; 19];
                for &o in ORDER.iter().take(hclen) {
                    cl[o] = b.get(3)? as u8;
                }
                let clh = Huff::new(&cl)?;
                let mut lens = vec![0u8; hlit + hdist];
                let mut i = 0;
                while i < hlit + hdist {
                    let sym = clh.decode(&mut b)?;
                    let (val, rep) = match sym {
                        0..=15 => (sym as u8, 1),
                        16 => {
                            if i == 0 {
                                return Err("deflate: repeat with no previous length".into());
                            }
                            (lens[i - 1], 3 + b.get(2)? as usize)
                        }
                        17 => (0, 3 + b.get(3)? as usize),
                        18 => (0, 11 + b.get(7)? as usize),
                        _ => return Err("deflate: bad code length symbol".into()),
                    };
                    if i + rep > lens.len() {
                        return Err("deflate: too many code lengths".into());
                    }
                    lens[i..i + rep].fill(val);
                    i += rep;
                }
                let lit = Huff::new(&lens[..hlit])?;
                let dist = Huff::new(&lens[hlit..])?;
                inflate_block(&mut b, &mut out, &lit, &dist, limit)?;
            }
            _ => return Err("deflate: invalid block type".into()),
        }
        if last == 1 {
            return Ok(out);
        }
    }
}
