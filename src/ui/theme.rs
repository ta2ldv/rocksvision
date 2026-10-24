//! Theme registry and resolved visual tokens. A theme is pure data (`ThemeDefinition`):
//! semantic palette, typography, widget styling and an optional surface decoration, addressed by
//! a stable id. `apply_theme` maps the active definition onto egui `Style`/`Visuals` once (on
//! change) and returns the resolved `Tokens` the renderers read. Renderers never use RGB
//! literals or theme-id conditionals; all color literals of the app live in the registry below.
//! Layout geometry lives in `layout.rs`, operational limits in `crate::limits`.

use crate::model::Status;
use eframe::egui::{self, Color32, CornerRadius, FontFamily, FontId, Stroke, TextStyle};

/// Shims for code written against the former combined module.
pub use super::layout::{LayoutDefaults, LAYOUT};
pub use crate::app::prefs::DEFAULT_THEME;
pub use crate::limits::{Limits, LIMITS};

// ---------------------------------------------------------------- theme data

#[derive(Clone, Copy, Debug)]
pub struct StatusColors {
    pub present: Color32,
    pub not_present: Color32,
    pub implicit: Color32,
    pub unsupported: Color32,
    pub unknown: Color32,
    pub invalid: Color32,
    pub truncated: Color32,
    pub limited: Color32,
}

impl StatusColors {
    pub fn of(&self, s: Status) -> Color32 {
        match s {
            Status::Present => self.present,
            Status::NotPresent => self.not_present,
            Status::Implicit => self.implicit,
            Status::Unsupported => self.unsupported,
            Status::Unknown => self.unknown,
            Status::Invalid => self.invalid,
            Status::Truncated => self.truncated,
            Status::Limited => self.limited,
        }
    }
    pub fn all(&self) -> [Color32; 8] {
        [self.present, self.not_present, self.implicit, self.unsupported, self.unknown, self.invalid, self.truncated, self.limited]
    }
}

/// Semantic color roles. Status text is drawn only on flat surfaces: data, stripes, panel,
/// explain (chips get a `data` fill), never on toolbar/canvas chrome.
#[derive(Clone, Copy, Debug)]
pub struct Palette {
    // surfaces
    pub canvas: Color32,
    pub panel: Color32,
    pub explain: Color32,
    pub toolbar: Color32,
    pub header_bg: Color32,
    pub input: Color32,
    pub menu: Color32,
    pub data: Color32,
    pub stripe_a: Color32,
    pub stripe_b: Color32,
    // text
    pub text: Color32,
    pub text_muted: Color32,
    pub text_disabled: Color32,
    pub header_fg: Color32,
    /// Accent for lines/marks (selected-row stripe, swatches).
    pub accent: Color32,
    /// Accent usable as text on panel/explain/data.
    pub accent_text: Color32,
    /// Section headings/captions (≥4.5:1 on canvas, panel, explain, toolbar and data).
    pub heading: Color32,
    // lines / interaction
    pub border: Color32,
    pub border_control: Color32,
    pub focus: Color32,
    /// Row hover fill (`row_hover`): tables, lists; subtler than `button_hover`.
    pub hover_bg: Color32,
    /// Button/menu-button hover fill. Derived rule: `widgets.button_bg` moved ~22 % toward
    /// `accent` (dark themes) or toward black (light themes), step raised until it differs
    /// from `toolbar` by ≥1.5:1 with text ≥4.5:1.
    pub button_hover: Color32,
    /// Pressed button fill (`button_active`), drawn with `active_fg`.
    pub active_bg: Color32,
    pub active_fg: Color32,
    /// Hover/focus outline of buttons and rows (≥3:1 on `button_hover` and `toolbar`).
    pub outline: Color32,
    // selection / search
    pub sel_bg: Color32,
    pub sel_fg: Color32,
    pub sel_edge: Color32,
    pub match_bg: Color32,
    pub match_fg: Color32,
    // hex
    pub hex_byte: Color32,
    pub hex_ascii: Color32,
    pub hex_offset: Color32,
    pub hex_file_space: Color32,
    pub hex_derived_space: Color32,
    pub status: StatusColors,
    // misc
    pub scroll_track: Color32,
    pub scroll_thumb: Color32,
    pub scroll_thumb_hover: Color32,
    pub lsm_bg: Color32,
    pub lsm_present: Color32,
    pub lsm_missing: Color32,
}

/// Font family ids ("proportional" / "monospace" = egui's bundled families) and sizes in points.
/// User zoom multiplies them (egui zoom factor). Future font assets register a new family id here.
#[derive(Clone, Copy, Debug)]
pub struct Typography {
    pub ui_family: &'static str,
    pub mono_family: &'static str,
    pub body: f32,
    pub mono: f32,
    pub small: f32,
    pub header: f32,
    pub heading: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bevel {
    Flat,
    /// Light top/left edge, dark bottom/right edge (Amiga style).
    Raised,
}

#[derive(Clone, Copy, Debug)]
pub struct WidgetStyle {
    pub rounding: u8,
    pub border_width: f32,
    pub bevel: Bevel,
    pub bevel_light: Color32,
    pub bevel_dark: Color32,
    pub button_bg: Color32,
    /// Scrollbar width in points.
    pub scroll_bar_width: f32,
    /// A 1 px `bevel_dark` line under table/hex headers (separates them from selected rows).
    pub header_rule: bool,
    /// Line between the side panels and the center (Wooden: 2 px brass inlay).
    pub separator: Stroke,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decoration {
    None,
    /// Cached procedural wood grain on chrome surfaces (toolbar, status bar, headers) only.
    WoodGrain,
}

#[derive(Clone, Copy, Debug)]
pub struct ThemeDefinition {
    pub id: &'static str,
    pub name: &'static str,
    pub palette: Palette,
    pub typography: Typography,
    pub widgets: WidgetStyle,
    pub decoration: Decoration,
}

const fn c(v: u32) -> Color32 {
    Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

const fn sep(width: f32, v: u32) -> Stroke {
    Stroke { width, color: c(v) }
}

const TYPE_UI: Typography = Typography { ui_family: "proportional", mono_family: "monospace", body: 14.5, mono: 13.5, small: 12.0, header: 15.5, heading: 18.5 };
const TYPE_MONO_UI: Typography = Typography { ui_family: "monospace", mono_family: "monospace", body: 14.0, mono: 13.5, small: 12.0, header: 15.0, heading: 18.0 };

/// Status set for dark surfaces (UI expert checklist, ≥4.5:1 on the darkest stripes).
const STATUS_DARK: StatusColors = StatusColors {
    present: c(0x9FE870),
    not_present: c(0xA6A09A),
    implicit: c(0x6FA8FF),
    unsupported: c(0xF2B13C),
    unknown: c(0xC79BFF),
    invalid: c(0xFF7777),
    truncated: c(0xFF9566),
    limited: c(0xF08BB8),
};

pub static THEMES: &[ThemeDefinition] = &[
    ThemeDefinition {
        id: "autistic",
        name: "Autistic",
        palette: Palette {
            canvas: c(0x252424),
            panel: c(0x2B2927),
            explain: c(0x2B2927),
            toolbar: c(0x35312E),
            header_bg: c(0x35312E),
            input: c(0x1E1D1D),
            menu: c(0x35312E),
            data: c(0x252424),
            stripe_a: c(0x292725),
            stripe_b: c(0x302D2B),
            text: c(0xE8E3DD),
            text_muted: c(0xBDB2A7),
            text_disabled: c(0x857B71),
            header_fg: c(0xE8E3DD),
            accent: c(0xA8876B),
            accent_text: c(0xC39B7B),
            heading: c(0xC39B7B),
            border: c(0x51473F),
            border_control: c(0x928374),
            focus: c(0xA8876B),
            outline: c(0xC39B7B),
            hover_bg: c(0x3F3A36),
            button_hover: c(0x584A3E),
            active_bg: c(0x6A5644),
            active_fg: c(0xFFFFFF),
            sel_bg: c(0x735A45),
            sel_fg: c(0xFFFFFF),
            sel_edge: c(0xA8876B),
            match_bg: c(0xE6A828),
            match_fg: c(0x1A1208),
            hex_byte: c(0xE8E3DD),
            hex_ascii: c(0xBDB2A7),
            hex_offset: c(0xC39B7B),
            hex_file_space: c(0x8FA3BC),
            hex_derived_space: c(0xC39B7B),
            status: StatusColors { present: c(0xE8E3DD), ..STATUS_DARK },
            scroll_track: c(0x1F1E1D),
            scroll_thumb: c(0x7A6C60),
            scroll_thumb_hover: c(0xA8876B),
            lsm_bg: c(0x2B2927),
            lsm_present: c(0xC39B7B),
            lsm_missing: c(0x857B71),
        },
        typography: TYPE_UI,
        widgets: WidgetStyle { rounding: 3, border_width: 1.0, bevel: Bevel::Flat, bevel_light: c(0x51473F), bevel_dark: c(0x1E1D1D), button_bg: c(0x3E3731), scroll_bar_width: 10.0, header_rule: false, separator: sep(1.0, 0x51473F) },
        decoration: Decoration::None,
    },
    ThemeDefinition {
        id: "matrix",
        name: "Matrix",
        palette: Palette {
            canvas: c(0x000000),
            panel: c(0x071007),
            explain: c(0x071007),
            toolbar: c(0x0B1A0B),
            header_bg: c(0x0B1A0B),
            input: c(0x000000),
            menu: c(0x071007),
            data: c(0x000000),
            stripe_a: c(0x000000),
            stripe_b: c(0x0A160A),
            text: c(0xB6FF00),
            text_muted: c(0x72B34B),
            text_disabled: c(0x3F6B2A),
            header_fg: c(0xB6FF00),
            accent: c(0xB6FF00),
            accent_text: c(0xB6FF00),
            heading: c(0xB6FF00),
            border: c(0x1E4A14),
            border_control: c(0x3E8F2A),
            focus: c(0x3E8F2A),
            outline: c(0xB6FF00),
            hover_bg: c(0x12300F),
            button_hover: c(0x2E4505),
            active_bg: c(0x1E4A14),
            active_fg: c(0xB6FF00),
            sel_bg: c(0xB6FF00),
            sel_fg: c(0x071007),
            sel_edge: c(0xB6FF00),
            match_bg: c(0xFFB000),
            match_fg: c(0x000000),
            hex_byte: c(0xB6FF00),
            hex_ascii: c(0x72B34B),
            hex_offset: c(0x72B34B),
            hex_file_space: c(0x72B34B),
            hex_derived_space: c(0xFFB000),
            status: StatusColors { present: c(0xB6FF00), ..STATUS_DARK },
            scroll_track: c(0x071007),
            scroll_thumb: c(0x3E8F2A),
            scroll_thumb_hover: c(0x72B34B),
            lsm_bg: c(0x071007),
            lsm_present: c(0xB6FF00),
            lsm_missing: c(0x3E8F2A),
        },
        typography: TYPE_MONO_UI,
        widgets: WidgetStyle { rounding: 0, border_width: 1.0, bevel: Bevel::Flat, bevel_light: c(0x3E8F2A), bevel_dark: c(0x000000), button_bg: c(0x071007), scroll_bar_width: 10.0, header_rule: false, separator: sep(1.0, 0x1E4A14) },
        decoration: Decoration::None,
    },
    ThemeDefinition {
        id: "amiga-workbench",
        name: "Amiga Workbench",
        palette: Palette {
            canvas: c(0x2454A4),
            panel: c(0x1D4387),
            explain: c(0x1D4387),
            toolbar: c(0x2454A4),
            header_bg: c(0x1D4387),
            input: c(0x0F2A5A),
            menu: c(0x1D4387),
            data: c(0x0F2A5A),
            stripe_a: c(0x0F2A5A),
            stripe_b: c(0x153470),
            text: c(0xFFFFFF),
            text_muted: c(0xC6D8F4),
            text_disabled: c(0x8FA8D4),
            header_fg: c(0xFFFFFF),
            accent: c(0xFFAA33),
            accent_text: c(0xFFAA33),
            heading: c(0xFFFFFF),
            border: c(0x0B1F4A),
            border_control: c(0xFFFFFF),
            focus: c(0xFFAA33),
            outline: c(0xFFAA33),
            hover_bg: c(0x0A1A40),
            button_hover: c(0x10306A),
            active_bg: c(0xFFAA33),
            active_fg: c(0x111111),
            sel_bg: c(0xFFAA33),
            sel_fg: c(0x111111),
            sel_edge: c(0xFFFFFF),
            match_bg: c(0xFFFFFF),
            match_fg: c(0x0F2A5A),
            hex_byte: c(0xFFFFFF),
            hex_ascii: c(0xC6D8F4),
            hex_offset: c(0xFFAA33),
            hex_file_space: c(0x9CC4FF),
            hex_derived_space: c(0xFFD27A),
            status: StatusColors {
                present: c(0x9FE870),
                not_present: c(0xB8C8E0),
                implicit: c(0x9CC4FF),
                unsupported: c(0xFFC24D),
                unknown: c(0xD9B8FF),
                invalid: c(0xFF9A9A),
                truncated: c(0xFFB38A),
                limited: c(0xFFA6D0),
            },
            scroll_track: c(0x0F2A5A),
            scroll_thumb: c(0xFFAA33),
            scroll_thumb_hover: c(0xFFD27A),
            lsm_bg: c(0x0F2A5A),
            lsm_present: c(0xFFAA33),
            lsm_missing: c(0x8FA8D4),
        },
        typography: TYPE_UI,
        widgets: WidgetStyle { rounding: 0, border_width: 2.0, bevel: Bevel::Raised, bevel_light: c(0xFFFFFF), bevel_dark: c(0x0B1F4A), button_bg: c(0x2454A4), scroll_bar_width: 6.0, header_rule: false, separator: sep(1.0, 0x0B1F4A) },
        decoration: Decoration::None,
    },
    ThemeDefinition {
        id: "amiga-tool-style",
        name: "Amiga Tool Style",
        palette: Palette {
            canvas: c(0xB8BCBB),
            panel: c(0xD6D9D7),
            explain: c(0xD6D9D7),
            toolbar: c(0xB8BCBB),
            header_bg: c(0x9AA1A0),
            input: c(0xF2F4F3),
            menu: c(0xD6D9D7),
            data: c(0xD6D9D7),
            stripe_a: c(0xD6D9D7),
            stripe_b: c(0xCDD1CF),
            text: c(0x182021),
            text_muted: c(0x3D494C),
            text_disabled: c(0x7C8789),
            header_fg: c(0x182021),
            accent: c(0x2454A4),
            accent_text: c(0x2454A4),
            heading: c(0x182021),
            border: c(0x9AA1A0),
            border_control: c(0x4F5A5D),
            focus: c(0x2454A4),
            outline: c(0x163F85),
            hover_bg: c(0xE4E7E5),
            button_hover: c(0x949796),
            active_bg: c(0x2454A4),
            active_fg: c(0xFFFFFF),
            sel_bg: c(0x2454A4),
            sel_fg: c(0xFFFFFF),
            sel_edge: c(0x163F85),
            match_bg: c(0xFFE066),
            match_fg: c(0x182021),
            hex_byte: c(0x182021),
            hex_ascii: c(0x3D494C),
            hex_offset: c(0x2454A4),
            hex_file_space: c(0x3D494C),
            hex_derived_space: c(0x8A4B12),
            status: StatusColors {
                present: c(0x14521F),
                not_present: c(0x3D494C),
                implicit: c(0x163F85),
                unsupported: c(0x6B3F00),
                unknown: c(0x5A2D91),
                invalid: c(0x8C0A0A),
                truncated: c(0x7E2600),
                limited: c(0x82185A),
            },
            scroll_track: c(0xCDD1CF),
            scroll_thumb: c(0x4F5A5D),
            scroll_thumb_hover: c(0x2454A4),
            lsm_bg: c(0xF2F4F3),
            lsm_present: c(0x2454A4),
            lsm_missing: c(0x4F5A5D),
        },
        typography: TYPE_UI,
        widgets: WidgetStyle { rounding: 0, border_width: 1.0, bevel: Bevel::Raised, bevel_light: c(0xFFFFFF), bevel_dark: c(0x4F5A5D), button_bg: c(0xC8CCCB), scroll_bar_width: 10.0, header_rule: true, separator: sep(1.0, 0x9AA1A0) },
        decoration: Decoration::None,
    },
    ThemeDefinition {
        id: "xcopy",
        name: "XCopy",
        palette: Palette {
            canvas: c(0x000000),
            panel: c(0x101A1E),
            explain: c(0x101A1E),
            toolbar: c(0x0C1519),
            header_bg: c(0x0C1519),
            input: c(0x000000),
            menu: c(0x101A1E),
            data: c(0x000000),
            stripe_a: c(0x000000),
            stripe_b: c(0x0A1418),
            text: c(0xC5E3E6),
            text_muted: c(0x8BB4BB),
            text_disabled: c(0x527A83),
            header_fg: c(0xC5E3E6),
            accent: c(0xFFDD55),
            accent_text: c(0xFFDD55),
            heading: c(0xFFDD55),
            border: c(0x527A83),
            border_control: c(0x527A83),
            focus: c(0xFFDD55),
            outline: c(0xFFDD55),
            hover_bg: c(0x17303A),
            button_hover: c(0x45452A),
            active_bg: c(0xFFDD55),
            active_fg: c(0x081014),
            sel_bg: c(0x8BBFC8),
            sel_fg: c(0x081014),
            sel_edge: c(0xC5E3E6),
            match_bg: c(0xFFDD55),
            match_fg: c(0x081014),
            hex_byte: c(0xC5E3E6),
            hex_ascii: c(0x8BB4BB),
            hex_offset: c(0x8BB4BB),
            hex_file_space: c(0x8BBFC8),
            hex_derived_space: c(0xFFDD55),
            status: StatusColors { present: c(0x9FE870), ..STATUS_DARK },
            scroll_track: c(0x101A1E),
            scroll_thumb: c(0x527A83),
            scroll_thumb_hover: c(0x8BBFC8),
            lsm_bg: c(0x101A1E),
            lsm_present: c(0x8BBFC8),
            lsm_missing: c(0x527A83),
        },
        typography: TYPE_UI,
        widgets: WidgetStyle { rounding: 0, border_width: 1.0, bevel: Bevel::Raised, bevel_light: c(0xC5E3E6), bevel_dark: c(0x1E3238), button_bg: c(0x101A1E), scroll_bar_width: 10.0, header_rule: false, separator: sep(1.0, 0x527A83) },
        decoration: Decoration::None,
    },
    ThemeDefinition {
        id: "wooden",
        name: "Wooden",
        palette: Palette {
            canvas: c(0x211A15),
            panel: c(0x2A2018),
            explain: c(0x2A2018),
            toolbar: c(0x503724),
            header_bg: c(0x42301F),
            input: c(0x17120D),
            menu: c(0x42301F),
            data: c(0x211A15),
            stripe_a: c(0x211A15),
            stripe_b: c(0x2A2018),
            text: c(0xF2E4C9),
            text_muted: c(0xC7AF8B),
            text_disabled: c(0x8A7458),
            header_fg: c(0xF2E4C9),
            accent: c(0xD4AE6A),
            accent_text: c(0xD4AE6A),
            heading: c(0xD4AE6A),
            border: c(0x17120D),
            border_control: c(0xA8854F),
            focus: c(0xD4AE6A),
            outline: c(0xD4AE6A),
            hover_bg: c(0x583C28),
            button_hover: c(0x6B5334),
            active_bg: c(0xB68B4C),
            active_fg: c(0x17120D),
            sel_bg: c(0xB68B4C),
            sel_fg: c(0x17120D),
            sel_edge: c(0xD4AE6A),
            match_bg: c(0xF2E4C9),
            match_fg: c(0x17120D),
            hex_byte: c(0xF2E4C9),
            hex_ascii: c(0xC7AF8B),
            hex_offset: c(0xD4AE6A),
            hex_file_space: c(0xC7AF8B),
            hex_derived_space: c(0xE0925A),
            status: StatusColors { present: c(0xF2E4C9), ..STATUS_DARK },
            scroll_track: c(0x17120D),
            scroll_thumb: c(0xA8854F),
            scroll_thumb_hover: c(0xD4AE6A),
            lsm_bg: c(0x2A2018),
            lsm_present: c(0xD4AE6A),
            lsm_missing: c(0x8A7458),
        },
        typography: TYPE_UI,
        widgets: WidgetStyle { rounding: 2, border_width: 1.0, bevel: Bevel::Raised, bevel_light: c(0x8A6644), bevel_dark: c(0x17120D), button_bg: c(0x42301F), scroll_bar_width: 10.0, header_rule: false, separator: sep(2.0, 0xA8854F) },
        decoration: Decoration::WoodGrain,
    },
];

/// Every non-ASCII glyph used in UI text (status symbols, links, paging, toolbar). Each must
/// render in the bundled proportional and monospace fonts (tested); e.g. U+2192 "→" does not.
pub const UI_GLYPHS: &str = "✔○≈⚠✖✂⛔⏹›◀▶−–·…Δ📂📄⌘";

/// Looks up a theme by stable id; unknown ids fall back to Autistic.
pub fn theme_by_id(id: &str) -> &'static ThemeDefinition {
    THEMES.iter().find(|t| t.id == id).unwrap_or(&THEMES[0])
}

// ---------------------------------------------------------------- resolved tokens

/// Resolved values the renderers read (palette + metrics + decoration texture).
#[derive(Clone)]
pub struct Tokens {
    pub id: &'static str,
    pub p: Palette,
    pub ty: Typography,
    pub w: WidgetStyle,
    pub decoration: Decoration,
    pub grain: Option<egui::TextureHandle>,
}

impl Tokens {
    /// Tokens of a definition without GPU resources (decoration texture is attached by `apply_theme`).
    pub fn from_def(def: &'static ThemeDefinition) -> Self {
        Tokens { id: def.id, p: def.palette, ty: def.typography, w: def.widgets, decoration: def.decoration, grain: None }
    }
    pub fn status(&self, s: Status) -> Color32 {
        self.p.status.of(s)
    }
    /// Table row height from the active body font metrics (no clipping at larger fonts/zoom).
    pub fn row_height(&self, ui: &egui::Ui) -> f32 {
        ui.text_style_height(&TextStyle::Body).max(ui.text_style_height(&TextStyle::Monospace)) + LAYOUT.row_pad
    }
    pub fn header_height(&self, ui: &egui::Ui) -> f32 {
        ui.text_style_height(&TextStyle::Name("Header".into())) + LAYOUT.header_pad
    }
    pub fn header_font(&self) -> FontId {
        FontId::new(self.ty.header, family(self.ty.ui_family))
    }

    /// Generic surface renderer: paints the optional decoration (wood grain) and bevel edges of a
    /// chrome surface. Call first inside the surface so widgets are drawn on top. Data areas
    /// (tables, hex, explanation) are never decorated.
    pub fn paint_chrome(&self, ui: &egui::Ui, rect: egui::Rect) {
        if let (Decoration::WoodGrain, Some(t)) = (self.decoration, &self.grain) {
            let s = t.size_vec2();
            let uv = egui::Rect::from_min_max(egui::pos2(rect.left() / s.x, rect.top() / s.y), egui::pos2(rect.right() / s.x, rect.bottom() / s.y));
            ui.painter().image(t.id(), rect, uv, Color32::WHITE);
        }
        if self.w.bevel == Bevel::Raised {
            self.bevel(ui.painter(), rect);
        }
    }

    /// Wood grain only (no bevel) on a chrome strip: side-panel title lines, table/hex header
    /// rows, collapse strips and the split handle. No-op for themes without decoration. Never
    /// used on data surfaces (table body, hex rows, Explanation body).
    pub fn paint_grain(&self, painter: &egui::Painter, rect: egui::Rect) {
        if let Some(s) = self.grain_shape(rect) {
            painter.add(s);
        }
    }

    /// The grain as a shape (for a background slot reserved before the content is laid out).
    pub fn grain_shape(&self, rect: egui::Rect) -> Option<egui::Shape> {
        let (Decoration::WoodGrain, Some(t)) = (self.decoration, &self.grain) else { return None };
        let s = t.size_vec2();
        let uv = egui::Rect::from_min_max(egui::pos2(rect.left() / s.x, rect.top() / s.y), egui::pos2(rect.right() / s.x, rect.bottom() / s.y));
        Some(egui::Shape::image(t.id(), rect, uv, Color32::WHITE))
    }

    /// Table/hex header background: fill, optional grain, optional 1 px rule underneath.
    pub fn paint_header(&self, painter: &egui::Painter, rect: egui::Rect) {
        painter.rect_filled(rect, 0.0, self.p.header_bg);
        self.paint_grain(painter, rect);
        if self.w.header_rule {
            painter.line_segment([rect.left_bottom() - egui::vec2(0.0, 0.5), rect.right_bottom() - egui::vec2(0.0, 0.5)], Stroke::new(1.0, self.w.bevel_dark));
        }
    }

    /// Hover outline of a popup / menu item (Copy… popup, in-window fallback menu, context
    /// menus): the same `outline` stroke buttons get from `apply_theme`. Needed because egui's
    /// menu style (`egui::containers::menu::menu_style`, egui 0.36) sets `hovered.bg_stroke` to
    /// NONE inside popups. None for unhovered or disabled items.
    pub fn menu_item_outline(&self, hovered: bool, enabled: bool) -> Option<Stroke> {
        (hovered && enabled).then(|| Stroke::new(self.w.border_width, self.p.outline))
    }

    /// Paints `menu_item_outline` around a popup item's response (call right after adding it).
    pub fn menu_item_hover(&self, ui: &egui::Ui, resp: &egui::Response) {
        if let Some(s) = self.menu_item_outline(resp.hovered(), resp.enabled()) {
            ui.painter().rect_stroke(resp.rect, self.w.rounding, s, egui::StrokeKind::Inside);
        }
    }

    /// Light top/left, dark bottom/right edge lines (Raised bevel only).
    pub fn bevel(&self, painter: &egui::Painter, rect: egui::Rect) {
        if self.w.bevel != Bevel::Raised {
            return;
        }
        let r = rect.shrink(0.5);
        let l = Stroke::new(1.0, self.w.bevel_light);
        let d = Stroke::new(1.0, self.w.bevel_dark);
        painter.line_segment([r.left_bottom(), r.left_top()], l);
        painter.line_segment([r.left_top(), r.right_top()], l);
        painter.line_segment([r.right_top(), r.right_bottom()], d);
        painter.line_segment([r.right_bottom(), r.left_bottom()], d);
    }
}

fn family(id: &str) -> FontFamily {
    match id {
        "monospace" => FontFamily::Monospace,
        "proportional" => FontFamily::Proportional,
        other => FontFamily::Name(other.into()),
    }
}

/// Maps a theme onto egui's style and returns the resolved tokens. Call on theme change only.
/// Zoom is independent (egui zoom factor). The wood-grain texture is generated once per theme
/// id and kept in `cache`.
pub fn apply_theme(ctx: &egui::Context, def: &'static ThemeDefinition, cache: &mut Vec<(&'static str, egui::TextureHandle)>) -> Tokens {
    let p = def.palette;
    let ty = def.typography;
    let w = def.widgets;
    let rnd = CornerRadius::same(w.rounding);
    ctx.all_styles_mut(|st| {
        let ui_f = family(ty.ui_family);
        let mono_f = family(ty.mono_family);
        st.text_styles.insert(TextStyle::Body, FontId::new(ty.body, ui_f.clone()));
        st.text_styles.insert(TextStyle::Button, FontId::new(ty.body, ui_f.clone()));
        st.text_styles.insert(TextStyle::Monospace, FontId::new(ty.mono, mono_f));
        st.text_styles.insert(TextStyle::Small, FontId::new(ty.small, ui_f.clone()));
        st.text_styles.insert(TextStyle::Heading, FontId::new(ty.heading, ui_f.clone()));
        st.text_styles.insert(TextStyle::Name("Header".into()), FontId::new(ty.header, ui_f));
        st.spacing.scroll.foreground_color = false;
        st.spacing.scroll.floating = false;
        st.spacing.scroll.bar_width = w.scroll_bar_width;
        let v = &mut st.visuals;
        v.dark_mode = luminance(p.canvas) < 0.4;
        v.override_text_color = None;
        v.weak_text_color = Some(p.text_muted);
        v.hyperlink_color = p.accent_text;
        v.faint_bg_color = p.stripe_b;
        v.extreme_bg_color = p.scroll_track;
        v.text_edit_bg_color = Some(p.input);
        v.code_bg_color = p.input;
        v.warn_fg_color = p.status.unsupported;
        v.error_fg_color = p.status.invalid;
        v.window_fill = p.menu;
        v.window_stroke = Stroke::new(1.0, p.border_control);
        v.window_corner_radius = rnd;
        v.menu_corner_radius = rnd;
        v.window_shadow = egui::Shadow::NONE;
        v.popup_shadow = egui::Shadow::NONE;
        v.panel_fill = p.panel;
        v.selection.bg_fill = p.sel_bg;
        v.selection.stroke = Stroke::new(1.0, p.sel_fg);
        v.text_cursor.stroke = Stroke::new(2.0, p.focus);
        v.striped = true;
        let wv = &mut v.widgets;
        wv.noninteractive.bg_fill = p.panel;
        wv.noninteractive.weak_bg_fill = p.panel;
        wv.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
        wv.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
        wv.noninteractive.corner_radius = rnd;
        wv.inactive.bg_fill = p.scroll_thumb;
        wv.inactive.weak_bg_fill = w.button_bg;
        wv.inactive.bg_stroke = Stroke::new(w.border_width, p.border_control);
        wv.inactive.fg_stroke = Stroke::new(1.0, p.text);
        wv.inactive.corner_radius = rnd;
        // bg_fill = table row hover (egui_extras); weak_bg_fill = buttons and menu buttons.
        wv.hovered.bg_fill = p.hover_bg;
        wv.hovered.weak_bg_fill = p.button_hover;
        wv.hovered.bg_stroke = Stroke::new(w.border_width, p.outline);
        wv.hovered.fg_stroke = Stroke::new(1.5, p.text);
        wv.hovered.corner_radius = rnd;
        wv.active.bg_fill = p.scroll_thumb_hover;
        wv.active.weak_bg_fill = p.active_bg;
        wv.active.bg_stroke = Stroke::new(w.border_width, p.focus);
        wv.active.fg_stroke = Stroke::new(2.0, p.active_fg);
        wv.active.corner_radius = rnd;
        wv.open.bg_fill = p.hover_bg;
        wv.open.weak_bg_fill = p.button_hover;
        wv.open.bg_stroke = Stroke::new(w.border_width, p.outline);
        wv.open.fg_stroke = Stroke::new(1.0, p.text);
        wv.open.corner_radius = rnd;
    });
    let grain = match def.decoration {
        Decoration::WoodGrain => Some(match cache.iter().find(|(id, _)| *id == def.id) {
            Some((_, t)) => t.clone(),
            None => {
                let opts = egui::TextureOptions { wrap_mode: egui::TextureWrapMode::Repeat, ..egui::TextureOptions::LINEAR };
                let t = ctx.load_texture(format!("grain-{}", def.id), grain_image(p.toolbar), opts);
                cache.push((def.id, t.clone()));
                t
            }
        }),
        Decoration::None => None,
    };
    Tokens { grain, ..Tokens::from_def(def) }
}

pub const GRAIN_SIZE: usize = 128;

/// Tileable procedural wood grain around `base` (±8 RGB, horizontal streaks). Generated once.
pub fn grain_image(base: Color32) -> egui::ColorImage {
    let n = GRAIN_SIZE;
    let mut seed = 0x9E37_79B9u32;
    let mut rnd = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        (seed % 1000) as f32 / 1000.0
    };
    // Per-row streak intensity, smoothed vertically and wrapped so the tile repeats.
    let raw: Vec<f32> = (0..n).map(|_| rnd()).collect();
    let rows: Vec<f32> = (0..n).map(|y| (raw[(y + n - 1) % n] + 2.0 * raw[y] + raw[(y + 1) % n]) / 4.0).collect();
    let mut px = Vec::with_capacity(n * n);
    for y in 0..n {
        for x in 0..n {
            let wave = ((x as f32 / n as f32) * std::f32::consts::TAU * 2.0 + y as f32 * 0.21).sin() * 0.25;
            let d = ((rows[y] - 0.5) * 2.0 + wave).clamp(-1.0, 1.0) * 8.0;
            let ch = |v: u8| (v as f32 + d).clamp(0.0, 255.0) as u8;
            px.push(Color32::from_rgb(ch(base.r()), ch(base.g()), ch(base.b())));
        }
    }
    egui::ColorImage::new([n, n], px)
}

// ---------------------------------------------------------------- contrast

fn lin(c: u8) -> f64 {
    let s = c as f64 / 255.0;
    if s <= 0.04045 {
        s / 12.92
    } else {
        ((s + 0.055) / 1.055).powf(2.4)
    }
}

/// WCAG relative luminance.
pub fn luminance(c: Color32) -> f32 {
    (0.2126 * lin(c.r()) + 0.7152 * lin(c.g()) + 0.0722 * lin(c.b())) as f32
}

/// WCAG contrast ratio (1..21).
pub fn contrast(a: Color32, b: Color32) -> f32 {
    let (x, y) = (luminance(a), luminance(b));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every role pair the renderers actually draw: text ≥4.5:1, control borders ≥3:1.
    #[test]
    fn theme_role_contrast() {
        let mut fails = Vec::new();
        for t in THEMES {
            let p = &t.palette;
            let mut chk = |what: &str, fg: Color32, bg: Color32, min: f32| {
                let r = contrast(fg, bg);
                if r < min {
                    fails.push(format!("{}: {what} {:?} on {:?} = {r:.2} < {min}", t.id, fg, bg));
                }
            };
            let text_surfaces = [("canvas", p.canvas), ("panel", p.panel), ("explain", p.explain), ("toolbar", p.toolbar), ("input", p.input), ("menu", p.menu), ("data", p.data), ("stripe_a", p.stripe_a), ("stripe_b", p.stripe_b), ("hover", p.hover_bg), ("lsm", p.lsm_bg), ("button", t.widgets.button_bg)];
            for (n, bg) in text_surfaces {
                chk(&format!("text/{n}"), p.text, bg, 4.5);
            }
            for (n, bg) in [("canvas", p.canvas), ("panel", p.panel), ("explain", p.explain), ("toolbar", p.toolbar), ("data", p.data), ("stripe_a", p.stripe_a), ("stripe_b", p.stripe_b), ("menu", p.menu)] {
                chk(&format!("muted/{n}"), p.text_muted, bg, 4.5);
            }
            for (n, bg) in [("panel", p.panel), ("explain", p.explain), ("data", p.data), ("stripe_a", p.stripe_a), ("stripe_b", p.stripe_b)] {
                chk(&format!("accent_text/{n}"), p.accent_text, bg, 4.5);
                for (i, s) in p.status.all().into_iter().enumerate() {
                    chk(&format!("status{i}/{n}"), s, bg, 4.5);
                }
            }
            for (n, bg) in [("canvas", p.canvas), ("panel", p.panel), ("explain", p.explain), ("toolbar", p.toolbar), ("data", p.data)] {
                chk(&format!("heading/{n}"), p.heading, bg, 4.5);
            }
            chk("muted/hover", p.text_muted, p.hover_bg, 4.5);
            // Button tokens (spec §5.5).
            chk("text/button_hover", p.text, p.button_hover, 4.5);
            chk("active_fg/button_active", p.active_fg, p.active_bg, 4.5);
            chk("button_hover/toolbar", p.button_hover, p.toolbar, 1.5);
            chk("outline/button_hover", p.outline, p.button_hover, 3.0);
            chk("outline/toolbar", p.outline, p.toolbar, 3.0);
            chk("header", p.header_fg, p.header_bg, 4.5);
            chk("sel", p.sel_fg, p.sel_bg, 4.5);
            chk("match", p.match_fg, p.match_bg, 4.5);
            chk("active", p.active_fg, p.active_bg, 4.5);
            for (n, f) in [("byte", p.hex_byte), ("ascii", p.hex_ascii), ("offset", p.hex_offset), ("file", p.hex_file_space), ("derived", p.hex_derived_space)] {
                chk(&format!("hex_{n}/data"), f, p.data, 4.5);
            }
            for (n, bg) in [("canvas", p.canvas), ("panel", p.panel), ("toolbar", p.toolbar), ("data", p.data), ("button", t.widgets.button_bg)] {
                chk(&format!("border_control/{n}"), p.border_control, bg, 3.0);
            }
            chk("scroll_thumb/track", p.scroll_thumb, p.scroll_track, 3.0);
            chk("lsm_present/lsm_bg", p.lsm_present, p.lsm_bg, 3.0);
            chk("lsm_missing/lsm_bg", p.lsm_missing, p.lsm_bg, 3.0);
        }
        assert!(fails.is_empty(), "contrast failures:\n{}", fails.join("\n"));
    }

    /// Glyphs in `UI_GLYPHS` must not fall back to egui's replacement box.
    #[test]
    fn ui_glyphs_render_in_bundled_fonts() {
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(Default::default(), |_| {});
        for fam in [FontId::proportional(14.0), FontId::monospace(14.0)] {
            let uv = |c: char| ctx.fonts_mut(|f| f.layout_no_wrap(c.to_string(), fam.clone(), Color32::WHITE).rows[0].row.glyphs[0].uv_rect);
            let tofu = uv('◻');
            let missing: String = UI_GLYPHS.chars().filter(|&c| uv(c) == tofu).collect();
            assert!(missing.is_empty(), "{:?} lacks glyphs {missing:?}", fam.family);
        }
        // Probe sanity: U+2192 is known to render as the replacement box in the proportional font.
        let uvp = |c: char| ctx.fonts_mut(|f| f.layout_no_wrap(c.to_string(), FontId::proportional(14.0), Color32::WHITE).rows[0].row.glyphs[0].uv_rect);
        assert_eq!(uvp('→'), uvp('◻'));
    }

    /// Spec §10.4: hover/header/brass pairs of the Phase B token changes.
    #[test]
    fn phase_b_token_contrast_pairs() {
        let mut fails = Vec::new();
        let mut chk = |id: &str, what: &str, fg: Color32, bg: Color32, min: f32| {
            let r = contrast(fg, bg);
            if r < min {
                fails.push(format!("{id}: {what} {fg:?} on {bg:?} = {r:.2} < {min}"));
            }
        };
        let wb = theme_by_id("amiga-workbench").palette;
        assert_eq!(wb.button_hover, c(0x10306A));
        chk("workbench", "text/button_hover", wb.text, wb.button_hover, 4.5);
        chk("workbench", "button_hover/toolbar", wb.button_hover, wb.toolbar, 1.5);
        chk("workbench", "outline/button_hover", wb.outline, wb.button_hover, 3.0);
        chk("workbench", "outline/menu", wb.outline, wb.menu, 3.0);
        let ts = theme_by_id("amiga-tool-style");
        assert_eq!((ts.palette.header_bg, ts.palette.header_fg), (c(0x9AA1A0), ts.palette.text));
        assert!(ts.widgets.header_rule);
        chk("tool-style", "header_fg/header_bg", ts.palette.header_fg, ts.palette.header_bg, 4.5);
        chk("tool-style", "header_bg/sel_bg", ts.palette.header_bg, ts.palette.sel_bg, 1.5);
        chk("tool-style", "header_bg/data", ts.palette.header_bg, ts.palette.data, 1.5);
        let wd = theme_by_id("wooden");
        let g = grain_image(wd.palette.toolbar);
        let lum_of = |p: &Color32| luminance(*p);
        let darkest = *g.pixels.iter().min_by(|a, b| lum_of(a).total_cmp(&lum_of(b))).unwrap();
        let lightest = *g.pixels.iter().max_by(|a, b| lum_of(a).total_cmp(&lum_of(b))).unwrap();
        for (n, texel) in [("darkest", darkest), ("lightest", lightest)] {
            // Text drawn on grained chrome: header rows, panel titles, collapse strips.
            chk("wooden", &format!("header_fg/grain {n}"), wd.palette.header_fg, texel, 4.5);
            chk("wooden", &format!("heading/grain {n}"), wd.palette.heading, texel, 4.5);
            chk("wooden", &format!("text/grain {n}"), wd.palette.text, texel, 4.5);
        }
        chk("wooden", "brass separator/panel", wd.widgets.separator.color, wd.palette.panel, 3.0);
        assert_eq!(wd.widgets.separator.width, 2.0);
        for t in THEMES {
            let p = &t.palette;
            // Collapse strips: muted label and text chevron on panel and on hover.
            chk(t.id, "strip muted/panel", p.text_muted, p.panel, 4.5);
            chk(t.id, "strip muted/hover", p.text_muted, p.hover_bg, 4.5);
            chk(t.id, "chevron text/panel", p.text, p.panel, 4.5);
            chk(t.id, "chevron text/hover", p.text, p.hover_bg, 4.5);
            chk(t.id, "chip file/data", p.hex_file_space, p.data, 4.5);
            chk(t.id, "chip derived/data", p.hex_derived_space, p.data, 4.5);
        }
        assert!(fails.is_empty(), "{}", fails.join("\n"));
    }

    #[test]
    fn display_names_are_human_readable() {
        let names: Vec<&str> = THEMES.iter().map(|t| t.name).collect();
        assert_eq!(names, ["Autistic", "Matrix", "Amiga Workbench", "Amiga Tool Style", "XCopy", "Wooden"]);
    }

    #[test]
    fn workbench_hover_is_visible() {
        let p = theme_by_id("amiga-workbench").palette;
        assert!(contrast(p.hover_bg, p.canvas) >= 1.5 && contrast(p.hover_bg, p.panel) >= 1.5);
    }

    /// 0009 item 3: egui's popup style drops the hovered stroke, so popup/menu items get the
    /// `outline` token painted explicitly (Workbench: 2 pt orange 0xFFAA33 on `button_hover`),
    /// in every theme, only when hovered and enabled, and visible on the popup fill.
    #[test]
    fn popup_items_get_the_outline_on_hover() {
        let ctx = egui::Context::default();
        let mut cache = Vec::new();
        let _ = apply_theme(&ctx, theme_by_id("amiga-workbench"), &mut cache);
        let mut style = (*ctx.global_style()).clone();
        assert_ne!(style.visuals.widgets.hovered.bg_stroke, Stroke::NONE);
        egui::containers::menu::menu_style(&mut style);
        assert_eq!(style.visuals.widgets.hovered.bg_stroke, Stroke::NONE, "egui popups keep the hovered stroke now; helper may be redundant");
        let wb = Tokens::from_def(theme_by_id("amiga-workbench"));
        assert_eq!(wb.menu_item_outline(true, true), Some(Stroke::new(2.0, c(0xFFAA33))));
        for t in THEMES {
            let tok = Tokens::from_def(t);
            assert_eq!(tok.menu_item_outline(true, true), Some(Stroke::new(t.widgets.border_width, t.palette.outline)), "{}", t.id);
            assert!(tok.menu_item_outline(true, true).unwrap().width >= 1.0, "{}", t.id);
            assert_eq!(tok.menu_item_outline(false, true), None);
            assert_eq!(tok.menu_item_outline(true, false), None, "disabled items get no outline");
            assert!(contrast(t.palette.outline, t.palette.button_hover) >= 3.0 && contrast(t.palette.outline, t.palette.menu) >= 3.0, "{}", t.id);
        }
    }

    #[test]
    fn registry_ids_and_fallback() {
        let ids: Vec<&str> = THEMES.iter().map(|t| t.id).collect();
        assert_eq!(ids, ["autistic", "matrix", "amiga-workbench", "amiga-tool-style", "xcopy", "wooden"]);
        assert_eq!(theme_by_id("no-such-theme").id, "autistic");
        assert_eq!(theme_by_id("").id, DEFAULT_THEME);
        assert_eq!(theme_by_id("wooden").decoration, Decoration::WoodGrain);
    }

    #[test]
    fn grain_is_tileable_and_bounded() {
        let base = THEMES[5].palette.toolbar;
        let img = grain_image(base);
        assert_eq!(img.size, [GRAIN_SIZE, GRAIN_SIZE]);
        for px in &img.pixels {
            assert!((px.r() as i32 - base.r() as i32).abs() <= 8);
        }
    }
}
