//! Operational resource limits: the one authoritative place for every cap that bounds memory,
//! I/O or work per request. Format constants (block sizes, magic numbers, tags) stay in their
//! format modules. Nothing here depends on the UI; jobs, parsers and the UI all read from here.

// ---- I/O ----
/// Hard cap for a single range read (protects against malformed handles/lengths).
pub const MAX_READ: u64 = 96 * 1024 * 1024;
/// Non-recursive folder listing stops after this many entries.
pub const MAX_DIR_ENTRIES: usize = 50_000;
/// Bounded text preview of LOG/OPTIONS/IDENTITY files.
pub const TEXT_PREVIEW: u64 = 64 * 1024;
/// Bytes read from a CURRENT file, and characters shown in its contents preview.
pub const CURRENT_READ: u64 = 4096;
pub const CURRENT_PREVIEW: usize = 200;
/// Chunk size of the background byte search.
pub const SEARCH_CHUNK: u64 = 1 << 20;

// ---- decoding ----
/// Bounded decompression: blocks claiming more than this are a resource-limit result.
pub const MAX_DECOMPRESSED: u64 = 64 * 1024 * 1024;
/// Bounded log reassembly: logical records larger than this are reported as Resource limit.
pub const MAX_RECORD_BYTES: usize = 64 * 1024 * 1024;
/// Cap on stored fragment references per logical record (counting continues).
pub const MAX_FRAG_REFS: usize = 4096;
/// Bytes checksummed while resynchronizing inside one 32 KiB log block.
pub const RESYNC_BUDGET: usize = 2 * 1024 * 1024;
/// Entries listed per SST block.
pub const MAX_BLOCK_ENTRIES: usize = 200_000;
/// Decoded SST blocks kept in the per-file LRU (items, bytes).
pub const BLOCK_CACHE_ITEMS: usize = 32;
pub const BLOCK_CACHE_BYTES: usize = 64 << 20;
/// Remembered block handles (for re-reading evicted derived spaces).
pub const KNOWN_BLOCK_HANDLES: usize = 100_000;
/// Live files tracked while replaying a MANIFEST.
pub const MAX_LIVE_FILES: usize = 200_000;
/// Materialized fields per VersionEdit.
pub const MAX_EDIT_FIELDS: usize = 2000;
/// Problems kept while replaying a MANIFEST.
pub const MAX_MANIFEST_PROBLEMS: usize = 100;
/// WriteBatch operations listed per record.
pub const MAX_OPS_LISTED: usize = 20_000;
/// Columns decoded per wide-column entity.
pub const MAX_ENTITY_COLUMNS: u32 = 1024;
/// Restart offsets read per block while iterating entries.
pub const MAX_RESTARTS_READ: u32 = 1 << 20;
/// VersionEdit tag names kept for the one-line edit summary.
pub const MAX_EDIT_TAGS: usize = 64;
/// Bytes kept per MANIFEST record while replaying edits for the observed levels.
pub const MANIFEST_RECORD_KEEP: usize = 4 * 1024 * 1024;
/// Key-range overlap is computed only for levels with at most this many files.
pub const OVERLAP_MAX_FILES: usize = 5000;
/// Lines shown in a bounded text preview.
pub const TEXT_PREVIEW_LINES: usize = 2000;
/// Bytes kept per record when listing a page of log records: plain WAL, compressed WAL
/// (before decompression), MANIFEST.
pub const LIST_KEEP_WAL: usize = 512;
pub const LIST_KEEP_COMPRESSED: usize = 1 << 20;
pub const LIST_KEEP_MANIFEST: usize = 64 * 1024;

// ---- paging of tree/table listings ----
pub const RECORDS_PER_PAGE: usize = 200;
pub const LOG_BLOCKS_PER_GROUP: u64 = 256;
pub const SST_TREE_CHILDREN: usize = 1000;
pub const SST_DATA_PAGE: usize = 500;
pub const RESTARTS_LISTED: usize = 10_000;

// ---- application ----
pub struct Limits {
    pub hex_bytes_per_row: u64,
    pub hex_page_bytes: u64,
    /// Bytes shown in the interpretation grid.
    pub interp_bytes: u64,
    /// Upper bound for "Copy hex".
    pub copy_cap: u64,
    pub page_rows: usize,
    pub change_check_secs: u64,
    pub busy_repaint_ms: u64,
    /// Concurrent background I/O jobs (folder scans, hex pages, copies) before new ones are refused.
    pub max_io_inflight: usize,
    /// Navigation (Back) history entries.
    pub history: usize,
}

pub const LIMITS: &Limits = &Limits {
    hex_bytes_per_row: 16,
    hex_page_bytes: 64 * 1024,
    interp_bytes: 64,
    copy_cap: 64 * 1024,
    page_rows: 20,
    change_check_secs: 2,
    busy_repaint_ms: 100,
    max_io_inflight: 16,
    history: 200,
};
