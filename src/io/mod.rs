//! Read-only bounded range reads. Parsers depend only on the `RangeRead` trait; the
//! platform-specific positional read lives in `file.rs`, an in-memory source for parser tests
//! in `mem.rs`. Never reads a whole file; every read is bounded by `limits::MAX_READ`.

mod file;
mod mem;

pub use crate::limits::MAX_READ;
pub use file::FileReader;
pub use mem::MemReader;

use std::io;
use std::time::SystemTime;

/// A fixed-size byte source with positional reads.
pub trait RangeRead: Send + Sync {
    /// Size observed when the source was opened. All decoding is bounded by this.
    fn len(&self) -> u64;
    /// One positional read into `buf` (may return fewer bytes; 0 at end of data).
    fn read_at(&self, off: u64, buf: &mut [u8]) -> io::Result<usize>;
    /// Modification time observed when the source was opened.
    fn modified(&self) -> io::Result<SystemTime>;
    /// True if the backing data changed since it was opened (files: size or mtime on disk).
    fn changed_since_open(&self) -> bool {
        false
    }
}

/// Shim for code and tests written against the former concrete reader.
pub type RangeReader = FileReader;

/// Reads `[offset, offset+len)` clipped to `r.len()`. Returns fewer bytes when the range runs
/// past the end (caller reports Truncated). Refuses ranges above `MAX_READ`.
pub fn read_vec(r: &dyn RangeRead, offset: u64, len: u64) -> io::Result<Vec<u8>> {
    if len > MAX_READ {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("resource limit: refusing to read {len} bytes in one range (cap {MAX_READ})"),
        ));
    }
    let size = r.len();
    if offset >= size {
        return Ok(Vec::new());
    }
    let n = len.min(size - offset) as usize;
    let mut buf = vec![0u8; n];
    let mut done = 0usize;
    while done < n {
        match r.read_at(offset + done as u64, &mut buf[done..]) {
            Ok(0) => break, // source shrank after open
            Ok(k) => done += k,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    buf.truncate(done);
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mem_reader_bounds() {
        let m = MemReader::new((0u8..100).collect::<Vec<u8>>());
        assert_eq!(read_vec(&m, 98, 10).unwrap(), vec![98, 99]);
        assert!(read_vec(&m, 100, 4).unwrap().is_empty());
        assert!(read_vec(&m, 0, MAX_READ + 1).is_err());
        assert_eq!(m.len(), 100);
    }
}
