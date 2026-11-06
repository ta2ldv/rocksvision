//! CURRENT and MANIFEST decoding, RocksDB 10.4.2.
//! Source: db/version_edit.h (Tag, NewFileCustomTag), db/version_edit.cc (DecodeFrom,
//! DecodeNewFile4From), db/wal_edit.cc, db/blob/blob_file_{addition,garbage}.cc.
//! A MANIFEST is a log-format file (see format::log) whose logical records are VersionEdits.
//! Output is typed (`Edit`, `Levels`) plus one `Fact` per encoded element; explanations are
//! added by `present::manifest`.

use crate::format::ikey::user_key;
pub use crate::format::ikey::{ikey_preview, split_ikey};
use crate::format::log;
use crate::io::RangeRead;
use crate::limits::{MANIFEST_RECORD_KEEP, MAX_EDIT_TAGS, MAX_LEVELS, MAX_MANIFEST_PROBLEMS as MAX_PROBLEMS, OVERLAP_MAX_FILES};
pub use crate::limits::{MAX_EDIT_FIELDS, MAX_LIVE_FILES};
use crate::model::{CheckKind, DecodedValue as DV, Diagnostic, Fact, FieldKind as K, KeyOrder, Space, Span, Status, Trust};
use crate::util::{bytes_preview, varint32, varint64};
use std::collections::BTreeMap;

pub fn tag_name(t: u32) -> &'static str {
    match t {
        1 => "Comparator",
        2 => "LogNumber",
        3 => "NextFileNumber",
        4 => "LastSequence",
        5 => "CompactCursor",
        6 => "DeletedFile",
        7 => "NewFile",
        9 => "PrevLogNumber",
        10 => "MinLogNumberToKeep",
        100 => "NewFile2",
        102 => "NewFile3",
        103 => "NewFile4",
        200 => "ColumnFamily",
        201 => "ColumnFamilyAdd",
        202 => "ColumnFamilyDrop",
        203 => "MaxColumnFamily",
        300 => "InAtomicGroup",
        400 => "BlobFileAddition",
        401 => "BlobFileGarbage",
        8193 => "DbId",
        8194 => "BlobFileAddition (deprecated tag)",
        8195 => "BlobFileGarbage (deprecated tag)",
        8196 => "WalAddition",
        8197 => "WalDeletion",
        8198 => "FullHistoryTsLow",
        8199 => "WalAddition2",
        8200 => "WalDeletion2",
        8201 => "PersistUserDefinedTimestamps",
        _ => "Unknown",
    }
}

pub fn custom_tag_name(t: u32) -> &'static str {
    match t {
        1 => "Terminate",
        2 => "NeedCompaction",
        3 => "MinLogNumberToKeepHack",
        4 => "OldestBlobFileNumber",
        5 => "OldestAncesterTime",
        6 => "FileCreationTime",
        7 => "FileChecksum",
        8 => "FileChecksumFuncName",
        9 => "Temperature",
        10 => "MinTimestamp",
        11 => "MaxTimestamp",
        12 => "UniqueId",
        13 => "EpochNumber",
        14 => "CompensatedRangeDeletionSize",
        15 => "TailSize",
        16 => "UserDefinedTimestampsPersisted",
        65 => "PathId",
        _ => "Unknown",
    }
}

#[derive(Clone, Debug)]
pub struct NewFile {
    pub cf: u32,
    pub level: u32,
    pub number: u64,
    pub size: u64,
    pub smallest: Vec<u8>,
    pub largest: Vec<u8>,
    pub smallest_seq: Option<u64>,
    pub largest_seq: Option<u64>,
}

#[derive(Clone, Debug, Default)]
pub struct Edit {
    pub fields: Vec<Fact>,
    pub cf: u32,
    pub adds: Vec<NewFile>,
    pub deletes: Vec<(u32, u64)>,
    pub cf_add: Option<String>,
    pub cf_drop: bool,
    pub comparator: Option<String>,
    pub status: Option<Status>,
    pub note: String,
    /// The problem behind `status` (its note equals `note`).
    pub diag: Option<Diagnostic>,
    pub tags: Vec<&'static str>,
    /// kLogNumber / kNextFileNumber / kLastSequence values, when the edit carries them.
    pub log_number: Option<u64>,
    pub next_file: Option<u64>,
    pub last_seq: Option<u64>,
    /// kPrevLogNumber (9) / kMinLogNumberToKeep (10), when the edit carries them.
    pub prev_log: Option<u64>,
    pub min_log_to_keep: Option<u64>,
}

const RECOVER_EDIT: &str = "decoding of this VersionEdit stops here; the following records are still read";

struct D<'a> {
    d: &'a [u8],
    pos: usize,
    sp: Space,
    f: Vec<Fact>,
    max: usize,
    dropped: u64,
    unknown: bool,
}

impl D<'_> {
    fn push(&mut self, f: Fact) {
        if self.f.len() < self.max {
            self.f.push(f);
        } else {
            self.dropped += 1;
        }
    }
    /// Last field, only while nothing has been dropped (so edits never hit the wrong field).
    fn last(&mut self) -> Option<&mut Fact> {
        if self.dropped == 0 { self.f.last_mut() } else { None }
    }
    fn span(&self, s: usize, n: usize) -> Option<Span> {
        Some(Span::new(self.sp, s as u64, n as u64))
    }
    fn v64(&mut self, name: &str) -> Option<u64> {
        let (v, n) = varint64(self.d, self.pos)?;
        self.push(Fact::new(name, self.span(self.pos, n), v.to_string()).dv(DV::VarInt(v)).raw(&self.d[self.pos..self.pos + n]).enc("varint64"));
        self.pos += n;
        Some(v)
    }
    fn v32(&mut self, name: &str) -> Option<u32> {
        let (v, n) = varint32(self.d, self.pos)?;
        self.push(Fact::new(name, self.span(self.pos, n), v.to_string()).dv(DV::VarInt(u64::from(v))).raw(&self.d[self.pos..self.pos + n]).enc("varint32"));
        self.pos += n;
        Some(v)
    }
    fn slice(&mut self, name: &str, ikey: bool) -> Option<Vec<u8>> {
        self.slice_kind(name, ikey, if ikey { K::InternalKey } else { K::Other })
    }
    fn slice_kind(&mut self, name: &str, ikey: bool, kind: K) -> Option<Vec<u8>> {
        let (len, n) = varint32(self.d, self.pos)?;
        let s = self.pos + n;
        let b = self.d.get(s..s + len as usize)?.to_vec();
        let val = if ikey { ikey_preview(&b) } else { bytes_preview(&b, 80) };
        let enc = if ikey { "len-prefixed internal key" } else { "len-prefixed bytes" };
        self.push(Fact::new(name, self.span(self.pos, n + len as usize), val).kind(kind).raw(&b).bytes(&b).enc(enc));
        self.pos = s + len as usize;
        Some(b)
    }
}

/// Decodes one VersionEdit record (`d` = reassembled record payload), keeping at most
/// MAX_EDIT_FIELDS fields (status Limited beyond that).
pub fn decode_edit(d: &[u8], sp: Space) -> Edit {
    decode_edit_capped(d, sp, MAX_EDIT_FIELDS)
}

/// Summary-only decode (no Fields): file adds/deletes, CF ops, comparator, tag names.
pub fn decode_edit_summary(d: &[u8], sp: Space) -> Edit {
    decode_edit_capped(d, sp, 0)
}

fn decode_edit_capped(d: &[u8], sp: Space, max: usize) -> Edit {
    let mut e = Edit::default();
    let mut x = D { d, pos: 0, sp, f: Vec::new(), max, dropped: 0, unknown: false };
    while x.pos < d.len() {
        let tstart = x.pos;
        let Some((tag, tn)) = varint32(d, x.pos) else {
            e.status = Some(Status::Truncated);
            e.note = "truncated tag varint".into();
            e.diag = Some(Diagnostic::new(CheckKind::Varint, x.span(x.pos, d.len() - x.pos), Trust::Partial, RECOVER_EDIT, e.note.clone()));
            break;
        };
        let name = tag_name(tag);
        if e.tags.len() < MAX_EDIT_TAGS {
            e.tags.push(name);
        }
        x.push(Fact::new(format!("tag {name}"), x.span(tstart, tn), tag.to_string()).kind(K::Tag).dv(DV::Enum { name, code: u64::from(tag) }).raw(&d[tstart..tstart + tn]).enc("varint32"));
        x.pos += tn;
        let ok = match tag {
            1 => x.slice("comparator", false).map(|b| e.comparator = Some(String::from_utf8_lossy(&b).into_owned())),
            2 => x.v64("log number").map(|v| e.log_number = Some(v)),
            3 => x.v64("next file number").map(|v| e.next_file = Some(v)),
            4 => x.v64("last sequence").map(|v| e.last_seq = Some(v)),
            9 => x.v64("prev log number").map(|v| e.prev_log = Some(v)),
            10 => x.v64("min log number to keep").map(|v| e.min_log_to_keep = Some(v)),
            203 => x.v32("max column family").map(|_| ()),
            5 => x.v32("level").and_then(|_| x.slice("cursor key", true)).map(|_| ()),
            6 => (|| {
                let l = x.v32("level")?;
                let n = x.v64("file number")?;
                e.deletes.push((l, n));
                Some(())
            })(),
            7 | 100 | 102 | 103 => (|| {
                let level = x.v32("level")?;
                let number = x.v64("file number")?;
                if tag == 102 {
                    x.v32("path id")?;
                }
                let size = x.v64("file size")?;
                let smallest = x.slice("smallest key", true)?;
                let largest = x.slice("largest key", true)?;
                let (mut ss, mut ls) = (None, None);
                if tag != 7 {
                    ss = Some(x.v64("smallest seqno")?);
                    ls = Some(x.v64("largest seqno")?);
                }
                if tag == 103 {
                    // DecodeNewFile4From: custom fields until kTerminate(1); each is len-prefixed.
                    loop {
                        let ct = x.v32("custom tag")?;
                        if let Some(l) = x.last() {
                            l.value = format!("{ct} ({})", custom_tag_name(ct));
                        }
                        if ct == 1 {
                            break;
                        }
                        let known = custom_tag_name(ct) != "Unknown";
                        let v = x.slice_kind(&format!("  {}", custom_tag_name(ct)), false, if known { K::Other } else { K::UnknownCustomField })?;
                        if let Some(l) = x.last() {
                            if matches!(ct, 3 | 4 | 5 | 6 | 13 | 14 | 15)
                                && let Some((n, _)) = varint64(&v, 0)
                            {
                                l.value = n.to_string();
                                l.decoded = Some(DV::VarInt(n));
                                l.encoding = "len-prefixed varint64".into();
                            }
                            if !known {
                                // Bit 6 set = must-understand.
                                l.status = if ct & 64 != 0 { Status::Invalid } else { Status::Unknown };
                                let note = format!("unknown NewFile4 custom tag {ct}");
                                // Verified: RocksDB 10.4.2 db/version_edit.cc DecodeNewFile4From lines 424–431.
                                let eng = if ct & 64 != 0 {
                                    "RocksDB 10.4.2 db/version_edit.cc DecodeNewFile4From (lines 427–429): an unknown custom tag with the must-understand bit (kCustomTagNonSafeIgnoreMask) fails with \"new-file4 custom field not supported\"."
                                } else {
                                    "RocksDB 10.4.2 db/version_edit.cc DecodeNewFile4From (lines 424–431): an unknown custom tag without the must-understand bit is skipped."
                                };
                                l.diag = Some(Box::new(
                                    Diagnostic::new(CheckKind::UnknownType, l.span, Trust::Partial, "RocksVision lists the field undecoded and continues", note)
                                        .values(None, Some(DV::U32(ct)))
                                        .engine(eng),
                                ));
                            }
                        }
                    }
                }
                e.adds.push(NewFile { cf: 0, level, number: number & 0x3FFF_FFFF_FFFF_FFFF, size, smallest, largest, smallest_seq: ss, largest_seq: ls });
                Some(())
            })(),
            200 => x.v32("column family").map(|c| e.cf = c),
            201 => x.slice("column family name", false).map(|b| e.cf_add = Some(String::from_utf8_lossy(&b).into_owned())),
            202 => {
                e.cf_drop = true;
                Some(())
            }
            300 => x.v32("remaining entries in atomic group").map(|_| ()),
            8193 => x.slice("db id", false).map(|_| ()),
            8198 => x.slice("full history ts low", false).map(|_| ()),
            8201 => x.slice("persist user-defined timestamps", false).map(|_| ()),
            400 | 8194 => (|| {
                x.v64("blob file number")?;
                x.v64("total blob count")?;
                x.v64("total blob bytes")?;
                x.slice("checksum method", false)?;
                x.slice("checksum value", false)?;
                custom_fields(&mut x, 0)
            })(),
            401 | 8195 => (|| {
                x.v64("blob file number")?;
                x.v64("garbage blob count")?;
                x.v64("garbage blob bytes")?;
                custom_fields(&mut x, 0)
            })(),
            8196 => wal_addition(&mut x),
            8199 => (|| {
                let (len, n) = varint32(d, x.pos)?;
                x.push(Fact::new("encoded length", x.span(x.pos, n), len.to_string()).enc("varint32"));
                x.pos += n;
                wal_addition(&mut x)
            })(),
            8197 => x.v64("wal number").map(|_| ()),
            8200 => x.slice("encoded wal deletion", false).map(|_| ()),
            // Unknown tag with kTagSafeIgnoreMask set: RocksDB skips it.
            t if t & 8192 != 0 => x.slice_kind("ignorable field", false, K::IgnorableField).map(|_| {
                if let Some(l) = x.last() {
                    l.status = Status::Unknown;
                    // Verified: RocksDB 10.4.2 db/version_edit.cc VersionEdit::DecodeFrom lines 771–781.
                    l.diag = Some(Box::new(
                        Diagnostic::new(
                            CheckKind::UnknownType,
                            l.span,
                            Trust::Trusted,
                            "RocksVision skips the length-prefixed field and continues",
                            format!("unknown tag {t} with the safe-ignore bit"),
                        )
                        .values(None, Some(DV::U32(t)))
                        .engine("RocksDB 10.4.2 db/version_edit.cc VersionEdit::DecodeFrom (lines 771–781): an unknown tag with kTagSafeIgnoreMask set is skipped by its length prefix."),
                    ));
                }
            }),
            _ => {
                let rest = &d[x.pos..];
                // Unknown tag without the safe-ignore bit: RocksDB fails with Corruption.
                let diag = Diagnostic::new(
                    CheckKind::UnknownType,
                    x.span(x.pos, rest.len()),
                    Trust::Untrusted,
                    "RocksVision stops decoding this edit; the rest cannot be framed",
                    format!("unknown tag {tag} without the safe-ignore bit"),
                )
                .values(None, Some(DV::U32(tag)))
                .engine("RocksDB 10.4.2 db/version_edit.cc VersionEdit::DecodeFrom (lines 782–784): an unknown tag without kTagSafeIgnoreMask sets the error \"unknown tag\".");
                x.push(Fact::new("undecodable remainder", x.span(x.pos, rest.len()), bytes_preview(rest, 48)).kind(K::Remainder).raw(rest).status(Status::Unknown).diag(diag.clone()));
                x.pos = d.len();
                e.status = Some(Status::Unknown);
                e.diag = Some(diag);
                Some(())
            }
        };
        if ok.is_some() && x.pos <= tstart {
            e.status = Some(Status::Invalid);
            e.note = "decoder did not advance; stopped".into();
            e.diag = Some(Diagnostic::new(CheckKind::Structure, x.span(tstart, 1), Trust::Untrusted, RECOVER_EDIT, e.note.clone()));
            break;
        }
        if ok.is_none() {
            e.status = Some(if x.unknown { Status::Unknown } else { Status::Truncated });
            e.note = format!("tag {name} truncated or malformed at record offset 0x{:x}", x.pos);
            let check = if x.unknown { CheckKind::UnknownType } else { CheckKind::Truncation };
            e.diag = Some(Diagnostic::new(check, x.span(tstart, d.len() - tstart), Trust::Partial, RECOVER_EDIT, e.note.clone()));
            break;
        }
    }
    for a in &mut e.adds {
        a.cf = e.cf;
    }
    if x.dropped > 0 && max > 0 {
        x.f.push(Fact::new("more fields", None, format!("{} fields not listed (cap {max})", x.dropped)).status(Status::Limited));
        if e.status.is_none() {
            e.status = Some(Status::Limited);
            e.note = format!("field listing capped at {max}");
            e.diag = Some(Diagnostic::new(CheckKind::Limit, None, Trust::Partial, "the remaining fields are decoded but not listed", e.note.clone()).in_space(sp));
        }
    }
    e.fields = x.f;
    e
}

fn custom_fields(x: &mut D, end: u32) -> Option<()> {
    loop {
        let t = x.v32("custom field tag")?;
        if t == end {
            return Some(());
        }
        x.slice("custom field value", false)?;
    }
}

fn wal_addition(x: &mut D) -> Option<()> {
    x.v64("wal number")?;
    loop {
        let t = x.v32("wal addition tag")?;
        match t {
            1 => return Some(()),
            2 => {
                x.v64("synced size")?;
            }
            _ => {
                // wal_edit.cc: unknown WalAddition tag is Corruption; framing beyond it is unknown.
                x.unknown = true;
                return None;
            }
        }
    }
}

// ---------------- observed levels ----------------

#[derive(Clone, Debug)]
pub struct LiveFile {
    pub f: NewFile,
    pub overlap_same: u32,
    pub overlap_next: u32,
    /// The counts above were computed (comparator order known and the levels within
    /// `OVERLAP_MAX_FILES`); otherwise they are 0 and mean nothing.
    pub overlap_same_known: bool,
    pub overlap_next_known: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Levels {
    pub cfs: BTreeMap<u32, (String, BTreeMap<u32, Vec<LiveFile>>)>,
    pub edits: u64,
    pub adds: u64,
    pub deletes: u64,
    /// Last comparator name seen in any edit (kept for the dump/notes).
    pub comparator: Option<String>,
    /// Comparator name per CF (VersionEdit kComparator applies to the edit's CF;
    /// db/version_set.cc reads it per column family).
    pub cf_comparators: BTreeMap<u32, String>,
    pub problems: Vec<String>,
    pub problems_dropped: u64,
    pub limited: bool,
    /// Diagnostics of problem records (bounded like `problems`).
    pub diagnostics: Vec<Diagnostic>,
    /// kLogNumber per live CF (applies to the edit's CF; a decreasing value is ignored like
    /// RocksDB 10.4.2 db/version_edit_handler.cc 567–576 does). Dropped CFs are removed.
    pub cf_log_numbers: BTreeMap<u32, u64>,
    /// Live (not dropped) CFs seen in the replay (default included).
    pub live_cfs: std::collections::BTreeSet<u32>,
    /// Largest kMinLogNumberToKeep (version_set.cc 5819–5825 keeps the maximum) and the last
    /// kPrevLogNumber.
    pub min_log_to_keep: Option<u64>,
    pub prev_log: Option<u64>,
}

fn push_problem(lv: &mut Levels, p: String) {
    if lv.problems.len() < MAX_PROBLEMS {
        lv.problems.push(p);
    } else {
        lv.problems_dropped += 1;
    }
}

/// Replays VersionEdits in file order (bounded per record) and returns the observed final file set.
pub fn observed_levels(r: &dyn RangeRead, cancel: &dyn Fn() -> bool) -> Levels {
    let mut lv = Levels::default();
    let mut live: BTreeMap<u32, BTreeMap<u64, NewFile>> = BTreeMap::new();
    let mut names: BTreeMap<u32, String> = BTreeMap::new();
    names.insert(0, "default".into());
    let mut off = 0u64;
    let mut count = 0usize;
    while let Some(l) = log::read_logical(r, off, MANIFEST_RECORD_KEEP) {
        if l.end <= off {
            push_problem(&mut lv, format!("scan did not advance at 0x{off:x}; stopped"));
            break;
        }
        if cancel() {
            push_problem(&mut lv, "cancelled".into());
            break;
        }
        off = l.end;
        if l.role != log::FragRole::Full && l.role != log::FragRole::First {
            push_problem(&mut lv, format!("skipped non-edit record at 0x{:x}: {}", l.start, l.note));
            continue;
        }
        if !l.data_complete() {
            push_problem(&mut lv, format!("record at 0x{:x} exceeds bounded size; skipped", l.start));
            continue;
        }
        if l.status != Status::Present {
            push_problem(&mut lv, format!("record at 0x{:x}: {} {}", l.start, l.status.label(), l.note));
            if lv.diagnostics.len() < MAX_PROBLEMS {
                lv.diagnostics.extend(l.diag.clone());
            }
        }
        let e = decode_edit_summary(&l.data, Space::Record(l.start));
        lv.edits += 1;
        if let Some(c) = &e.comparator {
            lv.comparator = Some(c.clone());
            lv.cf_comparators.insert(e.cf, c.clone());
        }
        if let Some(n) = &e.cf_add {
            names.insert(e.cf, n.clone());
        }
        if let Some(n) = e.log_number {
            // Monotonic per CF (version_edit_handler.cc 567–576: a smaller one is ignored).
            let cur = lv.cf_log_numbers.entry(e.cf).or_insert(n);
            *cur = (*cur).max(n);
        }
        if let Some(n) = e.min_log_to_keep {
            lv.min_log_to_keep = Some(lv.min_log_to_keep.map_or(n, |m| m.max(n)));
        }
        if e.prev_log.is_some() {
            lv.prev_log = e.prev_log;
        }
        if e.cf_drop {
            live.remove(&e.cf);
            lv.cf_comparators.remove(&e.cf);
            lv.cf_log_numbers.remove(&e.cf);
            names.remove(&e.cf);
        }
        let cfm = live.entry(e.cf).or_default();
        for (_, num) in &e.deletes {
            if cfm.remove(num).is_some() {
                count = count.saturating_sub(1);
            }
            lv.deletes += 1;
        }
        for a in e.adds {
            lv.adds += 1;
            // A level beyond any RocksDB configuration is not placed (it would make every
            // consumer materialise levels 0..=level); it is reported, never silently dropped.
            if a.level > MAX_LEVELS {
                push_problem(&mut lv, format!("record at 0x{:x}: file {:06} has level {} (> {MAX_LEVELS}); not placed in the levels", l.start, a.number, a.level));
                continue;
            }
            if count >= MAX_LIVE_FILES {
                lv.limited = true;
                continue;
            }
            if cfm.insert(a.number, a).is_none() {
                count += 1;
            }
        }
    }
    // Named CFs (default, added and not dropped) and any CF that still has live files.
    lv.live_cfs = names.keys().copied().chain(live.iter().filter(|(_, f)| !f.is_empty()).map(|(c, _)| *c)).collect();
    for (cf, files) in live {
        let name = names.get(&cf).cloned().unwrap_or_else(|| format!("cf {cf} (name not observed)"));
        let mut by_level: BTreeMap<u32, Vec<LiveFile>> = BTreeMap::new();
        for (_, f) in files {
            by_level.entry(f.level).or_default().push(LiveFile { f, overlap_same: 0, overlap_next: 0, overlap_same_known: false, overlap_next_known: false });
        }
        compute_overlaps(&mut by_level, &KeyOrder::from_comparator(lv.cf_comparators.get(&cf).map(String::as_str)));
        if !by_level.is_empty() || cf == 0 {
            lv.cfs.insert(cf, (name, by_level));
        }
    }
    lv
}

/// Per level: the user-key interval of each live file in bytewise terms (lo <= hi).
type LevelKeyRanges = BTreeMap<u32, Vec<(Vec<u8>, Vec<u8>)>>;

/// The file's user-key interval as bytewise (lo, hi). Under the reverse bytewise comparator
/// smallest >= largest bytewise; interval overlap is invariant under the order reversal, so the
/// normalized bytewise interval gives the same overlap answer (util/comparator.cc).
fn interval(f: &NewFile) -> (Vec<u8>, Vec<u8>) {
    let (a, b) = (ukey(&f.smallest), ukey(&f.largest));
    if a <= b { (a.to_vec(), b.to_vec()) } else { (b.to_vec(), a.to_vec()) }
}

/// User-key range overlap in the CF's comparator order (bytewise or reverse bytewise). For any
/// other comparator nothing is computed (`*_known` stay false) and files keep number order.
fn compute_overlaps(levels: &mut BTreeMap<u32, Vec<LiveFile>>, order: &KeyOrder) {
    if !order.ordered() {
        return;
    }
    let reverse = *order == KeyOrder::ReverseBytewise;
    let ranges: LevelKeyRanges = levels.iter().map(|(l, fs)| (*l, fs.iter().map(|f| interval(&f.f)).collect())).collect();
    for (l, fs) in levels.iter_mut() {
        if fs.len() > OVERLAP_MAX_FILES {
            continue;
        }
        // Comparator order of the smallest key (descending bytes for reverse bytewise).
        fs.sort_by(|a, b| {
            let o = ukey(&a.f.smallest).cmp(ukey(&b.f.smallest));
            if reverse { o.reverse() } else { o }
        });
        let same: Vec<(Vec<u8>, Vec<u8>)> = fs.iter().map(|f| interval(&f.f)).collect();
        let below = l.saturating_add(1);
        let next = ranges.get(&below).filter(|n| n.len() <= OVERLAP_MAX_FILES);
        for (i, f) in fs.iter_mut().enumerate() {
            let (a, b) = &same[i];
            f.overlap_same = same.iter().enumerate().filter(|(j, (s, e))| *j != i && s <= b && a <= e).count() as u32;
            f.overlap_same_known = true;
            if let Some(n) = next {
                f.overlap_next = n.iter().filter(|(s, e)| s <= b && a <= e).count() as u32;
                f.overlap_next_known = true;
            } else if !ranges.contains_key(&below) {
                // No next level: nothing to overlap.
                f.overlap_next_known = true;
            }
        }
    }
}

fn ukey(k: &[u8]) -> &[u8] {
    user_key(k)
}

/// CURRENT is a single text line "MANIFEST-NNNNNN\n" (db/filename.cc SetCurrentFile).
pub fn parse_current(bytes: &[u8]) -> Result<String, String> {
    let s = std::str::from_utf8(bytes).map_err(|_| "CURRENT is not UTF-8 text".to_string())?;
    let Some(line) = s.strip_suffix('\n') else {
        return Err("CURRENT does not end with a newline (incomplete write?)".into());
    };
    if !line.starts_with("MANIFEST-") || line.contains('/') {
        return Err(format!("unexpected CURRENT content {line:?}"));
    }
    Ok(line.to_string())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::io::MemReader;
    use crate::util::{crc_mask, crc32c};

    /// A one-record MANIFEST (bytewise comparator + NewFile2 per (level, number)) for tests.
    pub(crate) fn manifest_bytes(files: &[(u32, u64)]) -> Vec<u8> {
        record(&edit(files))
    }

    fn varint(mut v: u64, out: &mut Vec<u8>) {
        while v >= 0x80 {
            out.push((v as u8) | 0x80);
            v >>= 7;
        }
        out.push(v as u8);
    }
    fn ikey(k: &str) -> Vec<u8> {
        let mut v = k.as_bytes().to_vec();
        v.extend_from_slice(&((1u64 << 8) | 1).to_le_bytes());
        v
    }
    /// VersionEdit: comparator, then NewFile2 (tag 100) per (level, number).
    fn edit(files: &[(u32, u64)]) -> Vec<u8> {
        let mut e = Vec::new();
        varint(1, &mut e);
        let c = b"leveldb.BytewiseComparator";
        varint(c.len() as u64, &mut e);
        e.extend_from_slice(c);
        for &(level, number) in files {
            varint(100, &mut e);
            varint(u64::from(level), &mut e);
            varint(number, &mut e);
            varint(4096, &mut e);
            for k in [ikey("a"), ikey("z")] {
                varint(k.len() as u64, &mut e);
                e.extend_from_slice(&k);
            }
            varint(1, &mut e);
            varint(2, &mut e);
        }
        e
    }
    /// One FULL log record (db/log_writer.cc EmitPhysicalRecord, legacy header).
    fn record(payload: &[u8]) -> Vec<u8> {
        let mut r = vec![0u8; 7];
        r[4..6].copy_from_slice(&(payload.len() as u16).to_le_bytes());
        r[6] = 1;
        let mut c = vec![1u8];
        c.extend_from_slice(payload);
        r[0..4].copy_from_slice(&crc_mask(crc32c(&c)).to_le_bytes());
        r.extend_from_slice(payload);
        r
    }

    /// An edit of CF `cf` (kColumnFamily 200, optional kColumnFamilyAdd 201 / Drop 202) with the
    /// given log-number tags: (tag, value) for kLogNumber 2, kPrevLogNumber 9, kMinLogNumberToKeep 10.
    fn cf_edit(cf: u32, add: Option<&str>, drop: bool, tags: &[(u64, u64)]) -> Vec<u8> {
        let mut e = Vec::new();
        if cf != 0 {
            varint(200, &mut e);
            varint(u64::from(cf), &mut e);
        }
        if let Some(n) = add {
            varint(201, &mut e);
            varint(n.len() as u64, &mut e);
            e.extend_from_slice(n.as_bytes());
        }
        if drop {
            varint(202, &mut e);
        }
        for &(t, v) in tags {
            varint(t, &mut e);
            varint(v, &mut e);
        }
        record(&e)
    }

    /// 0026 item 4: kLogNumber applies to the edit's CF, never decreases (version_edit_handler.cc
    /// 567–576), a dropped CF loses it; kMinLogNumberToKeep keeps the maximum, kPrevLogNumber the
    /// last value.
    #[test]
    fn per_cf_log_numbers_are_replayed() {
        let mut b = cf_edit(0, None, false, &[(2, 5), (10, 3)]);
        b.extend(cf_edit(1, Some("one"), false, &[(2, 7)]));
        b.extend(cf_edit(2, Some("two"), false, &[(2, 6)]));
        b.extend(cf_edit(0, None, false, &[(2, 4), (9, 2), (10, 1)]));
        b.extend(cf_edit(0, None, false, &[(2, 9)]));
        b.extend(cf_edit(2, None, true, &[]));
        let lv = observed_levels(&MemReader::new(b), &|| false);
        assert_eq!(lv.cf_log_numbers, BTreeMap::from([(0, 9), (1, 7)]), "4 < 5 ignored; CF 2 dropped");
        assert_eq!(lv.live_cfs.iter().copied().collect::<Vec<_>>(), vec![0, 1]);
        assert_eq!((lv.min_log_to_keep, lv.prev_log), (Some(3), Some(2)));
        let e = decode_edit(&cf_edit(0, None, false, &[(2, 11), (9, 10), (10, 8)])[7..], Space::File);
        assert_eq!((e.log_number, e.prev_log, e.min_log_to_keep), (Some(11), Some(10), Some(8)));
    }

    /// 0024 D3 (reviewer blocker): a hostile level (u32::MAX, MAX_LEVELS + 1) is reported as a
    /// problem, never placed; the valid files stay; nothing allocates per level.
    #[test]
    fn hostile_levels_are_reported_not_placed() {
        let bytes = record(&edit(&[(0, 9), (u32::MAX, 10), (MAX_LEVELS + 1, 11), (MAX_LEVELS, 12)]));
        let lv = observed_levels(&MemReader::new(bytes), &|| false);
        assert!(lv.cf_log_numbers.is_empty() && lv.live_cfs.contains(&0), "no kLogNumber recorded");
        let (_, levels) = &lv.cfs[&0];
        let placed: Vec<(u32, u64)> = levels.iter().flat_map(|(l, fs)| fs.iter().map(move |f| (*l, f.f.number))).collect();
        assert_eq!(placed, vec![(0, 9), (MAX_LEVELS, 12)], "hostile levels not placed; MAX_LEVELS itself is valid");
        assert_eq!(lv.adds, 4, "every addition is counted");
        assert_eq!(lv.problems.iter().filter(|p| p.contains("not placed in the levels")).count(), 2, "{:?}", lv.problems);
        assert!(lv.problems.iter().any(|p| p.contains(&format!("level {}", u32::MAX))));
        // The overlap text at the top level does not overflow.
        let top = &levels[&MAX_LEVELS][0];
        assert!(top.overlap_next_known);
    }
}
