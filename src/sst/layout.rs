//! Physical layout of a block-based table (RocksDB 10.4.2): every structure the file's own
//! handles name — data blocks and index/filter partitions (from the index), meta blocks (from
//! the metaindex), the metaindex and the footer — sorted by file offset. Bytes no handle
//! accounts for are gaps ("Unaccounted bytes"), never assumed to be contiguous blocks.
//! Bounded: only index blocks/partitions are read (no data block), at most `LAYOUT_HANDLES`
//! data handles; `data_complete` says whether every data handle was listed.
//! Sources: table/format.h (BlockHandle, kBlockTrailerSize), table/meta_blocks.cc (metaindex
//! names), table/block_based/partitioned_index_reader.cc (top-level index → partitions).

use super::footer::{BLOCK_TRAILER, Handle};
use super::{SstInspector, meta_kind};
use crate::limits::{LAYOUT_GROUP, LAYOUT_HANDLES};
use crate::model::BlockKind;

/// What a layout item is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ItemKind {
    /// A block with its handle; `name` is the metaindex name for meta blocks ("" otherwise);
    /// `ordinal` numbers data blocks in index order.
    Block {
        kind: BlockKind,
        name: String,
        ordinal: Option<u64>,
    },
    Footer,
}

/// One physical structure: `[start, start + size)` stored contents, then `trailer` bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub start: u64,
    pub size: u64,
    pub trailer: u64,
    pub kind: ItemKind,
}

impl Item {
    pub fn end(&self) -> u64 {
        self.start.saturating_add(self.size).saturating_add(self.trailer)
    }
    pub fn handle(&self) -> Handle {
        Handle { offset: self.start, size: self.size }
    }
    pub fn block_kind(&self) -> Option<BlockKind> {
        match &self.kind {
            ItemKind::Block { kind, .. } => Some(*kind),
            ItemKind::Footer => None,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct PhysLayout {
    /// Sorted by start offset; duplicates (same handle named twice) removed.
    pub items: Vec<Item>,
    /// Data handles listed / declared by the properties (rocksdb.num.data.blocks).
    pub data_listed: u64,
    pub data_declared: Option<u64>,
    /// Every data handle of the index was listed (no cap, no unreadable index part).
    pub data_complete: bool,
    /// Why the data listing is incomplete (unreadable index part, cap).
    pub note: Option<String>,
    pub file_len: u64,
    /// The walk was cancelled (the result is not cached).
    pub cancelled: bool,
    /// The data listing stopped at `LAYOUT_HANDLES` (no further index partition was read).
    pub capped: bool,
    /// Handles seen by the walk (≥ `data_listed` when capped): the lower bound of "x of ≥N".
    pub data_seen: u64,
    /// Handles whose `[start, start+size+trailer)` is not entirely before the footer (corrupt
    /// handles): never placed in file order after the Footer; listed as Invalid before it.
    pub invalid: Vec<Item>,
    /// Footer offset (the end of the space handles may address).
    pub footer_off: u64,
}

/// One entry of a tree level: a layout item, a group of consecutive data items, a gap, or a
/// handle that points beyond the footer / file (index into `invalid`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Entry {
    Item(usize),
    Group { first: usize, count: usize },
    Gap { start: u64, end: u64 },
    Invalid(usize),
}

impl PhysLayout {
    /// "listed 24 of 24" / "listed 4096 of ≥4097": coverage of the data handles (never a bare
    /// count when the walk is incomplete).
    pub fn listed_text(&self) -> String {
        let total = match (self.data_complete, self.data_declared) {
            (true, _) => self.data_listed.to_string(),
            _ if self.capped => format!("≥{}", self.data_declared.unwrap_or(0).max(self.data_seen).max(self.data_listed + 1)),
            (false, Some(n)) => n.to_string(),
            (false, None) => format!("≥{}", self.data_listed),
        };
        format!("listed {} of {total}", self.data_listed)
    }
    /// Index into `invalid` of the handle `(off, size)`.
    pub fn invalid_at(&self, off: u64, size: u64) -> Option<usize> {
        self.invalid.iter().position(|i| i.start == off && i.size == size)
    }
    /// Inserts the invalid handles right before the Footer entry (the Footer stays last).
    fn with_invalid(&self, mut v: Vec<Entry>) -> Vec<Entry> {
        if self.invalid.is_empty() {
            return v;
        }
        let at = v.iter().position(|e| matches!(e, Entry::Item(i) if self.items[*i].kind == ItemKind::Footer)).unwrap_or(v.len());
        v.splice(at..at, (0..self.invalid.len()).map(Entry::Invalid));
        v
    }
    /// Index of the item that starts at `off`.
    pub fn item_at_start(&self, off: u64) -> Option<usize> {
        self.items.binary_search_by_key(&off, |i| i.start).ok()
    }
    /// Index of the item whose bytes `[start, end)` contain `off`.
    pub fn item_containing(&self, off: u64) -> Option<usize> {
        let i = self.items.partition_point(|it| it.start <= off).checked_sub(1)?;
        (off < self.items[i].end()).then_some(i)
    }
    /// The gap `[a, b)` holding `off` (bytes between known structures), if any.
    pub fn gap_containing(&self, off: u64) -> Option<(u64, u64)> {
        if off >= self.file_len || self.item_containing(off).is_some() {
            return None;
        }
        let i = self.items.partition_point(|it| it.start <= off);
        let a = if i == 0 { 0 } else { self.items[..i].iter().map(Item::end).max().unwrap_or(0) };
        let b = self.items.get(i).map(|it| it.start).unwrap_or(self.file_len);
        (a <= off && off < b).then_some((a, b))
    }
    fn range_of(&self, e: &Entry) -> (u64, u64) {
        match *e {
            Entry::Item(i) => (self.items[i].start, self.items[i].end()),
            Entry::Group { first, count } => (self.items[first].start, self.items[first + count - 1].end()),
            Entry::Gap { start, end } => (start, end),
            Entry::Invalid(i) => (self.invalid[i].start, self.invalid[i].end()),
        }
    }
    fn is_data(&self, i: usize) -> bool {
        matches!(self.items[i].kind, ItemKind::Block { kind: BlockKind::Data | BlockKind::IndexPartition | BlockKind::FilterPartition, .. })
    }
    /// Items `[first, first+count)` as tree entries: themselves when few, else groups of
    /// `LAYOUT_GROUP`^k (same rule as log block groups).
    fn split(first: usize, count: usize) -> Vec<Entry> {
        let g = LAYOUT_GROUP as usize;
        if count <= g {
            return (first..first + count).map(Entry::Item).collect();
        }
        let mut step = g;
        while step * g < count {
            step *= g;
        }
        let mut out = Vec::new();
        let mut s = first;
        while s < first + count {
            let c = step.min(first + count - s);
            out.push(Entry::Group { first: s, count: c });
            s += c;
        }
        out
    }
    /// Inserts the gaps between consecutive entries (and before the first one from `from`).
    fn with_gaps(&self, entries: Vec<Entry>, from: Option<u64>) -> Vec<Entry> {
        let mut out = Vec::with_capacity(entries.len());
        let mut prev = from;
        for e in entries {
            let (a, b) = self.range_of(&e);
            if let Some(p) = prev
                && p < a
            {
                out.push(Entry::Gap { start: p, end: a });
            }
            prev = Some(prev.map_or(b, |p| p.max(b)));
            out.push(e);
        }
        out
    }
    /// Top level of the physical tree: items in file order, long runs of data blocks /
    /// partitions grouped, gaps between them (from offset 0).
    pub fn top(&self) -> Vec<Entry> {
        let mut entries = Vec::new();
        let mut i = 0;
        while i < self.items.len() {
            if self.is_data(i) {
                let mut j = i;
                while j < self.items.len() && self.is_data(j) && self.items[j].kind_tag() == self.items[i].kind_tag() {
                    j += 1;
                }
                entries.extend(Self::split(i, j - i));
                i = j;
            } else {
                entries.push(Entry::Item(i));
                i += 1;
            }
        }
        let mut v = self.with_gaps(entries, Some(0));
        if let Some(end) = self.items.iter().map(Item::end).max()
            && end < self.file_len
        {
            v.push(Entry::Gap { start: end, end: self.file_len });
        }
        self.with_invalid(v)
    }
    /// Every item (no groups) with the gaps between them, from offset 0 to the end of file.
    pub fn with_all_gaps(&self) -> Vec<Entry> {
        let mut v = self.with_gaps((0..self.items.len()).map(Entry::Item).collect(), Some(0));
        if let Some(end) = self.items.iter().map(Item::end).max()
            && end < self.file_len
        {
            v.push(Entry::Gap { start: end, end: self.file_len });
        }
        self.with_invalid(v)
    }
    /// Furthest end of the file-order items before item `first` (0 for the first item): where
    /// the "Gap before" of a group page's first row is measured from.
    pub fn end_before(&self, first: usize) -> u64 {
        self.items[..first.min(self.items.len())].iter().map(Item::end).max().unwrap_or(0)
    }
    /// "Gap before" per entry: bytes between the furthest end so far (starting at `from`) and
    /// the entry's start; None for invalid entries, which do not advance the end.
    pub fn gaps_before(&self, entries: &[Entry], from: u64) -> Vec<Option<u64>> {
        let mut prev = from;
        entries
            .iter()
            .map(|e| {
                if matches!(e, Entry::Invalid(_)) {
                    return None;
                }
                let (a, b) = self.range_of(e);
                let gap = a.saturating_sub(prev);
                prev = prev.max(b);
                Some(gap)
            })
            .collect()
    }
    /// Children of a group entry.
    pub fn group(&self, first: usize, count: usize) -> Vec<Entry> {
        if first + count > self.items.len() || count == 0 {
            return Vec::new();
        }
        self.with_gaps(Self::split(first, count), None)
    }
    /// Groups (outermost first) whose range holds file offset `off`: the tree path of the
    /// item or gap at `off`. Pure.
    pub fn group_path(&self, off: u64) -> Vec<(usize, usize)> {
        let mut out = Vec::new();
        let mut level = self.top();
        loop {
            let hit = level.iter().find_map(|e| match *e {
                Entry::Group { first, count } => {
                    let (a, b) = self.range_of(e);
                    (off >= a && off < b).then_some((first, count))
                }
                _ => None,
            });
            match hit {
                Some((f, c)) => {
                    out.push((f, c));
                    level = self.group(f, c);
                }
                None => return out,
            }
        }
    }
}

impl Item {
    fn kind_tag(&self) -> Option<BlockKind> {
        self.block_kind()
    }
}

/// Builds the layout from the footer, metaindex and index (bounded; see module doc).
pub(super) fn build(s: &mut SstInspector, cancel: &dyn Fn() -> bool) -> PhysLayout {
    let mut l = PhysLayout { file_len: s.len(), data_declared: s.prop_u64("rocksdb.num.data.blocks"), ..Default::default() };
    let Ok(f) = s.footer().cloned() else {
        l.data_complete = false;
        l.note = Some("footer not decoded: no handle is known".into());
        return l;
    };
    let block = |h: Handle, kind: BlockKind, name: &str| Item { start: h.offset, size: h.size, trailer: BLOCK_TRAILER, kind: ItemKind::Block { kind, name: name.to_string(), ordinal: None } };
    let mut items = vec![Item { start: f.offset, size: f.len, trailer: 0, kind: ItemKind::Footer }, block(f.metaindex, BlockKind::Metaindex, "")];
    let meta = s.meta().map(|m| m.to_vec()).unwrap_or_default();
    let index = s.index_handle();
    for m in &meta {
        if Some(m.handle) == index && m.name == "rocksdb.index" {
            continue; // listed once below as the index
        }
        items.push(block(m.handle, s.meta_kind_for(&m.name), &m.name));
    }
    let top_kind = s.index_kind();
    if let Some(ih) = index {
        items.push(block(ih, top_kind, ""));
    }
    // Filter partitions (partitioned filter index entries).
    for m in meta.iter().filter(|m| meta_kind(&m.name) == BlockKind::FilterIndex) {
        let (_, res) = s.block_entries(m.handle, BlockKind::FilterIndex);
        if let Ok((_, es, _)) = res {
            items.extend(es.iter().filter_map(|e| e.handle.as_ref()).map(|(h, _, _)| block(*h, BlockKind::FilterPartition, "")));
        }
    }
    // Index partitions and data handles (index order).
    let mut data: Vec<Handle> = Vec::new();
    let mut complete = true;
    if let Some(ih) = index {
        let (_, res) = s.block_entries(ih, top_kind);
        match res {
            Ok((_, es, err)) => {
                if err.is_some() {
                    complete = false;
                    l.note = Some("the index block is not fully decodable".into());
                }
                let hs: Vec<Handle> = es.iter().filter_map(|e| e.handle.as_ref().map(|x| x.0)).collect();
                if top_kind == BlockKind::TopIndex {
                    let mut seen = 0u64;
                    'parts: for ph in hs {
                        if cancel() {
                            l.cancelled = true;
                            break;
                        }
                        items.push(block(ph, BlockKind::IndexPartition, ""));
                        let (_, r) = s.block_entries(ph, BlockKind::IndexPartition);
                        match r {
                            Ok((_, pes, perr)) => {
                                if perr.is_some() {
                                    complete = false;
                                    l.note = Some(format!("index partition at 0x{:x} is not fully decodable", ph.offset));
                                }
                                for e in pes.iter().filter_map(|e| e.handle.as_ref()) {
                                    seen += 1;
                                    if data.len() >= LAYOUT_HANDLES {
                                        // Cap reached: no further partition is read (bounded I/O).
                                        complete = false;
                                        l.capped = true;
                                        l.note = Some(format!("listing capped at {LAYOUT_HANDLES} data handles"));
                                        break 'parts;
                                    }
                                    data.push(e.0);
                                }
                            }
                            Err((_, e)) => {
                                complete = false;
                                l.note = Some(format!("index partition at 0x{:x}: {e}", ph.offset));
                            }
                        }
                    }
                    l.data_seen = seen;
                } else {
                    l.data_seen = hs.len() as u64;
                    if hs.len() > LAYOUT_HANDLES {
                        complete = false;
                        l.capped = true;
                        l.note = Some(format!("listing capped at {LAYOUT_HANDLES} data handles"));
                    }
                    data.extend(hs.into_iter().take(LAYOUT_HANDLES));
                }
            }
            Err((_, e)) => {
                complete = false;
                l.note = Some(format!("index block: {e}"));
            }
        }
    } else {
        complete = false;
        l.note = Some("no index handle".into());
    }
    l.data_listed = data.len() as u64;
    for (n, h) in data.into_iter().enumerate() {
        items.push(Item { start: h.offset, size: h.size, trailer: BLOCK_TRAILER, kind: ItemKind::Block { kind: BlockKind::Data, name: String::new(), ordinal: Some(n as u64) } });
    }
    items.sort_by_key(|a| (a.start, a.size));
    items.dedup_by(|b, a| a.start == b.start && a.size == b.size);
    // A handle must address bytes entirely before the footer (and inside the file); one that
    // does not is a corrupt handle, kept apart so the Footer stays the last structure.
    l.footer_off = f.offset;
    let limit = f.offset.min(l.file_len);
    let (ok, bad): (Vec<Item>, Vec<Item>) = items.into_iter().partition(|it| it.kind == ItemKind::Footer || (it.start < limit && it.end() <= limit && it.end() > it.start));
    l.items = ok;
    l.invalid = bad;
    l.data_complete = complete && !l.cancelled;
    l
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data(start: u64, size: u64, n: u64) -> Item {
        Item { start, size, trailer: BLOCK_TRAILER, kind: ItemKind::Block { kind: BlockKind::Data, name: String::new(), ordinal: Some(n) } }
    }

    /// File order with gaps: entries are sorted, gaps are explicit, the footer is last, and
    /// a long data run is grouped with the same path rule the tree uses.
    #[test]
    fn top_level_order_gaps_and_groups() {
        let mut items: Vec<Item> = (0..150).map(|i| data(i * 100, 95, i)).collect();
        items.push(Item { start: 15_010, size: 30, trailer: 5, kind: ItemKind::Block { kind: BlockKind::Index, name: String::new(), ordinal: None } });
        items.push(Item { start: 15_045, size: 53, trailer: 0, kind: ItemKind::Footer });
        let l = PhysLayout { items, file_len: 15_098, data_complete: true, ..Default::default() };
        let top = l.top();
        // 150 data items → groups of 64, 64, 22; gap [15000,15010); index; footer last.
        assert_eq!(top[..3], [Entry::Group { first: 0, count: 64 }, Entry::Group { first: 64, count: 64 }, Entry::Group { first: 128, count: 22 }]);
        assert_eq!(top[3], Entry::Gap { start: 15_000, end: 15_010 });
        assert_eq!(top[4], Entry::Item(150));
        assert_eq!(*top.last().unwrap(), Entry::Item(151));
        assert_eq!(l.group_path(64 * 100 + 7), vec![(64, 64)]);
        assert!(l.group_path(15_020).is_empty());
        assert_eq!(l.item_containing(64 * 100 + 99), Some(64));
        assert_eq!(l.gap_containing(15_003), Some((15_000, 15_010)));
        assert_eq!(l.gap_containing(15_020), None);
        assert_eq!(l.item_at_start(15_045), Some(151));
    }

    /// 0017 D2: the "Gap before" of a group page's first row is measured from the end of the
    /// items before the page (not from offset 0); contiguous → 0, a real gap → its byte count.
    #[test]
    fn group_page_first_row_gap_is_relative_to_previous_items() {
        // 150 contiguous 100-byte items, except a 50-byte hole before item 64.
        let items: Vec<Item> = (0..150).map(|i| data(i * 100 + if i >= 64 { 50 } else { 0 }, 95, i)).collect();
        let l = PhysLayout { items, file_len: 15_050, data_complete: true, ..Default::default() };
        let gaps = |first: usize, count: usize| l.gaps_before(&l.group(first, count), l.end_before(first));
        // The defect: measured from 0, page 2's first row reported its own start (6450).
        assert_eq!(l.gaps_before(&l.group(64, 64), 0)[0], Some(6450));
        assert_eq!(gaps(0, 64)[..2], [Some(0), Some(0)], "page 1 starts at offset 0");
        assert_eq!(gaps(64, 64)[..2], [Some(50), Some(0)], "page 2: real 50-byte gap, then contiguous");
        assert_eq!(gaps(128, 22)[0], Some(0), "page 3 is contiguous with page 2");
        assert!(gaps(128, 22).iter().all(|g| *g == Some(0)));
        // The whole-file table keeps its semantics: gaps from offset 0, invalid entries None.
        let all = l.with_all_gaps();
        let g = l.gaps_before(&all, 0);
        assert_eq!(g.len(), all.len());
        assert_eq!(g.iter().filter(|g| **g != Some(0)).count(), 0, "the hole is its own Gap entry: {all:?}");
    }

    /// Reviewer 1/2 (0011 fix): a handle beyond the footer is an Invalid entry placed before
    /// the Footer (Footer last in both lists); a capped walk says "of ≥N".
    #[test]
    fn invalid_handles_precede_footer_and_cap_says_at_least() {
        let items = vec![data(0, 95, 0), Item { start: 100, size: 53, trailer: 0, kind: ItemKind::Footer }];
        let mut l = PhysLayout { items, file_len: 153, footer_off: 100, data_complete: true, data_listed: 2, ..Default::default() };
        l.invalid.push(data(5000, 10, 1));
        for v in [l.top(), l.with_all_gaps()] {
            assert_eq!(v[v.len() - 2], Entry::Invalid(0), "{v:?}");
            assert_eq!(*v.last().unwrap(), Entry::Item(1), "Footer last: {v:?}");
        }
        assert_eq!(l.invalid_at(5000, 10), Some(0));
        assert!(l.item_containing(5000).is_none() && l.gap_containing(5000).is_none());
        l.data_complete = false;
        l.capped = true;
        l.data_seen = 2;
        l.data_declared = Some(1);
        assert_eq!(l.listed_text(), "listed 2 of ≥3");
    }
}
