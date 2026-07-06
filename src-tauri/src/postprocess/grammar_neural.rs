//! Stage 4 (neural path): grammar router + corrector.
//!
//! Model files are loaded at runtime from the downloaded dictation package
//! (see MODEL_PACKAGES.md), under the platform models directory as
//! `models/grammar/<version>/...`. `crate::models::ModelManager::grammar_files`
//! resolves the seven files and returns `None` unless every one is present
//! on disk. Until the package is downloaded — or if any file is missing —
//! the stage is a silent no-op, exactly like a not-yet-downloaded voice.
//!
//! `init_global` runs on every pipeline entry (`postprocess::warm_up` and a
//! background nudge after a package install) and is cheap on the absent
//! path (a handful of fs metadata checks), so the very first pipeline run
//! after the download finishes picks up grammar correction with no restart.
//!
//! Model-specific parameters (input prefix, encoder output name, thresholds)
//! are read from the package's `config.json` at load time. To swap models,
//! ship new ONNX/tokenizer files and an updated config.json in the package.
//!
//! The seven files consumed (see `models::GrammarFilePaths`):
//!   cola_model_quantized.onnx          - grammar router model (CoLA classifier)
//!   cola_tokenizer.json                - grammar router tokenizer
//!   encoder_model_quantized.onnx       - corrector encoder
//!   decoder_with_past_quantized.onnx   - corrector decoder with KV cache
//!   cross_attn_kv_weights.bin          - cross-attention K/V projection weights (8x [256,256] f32)
//!   t5_tokenizer.json                  - corrector tokenizer
//!   config.json                        - model parameters (prefix, thresholds, etc.)

/// Per-sentence routing and correction result, stored in pipeline history.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SentenceResult {
    pub text: String,
    pub score: Option<f32>,
    pub corrected: bool,
    /// True if the guard reverted some or all corrector edits (negation
    /// flip or contraction drop).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub guarded: bool,
}

mod neural {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Mutex, OnceLock};

    use ndarray::{s, Array2, ArrayD};
    use ort::inputs;
    use ort::session::Session;
    use ort::value::TensorRef;
    use tokenizers::Tokenizer;

    /// Runtime-configurable parameters loaded from config.json.
    #[derive(serde::Deserialize)]
    struct Config {
        router: RouterConfig,
        corrector: CorrectorConfig,
    }

    #[derive(serde::Deserialize)]
    struct RouterConfig {
        threshold: f32,
    }

    #[derive(serde::Deserialize)]
    struct CorrectorConfig {
        #[serde(default)]
        input_prefix: String,
        #[serde(default = "default_encoder_hidden_name")]
        encoder_hidden_name: String,
        #[serde(default = "default_eos_token_id")]
        eos_token_id: i64,
        #[serde(default)]
        decoder_start_token_id: i64,
        #[serde(default = "default_sentence_split_threshold")]
        sentence_split_threshold: usize,
        #[serde(default = "default_decode_headroom")]
        decode_headroom: usize,
    }

    fn default_encoder_hidden_name() -> String { "hidden_states".into() }
    fn default_eos_token_id() -> i64 { 1 }
    fn default_sentence_split_threshold() -> usize { 30 }
    fn default_decode_headroom() -> usize { 32 }

    static CHECKER: Mutex<Option<std::sync::Arc<GrammarNeuralChecker>>> = Mutex::new(None);

    /// Set once the not-yet-downloaded state has been logged, so repeated
    /// pipeline runs before the package is installed don't spam the log.
    static LOGGED_ABSENT: AtomicBool = AtomicBool::new(false);
    /// Set once a present-but-corrupt package has failed to load. Gates
    /// retries: a broken download is not retried on every pipeline run,
    /// only after a process restart.
    static LOAD_FAILED: AtomicBool = AtomicBool::new(false);

    pub fn global() -> Option<std::sync::Arc<GrammarNeuralChecker>> {
        CHECKER.lock().unwrap().clone()
    }

    pub fn init_global() {
        if CHECKER.lock().unwrap().is_some() || LOAD_FAILED.load(Ordering::Relaxed) {
            return;
        }
        let mgr = match crate::models::ModelManager::init_global() {
            Ok(m) => m,
            Err(e) => {
                if !LOGGED_ABSENT.swap(true, Ordering::Relaxed) {
                    log::info!("Neural grammar disabled: model manager unavailable ({e})");
                }
                return;
            }
        };
        let Some(paths) = mgr.grammar_files() else {
            if !LOGGED_ABSENT.swap(true, Ordering::Relaxed) {
                log::info!(
                    "Neural grammar models not downloaded yet; grammar stage is a \
                     no-op until the dictation package is installed"
                );
            }
            return;
        };
        match GrammarNeuralChecker::load(&paths) {
            Ok(checker) => {
                *CHECKER.lock().unwrap() = Some(std::sync::Arc::new(checker));
            }
            Err(e) => {
                if !LOAD_FAILED.swap(true, Ordering::Relaxed) {
                    log::warn!("Neural grammar failed to load ({e}), grammar stage disabled");
                }
            }
        }
    }

    /// Total KV cache tensors: 4 layers x 4 (self-attn K,V + cross-attn K,V) = 16.
    const NUM_KV: usize = 16;
    /// Number of decoder layers (T5-efficient-tiny has 4).
    const NUM_LAYERS: usize = 4;
    /// pkv indices that hold cross-attention KV (K,V pairs at offset 2,3 per layer).
    const CROSS_ATTN_INDICES: [usize; 8] = [2, 3, 6, 7, 10, 11, 14, 15];

    pub struct GrammarNeuralChecker {
        router_session: Mutex<Session>,
        router_tokenizer: Tokenizer,
        router_threshold: f32,
        t5_encoder: Mutex<Session>,
        t5_decoder: Mutex<Session>,
        /// Cross-attention K/V projection weights: 8 matrices of [256, 256].
        /// Order: layer0_K, layer0_V, layer1_K, layer1_V, ... layer3_K, layer3_V.
        cross_attn_weights: [ndarray::Array2<f32>; NUM_LAYERS * 2],
        t5_tokenizer: Tokenizer,
        corrector_prefix: String,
        encoder_hidden_name: String,
        eos_token_id: i64,
        decoder_start_token_id: i64,
        sentence_split_threshold: usize,
        decode_headroom: usize,
    }

    static ORT_INIT: OnceLock<Result<(), String>> = OnceLock::new();

    fn ensure_ort_init() -> Result<(), String> {
        ORT_INIT
            .get_or_init(|| {
                // commit() returns false when an ORT environment was already
                // configured by another subsystem (piper also inits ORT). That's
                // not a failure — both share the one global environment, and each
                // session sets its own execution provider anyway.
                ort::init().commit();
                Ok(())
            })
            .as_ref()
            .map(|_| ())
            .map_err(|e| e.clone())
    }

    impl GrammarNeuralChecker {
        fn load(paths: &crate::models::GrammarFilePaths) -> Result<Self, String> {
            let config_raw = std::fs::read_to_string(&paths.config)
                .map_err(|e| format!("grammar config.json: {e}"))?;
            let config: Config = serde_json::from_str(&config_raw)
                .map_err(|e| format!("grammar config.json: {e}"))?;

            ensure_ort_init()?;

            // Force CPU EP for all grammar sessions. The ORT dylib from
            // sherpa-onnx may have CoreML compiled in, which can silently
            // produce wrong results for INT8-quantized grammar models.
            let cpu_ep = vec![ort::ep::CPUExecutionProvider::default().build()];

            let router_session = Session::builder()
                .map_err(|e| format!("session builder: {e}"))?
                .with_execution_providers(&cpu_ep)
                .map_err(|e| format!("router ep: {e}"))?
                .commit_from_file(&paths.router_model)
                .map_err(|e| format!("router model: {e}"))?;

            let router_tokenizer = Tokenizer::from_file(&paths.router_tokenizer)
                .map_err(|e| format!("router tokenizer: {e}"))?;

            let t5_encoder = Session::builder()
                .map_err(|e| format!("session builder: {e}"))?
                .with_execution_providers(&cpu_ep)
                .map_err(|e| format!("encoder ep: {e}"))?
                .commit_from_file(&paths.encoder)
                .map_err(|e| format!("t5 encoder: {e}"))?;

            let t5_decoder = Session::builder()
                .map_err(|e| format!("session builder: {e}"))?
                .with_execution_providers(&cpu_ep)
                .map_err(|e| format!("decoder ep: {e}"))?
                .commit_from_file(&paths.decoder)
                .map_err(|e| format!("t5 decoder: {e}"))?;

            let cross_attn_weights = Self::load_cross_attn_weights(&paths.kv_weights)?;

            let t5_tokenizer = Tokenizer::from_file(&paths.t5_tokenizer)
                .map_err(|e| format!("t5 tokenizer: {e}"))?;

            log::info!(
                "Neural grammar loaded: router threshold={}, corrector prefix={:?}, encoder_hidden={}",
                config.router.threshold,
                config.corrector.input_prefix,
                config.corrector.encoder_hidden_name,
            );
            Ok(Self {
                router_session: Mutex::new(router_session),
                router_tokenizer,
                router_threshold: config.router.threshold,
                t5_encoder: Mutex::new(t5_encoder),
                t5_decoder: Mutex::new(t5_decoder),
                cross_attn_weights,
                t5_tokenizer,
                corrector_prefix: config.corrector.input_prefix,
                encoder_hidden_name: config.corrector.encoder_hidden_name,
                eos_token_id: config.corrector.eos_token_id,
                decoder_start_token_id: config.corrector.decoder_start_token_id,
                sentence_split_threshold: config.corrector.sentence_split_threshold,
                decode_headroom: config.corrector.decode_headroom,
            })
        }

        /// Route and correct text. Returns (corrected_text, per_sentence_results).
        /// Always splits on sentence boundaries so each sentence is routed and
        /// scored independently.
        pub fn apply(&self, text: &str) -> (String, Vec<super::SentenceResult>) {
            let sentences = Self::split_sentences(text);
            if sentences.len() > 1 {
                let mut parts: Vec<String> = Vec::with_capacity(sentences.len());
                let mut results: Vec<super::SentenceResult> = Vec::with_capacity(sentences.len());
                for s in &sentences {
                    let (needs_correction, score) = self.route(s.as_str());
                    let (out, guarded) = if needs_correction { self.correct(s.as_str()) } else { (s.clone(), false) };
                    let actually_changed = out != *s;
                    parts.push(out.clone());
                    results.push(super::SentenceResult { text: out, score, corrected: actually_changed, guarded });
                }
                return (parts.join(" "), results);
            }
            let (needs_correction, score) = self.route(text);
            let (corrected, guarded) = if needs_correction { self.correct(text) } else { (text.to_string(), false) };
            let actually_changed = corrected != text;
            let results = vec![super::SentenceResult { text: corrected.clone(), score, corrected: actually_changed, guarded }];
            (corrected, results)
        }

        /// Returns (needs_correction, score). Score is None on error.
        pub fn route(&self, text: &str) -> (bool, Option<f32>) {
            match self.p_acceptable(text) {
                Ok(p) => {
                    log::debug!("Grammar router p(acceptable)={p:.3} threshold={}", self.router_threshold);
                    (p < self.router_threshold, Some(p))
                }
                Err(e) => {
                    log::warn!("Grammar router error: {e}");
                    (false, None)
                }
            }
        }

        fn p_acceptable(&self, text: &str) -> Result<f32, String> {
            let enc = self
                .router_tokenizer
                .encode(text, true)
                .map_err(|e| format!("router encode: {e}"))?;

            let n = enc.get_ids().len();
            let input_ids = Array2::from_shape_vec(
                (1, n),
                enc.get_ids().iter().map(|&x| x as i64).collect(),
            )
            .map_err(|e| e.to_string())?;
            let attention_mask = Array2::from_shape_vec(
                (1, n),
                enc.get_attention_mask().iter().map(|&x| x as i64).collect(),
            )
            .map_err(|e| e.to_string())?;
            let token_type_ids = Array2::from_shape_vec(
                (1, n),
                enc.get_type_ids().iter().map(|&x| x as i64).collect(),
            )
            .map_err(|e| e.to_string())?;

            let ids_ref = TensorRef::<i64>::from_array_view(&input_ids)
                .map_err(|e| format!("ids tensor: {e}"))?;
            let mask_ref = TensorRef::<i64>::from_array_view(&attention_mask)
                .map_err(|e| format!("mask tensor: {e}"))?;
            let tids_ref = TensorRef::<i64>::from_array_view(&token_type_ids)
                .map_err(|e| format!("tids tensor: {e}"))?;

            let mut session = self.router_session.lock().unwrap();
            let out = session
                .run(inputs![
                    "input_ids"      => ids_ref,
                    "attention_mask" => mask_ref,
                    "token_type_ids" => tids_ref,
                ])
                .map_err(|e| format!("router run: {e}"))?;

            // logits shape [1, 2]: index 0 = not_acceptable, 1 = acceptable
            let logits = out
                .get("logits")
                .ok_or_else(|| format!("grammar router: no 'logits' output; got: {:?}", out.keys().collect::<Vec<_>>()))?
                .try_extract_array::<f32>()
                .map_err(|e| format!("extract logits: {e}"))?;
            let l0 = logits[[0, 0]];
            let l1 = logits[[0, 1]];
            let m = l0.max(l1);
            Ok(((l1 - m).exp()) / ((l0 - m).exp() + (l1 - m).exp()))
        }

        /// Run corrector with selective negation guard. Does a word-level
        /// diff between original and corrected text, then reverts only the
        /// edits that add or remove negation markers while keeping all other
        /// corrections. This prevents the corrector from inverting meaning
        /// (e.g. "isn't working" -> "is working") without throwing away
        /// unrelated fixes in the same sentence.
        /// Returns (corrected_text, guarded).
        pub fn correct(&self, text: &str) -> (String, bool) {
            match self.correct_inner(text) {
                Ok(s) if !s.trim().is_empty() => {
                    super::guard_negation_edits(text, &s)
                }
                Ok(_) => (text.to_string(), false),
                Err(e) => {
                    log::warn!("Corrector failed: {e}");
                    (text.to_string(), false)
                }
            }
        }

        /// Split on sentence boundaries: `. `, `! `, `? ` followed by an
        /// uppercase letter. Keeps punctuation with its sentence.
        fn split_sentences(text: &str) -> Vec<String> {
            let mut sentences = Vec::new();
            let mut buf = String::new();
            let mut chars = text.char_indices().peekable();
            while let Some((i, ch)) = chars.next() {
                buf.push(ch);
                if matches!(ch, '.' | '!' | '?') {
                    let rest = &text[i + ch.len_utf8()..];
                    let mut rc = rest.chars();
                    if rc.next() == Some(' ') && rc.next().map_or(false, |c| c.is_uppercase()) {
                        let s = buf.trim().to_string();
                        if !s.is_empty() { sentences.push(s); }
                        buf = String::new();
                    }
                }
            }
            let tail = buf.trim().to_string();
            if !tail.is_empty() { sentences.push(tail); }
            if sentences.is_empty() { sentences.push(text.trim().to_string()); }
            sentences
        }

        fn correct_inner(&self, text: &str) -> Result<String, String> {
            let input_text = if self.corrector_prefix.is_empty() {
                text.to_string()
            } else {
                format!("{}{}", self.corrector_prefix, text)
            };
            let enc = self
                .t5_tokenizer
                .encode(input_text.as_str(), true)
                .map_err(|e| format!("t5 encode: {e}"))?;

            let n = enc.get_ids().len();
            let input_ids = Array2::from_shape_vec(
                (1, n),
                enc.get_ids().iter().map(|&x| x as i64).collect(),
            )
            .map_err(|e| e.to_string())?;
            let attention_mask = Array2::<i64>::from_elem((1, n), 1);

            let ids_ref = TensorRef::<i64>::from_array_view(&input_ids)
                .map_err(|e| format!("ids tensor: {e}"))?;
            let mask_ref = TensorRef::<i64>::from_array_view(&attention_mask)
                .map_err(|e| format!("mask tensor: {e}"))?;

            let mut encoder = self.t5_encoder.lock().unwrap();
            let enc_out = encoder
                .run(inputs![
                    "input_ids"      => ids_ref,
                    "attention_mask" => mask_ref,
                ])
                .map_err(|e| format!("encoder run: {e}"))?;

            let hidden: ArrayD<f32> = enc_out
                .get(&self.encoder_hidden_name)
                .ok_or_else(|| format!("encoder: no '{}' output; got: {:?}",
                    self.encoder_hidden_name, enc_out.keys().collect::<Vec<_>>()))?
                .try_extract_array::<f32>()
                .map_err(|e| format!("encoder hidden: {e}"))?
                .into_owned();

            let hidden3 = hidden
                .into_dimensionality::<ndarray::Ix3>()
                .map_err(|e| format!("hidden dim: {e}"))?;

            let token_ids = self.decode_greedy(&hidden3, &attention_mask)?;

            let decoded = self.t5_tokenizer
                .decode(&token_ids, true)
                .map_err(|e| format!("t5 decode: {e}"))?;

            Ok(super::strip_task_prefix(&decoded, &self.corrector_prefix))
        }

        /// Load 8 cross-attention K/V projection weight matrices from binary.
        /// Layout: 8 contiguous [256, 256] f32 matrices, ordered by layer then K/V.
        fn load_cross_attn_weights(path: &std::path::Path) -> Result<[ndarray::Array2<f32>; NUM_LAYERS * 2], String> {
            const DIM: usize = 256;
            const MAT_BYTES: usize = DIM * DIM * 4;
            let bytes = std::fs::read(path)
                .map_err(|e| format!("cross_attn_kv_weights.bin: {e}"))?;
            if bytes.len() != MAT_BYTES * NUM_LAYERS * 2 {
                return Err(format!(
                    "cross_attn_kv_weights.bin: expected {} bytes, got {}",
                    MAT_BYTES * NUM_LAYERS * 2,
                    bytes.len()
                ));
            }
            let mut weights: Vec<ndarray::Array2<f32>> = Vec::with_capacity(NUM_LAYERS * 2);
            for i in 0..(NUM_LAYERS * 2) {
                let offset = i * MAT_BYTES;
                let floats: Vec<f32> = bytes[offset..offset + MAT_BYTES]
                    .chunks_exact(4)
                    .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                    .collect();
                let mat = ndarray::Array2::from_shape_vec((DIM, DIM), floats)
                    .map_err(|e| format!("cross_attn weight {i}: {e}"))?;
                weights.push(mat);
            }
            weights.try_into().map_err(|_| "wrong number of weights".to_string())
        }

        /// Compute initial cross-attention KV cache from encoder hidden states.
        /// Returns 16 ArrayD tensors: self-attention slots are empty (shape [1,4,0,64]),
        /// cross-attention slots are projected from hidden (shape [1,4,enc_seq,64]).
        fn init_kv_cache(&self, hidden: &ndarray::Array3<f32>) -> Vec<ArrayD<f32>> {
            let enc_seq = hidden.shape()[1];
            let hidden_2d = hidden.to_shape((enc_seq, 256)).unwrap();
            let mut kv: Vec<ArrayD<f32>> = Vec::with_capacity(NUM_KV);
            let mut weight_idx = 0;
            for i in 0..NUM_KV {
                if CROSS_ATTN_INDICES.contains(&i) {
                    let projected = hidden_2d.dot(&self.cross_attn_weights[weight_idx]);
                    let shaped = projected
                        .into_shape_with_order((1, enc_seq, 4, 64))
                        .unwrap()
                        .permuted_axes([0, 2, 1, 3])
                        .as_standard_layout()
                        .into_owned()
                        .into_dyn();
                    kv.push(shaped);
                    weight_idx += 1;
                } else {
                    kv.push(ArrayD::<f32>::zeros(ndarray::IxDyn(&[1, 4, 0, 64])));
                }
            }
            kv
        }

        /// Greedy decode with KV cache. Pre-computes cross-attention KV from
        /// encoder hidden states, then runs the decoder_with_past model for
        /// every token. O(n) total work instead of O(n^2).
        fn decode_greedy(
            &self,
            hidden: &ndarray::Array3<f32>,
            encoder_mask: &Array2<i64>,
        ) -> Result<Vec<u32>, String> {
            let limit = hidden.shape()[1] + self.decode_headroom;
            let mut kv = self.init_kv_cache(hidden);
            let mut tokens: Vec<i64> = Vec::new();
            let mut next_tok = self.decoder_start_token_id;

            for _ in 0..limit {
                let token_arr = Array2::from_shape_vec((1, 1), vec![next_tok])
                    .map_err(|e| e.to_string())?;

                let new_kv = {
                    let token_ref = TensorRef::<i64>::from_array_view(&token_arr)
                        .map_err(|e| format!("dec ids tensor: {e}"))?;
                    let mask_ref = TensorRef::<i64>::from_array_view(encoder_mask)
                        .map_err(|e| format!("enc mask tensor: {e}"))?;

                    let kv_refs: Vec<TensorRef<f32>> = kv
                        .iter()
                        .map(|a| TensorRef::<f32>::from_array_view(a))
                        .collect::<Result<_, _>>()
                        .map_err(|e| format!("kv tensor ref: {e}"))?;

                    let hidden_ref = TensorRef::<f32>::from_array_view(hidden)
                        .map_err(|e| format!("hidden tensor: {e}"))?;

                    let mut feed = inputs![
                        "input_ids"              => token_ref,
                        "encoder_attention_mask"  => mask_ref,
                        "encoder_hidden_states"   => hidden_ref,
                    ];
                    for (i, r) in kv_refs.into_iter().enumerate() {
                        feed.push((format!("pkv_{i}").into(), r.into()));
                    }

                    let mut decoder = self.t5_decoder.lock().unwrap();
                    let out = decoder.run(feed).map_err(|e| format!("decoder run: {e}"))?;

                    next_tok = Self::argmax_last_token(&out)?;

                    let mut new_kv: Vec<ArrayD<f32>> = Vec::with_capacity(NUM_KV);
                    for i in 0..NUM_KV {
                        new_kv.push(
                            out[i + 1]
                                .try_extract_array::<f32>()
                                .map_err(|e| format!("kv[{i}]: {e}"))?
                                .into_owned(),
                        );
                    }
                    new_kv
                };

                kv = new_kv;

                if next_tok == self.eos_token_id {
                    break;
                }
                tokens.push(next_tok);
            }

            Ok(tokens.iter().map(|&x| x as u32).collect())
        }

        /// Extract argmax of the last token position from decoder logits output.
        fn argmax_last_token(out: &ort::session::SessionOutputs<'_>) -> Result<i64, String> {
            let logits = out[0]
                .try_extract_array::<f32>()
                .map_err(|e| format!("logits: {e}"))?;
            let last_pos = logits.shape()[1] - 1;
            logits
                .slice(s![0, last_pos, ..])
                .iter()
                .enumerate()
                .max_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal))
                .map(|(i, _)| i as i64)
                .ok_or_else(|| "empty logits".to_string())
        }
    }

}

fn is_negation(word: &str) -> bool {
    let low = word.to_lowercase();
    let stripped: String = low.chars()
        .filter(|c| c.is_alphanumeric() || *c == '\'' || *c == '\u{2019}')
        .collect();
    stripped == "not" || stripped == "no" || stripped == "never" || stripped == "nor"
        || stripped.ends_with("n't") || stripped.ends_with("n\u{2019}t")
}

/// Returns the word a contraction expands to, if any.
/// e.g. "we'll" -> "will", "I've" -> "have", "it's" -> "is".
/// Negation contractions (n't) return None since they're handled by
/// the negation guard.
fn contraction_expansion(word: &str) -> Option<&'static str> {
    let low = word.to_lowercase();
    let stripped: String = low.chars()
        .filter(|c| c.is_alphanumeric() || *c == '\'' || *c == '\u{2019}')
        .collect();
    if stripped.ends_with("'ve") || stripped.ends_with("\u{2019}ve") { return Some("have"); }
    if stripped.ends_with("'ll") || stripped.ends_with("\u{2019}ll") { return Some("will"); }
    if stripped.ends_with("'re") || stripped.ends_with("\u{2019}re") { return Some("are"); }
    if stripped.ends_with("'m") || stripped.ends_with("\u{2019}m") { return Some("am"); }
    if stripped.ends_with("'d") || stripped.ends_with("\u{2019}d") { return Some("would"); }
    if stripped.ends_with("'s") || stripped.ends_with("\u{2019}s") { return Some("is"); }
    None
}

/// True if a contraction in the original was dropped without being expanded.
/// "we'll keep" -> "we keep" is a drop (no "will" in corrected). Reverts.
/// "it's always" -> "it is always" is an expansion ("is" present). Keeps.
/// A replacement carrying the same contraction type also keeps ("me've" ->
/// "I've"): the ASR can garble the stem and the corrector must be allowed
/// to fix it without the suffix counting as dropped.
fn is_contraction_dropped(orig_span: &[&str], corr_span: &[&str]) -> bool {
    for word in orig_span {
        if let Some(expansion) = contraction_expansion(word) {
            let preserved = corr_span.iter().any(|w| {
                w.eq_ignore_ascii_case(expansion)
                    || contraction_expansion(w) == Some(expansion)
            });
            if !preserved {
                return true;
            }
        }
    }
    false
}

/// Word-level diff between original and corrected text. Reverts edit regions
/// where the corrector changed meaning: negation added/removed, or
/// contractions dropped without expansion (e.g. "we'll keep" -> "we keep").
/// Valid expansions like "it's" -> "it is" are kept.
/// Returns (result_text, was_guarded).
fn guard_negation_edits(original: &str, corrected: &str) -> (String, bool) {
    let orig_words: Vec<&str> = original.split_whitespace().collect();
    let corr_words: Vec<&str> = corrected.split_whitespace().collect();
    if orig_words == corr_words {
        return (corrected.to_string(), false);
    }

    // LCS word-level diff with case-insensitive matching
    let n = orig_words.len();
    let m = corr_words.len();
    let mut dp = vec![vec![0u16; m + 1]; n + 1];
    for i in 1..=n {
        for j in 1..=m {
            if orig_words[i - 1].eq_ignore_ascii_case(corr_words[j - 1]) {
                dp[i][j] = dp[i - 1][j - 1] + 1;
            } else {
                dp[i][j] = dp[i - 1][j].max(dp[i][j - 1]);
            }
        }
    }

    // Backtrack to find LCS indices
    let mut lcs_orig: Vec<usize> = Vec::new();
    let mut lcs_corr: Vec<usize> = Vec::new();
    let (mut i, mut j) = (n, m);
    while i > 0 && j > 0 {
        if orig_words[i - 1].eq_ignore_ascii_case(corr_words[j - 1]) {
            lcs_orig.push(i - 1);
            lcs_corr.push(j - 1);
            i -= 1;
            j -= 1;
        } else if dp[i - 1][j] >= dp[i][j - 1] {
            i -= 1;
        } else {
            j -= 1;
        }
    }
    lcs_orig.reverse();
    lcs_corr.reverse();

    // Walk both sequences using LCS anchors. Between anchors are edit
    // regions where orig and corr diverge.
    let mut result: Vec<&str> = Vec::new();
    let mut oi = 0usize;
    let mut ci = 0usize;
    let mut reverted = false;
    for k in 0..lcs_orig.len() {
        let anchor_o = lcs_orig[k];
        let anchor_c = lcs_corr[k];
        let orig_span = &orig_words[oi..anchor_o];
        let corr_span = &corr_words[ci..anchor_c];
        let neg_changed = {
            let orig_neg = orig_span.iter().filter(|w| is_negation(w)).count();
            let corr_neg = corr_span.iter().filter(|w| is_negation(w)).count();
            orig_neg != corr_neg
        };
        if neg_changed || is_contraction_dropped(orig_span, corr_span) {
            result.extend_from_slice(orig_span);
            reverted = true;
        } else {
            result.extend_from_slice(corr_span);
        }
        result.push(corr_words[anchor_c]);
        oi = anchor_o + 1;
        ci = anchor_c + 1;
    }

    // Trailing edit region after last anchor
    let orig_tail = &orig_words[oi..];
    let corr_tail = &corr_words[ci..];
    let neg_changed = {
        let orig_neg = orig_tail.iter().filter(|w| is_negation(w)).count();
        let corr_neg = corr_tail.iter().filter(|w| is_negation(w)).count();
        orig_neg != corr_neg
    };
    if neg_changed || is_contraction_dropped(orig_tail, corr_tail) {
        result.extend_from_slice(orig_tail);
        reverted = true;
    } else {
        result.extend_from_slice(corr_tail);
    }

    if reverted {
        let merged = result.join(" ");
        log::info!(
            "Guard reverted edits: {:?} -> {:?} (merged: {:?})",
            original, corrected, merged,
        );
        (merged, true)
    } else {
        (corrected.to_string(), false)
    }
}

/// Strip the task prefix if the corrector echoes it back. The model
/// re-cases it as a sentence opener ("grammar: " -> "Grammar: "), so the
/// match must be case-insensitive.
fn strip_task_prefix(decoded: &str, prefix: &str) -> String {
    if prefix.is_empty() {
        return decoded.to_string();
    }
    for cand in [prefix, prefix.trim_end()] {
        if decoded.len() >= cand.len()
            && decoded.is_char_boundary(cand.len())
            && decoded[..cand.len()].eq_ignore_ascii_case(cand)
        {
            return decoded[cand.len()..].trim_start().to_string();
        }
    }
    decoded.to_string()
}

pub use neural::{global, init_global, GrammarNeuralChecker};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_negation() {
        assert!(is_negation("not"));
        assert!(is_negation("Not"));
        assert!(is_negation("no"));
        assert!(is_negation("never"));
        assert!(is_negation("nor"));
        assert!(is_negation("isn't"));
        assert!(is_negation("don't"));
        assert!(is_negation("isn\u{2019}t")); // curly apostrophe
        assert!(!is_negation("now"));
        assert!(!is_negation("note"));
        assert!(!is_negation("working"));
    }

    #[test]
    fn test_identical_text_passes_through() {
        let text = "The button is working fine.";
        let (result, guarded) = guard_negation_edits(text, text);
        assert_eq!(result, text);
        assert!(!guarded);
    }

    #[test]
    fn test_strip_task_prefix() {
        assert_eq!(strip_task_prefix("grammar: fixed text", "grammar: "), "fixed text");
        // The corrector re-cases the echoed prefix as a sentence opener.
        assert_eq!(strip_task_prefix("Grammar: fixed text", "grammar: "), "fixed text");
        assert_eq!(strip_task_prefix("Grammar: Fixed text.", "grammar: "), "Fixed text.");
        // Trimmed variant (no trailing space in the echo).
        assert_eq!(strip_task_prefix("Grammar:fixed", "grammar: "), "fixed");
        // No echo: text passes through untouched.
        assert_eq!(strip_task_prefix("fixed text", "grammar: "), "fixed text");
        assert_eq!(strip_task_prefix("fixed text", ""), "fixed text");
        // A sentence merely starting with the word is still stripped only
        // when it matches the exact prefix form.
        assert_eq!(strip_task_prefix("Grammatical text", "grammar: "), "Grammatical text");
    }

    #[test]
    fn test_no_negation_change_keeps_correction() {
        let orig = "The create snippet button works.";
        let corr = "The create a snippet button works.";
        let (result, guarded) = guard_negation_edits(orig, corr);
        assert_eq!(result, corr);
        assert!(!guarded);
    }

    #[test]
    fn test_negation_removal_reverted() {
        let orig = "The button isn't working.";
        let corr = "The button is working.";
        let (result, guarded) = guard_negation_edits(orig, corr);
        assert!(result.contains("isn't"), "should preserve negation, got: {result}");
        assert!(guarded);
    }

    #[test]
    fn test_mixed_edits_keeps_non_negation_fixes() {
        let orig = "The create snippet button isn't working";
        let corr = "The create a snippet button is working";
        let (result, guarded) = guard_negation_edits(orig, corr);
        assert!(result.contains("a snippet"), "should keep 'a' insertion, got: {result}");
        assert!(result.contains("isn't"), "should preserve negation, got: {result}");
        assert!(guarded);
    }

    #[test]
    fn test_negation_added_reverted() {
        let orig = "The system works correctly.";
        let corr = "The system doesn't work correctly.";
        let (result, guarded) = guard_negation_edits(orig, corr);
        assert!(!result.contains("doesn't"), "should revert added negation, got: {result}");
        assert!(guarded);
    }

    // ── Contraction guard tests ──

    #[test]
    fn test_contraction_expansion_returns_word() {
        assert_eq!(contraction_expansion("I've"), Some("have"));
        assert_eq!(contraction_expansion("we'll"), Some("will"));
        assert_eq!(contraction_expansion("it's"), Some("is"));
        assert_eq!(contraction_expansion("we're"), Some("are"));
        assert_eq!(contraction_expansion("I'm"), Some("am"));
        assert_eq!(contraction_expansion("he'd"), Some("would"));
        assert_eq!(contraction_expansion("we\u{2019}ll"), Some("will"));
        assert_eq!(contraction_expansion("don't"), None);
        assert_eq!(contraction_expansion("isn't"), None);
        assert_eq!(contraction_expansion("well"), None);
        assert_eq!(contraction_expansion("the"), None);
    }

    #[test]
    fn test_contraction_valid_expansion_kept() {
        let orig = "so it's always over the map";
        let corr = "so it is always over the map";
        let (result, guarded) = guard_negation_edits(orig, corr);
        assert_eq!(result, corr, "valid expansion should be kept");
        assert!(!guarded);
    }

    #[test]
    fn test_contraction_were_expansion_kept() {
        let orig = "see if we're losing tail audio.";
        let corr = "see if we are losing tail audio.";
        let (result, guarded) = guard_negation_edits(orig, corr);
        assert_eq!(result, corr, "valid expansion should be kept");
        assert!(!guarded);
    }

    #[test]
    fn test_contraction_drop_will_reverted() {
        let orig = "but we'll keep trying to see";
        let corr = "but we keep trying to see";
        let (result, guarded) = guard_negation_edits(orig, corr);
        assert!(result.contains("we'll"), "should preserve contraction, got: {result}");
        assert!(guarded);
    }

    #[test]
    fn test_contraction_stem_fix_kept() {
        // ASR-garbled stems: the correction keeps the same contraction type,
        // so it must NOT count as a drop.
        for (orig, corr) in [
            ("me've had a lot of fun", "I've had a lot of fun"),
            ("me'm not sure about it", "I'm not sure about it"),
            ("me've had a lot of fun", "I have had a lot of fun"),
        ] {
            let (result, guarded) = guard_negation_edits(orig, corr);
            assert_eq!(result, corr, "stem fix should be kept");
            assert!(!guarded);
        }
    }

    #[test]
    fn test_contraction_drop_have_reverted() {
        let orig = "I've got an issue";
        let corr = "I got an issue";
        let (result, guarded) = guard_negation_edits(orig, corr);
        assert!(result.contains("I've"), "should preserve contraction, got: {result}");
        assert!(guarded);
    }

    #[test]
    fn test_contraction_drop_am_reverted() {
        let orig = "I'm going to the store";
        let corr = "I going to the store";
        let (result, guarded) = guard_negation_edits(orig, corr);
        assert!(result.contains("I'm"), "should preserve contraction, got: {result}");
        assert!(guarded);
    }

    #[test]
    fn test_contraction_unrelated_fix_kept() {
        let orig = "I've got a problems with it";
        let corr = "I've got a problem with it";
        let (result, guarded) = guard_negation_edits(orig, corr);
        assert_eq!(result, corr, "non-contraction fix should be kept");
        assert!(!guarded);
    }

    #[test]
    fn test_mixed_contraction_drop_and_other_fix() {
        let orig = "I've got a problems";
        let corr = "I got a problem";
        let (result, guarded) = guard_negation_edits(orig, corr);
        assert!(result.contains("I've"), "should preserve contraction, got: {result}");
        assert!(result.contains("problem"), "should keep grammar fix, got: {result}");
        assert!(!result.contains("problems"), "should not revert grammar fix, got: {result}");
        assert!(guarded);
    }
}
