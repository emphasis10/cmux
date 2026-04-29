use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::path::PathBuf;
use std::sync::OnceLock;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    pub sidebar_width: i32,
    pub terminal_backend: TerminalBackendPreference,
    pub browser_state_automation: bool,
    pub desktop_notifications: bool,
    pub theme: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum TerminalBackendPreference {
    Auto,
    Pty,
    Vte,
    Ghostty,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            sidebar_width: 220,
            terminal_backend: TerminalBackendPreference::Pty,
            browser_state_automation: true,
            desktop_notifications: true,
            theme: "system".to_string(),
        }
    }
}

pub fn get() -> &'static Settings {
    static SETTINGS: OnceLock<Settings> = OnceLock::new();
    SETTINGS.get_or_init(load)
}

pub fn path() -> Option<PathBuf> {
    if let Some(path) = env::var_os("CMUX_SETTINGS_PATH").filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(path));
    }
    if let Some(config_home) = env::var_os("XDG_CONFIG_HOME").filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(config_home).join("cmux").join("settings.json"));
    }
    env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(|home| PathBuf::from(home).join(".config").join("cmux").join("settings.json"))
}

fn load() -> Settings {
    let Some(path) = path() else {
        return Settings::default();
    };
    let Ok(data) = fs::read_to_string(path) else {
        return Settings::default();
    };
    serde_json::from_str(&data).unwrap_or_else(|error| {
        eprintln!("cmux linux settings ignored: {error}");
        Settings::default()
    })
}
