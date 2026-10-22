//! RocksVision fixture generator (RocksDB 10.4.2 via rocksdb 0.24.0 / librocksdb-sys 0.17.3).
//!
//! Usage: fixture-gen examples <EXAMPLES_DIR> | examples-verify <EXAMPLES_DIR> <SCRATCH> | --verify <FIXTURES> <SCRATCH> | [OUT_DIR]   (default: ../fixtures relative to the crate dir)
//! Deletes and recreates OUT_DIR, writes all cases, MANIFEST.md and SHA256SUMS.
//! Keys/values are deterministic; file bytes are NOT guaranteed identical across runs
//! (DB id/session id, creation time, OPTIONS file timestamps). The committed files plus
//! SHA256SUMS are the reference for immutability tests.

use rocksdb::{
    BlockBasedIndexType, BlockBasedOptions, ChecksumType, ColumnFamilyDescriptor,
    DBCompressionType, DataBlockIndexType, MergeOperands, Options, SstFileWriter, WriteBatch, DB,
};
use sha2::{Digest, Sha256};
use std::ffi::CString;
use std::fmt::Write as _;
use std::fs;
use std::io::Read;
use std::os::raw::c_char;
use std::path::{Path, PathBuf};
use std::ptr;

mod examples;

type R<T> = Result<T, Box<dyn std::error::Error>>;

const BLOCK: usize = 32768;

// ---------------------------------------------------------------- helpers

fn concat_merge(_k: &[u8], existing: Option<&[u8]>, ops: &MergeOperands) -> Option<Vec<u8>> {
    let mut v = existing.map(|e| e.to_vec()).unwrap_or_default();
    for op in ops.iter() {
        if !v.is_empty() {
            v.push(b',');
        }
        v.extend_from_slice(op);
    }
    Some(v)
}

fn wal_opts() -> Options {
    let mut o = Options::default();
    o.create_if_missing(true);
    o.create_missing_column_families(true);
    // Large memtable so nothing flushes: records stay only in the WAL.
    o.set_write_buffer_size(256 << 20);
    o.set_max_write_buffer_number(4);
    o.set_disable_auto_compactions(true);
    o.set_merge_operator_associative("concat", concat_merge);
    o.set_keep_log_file_num(1);
    o
}

fn cf_descs(names: &[&str], base: &Options) -> Vec<ColumnFamilyDescriptor> {
    names
        .iter()
        .map(|n| ColumnFamilyDescriptor::new(*n, base.clone()))
        .collect()
}

fn clean_db_dir(dir: &Path) -> R<()> {
    if std::env::var_os("KEEP_LOG").is_some() {
        return Ok(());
    }
    for e in fs::read_dir(dir)? {
        let e = e?;
        let n = e.file_name().to_string_lossy().to_string();
        if n == "LOCK" || n == "LOG" || n.starts_with("LOG.old") {
            fs::remove_file(e.path())?;
        }
    }
    Ok(())
}

fn list_files(dir: &Path, ext: &str) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().map(|x| x == ext).unwrap_or(false))
        .collect();
    v.sort();
    v
}

fn esc(b: &[u8]) -> String {
    let mut s = String::new();
    for &c in b {
        if (0x20..0x7f).contains(&c) && c != b'|' && c != b'`' && c != b'\\' {
            s.push(c as char);
        } else {
            let _ = write!(s, "\\x{:02x}", c);
        }
    }
    s
}

fn varint(mut n: u64, out: &mut Vec<u8>) {
    while n >= 0x80 {
        out.push((n as u8 & 0x7f) | 0x80);
        n >>= 7;
    }
    out.push(n as u8);
}

/// Execute one write and log its observed sequence range (from DB::latest_sequence_number).
fn step(db: &DB, log: &mut String, desc: &str, f: impl FnOnce(&DB) -> Result<(), rocksdb::Error>) -> R<()> {
    let s0 = db.latest_sequence_number();
    f(db)?;
    let s1 = db.latest_sequence_number();
    if s1 == s0 {
        writeln!(log, "| - (no sequence consumed) | 0 | {desc} |")?;
    } else {
        writeln!(log, "| {}..{} | {} | {desc} |", s0 + 1, s1, s1 - s0)?;
    }
    Ok(())
}

/// SingleDelete batch built with the raw C API (the Rust crate does not expose SingleDelete).
fn single_delete_batch(keys: &[&[u8]]) -> WriteBatch {
    unsafe {
        let b = librocksdb_sys::rocksdb_writebatch_create();
        for k in keys {
            librocksdb_sys::rocksdb_writebatch_singledelete(b, k.as_ptr() as *const c_char, k.len());
        }
        let mut n: usize = 0;
        let p = librocksdb_sys::rocksdb_writebatch_data(b, &mut n);
        let wb = WriteBatch::from_data(std::slice::from_raw_parts(p as *const u8, n));
        librocksdb_sys::rocksdb_writebatch_destroy(b);
        wb
    }
}

enum HOp<'a> {
    DeleteCf(u32, &'a [u8]),         // tag 0x04
    PutCf(u32, &'a [u8], &'a [u8]),  // tag 0x05
    MergeCf(u32, &'a [u8], &'a [u8]), // tag 0x06
    SingleDeleteCf(u32, &'a [u8]),   // tag 0x08
}

/// Hand-encoded WriteBatch (tags per db/dbformat.h ValueType: kTypeColumnFamilyDeletion=0x4,
/// kTypeColumnFamilyValue=0x5, kTypeColumnFamilyMerge=0x6, kTypeColumnFamilySingleDeletion=0x8).
fn hand_batch(ops: &[HOp]) -> WriteBatch {
    let mut d = vec![0u8; 8];
    d.extend_from_slice(&(ops.len() as u32).to_le_bytes());
    let lp = |d: &mut Vec<u8>, s: &[u8]| {
        varint(s.len() as u64, d);
        d.extend_from_slice(s);
    };
    for op in ops {
        match op {
            HOp::DeleteCf(c, k) => {
                d.push(0x04);
                varint(*c as u64, &mut d);
                lp(&mut d, k);
            }
            HOp::PutCf(c, k, v) => {
                d.push(0x05);
                varint(*c as u64, &mut d);
                lp(&mut d, k);
                lp(&mut d, v);
            }
            HOp::MergeCf(c, k, v) => {
                d.push(0x06);
                varint(*c as u64, &mut d);
                lp(&mut d, k);
                lp(&mut d, v);
            }
            HOp::SingleDeleteCf(c, k) => {
                d.push(0x08);
                varint(*c as u64, &mut d);
                lp(&mut d, k);
            }
        }
    }
    WriteBatch::from_data(&d)
}

// ------------------------------------------------------- WAL physical scanner
// Independent mini-scanner used only to describe fixtures in MANIFEST.md.
// Physical format: db/log_format.h (7-byte header: crc32 u32, len u16, type u8; recyclable types
// 5..8 / 11 / 131 add a u32 log number => 11-byte header; blocks are 32768 bytes; a block tail < 7
// bytes is zero padding).

fn rtype_name(t: u8) -> &'static str {
    match t {
        0 => "ZERO",
        1 => "FULL",
        2 => "FIRST",
        3 => "MIDDLE",
        4 => "LAST",
        5 => "RECYCLABLE_FULL",
        6 => "RECYCLABLE_FIRST",
        7 => "RECYCLABLE_MIDDLE",
        8 => "RECYCLABLE_LAST",
        9 => "SET_COMPRESSION",
        10 => "USER_DEFINED_TIMESTAMP_SIZE",
        11 => "RECYCLABLE_USER_DEFINED_TIMESTAMP_SIZE",
        130 => "PREDECESSOR_WAL_INFO",
        131 => "RECYCLABLE_PREDECESSOR_WAL_INFO",
        _ => "UNKNOWN",
    }
}

struct PRec {
    off: usize,
    ty: u8,
    len: usize,
    hdr: usize,
    lognum: Option<u32>,
    stale: bool,
}

fn scan_wal(path: &Path) -> R<String> {
    let mut f = fs::File::open(path)?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf)?;
    let mut recs: Vec<PRec> = Vec::new();
    let mut pads: Vec<(usize, usize)> = Vec::new();
    let mut pos = 0usize;
    let mut first_ln: Option<u32> = None;
    let mut tail_note = String::new();
    while pos < buf.len() {
        let rem = BLOCK - pos % BLOCK;
        if rem < 7 {
            pads.push((pos, rem.min(buf.len() - pos)));
            pos += rem;
            continue;
        }
        if pos + 7 > buf.len() {
            tail_note = format!("truncated header at {pos}");
            break;
        }
        let len = u16::from_le_bytes([buf[pos + 4], buf[pos + 5]]) as usize;
        let ty = buf[pos + 6];
        if ty == 0 && len == 0 {
            tail_note = format!("zero-filled tail from offset {pos} ({} bytes)", buf.len() - pos);
            break;
        }
        let rec = matches!(ty, 5..=8 | 11 | 131);
        let hdr = if rec { 11 } else { 7 };
        if pos + hdr > buf.len() || pos + hdr + len > buf.len() {
            tail_note = format!("undecodable bytes from offset {pos} to EOF ({} bytes): header there reads type {ty}, len {len} (runs past EOF) - for a recycled WAL this is the middle of an older record", buf.len() - pos);
            // Probe forward for the first offset that parses as a recyclable record of an OLDER log.
            if let Some(fl) = first_ln {
                for q in pos + 1..buf.len().saturating_sub(11) {
                    let t = buf[q + 6];
                    let l = u16::from_le_bytes([buf[q + 4], buf[q + 5]]) as usize;
                    let ln = u32::from_le_bytes([buf[q + 7], buf[q + 8], buf[q + 9], buf[q + 10]]);
                    if (5..=8).contains(&t) && ln >= 1 && ln < fl && l > 0 && q + 11 + l <= buf.len() && (BLOCK - q % BLOCK) >= 11 {
                        tail_note.push_str(&format!("; first offset that parses as a record of an older log: {q} (type {}, log number {ln}, payload {l})", rtype_name(t)));
                        break;
                    }
                }
            }
            break;
        }
        let ln = if rec {
            Some(u32::from_le_bytes([buf[pos + 7], buf[pos + 8], buf[pos + 9], buf[pos + 10]]))
        } else {
            None
        };
        let mut stale = false;
        if let Some(l) = ln {
            match first_ln {
                None => first_ln = Some(l),
                Some(fl) if fl != l => stale = true,
                _ => {}
            }
        }
        recs.push(PRec { off: pos, ty, len, hdr, lognum: ln, stale });
        pos += hdr + len;
    }
    let mut s = String::new();
    writeln!(s, "- file size {} bytes = {:.2} blocks of {BLOCK}", buf.len(), buf.len() as f64 / BLOCK as f64)?;
    let mut counts: std::collections::BTreeMap<&str, usize> = Default::default();
    for r in &recs {
        *counts.entry(rtype_name(r.ty)).or_default() += 1;
    }
    writeln!(s, "- physical records: {} total; by type: {:?}", recs.len(), counts)?;
    let live: Vec<&PRec> = recs.iter().filter(|r| !r.stale).collect();
    let stale: Vec<&PRec> = recs.iter().filter(|r| r.stale).collect();
    if let Some(fl) = first_ln {
        writeln!(s, "- recyclable log number in first record header: {fl}; records with that number: {}; records with a DIFFERENT (stale) log number: {}", live.len(), stale.len())?;
        if let Some(r) = stale.first() {
            writeln!(s, "  - stale tail begins at offset {} (log number {:?}, {} stale physical records)", r.off, r.lognum, stale.len())?;
        }
    }
    if !pads.is_empty() {
        writeln!(s, "- block-tail zero padding (<7 bytes, offset:length): {:?}", pads)?;
    }
    if !tail_note.is_empty() {
        writeln!(s, "- tail: {tail_note}")?;
    }
    let all = recs.len() <= 40;
    if all {
        writeln!(s, "- records (offset, type, payload_len, header_len):")?;
    } else {
        writeln!(s, "- non-FULL records plus first/last 3 (offset, type, payload_len, header_len):")?;
    }
    for (i, r) in recs.iter().enumerate() {
        let full = matches!(r.ty, 1 | 5);
        if all || !full || i < 3 || i + 3 >= recs.len() {
            writeln!(s, "  - {} {} {} {}{}", r.off, rtype_name(r.ty), r.len, r.hdr, if r.stale { " STALE" } else { "" })?;
        }
    }
    Ok(s)
}

// ----------------------------------------------------------- SST helpers

/// Footer facts every block-based SST (format_version >= 1) ends with: [..][format_version u32 LE][magic u64 LE]
fn sst_footer(path: &Path) -> R<String> {
    let b = fs::read(path)?;
    let n = b.len();
    let magic = u64::from_le_bytes(b[n - 8..].try_into()?);
    let fv = u32::from_le_bytes(b[n - 12..n - 8].try_into()?);
    let cks = b[n - 53];
    Ok(format!(
        "size={n} footer_format_version={fv} footer_checksum_type_byte={cks} magic=0x{magic:016x}{}",
        if magic == 0x88e241b785f4cff7 { " (block-based magic OK)" } else { " (UNEXPECTED magic)" }
    ))
}

fn data_val(i: usize) -> String {
    format!("value-{i:06}:{}", "The quick brown fox jumps over the lazy dog. ".repeat(3))
}

fn key(i: usize) -> String {
    format!("key-{i:06}")
}

fn bbo_base() -> BlockBasedOptions {
    let mut b = BlockBasedOptions::default();
    b.set_bloom_filter(10.0, false);
    b
}

fn sst_opts(codec: DBCompressionType, bbo: &BlockBasedOptions) -> Options {
    let mut o = Options::default();
    o.set_compression_type(codec);
    o.set_block_based_table_factory(bbo);
    o
}

/// Write n sequential Put entries (key-000000..) with SstFileWriter. Returns footer description.
fn write_sst_puts(path: &Path, o: &Options, n: usize) -> R<String> {
    let mut w = SstFileWriter::create(o);
    w.open(path)?;
    for i in 0..n {
        w.put(key(i), data_val(i))?;
    }
    w.finish()?;
    sst_footer(path)
}

struct Man {
    s: String,
}
impl Man {
    fn line(&mut self, t: impl AsRef<str>) {
        self.s.push_str(t.as_ref());
        self.s.push('\n');
    }
}

// ---------------------------------------------------------------- cases

fn wal_basic(out: &Path, m: &mut Man) -> R<()> {
    let dir = out.join("wal-basic");
    let names = ["default", "cf_two", "cf_three"];
    let mut log = String::new();
    {
        let o = wal_opts();
        let db = DB::open_cf_descriptors(&o, &dir, cf_descs(&names, &o))?;
        let c2 = db.cf_handle("cf_two").unwrap();
        let c3 = db.cf_handle("cf_three").unwrap();
        writeln!(log, "| sequence | count | operation |\n|---|---|---|")?;
        step(&db, &mut log, "default CF: Put k1=v1", |d| d.put(b"k1", b"v1"))?;
        step(&db, &mut log, "default CF: Put k2=value-two", |d| d.put(b"k2", b"value-two"))?;
        step(&db, &mut log, "default CF: Put empty-value key `empty` = (zero-length value)", |d| d.put(b"empty", b""))?;
        step(&db, &mut log, "default CF: Put binary key `bin\\x00key` = `\\x00\\x01\\x02\\xff\\xfe`", |d| d.put(b"bin\x00key", [0u8, 1, 2, 0xff, 0xfe]))?;
        step(&db, &mut log, "default CF: Delete k1", |d| d.delete(b"k1"))?;
        step(&db, &mut log, "default CF: Put k3=v3", |d| d.put(b"k3", b"v3"))?;
        step(&db, &mut log, "default CF: SingleDelete k3 (raw C API batch, tag 0x07)", |d| d.write(single_delete_batch(&[b"k3"])))?;
        step(&db, &mut log, "default CF: Merge m1 <- `a`", |d| d.merge(b"m1", b"a"))?;
        step(&db, &mut log, "default CF: Merge m1 <- `b`", |d| d.merge(b"m1", b"b"))?;
        step(&db, &mut log, "default CF: Put r_b=rb", |d| d.put(b"r_b", b"rb"))?;
        step(&db, &mut log, "default CF: DeleteRange [r_a, r_z)", |d| d.delete_range_cf(&d.cf_handle("default").unwrap(), b"r_a", b"r_z"))?;
        step(&db, &mut log, "cf_two (id 1): Put c2k1=c2v1", |d| d.put_cf(&c2, b"c2k1", b"c2v1"))?;
        step(&db, &mut log, "cf_two (id 1): Put c2k2=c2v2", |d| d.put_cf(&c2, b"c2k2", b"c2v2"))?;
        step(&db, &mut log, "cf_two (id 1): Delete c2k1", |d| d.delete_cf(&c2, b"c2k1"))?;
        step(&db, &mut log, "cf_three (id 2): Put c3k1=c3v1", |d| d.put_cf(&c3, b"c3k1", b"c3v1"))?;
        step(&db, &mut log, "cf_three (id 2): Merge c3m <- `x`", |d| d.merge_cf(&c3, b"c3m", b"x"))?;
        step(&db, &mut log, "cf_three (id 2): Delete c3k1", |d| d.delete_cf(&c3, b"c3k1"))?;
        step(
            &db,
            &mut log,
            "ONE WriteBatch (single WAL record, 6 counted ops + 1 LogData): [1] Put default b1=bv1; [2] Put_cf cf_two b2=bv2; [3] Delete default k2; [4] Merge default m1 <- `c`; LogData `LOGDATA-blob-1` (tag 0x03, consumes no sequence); [5] DeleteRange_cf cf_three [c3a, c3z) (covers key c3m, so the earlier Merge `x` is deleted; after the later hand-batch Merge `y`, a read gives c3m=`y`); [6] Put_cf cf_three b3=bv3",
            |d| {
                let mut b = WriteBatch::default();
                b.put(b"b1", b"bv1");
                b.put_cf(&c2, b"b2", b"bv2");
                b.delete(b"k2");
                b.merge(b"m1", b"c");
                b.put_log_data(b"LOGDATA-blob-1");
                b.delete_range_cf(&c3, b"c3a", b"c3z");
                b.put_cf(&c3, b"b3", b"bv3");
                d.write(b)
            },
        )?;
        step(
            &db,
            &mut log,
            "ONE hand-encoded WriteBatch (explicit CF-id tags): [1] tag 0x05 CF1 Put hc-put1=hc-val1; [2] tag 0x05 CF1 Put hc-sd=x; [3] tag 0x08 CF1 SingleDelete hc-sd; [4] tag 0x05 CF2 Put hc-put2=hc-val2; [5] tag 0x06 CF2 Merge c3m <- `y`; [6] tag 0x04 CF0 Delete hc-gone",
            |d| {
                d.write(hand_batch(&[
                    HOp::PutCf(1, b"hc-put1", b"hc-val1"),
                    HOp::PutCf(1, b"hc-sd", b"x"),
                    HOp::SingleDeleteCf(1, b"hc-sd"),
                    HOp::PutCf(2, b"hc-put2", b"hc-val2"),
                    HOp::MergeCf(2, b"c3m", b"y"),
                    HOp::DeleteCf(0, b"hc-gone"),
                ]))
            },
        )?;
        // Verify CF id mapping default=0, cf_two=1, cf_three=2 and that the memtable saw everything.
        assert_eq!(db.get_cf(&c2, b"hc-put1")?.as_deref(), Some(&b"hc-val1"[..]), "cf id 1 != cf_two");
        assert_eq!(db.get_cf(&c3, b"hc-put2")?.as_deref(), Some(&b"hc-val2"[..]), "cf id 2 != cf_three");
        assert_eq!(db.get_cf(&c2, b"hc-sd")?, None);
        assert_eq!(db.get(b"m1")?.as_deref(), Some(&b"a,b,c"[..]));
        assert_eq!(db.get(b"r_b")?, None);
        writeln!(log, "\nFinal latest_sequence_number: {}", db.latest_sequence_number())?;
    }
    clean_db_dir(&dir)?;
    let wals = list_files(&dir, "log");
    let ssts = list_files(&dir, "sst");
    assert!(ssts.is_empty(), "unexpected flush in wal-basic");
    m.line("## wal-basic/");
    m.line("Real DB dir (CURRENT, MANIFEST, OPTIONS, IDENTITY, WAL). Opened with `create_if_missing`, `create_missing_column_families`, `write_buffer_size=256MiB`, `disable_auto_compactions`, merge operator `concat` (joins operands with `,`), default `recycle_log_file_num=0` (=> legacy 7-byte headers), CFs `default`(id 0), `cf_two`(id 1), `cf_three`(id 2) (ids verified by reading back). DB closed without flush: NO .sst files, all data lives only in the WAL. Start sequence 1 (fresh DB).");
    m.line("");
    m.line("Operations in write order (sequence numbers observed with `DB::latest_sequence_number()`; a WriteBatch consumes one sequence per counted op; LogData consumes none):");
    m.line("");
    m.line(&log);
    m.line("");
    m.line("WAL file(s): ".to_string() + &wals.iter().map(|p| p.file_name().unwrap().to_string_lossy().to_string()).collect::<Vec<_>>().join(", "));
    for w in &wals {
        m.line(format!("\nPhysical scan of `{}` (generator's own scanner):", w.file_name().unwrap().to_string_lossy()));
        m.line(scan_wal(w)?);
    }
    m.line("");
    Ok(())
}

fn wal_fragmented(out: &Path, m: &mut Man) -> R<()> {
    let dir = out.join("wal-fragmented");
    let names = ["default", "cf_two"];
    let mut log = String::new();
    let big100: Vec<u8> = (0..100 * 1024).map(|i| b'a' + (i % 26) as u8).collect();
    let big40: Vec<u8> = (0..40 * 1024).map(|i| b'0' + (i % 10) as u8).collect();
    {
        let o = wal_opts();
        let db = DB::open_cf_descriptors(&o, &dir, cf_descs(&names, &o))?;
        let c2 = db.cf_handle("cf_two").unwrap();
        let sval = |i: usize| format!("small-value-{i:04}-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx");
        writeln!(log, "| sequence | count | operation |\n|---|---|---|")?;
        step(&db, &mut log, "300 x default Put `small-0000..small-0299` = `small-value-NNNN-` + 32 x `x` (48 bytes), one write each", |d| {
            for i in 0..300 {
                d.put(format!("small-{i:04}"), sval(i))?;
            }
            Ok(())
        })?;
        step(&db, &mut log, "default Put `big-100k` = 102400 bytes, byte[i] = 'a' + (i mod 26)", |d| d.put(b"big-100k", &big100))?;
        step(&db, &mut log, "150 x default Put `small-0300..small-0449` (same value shape), plus 10 x cf_two Put `c2-small-0..9` = 48-byte values", |d| {
            for i in 300..450 {
                d.put(format!("small-{i:04}"), sval(i))?;
            }
            for i in 0..10 {
                d.put_cf(&c2, format!("c2-small-{i}"), sval(i))?;
            }
            Ok(())
        })?;
        step(&db, &mut log, "default Put `big-40k` = 40960 bytes, byte[i] = '0' + (i mod 10)", |d| d.put(b"big-40k", &big40))?;
        // Fill until the current block has 100..170 bytes left, then write one record that ends
        // exactly 3 bytes before the block end so the writer must zero-pad the 3-byte tail.
        let wal = list_files(&dir, "log").pop().unwrap();
        let mut k = 0;
        loop {
            let p = fs::metadata(&wal)?.len() as usize;
            let rem = BLOCK - p % BLOCK;
            if (100..=170).contains(&rem) {
                break;
            }
            let kk = format!("fill-{k:03}");
            db.put(kk.as_bytes(), b"fillfillfillfillfillfillfillfill")?;
            k += 1;
        }
        writeln!(log, "| (filler) | {k} | {k} x default Put `fill-000..` = 31-byte value, used to position the WAL write offset near a block end |")?;
        let p = fs::metadata(&wal)?.len() as usize;
        let block_end = (p / BLOCK + 1) * BLOCK;
        let want_payload = block_end - 3 - p - 7;
        let klen = 8;
        let mut vlen = None;
        for v in 0..400usize {
            let mut vv = Vec::new();
            varint(v as u64, &mut vv);
            if 12 + 1 + 1 + klen + vv.len() + v == want_payload {
                vlen = Some(v);
                break;
            }
        }
        let vlen = vlen.expect("padding solve");
        step(&db, &mut log, &format!("default Put `padkey-1` (8-byte key) = {vlen} x `p`; batch payload sized so the physical record ends exactly 3 bytes before a block boundary (expect 3 zero bytes of block padding at the end of that block, WAL offset {})", block_end - 3), |d| d.put(b"padkey-1", vec![b'p'; vlen]))?;
        step(&db, &mut log, "400 x default Put `post-0000..post-0399` = 48-byte values (first starts at the next block)", |d| {
            for i in 0..400 {
                d.put(format!("post-{i:04}"), sval(i))?;
            }
            Ok(())
        })?;
        writeln!(log, "\nFinal latest_sequence_number: {}", db.latest_sequence_number())?;
    }
    clean_db_dir(&dir)?;
    assert!(list_files(&dir, "sst").is_empty());
    let wals = list_files(&dir, "log");
    m.line("## wal-fragmented/");
    m.line("Same options as wal-basic (no flush, legacy headers), CFs `default`(0), `cf_two`(1). Contains a 100 KiB value record (FIRST + MIDDLE... + LAST), a 40 KiB value record, many small records crossing several 32 KiB blocks, and one engineered 3-byte zero block-tail padding. Values: big-100k byte[i]='a'+(i mod 26); big-40k byte[i]='0'+(i mod 10). Merge operator `concat` set but unused.");
    m.line("");
    m.line(&log);
    for w in &wals {
        m.line(format!("\nPhysical scan of `{}`:", w.file_name().unwrap().to_string_lossy()));
        m.line(scan_wal(w)?);
    }
    m.line("");
    Ok(())
}

fn wal_recycled(out: &Path, m: &mut Man) -> R<()> {
    let dir = out.join("wal-recycled");
    let work = out.join(".work-recycled");
    fs::create_dir_all(&dir)?;
    let names = ["default", "cf_two"];
    let mut log = String::new();
    {
        let mut o = wal_opts();
        o.set_recycle_log_file_num(2);
        let db = DB::open_cf_descriptors(&o, &work, cf_descs(&names, &o))?;
        let c2 = db.cf_handle("cf_two").unwrap();
        // (tag, number of default-CF puts, flush afterwards?)
        let rounds: [(&str, usize, bool); 4] = [("r1", 8, true), ("r2", 120, true), ("r3", 5, true), ("r4", 8, false)];
        writeln!(log, "| round | puts to default (+1 `<tag>-c2` to cf_two) | flushed after? |\n|---|---|---|")?;
        for (tag, n, flush) in rounds {
            for i in 0..n {
                db.put(format!("{tag}-{i:04}"), format!("{tag}-value-{i:04}-{}", "y".repeat(180)))?;
            }
            db.put_cf(&c2, format!("{tag}-c2"), b"c2")?;
            writeln!(log, "| {tag} | {n} keys `{tag}-NNNN` = `{tag}-value-NNNN-` + 180 x `y` | {} |", if flush { "yes (default + cf_two)" } else { "NO (stays in live WAL)" })?;
            if flush {
                db.flush()?;
                db.flush_cf(&c2)?;
                std::thread::sleep(std::time::Duration::from_millis(700));
            }
        }
        writeln!(log, "\nlive_files at snapshot: {:?}", db.live_files()?.iter().map(|f| (f.column_family_name.clone(), f.name.clone(), f.level)).collect::<Vec<_>>())?;
        writeln!(log, "Final latest_sequence_number: {}", db.latest_sequence_number())?;
        // Snapshot the directory while the DB is still OPEN (crash-consistent copy). A clean close
        // truncates the live WAL to the bytes written (PosixWritableFile::Close), which would remove
        // the stale tail that makes recycled logs interesting.
        for e in fs::read_dir(&work)? {
            let e = e?;
            let n = e.file_name().to_string_lossy().to_string();
            if n == "LOCK" || n == "LOG" || n.starts_with("LOG.old") {
                continue;
            }
            fs::copy(e.path(), dir.join(&n))?;
        }
    }
    fs::remove_dir_all(&work)?;
    let wals = list_files(&dir, "log");
    m.line("## wal-recycled/");
    m.line("Options as wal-basic plus `recycle_log_file_num=2` (=> EVERY WAL record uses recyclable types 5..8 with 11-byte headers carrying the log number). Rounds below. r2 is large (~28 KiB), the others small. Each flush obsoletes the previous WAL and queues it for recycling; the next new WAL is created by renaming a queued file (engine LOG: `reusing log N from recycle list`). The WAL that holds r4 is the renamed r2 file, so it keeps r2's longer content behind r4's records. Because r4's records are not the same size as r2's, the first byte after r4's last record lands in the middle of an older record (undecodable); intact older records (header log number = the previous owner of the file) resume further on. See scan for exact offsets.");
    m.line("");
    m.line("**The directory is a snapshot copied while the DB was still open** (equivalent to a crash/power-loss image): a clean close truncates the live WAL to the bytes written and would remove the stale tail. CURRENT/MANIFEST/OPTIONS are consistent with it; r4 is unflushed so only the WAL holds r4 (WAL replay on open would be needed to see it; RocksVision never replays).");
    m.line("");
    m.line(&log);
    m.line("");
    m.line(format!("WAL files in dir: {}. The highest-numbered .log is the live WAL; the other is an obsolete file that was waiting in the recycle queue (closed, contains r3; it was itself created by renaming an older log, but its tail was truncated when its writer was closed, so no stale tail). A file is a *recycled reuse* iff its scan reports bytes after the last valid record of the live log number (undecodable/older-log data).", wals.iter().map(|p| p.file_name().unwrap().to_string_lossy().to_string()).collect::<Vec<_>>().join(", ")));
    for w in &wals {
        m.line(format!("\nPhysical scan of `{}`:", w.file_name().unwrap().to_string_lossy()));
        m.line(scan_wal(w)?);
    }
    m.line("");
    Ok(())
}

fn wal_extras(out: &Path, m: &mut Man) -> R<()> {
    // wal-zstd: WAL compression => SET_COMPRESSION record + compressed payload records.
    let dir = out.join("wal-zstd");
    {
        let mut o = wal_opts();
        o.set_wal_compression_type(DBCompressionType::Zstd);
        let db = DB::open_cf_descriptors(&o, &dir, cf_descs(&["default"], &o))?;
        for i in 0..40 {
            db.put(format!("zkey-{i:03}"), format!("zvalue-{i:03}-{}", "compressible ".repeat(20)))?;
        }
        db.put(b"zsmall", b"s")?;
    }
    clean_db_dir(&dir)?;
    m.line("## wal-zstd/ (EXTRA)");
    m.line("Options as wal-basic plus `set_wal_compression_type(Zstd)` (only zstd is valid for WAL). 40 Puts `zkey-000..zkey-039` = `zvalue-NNN-` + 20 x `compressible ` (default CF, seq 1..40) then Put `zsmall`=`s` (seq 41). Expect a SET_COMPRESSION (type 9) record first; payloads of later records are zstd-compressed streams (records smaller than the compression threshold may be stored uncompressed — see scan).");
    for w in list_files(&dir, "log") {
        m.line(format!("\nPhysical scan of `{}`:", w.file_name().unwrap().to_string_lossy()));
        m.line(scan_wal(&w)?);
    }
    m.line("");

    m.line("## wal-predecessor/ NOT GENERATED");
    m.line("The PREDECESSOR_WAL_INFO record (types 130/131) is only written when the DB option `track_and_verify_wals` is on. The Rust crate / C API only expose `track_and_verify_wals_in_manifest` (tried: it does NOT cause the record; scan of a live WAL after a flush showed only FULL records). No fixture produced.");
    m.line("");
    Ok(())
}

fn sst_codecs(out: &Path, m: &mut Man) -> R<()> {
    let codecs: [(&str, DBCompressionType); 7] = [
        ("none", DBCompressionType::None),
        ("snappy", DBCompressionType::Snappy),
        ("zlib", DBCompressionType::Zlib),
        ("bzip2", DBCompressionType::Bz2),
        ("lz4", DBCompressionType::Lz4),
        ("lz4hc", DBCompressionType::Lz4hc),
        ("zstd", DBCompressionType::Zstd),
    ];
    let n = 2000;
    m.line("## sst-<codec>/ (SstFileWriter, no DB)");
    m.line(format!("Each dir holds `data.sst`: {n} Put entries, key `key-NNNNNN` (NNNNNN = 000000..{:06}), value `value-NNNNNN:` + 3 x `The quick brown fox jumps over the lazy dog. ` (~146 bytes, compressible). Options: `set_compression_type(<codec>)`, BlockBasedOptions `set_bloom_filter(10, false)` (full filter), everything else default (block_size 4096, format_version default = 6 for RocksDB 10.4.2 per include/rocksdb/table.h, checksum crc32c). Written via SstFileWriter => all entries have sequence 0 / type Put (internal key trailer = 0x01 type, seq 0 until ingestion), global seqno 0. Expected: entries {n}, first key `{}`, last key `{}`. SstFileWriter output needs no DB; table properties carry creation time / session id so byte-exact regeneration is not expected.", n - 1, key(0), key(n - 1)));
    m.line("");
    m.line("| dir | compression option | result | footer facts |");
    m.line("|---|---|---|---|");
    for (name, c) in codecs {
        let dir = out.join(format!("sst-{name}"));
        fs::create_dir_all(&dir)?;
        let o = sst_opts(c, &bbo_base());
        match write_sst_puts(&dir.join("data.sst"), &o, n) {
            Ok(f) => m.line(format!("| sst-{name} | {c:?} | generated | {f} |")),
            Err(e) => {
                let _ = fs::remove_dir_all(&dir);
                m.line(format!("| sst-{name} | {c:?} | NOT GENERATED: {e} | |"))
            }
        }
    }
    m.line("");
    m.line("Raw (uncompressed) payload per entry is ~165 bytes of key+value; compare `size=` above with sst-none to see compression took effect (a block is stored uncompressed when savings < 12.5%).");
    m.line("");
    Ok(())
}

unsafe fn ffi_rangedel_sst(path: &Path) -> R<()> {
    use librocksdb_sys as f;
    let opts = f::rocksdb_options_create();
    f::rocksdb_options_set_compression(opts, 0);
    let env = f::rocksdb_envoptions_create();
    let w = f::rocksdb_sstfilewriter_create(env, opts);
    let mut err: *mut c_char = ptr::null_mut();
    let cpath = CString::new(path.to_str().unwrap())?;
    f::rocksdb_sstfilewriter_open(w, cpath.as_ptr(), &mut err);
    let check = |err: &mut *mut c_char| -> R<()> {
        if !err.is_null() {
            let s = std::ffi::CStr::from_ptr(*err).to_string_lossy().to_string();
            return Err(s.into());
        }
        Ok(())
    };
    check(&mut err)?;
    // Order of adds: SstFileWriter requires point keys ascending; range tombstones are separate.
    for (k, v) in [("a1", "va1"), ("a2", "va2"), ("m1", "vm1"), ("z9", "vz9")] {
        f::rocksdb_sstfilewriter_put(w, k.as_ptr() as *const c_char, k.len(), v.as_ptr() as *const c_char, v.len(), &mut err);
        check(&mut err)?;
    }
    let (b, e) = ("b", "n");
    f::rocksdb_sstfilewriter_delete_range(w, b.as_ptr() as *const c_char, b.len(), e.as_ptr() as *const c_char, e.len(), &mut err);
    check(&mut err)?;
    f::rocksdb_sstfilewriter_finish(w, &mut err);
    check(&mut err)?;
    f::rocksdb_sstfilewriter_destroy(w);
    f::rocksdb_envoptions_destroy(env);
    f::rocksdb_options_destroy(opts);
    Ok(())
}

fn sst_variants(out: &Path, m: &mut Man) -> R<()> {
    let dir = out.join("sst-variants");
    fs::create_dir_all(&dir)?;
    let n = 600;
    m.line("## sst-variants/");
    m.line(format!("All point-data files hold {n} Puts `key-000000..key-{:06}` with the same compressible values as sst-<codec> (`data_val`), compression NONE, bloom filter 10 bits/key (full), SstFileWriter. Expected entries {n}, first `{}`, last `{}`, unless stated.", n - 1, key(0), key(n - 1)));
    m.line("");
    m.line("| file | option(s) beyond the base | result | footer facts |");
    m.line("|---|---|---|---|");
    let add = |m: &mut Man, file: &str, what: &str, mk: &dyn Fn(&mut BlockBasedOptions)| {
        let mut b = bbo_base();
        mk(&mut b);
        let o = sst_opts(DBCompressionType::None, &b);
        let p = dir.join(file);
        match write_sst_puts(&p, &o, n) {
            Ok(f) => m.line(format!("| {file} | {what} | generated | {f} |")),
            Err(e) => {
                let _ = fs::remove_file(&p);
                m.line(format!("| {file} | {what} | NOT GENERATED: {e} | |"))
            }
        }
    };
    for fv in 2..=7 {
        add(m, &format!("format-v{fv}.sst"), &format!("set_format_version({fv})"), &|b| b.set_format_version(fv));
    }
    add(m, "partitioned.sst", "set_format_version(6), set_index_type(TwoLevelIndexSearch), set_partition_filters(true), set_metadata_block_size(512)", &|b| {
        b.set_index_type(BlockBasedIndexType::TwoLevelIndexSearch);
        b.set_partition_filters(true);
        b.set_metadata_block_size(512);
    });
    for nm in ["crc32c", "xxhash", "xxhash64", "xxh3", "nochecksum"] {
        let ct = || match nm {
            "crc32c" => ChecksumType::CRC32c,
            "xxhash" => ChecksumType::XXHash,
            "xxhash64" => ChecksumType::XXHash64,
            "xxh3" => ChecksumType::XXH3,
            _ => ChecksumType::NoChecksum,
        };
        add(m, &format!("checksum-{nm}.sst"), &format!("set_checksum_type({nm})"), &|b| b.set_checksum_type(ct()));
    }
    add(m, "datablock-hash-index.sst", "set_data_block_index_type(BinaryAndHash), set_data_block_hash_ratio(0.75)", &|b| {
        b.set_data_block_index_type(DataBlockIndexType::BinaryAndHash);
        b.set_data_block_hash_ratio(0.75);
    });
    // no-filter: the shared base adds a bloom filter, so build this one separately.
    {
        let p = dir.join("no-filter.sst");
        let mut b = BlockBasedOptions::default();
        b.set_format_version(6);
        let o = sst_opts(DBCompressionType::None, &b);
        write_sst_puts(&p, &o, n)?;
        m.line(format!("| no-filter.sst (rewritten without bloom) | no filter policy | generated | {} |", sst_footer(&p)?));
    }

    // Mixed entry kinds via SstFileWriter put/merge/delete (ascending keys).
    {
        let p = dir.join("mixed-kinds.sst");
        let o = sst_opts(DBCompressionType::None, &bbo_base());
        let mut w = SstFileWriter::create(&o);
        w.open(&p)?;
        w.put(b"k-a", b"val-a")?;
        w.merge(b"k-b", b"merge-b")?;
        w.delete(b"k-c")?;
        w.put(b"k-d", b"val-d")?;
        w.finish()?;
        m.line(format!("| mixed-kinds.sst | 4 entries via SstFileWriter: Put `k-a`=`val-a`; Merge `k-b`<-`merge-b`; Delete `k-c`; Put `k-d`=`val-d` (expected internal types 0x01, 0x02, 0x00, 0x01; first `k-a`, last `k-d`) | generated | {} |", sst_footer(&p)?));
    }
    // Range deletion (meta block "rocksdb.range_del") via raw C API.
    {
        let p = dir.join("rangedel.sst");
        match unsafe { ffi_rangedel_sst(&p) } {
            Ok(()) => m.line(format!("| rangedel.sst | raw C API SstFileWriter: Put a1=va1, a2=va2, m1=vm1, z9=vz9; DeleteRange [`b`,`n`) (compression none, no bloom) | generated | {} |", sst_footer(&p)?)),
            Err(e) => m.line(format!("| rangedel.sst | raw C API DeleteRange | NOT GENERATED: {e} | |")),
        }
    }
    m.line("");
    m.line("NOT GENERATED (not exposed by the Rust crate or out of scope): PlainTable/CuckooTable files, SST with user-defined timestamps, SST with blob files, compression dictionary (`zstd` dict) files, `HashSearch` index type (needs prefix extractor), SingleDelete inside an SST (SstFileWriter has no such call).");
    m.line("");
    Ok(())
}

fn db_levels(out: &Path, m: &mut Man) -> R<()> {
    let dir = out.join("db-levels");
    let mut o = Options::default();
    o.create_if_missing(true);
    o.create_missing_column_families(true);
    o.set_disable_auto_compactions(true);
    o.set_level_compaction_dynamic_level_bytes(false); // so a manual compaction lands in L1, not L6
    o.set_num_levels(7);
    o.set_write_buffer_size(256 << 20);
    o.set_keep_log_file_num(1);
    let mut o_users = o.clone();
    o_users.set_compression_type(DBCompressionType::Zstd);
    let mut o_events = o.clone();
    o_events.set_compression_type(DBCompressionType::Lz4);
    o_events.set_block_based_table_factory(&bbo_base());
    let descs = vec![
        ColumnFamilyDescriptor::new("default", o.clone()),
        ColumnFamilyDescriptor::new("cf_users", o_users),
        ColumnFamilyDescriptor::new("cf_events", o_events),
    ];
    let mut plan = String::new();
    let mut layout = String::new();
    {
        let db = DB::open_cf_descriptors(&o, &dir, descs)?;
        let cu = db.cf_handle("cf_users").unwrap();
        let ce = db.cf_handle("cf_events").unwrap();
        let cd = db.cf_handle("default").unwrap();
        let val = |tag: &str, i: usize| format!("{tag}-{i:05}-{}", "payload-".repeat(12));
        // default: two flushes with overlapping ranges, compact -> L1, then one more flush -> L0
        for i in 0..300 { db.put(format!("d-{i:05}"), val("d1", i))?; }
        db.flush_cf(&cd)?;
        for i in 150..450 { db.put(format!("d-{i:05}"), val("d2", i))?; }
        db.delete(b"d-00010")?;
        db.flush_cf(&cd)?;
        db.compact_range_cf(&cd, None::<&[u8]>, None::<&[u8]>);
        for i in 400..500 { db.put(format!("d-{i:05}"), val("d3", i))?; }
        db.flush_cf(&cd)?;
        writeln!(plan, "- default: 300 Puts d-00000..d-00299 (`d1-NNNNN-` + 12 x `payload-`), flush; 300 Puts d-00150..d-00449 (`d2-`) + Delete d-00010, flush; compact_range(None,None) => merged into one L1 file; 100 Puts d-00400..d-00499 (`d3-`), flush => new L0 file overlapping the L1 file.")?;
        // cf_users: three overlapping L0 flushes (compression zstd), no compaction
        for i in 0..200 { db.put_cf(&cu, format!("u-{i:05}"), val("u1", i))?; }
        db.flush_cf(&cu)?;
        for i in 100..300 { db.put_cf(&cu, format!("u-{i:05}"), val("u2", i))?; }
        db.flush_cf(&cu)?;
        for i in 500..600 { db.put_cf(&cu, format!("u-{i:05}"), val("u3", i))?; }
        db.flush_cf(&cu)?;
        writeln!(plan, "- cf_users (compression zstd): 3 flushes, never compacted => 3 L0 files: u-00000..u-00199 (`u1-`), u-00100..u-00299 (`u2-`, overlaps previous), u-00500..u-00599 (`u3-`, disjoint).")?;
        // cf_events: flush x2, compact -> L1; then tombstones (delete + delete_range) flush -> L0
        for i in 0..250 { db.put_cf(&ce, format!("e-{i:05}"), val("e1", i))?; }
        db.flush_cf(&ce)?;
        for i in 250..500 { db.put_cf(&ce, format!("e-{i:05}"), val("e2", i))?; }
        db.flush_cf(&ce)?;
        db.compact_range_cf(&ce, None::<&[u8]>, None::<&[u8]>);
        db.delete_cf(&ce, b"e-00005")?;
        db.delete_range_cf(&ce, b"e-00100", b"e-00120")?;
        db.put_cf(&ce, b"e-00600", val("e3", 600))?;
        db.flush_cf(&ce)?;
        writeln!(plan, "- cf_events (compression lz4, bloom 10): 250 Puts e-00000..e-00249, flush; 250 Puts e-00250..e-00499, flush; compact_range => one L1 file; then Delete e-00005, DeleteRange [e-00100,e-00120), Put e-00600, flush => one L0 file holding tombstones.")?;

        writeln!(layout, "From `DB::live_files()` (level, file, entries, deletions, smallest/largest user key) at close:\n")?;
        writeln!(layout, "| CF | level | file | size | entries | deletions | smallest | largest |\n|---|---|---|---|---|---|---|---|")?;
        let mut lf = db.live_files()?;
        lf.sort_by(|a, b| (a.column_family_name.clone(), a.level, a.name.clone()).cmp(&(b.column_family_name.clone(), b.level, b.name.clone())));
        for f in &lf {
            writeln!(layout, "| {} | L{} | {} | {} | {} | {} | `{}` | `{}` |", f.column_family_name, f.level, f.name.trim_start_matches('/'), f.size,
                f.num_entries, f.num_deletions, esc(f.start_key.as_deref().unwrap_or(b"")), esc(f.end_key.as_deref().unwrap_or(b"")))?;
        }
        for (cfname, h) in [("default", &cd), ("cf_users", &cu), ("cf_events", &ce)] {
            writeln!(layout, "\n`rocksdb.levelstats` for {cfname}:\n```")?;
            writeln!(layout, "{}", db.property_value_cf(h, "rocksdb.levelstats")?.unwrap_or_default())?;
            writeln!(layout, "```")?;
        }
        writeln!(layout, "Final latest_sequence_number: {}", db.latest_sequence_number())?;
    }
    clean_db_dir(&dir)?;
    let mut names: Vec<String> = fs::read_dir(&dir)?.map(|e| e.unwrap().file_name().to_string_lossy().to_string()).collect();
    names.sort();
    m.line("## db-levels/");
    m.line("Real DB dir with CURRENT, MANIFEST-*, OPTIONS-*, IDENTITY, SSTs and any WAL left by close. Options: 3 CFs (`default`, `cf_users`, `cf_events`), `disable_auto_compactions`, `level_compaction_dynamic_level_bytes=false` (so manual compaction output goes to L1, not L6), `num_levels=7`, write_buffer_size 256 MiB. Plan:");
    m.line(&plan);
    m.line("");
    m.line(&format!("Files in dir: {}", names.join(", ")));
    m.line("");
    m.line(&layout);
    for w in list_files(&dir, "log") {
        m.line(format!("\nPhysical scan of leftover WAL `{}`:", w.file_name().unwrap().to_string_lossy()));
        m.line(scan_wal(&w)?);
    }
    m.line("");
    Ok(())
}

fn corrupt(out: &Path, m: &mut Man) -> R<()> {
    let dir = out.join("corrupt");
    fs::create_dir_all(&dir)?;
    m.line("## corrupt/");
    m.line("Copies of pristine fixtures with exactly one controlled damage each. Originals untouched. 'flip' = XOR the byte with 0xFF.");
    m.line("");
    m.line("| file | source | damage | offset(s) |");
    m.line("|---|---|---|---|");
    // WAL
    let wal = list_files(&out.join("wal-basic"), "log").pop().unwrap();
    let wb = fs::read(&wal)?;
    // locate physical records (legacy 7-byte headers in wal-basic)
    let mut offs = vec![];
    let mut p = 0;
    while p + 7 <= wb.len() {
        let l = u16::from_le_bytes([wb[p + 4], wb[p + 5]]) as usize;
        offs.push((p, l));
        p += 7 + l;
    }
    let (r2off, r2len) = offs[2]; // third physical record (0-based index 2)
    let trunc_at = r2off + 7 + r2len / 2;
    fs::write(dir.join("wal-truncated-mid-record.log"), &wb[..trunc_at])?;
    m.line(format!("| wal-truncated-mid-record.log | wal-basic/{} | truncated to {trunc_at} bytes (original {}); physical record index 2 starts at {r2off}, header 7, payload {r2len} bytes, cut {} bytes into its payload | length {trunc_at} |", wal.file_name().unwrap().to_string_lossy(), wb.len(), r2len / 2));
    let (r1off, r1len) = offs[1];
    let flip = r1off + 7 + r1len / 2;
    let mut c = wb.clone();
    let before = c[flip];
    c[flip] ^= 0xFF;
    fs::write(dir.join("wal-bitflip-payload.log"), &c)?;
    m.line(format!("| wal-bitflip-payload.log | wal-basic/{} | one byte inside payload of physical record index 1 (record at {r1off}, payload {r1len} bytes) flipped; record CRC no longer matches | offset {flip}: 0x{before:02x} -> 0x{:02x} |", wal.file_name().unwrap().to_string_lossy(), c[flip]));
    // SST (uncompressed, so data-block flip is in plain bytes) + snappy
    let sst = out.join("sst-none/data.sst");
    let sb = fs::read(&sst)?;
    let trunc = sb.len() / 2;
    fs::write(dir.join("sst-truncated-mid-block.sst"), &sb[..trunc])?;
    m.line(format!("| sst-truncated-mid-block.sst | sst-none/data.sst | truncated to {trunc} of {} bytes (cuts through data blocks; footer/index/properties lost) | length {trunc} |", sb.len()));
    let trunc2 = sb.len() - 10;
    fs::write(dir.join("sst-truncated-footer.sst"), &sb[..trunc2])?;
    m.line(format!("| sst-truncated-footer.sst | sst-none/data.sst | last 10 bytes removed (footer partially present; magic missing) | length {trunc2} of {} |", sb.len()));
    let doff = 100; // inside the first data block (first block starts at offset 0; entries begin at 0)
    let mut c = sb.clone();
    let before = c[doff];
    c[doff] ^= 0xFF;
    fs::write(dir.join("sst-bitflip-data-block.sst"), &c)?;
    m.line(format!("| sst-bitflip-data-block.sst | sst-none/data.sst | byte inside the first data block flipped (block checksum no longer matches) | offset {doff}: 0x{before:02x} -> 0x{:02x} |", c[doff]));
    let moff = sb.len() - 8; // first (lowest) magic byte, footer magic is the last 8 bytes
    let mut c = sb.clone();
    let before = c[moff];
    c[moff] ^= 0xFF;
    fs::write(dir.join("sst-bad-footer-magic.sst"), &c)?;
    m.line(format!("| sst-bad-footer-magic.sst | sst-none/data.sst | first byte of the 8-byte footer magic flipped (file length {}) | offset {moff}: 0x{before:02x} -> 0x{:02x} |", sb.len(), c[moff]));
    let sn = fs::read(out.join("sst-snappy/data.sst"))?;
    let mut c = sn.clone();
    let before = c[100];
    c[100] ^= 0xFF;
    fs::write(dir.join("sst-snappy-bitflip-data-block.sst"), &c)?;
    m.line(format!("| sst-snappy-bitflip-data-block.sst | sst-snappy/data.sst | byte inside first (snappy-compressed) data block flipped | offset 100: 0x{before:02x} -> 0x{:02x} |", c[100]));
    m.line("");
    Ok(())
}


// ------------------------------------------------------------ --verify mode
// Opens COPIES of the fixtures with the real engine to prove they are valid. Never touches originals.
fn copy_dir(a: &Path, b: &Path) -> R<()> {
    fs::create_dir_all(b)?;
    for e in fs::read_dir(a)? {
        let e = e?;
        fs::copy(e.path(), b.join(e.file_name()))?;
    }
    Ok(())
}

fn count_cf(db: &DB, cf: &str) -> R<(usize, Option<Vec<u8>>, Option<Vec<u8>>)> {
    let h = db.cf_handle(cf).unwrap();
    let mut it = db.raw_iterator_cf(&h);
    it.seek_to_first();
    let (mut n, mut first, mut last) = (0, None, None);
    while it.valid() {
        if first.is_none() {
            first = it.key().map(|k| k.to_vec());
        }
        last = it.key().map(|k| k.to_vec());
        n += 1;
        it.next();
    }
    it.status()?;
    Ok((n, first, last))
}

fn verify(fx: &Path, scratch: &Path) -> R<()> {
    if scratch.exists() {
        fs::remove_dir_all(scratch)?;
    }
    fs::create_dir_all(scratch)?;
    let mut ok = true;
    let mut chk = |name: &str, cond: bool, detail: String| {
        println!("{} {name}: {detail}", if cond { "PASS" } else { "FAIL" });
        ok &= cond;
    };
    // SST files: ingest a copy into an empty DB and iterate.
    let mut ssts: Vec<(PathBuf, usize)> = vec![];
    for c in ["none", "snappy", "zlib", "bzip2", "lz4", "lz4hc", "zstd"] {
        ssts.push((fx.join(format!("sst-{c}/data.sst")), 2000));
    }
    for e in list_files(&fx.join("sst-variants"), "sst") {
        let nm = e.file_name().unwrap().to_string_lossy().to_string();
        let n = match nm.as_str() { "mixed-kinds.sst" | "rangedel.sst" => 0, _ => 600 };
        ssts.push((e, n));
    }
    for (i, (p, n)) in ssts.iter().enumerate() {
        let cp = scratch.join(format!("in-{i}.sst"));
        fs::copy(p, &cp)?;
        let d = scratch.join(format!("ing-{i}"));
        let mut o = Options::default();
        o.create_if_missing(true);
        o.set_merge_operator_associative("concat", concat_merge);
        let db = DB::open(&o, &d)?;
        let r = db.ingest_external_file(vec![&cp]);
        let label = p.strip_prefix(fx)?.display().to_string();
        match r {
            Err(e) => chk(&label, false, format!("ingest failed: {e}")),
            Ok(()) => {
                let mut it = db.raw_iterator();
                it.seek_to_first();
                let mut c = 0;
                let mut f = None;
                let mut l = None;
                while it.valid() {
                    if f.is_none() { f = it.key().map(|k| k.to_vec()); }
                    l = it.key().map(|k| k.to_vec());
                    c += 1;
                    it.next();
                }
                let st = it.status();
                if *n > 0 {
                    chk(&label, st.is_ok() && c == *n && f.as_deref() == Some(key(0).as_bytes()) && l.as_deref() == Some(key(n - 1).as_bytes()),
                        format!("ingested+iterated {c} entries first={:?} last={:?} status={:?}", f.map(|x| esc(&x)), l.map(|x| esc(&x)), st.is_ok()));
                } else {
                    chk(&label, st.is_ok(), format!("ingested; visible entries {c} (special-kind file) keys {:?}..{:?} status ok={}", f.map(|x| esc(&x)), l.map(|x| esc(&x)), st.is_ok()));
                }
            }
        }
        drop(db);
    }
    // Corrupt SSTs must be rejected/detected.
    for e in list_files(&fx.join("corrupt"), "sst") {
        let cp = scratch.join("bad.sst");
        fs::copy(&e, &cp)?;
        let d = scratch.join(format!("bad-{}", e.file_name().unwrap().to_string_lossy()));
        let mut o = Options::default();
        o.create_if_missing(true);
        let db = DB::open(&o, &d)?;
        let r = db.ingest_external_file(vec![&cp]);
        let mut detail = format!("ingest -> {:?}", r.as_ref().map(|_| "ok").map_err(|e| e.to_string()));
        let mut bad = r.is_err();
        if r.is_ok() {
            let mut it = db.raw_iterator();
            it.seek_to_first();
            while it.valid() { it.next(); }
            if it.status().is_err() { bad = true; detail.push_str(&format!("; iterate error: {}", it.status().unwrap_err())); }
        }
        chk(&format!("corrupt/{}", e.file_name().unwrap().to_string_lossy()), bad, format!("engine detects damage: {detail}"));
    }
    // WAL dirs: open copy (recovers from WAL) and check contents.
    let wo = |rec: usize| { let mut o = wal_opts(); if rec > 0 { o.set_recycle_log_file_num(rec); } o };
    {
        let d = scratch.join("wb");
        copy_dir(&fx.join("wal-basic"), &d)?;
        let o = wo(0);
        let db = DB::open_cf_descriptors(&o, &d, cf_descs(&["default", "cf_two", "cf_three"], &o))?;
        let c2 = db.cf_handle("cf_two").unwrap();
        let c3 = db.cf_handle("cf_three").unwrap();
        let g = |k: &[u8]| db.get(k).unwrap();
        let cond = g(b"m1").as_deref() == Some(&b"a,b,c"[..]) && g(b"k1").is_none() && g(b"k2").is_none() && g(b"k3").is_none()
            && g(b"bin\0key").as_deref() == Some(&[0u8, 1, 2, 0xff, 0xfe][..]) && g(b"r_b").is_none() && g(b"b1").as_deref() == Some(&b"bv1"[..])
            && db.get_cf(&c2, b"c2k2")?.as_deref() == Some(&b"c2v2"[..]) && db.get_cf(&c2, b"c2k1")?.is_none() && db.get_cf(&c2, b"b2")?.as_deref() == Some(&b"bv2"[..])
            && db.get_cf(&c3, b"b3")?.as_deref() == Some(&b"bv3"[..]) && db.get_cf(&c3, b"c3m")?.as_deref() == Some(&b"y"[..]);
        chk("wal-basic replay", cond, format!("latest_seq={} (expect 29)", db.latest_sequence_number()));
    }
    {
        let d = scratch.join("wf");
        copy_dir(&fx.join("wal-fragmented"), &d)?;
        let o = wo(0);
        let db = DB::open_cf_descriptors(&o, &d, cf_descs(&["default", "cf_two"], &o))?;
        let b100: Vec<u8> = (0..100 * 1024).map(|i| b'a' + (i % 26) as u8).collect();
        let b40: Vec<u8> = (0..40 * 1024).map(|i| b'0' + (i % 10) as u8).collect();
        let cond = db.get(b"big-100k")?.as_deref() == Some(&b100[..]) && db.get(b"big-40k")?.as_deref() == Some(&b40[..]) && db.get(b"post-0399")?.is_some() && db.get(b"padkey-1")?.is_some();
        chk("wal-fragmented replay", cond, format!("latest_seq={} (expect 1117)", db.latest_sequence_number()));
    }
    {
        let d = scratch.join("wr");
        copy_dir(&fx.join("wal-recycled"), &d)?;
        let o = wo(2);
        let db = DB::open_cf_descriptors(&o, &d, cf_descs(&["default", "cf_two"], &o))?;
        let cond = db.get(b"r4-0007")?.is_some() && db.get(b"r2-0119")?.is_some() && db.get(b"r4-c2").is_ok();
        let n = count_cf(&db, "default")?.0;
        chk("wal-recycled replay", cond && n == 8 + 120 + 5 + 8, format!("default keys {n} (expect 141), latest_seq={} (expect 145)", db.latest_sequence_number()));
    }
    {
        let d = scratch.join("wz");
        copy_dir(&fx.join("wal-zstd"), &d)?;
        let mut o = wal_opts();
        o.set_wal_compression_type(DBCompressionType::Zstd);
        let db = DB::open_cf_descriptors(&o, &d, cf_descs(&["default"], &o))?;
        chk("wal-zstd replay", db.get(b"zsmall")?.as_deref() == Some(&b"s"[..]) && count_cf(&db, "default")?.0 == 41, format!("latest_seq={} (expect 41)", db.latest_sequence_number()));
    }
    {
        let d = scratch.join("dl");
        copy_dir(&fx.join("db-levels"), &d)?;
        let mut o = Options::default();
        o.set_level_compaction_dynamic_level_bytes(false);
        o.set_merge_operator_associative("concat", concat_merge);
        let db = DB::open_cf(&o, &d, ["default", "cf_users", "cf_events"])?;
        let a = count_cf(&db, "default")?.0;
        let b = count_cf(&db, "cf_users")?.0;
        let c = count_cf(&db, "cf_events")?.0;
        chk("db-levels open", a == 499 && b == 400 && c == 480, format!("visible keys default={a} (expect 499) cf_users={b} (expect 400) cf_events={c} (expect 480 = 500 + e-00600 - e-00005 - 20 range-deleted)"));
    }
    println!("{}", if ok { "ALL PASS" } else { "SOME FAIL" });
    if !ok { std::process::exit(1); }
    Ok(())
}

fn walk(dir: &Path, base: &Path, acc: &mut Vec<PathBuf>) -> R<()> {
    let mut es: Vec<_> = fs::read_dir(dir)?.map(|e| e.unwrap().path()).collect();
    es.sort();
    for p in es {
        if p.is_dir() {
            walk(&p, base, acc)?;
        } else {
            acc.push(p.strip_prefix(base)?.to_path_buf());
        }
    }
    Ok(())
}

fn main() -> R<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(|s| s == "examples").unwrap_or(false) {
        return examples::run(Path::new(&args[2]));
    }
    if args.get(1).map(|s| s == "examples-verify").unwrap_or(false) {
        return examples::verify(Path::new(&args[2]), Path::new(&args[3]));
    }
    if args.get(1).map(|s| s == "--verify").unwrap_or(false) {
        return verify(Path::new(&args[2]), Path::new(&args[3]));
    }
    let out = std::env::args().nth(1).map(PathBuf::from).unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../fixtures"));
    if out.exists() {
        fs::remove_dir_all(&out)?;
    }
    fs::create_dir_all(&out)?;
    let out = out.canonicalize()?;
    let mut m = Man { s: String::new() };
    m.line("# RocksVision test fixtures");
    m.line("");
    m.line("Generated by `tests/fixture-gen` (standalone crate; `cargo run --release -- <out_dir>`), rocksdb 0.24.0 / librocksdb-sys 0.17.3+10.4.2 (RocksDB 10.4.2), features snappy, lz4, zstd, zlib, bzip2, multi-threaded-cf. Keys/values are deterministic; file bytes (DB id, session id, creation time, OPTIONS) are not, so treat the committed files + `SHA256SUMS` as the reference and do not regenerate in tests. Sequence numbers, file numbers and scan facts below were captured from this exact run. Validity was cross-checked with the real engine via `fixture-gen --verify <fixtures_dir> <scratch_dir>` (ingests every SST copy, replays every WAL dir copy, opens db-levels copy, confirms every corrupt/*.sst is rejected); the generator's WAL scan is its own mini-scanner, not RocksVision code.");
    m.line("");
    wal_basic(&out, &mut m)?;
    wal_fragmented(&out, &mut m)?;
    wal_recycled(&out, &mut m)?;
    wal_extras(&out, &mut m)?;
    sst_codecs(&out, &mut m)?;
    sst_variants(&out, &mut m)?;
    db_levels(&out, &mut m)?;
    corrupt(&out, &mut m)?;

    // File inventory + SHA256SUMS (MANIFEST.md and SHA256SUMS themselves excluded).
    let mut files = vec![];
    walk(&out, &out, &mut files)?;
    let mut sums = String::new();
    m.line("## File inventory");
    m.line("");
    m.line("| path | bytes |");
    m.line("|---|---|");
    let mut total = 0u64;
    for f in &files {
        let b = fs::read(out.join(f))?;
        total += b.len() as u64;
        let h = Sha256::digest(&b);
        let hex: String = h.iter().map(|x| format!("{x:02x}")).collect();
        writeln!(sums, "{hex}  {}", f.display())?;
        m.line(format!("| {} | {} |", f.display(), b.len()));
        assert!(b.len() < 2 << 20, "{} too large", f.display());
    }
    m.line(format!("\nTotal: {} files, {} bytes. SHA-256 of every file above: `SHA256SUMS` (`shasum -a 256 -c SHA256SUMS` from this directory).", files.len(), total));
    assert!(total < 10 << 20, "total too large");
    fs::write(out.join("MANIFEST.md"), &m.s)?;
    fs::write(out.join("SHA256SUMS"), &sums)?;
    println!("OK {} files, {} bytes", files.len(), total);
    Ok(())
}
