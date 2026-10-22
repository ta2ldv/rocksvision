# RocksVision public examples

Ready-to-open, committed RocksDB data. No generation step and no RocksDB installation is needed to use it: open the files with RocksVision.

```
examples/
  data/<scenario>/    complete, self-contained RocksDB database directories (18)   -> Open Folder
  files/<scenario>/   ONE standalone file per directory (18; valid and corrupt)    -> Open File
  SHA256SUMS          SHA-256 of every file under data/ and files/ (relative to examples/)
  prior-work/         older hand notes, not part of this dataset
```

Total size of `data/` + `files/`: 420,170 bytes in 136 files (about 410 KiB). Mostly OPTIONS files (7-17 KB each) and the 44 KB fragmented WAL.

## Provenance, regeneration, hashes

- Produced by real RocksDB 10.4.2 (Rust crate `rocksdb` 0.24.0 / `librocksdb-sys` 0.17.3+10.4.2) through the pinned generator `tests/fixture-gen` (subcommand `examples`). Nothing is hand-written binary. The only files modified after the engine wrote them are the clearly named `corrupt-*` copies (exact mutations below).
- Regenerate (developer only): `cd tests/fixture-gen && LIBCLANG_PATH=/Library/Developer/CommandLineTools/usr/lib DYLD_FALLBACK_LIBRARY_PATH=/Library/Developer/CommandLineTools/usr/lib cargo build --release && ./target/release/fixture-gen examples ../../examples` (rewrites `data/`, `files/` and `SHA256SUMS`; leaves `README.md` and `prior-work/` alone).
- **Byte-identical regeneration is NOT guaranteed.** DB id, session id, creation times and OPTIONS contents vary between runs, so regenerated files get different hashes. The committed files plus `SHA256SUMS` are the reference. Check them with `cd examples && shasum -a 256 -c SHA256SUMS`.
- Keys, values, sequence numbers, file numbers, record layout and level layout are deterministic and are documented below.
- Snapshots: every `data/` directory except `wal-recycled` was copied after a **clean close** of the DB (LOCK and LOG removed). WAL data that was meant to stay un-flushed was left in the WAL by closing without a flush (RocksDB does not flush on close when the WAL is enabled), so the WAL is retained and consistent with the MANIFEST. `wal-recycled` is the one exception: it is a crash-consistent copy taken while the DB was still open (a clean close would truncate the recycled WAL and remove the stale tail that makes it interesting).
- Common options (all SST scenarios): `create_if_missing`, `disable_auto_compactions`, 64 MiB write buffer (nothing flushes unless the scenario flushes), merge operator `concat`, `block_size` 256 B (instead of the 4 KiB default, so 48 records already give 16 data blocks), format_version 6 (the 10.4.2 default), checksum XXH3 (default). Compression is set to **None explicitly** everywhere except the `codec-*` scenarios, because the Rust crate's `Options::default()` would otherwise silently choose Snappy.
- Expected-content, engine and RocksVision checks are listed in [Verification](#verification).

## Two concepts that are easy to mix up

**LSM levels vs the two-level (partitioned) index.** These are unrelated. LSM *levels* (L0, L1, ...) describe which SST *file* sits where in the database: the MANIFEST records each live file with its level, and a standalone SST cannot tell its own level. A *two-level index* (`kTwoLevelIndexSearch`, "partitioned index") is a structure *inside one SST file*: a top-level index block points at several index partition blocks, which point at the data blocks. A database with a single L0 file can still use a two-level index (`index-partitioned`), and a file on L6 can use an ordinary single index block. Compare `multilevel-db` (levels, many files) with `index-partitioned` (one file, two index levels).

**What is mandatory in a block-based SST.** An index block is mandatory in every block-based SST: it is how a reader finds the data block for a key, so `bloom-absent` still has its index (`Index block (kBinarySearch)`). Everything else shown here is optional and can be absent: the Bloom filter (`bloom-absent`), partitioning of index/filter, the data-block hash index (`index-hash` has it, every other scenario does not), range deletions and the compression dictionary. No valid index-less block-based SST was fabricated; where "absent" is shown, it is an absent *optional* feature.

## How to open

- **Open Folder**: choose `examples/data/<scenario>`; the left list shows CURRENT, MANIFEST-*, *.log, *.sst, OPTIONS, IDENTITY. Select CURRENT to see "Observed levels (via MANIFEST)".
- **Open File**: choose a file under `examples/files/<scenario>/` (each is alone in its directory, so there is no CURRENT, MANIFEST or sibling to find: level, column family name and WAL context are shown as unknown rather than invented). Any file under `data/` can also be opened this way.
- **Path + Go**: paste the absolute path of a scenario directory or file into the path field and press Go.
- Tree labels quoted below are from the frozen binary (SHA-256 `f72227dc...`); wording in later builds may differ slightly.

## Shared logical dataset (bloom-*, index-*, codec-*)

48 Puts into the default column family, one write each, then one `flush()`, clean close: keys `key-000` .. `key-047`, value `value-NNN:` + 3 x `The quick brown fox jumps. ` (91 bytes), sequence numbers 1..48, **one SST** `000009.sst` on L0 with 16 data blocks of 3 entries (each block is 323 B of contents; 328 B on disk uncompressed including the 5-byte trailer). The directory also holds `000008.log` (**0 bytes**: the empty WAL RocksDB opens at flush; this is a real engine state and exercises the empty-WAL case), `MANIFEST-000005`, `CURRENT`, `OPTIONS-000007`, `IDENTITY`. Reopened read-only: 48 keys in `default`. Only the table options differ between the scenarios below.

---

# data/ scenarios

## basic-db
- **Purpose**: the smallest complete database: one SST (flushed data) next to a retained WAL (unflushed data), plus MANIFEST/CURRENT/OPTIONS/IDENTITY.
- **Generated**: default CF, full Bloom (10 bits/key), block_size 256, no compression. 12 Puts `fruit:apple` .. `fruit:orange` (`fruit:apple`,`apricot`,`banana`,`blueberry`,`cherry`,`date`,`fig`,`grape`,`kiwi`,`lemon`,`mango`,`orange` = `sweet-00`..`sweet-11`), `flush()`, then 3 Puts `veg:carrot`=orange, `veg:leek`=green, `veg:beet`=red, clean close without flush.
- **Contents**: SST `000009.sst` (1618 B, L0, 12 entries, seq 1..12, 2 data blocks); WAL `000008.log` (106 B, 3 FULL records, seq 13..15); MANIFEST-000005 (234 B); last sequence 15. Reopened: 15 keys in `default`.
- **Open**: Open Folder `data/basic-db`, then CURRENT / MANIFEST-000005 / 000008.log / 000009.sst.
- **Observe**: CURRENT -> referenced MANIFEST Present, Observed levels: `0 default | L0 | 000009 | fruit:apple .. fruit:orange | seq 1..12 | on disk`; WAL: three WriteBatch records with the veg keys; SST: Filter block Present, Compression NoCompression. `files/manifest-basic` and `files/current-basic` are copies of its MANIFEST-000005 and CURRENT.
<details><summary>SHA-256 of the 6 file(s)</summary>

```
0c878124ec39ad0a676f8c3b6e2d7e9cdd95218939274fbc1bd5b43cb7423035  data/basic-db/000008.log  (106 B)
2b1b6a079789f4b25c48186b7d4502d82eba0423b677569c2c7152bedf63f599  data/basic-db/000009.sst  (1618 B)
9c283f6e81028b9eb0760d918ee4bc0aa256ed3b926393c1734c760c4bd724fd  data/basic-db/CURRENT  (16 B)
5461dcc394e223f6986f1a132eb7ec12d2128f02c261e1a635afdb7fceee7d14  data/basic-db/IDENTITY  (36 B)
f809b912cd6e7e561b64700e79cdf696ff57c27c6cb5ef9eda6f9b7c1ff11c97  data/basic-db/MANIFEST-000005  (234 B)
00a2962ab0f90a4248e30a200d7795a253f557eaca1a69cd6350aa601f24e5d3  data/basic-db/OPTIONS-000007  (7726 B)
```

</details>

## bloom-full / bloom-partitioned / bloom-absent
- **Purpose**: the same data with a full Bloom filter, a partitioned Bloom filter, and no filter.
- **Generated**: shared dataset. `bloom-full`: `set_bloom_filter(10, false)`. `bloom-partitioned`: Bloom 10 + `partition_filters=true` + `index_type=TwoLevelIndexSearch` (RocksDB requires the two-level index for partitioned filters) + `metadata_block_size=128`. `bloom-absent`: no filter policy.
- **Observe in the SST tree** (dump-verified with the frozen binary):
  - `bloom-full`: `Filter block (fullfilter.rocksdb.BuiltinBloomFilter)` Present; property `filter policy` = `bloomfilter`; `Index block (kBinarySearch)`. 6777 B.
  - `bloom-partitioned`: `Partitioned filter index (partitionedfilter.rocksdb.BuiltinBloomFilter)` Present with 3 `Filter partition` blocks; index is `Top-level index (partitioned)` with 3 `Index partition` blocks. 7101 B.
  - `bloom-absent`: tree shows `Filter: not present` (Not present), property `filter policy` = "property not stored"; the index is still there. 6636 B.
- **Note**: `bloom-partitioned` and `index-partitioned` are generated with identical options (two entry points, one for each learning path); they differ only in DB id/timestamps.
<details><summary>SHA-256 of the 6 file(s)</summary>

```
e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855  data/bloom-full/000008.log  (0 B)
a6d17ddd47df30085785a64a9d181022c46c69803c0edc309f8d97c2628e7b08  data/bloom-full/000009.sst  (6777 B)
9c283f6e81028b9eb0760d918ee4bc0aa256ed3b926393c1734c760c4bd724fd  data/bloom-full/CURRENT  (16 B)
001f43061563b7af368f8d79b4749f5a45b3ebc84d2a117059dc322d2b1a2170  data/bloom-full/IDENTITY  (36 B)
a640cc24657a3f95d52b0879c2d0cf49ed7ceb6787bac9387049f9ad24471105  data/bloom-full/MANIFEST-000005  (225 B)
00a2962ab0f90a4248e30a200d7795a253f557eaca1a69cd6350aa601f24e5d3  data/bloom-full/OPTIONS-000007  (7726 B)
```

</details>
<details><summary>SHA-256 of the 6 file(s)</summary>

```
e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855  data/bloom-partitioned/000008.log  (0 B)
679afc801e99073ae9430b97517a3df5de7650fbbf2c811043a1e391f0872c7a  data/bloom-partitioned/000009.sst  (7101 B)
9c283f6e81028b9eb0760d918ee4bc0aa256ed3b926393c1734c760c4bd724fd  data/bloom-partitioned/CURRENT  (16 B)
5e47b532c59cb0733b9962170b68168181753e049eb698a9dc9b8896f4bdf38f  data/bloom-partitioned/IDENTITY  (36 B)
d6eb941f4dedade263f916f056737a271a1aa6825b8f4085f6d15a5d16f9e813  data/bloom-partitioned/MANIFEST-000005  (225 B)
9b21f3704fdc2a605146d8f1818e99159895db924c54f6a32e9e48fcb30f5faf  data/bloom-partitioned/OPTIONS-000007  (7731 B)
```

</details>
<details><summary>SHA-256 of the 6 file(s)</summary>

```
e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855  data/bloom-absent/000008.log  (0 B)
b0101835ea05056f4aa882841b94709a643ab9315947b4423adf74363006b535  data/bloom-absent/000009.sst  (6636 B)
9c283f6e81028b9eb0760d918ee4bc0aa256ed3b926393c1734c760c4bd724fd  data/bloom-absent/CURRENT  (16 B)
8a5e1659de02ce857f57e676226cac747f2a50d8996899aba820a73f7b68f020  data/bloom-absent/IDENTITY  (36 B)
2a922f6fa2e70813f48e1bb4810dd087be6bca2faf6982a2a70c882ab1c30443  data/bloom-absent/MANIFEST-000005  (225 B)
3176d4f28924edeeb4ded0f2192e1c96342c00acb83f1218b86edea4d3625149  data/bloom-absent/OPTIONS-000007  (7722 B)
```

</details>

## index-binary / index-partitioned / index-hash
- **Purpose**: ordinary index block, two-level index, and the optional data-block hash index.
- **Generated**: shared dataset. `index-binary`: defaults (single `kBinarySearch` index) + full Bloom. `index-partitioned`: `TwoLevelIndexSearch` + `partition_filters` + Bloom 10 + `metadata_block_size=128`. `index-hash`: `data_block_index_type=BinaryAndHash`, `data_block_hash_ratio=0.75` + full Bloom.
- **Observe**:
  - `index-binary` (6777 B): `Index block (kBinarySearch)`, one index block with 16 entries; no hash index (each data block: `data-block hash index: not present (footer MSB clear)`).
  - `index-partitioned` (7101 B): property `index type` = `2 kTwoLevelIndexSearch (partitioned)`; `Top-level index (partitioned)` -> 3 `Index partition` blocks -> 16 data blocks; filter side `Partitioned filter index` -> 3 `Filter partition` blocks.
  - `index-hash` (6889 B): still `kBinarySearch` index, and every one of the 16 data blocks reports `data-block hash index present` (3 entries, 1 restart point). The index block is mandatory; the hash index is the optional extra.
<details><summary>SHA-256 of the 6 file(s)</summary>

```
e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855  data/index-binary/000008.log  (0 B)
a4659358a25391780ff6054ab2002f61c7d8632664dd1027e6b63bec2f441a2c  data/index-binary/000009.sst  (6777 B)
9c283f6e81028b9eb0760d918ee4bc0aa256ed3b926393c1734c760c4bd724fd  data/index-binary/CURRENT  (16 B)
25af1f1618dbc37fd3d7368d227bf464043bdaeeeeb1255f3469397e21f34808  data/index-binary/IDENTITY  (36 B)
35e6d0f0aea68a0ccc644503eacad945a6f23e88b8c1e5760fa8c732b0ec1f45  data/index-binary/MANIFEST-000005  (225 B)
00a2962ab0f90a4248e30a200d7795a253f557eaca1a69cd6350aa601f24e5d3  data/index-binary/OPTIONS-000007  (7726 B)
```

</details>
<details><summary>SHA-256 of the 6 file(s)</summary>

```
e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855  data/index-partitioned/000008.log  (0 B)
265ad70de0e305d51d7cfc03ac5d3405c0b91b807a7a4de3b5b2dc853f17c48a  data/index-partitioned/000009.sst  (7101 B)
9c283f6e81028b9eb0760d918ee4bc0aa256ed3b926393c1734c760c4bd724fd  data/index-partitioned/CURRENT  (16 B)
ed76dfb3e6e3870354a34be9857d437aa93f2480d632cc2faf437cc71a09ba15  data/index-partitioned/IDENTITY  (36 B)
e99577a04e501944458d651542f51f9922999fd1322d85aa7018bc3a05de1aac  data/index-partitioned/MANIFEST-000005  (225 B)
9b21f3704fdc2a605146d8f1818e99159895db924c54f6a32e9e48fcb30f5faf  data/index-partitioned/OPTIONS-000007  (7731 B)
```

</details>
<details><summary>SHA-256 of the 6 file(s)</summary>

```
e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855  data/index-hash/000008.log  (0 B)
b24116854e34900780a4e5224c5b7c15719d436433b17dbda891714b85343543  data/index-hash/000009.sst  (6889 B)
9c283f6e81028b9eb0760d918ee4bc0aa256ed3b926393c1734c760c4bd724fd  data/index-hash/CURRENT  (16 B)
4ec3c39f6f224a1354d7b3616b557d23c8692e80b8b37acc711ffbc4c1598afb  data/index-hash/IDENTITY  (36 B)
9cfff1be7e59dac6c0a15fe641ee222ce72123a1b183a57fec82ca1b0be63396  data/index-hash/MANIFEST-000005  (225 B)
485925299af1499ef85d2cee7618bafce387b88cb2063740739c78d1b2a58919  data/index-hash/OPTIONS-000007  (7727 B)
```

</details>

## codec-none / codec-snappy / codec-zlib / codec-bzip2 / codec-lz4 / codec-lz4hc / codec-zstd
- **Purpose**: the same data stored with each supported compression codec, to compare stored vs decompressed sizes and see the per-block compression type.
- **Generated**: shared dataset, `set_compression_type(<codec>)`, full Bloom, flush to L0 (flush output uses the column family `compression` setting).
- **Verified actual stored type** (from each block trailer in the dump, not from options): all 16 data blocks AND the index block carry the codec; metaindex, filter and properties blocks are always stored uncompressed. LZ4 and LZ4HC are two codec ids in 10.4.2 (trailer byte 4 = LZ4, 5 = LZ4HC); their block payload format is the same LZ4 format but **they do not share the on-disk id** (this corrects the earlier assumption that they do).

| scenario | trailer id/name | data block 0 stored -> contents | SST bytes |
|---|---|---|---|
| codec-none | 0 NoCompression | 323 -> 323 | 6777 |
| codec-snappy | 1 Snappy | 101 -> 323 | 3148 |
| codec-zlib | 2 Zlib | 81 -> 323 | 2775 |
| codec-bzip2 | 3 BZip2 | 140 -> 323 | 3829 |
| codec-lz4 | 4 LZ4 | 97 -> 323 | 3072 |
| codec-lz4hc | 5 LZ4HC | 93 -> 323 | 3009 |
| codec-zstd | 7 ZSTD | 99 -> 323 | 3098 |

- **Observe**: property `compression` ("Snappy", "ZSTD", ...); `Data block @0 (101 B)` (stored size) and, expanded, `101 B stored, 323 B contents` with block-local (decompressed) offsets; Trailer `compression type` row. `files/sst-<codec>` are byte-copies of these SSTs.
<details><summary>SHA-256 of the 6 file(s)</summary>

```
e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855  data/codec-none/000008.log  (0 B)
3b3ebd0c83b0ed40c1f9678a9b94508c87fdeffabc6a2cadad752821f08a4dab  data/codec-none/000009.sst  (6777 B)
9c283f6e81028b9eb0760d918ee4bc0aa256ed3b926393c1734c760c4bd724fd  data/codec-none/CURRENT  (16 B)
1214791c7dd601fe2b73645a19f1b284244b6ae52bd8ae0f5e58508a3179bfe1  data/codec-none/IDENTITY  (36 B)
a83bcf28196dbae598bc97b3bf9835734281116a262bb70930b87db53008a289  data/codec-none/MANIFEST-000005  (225 B)
00a2962ab0f90a4248e30a200d7795a253f557eaca1a69cd6350aa601f24e5d3  data/codec-none/OPTIONS-000007  (7726 B)
```

</details>
<details><summary>SHA-256 of the 6 file(s)</summary>

```
e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855  data/codec-snappy/000008.log  (0 B)
36768e7100afb29ba87461f698d57ff5b0a3eba1df3e0475748bd77c8de4b2ac  data/codec-snappy/000009.sst  (3148 B)
9c283f6e81028b9eb0760d918ee4bc0aa256ed3b926393c1734c760c4bd724fd  data/codec-snappy/CURRENT  (16 B)
05f2cb9a78a0e355b996b8e00482e98c199f54205ceab9e11578cef1691b582c  data/codec-snappy/IDENTITY  (36 B)
433e6af7c031bab3d782eb1d987f9021a32f3b4b19be5b45003d443ac67cf719  data/codec-snappy/MANIFEST-000005  (225 B)
c494434e3bb5f5a3c35f8c65b22150623cefa28529aa6b79a25a7405c6a90116  data/codec-snappy/OPTIONS-000007  (7730 B)
```

</details>
<details><summary>SHA-256 of the 6 file(s)</summary>

```
e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855  data/codec-zlib/000008.log  (0 B)
ad291495e8c45158247a6e4fb6e540c2eedb4a927e0360c73c11d5b43ec03af7  data/codec-zlib/000009.sst  (2775 B)
9c283f6e81028b9eb0760d918ee4bc0aa256ed3b926393c1734c760c4bd724fd  data/codec-zlib/CURRENT  (16 B)
eca87bad9c72d0e787de158ecd2e2ffdab8c7b1cc7d7b82ac73d4e6f1622f648  data/codec-zlib/IDENTITY  (36 B)
57e4d4eebcec5227992b347b2e418f98ff4275fa529637fb6c55b5390d5328df  data/codec-zlib/MANIFEST-000005  (225 B)
1e680d867b23b5c49f767d853d1d51794a5069a6f5fd1feed3954283b1e4b6d4  data/codec-zlib/OPTIONS-000007  (7728 B)
```

</details>
<details><summary>SHA-256 of the 6 file(s)</summary>

```
e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855  data/codec-bzip2/000008.log  (0 B)
f924f965d2228db41f08b926423b8c41dbd54e5f2d396bcec248ba644f30e5dd  data/codec-bzip2/000009.sst  (3829 B)
9c283f6e81028b9eb0760d918ee4bc0aa256ed3b926393c1734c760c4bd724fd  data/codec-bzip2/CURRENT  (16 B)
8cf43bdd37092eb710f17be55ebd94ddaaa7e94733d90743d8e60d3d51e2af73  data/codec-bzip2/IDENTITY  (36 B)
89dc9bec4d87377eae29f98614f49dcd29ba32d2987ed7ca36a1334fb40723ed  data/codec-bzip2/MANIFEST-000005  (225 B)
b0891cd0561b60bbdb71cf349ae80655b9ee8cc477bce4f4d350ac4e55f35760  data/codec-bzip2/OPTIONS-000007  (7729 B)
```

</details>
<details><summary>SHA-256 of the 6 file(s)</summary>

```
e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855  data/codec-lz4/000008.log  (0 B)
a69fc45c9f0822ae7a26d2f880357ec208a97c737a8a40a5c46365b6fc10016e  data/codec-lz4/000009.sst  (3072 B)
9c283f6e81028b9eb0760d918ee4bc0aa256ed3b926393c1734c760c4bd724fd  data/codec-lz4/CURRENT  (16 B)
6c3e3c13fc30d3f79047e2526e16999a605cf2c8bb08ff00100603c2237eabbb  data/codec-lz4/IDENTITY  (36 B)
4df3430e5e33bb9d56b7b77e9ed492b4fa24487935e73d1571ad7be6b2ab15f4  data/codec-lz4/MANIFEST-000005  (225 B)
fc67e4535ad18965002d8b75534801aeda4f0d74e5f1d416e3d22dd98df5afb1  data/codec-lz4/OPTIONS-000007  (7727 B)
```

</details>
<details><summary>SHA-256 of the 6 file(s)</summary>

```
e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855  data/codec-lz4hc/000008.log  (0 B)
062f69529823cfca06633310170fae8176af54e012678b57eb2a375c5ecdf73d  data/codec-lz4hc/000009.sst  (3009 B)
9c283f6e81028b9eb0760d918ee4bc0aa256ed3b926393c1734c760c4bd724fd  data/codec-lz4hc/CURRENT  (16 B)
dea90beb67667191044812a1e357be0144b4c8f5dafc9116a314e8da43e168cc  data/codec-lz4hc/IDENTITY  (36 B)
ff84627db0ae5d87ea95e6c7b60d52c90b35aac2b07c7e5ac6428c204b09344f  data/codec-lz4hc/MANIFEST-000005  (225 B)
2975f7f58f3e2204e3f7ca831747fc85e11a876c5e1c40b43f3232bf8bd2f304  data/codec-lz4hc/OPTIONS-000007  (7729 B)
```

</details>
<details><summary>SHA-256 of the 6 file(s)</summary>

```
e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855  data/codec-zstd/000008.log  (0 B)
6ffb3ac06df72c667c908ebcddf2b572133625c4e54fb28269a2dc98f8c111a5  data/codec-zstd/000009.sst  (3098 B)
9c283f6e81028b9eb0760d918ee4bc0aa256ed3b926393c1734c760c4bd724fd  data/codec-zstd/CURRENT  (16 B)
a1bfa88e8fbbdbbe01dcfeac895474ecbeeac20292c34fdc4e2bccea0ea918e1  data/codec-zstd/IDENTITY  (36 B)
b3f56a1c1d6fce8fe3d8a82035e910f76c29a19d98c131a96370cf06047223e9  data/codec-zstd/MANIFEST-000005  (225 B)
77a3e53453c1932c3b312ea942636255edd269c87f026bfef978d34f038a8292  data/codec-zstd/OPTIONS-000007  (7717 B)
```

</details>

## multilevel-db
- **Purpose**: a complete small database with 3 column families and SST files on L0 **and** L1, plus a retained WAL.
- **Generated**: `level_compaction_dynamic_level_bytes=false`, `num_levels=7`, auto compactions disabled, block_size 256, Bloom 10, no compression. Per CF (values `<tag>-NNN-` + 4 x `payload-`):
  - `default`: Puts d-000..d-019, flush; Puts d-010..d-029 + Delete d-005, flush; `compact_range` (merged into one L1 file; the tombstone is dropped at the bottom level); Puts d-025..d-034, flush (new L0 file overlapping the L1 file).
  - `cf_users`: Puts u-000..u-014, flush; Puts u-010..u-024, flush; never compacted (two overlapping L0 files).
  - `cf_events`: Puts e-000..e-014, flush; Puts e-015..e-029, flush; `compact_range` (the two disjoint files are moved to L1, two files); Delete e-003, DeleteRange [e-010, e-014), Put e-040, flush (L0 file with tombstones).
  - Finally 3 Puts `d-w0..d-w2` (`wal-only-N`) to `default` left only in the WAL (`000023.log`, 108 B, 3 FULL records). Last sequence 117.
- **Level layout, engine evidence** (`DB::live_files()` / `rocksdb.levelstats`, at generation and again after a read-only reopen) **and MANIFEST evidence** (frozen binary, MANIFEST-000005 "Observed levels", 25 edits replayed, 11 additions, 4 deletions) agree:

| CF | level | file | size | smallest .. largest | seq range |
|---|---|---|---|---|---|
| default | L0 | 000014 | 1826 | d-025 .. d-034 | 42..51 |
| default | L1 | 000012 | 2922 | d-000 .. d-029 | 0..0 |
| cf_users | L0 | 000016 | 2114 | u-000 .. u-014 | 52..66 |
| cf_users | L0 | 000018 | 2114 | u-010 .. u-024 | 67..81 |
| cf_events | L0 | 000024 | 1406 | e-003 .. e-040 | 112..114 |
| cf_events | L1 | 000020 | 2115 | e-000 .. e-014 | 82..96 |
| cf_events | L1 | 000022 | 2115 | e-015 .. e-029 | 97..111 |

  levelstats: default L0 1 / L1 1; cf_users L0 2; cf_events L0 1 / L1 2. (Levels come from the MANIFEST, not from file names: file numbers are not ordered by level.)
- **Expected content** (read-only reopen): `default` 37 keys (d-000..d-034 minus d-005 = 34, plus 3 WAL-only), `cf_users` 25, `cf_events` 26 (30 - e-003 - 4 range-deleted + e-040).
- **Open**: Open Folder `data/multilevel-db`; select CURRENT (or MANIFEST-000005) -> "Observed levels": 7 rows, L0 and L1 files in 3 CFs.
<details><summary>SHA-256 of the 12 file(s)</summary>

```
e589191ed06e108d91ef41c1b23df208031b6b38b73a294d29851930e70b79b9  data/multilevel-db/000012.sst  (2922 B)
83ab8765b5ee3dc99bc6b6bc1da8c10e7a5fbe64b1c09da34fd77b78c458d4bc  data/multilevel-db/000014.sst  (1826 B)
6b2775281999aef1b2c200d9b41676e117174a542bd4b58f811afb8366a6bcf8  data/multilevel-db/000016.sst  (2114 B)
0dd5f36f60baf082cc6ee70c2a02e6d503929df2c832538ad65b85d6cac99c3b  data/multilevel-db/000018.sst  (2114 B)
cdf622a058fe4c800e92d3f2c32ba81834503ebd9823c58df96c72a3f6218734  data/multilevel-db/000020.sst  (2115 B)
087e3b95fbc2313a19b35d48ce068253bc9116b37f330e0e536ec2dbe912a1d1  data/multilevel-db/000022.sst  (2115 B)
37ccc5c8a043686cee666d63506ef2c4d6f5982739f7f4856885d0337da64a9e  data/multilevel-db/000023.log  (108 B)
85b2919ddecbf8ee33f42480aa98ccd0d9307640d7f0bc9d37fa36d0d0eedf61  data/multilevel-db/000024.sst  (1406 B)
9c283f6e81028b9eb0760d918ee4bc0aa256ed3b926393c1734c760c4bd724fd  data/multilevel-db/CURRENT  (16 B)
50dfde0a07cfafe2ba2844d261c841e650849007a03358e3c22be271fe967f13  data/multilevel-db/IDENTITY  (36 B)
c6c5a711a21b286c9598323f0de784110e9c0550277412b938f56ee7ad052ded  data/multilevel-db/MANIFEST-000005  (1356 B)
1839902f1ad7a544f9323d76d9b46fee771bdeeac69590756a6219fc009126fb  data/multilevel-db/OPTIONS-000007  (17339 B)
```

</details>

## wal-basic
- **Purpose**: a WAL-only database showing every record kind in a legal order across 2 CFs. No SST at all.
- **Generated**: `write_buffer_size` 256 MiB, merge operator `concat`, legacy 7-byte WAL headers (`recycle_log_file_num=0`), CFs `default` (id 0) and `cf_two` (id 1). Clean close without flush. `000004.log`, 443 B, 14 FULL records (offsets 0, 30, 64, 100, 127, 154, 179, 209, 239, 270, 301, 332, 369, 401):

| record | seq | operation |
|---|---|---|
| 0 | 1 | default Put apple=red |
| 1 | 2 | default Put banana=yellow |
| 2 | 3 | default Put cherry=dark-red |
| 3 | 4 | default Delete banana |
| 4 | 5 | default Put once=1 (written exactly once, as SingleDelete requires) |
| 5 | 6 | default SingleDelete once |
| 6, 7 | 7, 8 | default Merge counter <- a ; Merge counter <- b |
| 8, 9, 10 | 9, 10, 11 | cf_two Put range-a=1 ; range-b=2 ; range-c=3 |
| 11 | 12 | cf_two DeleteRange [range-a, range-c) (end exclusive: range-c survives) |
| 12 | 13 | cf_two Put note=hello |
| 13 | 14..15 | ONE WriteBatch across both CFs: default Put zeta=last + cf_two Put omega=end |

- **Expected content** (replayed by the engine on a read-only reopen): `default` apple=red, cherry=dark-red, counter=`a,b`, zeta=last (4 keys; banana deleted, once SingleDeleted); `cf_two` note=hello, omega=end, range-c=3 (3 keys). Last sequence 15.
- **Observe**: WAL tree -> Logical records with `Value (Put)`, `Deletion`, `SingleDeletion`, `Merge`, `ColumnFamilyValue (Put)` with cf 1, range deletion in cf 1; record 13 has two ops with cf 0 and cf 1. `files/wal-plain/000004.log` is a copy of this WAL.
<details><summary>SHA-256 of the 5 file(s)</summary>

```
9d332f589ab493429e6469552e45adc45e9e7e5dc7e5cf53109283bdb37e028a  data/wal-basic/000004.log  (443 B)
9c283f6e81028b9eb0760d918ee4bc0aa256ed3b926393c1734c760c4bd724fd  data/wal-basic/CURRENT  (16 B)
41ca57af84c41de28a9215821e03ac66c2a72e1098aa271b8754b257672e8efa  data/wal-basic/IDENTITY  (36 B)
56bbe091f5bc44d9ec7d4aa1ef6db2203196411b9ad03e8acedc41574756faf9  data/wal-basic/MANIFEST-000005  (173 B)
ede79745b2c41dde471c089364f334cf1fb4ac302943c5e3a03489b66413f1df  data/wal-basic/OPTIONS-000007  (12530 B)
```

</details>

## wal-fragmented
- **Purpose**: a WAL record that crosses a 32 KiB physical block (FIRST ... LAST fragments).
- **Generated**: wal-basic options, default CF only. 30 small Puts `small-000..029` (`small-value-NNN-` + 24 x `x`), then Put `big-40k` = 40960 bytes (byte[i] = '0' + (i mod 10)), then 20 small Puts `small-030..049`. No flush. **Minimum size**: the WAL block is 32768 B, so a fragmented record needs a single value larger than about 32 KiB; this one (40 KiB) is the smallest round value used, giving a 44,548 B WAL.
- **Layout**: 52 physical records: 50 FULL (payload 64 B each), `FIRST` at 0x852 (payload 30631, filling block 0 to its end), `LAST` at 0x8000 (payload 10353), no MIDDLE. 51 keys, last sequence 51.
- **Observe**: Physical blocks -> Block 0 ends with a FIRST fragment, Block 1 starts with the LAST fragment; Logical record 30 is reassembled from 2 fragments (`1 frag` for the others). `files/wal-fragmented/000004.log` is a copy.
<details><summary>SHA-256 of the 5 file(s)</summary>

```
953aba0fb9c1ad49e528a8df49c6358f554352aebf19fe52c3d5b6d137c82938  data/wal-fragmented/000004.log  (44548 B)
9c283f6e81028b9eb0760d918ee4bc0aa256ed3b926393c1734c760c4bd724fd  data/wal-fragmented/CURRENT  (16 B)
1d22fb6f139b18f25f449c2d76c34a3b78d8eefd4b86d65fc6164551dbef4086  data/wal-fragmented/IDENTITY  (36 B)
5903ea1f1de442fa595fc4924964155bac1c55f06aaaca5a292b15951ef1ceac  data/wal-fragmented/MANIFEST-000005  (116 B)
3ec02f31b86eaf7bb92001d0722d4b029f79606d614c2834ffa67c86d12da561  data/wal-fragmented/OPTIONS-000007  (7728 B)
```

</details>

## wal-recycled
- **Purpose**: a recycled WAL (`recycle_log_file_num=2`): every record uses the 11-byte recyclable header carrying the log number, and the live WAL is a renamed older file, so it ends with stale bytes from its previous owner.
- **Generated**: wal-basic options + `recycle_log_file_num=2`, CFs `default`, `cf_two`. Rounds: r1 (6 puts `r1-NNN`), r2 (50 puts), r3 (4 puts), each followed by a flush of both CFs, then r4 (6 puts) left unflushed; value = `<tag>-value-NNN-` + 180 x `y`; plus one `<tag>-c2`=`c2` in `cf_two` per round. **Snapshot copied while the DB was open** (see Provenance). Last sequence 70.
- **Contents**: 6 L0 SSTs (3 per CF), `000011.log` (938 B, obsolete r3 log, 5 records, log number 11), `000014.log` (11,334 B, the live WAL, log number 14: 7 valid records [6 r4 puts + `r4-c2`] up to offset 0x56e, then stale bytes of the previous owner, log number 8, from r2). Reopened read-only: `default` 66 keys, `cf_two` 4 keys (`r4-c2` exists only in the WAL).
- **Observe**: record headers `recyclable 11 B` with Log # 14; at 0x56e an `Invalid` "BadLength" entry (the engine's end-of-log condition: the bytes there are the middle of an old record), followed by CRC-valid records of log 8 shown as `Unknown` with "stale recycled record: log number 8 != 14". **This Invalid/Unknown tail is expected and is not file corruption**. `files/wal-recycled/000014.log` is a copy.
<details><summary>SHA-256 of the 12 file(s)</summary>

```
89a0359425074a8a1a7c27bd8e761d5ef0b4ddf9e35498043c555c48e3a3dde2  data/wal-recycled/000009.sst  (1271 B)
7077bcdb05d3a12b1c5059b9b9b1c00cb14c56b443cba8d07ae287de7109ef56  data/wal-recycled/000010.sst  (1143 B)
0a212e669575a2e89f1f12271ff275b673db292814a057fb6b6f39dc9e31e7b5  data/wal-recycled/000011.log  (938 B)
3a4e32fffb681554dfb1234f360452c985c3e0b444cf993f1f0242c0eb6c894b  data/wal-recycled/000012.sst  (2153 B)
a4299a1db4cff6de28fa7c4b023b0b0ea6493d8b13ddb23ce1e78bff4cc95085  data/wal-recycled/000013.sst  (1143 B)
875580945e3ea00b035def8167e74590a32a7185cf14710e0cdb88eebd5cf2b2  data/wal-recycled/000014.log  (11334 B)
00ef87abd73255c15be1611eb0d7ca96b9dcbf1603d044cd5f2b89fb188ca7eb  data/wal-recycled/000015.sst  (1231 B)
901c2802b011a950464f965d58e971ecb0059ea555c280b0121958d7f48ae867  data/wal-recycled/000016.sst  (1143 B)
9c283f6e81028b9eb0760d918ee4bc0aa256ed3b926393c1734c760c4bd724fd  data/wal-recycled/CURRENT  (16 B)
13df3f1ee9c1872247e177a2f3a5c0a9224f8f9d13bc4f56e651ba619666992f  data/wal-recycled/IDENTITY  (36 B)
d25c9ea07b80ffa20e19ffc01b205e1dfde7d9b4f21d0fd992b5644e47d8cf3b  data/wal-recycled/MANIFEST-000005  (818 B)
e7079f54b0b84e0f318885cb6be2005174dab9e506fc76e3b8123138f979e844  data/wal-recycled/OPTIONS-000007  (12530 B)
```

</details>

---

# files/ (standalone files, one per directory)

Valid files are exact byte copies of files in `data/` (hash-equal); corrupt files are single-mutation copies of those. Originals are never modified; only the five `corrupt-*` entries are damaged. 'flip' = the byte XOR 0xFF.

| files/ entry | file | source | expected |
|---|---|---|---|
| sst-none, sst-snappy, sst-zlib, sst-bzip2, sst-lz4, sst-lz4hc, sst-zstd | `sst-<codec>.sst` | `data/codec-<codec>/000009.sst` | SST, Footer/Metaindex/Properties/Index/Filter/16 data blocks all Present; level "unknown for a standalone SST"; producer DB id and column family "default" are read from the SST's own properties; compression per the codec table |
| sst-bloom-full | `sst-bloom-full.sst` | `data/bloom-full/000009.sst` | SST, `Filter block (fullfilter.rocksdb.BuiltinBloomFilter)` Present |
| wal-plain | `000004.log` | `data/wal-basic/000004.log` | WAL, 14 FULL records, all Present, log number 4 from the file name |
| wal-fragmented | `000004.log` | `data/wal-fragmented/000004.log` | WAL, FIRST+LAST pair across the block boundary, all Present |
| wal-recycled | `000014.log` | `data/wal-recycled/000014.log` | WAL, 7 Present records (log number 14) then the expected stale tail (Invalid at 0x56e, then Unknown) |
| manifest-basic | `MANIFEST-000005` | `data/basic-db/MANIFEST-000005` | MANIFEST, all records Present; "Observed levels": `default L0 000009` with disk status "missing" (the SST is not in this directory: honest, not an error of the file) |
| current-basic | `CURRENT` | `data/basic-db/CURRENT` | CURRENT: `MANIFEST-000005\n`; referenced MANIFEST **Not present** (standalone) |

Corrupt copies (exact mutations; source SST is `data/codec-none/000009.sst`, 6777 B, uncompressed so the flipped byte is plain data; source WAL is `files/wal-plain/000004.log`, 443 B):

| files/ entry | mutation | expected detection |
|---|---|---|
| corrupt-sst-checksum-flip / `corrupt-sst-checksum-flip.sst` | offset 100 (inside data block 0, bytes 0..322): `0x78 -> 0x87` | Footer and all other blocks Present; `Data block @0 (323 B)` **Invalid**; its Trailer row `checksum @0x144` **Invalid** ("checksum Invalid" in the block notes). The engine reports `block checksum mismatch ... offset 0 size 323`. |
| corrupt-sst-truncated / `corrupt-sst-truncated.sst` | truncated to 3388 of 6777 bytes (cuts block 10, which spans 3280..3607; footer, index, filter, properties lost) | `Footer` **Invalid**: "bad table magic number 0x706d756a20786f66" (the last 8 bytes now are ordinary data-block text). RocksVision cannot tell a cut file from a bad footer without the MANIFEST size; the engine reports it as "Sst file size mismatch. Expected 6777, actual size 3388" when the MANIFEST is present. Nothing below the footer is decoded. |
| corrupt-sst-bad-footer / `corrupt-sst-bad-footer.sst` | offset 6769 (first byte of the 8-byte footer magic, file length 6777): `0xf7 -> 0x08` | `Footer` **Invalid**: "bad table magic number 0x88e241b785f4cf08" (expected `0x88e241b785f4cff7`). Nothing below the footer is decoded. |
| corrupt-wal-checksum-flip / `corrupt-wal-checksum-flip.log` | offset 50 (payload byte of physical record 1; record at 30, header 7, payload 27): `0x06 -> 0xf9` | record 0 Present; record at 0x1e (`FULL len 27`) **Invalid**, `checksum (masked crc32c)` row Invalid, CRC stored/computed `2f7e1552 / 0ccd31b3`; its WriteBatch decode is marked mismatch/incomplete; later records stay readable. |
| corrupt-wal-truncated / `corrupt-wal-truncated.log` | truncated to 85 of 443 bytes (record 2 at 64: header 7 + payload 29, cut 14 bytes into its payload) | records 0 and 1 Present; record at 0x40 **Truncated** (payload has 14 of 29 bytes); no later records. |

Wording for the engine messages above comes from the verification run, not from RocksVision. For `corrupt-sst-truncated` the app status is Invalid, not Truncated: that is the actual behavior of the frozen binary and is recorded here instead of being papered over.

<details><summary>SHA-256 of the 18 file(s)</summary>

```
4ec87f1eae711c4d33d7d3f1566ec761040e89ed946098bb32a63df23f70f765  files/corrupt-sst-bad-footer/corrupt-sst-bad-footer.sst  (6777 B)
4b8d33dafc2226fead00bbf72baf53fbde6c56bd716f3e87cbbb9c333df3472c  files/corrupt-sst-checksum-flip/corrupt-sst-checksum-flip.sst  (6777 B)
2f88fbd2ef364143b953041195b5aafff8b872c1432a416f2f06ff04496e8c0a  files/corrupt-sst-truncated/corrupt-sst-truncated.sst  (3388 B)
312fa8ddfba13f52a239478f446e2128241abf3129917c00c55fa8d9f4739190  files/corrupt-wal-checksum-flip/corrupt-wal-checksum-flip.log  (443 B)
99e342a9daee5a65e35cf501676de508a0e685f8501efaf5d938b291c95d001b  files/corrupt-wal-truncated/corrupt-wal-truncated.log  (85 B)
9c283f6e81028b9eb0760d918ee4bc0aa256ed3b926393c1734c760c4bd724fd  files/current-basic/CURRENT  (16 B)
f809b912cd6e7e561b64700e79cdf696ff57c27c6cb5ef9eda6f9b7c1ff11c97  files/manifest-basic/MANIFEST-000005  (234 B)
a6d17ddd47df30085785a64a9d181022c46c69803c0edc309f8d97c2628e7b08  files/sst-bloom-full/sst-bloom-full.sst  (6777 B)
f924f965d2228db41f08b926423b8c41dbd54e5f2d396bcec248ba644f30e5dd  files/sst-bzip2/sst-bzip2.sst  (3829 B)
a69fc45c9f0822ae7a26d2f880357ec208a97c737a8a40a5c46365b6fc10016e  files/sst-lz4/sst-lz4.sst  (3072 B)
062f69529823cfca06633310170fae8176af54e012678b57eb2a375c5ecdf73d  files/sst-lz4hc/sst-lz4hc.sst  (3009 B)
3b3ebd0c83b0ed40c1f9678a9b94508c87fdeffabc6a2cadad752821f08a4dab  files/sst-none/sst-none.sst  (6777 B)
36768e7100afb29ba87461f698d57ff5b0a3eba1df3e0475748bd77c8de4b2ac  files/sst-snappy/sst-snappy.sst  (3148 B)
ad291495e8c45158247a6e4fb6e540c2eedb4a927e0360c73c11d5b43ec03af7  files/sst-zlib/sst-zlib.sst  (2775 B)
6ffb3ac06df72c667c908ebcddf2b572133625c4e54fb28269a2dc98f8c111a5  files/sst-zstd/sst-zstd.sst  (3098 B)
953aba0fb9c1ad49e528a8df49c6358f554352aebf19fe52c3d5b6d137c82938  files/wal-fragmented/000004.log  (44548 B)
9d332f589ab493429e6469552e45adc45e9e7e5dc7e5cf53109283bdb37e028a  files/wal-plain/000004.log  (443 B)
875580945e3ea00b035def8167e74590a32a7185cf14710e0cdb88eebd5cf2b2  files/wal-recycled/000014.log  (11334 B)
```

</details>

---

# Not generated / honest limits

- **No index-less block-based SST**: not a valid RocksDB layout, deliberately not fabricated (see the concept paragraph).
- **LZ4HC vs LZ4 id**: requested assumption ("LZ4HC shares the LZ4 on-disk id") is false for 10.4.2: id 5 vs 4 (same payload format). Both are separate scenarios with their real ids.
- **Valid standalone SSTs are DB-flushed SSTs**, not `SstFileWriter` output. They cannot be ingested (the engine requires the external-file version property that only `SstFileWriter` writes) and the RocksDB C API used here has no SstFileReader. The engine check for them is therefore "substitute the file into its source scenario in a scratch copy, reopen read-only, iterate with checksum verification" (see Verification).
- **bloom-partitioned == index-partitioned** (identical options, different entry points).
- Zero-length `000008.log` in the single-SST scenarios is intentional (real engine state), not a retained data WAL; the data WALs are in basic-db, multilevel-db, wal-*.
- Only L0 and L1 are populated in `multilevel-db` (no L2+); one L0/L1 layout was generated, not a family of them.
- `wal-recycled` is a crash-consistent copy of an open DB, not a closed one. Not generated: WAL with compression (covered by the older `tests/fixtures/wal-zstd`), predecessor-WAL records, blob files, user-defined timestamps, PlainTable.
- One fixture per structure: nothing here claims full format support.

# Verification

Run on this machine (macOS ARM) against the committed files; RocksDB engine = the pinned generator build, RocksVision = the frozen `target/release/rocksvision` (SHA-256 `f72227dc6369853c1161dcedce522f0c9e0cac2def5b39ecafab4037588dcbb3`, read-only `--dump` only):

1. `shasum -a 256 -c SHA256SUMS` (in `examples/`): 136/136 OK. `shasum -a 256 -c tests/fixtures/SHA256SUMS`: 70/70 OK (existing fixtures untouched).
2. Engine: `tests/fixture-gen/target/release/fixture-gen examples-verify examples <scratch>`: every `data/` scenario copied to scratch and reopened **read-only** with the engine, key count per CF compared with the expected values in this README (18/18 PASS); multilevel-db `live_files()`/levelstats printed after the reopen (matches the table above); every valid `files/` SST substituted into its source scenario (byte-identical, 48 entries iterated with checksum verification, 8/8 PASS); the three corrupt SSTs rejected or failing iteration (3/3 PASS). Total 29 PASS, `ALL PASS`.
3. RocksVision: `rocksvision --dump <path> --limit 5000 --rows 300` for **every one of the 136 files** (all data/ CURRENT/MANIFEST/WAL/SST/OPTIONS/IDENTITY and all files/ entries including the five corrupt ones) under `timeout 60`: 136/136 exit code 0, no panic; plus `--dump <dir>` for all 36 directories. Corrupt files show Invalid/Truncated rows as tabulated above.
4. `cargo test --release --test examples` (tests/examples.rs: each files/ entry opened from a temp dir that holds only that file, expected kind and status; each data/ CURRENT resolves its MANIFEST; multilevel observed levels contain L0 and L1; SHA256SUMS lists every file) is part of the freeze verification set; see the tester report for whether it has been run.
