//! Pure-Rust RFC 1951 (DEFLATE), RFC 1950 (zlib), and RFC 1952 (gzip)
//! compression and decompression for Python's `zlib` and `zipfile` modules
//! (`__zipp_py_zlib`).
//!
//! Zero external crate dependencies and zero `unsafe`, so it works identically
//! across native `zipp-cli`, `zipp-sandbox` (`safe-sandbox`), and `zipp-wasm`.
//!
//! Calling convention from JS:
//! - `__zipp_py_zlib(0, items, level, wbits)` -> compress bytes, returns Array<u8>
//! - `__zipp_py_zlib(1, items, wbits, max_length)` -> decompress bytes, returns Array<u8>
//! - `__zipp_py_zlib(2, items, start)` -> IEEE CRC-32 (unsigned u32 as f64)
//! - `__zipp_py_zlib(3, items, start)` -> Adler-32 (unsigned u32 as f64)

use crate::heap::HeapObj;
use crate::value::Value;
use crate::vm::{Thrown, Vm};

/// Hard ceiling for decompressed output when no smaller `max_length` is given,
/// preventing zip bombs from exhausting memory in sandboxed/WASM runs.
const DEFAULT_MAX_OUTPUT: usize = 64 * 1024 * 1024;

// RFC 1951 length base values and extra-bit counts for symbols 257..=285.
const LEN_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LEN_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];

// RFC 1951 distance base values and extra-bit counts for symbols 0..=29.
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DIST_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];

// RFC 1951 code-length alphabet order.
const CL_ORDER: [usize; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

// ── Checksums ────────────────────────────────────────────────────────────────

pub(crate) fn crc32(data: &[u8], start: u32) -> u32 {
    let mut c = !start;
    for &b in data {
        c ^= b as u32;
        for _ in 0..8 {
            c = if (c & 1) != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
        }
    }
    !c
}

pub(crate) fn adler32(data: &[u8], start: u32) -> u32 {
    const MOD: u32 = 65521;
    let mut s1 = start & 0xFFFF;
    let mut s2 = (start >> 16) & 0xFFFF;
    // Process in chunks of 5552 bytes so s2 never overflows u32 before modulo.
    for chunk in data.chunks(5552) {
        for &b in chunk {
            s1 += b as u32;
            s2 += s1;
        }
        s1 %= MOD;
        s2 %= MOD;
    }
    (s2 << 16) | s1
}

// ── Bit reversal & canonical Huffman codes ───────────────────────────────────

#[inline]
fn reverse_bits(mut v: u16, bits: u8) -> u16 {
    let mut out = 0u16;
    for _ in 0..bits {
        out = (out << 1) | (v & 1);
        v >>= 1;
    }
    out
}

/// Build canonical Huffman codes (LSB-first bit order, ready for writing to
/// RFC 1951 bitstreams) from symbol bit lengths.
fn build_canonical_codes(lengths: &[u8]) -> Result<Vec<u16>, String> {
    let mut bl_count = [0u32; 16];
    let mut max_bits = 0usize;
    for &len in lengths {
        if len > 15 {
            return Err("invalid Huffman code length".into());
        }
        if len > 0 {
            bl_count[len as usize] += 1;
            max_bits = max_bits.max(len as usize);
        }
    }
    let mut next_code = [0u32; 16];
    let mut code = 0u32;
    for bits in 1..=max_bits {
        code = (code + bl_count[bits - 1]) << 1;
        next_code[bits] = code;
    }
    let mut codes = vec![0u16; lengths.len()];
    for (sym, &len) in lengths.iter().enumerate() {
        if len > 0 {
            let c = next_code[len as usize];
            next_code[len as usize] = c + 1;
            codes[sym] = reverse_bits(c as u16, len);
        }
    }
    Ok(codes)
}

/// Length-limited optimal Huffman code lengths via package-merge.
fn build_huffman_lengths(freqs: &[u32], max_bits: u8) -> Vec<u8> {
    let mut active: Vec<(u64, usize)> = freqs
        .iter()
        .enumerate()
        .filter(|&(_, &f)| f > 0)
        .map(|(s, &f)| (f as u64, s))
        .collect();
    let mut lengths = vec![0u8; freqs.len()];
    if active.is_empty() {
        return lengths;
    }
    if active.len() == 1 {
        lengths[active[0].1] = 1;
        return lengths;
    }
    // Sort ascending by (weight, symbol).
    active.sort_unstable();
    let n = active.len();

    #[derive(Clone, Copy)]
    enum Node {
        Leaf(u16),
        Merge(u32, u32),
    }

    let mut nodes: Vec<Node> = Vec::with_capacity(n * (max_bits as usize) * 2);
    let mut leaf_items: Vec<(u64, u32)> = Vec::with_capacity(n);
    for &(w, sym) in &active {
        let id = nodes.len() as u32;
        nodes.push(Node::Leaf(sym as u16));
        leaf_items.push((w, id));
    }

    let mut cur: Vec<(u64, u32)> = leaf_items.clone();
    for _ in 1..max_bits {
        let pairs = cur.len() / 2;
        let mut next: Vec<(u64, u32)> = Vec::with_capacity(n + pairs);
        let mut li = 0usize;
        let mut pi = 0usize;
        while li < n || pi < pairs {
            if pi >= pairs {
                next.push(leaf_items[li]);
                li += 1;
            } else {
                let (w0, id0) = cur[pi * 2];
                let (w1, id1) = cur[pi * 2 + 1];
                let pw = w0.saturating_add(w1);
                if li < n && leaf_items[li].0 <= pw {
                    next.push(leaf_items[li]);
                    li += 1;
                } else {
                    let id = nodes.len() as u32;
                    nodes.push(Node::Merge(id0, id1));
                    next.push((pw, id));
                    pi += 1;
                }
            }
        }
        cur = next;
    }

    let take = (2 * n - 2).min(cur.len());
    let mut stack: Vec<u32> = Vec::with_capacity(take);
    for &(_, id) in &cur[..take] {
        stack.push(id);
    }
    while let Some(id) = stack.pop() {
        match nodes[id as usize] {
            Node::Leaf(sym) => lengths[sym as usize] += 1,
            Node::Merge(a, b) => {
                stack.push(a);
                stack.push(b);
            }
        }
    }
    lengths
}

// ── Inflate (RFC 1951 decompression) ─────────────────────────────────────────

struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
    buf: u64,
    bits: u32,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            pos: 0,
            buf: 0,
            bits: 0,
        }
    }

    #[inline]
    fn fill(&mut self, needed: u32) {
        while self.bits < needed && self.pos < self.data.len() {
            self.buf |= (self.data[self.pos] as u64) << self.bits;
            self.pos += 1;
            self.bits += 8;
        }
    }

    #[inline]
    fn read_bits(&mut self, n: u32) -> Result<u32, String> {
        if n == 0 {
            return Ok(0);
        }
        self.fill(n);
        if self.bits < n {
            return Err("Error -5 while decompressing data: incomplete or truncated stream".into());
        }
        let val = (self.buf & ((1u64 << n) - 1)) as u32;
        self.buf >>= n;
        self.bits -= n;
        Ok(val)
    }

    fn align_byte(&mut self) {
        let drop = self.bits & 7;
        self.buf >>= drop;
        self.bits -= drop;
    }

    fn read_u16_le(&mut self) -> Result<u16, String> {
        let lo = self.read_bits(8)? as u16;
        let hi = self.read_bits(8)? as u16;
        Ok(lo | (hi << 8))
    }

    fn read_exact_bytes(&mut self, count: usize, out: &mut Vec<u8>) -> Result<(), String> {
        // Drain whole bytes already sitting in `buf` first.
        let mut remaining = count;
        while remaining > 0 && self.bits >= 8 {
            out.push((self.buf & 0xFF) as u8);
            self.buf >>= 8;
            self.bits -= 8;
            remaining -= 1;
        }
        if remaining > self.data.len().saturating_sub(self.pos) {
            return Err("Error -5 while decompressing data: incomplete or truncated stream".into());
        }
        out.extend_from_slice(&self.data[self.pos..self.pos + remaining]);
        self.pos += remaining;
        Ok(())
    }

    /// Bytes consumed from `data` (backing out unread whole bytes buffered in `buf`).
    fn byte_offset(&self) -> usize {
        self.pos - (self.bits as usize / 8)
    }
}

struct HuffTable {
    /// Primary lookup table indexed by `table_bits` LSB bits:
    /// entry = (sym << 5) | bit_len, or 0 for invalid.
    table: Vec<u32>,
    table_bits: u32,
    /// Fallback for codes longer than `table_bits`: sorted by (len, canonical_code).
    counts: [u16; 16],
    symbols: Vec<u16>,
    max_bits: u32,
}

impl HuffTable {
    fn build(lengths: &[u8]) -> Result<Self, String> {
        let mut counts = [0u16; 16];
        let mut max_bits = 0u32;
        for &len in lengths {
            if len > 15 {
                return Err("Error -3 while decompressing data: invalid bit length".into());
            }
            if len > 0 {
                counts[len as usize] += 1;
                max_bits = max_bits.max(len as u32);
            }
        }
        if max_bits == 0 {
            return Ok(Self {
                table: vec![0],
                table_bits: 1,
                counts,
                symbols: Vec::new(),
                max_bits: 0,
            });
        }
        // Check Kraft inequality (allow single-symbol length-1 tree as RFC 1951 permits).
        let mut left = 1i32;
        for bits in 1..=max_bits as usize {
            left = (left << 1) - counts[bits] as i32;
            if left < 0 {
                return Err("Error -3 while decompressing data: oversubscribed Huffman tree".into());
            }
        }
        let mut offsets = [0u16; 16];
        for bits in 1..15 {
            offsets[bits + 1] = offsets[bits] + counts[bits];
        }
        let total = offsets[15] as usize + counts[15] as usize;
        let mut symbols = vec![0u16; total];
        for (sym, &len) in lengths.iter().enumerate() {
            if len > 0 {
                let idx = offsets[len as usize] as usize;
                offsets[len as usize] += 1;
                symbols[idx] = sym as u16;
            }
        }
        let table_bits = max_bits.min(10);
        let table_size = 1usize << table_bits;
        let mut table = vec![0u32; table_size];
        let mut code = 0u32;
        let mut sym_idx = 0usize;
        for bits in 1..=max_bits as usize {
            let count = counts[bits] as usize;
            if bits <= table_bits as usize {
                let step = 1usize << bits;
                for _ in 0..count {
                    let sym = symbols[sym_idx] as u32;
                    sym_idx += 1;
                    let rev = reverse_bits(code as u16, bits as u8) as usize;
                    let entry = (sym << 5) | (bits as u32);
                    let mut idx = rev;
                    while idx < table_size {
                        table[idx] = entry;
                        idx += step;
                    }
                    code += 1;
                }
            } else {
                sym_idx += count;
                code += count as u32;
            }
            code <<= 1;
        }
        Ok(Self {
            table,
            table_bits,
            counts,
            symbols,
            max_bits,
        })
    }

    #[inline]
    fn decode(&self, r: &mut BitReader<'_>) -> Result<u16, String> {
        if self.max_bits == 0 {
            return Err("Error -3 while decompressing data: empty Huffman tree".into());
        }
        r.fill(self.max_bits);
        let mask = (1u64 << self.table_bits) - 1;
        let entry = self.table[(r.buf & mask) as usize];
        if entry != 0 {
            let len = entry & 31;
            if r.bits < len {
                return Err("Error -5 while decompressing data: incomplete or truncated stream".into());
            }
            r.buf >>= len;
            r.bits -= len;
            return Ok((entry >> 5) as u16);
        }
        // Slow path for codes > table_bits (11..=15 bits).
        let mut code = 0i32;
        let mut first = 0i32;
        let mut index = 0i32;
        for bits in 1..=self.max_bits as usize {
            let b = r.read_bits(1)? as i32;
            code |= b;
            let count = self.counts[bits] as i32;
            if code - count < first {
                return Ok(self.symbols[(index + (code - first)) as usize]);
            }
            index += count;
            first = (first + count) << 1;
            code <<= 1;
        }
        Err("Error -3 while decompressing data: invalid Huffman code".into())
    }
}

fn fixed_tables() -> (HuffTable, HuffTable) {
    let mut lit_len = [0u8; 288];
    for i in 0..=143 {
        lit_len[i] = 8;
    }
    for i in 144..=255 {
        lit_len[i] = 9;
    }
    for i in 256..=279 {
        lit_len[i] = 7;
    }
    for i in 280..=287 {
        lit_len[i] = 8;
    }
    let dist_len = [5u8; 32];
    (
        HuffTable::build(&lit_len).expect("fixed lit table"),
        HuffTable::build(&dist_len).expect("fixed dist table"),
    )
}

pub(crate) fn inflate_raw(data: &[u8], max_output: usize) -> Result<(Vec<u8>, usize), String> {
    let mut r = BitReader::new(data);
    let mut out: Vec<u8> = Vec::with_capacity(data.len().min(65536));
    let mut fixed_cache: Option<(HuffTable, HuffTable)> = None;

    loop {
        let bfinal = r.read_bits(1)?;
        let btype = r.read_bits(2)?;
        match btype {
            0 => {
                r.align_byte();
                let len = r.read_u16_le()? as usize;
                let nlen = r.read_u16_le()? as usize;
                if (len ^ 0xFFFF) != nlen {
                    return Err(
                        "Error -3 while decompressing data: invalid stored block lengths".into(),
                    );
                }
                if out.len().saturating_add(len) > max_output {
                    return Err("Error -3 while decompressing data: output exceeds size limit".into());
                }
                r.read_exact_bytes(len, &mut out)?;
            }
            1 | 2 => {
                let (dyn_lit, dyn_dist);
                let (lit_tbl, dist_tbl) = if btype == 1 {
                    let (lt, dt) = fixed_cache.get_or_insert_with(fixed_tables);
                    (lt as &HuffTable, dt as &HuffTable)
                } else {
                    let hlit = r.read_bits(5)? as usize + 257;
                    let hdist = r.read_bits(5)? as usize + 1;
                    let hclen = r.read_bits(4)? as usize + 4;
                    if hlit > 286 || hdist > 30 {
                        return Err(
                            "Error -3 while decompressing data: too many length or distance symbols"
                                .into(),
                        );
                    }
                    let mut cl_lens = [0u8; 19];
                    for i in 0..hclen {
                        cl_lens[CL_ORDER[i]] = r.read_bits(3)? as u8;
                    }
                    let cl_tbl = HuffTable::build(&cl_lens)?;
                    let total = hlit + hdist;
                    let mut lengths = Vec::with_capacity(total);
                    while lengths.len() < total {
                        let sym = cl_tbl.decode(&mut r)?;
                        match sym {
                            0..=15 => lengths.push(sym as u8),
                            16 => {
                                let prev = *lengths.last().ok_or_else(|| {
                                    "Error -3 while decompressing data: invalid code lengths set"
                                        .to_string()
                                })?;
                                let rep = r.read_bits(2)? as usize + 3;
                                if lengths.len() + rep > total {
                                    return Err(
                                        "Error -3 while decompressing data: invalid code lengths set"
                                            .into(),
                                    );
                                }
                                lengths.resize(lengths.len() + rep, prev);
                            }
                            17 => {
                                let rep = r.read_bits(3)? as usize + 3;
                                if lengths.len() + rep > total {
                                    return Err(
                                        "Error -3 while decompressing data: invalid code lengths set"
                                            .into(),
                                    );
                                }
                                lengths.resize(lengths.len() + rep, 0);
                            }
                            18 => {
                                let rep = r.read_bits(7)? as usize + 11;
                                if lengths.len() + rep > total {
                                    return Err(
                                        "Error -3 while decompressing data: invalid code lengths set"
                                            .into(),
                                    );
                                }
                                lengths.resize(lengths.len() + rep, 0);
                            }
                            _ => {
                                return Err(
                                    "Error -3 while decompressing data: invalid code length symbol"
                                        .into(),
                                );
                            }
                        }
                    }
                    dyn_lit = HuffTable::build(&lengths[..hlit])?;
                    dyn_dist = HuffTable::build(&lengths[hlit..])?;
                    (&dyn_lit, &dyn_dist)
                };

                loop {
                    let sym = lit_tbl.decode(&mut r)?;
                    if sym < 256 {
                        if out.len() >= max_output {
                            return Err(
                                "Error -3 while decompressing data: output exceeds size limit"
                                    .into(),
                            );
                        }
                        out.push(sym as u8);
                    } else if sym == 256 {
                        break;
                    } else if sym <= 285 {
                        let li = (sym - 257) as usize;
                        let length =
                            LEN_BASE[li] as usize + r.read_bits(LEN_EXTRA[li] as u32)? as usize;
                        let dsym = dist_tbl.decode(&mut r)? as usize;
                        if dsym >= 30 {
                            return Err(
                                "Error -3 while decompressing data: invalid distance code".into(),
                            );
                        }
                        let dist =
                            DIST_BASE[dsym] as usize + r.read_bits(DIST_EXTRA[dsym] as u32)? as usize;
                        if dist > out.len() {
                            return Err(
                                "Error -3 while decompressing data: invalid distance too far back"
                                    .into(),
                            );
                        }
                        if out.len().saturating_add(length) > max_output {
                            return Err(
                                "Error -3 while decompressing data: output exceeds size limit"
                                    .into(),
                            );
                        }
                        let start = out.len() - dist;
                        if dist >= length {
                            out.extend_from_within(start..start + length);
                        } else {
                            for i in 0..length {
                                let b = out[start + i];
                                out.push(b);
                            }
                        }
                    } else {
                        return Err(
                            "Error -3 while decompressing data: invalid literal/length code".into(),
                        );
                    }
                }
            }
            _ => {
                return Err("Error -3 while decompressing data: invalid block type".into());
            }
        }
        if bfinal != 0 {
            break;
        }
    }

    r.align_byte();
    Ok((out, r.byte_offset()))
}

// ── Deflate (RFC 1951 compression) ───────────────────────────────────────────

struct BitWriter {
    out: Vec<u8>,
    buf: u64,
    bits: u32,
}

impl BitWriter {
    fn new(cap: usize) -> Self {
        Self {
            out: Vec::with_capacity(cap),
            buf: 0,
            bits: 0,
        }
    }

    #[inline]
    fn write_bits(&mut self, val: u32, n: u32) {
        self.buf |= (val as u64) << self.bits;
        self.bits += n;
        while self.bits >= 8 {
            self.out.push((self.buf & 0xFF) as u8);
            self.buf >>= 8;
            self.bits -= 8;
        }
    }

    fn align_byte(&mut self) {
        if self.bits > 0 {
            self.out.push((self.buf & 0xFF) as u8);
            self.buf = 0;
            self.bits = 0;
        }
    }

    fn write_u16_le(&mut self, v: u16) {
        self.out.push((v & 0xFF) as u8);
        self.out.push((v >> 8) as u8);
    }

    fn finish(mut self) -> Vec<u8> {
        self.align_byte();
        self.out
    }
}

#[inline]
fn len_code(len: u16) -> (usize, u8, u16) {
    // Returns (symbol_257_285, extra_bits, extra_val).
    if len == 258 {
        return (285, 0, 0);
    }
    for i in 0..28 {
        let next_base = LEN_BASE[i + 1];
        if len < next_base {
            return (257 + i, LEN_EXTRA[i], len - LEN_BASE[i]);
        }
    }
    (285, 0, 0)
}

#[inline]
fn dist_code(dist: u16) -> (usize, u8, u16) {
    // Returns (symbol_0_29, extra_bits, extra_val).
    for i in 0..29 {
        if dist < DIST_BASE[i + 1] {
            return (i, DIST_EXTRA[i], dist - DIST_BASE[i]);
        }
    }
    (29, DIST_EXTRA[29], dist - DIST_BASE[29])
}

#[derive(Clone, Copy)]
enum Token {
    Lit(u8),
    Match { len: u16, dist: u16 },
}

fn fixed_lit_lengths() -> [u8; 288] {
    let mut l = [0u8; 288];
    for i in 0..=143 {
        l[i] = 8;
    }
    for i in 144..=255 {
        l[i] = 9;
    }
    for i in 256..=279 {
        l[i] = 7;
    }
    for i in 280..=287 {
        l[i] = 8;
    }
    l
}

/// RLE-encode `hlit + hdist` Huffman lengths into code-length tokens `(sym, extra_bits, extra_val)`.
fn rle_code_lengths(lens: &[u8]) -> Vec<(u8, u8, u8)> {
    let mut out = Vec::with_capacity(lens.len());
    let mut i = 0usize;
    while i < lens.len() {
        let v = lens[i];
        let mut run = 1usize;
        while i + run < lens.len() && lens[i + run] == v {
            run += 1;
        }
        if v == 0 {
            let mut rem = run;
            while rem >= 11 {
                let take = rem.min(138);
                out.push((18, 7, (take - 11) as u8));
                rem -= take;
            }
            if rem >= 3 {
                out.push((17, 3, (rem - 3) as u8));
                rem = 0;
            }
            for _ in 0..rem {
                out.push((0, 0, 0));
            }
        } else {
            out.push((v, 0, 0));
            let mut rem = run - 1;
            while rem >= 3 {
                let take = rem.min(6);
                out.push((16, 2, (take - 3) as u8));
                rem -= take;
            }
            for _ in 0..rem {
                out.push((v, 0, 0));
            }
        }
        i += run;
    }
    out
}

fn emit_block(bw: &mut BitWriter, tokens: &[Token], raw: &[u8], is_final: bool) {
    let bfinal = if is_final { 1u32 } else { 0u32 };

    // Count symbol frequencies.
    let mut lit_freqs = [0u32; 286];
    let mut dist_freqs = [0u32; 30];
    let mut extra_bits_total = 0u64;
    lit_freqs[256] = 1; // End-of-block symbol.
    for &tok in tokens {
        match tok {
            Token::Lit(b) => lit_freqs[b as usize] += 1,
            Token::Match { len, dist } => {
                let (lsym, lextra, _) = len_code(len);
                let (dsym, dextra, _) = dist_code(dist);
                lit_freqs[lsym] += 1;
                dist_freqs[dsym] += 1;
                extra_bits_total += (lextra + dextra) as u64;
            }
        }
    }

    // Cost of fixed Huffman block (BTYPE = 1).
    let fixed_ll = fixed_lit_lengths();
    let mut fixed_bits: u64 = 3 + extra_bits_total;
    for (s, &f) in lit_freqs.iter().enumerate() {
        fixed_bits += f as u64 * fixed_ll[s] as u64;
    }
    for &f in &dist_freqs {
        fixed_bits += f as u64 * 5;
    }

    // Build dynamic Huffman trees (BTYPE = 2) and compute exact bit cost.
    let dyn_ll = build_huffman_lengths(&lit_freqs, 15);
    let mut dyn_dl = build_huffman_lengths(&dist_freqs, 15);
    if dyn_dl.iter().all(|&l| l == 0) {
        dyn_dl[0] = 1;
    }
    let mut hlit = 286usize;
    while hlit > 257 && dyn_ll[hlit - 1] == 0 {
        hlit -= 1;
    }
    let mut hdist = 30usize;
    while hdist > 1 && dyn_dl[hdist - 1] == 0 {
        hdist -= 1;
    }
    let mut combined = Vec::with_capacity(hlit + hdist);
    combined.extend_from_slice(&dyn_ll[..hlit]);
    combined.extend_from_slice(&dyn_dl[..hdist]);
    let rle = rle_code_lengths(&combined);
    let mut cl_freqs = [0u32; 19];
    for &(sym, _, _) in &rle {
        cl_freqs[sym as usize] += 1;
    }
    let cl_lens = build_huffman_lengths(&cl_freqs, 7);
    let mut hclen = 19usize;
    while hclen > 4 && cl_lens[CL_ORDER[hclen - 1]] == 0 {
        hclen -= 1;
    }
    let mut dyn_bits: u64 = 3 + 5 + 5 + 4 + (hclen as u64 * 3) + extra_bits_total;
    for &(sym, eb, _) in &rle {
        dyn_bits += cl_lens[sym as usize] as u64 + eb as u64;
    }
    for (s, &f) in lit_freqs.iter().enumerate() {
        dyn_bits += f as u64 * dyn_ll[s] as u64;
    }
    for (s, &f) in dist_freqs.iter().enumerate() {
        dyn_bits += f as u64 * dyn_dl[s] as u64;
    }

    // Cost of stored block(s) (BTYPE = 0) if raw fits in one 65535-byte block.
    let pad_bits = (8 - ((bw.bits + 3) & 7)) & 7;
    let stored_bits = if raw.len() <= 65535 {
        3 + pad_bits as u64 + 32 + raw.len() as u64 * 8
    } else {
        u64::MAX
    };

    if stored_bits <= fixed_bits && stored_bits <= dyn_bits {
        bw.write_bits(bfinal, 1);
        bw.write_bits(0, 2);
        bw.align_byte();
        let len = raw.len() as u16;
        bw.write_u16_le(len);
        bw.write_u16_le(!len);
        bw.out.extend_from_slice(raw);
    } else if fixed_bits <= dyn_bits {
        let lit_codes = build_canonical_codes(&fixed_ll).expect("fixed lit codes");
        let dist_codes = build_canonical_codes(&[5u8; 32]).expect("fixed dist codes");
        bw.write_bits(bfinal, 1);
        bw.write_bits(1, 2);
        write_tokens(bw, tokens, &lit_codes, &fixed_ll, &dist_codes, &[5u8; 32]);
    } else {
        let cl_codes = build_canonical_codes(&cl_lens).expect("cl codes");
        let lit_codes = build_canonical_codes(&dyn_ll).expect("dyn lit codes");
        let dist_codes = build_canonical_codes(&dyn_dl).expect("dyn dist codes");
        bw.write_bits(bfinal, 1);
        bw.write_bits(2, 2);
        bw.write_bits((hlit - 257) as u32, 5);
        bw.write_bits((hdist - 1) as u32, 5);
        bw.write_bits((hclen - 4) as u32, 4);
        for i in 0..hclen {
            bw.write_bits(cl_lens[CL_ORDER[i]] as u32, 3);
        }
        for &(sym, eb, ev) in &rle {
            bw.write_bits(cl_codes[sym as usize] as u32, cl_lens[sym as usize] as u32);
            if eb > 0 {
                bw.write_bits(ev as u32, eb as u32);
            }
        }
        write_tokens(bw, tokens, &lit_codes, &dyn_ll, &dist_codes, &dyn_dl);
    }
}

fn write_tokens(
    bw: &mut BitWriter,
    tokens: &[Token],
    lit_codes: &[u16],
    lit_lens: &[u8],
    dist_codes: &[u16],
    dist_lens: &[u8],
) {
    for &tok in tokens {
        match tok {
            Token::Lit(b) => {
                let s = b as usize;
                bw.write_bits(lit_codes[s] as u32, lit_lens[s] as u32);
            }
            Token::Match { len, dist } => {
                let (lsym, lextra, lval) = len_code(len);
                bw.write_bits(lit_codes[lsym] as u32, lit_lens[lsym] as u32);
                if lextra > 0 {
                    bw.write_bits(lval as u32, lextra as u32);
                }
                let (dsym, dextra, dval) = dist_code(dist);
                bw.write_bits(dist_codes[dsym] as u32, dist_lens[dsym] as u32);
                if dextra > 0 {
                    bw.write_bits(dval as u32, dextra as u32);
                }
            }
        }
    }
    bw.write_bits(lit_codes[256] as u32, lit_lens[256] as u32);
}

struct LevelConfig {
    max_chain: usize,
    good_len: usize,
    nice_len: usize,
    max_lazy: usize,
}

fn level_config(level: i32) -> LevelConfig {
    match level {
        1 => LevelConfig { max_chain: 4, good_len: 4, nice_len: 8, max_lazy: 0 },
        2 => LevelConfig { max_chain: 8, good_len: 4, nice_len: 16, max_lazy: 0 },
        3 => LevelConfig { max_chain: 32, good_len: 4, nice_len: 32, max_lazy: 0 },
        4 => LevelConfig { max_chain: 16, good_len: 4, nice_len: 16, max_lazy: 4 },
        5 => LevelConfig { max_chain: 32, good_len: 8, nice_len: 32, max_lazy: 16 },
        6 => LevelConfig { max_chain: 128, good_len: 8, nice_len: 128, max_lazy: 16 },
        7 => LevelConfig { max_chain: 256, good_len: 8, nice_len: 128, max_lazy: 32 },
        8 => LevelConfig { max_chain: 1024, good_len: 32, nice_len: 258, max_lazy: 128 },
        _ => LevelConfig { max_chain: 4096, good_len: 32, nice_len: 258, max_lazy: 258 },
    }
}

#[inline]
fn hash3(data: &[u8], pos: usize) -> usize {
    let v = ((data[pos] as u32) << 16) | ((data[pos + 1] as u32) << 8) | (data[pos + 2] as u32);
    ((v.wrapping_mul(0x1E35_A7BD)) >> 17) as usize & 0x7FFF
}

pub(crate) fn deflate_raw(input: &[u8], level: i32) -> Result<Vec<u8>, String> {
    let level = if level == -1 { 6 } else { level };
    if !(0..=9).contains(&level) {
        return Err("Bad compression level".into());
    }
    if level == 0 || input.is_empty() {
        let mut bw = BitWriter::new(input.len() + (input.len() / 65535 + 1) * 5);
        if input.is_empty() {
            bw.write_bits(1, 1);
            bw.write_bits(0, 2);
            bw.align_byte();
            bw.write_u16_le(0);
            bw.write_u16_le(0xFFFF);
            return Ok(bw.finish());
        }
        let mut pos = 0usize;
        while pos < input.len() {
            let chunk = (input.len() - pos).min(65535);
            let is_final = pos + chunk == input.len();
            bw.write_bits(if is_final { 1 } else { 0 }, 1);
            bw.write_bits(0, 2);
            bw.align_byte();
            bw.write_u16_le(chunk as u16);
            bw.write_u16_le(!(chunk as u16));
            bw.out.extend_from_slice(&input[pos..pos + chunk]);
            pos += chunk;
        }
        return Ok(bw.finish());
    }

    const WSIZE: usize = 32768;
    const WMASK: usize = WSIZE - 1;
    const NIL: u32 = u32::MAX;
    const BLOCK_TOKENS: usize = 32768;

    let cfg = level_config(level);
    let mut head = vec![NIL; WSIZE];
    let mut prev = vec![NIL; WSIZE];
    let mut bw = BitWriter::new(input.len() / 2 + 64);
    let mut tokens: Vec<Token> = Vec::with_capacity(BLOCK_TOKENS);
    let mut block_start = 0usize;

    let find_match = |pos: usize,
                      prev_len: usize,
                      head_pos: u32,
                      prev: &[u32]|
     -> (usize, usize) {
        let max_len = (input.len() - pos).min(258);
        if max_len < 3 {
            return (0, 0);
        }
        let mut best_len = prev_len.max(2);
        let mut best_dist = 0usize;
        let nice = cfg.nice_len.min(max_len);
        let mut chain = if prev_len >= cfg.good_len {
            cfg.max_chain >> 2
        } else {
            cfg.max_chain
        };
        let mut cur = head_pos;
        let min_pos = pos.saturating_sub(WSIZE);
        while cur != NIL && chain > 0 {
            let c = cur as usize;
            if c < min_pos || c >= pos {
                break;
            }
            let dist = pos - c;
            if dist > WSIZE {
                break;
            }
            // Quick check on the byte at `best_len` before full comparison.
            if best_len < max_len
                && input[c + best_len] == input[pos + best_len]
                && input[c] == input[pos]
                && input[c + 1] == input[pos + 1]
            {
                let mut mlen = 2usize;
                while mlen < max_len && input[c + mlen] == input[pos + mlen] {
                    mlen += 1;
                }
                if mlen > best_len
                    && !(mlen == 3 && dist > 4096)
                    && !(best_dist > 0 && best_dist <= 1024 && dist > 4096 && mlen == best_len + 1)
                {
                    best_len = mlen;
                    best_dist = dist;
                    if best_len >= nice {
                        break;
                    }
                }
            }
            let next = prev[c & WMASK];
            if next >= cur {
                break;
            }
            cur = next;
            chain -= 1;
        }
        if best_dist == 0 {
            (0, 0)
        } else {
            (best_len, best_dist)
        }
    };

    let insert_hash = |pos: usize, head: &mut [u32], prev: &mut [u32]| -> u32 {
        let h = hash3(input, pos);
        let old = head[h];
        prev[pos & WMASK] = old;
        head[h] = pos as u32;
        old
    };

    let mut pos = 0usize;
    let mut pending_match: Option<(usize, usize)> = None; // (len, dist) for lazy matching

    while pos < input.len() {
        let (mlen, mdist) = if pos + 2 < input.len() {
            let old = insert_hash(pos, &mut head, &mut prev);
            let prev_best = pending_match.map_or(0, |(l, _)| l);
            find_match(pos, prev_best, old, &prev)
        } else {
            (0, 0)
        };

        if let Some((plen, pdist)) = pending_match {
            if mlen > plen {
                // Better match at pos: emit previous literal at pos - 1 and keep new match.
                tokens.push(Token::Lit(input[pos - 1]));
                if mlen < cfg.max_lazy && pos + 1 < input.len() {
                    pending_match = Some((mlen, mdist));
                    pos += 1;
                } else {
                    pending_match = None;
                    tokens.push(Token::Match {
                        len: mlen as u16,
                        dist: mdist as u16,
                    });
                    for k in 1..mlen {
                        if pos + k + 2 < input.len() {
                            insert_hash(pos + k, &mut head, &mut prev);
                        }
                    }
                    pos += mlen;
                }
            } else {
                // Emit pending match at pos - 1.
                pending_match = None;
                tokens.push(Token::Match {
                    len: plen as u16,
                    dist: pdist as u16,
                });
                // Hash positions covered by the match (pos was already hashed above).
                for k in 2..plen {
                    if pos - 1 + k + 2 < input.len() {
                        insert_hash(pos - 1 + k, &mut head, &mut prev);
                    }
                }
                pos = (pos - 1) + plen;
            }
        } else if mlen >= 3 {
            if cfg.max_lazy > 0 && mlen < cfg.max_lazy && pos + 1 < input.len() {
                pending_match = Some((mlen, mdist));
                pos += 1;
            } else {
                tokens.push(Token::Match {
                    len: mlen as u16,
                    dist: mdist as u16,
                });
                for k in 1..mlen {
                    if pos + k + 2 < input.len() {
                        insert_hash(pos + k, &mut head, &mut prev);
                    }
                }
                pos += mlen;
            }
        } else {
            tokens.push(Token::Lit(input[pos]));
            pos += 1;
        }

        if tokens.len() >= BLOCK_TOKENS && pending_match.is_none() && pos < input.len() {
            emit_block(&mut bw, &tokens, &input[block_start..pos], false);
            tokens.clear();
            block_start = pos;
        }
    }

    if let Some((plen, pdist)) = pending_match {
        tokens.push(Token::Match {
            len: plen as u16,
            dist: pdist as u16,
        });
    }
    emit_block(&mut bw, &tokens, &input[block_start..], true);
    Ok(bw.finish())
}

// ── Framing: raw / zlib (RFC 1950) / gzip (RFC 1952) ─────────────────────────

pub(crate) fn compress_framed(data: &[u8], level: i32, wbits: i32) -> Result<Vec<u8>, String> {
    let eff_level = if level == -1 { 6 } else { level };
    if !(0..=9).contains(&eff_level) {
        return Err("Bad compression level".into());
    }
    if (-15..=-9).contains(&wbits) {
        // Raw DEFLATE stream (used by zipfile).
        deflate_raw(data, eff_level)
    } else if (9..=15).contains(&wbits) {
        // RFC 1950 zlib stream.
        let raw = deflate_raw(data, eff_level)?;
        let cmf: u8 = (((wbits as u8) - 8) << 4) | 8;
        let flevel: u8 = match eff_level {
            0..=1 => 0,
            2..=5 => 1,
            6 => 2,
            _ => 3,
        };
        let mut flg: u8 = flevel << 6;
        let rem = ((cmf as u16) * 256 + flg as u16) % 31;
        if rem != 0 {
            flg += (31 - rem) as u8;
        }
        let mut out = Vec::with_capacity(raw.len() + 6);
        out.push(cmf);
        out.push(flg);
        out.extend_from_slice(&raw);
        out.extend_from_slice(&adler32(data, 1).to_be_bytes());
        Ok(out)
    } else if (25..=31).contains(&wbits) {
        // RFC 1952 gzip stream.
        let raw = deflate_raw(data, eff_level)?;
        let mut out = Vec::with_capacity(raw.len() + 18);
        out.extend_from_slice(&[0x1f, 0x8b, 8, 0, 0, 0, 0, 0, if eff_level == 9 { 2 } else if eff_level == 1 { 4 } else { 0 }, 255]);
        out.extend_from_slice(&raw);
        out.extend_from_slice(&crc32(data, 0).to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        Ok(out)
    } else {
        Err("Invalid initialization option".into())
    }
}

pub(crate) fn decompress_framed(
    data: &[u8],
    wbits: i32,
    max_length: usize,
) -> Result<Vec<u8>, String> {
    let cap = if max_length == 0 {
        DEFAULT_MAX_OUTPUT
    } else {
        max_length.min(DEFAULT_MAX_OUTPUT)
    };
    if (-15..=-8).contains(&wbits) {
        let (out, _) = inflate_raw(data, cap)?;
        return Ok(out);
    }
    let is_gzip = if wbits >= 32 + 8 && wbits <= 32 + 15 {
        data.len() >= 2 && data[0] == 0x1f && data[1] == 0x8b
    } else if (16 + 8..=16 + 15).contains(&wbits) {
        true
    } else if wbits == 0 || (8..=15).contains(&wbits) {
        false
    } else {
        return Err("Invalid initialization option".into());
    };

    if is_gzip {
        if data.len() < 18 || data[0] != 0x1f || data[1] != 0x8b || data[2] != 8 {
            return Err("Error -3 while decompressing data: incorrect header check".into());
        }
        let flg = data[3];
        let mut pos = 10usize;
        if (flg & 0x04) != 0 {
            if pos + 2 > data.len() {
                return Err("Error -5 while decompressing data: truncated gzip extra".into());
            }
            let xlen = u16::from_le_bytes([data[pos], data[pos + 1]]) as usize;
            pos += 2 + xlen;
        }
        if (flg & 0x08) != 0 {
            while pos < data.len() && data[pos] != 0 {
                pos += 1;
            }
            pos += 1;
        }
        if (flg & 0x10) != 0 {
            while pos < data.len() && data[pos] != 0 {
                pos += 1;
            }
            pos += 1;
        }
        if (flg & 0x02) != 0 {
            pos += 2;
        }
        if pos > data.len() {
            return Err("Error -5 while decompressing data: truncated gzip header".into());
        }
        let (out, consumed) = inflate_raw(&data[pos..], cap)?;
        let trailer = pos + consumed;
        if trailer + 8 > data.len() {
            return Err("Error -5 while decompressing data: incomplete or truncated stream".into());
        }
        let want_crc = u32::from_le_bytes([
            data[trailer],
            data[trailer + 1],
            data[trailer + 2],
            data[trailer + 3],
        ]);
        let want_size = u32::from_le_bytes([
            data[trailer + 4],
            data[trailer + 5],
            data[trailer + 6],
            data[trailer + 7],
        ]);
        if crc32(&out, 0) != want_crc || (out.len() as u32) != want_size {
            return Err("Error -3 while decompressing data: incorrect data check".into());
        }
        Ok(out)
    } else {
        // RFC 1950 zlib stream.
        if data.len() < 6 {
            return Err("Error -5 while decompressing data: incomplete or truncated stream".into());
        }
        let cmf = data[0];
        let flg = data[1];
        if (cmf & 0x0F) != 8 || (cmf >> 4) > 7 || (((cmf as u16) * 256 + flg as u16) % 31) != 0 {
            return Err("Error -3 while decompressing data: incorrect header check".into());
        }
        if (flg & 0x20) != 0 {
            return Err("Error -2 while decompressing data: preset dictionary not supported".into());
        }
        let (out, consumed) = inflate_raw(&data[2..], cap)?;
        let trailer = 2 + consumed;
        if trailer + 4 > data.len() {
            return Err("Error -5 while decompressing data: incomplete or truncated stream".into());
        }
        let want_adler = u32::from_be_bytes([
            data[trailer],
            data[trailer + 1],
            data[trailer + 2],
            data[trailer + 3],
        ]);
        if adler32(&out, 1) != want_adler {
            return Err("Error -3 while decompressing data: incorrect data check".into());
        }
        Ok(out)
    }
}

// ── VM entry point ───────────────────────────────────────────────────────────

impl<'p> Vm<'p> {
    fn py_zlib_read_bytes(&self, v: Value) -> Result<Vec<u8>, Thrown> {
        if !v.is_heap() {
            return Err(Thrown::from_static("TypeError: expected bytes array"));
        }
        match self.heap.get(v.heap_index()) {
            HeapObj::Array(items) => {
                let mut out = Vec::with_capacity(items.len());
                for &x in items {
                    out.push(x.as_f64() as u8);
                }
                Ok(out)
            }
            HeapObj::TypedArray {
                buffer,
                byte_offset,
                length,
                ..
            } => {
                let off = *byte_offset;
                let len = *length;
                match self.heap.get(*buffer) {
                    HeapObj::ArrayBuffer { data, detached } if !*detached => {
                        Ok(data.get(off..off + len).unwrap_or(&[]).to_vec())
                    }
                    _ => Err(Thrown::from_static("TypeError: detached buffer")),
                }
            }
            _ => Err(Thrown::from_static("TypeError: expected bytes array")),
        }
    }

    /// `__zipp_py_zlib(op, items, arg2, arg3)`: see module doc.
    pub(crate) fn py_zlib(&mut self, args: &[Value]) -> Result<Value, Thrown> {
        let arg = |i: usize| args.get(i).copied().unwrap_or(Value::UNDEFINED);
        let op = arg(0).as_f64() as i32;
        let data = self.py_zlib_read_bytes(arg(1))?;
        match op {
            0 => {
                let level = arg(2).as_f64() as i32;
                let wbits = arg(3).as_f64() as i32;
                let compressed = compress_framed(&data, level, wbits)
                    .map_err(|e| Thrown(format!("Error: {e}")))?;
                let items: Vec<Value> = compressed.into_iter().map(|b| Value::num(b as f64)).collect();
                Ok(self.alloc_array_current_realm(items))
            }
            1 => {
                let wbits = arg(2).as_f64() as i32;
                let max_len = if arg(3).is_number() && arg(3).as_f64() > 0.0 {
                    arg(3).as_f64() as usize
                } else {
                    0
                };
                let decompressed = decompress_framed(&data, wbits, max_len)
                    .map_err(|e| Thrown(format!("Error: {e}")))?;
                let items: Vec<Value> = decompressed
                    .into_iter()
                    .map(|b| Value::num(b as f64))
                    .collect();
                Ok(self.alloc_array_current_realm(items))
            }
            2 => {
                let start = if arg(2).is_number() {
                    arg(2).as_f64() as i64 as u32
                } else {
                    0
                };
                Ok(Value::num(crc32(&data, start) as f64))
            }
            3 => {
                let start = if arg(2).is_number() {
                    arg(2).as_f64() as i64 as u32
                } else {
                    1
                };
                Ok(Value::num(adler32(&data, start) as f64))
            }
            _ => Err(Thrown::from_static("TypeError: unknown zlib op")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deflate_inflate_roundtrip_all_levels() {
        let mut mixed = Vec::new();
        for i in 0..2000u32 {
            mixed.extend_from_slice(format!("line {i}: hello world hello world!\n").as_bytes());
        }
        let pseudo_random: Vec<u8> = (0..5000u32)
            .map(|i| ((i.wrapping_mul(2654435761)) >> 24) as u8)
            .collect();
        let cases: &[&[u8]] = &[
            b"",
            b"a",
            b"hello world",
            &b"hello world".repeat(1000),
            &mixed,
            &pseudo_random,
        ];
        for &data in cases {
            for level in [-1, 0, 1, 3, 6, 9] {
                for wbits in [-15, 15, 31] {
                    let comp = compress_framed(data, level, wbits)
                        .unwrap_or_else(|e| panic!("compress len={} lvl={level} w={wbits}: {e}", data.len()));
                    let back = decompress_framed(&comp, wbits, 0)
                        .unwrap_or_else(|e| panic!("decompress len={} lvl={level} w={wbits}: {e}", data.len()));
                    assert_eq!(back, data, "roundtrip len={} lvl={level} w={wbits}", data.len());
                }
            }
        }
        // Verify real compression on repetitive and mixed inputs.
        let rep = b"hello world".repeat(1000);
        let c9 = compress_framed(&rep, 9, -15).unwrap();
        assert!(c9.len() < 200, "expected strong compression, got {} bytes", c9.len());
        let m0 = compress_framed(&mixed, 0, -15).unwrap();
        let m9 = compress_framed(&mixed, 9, -15).unwrap();
        assert!(m9.len() * 10 < m0.len(), "m9={} vs m0={}", m9.len(), m0.len());
    }
}
