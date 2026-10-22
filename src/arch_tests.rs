//! Architecture checks: read the source tree at test time and assert the dependency rules of
//! docs/module-map.md (text-based; comments are ignored).

use std::path::{Path, PathBuf};

fn src() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// All .rs files under `rel` (a file or a directory below src/), with comment-free code.
fn code(rel: &str) -> Vec<(String, String)> {
    let p = src().join(rel);
    let mut files = Vec::new();
    collect(&p, &mut files);
    assert!(!files.is_empty(), "no sources under src/{rel}");
    files
        .into_iter()
        .map(|f| {
            let text = std::fs::read_to_string(&f).unwrap();
            let code: Vec<&str> = text
                .lines()
                .filter(|l| !l.trim_start().starts_with("//"))
                .map(|l| match l.find(" // ") {
                    Some(i) => &l[..i],
                    None => l,
                })
                .collect();
            (f.strip_prefix(src()).unwrap().display().to_string(), expand_groups(&code.join("\n")))
        })
        .collect()
}

/// Appends one `crate::item` line per member of every grouped import `crate::{a, b::{..}}`, so
/// grouped imports are matched like single-path ones (`crate::x::{..}` already contains `crate::x`).
fn expand_groups(code: &str) -> String {
    let mut out = code.to_string();
    let mut rest = code;
    while let Some(i) = rest.find("crate::{") {
        let body = &rest[i + "crate::{".len()..];
        let (mut depth, mut start, mut end) = (1, 0, body.len());
        for (j, ch) in body.char_indices() {
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = j;
                        break;
                    }
                }
                ',' if depth == 1 => {
                    out.push_str(&format!("\ncrate::{}", body[start..j].trim()));
                    start = j + 1;
                }
                _ => {}
            }
        }
        out.push_str(&format!("\ncrate::{}", body[start..end].trim()));
        rest = &body[end.min(body.len())..];
    }
    out
}

#[test]
fn grouped_imports_are_expanded() {
    let e = expand_groups("use crate::{app::{jobs, state}, ui};");
    assert!(e.contains("crate::app::{jobs, state}") && e.contains("\ncrate::ui"), "{e}");
}

fn collect(p: &Path, out: &mut Vec<PathBuf>) {
    if p.is_dir() {
        for e in std::fs::read_dir(p).unwrap() {
            collect(&e.unwrap().path(), out);
        }
    } else if p.extension().map(|e| e == "rs").unwrap_or(false) {
        out.push(p.to_path_buf());
    }
}

/// Positive control: the scanner sees real imports (guards against an empty/blind scan).
fn require(rel: &str, needle: &str) {
    assert!(code(rel).iter().any(|(_, c)| c.contains(needle)), "expected src/{rel} to use {needle}");
}

fn forbid(rels: &[&str], needles: &[&str]) {
    let mut bad = Vec::new();
    for rel in rels {
        for (f, c) in code(rel) {
            for n in needles {
                if c.contains(n) {
                    bad.push(format!("src/{f} uses {n}"));
                }
            }
        }
    }
    assert!(bad.is_empty(), "forbidden dependencies:\n{}", bad.join("\n"));
}

/// Everything below the UI layer.
const NON_UI: &[&str] = &["app", "io", "format", "wal", "sst", "manifest.rs", "present", "model.rs", "limits.rs", "inspect.rs", "dump.rs", "discover.rs", "util.rs", "branding.rs"];

#[test]
fn nothing_below_ui_imports_ui_or_egui() {
    require("ui", "egui");
    forbid(NON_UI, &["crate::ui", "eframe", "egui", "rfd::"]);
}

#[test]
fn objc2_only_in_platform_adapter() {
    require("ui/platform", "objc2");
    let mut bad = Vec::new();
    for (f, c) in code("") {
        if c.contains("objc2") && !f.starts_with("ui/platform") && f != "arch_tests.rs" {
            bad.push(f);
        }
    }
    assert!(bad.is_empty(), "objc2 outside src/ui/platform: {bad:?}");
}

#[test]
fn ui_does_not_import_parsers() {
    require("ui", "crate::app::");
    forbid(&["ui"], &["crate::wal", "crate::sst", "crate::manifest", "crate::format", "crate::inspect", "crate::present"]);
}

#[test]
fn parsers_do_not_import_each_other_or_presenters() {
    require("wal", "crate::format::");
    require("present", "crate::wal::");
    forbid(&["wal"], &["crate::sst", "crate::manifest", "crate::present", "crate::inspect", "crate::app"]);
    forbid(&["sst"], &["crate::wal", "crate::manifest", "crate::present", "crate::inspect", "crate::app"]);
    forbid(&["manifest.rs"], &["crate::wal", "crate::sst", "crate::present", "crate::inspect", "crate::app"]);
    forbid(&["format", "io", "model.rs", "limits.rs", "util.rs"], &["crate::wal", "crate::sst", "crate::manifest", "crate::present", "crate::inspect", "crate::app"]);
}

#[test]
fn panels_only_read_app_state() {
    forbid(&["ui/panels"], &["&mut AppState", "&mut self.st", "crate::app::jobs"]);
    forbid(&["ui/keys.rs", "ui/menu.rs", "ui/platform"], &["&mut AppState", "&mut self.st"]);
}

#[test]
fn model_has_no_pixel_geometry() {
    forbid(&["model.rs"], &["f32", "f64", "width", "px"]);
}

/// Every request variant has a kind-correct Failed reply (and I/O requests a Cancelled one).
#[test]
fn every_request_has_a_matching_failed_arm() {
    let jobs = std::fs::read_to_string(src().join("app/jobs/mod.rs")).unwrap();
    let variants = |enum_name: &str| -> Vec<String> {
        let start = jobs.find(&format!("pub enum {enum_name} {{")).unwrap_or_else(|| panic!("enum {enum_name}"));
        let body = &jobs[start..start + jobs[start..].find("\n}").unwrap()];
        body.lines()
            .skip(1)
            .map(str::trim)
            .filter(|l| !l.starts_with("//") && !l.is_empty())
            .map(|l| l.split(|c: char| !c.is_alphanumeric()).next().unwrap().to_string())
            .filter(|v| !v.is_empty())
            .collect()
    };
    let decode = std::fs::read_to_string(src().join("app/jobs/decode.rs")).unwrap();
    let io = std::fs::read_to_string(src().join("app/jobs/io_pool.rs")).unwrap();
    let reqs = variants("Request");
    assert_eq!(reqs, ["Open", "Children", "View", "Bytes", "CopyBytes"]);
    let count = |s: &str, n: &str| s.matches(n).count();
    for v in &reqs {
        // DecodeKind::of maps the request, DecodeKind::failed answers it, handle() serves it.
        assert!(count(&decode, &format!("Request::{v}")) >= 2, "decode: Request::{v} not mapped and handled");
        assert!(count(&decode, &format!("DecodeKind::{v}")) >= 2, "decode: no Failed arm for {v}");
    }
    let ios = variants("IoRequest");
    assert_eq!(ios, ["Resolve", "Scan", "Page", "Copy", "Stat"]);
    for v in &ios {
        // IoKind::of, run_io, io_cancelled.
        assert!(count(&io, &format!("IoRequest::{v}")) >= 3, "io: IoRequest::{v} lacks a run or Cancelled arm");
        assert!(count(&io, &format!("IoKind::{v}")) >= 2, "io: no Failed arm for {v}");
    }
}

/// The five AppState components keep their fields private and expose only `&self` methods
/// publicly (mutators are `pub(super)`, i.e. visible inside `app` only). The compiler enforces
/// the visibility; this text check guards against a field or mutator becoming `pub` again.
#[test]
fn app_components_have_private_fields_and_read_only_pub_api() {
    let comps = [("app/session.rs", "DocumentSession"), ("app/selection.rs", "SelectionState"), ("app/search.rs", "SearchState"), ("app/job_state.rs", "JobState"), ("app/view_prefs.rs", "ViewPreferences"), ("app/state.rs", "AppState")];
    let mut bad = Vec::new();
    for (rel, name) in comps {
        let text = std::fs::read_to_string(src().join(rel)).unwrap();
        let start = text.find(&format!("pub struct {name} {{")).unwrap_or_else(|| panic!("{name} in {rel}"));
        let body = &text[start..start + text[start..].find("\n}").unwrap()];
        let fields = body.lines().skip(1).map(str::trim).filter(|l| !l.starts_with("//") && !l.is_empty()).collect::<Vec<_>>();
        assert!(!fields.is_empty(), "{name}: no fields seen");
        for l in fields {
            if l.starts_with("pub") {
                bad.push(format!("{rel}: {name} field `{l}` is not private"));
            }
        }
        if name != "AppState" {
            for l in text.lines().map(str::trim).filter(|l| l.starts_with("pub fn ") && l.contains("&mut self")) {
                bad.push(format!("{rel}: public mutator `{l}`"));
            }
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

/// The legacy `Deref` snapshot (`app::compat`) is gone: the UI and tests read the component
/// getters (the compiler rejects field reads such as `st.zoom`).
#[test]
fn legacy_snapshot_shim_is_removed() {
    assert!(!src().join("app/compat.rs").exists(), "app/compat.rs is back");
    require("ui", "st.prefs()");
    let bad: Vec<String> = code("").into_iter().filter(|(f, c)| f != "arch_tests.rs" && (c.contains("LegacyFields") || c.contains("impl std::ops::Deref for AppState"))).map(|(f, _)| f).collect();
    assert!(bad.is_empty(), "legacy shim still referenced: {bad:?}");
}

/// Presenters and the UI select behaviour by `FieldKind`/`DecodedValue`, not by display labels
/// or by parsing their own strings.
#[test]
fn presenters_do_not_branch_on_labels() {
    require("present", "FieldKind");
    forbid(&["present", "ui"], &["name.as_str()", ".name ==", "name.starts_with(", "value.split_whitespace", "value.parse"]);
}
