//! User settings (persisted as JSON) and colour themes.

use std::collections::BTreeMap;
use std::path::PathBuf;

use egui::{Color32, Visuals};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Theme {
    Light,
    Dark,
    Dracula,
    SolarizedDark,
    SolarizedLight,
}

impl Theme {
    pub const ALL: [Theme; 5] = [Theme::Light, Theme::Dark, Theme::Dracula, Theme::SolarizedDark, Theme::SolarizedLight];
    pub fn label(self) -> &'static str {
        match self {
            Theme::Light => "Light",
            Theme::Dark => "Dark",
            Theme::Dracula => "Dracula",
            Theme::SolarizedDark => "Solarized Dark",
            Theme::SolarizedLight => "Solarized Light",
        }
    }

    pub fn visuals(self) -> Visuals {
        let hex = |s: u32| Color32::from_rgb((s >> 16) as u8, (s >> 8) as u8, s as u8);
        // (dark?, background, panel, faint/stripe, text, accent, selection)
        let (dark, bg, panel, faint, text, accent, sel) = match self {
            Theme::Light => return Visuals::light(),
            Theme::Dark => return Visuals::dark(),
            Theme::Dracula => (true, 0x282a36, 0x21222c, 0x2f3140, 0xf8f8f2, 0xbd93f9, 0x44475a),
            Theme::SolarizedDark => (true, 0x002b36, 0x073642, 0x053340, 0x93a1a1, 0x268bd2, 0x0a4a5c),
            Theme::SolarizedLight => (false, 0xfdf6e3, 0xeee8d5, 0xf5efdc, 0x586e75, 0x268bd2, 0xd3e5f0),
        };
        let mut v = if dark { Visuals::dark() } else { Visuals::light() };
        let (bg, panel, faint, text, accent, sel) = (hex(bg), hex(panel), hex(faint), hex(text), hex(accent), hex(sel));
        v.extreme_bg_color = bg;
        v.panel_fill = panel;
        v.window_fill = panel;
        v.faint_bg_color = faint;
        v.override_text_color = Some(text);
        v.hyperlink_color = accent;
        v.selection.bg_fill = sel.lerp_to_gamma(accent, 0.35);
        v.selection.stroke.color = accent;
        v.widgets.noninteractive.bg_fill = panel;
        v.widgets.noninteractive.weak_bg_fill = panel;
        v.widgets.inactive.bg_fill = sel;
        v.widgets.inactive.weak_bg_fill = sel.gamma_multiply(0.8);
        v.widgets.hovered.weak_bg_fill = sel.lerp_to_gamma(accent, 0.2);
        v.widgets.active.weak_bg_fill = sel.lerp_to_gamma(accent, 0.4);
        v.widgets.noninteractive.bg_stroke.color = sel;
        v
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum ExternalChange {
    Refresh,
    Ask,
    Ignore,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub theme: Theme,
    pub font_size: f32,
    pub row_height: f32,
    pub shade_rows: bool,
    pub shade_cols: bool,
    pub highlight_cursor_cross: bool,
    pub show_row_labels: bool,
    pub show_col_labels: bool,
    pub col_letters: bool,
    pub first_row_index: i64,
    pub first_col_index: i64,
    pub default_header: bool,
    pub auto_detect_delimiter: bool,
    pub max_autofit_width: f32,
    pub external_change: ExternalChange,
    pub retain_filters_on_refresh: bool,
    pub save_after_each_edit: bool,
    pub read_only_threshold_mb: u64,
    pub search_engine: String,
    pub map_engine: String,
    pub text_editor: String,
    /// extension (lowercase, no dot) -> delimiter character
    pub ext_delimiters: BTreeMap<String, String>,
    pub recent: Vec<String>,
    pub show_sidebar: bool,
    pub show_metadata: bool,
    pub show_editor_pane: bool,
    pub folders: Vec<String>,
    /// Path to the Faruma font for Dhivehi (blank = auto-detect).
    pub dhivehi_font: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: Theme::Dark,
            font_size: 13.0,
            row_height: 22.0,
            shade_rows: true,
            shade_cols: false,
            highlight_cursor_cross: false,
            show_row_labels: true,
            show_col_labels: true,
            col_letters: true,
            first_row_index: 1,
            first_col_index: 1,
            default_header: true,
            auto_detect_delimiter: true,
            max_autofit_width: 600.0,
            external_change: ExternalChange::Refresh,
            retain_filters_on_refresh: true,
            save_after_each_edit: false,
            read_only_threshold_mb: 0,
            search_engine: "https://duckduckgo.com/?q=".into(),
            map_engine: "https://duckduckgo.com/?iaxm=maps&q=".into(),
            text_editor: String::new(),
            ext_delimiters: BTreeMap::new(),
            recent: Vec::new(),
            show_sidebar: false,
            show_metadata: false,
            show_editor_pane: false,
            folders: Vec::new(),
            dhivehi_font: String::new(),
        }
    }
}

pub fn config_dir() -> Option<PathBuf> {
    let base = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Application Support"))
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
    };
    base.map(|b| b.join("ficsv"))
}

impl Settings {
    pub fn load() -> Settings {
        config_dir()
            .and_then(|d| std::fs::read_to_string(d.join("settings.json")).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        if let Some(d) = config_dir() {
            let _ = std::fs::create_dir_all(&d);
            if let Ok(s) = serde_json::to_string_pretty(self) {
                let _ = std::fs::write(d.join("settings.json"), s);
            }
        }
    }

    pub fn push_recent(&mut self, path: &std::path::Path) {
        let p = path.to_string_lossy().into_owned();
        self.recent.retain(|r| r != &p);
        self.recent.insert(0, p);
        self.recent.truncate(15);
    }

}
