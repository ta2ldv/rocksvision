# Expected clipboard strings (v3 FINAL: v2 table + section "v3 (final)" below, rules read from src at FREEZE B) - v2 part from src at FREEZE B1; compare byte-for-byte with `pbpaste`/`clipsave` (clipsave writes the exact bytes)
Sources (read-only): `AppState::do_copy` + `bytes_copy_text` (app/state.rs), `jobs::hex_copy_text`, `present/common.rs`, `present/sst.rs`,
dump rows (`--dump` prints the row cells joined by " | "; Copy row joins them with TAB). No copy adds a trailing newline of its own.
TAB = 0x09. In the row-TSV cells, `\n` = backslash + n (the display preview). Value/Key copy = the real bytes when valid UTF-8,
otherwise an escaped form (`\` -> `\\`, non-UTF-8 byte -> `\xNN`), bounded by the 64 KiB copy cap.

## db-levels/CURRENT, row 1 "contents" (bytes 0x0..0x10)
| item | exact clipboard | status note |
|---|---|---|
| Copy value | `MANIFEST-000005` + real LF = 16 bytes, last byte 0x0a (`pbpaste | xxd` ends `...3035 0a`) | Copied value (16 bytes, UTF-8 text) |
| Copy key | disabled in the menu (row has no key); if forced: "This row has no key", clipboard unchanged | - |
| Copy hex (max 64 KiB) | `4d 41 4e 49 46 45 53 54 2d 30 30 30 30 30 35 0a` | Copied 16 bytes as hex |
| Copy offset | `file 0x0..0x10 (16 bytes)` | Copied offset range |
| Copy row (TSV) | `contents` TAB `0x0` TAB `16` TAB `MANIFEST-000005\n` TAB `text line + \n` TAB `4d 41 4e 49 46 45 53 54 2d 30 30 30 30 30 35 0a` TAB `✔ Present` (7 cells; literal backslash-n inside) | Copied row (7 columns, TSV; cells are the table previews) |
Row 2 "referenced MANIFEST" (no value_bytes): Copy value = `MANIFEST-000005` (15 chars, no LF), note "Copied value (15 chars, displayed text)"; hex `4d 41 4e 49 46 45 53 54 2d 30 30 30 30 30 35`; offset `file 0x0..0xf (15 bytes)`.

## sst-zstd/data.sst, Data block @0x0 (decompressed), row 0 (entry 0)
| item | exact clipboard |
|---|---|
| Copy value | the 148-byte value `value-000000:` + 3 x `The quick brown fox jumps over the lazy dog. ` (ends with one space, no newline); note "Copied value (148 bytes, UTF-8 text)" |
| Copy key | the INTERNAL key, 18 bytes: `key-000000` + 7 control bytes `01 00 00 00 00 00 00 00` (8 trailer bytes: 0x01 then seven NULs) - valid UTF-8, copied raw; note "Copied key (18 bytes, UTF-8 text)". TO CONFIRM: macOS pasteboard may cut/strip at the NUL bytes; if `clipsave` shows fewer than 18 bytes record the actual bytes (`xxd`) as a finding |
| Copy hex | 170 bytes, space-separated lowercase: `00 12 94 01 6b 65 79 2d 30 30 30 30 30 30 01 00 00 00 00 00 00 00 76 61 6c 75 65 2d ...` (509 chars; SHA-256 of the string `0223cd9578fcf6df0758ff8e1cf1f6bae120240fc32f7370fff30fc90ddf0532`) |
| Copy offset | `block@0x0 (decompressed) 0x0..0xaa (170 bytes)` |
| Copy row (TSV) | `0` TAB `blk+0x0` TAB `R` TAB `0` TAB `18` TAB `148` TAB `"key-000000"` TAB `0` TAB `1 Value (Put)` TAB `"value-000000:The quick brown fox jumps over the lazy dog. The qu … (+84 B)"` TAB `✔ Present` (11 cells) |
SHA-256 of the clipsave'd bytes: value (148 B) `d72e28f4c1bb776771f6d2451ba50844c397baf9a43a6a7738b975dc3fd67422`; key (18 B) `cec99885c2ffc7326030dfef014358f313b21841bea062eaf298a40b93428418` (`shasum -a 256 <clipsave>.txt`).
Hex string: `python3 -c "v=b'value-000000:'+b'The quick brown fox jumps over the lazy dog. '*3; e=bytes([0,18,0x94,1])+b'key-000000'+bytes([1]+[0]*7)+v; print(' '.join('%02x'%b for b in e))"`

## Other
- Hex copy capped at 65536 bytes (LIMITS.copy_cap); note `Copied N of M bytes as hex (truncated: 64 KiB copy cap / end of data)`; big-100k (wal-fragmented) hex = 196607 chars. Value/Key over the cap: note ends `; first 65536 of N bytes, 64 KiB copy cap`.
- Binary values and the 64 KiB caps: superseded by section "v3 (final)" below (confirmed rules, exact bytes).
- Copy row cells are previews: long values end ` … (+N B)`.

## v3 (final) - byte-level, rules read from src/app/state.rs (`bytes_copy_text`, `copy`) and src/app/jobs/mod.rs (`hex_copy_text`) at FREEZE B
Method for every item: `clipclear`, copy via the app, `clipsave NAME`, then `xxd NAME.txt | head` and `shasum -a 256 NAME.txt` (+ `wc -c`); compare with the values below. Never judge from a screenshot or from `clip` (repr) alone.
**Rule (source, exact)**: Copy value / Copy key take the row's `value_bytes` / `key_bytes` (each bounded to the first 65536 bytes, `total` = full length). If those bytes are valid UTF-8 the text is copied RAW (controls and NUL included, real newlines, backslash NOT doubled); note `Copied <what> (<n> bytes, UTF-8 text[; first <n> of <total> bytes, 64 KiB copy cap])`. Otherwise the text is built per `utf8_chunks`: backslash inside valid chunks -> `\\`, each invalid byte -> `\xNN` (lowercase hex), valid chunks (including control bytes 00-1f) stay RAW; note `Copied <what> (escaped, <n> bytes[; first ...])`. No row bytes (e.g. WAL op rows have key_bytes = none): Copy value falls back to the displayed text (`Copied value (<n> chars, displayed text)`), Copy key reports `This row has no key` and leaves the clipboard unchanged. Hex: `Copied <n> bytes as hex` or `Copied <n> of <requested> bytes as hex (truncated: 64 KiB copy cap / end of data)`; hex string = lowercase bytes joined by single spaces, no trailing space/newline.
What a text pasteboard keeps (measured with pbcopy|pbpaste): NUL, controls and even invalid bytes survive the raw byte pipe. The app goes through `ctx.copy_text` (egui -> OS clipboard as a string); NUL survival on THAT path is not derivable from source -> decided by the GUI run via `xxd`.

### 1. SST entry with NUL bytes: sst-zstd/data.sst, Data block @0x0, entry row 0, "Copy key"
Key = internal key, 18 bytes: `key-000000` + `01 00 00 00 00 00 00 00` (valid UTF-8 -> copied raw, never escaped by the app).
- Expected note: `Copied key (18 bytes, UTF-8 text)`.
- IDEAL / expected: clipsave = `6b65792d3030303030300100000000000000`, 18 bytes, SHA-256 `cec99885c2ffc7326030dfef014358f313b21841bea062eaf298a40b93428418`.
- ACCEPTABLE-but-record-as-finding (OS/egui layer, not the app's rule): the pasteboard keeps the text but the clipsave file is shorter because NULs were dropped/cut: `6b65792d30303030303001` (11 bytes, cut at first NUL, SHA `451e8e1987a75e5efb6bc19628a1b3a3b91681e7fd238357bf97187d4eab939a`) (NULs dropped instead of cut gives the same 11 bytes here). This means the app's note ("18 bytes") overstates what the clipboard holds; byte-exact fallback = Copy hex of the key bytes.
- An escaped form (`\x00` text) is NOT expected from this build (valid UTF-8 is never escaped); if seen, the rule changed.
- Same on wal-basic: select the op node's `key` FIELD row (bin\x00key = `62 69 6e 00 6b 65 79`, 7 bytes, valid UTF-8): Copy VALUE (not Copy key; WAL fact rows carry no key_bytes -> Copy key says `This row has no key`) gives raw `62696e006b6579`, SHA `fbcc2f5327cd59ef2edd2da6fa40e73a8dcfee2df32e7167e1d6f7fc0d082095`, note `Copied value (7 bytes, UTF-8 text)`.

### 2. Non-UTF-8 value: wal-basic op `Put bin\x00key`, the op's `value` FIELD row (bytes `00 01 02 ff fe`), "Copy value"
Confirmed reading = A: bytes 00 01 02 are valid UTF-8 (stay raw), `ff` and `fe` are invalid -> `\xff\xfe`.
- Expected clipsave: `0001025c7866665c786665` = 00 01 02 + literal `\xff\xfe` (11 bytes), SHA-256 `7c5b2111e46436c8e8cc0ebc7a6886fc34ee8dba885167f32f34fe56b5f20b88`; note `Copied value (escaped, 5 bytes)`.
- If the pasteboard drops the leading NUL: `01025c7866665c786665` (10 bytes) = same finding as item 1.
- Copy hex of the same row: `00 01 02 ff fe` (14 bytes, SHA `f11d02d21d6d6d92aec54b4e7a77f50d11254fecbb4a3ce748d515270fde7cfd`), note `Copied 5 bytes as hex`.
- Reading B (everything escaped, 20 bytes) is NOT what the source does.

### 3. 64 KiB caps: wal-fragmented, op Put `big-100k` (102400 bytes, byte[i] = 'a' + (i mod 26)), the op's `value` FIELD row (span = the 102400 value bytes)
- Copy hex: exactly 196607 characters (65536*3-1), starts `61 62 63`, ends `6e 6f 70` (byte 65535 = 'a'+15 = 0x70), no trailing space/newline; SHA-256 `c9e375d36fa5979d54ca513423ccacd7d7de07e308b61c09f372379e1fbb2a23`; note exactly `Copied 65536 of 102400 bytes as hex (truncated: 64 KiB copy cap / end of data)`. Uncapped would be 307199 chars = FAIL.
- Copy value: first 65536 bytes (valid UTF-8 `abc...`), 65536 bytes, SHA-256 `62b3a2ef06cf977623a5936a8fa653e3caecbf69b5f393ebdfe5022affc5331f`; note exactly `Copied value (65536 bytes, UTF-8 text; first 65536 of 102400 bytes, 64 KiB copy cap)`.
- Copy offset for the same selection: `file 0x<start>..0x<end> (102400 bytes)` (uncapped range text, note `Copied offset range`).
