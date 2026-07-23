use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

fn default_true() -> bool { true }
fn default_silence_timeout() -> u32 { 300 }
/// Push-to-talk dictation shortcut, as a Tauri accelerator string. The key part
/// uses W3C KeyboardEvent `code` names, which is exactly what the frontend
/// capture sends and what global-hotkey's parser accepts.
pub fn default_dictation_hotkey() -> String { "Alt+KeyD".into() }

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
    /// Global push-to-talk shortcut (desktop). Registered at startup and
    /// re-registered live by `set_dictation_hotkey`.
    #[serde(default = "default_dictation_hotkey")]
    pub dictation_hotkey: String,
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
    /// Where meeting transcripts are written as Markdown (desktop Meeting
    /// mode). Empty means the default: Documents/Verba Meetings.
    #[serde(default = "default_meetings_dir")]
    pub meeting_transcript_dir: String,
    /// Where meeting summaries are written; defaults with the transcripts.
    #[serde(default = "default_meetings_dir")]
    pub meeting_summary_dir: String,
    /// Chosen summarizer component id (e.g. "sum-qwen3-1.7b"); "" = unchosen.
    #[serde(default)]
    pub meeting_summarizer: String,
    /// Label remote speakers via embedding clustering (experimental).
    #[serde(default = "default_true")]
    pub meeting_diarize: bool,
    /// Meeting-mode microphone by device name; "" = default/dictation input.
    #[serde(default)]
    pub meeting_mic_device: String,
    /// Meeting-mode system audio to capture (loopback) by output device name;
    /// "" = default output. macOS/Windows only.
    #[serde(default)]
    pub meeting_output_device: String,
    /// Offer to stop the meeting after this many seconds with no speech from
    /// either stream (end-of-meeting detection). 0 disables it.
    #[serde(default = "default_silence_timeout")]
    pub meeting_silence_timeout_secs: u32,
    /// Playback speed remembered per voice (voice string -> speed multiplier).
    /// Selecting a voice restores its last speed (default 1.0). MUST stay the
    /// last field: `toml` emits map fields as `[tables]`, which must follow all
    /// scalar fields, or serialization fails.
    #[serde(default)]
    pub tts_voice_speeds: HashMap<String, f32>,
}

fn default_meetings_dir() -> String {
    dirs::document_dir()
        .map(|d| d.join("Verba Meetings").to_string_lossy().into_owned())
        .unwrap_or_default()
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
            dictation_hotkey: default_dictation_hotkey(),
            tts_favourite_sids: Vec::new(),
            tts_favourite_voices: Vec::new(),
            tts_voice: String::new(),
            tts_model: String::new(),
            meeting_transcript_dir: default_meetings_dir(),
            meeting_summary_dir: default_meetings_dir(),
            meeting_summarizer: String::new(),
            meeting_diarize: true,
            meeting_mic_device: String::new(),
            meeting_output_device: String::new(),
            meeting_silence_timeout_secs: 300,
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
    fn pre_meeting_config_loads_with_defaults() {
        // A config written before the meeting fields existed: they must
        // default (dirs from Documents, diarize on) rather than fail.
        let toml_src = r#"
language = "en"
threads = 4
output_dir = ""
device_index = -1
active_engine = "parakeet"
active_model_id = ""
haptic_feedback = true
tts_favourite_sids = []
tts_favourite_voices = []
tts_voice = ""
tts_model = ""

[tts_voice_speeds]
"#;
        let cfg: AppConfig = toml::from_str(toml_src).unwrap();
        assert!(cfg.meeting_diarize);
        assert!(cfg.meeting_summarizer.is_empty());
        assert_eq!(cfg.meeting_transcript_dir, default_meetings_dir());
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
            dictation_hotkey: "Control+Shift+Space".into(),
            tts_favourite_sids: vec![3, 7],
            tts_favourite_voices: vec!["tts-piper-alba:0".into()],
            tts_voice: "7".into(),
            tts_model: "tts-piper-alba".into(),
            meeting_transcript_dir: "/tmp/meet-t".into(),
            meeting_summary_dir: "/tmp/meet-s".into(),
            meeting_summarizer: "sum-qwen3-1.7b".into(),
            meeting_diarize: false,
            meeting_mic_device: String::new(),
            meeting_output_device: "MacBook Pro Speakers".into(),
            meeting_silence_timeout_secs: 600,
            tts_voice_speeds: HashMap::from([("7".to_string(), 0.75f32)]),
        };

        let serialized = toml::to_string_pretty(&cfg).unwrap();
        let deserialized: AppConfig = toml::from_str(&serialized).unwrap();

        assert_eq!(deserialized.language, "fr");
        assert_eq!(deserialized.threads, 8);
        assert_eq!(deserialized.device_index, 2);
        assert_eq!(deserialized.active_engine, "parakeet");
        assert_eq!(deserialized.meeting_transcript_dir, "/tmp/meet-t");
        assert_eq!(deserialized.meeting_summarizer, "sum-qwen3-1.7b");
        assert!(!deserialized.meeting_diarize);
        assert_eq!(deserialized.meeting_output_device, "MacBook Pro Speakers");
        assert!(deserialized.meeting_mic_device.is_empty());
        assert_eq!(deserialized.active_model_id, "parakeet-v3-int8");
        assert!(!deserialized.haptic_feedback);
        assert_eq!(deserialized.dictation_hotkey, "Control+Shift+Space");
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
