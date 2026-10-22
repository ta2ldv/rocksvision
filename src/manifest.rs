//! CURRENT and MANIFEST decoding, RocksDB 10.4.2.
//! Source: db/version_edit.h (Tag, NewFileCustomTag), db/version_edit.cc (DecodeFrom,
//! DecodeNewFile4From), db/wal_edit.cc, db/blob/blob_file_{addition,garbage}.cc.
//! A MANIFEST is a log-format file (see format::log) whose logical records are VersionEdits.
//! Output is typed (`Edit`, `Levels`) plus one `Fact` per encoded element; explanations are
//! added by `present::manifest`.

pub use crate::format::ikey::{ikey_preview, split_ikey};
use crate::format::ikey::user_key;
use crate::format::log;
use crate::io::RangeRead;
pub use crate::limits::{MAX_EDIT_FIELDS, MAX_LIVE_FILES};
use crate::limits::{MANIFEST_RECORD_KEEP, MAX_EDIT_TAGS, MAX_MANIFEST_PROBLEMS as MAX_PROBLEMS, OVERLAP_MAX_FILES};
use crate::model::{CheckKind, DecodedValue as DV, Diagnostic, Fact, FieldKind as K, Space, Span, Status, Trust};
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
        if self.dropped == 0 {
            self.f.last_mut()
        } else {
            None
        }
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
            2 => x.v64("log number").map(|_| ()),
            3 => x.v64("next file number").map(|_| ()),
            4 => x.v64("last sequence").map(|_| ()),
            9 => x.v64("prev log number").map(|_| ()),
            10 => x.v64("min log number to keep").map(|_| ()),
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
                            if matches!(ct, 3 | 4 | 5 | 6 | 13 | 14 | 15) {
                                if let Some((n, _)) = varint64(&v, 0) {
                                    l.value = n.to_string();
                                    l.decoded = Some(DV::VarInt(n));
                                    l.encoding = "len-prefixed varint64".into();
                                }
                            }
                            if !known {
                                // Bit 6 set = must-understand.
                                l.status = if ct & 64 != 0 { Status::Invalid } else { Status::Unknown };
                                let note = format!("unknown NewFile4 custom tag {ct}");
                                let rec = if ct & 64 != 0 { "RocksDB fails with Corruption (must-understand bit set)" } else { "RocksDB skips this custom field" };
                                l.diag = Some(Box::new(Diagnostic::new(CheckKind::UnknownType, l.span, Trust::Partial, rec, note).values(None, Some(DV::U32(ct)))));
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
                    l.diag = Some(Box::new(Diagnostic::new(CheckKind::UnknownType, l.span, Trust::Trusted, "RocksDB skips it (kTagSafeIgnoreMask)", format!("unknown tag {t} with the safe-ignore bit")).values(None, Some(DV::U32(t)))));
                }
            }),
            _ => {
                let rest = &d[x.pos..];
                // Unknown tag without the safe-ignore bit: RocksDB fails with Corruption.
                let diag = Diagnostic::new(CheckKind::UnknownType, x.span(x.pos, rest.len()), Trust::Untrusted, "RocksDB fails with Corruption; the rest of the edit cannot be framed", format!("unknown tag {tag} without the safe-ignore bit")).values(None, Some(DV::U32(tag)));
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
}

#[derive(Clone, Debug, Default)]
pub struct Levels {
    pub cfs: BTreeMap<u32, (String, BTreeMap<u32, Vec<LiveFile>>)>,
    pub edits: u64,
    pub adds: u64,
    pub deletes: u64,
    pub comparator: Option<String>,
    pub problems: Vec<String>,
    pub problems_dropped: u64,
    pub limited: bool,
    /// Diagnostics of problem records (bounded like `problems`).
    pub diagnostics: Vec<Diagnostic>,
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
        if e.comparator.is_some() {
            lv.comparator = e.comparator.clone();
        }
        if let Some(n) = &e.cf_add {
            names.insert(e.cf, n.clone());
        }
        if e.cf_drop {
            live.remove(&e.cf);
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
            if count >= MAX_LIVE_FILES {
                lv.limited = true;
                continue;
            }
            if cfm.insert(a.number, a).is_none() {
                count += 1;
            }
        }
    }
    for (cf, files) in live {
        let name = names.get(&cf).cloned().unwrap_or_else(|| format!("cf {cf} (name not observed)"));
        let mut by_level: BTreeMap<u32, Vec<LiveFile>> = BTreeMap::new();
        for (_, f) in files {
            by_level.entry(f.level).or_default().push(LiveFile { f, overlap_same: 0, overlap_next: 0 });
        }
        compute_overlaps(&mut by_level);
        if !by_level.is_empty() || cf == 0 {
            lv.cfs.insert(cf, (name, by_level));
        }
    }
    lv
}

/// User-key range overlap, assuming the bytewise comparator.
fn compute_overlaps(levels: &mut BTreeMap<u32, Vec<LiveFile>>) {
    let ranges: BTreeMap<u32, Vec<(Vec<u8>, Vec<u8>)>> = levels
        .iter()
        .map(|(l, fs)| {
            (*l, fs.iter().map(|f| (ukey(&f.f.smallest).to_vec(), ukey(&f.f.largest).to_vec())).collect())
        })
        .collect();
    for (l, fs) in levels.iter_mut() {
        if fs.len() > OVERLAP_MAX_FILES {
            continue;
        }
        fs.sort_by(|a, b| ukey(&a.f.smallest).cmp(ukey(&b.f.smallest)));
        let same: Vec<(Vec<u8>, Vec<u8>)> = fs.iter().map(|f| (ukey(&f.f.smallest).to_vec(), ukey(&f.f.largest).to_vec())).collect();
        let next = ranges.get(&(l + 1));
        for (i, f) in fs.iter_mut().enumerate() {
            let (a, b) = (ukey(&f.f.smallest), ukey(&f.f.largest));
            f.overlap_same = same.iter().enumerate().filter(|(j, (s, e))| *j != i && s.as_slice() <= b && a <= e.as_slice()).count() as u32;
            if let Some(n) = next {
                if n.len() <= OVERLAP_MAX_FILES {
                    f.overlap_next = n.iter().filter(|(s, e)| s.as_slice() <= b && a <= e.as_slice()).count() as u32;
                }
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
