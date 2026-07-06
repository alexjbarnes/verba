use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

static MODEL_MANAGER: OnceLock<ModelManager> = OnceLock::new();

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tauri::Emitter;
use tokio::io::AsyncWriteExt;

// ── Types ──

#[derive(Clone, Deserialize)]
pub struct ModelFile {
    pub url: String,
    pub rel_path: String,
    pub bytes: u64,
    pub role: String,
}

#[derive(Clone, Deserialize)]
pub struct ModelDef {
    pub id: String,
    pub name: String,
    pub desc: String,
    pub engine: String,
    pub size: String,
    pub files: Vec<ModelFile>,
}

// ── Manifest (packages + voices catalogue) ──
//
// The registry is no longer a Rust literal: it is derived from a manifest —
// the freshest of (remote fetch, disk cache, embedded snapshot). The three
// share one JSON shape (see MODEL_PACKAGES.md), so adding a voice or bumping
// a dictation component is a server-side manifest edit, not an app release.

pub const MANIFEST_URL: &str =
    "https://pub-c88baaac61224fbba973b547f1d947ca.r2.dev/manifest/v1.json";
/// Compiled-in fallback so a fresh offline install still has a full registry.
const EMBEDDED_MANIFEST: &str = include_str!("../data/model-manifest.json");
/// Background refresh cadence; an explicit "check for updates" bypasses it.
const MANIFEST_MAX_AGE_SECS: u64 = 24 * 60 * 60;

#[derive(Clone, Deserialize)]
pub struct Manifest {
    pub schema: u32,
    pub dictation: DictationSpec,
    pub voices: VoicesSpec,
}

#[derive(Clone, Deserialize)]
pub struct DictationSpec {
    /// Aggregate version the UI compares; bumps when any component bumps.
    pub version: u32,
    pub components: HashMap<String, ComponentSpec>,
}

#[derive(Clone, Deserialize)]
pub struct ComponentSpec {
    pub version: u32,
    /// Present on ASR components: they double as registry entries so the
    /// transcriber path (`model_engine`) keeps working unchanged.
    #[serde(default)]
    pub model: Option<ModelDef>,
    /// Plain files for non-model components (vad, grammar).
    #[serde(default)]
    pub files: Vec<ModelFile>,
    /// Verify byte counts after download. Off for ASR components whose
    /// registry sizes are approximate.
    #[serde(default)]
    pub verify_bytes: bool,
}

impl ComponentSpec {
    pub fn all_files(&self) -> Vec<ModelFile> {
        match &self.model {
            Some(m) => m.files.clone(),
            None => self.files.clone(),
        }
    }
}

#[derive(Clone, Deserialize)]
pub struct VoicesSpec {
    pub version: u32,
    pub list: Vec<ModelDef>,
}

fn parse_manifest(raw: &str) -> Result<Manifest, String> {
    let m: Manifest = serde_json::from_str(raw).map_err(|e| format!("manifest parse: {e}"))?;
    if m.schema != 1 {
        return Err(format!("unsupported manifest schema {}", m.schema));
    }
    Ok(m)
}

/// The platform's dictation components, in install order.
fn platform_components(m: &Manifest) -> Vec<(String, ComponentSpec)> {
    let asr = if cfg!(target_os = "android") { "asr-mobile" } else { "asr-desktop" };
    [asr, "vad", "grammar"]
        .iter()
        .filter_map(|k| m.dictation.components.get(*k).map(|c| (k.to_string(), c.clone())))
        .collect()
}

/// Registry = voice list + the platform-relevant ASR model entries.
/// Both ASR variants are included so a desktop install that previously
/// downloaded the INT8 build keeps resolving it.
fn registry_from_manifest(m: &Manifest) -> Vec<ModelDef> {
    let mut reg: Vec<ModelDef> = m
        .dictation
        .components
        .values()
        .filter_map(|c| c.model.clone())
        .collect();
    reg.extend(m.voices.list.iter().cloned());
    reg
}

/// Installed-package record (`models/packages.json`).
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct InstalledPackages {
    #[serde(default)]
    pub dictation_version: u32,
    #[serde(default)]
    pub dictation_components: HashMap<String, u32>,
}

/// Absolute paths to the seven grammar model files, present only when every
/// one exists on disk. `grammar_neural.rs` builds its sessions from these.
pub struct GrammarFilePaths {
    pub router_model: PathBuf,
    pub router_tokenizer: PathBuf,
    pub encoder: PathBuf,
    pub decoder: PathBuf,
    pub kv_weights: PathBuf,
    pub t5_tokenizer: PathBuf,
    pub config: PathBuf,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelInfo {
    pub id: String,
    pub name: String,
    pub desc: String,
    pub engine: String,
    pub size: String,
    pub status: String,
    pub progress: f64,
}

// ── Manager ──

pub struct ModelManager {
    pub base_dir: PathBuf,
    alt_dirs: Vec<PathBuf>,
    manifest: Mutex<Manifest>,
    registry: Mutex<Vec<ModelDef>>,
    progress: Mutex<HashMap<String, f64>>,
    active_model: Mutex<String>,
}

impl ModelManager {
    /// Initialize the global singleton. Safe to call multiple times; only the
    /// first call actually creates the manager.
    pub fn init_global() -> Result<&'static Self, String> {
        if let Some(m) = MODEL_MANAGER.get() {
            return Ok(m);
        }
        let mgr = Self::new()?;
        Ok(MODEL_MANAGER.get_or_init(|| mgr))
    }

    /// Access the global singleton. Panics if not initialized.
    pub fn global() -> &'static Self {
        MODEL_MANAGER.get().expect("ModelManager not initialized")
    }

    pub fn new() -> Result<Self, String> {
        let base_dir = Self::default_base_dir()?;
        std::fs::create_dir_all(&base_dir).map_err(|e| format!("create models dir: {e}"))?;

        let mut alt_dirs = Vec::new();
        #[cfg(target_os = "macos")]
        {
            if let Some(home) = dirs::home_dir() {
                alt_dirs.push(home.join("Library/Application Support/com.meetily.ai/models"));
            }
        }

        // Restore active model from config
        let cfg = crate::config::AppConfig::load();
        let active = if !cfg.active_model_id.is_empty() {
            cfg.active_model_id.clone()
        } else {
            String::new()
        };

        // Voices dropped from the catalogue; reclaim their disk on installs
        // that had downloaded them.
        for stale in ["tts/piper-libritts", "tts/piper-vctk"] {
            let _ = std::fs::remove_dir_all(base_dir.join(stale));
        }

        // Freshest usable manifest: the disk cache from a previous fetch,
        // else the embedded snapshot. A malformed cache (interrupted write,
        // future schema) silently falls back rather than bricking startup.
        let embedded = parse_manifest(EMBEDDED_MANIFEST)
            .map_err(|e| format!("embedded manifest: {e}"))?;
        let manifest = std::fs::read_to_string(base_dir.join("manifest.json"))
            .ok()
            .and_then(|raw| parse_manifest(&raw).ok())
            .unwrap_or(embedded);
        let registry = registry_from_manifest(&manifest);

        Ok(Self {
            base_dir,
            alt_dirs,
            manifest: Mutex::new(manifest),
            registry: Mutex::new(registry),
            progress: Mutex::new(HashMap::new()),
            active_model: Mutex::new(active),
        })
    }

    // ── Manifest refresh + package state ──

    fn manifest_cache_path(&self) -> PathBuf {
        self.base_dir.join("manifest.json")
    }

    fn packages_path(&self) -> PathBuf {
        self.base_dir.join("packages.json")
    }

    pub fn installed_packages(&self) -> InstalledPackages {
        std::fs::read_to_string(self.packages_path())
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default()
    }

    fn save_installed_packages(&self, p: &InstalledPackages) -> Result<(), String> {
        let raw = serde_json::to_string_pretty(p).map_err(|e| e.to_string())?;
        let path = self.packages_path();
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, raw).map_err(|e| format!("packages.json write: {e}"))?;
        std::fs::rename(&tmp, &path).map_err(|e| format!("packages.json rename: {e}"))
    }

    fn manifest_age_secs(&self) -> Option<u64> {
        std::fs::metadata(self.manifest_cache_path())
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .map(|d| d.as_secs())
    }

    /// Fetch the remote manifest and swap it in. `force` bypasses the 24 h
    /// staleness window (the explicit Check-for-updates path). Returns true
    /// when a fetch actually happened.
    pub async fn refresh_manifest(&self, force: bool) -> Result<bool, String> {
        if !force {
            if let Some(age) = self.manifest_age_secs() {
                if age < MANIFEST_MAX_AGE_SECS {
                    return Ok(false);
                }
            }
        }
        let raw = reqwest::Client::new()
            .get(MANIFEST_URL)
            .send()
            .await
            .map_err(|e| format!("manifest fetch: {e}"))?
            .error_for_status()
            .map_err(|e| format!("manifest fetch: {e}"))?
            .text()
            .await
            .map_err(|e| format!("manifest read: {e}"))?;
        let m = parse_manifest(&raw)?;

        let path = self.manifest_cache_path();
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, &raw).map_err(|e| format!("manifest cache write: {e}"))?;
        std::fs::rename(&tmp, &path).map_err(|e| format!("manifest cache rename: {e}"))?;

        *self.registry.lock().unwrap() = registry_from_manifest(&m);
        *self.manifest.lock().unwrap() = m;
        log::info!("Model manifest refreshed from {MANIFEST_URL}");
        Ok(true)
    }

    pub fn manifest(&self) -> Manifest {
        self.manifest.lock().unwrap().clone()
    }

    /// Dictation package status for the UI: one aggregate version pair plus
    /// a coarse state string. Component detail stays internal by design.
    pub fn packages_status(&self) -> serde_json::Value {
        let m = self.manifest();
        let installed = self.installed_packages();
        let components = platform_components(&m);
        let files_complete = components
            .iter()
            .all(|(_, c)| c.all_files().iter().all(|f| self.find_file(&f.rel_path).is_some()));
        let any_state = installed.dictation_version > 0
            || components
                .iter()
                .any(|(_, c)| c.all_files().iter().any(|f| self.find_file(&f.rel_path).is_some()));
        let downloading = self.progress.lock().unwrap().contains_key("pkg-dictation");
        let versions_current = components
            .iter()
            .all(|(name, c)| installed.dictation_components.get(name) == Some(&c.version));
        let state = if downloading {
            "downloading"
        } else if files_complete && versions_current {
            "installed"
        } else if any_state {
            "update_available"
        } else {
            "not_installed"
        };
        // Bytes the install button would actually fetch (missing files only).
        let pending_bytes: u64 = components
            .iter()
            .flat_map(|(_, c)| c.all_files())
            .filter(|f| self.find_file(&f.rel_path).is_none())
            .map(|f| f.bytes)
            .sum();
        serde_json::json!({
            "dictation": {
                "installed_version": installed.dictation_version,
                "available_version": m.dictation.version,
                "state": state,
                "pending_bytes": pending_bytes,
                "progress": self.progress.lock().unwrap().get("pkg-dictation").copied().unwrap_or(0.0),
            },
            "voices_version": m.voices.version,
            "manifest_age_secs": self.manifest_age_secs(),
        })
    }

    /// Install or update the dictation package: for each platform component,
    /// re-download files when its version changed (stable rel_paths, so
    /// delete-then-download replaces in place) or when files are missing.
    /// Unchanged-and-present components cost nothing.
    pub async fn install_dictation(&self, app: &tauri::AppHandle) -> Result<(), String> {
        let m = self.manifest();
        let installed = self.installed_packages();
        let components = platform_components(&m);

        let mut work: Vec<ModelFile> = Vec::new();
        for (name, c) in &components {
            let version_changed = installed.dictation_components.get(name) != Some(&c.version);
            for f in c.all_files() {
                let missing = self.find_file(&f.rel_path).is_none();
                if version_changed || missing {
                    if version_changed {
                        let _ = std::fs::remove_file(self.base_dir.join(&f.rel_path));
                    }
                    work.push(f);
                }
            }
        }

        if !work.is_empty() {
            self.download_files("pkg-dictation", &work, app).await?;
        }

        // Byte verification for the components that declare exact sizes.
        for (_, c) in &components {
            if !c.verify_bytes {
                continue;
            }
            for f in c.all_files() {
                let path = self.base_dir.join(&f.rel_path);
                let actual = std::fs::metadata(&path).map(|md| md.len()).unwrap_or(0);
                if f.bytes > 0 && actual != f.bytes {
                    let _ = std::fs::remove_file(&path);
                    return Err(format!(
                        "download verification failed for {} ({} bytes, expected {})",
                        f.rel_path, actual, f.bytes
                    ));
                }
            }
        }

        let mut rec = InstalledPackages {
            dictation_version: m.dictation.version,
            dictation_components: HashMap::new(),
        };
        for (name, c) in &components {
            rec.dictation_components.insert(name.clone(), c.version);
        }
        self.save_installed_packages(&rec)?;
        log::info!("Dictation package installed at version {}", m.dictation.version);
        Ok(())
    }

    /// Grammar model paths, only when every file is on disk. The grammar
    /// stage treats None exactly like the old not-compiled-in state.
    pub fn grammar_files(&self) -> Option<GrammarFilePaths> {
        let by_role = |role: &str| -> Option<PathBuf> {
            let m = self.manifest.lock().unwrap();
            let c = m.dictation.components.get("grammar")?.clone();
            drop(m);
            c.files
                .iter()
                .find(|f| f.role == role)
                .and_then(|f| self.find_file(&f.rel_path))
        };
        Some(GrammarFilePaths {
            router_model: by_role("router_model")?,
            router_tokenizer: by_role("router_tokenizer")?,
            encoder: by_role("encoder")?,
            decoder: by_role("decoder")?,
            kv_weights: by_role("kv_weights")?,
            t5_tokenizer: by_role("t5_tokenizer")?,
            config: by_role("config")?,
        })
    }

    fn default_base_dir() -> Result<PathBuf, String> {
        #[cfg(target_os = "android")]
        {
            std::env::var_os("VERBA_DATA_DIR")
                .map(|d| PathBuf::from(d).join("models"))
                .ok_or_else(|| "VERBA_DATA_DIR not set".into())
        }
        #[cfg(not(target_os = "android"))]
        {
            dirs::data_dir()
                .map(|d| d.join("verba").join("models"))
                .ok_or_else(|| "no data dir".into())
        }
    }

    pub fn set_active(&self, id: &str) -> Result<(), String> {
        if !self.is_downloaded(id) {
            return Err("model not downloaded".into());
        }
        *self.active_model.lock().unwrap() = id.to_string();

        // Persist to config
        let mut cfg = crate::config::AppConfig::load();
        cfg.active_model_id = id.to_string();
        if let Err(e) = cfg.save() {
            log::warn!("Failed to persist active model: {e}");
        }
        Ok(())
    }

    pub fn active_model_id(&self) -> String {
        self.active_model.lock().unwrap().clone()
    }

    /// Clear the active model selection and persist the change.
    /// Called when a model fails to load so the next startup falls back to
    /// the preferred list rather than looping on the same broken model.
    pub fn clear_active(&self) {
        *self.active_model.lock().unwrap() = String::new();
        let mut cfg = crate::config::AppConfig::load();
        cfg.active_model_id = String::new();
        if let Err(e) = cfg.save() {
            log::warn!("Failed to clear active model in config: {e}");
        }
    }

    pub fn list(&self) -> Vec<ModelInfo> {
        let registry = self.registry.lock().unwrap().clone();
        let progress = self.progress.lock().unwrap();
        let active = self.active_model.lock().unwrap();
        registry
            .iter()
            .map(|m| {
                let (status, prog) = if *active == m.id && self.is_downloaded(&m.id) {
                    ("active".into(), 1.0)
                } else if let Some(&p) = progress.get(&m.id) {
                    ("downloading".into(), p)
                } else if self.is_downloaded(&m.id) {
                    ("downloaded".into(), 1.0)
                } else {
                    ("not_downloaded".into(), 0.0)
                };
                ModelInfo {
                    id: m.id.clone(),
                    name: m.name.clone(),
                    desc: m.desc.clone(),
                    engine: m.engine.clone(),
                    size: m.size.clone(),
                    status,
                    progress: prog,
                }
            })
            .collect()
    }

    pub fn find(&self, id: &str) -> Option<ModelDef> {
        self.registry.lock().unwrap().iter().find(|m| m.id == id).cloned()
    }

    /// Build a `ModelEngine` for a downloaded model.
    pub fn model_engine(&self, id: &str) -> Option<crate::transcribe::ModelEngine> {
        let model = self.find(id)?;
        if !self.is_downloaded(id) {
            return None;
        }

        let path = |role: &str| -> Option<String> {
            self.find_file_by_role(&model.files, role)
                .map(|p| p.to_string_lossy().into_owned())
        };

        match model.engine.as_str() {
            "parakeet" => Some(crate::transcribe::ModelEngine::Transducer {
                encoder: path("encoder")?,
                decoder: path("decoder")?,
                joiner: path("joiner")?,
                tokens: path("tokens")?,
                model_type: "nemo_transducer".into(),
            }),
            "zipformer" => Some(crate::transcribe::ModelEngine::Transducer {
                encoder: path("encoder")?,
                decoder: path("decoder")?,
                joiner: path("joiner")?,
                tokens: path("tokens")?,
                model_type: "transducer".into(),
            }),
            "whisper" => Some(crate::transcribe::ModelEngine::Whisper {
                encoder: path("encoder")?,
                decoder: path("decoder")?,
                tokens: path("tokens")?,
                language: "en".into(),
            }),
            "conformer_ctc" => Some(crate::transcribe::ModelEngine::NemoCTC {
                model: path("model")?,
                tokens: path("tokens")?,
            }),
            _ => None,
        }
    }

    pub fn tts_model_config(&self, id: &str) -> Option<crate::tts::TtsModelConfig> {
        let model = self.find(id)?;
        if !self.is_downloaded(id) {
            return None;
        }

        let path = |role: &str| -> Option<String> {
            self.find_file_by_role(&model.files, role)
                .map(|p| p.to_string_lossy().into_owned())
        };

        match model.engine.as_str() {
            "tts_piper_ort" => {
                Some(crate::tts::TtsModelConfig::PiperOrt {
                    model: path("model")?,
                    config: path("config")?,
                })
            }
            _ => None,
        }
    }

    pub fn is_tts_model(&self, id: &str) -> bool {
        self.find(id)
            .map(|m| m.engine.starts_with("tts_"))
            .unwrap_or(false)
    }

    pub fn find_file_by_role(&self, files: &[ModelFile], role: &str) -> Option<std::path::PathBuf> {
        files
            .iter()
            .find(|f| f.role == role)
            .and_then(|f| self.find_file(&f.rel_path))
    }

    /// Find the best downloaded model. Checks the active model first,
    /// then falls back to preferred order (Parakeet INT8, then Whisper INT8).
    pub fn first_downloaded_model(&self) -> Option<(String, crate::transcribe::ModelEngine)> {
        // Check active model first
        {
            let active = self.active_model.lock().unwrap();
            if !active.is_empty() {
                if let Some(engine) = self.model_engine(&active) {
                    return Some((active.clone(), engine));
                }
            }
        }

        // Platform-appropriate order: quantized first on Android (memory,
        // speed), full precision first on desktop, the other as fallback so
        // an install that downloaded the opposite variant keeps working.
        let preferred: [&str; 2] = if cfg!(target_os = "android") {
            ["parakeet-tdt-0.6b-v3-int8", "parakeet-tdt-0.6b-v3"]
        } else {
            ["parakeet-tdt-0.6b-v3", "parakeet-tdt-0.6b-v3-int8"]
        };
        for id in preferred {
            if let Some(engine) = self.model_engine(id) {
                return Some((id.to_string(), engine));
            }
        }
        None
    }

    /// Backwards-compatible wrapper for code that only needs Parakeet paths.
    pub fn first_downloaded_parakeet(&self) -> Option<(String, (String, String, String, String))> {
        let preferred: [&str; 2] = if cfg!(target_os = "android") {
            ["parakeet-tdt-0.6b-v3-int8", "parakeet-tdt-0.6b-v3"]
        } else {
            ["parakeet-tdt-0.6b-v3", "parakeet-tdt-0.6b-v3-int8"]
        };
        for id in preferred {
            if let Some(crate::transcribe::ModelEngine::Transducer { encoder, decoder, joiner, tokens, .. }) =
                self.model_engine(id)
            {
                return Some((id.to_string(), (encoder, decoder, joiner, tokens)));
            }
        }
        None
    }

    /// Path where the Silero VAD model lives (vad component of the
    /// dictation package).
    pub fn vad_model_path(&self) -> PathBuf {
        self.base_dir.join("vad/silero_vad.onnx")
    }

    /// Resolve the Silero VAD model on disk. No longer embedded in the
    /// binary: it downloads with the dictation package. Installs from before
    /// the package system wrote it to the models root — honor that copy so
    /// they keep dictating without re-downloading anything.
    pub fn ensure_vad_model(&self) -> Result<PathBuf, String> {
        let path = self.vad_model_path();
        if path.exists() {
            return Ok(path);
        }
        let legacy = self.base_dir.join("silero_vad.onnx");
        if legacy.exists() {
            return Ok(legacy);
        }
        Err("Voice detection model not downloaded — install the dictation package".into())
    }

    pub fn delete(&self, id: &str) -> Result<(), String> {
        let model = self.find(id).ok_or("unknown model")?;

        // Clear active if deleting the active model
        {
            let mut active = self.active_model.lock().unwrap();
            if *active == id {
                *active = String::new();
                let mut cfg = crate::config::AppConfig::load();
                cfg.active_model_id = String::new();
                let _ = cfg.save();
            }
        }

        let mut deleted = 0u32;
        for file in &model.files {
            let path = self.base_dir.join(&file.rel_path);
            if path.exists() {
                std::fs::remove_file(&path).map_err(|e| format!("delete {}: {e}", file.rel_path))?;
                deleted += 1;
            }
        }

        // Clean up empty parent directories
        for file in &model.files {
            let path = self.base_dir.join(&file.rel_path);
            if let Some(parent) = path.parent() {
                let _ = Self::remove_empty_dirs(parent, &self.base_dir);
            }
        }

        log::info!("Deleted model {id} ({deleted} files)");
        Ok(())
    }

    /// Remove empty directories up to (but not including) `stop_at`.
    fn remove_empty_dirs(dir: &std::path::Path, stop_at: &std::path::Path) -> std::io::Result<()> {
        let mut current = dir.to_path_buf();
        while current != stop_at && current.starts_with(stop_at) {
            match std::fs::read_dir(&current) {
                Ok(mut entries) => {
                    if entries.next().is_none() {
                        std::fs::remove_dir(&current)?;
                    } else {
                        break;
                    }
                }
                Err(_) => break,
            }
            match current.parent() {
                Some(p) => current = p.to_path_buf(),
                None => break,
            }
        }
        Ok(())
    }

    pub fn is_downloaded(&self, id: &str) -> bool {
        let Some(model) = self.find(id) else {
            return false;
        };
        model.files.iter().all(|f| self.find_file(&f.rel_path).is_some())
    }

    fn find_file(&self, rel_path: &str) -> Option<PathBuf> {
        let p = self.base_dir.join(rel_path);
        if p.exists() {
            return Some(p);
        }
        for dir in &self.alt_dirs {
            let p = dir.join(rel_path);
            if p.exists() {
                return Some(p);
            }
        }
        None
    }

    pub async fn download(&self, id: &str, app: &tauri::AppHandle) -> Result<(), String> {
        let model = self.find(id).ok_or("unknown model")?;
        self.download_files(id, &model.files, app).await
    }

    /// Shared per-file download loop: tmp + rename, skip-if-exists, progress
    /// events under `progress_id`. Used by both single-model downloads (the
    /// Voices tab) and dictation package installs.
    async fn download_files(
        &self,
        progress_id: &str,
        files: &[ModelFile],
        app: &tauri::AppHandle,
    ) -> Result<(), String> {
        let id = progress_id;
        let base_dir = self.base_dir.clone();

        // Init progress
        self.progress.lock().unwrap().insert(id.to_string(), 0.0);

        let total_bytes: u64 = files.iter().map(|f| f.bytes).sum();
        let mut downloaded: u64 = 0;
        let client = reqwest::Client::new();

        for file in files {
            let dest = base_dir.join(&file.rel_path);
            if let Some(parent) = dest.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(|e| format!("create dir: {e}"))?;
            }

            // Skip if already exists
            if dest.exists() {
                downloaded += file.bytes;
                continue;
            }

            let resp = client
                .get(&file.url)
                .send()
                .await
                .map_err(|e| format!("HTTP request: {e}"))?;

            if !resp.status().is_success() {
                self.progress.lock().unwrap().remove(id);
                return Err(format!("HTTP {}", resp.status()));
            }

            let tmp = dest.with_extension("tmp");
            let mut out = tokio::fs::File::create(&tmp)
                .await
                .map_err(|e| format!("create file: {e}"))?;

            let mut stream = resp.bytes_stream();
            let mut last_emit = Instant::now();

            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|e| format!("download: {e}"))?;
                out.write_all(&chunk)
                    .await
                    .map_err(|e| format!("write: {e}"))?;
                downloaded += chunk.len() as u64;

                // Throttle progress updates to 500ms
                if total_bytes > 0 && last_emit.elapsed().as_millis() > 500 {
                    let pct = downloaded as f64 / total_bytes as f64;
                    self.progress.lock().unwrap().insert(id.to_string(), pct);
                    let _ = app.emit(
                        "download-progress",
                        serde_json::json!({ "id": id, "progress": pct }),
                    );
                    last_emit = Instant::now();
                }
            }

            out.flush().await.map_err(|e| format!("flush: {e}"))?;
            drop(out);
            tokio::fs::rename(&tmp, &dest)
                .await
                .map_err(|e| format!("rename: {e}"))?;
        }

        // Done
        self.progress.lock().unwrap().remove(id);
        let _ = app.emit("download-complete", serde_json::json!({ "id": id }));
        Ok(())
    }

    // ── Storage management ──

    /// Byte totals per user-facing storage category. `unclaimed` is anything
    /// under models/ that no current registry entry, package component, or
    /// bookkeeping file owns — legacy downloads from the old model zoo.
    pub fn storage_summary(&self) -> serde_json::Value {
        let m = self.manifest();
        let mut owned: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
        let mut dictation: u64 = 0;
        for (_, c) in platform_components(&m) {
            for f in c.all_files() {
                let p = self.base_dir.join(&f.rel_path);
                dictation += std::fs::metadata(&p).map(|md| md.len()).unwrap_or(0);
                owned.insert(p);
            }
        }
        // The non-platform ASR variant is still "owned" (not legacy junk).
        for c in m.dictation.components.values() {
            for f in c.all_files() {
                owned.insert(self.base_dir.join(&f.rel_path));
            }
        }
        let mut voices: u64 = 0;
        for v in &m.voices.list {
            for f in &v.files {
                let p = self.base_dir.join(&f.rel_path);
                voices += std::fs::metadata(&p).map(|md| md.len()).unwrap_or(0);
                owned.insert(p);
            }
        }
        for keep in ["manifest.json", "packages.json"] {
            owned.insert(self.base_dir.join(keep));
        }
        // Custom voices imported by the user live under tts/custom.
        let custom_dir = self.base_dir.join("tts/custom");
        let custom = dir_size(&custom_dir);
        let mut unclaimed: u64 = 0;
        walk_files(&self.base_dir, &mut |p, len| {
            if !owned.contains(p) && !p.starts_with(&custom_dir) {
                unclaimed += len;
            }
        });
        serde_json::json!({
            "dictation": dictation,
            "voices": voices,
            "custom_voices": custom,
            "unclaimed": unclaimed,
        })
    }

    /// Delete a storage category. Returns bytes reclaimed.
    pub fn storage_clear(&self, category: &str) -> Result<u64, String> {
        let m = self.manifest();
        let before = self.storage_summary();
        let take = |v: &serde_json::Value, k: &str| v.get(k).and_then(|x| x.as_u64()).unwrap_or(0);
        match category {
            "dictation" => {
                for c in m.dictation.components.values() {
                    for f in c.all_files() {
                        let _ = std::fs::remove_file(self.base_dir.join(&f.rel_path));
                    }
                }
                let _ = std::fs::remove_file(self.packages_path());
                let _ = std::fs::remove_file(self.base_dir.join("silero_vad.onnx"));
                Ok(take(&before, "dictation"))
            }
            "voices" => {
                for v in &m.voices.list {
                    for f in &v.files {
                        let _ = std::fs::remove_file(self.base_dir.join(&f.rel_path));
                    }
                }
                self.clear_active();
                Ok(take(&before, "voices"))
            }
            "unclaimed" => {
                let mut owned: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
                for c in m.dictation.components.values() {
                    for f in c.all_files() {
                        owned.insert(self.base_dir.join(&f.rel_path));
                    }
                }
                for v in &m.voices.list {
                    for f in &v.files {
                        owned.insert(self.base_dir.join(&f.rel_path));
                    }
                }
                for keep in ["manifest.json", "packages.json"] {
                    owned.insert(self.base_dir.join(keep));
                }
                let custom_dir = self.base_dir.join("tts/custom");
                let mut victims: Vec<PathBuf> = Vec::new();
                walk_files(&self.base_dir, &mut |p, _| {
                    if !owned.contains(p) && !p.starts_with(&custom_dir) {
                        victims.push(p.to_path_buf());
                    }
                });
                for v in &victims {
                    let _ = std::fs::remove_file(v);
                    if let Some(parent) = v.parent() {
                        let _ = Self::remove_empty_dirs(parent, &self.base_dir);
                    }
                }
                Ok(take(&before, "unclaimed"))
            }
            other => Err(format!("unknown storage category: {other}")),
        }
    }
}

/// Recursive size of a directory (0 if absent).
fn dir_size(dir: &std::path::Path) -> u64 {
    let mut total = 0u64;
    walk_files(dir, &mut |_, len| total += len);
    total
}

/// Depth-first file walk calling `f(path, len)` per regular file.
fn walk_files(dir: &std::path::Path, f: &mut dyn FnMut(&std::path::Path, u64)) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk_files(&path, f);
        } else if let Ok(md) = entry.metadata() {
            f(&path, md.len());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn test_manager(dir: &std::path::Path) -> ModelManager {
        let manifest = parse_manifest(EMBEDDED_MANIFEST).unwrap();
        let registry = registry_from_manifest(&manifest);
        ModelManager {
            base_dir: dir.to_path_buf(),
            alt_dirs: vec![],
            manifest: Mutex::new(manifest),
            registry: Mutex::new(registry),
            progress: Mutex::new(HashMap::new()),
            active_model: Mutex::new(String::new()),
        }
    }

    /// Write every file of a registry entry so is_downloaded() sees it.
    fn fake_download(mgr: &ModelManager, id: &str) {
        let model = mgr.find(id).unwrap();
        for f in &model.files {
            let p = mgr.base_dir.join(&f.rel_path);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, b"fake").unwrap();
        }
    }

    #[test]
    fn embedded_manifest_parses_and_is_complete() {
        let m = parse_manifest(EMBEDDED_MANIFEST).expect("embedded manifest must parse");
        assert_eq!(m.schema, 1);
        for key in ["asr-mobile", "asr-desktop", "vad", "grammar"] {
            let c = m.dictation.components.get(key).unwrap_or_else(|| panic!("missing {key}"));
            assert!(!c.all_files().is_empty(), "{key} has no files");
        }
        assert_eq!(m.dictation.components["grammar"].files.len(), 7);
        assert!(m.voices.list.len() >= 9, "voice list shrank unexpectedly");
    }

    #[test]
    fn registry_has_parakeet_and_voices() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = test_manager(dir.path());
        assert!(mgr.find("parakeet-tdt-0.6b-v3-int8").is_some());
        assert!(mgr.find("parakeet-tdt-0.6b-v3").is_some());
        assert!(mgr.find("tts-piper-alba").is_some());
        // The old model zoo is gone.
        assert!(mgr.find("whisper-base.en-int8").is_none());
        assert!(mgr.find("parakeet-tdt-0.6b-v2-int8").is_none());
    }

    #[test]
    fn registry_ids_are_unique() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = test_manager(dir.path());
        let list = mgr.list();
        let mut ids: Vec<String> = list.iter().map(|m| m.id.clone()).collect();
        let original_len = ids.len();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), original_len, "duplicate model IDs in registry");
    }

    #[test]
    fn not_downloaded_when_files_missing() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = test_manager(dir.path());
        assert!(!mgr.is_downloaded("parakeet-tdt-0.6b-v3-int8"));
    }

    #[test]
    fn parakeet_requires_all_files() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = test_manager(dir.path());

        let enc_dir = dir.path().join("parakeet/v3-int8");
        fs::create_dir_all(&enc_dir).unwrap();
        fs::write(enc_dir.join("encoder.int8.onnx"), b"fake").unwrap();
        assert!(!mgr.is_downloaded("parakeet-tdt-0.6b-v3-int8"));

        fake_download(&mgr, "parakeet-tdt-0.6b-v3-int8");
        assert!(mgr.is_downloaded("parakeet-tdt-0.6b-v3-int8"));
    }

    #[test]
    fn model_engine_returns_transducer() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = test_manager(dir.path());
        fake_download(&mgr, "parakeet-tdt-0.6b-v3-int8");
        let engine = mgr.model_engine("parakeet-tdt-0.6b-v3-int8").unwrap();
        assert!(matches!(engine, crate::transcribe::ModelEngine::Transducer { .. }));
    }

    #[test]
    fn set_active_requires_downloaded() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = test_manager(dir.path());
        assert!(mgr.set_active("parakeet-tdt-0.6b-v3-int8").is_err());
    }

    #[test]
    fn list_shows_downloaded_status() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = test_manager(dir.path());
        fake_download(&mgr, "tts-piper-alba");
        let list = mgr.list();
        let alba = list.iter().find(|m| m.id == "tts-piper-alba").unwrap();
        assert_eq!(alba.status, "downloaded");
    }

    #[test]
    fn vad_prefers_component_path_then_legacy() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = test_manager(dir.path());
        assert!(mgr.ensure_vad_model().is_err());

        // Legacy location (pre-package installs).
        fs::write(dir.path().join("silero_vad.onnx"), b"fake").unwrap();
        assert_eq!(mgr.ensure_vad_model().unwrap(), dir.path().join("silero_vad.onnx"));

        // Component location wins once present.
        fs::create_dir_all(dir.path().join("vad")).unwrap();
        fs::write(dir.path().join("vad/silero_vad.onnx"), b"fake").unwrap();
        assert_eq!(mgr.ensure_vad_model().unwrap(), dir.path().join("vad/silero_vad.onnx"));
    }

    #[test]
    fn grammar_files_requires_all_seven() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = test_manager(dir.path());
        assert!(mgr.grammar_files().is_none());

        let m = mgr.manifest();
        let files = m.dictation.components["grammar"].files.clone();
        for (i, f) in files.iter().enumerate() {
            let p = dir.path().join(&f.rel_path);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, b"fake").unwrap();
            if i + 1 < files.len() {
                assert!(mgr.grammar_files().is_none(), "partial grammar must be None");
            }
        }
        assert!(mgr.grammar_files().is_some());
    }

    #[test]
    fn packages_status_states() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = test_manager(dir.path());

        let st = mgr.packages_status();
        assert_eq!(st["dictation"]["state"], "not_installed");

        // Some files present but no recorded install -> update_available.
        fake_download(&mgr, if cfg!(target_os = "android") { "parakeet-tdt-0.6b-v3-int8" } else { "parakeet-tdt-0.6b-v3" });
        let st = mgr.packages_status();
        assert_eq!(st["dictation"]["state"], "update_available");

        // Everything present + versions recorded -> installed.
        let m = mgr.manifest();
        let mut rec = InstalledPackages { dictation_version: m.dictation.version, dictation_components: HashMap::new() };
        for (name, c) in platform_components(&m) {
            for f in c.all_files() {
                let p = dir.path().join(&f.rel_path);
                fs::create_dir_all(p.parent().unwrap()).unwrap();
                fs::write(p, b"fake").unwrap();
            }
            rec.dictation_components.insert(name, c.version);
        }
        mgr.save_installed_packages(&rec).unwrap();
        let st = mgr.packages_status();
        assert_eq!(st["dictation"]["state"], "installed");
        assert_eq!(st["dictation"]["pending_bytes"], 0);

        // A component version bump flips it back to update_available.
        let mut stale = rec.clone();
        stale.dictation_components.insert("grammar".into(), 0);
        mgr.save_installed_packages(&stale).unwrap();
        let st = mgr.packages_status();
        assert_eq!(st["dictation"]["state"], "update_available");
    }

    #[test]
    fn storage_summary_counts_and_clears_unclaimed() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = test_manager(dir.path());
        fake_download(&mgr, "tts-piper-alba");

        // A legacy whisper file no registry entry owns.
        let legacy = dir.path().join("whisper/base.en-int8");
        fs::create_dir_all(&legacy).unwrap();
        fs::write(legacy.join("encoder.int8.onnx"), vec![0u8; 1000]).unwrap();

        let s = mgr.storage_summary();
        assert!(s["voices"].as_u64().unwrap() > 0);
        assert_eq!(s["unclaimed"].as_u64().unwrap(), 1000);

        let freed = mgr.storage_clear("unclaimed").unwrap();
        assert_eq!(freed, 1000);
        assert!(!legacy.join("encoder.int8.onnx").exists());
        // Owned voice files survive an unclaimed clear.
        assert!(mgr.is_downloaded("tts-piper-alba"));
    }

    #[test]
    fn first_downloaded_parakeet_platform_order() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = test_manager(dir.path());
        fake_download(&mgr, "parakeet-tdt-0.6b-v3-int8");
        fake_download(&mgr, "parakeet-tdt-0.6b-v3");
        let (id, _) = mgr.first_downloaded_parakeet().unwrap();
        if cfg!(target_os = "android") {
            assert_eq!(id, "parakeet-tdt-0.6b-v3-int8");
        } else {
            assert_eq!(id, "parakeet-tdt-0.6b-v3");
        }
    }
}
