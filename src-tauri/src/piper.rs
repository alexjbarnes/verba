//! Piper VITS TTS via the `ort` crate + `piper-plus-g2p` (no espeak / GPL).
//!
//! Runs the Piper `.onnx` model directly: text is phonemized to IPA by
//! `piper-plus-g2p` (CMU dict, bundled here as JSON), encoded to phoneme ids
//! against the model's `phoneme_id_map`, then fed to ONNX Runtime. OOV words
//! are spelled letter-by-letter so they still produce audio instead of
//! vanishing.
//!
//! This is the GPL-free alternative to the sherpa-onnx TTS path in `tts.rs`;
//! both coexist. The ort init and CPU-EP session build mirror
//! `postprocess::grammar_neural` so ORT finds `libonnxruntime.so` the same way.

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use ort::session::{builder::GraphOptimizationLevel, Session};
use ort::value::Tensor;

use piper_plus_g2p::english::EnglishPhonemizer;
use piper_plus_g2p::{PhonemeIdMap, Phonemizer, PiperEncoder, UnknownTokenMode};

use serde::Deserialize;

/// CMU pronunciation dictionary (word -> ARPABET), embedded at compile time so
/// the phonemizer needs no filesystem or env var at runtime. Parsed once into a
/// `HashMap<String, String>` and handed to `EnglishPhonemizer::new_with_hashmap`.
static CMUDICT_BYTES: &[u8] = include_bytes!("../data/cmudict_data.json");
/// British English dictionary (wikipron-derived, espeak en-gb-x-rp
/// conventions, stress transferred from CMUdict). Built by
/// scripts/build_gb_dict.py; ~76k words -> final IPA strings.
static GB_DICT_BYTES: &[u8] = include_bytes!("../data/gb_dict.json");

/// GB-only pronunciation fixes, final IPA (same conventions as gb_dict.json).
/// For words the GB dictionary lacks or gets wrong where the US override +
/// transform fallback isn't right either (lexical UK/US differences).
const GB_PRONUNCIATION_OVERRIDES: &[(&str, &str)] = &[
    ("schedule", "ʃˈɛdjuːl"),
    ("schedules", "ʃˈɛdjuːlz"),
    ("scheduled", "ʃˈɛdjuːld"),
    ("scheduling", "ʃˈɛdjuːlɪŋ"),
    ("z", "zˈɛd"),
    // GB dictionary entries that shadow the US path with a bad variant or a
    // mis-transferred stress (the GB lookup wins, so these must be fixed here,
    // not in PRONUNCIATION_OVERRIDES). espeak-en-gb-x-rp-verified forms.
    ("dig", "dˈɪɡ"),
    ("microsoft", "mˈaɪkɹəsˌɒft"),
    ("weaponization", "wˌɛpənaɪzˈeɪʃən"),
    ("recursive", "ɹɪkˈɜːsɪv"),
    ("recursively", "ɹɪkˈɜːsɪvlɪ"),
];

/// Piper `.onnx.json` sidecar: audio params, speaker count, phoneme id map and
/// the optional default inference scales.
#[derive(Deserialize)]
struct PiperConfig {
    audio: PiperAudio,
    #[serde(default)]
    num_speakers: i64,
    #[serde(default)]
    inference: PiperInference,
    phoneme_id_map: HashMap<String, Vec<i64>>,
    /// Which espeak voice the model was trained against. Never used to run
    /// espeak — it identifies the locale ("en-gb*" selects the GB dictionary
    /// and the US->RP fallback transform).
    #[serde(default)]
    espeak: PiperEspeak,
}

#[derive(Deserialize, Default)]
struct PiperEspeak {
    #[serde(default)]
    voice: String,
}

#[derive(Deserialize)]
struct PiperAudio {
    sample_rate: u32,
}

/// Default generation scales from the model card. Falls back to the Piper
/// defaults when the sidecar omits a field.
#[derive(Deserialize)]
struct PiperInference {
    #[serde(default = "default_noise_scale")]
    noise_scale: f32,
    #[serde(default = "default_length_scale")]
    length_scale: f32,
    #[serde(default = "default_noise_w")]
    noise_w: f32,
}

impl Default for PiperInference {
    fn default() -> Self {
        Self {
            noise_scale: default_noise_scale(),
            length_scale: default_length_scale(),
            noise_w: default_noise_w(),
        }
    }
}

fn default_noise_scale() -> f32 { 0.667 }
fn default_length_scale() -> f32 { 1.0 }
fn default_noise_w() -> f32 { 0.8 }

/// Read `num_speakers` from a Piper `.onnx.json` sidecar without loading the
/// model (lets the UI show the speaker picker before generation). Returns 0
/// when the file is missing or unparseable.
pub fn num_speakers_from_config(json_path: &std::path::Path) -> i32 {
    #[derive(Deserialize)]
    struct OnlySpeakers {
        #[serde(default)]
        num_speakers: i64,
    }
    std::fs::read_to_string(json_path)
        .ok()
        .and_then(|s| serde_json::from_str::<OnlySpeakers>(&s).ok())
        .map(|c| c.num_speakers as i32)
        .unwrap_or(0)
}

/// Identity of a model file for cache keys: file name + byte length. Shared by
/// `PiperEngine::load` and the cache-coverage path so the two can never compute
/// a different key for the same model (a mismatch would make every lookup miss).
pub fn model_fingerprint(model_path: &str) -> String {
    let len = std::fs::metadata(model_path).map(|m| m.len()).unwrap_or(0);
    let name = std::path::Path::new(model_path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("model");
    format!("{name}:{len}")
}

/// Bump when data/gb_dict.json (or the RP transform) changes pronunciations:
/// GB models' cached audio embeds the old phoneme stream, so their cache keys
/// must roll without invalidating US models' caches.
const GB_DICT_VERSION: u32 = 3;

/// True when the sidecar declares a GB espeak voice (same check as engine load).
fn config_is_gb(config_path: &str) -> bool {
    #[derive(Deserialize)]
    struct OnlyEspeak {
        #[serde(default)]
        espeak: PiperEspeak,
    }
    std::fs::read_to_string(config_path)
        .ok()
        .and_then(|s| serde_json::from_str::<OnlyEspeak>(&s).ok())
        .map(|c| c.espeak.voice.starts_with("en-gb"))
        .unwrap_or(false)
}

/// Cache fingerprint for a model+config pair: the file identity plus, for
/// GB-locale models, the bundled dictionary version. All cache producers and
/// consumers (engine load, cache coverage, cache-only playback) go through
/// this so they can never disagree.
pub fn cache_fingerprint(model_path: &str, config_path: &str) -> String {
    let fp = model_fingerprint(model_path);
    if config_is_gb(config_path) {
        format!("{fp}+gb{GB_DICT_VERSION}")
    } else {
        fp
    }
}

/// Per-segment cache coverage for an article, computed WITHOUT loading ORT (so
/// the reading view can show what's already on disk before play). Mirrors
/// `synth_chunk`'s segmentation, key, and speaker clamp exactly, reading each
/// entry's header (not its PCM). Returns cached ms-ranges merged into blocks
/// along an article timeline (cached segments use their real duration, uncached
/// a word-rate estimate), the total ms, and whether every segment is cached.
pub struct CacheCoverage {
    pub ranges: Vec<(u64, u64)>,
    pub total_ms: u64,
    pub cached_ms: u64,
    pub total_segments: u32,
    pub cached_segments: u32,
}

impl CacheCoverage {
    /// True if every segment this text needs is already on disk — the article
    /// can be played straight from the cache with the ONNX engine never loaded.
    pub fn is_all_cached(&self) -> bool {
        self.total_segments > 0 && self.cached_segments == self.total_segments
    }
}

/// Read `(sample_rate, num_speakers)` from a Piper `.onnx.json` sidecar without
/// loading the ORT session. Shared by `cache_coverage` and the cache-only
/// playback path, both of which need this metadata before (or instead of)
/// committing to the expensive engine load.
pub fn read_piper_meta(config_path: &str) -> Option<(u32, i64)> {
    let cfg_text = std::fs::read_to_string(config_path).ok()?;
    let cfg: PiperConfig = serde_json::from_str(&cfg_text).ok()?;
    Some((cfg.audio.sample_rate.max(1), cfg.num_speakers))
}

/// Clamp a requested speaker id into a model's valid range (0 if it declares no
/// speakers). Shared by `cache_coverage` and cache-only playback so both derive
/// the same cache key for the same request.
fn clamp_speaker(sid: i32, num_speakers: i64) -> i32 {
    if num_speakers > 0 {
        (sid as i64).clamp(0, num_speakers - 1) as i32
    } else {
        0
    }
}

pub fn cache_coverage(
    model_path: &str,
    config_path: &str,
    sid: i32,
    speed: f32,
    text: &str,
) -> CacheCoverage {
    let mut cov = CacheCoverage {
        ranges: Vec::new(),
        total_ms: 0,
        cached_ms: 0,
        total_segments: 0,
        cached_segments: 0,
    };
    let Some((sample_rate, num_speakers)) = read_piper_meta(config_path) else { return cov };
    let sr = sample_rate as u64;
    let model_fp = cache_fingerprint(model_path, config_path);
    let speed_milli = (speed.max(0.0) * 1000.0).round() as u32;
    let speed_safe = if speed > 0.0 { speed } else { 1.0 } as f64;
    let speaker = clamp_speaker(sid, num_speakers);

    // Replicate the synthesis segmentation EXACTLY so the keys match what
    // synth_chunk stored: speak() chunks the text via split_sentences +
    // batch_sentences, then synth_chunk runs split_for_pauses on each CHUNK.
    // Running split_for_pauses on the whole article instead yields different
    // segment strings at chunk boundaries, so every lookup would miss and the
    // total would fall back to the estimate.
    let clean = text.replace('\u{0}', " ");
    let chunks = crate::tts::batch_sentences(crate::tts::split_sentences(&clean), 15, 45);
    let mut cum: u64 = 0;
    for (segment, pause_ms) in chunks.iter().flat_map(|c| split_for_pauses(c)) {
        cov.total_segments += 1;
        let key = crate::tts_cache::key(&model_fp, speaker, speed_milli, &segment);
        match crate::tts_cache::cached_meta(&key, sr as u32, &segment) {
            Some(pcm_len) => {
                cov.cached_segments += 1;
                let speech_ms = (pcm_len as u64 * 1000) / sr;
                // Cover speech + its trailing pause so consecutive cached
                // segments form one solid block after the merge below.
                cov.ranges.push((cum, cum + speech_ms + pause_ms as u64));
                cov.cached_ms += speech_ms + pause_ms as u64;
                cum += speech_ms + pause_ms as u64;
            }
            None => {
                let words = segment.split_whitespace().count() as f64;
                let est = (words / 255.0 * 60_000.0 / speed_safe) as u64;
                cum += est + pause_ms as u64;
            }
        }
    }
    cov.total_ms = cum;

    // Merge touching/overlapping ranges so a run of cached segments is one block.
    cov.ranges.sort_by_key(|r| r.0);
    let mut merged: Vec<(u64, u64)> = Vec::new();
    for (s, e) in cov.ranges.drain(..) {
        match merged.last_mut() {
            Some(last) if s <= last.1 => last.1 = last.1.max(e),
            _ => merged.push((s, e)),
        }
    }
    cov.ranges = merged;
    cov
}

/// Build one chunk's PCM + timing straight from the persistent cache, touching
/// neither the ONNX session nor the phonemizer/encoder — this is what lets
/// playback of a fully-cached article skip the (multi-second) engine load
/// entirely. Segmentation/keying mirrors `synth_chunk` exactly (same
/// `split_for_pauses` per chunk, same key), so this only ever hits entries that
/// `synth_chunk` itself wrote. Returns `None` if ANY segment misses — the
/// caller (`tts::speak_from_cache`) only calls this after `cache_coverage`
/// confirms full coverage, so a miss here means a genuine race (e.g. a
/// concurrent eviction) rather than a normal case.
pub fn synth_chunk_cache_only(
    model_fp: &str,
    sample_rate: i32,
    num_speakers: i64,
    sid: i32,
    speed: f32,
    text: &str,
) -> Option<(Vec<f32>, Vec<SegmentSpan>)> {
    let sr = sample_rate.max(1) as usize;
    let speed_milli = (speed.max(0.0) * 1000.0).round() as u32;
    let speaker = clamp_speaker(sid, num_speakers);

    let mut out: Vec<f32> = Vec::new();
    let mut spans: Vec<SegmentSpan> = Vec::new();
    for (segment, pause_ms) in split_for_pauses(text) {
        let key = crate::tts_cache::key(model_fp, speaker, speed_milli, &segment);
        let cached = crate::tts_cache::get(&key, sr as u32, &segment)?;
        let speech_start = out.len();
        out.extend_from_slice(&cached.pcm);
        let speech_samples = out.len() - speech_start;
        let pause_samples = if pause_ms > 0 && !out.is_empty() {
            pause_ms * sr / 1000
        } else {
            0
        };
        if pause_samples > 0 {
            out.extend(std::iter::repeat(0.0f32).take(pause_samples));
        }
        spans.push(SegmentSpan {
            text: segment,
            speech_ms: (speech_samples as u64 * 1000) / sr as u64,
            pause_ms: (pause_samples as u64 * 1000) / sr as u64,
            word_ms: cached.word_ms,
        });
    }
    Some((out, spans))
}

/// Shortest first piece (so the second word keeps its leading letter) that a
/// dictionary segmentation can start with. 3 keeps common prefixes ("pre",
/// "mis", "non") while excluding 1-2 char noise that fragments everything.
const SEGMENT_MIN_PIECE: usize = 3;

/// Try to split an out-of-vocabulary word into exactly two dictionary words
/// (e.g. "pushbuffer" -> ["push","buffer"], "codebase" -> ["code","base"]) so
/// the phonemizer can pronounce each half instead of spelling the whole thing
/// letter-by-letter. `word_set` is the set of dict keys (lowercased).
///
/// Deliberately conservative: exactly two pieces (binary compounds are the
/// common tech case; 3+ splits multiply garbage like "rat eli miter"), each at
/// least `SEGMENT_MIN_PIECE` chars, and among candidates the one with the
/// SHORTEST first piece — because the frequent failure mode of a greedy match
/// is stealing the second word's first letter ("standa|lone", "pret|raining").
/// Without word-frequency data this can still mis-split the odd word
/// ("datastore" -> "dat|astore"), but even a slightly-off split reads far closer
/// than letter spelling, and acronyms/unsplittable proper nouns return None and
/// fall through to spelling unchanged. Returns None unless a full two-word cover
/// exists.
fn segment_compound(word_set: &std::collections::HashSet<String>, word: &str) -> Option<Vec<String>> {
    let lower = word.to_lowercase();
    let n = lower.len();
    if n < SEGMENT_MIN_PIECE * 2 || !lower.chars().all(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    // Char-boundary safe because we required all-ASCII above.
    for cut in SEGMENT_MIN_PIECE..=(n - SEGMENT_MIN_PIECE) {
        let (head, tail) = (&lower[..cut], &lower[cut..]);
        if word_set.contains(head) && word_set.contains(tail) {
            return Some(vec![head.to_string(), tail.to_string()]);
        }
    }
    None
}

/// A-Z letter-name IPA spell-out table (OOV fallback). Each value is the IPA for
/// the English *name* of the letter as a sequence of single-codepoint IPA
/// symbols (the libritts_r id_map is keyed by single chars). e.g. "GPU" ->
/// dʒiː piː juː. Every symbol used here exists in the libritts_r phoneme_id_map,
/// so `UnknownTokenMode::Strict` will not reject it.
fn letter_ipa(c: char) -> Option<&'static [&'static str]> {
    Some(match c.to_ascii_lowercase() {
        'a' => &["e", "ɪ"],
        'b' => &["b", "iː"],
        'c' => &["s", "iː"],
        'd' => &["d", "iː"],
        'e' => &["iː"],
        'f' => &["ɛ", "f"],
        'g' => &["d", "ʒ", "iː"],
        'h' => &["e", "ɪ", "t", "ʃ"],
        'i' => &["a", "ɪ"],
        'j' => &["d", "ʒ", "e", "ɪ"],
        'k' => &["k", "e", "ɪ"],
        'l' => &["ɛ", "l"],
        'm' => &["ɛ", "m"],
        'n' => &["ɛ", "n"],
        'o' => &["o", "ʊ"],
        'p' => &["p", "iː"],
        'q' => &["k", "j", "uː"],
        'r' => &["ɑː", "ɹ"],
        's' => &["ɛ", "s"],
        't' => &["t", "iː"],
        'u' => &["j", "uː"],
        'v' => &["v", "iː"],
        'w' => &["d", "ʌ", "b", "ə", "l", "j", "uː"],
        'x' => &["ɛ", "k", "s"],
        'y' => &["w", "a", "ɪ"],
        'z' => &["z", "iː"],
        _ => return None,
    })
}

/// Manual pronunciation entries merged into the bundled CMUdict at load time
/// (same lowercase-key, space-separated-ARPAbet format as the dict itself).
/// Reported mispronounced (2026-06-30): proper nouns/products absent from a
/// 1990s dictionary, two-word brand names glued into one token (openai,
/// humanlayer, victoriametrics, victorialogs — the standalone words already
/// phonemize correctly), and British spellings CMUdict only has as American
/// (optimise/behaviour/authorised alias their -ize/-or/-ized entries). Checked
/// against the bundled dict first (data/cmudict_data.json) so this doesn't
/// paper over words that were already correct: "meta", "tendency", and
/// "apache" are already present and unchanged; not in this table.
const PRONUNCIATION_OVERRIDES: &[(&str, &str)] = &[
    ("google", "G UW1 G AH0 L"),
    ("openai", "OW1 P AH0 N AY1"),
    ("agentic", "AH0 JH EH1 N T IH0 K"),
    ("misalignment", "M IH0 S AH0 L AY1 N M AH0 N T"),
    ("optimise", "AA1 P T AH0 M AY2 Z"),
    ("behaviour", "B IH0 HH EY1 V Y ER0"),
    ("humanlayer", "HH Y UW1 M AH0 N L EY1 ER0"),
    ("observability", "AH0 B Z ER1 V AH0 B IH1 L AH0 T IY0"),
    ("ollama", "OW0 L AA1 M AH0"),
    ("qwen", "K W EH1 N"),
    ("api", "EY1 P IY1 AY1"),
    ("semgrep", "S EH1 M G R EH1 P"),
    ("incomprehension", "IH2 N K AA2 M P R IY0 HH EH1 N SH AH0 N"),
    ("codebase", "K OW1 D B EY1 S"),
    ("codebases", "K OW1 D B EY1 S AH0 Z"),
    ("affordance", "AH0 F AO1 R D AH0 N S"),
    ("affordances", "AH0 F AO1 R D AH0 N S AH0 Z"),
    ("anthropic", "AE0 N TH R AA1 P IH0 K"),
    ("pushback", "P UH1 SH B AE0 K"),
    ("authorised", "AO1 TH ER0 AY2 Z D"),
    ("pretraining", "P R IY0 T R EY1 N IH0 NG"),
    ("hadoop", "HH AH0 D UW1 P"),
    ("filesystem", "F AY1 L S IH0 S T AH0 M"),
    ("victoriametrics", "V IH0 K T AO1 R IY0 AH0 M EH1 T R IH0 K S"),
    ("victorialogs", "V IH0 K T AO1 R IY0 AH0 L AO1 G Z"),
    ("loki", "L OW1 K IY0"),
    // Reported 2026-07-01 (CUDA/GPU-tooling article). Checked first: "embedded"
    // is already correct in the bundled dict (EH0 M B EH1 D IH0 D) and NOT
    // touched. Rest were genuinely missing/OOV, same causes as above: -er/-tion/
    // -able suffix derivations built from real entries (classify, execute,
    // action), compounds glued into one token (standalone, walkthrough,
    // runtime, fatbin — a real CUDA term for a multi-arch bundled binary), and
    // acronyms read letter-by-letter (gpu, matching the existing "api" entry).
    ("classifier", "K L AE1 S AH0 F AY2 ER0"),
    ("classifiers", "K L AE1 S AH0 F AY2 ER0 Z"),
    ("compaction", "K AH0 M P AE1 K SH AH0 N"),
    ("compactions", "K AH0 M P AE1 K SH AH0 N Z"),
    ("standalone", "S T AE1 N D AH0 L OW2 N"),
    ("gpu", "JH IY1 P IY1 Y UW1"),
    ("gpus", "JH IY1 P IY1 Y UW1 Z"),
    ("walkthrough", "W AO1 K TH R UW2"),
    ("walkthroughs", "W AO1 K TH R UW2 S"),
    ("fatbin", "F AE1 T B IH0 N"),
    ("fatbins", "F AE1 T B IH0 N Z"),
    ("executable", "EH1 K S AH0 K Y UW2 T AH0 B AH0 L"),
    ("executables", "EH1 K S AH0 K Y UW2 T AH0 B AH0 L Z"),
    ("cuda", "K UW1 D AH0"),
    ("runtime", "R AH1 N T AY2 M"),
    ("runtimes", "R AH1 N T AY2 M Z"),
    // Reported 2026-07-01. "lazily" was already correct in the dict (not added).
    // "upload(s)" can't reach the compound splitter (its "up" prefix is 2 chars,
    // below SEGMENT_MIN_PIECE); "pushbuffer" and "mispronunciation(s)" are left
    // to the splitter (push+buffer, mis+pronunciation) rather than pinned here.
    ("upload", "AH1 P L OW2 D"),
    ("uploads", "AH1 P L OW2 D Z"),
    ("uploaded", "AH1 P L OW2 D IH0 D"),
    ("uploading", "AH1 P L OW2 D IH0 NG"),
    // Reported 2026-07-01. Missing words, built from existing morphemes
    // (extensive/extend for extensible; purpose + re- for repurpose*).
    ("extensible", "IH0 K S T EH1 N S AH0 B AH0 L"),
    ("repurpose", "R IY0 P ER1 P AH0 S"),
    ("repurposed", "R IY0 P ER1 P AH0 S T"),
    ("repurposes", "R IY0 P ER1 P AH0 S IH0 Z"),
    ("repurposing", "R IY0 P ER1 P AH0 S IH0 NG"),
    // Heteronyms whose CMUdict default is the wrong sense for a reader: it stores
    // read -> "R EH1 D" (past tense) and reading -> "R EH1 D IH0 NG" (the town),
    // but the present/infinitive/noun sense (reed) dominates article prose. Flip
    // them (insert overwrites the dict entry). TRADEOFF: past-tense "I read it
    // yesterday" now says "reed" — correct disambiguation needs POS/tense from a
    // grammar tagger, which the TTS path doesn't run.
    ("read", "R IY1 D"),
    ("reading", "R IY1 D IH0 NG"),
    // Reported 2026-07-01. json is pronounced "Jason" (already in the dict under
    // that spelling). "scapes" was missing (only "scape" existed), so the
    // compound splitter couldn't do hellscapes -> hell+scapes; adding it unlocks
    // the whole -scapes family (soundscapes, cityscapes...), and hellscape(s) are
    // pinned too for certainty. Built from scape (S K EY1 P) / landscape's -scape.
    ("json", "JH EY1 S AH0 N"),
    ("scapes", "S K EY1 P S"),
    ("hellscape", "HH EH1 L S K EY2 P"),
    ("hellscapes", "HH EH1 L S K EY2 P S"),
    // Reported 2026-07-01. Missing words built from morphemes (iterative stem;
    // un- + ordered).
    ("iteration", "IH2 T ER0 EY1 SH AH0 N"),
    ("iterations", "IH2 T ER0 EY1 SH AH0 N Z"),
    ("iterate", "IH1 T ER0 EY2 T"),
    ("unordered", "AH0 N AO1 R D ER0 D"),
    // "AI" collides with the dict word "ai" (AY1 = "eye"), so it was read as "I".
    // Say the letters A-I. Composes with the possessive handler: "AI's" -> A-I-z.
    ("ai", "EY1 AY1"),
    // Heteronym: dict stores the verb "lives" (L IH1 V Z, "he lives"); the noun
    // plural of life (L AY1 V Z, "our lives") dominates article prose. Flip it.
    // TRADEOFF: verb "she lives here" now uses the long-i, like read/reading.
    ("lives", "L AY1 V Z"),
    // Reported 2026-07-01. Missing words built from morphemes (val; verify + er;
    // plug + in). "MCPs" is handled generically by the acronym-plural rule above.
    ("eval", "IY0 V AE1 L"),
    ("verifier", "V EH1 R AH0 F AY2 ER0"),
    ("verifiers", "V EH1 R AH0 F AY2 ER0 Z"),
    ("plugin", "P L AH1 G IH0 N"),
    ("plugins", "P L AH1 G IH0 N Z"),
    // Reported 2026-07-02. All missing (CMUdict predates blog/bot too). Proper
    // nouns and neologisms/compounds built from morphemes in the dict; macOS and
    // ChatGPT mix a word with a letter-acronym (O-S, G-P-T).
    ("macos", "M AE1 K OW1 EH1 S"),
    ("reproducible", "R IY2 P R AH0 D UW1 S AH0 B AH0 L"),
    ("bot", "B AA1 T"),
    ("bots", "B AA1 T S"),
    ("chatbot", "CH AE1 T B AA2 T"),
    ("chatbots", "CH AE1 T B AA2 T S"),
    ("nvidia", "EH0 N V IH1 D IY0 AH0"),
    ("blog", "B L AA1 G"),
    ("blogs", "B L AA1 G Z"),
    ("reframe", "R IY0 F R EY1 M"),
    ("kubernetes", "K UW2 B ER0 N EH1 T IY0 Z"),
    ("reanimate", "R IY0 AE1 N AH0 M EY2 T"),
    ("reanimated", "R IY0 AE1 N AH0 M EY2 T AH0 D"),
    ("chatgpt", "CH AE1 T JH IY1 P IY1 T IY1"),
    ("trillionaire", "T R IH2 L Y AH0 N EH1 R"),
    ("unquenchable", "AH0 N K W EH1 N CH AH0 B AH0 L"),
    ("vexation", "V EH0 K S EY1 SH AH0 N"),
    // Reported 2026-07-04. OOV proper nouns/tech terms, plus two words CMUdict
    // stresses oddly (microsoft on -soft, mythos absent entirely).
    ("gvisor", "JH IY1 V AY2 Z ER0"),
    ("microvm", "M AY1 K R OW0 V IY2 EH1 M"),
    ("microvms", "M AY1 K R OW0 V IY2 EH1 M Z"),
    ("mythos", "M IH1 TH AA2 S"),
    ("microsoft", "M AY1 K R OW0 S AO2 F T"),
    ("weaponization", "W EH2 P AH0 N AH0 Z EY1 SH AH0 N"),
    ("weaponize", "W EH1 P AH0 N AY2 Z"),
    ("weaponized", "W EH1 P AH0 N AY2 Z D"),
    ("colocate", "K OW1 L OW0 K EY2 T"),
    ("colocated", "K OW1 L OW0 K EY2 T IH0 D"),
    ("colocating", "K OW1 L OW0 K EY2 T IH0 NG"),
    ("colocation", "K OW2 L OW0 K EY1 SH AH0 N"),
    ("tabular", "T AE1 B Y AH0 L ER0"),
    ("currant", "K AH1 R AH0 N T"),
    ("currants", "K AH1 R AH0 N T S"),
    ("redcurrant", "R EH1 D K AH2 R AH0 N T"),
    ("redcurrants", "R EH1 D K AH2 R AH0 N T S"),
    ("blackcurrant", "B L AE1 K K AH2 R AH0 N T"),
    ("blackcurrants", "B L AE1 K K AH2 R AH0 N T S"),
    ("recursive", "R IH0 K ER1 S IH0 V"),
    ("recursively", "R IH0 K ER1 S IH0 V L IY0"),
    ("recursion", "R IH0 K ER1 ZH AH0 N"),
    ("rationalist", "R AE1 SH AH0 N AH0 L IH2 S T"),
    ("rationalists", "R AE1 SH AH0 N AH0 L IH2 S T S"),
    ("totalize", "T OW1 T AH0 L AY2 Z"),
    ("totalizing", "T OW1 T AH0 L AY2 Z IH0 NG"),
    ("commoditize", "K AH0 M AA1 D AH0 T AY2 Z"),
    ("commoditized", "K AH0 M AA1 D AH0 T AY2 Z D"),
    ("commoditizing", "K AH0 M AA1 D AH0 T AY2 Z IH0 NG"),
];

/// Expand each (possibly multi-codepoint, like "ɑː" or "iː") token into
/// single-codepoint tokens, since the model id_map is keyed by single chars.
fn push_chars(out: &mut Vec<String>, multi: &str) {
    for ch in multi.chars() {
        out.push(ch.to_string());
    }
}

/// Spell a single OOV word letter-by-letter via `letter_ipa`. Digits and other
/// chars are skipped. Returns single-codepoint IPA tokens.
fn spell_word(word: &str) -> Vec<String> {
    let mut toks = Vec::new();
    for ch in word.chars() {
        if let Some(parts) = letter_ipa(ch) {
            for p in parts {
                push_chars(&mut toks, p);
            }
        }
    }
    toks
}

/// True if a word produced no real phoneme tokens (ignoring spaces/punctuation),
/// i.e. piper-plus-g2p treated it as OOV and skipped it.
fn is_oov(phonemes: &[String]) -> bool {
    phonemes
        .iter()
        .all(|t| t == " " || t.chars().all(|c| !c.is_alphabetic() && c != 'ː'))
}

const ONES: [&str; 20] = [
    "zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine",
    "ten", "eleven", "twelve", "thirteen", "fourteen", "fifteen", "sixteen",
    "seventeen", "eighteen", "nineteen",
];
const TENS: [&str; 10] = [
    "", "", "twenty", "thirty", "forty", "fifty", "sixty", "seventy", "eighty", "ninety",
];

/// Spell a non-negative integer in English words ("2026" -> "two thousand twenty six").
fn say_cardinal(n: u64) -> String {
    if n < 20 {
        return ONES[n as usize].to_string();
    }
    if n < 100 {
        let t = TENS[(n / 10) as usize];
        let o = n % 10;
        return if o == 0 { t.to_string() } else { format!("{t} {}", ONES[o as usize]) };
    }
    if n < 1000 {
        let h = format!("{} hundred", ONES[(n / 100) as usize]);
        let r = n % 100;
        return if r == 0 { h } else { format!("{h} {}", say_cardinal(r)) };
    }
    for (div, name) in [
        (1_000_000_000_000u64, "trillion"),
        (1_000_000_000, "billion"),
        (1_000_000, "million"),
        (1_000, "thousand"),
    ] {
        if n >= div {
            let head = format!("{} {name}", say_cardinal(n / div));
            let r = n % div;
            return if r == 0 { head } else { format!("{head} {}", say_cardinal(r)) };
        }
    }
    say_cardinal(n)
}

/// Spell an ordinal ("1" -> "first", "21" -> "twenty first", "30" -> "thirtieth").
fn say_ordinal(n: u64) -> String {
    let mut words: Vec<String> = say_cardinal(n).split(' ').map(str::to_string).collect();
    if let Some(last) = words.last_mut() {
        *last = match last.as_str() {
            "one" => "first".into(),
            "two" => "second".into(),
            "three" => "third".into(),
            "five" => "fifth".into(),
            "eight" => "eighth".into(),
            "nine" => "ninth".into(),
            "twelve" => "twelfth".into(),
            w if w.ends_with('y') => format!("{}ieth", &w[..w.len() - 1]),
            w => format!("{w}th"),
        };
    }
    words.join(" ")
}

/// Normalize numbers in TTS input to spoken words. piper-plus-g2p / CMUdict has
/// no entries for digits, so without this numbers are silently dropped. Handles
/// integers (with thousands commas), decimals, currency ($), percent, ordinals.
fn normalize_numbers(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let currency = c == '$' && chars.get(i + 1).is_some_and(|d| d.is_ascii_digit());
        if !currency && !c.is_ascii_digit() {
            out.push(c);
            i += 1;
            continue;
        }
        if currency {
            i += 1;
        }
        let mut int_str = String::new();
        while i < chars.len() {
            let d = chars[i];
            if d.is_ascii_digit() {
                int_str.push(d);
                i += 1;
            } else if d == ',' && chars.get(i + 1).is_some_and(|x| x.is_ascii_digit()) {
                i += 1; // thousands separator
            } else {
                break;
            }
        }
        let mut frac = String::new();
        if chars.get(i) == Some(&'.') && chars.get(i + 1).is_some_and(|x| x.is_ascii_digit()) {
            i += 1;
            while i < chars.len() && chars[i].is_ascii_digit() {
                frac.push(chars[i]);
                i += 1;
            }
        }
        let mut percent = false;
        let mut ordinal = false;
        if chars.get(i) == Some(&'%') {
            percent = true;
            i += 1;
        } else if frac.is_empty() {
            let suf: String = chars[i..(i + 2).min(chars.len())]
                .iter().collect::<String>().to_ascii_lowercase();
            if matches!(suf.as_str(), "st" | "nd" | "rd" | "th") {
                ordinal = true;
                i += 2;
            }
        }
        let int_val: u64 = int_str.parse().unwrap_or(0);
        if out.ends_with(|c: char| c.is_alphanumeric()) {
            out.push(' ');
        }
        if currency {
            out.push_str(&say_cardinal(int_val));
            out.push_str(if int_val == 1 { " dollar" } else { " dollars" });
            if !frac.is_empty() {
                let cents: u64 = format!("{frac:0<2}")[..2].parse().unwrap_or(0);
                out.push_str(&format!(" and {} cents", say_cardinal(cents)));
            }
        } else if ordinal {
            out.push_str(&say_ordinal(int_val));
        } else if !frac.is_empty() {
            out.push_str(&say_cardinal(int_val));
            out.push_str(" point");
            for d in frac.chars() {
                out.push(' ');
                out.push_str(ONES[d as usize - '0' as usize]);
            }
        } else {
            out.push_str(&say_cardinal(int_val));
        }
        if percent {
            out.push_str(" percent");
        }
        // A letter glued to the number ("10x", "1990s") would fuse with the
        // spelled-out form ("tenx") and turn OOV; separate it.
        if chars.get(i).is_some_and(|c| c.is_ascii_alphabetic()) {
            out.push(' ');
        }
    }
    out
}

/// Normalize typographic characters the phonemizer and CMUdict don't understand
/// into ASCII equivalents. Curly apostrophes become straight so contractions
/// like "don't" stay one dictionary word instead of splitting into "don" + "t";
/// curly quotes become straight; em/en dashes and ellipsis become pause marks.
/// Runs before tokenization.
fn normalize_text(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            '\u{2019}' | '\u{2018}' | '\u{02BC}' => '\'',
            '\u{201C}' | '\u{201D}' => '"',
            '\u{2014}' | '\u{2013}' => ',',
            '\u{2026}' => '.',
            _ => c,
        })
        .collect()
}

/// Split text into word / standalone-punctuation runs, preserving punctuation so
/// it still phonemizes (commas, periods etc. are in the id_map and add prosody).
fn split_tokens(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for ch in text.chars() {
        if ch.is_alphanumeric() || ch == '\'' {
            cur.push(ch);
        } else {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            if !ch.is_whitespace() {
                out.push(ch.to_string());
            }
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// All-caps initialisms that collide with ordinary dictionary words: "US"
/// would otherwise read as the pronoun "us", "IT" as "it". Case is the only
/// signal, so these are spelled letter-by-letter before the dict lookup.
const SPELLED_ACRONYMS: &[&str] = &[
    "us", "uk", "eu", "un", "tv", "id", "os", "ip", "ui", "ux", "pr", "hr",
];

/// Prefixes tried against an OOV word ("unpatched" -> un + patched). The
/// compound splitter requires both halves >= 3 chars, which two-letter
/// prefixes fail. Each is pronounced via its own dictionary entry, so only
/// prefixes whose standalone word sounds like the prefix qualify ("re" is in
/// the dict as the musical "ray" — excluded). Longest first so "under" wins
/// over "un".
const OOV_PREFIXES: &[&str] = &[
    "under", "over", "multi", "anti", "non", "pre", "mis", "sub", "un", "co",
];

/// Respell British forms the dictionary only has in US spelling (-ise/-our):
/// candidates to retry before falling back to splitting/spelling. Only OOV
/// words reach this, so common words ("hour", "course") are never touched.
fn british_respellings(word: &str) -> Vec<String> {
    let w = word.to_lowercase();
    let mut out = Vec::new();
    for (suf, rep) in [
        ("isation", "ization"), ("isations", "izations"),
        ("ising", "izing"), ("ised", "ized"), ("ises", "izes"),
        ("iser", "izer"), ("isers", "izers"), ("ise", "ize"),
        ("ysing", "yzing"), ("ysed", "yzed"), ("yses", "yzes"), ("yse", "yze"),
    ] {
        if let Some(stem) = w.strip_suffix(suf) {
            out.push(format!("{stem}{rep}"));
        }
    }
    if let Some(i) = w.find("our") {
        out.push(format!("{}or{}", &w[..i], &w[i + 3..]));
    }
    out
}

/// Silence inserted after punctuation / line breaks, in ms. This voice's own
/// punctuation pause is negligible, so segments are synthesized separately and
/// joined with real silence. Paragraph > sentence > clause. Tune to taste.
const PARAGRAPH_PAUSE_MS: usize = 800; // newline / blank line
const SENTENCE_PAUSE_MS: usize = 500; // . ! ?
const CLAUSE_PAUSE_MS: usize = 250; // , ; :

/// Phonemize whole text to a single IPA token stream with " " word separators.
/// OOV words are spelled letter-by-letter so they still produce audio.
fn phonemize(
    ph: &EnglishPhonemizer,
    gb: Option<&HashMap<String, String>>,
    word_set: &std::collections::HashSet<String>,
    text: &str,
) -> Result<Vec<String>, String> {
    // Phonemize one already-isolated word, trimming the phonemizer's edge
    // spaces. Returns whatever it produced (possibly OOV/empty — caller checks).
    // GB locale: the GB dictionary wins; anything it lacks takes the US path
    // and gets the US->RP transform, so OOV handling (possessives, acronyms,
    // compounds) needs no locale-specific code.
    let phonemize_word = |w: &str| -> Result<Vec<String>, String> {
        if let Some(gb) = gb {
            if let Some(ipa) = gb.get(&w.to_lowercase()) {
                return Ok(ipa.chars().map(|c| c.to_string()).collect());
            }
        }
        let (mut pt, _prosody) = ph
            .phonemize_with_prosody(w)
            .map_err(|e| format!("phonemize: {e}"))?;
        while pt.first().map(|s| s == " ").unwrap_or(false) {
            pt.remove(0);
        }
        while pt.last().map(|s| s == " ").unwrap_or(false) {
            pt.pop();
        }
        if gb.is_some() && !is_oov(&pt) {
            pt = crate::gb_english::us_to_rp(pt);
        }
        Ok(pt)
    };

    // Letter-by-letter spelling, RP-adjusted for GB models (letter names like
    // R carry rhotic vowels otherwise).
    let spell = |w: &str| -> Vec<String> {
        let t = spell_word(w);
        if gb.is_some() {
            crate::gb_english::us_to_rp(t)
        } else {
            t
        }
    };

    let pieces = split_tokens(text);
    let mut tokens: Vec<String> = Vec::new();
    let mut first = true;

    for piece in &pieces {
        let is_word = piece.chars().next().map(|c| c.is_alphanumeric()).unwrap_or(false);

        let mut p_tokens: Vec<String> = if is_word {
            let all_caps =
                piece.chars().count() >= 2 && piece.chars().all(|c| c.is_ascii_uppercase());
            let mut pt = if all_caps
                && SPELLED_ACRONYMS.contains(&piece.to_ascii_lowercase().as_str())
            {
                spell(piece)
            } else {
                phonemize_word(piece)?
            };
            if is_oov(&pt) {
                let mut replaced = false;
                // Possessive of an OOV word ("Claude's", "Anthropic's"): the base
                // is often pronounceable (in the dict or an override) even when
                // the whole "word's" token isn't. Pronounce the base and append
                // the possessive /z/ instead of spelling the whole thing out.
                // (Common possessives like "cat's"/"it's" are in the dict already,
                // so they never reach here; this is for proper nouns.)
                if let Some(base) = piece.strip_suffix("'s") {
                    if !base.is_empty() {
                        let mut b = phonemize_word(base)?;
                        if !is_oov(&b) {
                            b.push("z".to_string());
                            pt = b;
                            replaced = true;
                        }
                    }
                }
                // Acronym plural: an all-caps acronym + a lowercase "s" ("MCPs",
                // "APIs", "LLMs"). Spelling it letter-by-letter would read the "s"
                // as "ess"; instead say/spell the acronym and append /z/ so it's
                // "em-see-peez". The base goes through phonemize first so an
                // acronym with an override (API, GPU) uses it.
                if !replaced {
                    let ch: Vec<char> = piece.chars().collect();
                    let n = ch.len();
                    if n >= 3 && ch[n - 1] == 's' && ch[..n - 1].iter().all(|c| c.is_ascii_uppercase()) {
                        let base: String = ch[..n - 1].iter().collect();
                        let mut b = phonemize_word(&base)?;
                        if is_oov(&b) {
                            b = spell(&base);
                        }
                        if !b.is_empty() {
                            b.push("z".to_string());
                            pt = b;
                            replaced = true;
                        }
                    }
                }
                // British spelling the dict only has in US form ("sterilised",
                // "neighbour"): respell and retry before splitting/spelling.
                if !replaced {
                    for cand in british_respellings(piece) {
                        let b = phonemize_word(&cand)?;
                        if !is_oov(&b) {
                            pt = b;
                            replaced = true;
                            break;
                        }
                    }
                }
                // Common prefix + dictionary word ("unpatched", "colocating").
                // Keep the base word's stress primary and demote the prefix's,
                // so it reads as one word stressed on the stem.
                if !replaced {
                    let lower = piece.to_lowercase();
                    for prefix in OOV_PREFIXES {
                        let Some(rest) = lower.strip_prefix(prefix) else { continue };
                        if rest.chars().count() < 3 {
                            continue;
                        }
                        let mut p = phonemize_word(prefix)?;
                        let r = phonemize_word(rest)?;
                        if is_oov(&p) || is_oov(&r) {
                            continue;
                        }
                        for t in p.iter_mut() {
                            if t == "\u{02C8}" {
                                *t = "\u{02CC}".to_string();
                            }
                        }
                        p.extend(r);
                        pt = p;
                        replaced = true;
                        break;
                    }
                }
                // Else split a glued compound into two dictionary words and
                // pronounce each half (pushbuffer -> push + buffer). Sub-words
                // come from `word_set` so they phonemize; concatenate their tokens
                // with no separator so it reads as one word, not two with a pause.
                if !replaced {
                    if let Some(parts) = segment_compound(word_set, piece) {
                        let mut combined: Vec<String> = Vec::new();
                        let mut ok = true;
                        for (pi, part) in parts.iter().enumerate() {
                            let mut sub = phonemize_word(part)?;
                            if is_oov(&sub) { ok = false; break; }
                            // Each word carries a primary stress marker (ˈ). Joined
                            // as-is, the second word's ˈ starts a new prosodic unit
                            // and the model inserts an audible pause (salesforce ->
                            // "sales ... force"). A compound has ONE primary stress,
                            // so demote later parts' ˈ (U+02C8) to secondary ˌ
                            // (U+02CC): it reads as one word, no mid-word pause.
                            if pi > 0 {
                                for t in sub.iter_mut() {
                                    if t == "\u{02C8}" { *t = "\u{02CC}".to_string(); }
                                }
                            }
                            combined.append(&mut sub);
                        }
                        if ok && !combined.is_empty() {
                            pt = combined;
                            replaced = true;
                        }
                    }
                }
                if !replaced {
                    let spelled = spell(piece);
                    if !spelled.is_empty() {
                        pt = spelled;
                    }
                }
            }
            pt
        } else {
            // Punctuation pieces: the phonemizer skips bare punctuation, dropping
            // the pause cue. Emit the marks it recognizes, repeated, so the model
            // allocates more silent frames for a longer, clearer pause.
            piece
                .chars()
                .filter(|c| matches!(c, ',' | '.' | ';' | ':' | '!' | '?'))
                .map(|c| c.to_string())
                .collect()
        };

        if p_tokens.is_empty() {
            continue;
        }

        if !first {
            tokens.push(" ".to_string());
        }
        first = false;
        tokens.append(&mut p_tokens);
    }

    Ok(tokens)
}

/// Split text into segments at sentence/clause punctuation, each paired with the
/// silence (ms) to insert after it. Punctuation stays attached to its segment so
/// the model still gets final intonation; the silence supplies the audible pause.
/// Per-segment timing for the reading-view highlight: the raw segment text, its
/// SPEECH duration (excluding the trailing pause) and the pause inserted after.
pub struct SegmentSpan {
    pub text: String,
    pub speech_ms: u64,
    pub pause_ms: u64,
    /// Per-word speech duration (ms), aligned with `text.split_whitespace()`.
    /// Exact (from the model's `w_ceil`) when available, else a char-length
    /// estimate. Lets the reading view highlight per word without guessing.
    pub word_ms: Vec<u64>,
}

pub fn split_for_pauses(text: &str) -> Vec<(String, usize)> {
    fn pause_of(c: char) -> Option<usize> {
        match c {
            '\n' => Some(PARAGRAPH_PAUSE_MS),
            '.' | '!' | '?' | '\u{2026}' => Some(SENTENCE_PAUSE_MS),
            ',' | ';' | ':' | '\u{2014}' | '\u{2013}' => Some(CLAUSE_PAUSE_MS),
            _ => None,
        }
    }

    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut i = 0;

    while i < n {
        let ch = chars[i];
        // Only pause at punctuation that ends a token — i.e. a newline, or a mark
        // followed by whitespace / end / another mark. A mark mid-token (no space
        // around it: "death,4", "1,000", "3.14", "e.g.") is NOT a clause/sentence
        // break: splitting there would insert a wrong pause inside the token AND
        // make the backend word count diverge from the reader's \S+ tokens.
        let is_break = match pause_of(ch) {
            None => false,
            Some(_) => {
                ch == '\n'
                    || chars
                        .get(i + 1)
                        .map_or(true, |&c| c.is_whitespace() || pause_of(c).is_some())
            }
        };
        if !is_break {
            cur.push(ch);
            i += 1;
            continue;
        }
        let mut pause = pause_of(ch).unwrap();
        // Keep a spoken punctuation mark in the segment (for intonation);
        // newlines are whitespace, so don't. Then consume the rest of the break
        // run, taking the longest pause, so "...", "?!", "\n\n" and trailing
        // spaces collapse into a single gap instead of stacking.
        if ch != '\n' {
            cur.push(ch);
        }
        i += 1;
        while i < n {
            let c = chars[i];
            if let Some(p) = pause_of(c) {
                pause = pause.max(p);
                i += 1;
            } else if c.is_whitespace() {
                i += 1;
            } else {
                break;
            }
        }
        out.push((std::mem::take(&mut cur), pause));
    }
    if !cur.trim().is_empty() {
        out.push((cur, 0));
    }
    out
}

/// Number of phoneme ids the encoder emits for one token. Mirrors PiperEncoder:
/// PUA-map the token, then sum id_map entry lengths over its chars (unknown
/// chars contribute 0, as Skip mode drops them). Used to walk the id layout.
fn token_id_count(token: &str, id_map: &PhonemeIdMap) -> usize {
    let mapped = match piper_plus_g2p::token_map::token_to_pua(token) {
        Some(c) => c.to_string(),
        None => token.to_string(),
    };
    mapped
        .chars()
        .map(|ch| id_map.get(&ch.to_string()).map_or(0, |v| v.len()))
        .sum()
}

/// Attribute the model's per-id frame counts (`wceil`) to each whitespace word.
/// Re-phonemizes word by word and checks the concatenation matches the segment's
/// actual token stream, then walks the encoder id layout
/// (`BOS, PAD, [token ids, PAD]*, EOS`) summing frames per word. Returns `None`
/// on any inconsistency so the caller falls back to a char estimate — a mismatch
/// never yields wrong timing.
fn map_words_to_frames(
    words: &[&str],
    tokens: &[String],
    wceil: &[f32],
    ph: &EnglishPhonemizer,
    gb: Option<&HashMap<String, String>>,
    word_set: &HashSet<String>,
    id_map: &PhonemeIdMap,
) -> Option<Vec<f64>> {
    if words.is_empty() {
        return None;
    }
    // Rebuild the token stream grouped by word; confirm it equals the stream
    // actually synthesized so the mapping lines up with the audio.
    let mut full: Vec<String> = Vec::new();
    let mut tok_word: Vec<usize> = Vec::new();
    for (wi, w) in words.iter().enumerate() {
        let norm = normalize_numbers(&normalize_text(w));
        let wt = phonemize(ph, gb, word_set, &norm).ok()?;
        if wt.is_empty() {
            continue;
        }
        if !full.is_empty() {
            full.push(" ".to_string());
            tok_word.push(wi);
        }
        for t in wt {
            full.push(t);
            tok_word.push(wi);
        }
    }
    if full != tokens {
        return None;
    }
    // Walk the id layout, summing wceil per word. BOS + leading PAD -> word 0;
    // each token contributes its content ids + a trailing PAD; EOS -> last word.
    let mut frames = vec![0f64; words.len()];
    let mut idx = 0usize;
    for _ in 0..2 {
        frames[0] += *wceil.get(idx)? as f64;
        idx += 1;
    }
    for (k, token) in full.iter().enumerate() {
        let w = tok_word[k];
        for _ in 0..(token_id_count(token, id_map) + 1) {
            frames[w] += *wceil.get(idx)? as f64;
            idx += 1;
        }
    }
    frames[words.len() - 1] += *wceil.get(idx)? as f64;
    idx += 1;
    if idx != wceil.len() {
        return None;
    }
    Some(frames)
}

/// Per-word speech durations (ms) for a segment, aligned with
/// `segment.split_whitespace()`. Exact via `wceil` when it maps cleanly, else a
/// char-length split of the segment's measured speech time.
fn compute_word_ms(
    words: &[&str],
    tokens: &[String],
    wceil: Option<&[f32]>,
    speech_samples: usize,
    sr: usize,
    ph: &EnglishPhonemizer,
    gb: Option<&HashMap<String, String>>,
    word_set: &HashSet<String>,
    id_map: &PhonemeIdMap,
) -> Vec<u64> {
    if words.is_empty() {
        return Vec::new();
    }
    let total_ms = (speech_samples as u64 * 1000) / sr.max(1) as u64;
    if let Some(wc) = wceil {
        if let Some(frames) = map_words_to_frames(words, tokens, wc, ph, gb, word_set, id_map) {
            let total: f64 = frames.iter().sum();
            if total > 0.0 {
                return frames
                    .iter()
                    .map(|&f| ((f / total) * total_ms as f64).round() as u64)
                    .collect();
            }
        }
    }
    // Fallback: split by character length (what the frontend used to do).
    let lens: Vec<usize> = words.iter().map(|w| w.chars().count().max(1)).collect();
    let total_len: usize = lens.iter().sum::<usize>().max(1);
    lens.iter().map(|&l| (total_ms * l as u64) / total_len as u64).collect()
}

static ORT_INIT: OnceLock<Result<(), String>> = OnceLock::new();

/// Initialize ORT once. Mirrors `grammar_neural::ensure_ort_init` so the
/// load-dynamic backend resolves `libonnxruntime.so` the same way across the app.
fn ensure_ort_init() -> Result<(), String> {
    ORT_INIT
        .get_or_init(|| {
            if ort::init().commit() {
                Ok(())
            } else {
                Err("ort init failed".to_string())
            }
        })
        .as_ref()
        .map(|_| ())
        .map_err(|e| e.clone())
}

/// Piper TTS engine: owns the ORT session, the phonemizer, the phoneme encoder
/// and the model's audio params. `synth_chunk` is the per-chunk hot path.
pub struct PiperEngine {
    session: Session,
    phonemizer: EnglishPhonemizer,
    encoder: PiperEncoder,
    /// Kept alongside the encoder to replicate its id layout when attributing
    /// the model's per-id durations (`w_ceil`) to words.
    id_map: PhonemeIdMap,
    /// Lowercased dictionary keys (CMUdict + overrides, plus GB entries when
    /// applicable), for splitting an OOV compound into two known words before
    /// falling back to letter-spelling.
    word_set: HashSet<String>,
    /// British dictionary (word -> final IPA), present only for GB-locale
    /// models; words it lacks take the US phonemizer + US->RP transform.
    gb_dict: Option<HashMap<String, String>>,
    sample_rate: i32,
    num_speakers: i64,
    noise_scale: f32,
    length_scale: f32,
    noise_w: f32,
    /// Identity of the loaded model (file name + size) folded into cache keys so
    /// a different voice/model never collides with another's stored audio.
    model_fp: String,
}

impl PiperEngine {
    /// Build the engine from the model `.onnx` and its `.onnx.json` sidecar.
    pub fn load(model_path: &str, config_path: &str, num_threads: i32) -> Result<Self, String> {
        let cfg_text = std::fs::read_to_string(config_path)
            .map_err(|e| format!("read piper config {config_path}: {e}"))?;
        let cfg: PiperConfig = serde_json::from_str(&cfg_text)
            .map_err(|e| format!("parse piper config: {e}"))?;

        let id_map: PhonemeIdMap = cfg.phoneme_id_map;
        // Skip (not Strict) so a punctuation mark or stray token absent from a
        // given model's id_map is dropped with a warning instead of failing the
        // whole chunk.
        let encoder = PiperEncoder::new(id_map.clone(), UnknownTokenMode::Skip)
            .map_err(|e| format!("piper encoder: {e}"))?;

        let mut dict: HashMap<String, String> = serde_json::from_slice(CMUDICT_BYTES)
            .map_err(|e| format!("parse bundled cmudict: {e}"))?;
        // CMUdict predates these terms (proper nouns, compounds glued into one
        // token, British spellings), so without an entry the OOV fallback in
        // `phonemize` spells them out letter-by-letter. Override/add entries in
        // the same ARPAbet format as the bundled dict so they route through the
        // normal phonemizer instead.
        for (word, phonemes) in PRONUNCIATION_OVERRIDES {
            dict.insert(word.to_string(), phonemes.to_string());
        }
        // Snapshot the keys (dict is moved into the phonemizer next) for the
        // OOV compound splitter. CMUdict keys are already lowercase, as are the
        // overrides; a couple hundred KB of Strings, built once at load.
        let mut word_set: HashSet<String> = dict.keys().cloned().collect();
        let phonemizer = EnglishPhonemizer::new_with_hashmap(dict);

        // Locale from the model's own sidecar: GB models get the British
        // dictionary (US remains the fallback + transform for words it lacks).
        let gb_dict = if cfg.espeak.voice.starts_with("en-gb") {
            let mut gb: HashMap<String, String> = serde_json::from_slice(GB_DICT_BYTES)
                .map_err(|e| format!("parse bundled gb dict: {e}"))?;
            for (word, ipa) in GB_PRONUNCIATION_OVERRIDES {
                gb.insert(word.to_string(), ipa.to_string());
            }
            word_set.extend(gb.keys().cloned());
            log::info!("Piper locale: en-GB ({} GB dict entries)", gb.len());
            Some(gb)
        } else {
            None
        };

        ensure_ort_init()?;

        // Force CPU EP for parity with grammar_neural: the ORT dylib bundled
        // from sherpa-onnx may have other execution providers compiled in, and
        // CPU is the only provider validated for these models.
        let cpu_ep = vec![ort::ep::CPUExecutionProvider::default().build()];
        let session = Session::builder()
            .map_err(|e| format!("session builder: {e}"))?
            .with_execution_providers(&cpu_ep)
            .map_err(|e| format!("piper ep: {e}"))?
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .map_err(|e| format!("piper opt level: {e}"))?
            .with_intra_threads(num_threads.max(1) as usize)
            .map_err(|e| format!("piper threads: {e}"))?
            .commit_from_file(model_path)
            .map_err(|e| format!("piper model {model_path}: {e}"))?;

        log::info!(
            "Piper loaded: sample_rate={} num_speakers={} (ort, no espeak)",
            cfg.audio.sample_rate, cfg.num_speakers
        );

        let model_fp = cache_fingerprint(model_path, config_path);

        Ok(Self {
            session,
            phonemizer,
            encoder,
            id_map,
            word_set,
            gb_dict,
            sample_rate: cfg.audio.sample_rate as i32,
            num_speakers: cfg.num_speakers,
            noise_scale: cfg.inference.noise_scale,
            length_scale: cfg.inference.length_scale,
            noise_w: cfg.inference.noise_w,
            model_fp,
        })
    }

    pub fn sample_rate(&self) -> i32 {
        self.sample_rate
    }

    pub fn num_speakers(&self) -> i32 {
        self.num_speakers as i32
    }

    /// Run the Piper ONNX forward pass for one phoneme-id sequence. Inference is
    /// wrapped in `catch_unwind` so a native crash surfaces as an `Err` instead
    /// of unwinding through the C boundary (mirrors the tts.rs/transcribe.rs
    /// crash-isolation style).
    /// Returns the waveform PCM and, when the model exposes it, the per-input-id
    /// frame counts (`w_ceil` = `/Ceil_output_0`). The patched model exposes it
    /// (see scripts/patch_piper_durations.py); the stock model doesn't, so it's
    /// `None` and the caller falls back to a char-length estimate.
    fn infer(
        &mut self,
        ids: &[i64],
        speaker: i64,
        length_scale: f32,
    ) -> Result<(Vec<f32>, Option<Vec<f32>>), String> {
        let n = ids.len();
        let noise_scale = self.noise_scale;
        let noise_w = self.noise_w;
        let multi_speaker = self.num_speakers > 1;
        let session = &mut self.session;
        let ids = ids.to_vec();

        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<(Vec<f32>, Option<Vec<f32>>), String> {
            let input = Tensor::from_array(([1_usize, n], ids))
                .map_err(|e| format!("input tensor: {e}"))?;
            let input_lengths = Tensor::from_array(([1_usize], vec![n as i64]))
                .map_err(|e| format!("input_lengths tensor: {e}"))?;
            let scales = Tensor::from_array(([3_usize], vec![noise_scale, length_scale, noise_w]))
                .map_err(|e| format!("scales tensor: {e}"))?;

            // Single-speaker Piper models have NO sid input; passing one is an
            // InvalidArgument error. Only multi-speaker graphs take it.
            let outputs = if multi_speaker {
                let sid = Tensor::from_array(([1_usize], vec![speaker]))
                    .map_err(|e| format!("sid tensor: {e}"))?;
                session
                    .run(ort::inputs! {
                        "input" => input,
                        "input_lengths" => input_lengths,
                        "scales" => scales,
                        "sid" => sid,
                    })
                    .map_err(|e| format!("piper run: {e}"))?
            } else {
                session
                    .run(ort::inputs! {
                        "input" => input,
                        "input_lengths" => input_lengths,
                        "scales" => scales,
                    })
                    .map_err(|e| format!("piper run: {e}"))?
            };

            // Output is f32 with raw shape (1, 1, 1, T); flatten to [T].
            let (_shape, data) = outputs["output"]
                .try_extract_tensor::<f32>()
                .map_err(|e| format!("extract output: {e}"))?;
            let pcm = data.to_vec();
            // Per-id durations (one value per input id), if the model exposes it.
            let wceil = outputs
                .get("/Ceil_output_0")
                .and_then(|v| v.try_extract_tensor::<f32>().ok())
                .map(|(_s, d)| d.to_vec());
            Ok((pcm, wceil))
        }))
        .map_err(|panic| panic_to_string("Piper inference", panic))?
    }

    /// Synthesize one already-batched chunk of text for speaker `sid` at the
    /// given `speed` (>1 faster). Returns f32 PCM at `sample_rate`. Empty output
    /// (e.g. a chunk that phonemizes to nothing) yields an empty Vec. This is the
    /// per-chunk primitive `tts.rs` drives so each batch streams to the player.
    pub fn synth_chunk(
        &mut self,
        text: &str,
        sid: i32,
        speed: f32,
    ) -> Result<(Vec<f32>, Vec<SegmentSpan>), String> {
        // length_scale is inverse to speed: larger stretches audio (slower).
        let length_scale = if speed > 0.0 {
            self.length_scale / speed
        } else {
            self.length_scale
        };
        // The model is multi-speaker, so a sid is mandatory; clamp to range.
        let speaker = if self.num_speakers > 0 {
            (sid as i64).clamp(0, self.num_speakers - 1)
        } else {
            0
        };

        let sr = self.sample_rate.max(1) as usize;
        // Quantize speed so float jitter (0.7499 vs 0.75) can't fragment the
        // cache; it keys the same render the user perceives as one speed.
        let speed_milli = (speed.max(0.0) * 1000.0).round() as u32;
        let mut out: Vec<f32> = Vec::new();
        let mut spans: Vec<SegmentSpan> = Vec::new();
        let (mut cache_hits, mut cache_misses) = (0u32, 0u32);
        // Split the RAW text (so segment word counts match the UI's tokens), then
        // normalize each segment for synthesis. Each segment is synthesized
        // separately and joined with real silence. Returning per-segment speech
        // timing (excluding the pause) lets the reading view highlight track
        // speech and treat the inserted pauses as gaps, not part of a word.
        for (segment, pause_ms) in split_for_pauses(text) {
            let speech_start = out.len();
            // Persistent cache: a segment in this model+voice+speed is synthesized
            // at most once, ever. Hit -> load pcm+timing and skip inference; miss
            // -> synthesize then store. The pause silence below is deterministic
            // and re-spliced either way, so it is not part of the cached audio.
            let ck = crate::tts_cache::key(&self.model_fp, speaker as i32, speed_milli, &segment);
            let (speech_pcm, word_ms) = match crate::tts_cache::get(&ck, sr as u32, &segment) {
                Some(seg) => {
                    cache_hits += 1;
                    (seg.pcm, seg.word_ms)
                }
                None => {
                    cache_misses += 1;
                    let normalized = normalize_numbers(&normalize_text(&segment));
                    let tokens =
                        phonemize(&self.phonemizer, self.gb_dict.as_ref(), &self.word_set, &normalized)?;
                    let ids = self
                        .encoder
                        .encode(&tokens)
                        .map_err(|e| format!("encode: {e}"))?;
                    let (pcm, wceil) = if !ids.is_empty() {
                        self.infer(&ids, speaker, length_scale)?
                    } else {
                        (Vec::new(), None)
                    };
                    // Per-word speech timing for the reading-view highlight: exact
                    // from the model's durations when available, else a char-length
                    // estimate.
                    let words: Vec<&str> = segment.split_whitespace().collect();
                    let word_ms = compute_word_ms(
                        &words, &tokens, wceil.as_deref(), pcm.len(), sr,
                        &self.phonemizer, self.gb_dict.as_ref(), &self.word_set, &self.id_map,
                    );
                    crate::tts_cache::put(&ck, sr as u32, &segment, &pcm, &word_ms);
                    (pcm, word_ms)
                }
            };
            out.extend_from_slice(&speech_pcm);
            let speech_samples = out.len() - speech_start;
            let pause_samples = if pause_ms > 0 && !out.is_empty() {
                pause_ms * sr / 1000
            } else {
                0
            };
            if pause_samples > 0 {
                out.extend(std::iter::repeat(0.0f32).take(pause_samples));
            }
            spans.push(SegmentSpan {
                text: segment,
                speech_ms: (speech_samples as u64 * 1000) / sr as u64,
                pause_ms: (pause_samples as u64 * 1000) / sr as u64,
                word_ms,
            });
        }
        if cache_hits + cache_misses > 0 {
            log::info!("TTS cache: {cache_hits} hit / {cache_misses} miss");
        }
        Ok((out, spans))
    }

    /// Synthesize `text` for speaker `sid` at the given `speed`, emitting whole
    /// f32 sample chunks (one per sentence batch) via `on_samples`.
    /// `should_continue` is polled before each chunk so playback can cancel
    /// generation. Sentence splitting/batching reuses the helpers from `tts.rs`.
    ///
    /// `tts::speak` drives `synth_chunk` directly so each chunk streams to the
    /// shared `AudioPlayer`; this is the standalone variant (used by the eval
    /// harness and tests) that owns the batching loop.
    #[allow(dead_code)]
    pub fn synthesize(
        &mut self,
        text: &str,
        sid: i32,
        speed: f32,
        mut on_samples: impl FnMut(Vec<f32>),
        should_continue: impl Fn() -> bool,
    ) -> Result<(), String> {
        let raw_sentences = crate::tts::split_sentences(text);
        let chunks = crate::tts::batch_sentences(raw_sentences, 15, 45);
        let total = chunks.len();

        for (i, chunk) in chunks.iter().enumerate() {
            if !should_continue() {
                break;
            }
            let t = std::time::Instant::now();
            let (samples, _spans) = self.synth_chunk(chunk, sid, speed)?;
            if samples.is_empty() {
                continue;
            }
            let audio_ms = (samples.len() as u64 * 1000) / self.sample_rate.max(1) as u64;
            let elapsed_ms = t.elapsed().as_millis();
            let rtf = if audio_ms > 0 { elapsed_ms as f32 / audio_ms as f32 } else { 0.0 };
            log::info!(
                "Piper [{}/{}]: {:.1}s audio in {}ms, RTF {:.2}x",
                i + 1, total, audio_ms as f32 / 1000.0, elapsed_ms, rtf
            );
            on_samples(samples);
        }

        Ok(())
    }
}

fn panic_to_string(context: &str, panic: Box<dyn std::any::Any + Send>) -> String {
    let detail = if let Some(s) = panic.downcast_ref::<String>() {
        s.clone()
    } else if let Some(s) = panic.downcast_ref::<&str>() {
        s.to_string()
    } else {
        "unknown panic".into()
    };
    format!("{context} crashed: {detail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cardinals() {
        assert_eq!(say_cardinal(0), "zero");
        assert_eq!(say_cardinal(7), "seven");
        assert_eq!(say_cardinal(21), "twenty one");
        assert_eq!(say_cardinal(105), "one hundred five");
        assert_eq!(say_cardinal(2026), "two thousand twenty six");
        assert_eq!(say_cardinal(1_000_000), "one million");
    }

    #[test]
    fn ordinals() {
        assert_eq!(say_ordinal(1), "first");
        assert_eq!(say_ordinal(3), "third");
        assert_eq!(say_ordinal(21), "twenty first");
        assert_eq!(say_ordinal(30), "thirtieth");
    }

    #[test]
    fn normalize() {
        assert_eq!(normalize_numbers("I have 3 apples"), "I have three apples");
        assert_eq!(normalize_numbers("in 2026."), "in two thousand twenty six.");
        assert_eq!(normalize_numbers("1,000 items"), "one thousand items");
        assert_eq!(normalize_numbers("$50"), "fifty dollars");
        assert_eq!(
            normalize_numbers("$1,234.50"),
            "one thousand two hundred thirty four dollars and fifty cents"
        );
        assert_eq!(normalize_numbers("pi is 3.14"), "pi is three point one four");
        assert_eq!(normalize_numbers("50% off"), "fifty percent off");
        assert_eq!(normalize_numbers("3rd place"), "third place");
        assert_eq!(normalize_numbers("no digits here"), "no digits here");
    }

    #[test]
    fn pauses_only_at_token_boundaries() {
        let segs = |t: &str| -> Vec<String> {
            split_for_pauses(t).into_iter().map(|(s, _)| s).collect()
        };
        // Boundary punctuation (followed by space/end) splits; mid-token does not.
        assert_eq!(segs("Hello, world."), vec!["Hello,", "world."]);
        assert_eq!(segs("death,4 end"), vec!["death,4 end"]);
        // Numbers stay intact (no pause inside, no extra segment word).
        assert_eq!(segs("pi 3.14 and 1,000 items."), vec!["pi 3.14 and 1,000 items."]);
        // Each kept segment's whitespace-word count matches the raw text's, so the
        // reading-view word indices stay aligned.
        let raw = "Hello, world. death,4 end";
        let seg_words: usize = segs(raw).iter().map(|s| s.split_whitespace().count()).sum();
        assert_eq!(seg_words, raw.split_whitespace().count());
    }

    #[test]
    fn compound_split() {
        let ws: HashSet<String> = ["push", "buffer", "code", "base", "stand", "alone", "pre", "training", "up", "load"]
            .iter().map(|s| s.to_string()).collect();
        // Clean binary compounds split into their two words.
        assert_eq!(segment_compound(&ws, "pushbuffer"), Some(vec!["push".into(), "buffer".into()]));
        assert_eq!(segment_compound(&ws, "codebase"), Some(vec!["code".into(), "base".into()]));
        // Shortest-first keeps the second word's leading letter (not "standa|lone").
        assert_eq!(segment_compound(&ws, "standalone"), Some(vec!["stand".into(), "alone".into()]));
        assert_eq!(segment_compound(&ws, "pretraining"), Some(vec!["pre".into(), "training".into()]));
        // A 2-char word ("up") is below the min piece length, so "upload" doesn't split.
        assert_eq!(segment_compound(&ws, "upload"), None);
        // No cover -> None (would fall through to letter spelling).
        assert_eq!(segment_compound(&ws, "buffed"), None);
        // Non-alphabetic content is never segmented.
        assert_eq!(segment_compound(&ws, "push2buffer"), None);
    }
}

