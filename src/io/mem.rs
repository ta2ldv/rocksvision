//! In-memory `RangeRead` for parser tests and derived byte sources.

use super::RangeRead;
use std::io;
use std::time::SystemTime;

pub struct MemReader {
    data: Vec<u8>,
}

impl MemReader {
    pub fn new(data: impl Into<Vec<u8>>) -> Self {
        MemReader { data: data.into() }
    }
}

impl RangeRead for MemReader {
    fn len(&self) -> u64 {
        self.data.len() as u64
    }

    fn read_at(&self, off: u64, buf: &mut [u8]) -> io::Result<usize> {
        let Ok(s) = usize::try_from(off) else { return Ok(0) };
        let Some(src) = self.data.get(s..) else { return Ok(0) };
        let n = src.len().min(buf.len());
        buf[..n].copy_from_slice(&src[..n]);
        Ok(n)
    }

    fn modified(&self) -> io::Result<SystemTime> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "in-memory source has no modification time"))
    }
}
