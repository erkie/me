//! User settings, kept as JSON next to the session data:
//! `~/Library/Application Support/Me/settings.json` on macOS and
//! `~/.config/me/settings.json` elsewhere.

use std::collections::HashMap;
use std::path::PathBuf;

use gpui::{App, Global};
use serde::{Deserialize, Serialize};

pub const DEFAULT_FONT_SIZE: f32 = 13.;
const MIN_FONT_SIZE: f32 = 8.;
const MAX_FONT_SIZE: f32 = 36.;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Editor font size; the markdown preview scales with it.
    pub font_size: f32,
    pub word_wrap: bool,
    /// Language server overrides by group ("rust", "typescript", "python",
    /// "go", "c", …): `{"python": {"command": "pylsp"}}`,
    /// `{"rust": {"enabled": false}}`.
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub language_servers: HashMap<String, LanguageServerSettings>,
    /// Download new builds from GitHub and install them on restart.
    pub auto_update: bool,
    /// A GitHub token for update checks (optional; needed only if the
    /// repository is private). By default `GH_TOKEN`/`GITHUB_TOKEN` or the
    /// GitHub CLI's is used.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub github_token: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LanguageServerSettings {
    /// `false` turns the server off.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// A command name (found on PATH) or path, instead of the default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub initialization_options: Option<serde_json::Value>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            font_size: DEFAULT_FONT_SIZE,
            word_wrap: false,
            language_servers: HashMap::new(),
            auto_update: true,
            github_token: None,
        }
    }
}

impl Global for Settings {}

impl Settings {
    /// Editor line height for the current font size.
    pub fn line_height(&self) -> f32 {
        (self.font_size * 1.54).round()
    }

    /// Body text size of the markdown preview.
    pub fn markdown_size(&self) -> f32 {
        self.font_size + 3.
    }
}

/// Where settings and sessions live. `ME_DATA_DIR` overrides it (tests).
pub fn data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("ME_DATA_DIR") {
        return PathBuf::from(dir);
    }
    let base = if cfg!(target_os = "macos") {
        dirs::data_dir()
    } else {
        dirs::config_dir()
    };
    let name = if cfg!(target_os = "macos") {
        "Me"
    } else {
        "me"
    };
    base.unwrap_or_else(std::env::temp_dir).join(name)
}

fn settings_path() -> PathBuf {
    data_dir().join("settings.json")
}

pub fn init(cx: &mut App) {
    let settings = std::fs::read(settings_path())
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Settings>(&bytes).ok())
        .unwrap_or_default();
    cx.set_global(settings);
}

pub fn get(cx: &App) -> &Settings {
    cx.global::<Settings>()
}

/// Changes settings, saves them and repaints every window.
pub fn update(cx: &mut App, f: impl FnOnce(&mut Settings)) {
    let mut settings = get(cx).clone();
    f(&mut settings);
    settings.font_size = settings.font_size.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE);
    if &settings == get(cx) {
        return;
    }
    cx.set_global(settings.clone());
    cx.background_executor()
        .spawn(async move {
            let path = settings_path();
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            if let Ok(json) = serde_json::to_vec_pretty(&settings) {
                let _ = std::fs::write(path, json);
            }
        })
        .detach();
    cx.refresh_windows();
}

pub fn change_font_size(delta: f32, cx: &mut App) {
    update(cx, |s| {
        s.font_size = if delta == 0. {
            DEFAULT_FONT_SIZE
        } else {
            s.font_size + delta
        }
    });
}
