//! Hand-written baseline JPEG (JFIF) encoder, no external dependency.
//!
//! Same approach as the other hand-written codecs in this crate (TIFF, EPS,
//! PDF, EDF, npy): the subset rsplot needs, written against the spec — here
//! ITU-T T.81 baseline sequential DCT, 8-bit, three components, **4:4:4** (no
//! chroma subsampling), with the standard Annex K quantization and Huffman
//! tables. silx saves JPEG through matplotlib/PIL; the on-disk container is
//! the same JFIF baseline stream.
//!
//! Encoding pipeline per 8×8 block: RGB → YCbCr (JFIF/BT.601 full-range) →
//! level shift (−128) → forward DCT → quantize (Annex K tables scaled by
//! [`JPEG_QUALITY`], IJG scaling) → zigzag → DC-difference / AC run-length
//! Huffman coding with 0xFF byte stuffing. Edge blocks replicate the last
//! row/column (standard edge padding, keeps edge gradients flat).

/// Fixed encode quality (IJG 1–100 scale; 50 = Annex K tables unscaled).
/// PIL's default is 75; rsplot uses 90 for crisper plot text at a small file
/// cost. Not configurable — the save path has no quality UI (like DPI, a
/// constant until a real need appears).
pub const JPEG_QUALITY: u8 = 90;

/// Zigzag scan order: `ZIGZAG[i]` is the (row-major) coefficient index emitted
/// at scan position `i` (T.81 Figure A.6).
const ZIGZAG: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20,
    13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59,
    52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

/// Annex K Table K.1 — luminance quantization (row-major).
const QUANT_LUMA: [u16; 64] = [
    16, 11, 10, 16, 24, 40, 51, 61, 12, 12, 14, 19, 26, 58, 60, 55, 14, 13, 16, 24, 40, 57, 69, 56,
    14, 17, 22, 29, 51, 87, 80, 62, 18, 22, 37, 56, 68, 109, 103, 77, 24, 35, 55, 64, 81, 104, 113,
    92, 49, 64, 78, 87, 103, 121, 120, 101, 72, 92, 95, 98, 112, 100, 103, 99,
];

/// Annex K Table K.2 — chrominance quantization (row-major).
const QUANT_CHROMA: [u16; 64] = [
    17, 18, 24, 47, 99, 99, 99, 99, 18, 21, 26, 66, 99, 99, 99, 99, 24, 26, 56, 99, 99, 99, 99, 99,
    47, 66, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99,
    99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99,
];

// Annex K Huffman table specs: BITS (codes per length 1..=16) + HUFFVAL.
const DC_LUMA_BITS: [u8; 16] = [0, 1, 5, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0];
const DC_LUMA_VALS: [u8; 12] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];
const DC_CHROMA_BITS: [u8; 16] = [0, 3, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0];
const DC_CHROMA_VALS: [u8; 12] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];
const AC_LUMA_BITS: [u8; 16] = [0, 2, 1, 3, 3, 2, 4, 3, 5, 5, 4, 4, 0, 0, 1, 125];
#[rustfmt::skip]
const AC_LUMA_VALS: [u8; 162] = [
    0x01, 0x02, 0x03, 0x00, 0x04, 0x11, 0x05, 0x12, 0x21, 0x31, 0x41, 0x06, 0x13, 0x51, 0x61,
    0x07, 0x22, 0x71, 0x14, 0x32, 0x81, 0x91, 0xa1, 0x08, 0x23, 0x42, 0xb1, 0xc1, 0x15, 0x52,
    0xd1, 0xf0, 0x24, 0x33, 0x62, 0x72, 0x82, 0x09, 0x0a, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x25,
    0x26, 0x27, 0x28, 0x29, 0x2a, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x43, 0x44, 0x45,
    0x46, 0x47, 0x48, 0x49, 0x4a, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5a, 0x63, 0x64,
    0x65, 0x66, 0x67, 0x68, 0x69, 0x6a, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a, 0x83,
    0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99,
    0x9a, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4, 0xb5, 0xb6,
    0xb7, 0xb8, 0xb9, 0xba, 0xc2, 0xc3, 0xc4, 0xc5, 0xc6, 0xc7, 0xc8, 0xc9, 0xca, 0xd2, 0xd3,
    0xd4, 0xd5, 0xd6, 0xd7, 0xd8, 0xd9, 0xda, 0xe1, 0xe2, 0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8,
    0xe9, 0xea, 0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa,
];
const AC_CHROMA_BITS: [u8; 16] = [0, 2, 1, 2, 4, 4, 3, 4, 7, 5, 4, 4, 0, 1, 2, 119];
#[rustfmt::skip]
const AC_CHROMA_VALS: [u8; 162] = [
    0x00, 0x01, 0x02, 0x03, 0x11, 0x04, 0x05, 0x21, 0x31, 0x06, 0x12, 0x41, 0x51, 0x07, 0x61,
    0x71, 0x13, 0x22, 0x32, 0x81, 0x08, 0x14, 0x42, 0x91, 0xa1, 0xb1, 0xc1, 0x09, 0x23, 0x33,
    0x52, 0xf0, 0x15, 0x62, 0x72, 0xd1, 0x0a, 0x16, 0x24, 0x34, 0xe1, 0x25, 0xf1, 0x17, 0x18,
    0x19, 0x1a, 0x26, 0x27, 0x28, 0x29, 0x2a, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x43, 0x44,
    0x45, 0x46, 0x47, 0x48, 0x49, 0x4a, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5a, 0x63,
    0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0x6a, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a,
    0x82, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97,
    0x98, 0x99, 0x9a, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4,
    0xb5, 0xb6, 0xb7, 0xb8, 0xb9, 0xba, 0xc2, 0xc3, 0xc4, 0xc5, 0xc6, 0xc7, 0xc8, 0xc9, 0xca,
    0xd2, 0xd3, 0xd4, 0xd5, 0xd6, 0xd7, 0xd8, 0xd9, 0xda, 0xe2, 0xe3, 0xe4, 0xe5, 0xe6, 0xe7,
    0xe8, 0xe9, 0xea, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa,
];

/// Scale an Annex K quantization table by `quality` (IJG mapping: 50 = the
/// table unscaled, 100 = all 1s) into the table actually used, clamped to the
/// baseline 1..=255 range, in zigzag order (as DQT stores it).
fn scaled_quant_zigzag(base: &[u16; 64], quality: u8) -> [u8; 64] {
    let q = quality.clamp(1, 100) as u32;
    let scale = if q < 50 { 5000 / q } else { 200 - 2 * q };
    let mut out = [0u8; 64];
    for (i, slot) in out.iter_mut().enumerate() {
        let v = (u32::from(base[ZIGZAG[i]]) * scale + 50) / 100;
        *slot = v.clamp(1, 255) as u8;
    }
    out
}

/// Canonical Huffman codes for a BITS + HUFFVAL spec: `codes[symbol]` =
/// `(code, length)` (T.81 Annex C code generation).
fn build_huffman_codes(bits: &[u8; 16], vals: &[u8]) -> [(u16, u8); 256] {
    let mut codes = [(0u16, 0u8); 256];
    let mut code = 0u16;
    let mut k = 0usize;
    for (len_minus_1, &count) in bits.iter().enumerate() {
        for _ in 0..count {
            codes[vals[k] as usize] = (code, len_minus_1 as u8 + 1);
            code += 1;
            k += 1;
        }
        code <<= 1;
    }
    codes
}

/// MSB-first bit writer with JPEG 0xFF byte stuffing.
struct BitWriter {
    out: Vec<u8>,
    acc: u32,
    nbits: u8,
}

impl BitWriter {
    fn new() -> Self {
        Self {
            out: Vec::new(),
            acc: 0,
            nbits: 0,
        }
    }

    fn put(&mut self, bits: u16, n: u8) {
        debug_assert!(n <= 16);
        self.acc = (self.acc << n) | u32::from(bits);
        self.nbits += n;
        while self.nbits >= 8 {
            let byte = ((self.acc >> (self.nbits - 8)) & 0xFF) as u8;
            self.out.push(byte);
            if byte == 0xFF {
                self.out.push(0x00); // byte stuffing (T.81 F.1.2.3)
            }
            self.nbits -= 8;
        }
        self.acc &= (1 << self.nbits) - 1;
    }

    /// Pad the final partial byte with 1-bits (T.81 F.1.2.3) and return the
    /// entropy-coded bytes.
    fn finish(mut self) -> Vec<u8> {
        if self.nbits > 0 {
            let pad = 8 - self.nbits;
            self.put((1 << pad) - 1, pad);
        }
        self.out
    }
}

/// Bit category of a DC difference / AC coefficient (T.81 F.1.2.1.1: the
/// number of bits needed for `|v|`), with the value bits to append (negative
/// values are stored as `v - 1` in `size` bits, i.e. one's complement).
fn category_and_bits(v: i32) -> (u8, u16) {
    let size = (32 - v.unsigned_abs().leading_zeros()) as u8;
    let bits = if v < 0 { v - 1 } else { v } as u16 & ((1u32 << size) - 1) as u16;
    (size, bits)
}

/// Forward 8×8 DCT (T.81 A.3.3) of a level-shifted block, row-major in/out.
fn fdct_8x8(block: &[f32; 64]) -> [f32; 64] {
    // cos((2x+1) u π / 16) lookup; small enough to recompute per call site
    // cheaply, but hoisted to a table for clarity.
    let mut cos_t = [[0.0f32; 8]; 8];
    for (x, row) in cos_t.iter_mut().enumerate() {
        for (u, c) in row.iter_mut().enumerate() {
            *c = (((2 * x + 1) as f32) * (u as f32) * std::f32::consts::PI / 16.0).cos();
        }
    }
    let cu = |u: usize| {
        if u == 0 {
            std::f32::consts::FRAC_1_SQRT_2
        } else {
            1.0
        }
    };
    let mut out = [0.0f32; 64];
    for v in 0..8 {
        for u in 0..8 {
            let mut sum = 0.0f32;
            for y in 0..8 {
                for x in 0..8 {
                    sum += block[y * 8 + x] * cos_t[x][u] * cos_t[y][v];
                }
            }
            out[v * 8 + u] = 0.25 * cu(u) * cu(v) * sum;
        }
    }
    out
}

/// JFIF (BT.601 full-range) RGB → YCbCr, per pixel.
fn rgb_to_ycbcr(r: u8, g: u8, b: u8) -> (f32, f32, f32) {
    let (r, g, b) = (f32::from(r), f32::from(g), f32::from(b));
    let y = 0.299 * r + 0.587 * g + 0.114 * b;
    let cb = -0.168_736 * r - 0.331_264 * g + 0.5 * b + 128.0;
    let cr = 0.5 * r - 0.418_688 * g - 0.081_312 * b + 128.0;
    (y, cb, cr)
}

/// Append a marker segment: 0xFF `marker`, 2-byte big-endian length covering
/// the length field + payload, then the payload.
fn push_segment(out: &mut Vec<u8>, marker: u8, payload: &[u8]) {
    out.extend_from_slice(&[0xFF, marker]);
    out.extend_from_slice(&((payload.len() as u16 + 2).to_be_bytes()));
    out.extend_from_slice(payload);
}

/// Encode an RGBA image (alpha ignored — JPEG has no alpha; the figure is
/// rendered over an opaque background) as a baseline JFIF JPEG at
/// [`JPEG_QUALITY`], recording `dpi` in the JFIF density fields.
pub fn encode_jpeg(rgba: &[u8], width: u32, height: u32, dpi: u32) -> Vec<u8> {
    assert_eq!(rgba.len(), (width * height * 4) as usize);
    let (w, h) = (width as usize, height as usize);

    let q_luma = scaled_quant_zigzag(&QUANT_LUMA, JPEG_QUALITY);
    let q_chroma = scaled_quant_zigzag(&QUANT_CHROMA, JPEG_QUALITY);
    let dc_l = build_huffman_codes(&DC_LUMA_BITS, &DC_LUMA_VALS);
    let ac_l = build_huffman_codes(&AC_LUMA_BITS, &AC_LUMA_VALS);
    let dc_c = build_huffman_codes(&DC_CHROMA_BITS, &DC_CHROMA_VALS);
    let ac_c = build_huffman_codes(&AC_CHROMA_BITS, &AC_CHROMA_VALS);

    // ── Headers ─────────────────────────────────────────────────────────
    let mut out = vec![0xFF, 0xD8]; // SOI
    // APP0 / JFIF 1.01, density in dots-per-inch.
    let density = (dpi.clamp(1, u32::from(u16::MAX)) as u16).to_be_bytes();
    let mut app0 = Vec::new();
    app0.extend_from_slice(b"JFIF\0");
    app0.extend_from_slice(&[1, 1, 1]); // version 1.01, units = dpi
    app0.extend_from_slice(&density);
    app0.extend_from_slice(&density);
    app0.extend_from_slice(&[0, 0]); // no thumbnail
    push_segment(&mut out, 0xE0, &app0);
    // DQT: table 0 = luma, table 1 = chroma (8-bit precision).
    for (id, table) in [(0u8, &q_luma), (1u8, &q_chroma)] {
        let mut dqt = vec![id];
        dqt.extend_from_slice(table.as_slice());
        push_segment(&mut out, 0xDB, &dqt);
    }
    // SOF0: baseline, 8-bit, 3 components, 4:4:4 (all sampling factors 1×1).
    let mut sof = vec![8];
    sof.extend_from_slice(&(height as u16).to_be_bytes());
    sof.extend_from_slice(&(width as u16).to_be_bytes());
    sof.push(3);
    sof.extend_from_slice(&[1, 0x11, 0]); // Y: id 1, 1×1, quant table 0
    sof.extend_from_slice(&[2, 0x11, 1]); // Cb: id 2, 1×1, quant table 1
    sof.extend_from_slice(&[3, 0x11, 1]); // Cr: id 3, 1×1, quant table 1
    push_segment(&mut out, 0xC0, &sof);
    // DHT: class<<4 | id; class 0 = DC, 1 = AC.
    for (cls_id, bits, vals) in [
        (0x00u8, &DC_LUMA_BITS, DC_LUMA_VALS.as_slice()),
        (0x10, &AC_LUMA_BITS, AC_LUMA_VALS.as_slice()),
        (0x01, &DC_CHROMA_BITS, DC_CHROMA_VALS.as_slice()),
        (0x11, &AC_CHROMA_BITS, AC_CHROMA_VALS.as_slice()),
    ] {
        let mut dht = vec![cls_id];
        dht.extend_from_slice(bits.as_slice());
        dht.extend_from_slice(vals);
        push_segment(&mut out, 0xC4, &dht);
    }
    // SOS: 3 components, DC/AC table ids, spectral selection 0..63.
    let sos: [u8; 10] = [3, 1, 0x00, 2, 0x11, 3, 0x11, 0, 63, 0];
    push_segment(&mut out, 0xDA, &sos);

    // ── Entropy-coded scan ──────────────────────────────────────────────
    // Sample (clamped = edge-replicated) pixel → YCbCr component planes are
    // materialized lazily per block to keep memory at one block per component.
    let sample = |x: usize, y: usize| {
        let xi = x.min(w - 1);
        let yi = y.min(h - 1);
        let p = (yi * w + xi) * 4;
        rgb_to_ycbcr(rgba[p], rgba[p + 1], rgba[p + 2])
    };

    let mut bw = BitWriter::new();
    let mut prev_dc = [0i32; 3]; // Y, Cb, Cr predictors
    let blocks_x = w.div_ceil(8);
    let blocks_y = h.div_ceil(8);
    for by in 0..blocks_y {
        for bx in 0..blocks_x {
            // 4:4:4 MCU = one 8×8 block per component at the same position.
            for comp in 0..3 {
                let (quant, dc_tab, ac_tab) = if comp == 0 {
                    (&q_luma, &dc_l, &ac_l)
                } else {
                    (&q_chroma, &dc_c, &ac_c)
                };
                let mut block = [0.0f32; 64];
                for (i, slot) in block.iter_mut().enumerate() {
                    let (yv, cb, cr) = sample(bx * 8 + i % 8, by * 8 + i / 8);
                    *slot = [yv, cb, cr][comp] - 128.0;
                }
                let coeffs = fdct_8x8(&block);
                // Quantize straight into zigzag order (DQT is zigzag too).
                let mut zz = [0i32; 64];
                for (i, z) in zz.iter_mut().enumerate() {
                    *z = (coeffs[ZIGZAG[i]] / f32::from(quant[i])).round() as i32;
                }

                // DC difference.
                let diff = zz[0] - prev_dc[comp];
                prev_dc[comp] = zz[0];
                let (size, bits) = category_and_bits(diff);
                let (code, len) = dc_tab[size as usize];
                bw.put(code, len);
                if size > 0 {
                    bw.put(bits, size);
                }

                // AC run-length coding with ZRL (16 zeros) and EOB.
                let mut run = 0u8;
                for &c in &zz[1..] {
                    if c == 0 {
                        run += 1;
                        continue;
                    }
                    while run >= 16 {
                        let (code, len) = ac_tab[0xF0]; // ZRL
                        bw.put(code, len);
                        run -= 16;
                    }
                    let (size, bits) = category_and_bits(c);
                    let (code, len) = ac_tab[((run << 4) | size) as usize];
                    bw.put(code, len);
                    bw.put(bits, size);
                    run = 0;
                }
                if run > 0 {
                    let (code, len) = ac_tab[0x00]; // EOB
                    bw.put(code, len);
                }
            }
        }
    }
    out.extend_from_slice(&bw.finish());
    out.extend_from_slice(&[0xFF, 0xD9]); // EOI
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zigzag_is_a_permutation_with_known_anchors() {
        let mut seen = [false; 64];
        for &i in &ZIGZAG {
            assert!(!seen[i], "duplicate zigzag index {i}");
            seen[i] = true;
        }
        // T.81 Figure A.6 anchors: DC first, then (0,1), (1,0); last is (7,7).
        assert_eq!(ZIGZAG[0], 0);
        assert_eq!(ZIGZAG[1], 1);
        assert_eq!(ZIGZAG[2], 8);
        assert_eq!(ZIGZAG[63], 63);
    }

    #[test]
    fn quality_50_keeps_annex_k_tables_unscaled() {
        let q = scaled_quant_zigzag(&QUANT_LUMA, 50);
        for (i, &v) in q.iter().enumerate() {
            assert_eq!(u16::from(v), QUANT_LUMA[ZIGZAG[i]]);
        }
    }

    #[test]
    fn quality_100_is_all_ones_and_low_quality_clamps() {
        assert!(
            scaled_quant_zigzag(&QUANT_LUMA, 100)
                .iter()
                .all(|&v| v == 1)
        );
        // Quality 1 → scale 5000: 16*50 would be 800, clamped to 255.
        assert!(
            scaled_quant_zigzag(&QUANT_LUMA, 1)
                .iter()
                .all(|&v| v == 255)
        );
    }

    #[test]
    fn huffman_codes_are_prefix_free_and_match_counts() {
        for (bits, vals) in [
            (&DC_LUMA_BITS, DC_LUMA_VALS.as_slice()),
            (&DC_CHROMA_BITS, DC_CHROMA_VALS.as_slice()),
            (&AC_LUMA_BITS, AC_LUMA_VALS.as_slice()),
            (&AC_CHROMA_BITS, AC_CHROMA_VALS.as_slice()),
        ] {
            let total: usize = bits.iter().map(|&b| b as usize).sum();
            assert_eq!(total, vals.len(), "BITS total must equal HUFFVAL length");
            let codes = build_huffman_codes(bits, vals);
            let assigned: Vec<(u16, u8)> = vals.iter().map(|&v| codes[v as usize]).collect();
            for (i, &(ca, la)) in assigned.iter().enumerate() {
                assert!((1..=16).contains(&la));
                for &(cb, lb) in &assigned[i + 1..] {
                    let l = la.min(lb);
                    assert_ne!(
                        ca >> (la - l),
                        cb >> (lb - l),
                        "prefix collision between codes"
                    );
                }
            }
        }
    }

    #[test]
    fn category_and_bits_matches_t81_examples() {
        assert_eq!(category_and_bits(0), (0, 0));
        assert_eq!(category_and_bits(1), (1, 1));
        assert_eq!(category_and_bits(-1), (1, 0));
        assert_eq!(category_and_bits(3), (2, 3));
        assert_eq!(category_and_bits(-3), (2, 0));
        assert_eq!(category_and_bits(-2), (2, 1));
        assert_eq!(category_and_bits(255), (8, 255));
        assert_eq!(category_and_bits(-255), (8, 0));
    }

    #[test]
    fn fdct_of_constant_block_is_dc_only() {
        let block = [37.0f32; 64];
        let out = fdct_8x8(&block);
        // DC = 8 × level (1/4 · (1/√2)² · 64 · level).
        assert!((out[0] - 8.0 * 37.0).abs() < 1e-3, "{}", out[0]);
        for &ac in &out[1..] {
            assert!(ac.abs() < 1e-3, "AC leak {ac}");
        }
    }

    #[test]
    fn rgb_to_ycbcr_known_points() {
        let (y, cb, cr) = rgb_to_ycbcr(0, 0, 0);
        assert!((y, cb, cr) == (0.0, 128.0, 128.0));
        let (y, cb, cr) = rgb_to_ycbcr(255, 255, 255);
        assert!((y - 255.0).abs() < 1e-3 && (cb - 128.0).abs() < 1e-3 && (cr - 128.0).abs() < 1e-3);
        let (y, _, cr) = rgb_to_ycbcr(255, 0, 0);
        assert!((y - 76.245).abs() < 1e-2);
        assert!((cr - 255.5).abs() < 1e-2); // 0.5·255 + 128
    }

    #[test]
    fn bitwriter_stuffs_ff_and_pads_with_ones() {
        let mut bw = BitWriter::new();
        bw.put(0xFF, 8);
        assert_eq!(bw.out, vec![0xFF, 0x00]);
        let mut bw = BitWriter::new();
        bw.put(0b101, 3);
        assert_eq!(bw.finish(), vec![0b1011_1111]);
    }

    /// Walk the marker segments of an encoded stream: returns
    /// `(marker, payload)` pairs up to SOS, whose payload runs to EOI.
    fn parse_segments(jpeg: &[u8]) -> Vec<(u8, Vec<u8>)> {
        assert_eq!(&jpeg[..2], &[0xFF, 0xD8], "missing SOI");
        assert_eq!(&jpeg[jpeg.len() - 2..], &[0xFF, 0xD9], "missing EOI");
        let mut segments = Vec::new();
        let mut i = 2;
        loop {
            assert_eq!(jpeg[i], 0xFF, "expected marker at {i}");
            let marker = jpeg[i + 1];
            let len = u16::from_be_bytes([jpeg[i + 2], jpeg[i + 3]]) as usize;
            let payload = jpeg[i + 4..i + 2 + len].to_vec();
            i += 2 + len;
            let is_sos = marker == 0xDA;
            segments.push((marker, payload));
            if is_sos {
                return segments;
            }
        }
    }

    #[test]
    fn encode_jpeg_structure_is_valid_baseline_jfif() {
        let (w, h) = (20u32, 12u32); // non-multiple-of-8 exercises edge padding
        let mut rgba = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                rgba.extend_from_slice(&[(x * 12) as u8, (y * 20) as u8, 90, 255]);
            }
        }
        let jpeg = encode_jpeg(&rgba, w, h, 150);
        let segs = parse_segments(&jpeg);

        let app0 = &segs[0];
        assert_eq!(app0.0, 0xE0);
        assert_eq!(&app0.1[..5], b"JFIF\0");
        assert_eq!(app0.1[7], 1, "density units must be dpi");
        assert_eq!(u16::from_be_bytes([app0.1[8], app0.1[9]]), 150);

        let dqts: Vec<_> = segs.iter().filter(|(m, _)| *m == 0xDB).collect();
        assert_eq!(dqts.len(), 2);
        assert_eq!(dqts[0].1.len(), 65); // id byte + 64 entries

        let sof = segs.iter().find(|(m, _)| *m == 0xC0).expect("SOF0");
        assert_eq!(sof.1[0], 8, "8-bit precision");
        assert_eq!(u16::from_be_bytes([sof.1[1], sof.1[2]]), h as u16);
        assert_eq!(u16::from_be_bytes([sof.1[3], sof.1[4]]), w as u16);
        assert_eq!(sof.1[5], 3, "3 components");
        assert_eq!(sof.1[7], 0x11, "4:4:4 (1×1 sampling)");

        assert_eq!(segs.iter().filter(|(m, _)| *m == 0xC4).count(), 4);
        assert_eq!(segs.last().expect("segments").0, 0xDA, "scan header last");
    }

    #[test]
    fn encode_jpeg_scan_has_no_unstuffed_ff_markers() {
        // Every 0xFF inside the entropy-coded scan must be followed by 0x00
        // (stuffing) — a bare 0xFF would be read as a marker and truncate the
        // image. Noise-ish content maximizes the chance of raw 0xFF bytes.
        let (w, h) = (32u32, 32u32);
        let mut rgba = Vec::with_capacity((w * h * 4) as usize);
        for i in 0..(w * h) {
            let v = (i.wrapping_mul(2654435761) >> 8) as u8;
            rgba.extend_from_slice(&[v, v.wrapping_add(85), v.wrapping_add(170), 255]);
        }
        let jpeg = encode_jpeg(&rgba, w, h, 96);
        // Locate the scan: just past the SOS segment.
        let sos_at = jpeg
            .windows(2)
            .position(|p| p == [0xFF, 0xDA])
            .expect("SOS");
        let sos_len = u16::from_be_bytes([jpeg[sos_at + 2], jpeg[sos_at + 3]]) as usize;
        let scan = &jpeg[sos_at + 2 + sos_len..jpeg.len() - 2];
        let mut i = 0;
        while i < scan.len() {
            if scan[i] == 0xFF {
                assert_eq!(scan[i + 1], 0x00, "unstuffed 0xFF at scan offset {i}");
                i += 2;
            } else {
                i += 1;
            }
        }
    }

    #[test]
    fn encode_jpeg_uniform_gray_has_minimal_scan() {
        // (128,128,128) → Y=Cb=Cr=128 → level shift 0 → all-zero blocks: every
        // block is DC-diff 0 + EOB. Sanity-checks the whole pipeline nulls out.
        let (w, h) = (16u32, 16u32);
        let rgba: Vec<u8> = std::iter::repeat_n([128, 128, 128, 255], (w * h) as usize)
            .flatten()
            .collect();
        let jpeg = encode_jpeg(&rgba, w, h, 96);
        let sos_at = jpeg
            .windows(2)
            .position(|p| p == [0xFF, 0xDA])
            .expect("SOS");
        let sos_len = u16::from_be_bytes([jpeg[sos_at + 2], jpeg[sos_at + 3]]) as usize;
        let scan_len = jpeg.len() - 2 - (sos_at + 2 + sos_len);
        // 4 MCUs × 3 components × (2-bit DC zero + ≤4-bit EOB) ≈ 9 bytes; allow
        // slack but assert it stays tiny (a leaking AC would blow this up).
        assert!(scan_len <= 16, "uniform-gray scan too large: {scan_len}");
    }
}
