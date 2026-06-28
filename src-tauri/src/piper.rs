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

use std::collections::HashMap;
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

/// Silence inserted after punctuation / line breaks, in ms. This voice's own
/// punctuation pause is negligible, so segments are synthesized separately and
/// joined with real silence. Paragraph > sentence > clause. Tune to taste.
const PARAGRAPH_PAUSE_MS: usize = 800; // newline / blank line
const SENTENCE_PAUSE_MS: usize = 500; // . ! ?
const CLAUSE_PAUSE_MS: usize = 250; // , ; :

/// Phonemize whole text to a single IPA token stream with " " word separators.
/// OOV words are spelled letter-by-letter so they still produce audio.
fn phonemize(ph: &EnglishPhonemizer, text: &str) -> Result<Vec<String>, String> {
    let pieces = split_tokens(text);
    let mut tokens: Vec<String> = Vec::new();
    let mut first = true;

    for piece in &pieces {
        let is_word = piece.chars().next().map(|c| c.is_alphanumeric()).unwrap_or(false);

        let mut p_tokens: Vec<String> = if is_word {
            let (mut pt, _prosody) = ph
                .phonemize_with_prosody(piece)
                .map_err(|e| format!("phonemize: {e}"))?;
            while pt.first().map(|s| s == " ").unwrap_or(false) {
                pt.remove(0);
            }
            while pt.last().map(|s| s == " ").unwrap_or(false) {
                pt.pop();
            }
            if is_oov(&pt) {
                let spelled = spell_word(piece);
                if !spelled.is_empty() {
                    pt = spelled;
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
}

fn split_for_pauses(text: &str) -> Vec<(String, usize)> {
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
        match pause_of(ch) {
            None => {
                cur.push(ch);
                i += 1;
            }
            Some(mut pause) => {
                // Keep a spoken punctuation mark in the segment (for intonation);
                // newlines are whitespace, so don't. Then consume the rest of the
                // break run, taking the longest pause, so "...", "?!", "\n\n" and
                // trailing spaces collapse into a single gap instead of stacking.
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
        }
    }
    if !cur.trim().is_empty() {
        out.push((cur, 0));
    }
    out
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
    sample_rate: i32,
    num_speakers: i64,
    noise_scale: f32,
    length_scale: f32,
    noise_w: f32,
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
        let encoder = PiperEncoder::new(id_map, UnknownTokenMode::Skip)
            .map_err(|e| format!("piper encoder: {e}"))?;

        let dict: HashMap<String, String> = serde_json::from_slice(CMUDICT_BYTES)
            .map_err(|e| format!("parse bundled cmudict: {e}"))?;
        let phonemizer = EnglishPhonemizer::new_with_hashmap(dict);

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

        Ok(Self {
            session,
            phonemizer,
            encoder,
            sample_rate: cfg.audio.sample_rate as i32,
            num_speakers: cfg.num_speakers,
            noise_scale: cfg.inference.noise_scale,
            length_scale: cfg.inference.length_scale,
            noise_w: cfg.inference.noise_w,
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
    fn infer(&mut self, ids: &[i64], speaker: i64, length_scale: f32) -> Result<Vec<f32>, String> {
        let n = ids.len();
        let noise_scale = self.noise_scale;
        let noise_w = self.noise_w;
        let session = &mut self.session;
        let ids = ids.to_vec();

        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<Vec<f32>, String> {
            let input = Tensor::from_array(([1_usize, n], ids))
                .map_err(|e| format!("input tensor: {e}"))?;
            let input_lengths = Tensor::from_array(([1_usize], vec![n as i64]))
                .map_err(|e| format!("input_lengths tensor: {e}"))?;
            let scales = Tensor::from_array(([3_usize], vec![noise_scale, length_scale, noise_w]))
                .map_err(|e| format!("scales tensor: {e}"))?;
            let sid = Tensor::from_array(([1_usize], vec![speaker]))
                .map_err(|e| format!("sid tensor: {e}"))?;

            let outputs = session
                .run(ort::inputs! {
                    "input" => input,
                    "input_lengths" => input_lengths,
                    "scales" => scales,
                    "sid" => sid,
                })
                .map_err(|e| format!("piper run: {e}"))?;

            // Output is f32 with raw shape (1, 1, 1, T); flatten to [T].
            let (_shape, data) = outputs["output"]
                .try_extract_tensor::<f32>()
                .map_err(|e| format!("extract output: {e}"))?;
            Ok(data.to_vec())
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
        let mut out: Vec<f32> = Vec::new();
        let mut spans: Vec<SegmentSpan> = Vec::new();
        // Split the RAW text (so segment word counts match the UI's tokens), then
        // normalize each segment for synthesis. Each segment is synthesized
        // separately and joined with real silence. Returning per-segment speech
        // timing (excluding the pause) lets the reading view highlight track
        // speech and treat the inserted pauses as gaps, not part of a word.
        for (segment, pause_ms) in split_for_pauses(text) {
            let normalized = normalize_numbers(&normalize_text(&segment));
            let tokens = phonemize(&self.phonemizer, &normalized)?;
            let ids = self
                .encoder
                .encode(&tokens)
                .map_err(|e| format!("encode: {e}"))?;
            let speech_start = out.len();
            if !ids.is_empty() {
                let pcm = self.infer(&ids, speaker, length_scale)?;
                out.extend_from_slice(&pcm);
            }
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
            });
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
}

