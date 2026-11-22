//! User settings (persisted as JSON) and colour themes.

use std::collections::BTreeMap;
use std::path::PathBuf;

use egui::Visuals;
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
        crate::ui_kit::visuals(self)
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
    /// Fluid motion: sliding panels, gliding cursor, dialog and toast transitions.
    pub animations: bool,
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
    pub show_toolbar: bool,
    pub check_updates: bool,
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
            animations: true,
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
            show_toolbar: true,
            check_updates: true,
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
