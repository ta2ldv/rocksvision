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
            (rel_string(f.strip_prefix(src()).unwrap()), expand_groups(&code.join("\n")))
        })
        .collect()
}

/// A path below src/ in one portable form: its components joined with '/', whatever the host
/// separator is (Windows `display()` gives `ui\platform\macos.rs`; the first CI run on
/// windows-2022 failed on exactly that).
fn rel_string(rel: &Path) -> String {
    rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect::<Vec<_>>().join("/")
}

/// True if `rel` is `prefix` or lies below it, compared component-wise after normalizing both
/// separators to '/' (so `ui\platform\macos.rs` matches `ui/platform`; `ui/platformx.rs` does not).
fn rel_matches(rel: &str, prefix: &str) -> bool {
    let (rel, prefix) = (rel.replace('\\', "/"), prefix.replace('\\', "/"));
    let prefix = prefix.trim_end_matches('/');
    rel == prefix || rel.strip_prefix(prefix).is_some_and(|rest| rest.starts_with('/'))
}

/// objc2 may appear only in the platform adapter (and in this file's own needles).
fn objc2_allowed(rel: &str) -> bool {
    rel_matches(rel, "ui/platform") || rel_matches(rel, "arch_tests.rs")
}

/// Regression for the Windows failure: string inputs, so it runs (and is not skipped) on every OS.
#[test]
fn rel_paths_match_with_either_separator() {
    for ok in ["ui/platform/macos.rs", "ui\\platform\\macos.rs", "ui/platform/mod.rs", "ui\\platform\\mod.rs", "ui/platform", "arch_tests.rs"] {
        assert!(objc2_allowed(ok), "valid path rejected: {ok}");
    }
    for bad in ["ui/panels/hex.rs", "ui\\panels\\hex.rs", "ui/platformx.rs", "ui\\platform_extra\\a.rs", "x/ui/platform/macos.rs", "platform/ui/macos.rs", "app\\jobs\\mod.rs", "main.rs"] {
        assert!(!objc2_allowed(bad), "forbidden path accepted: {bad}");
    }
    assert_eq!(rel_string(&Path::new("ui").join("platform").join("macos.rs")), "ui/platform/macos.rs");
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
        if c.contains("objc2") && !objc2_allowed(&f) {
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
    // D3 N9: code only; prose in string literals ("…boxes are equal in size") is not geometry.
    let mut bad = Vec::new();
    for (f, c) in code("model.rs") {
        let c = strip_string_literals(&c);
        for n in ["f32", "f64", "width", "px"] {
            if c.contains(n) {
                bad.push(format!("src/{f} uses {n}"));
            }
        }
    }
    assert!(bad.is_empty(), "pixel geometry in the model:\n{}", bad.join("\n"));
}

/// `code` with the contents of every "…" string literal removed (escapes honoured; char
/// literals such as '"' are kept as they are).
fn strip_string_literals(code: &str) -> String {
    let mut out = String::with_capacity(code.len());
    let mut chars = code.chars().peekable();
    let mut in_str = false;
    let mut prev = '\0';
    while let Some(ch) = chars.next() {
        if in_str {
            match ch {
                '\\' => {
                    chars.next();
                }
                '"' => {
                    in_str = false;
                    out.push('"');
                }
                _ => {}
            }
        } else if ch == '"' && !(prev == '\'' && chars.peek() == Some(&'\'')) {
            in_str = true;
            out.push('"');
        } else {
            out.push(ch);
        }
        prev = ch;
    }
    out
}

#[test]
fn string_literals_are_stripped_but_code_is_kept() {
    assert_eq!(strip_string_literals(r#"let w = "width \" px"; f32"#), r#"let w = ""; f32"#);
    assert_eq!(strip_string_literals("let q = '\"'; width"), "let q = '\"'; width");
}

/// 0026 D3 (architect §6.3): one WAL lifecycle rule, `WalLifecycle::of`; app and ui never
/// compare a WAL number against a CF log number themselves.
#[test]
fn wal_lifecycle_has_one_rule() {
    require("model.rs", "pub fn of(number: Option<u64>");
    forbid_in_prod(&["app", "ui"], &["per_cf.get("]);
}

/// 0026 D3 (architect §6.4): the UI renders map facts from `app::map_facts`; it never builds a
/// fact, decides a lifecycle or a CF membership itself.
#[test]
fn ui_builds_no_map_facts() {
    require("ui/panels/lsm_map.rs", "map_facts::");
    forbid_in_prod(&["ui"], &["MapFact {", "WalLifecycle::of", "summary.membership("]);
}

/// Every request variant has a kind-correct Failed reply (and I/O requests a Cancelled one).
#[test]
fn every_request_has_a_matching_failed_arm() {
    let jobs = std::fs::read_to_string(src().join("app/jobs/mod.rs")).unwrap();
    let variants = |enum_name: &str| -> Vec<String> {
        let start = jobs.find(&format!("pub enum {enum_name} {{")).unwrap_or_else(|| panic!("enum {enum_name}"));
        let body = &jobs[start..start + jobs[start..].find("\n}").unwrap()];
        // Variant lines only (enum-body indent; rustfmt may put struct-variant fields on their own lines).
        body.lines()
            .skip(1)
            .filter(|l| l.starts_with("    ") && !l.starts_with("     "))
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
    assert_eq!(ios, ["Resolve", "Scan", "Page", "Copy", "Stat", "WalSummaries"]);
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
    let comps = [
        ("app/session.rs", "DocumentSession"),
        ("app/selection.rs", "SelectionState"),
        ("app/search.rs", "SearchState"),
        ("app/job_state.rs", "JobState"),
        ("app/view_prefs.rs", "ViewPreferences"),
        ("app/state.rs", "AppState"),
        ("app/hex.rs", "HexState"),
        ("app/navigation.rs", "Navigation"),
        ("app/clipboard.rs", "ClipboardState"),
    ];
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
    let bad: Vec<String> =
        code("").into_iter().filter(|(f, c)| !rel_matches(f, "arch_tests.rs") && (c.contains("LegacyFields") || c.contains("impl std::ops::Deref for AppState"))).map(|(f, _)| f).collect();
    assert!(bad.is_empty(), "legacy shim still referenced: {bad:?}");
}

/// Production code of the .rs files under `rel`: comment-free (`code`), each file cut at its
/// first inline `#[cfg(test)] mod … {` module; whole test files (`*_tests.rs`) are skipped.
fn prod_code(rel: &str) -> Vec<(String, String)> {
    code(rel)
        .into_iter()
        .filter(|(f, _)| !f.ends_with("_tests.rs"))
        .map(|(f, c)| {
            let lines: Vec<&str> = c.lines().collect();
            let cut = (0..lines.len())
                .find(|&i| lines[i].trim() == "#[cfg(test)]" && lines.get(i + 1).is_some_and(|n| n.trim_start().starts_with("mod ") && n.trim_end().ends_with('{')))
                .unwrap_or(lines.len());
            (f, lines[..cut].join("\n"))
        })
        .collect()
}

fn forbid_in_prod(rels: &[&str], needles: &[&str]) {
    let mut bad = Vec::new();
    for rel in rels {
        for (f, c) in prod_code(rel) {
            bad.extend(needles.iter().filter(|n| c.contains(*n)).map(|n| format!("src/{f} uses {n}")));
        }
    }
    assert!(bad.is_empty(), "forbidden in production code:\n{}", bad.join("\n"));
}

#[test]
fn prod_code_cuts_inline_test_modules_only() {
    let hex = prod_code("app/hex.rs");
    assert!(hex[0].1.contains("fn reveal(") && !hex[0].1.contains("fn reveal_table"), "inline test module cut");
    let state = prod_code("app/state.rs");
    assert!(state[0].1.contains("fn copy_notice"), "a `#[cfg(test)] #[path] mod x;` declaration does not cut the file");
    assert!(prod_code("app").iter().all(|(f, _)| !f.ends_with("state_tests.rs")));
}

/// 0020 §6: rendering and the pure app services never do I/O or issue jobs. Data types of
/// `app::jobs` (Outcome, SelectHint, ViewTarget, parse_query) are allowed; the worker, the I/O
/// backend and the byte search are not.
#[test]
fn rendering_and_pure_services_do_no_io() {
    require("app/jobs/io_pool.rs", "RangeReader");
    forbid_in_prod(
        &[
            "ui/panels",
            "ui/layout.rs",
            "ui/menu.rs",
            "app/provenance.rs",
            "app/diagnosis.rs",
            "app/search.rs",
            "app/explanation.rs",
            "app/clipboard.rs",
            "app/navigation.rs",
            "app/lsm_map.rs",
            "app/map_facts.rs",
        ],
        &["std::fs", "crate::io", "RangeReader", "std::process", "Worker", "JobState", "IoRequest", "IoBackend", "FsBackend", "search_file", "jobs::Request"],
    );
}

/// 0020 §6: read-only — no production code creates, writes, renames or removes files; the one
/// file opener opens for reading.
#[test]
fn production_code_never_writes_files() {
    require("io/file.rs", "OpenOptions::new().read(true)");
    let mut bad = Vec::new();
    for (f, c) in prod_code("") {
        if rel_matches(&f, "arch_tests.rs") {
            continue;
        }
        for n in ["File::create", "fs::write", "remove_file", "fs::rename", "create_dir", ".write(true)", ".append(true)"] {
            if c.contains(n) {
                bad.push(format!("src/{f} uses {n}"));
            }
        }
    }
    assert!(bad.is_empty(), "file writes in production code:\n{}", bad.join("\n"));
}

/// 0024 §5: every app-owned tooltip in src/ui goes through the one gate (`panels::TipExt`,
/// View › Show Tooltips); only the gate itself calls egui's hover-text API.
#[test]
fn tooltips_go_through_the_one_gate() {
    require("ui/panels/mod.rs", "impl TipExt for egui::Response");
    let mut bad = Vec::new();
    // The gate itself (`impl TipExt`, ui/panels/mod.rs) shows every tooltip through egui's
    // `Tooltip` with the theme's tooltip frame (0026 §3: `show_tip`); nothing else in src/ui
    // calls any hover-tooltip API, and the gate no longer uses the frame-less hover helpers.
    require("ui/panels/mod.rs", "fn show_tip(");
    require("ui/panels/mod.rs", ".truncate().show_tooltip_when_elided(false)");
    let gate: &[(&str, usize)] = &[("egui::Tooltip", usize::MAX), ("show_tooltip", 1), (".truncate()", 1)];
    for (f, c) in prod_code("ui") {
        for n in [
            // 0030 B: egui's own elided-text tooltip was a second popup beside the gate's; the
            // gate's `Elided` is the only truncated label (it turns egui's tooltip off once).
            ".truncate()",
            ".on_hover_text(",
            ".on_disabled_hover_text(",
            ".on_hover_ui(",
            ".on_hover_ui_at_pointer(",
            ".on_hover_text_at_pointer(",
            ".on_disabled_hover_ui(",
            "show_tooltip",
            "show_tooltip_text",
            "egui::Tooltip",
        ] {
            let allowed = if rel_matches(&f, "ui/panels/mod.rs") { gate.iter().find(|g| g.0 == n).map(|g| g.1).unwrap_or(0) } else { 0 };
            if c.matches(n).count() > allowed {
                bad.push(format!("src/{f} uses {n}"));
            }
        }
    }
    assert!(bad.is_empty(), "tooltips outside the gate:\n{}", bad.join("\n"));
}

/// 0024 D2 (architect should-fix 1): the app layer never branches on display labels or parses
/// presenter strings back (typed fields such as `View::lsm_blocked` instead).
#[test]
fn app_does_not_branch_on_labels() {
    require("app/session.rs", "lsm_blocked");
    forbid_in_prod(&["app"], &[".name ==", "name.as_str()", "name.starts_with(", "trim_start_matches(\"Layout", "value.split_whitespace", "value.parse"]);
}

/// 0024 §8: the map and tooltips did not grow `AppState` (five components); the map state lives
/// in the session, the tooltips flag in `ViewPreferences`, order/range logic in the inventory.
#[test]
fn app_state_keeps_five_components() {
    let text = std::fs::read_to_string(src().join("app/state.rs")).unwrap();
    let start = text.find("pub struct AppState {").unwrap();
    let body = &text[start..start + text[start..].find("\n}").unwrap()];
    assert_eq!(body.lines().skip(1).filter(|l| !l.trim().is_empty() && !l.trim().starts_with("//")).count(), 5, "{body}");
    require("app/session.rs", "map: MapState");
    require("app/view_prefs.rs", "tooltips: bool");
    // 0026 D3 (architect §6.2): the WAL inventory lives in the session, the map split next to
    // the centre split.
    require("app/session.rs", "wals: Option<WalInventory>");
    require("app/view_prefs.rs", "map_split: Option<f32>");
    // 0030 (architect §5.1): the levels table's split next to the others, the lane tier in the
    // map state and in its snapshot (Back restores it).
    require("app/view_prefs.rs", "levels_split: Option<f32>");
    require("app/lsm_map.rs", "tier: LaneTier,");
    require("app/lsm_map.rs", "tier: self.tier }");
    forbid_in_prod(&["app/lsm_map.rs", "ui/panels/lsm_map.rs"], &["binary_search", "sort_unstable", "ikey", "user_key", "from_comparator"]);
}

/// 0030 A7 (architect §5.2): the SST's MANIFEST context is read only by `inspect` (in the decode
/// job); nothing else looks for a sibling CURRENT.
#[test]
fn sst_manifest_context_only_in_the_decode_job() {
    require("inspect.rs", "pub fn sst_manifest_context(");
    forbid_in_prod(&["app", "ui", "present", "sst", "model.rs"], &["sst_manifest_context(", "join(\"CURRENT\")"]);
}

/// 0030 A1/A3 (architect §5.3): the map camera policy (Fit's tier, zoom to a density bar, the
/// tier after a zoom) lives in `app::lsm_map`; the painter only calls it.
#[test]
fn map_camera_policy_lives_in_app() {
    require("app/lsm_map.rs", "pub fn zoom_to_bar(");
    require("app/lsm_map.rs", "pub fn reveal_zoom(");
    require("app/lsm_map.rs", "pub fn tier_after_zoom(");
    forbid_in_prod(&["ui"], &["fn zoom_to_bar", "fn reveal_zoom", "fit_tier(", "LaneTier::Full } else"]);
}

/// Presenters and the UI select behaviour by `FieldKind`/`DecodedValue`, not by display labels
/// or by parsing their own strings.
#[test]
fn presenters_do_not_branch_on_labels() {
    require("present", "FieldKind");
    forbid(&["present", "ui"], &["name.as_str()", ".name ==", "name.starts_with(", "value.split_whitespace", "value.parse"]);
}
