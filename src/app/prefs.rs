//! Persisted user preferences (theme id, zoom, tooltips) and their string encoding. The storage
//! itself (eframe's per-user storage) is adapted in `ui`; prefs never live in inspected folders.

/// Stable id of the default theme, XCopy (the UI theme registry falls back to it for unknown ids).
pub const DEFAULT_THEME: &str = "xcopy";
pub const KEY_THEME: &str = "theme";
pub const KEY_ZOOM: &str = "zoom";
/// View › Show Tooltips ("true" / "false"; default on).
pub const KEY_TOOLTIPS: &str = "tooltips";

#[derive(Clone, Debug, PartialEq)]
pub struct Prefs {
    pub theme: String,
    pub zoom: f32,
    pub tooltips: bool,
}

impl Default for Prefs {
    fn default() -> Self {
        Prefs { theme: DEFAULT_THEME.to_string(), zoom: 1.0, tooltips: true }
    }
}

impl Prefs {
    /// Reads preferences through `get(key)`; missing or invalid values use defaults
    /// (the zoom is clamped by the caller's zoom policy). A stored theme id is kept verbatim;
    /// the UI resolves unknown ids to `DEFAULT_THEME` through its theme registry.
    pub fn load(get: impl Fn(&str) -> Option<String>) -> Self {
        let theme = get(KEY_THEME).unwrap_or_else(|| DEFAULT_THEME.to_string());
        let zoom = get(KEY_ZOOM).and_then(|z| z.parse::<f32>().ok()).filter(|z| z.is_finite()).unwrap_or(1.0);
        let tooltips = get(KEY_TOOLTIPS).map(|t| t.trim() != "false").unwrap_or(true);
        Prefs { theme, zoom, tooltips }
    }

    pub fn store(&self, mut set: impl FnMut(&str, String)) {
        set(KEY_THEME, self.theme.clone());
        set(KEY_ZOOM, format!("{}", self.zoom));
        set(KEY_TOOLTIPS, self.tooltips.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(p: &Prefs) -> Prefs {
        let mut kv = Vec::new();
        p.store(|k, v| kv.push((k.to_string(), v)));
        Prefs::load(|k| kv.iter().find(|(kk, _)| kk == k).map(|(_, v)| v.clone()))
    }

    #[test]
    fn prefs_roundtrip_and_defaults() {
        let p = Prefs::load(|_| None);
        assert_eq!(p, Prefs { theme: DEFAULT_THEME.into(), zoom: 1.0, tooltips: true });
        assert_eq!(p, Prefs::default());
        let bad = Prefs::load(|k| Some(if k == KEY_ZOOM { "NaN".into() } else { "matrix".into() }));
        assert_eq!(bad, Prefs { theme: "matrix".into(), zoom: 1.0, tooltips: true }, "an unreadable tooltips value keeps them on");
        let p = Prefs { theme: "wooden".into(), zoom: 1.3, tooltips: true };
        assert_eq!(roundtrip(&p), p);
    }

    /// 0024 §5: Show Tooltips defaults on, an off choice survives store/load (a restart).
    #[test]
    fn tooltips_pref_roundtrip() {
        assert!(Prefs::load(|_| None).tooltips);
        let off = Prefs { tooltips: false, ..Prefs::default() };
        assert!(!roundtrip(&off).tooltips);
        assert!(!Prefs::load(|k| (k == KEY_TOOLTIPS).then(|| "false".into())).tooltips);
        assert!(Prefs::load(|k| (k == KEY_TOOLTIPS).then(|| "true".into())).tooltips);
    }

    /// 0017 A: XCopy is the default only when nothing is stored; a saved choice survives a
    /// store/load roundtrip and an unknown id is kept verbatim (the UI registry resolves it).
    #[test]
    fn default_theme_is_xcopy_and_saved_choice_survives() {
        assert_eq!(DEFAULT_THEME, "xcopy");
        assert_eq!(Prefs::load(|_| None).theme, "xcopy");
        assert_eq!(Prefs::load(|k| (k == KEY_ZOOM).then(|| "1.2".into())).theme, "xcopy");
        assert_eq!(roundtrip(&Prefs { theme: "matrix".into(), ..Prefs::default() }).theme, "matrix");
        assert_eq!(Prefs::load(|k| (k == KEY_THEME).then(|| "no-such-theme".into())).theme, "no-such-theme");
    }
}
