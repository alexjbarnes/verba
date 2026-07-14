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
    // Found by the round-trip harness: wikipron variants with wrong vowels
    // that the sweep's benign-notation folding can't distinguish (ɹiː- onset,
    // ʊ for ʌ).
    ("ridiculous", "ɹɪdˈɪkjʊləs"),
    ("ridiculously", "ɹɪdˈɪkjʊləslɪ"),
    ("rubberneck", "ɹˈʌbənˌɛk"),
    ("rubbernecking", "ɹˈʌbənˌɛkɪŋ"),
    // Reported 2026-07-05: the GB dict's "we're" is a bad wikipron entry (wˈɐ,
    // ~"wuh"). Fix to the NEAR vowel like here/near/fear (hˈiə/nˈiə/fˈiə).
    ("we're", "wˈiə"),
    // Reported 2026-07-05 (alba), second batch: gb_dict's "un" is the wikipron
    // entry for the abbreviation "U.N." (jˈuːɛn), which broke hyphen-split
    // prefixes like "un-iterated" ("U-N iterated"). No trade-off: uppercase
    // "UN" never reaches this lookup — the all-caps branch spells it via
    // SPELLED_ACRONYMS (which already lists "un") before the dict is checked.
    ("un", "ʌn"),
    // Reported 2026-07-11 (alba): wikipron entries with the stress transferred
    // onto the wrong syllable (mi-SO-jin-ist, not MISS-o-jin-ist) and, for
    // jacobian, an untransformed US GOAT vowel plus a stressed schwa.
    ("misogynist", "mɪsˈɒdʒɪnɪst"),
    ("misogynists", "mɪsˈɒdʒɪnɪsts"),
    ("misogyny", "mɪsˈɒdʒɪnɪ"),
    ("misogynistic", "mɪsˌɒdʒɪnˈɪstɪk"),
    ("jacobian", "dʒəkˈəʊbiən"),
    // Letter-A stand-in used by normalize_text's caps+digit acronym expansion
    // ("C2PA" -> "C 2 P ay") and the "Plan A" trigger rule: bare "a" must stay
    // the article's weak schwa, so those rewrites emit "ay" instead. wikipron
    // has "ay" as the archaic assent /aɪ/ ("ay lad"), which would read the
    // letter as "eye" — the letter name wins here (US cmudict agrees: EY1).
    ("ay", "ˈeɪ"),
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
    let path = std::path::Path::new(model_path);
    let len = std::fs::metadata(model_path).map(|m| m.len()).unwrap_or(0);
    let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("model");
    // The file is always "model_dur.onnx" (every voice's rel_path basename),
    // and many medium Piper models are byte-identical in size (alan, jenny,
    // northern_english_male all == alba == 63201318). So {name}:{len} alone
    // COLLIDES across those voices and they play each other's cached segments.
    // The parent dir (piper-alba, piper-alan, ...) is the per-voice identity;
    // fold it in. NOTE: changing this format orphans existing cache files —
    // intended, since those were cross-contaminated.
    let parent = path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|s| s.to_str())
        .unwrap_or("");
    format!("{parent}/{name}:{len}")
}

/// Bump when data/gb_dict.json (or the RP transform) changes pronunciations:
/// GB models' cached audio embeds the old phoneme stream, so their cache keys
/// must roll without invalidating US models' caches.
const GB_DICT_VERSION: u32 = 10;

/// espeak's bare "en" IS the British base voice (cori trained on it), so it
/// takes the GB dictionary path too. The cache path and the engine loader
/// MUST share this predicate: they disagreed until 2026-07-12, so cori
/// synthesized raw US phonemes (rhotic ɚ, oʊ, no GB dictionary) through a
/// GB-trained model while its cache keys claimed GB — the root cause of a
/// whole batch of cori mispronunciation reports (parameter, solar, quantized).
fn espeak_voice_is_gb(voice: &str) -> bool {
    voice.starts_with("en-gb") || voice == "en"
}

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
        .map(|c| espeak_voice_is_gb(&c.espeak.voice))
        .unwrap_or(false)
}

/// Cache fingerprint for a model+config pair: the file identity plus, for
/// GB-locale models, the bundled dictionary version. All cache producers and
/// consumers (engine load, cache coverage, cache-only playback) go through
/// this so they can never disagree.
/// Bump when pronunciation logic changes for ALL locales (heteronym rules,
/// tokenizer fixes) so cached audio regenerates with the new readings.
const PRON_VERSION: u32 = 9;

pub fn cache_fingerprint(model_path: &str, config_path: &str) -> String {
    let fp = model_fingerprint(model_path);
    if config_is_gb(config_path) {
        format!("{fp}+p{PRON_VERSION}+gb{GB_DICT_VERSION}")
    } else {
        format!("{fp}+p{PRON_VERSION}")
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

/// Delete the cached audio for every segment `text` needs, engine-free (same
/// segmentation as `cache_coverage`, so it targets exactly what `synth_chunk`
/// would have stored). Used to trim a chapter's audio once playback has moved
/// past it. Returns the number of entries actually removed — 0 on an
/// already-cold cache, which is the normal case, not an error. Cache keys
/// carry no chapter identity (voice, speed, segment text only), so this can
/// also remove a sentence that's cached for another item; it simply
/// regenerates there on next play.
pub fn forget_cached_segments(
    model_path: &str,
    config_path: &str,
    sid: i32,
    speed: f32,
    text: &str,
) -> u32 {
    let Some((_sample_rate, num_speakers)) = read_piper_meta(config_path) else { return 0 };
    let model_fp = cache_fingerprint(model_path, config_path);
    let speed_milli = (speed.max(0.0) * 1000.0).round() as u32;
    let speaker = clamp_speaker(sid, num_speakers);

    // Same prologue as cache_coverage: replicate synth_chunk's segmentation
    // exactly so these are the same keys it wrote.
    let clean = text.replace('\u{0}', " ");
    let chunks = crate::tts::batch_sentences(crate::tts::split_sentences(&clean), 15, 45);
    let mut forgotten = 0u32;
    for (segment, _pause_ms) in chunks.iter().flat_map(|c| split_for_pauses(c)) {
        let key = crate::tts_cache::key(&model_fp, speaker, speed_milli, &segment);
        if crate::tts_cache::remove(&key) {
            forgotten += 1;
        }
    }
    forgotten
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
    // Reported 2026-07-01. "lazily" sounded fine in that batch's voice but the
    // dict vowel is wrong (L AE1...) — overridden in the 2026-07-05 batch below.
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
    // Reported 2026-07-04 (batch 3).
    ("contextualize", "K AH0 N T EH1 K S CH UW0 AH0 L AY2 Z"),
    ("contextualized", "K AH0 N T EH1 K S CH UW0 AH0 L AY2 Z D"),
    ("contextualizing", "K AH0 N T EH1 K S CH UW0 AH0 L AY2 Z IH0 NG"),
    // Found by the round-trip harness 2026-07-04 ("elon" was being spelled
    // out letter-by-letter — heard as "yellow in").
    ("elon", "IY1 L AA2 N"),
    // Reported 2026-07-05 (alba + jenny). All OOV -> the phonemizer was spelling
    // them letter-by-letter ("R-E-Q-U-E-U-I-N-G"). Built from morphemes (queue,
    // monetary/monetize) or pinned proper/brand forms. The generated/trickery/
    // separate reports were a separate us_to_rp linking-r bug (fixed there), not
    // OOV — not listed here. layoff(s)/Claude/monetize/utils were CHECKED and are
    // already correct on GB (not touched).
    ("requeue", "R IY0 K Y UW1"),
    ("requeued", "R IY0 K Y UW1 D"),
    ("requeues", "R IY0 K Y UW1 Z"),
    ("requeuing", "R IY0 K Y UW1 IH0 NG"),
    ("homunculus", "HH OW0 M AH1 NG K Y AH0 L AH0 S"),
    ("homunculi", "HH OW0 M AH1 NG K Y AH0 L AY0"),
    ("computability", "K AH0 M P Y UW2 T AH0 B IH1 L AH0 T IY0"),
    ("monetize", "M AA1 N AH0 T AY2 Z"),
    ("monetized", "M AA1 N AH0 T AY2 Z D"),
    ("monetizes", "M AA1 N AH0 T AY2 Z IH0 Z"),
    ("monetizing", "M AA1 N AH0 T AY2 Z IH0 NG"),
    ("monetization", "M AA2 N AH0 T AH0 Z EY1 SH AH0 N"),
    ("unmonetized", "AH0 N M AA1 N AH0 T AY2 Z D"),
    // Tech terms mixing a word with letters, or dev tools CMUdict predates.
    ("async", "EY1 S IH0 NG K"),
    ("vscode", "V IY1 EH1 S K OW1 D"),
    ("synthid", "S IH1 N TH AY2 D IY2"),
    ("sqlite", "EH1 S K Y UW1 EH1 L AY2 T"),
    // Plural of the acronym "ID": the generic acronym-plural rule found the dict
    // word "id" (Freudian /ɪd/) for the base and produced "idz". Pin the plural.
    ("ids", "AY1 D IY1 Z"),
    // Expansion target for "Ms." (normalize_text). The GB dict has "mizz" (mˈɪz)
    // but US would spell it out, so pin the US reading too.
    ("mizz", "M IH1 Z"),
    // Reported 2026-07-05 (alba), second batch. Brand/tech proper nouns and an
    // OOV suffix family: iPhone/xhigh/sql are letter-name-heavy OOV terms (sql
    // mirrors the existing sqlite override's stressed letter names); github
    // pins the US reading and lets the possessive fallback resolve "GitHub's"
    // through it on every locale, alongside GB's own git+hub compound path.
    // upsert/iterate are missing from both dicts and the g2p LTS mangles them
    // ("EYE-ter-ot-or"); "iterate" itself is already pinned above, so only its
    // unlisted derived forms are added here. lazily corrects a wrong vowel in
    // cmudict_data (L AE1 Z AH0 L IY0; lazy is L EY1 Z IY0).
    ("iphone", "AY1 F OW2 N"),
    ("github", "G IH1 T HH AH2 B"),
    ("xhigh", "EH1 K S HH AY2"),
    ("sql", "EH1 S K Y UW1 EH1 L"),
    ("upsert", "AH1 P S ER2 T"),
    ("upserts", "AH1 P S ER2 T S"),
    ("upserted", "AH1 P S ER2 T IH0 D"),
    ("upserting", "AH1 P S ER2 T IH0 NG"),
    ("lazily", "L EY1 Z AH0 L IY0"),
    ("iterates", "IH1 T ER0 EY2 T S"),
    ("iterated", "IH1 T ER0 EY2 T IH0 D"),
    ("iterating", "IH1 T ER0 EY2 T IH0 NG"),
    ("iterator", "IH1 T ER0 EY2 T ER0"),
    ("iterators", "IH1 T ER0 EY2 T ER0 Z"),
    // Reported 2026-07-06 (alba, GB voice). OOV tech/brand terms that were
    // spelling out letter-by-letter; built from real morphemes/compounds
    // already in the dict (pie+torch, mat+mul rhyming with dull/hull/cull,
    // in+painting, convolution's first syllable, re+size — "re-" is
    // deliberately excluded from OOV_PREFIXES because the dict's "re" is the
    // musical note "ray", so resize's family needs its own entries here,
    // using the real prefix sound like repurpose/reanimate/requeue), or a
    // rhyme match for a proper noun ("vanilla"'s V AH0 N IH1 L AH0 template
    // restressed for Mozilla). "amount" and "Einsum" were reported in the
    // same batch but already probe correct on GB (Einsum resolves through
    // the phonemizer's own letter-to-sound guess, not letter spelling) and
    // are deliberately left untouched.
    ("pytorch", "P AY1 T AO1 R CH"),
    ("inpainting", "IH0 N P EY1 N T IH0 NG"),
    ("matmul", "M AE1 T M AH1 L"),
    ("conv", "K AA1 N V"),
    ("mozilla", "M AH0 Z IH1 L AH0"),
    ("resize", "R IY0 S AY1 Z"),
    ("resizes", "R IY0 S AY1 Z AH0 Z"),
    ("resized", "R IY0 S AY1 Z D"),
    ("resizing", "R IY0 S AY1 Z IH0 NG"),
    // "initialize"/"initialized" already phonemize correctly (real cmudict
    // entries); only the agentive noun forms were missing.
    ("initializer", "IH0 N IH1 SH AH0 L AY2 Z ER0"),
    ("initializers", "IH0 N IH1 SH AH0 L AY2 Z ER0 Z"),
    // Feeds the "Q&A"/"Q & A" -> "QandA" glue in normalize_text: Q's letter
    // name (matching the existing "sql" override), the dict word "and", and
    // A's letter name (matching the existing "ai" override). Keeps the bare
    // dictionary word "a" (the indefinite article) untouched everywhere else.
    ("qanda", "K Y UW1 AH0 N D EY1"),
    // Reported 2026-07-11 (alba/cori), large batch: OOV tech terms, brands,
    // and derived forms CMUdict lacks, previously spelled letter-by-letter.
    // Phones built from the dict's own components (guesser, bouncer, coder,
    // vector, rank, spoof, forge, implement, evaluate, validate, optimize).
    ("geoguessr", "JH IY1 OW0 G EH2 S ER0"),
    ("luddism", "L AH1 D IH2 Z AH0 M"),
    ("spoofable", "S P UW1 F AH0 B AH0 L"),
    ("unspoofable", "AH0 N S P UW1 F AH0 B AH0 L"),
    ("forgeable", "F AO1 R JH AH0 B AH0 L"),
    ("unforgeable", "AH0 N F AO1 R JH AH0 B AH0 L"),
    ("validator", "V AE1 L AH0 D EY2 T ER0"),
    ("evaluator", "IH0 V AE1 L Y UW0 EY2 T ER0"),
    ("optimizer", "AA1 P T AH0 M AY2 Z ER0"),
    ("reformat", "R IY2 F AO1 R M AE2 T"),
    ("reimplement", "R IY2 IH1 M P L AH0 M EH2 N T"),
    ("reimplemented", "R IY2 IH1 M P L AH0 M EH2 N T AH0 D"),
    ("rerank", "R IY0 R AE1 NG K"),
    ("reranker", "R IY0 R AE1 NG K ER0"),
    ("reranking", "R IY0 R AE1 NG K IH0 NG"),
    ("contextual", "K AH0 N T EH1 K S CH UW0 AH0 L"),
    ("contextually", "K AH0 N T EH1 K S CH UW0 AH0 L IY0"),
    ("gamification", "G EY2 M AH0 F IH0 K EY1 SH AH0 N"),
    ("parallelization", "P EH2 R AH0 L EH0 L IH0 Z EY1 SH AH0 N"),
    ("writeup", "R AY1 T AH2 P"),
    // The common "manageable" misspelling — frequent enough in article prose
    // to deserve the intended word's phones instead of a spell-out.
    ("managable", "M AE1 N IH0 JH AH0 B AH0 L"),
    ("pareto", "P ER0 EY1 T OW0"),
    ("canva", "K AE1 N V AH0"),
    ("technica", "T EH1 K N IH0 K AH0"),
    ("naur", "N AW1 R"),
    ("goldratt", "G OW1 L D R AE2 T"),
    ("auggie", "AO1 G IY0"),
    ("junie", "JH UW1 N IY0"),
    ("qoder", "K OW1 D ER0"),
    ("vybe", "V AY1 B"),
    ("vram", "V IY1 R AE1 M"),
    ("llm", "EH1 L EH1 L EH1 M"),
    // Both forms: the phonemizer's letter-to-sound model GUESSES "ctes"
    // (kɔːɹtɪz, rhyme-by-analogy to "cortes") instead of leaving it OOV, so
    // the acronym-plural fallback never runs — the plural needs its own entry.
    ("cte", "S IY1 T IY1 IY1"),
    ("ctes", "S IY1 T IY1 IY1 Z"),
    ("sci", "S AY1"),
    ("prismml", "P R IH1 Z AH0 M EH1 M EH1 L"),
    ("spacexai", "S P EY1 S EH2 K S EY1 AY1"),
    // The Postgres family: the official "post-gres-Q-L" reading, and the
    // pg-prefixed ecosystem tools ("pee-gee-..."). "trgm" is spoken as the
    // trigram it abbreviates ("pg_trgm" -> "pee-gee trigram").
    ("postgres", "P OW1 S T G R EH2 S"),
    ("postgresql", "P OW1 S T G R EH2 S K Y UW2 EH2 L"),
    ("pgdog", "P IY1 JH IY1 D AA1 G"),
    ("pgbouncer", "P IY1 JH IY1 B AW1 N S ER0"),
    ("pgvector", "P IY1 JH IY1 V EH1 K T ER0"),
    ("tsvector", "T IY1 EH1 S V EH1 K T ER0"),
    ("trgm", "T R AY1 G R AE2 M"),
    ("jsonb", "JH EY1 S AH0 N B IY1"),
    ("mongodb", "M AA1 NG G OW0 D IY1 B IY1"),
    // The hertz family above ~1kHz is OOV as words ("5GHz" -> normalize says
    // "five gigahertz" -> spelled G-I-G-A-H-E-R-T-Z). "hertz" itself is in
    // CMUdict; the prefixed forms aren't (and the compound splitter's "kilo"
    // is the GB dict's "keelo").
    ("kilohertz", "K IH1 L AH0 HH ER2 T S"),
    ("megahertz", "M EH1 G AH0 HH ER2 T S"),
    ("gigahertz", "G IH1 G AH0 HH ER2 T S"),
    // The quantize family: only "quantized" exists in the GB dict, nothing in
    // CMUdict, so the others spelled out on every voice.
    ("quantize", "K W AA1 N T AY2 Z"),
    ("quantized", "K W AA1 N T AY2 Z D"),
    ("quantizer", "K W AA1 N T AY2 Z ER0"),
    ("quantizing", "K W AA1 N T AY2 Z IH0 NG"),
    ("quantization", "K W AA2 N T AH0 Z EY1 SH AH0 N"),
    ("timescaledb", "T AY1 M S K EY2 L D IY1 B IY1"),
    ("duckdb", "D AH1 K D IY1 B IY1"),
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

/// The text as it will be spoken, after typography and number normalization
/// ("since 1980, $10B" -> "since nineteen eighty, ten billion dollars").
/// Exposed for the round-trip harness, which aligns an ASR transcript of the
/// synthesized audio against this form.
pub fn spoken_text(text: &str) -> String {
    normalize_numbers(&normalize_text(text))
}

/// Read a 4-digit year the spoken way: "1980" -> "nineteen eighty",
/// "1905" -> "nineteen oh five", "1900" -> "nineteen hundred",
/// "2007" -> "two thousand seven", "2026" -> "twenty twenty six".
fn say_year(y: u64) -> String {
    let hi = y / 100;
    let lo = y % 100;
    if (2000..=2009).contains(&y) {
        return if lo == 0 {
            "two thousand".to_string()
        } else {
            format!("two thousand {}", ONES[lo as usize])
        };
    }
    if lo == 0 {
        return format!("{} hundred", say_cardinal(hi));
    }
    if lo < 10 {
        return format!("{} oh {}", say_cardinal(hi), ONES[lo as usize]);
    }
    format!("{} {}", say_cardinal(hi), say_cardinal(lo))
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
        // Leading sign ('+', '-', the U+2212 minus, or '~' for "about")
        // immediately before a digit, at a left boundary (start of text,
        // after whitespace, or after '('): consumes the sign and speaks a
        // word before the number. Boundary-gated so mid-token hyphens pass
        // through untouched: in "2019-2021" the '-' is preceded by '9'
        // (alphanumeric, not a boundary), so it falls to the plain
        // passthrough branch below. Reported 2026-07-06: "~1.3GB" left the
        // '~' silently dropped (not in phonemize's kept-punctuation set),
        // reading as if the approximation were never there.
        let sign = if matches!(c, '+' | '-' | '\u{2212}' | '~')
            && chars.get(i + 1).is_some_and(|d| d.is_ascii_digit())
            && (out.is_empty() || out.ends_with(|b: char| b.is_whitespace() || b == '('))
        {
            Some(match c {
                '+' => "plus",
                '~' => "about",
                _ => "minus",
            })
        } else {
            None
        };
        if !currency && sign.is_none() && !c.is_ascii_digit() {
            out.push(c);
            i += 1;
            continue;
        }
        if currency || sign.is_some() {
            i += 1;
        }
        let mut int_str = String::new();
        let mut had_sep = false;
        while i < chars.len() {
            let d = chars[i];
            if d.is_ascii_digit() {
                int_str.push(d);
                i += 1;
            } else if d == ',' && chars.get(i + 1).is_some_and(|x| x.is_ascii_digit()) {
                had_sep = true;
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
        // Magnitude suffix: "$10B" / "1.5M" / "200k" / "£3bn"-style. Lowercase
        // m/b/t are only magnitudes with a currency sign ("10m" is metres in
        // prose); uppercase and "k"/"bn" are safe either way.
        let mut magnitude = "";
        if !percent && !ordinal {
            let c1 = chars.get(i).copied().unwrap_or('\0');
            let c2 = chars.get(i + 1).copied().unwrap_or('\0');
            let boundary_after = |n: usize| {
                chars.get(i + n).map_or(true, |c| !c.is_alphanumeric())
            };
            if (c1 == 'b' || c1 == 'B') && (c2 == 'n' || c2 == 'N') && boundary_after(2) {
                magnitude = "billion";
                i += 2;
            } else if boundary_after(1) {
                magnitude = match c1 {
                    'k' | 'K' => "thousand",
                    'M' => "million",
                    'B' => "billion",
                    'T' => "trillion",
                    'm' if currency => "million",
                    'b' if currency => "billion",
                    't' if currency => "trillion",
                    _ => "",
                };
                if !magnitude.is_empty() {
                    i += 1;
                }
            }
        }
        // Byte-size unit suffix ("10GB" / "1.5MB" / "200KB" / "2TB" / "3PB"):
        // unlike magnitude, the two letters name the unit itself, spoken as
        // its own singular/plural word after the number, rather than scaling
        // an implicit noun ("$10B" is ten billion DOLLARS; "10GB" is not "ten
        // billion bytes"). Checked as its own two-letter unit so a bare
        // "GB"/"MB"/... is never left OOV for the letter-spelling fallback
        // ("G-B") to spell out instead. Exact uppercase pairs only:
        // lowercase "mb"/"tb" collide with other abbreviations (millibar,
        // tuberculosis) too often to guess at case-insensitively. Reported
        // 2026-07-06 ("~1.3GB" read the unit as "G-B" letters).
        let mut byte_unit = "";
        if !percent && !ordinal && magnitude.is_empty() {
            let c1 = chars.get(i).copied().unwrap_or('\0');
            let c2 = chars.get(i + 1).copied().unwrap_or('\0');
            let boundary_after2 = chars.get(i + 2).map_or(true, |c| !c.is_alphanumeric());
            if c2 == 'B' && boundary_after2 {
                byte_unit = match c1 {
                    'K' => "kilobyte",
                    'M' => "megabyte",
                    'G' => "gigabyte",
                    'T' => "terabyte",
                    'P' => "petabyte",
                    _ => "",
                };
                if !byte_unit.is_empty() {
                    i += 2;
                }
            }
        }
        // Time / frequency unit suffix ("100ms" read "one hundred M's",
        // reported 2026-07-11; "5GHz", "44.1kHz"): same shape as byte units.
        // ms/ns are lowercase-exact (uppercase "MS" is an initialism too
        // often); the hertz family is case-insensitive ("kHz"/"GHz"/"ghz"
        // all appear in prose). "hertz" is its own plural.
        let mut time_unit = "";
        let mut time_unit_pluralizes = true;
        if !percent && !ordinal && magnitude.is_empty() && byte_unit.is_empty() {
            let c1 = chars.get(i).copied().unwrap_or('\0');
            let c2 = chars.get(i + 1).copied().unwrap_or('\0');
            let c3 = chars.get(i + 2).copied().unwrap_or('\0');
            let boundary2 = chars.get(i + 2).map_or(true, |c| !c.is_alphanumeric());
            let boundary3 = chars.get(i + 3).map_or(true, |c| !c.is_alphanumeric());
            let (l1, l2, l3) =
                (c1.to_ascii_lowercase(), c2.to_ascii_lowercase(), c3.to_ascii_lowercase());
            if l2 == 'h' && l3 == 'z' && boundary3 && matches!(l1, 'k' | 'm' | 'g') {
                time_unit = match l1 {
                    'k' => "kilohertz",
                    'm' => "megahertz",
                    _ => "gigahertz",
                };
                time_unit_pluralizes = false;
                i += 3;
            } else if l1 == 'h' && l2 == 'z' && boundary2 {
                time_unit = "hertz";
                time_unit_pluralizes = false;
                i += 2;
            } else if c1 == 'm' && c2 == 's' && boundary2 {
                time_unit = "millisecond";
                i += 2;
            } else if c1 == 'n' && c2 == 's' && boundary2 {
                time_unit = "nanosecond";
                i += 2;
            }
        }
        // Pre-release version suffix ("4.0a1" -> "... alpha one", "1.0rc2" ->
        // "... release candidate two"): only after a dotted version (frac
        // non-empty), when a/b/rc is immediately followed by 1+ digits and
        // then a non-alphanumeric boundary. Only the tag letters are consumed
        // here; the trailing digits are left for the next loop pass so they
        // still render as their own number ("3.5ab", "2.0abc1", "4.0a" all
        // fail one of these conditions and fall through unchanged).
        let mut prerelease = "";
        if !frac.is_empty() {
            for (tag, word) in [("a", "alpha"), ("b", "beta"), ("rc", "release candidate")] {
                let tag_chars: Vec<char> = tag.chars().collect();
                let matches_tag = tag_chars.iter().enumerate().all(|(k, &tc)| {
                    chars.get(i + k).is_some_and(|c| c.to_ascii_lowercase() == tc)
                });
                if !matches_tag {
                    continue;
                }
                let after = i + tag_chars.len();
                let digits = chars[after..].iter().take_while(|c| c.is_ascii_digit()).count();
                let boundary = chars.get(after + digits).map_or(true, |c| !c.is_alphanumeric());
                if digits > 0 && boundary {
                    prerelease = word;
                    i = after;
                    break;
                }
            }
        }
        let int_val: u64 = int_str.parse().unwrap_or(0);
        // A bare 4-digit 1100-2099 integer in prose is a year: "since 1980"
        // must read "nineteen eighty", not "one thousand nine hundred eighty".
        // Comma-grouped ("1,321") or signed ("+1980") numbers are never years.
        let is_year = !currency && !percent && !ordinal && frac.is_empty() && !had_sep
            && sign.is_none() && magnitude.is_empty() && int_str.len() == 4
            && (1100..=2099).contains(&int_val);
        if out.ends_with(|c: char| c.is_alphanumeric()) {
            out.push(' ');
        }
        if let Some(word) = sign {
            out.push_str(word);
            out.push(' ');
        }
        if is_year {
            out.push_str(&say_year(int_val));
        } else if currency {
            out.push_str(&say_cardinal(int_val));
            if !magnitude.is_empty() {
                // "$1.5B" -> "one point five billion dollars"
                if !frac.is_empty() {
                    out.push_str(" point");
                    for d in frac.chars() {
                        out.push(' ');
                        out.push_str(ONES[d as usize - '0' as usize]);
                    }
                }
                out.push(' ');
                out.push_str(magnitude);
                out.push_str(" dollars");
            } else if !frac.is_empty() {
                out.push_str(if int_val == 1 { " dollar" } else { " dollars" });
                let cents: u64 = format!("{frac:0<2}")[..2].parse().unwrap_or(0);
                out.push_str(&format!(" and {} cents", say_cardinal(cents)));
            } else {
                out.push_str(if int_val == 1 { " dollar" } else { " dollars" });
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
            if !magnitude.is_empty() {
                out.push(' ');
                out.push_str(magnitude);
            }
            if !prerelease.is_empty() {
                out.push(' ');
                out.push_str(prerelease);
            }
        } else {
            out.push_str(&say_cardinal(int_val));
            if !magnitude.is_empty() {
                out.push(' ');
                out.push_str(magnitude);
            }
        }
        if percent {
            out.push_str(" percent");
        }
        if !byte_unit.is_empty() {
            out.push(' ');
            out.push_str(byte_unit);
            // Singular only for an exact, unfractioned one ("1GB" -> "one
            // gigabyte"); "1.3GB" and "1,000GB" both stay plural.
            if !(frac.is_empty() && !had_sep && int_val == 1) {
                out.push('s');
            }
        }
        if !time_unit.is_empty() {
            out.push(' ');
            out.push_str(time_unit);
            if time_unit_pluralizes && !(frac.is_empty() && !had_sep && int_val == 1) {
                out.push('s');
            }
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
/// curly quotes become straight; ellipsis becomes a full stop; dashes used as
/// a spoken pause become a clause-pause comma (see the dash block below).
/// Runs before tokenization.
fn normalize_text(text: &str) -> String {
    // Latin abbreviations: the internal dots would otherwise read as pauses
    // between the spelled letters ("eye. ee."). Stripped to bare letters they
    // read naturally ("eye ee"); "etc" has a dictionary entry already. Word
    // counts are unaffected (one \S+ token either way is rebuilt identically
    // by the timing path).
    let text = text
        .replace("i.e.", "i e")
        .replace("I.e.", "i e")
        .replace("e.g.", "e g")
        .replace("E.g.", "e g");
    // Honorific abbreviations: the dict stores "Dr" as "drive" ("Dr. Hinton" ->
    // "Drive Hinton") and spells the rest letter-by-letter. Expand the
    // unambiguous ones to words. TRADEOFF: a street "Dr." (Drive) also becomes
    // "Doctor", but that's vanishingly rare in the article prose this reads.
    // Boundary-gated at the left edge: the old bare substring replace fired
    // MID-WORD, so a sentence-final "LLMs." became "LLMizz" and was spelled
    // out as L-L-M-I-Z-Z (reported 2026-07-11).
    let text = expand_title_abbrevs(&text);

    // Reported 2026-07-06: "TL;DR:" split across the ';' (clause pause) and
    // ':' (another pause): "TL" spelled letter-by-letter, then, after a beat,
    // "DR" resolved through the dict entry for the street/honorific
    // abbreviation "Dr" ("drive") instead of being OOV — probed as
    // "tiːɛl; dɹˈaɪv:" ("T-L, drive:"). Gluing the two halves into one
    // "TLDR" routes it through the ordinary OOV letter-spelling fallback
    // instead, reading a clean "T-L-D-R" (it's too short for the compound
    // splitter, which needs 3+ chars per half). Covers both "TL;DR" and
    // "TL;DR:" (a trailing colon just stays in the text and reads as its
    // own pause, same as any other colon).
    let text = text.replace("TL;DR", "TLDR");
    // Reported 2026-07-06: '&' isn't in phonemize's kept-punctuation set, so
    // "Q&A" silently dropped it and read standalone "A" as the indefinite
    // article's weak schwa (dict "a"), not the letter name — losing the
    // "and" and mispronouncing the "A" in one go. Glue it into a single
    // placeholder word so it resolves through the "qanda" override (which
    // spells Q, "and", and the letter A) instead of touching how bare "a"
    // reads everywhere else in running prose.
    let text = text.replace("Q&A", "QandA").replace("Q & A", "QandA");
    // Reported 2026-07-11: all-caps "VS" hits CMUdict's "vs" entry and reads
    // "versus" — right for "cats vs dogs", wrong for the editor. Brand-exact
    // rewrite (same approach as Q&A) so lowercase "vs"/"vs." keep "versus".
    let text = text.replace("VS Code", "V S Code");

    // Dashes used as a spoken pause ("wait — no", "wait -- no", "wait - no")
    // rewrite to a clause-pause comma so they ride the CLAUSE_PAUSE_MS
    // machinery split_for_pauses and phonemize's kept-punctuation filter
    // already give ',' — em/en dash used to map straight to ',' with no
    // regard for spacing or neighboring digits, which (a) left a bare ASCII
    // hyphen used the same way ("wait - no") without any pause cue at all,
    // since '-' isn't in that kept-punctuation set, and (b) mangled a digit
    // range ("2019\u{2013}2021") into a comma that normalize_numbers then
    // read as a thousands separator, fusing the two numbers into one huge
    // cardinal. Any surrounding space is collapsed so the result is always
    // "word, word", never "word , word". A digit-adjacent dash is a RANGE,
    // not a pause, and becomes a plain hyphen instead — same as an ASCII
    // hyphen range, which already reads silently (split_tokens/phonemize
    // drop a bare mid-token '-'; see normalize_signs_and_prerelease).
    // Unspaced hyphens ("un-iterated", "well-known") are compound words, not
    // dashes, and are left untouched.
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    // Rewrite the dash of length `len` starting at `from` into a clause-pause
    // comma: drop one trailing space already written to `out` (if any) and
    // swallow one leading space in the source (if any). Returns the index
    // just past the consumed dash (and any swallowed trailing space).
    let push_dash_pause = |out: &mut String, from: usize, len: usize| -> usize {
        if out.ends_with(' ') {
            out.pop();
        }
        out.push_str(", ");
        let mut j = from + len;
        if chars.get(j).is_some_and(|&d| d == ' ') {
            j += 1;
        }
        j
    };
    let mut out = String::new();
    let mut i = 0;
    while i < n {
        let c = chars[i];
        if matches!(c, '\u{2019}' | '\u{2018}' | '\u{02BC}') {
            out.push('\'');
            i += 1;
        } else if matches!(c, '\u{201C}' | '\u{201D}') {
            out.push('"');
            i += 1;
        } else if c == '\u{2026}' {
            out.push('.');
            i += 1;
        } else if matches!(c, '\u{2014}' | '\u{2013}') {
            let is_range = i >= 1
                && chars[i - 1].is_ascii_digit()
                && chars.get(i + 1).is_some_and(|d| d.is_ascii_digit());
            if is_range {
                out.push('-');
                i += 1;
            } else {
                i = push_dash_pause(&mut out, i, 1);
            }
        } else if c == '-' && chars.get(i + 1).is_some_and(|&d| d == '-') {
            // Double (or longer) hyphen run: an old-typewriter dash substitute.
            let mut end = i;
            while chars.get(end).is_some_and(|&d| d == '-') {
                end += 1;
            }
            let is_range = i >= 1
                && chars[i - 1].is_ascii_digit()
                && chars.get(end).is_some_and(|d| d.is_ascii_digit());
            if is_range {
                for _ in i..end {
                    out.push('-');
                }
                i = end;
            } else {
                i = push_dash_pause(&mut out, i, end - i);
            }
        } else if c == '-'
            && i >= 2
            && chars[i - 1] == ' '
            && chars[i - 2].is_alphabetic()
            && chars.get(i + 1).is_some_and(|&d| d == ' ')
            && chars.get(i + 2).is_some_and(|d| d.is_alphabetic())
        {
            // A spaced single hyphen with letters on both ends is a dash
            // ("word - word"), not a line-start list marker (which has no
            // letter immediately before the leading space) and not a
            // compound word (which has no space around its hyphen).
            i = push_dash_pause(&mut out, i, 1);
        } else if c == '.'
            && chars.get(i + 1).is_some_and(|&d| d == 'j')
            && chars.get(i + 2).is_some_and(|&d| d == 's')
            && out.ends_with(|c: char| c.is_alphanumeric())
            && chars.get(i + 3).map_or(true, |d| !d.is_alphanumeric())
        {
            // Reported 2026-07-06 ("Transformers.js"): a glued library-name
            // suffix. The bare '.' is punctuation phonemize treats as a
            // full-stop pause (its kept-punctuation filter), splitting one
            // name into two sentences ("Transformers... J-S"). Word-boundary
            // gated on both sides so it doesn't fire inside ".json"/".jsx"
            // (a real alphanumeric char right after "js" fails the check) or
            // a standalone ".js" with nothing before it.
            out.push(' ');
            i += 1;
        } else if c == '+' && i >= 1 && chars[i - 1].is_alphanumeric() {
            // '+' glued after a word or number ("TLA+", "C++", "A+", "1+1"):
            // not in phonemize's kept-punctuation set, so it read as silence
            // (reported 2026-07-11, "TLA+"). Speak each one ("C++" ->
            // "C plus plus"). A '+' at a left boundary ("+5") is left for
            // normalize_numbers' sign handling.
            // A grade-style standalone "A" before the plus ("A+") is the
            // letter, not the article — rewrite it to "ay" (see the "ay"
            // overrides) while the '+' still marks the distinction. Plain
            // "A plus point" prose (no glued '+') keeps its article.
            if out.ends_with('A')
                && !out[..out.len() - 1].ends_with(|b: char| b.is_alphanumeric())
            {
                out.pop();
                out.push_str("ay");
            }
            let mut end = i;
            while chars.get(end).is_some_and(|&d| d == '+') {
                end += 1;
                out.push_str(" plus");
            }
            if chars.get(end).is_some_and(|d| d.is_alphanumeric()) {
                out.push(' ');
            }
            i = end;
        } else {
            out.push(c);
            i += 1;
        }
    }
    expand_caps_digit_acronyms(&out)
}

/// Left-boundary-gated honorific expansion ("Dr. Hinton" -> "Doctor Hinton").
/// A match must start the text or follow a non-alphanumeric char so word
/// tails never fire: "LLMs." contains "Ms." but must stay intact. "Mrs."
/// before "Mr." isn't required (the dot makes them non-overlapping) but
/// keeps intent obvious.
fn expand_title_abbrevs(text: &str) -> String {
    const TITLES: &[(&str, &str)] = &[
        ("Mrs.", "Missus"),
        ("Ms.", "Mizz"),
        ("Mr.", "Mister"),
        ("Dr.", "Doctor"),
        ("Prof.", "Professor"),
    ];
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    'outer: while i < chars.len() {
        if i == 0 || !chars[i - 1].is_alphanumeric() {
            for (pat, rep) in TITLES {
                let p: Vec<char> = pat.chars().collect();
                if chars[i..].starts_with(&p[..]) {
                    out.push_str(rep);
                    i += p.len();
                    continue 'outer;
                }
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// Words after which a standalone capital "A" is the letter, not the article
/// ("Plan A", "Type A", "Vitamin A"). Compared lowercased. "an"/"the" are
/// here because a determiner can never precede the article ("an A grade",
/// "the A team" are always the letter).
const LETTER_A_TRIGGERS: &[&str] = &[
    "plan", "type", "section", "option", "grade", "team", "group", "tier",
    "phase", "block", "schedule", "exhibit", "appendix", "vitamin", "route",
    "model", "class", "category", "variant", "version", "series", "an", "the",
];

/// Expand all-caps tokens mixing letters and digits ("C2PA", "ES5", "MP3",
/// "EC2", "B52") into spaced letters and digit runs ("C 2 P ay", "E S 5") so
/// the letters read as names and the digits as numbers. Without this the
/// letter runs collide with dictionary words ("C2PA" read "see two PAH" via
/// the dict word "pa"; "ES5" read "ess five", losing the E). Rules:
/// - Tokens starting with a digit are left alone: "100ms" and "8GB" are
///   unit-suffixed numbers for normalize_numbers, not acronyms.
/// - Any letter run of 4+ chars leaves the whole token alone: "COVID19" and
///   "HTML5" are word-acronyms the dictionary path already reads better.
/// - The letter A becomes "ay" (see the GB/US "ay" overrides) because a bare
///   "A" token is the indefinite article in both dictionaries.
/// Also rewrites a standalone "A" after a LETTER_A_TRIGGERS word ("Plan A"
/// read "plan uh" as the article, reported 2026-07-11); any sentence-ish
/// punctuation between them resets the trigger so "...the plan. A new..."
/// keeps its article.
fn expand_caps_digit_acronyms(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    let mut last_word = String::new();
    while i < chars.len() {
        let c = chars[i];
        if !c.is_alphanumeric() {
            if matches!(c, '.' | '!' | '?' | ',' | ';' | ':' | '\n') {
                last_word.clear();
            }
            out.push(c);
            i += 1;
            continue;
        }
        let start = i;
        while i < chars.len() && chars[i].is_alphanumeric() {
            i += 1;
        }
        let tok: String = chars[start..i].iter().collect();
        let n = i - start;
        let mut max_letter_run = 0usize;
        let mut run = 0usize;
        for tc in tok.chars() {
            if tc.is_ascii_alphabetic() {
                run += 1;
                max_letter_run = max_letter_run.max(run);
            } else {
                run = 0;
            }
        }
        let caps_digit = (2..=8).contains(&n)
            && tok.chars().all(|tc| tc.is_ascii_uppercase() || tc.is_ascii_digit())
            && tok.chars().next().is_some_and(|tc| tc.is_ascii_uppercase())
            && tok.chars().any(|tc| tc.is_ascii_digit())
            && max_letter_run <= 3;
        if caps_digit {
            let mut cs = tok.chars().peekable();
            let mut first = true;
            while let Some(tc) = cs.next() {
                if !first {
                    out.push(' ');
                }
                first = false;
                if tc == 'A' {
                    out.push_str("ay");
                } else if tc.is_ascii_digit() {
                    out.push(tc);
                    while let Some(&d) = cs.peek() {
                        if !d.is_ascii_digit() {
                            break;
                        }
                        out.push(d);
                        cs.next();
                    }
                } else {
                    out.push(tc);
                }
            }
        } else if tok == "A" && LETTER_A_TRIGGERS.contains(&last_word.as_str()) {
            out.push_str("ay");
        } else {
            out.push_str(&tok);
        }
        last_word = tok.to_lowercase();
    }
    out
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
    "us", "uk", "eu", "un", "tv", "id", "os", "ip", "ui", "ux", "pr", "hr", "it",
    // Reported 2026-07-06: "aws" isn't a dict word, but the phonemizer's own
    // letter-to-sound guess reads it by analogy to "-aws" words like "jaws"
    // (/ɔːz/) instead of leaving it OOV for spell()'s letter-by-letter path,
    // so it needs the same explicit spelling as the acronyms above.
    "aws",
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
    // R carry rhotic vowels otherwise). The spelled variant of the transform
    // skips the word-final happY/lettER adjustments, which would clip the
    // last letter's name ("CVE" -> see-vee-ih).
    let spell = |w: &str| -> Vec<String> {
        let t = spell_word(w);
        if gb.is_some() {
            crate::gb_english::us_to_rp_spelled(t)
        } else {
            t
        }
    };

    // Split an OOV word into two dictionary words and pronounce each half
    // (pushbuffer -> push + buffer), demoting the second part's primary
    // stress (U+02C8) to secondary (U+02CC) so the pair reads as one
    // prosodic word instead of two with a pause. Shared by the possessive
    // fallback (so "GitHub's" can resolve its base "GitHub" through a
    // compound split even when the base alone is OOV) and the compound
    // fallback below, so the two paths can't drift apart.
    let compound_join = |w: &str| -> Result<Option<Vec<String>>, String> {
        let Some(parts) = segment_compound(word_set, w) else { return Ok(None) };
        let mut combined: Vec<String> = Vec::new();
        for (pi, part) in parts.iter().enumerate() {
            let mut sub = phonemize_word(part)?;
            if is_oov(&sub) {
                return Ok(None);
            }
            if pi > 0 {
                for t in sub.iter_mut() {
                    if t == "\u{02C8}" { *t = "\u{02CC}".to_string(); }
                }
            }
            combined.append(&mut sub);
        }
        if combined.is_empty() { Ok(None) } else { Ok(Some(combined)) }
    };

    let pieces = split_tokens(text);
    // Context-resolved heteronyms: piece index -> pseudo dictionary key.
    let het = crate::heteronyms::resolve(&pieces);
    let mut tokens: Vec<String> = Vec::new();
    let mut first = true;

    for (piece_idx, piece) in pieces.iter().enumerate() {
        // A piece is a word if it contains ANY alphanumeric — judging by the
        // first character silently dropped quote-wrapped words ("'last" in
        // «the 'last mile' stuff» classified as punctuation and was never
        // spoken). Surrounding quotes/apostrophes are trimmed for processing;
        // internal ones (don't, cat's) are untouched.
        let is_word = piece.chars().any(|c| c.is_alphanumeric());
        let piece: &str = if is_word {
            piece.trim_matches(|c: char| !c.is_alphanumeric())
        } else {
            piece
        };

        let mut p_tokens: Vec<String> = if is_word {
            let all_caps =
                piece.chars().count() >= 2 && piece.chars().all(|c| c.is_ascii_uppercase());
            let mut pt = if let Some(key) = het.get(&piece_idx) {
                // Context-resolved heteronym: look up its dict_key() form (raw
                // "read1" would be stripped to "read" by the tokenizer). Rides
                // the normal dict path + US->RP transform on GB models.
                phonemize_word(&crate::heteronyms::dict_key(key))?
            } else if all_caps
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
                // so they never reach here; this is for proper nouns.) If the
                // base itself is OOV, also try splitting it as a compound before
                // giving up — this is what resolves "GitHub's" (base "GitHub")
                // when only the compound path (git + hub) can pronounce it.
                if let Some(base) = piece.strip_suffix("'s") {
                    if !base.is_empty() {
                        let mut b = phonemize_word(base)?;
                        if is_oov(&b) {
                            if let Some(joined) = compound_join(base)? {
                                b = joined;
                            }
                        }
                        if !is_oov(&b) {
                            // Same phonology as the plural fallback below:
                            // /ɪz/ after sibilants ("Postgres's"), /s/ after
                            // voiceless stops/fricatives ("Goldratt's"), else
                            // /z/. A bare "z" glued a sibilant cluster.
                            let last = b.last().cloned().unwrap_or_default();
                            match last.as_str() {
                                "s" | "z" | "ʃ" | "ʒ" => {
                                    b.push("ɪ".to_string());
                                    b.push("z".to_string());
                                }
                                "p" | "t" | "k" | "f" | "θ" => b.push("s".to_string()),
                                _ => b.push("z".to_string()),
                            }
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
                // Plural of a known word ("hedonists" when only "hedonist" is
                // in a dictionary): pronounce the base and append the plural
                // phone by ordinary phonology — /ɪz/ after sibilants, /s/
                // after voiceless stops/fricatives, else /z/.
                if !replaced {
                    let lower = piece.to_lowercase();
                    if lower.len() > 3 && lower.ends_with('s') && !lower.ends_with("ss") {
                        let base = &lower[..lower.len() - 1];
                        let mut b = phonemize_word(base)?;
                        if !is_oov(&b) {
                            let last = b.last().cloned().unwrap_or_default();
                            match last.as_str() {
                                "s" | "z" | "ʃ" | "ʒ" => {
                                    b.push("ɪ".to_string());
                                    b.push("z".to_string());
                                }
                                "p" | "t" | "k" | "f" | "θ" => b.push("s".to_string()),
                                _ => b.push("z".to_string()),
                            }
                            pt = b;
                            replaced = true;
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
                // camelCase/PascalCase identifiers ("NotImplementedError",
                // "PrimaryKeyRequired"): split at every lower->upper boundary
                // and phonemize each piece as its own word, joined by a
                // literal space token — identifiers read as separate words
                // ("Not Implemented Error"), unlike glued compounds which read
                // as one. Gated on >= 2 transitions (>= 3 parts) so
                // single-transition brand names fall through to their own
                // overrides/compound split instead (iPhone, GitHub); iOS (1
                // transition, 2 parts) keeps spelling letter-by-letter.
                if !replaced {
                    let mut parts: Vec<String> = Vec::new();
                    let mut cur = String::new();
                    let mut it = piece.chars().peekable();
                    while let Some(c) = it.next() {
                        cur.push(c);
                        if c.is_lowercase() && it.peek().is_some_and(|n| n.is_uppercase()) {
                            parts.push(std::mem::take(&mut cur));
                        }
                    }
                    if !cur.is_empty() {
                        parts.push(cur);
                    }
                    if parts.len() >= 3 {
                        let mut combined: Vec<String> = Vec::new();
                        let mut ok = true;
                        for (pi, part) in parts.iter().enumerate() {
                            let sub = phonemize_word(part)?;
                            if is_oov(&sub) { ok = false; break; }
                            if pi > 0 {
                                combined.push(" ".to_string());
                            }
                            combined.extend(sub);
                        }
                        if ok && !combined.is_empty() {
                            pt = combined;
                            replaced = true;
                        }
                    }
                }
                // Else split a glued compound into two dictionary words and
                // pronounce each half (pushbuffer -> push + buffer).
                if !replaced {
                    if let Some(joined) = compound_join(piece)? {
                        pt = joined;
                        replaced = true;
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

        // Words are separated by spaces; punctuation ATTACHES to the previous
        // word ("mˈiːn," not "mˈiːn ,") — that's the shape of the training
        // streams. A space before the mark is out-of-distribution and renders
        // as a faint stop-release ("ed") on some voices; measured on alba, the
        // spaced form's tail is ~2x louder than the attached form's.
        if !first && is_word {
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
        // A dot in a single-letter abbreviation ("i.e.", "e.g.", "U.S.") is
        // not a break: the letter before it starts at a word boundary. A
        // sentence pause there splits the abbreviation in half.
        let abbrev_dot = ch == '.'
            && i >= 1
            && chars[i - 1].is_alphabetic()
            && (i < 2 || !chars[i - 2].is_alphanumeric());
        let is_break = !abbrev_dot
            && match pause_of(ch) {
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
pub(crate) fn ensure_ort_init() -> Result<(), String> {
    ORT_INIT
        .get_or_init(|| {
            // commit() returns false when an ORT environment was already
            // configured by another subsystem (grammar_neural also inits ORT).
            // That's not a failure — both share the one global environment, and
            // each session sets its own execution provider anyway. Only the
            // first caller's env config wins; the rest are benign no-ops.
            ort::init().commit();
            Ok(())
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
        // Heteronym pseudo-keys ("read1"/"read2"), stored under dict_key() so
        // the tokenizer's digit-stripping doesn't collapse them to the base
        // word. Absent from the GB dict by construction, so both readings take
        // the US->RP path and the locales agree on the resolver's pick.
        for (key, phonemes) in crate::heteronyms::PRONS {
            dict.insert(crate::heteronyms::dict_key(key), phonemes.to_string());
        }
        // Snapshot the keys (dict is moved into the phonemizer next) for the
        // OOV compound splitter. CMUdict keys are already lowercase, as are the
        // overrides; a couple hundred KB of Strings, built once at load.
        let mut word_set: HashSet<String> = dict.keys().cloned().collect();
        let phonemizer = EnglishPhonemizer::new_with_hashmap(dict);

        // Locale from the model's own sidecar: GB models get the British
        // dictionary (US remains the fallback + transform for words it lacks).
        // Same predicate as config_is_gb — bare "en" is British (cori).
        let gb_dict = if espeak_voice_is_gb(&cfg.espeak.voice) {
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
    fn quoted_words_are_spoken() {
        // Regression (found by the round-trip harness): "'last" classified as
        // punctuation because of its leading quote and was silently dropped.
        let dict: HashMap<String, String> = serde_json::from_slice(CMUDICT_BYTES).unwrap();
        let word_set: HashSet<String> = dict.keys().cloned().collect();
        let ph = EnglishPhonemizer::new_with_hashmap(dict);
        let quoted = phonemize(&ph, None, &word_set, "the 'last mile' stuff").unwrap();
        let plain = phonemize(&ph, None, &word_set, "the last mile stuff").unwrap();
        assert_eq!(quoted, plain);
    }

    // Build a phonemizer with the SAME dict the engine uses: CMUdict + overrides
    // + heteronym pseudo-keys. Without the PRONS insertion, "lives1"/"lives2"
    // would be OOV and the het path would silently no-op.
    fn engine_phonemizer() -> (EnglishPhonemizer, HashSet<String>) {
        let mut dict: HashMap<String, String> = serde_json::from_slice(CMUDICT_BYTES).unwrap();
        for (w, p) in PRONUNCIATION_OVERRIDES {
            dict.insert(w.to_string(), p.to_string());
        }
        for (k, p) in crate::heteronyms::PRONS {
            dict.insert(crate::heteronyms::dict_key(k), p.to_string());
        }
        let word_set: HashSet<String> = dict.keys().cloned().collect();
        (EnglishPhonemizer::new_with_hashmap(dict), word_set)
    }

    // Build the GB engine path exactly as load() does: US dict + overrides +
    // heteronyms feed the phonemizer; the separate GB dict + GB overrides win on
    // lookup, with the US->RP transform behind them. Returns the pieces phonemize
    // needs for the gb=Some(...) path.
    fn gb_engine() -> (EnglishPhonemizer, HashSet<String>, HashMap<String, String>) {
        let mut dict: HashMap<String, String> = serde_json::from_slice(CMUDICT_BYTES).unwrap();
        for (w, p) in PRONUNCIATION_OVERRIDES { dict.insert(w.to_string(), p.to_string()); }
        for (k, p) in crate::heteronyms::PRONS { dict.insert(crate::heteronyms::dict_key(k), p.to_string()); }
        let mut word_set: HashSet<String> = dict.keys().cloned().collect();
        let ph = EnglishPhonemizer::new_with_hashmap(dict);
        let mut gb: HashMap<String, String> = serde_json::from_slice(GB_DICT_BYTES).unwrap();
        for (w, ipa) in GB_PRONUNCIATION_OVERRIDES { gb.insert(w.to_string(), ipa.to_string()); }
        word_set.extend(gb.keys().cloned());
        (ph, word_set, gb)
    }

    // Diagnostic for triaging a batch of reported mispronunciations: prints the
    // GB + US phonemes each word resolves to so OOV-spelling vs bad-dict-entry vs
    // transform bugs can be told apart. Kept (ignored) for the next batch.
    #[test]
    #[ignore = "diagnostic probe; run with --ignored --nocapture"]
    fn probe_reported_words() {
        let (ph, ws, gb) = gb_engine();
        let words = std::env::var("PROBE_WORDS").unwrap_or_default();
        // ';' separator when present, so words containing commas ("+1,321") probe whole.
        let sep = if words.contains(';') { ';' } else { ',' };
        for w in words.split(sep) {
            if w.is_empty() { continue; }
            let norm = spoken_text(w);
            let gbp = phonemize(&ph, Some(&gb), &ws, &norm).unwrap().join("");
            let usp = phonemize(&ph, None, &ws, &norm).unwrap().join("");
            eprintln!("{w:<16} gb={gbp:<30} us={usp}");
        }
    }

    // Honorific abbreviations expand to words (the dict reads "Dr" as "drive").
    #[test]
    fn honorific_expansion() {
        assert_eq!(normalize_text("Dr. Hinton wrote"), "Doctor Hinton wrote");
        assert_eq!(normalize_text("Mr. and Mrs. Jones"), "Mister and Missus Jones");
        assert_eq!(normalize_text("Ms. Chen"), "Mizz Chen");
        assert_eq!(normalize_text("Prof. Lee"), "Professor Lee");
    }

    // Reported 2026-07-06: "TL;DR:" and "Q&A" each split across a separator
    // character that phonemize's tokenizer treats as pause punctuation (';'/
    // ':' for the first, dropping '&' outright for the second), spelling one
    // abbreviation as two disjoint letter-groups or losing a word entirely.
    // Gluing each into one placeholder word routes it through a single OOV/
    // override lookup instead (see PRONUNCIATION_OVERRIDES's "qanda" entry
    // and the OOV letter-spelling fallback for "TLDR").
    #[test]
    fn glued_abbreviation_expansions() {
        assert_eq!(normalize_text("TL;DR: too long"), "TLDR: too long");
        assert_eq!(normalize_text("TL;DR too long"), "TLDR too long");
        assert_eq!(normalize_text("a Q&A session"), "a QandA session");
        assert_eq!(normalize_text("a Q & A session"), "a QandA session");
    }

    // Reported 2026-07-06: "Transformers.js" read as two sentences ("...js"
    // is punctuation-piece machinery treats the bare '.' as a full stop).
    // Word-boundary gated so it doesn't fire inside a real "file.json"/
    // "app.jsx" extension.
    #[test]
    fn dotjs_suffix_glue() {
        assert_eq!(normalize_text("uses Transformers.js today"), "uses Transformers js today");
        assert_eq!(normalize_text("built with Node.js"), "built with Node js");
        assert_eq!(normalize_text("Transformers.js."), "Transformers js.");
        // Negative guards: a real file extension keeps its pause.
        assert_eq!(normalize_text("save objects.json"), "save objects.json");
        assert_eq!(normalize_text("open app.jsx"), "open app.jsx");
    }

    // Reported 2026-07-05: dashes read aloud with no audible pause. Em/en
    // dash, a "--" run, and a spaced ASCII hyphen used the same way all
    // rewrite to a clause-pause comma so they ride the CLAUSE_PAUSE_MS
    // machinery (see split_for_pauses / the punctuation-piece filter in
    // phonemize). Spacing is collapsed either way: glued or spaced input
    // both land on "word, word".
    #[test]
    fn dash_pause_rewrites() {
        assert_eq!(normalize_text("wait \u{2014} no"), "wait, no");
        assert_eq!(normalize_text("one\u{2014}two"), "one, two");
        assert_eq!(normalize_text("a \u{2013} b"), "a, b");
        assert_eq!(normalize_text("one\u{2013}two"), "one, two");
        assert_eq!(normalize_text("word -- word"), "word, word");
        assert_eq!(normalize_text("word--word"), "word, word");
        assert_eq!(normalize_text("word - word"), "word, word");
    }

    // Negative guards: shapes that must NOT be touched, so the dash rewrite
    // never corrupts a compound word or a digit range.
    #[test]
    fn dash_pause_negative_guards() {
        // Unspaced hyphen compounds are words, not dashes.
        assert_eq!(normalize_text("un-iterated"), "un-iterated");
        assert_eq!(normalize_text("well-known plan"), "well-known plan");
        // A digit-adjacent em/en dash is a range: becomes '-', matching how
        // an already-ASCII-hyphen range reads (see normalize_numbers).
        assert_eq!(normalize_text("2019\u{2013}2021"), "2019-2021");
        assert_eq!(normalize_text("2019\u{2014}2021"), "2019-2021");
        // A plain ASCII hyphen range is already '-' and stays that way.
        assert_eq!(normalize_text("2019-2021"), "2019-2021");
        // A spaced hyphen flanked by digits (not letters) is not this dash.
        assert_eq!(normalize_text("5 - 3"), "5 - 3");
        // A line-start hyphen is a list marker, not a dash: no letter
        // precedes the space before it.
        assert_eq!(normalize_text("Intro.\n- item one"), "Intro.\n- item one");
    }

    // Regression for the 2026-07-05 report batch: each word must reach real
    // phonemes on the GB voices (alba/jenny), not a letter-by-letter spelling or
    // a bad dict entry. Exact GB IPA pinned so a dict/override/transform change
    // that reverts one fails here. (The linking-r transform fix has its own unit
    // test in gb_english; this checks it through the full engine path.)
    #[test]
    fn reported_words_reach_phonemes_gb() {
        let (ph, ws, gb) = gb_engine();
        let say = |t: &str| phonemize(&ph, Some(&gb), &ws, &spoken_text(t)).unwrap().join("");
        // OOV words that used to spell out letter-by-letter.
        assert_eq!(say("requeuing"), "ɹiːkjˈuːɪŋ");
        assert_eq!(say("homunculi"), "həʊmˈʌŋkjəlaɪ");
        assert_eq!(say("async"), "ˈeɪsɪŋk");
        assert_eq!(say("VSCode"), "vˈiːˈɛskˈəʊd");
        assert_eq!(say("IDs"), "ˈaɪdˈiːz");
        // Bad GB dict entry, fixed via GB override.
        assert_eq!(say("We're"), "wˈiə");
        // Honorific expansion reaches "Doctor", not "Drive".
        assert_eq!(say("Dr. Hinton"), "dˈɒktɐ hˈɪntən");
        // Linking-r class fix, through the engine (was dʒˈɛnəˌeɪtəd / sˈɛpəət).
        assert_eq!(say("generated"), "dʒˈɛnəɹˌeɪtəd");
        assert_eq!(say("separate"), "sˈɛpəɹət");
        // Heteronym disambiguation for "separate" still holds after the r fix.
        assert_eq!(say("to separate the files"), "tuː sˈɛpəɹˌeɪt ðɐ fˈaɪlz");

        // 2026-07-05 second batch. Exact strings pinned from the probe (never
        // hand-derived): PROBE_WORDS="iPhone;GitHub's;xhigh;SQL;upsert;
        // upserting;iterator;iterated;un-iterated;lazily;+1,321;4.0a1;-40;
        // NotImplementedError;PrimaryKeyRequired;refuses;raises;UN;the UN said"
        // OOV brand/tech terms, now via PRONUNCIATION_OVERRIDES.
        assert_eq!(say("iPhone"), "ˈaɪfˌəʊn");
        assert_eq!(say("xhigh"), "ˈɛkshˌaɪ");
        assert_eq!(say("SQL"), "ˈɛskjˈuːˈɛl");
        // Updated 2026-07-12 by the stressed-ɚ NURSE fix: ER2 now folds to
        // ɜː ("up-SURT"), not a weak ə ("up-suht").
        assert_eq!(say("upsert"), "ˈʌpsˌɜːt");
        assert_eq!(say("upserting"), "ˈʌpsˌɜːtɪŋ");
        assert_eq!(say("iterator"), "ˈɪtəɹˌeɪtɐ");
        assert_eq!(say("iterated"), "ˈɪtəɹˌeɪtɪd");
        assert_eq!(say("lazily"), "lˈeɪzəlɪ");
        // Possessive of an overridden OOV base, plus /z/.
        assert_eq!(say("GitHub's"), "ɡˈɪthˌʌbz");
        // Possessive-compound fallback (no override involved): "pushbuffer"
        // has none (it relies on the plain compound split, per the comment on
        // PRONUNCIATION_OVERRIDES), so this exercises compound_join purely
        // through the possessive branch added in this batch. Verified by
        // temporarily removing the github override that "GitHub's" would
        // otherwise also resolve through the compound path (git + hub) alone.
        assert_eq!(say("pushbuffer's"), "pˈʊʃbˌʌfɐz");
        // GB "un" override fixes the hyphen-split prefix.
        assert_eq!(say("un-iterated"), "ʌn ˈɪtəɹˌeɪtɪd");
        // normalize_numbers: signed/comma-grouped numbers and version suffixes,
        // through the full engine path.
        assert_eq!(say("+1,321"), "plˈʌs wˈɒn θˈaʊzənd θɹˈiː hˈʌndɹəd twˈɛntɪ wˈɒn");
        assert_eq!(say("4.0a1"), "fˈɔː pˈɔɪnt zˈiəɹəʊ ˈælfɐ wˈɒn");
        // camelCase/PascalCase identifier fallback.
        assert_eq!(say("NotImplementedError"), "nɒt ˈɪmpləmˌɛntəd ˈɛɹɐ");
        assert_eq!(say("PrimaryKeyRequired"), "pɹˈaɪmˌəɹɪ kˈiː ɹɪkwˈaɪəd");
        // Checked-correct guards: reported, but already right on GB — do not
        // "fix" these if a future change makes them regress.
        assert_eq!(say("refuses"), "ɹəfjˈuːzəz");
        assert_eq!(say("raises"), "ɹˈeɪzəz");
        // The GB "un" override must never leak into the all-caps acronym path:
        // uppercase "UN" still spells letter-by-letter, standalone and in a
        // sentence.
        assert_eq!(say("UN"), "juːɛn");
        assert_eq!(say("the UN said"), "ðɐ juːɛn sˈɛd");

        // 2026-07-06 batch. Exact strings pinned from the probe (never
        // hand-derived): PROBE_WORDS="Inpainting,PyTorch,Transformers.js,
        // ~1.3GB,amount,Resize,Einsum,MatMul,Conv,initializers,AWS,Mozilla,
        // Mozilla's,Q&A" (TL;DR probed separately: its own ';' collides with
        // the probe's list separator).
        // OOV brand/tech terms and compounds, now via PRONUNCIATION_OVERRIDES.
        assert_eq!(say("Inpainting"), "ɪnpˈeɪntɪŋ");
        assert_eq!(say("PyTorch"), "pˈaɪtˈɔːtʃ");
        assert_eq!(say("Resize"), "ɹiːsˈaɪz");
        assert_eq!(say("MatMul"), "mˈætmˈʌl");
        assert_eq!(say("Conv"), "kˈɒnv");
        assert_eq!(say("initializers"), "ɪnˈɪʃəlˌaɪzəz");
        assert_eq!(say("Mozilla"), "məzˈɪlɐ");
        // Possessive of an overridden OOV base resolves automatically (no
        // separate "mozilla's" override needed, per the possessive fallback).
        assert_eq!(say("Mozilla's"), "məzˈɪlɐz");
        // "AWS" isn't OOV to the phonemizer (it guesses a rhyme-by-analogy
        // reading, like "jaws"), so it needed SPELLED_ACRONYMS, not an override.
        assert_eq!(say("AWS"), "eɪdʌbəljuːɛs");
        // ".js" glue survives the full engine path: "Transformers" phonemizes
        // normally, "js" spells as letters, no mid-word sentence pause.
        assert_eq!(say("Transformers.js"), "tɹænsfˈɔːməz dʒeɪɛs");
        // "TL;DR" glue: too short for the compound splitter (SEGMENT_MIN_PIECE
        // needs 3+ chars/half), so it falls all the way to letter spelling.
        assert_eq!(say("TL;DR"), "tiːɛldiːɑː");
        assert_eq!(say("TL;DR:"), "tiːɛldiːɑː:");
        // "Q&A" glue resolves through the "qanda" override: Q, "and", A.
        assert_eq!(say("Q&A"), "kjˈuːəndˈeɪ");
        // normalize_numbers: '~' + byte-size unit, through the full engine path.
        assert_eq!(say("~1.3GB"), "əbaʊt wˈɒn pˈɔɪnt θɹˈiː ɡˈɪɡəbˌaɪts");
        // Checked-correct guards: reported, but already right on GB — do not
        // "fix" these if a future change makes them regress. "Einsum" resolves
        // through the phonemizer's own letter-to-sound guess, not our letter
        // spelling or dict path.
        assert_eq!(say("amount"), "əmˈaʊnt");
        assert_eq!(say("Einsum"), "ˈaɪnsˌʌm");
    }

    // Reported 2026-07-05: dashes produced no audible pause. Confirms the
    // clause-pause comma from normalize_text's dash rewrite survives the full
    // engine path (not just normalize_text in isolation), through the same
    // GB voice path the report came in on.
    #[test]
    fn dash_pause_reaches_phonemes_gb() {
        let (ph, ws, gb) = gb_engine();
        let say = |t: &str| phonemize(&ph, Some(&gb), &ws, &spoken_text(t)).unwrap().join("");
        // Plain ASCII hyphen and "--" used as a spoken dash: previously
        // silent (bare '-' isn't in phonemize's kept-punctuation set), now a
        // comma.
        assert_eq!(say("wait - no"), "wˈeɪt, nəʊ");
        assert_eq!(say("word -- word"), "wˈɜːd, wˈɜːd");
        // Digit-adjacent en dash is a range, not a pause: two plain cardinals
        // back to back, same as an ASCII hyphen range already reads (no
        // comma, and no longer fused into one garbled number via
        // normalize_numbers' thousands-separator comma handling).
        assert_eq!(say("2019\u{2013}2021"), "twˈɛntɪ nˈaɪntˈiːn twˈɛntɪ twˈɛntɪ wˈɒn");
        // Unspaced hyphen compound: still one phrase, no pause.
        assert_eq!(say("un-iterated"), "ʌn ˈɪtəɹˌeɪtɪd");
    }

    #[test]
    fn heteronym_lives_verb_reaches_phonemes() {
        let (ph, ws) = engine_phonemizer();
        // Ground-truth readings via the dict_key() forms (raw "lives1" would be
        // digit-stripped to "lives" by the tokenizer).
        let verb = phonemize(&ph, None, &ws, &crate::heteronyms::dict_key("lives1")).unwrap();
        let noun = phonemize(&ph, None, &ws, &crate::heteronyms::dict_key("lives2")).unwrap();
        assert_ne!(verb, noun, "verb/noun pseudo-keys must phonemize differently");
        assert!(!verb.contains(&"a".to_string()),
                "verb 'lives' should be /lɪvz/ (no diphthong), got {verb:?}");
        // The phrase ends in "lives"; its trailing tokens must be the VERB
        // reading (Storybook resides), not the noun (plural of life).
        let phrase = phonemize(&ph, None, &ws, "and where storybook lives").unwrap();
        assert_eq!(&phrase[phrase.len() - verb.len()..], &verb[..],
                   "'Storybook lives' must use the verb reading; got {phrase:?}");
    }

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
        // Bare 4-digit numbers read as years.
        assert_eq!(normalize_numbers("in 2026."), "in twenty twenty six.");
        assert_eq!(normalize_numbers("since 1980"), "since nineteen eighty");
        assert_eq!(normalize_numbers("in 1905"), "in nineteen oh five");
        assert_eq!(normalize_numbers("in 2007"), "in two thousand seven");
        // Magnitude suffixes.
        assert_eq!(normalize_numbers("$10B fund"), "ten billion dollars fund");
        assert_eq!(normalize_numbers("a $1.5M house"), "a one point five million dollars house");
        assert_eq!(normalize_numbers("200k users"), "two hundred thousand users");
        // A glued letter separates instead of fusing ("10x" -> "ten x").
        assert_eq!(normalize_numbers("10x faster"), "ten x faster");
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

    // Reported 2026-07-05 (alba), second batch: signed numbers, comma-grouped
    // numbers, and pre-release version suffixes.
    #[test]
    fn normalize_signs_and_prerelease() {
        // Comma-grouped and signed numbers are never read as years.
        assert_eq!(normalize_numbers("+1,321"), "plus one thousand three hundred twenty one");
        assert_eq!(normalize_numbers("-40"), "minus forty");
        assert_eq!(
            normalize_numbers("up +1,321 today"),
            "up plus one thousand three hundred twenty one today"
        );
        // Mid-token hyphen (range): '-' is preceded by '9', not a boundary, so
        // it is untouched passthrough, same as before this change. (The bare
        // hyphen carries no phoneme once split_tokens/phonemize see it, so
        // this is inaudible either way — see reported_words_reach_phonemes_gb.)
        assert_eq!(normalize_numbers("2019-2021"), "twenty nineteen-twenty twenty one");
        // Pre-release version suffixes.
        assert_eq!(normalize_numbers("4.0a1"), "four point zero alpha one");
        assert_eq!(normalize_numbers("1.0rc2"), "one point zero release candidate two");
        // Non-matches fall through unchanged.
        assert_eq!(normalize_numbers("3.5ab"), "three point five ab");
        assert_eq!(normalize_numbers("2.0abc1"), "two point zero abc one");
        assert_eq!(normalize_numbers("4.0a"), "four point zero a");
        // Regression: pre-existing number handling still holds.
        assert_eq!(normalize_numbers("in 2026."), "in twenty twenty six.");
        assert_eq!(normalize_numbers("since 1980"), "since nineteen eighty");
        assert_eq!(
            normalize_numbers("$1,234.50"),
            "one thousand two hundred thirty four dollars and fifty cents"
        );
        assert_eq!(normalize_numbers("200k users"), "two hundred thousand users");
    }

    // Reported 2026-07-06: "~1.3GB" left '~' silently dropped (not in
    // phonemize's kept-punctuation set) and "GB" spelled out as letters
    // ("G-B") since a bare two-letter unit never matched the single-letter
    // magnitude suffix (which requires nothing alphanumeric right after it).
    #[test]
    fn byte_units_and_about() {
        // Byte-size units speak the full word; plural except exactly one.
        assert_eq!(normalize_numbers("~1.3GB"), "about one point three gigabytes");
        assert_eq!(normalize_numbers("1GB"), "one gigabyte");
        assert_eq!(normalize_numbers("10MB file"), "ten megabytes file");
        assert_eq!(normalize_numbers("200KB"), "two hundred kilobytes");
        assert_eq!(normalize_numbers("2TB drive"), "two terabytes drive");
        assert_eq!(normalize_numbers("3PB"), "three petabytes");
        // Lowercase/mixed-case units are left alone (avoids "10m" prose
        // metres and millibar/tuberculosis-style abbreviation collisions).
        assert_eq!(normalize_numbers("10mb"), "ten mb");
        // A unit-looking suffix with more letters after it (currency code,
        // not a byte unit) is not consumed.
        assert_eq!(normalize_numbers("GBP 10"), "GBP ten");
        // '~' reads "about" only at a left boundary immediately before a
        // digit; mid-token/path use is untouched.
        assert_eq!(normalize_numbers("~40 items"), "about forty items");
        assert_eq!(normalize_numbers("(~5 min)"), "(about five min)");
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

    // Regression: every voice's model file is named "model_dur.onnx" and many
    // are byte-identical in size, so the fingerprint MUST fold in the per-voice
    // parent dir — otherwise their TTS caches collide and voices play each
    // other's cached segments. (Paths need not exist; missing size is 0 for
    // both, isolating the dir as the distinguishing factor.)
    #[test]
    fn fingerprint_distinguishes_voices_in_different_dirs() {
        let alba = model_fingerprint("/m/tts/piper-alba/model_dur.onnx");
        let alan = model_fingerprint("/m/tts/piper-alan/model_dur.onnx");
        assert_ne!(alba, alan, "same basename+size in different dirs must differ");
        assert!(alba.contains("piper-alba"));
        assert!(alan.contains("piper-alan"));
    }

    // Regression: the trim path must walk the exact same segments
    // cache_coverage counts, and must be a no-op (not a panic) when nothing was
    // ever cached for this text.
    #[test]
    fn forget_cached_segments_misses_are_zero() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("model_dur.onnx.json");
        std::fs::write(&config_path, r#"{"audio":{"sample_rate":22050},"phoneme_id_map":{}}"#).unwrap();
        let model_path = dir.path().join("piper-test/model_dur.onnx");
        let model_path = model_path.to_str().unwrap();
        let config_path = config_path.to_str().unwrap();
        let text = "Hello, world. This is a cold cache test, with a clause too.";

        let forgotten = forget_cached_segments(model_path, config_path, 0, 1.0, text);
        assert_eq!(forgotten, 0, "nothing was ever cached, so nothing can be forgotten");

        // Same split cache_coverage counts, so a real trim walks exactly the
        // segments it reports as (un)cached.
        let clean = text.replace('\u{0}', " ");
        let chunks = crate::tts::batch_sentences(crate::tts::split_sentences(&clean), 15, 45);
        let walked = chunks.iter().flat_map(|c| split_for_pauses(c)).count() as u32;
        let cov = cache_coverage(model_path, config_path, 0, 1.0, text);
        assert_eq!(walked, cov.total_segments);
    }

    // 2026-07-11 batch, normalize-level fixes. Boundary-gated honorifics: the
    // old bare substring replace turned "LLMs." into "LLMizz" (spelled
    // L-L-M-I-Z-Z).
    #[test]
    fn honorific_needs_word_boundary() {
        assert_eq!(normalize_text("modern LLMs."), "modern LLMs.");
        assert_eq!(normalize_text("PhDs. later"), "PhDs. later");
        // Still expands at a real boundary, including after punctuation.
        assert_eq!(normalize_text("(Dr. Smith)"), "(Doctor Smith)");
        assert_eq!(normalize_text("said Ms. Chen"), "said Mizz Chen");
    }

    // Glued '+' speaks ("TLA+" read as silence); left-boundary '+' is left
    // for normalize_numbers' sign handling; grade-A backpatch.
    #[test]
    fn plus_speaks() {
        assert_eq!(normalize_text("uses TLA+ today"), "uses TLA plus today");
        assert_eq!(normalize_text("C++ code"), "C plus plus code");
        assert_eq!(normalize_text("1+1"), "1 plus 1");
        assert_eq!(normalize_text("+5 today"), "+5 today");
        // "A+" is the letter grade, not the article.
        assert_eq!(normalize_text("got an A+ grade"), "got an ay plus grade");
        // Prose "A plus" (no glued '+') keeps its article.
        assert_eq!(normalize_text("A plus point"), "A plus point");
    }

    // All-caps letter+digit acronyms split into letters and digit runs so
    // "C2PA" stops reading "see two PAH" (dict word "pa") and "ES5" stops
    // losing its E ("ess five").
    #[test]
    fn caps_digit_acronyms_split() {
        assert_eq!(normalize_text("the C2PA spec"), "the C 2 P ay spec");
        assert_eq!(normalize_text("ES5 output"), "E S 5 output");
        assert_eq!(normalize_text("an MP3 file"), "an M P 3 file");
        assert_eq!(normalize_text("EC2 hosts"), "E C 2 hosts");
        assert_eq!(normalize_text("a B52 flyover"), "a B 52 flyover");
        // Guards: digit-first tokens are unit-suffixed numbers, and a 4+
        // letter run is a word-acronym the dict path reads better.
        assert_eq!(normalize_text("takes 100ms"), "takes 100ms");
        assert_eq!(normalize_text("8GB of RAM"), "8GB of RAM");
        assert_eq!(normalize_text("COVID19 era"), "COVID19 era");
        assert_eq!(normalize_text("HTML5 apps"), "HTML5 apps");
        // Mixed case never splits.
        assert_eq!(normalize_text("iPhone15 Pro"), "iPhone15 Pro");
    }

    // A standalone capital "A" after a trigger word is the letter ("Plan A"
    // read "plan uh"); sentence punctuation resets the trigger so a genuine
    // article survives.
    #[test]
    fn letter_a_after_trigger_words() {
        assert_eq!(normalize_text("Plan A failed"), "Plan ay failed");
        assert_eq!(normalize_text("a Type A person"), "a Type ay person");
        assert_eq!(normalize_text("an A grade"), "an ay grade");
        assert_eq!(normalize_text("the A team"), "the ay team");
        assert_eq!(normalize_text("the plan. A new model"), "the plan. A new model");
        // Lowercase "a" is always the article, triggers or not.
        assert_eq!(normalize_text("plan a meeting"), "plan a meeting");
    }

    // "VS Code" brand rewrite; lowercase "vs" keeps "versus".
    #[test]
    fn vs_code_brand() {
        assert_eq!(normalize_text("open VS Code now"), "open V S Code now");
        assert_eq!(normalize_text("cats vs dogs"), "cats vs dogs");
    }

    // Reported 2026-07-11 ("100ms" read "one hundred M's"): time/frequency
    // unit suffixes, same shape as the byte units.
    #[test]
    fn time_and_frequency_units() {
        assert_eq!(normalize_numbers("takes 100ms"), "takes one hundred milliseconds");
        assert_eq!(normalize_numbers("1ms tick"), "one millisecond tick");
        assert_eq!(normalize_numbers("50ns gate"), "fifty nanoseconds gate");
        assert_eq!(normalize_numbers("5GHz band"), "five gigahertz band");
        assert_eq!(normalize_numbers("800MHz bus"), "eight hundred megahertz bus");
        assert_eq!(normalize_numbers("44.1kHz audio"), "forty four point one kilohertz audio");
        assert_eq!(normalize_numbers("60Hz refresh"), "sixty hertz refresh");
        // Uppercase "MS" is an initialism, not milliseconds.
        assert_eq!(normalize_numbers("100MS"), "one hundred MS");
        // A unit-looking suffix with more letters after it is not consumed.
        assert_eq!(normalize_numbers("100mspaint"), "one hundred mspaint");
    }

    // 2026-07-11 report batch, full engine path on GB. Exact strings pinned
    // from the probe (never hand-derived). Covers: the cori "en"-voice GB
    // gating fix (these all assume the GB dict path), the ʌɪ->aɪ and
    // single-letter GB dict data patches, us_to_rp_spelled (final letter-name
    // FLEECE), stressed-ɚ NURSE, the aʊɹ centring fix, possessive phonology,
    // and the PRONUNCIATION_OVERRIDES additions.
    #[test]
    fn reported_words_2026_07_11_gb() {
        let (ph, ws, gb) = gb_engine();
        let say = |t: &str| phonemize(&ph, Some(&gb), &ws, &spoken_text(t)).unwrap().join("");
        // OOV tech terms / brands, now via overrides.
        assert_eq!(say("geoguessr"), "dʒˈiːəʊɡˌɛsɐ");
        assert_eq!(say("Luddism"), "lˈʌdˌɪzəm");
        assert_eq!(say("unspoofable"), "ənspˈuːfəbəl");
        assert_eq!(say("unforgeable"), "ənfˈɔːdʒəbəl");
        assert_eq!(say("validators"), "vˈælədˌeɪtəz");
        assert_eq!(say("evaluators"), "ɪvˈæljuːˌeɪtəz");
        assert_eq!(say("Optimizer"), "ˈɒptəmˌaɪzɐ");
        assert_eq!(say("reformat"), "ɹˌiːfˈɔːmˌæt");
        assert_eq!(say("reimplemented"), "ɹˌiːˈɪmpləmˌɛntəd");
        assert_eq!(say("reranker"), "ɹiːɹˈæŋkɐ");
        assert_eq!(say("contextually"), "kəntˈɛkstʃuːəlɪ");
        assert_eq!(say("gamification"), "ɡˌeɪməfɪkˈeɪʃən");
        assert_eq!(say("parallelization"), "pˌɛɹəlɛlɪzˈeɪʃən");
        assert_eq!(say("writeup"), "ɹˈaɪtˌʌp");
        assert_eq!(say("managable"), "mˈænɪdʒəbəl");
        assert_eq!(say("Pareto"), "pəɹˈeɪtəʊ");
        assert_eq!(say("Canva"), "kˈænvɐ");
        assert_eq!(say("Ars Technica"), "ˈɑːz tˈɛknɪkɐ");
        assert_eq!(say("Auggie"), "ˈɔːɡɪ");
        assert_eq!(say("Junie"), "dʒˈuːnɪ");
        assert_eq!(say("Qoder"), "kˈəʊdɐ");
        assert_eq!(say("Vybe"), "vˈaɪb");
        assert_eq!(say("VRAM"), "vˈiːɹˈæm");
        assert_eq!(say("sci-fi"), "sˈaɪ fˈaɪ");
        // The Postgres family.
        assert_eq!(say("PostgreSQL"), "pˈəʊstɡɹˌɛskjˌuːˌɛl");
        assert_eq!(say("PgDog"), "pˈiːdʒˈiːdˈɒɡ");
        assert_eq!(say("PgBouncer"), "pˈiːdʒˈiːbˈaʊnsɐ");
        assert_eq!(say("pgvector"), "pˈiːdʒˈiːvˈɛktɐ");
        assert_eq!(say("tsvector"), "tˈiːˈɛsvˈɛktɐ");
        assert_eq!(say("pg_trgm"), "piːdʒiː tɹˈaɪɡɹˌæm");
        assert_eq!(say("JSONB"), "dʒˈeɪsənbˈiː");
        assert_eq!(say("MongoDB"), "mˈɒŋɡəʊdˈiːbˈiː");
        assert_eq!(say("TimescaleDB"), "tˈaɪmskˌeɪldˈiːbˈiː");
        assert_eq!(say("DuckDB"), "dˈʌkdˈiːbˈiː");
        assert_eq!(say("CTEs"), "sˈiːtˈiːˈiːz");
        // Bad GB dict entries, fixed via GB overrides.
        assert_eq!(say("misogynist"), "mɪsˈɒdʒɪnɪst");
        assert_eq!(say("misogyny"), "mɪsˈɒdʒɪnɪ");
        assert_eq!(say("Jacobian"), "dʒəkˈəʊbiən");
        // GB dict data patches: ʌɪ->aɪ (quantized) and single letters (Plan L).
        assert_eq!(say("quantized"), "kwˈɒntaɪzd");
        assert_eq!(say("quantization"), "kwˌɒntəzˈeɪʃən");
        assert_eq!(say("Plan L"), "plˈæn ˈɛl");
        assert_eq!(say("Plan A"), "plˈæn ˈeɪ");
        // Acronym plural/possessive with an "llm" override + boundary-gated
        // honorifics ("LLMs." no longer becomes "LLMizz").
        assert_eq!(say("LLMs"), "ˈɛlˈɛlˈɛmz");
        assert_eq!(say("LLMs."), "ˈɛlˈɛlˈɛmz.");
        assert_eq!(say("LLM\u{2019}s"), "ˈɛlˈɛlˈɛmz");
        // Possessive phonology (/ɪz/ after sibilants, /s/ after voiceless).
        assert_eq!(say("Postgres's"), "pˈəʊstɡɹˌɛsɪz");
        assert_eq!(say("Goldratt's"), "ɡˈəʊldɹˌæts");
        // aʊɹ keeps its centring vowel (was "naɔːz").
        assert_eq!(say("Peter Naur\u{2019}s"), "pˈiːtɐ nˈaʊəz");
        // Spelled letter names keep the final FLEECE (was see-vee-ih).
        assert_eq!(say("CVE"), "siːviːiː");
        assert_eq!(say("ABCs"), "eɪbiːsiːz");
        assert_eq!(say("SWE-Bench"), "ɛsdʌbəljuːiː bˈɛntʃ");
        // normalize-level fixes through the full path.
        assert_eq!(say("C2PA"), "sˈiː tˈu pˈiː ˈeɪ");
        assert_eq!(say("ES5"), "ˈiː ˈɛs fˈaɪv");
        assert_eq!(say("TLA+"), "tiːɛleɪ plˈʌs");
        assert_eq!(say("C++"), "sˈiː plˈʌs plˈʌs");
        assert_eq!(say("A+ grade"), "ˈeɪ plˈʌs ɡɹˈeɪd");
        assert_eq!(say("VS Code"), "vˈiː ˈɛs kˈəʊd");
        assert_eq!(say("100ms"), "wˈɒn hˈʌndɹəd mˈɪlɪsˌɛkəndz");
        assert_eq!(say("5GHz"), "fˈaɪv ɡˈɪɡəhˌɜːts");
        assert_eq!(say("the plan. A new model"), "ðɐ plˈæn. ɐ njˈuː mˈɒdəl");
        assert_eq!(say("COVID19"), "kˈɒvɪd nˈaɪntˈiːn");
        assert_eq!(say("x86"), "ˈɛks ˈeɪtɪ sˈɪks");
        assert_eq!(say("3D printing"), "θɹˈiː dˈiː pɹˈɪntɪŋ");
        // Checked-correct guards: reported (cori), right at the phoneme level
        // once the "en" espeak voice takes the GB path — do not "fix" later.
        assert_eq!(say("parameter"), "pəɹˈæmɪtɐ");
        assert_eq!(say("solar"), "sˈəʊlɐ");
        assert_eq!(say("successes"), "səksˈɛsəz");
        assert_eq!(say("popups"), "pˈɒpˌʌps");
        assert_eq!(say("Amp"), "ˈæmp");
        assert_eq!(say("Cognition"), "kɒɡnˈɪʃən");
    }

    // The cache/loader GB predicate: bare "en" is the British base voice
    // (cori). The loader and config_is_gb disagreed on this until 2026-07-12,
    // which ran cori on raw US phonemes with no GB dictionary.
    #[test]
    fn bare_en_is_gb() {
        assert!(espeak_voice_is_gb("en-gb-x-rp"));
        assert!(espeak_voice_is_gb("en-gb"));
        assert!(espeak_voice_is_gb("en"));
        assert!(!espeak_voice_is_gb("en-us"));
        assert!(!espeak_voice_is_gb(""));
    }

    // Regression: grammar_neural commits the ORT environment first (during
    // warm_up), so by the time TTS plays, ort::init().commit() returns false
    // ("already configured"). ensure_ort_init must NOT treat that as an error,
    // and a real session must still build against the shared environment.
    //
    // 2026-07-13: ignored — ort::init().commit() deadlocks (0% CPU, forever)
    // inside ONNX Runtime env creation on the Linux dev box, hanging the whole
    // suite. Reproduces at HEAD with no working-tree changes, so it is the
    // environment (likely the documented dual-runtime fragility: sherpa's
    // static libonnxruntime.a + the ort crate's dylib in one process), not a
    // code regression. The guard it provides is exercised by any real
    // grammar+TTS session. Run manually with --ignored when touching ORT init.
    #[test]
    #[ignore = "ort env init deadlocks on this machine; see comment"]
    fn ort_init_survives_prior_commit() {
        // Simulate another subsystem winning the commit race.
        let _ = ort::init().commit();
        // Our init must succeed even though the env was already configured.
        ensure_ort_init().expect("ensure_ort_init after prior commit");
        // And a session must still be creatable in the shared environment.
        let onnx = concat!(env!("CARGO_MANIFEST_DIR"), "/data/grammar/encoder_model_quantized.0.0.1.onnx");
        if std::path::Path::new(onnx).exists() {
            let cpu = vec![ort::ep::CPUExecutionProvider::default().build()];
            Session::builder()
                .unwrap()
                .with_execution_providers(&cpu)
                .unwrap()
                .commit_from_file(onnx)
                .expect("session build in shared ORT env");
        }
    }
}

