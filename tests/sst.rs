//! SST integration tests against tests/fixtures/sst-* and sst-variants.
mod common;
use common::*;
use rocksvision::inspect;
use rocksvision::model::{BlockKind, NodeKind, Space, Status};
use rocksvision::sst::footer;
use rocksvision::sst::{compress, SstInspector};

fn open_sst(rel: &str) -> SstInspector {
    SstInspector::open(rd(rel))
}

fn tail(rel: &str) -> (Vec<u8>, u64) {
    let r = rd(rel);
    let n = r.len().min(53);
    (r.read_at(r.len() - n, n).unwrap(), r.len())
}

struct Scan {
    blocks: usize,
    entries: usize,
    first: Vec<u8>,
    last: Vec<u8>,
    prefix_checked: usize,
    compressed_blocks: usize,
    stored: u64,
    contents: u64,
}

/// Walks every data block through the index; asserts per-block invariants.
fn scan_data(s: &mut SstInspector, name: &str, expect_ctype: Option<u8>) -> Scan {
    let (hs, more, err) = s.data_handles(0, 1_000_000, &never);
    assert!(!more && err.is_none(), "{name}: data_handles more={more} err={err:?}");
    assert!(!hs.is_empty(), "{name}: no data blocks");
    let mut sc = Scan { blocks: 0, entries: 0, first: vec![], last: vec![], prefix_checked: 0, compressed_blocks: 0, stored: 0, contents: 0 };
    for (h, _sep) in hs {
        let (b, res) = s.block_entries(h, BlockKind::Data);
        assert!(b.error.is_none(), "{name}: block @{} error {:?}", h.offset, b.error);
        assert_eq!(b.checksum_status, Status::Present, "{name}: block @{} checksum", h.offset);
        assert_eq!(b.stored_checksum.wrapping_sub(b.modifier), b.computed_checksum.unwrap());
        if let Some(c) = expect_ctype {
            if b.ctype != 0 {
                assert_eq!(b.ctype, c, "{name}: ctype");
            }
        }
        if b.compressed {
            sc.compressed_blocks += 1;
            assert_ne!(h.size as usize, b.data.len(), "{name}: stored size == decompressed size for compressed block");
        } else {
            assert_eq!(h.size as usize, b.data.len(), "{name}: uncompressed block size");
        }
        sc.stored += h.size;
        sc.contents += b.data.len() as u64;
        let (lay, es, e) = res.unwrap_or_else(|e| panic!("{name}: entries {e:?}"));
        assert!(e.is_none(), "{name}: entries err {e:?}");
        assert!(!es.is_empty());
        assert!(lay.num_restarts >= 1);
        for en in &es {
            assert_eq!(en.status, Status::Present);
            assert!(en.key.len() >= 8);
            let n = sc.entries;
            let (uk, trailer) = en.key.split_at(en.key.len() - 8);
            assert_eq!(uk, format!("key-{n:06}").as_bytes(), "{name}: key of entry {n}");
            assert_eq!(trailer, &[1, 0, 0, 0, 0, 0, 0, 0], "{name}: trailer of entry {n}");
            let v = &b.data[en.value_off..en.value_off + en.value_len];
            assert!(v.starts_with(format!("value-{n:06}:").as_bytes()), "{name}: value of entry {n}");
            if en.shared > 0 {
                // prefix reconstruction: stored suffix shorter than the full key
                assert!((en.non_shared as usize) < en.key.len());
                assert!(!en.restart);
                sc.prefix_checked += 1;
            }
            if sc.entries == 0 {
                sc.first = uk.to_vec();
            }
            sc.last = uk.to_vec();
            sc.entries += 1;
        }
        sc.blocks += 1;
    }
    sc
}

#[test]
fn sst_codecs_all_blocks_verify_and_decode() {
    // (dir, expected on-disk compression type byte of data blocks, 0 = none)
    let cases: [(&str, u8); 7] = [("sst-none", 0), ("sst-snappy", 1), ("sst-zlib", 2), ("sst-bzip2", 3), ("sst-lz4", 4), ("sst-lz4hc", 5), ("sst-zstd", 7)];
    for (dir, ct) in cases {
        let rel = format!("{dir}/data.sst");
        let mut s = open_sst(&rel);
        let f = s.footer.clone().unwrap_or_else(|e| panic!("{dir}: footer {e}"));
        assert_eq!(f.format_version, 6, "{dir}");
        assert_eq!(f.checksum_type, 4, "{dir}");
        assert_eq!(f.magic, footer::BLOCK_BASED_MAGIC);
        assert!(!f.legacy);
        let (stored, computed) = f.footer_checksum.expect("fv6 footer checksum");
        assert_eq!(stored, computed, "{dir}: footer checksum");
        assert!(f.fields.iter().filter(|x| x.name == "footer checksum").all(|x| x.status == Status::Present));
        assert!(s.meta.is_ok(), "{dir}: metaindex {:?}", s.meta);
        assert_eq!(s.prop_u64("rocksdb.num.entries"), Some(2000), "{dir}");
        let sc = scan_data(&mut s, dir, if ct == 0 { None } else { Some(ct) });
        assert_eq!(sc.entries, 2000, "{dir}: entries");
        assert_eq!(sc.first, b"key-000000", "{dir}");
        assert_eq!(sc.last, b"key-001999", "{dir}");
        assert_eq!(s.prop_u64("rocksdb.num.data.blocks"), Some(sc.blocks as u64), "{dir}: num.data.blocks");
        assert!(sc.prefix_checked > 1000, "{dir}: prefix-reconstructed entries {}", sc.prefix_checked);
        if ct == 0 {
            assert_eq!(sc.compressed_blocks, 0, "{dir}");
            assert_eq!(sc.stored, sc.contents);
        } else {
            assert!(sc.compressed_blocks as f64 >= 0.95 * sc.blocks as f64, "{dir}: compressed {}/{}", sc.compressed_blocks, sc.blocks);
            assert!(sc.stored < sc.contents, "{dir}: stored {} !< contents {}", sc.stored, sc.contents);
        }
    }
}

#[test]
fn sst_physical_and_decompressed_are_separate_spaces() {
    for (dir, compressed) in [("sst-none", false), ("sst-snappy", true), ("sst-zlib", true), ("sst-bzip2", true), ("sst-lz4", true), ("sst-lz4hc", true), ("sst-zstd", true)] {
        let rel = format!("{dir}/data.sst");
        let mut s = open_sst(&rel);
        let (hs, _, _) = s.data_handles(0, 3, &never);
        let (h, _) = hs[0];
        let b = s.block(h, BlockKind::Data);
        let v = s.view(&NodeKind::SstBlock { off: h.offset, size: h.size, kind: BlockKind::Data }, &never);
        assert!(!v.rows.is_empty());
        if compressed {
            assert!(b.compressed, "{dir}");
            assert_eq!(v.spaces.len(), 1, "{dir}: spaces");
            assert_eq!(v.spaces[0].space, Space::Block(h.offset), "{dir}");
            assert_eq!(v.spaces[0].size, b.data.len() as u64);
            assert_ne!(v.spaces[0].size, h.size, "{dir}: sizes must differ");
            assert!(v.title.contains(&format!("{} B stored", h.size)), "{}", v.title);
            for r in &v.rows {
                let sp = r.field.span.unwrap();
                assert_eq!(sp.space, Space::Block(h.offset), "{dir}: entry span must be block-local");
                assert!(sp.offset + sp.len <= b.data.len() as u64);
            }
            let phys = s.space_bytes(Space::File, h.offset, h.size).unwrap();
            let dec = s.space_bytes(Space::Block(h.offset), 0, 1 << 24).unwrap();
            assert_eq!(phys.len() as u64, h.size);
            assert_eq!(dec.len(), b.data.len());
            assert_ne!(phys.len(), dec.len());
            assert_eq!(dec, *b.data);
            let d = compress::decompress(b.ctype, &phys, 2, None).expect("decompress");
            assert_eq!(d.data, *b.data, "{dir}");
        } else {
            assert!(!b.compressed);
            assert!(v.spaces.is_empty(), "{dir}: uncompressed blocks have no derived space");
            for r in &v.rows {
                let sp = r.field.span.unwrap();
                assert_eq!(sp.space, Space::File);
                assert!(sp.offset >= h.offset && sp.offset + sp.len <= h.offset + h.size);
            }
            let phys = s.space_bytes(Space::File, h.offset, h.size).unwrap();
            assert_eq!(phys, *b.data);
        }
        // trailer fields live in file space in both cases
        let t = s.view(&NodeKind::SstTrailer { off: h.offset, size: h.size, kind: BlockKind::Data }, &never);
        assert!(t.rows.iter().any(|r| r.field.span.map(|sp| sp.space == Space::File && sp.offset >= h.offset + h.size).unwrap_or(false)), "{dir}: trailer spans");
    }
}

#[test]
fn sst_prefix_reconstruction_from_entry_view() {
    let mut s = open_sst("sst-none/data.sst");
    let (hs, _, _) = s.data_handles(0, 1, &never);
    let (h, _) = hs[0];
    let (_, res) = s.block_entries(h, BlockKind::Data);
    let (_, es, _) = res.unwrap();
    let e = es.iter().find(|e| e.shared > 0).expect("shared entry");
    assert_eq!(e.shared as usize + e.non_shared as usize, e.key.len());
    assert_eq!(&e.key[..e.key.len() - 8], format!("key-{:06}", e.idx).as_bytes());
    let v = s.view(&NodeKind::SstEntry { off: h.offset, size: h.size, kind: BlockKind::Data, idx: e.idx }, &never);
    let by = |n: &str| v.rows.iter().find(|r| r.field.name.starts_with(n)).unwrap_or_else(|| panic!("no {n}"));
    assert_eq!(by("shared").field.value, e.shared.to_string());
    assert_eq!(by("key (reconstructed)").field.span, None); // no invented on-disk range
    assert_eq!(by("key (reconstructed)").field.status, Status::Implicit);
    assert_eq!(by("user key").field.value.trim_matches('"'), format!("key-{:06}", e.idx));
    let stored = by("key delta");
    assert_eq!(stored.field.span.unwrap().len as u32, e.non_shared);
    let sp = stored.field.span.unwrap();
    assert_eq!(sp.space, Space::File);
    let raw = s.space_bytes(Space::File, sp.offset, sp.len).unwrap();
    assert_eq!(raw, e.key[e.shared as usize..]);
    assert_eq!(by("sequence").field.value, "0");
    assert!(by("value type").field.value.starts_with("1 "));
}

#[test]
fn sst_format_versions_decode() {
    for v in 2u32..=7 {
        let rel = format!("sst-variants/format-v{v}.sst");
        let (t, len) = tail(&rel);
        let f = footer::decode(&t, len).unwrap_or_else(|e| panic!("v{v}: {e}"));
        assert_eq!(f.format_version, v, "direct decode");
        assert_eq!(f.checksum_type, 4);
        let mut s = open_sst(&rel);
        assert_eq!(s.footer.as_ref().unwrap().format_version, v);
        assert!(s.meta.is_ok(), "v{v}: {:?}", s.meta);
        assert_eq!(s.prop_u64("rocksdb.num.entries"), Some(600), "v{v}");
        assert_eq!(s.prop_u64("rocksdb.format.version"), Some(v as u64));
        if v >= 6 {
            assert!(f.index.is_none());
            assert!(s.index_handle().is_some(), "v{v}: index via metaindex");
        } else {
            assert!(f.index.is_some(), "v{v}: index handle in footer");
        }
        let (hs, _, err) = s.data_handles(0, 100_000, &never);
        assert!(err.is_none(), "v{v}: {err:?}");
        let mut n = 0usize;
        for (h, _) in hs {
            let (b, res) = s.block_entries(h, BlockKind::Data);
            assert_eq!(b.checksum_status, Status::Present, "v{v}");
            let (_, es, e) = res.unwrap();
            assert!(e.is_none());
            for en in es {
                assert_eq!(&en.key[..en.key.len() - 8], format!("key-{n:06}").as_bytes(), "v{v} entry {n}");
                n += 1;
            }
        }
        assert_eq!(n, 600, "v{v}");
    }
}

#[test]
fn sst_all_checksum_types_verify() {
    for (name, ty) in [("nochecksum", 0u8), ("crc32c", 1), ("xxhash", 2), ("xxhash64", 3), ("xxh3", 4)] {
        let rel = format!("sst-variants/checksum-{name}.sst");
        let mut s = open_sst(&rel);
        let f = s.footer.clone().unwrap();
        assert_eq!(f.checksum_type, ty, "{name}");
        assert_eq!(f.format_version, 6);
        if ty != 0 {
            let (st, c) = f.footer_checksum.unwrap();
            assert_eq!(st, c, "{name}: footer checksum");
        }
        let want = if ty == 0 { Status::NotPresent } else { Status::Present };
        let mut blocks = vec![f.metaindex, s.index_handle().unwrap()];
        for m in s.meta.clone().unwrap() {
            blocks.push(m.handle);
        }
        let (hs, _, _) = s.data_handles(0, 100_000, &never);
        blocks.extend(hs.into_iter().map(|x| x.0));
        assert!(blocks.len() > 5);
        for h in blocks {
            let b = s.block(h, BlockKind::Meta);
            assert!(b.error.is_none(), "{name} @{}: {:?}", h.offset, b.error);
            assert_eq!(b.checksum_status, want, "{name}: block @{}", h.offset);
            if ty != 0 {
                assert_eq!(b.stored_checksum.wrapping_sub(b.modifier), b.computed_checksum.unwrap());
            }
        }
        assert_eq!(s.prop_u64("rocksdb.num.entries"), Some(600));
    }
}

#[test]
fn sst_partitioned_index_resolves_to_data_blocks() {
    let mut s = open_sst("sst-variants/partitioned.sst");
    assert_eq!(s.index_type(), 2, "index type kTwoLevelIndexSearch");
    assert_eq!(s.index_kind(), BlockKind::TopIndex);
    let top = s.index_handle().unwrap();
    let (tb, res) = s.block_entries(top, BlockKind::TopIndex);
    assert_eq!(tb.checksum_status, Status::Present);
    let (_, tes, _) = res.unwrap();
    assert!(tes.len() >= 2, "expected >=2 partitions, got {}", tes.len());
    assert_eq!(s.prop_u64("rocksdb.index.partitions"), Some(tes.len() as u64));
    let mut total_data_blocks = 0usize;
    let mut total_entries = 0usize;
    for te in &tes {
        let (ph, _, _) = te.handle.clone().expect("top-level entry has partition handle");
        let (pb, pres) = s.block_entries(ph, BlockKind::IndexPartition);
        assert_eq!(pb.checksum_status, Status::Present, "partition @{}", ph.offset);
        let (_, pes, perr) = pres.unwrap();
        assert!(perr.is_none());
        assert!(!pes.is_empty());
        for pe in &pes {
            let (dh, _, _) = pe.handle.clone().expect("partition entry has data handle");
            let (db, dres) = s.block_entries(dh, BlockKind::Data);
            assert_eq!(db.checksum_status, Status::Present);
            let (_, des, _) = dres.unwrap();
            total_entries += des.len();
            total_data_blocks += 1;
        }
    }
    assert_eq!(total_entries, 600);
    assert_eq!(s.prop_u64("rocksdb.num.data.blocks"), Some(total_data_blocks as u64));
    let (hs, more, err) = s.data_handles(0, 100_000, &never);
    assert!(!more && err.is_none());
    assert_eq!(hs.len(), total_data_blocks);
    // tree: TopIndex children are IndexPartition blocks; their children are data blocks
    let kids = s.children(&NodeKind::SstBlock { off: top.offset, size: top.size, kind: BlockKind::TopIndex }, &never);
    let parts: Vec<_> = kids.iter().filter(|c| matches!(c.kind, NodeKind::SstBlock { kind: BlockKind::IndexPartition, .. })).collect();
    assert_eq!(parts.len(), tes.len());
    let gk = s.children(&parts[0].kind, &never);
    assert!(gk.iter().any(|c| matches!(c.kind, NodeKind::SstBlock { kind: BlockKind::Data, .. })));
}

#[test]
fn sst_rangedel_meta_block_exposed() {
    let mut s = open_sst("sst-variants/rangedel.sst");
    let h = s.meta_handle("rocksdb.range_del").expect("range_del meta block");
    assert_eq!(s.prop_u64("rocksdb.num.range-deletions"), Some(1));
    let (b, res) = s.block_entries(h, BlockKind::RangeDel);
    assert_eq!(b.checksum_status, Status::Present);
    let (_, es, _) = res.unwrap();
    assert_eq!(es.len(), 1);
    let (uk, _seq, ty) = rocksvision::manifest::split_ikey(&es[0].key).unwrap();
    assert_eq!(uk, b"b");
    assert_eq!(ty, 0xf, "kTypeRangeDeletion");
    assert_eq!(&b.data[es[0].value_off..es[0].value_off + es[0].value_len], b"n");
    let root = s.children(&NodeKind::Root, &never);
    let c = root.iter().find(|c| c.label.starts_with("Range deletions")).unwrap();
    assert_eq!(c.status, Status::Present);
    assert!(matches!(c.kind, NodeKind::SstBlock { kind: BlockKind::RangeDel, .. }));
    // 4 point entries; the file's num.entries property also counts the range tombstone (value read from the file: 5)
    let (hs, _, _) = s.data_handles(0, 10, &never);
    let (_, dres) = s.block_entries(hs[0].0, BlockKind::Data);
    assert_eq!(dres.unwrap().1.len(), 4);
    assert_eq!(s.prop_u64("rocksdb.num.entries"), Some(5));
    // a file without range deletions reports NotPresent for that child
    let mut other = open_sst("sst-variants/format-v6.sst");
    let root = other.children(&NodeKind::Root, &never);
    let c = root.iter().find(|c| c.label.starts_with("Range deletions")).unwrap();
    assert_eq!(c.status, Status::NotPresent);
}

#[test]
fn sst_properties_block_contents() {
    for rel in ["sst-none/data.sst", "sst-zstd/data.sst"] {
        let mut s = open_sst(rel);
        let ph = s.meta_handle("rocksdb.properties").expect("properties handle");
        let v = s.view(&NodeKind::SstBlock { off: ph.offset, size: ph.size, kind: BlockKind::Properties }, &never);
        let row = v.rows.iter().find(|r| r.field.value.starts_with("rocksdb.num.entries = ")).expect("num.entries row");
        assert_eq!(row.field.value, "rocksdb.num.entries = 2000");
        assert_eq!(row.field.status, Status::Present);
        assert_eq!(s.prop_u64("rocksdb.num.entries"), Some(2000));
        assert_eq!(s.prop_u64("rocksdb.format.version"), Some(6));
    }
}

#[test]
fn sst_misc_variants() {
    // mixed kinds: Put, Merge, Delete, Put with internal types 1,2,0,1
    let mut s = open_sst("sst-variants/mixed-kinds.sst");
    let (hs, _, _) = s.data_handles(0, 10, &never);
    let (_, res) = s.block_entries(hs[0].0, BlockKind::Data);
    let (_, es, _) = res.unwrap();
    let got: Vec<(Vec<u8>, u8)> = es
        .iter()
        .map(|e| {
            let (u, _, t) = rocksvision::manifest::split_ikey(&e.key).unwrap();
            (u.to_vec(), t)
        })
        .collect();
    assert_eq!(got, vec![(b"k-a".to_vec(), 1), (b"k-b".to_vec(), 2), (b"k-c".to_vec(), 0), (b"k-d".to_vec(), 1)]);
    // data-block hash index
    let mut s = open_sst("sst-variants/datablock-hash-index.sst");
    let (hs, _, _) = s.data_handles(0, 10, &never);
    let (_, res) = s.block_entries(hs[0].0, BlockKind::Data);
    let (lay, es, e) = res.unwrap();
    assert!(lay.hash_index && lay.hash.is_some(), "hash index not detected");
    assert!(e.is_none());
    assert_eq!(&es[0].key[..es[0].key.len() - 8], b"key-000000");
    // no-filter: filter reported NotPresent
    let mut s = open_sst("sst-variants/no-filter.sst");
    let root = s.children(&NodeKind::Root, &never);
    assert!(root.iter().any(|c| c.label.starts_with("Filter") && c.status == Status::NotPresent), "{:?}", root.iter().map(|c| (&c.label, c.status)).collect::<Vec<_>>());
    let mut s = open_sst("sst-variants/format-v6.sst");
    let root = s.children(&NodeKind::Root, &never);
    assert!(root.iter().any(|c| c.label.contains("Filter") && c.status == Status::Present));
}

#[test]
fn sst_decompress_limits_and_errors() {
    assert_eq!(compress::MAX_DECOMPRESSED, 64 * 1024 * 1024);
    // declared size above the cap -> Limit, no allocation
    for t in [2u8, 3, 4, 5, 7] {
        let mut p = vec![];
        let mut v = 1u64 << 40;
        while v >= 0x80 {
            p.push((v as u8) | 0x80);
            v >>= 7;
        }
        p.push(v as u8);
        p.extend_from_slice(&[0u8; 16]);
        match compress::decompress(t, &p, 2, None) {
            Err(compress::DecompError::Limit(_)) => {}
            o => panic!("type {t}: expected Limit, got {o:?}"),
        }
    }
    let mut p = vec![0x80, 0x80, 0x80, 0x80, 0x08]; // snappy header: 2^31
    p.extend_from_slice(&[0u8; 8]);
    assert!(matches!(compress::decompress(1, &p, 2, None), Err(compress::DecompError::Limit(_))));
    // real bomb: zstd stream of 70 MiB zeros behind a *small* declared size must fail, not expand
    let big = vec![0u8; 70 * 1024 * 1024];
    let z = zstd::bulk::compress(&big, 1).unwrap();
    let mut p = vec![100u8];
    p.extend_from_slice(&z);
    assert!(compress::decompress(7, &p, 2, None).is_err());
    assert!(matches!(compress::decompress(6, &[1, 2, 3], 2, None), Err(compress::DecompError::Unsupported(_))));
    assert!(matches!(compress::decompress(0x55, &[1, 2, 3], 2, None), Err(compress::DecompError::Unsupported(_))));
}

#[test]
fn sst_footer_decode_edge_cases() {
    assert!(footer::decode(&[0u8; 10], 10).is_err());
    assert!(footer::decode(&[0u8; 53], 53).is_err());
    let mut i = inspect::open(&fx("corrupt/sst-bad-footer-magic.sst")).unwrap();
    let kids = i.children(&NodeKind::Root, &never);
    assert!(kids.iter().any(|c| c.kind == NodeKind::SstFooter && c.status == Status::Invalid));
}
