//! Compression type ids and decompression shared by SST blocks and WAL streams, RocksDB 10.4.2.
//! Source: include/rocksdb/compression_type.h (codes), util/compression.cc
//! (BuiltinDecompressorV2: compress_format_version 2 prefixes a varint uncompressed
//! size for every codec except Snappy (own header) and XPRESS; Zlib is raw deflate
//! with windowBits -14), table/format.h GetCompressFormatForVersion (fv>=2 → 2).
//! WAL streams: db/log_writer.cc / db/log_reader.cc (one ZSTD stream per logical record).

pub use crate::limits::MAX_DECOMPRESSED;
use crate::limits::MAX_RECORD_BYTES;
use crate::util::varint64;
use std::io::Read;

/// ZSTD compression type id (the only codec RocksDB allows for WAL streams).
pub const ZSTD: u8 = 0x07;

pub fn compression_name(t: u8) -> String {
    match t {
        0x00 => "NoCompression".into(),
        0x01 => "Snappy".into(),
        0x02 => "Zlib".into(),
        0x03 => "BZip2".into(),
        0x04 => "LZ4".into(),
        0x05 => "LZ4HC".into(),
        0x06 => "XPRESS".into(),
        0x07 => "ZSTD".into(),
        t if t >= 0x80 => format!("Custom(0x{t:02x})"),
        t => format!("Unknown(0x{t:02x})"),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecompError {
    /// Codec not available in this reader (e.g. XPRESS is Windows-only, custom codecs).
    Unsupported(String),
    /// Declared/actual size exceeds MAX_DECOMPRESSED.
    Limit(String),
    Corrupt(String),
}

impl std::fmt::Display for DecompError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecompError::Unsupported(s) => write!(f, "unsupported: {s}"),
            DecompError::Limit(s) => write!(f, "resource limit: {s}"),
            DecompError::Corrupt(s) => write!(f, "corrupt: {s}"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Decompressed {
    pub data: Vec<u8>,
    /// Varint size prefix (value, encoded length) when compress_format_version 2 uses one.
    pub size_prefix: Option<(u64, usize)>,
    pub declared_size: Option<u64>,
}

/// Decompresses `payload` (block bytes without trailer) of compression type `t`.
/// `cfv` = compress_format_version (2 for format_version >= 2). `dict` = compression
/// dictionary for data blocks, when the table has one.
pub fn decompress(t: u8, payload: &[u8], cfv: u32, dict: Option<&[u8]>) -> Result<Decompressed, DecompError> {
    let cap = MAX_DECOMPRESSED;
    let prefixed = cfv >= 2 && matches!(t, 0x02 | 0x03 | 0x04 | 0x05 | 0x07);
    let (size_prefix, body) = if prefixed {
        let (v, n) = varint64(payload, 0).ok_or_else(|| DecompError::Corrupt("unable to read varint uncompressed size".into()))?;
        (Some((v, n)), &payload[n..])
    } else {
        (None, payload)
    };
    if let Some((v, _)) = size_prefix {
        if v > cap {
            return Err(DecompError::Limit(format!("declared uncompressed size {v} exceeds cap {cap}")));
        }
    }
    let declared = size_prefix.map(|x| x.0);
    let data = match t {
        0x01 => {
            let n = snap::raw::decompress_len(body).map_err(|e| DecompError::Corrupt(format!("snappy header: {e}")))? as u64;
            if n > cap {
                return Err(DecompError::Limit(format!("snappy uncompressed size {n} exceeds cap {cap}")));
            }
            snap::raw::Decoder::new().decompress_vec(body).map_err(|e| DecompError::Corrupt(format!("snappy: {e}")))?
        }
        0x02 => {
            if dict.is_some() {
                return Err(DecompError::Unsupported("Zlib with a preset dictionary is not implemented".into()));
            }
            let limit = declared.unwrap_or(cap);
            read_bounded(flate2::read::DeflateDecoder::new(body), limit, "zlib")?
        }
        0x03 => {
            if cfv < 2 {
                return Err(DecompError::Unsupported("BZip2 with compress_format_version 1".into()));
            }
            read_bounded(bzip2::read::BzDecoder::new(body), declared.unwrap_or(cap), "bzip2")?
        }
        0x04 | 0x05 => {
            let Some(n) = declared else {
                return Err(DecompError::Unsupported("LZ4 with compress_format_version 1".into()));
            };
            let r = match dict {
                Some(d) => lz4_flex::block::decompress_with_dict(body, n as usize, d),
                None => lz4_flex::block::decompress(body, n as usize),
            };
            r.map_err(|e| DecompError::Corrupt(format!("lz4: {e}")))?
        }
        0x06 => return Err(DecompError::Unsupported("XPRESS is a Windows-only codec; not available in this reader".into())),
        0x07 => {
            let n = declared.ok_or_else(|| DecompError::Unsupported("ZSTD with compress_format_version 1".into()))?;
            let mut out = Vec::with_capacity(n as usize);
            let res = match dict {
                Some(d) => zstd::bulk::Decompressor::with_dictionary(d).and_then(|mut z| z.decompress_to_buffer(body, &mut out)),
                None => zstd::bulk::Decompressor::new().and_then(|mut z| z.decompress_to_buffer(body, &mut out)),
            };
            res.map_err(|e| DecompError::Corrupt(format!("zstd: {e}")))?;
            out
        }
        t => return Err(DecompError::Unsupported(format!("compression type {} not built in", compression_name(t)))),
    };
    if let Some(n) = declared {
        if data.len() as u64 != n {
            return Err(DecompError::Corrupt(format!("size mismatch: declared {n}, got {}", data.len())));
        }
    }
    Ok(Decompressed { data, size_prefix, declared_size: declared })
}

fn read_bounded<R: Read>(r: R, limit: u64, what: &str) -> Result<Vec<u8>, DecompError> {
    let mut out = Vec::new();
    let mut lim = r.take(limit.min(MAX_DECOMPRESSED) + 1);
    lim.read_to_end(&mut out).map_err(|e| DecompError::Corrupt(format!("{what}: {e}")))?;
    if out.len() as u64 > limit {
        return Err(DecompError::Limit(format!("{what} output exceeds {limit} bytes")));
    }
    Ok(out)
}

/// Bounded ZSTD stream decode of one WAL logical record. Returns (first `keep` bytes,
/// total decompressed length counted up to MAX_RECORD_BYTES, complete). Bytes beyond `keep`
/// are counted into a sink, not stored.
pub fn zstd_stream(data: &[u8], keep: usize) -> Result<(Vec<u8>, u64, bool), String> {
    let mut dec = zstd::stream::read::Decoder::new(data).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    (&mut dec).take(keep as u64).read_to_end(&mut out).map_err(|e| e.to_string())?;
    let limit = (MAX_RECORD_BYTES as u64).saturating_sub(out.len() as u64) + 1;
    let rest = std::io::copy(&mut dec.take(limit), &mut std::io::sink()).map_err(|e| e.to_string())?;
    let total = out.len() as u64 + rest;
    Ok((out, total.min(MAX_RECORD_BYTES as u64), total <= MAX_RECORD_BYTES as u64))
}
