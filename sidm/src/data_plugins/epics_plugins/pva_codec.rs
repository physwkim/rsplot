//! NTNDArray codec decompression — the sidm counterpart of PyDM's
//! `pydm/data_plugins/epics_plugins/pva_codec.py`.
//!
//! PyDM's `decompress` delegates to the C libraries the producer
//! (areaDetector's `NDPluginCodec.cpp`) compressed with: `lz4.block`,
//! `bitshuffle` and `blosc`. This module ports the *decode* side of those
//! wire formats from the same C sources:
//!
//! - `"lz4"` — one raw LZ4 block over the whole array
//!   (`NDPluginCodec.cpp:483` uses `LZ4_compress_default` on the full
//!   buffer, no framing). Decoded with `lz4_flex::block`, the same
//!   spec-conformant block codec `rust-hdf5`'s `lz4` feature uses.
//! - `"bslz4"` — a headerless bitshuffle-LZ4 stream
//!   (`NDPluginCodec.cpp:556` calls `bshuf_compress_lz4` with
//!   `blockSize = 0`, so the block size is the library default). Frame walk
//!   and bit transpose ported from `bitshuffle.c` / `bitshuffle_core.c`
//!   (the scalar reference implementations).
//! - `"blosc"` — a self-describing c-blosc 1.x frame
//!   (`NDPluginCodec.cpp:388` calls `blosc_compress_ctx`). Frame walk,
//!   split-stream layout, per-block shuffles and the BloscLZ sub-codec
//!   ported from `blosc/blosc.c`, `blosc/shuffle*.{c,h}` and
//!   `blosc/blosclz.c` (c-blosc 1.21.7); LZ4/LZ4HC sub-streams decode with
//!   `lz4_flex`, ZLIB with `flate2`. The SNAPPY and ZSTD sub-codecs are
//!   not ported (no decoder in the dependency tree) and return an error,
//!   which the plugin surfaces as its one-time warning.
//! - `"jpeg"` — not supported (PyDM decodes via PIL; sidm has no JPEG
//!   decoder dependency); returns an error.
//!
//! rust-hdf5's *public* filter pipeline is deliberately not used here: its
//! LZ4 filter expects the registered HDF5 filter framing (12-byte header,
//! 64 MB cap) around what NTNDArray sends raw, and its blosc/bitshuffle
//! implementations are self-consistent but not wire-compatible with the C
//! libraries (no `bstarts` offset table, whole-buffer shuffle, reversed
//! bit order in the transpose) — only the underlying block codecs are
//! shared.
//!
//! All decoders are strict: a malformed stream returns `Err`, and the
//! caller keeps the value-skipping warn path (sidm's documented deviation
//! from PyDM, which logs and then emits the raw compressed bytes as the
//! value — garbage for every consumer).

/// Decompress an NTNDArray payload compressed with `codec_name`.
///
/// `uncompressed_size` is the NTNDArray `uncompressedSize` field (total
/// bytes of the decoded array); `elem_size` is the byte width of the
/// original element type from `codec.parameters`. Returns exactly
/// `uncompressed_size` bytes or an error naming the failure.
pub(crate) fn decompress(
    codec_name: &str,
    payload: &[u8],
    uncompressed_size: usize,
    elem_size: usize,
) -> Result<Vec<u8>, String> {
    // NDArray dimensions are int32-bound; anything past 4 GiB is a
    // corrupt/hostile header, not a real image — refuse before allocating.
    if uncompressed_size > 1 << 32 {
        return Err(format!(
            "uncompressedSize {uncompressed_size} exceeds the 4 GiB sanity limit"
        ));
    }
    let out = match codec_name {
        "lz4" => lz4_block_decompress(payload, uncompressed_size)?,
        "bslz4" => bslz4_decompress(payload, uncompressed_size, elem_size)?,
        "blosc" => blosc_decompress(payload)?,
        other => return Err(format!("codec {other:?} is not supported")),
    };
    if out.len() != uncompressed_size {
        return Err(format!(
            "codec {codec_name:?} produced {} bytes, expected {uncompressed_size}",
            out.len()
        ));
    }
    Ok(out)
}

/// One raw LZ4 block decoded to exactly `size` bytes — the contract of
/// PyDM's `lz4.block.decompress(data, uncompressed_size)`
/// (pva_codec.py:41-47). `decompress_into` a sized buffer so a stream
/// that under- or over-runs the advertised size is an error, never a
/// silent mismatch or an unbounded allocation.
fn lz4_block_decompress(payload: &[u8], size: usize) -> Result<Vec<u8>, String> {
    let mut out = vec![0u8; size];
    let n = lz4_flex::block::decompress_into(payload, &mut out).map_err(|e| format!("lz4: {e}"))?;
    if n != size {
        return Err(format!("lz4: decoded {n} bytes, expected {size}"));
    }
    Ok(out)
}

// =========================================================================
// bslz4 — bitshuffle-LZ4 stream (bitshuffle.c / bitshuffle_core.c)
// =========================================================================

/// `bshuf_default_block_size` (bitshuffle_core.c:2008-2018) — block size in
/// *elements* when the compressor was called with `block_size = 0`, as
/// NDPluginCodec does. The C comment: "This function needs to be absolutely
/// stable between versions. Otherwise encoded data will not be decodable."
fn bshuf_default_block_size(elem_size: usize) -> usize {
    let block_size = (8192 / elem_size) / 8 * 8;
    block_size.max(128)
}

/// Decode a `bshuf_compress_lz4` stream (`bshuf_blocked_wrap_fun`,
/// bitshuffle_core.c:1852-1905): `size/block` full blocks, then one
/// smaller block of `(size % block)` rounded down to a multiple of 8
/// elements, each block `[u32 BE compressed len][LZ4 block]` holding the
/// bit-transposed block; finally `(size % 8) * elem_size` leftover bytes
/// copied verbatim (untransposed, uncompressed, no length prefix).
fn bslz4_decompress(
    payload: &[u8],
    uncompressed_size: usize,
    elem_size: usize,
) -> Result<Vec<u8>, String> {
    if elem_size == 0 {
        return Err("bslz4: element size is zero".into());
    }
    if !uncompressed_size.is_multiple_of(elem_size) {
        return Err(format!(
            "bslz4: uncompressedSize {uncompressed_size} is not a multiple of \
             the element size {elem_size}"
        ));
    }
    let total_elems = uncompressed_size / elem_size;
    let block_elems = bshuf_default_block_size(elem_size);

    let mut out = Vec::with_capacity(uncompressed_size);
    let mut pos = 0usize;
    let decode_block = |pos: &mut usize, n_elems: usize| -> Result<Vec<u8>, String> {
        let hdr = payload
            .get(*pos..*pos + 4)
            .ok_or("bslz4: truncated block header")?;
        let csize = u32::from_be_bytes(hdr.try_into().expect("4-byte slice")) as usize;
        *pos += 4;
        let block = payload
            .get(*pos..*pos + csize)
            .ok_or("bslz4: truncated block body")?;
        *pos += csize;
        let mut shuffled = vec![0u8; n_elems * elem_size];
        let n = lz4_flex::block::decompress_into(block, &mut shuffled)
            .map_err(|e| format!("bslz4: {e}"))?;
        if n != shuffled.len() {
            return Err(format!(
                "bslz4: block decoded {n} bytes, expected {}",
                shuffled.len()
            ));
        }
        Ok(untrans_bit_elem(&shuffled, n_elems, elem_size))
    };

    for _ in 0..total_elems / block_elems {
        out.extend_from_slice(&decode_block(&mut pos, block_elems)?);
    }
    // The last sub-multiple-of-8 block (bitshuffle_core.c:1885-1891).
    let last_block = (total_elems % block_elems) / 8 * 8;
    if last_block > 0 {
        out.extend_from_slice(&decode_block(&mut pos, last_block)?);
    }
    // Raw leftover elements (bitshuffle_core.c:1896-1903).
    let leftover = (total_elems % 8) * elem_size;
    let tail = payload
        .get(pos..pos + leftover)
        .ok_or("bslz4: truncated leftover bytes")?;
    out.extend_from_slice(tail);
    if pos + leftover != payload.len() {
        return Err(format!(
            "bslz4: {} trailing bytes after the stream",
            payload.len() - pos - leftover
        ));
    }
    Ok(out)
}

/// The 8×8 bit-matrix transpose (`TRANS_BIT_8X8`, bitshuffle_core.c:110-118).
fn trans_bit_8x8(mut x: u64) -> u64 {
    let mut t = (x ^ (x >> 7)) & 0x00AA00AA00AA00AA;
    x ^= t ^ (t << 7);
    t = (x ^ (x >> 14)) & 0x0000CCCC0000CCCC;
    x ^= t ^ (t << 14);
    t = (x ^ (x >> 28)) & 0x00000000F0F0F0F0;
    x ^= t ^ (t << 28);
    x
}

/// `bshuf_untrans_bit_elem_scal` (bitshuffle_core.c:373-392): undo the bit
/// transpose of one block of `size` elements (`size % 8 == 0`) of
/// `elem_size` bytes. Stage 1 is `bshuf_trans_byte_bitrow_scal` (:307-329),
/// stage 2 `bshuf_shuffle_bit_eightelem_scal` (:331-370, the little-endian
/// branch — the byte order below is fixed by `from_le_bytes`, so the port
/// is host-endian independent).
fn untrans_bit_elem(input: &[u8], size: usize, elem_size: usize) -> Vec<u8> {
    debug_assert_eq!(size % 8, 0);
    debug_assert_eq!(input.len(), size * elem_size);
    let nbyte_row = size / 8;
    let mut tmp = vec![0u8; size * elem_size];
    for jj in 0..elem_size {
        for ii in 0..nbyte_row {
            for kk in 0..8 {
                tmp[ii * 8 * elem_size + jj * 8 + kk] = input[(jj * 8 + kk) * nbyte_row + ii];
            }
        }
    }
    let nbyte = size * elem_size;
    let mut out = vec![0u8; nbyte];
    for jj in (0..8 * elem_size).step_by(8) {
        let mut ii = 0;
        while ii + 8 * elem_size - 1 < nbyte {
            let word = tmp[ii + jj..ii + jj + 8].try_into().expect("8-byte slice");
            let mut x = trans_bit_8x8(u64::from_le_bytes(word));
            for kk in 0..8 {
                out[ii + jj / 8 + kk * elem_size] = x as u8;
                x >>= 8;
            }
            ii += 8 * elem_size;
        }
    }
    out
}

// =========================================================================
// blosc — c-blosc 1.x frame (blosc/blosc.c, 1.21.7)
// =========================================================================

/// c-blosc header flag bits (blosc.h:59-61) and the compressor code held
/// in bits 5-7 (`initialize_decompress_func`, blosc.c:527).
const BLOSC_DOSHUFFLE: u8 = 0x1;
const BLOSC_MEMCPYED: u8 = 0x2;
const BLOSC_DOBITSHUFFLE: u8 = 0x4;
const BLOSC_DONT_SPLIT: u8 = 0x10;
/// `MAX_SPLITS` / `MIN_BUFFERSIZE` (blosc.c:73-76).
const BLOSC_MAX_SPLITS: usize = 16;
const BLOSC_MIN_BUFFERSIZE: usize = 128;

/// Decode a complete blosc 1.x frame (`blosc_run_decompression_with_context`
/// / `_blosc_getitem` framing, blosc.c:1583-1700): a 16-byte header
/// (version, compressor version, flags, typesize, `nbytes`/`blocksize`/
/// `compressedsize` as LE i32), then — unless `MEMCPYED` — an absolute
/// `bstarts` offset table of `ceil(nbytes/blocksize)` LE i32 entries and
/// the per-block split streams.
fn blosc_decompress(payload: &[u8]) -> Result<Vec<u8>, String> {
    if payload.len() < 16 {
        return Err("blosc: header too short".into());
    }
    let version = payload[0];
    let compversion = payload[1];
    let flags = payload[2];
    let typesize = payload[3] as usize;
    let nbytes = le_i32(payload, 4)?;
    let blocksize = le_i32(payload, 8)?;
    let compressedsize = le_i32(payload, 12)?;

    // BLOSC_VERSION_FORMAT == 2 (blosc.h:29; blosc.c:1603-1604).
    if version != 2 {
        return Err(format!("blosc: unsupported format version {version}"));
    }
    // blosc.c:1606-1609.
    if blocksize == 0 || blocksize > nbytes || typesize == 0 || typesize > 255 {
        return Err(format!(
            "blosc: invalid header (nbytes {nbytes}, blocksize {blocksize}, \
             typesize {typesize})"
        ));
    }
    if compressedsize != payload.len() {
        return Err(format!(
            "blosc: header compressed size {compressedsize} != payload {}",
            payload.len()
        ));
    }

    if flags & BLOSC_MEMCPYED != 0 {
        // blosc.c:1622-1625 + the per-block fastcopy path (:1679-1683).
        if nbytes + 16 != compressedsize {
            return Err("blosc: memcpyed frame size mismatch".into());
        }
        return Ok(payload[16..16 + nbytes].to_vec());
    }

    let compcode = (flags & 0xe0) >> 5;
    // Every sub-codec's VERSION_FORMAT is 1 (blosc.h:104-109); the C
    // decoder rejects a mismatch per codec (blosc.c:530-570).
    if compversion != 1 {
        return Err(format!(
            "blosc: unsupported compressor format version {compversion}"
        ));
    }

    let nblocks = nbytes.div_ceil(blocksize);
    let leftover = nbytes % blocksize;
    // blosc.c:1630-1632.
    if nblocks >= (compressedsize - 16) / 4 {
        return Err("blosc: block count exceeds the frame".into());
    }

    let mut out = Vec::with_capacity(nbytes);
    for j in 0..nblocks {
        let bsize = if j == nblocks - 1 && leftover > 0 {
            leftover
        } else {
            blocksize
        };
        let src_offset = le_i32(payload, 16 + 4 * j)?;
        out.extend_from_slice(&blosc_d(
            payload,
            src_offset,
            bsize,
            j == nblocks - 1 && leftover > 0,
            flags,
            typesize,
            compcode,
        )?);
    }
    Ok(out)
}

/// One block (`blosc_d`, blosc.c:725-800): `nsplits` sub-streams of
/// `[i32 LE compressed len][data]` (a length equal to the split size means
/// the split is stored raw), concatenated and then byte-unshuffled or
/// bit-unshuffled over the whole block.
fn blosc_d(
    frame: &[u8],
    mut src_offset: usize,
    bsize: usize,
    leftoverblock: bool,
    flags: u8,
    typesize: usize,
    compcode: u8,
) -> Result<Vec<u8>, String> {
    // blosc.c:748-757.
    let dont_split = flags & BLOSC_DONT_SPLIT != 0;
    let nsplits = if !dont_split
        && typesize <= BLOSC_MAX_SPLITS
        && bsize / typesize >= BLOSC_MIN_BUFFERSIZE
        && !leftoverblock
    {
        typesize
    } else {
        1
    };
    let neblock = bsize / nsplits;
    let mut block = vec![0u8; bsize];
    let mut w = 0usize;
    for _ in 0..nsplits {
        let cbytes = le_i32(frame, src_offset)?;
        src_offset += 4;
        let src = frame
            .get(src_offset..src_offset + cbytes)
            .ok_or("blosc: truncated split stream")?;
        if cbytes == neblock {
            // Stored split (blosc.c:773-776).
            block[w..w + neblock].copy_from_slice(src);
        } else {
            let n = blosc_sub_decompress(compcode, src, &mut block[w..w + neblock])?;
            if n != neblock {
                return Err(format!(
                    "blosc: split decoded {n} bytes, expected {neblock}"
                ));
            }
        }
        src_offset += cbytes;
        w += neblock;
    }

    // blosc.c:739-741 + :789-796.
    let doshuffle = flags & BLOSC_DOSHUFFLE != 0 && typesize > 1;
    let dobitshuffle = flags & BLOSC_DOBITSHUFFLE != 0 && bsize >= typesize;
    if doshuffle {
        Ok(blosc_unshuffle(typesize, &block))
    } else if dobitshuffle {
        Ok(blosc_bitunshuffle(typesize, &block))
    } else {
        Ok(block)
    }
}

/// Byte-unshuffle one block (`unshuffle_generic_inline`,
/// shuffle-generic.h:61-81): element `i` byte `j` comes from stream `j`;
/// the `blocksize % typesize` tail bytes are copied unshuffled.
fn blosc_unshuffle(typesize: usize, src: &[u8]) -> Vec<u8> {
    let blocksize = src.len();
    let neblock_quot = blocksize / typesize;
    let neblock_rem = blocksize % typesize;
    let mut dest = vec![0u8; blocksize];
    for i in 0..neblock_quot {
        for j in 0..typesize {
            dest[i * typesize + j] = src[j * neblock_quot + i];
        }
    }
    dest[blocksize - neblock_rem..].copy_from_slice(&src[blocksize - neblock_rem..]);
    dest
}

/// Bit-unshuffle one block (`blosc_internal_bitunshuffle`,
/// shuffle.c:420-443): when the element count is a multiple of 8, undo the
/// bitshuffle transpose and copy the `blocksize % typesize` tail raw;
/// otherwise the compressor stored the whole block unshuffled.
fn blosc_bitunshuffle(typesize: usize, src: &[u8]) -> Vec<u8> {
    let blocksize = src.len();
    let size = blocksize / typesize;
    if size.is_multiple_of(8) {
        let mut dest = untrans_bit_elem(&src[..size * typesize], size, typesize);
        dest.extend_from_slice(&src[size * typesize..]);
        dest
    } else {
        src.to_vec()
    }
}

/// Decode one split stream with the frame's sub-codec
/// (`initialize_decompress_func`, blosc.c:525-570; codes blosc.h:80-84).
/// Returns the decoded byte count, which must equal the split size.
fn blosc_sub_decompress(compcode: u8, src: &[u8], out: &mut [u8]) -> Result<usize, String> {
    match compcode {
        // BLOSC_BLOSCLZ_LIB
        0 => blosclz_decompress(src, out),
        // BLOSC_LZ4_LIB (LZ4 and LZ4HC share the block format).
        1 => lz4_flex::block::decompress_into(src, out).map_err(|e| format!("blosc lz4: {e}")),
        // BLOSC_ZLIB_LIB — a zlib stream (`zlib_wrap_decompress` uses
        // uncompress(), i.e. zlib framing).
        3 => {
            use std::io::Read as _;
            let mut dec = flate2::read::ZlibDecoder::new(src);
            let mut n = 0usize;
            loop {
                let r = dec
                    .read(&mut out[n..])
                    .map_err(|e| format!("blosc zlib: {e}"))?;
                if r == 0 {
                    break;
                }
                n += r;
                if n == out.len() {
                    // Full — confirm the stream is exhausted.
                    let mut probe = [0u8; 1];
                    if dec
                        .read(&mut probe)
                        .map_err(|e| format!("blosc zlib: {e}"))?
                        != 0
                    {
                        return Err("blosc zlib: stream longer than the split".into());
                    }
                    break;
                }
            }
            Ok(n)
        }
        2 => Err("blosc: the snappy sub-codec is not supported".into()),
        4 => Err("blosc: the zstd sub-codec is not supported".into()),
        other => Err(format!("blosc: unknown compressor code {other}")),
    }
}

/// A non-negative LE i32 read as `usize`, erroring on truncation or a
/// negative value (the C reads with `sw32_` into `int32_t` and rejects
/// negatives where it validates at all).
fn le_i32(buf: &[u8], at: usize) -> Result<usize, String> {
    let b = buf.get(at..at + 4).ok_or("blosc: truncated i32 field")?;
    let v = i32::from_le_bytes(b.try_into().expect("4-byte slice"));
    usize::try_from(v).map_err(|_| format!("blosc: negative size field {v}"))
}

/// `blosclz_decompress` (blosclz.c:679-788, c-blosc 1.21.7). Token stream:
/// a control byte < 32 is a literal run of `ctrl + 1` bytes; >= 32 is a
/// back-reference of `(ctrl >> 5) + 2` bytes (7 extends by continuation
/// bytes while 255) at distance `((ctrl & 31) << 8) + next + 1`, with the
/// far-distance escape (`next == 255` and offset field saturated) reading
/// a 16-bit big-endian offset biased by `MAX_DISTANCE`. The first control
/// byte's high bits are a version marker and masked off. Faithfully ported
/// including the C's exit points (a stream ending on a match token drops
/// that match — the paired compressor never emits one).
fn blosclz_decompress(input: &[u8], out: &mut [u8]) -> Result<usize, String> {
    const MAX_DISTANCE: usize = 8191; // blosclz.c:43
    if input.is_empty() {
        return Ok(0);
    }
    let maxout = out.len();
    let mut ip = 0usize;
    let mut op = 0usize;
    let mut ctrl = (input[ip] & 31) as usize;
    ip += 1;

    loop {
        if ctrl >= 32 {
            let mut len = (ctrl >> 5) - 1;
            let ofs = (ctrl & 31) << 8;
            if len == 7 - 1 {
                loop {
                    if ip + 1 >= input.len() {
                        return Err("blosclz: truncated length extension".into());
                    }
                    let code = input[ip];
                    ip += 1;
                    len += code as usize;
                    if code != 255 {
                        break;
                    }
                }
            } else if ip + 1 >= input.len() {
                return Err("blosclz: truncated match".into());
            }
            let code = input[ip];
            ip += 1;
            len += 3;
            // `ref = op - ofs; ref -= code;` then the final `ref--` below:
            // distance from the *next* output byte back to the copy source.
            let mut dist = ofs + code as usize;
            if code == 255 && ofs == (31 << 8) {
                if ip + 1 >= input.len() {
                    return Err("blosclz: truncated far offset".into());
                }
                let far = ((input[ip] as usize) << 8) + input[ip + 1] as usize;
                ip += 2;
                dist = far + MAX_DISTANCE;
            }

            if op + len > maxout {
                return Err("blosclz: output overrun".into());
            }
            // `ref - 1 < output` (blosclz.c:732): source before buffer start.
            if dist + 1 > op {
                return Err("blosclz: match distance before buffer start".into());
            }
            if ip >= input.len() {
                break;
            }
            ctrl = input[ip] as usize;
            ip += 1;

            // ref-- : the source starts dist + 1 bytes back; byte-wise copy
            // handles every overlap (including the run/memset case).
            let src_start = op - dist - 1;
            for k in 0..len {
                out[op + k] = out[src_start + k];
            }
            op += len;
        } else {
            let len = ctrl + 1;
            if op + len > maxout {
                return Err("blosclz: literal output overrun".into());
            }
            if ip + len > input.len() {
                return Err("blosclz: truncated literal run".into());
            }
            out[op..op + len].copy_from_slice(&input[ip..ip + len]);
            op += len;
            ip += len;
            if ip >= input.len() {
                break;
            }
            ctrl = input[ip] as usize;
            ip += 1;
        }
    }
    Ok(op)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- test-side encoders, ported from the compress halves of the same
    // C sources so the round trips exercise the real wire formats ----

    /// `bshuf_trans_bit_elem_scal` (bitshuffle_core.c:280-305): the forward
    /// bit transpose — byte-in-element transpose (:197-201), bit-in-byte
    /// transpose (:244-249, LE branch), then `bshuf_trans_bitrow_eight`
    /// (:268-278).
    fn trans_bit_elem(input: &[u8], size: usize, elem_size: usize) -> Vec<u8> {
        assert_eq!(size % 8, 0);
        // Stage A: bshuf_trans_byte_elem.
        let mut a = vec![0u8; size * elem_size];
        for ii in 0..size {
            for jj in 0..elem_size {
                a[jj * size + ii] = input[ii * elem_size + jj];
            }
        }
        // Stage B: bshuf_trans_bit_byte (little-endian branch).
        let nbyte_bitrow = size * elem_size / 8;
        let mut b = vec![0u8; size * elem_size];
        for ii in 0..nbyte_bitrow {
            let word = a[ii * 8..ii * 8 + 8].try_into().expect("8-byte slice");
            let mut x = trans_bit_8x8(u64::from_le_bytes(word));
            for kk in 0..8 {
                b[kk * nbyte_bitrow + ii] = x as u8;
                x >>= 8;
            }
        }
        // Stage C: bshuf_trans_bitrow_eight = trans_elem(lda=8, ldb=elem).
        let nbr = size / 8;
        let mut c = vec![0u8; size * elem_size];
        for ii in 0..8 {
            for jj in 0..elem_size {
                c[(jj * 8 + ii) * nbr..(jj * 8 + ii + 1) * nbr].copy_from_slice(
                    &b[(ii * elem_size + jj) * nbr..(ii * elem_size + jj + 1) * nbr],
                );
            }
        }
        c
    }

    /// `bshuf_compress_lz4` with `block_size = 0` — the NDPluginCodec
    /// producer form (bitshuffle.c:35-79 per block, bitshuffle_core.c:
    /// 1852-1905 for the block/tail walk).
    fn bslz4_compress(data: &[u8], elem_size: usize) -> Vec<u8> {
        let total_elems = data.len() / elem_size;
        assert_eq!(data.len() % elem_size, 0);
        let block_elems = bshuf_default_block_size(elem_size);
        let mut out = Vec::new();
        let mut pos = 0usize;
        let emit = |pos: &mut usize, n: usize, out: &mut Vec<u8>| {
            let shuffled = trans_bit_elem(&data[*pos..*pos + n * elem_size], n, elem_size);
            let comp = lz4_flex::block::compress(&shuffled);
            out.extend_from_slice(&(comp.len() as u32).to_be_bytes());
            out.extend_from_slice(&comp);
            *pos += n * elem_size;
        };
        for _ in 0..total_elems / block_elems {
            emit(&mut pos, block_elems, &mut out);
        }
        let last = (total_elems % block_elems) / 8 * 8;
        if last > 0 {
            emit(&mut pos, last, &mut out);
        }
        out.extend_from_slice(&data[pos..]);
        out
    }

    /// Byte-shuffle forward (`shuffle_generic_inline`), tail copied raw.
    fn blosc_shuffle(typesize: usize, src: &[u8]) -> Vec<u8> {
        let blocksize = src.len();
        let quot = blocksize / typesize;
        let rem = blocksize % typesize;
        let mut dest = vec![0u8; blocksize];
        for i in 0..quot {
            for j in 0..typesize {
                dest[j * quot + i] = src[i * typesize + j];
            }
        }
        dest[blocksize - rem..].copy_from_slice(&src[blocksize - rem..]);
        dest
    }

    /// Bitshuffle forward for blosc blocks (`blosc_internal_bitshuffle`):
    /// whole-elements transpose when the count is a multiple of 8, raw
    /// otherwise; sub-element tail bytes raw.
    fn blosc_bitshuffle_fwd(typesize: usize, src: &[u8]) -> Vec<u8> {
        let size = src.len() / typesize;
        if size.is_multiple_of(8) {
            let mut d = trans_bit_elem(&src[..size * typesize], size, typesize);
            d.extend_from_slice(&src[size * typesize..]);
            d
        } else {
            src.to_vec()
        }
    }

    /// Build a c-blosc 1.x frame the way `blosc_compress` does: 16-byte
    /// header, absolute `bstarts`, per-block splits each `[i32 LE len]` +
    /// stream (stored when the LZ4 result is not smaller, like blosc_c).
    fn blosc_frame(
        data: &[u8],
        typesize: usize,
        blocksize: usize,
        flags: u8, // shuffle/bitshuffle/dont-split bits; compressor added here
        compcode: u8,
    ) -> Vec<u8> {
        let nbytes = data.len();
        let nblocks = nbytes.div_ceil(blocksize);
        let leftover = nbytes % blocksize;
        let flags = flags | (compcode << 5);
        let mut body: Vec<Vec<u8>> = Vec::new();
        for j in 0..nblocks {
            let bsize = if j == nblocks - 1 && leftover > 0 {
                leftover
            } else {
                blocksize
            };
            let leftoverblock = j == nblocks - 1 && leftover > 0;
            let raw = &data[j * blocksize..j * blocksize + bsize];
            let transformed = if flags & BLOSC_DOSHUFFLE != 0 && typesize > 1 {
                blosc_shuffle(typesize, raw)
            } else if flags & BLOSC_DOBITSHUFFLE != 0 && bsize >= typesize {
                blosc_bitshuffle_fwd(typesize, raw)
            } else {
                raw.to_vec()
            };
            let dont_split = flags & BLOSC_DONT_SPLIT != 0;
            let nsplits = if !dont_split
                && typesize <= BLOSC_MAX_SPLITS
                && bsize / typesize >= BLOSC_MIN_BUFFERSIZE
                && !leftoverblock
            {
                typesize
            } else {
                1
            };
            let neblock = bsize / nsplits;
            let mut blk = Vec::new();
            for s in 0..nsplits {
                let split = &transformed[s * neblock..(s + 1) * neblock];
                let comp = match compcode {
                    1 => lz4_flex::block::compress(split),
                    _ => split.to_vec(), // store — valid for every codec
                };
                if comp.len() < neblock {
                    blk.extend_from_slice(&(comp.len() as u32).to_le_bytes());
                    blk.extend_from_slice(&comp);
                } else {
                    blk.extend_from_slice(&(neblock as u32).to_le_bytes());
                    blk.extend_from_slice(split);
                }
            }
            body.push(blk);
        }
        let mut frame = vec![2u8, 1, flags, typesize as u8];
        frame.extend_from_slice(&(nbytes as i32).to_le_bytes());
        frame.extend_from_slice(&(blocksize as i32).to_le_bytes());
        let total: usize = 16 + 4 * nblocks + body.iter().map(Vec::len).sum::<usize>();
        frame.extend_from_slice(&(total as i32).to_le_bytes());
        let mut off = 16 + 4 * nblocks;
        for blk in &body {
            frame.extend_from_slice(&(off as i32).to_le_bytes());
            off += blk.len();
        }
        for blk in &body {
            frame.extend_from_slice(blk);
        }
        frame
    }

    /// Deterministic mildly-compressible test bytes.
    fn pattern(n: usize) -> Vec<u8> {
        (0..n).map(|i| ((i * 7 + i / 13) % 251) as u8).collect()
    }

    #[test]
    fn lz4_raw_block_round_trip() {
        let data = pattern(10_000);
        let comp = lz4_flex::block::compress(&data);
        assert_eq!(decompress("lz4", &comp, data.len(), 1).unwrap(), data);
        // Wrong advertised size is an error, not a truncated buffer.
        assert!(decompress("lz4", &comp, data.len() + 1, 1).is_err());
    }

    #[test]
    fn bitshuffle_transpose_known_answers() {
        // 8 × u8 all 0x01: bit 0 of every element gathers into byte 0.
        assert_eq!(
            trans_bit_elem(&[1u8; 8], 8, 1),
            vec![0xFF, 0, 0, 0, 0, 0, 0, 0]
        );
        // Only element 0 has bit 0 set → element 0 maps to the LSB of the
        // packed byte (the little-endian TRANS_BIT_8X8 orientation).
        assert_eq!(
            trans_bit_elem(&[1, 0, 0, 0, 0, 0, 0, 0], 8, 1),
            vec![0x01, 0, 0, 0, 0, 0, 0, 0]
        );
        // The decoder inverts both.
        assert_eq!(
            untrans_bit_elem(&[0xFF, 0, 0, 0, 0, 0, 0, 0], 8, 1),
            vec![1u8; 8]
        );
    }

    #[test]
    fn bitshuffle_transpose_round_trips_all_element_sizes() {
        for &elem in &[1usize, 2, 4, 8] {
            for &n in &[8usize, 64, 4096] {
                let data = pattern(n * elem);
                let t = trans_bit_elem(&data, n, elem);
                assert_eq!(untrans_bit_elem(&t, n, elem), data, "elem={elem} n={n}");
            }
        }
    }

    #[test]
    fn bslz4_stream_round_trips_including_tail_shapes() {
        // (element size, element count): exact block multiples, a tail
        // block (multiple of 8), and raw leftovers (n % 8 != 0).
        for &(elem, n) in &[
            (1usize, 8192usize), // exactly one default block
            (1, 20000), // 2 blocks + tail block + 0 leftover? 20000%8192=3616 → tail 3616, leftover 0
            (2, 4096),  // one block exactly (8192/2)
            (2, 5000),  // tail 904, leftover 0
            (4, 2059),  // tail 8, leftover 3 → raw bytes
            (8, 1023),  // leftover 7 elements
            (8, 100),   // smaller than a block, tail 96, leftover 4
        ] {
            let data = pattern(n * elem);
            let stream = bslz4_compress(&data, elem);
            let out = decompress("bslz4", &stream, data.len(), elem).unwrap();
            assert_eq!(out, data, "elem={elem} n={n}");
        }
    }

    #[test]
    fn bslz4_rejects_corrupt_streams() {
        let data = pattern(4096);
        let mut stream = bslz4_compress(&data, 1);
        // Truncated body.
        stream.truncate(stream.len() - 1);
        assert!(decompress("bslz4", &stream, data.len(), 1).is_err());
        // Element size that does not divide the total.
        assert!(decompress("bslz4", &bslz4_compress(&data, 1), data.len(), 3).is_err());
    }

    #[test]
    fn blosc_lz4_frames_round_trip_across_layouts() {
        let data = pattern(40_000);
        // (typesize, blocksize, flags): split + byte shuffle, dont-split,
        // bitshuffle, no shuffle, multi-block with leftover.
        for &(ts, bs, flags) in &[
            (4usize, 8192usize, BLOSC_DOSHUFFLE),
            (4, 8192, BLOSC_DOSHUFFLE | BLOSC_DONT_SPLIT),
            (2, 16384, BLOSC_DOBITSHUFFLE | BLOSC_DONT_SPLIT),
            (8, 9000, BLOSC_DOSHUFFLE), // leftover block (40000 % 9000 != 0)
            (1, 40_000, 0),
        ] {
            let frame = blosc_frame(&data, ts, bs, flags, 1);
            let out = decompress("blosc", &frame, data.len(), ts).unwrap();
            assert_eq!(out, data, "ts={ts} bs={bs} flags={flags:#x}");
        }
    }

    #[test]
    fn blosc_memcpyed_frame_copies_through() {
        let data = pattern(500);
        let mut frame = vec![2u8, 1, BLOSC_MEMCPYED, 1];
        frame.extend_from_slice(&(data.len() as i32).to_le_bytes());
        frame.extend_from_slice(&(data.len() as i32).to_le_bytes());
        frame.extend_from_slice(&((data.len() + 16) as i32).to_le_bytes());
        frame.extend_from_slice(&data);
        assert_eq!(decompress("blosc", &frame, data.len(), 1).unwrap(), data);
    }

    #[test]
    fn blosc_stored_splits_and_unsupported_subcodecs() {
        let data = pattern(2048);
        // compcode 0 with store-only splits exercises the cbytes == neblock
        // path without needing a blosclz encoder.
        let frame = blosc_frame(&data, 2, 2048, BLOSC_DOSHUFFLE, 0);
        assert_eq!(decompress("blosc", &frame, data.len(), 2).unwrap(), data);
        // A stored split decodes regardless of the codec code (cbytes ==
        // neblock short-circuits before the sub-codec dispatch, like the C).
        let frame = blosc_frame(&data, 1, 2048, BLOSC_DONT_SPLIT, 4);
        assert_eq!(decompress("blosc", &frame, data.len(), 1).unwrap(), data);
        // A genuinely compressed split under an unported sub-codec must
        // fail naming it: one 4-byte block, zstd (code 4), cbytes 1 != 4.
        let mut frame = vec![2u8, 1, BLOSC_DONT_SPLIT | (4 << 5), 1];
        frame.extend_from_slice(&4i32.to_le_bytes()); // nbytes
        frame.extend_from_slice(&4i32.to_le_bytes()); // blocksize
        frame.extend_from_slice(&25i32.to_le_bytes()); // compressedsize
        frame.extend_from_slice(&20i32.to_le_bytes()); // bstarts[0]
        frame.extend_from_slice(&1i32.to_le_bytes()); // split cbytes
        frame.push(0); // split body
        assert!(
            decompress("blosc", &frame, 4, 1)
                .unwrap_err()
                .contains("zstd")
        );
    }

    #[test]
    fn blosclz_token_stream_known_answers() {
        // Literal run: ctrl = len-1 (< 32), first byte's high bits masked.
        let mut out = vec![0u8; 5];
        let n = blosclz_decompress(&[4, b'a', b'b', b'c', b'd', b'e'], &mut out).unwrap();
        assert_eq!((n, out.as_slice()), (5, b"abcde".as_slice()));

        // Literal "ab", then a match (ctrl 0b010_00000: length token 2 →
        // len 1-1+3+... = 4 with dist byte 0 → distance 1, an RLE of 'b'),
        // then a literal 'X'. The match's copy runs after the next control
        // byte is loaded (C order).
        let mut out = vec![0u8; 8];
        let n = blosclz_decompress(&[1, b'a', b'b', 0b0100_0000, 0, 0, b'X'], &mut out).unwrap();
        assert_eq!((n, &out[..7]), (7, b"abbbbbX".as_slice()));

        // C parity: a stream whose *last* token is a match drops the copy
        // (blosclz.c:736 breaks before the copy when no next control byte
        // exists — reachable only via the extended-length path, where the
        // distance byte can be the final byte). Literal 'a', then an
        // extended match (0xE0 → length token 7) whose ext byte 3 and dist
        // byte 0 end the stream: the match is discarded, output stays "a".
        let mut out = vec![0u8; 20];
        let n = blosclz_decompress(&[0, b'a', 0xE0, 3, 0], &mut out).unwrap();
        assert_eq!((n, &out[..1]), (1, b"a".as_slice()));
    }

    #[test]
    fn unsupported_codecs_are_named() {
        assert!(
            decompress("jpeg", &[0xFF, 0xD8], 100, 1)
                .unwrap_err()
                .contains("jpeg")
        );
        assert!(decompress("nope", &[], 0, 1).unwrap_err().contains("nope"));
    }
}
