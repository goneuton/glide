//! Preferences, stored as JSON in `%APPDATA%\Glide\settings.json`.

use crate::theme::Appearance;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub appearance: Appearance,
    /// Index into `theme::ACCENTS`.
    pub accent: usize,
    /// The mode that was open last; it is the one shown on the next hover.
    pub last_mode: String,
    /// Show other apps' notifications on the island.
    pub notifications: bool,
    /// Blur of the album cover behind the media page: 0 off, 1 light, 2 medium, 3 heavy.
    pub cover_blur: u8,
    /// Show who a notification is from, never what it says.
    pub privacy: bool,
    /// The first-run welcome has been completed.
    pub welcomed: bool,
    /// Start Glide when you sign in to Windows.
    pub autostart: bool,
    /// City for the weather when Windows location is off (empty = ask Windows).
    pub city: String,
    /// Country code for public holidays (empty = the Windows region).
    pub region: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self { appearance: Appearance::System, accent: 0, last_mode: String::new(), notifications: true, cover_blur: 2, privacy: false, welcomed: false, autostart: false, city: String::new(), region: String::new() }
    }
}

pub fn data_dir() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("APPDATA")?).join("Glide"))
}

impl Settings {
    pub fn load() -> Self {
        let Some(path) = data_dir().map(|d| d.join("settings.json")) else { return Self::default() };
        match std::fs::read_to_string(&path) {
            // Editors on Windows often prepend a byte-order mark.
            Ok(text) => match serde_json::from_str(text.trim_start_matches('\u{feff}')) {
                Ok(settings) => settings,
                Err(_) => {
                    // Unreadable: keep a copy so the next save does not destroy it.
                    let _ = std::fs::copy(&path, path.with_extension("json.bad"));
                    Self::default()
                }
            },
            Err(_) => Self::default(),
        }
    }

    /// Written to a temporary file first so a crash cannot leave half a file.
    pub fn save(&self) {
        let Some(dir) = data_dir() else { return };
        if std::fs::create_dir_all(&dir).is_err() {
            return;
        }
        let Ok(text) = serde_json::to_string_pretty(self) else { return };
        let (path, tmp) = (dir.join("settings.json"), dir.join("settings.json.tmp"));
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
}
