use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

fn default_true() -> bool { true }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub language: String,
    pub threads: u32,
    pub output_dir: String,
    pub device_index: i32,
    pub active_engine: String,
    pub active_model_id: String,
    #[serde(default = "default_true")]
    pub haptic_feedback: bool,
    /// Speaker ids the user has starred on the Voices page. The player voice
    /// picker shows only these (or all voices when empty).
    /// Superseded by `tts_favourite_voices`; kept for migration of old configs.
    #[serde(default)]
    pub tts_favourite_sids: Vec<i32>,
    /// Starred voices as "model-id:sid" keys, spanning every TTS model in the
    /// catalogue (favourites survive switching models).
    #[serde(default)]
    pub tts_favourite_voices: Vec<String>,
    /// Last-selected TTS voice: either a speaker-id string ("37") or
    /// "custom:<name>" for a custom voice. Empty means default (speaker 0).
    #[serde(default)]
    pub tts_voice: String,
    /// Active TTS model id (e.g. "tts-piper-alba"). Empty means the first TTS
    /// model in the registry (the original single-model behavior).
    #[serde(default)]
    pub tts_model: String,
    /// Playback speed remembered per voice (voice string -> speed multiplier).
    /// Selecting a voice restores its last speed (default 1.0). MUST stay the
    /// last field: `toml` emits map fields as `[tables]`, which must follow all
    /// scalar fields, or serialization fails.
    #[serde(default)]
    pub tts_voice_speeds: HashMap<String, f32>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            language: "en".into(),
            threads: 4,
            output_dir: dirs::document_dir()
                .map(|d| d.join("Meetings").to_string_lossy().into_owned())
                .unwrap_or_default(),
            device_index: -1,
            active_engine: "whisper".into(),
            active_model_id: String::new(),
            haptic_feedback: true,
            tts_favourite_sids: Vec::new(),
            tts_favourite_voices: Vec::new(),
            tts_voice: String::new(),
            tts_model: String::new(),
            tts_voice_speeds: HashMap::new(),
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
        assert_eq!(cfg.threads, 4);
        assert_eq!(cfg.active_engine, "whisper");
        assert!(cfg.active_model_id.is_empty());
        assert!(cfg.device_index < 0); // -1 means default device
    }

    #[test]
    fn roundtrip_toml() {
        let cfg = AppConfig {
            language: "fr".into(),
            threads: 8,
            output_dir: "/tmp/test".into(),
            device_index: 2,
            active_engine: "parakeet".into(),
            active_model_id: "parakeet-v3-int8".into(),
            haptic_feedback: false,
            tts_favourite_sids: vec![3, 7],
            tts_favourite_voices: vec!["tts-piper-alba:0".into()],
            tts_voice: "7".into(),
            tts_model: "tts-piper-alba".into(),
            tts_voice_speeds: HashMap::from([("7".to_string(), 0.75f32)]),
        };

        let serialized = toml::to_string_pretty(&cfg).unwrap();
        let deserialized: AppConfig = toml::from_str(&serialized).unwrap();

        assert_eq!(deserialized.language, "fr");
        assert_eq!(deserialized.threads, 8);
        assert_eq!(deserialized.device_index, 2);
        assert_eq!(deserialized.active_engine, "parakeet");
        assert_eq!(deserialized.active_model_id, "parakeet-v3-int8");
        assert!(!deserialized.haptic_feedback);
        assert_eq!(deserialized.tts_favourite_sids, vec![3, 7]);
        assert_eq!(deserialized.tts_voice, "7");
        assert_eq!(deserialized.tts_voice_speeds.get("7"), Some(&0.75f32));
    }

    #[test]
    fn deserialize_partial_uses_defaults() {
        // Missing fields should fail with strict deserialization
        let partial = r#"
language = "de"
threads = 2
"#;
        let result: Result<AppConfig, _> = toml::from_str(partial);
        // toml strict deserialize requires all fields
        assert!(result.is_err());
    }

    #[test]
    fn deserialize_with_all_fields() {
        let full = r#"
language = "en"
threads = 4
output_dir = "/tmp"
device_index = -1
active_engine = "whisper"
active_model_id = ""
haptic_feedback = true
"#;
        let cfg: AppConfig = toml::from_str(full).unwrap();
        assert_eq!(cfg.language, "en");
        assert_eq!(cfg.threads, 4);
    }
}
