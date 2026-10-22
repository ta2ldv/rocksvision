//! Persisted user preferences (theme id + zoom) and their string encoding. The storage itself
//! (eframe's per-user storage) is adapted in `ui`; prefs never live in inspected folders.

/// Stable id of the default theme (the UI theme registry falls back to it for unknown ids).
pub const DEFAULT_THEME: &str = "autistic";
pub const KEY_THEME: &str = "theme";
pub const KEY_ZOOM: &str = "zoom";

#[derive(Clone, Debug, PartialEq)]
pub struct Prefs {
    pub theme: String,
    pub zoom: f32,
}

impl Prefs {
    /// Reads preferences through `get(key)`; missing or invalid values use defaults
    /// (the zoom is clamped by the caller's zoom policy).
    pub fn load(get: impl Fn(&str) -> Option<String>) -> Self {
        let theme = get(KEY_THEME).unwrap_or_else(|| DEFAULT_THEME.to_string());
        let zoom = get(KEY_ZOOM).and_then(|z| z.parse::<f32>().ok()).filter(|z| z.is_finite()).unwrap_or(1.0);
        Prefs { theme, zoom }
    }

    pub fn store(&self, mut set: impl FnMut(&str, String)) {
        set(KEY_THEME, self.theme.clone());
        set(KEY_ZOOM, format!("{}", self.zoom));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefs_roundtrip_and_defaults() {
        let p = Prefs::load(|_| None);
        assert_eq!(p, Prefs { theme: DEFAULT_THEME.into(), zoom: 1.0 });
        let bad = Prefs::load(|k| Some(if k == KEY_ZOOM { "NaN".into() } else { "matrix".into() }));
        assert_eq!(bad, Prefs { theme: "matrix".into(), zoom: 1.0 });
        let mut kv = Vec::new();
        Prefs { theme: "wooden".into(), zoom: 1.3 }.store(|k, v| kv.push((k.to_string(), v)));
        let back = Prefs::load(|k| kv.iter().find(|(kk, _)| kk == k).map(|(_, v)| v.clone()));
        assert_eq!(back, Prefs { theme: "wooden".into(), zoom: 1.3 });
    }
}
