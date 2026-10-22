//! Small encoding helpers: varints, fixed ints, CRC32C, hex/ASCII previews.
//! Encodings follow RocksDB 10.4.2 util/coding.h (little-endian fixed ints, LEB128 varints).

/// Decode a varint (max `max_bytes` bytes) at `pos`. Returns (value, encoded length).
pub fn varint(buf: &[u8], pos: usize, max_bytes: usize) -> Option<(u64, usize)> {
    let mut v: u64 = 0;
    let mut shift = 0u32;
    for i in 0..max_bytes {
        let b = *buf.get(pos + i)?;
        v |= u64::from(b & 0x7f) << shift;
        if b & 0x80 == 0 {
            return Some((v, i + 1));
        }
        shift += 7;
    }
    None
}

pub fn varint32(buf: &[u8], pos: usize) -> Option<(u32, usize)> {
    varint(buf, pos, 5).and_then(|(v, n)| u32::try_from(v).ok().map(|v| (v, n)))
}

pub fn varint64(buf: &[u8], pos: usize) -> Option<(u64, usize)> {
    varint(buf, pos, 10)
}

/// RocksDB GetVarsignedint64 (util/coding.h): zigzag-encoded varint64.
pub fn varsigned64(buf: &[u8], pos: usize) -> Option<(i64, usize)> {
    varint64(buf, pos).map(|(u, n)| (((u >> 1) as i64) ^ -((u & 1) as i64), n))
}

pub fn fixed16(buf: &[u8], pos: usize) -> Option<u16> {
    buf.get(pos..pos + 2).map(|b| u16::from_le_bytes([b[0], b[1]]))
}

pub fn fixed32(buf: &[u8], pos: usize) -> Option<u32> {
    buf.get(pos..pos + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

pub fn fixed64(buf: &[u8], pos: usize) -> Option<u64> {
    buf.get(pos..pos + 8).map(|b| {
        let mut a = [0u8; 8];
        a.copy_from_slice(b);
        u64::from_le_bytes(a)
    })
}

// ---- CRC32C (Castagnoli), as used by RocksDB util/crc32c.h ----
const CRC32C_POLY: u32 = 0x82F6_3B78;

fn crc_table() -> &'static [[u32; 256]; 8] {
    use std::sync::OnceLock;
    static T: OnceLock<[[u32; 256]; 8]> = OnceLock::new();
    T.get_or_init(|| {
        let mut t = [[0u32; 256]; 8];
        for i in 0..256u32 {
            let mut c = i;
            for _ in 0..8 {
                c = if c & 1 != 0 { (c >> 1) ^ CRC32C_POLY } else { c >> 1 };
            }
            t[0][i as usize] = c;
        }
        for i in 0..256 {
            for k in 1..8 {
                let prev = t[k - 1][i];
                t[k][i] = (prev >> 8) ^ t[0][(prev & 0xff) as usize];
            }
        }
        t
    })
}

pub fn crc32c_extend(crc: u32, data: &[u8]) -> u32 {
    let t = crc_table();
    let mut c = !crc;
    let mut chunks = data.chunks_exact(8);
    for ch in &mut chunks {
        let lo = c ^ u32::from_le_bytes([ch[0], ch[1], ch[2], ch[3]]);
        let hi = u32::from_le_bytes([ch[4], ch[5], ch[6], ch[7]]);
        c = t[7][(lo & 0xff) as usize]
            ^ t[6][((lo >> 8) & 0xff) as usize]
            ^ t[5][((lo >> 16) & 0xff) as usize]
            ^ t[4][(lo >> 24) as usize]
            ^ t[3][(hi & 0xff) as usize]
            ^ t[2][((hi >> 8) & 0xff) as usize]
            ^ t[1][((hi >> 16) & 0xff) as usize]
            ^ t[0][(hi >> 24) as usize];
    }
    for &b in chunks.remainder() {
        c = (c >> 8) ^ t[0][((c ^ u32::from(b)) & 0xff) as usize];
    }
    !c
}

pub fn crc32c(data: &[u8]) -> u32 {
    crc32c_extend(0, data)
}

/// util/crc32c.h Mask(): rotate right 15 bits and add a constant.
pub fn crc_mask(crc: u32) -> u32 {
    crc.rotate_right(15).wrapping_add(0xa282_ead8)
}

pub fn crc_unmask(masked: u32) -> u32 {
    masked.wrapping_sub(0xa282_ead8).rotate_left(15)
}

// ---- previews ----
pub fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 3);
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 {
            s.push(' ');
        }
        s.push_str(&format!("{b:02x}"));
    }
    s
}

pub fn hex_preview(bytes: &[u8], max: usize) -> String {
    if bytes.len() <= max {
        hex(bytes)
    } else {
        format!("{} … (+{} B)", hex(&bytes[..max]), bytes.len() - max)
    }
}

/// Printable preview: UTF-8/ASCII text with escapes for non-printable bytes.
pub fn text_preview(bytes: &[u8], max: usize) -> String {
    let mut s = String::new();
    for &b in bytes.iter().take(max) {
        match b {
            0x20..=0x7e => s.push(b as char),
            b'\n' => s.push_str("\\n"),
            b'\t' => s.push_str("\\t"),
            _ => s.push_str(&format!("\\x{b:02x}")),
        }
    }
    if bytes.len() > max {
        s.push_str(&format!(" … (+{} B)", bytes.len() - max));
    }
    s
}

/// Preview a key/value: text if mostly printable, otherwise hex.
pub fn bytes_preview(bytes: &[u8], max: usize) -> String {
    let printable = bytes.iter().take(max).filter(|b| (0x20..=0x7e).contains(*b)).count();
    let n = bytes.len().min(max);
    if n == 0 {
        return "(empty)".into();
    }
    if printable * 10 >= n * 8 {
        format!("\"{}\"", text_preview(bytes, max))
    } else {
        format!("0x{}", hex_preview(bytes, max / 2).replace(' ', ""))
    }
}

pub fn human_size(n: u64) -> String {
    const U: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {} ({n} B)", U[i])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn crc32c_known_vectors() {
        // RFC 3720 test vector: 32 zero bytes.
        assert_eq!(crc32c(&[0u8; 32]), 0x8a91_36aa);
        assert_eq!(crc32c(b"123456789"), 0xe306_9283);
        let m = crc_mask(0x1234_5678);
        assert_eq!(crc_unmask(m), 0x1234_5678);
    }
    #[test]
    fn varints() {
        assert_eq!(varint64(&[0xac, 0x02], 0), Some((300, 2)));
        assert_eq!(varsigned64(&[0x03], 0), Some((-2, 1)));
        assert_eq!(varint32(&[0xff, 0xff, 0xff, 0xff, 0x7f], 0), None);
    }
}
