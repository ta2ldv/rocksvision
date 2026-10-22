//! Public examples dataset generator (`fixture-gen examples <examples_dir>`) and its engine
//! verification (`fixture-gen examples-verify <examples_dir> <scratch_dir>`).
//!
//! Writes <examples_dir>/data/<scenario>/ (complete RocksDB directories) and
//! <examples_dir>/files/<scenario>/ (ONE standalone file per directory) plus
//! <examples_dir>/SHA256SUMS. Only data/, files/ and SHA256SUMS are (re)created; README.md and
//! prior-work/ are never touched. Facts (sequence ranges, live_files, levelstats, WAL scans,
//! exact corruption mutations) are printed to stdout for the README.
//! File bytes are NOT byte-reproducible (DB id, session id, creation time, OPTIONS).

use super::*;

const BS: usize = 256; // data block size for every SST scenario (tiny so structures appear with little data)
const N: usize = 48; // logical records shared by bloom/index/codec scenarios

fn ekey(i: usize) -> String {
    format!("key-{i:03}")
}
fn eval(i: usize) -> String {
    format!("value-{i:03}:{}", "The quick brown fox jumps. ".repeat(3))
}

fn base_opts() -> Options {
    let mut o = Options::default();
    o.create_if_missing(true);
    o.create_missing_column_families(true);
    o.set_disable_auto_compactions(true);
    o.set_write_buffer_size(64 << 20); // nothing flushes unless the scenario flushes
    o.set_keep_log_file_num(1);
    o.set_merge_operator_associative("concat", concat_merge);
    // rust-rocksdb's Options::default() would silently pick Snappy; make every non-codec scenario
    // uncompressed so block contents are readable. codec-* scenarios override this explicitly.
    o.set_compression_type(DBCompressionType::None);
    o
}

fn bbo(bloom: bool) -> BlockBasedOptions {
    let mut b = BlockBasedOptions::default();
    b.set_block_size(BS);
    if bloom {
        b.set_bloom_filter(10.0, false);
    }
    b
}

fn list_dir(d: &Path) -> String {
    let mut v: Vec<String> = fs::read_dir(d)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| format!("{} ({} B)", e.file_name().to_string_lossy(), e.metadata().map(|m| m.len()).unwrap_or(0)))
        .collect();
    v.sort();
    v.join(", ")
}

fn facts_live(db: &DB, cfs: &[&str]) -> R<()> {
    let mut lf = db.live_files()?;
    lf.sort_by(|a, b| (a.column_family_name.clone(), a.level, a.name.clone()).cmp(&(b.column_family_name.clone(), b.level, b.name.clone())));
    println!("  live_files: CF | level | file | size | entries | deletions | smallest | largest");
    for f in &lf {
        println!(
            "    {} | L{} | {} | {} | {} | {} | {} | {}",
            f.column_family_name,
            f.level,
            f.name.trim_start_matches('/'),
            f.size,
            f.num_entries,
            f.num_deletions,
            esc(f.start_key.as_deref().unwrap_or(b"")),
            esc(f.end_key.as_deref().unwrap_or(b""))
        );
    }
    for cf in cfs {
        if let Some(h) = db.cf_handle(cf) {
            println!("  levelstats {cf}:");
            for l in db.property_value_cf(&h, "rocksdb.levelstats")?.unwrap_or_default().lines() {
                println!("    {l}");
            }
        }
    }
    Ok(())
}

/// One flushed SST over the shared logical data (N keys, default CF), DB closed cleanly.
fn single_sst(data: &Path, name: &str, desc: &str, o: Options) -> R<()> {
    let dir = data.join(name);
    {
        let db = DB::open(&o, &dir)?;
        for i in 0..N {
            db.put(ekey(i), eval(i))?;
        }
        db.flush()?;
        println!("== data/{name}: {desc}");
        facts_live(&db, &["default"])?;
        println!("  latest_sequence_number {}", db.latest_sequence_number());
    }
    clean_db_dir(&dir)?;
    println!("  files: {}", list_dir(&dir));
    Ok(())
}

fn scenario_basic(data: &Path) -> R<()> {
    let name = "basic-db";
    let dir = data.join(name);
    let mut o = base_opts();
    o.set_block_based_table_factory(&bbo(true));
    {
        let db = DB::open(&o, &dir)?;
        let fruit = ["apple", "apricot", "banana", "blueberry", "cherry", "date", "fig", "grape", "kiwi", "lemon", "mango", "orange"];
        for (i, f) in fruit.iter().enumerate() {
            db.put(format!("fruit:{f}"), format!("sweet-{i:02}"))?;
        }
        db.flush()?;
        for (k, v) in [("veg:carrot", "orange"), ("veg:leek", "green"), ("veg:beet", "red")] {
            db.put(k, v)?;
        }
        println!("== data/{name}: 12 Puts fruit:* -> flush -> 3 Puts veg:* left only in the WAL; clean close WITHOUT flush (WAL retained)");
        facts_live(&db, &["default"])?;
        println!("  latest_sequence_number {}", db.latest_sequence_number());
    }
    clean_db_dir(&dir)?;
    println!("  files: {}", list_dir(&dir));
    for w in list_files(&dir, "log") {
        println!("  WAL scan {}:\n{}", w.file_name().unwrap().to_string_lossy(), scan_wal(&w)?);
    }
    Ok(())
}

fn scenario_multilevel(data: &Path) -> R<()> {
    let name = "multilevel-db";
    let dir = data.join(name);
    let mut o = base_opts();
    o.set_level_compaction_dynamic_level_bytes(false); // manual compaction lands in L1, not L6
    o.set_num_levels(7);
    o.set_block_based_table_factory(&bbo(true));
    let descs = vec![
        ColumnFamilyDescriptor::new("default", o.clone()),
        ColumnFamilyDescriptor::new("cf_users", o.clone()),
        ColumnFamilyDescriptor::new("cf_events", o.clone()),
    ];
    {
        let db = DB::open_cf_descriptors(&o, &dir, descs)?;
        let cd = db.cf_handle("default").unwrap();
        let cu = db.cf_handle("cf_users").unwrap();
        let ce = db.cf_handle("cf_events").unwrap();
        let v = |tag: &str, i: usize| format!("{tag}-{i:03}-{}", "payload-".repeat(4));
        // default: 2 overlapping flushes -> compact into one L1 file -> 1 newer L0 file -> 3 WAL-only puts
        for i in 0..20 { db.put(format!("d-{i:03}"), v("d1", i))?; }
        db.flush_cf(&cd)?;
        for i in 10..30 { db.put(format!("d-{i:03}"), v("d2", i))?; }
        db.delete(b"d-005")?;
        db.flush_cf(&cd)?;
        db.compact_range_cf(&cd, None::<&[u8]>, None::<&[u8]>);
        for i in 25..35 { db.put(format!("d-{i:03}"), v("d3", i))?; }
        db.flush_cf(&cd)?;
        // cf_users: two overlapping flushes, never compacted -> 2 L0 files
        for i in 0..15 { db.put_cf(&cu, format!("u-{i:03}"), v("u1", i))?; }
        db.flush_cf(&cu)?;
        for i in 10..25 { db.put_cf(&cu, format!("u-{i:03}"), v("u2", i))?; }
        db.flush_cf(&cu)?;
        // cf_events: 2 disjoint flushes -> compact into L1 -> tombstones + 1 Put flushed to a newer L0 file
        for i in 0..15 { db.put_cf(&ce, format!("e-{i:03}"), v("e1", i))?; }
        db.flush_cf(&ce)?;
        for i in 15..30 { db.put_cf(&ce, format!("e-{i:03}"), v("e2", i))?; }
        db.flush_cf(&ce)?;
        db.compact_range_cf(&ce, None::<&[u8]>, None::<&[u8]>);
        db.delete_cf(&ce, b"e-003")?;
        db.delete_range_cf(&ce, b"e-010", b"e-014")?;
        db.put_cf(&ce, b"e-040", v("e3", 40))?;
        db.flush_cf(&ce)?;
        // WAL-only tail
        for i in 0..3 { db.put(format!("d-w{i}"), format!("wal-only-{i}"))?; }
        println!("== data/{name}: 3 CFs, controlled flushes + compact_range; clean close WITHOUT flush (3 default Puts d-w0..d-w2 only in WAL)");
        facts_live(&db, &["default", "cf_users", "cf_events"])?;
        println!("  latest_sequence_number {}", db.latest_sequence_number());
    }
    clean_db_dir(&dir)?;
    println!("  files: {}", list_dir(&dir));
    for w in list_files(&dir, "log") {
        println!("  WAL scan {}:\n{}", w.file_name().unwrap().to_string_lossy(), scan_wal(&w)?);
    }
    Ok(())
}

fn scenario_wal_basic(data: &Path) -> R<()> {
    let name = "wal-basic";
    let dir = data.join(name);
    let names = ["default", "cf_two"];
    let mut log = String::new();
    {
        let o = wal_opts(); // 256 MiB memtable, no flush, merge operator concat, legacy WAL headers
        let db = DB::open_cf_descriptors(&o, &dir, cf_descs(&names, &o))?;
        let c2 = db.cf_handle("cf_two").unwrap();
        writeln!(log, "| sequence | count | operation |\n|---|---|---|")?;
        step(&db, &mut log, "default: Put apple=red", |d| d.put(b"apple", b"red"))?;
        step(&db, &mut log, "default: Put banana=yellow", |d| d.put(b"banana", b"yellow"))?;
        step(&db, &mut log, "default: Put cherry=dark-red", |d| d.put(b"cherry", b"dark-red"))?;
        step(&db, &mut log, "default: Delete banana", |d| d.delete(b"banana"))?;
        step(&db, &mut log, "default: Put once=1 (written exactly once, as SingleDelete requires)", |d| d.put(b"once", b"1"))?;
        step(&db, &mut log, "default: SingleDelete once", |d| d.write(single_delete_batch(&[b"once"])))?;
        step(&db, &mut log, "default: Merge counter <- a", |d| d.merge(b"counter", b"a"))?;
        step(&db, &mut log, "default: Merge counter <- b", |d| d.merge(b"counter", b"b"))?;
        step(&db, &mut log, "cf_two: Put range-a=1, range-b=2, range-c=3 (3 writes)", |d| {
            d.put_cf(&c2, b"range-a", b"1")?;
            d.put_cf(&c2, b"range-b", b"2")?;
            d.put_cf(&c2, b"range-c", b"3")
        })?;
        step(&db, &mut log, "cf_two: DeleteRange [range-a, range-c) (end exclusive, range-c survives)", |d| d.delete_range_cf(&c2, b"range-a", b"range-c"))?;
        step(&db, &mut log, "cf_two: Put note=hello", |d| d.put_cf(&c2, b"note", b"hello"))?;
        step(&db, &mut log, "ONE WriteBatch across both CFs: Put default zeta=last; Put cf_two omega=end", |d| {
            let mut b = WriteBatch::default();
            b.put(b"zeta", b"last");
            b.put_cf(&c2, b"omega", b"end");
            d.write(b)
        })?;
        assert_eq!(db.get(b"counter")?.as_deref(), Some(&b"a,b"[..]));
        assert_eq!(db.get(b"banana")?, None);
        assert_eq!(db.get(b"once")?, None);
        assert_eq!(db.get_cf(&c2, b"range-a")?, None);
        assert_eq!(db.get_cf(&c2, b"range-c")?.as_deref(), Some(&b"3"[..]));
        writeln!(log, "\nFinal latest_sequence_number: {}", db.latest_sequence_number())?;
    }
    clean_db_dir(&dir)?;
    assert!(list_files(&dir, "sst").is_empty());
    println!("== data/{name}: WAL-only DB, clean close without flush, no SST");
    println!("{log}");
    println!("  files: {}", list_dir(&dir));
    for w in list_files(&dir, "log") {
        println!("  WAL scan {}:\n{}", w.file_name().unwrap().to_string_lossy(), scan_wal(&w)?);
    }
    Ok(())
}

fn scenario_wal_fragmented(data: &Path) -> R<()> {
    let name = "wal-fragmented";
    let dir = data.join(name);
    let big: Vec<u8> = (0..40 * 1024).map(|i| b'0' + (i % 10) as u8).collect();
    {
        let o = wal_opts();
        let db = DB::open_cf_descriptors(&o, &dir, cf_descs(&["default"], &o))?;
        for i in 0..30 { db.put(format!("small-{i:03}"), format!("small-value-{i:03}-{}", "x".repeat(24)))?; }
        db.put(b"big-40k", &big)?;
        for i in 30..50 { db.put(format!("small-{i:03}"), format!("small-value-{i:03}-{}", "x".repeat(24)))?; }
        println!("== data/{name}: 30 small Puts, Put big-40k (40960 B, byte[i]='0'+(i mod 10)), 20 small Puts; no flush");
        println!("  latest_sequence_number {}", db.latest_sequence_number());
    }
    clean_db_dir(&dir)?;
    println!("  files: {}", list_dir(&dir));
    for w in list_files(&dir, "log") {
        println!("  WAL scan {}:\n{}", w.file_name().unwrap().to_string_lossy(), scan_wal(&w)?);
    }
    Ok(())
}

fn scenario_wal_recycled(data: &Path, scratch: &Path) -> R<()> {
    let name = "wal-recycled";
    let dir = data.join(name);
    let work = scratch.join("work-recycled");
    if work.exists() {
        fs::remove_dir_all(&work)?;
    }
    fs::create_dir_all(&dir)?;
    let mut log = String::new();
    {
        let mut o = wal_opts();
        o.set_recycle_log_file_num(2);
        let db = DB::open_cf_descriptors(&o, &work, cf_descs(&["default", "cf_two"], &o))?;
        let c2 = db.cf_handle("cf_two").unwrap();
        let rounds: [(&str, usize, bool); 4] = [("r1", 6, true), ("r2", 50, true), ("r3", 4, true), ("r4", 6, false)];
        writeln!(log, "| round | puts to default (+1 `<tag>-c2` to cf_two) | flushed after? |\n|---|---|---|")?;
        for (tag, n, flush) in rounds {
            for i in 0..n {
                db.put(format!("{tag}-{i:03}"), format!("{tag}-value-{i:03}-{}", "y".repeat(180)))?;
            }
            db.put_cf(&c2, format!("{tag}-c2"), b"c2")?;
            writeln!(log, "| {tag} | {n} keys `{tag}-NNN` = `{tag}-value-NNN-` + 180 x `y` | {} |", if flush { "yes (default + cf_two)" } else { "NO (stays in live WAL)" })?;
            if flush {
                db.flush()?;
                db.flush_cf(&c2)?;
                std::thread::sleep(std::time::Duration::from_millis(700));
            }
        }
        writeln!(log, "Final latest_sequence_number: {}", db.latest_sequence_number())?;
        // Crash-consistent image copied while the DB is still OPEN: a clean close truncates the live
        // WAL to the bytes written, which would remove the stale tail that makes a recycled log interesting.
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
    println!("== data/{name}: recycle_log_file_num=2; snapshot copied while DB open (crash image)");
    println!("{log}");
    println!("  files: {}", list_dir(&dir));
    for w in list_files(&dir, "log") {
        println!("  WAL scan {}:\n{}", w.file_name().unwrap().to_string_lossy(), scan_wal(&w)?);
    }
    Ok(())
}

fn put_file(files: &Path, scen: &str, fname: &str, bytes: &[u8]) -> R<PathBuf> {
    let d = files.join(scen);
    fs::create_dir_all(&d)?;
    let p = d.join(fname);
    fs::write(&p, bytes)?;
    Ok(p)
}

fn one(dir: &Path, ext: &str) -> PathBuf {
    let v = list_files(dir, ext);
    assert_eq!(v.len(), 1, "expected exactly one .{ext} in {}", dir.display());
    v[0].clone()
}

fn last(dir: &Path, ext: &str) -> PathBuf {
    list_files(dir, ext).pop().unwrap_or_else(|| panic!("no .{ext} in {}", dir.display()))
}

fn build_files(data: &Path, files: &Path) -> R<()> {
    println!("== files/ (standalone, one file per directory)");
    for c in ["none", "snappy", "zlib", "bzip2", "lz4", "lz4hc", "zstd"] {
        let src = one(&data.join(format!("codec-{c}")), "sst");
        put_file(files, &format!("sst-{c}"), &format!("sst-{c}.sst"), &fs::read(&src)?)?;
        println!("  files/sst-{c}/sst-{c}.sst <- data/codec-{c}/{}", src.file_name().unwrap().to_string_lossy());
    }
    let src = one(&data.join("bloom-full"), "sst");
    put_file(files, "sst-bloom-full", "sst-bloom-full.sst", &fs::read(&src)?)?;
    println!("  files/sst-bloom-full/sst-bloom-full.sst <- data/bloom-full/{}", src.file_name().unwrap().to_string_lossy());

    let wal_src = last(&data.join("wal-basic"), "log");
    let wb = fs::read(&wal_src)?;
    // valid WALs keep their original numeric names so the log number stays derivable from the name
    let wn = wal_src.file_name().unwrap().to_string_lossy().to_string();
    put_file(files, "wal-plain", &wn, &wb)?;
    println!("  files/wal-plain/{wn} <- data/wal-basic/{wn}");
    let frag_src = last(&data.join("wal-fragmented"), "log");
    let fnm = frag_src.file_name().unwrap().to_string_lossy().to_string();
    put_file(files, "wal-fragmented", &fnm, &fs::read(&frag_src)?)?;
    println!("  files/wal-fragmented/{fnm} <- data/wal-fragmented/{fnm}");
    let rec_src = last(&data.join("wal-recycled"), "log");
    let rnm = rec_src.file_name().unwrap().to_string_lossy().to_string();
    put_file(files, "wal-recycled", &rnm, &fs::read(&rec_src)?)?;
    println!("  files/wal-recycled/{rnm} <- data/wal-recycled/{rnm}");

    // MANIFEST + CURRENT from basic-db (original file names kept: kind detection uses the name).
    let bdir = data.join("basic-db");
    let cur = fs::read_to_string(bdir.join("CURRENT"))?;
    let mname = cur.trim().to_string();
    put_file(files, "manifest-basic", &mname, &fs::read(bdir.join(&mname))?)?;
    put_file(files, "current-basic", "CURRENT", cur.as_bytes())?;
    println!("  files/manifest-basic/{mname} and files/current-basic/CURRENT <- data/basic-db/");

    // ---- corrupt copies (originals untouched). 'flip' = XOR 0xFF.
    println!("== corrupt copies");
    let sst_src = one(&data.join("codec-none"), "sst");
    let sb = fs::read(&sst_src)?;
    let sname = sst_src.file_name().unwrap().to_string_lossy().to_string();
    let off = 100usize;
    let mut c = sb.clone();
    let old = c[off];
    c[off] ^= 0xFF;
    put_file(files, "corrupt-sst-checksum-flip", "corrupt-sst-checksum-flip.sst", &c)?;
    println!("  corrupt-sst-checksum-flip.sst <- data/codec-none/{sname} (size {}): offset {off}: 0x{old:02x} -> 0x{:02x} (inside data block 0, which starts at offset 0)", sb.len(), c[off]);
    let cut = sb.len() / 2;
    put_file(files, "corrupt-sst-truncated", "corrupt-sst-truncated.sst", &sb[..cut])?;
    println!("  corrupt-sst-truncated.sst <- data/codec-none/{sname}: truncated to {cut} of {} bytes (footer, index, properties lost)", sb.len());
    let moff = sb.len() - 8;
    let mut c = sb.clone();
    let old = c[moff];
    c[moff] ^= 0xFF;
    put_file(files, "corrupt-sst-bad-footer", "corrupt-sst-bad-footer.sst", &c)?;
    println!("  corrupt-sst-bad-footer.sst <- data/codec-none/{sname}: offset {moff} (first byte of the 8-byte footer magic, file length {}): 0x{old:02x} -> 0x{:02x}", sb.len(), c[moff]);

    // WAL record offsets (legacy 7-byte headers)
    let mut offs = vec![];
    let mut p = 0;
    while p + 7 <= wb.len() {
        let l = u16::from_le_bytes([wb[p + 4], wb[p + 5]]) as usize;
        offs.push((p, l));
        p += 7 + l;
    }
    assert!(offs.len() >= 4 && p == wb.len(), "wal-basic must be small legacy FULL records only");
    let (r1, l1) = offs[1];
    let flip = r1 + 7 + l1 / 2;
    let mut c = wb.clone();
    let old = c[flip];
    c[flip] ^= 0xFF;
    put_file(files, "corrupt-wal-checksum-flip", "corrupt-wal-checksum-flip.log", &c)?;
    println!("  corrupt-wal-checksum-flip.log <- files/wal-plain/{wn} (= data/wal-basic/{wn}): offset {flip}: 0x{old:02x} -> 0x{:02x} (payload byte of physical record index 1; record at {r1}, header 7, payload {l1}); {} records in file", c[flip], offs.len());
    let (r2, l2) = offs[2];
    let tcut = r2 + 7 + l2 / 2;
    put_file(files, "corrupt-wal-truncated", "corrupt-wal-truncated.log", &wb[..tcut])?;
    println!("  corrupt-wal-truncated.log <- files/wal-plain/{wn}: truncated to {tcut} of {} bytes (record index 2 at {r2}, header 7, payload {l2}, cut {} bytes into its payload)", wb.len(), l2 / 2);
    Ok(())
}

fn walk_files(dir: &Path, base: &Path, acc: &mut Vec<PathBuf>) -> R<()> {
    walk(dir, base, acc)
}

pub fn run(out: &Path) -> R<()> {
    fs::create_dir_all(out)?;
    let out = out.canonicalize()?;
    for d in ["data", "files"] {
        let p = out.join(d);
        if p.exists() {
            fs::remove_dir_all(&p)?;
        }
    }
    let _ = fs::remove_file(out.join("SHA256SUMS"));
    let data = out.join("data");
    let files = out.join("files");
    fs::create_dir_all(&data)?;
    fs::create_dir_all(&files)?;
    let scratch = out.join(".gen-scratch");
    fs::create_dir_all(&scratch)?;

    scenario_basic(&data)?;

    // Bloom: same logical data, only the filter differs.
    single_sst(&data, "bloom-full", "full Bloom filter (bloom 10 bits/key, block-based = false)", {
        let mut o = base_opts();
        o.set_block_based_table_factory(&bbo(true));
        o
    })?;
    single_sst(&data, "bloom-partitioned", "partitioned Bloom filter (partition_filters, metadata_block_size 128; requires the two-level index)", {
        let mut b = bbo(true);
        b.set_index_type(BlockBasedIndexType::TwoLevelIndexSearch);
        b.set_partition_filters(true);
        b.set_metadata_block_size(128);
        let mut o = base_opts();
        o.set_block_based_table_factory(&b);
        o
    })?;
    single_sst(&data, "bloom-absent", "no filter policy (index still present)", {
        let mut o = base_opts();
        o.set_block_based_table_factory(&bbo(false));
        o
    })?;

    // Index variants (full Bloom except where partition filters are the point).
    single_sst(&data, "index-binary", "default binary-search index (single index block), full Bloom", {
        let mut o = base_opts();
        o.set_block_based_table_factory(&bbo(true));
        o
    })?;
    single_sst(&data, "index-partitioned", "two-level (partitioned) index + partitioned filter, metadata_block_size 128", {
        let mut b = bbo(true);
        b.set_index_type(BlockBasedIndexType::TwoLevelIndexSearch);
        b.set_partition_filters(true);
        b.set_metadata_block_size(128);
        let mut o = base_opts();
        o.set_block_based_table_factory(&b);
        o
    })?;
    single_sst(&data, "index-hash", "binary-search index + data-block hash index (BinaryAndHash, ratio 0.75), full Bloom", {
        let mut b = bbo(true);
        b.set_data_block_index_type(DataBlockIndexType::BinaryAndHash);
        b.set_data_block_hash_ratio(0.75);
        let mut o = base_opts();
        o.set_block_based_table_factory(&b);
        o
    })?;

    // Codecs: same logical data, only compression differs.
    for (n, c) in [
        ("none", DBCompressionType::None),
        ("snappy", DBCompressionType::Snappy),
        ("zlib", DBCompressionType::Zlib),
        ("bzip2", DBCompressionType::Bz2),
        ("lz4", DBCompressionType::Lz4),
        ("lz4hc", DBCompressionType::Lz4hc),
        ("zstd", DBCompressionType::Zstd),
    ] {
        let mut o = base_opts();
        o.set_compression_type(c);
        o.set_block_based_table_factory(&bbo(true));
        single_sst(&data, &format!("codec-{n}"), &format!("compression option {c:?}, full Bloom"), o)?;
    }

    scenario_multilevel(&data)?;
    scenario_wal_basic(&data)?;
    scenario_wal_fragmented(&data)?;
    scenario_wal_recycled(&data, &scratch)?;
    fs::remove_dir_all(&scratch)?;

    build_files(&data, &files)?;

    // inventory + SHA256SUMS (paths relative to examples/)
    let mut all = vec![];
    for sub in ["data", "files"] {
        walk_files(&out.join(sub), &out, &mut all)?;
    }
    let mut sums = String::new();
    let mut total = 0u64;
    for f in &all {
        let b = fs::read(out.join(f))?;
        total += b.len() as u64;
        let h = Sha256::digest(&b);
        let hex: String = h.iter().map(|x| format!("{x:02x}")).collect();
        writeln!(sums, "{hex}  {}", f.display())?;
    }
    fs::write(out.join("SHA256SUMS"), &sums)?;
    for sub in ["data", "files"] {
        let mut ds: Vec<PathBuf> = fs::read_dir(out.join(sub))?.map(|e| e.unwrap().path()).collect();
        ds.sort();
        for p in ds {
            let mut v = vec![];
            walk(&p, &p, &mut v)?;
            let bytes: u64 = v.iter().map(|x| fs::metadata(p.join(x)).map(|m| m.len()).unwrap_or(0)).sum();
            println!("size {}/{}: {} B in {} files", sub, p.file_name().unwrap().to_string_lossy(), bytes, v.len());
        }
    }
    println!("OK {} files, {} bytes (data+files)", all.len(), total);
    assert!(total < 5 << 20);
    Ok(())
}

// ------------------------------------------------------------------------ verify

fn ro_counts(src: &Path, scratch: &Path) -> R<Vec<(String, usize)>> {
    let d = scratch.join(src.file_name().unwrap());
    copy_dir(src, &d)?;
    let mut o = Options::default();
    o.set_merge_operator_associative("concat", concat_merge);
    o.set_level_compaction_dynamic_level_bytes(false);
    let cfs = DB::list_cf(&o, &d)?;
    // NOTE: DB::open_cf*(names) gives every CF Options::default() (no merge operator), which makes WAL
    // replay of Merge records stop early; pass descriptors carrying the merge-operator options instead.
    let descs: Vec<ColumnFamilyDescriptor> = cfs.iter().map(|n| ColumnFamilyDescriptor::new(n.as_str(), o.clone())).collect();
    let db = DB::open_cf_descriptors_read_only(&o, &d, descs, false)?;
    let mut v = vec![];
    for cf in &cfs {
        v.push((cf.clone(), count_cf(&db, cf)?.0));
        if std::env::var_os("PEEK").is_some() {
            let h = db.cf_handle(cf).unwrap();
            let mut it = db.raw_iterator_cf(&h);
            it.seek_to_first();
            while it.valid() {
                println!("    peek {cf}: {} = {}", esc(it.key().unwrap()), esc(&it.value().unwrap()[..it.value().unwrap().len().min(30)]));
                it.next();
            }
            println!("    peek {cf}: status {:?}", it.status());
        }
    }
    if src.ends_with("multilevel-db") {
        println!("  [multilevel-db read-only reopen]");
        facts_live(&db, &["default", "cf_users", "cf_events"])?;
    }
    Ok(v)
}

fn expected_counts(name: &str) -> Vec<(&'static str, usize)> {
    match name {
        "basic-db" => vec![("default", 15)],
        "multilevel-db" => vec![("default", 37), ("cf_users", 25), ("cf_events", 26)],
        "wal-basic" => vec![("default", 4), ("cf_two", 3)],
        "wal-fragmented" => vec![("default", 51)],
        "wal-recycled" => vec![("default", 6 + 50 + 4 + 6), ("cf_two", 4)],
        _ => vec![("default", N)],
    }
}

pub fn verify(ex: &Path, scratch: &Path) -> R<()> {
    if scratch.exists() {
        fs::remove_dir_all(scratch)?;
    }
    fs::create_dir_all(scratch)?;
    let mut ok = true;
    let mut chk = |name: &str, cond: bool, detail: String| {
        println!("{} {name}: {detail}", if cond { "PASS" } else { "FAIL" });
        ok &= cond;
    };
    // 1. every data/ scenario reopens read-only (on a scratch copy) with the expected key counts
    let mut scen: Vec<PathBuf> = fs::read_dir(ex.join("data"))?.map(|e| e.unwrap().path()).collect();
    scen.sort();
    for s in &scen {
        let nm = s.file_name().unwrap().to_string_lossy().to_string();
        match ro_counts(s, scratch) {
            Err(e) => chk(&format!("data/{nm}"), false, format!("read-only open failed: {e}")),
            Ok(got) => {
                let exp = expected_counts(&nm);
                let mut g = got.clone();
                g.sort();
                let mut e: Vec<(String, usize)> = exp.iter().map(|(a, b)| (a.to_string(), *b)).collect();
                e.sort();
                chk(&format!("data/{nm}"), g == e, format!("keys per CF {:?} (expected {:?})", got, exp));
            }
        }
    }
    // 2. every files/ SST is checked with the engine by substituting it for the SST of its source
    //    scenario (scratch copy): DB-flushed SSTs cannot be ingested (no external-file version
    //    property), and the C API has no SstFileReader. Valid ones must reopen with all keys and
    //    iterate with checksum verification; corrupt ones must make open or iteration fail.
    let src_of = |d: &str| -> String {
        if let Some(c) = d.strip_prefix("sst-bloom-full") { let _ = c; "bloom-full".to_string() }
        else if let Some(c) = d.strip_prefix("sst-") { format!("codec-{c}") }
        else { "codec-none".to_string() }
    };
    let mut dirs: Vec<PathBuf> = fs::read_dir(ex.join("files"))?.map(|e| e.unwrap().path()).filter(|p| {
        let n = p.file_name().unwrap().to_string_lossy().to_string();
        n.starts_with("sst-") || n.starts_with("corrupt-sst")
    }).collect();
    dirs.sort();
    for d in dirs {
        let dn = d.file_name().unwrap().to_string_lossy().to_string();
        let f = one(&d, "sst");
        let corrupt = dn.starts_with("corrupt-");
        let src = ex.join("data").join(src_of(&dn));
        let work = scratch.join(format!("sub-{dn}"));
        copy_dir(&src, &work)?;
        let target = one(&work, "sst");
        let same = fs::read(&f)? == fs::read(&target)?;
        fs::copy(&f, &target)?;
        let mut o = Options::default();
        o.set_merge_operator_associative("concat", concat_merge);
        let label = format!("files/{dn}/{}", f.file_name().unwrap().to_string_lossy());
        let r = (|| -> R<(usize, Option<String>)> {
            let db = DB::open_for_read_only(&o, &work, false)?;
            let mut it = db.raw_iterator();
            it.seek_to_first();
            let mut n = 0;
            while it.valid() { n += 1; it.next(); }
            Ok((n, it.status().err().map(|e| e.to_string())))
        })();
        if corrupt {
            let (bad, detail) = match r {
                Err(e) => (true, format!("read-only open failed: {e}")),
                Ok((n, Some(e))) => (true, format!("opened; iteration error after {n} entries: {e}")),
                Ok((n, None)) => (false, format!("NOT detected: {n} entries read")),
            };
            chk(&label, bad && !same, format!("engine detects damage ({detail}); differs from source SST: {}", !same));
        } else {
            match r {
                Ok((n, None)) => chk(&label, n == N && same, format!("substituted into {}: {n} entries iterated with checksum verification; byte-identical to source SST: {same}", src.file_name().unwrap().to_string_lossy())),
                Ok((n, Some(e))) => chk(&label, false, format!("iteration error after {n}: {e}")),
                Err(e) => chk(&label, false, format!("open failed: {e}")),
            }
        }
    }
    println!("{}", if ok { "ALL PASS" } else { "SOME FAIL" });
    if !ok {
        std::process::exit(1);
    }
    Ok(())
}
