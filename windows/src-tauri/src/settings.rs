// Preferences, stored as plain JSON in settings.json under platform::config_dir().
// No secret ever lands here — API keys live in the OS keychain (see secrets.rs).

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub sound_enabled: bool,
    pub sound_volume: f64,
    pub auto_close_interval: f64,
    /// Whether the compact mini header retracts completely after one minute.
    #[serde(default = "default_auto_hide")]
    pub auto_hide: bool,
    pub absence_interval: f64,
    pub active_integrations: Vec<String>,
    /// Schema marker for one-time integration-list migrations.
    #[serde(default)]
    pub integrations_version: u32,
    /// "primary" = the main display, "cursor" = whichever display the mouse is on.
    pub screen: String,
    pub autostart: bool,
    pub hooks_installed: bool,
    /// Claude model used by the chat. Changeable in the settings window.
    /// Defaulted explicitly so a settings.json written by an older build still loads.
    #[serde(default = "default_model")]
    pub model: String,
    /// API used by the island chat: "claude" or "openai".
    #[serde(default = "default_chat_provider")]
    pub chat_provider: String,
    /// OpenAI model used when the OpenAI chat provider is selected.
    #[serde(default = "default_openai_model")]
    pub openai_model: String,
    /// Credential used by the OpenAI chat: "api_key" or "chatgpt".
    #[serde(default = "default_openai_auth")]
    pub openai_auth: String,
}

fn default_model() -> String {
    crate::claude::DEFAULT_MODEL.to_string()
}

fn default_auto_hide() -> bool {
    true
}

fn default_chat_provider() -> String {
    "claude".to_string()
}

fn default_openai_model() -> String {
    crate::openai::DEFAULT_MODEL.to_string()
}

fn default_openai_auth() -> String {
    "api_key".to_string()
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            sound_enabled: true,
            sound_volume: 0.12,
            auto_close_interval: 15.0,
            auto_hide: true,
            absence_interval: 180.0,
            active_integrations: vec![
                "integration_claude".into(),
                "integration_codex".into(),
                "integration_resend".into(),
                "integration_n8n".into(),
                "integration_vercel".into(),
                "integration_github".into(),
            ],
            integrations_version: 1,
            screen: "primary".into(),
            autostart: false,
            hooks_installed: false,
            model: default_model(),
            chat_provider: default_chat_provider(),
            openai_model: default_openai_model(),
            openai_auth: default_openai_auth(),
        }
    }
}

pub use crate::platform::{config_dir, local_dir};

pub fn hook_exe_path() -> PathBuf {
    local_dir().join("bin").join(crate::platform::HOOK_EXE)
}

fn settings_path() -> PathBuf {
    config_dir().join("settings.json")
}

pub fn load() -> Settings {
    let mut settings = match std::fs::read(settings_path()) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
        Err(_) => Settings::default(),
    };
    if migrate_integrations(&mut settings) {
        let _ = save(&settings);
    }
    settings
}

fn migrate_integrations(settings: &mut Settings) -> bool {
    if settings.integrations_version < 1 {
        for id in ["integration_claude", "integration_codex"] {
            if !settings
                .active_integrations
                .iter()
                .any(|active| active == id)
            {
                settings.active_integrations.insert(0, id.to_string());
            }
        }
        settings.integrations_version = 1;
        return true;
    }
    false
}

pub fn save(settings: &Settings) -> std::io::Result<()> {
    let dir = config_dir();
    crate::platform::ensure_private_dir(&dir)?;
    let json = serde_json::to_vec_pretty(settings)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(settings_path(), json)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_settings_enable_both_editor_integrations_once() {
        let mut settings = Settings::default();
        settings.integrations_version = 0;
        settings
            .active_integrations
            .retain(|id| !id.starts_with("integration_c"));

        assert!(migrate_integrations(&mut settings));
        assert!(settings
            .active_integrations
            .iter()
            .any(|id| id == "integration_claude"));
        assert!(settings
            .active_integrations
            .iter()
            .any(|id| id == "integration_codex"));
        assert!(!migrate_integrations(&mut settings));
    }

    #[test]
    fn current_settings_respect_disabled_editor_integrations() {
        let mut settings = Settings::default();
        settings.active_integrations.clear();

        assert!(!migrate_integrations(&mut settings));
        assert!(settings.active_integrations.is_empty());
    }
}
