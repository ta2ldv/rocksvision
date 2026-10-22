//! Headless text dump: `rocksvision --dump <path> [--limit N] [--rows N]`.
//! Walks the same lazy tree and views as the UI (depth-first, bounded).

use crate::discover;
use crate::inspect;
use crate::model::{Child, NodeKind, View};
use std::io::Write;
use std::path::Path;

pub struct DumpOpts {
    /// Maximum number of tree nodes visited.
    pub limit: usize,
    /// Maximum rows printed per view.
    pub rows: usize,
}

impl Default for DumpOpts {
    fn default() -> Self {
        DumpOpts { limit: 200, rows: 60 }
    }
}

pub fn dump(path: &Path, o: &DumpOpts, out: &mut dyn Write) -> std::io::Result<bool> {
    if path.is_dir() {
        let (entries, trunc) = discover::scan(path)?;
        writeln!(out, "# directory {} ({} files{})", path.display(), entries.len(), if trunc { ", truncated" } else { "" })?;
        for e in &entries {
            writeln!(out, "{:<28} {:<26} {:>12}{}", e.name, e.kind.label(), e.size, if e.is_symlink { " (symlink, not followed)" } else { "" })?;
        }
        return Ok(true);
    }
    let mut insp = match inspect::open(path) {
        Ok(i) => i,
        Err(e) => {
            writeln!(out, "ERROR {e}")?;
            return Ok(false);
        }
    };
    let never = || false;
    writeln!(out, "# {} — {}", path.display(), insp.kind_label())?;
    let root = insp.view(&NodeKind::Root, &never);
    print_view(&root, 0, o, out)?;
    let mut stack: Vec<(Child, usize)> = insp.children(&NodeKind::Root, &never).into_iter().rev().map(|c| (c, 1)).collect();
    let mut visited = 0usize;
    while let Some((c, depth)) = stack.pop() {
        if visited >= o.limit {
            writeln!(out, "… node limit {} reached", o.limit)?;
            break;
        }
        visited += 1;
        let ind = "  ".repeat(depth);
        writeln!(out, "{ind}+ {} [{} {}]", c.label, c.status.symbol(), c.status.label())?;
        let v = insp.view(&c.kind, &never);
        print_view(&v, depth + 1, o, out)?;
        if !c.leaf {
            for ch in insp.children(&c.kind, &never).into_iter().rev() {
                stack.push((ch, depth + 1));
            }
        }
    }
    Ok(true)
}

fn print_view(v: &View, depth: usize, o: &DumpOpts, out: &mut dyn Write) -> std::io::Result<()> {
    let ind = "  ".repeat(depth);
    writeln!(out, "{ind}== {}{}", v.title, v.status.map(|s| format!(" [{}]", s.label())).unwrap_or_default())?;
    for n in &v.notes {
        writeln!(out, "{ind}   note: {n}")?;
    }
    for s in &v.spaces {
        writeln!(out, "{ind}   space: {}", s.title)?;
    }
    if !v.columns.is_empty() {
        writeln!(out, "{ind}   {}", v.columns.iter().map(|c| c.title).collect::<Vec<_>>().join(" | "))?;
    }
    for r in v.rows.iter().take(o.rows) {
        writeln!(out, "{ind}   {}", r.cells.join(" | "))?;
    }
    if v.rows.len() > o.rows {
        writeln!(out, "{ind}   … {} more rows", v.rows.len() - o.rows)?;
    }
    Ok(())
}
