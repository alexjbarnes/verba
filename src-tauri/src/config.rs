use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    /// Content/UI language tag ("en"). Kept from the original config shape so
    /// existing installs load unchanged; unknown legacy fields are ignored.
    pub language: String,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            language: "en".into(),
        }
    }
}

impl AppConfig {
    fn config_path() -> Option<PathBuf> {
        #[cfg(target_os = "android")]
        {
            std::env::var_os("VERBA_DATA_DIR")
                .map(|d| PathBuf::from(d).join("config.toml"))
        }
        #[cfg(not(target_os = "android"))]
        {
            dirs::config_dir().map(|d| d.join("verba").join("config.toml"))
        }
    }

    pub fn load() -> Self {
        let Some(path) = Self::config_path() else {
            return Self::default();
        };
        match std::fs::read_to_string(&path) {
            Ok(contents) => toml::from_str(&contents).unwrap_or_else(|e| {
                log::warn!("Bad config file, using defaults: {e}");
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self) -> Result<(), String> {
        let path = Self::config_path().ok_or("no home dir")?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("create dir: {e}"))?;
        }
        let contents = toml::to_string_pretty(self).map_err(|e| format!("serialize: {e}"))?;
        std::fs::write(&path, contents).map_err(|e| format!("write: {e}"))?;
        log::info!("Config saved to {}", path.display());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sane() {
        let cfg = AppConfig::default();
        assert_eq!(cfg.language, "en");
    }

    /// A pre-restart config carries many dictation/TTS/meeting fields; they
    /// must be ignored on load rather than rejected.
    #[test]
    fn legacy_config_loads_ignoring_unknown_fields() {
        let toml_src = r#"
language = "en"
threads = 4
output_dir = ""
device_index = -1
active_engine = "parakeet"
active_model_id = ""
haptic_feedback = true
dictation_hotkey = "Alt+KeyD"

[tts_voice_speeds]
"#;
        let cfg: AppConfig = toml::from_str(toml_src).unwrap();
        assert_eq!(cfg.language, "en");
    }

    #[test]
    fn roundtrip_toml() {
        let cfg = AppConfig { language: "fr".into(), ..AppConfig::default() };
        let serialized = toml::to_string_pretty(&cfg).unwrap();
        let deserialized: AppConfig = toml::from_str(&serialized).unwrap();
        assert_eq!(deserialized.language, "fr");
    }

    #[test]
    fn deserialize_partial_uses_defaults_on_missing_required_field_error() {
        // Missing required fields fail strict deserialization; load() catches
        // that and falls back to defaults.
        let partial = "threads = 2";
        let result: Result<AppConfig, _> = toml::from_str(partial);
        assert!(result.is_err());
    }
}
