//! File-backed `RangeRead`: opened strictly read-only, positional reads only (no seek state
//! shared between readers). Unix uses `pread` (FileExt::read_at), Windows `seek_read`.

use super::RangeRead;
use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Debug)]
pub struct FileReader {
    file: File,
    path: PathBuf,
    size: u64,
    mtime: Option<SystemTime>,
}

impl FileReader {
    /// Opens strictly read-only (no create/write/truncate flags).
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new().read(true).open(path)?;
        let meta = file.metadata()?;
        if !meta.is_file() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "not a regular file"));
        }
        Ok(Self { file, path: path.to_path_buf(), size: meta.len(), mtime: meta.modified().ok() })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Size observed when the file was opened.
    pub fn len(&self) -> u64 {
        self.size
    }

    /// Modification time observed when the file was opened.
    pub fn mtime(&self) -> Option<SystemTime> {
        self.mtime
    }

    pub fn is_empty(&self) -> bool {
        self.size == 0
    }

    /// True if size or mtime differs from what was observed at open time.
    pub fn changed_on_disk(&self) -> bool {
        match std::fs::metadata(&self.path) {
            Ok(m) => m.len() != self.size || m.modified().ok() != self.mtime,
            Err(_) => true,
        }
    }

    /// Bounded read of `[offset, offset+len)` (see `io::read_vec`).
    pub fn read_at(&self, offset: u64, len: u64) -> io::Result<Vec<u8>> {
        super::read_vec(self, offset, len)
    }
}

impl RangeRead for FileReader {
    fn len(&self) -> u64 {
        self.size
    }

    #[cfg(unix)]
    fn read_at(&self, off: u64, buf: &mut [u8]) -> io::Result<usize> {
        std::os::unix::fs::FileExt::read_at(&self.file, buf, off)
    }

    #[cfg(windows)]
    fn read_at(&self, off: u64, buf: &mut [u8]) -> io::Result<usize> {
        // seek_read moves the file cursor; this reader never uses the cursor otherwise.
        std::os::windows::fs::FileExt::seek_read(&self.file, buf, off)
    }

    fn modified(&self) -> io::Result<SystemTime> {
        self.mtime.ok_or_else(|| io::Error::new(io::ErrorKind::Unsupported, "modification time unavailable"))
    }

    fn changed_since_open(&self) -> bool {
        self.changed_on_disk()
    }
}
