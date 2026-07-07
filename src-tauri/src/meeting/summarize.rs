//! Local LLM summarization via ONNX Runtime: a decoder-only chat model
//! (Qwen3 / Gemma / Llama ONNX exports) run with a hand-rolled KV-cache loop.
//!
//! Mirrors the grammar corrector's session handling (shared ORT env, CPU EP
//! forced, `commit_from_file`, `tokenizers` crate) but the generation loop is
//! decoder-only: one prefill run over the whole prompt with empty caches,
//! then one token per step, with the model's `present.*` outputs fed back as
//! the next step's `past_key_values.*` inputs. The KV tensors are moved as
//! opaque `DynValue`s — never extracted or converted — so dtype (f16 in the
//! q4f16 exports) and layout are the model's own business.
//!
//! Model-specific facts (layer count, prompt template, EOS ids) come from an
//! `llm_config.json` beside the weights, authored when a model is staged for
//! the manifest — nothing per-family is hardcoded here.

use std::path::Path;
use std::sync::Mutex;

use ndarray::Array2;
use ort::session::Session;
use ort::value::{DynValue, TensorRef};
use tokenizers::Tokenizer;

/// Per-model runtime description, `llm_config.json` in the model directory.
#[derive(serde::Deserialize)]
pub struct LlmConfig {
    /// Display/debug only (e.g. "qwen3").
    #[serde(default)]
    pub family: String,
    pub num_layers: usize,
    pub num_kv_heads: usize,
    pub head_dim: usize,
    /// Token ids that end generation (e.g. <|im_end|> and <|endoftext|>).
    pub eos_token_ids: Vec<i64>,
    /// Hard ceiling on prompt+generation tokens the model supports.
    pub max_context: usize,
    /// Prompt template with `{system}` and `{user}` placeholders. Must end
    /// with the assistant-turn opener (and, for families with a thinking
    /// mode, whatever suppresses it — e.g. Qwen3's empty <think/> block).
    pub prompt_template: String,
}

impl LlmConfig {
    pub fn load(dir: &Path) -> Result<Self, String> {
        let raw = std::fs::read_to_string(dir.join("llm_config.json"))
            .map_err(|e| format!("llm_config.json: {e}"))?;
        serde_json::from_str(&raw).map_err(|e| format!("llm_config.json parse: {e}"))
    }

    pub fn render_prompt(&self, system: &str, user: &str) -> String {
        self.prompt_template
            .replace("{system}", system)
            .replace("{user}", user)
    }
}

pub struct LlmRunner {
    session: Mutex<Session>,
    tokenizer: Tokenizer,
    pub cfg: LlmConfig,
    /// Which optional inputs this export declares (probed from the session).
    wants_position_ids: bool,
    /// Element type of the KV inputs, for building the empty prefill caches.
    kv_is_f16: bool,
}

/// Result of one generation, with the numbers the probe/progress UIs want.
pub struct Generation {
    pub text: String,
    pub prompt_tokens: usize,
    pub new_tokens: usize,
    pub prefill_ms: u128,
    pub decode_ms: u128,
}

impl LlmRunner {
    /// Load a model directory: `model.onnx` + `tokenizer.json` +
    /// `llm_config.json`.
    pub fn load(dir: &Path) -> Result<Self, String> {
        let cfg = LlmConfig::load(dir)?;

        crate::piper::ensure_ort_init()?;

        // CPU only: the shared ORT dylib may have platform EPs compiled in
        // (CoreML) that silently mis-execute quantized graphs — same policy
        // as the grammar sessions.
        let cpu_ep = vec![ort::ep::CPUExecutionProvider::default().build()];
        let session = Session::builder()
            .map_err(|e| format!("session builder: {e}"))?
            .with_execution_providers(&cpu_ep)
            .map_err(|e| format!("llm ep: {e}"))?
            .commit_from_file(dir.join("model.onnx"))
            .map_err(|e| format!("llm model: {e}"))?;

        let tokenizer = Tokenizer::from_file(dir.join("tokenizer.json"))
            .map_err(|e| format!("llm tokenizer: {e}"))?;

        // Probe the export's actual input surface instead of assuming: some
        // Optimum exports take position_ids, some derive them internally,
        // and the KV dtype differs between q4 (f32) and q4f16 (f16).
        let mut wants_position_ids = false;
        let mut kv_is_f16 = false;
        for input in session.inputs() {
            if input.name() == "position_ids" {
                wants_position_ids = true;
            }
            if input.name().starts_with("past_key_values.0.") {
                kv_is_f16 = matches!(
                    input.dtype().tensor_type(),
                    Some(ort::value::TensorElementType::Float16)
                );
            }
        }

        log::info!(
            "LLM loaded: family={}, {} layers, kv {}x{} ({}), position_ids={}",
            cfg.family,
            cfg.num_layers,
            cfg.num_kv_heads,
            cfg.head_dim,
            if kv_is_f16 { "f16" } else { "f32" },
            wants_position_ids,
        );

        Ok(Self {
            session: Mutex::new(session),
            tokenizer,
            cfg,
            wants_position_ids,
            kv_is_f16,
        })
    }

    /// Render the chat prompt and generate up to `max_new_tokens` greedily.
    pub fn generate(
        &self,
        system: &str,
        user: &str,
        max_new_tokens: usize,
    ) -> Result<Generation, String> {
        let prompt = self.cfg.render_prompt(system, user);
        let encoding = self
            .tokenizer
            .encode(prompt, false)
            .map_err(|e| format!("encode: {e}"))?;
        let prompt_ids: Vec<i64> = encoding.get_ids().iter().map(|&t| t as i64).collect();
        if prompt_ids.is_empty() {
            return Err("empty prompt after tokenization".into());
        }
        if prompt_ids.len() + max_new_tokens > self.cfg.max_context {
            return Err(format!(
                "prompt too long: {} tokens + {} new exceeds the {}-token context",
                prompt_ids.len(),
                max_new_tokens,
                self.cfg.max_context
            ));
        }

        let mut session = self.session.lock().unwrap();

        // Prefill: the whole prompt in one run against empty caches. The
        // empty tensors carry the KV dtype/shape the export declares.
        let t_prefill = std::time::Instant::now();
        let mut kv: Vec<DynValue> = self.empty_kv()?;
        let mut past_len = 0usize;
        let (mut next_token, new_kv) =
            self.run_step(&mut session, &prompt_ids, past_len, kv)?;
        kv = new_kv;
        past_len += prompt_ids.len();
        let prefill_ms = t_prefill.elapsed().as_millis();

        // Decode: one token per step until EOS or the cap.
        let t_decode = std::time::Instant::now();
        let mut generated: Vec<u32> = Vec::new();
        for _ in 0..max_new_tokens {
            if self.cfg.eos_token_ids.contains(&next_token) {
                break;
            }
            generated.push(next_token as u32);
            let (tok, new_kv) = self.run_step(&mut session, &[next_token], past_len, kv)?;
            kv = new_kv;
            past_len += 1;
            next_token = tok;
        }
        let decode_ms = t_decode.elapsed().as_millis();

        let text = self
            .tokenizer
            .decode(&generated, true)
            .map_err(|e| format!("decode: {e}"))?
            .trim()
            .to_string();

        Ok(Generation {
            text,
            prompt_tokens: prompt_ids.len(),
            new_tokens: generated.len(),
            prefill_ms,
            decode_ms,
        })
    }

    /// One forward pass: `ids` at positions `past_len..`, prior caches moved
    /// in, updated caches moved out. Returns the argmax of the last position.
    fn run_step(
        &self,
        session: &mut Session,
        ids: &[i64],
        past_len: usize,
        kv: Vec<DynValue>,
    ) -> Result<(i64, Vec<DynValue>), String> {
        let seq = ids.len();
        let ids_arr = Array2::from_shape_vec((1, seq), ids.to_vec()).map_err(|e| e.to_string())?;
        // Attention over everything: past + current.
        let mask_arr = Array2::<i64>::ones((1, past_len + seq));
        let pos_arr = Array2::from_shape_vec(
            (1, seq),
            (past_len..past_len + seq).map(|p| p as i64).collect(),
        )
        .map_err(|e| e.to_string())?;

        let ids_ref = TensorRef::<i64>::from_array_view(&ids_arr)
            .map_err(|e| format!("ids tensor: {e}"))?;
        let mask_ref = TensorRef::<i64>::from_array_view(&mask_arr)
            .map_err(|e| format!("mask tensor: {e}"))?;

        let mut feed = ort::inputs![
            "input_ids" => ids_ref,
            "attention_mask" => mask_ref,
        ];
        if self.wants_position_ids {
            let pos_ref = TensorRef::<i64>::from_array_view(&pos_arr)
                .map_err(|e| format!("pos tensor: {e}"))?;
            feed.push(("position_ids".into(), pos_ref.into()));
        }
        // Move the caches in by name (k then v per layer, matching empty_kv
        // and the extraction order below).
        let mut kv_iter = kv.into_iter();
        for layer in 0..self.cfg.num_layers {
            let k = kv_iter.next().ok_or("kv underflow")?;
            let v = kv_iter.next().ok_or("kv underflow")?;
            feed.push((format!("past_key_values.{layer}.key").into(), k.into()));
            feed.push((format!("past_key_values.{layer}.value").into(), v.into()));
        }

        let mut outputs = session.run(feed).map_err(|e| format!("llm run: {e}"))?;

        let next = argmax_last(&outputs)?;

        let mut new_kv: Vec<DynValue> = Vec::with_capacity(self.cfg.num_layers * 2);
        for layer in 0..self.cfg.num_layers {
            for part in ["key", "value"] {
                new_kv.push(
                    outputs
                        .remove(format!("present.{layer}.{part}"))
                        .ok_or_else(|| format!("missing output present.{layer}.{part}"))?,
                );
            }
        }
        Ok((next, new_kv))
    }

    /// Empty per-layer caches for the prefill run: shape [1, kv_heads, 0,
    /// head_dim] in the dtype the export declares.
    fn empty_kv(&self) -> Result<Vec<DynValue>, String> {
        let shape = (1usize, self.cfg.num_kv_heads, 0usize, self.cfg.head_dim);
        let mut kv = Vec::with_capacity(self.cfg.num_layers * 2);
        for _ in 0..self.cfg.num_layers * 2 {
            let value: DynValue = if self.kv_is_f16 {
                let arr = ndarray::Array4::from_elem(shape, half::f16::ZERO);
                ort::value::Tensor::from_array(arr)
                    .map_err(|e| format!("empty kv: {e}"))?
                    .into_dyn()
            } else {
                let arr = ndarray::Array4::<f32>::zeros(shape);
                ort::value::Tensor::from_array(arr)
                    .map_err(|e| format!("empty kv: {e}"))?
                    .into_dyn()
            };
            kv.push(value);
        }
        Ok(kv)
    }
}

/// Argmax over the last position of the logits output, handling both f32 and
/// f16 (q4f16 exports emit f16 logits).
fn argmax_last(outputs: &ort::session::SessionOutputs<'_>) -> Result<i64, String> {
    let logits = &outputs["logits"];
    if let Ok(arr) = logits.try_extract_array::<f32>() {
        let last = arr.shape()[1] - 1;
        return arr
            .slice(ndarray::s![0, last, ..])
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(i, _)| i as i64)
            .ok_or_else(|| "empty logits".into());
    }
    let arr = logits
        .try_extract_array::<half::f16>()
        .map_err(|e| format!("logits (f16): {e}"))?;
    let last = arr.shape()[1] - 1;
    arr.slice(ndarray::s![0, last, ..])
        .iter()
        .enumerate()
        .max_by(|a, b| {
            a.1.to_f32()
                .partial_cmp(&b.1.to_f32())
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(i, _)| i as i64)
        .ok_or_else(|| "empty logits".into())
}
