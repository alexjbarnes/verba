//! Post-processing pipeline for transcribed text.
//!
//! 1. Filler word removal (rule-based, ~1ms)
//! 2. Inverse text normalization (rule-based, ~5ms)
//! 3. User vocab substitution (rule-based, <1ms)
//! 4. Grammar correction — neural (CoLA router + T5 corrector, ~4-65ms);
//!    requires the bundled models (grammar_neural_bundled), no-op otherwise

mod filler;
pub mod grammar_neural;
mod itn;
mod spelling;
pub mod vocab;

use std::sync::OnceLock;
use std::time::Instant;

use serde::{Deserialize, Serialize};

/// A snapshot of text after a pipeline stage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineStage {
    pub name: String,
    pub text: String,
    #[serde(default)]
    pub changed: bool,
    #[serde(default)]
    pub duration_ms: u64,
    /// Grammar router score [0,1]: P(acceptable). Only present when the
    /// neural path ran. Below the routing threshold → correction was applied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grammar_score: Option<f32>,
    /// Per-sentence routing and correction detail. Populated by the neural
    /// path when the input contained multiple sentences.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub grammar_sentences: Vec<grammar_neural::SentenceResult>,
}

/// Result of the post-processing pipeline with intermediate stage snapshots.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineResult {
    pub text: String,
    pub stages: Vec<PipelineStage>,
    #[serde(default)]
    pub total_ms: u64,
}

struct Pipeline {
    #[allow(dead_code)] // parked: see the disabled Spelling stage below
    spelling: spelling::SpellCorrector,
}

static PIPELINE: OnceLock<Pipeline> = OnceLock::new();

fn get_pipeline() -> &'static Pipeline {
    PIPELINE.get_or_init(|| Pipeline {
        spelling: spelling::SpellCorrector::new(),
    })
}

/// Force-initialize the pipeline (spelling dictionary + grammar models).
/// Call during startup so the first transcription isn't slow.
pub fn warm_up() {
    let t = Instant::now();
    let _ = get_pipeline();
    grammar_neural::init_global();
    log::info!("Pipeline: warm-up took {}ms", t.elapsed().as_millis());
}

/// Run the full post-processing pipeline on transcribed text.
/// Returns the final text along with snapshots after each stage that made a change.
pub fn postprocess(text: &str) -> PipelineResult {
    if text.trim().is_empty() {
        return PipelineResult {
            text: String::new(),
            stages: Vec::new(),
            total_ms: 0,
        };
    }

    let total_start = Instant::now();
    let mut stages = Vec::new();
    let mut s = text.to_string();

    stages.push(PipelineStage {
        name: "Raw transcription".to_string(),
        text: s.clone(),
        changed: false,
        duration_ms: 0,
        grammar_score: None,
        grammar_sentences: vec![],
    });

    // Stage 1: Filler word removal
    let t = Instant::now();
    let prev = s.clone();
    s = filler::remove_fillers(&s);
    let changed = s != prev;
    let ms = t.elapsed().as_millis() as u64;
    log::debug!("Pipeline stage 1 (fillers): {ms}ms changed={changed}");
    stages.push(PipelineStage {
        name: "Filler removal".to_string(),
        text: s.clone(),
        changed,
        duration_ms: ms,
        grammar_score: None,
        grammar_sentences: vec![],
    });

    // Stage 2: Inverse text normalization
    let t = Instant::now();
    let prev = s.clone();
    s = itn::normalize(&s);
    let changed = s != prev;
    let ms = t.elapsed().as_millis() as u64;
    log::debug!("Pipeline stage 2 (ITN): {ms}ms changed={changed}");
    stages.push(PipelineStage {
        name: "ITN".to_string(),
        text: s.clone(),
        changed,
        duration_ms: ms,
        grammar_score: None,
        grammar_sentences: vec![],
    });

    // Stage 3: User vocab substitution
    let t = Instant::now();
    let prev = s.clone();
    s = vocab::apply(&s);
    let changed = s != prev;
    let ms = t.elapsed().as_millis() as u64;
    log::debug!("Pipeline stage 3 (Vocab): {ms}ms changed={changed}");
    stages.push(PipelineStage {
        name: "Vocab".to_string(),
        text: s.clone(),
        changed,
        duration_ms: ms,
        grammar_score: None,
        grammar_sentences: vec![],
    });

    // Stage 4: Grammar correction — CoLA router + T5 corrector. No-op when
    // the models aren't bundled (grammar_neural_bundled unset at build time).
    //
    // Skipped for short texts: fragments and replacement phrases (e.g. two
    // words dictated over a selection) score very low on the grammar router
    // and get routed to T5, which adds unwanted content.
    //
    // Long texts are handled by the corrector splitting on sentence boundaries
    // and correcting each sentence individually.
    //
    // catch_unwind guards against any Rust panic (e.g., from ORT internals)
    // crossing a JNI boundary and aborting the process on Android. Native
    // crashes (SIGSEGV) are not caught here.
    const MIN_GRAMMAR_WORDS: usize = 5;
    let t = Instant::now();
    let prev = s.clone();
    let grammar_label;
    let mut grammar_score: Option<f32> = None;
    let mut grammar_sentences: Vec<grammar_neural::SentenceResult> = vec![];
    let word_count = s.split_whitespace().count();
    if word_count < MIN_GRAMMAR_WORDS {
        // Still score via router for data collection, but skip correction.
        if let Some(neural) = grammar_neural::global() {
            let (_, score) = neural.route(&s);
            grammar_score = score;
            grammar_sentences = vec![grammar_neural::SentenceResult {
                text: s.clone(),
                score,
                corrected: false,
                guarded: false,
            }];
            log::debug!("Pipeline stage 4: grammar skipped ({word_count} words < {MIN_GRAMMAR_WORDS}), score={score:?}");
        } else {
            log::debug!("Pipeline stage 4: grammar skipped ({word_count} words < {MIN_GRAMMAR_WORDS})");
        }
        grammar_label = "Grammar (skipped)";
    } else {
        let grammar_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if let Some(neural) = grammar_neural::global() {
                let (corrected, results) = neural.apply(&s);
                let min_score = results.iter().filter_map(|r| r.score).reduce(f32::min);
                (corrected, "Grammar (neural)", min_score, results)
            } else {
                (s.clone(), "Grammar (unavailable)", None, vec![])
            }
        }));
        match grammar_result {
            Ok((corrected, label, score, sentences)) => {
                s = corrected;
                grammar_label = label;
                grammar_score = score;
                grammar_sentences = sentences;
            }
            Err(payload) => {
                let msg = payload
                    .downcast_ref::<&str>()
                    .copied()
                    .or_else(|| payload.downcast_ref::<String>().map(|s| s.as_str()))
                    .unwrap_or("(non-string panic)");
                log::error!("Pipeline stage 4: grammar correction panicked: {msg}");
                grammar_label = "Grammar (error)";
            }
        }
    }
    let changed = s != prev;
    let ms = t.elapsed().as_millis() as u64;
    log::debug!("Pipeline stage 4 ({grammar_label}): {ms}ms changed={changed}");
    stages.push(PipelineStage {
        name: grammar_label.to_string(),
        text: s.clone(),
        changed,
        duration_ms: ms,
        grammar_score,
        grammar_sentences,
    });

    // Stage 4: Spell correction (disabled — SymSpell corrupts proper nouns and
    // CamelCase tokens that are absent from its British English corpus. ASR
    // output doesn't produce edit-distance typos so the stage adds no value.)
    // let t = Instant::now();
    // let prev = s.clone();
    // s = pipeline.spelling.correct(&s);
    // let changed = s != prev;
    // let ms = t.elapsed().as_millis() as u64;
    // log::debug!("Pipeline stage 4 (Spelling): {ms}ms changed={changed}");
    // stages.push(PipelineStage {
    //     name: "Spelling".to_string(),
    //     text: s.clone(),
    //     changed,
    //     duration_ms: ms,
    // });

    // Final cleanup
    let t = Instant::now();
    let prev = s.clone();
    s = final_cleanup(&s);
    let changed = s != prev;
    let ms = t.elapsed().as_millis() as u64;
    stages.push(PipelineStage {
        name: "Cleanup".to_string(),
        text: s.clone(),
        changed,
        duration_ms: ms,
        grammar_score: None,
        grammar_sentences: vec![],
    });

    let total_ms = total_start.elapsed().as_millis() as u64;
    log::info!("Pipeline total: {total_ms}ms ({} stages changed)", stages.iter().filter(|s| s.changed).count());
    PipelineResult { text: s, stages, total_ms }
}

/// Join multiple transcription chunks, fixing false sentence boundaries.
///
/// When VAD splits audio mid-sentence, each chunk gets independent punctuation
/// from the transcriber. "if it's one." + "chunk of text" should join as
/// "if it's one chunk of text", not "if it's one. chunk of text".
///
/// Rule: strip trailing period from a chunk if the next chunk starts with a
/// lowercase letter (suggesting the sentence continues).
pub fn join_chunks(chunks: &[&str]) -> String {
    if chunks.is_empty() {
        return String::new();
    }
    if chunks.len() == 1 {
        return chunks[0].to_string();
    }

    let mut parts: Vec<String> = Vec::with_capacity(chunks.len());
    for (i, chunk) in chunks.iter().enumerate() {
        let trimmed = chunk.trim();
        if trimmed.is_empty() {
            continue;
        }

        if i + 1 < chunks.len() {
            let next_first = chunks[i + 1].trim().chars().next();
            if let Some(c) = next_first {
                if c.is_ascii_lowercase() && trimmed.ends_with('.') {
                    // Next chunk continues the sentence -- strip the false period
                    parts.push(trimmed.trim_end_matches('.').to_string());
                    continue;
                }
            }
        }

        parts.push(trimmed.to_string());
    }

    parts.join(" ")
}

/// Final pass: ensure capitalization and trailing punctuation.
fn final_cleanup(text: &str) -> String {
    let mut s = text.trim().to_string();
    if s.is_empty() {
        return s;
    }

    // Collapse multiple spaces
    while s.contains("  ") {
        s = s.replace("  ", " ");
    }

    // Fix false mid-sentence capitalization from the model.
    // Lowercase words that are capitalized but don't follow sentence-ending
    // punctuation and aren't acronyms or "I".
    s = fix_mid_sentence_caps(&s);

    // Capitalize first character
    let mut chars = s.chars();
    if let Some(first) = chars.next() {
        if first.is_ascii_lowercase() {
            s = first.to_ascii_uppercase().to_string() + chars.as_str();
        }
    }

    // Add trailing period if no sentence-ending punctuation
    let last = s.chars().last().unwrap_or(' ');
    if !matches!(last, '.' | '!' | '?' | ',' | ';' | ':' | '"' | '\'' | ')') {
        s.push('.');
    }

    s
}

/// Lowercase words that the model falsely capitalized mid-sentence.
/// Preserves capitalization after sentence-ending punctuation, for acronyms
/// (all caps, 2+ chars), and for "I".
fn fix_mid_sentence_caps(text: &str) -> String {
    let words: Vec<&str> = text.split(' ').collect();
    if words.len() <= 1 {
        return text.to_string();
    }

    let mut result = Vec::with_capacity(words.len());
    result.push(words[0].to_string());

    for i in 1..words.len() {
        let word = words[i];
        let prev = words[i - 1];

        // Check if previous word ends with sentence-ending punctuation
        let after_sentence_end = prev.ends_with('.') || prev.ends_with('!') || prev.ends_with('?');

        if after_sentence_end {
            result.push(word.to_string());
            continue;
        }

        // Strip leading punctuation to examine the core word
        let core_start = word.find(|c: char| !c.is_ascii_punctuation()).unwrap_or(word.len());
        let prefix = &word[..core_start];
        let rest = &word[core_start..];

        if rest.is_empty() {
            result.push(word.to_string());
            continue;
        }

        let first_char = rest.chars().next().unwrap();
        if !first_char.is_uppercase() {
            result.push(word.to_string());
            continue;
        }

        // Find where the alphabetic part ends (core word without trailing punct)
        let alpha_end = rest.find(|c: char| !c.is_alphabetic() && c != '\'').unwrap_or(rest.len());
        let core = &rest[..alpha_end];

        // Preserve "I" and contractions: I've, I'd, I'll, I'm
        if core == "I" || rest.starts_with("I'") {
            result.push(word.to_string());
            continue;
        }

        // Preserve tokens containing digits: R2, v3, C4, H2O, INT8
        if rest.chars().any(|c| c.is_ascii_digit()) {
            result.push(word.to_string());
            continue;
        }

        // Preserve acronyms (all uppercase, 2+ chars): API, DNS, USA
        if core.len() >= 2 && core.chars().all(|c| c.is_uppercase()) {
            result.push(word.to_string());
            continue;
        }

        // Lowercase the first character
        let lowered_first: String = first_char.to_lowercase().collect();
        let fixed = format!("{prefix}{lowered_first}{}", &rest[first_char.len_utf8()..]);
        result.push(fixed);
    }

    result.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reporting test: run all cases in /tmp/pipeline_input.json through
    /// the full postprocess pipeline and write TSV results to
    /// /tmp/pipeline_output.tsv.  Run with:
    ///   cargo test --lib pipeline_report -- --nocapture --ignored
    #[test]
    #[ignore]
    fn pipeline_report() {
        use std::io::Write;
        let raw = std::fs::read_to_string("/tmp/pipeline_input.json")
            .expect("/tmp/pipeline_input.json not found — run scripts/test_full_pipeline.py first");
        let cases: Vec<(String, String)> = serde_json::from_str(&raw).unwrap();
        let mut out = std::fs::File::create("/tmp/pipeline_output.tsv").unwrap();
        for (input, category) in &cases {
            let result = postprocess(input);
            let norm_in  = input.trim().trim_end_matches(['.','!','?']).trim().to_lowercase();
            let norm_out = result.text.trim().trim_end_matches(['.','!','?']).trim().to_lowercase();
            let changed = norm_in != norm_out;
            writeln!(out, "{}\t{}\t{}\t{}", input, result.text, changed, category).unwrap();
        }
    }

    #[test]
    fn postprocess_empty() {
        assert_eq!(postprocess("").text, "");
        assert_eq!(postprocess("  ").text, "");
    }

    #[test]
    fn postprocess_basic_text() {
        let result = postprocess("hello world");
        assert!(result.text.starts_with('H'));
        assert!(result.text.ends_with('.'));
        assert!(!result.stages.is_empty());
        assert_eq!(result.stages[0].name, "Raw transcription");
    }

    #[test]
    fn postprocess_removes_fillers() {
        let result = postprocess("um hello uh world");
        assert!(!result.text.contains(" um "));
        assert!(!result.text.contains(" uh "));
    }

    #[test]
    fn final_cleanup_capitalizes() {
        assert_eq!(final_cleanup("hello world"), "Hello world.");
    }

    #[test]
    fn final_cleanup_preserves_punctuation() {
        assert_eq!(final_cleanup("Hello world!"), "Hello world!");
    }

    #[test]
    fn final_cleanup_collapses_spaces() {
        assert_eq!(final_cleanup("hello  world"), "Hello world.");
    }

    #[test]
    fn postprocess_counting_not_summed() {
        let result = postprocess("one, two, three, four, five");
        assert!(
            result.text.contains("1, 2, 3, 4, 5"),
            "Expected individual numbers, got: {}",
            result.text
        );
    }

    #[test]
    fn postprocess_preserves_trailing_punct() {
        let result = postprocess("I have three.");
        assert_eq!(result.text, "I have 3.");
    }

    #[test]
    fn join_chunks_strips_false_boundary() {
        // "one." + "chunk of text" -- next starts lowercase, strip period
        let chunks = vec!["if it's one.", "chunk of text or speech."];
        assert_eq!(join_chunks(&chunks), "if it's one chunk of text or speech.");
    }

    #[test]
    fn join_chunks_keeps_real_boundary() {
        // "one." + "Chunk" -- next starts uppercase, keep period
        let chunks = vec!["That was number one.", "Chunk two is next."];
        assert_eq!(join_chunks(&chunks), "That was number one. Chunk two is next.");
    }

    #[test]
    fn join_chunks_single() {
        assert_eq!(join_chunks(&vec!["hello world"]), "hello world");
    }

    #[test]
    fn join_chunks_empty() {
        let empty: Vec<&str> = vec![];
        assert_eq!(join_chunks(&empty), "");
    }

    #[test]
    fn join_chunks_three_chunks_mixed() {
        let chunks = vec!["First sentence.", "but continues here.", "New sentence."];
        assert_eq!(join_chunks(&chunks), "First sentence but continues here. New sentence.");
    }

    #[test]
    fn fix_caps_lowercases_mid_sentence() {
        assert_eq!(fix_mid_sentence_caps("assert what We want"), "assert what we want");
    }

    #[test]
    fn fix_caps_preserves_after_period() {
        assert_eq!(fix_mid_sentence_caps("Hello. World is great."), "Hello. World is great.");
    }

    #[test]
    fn fix_caps_preserves_after_exclamation() {
        assert_eq!(fix_mid_sentence_caps("Stop! That hurts."), "Stop! That hurts.");
    }

    #[test]
    fn fix_caps_preserves_after_question() {
        assert_eq!(fix_mid_sentence_caps("Really? That works."), "Really? That works.");
    }

    #[test]
    fn fix_caps_preserves_acronyms() {
        assert_eq!(fix_mid_sentence_caps("the API is working"), "the API is working");
        assert_eq!(fix_mid_sentence_caps("use DNS for that"), "use DNS for that");
    }

    #[test]
    fn fix_caps_preserves_i() {
        assert_eq!(fix_mid_sentence_caps("then I went home"), "then I went home");
    }

    #[test]
    fn fix_caps_preserves_i_contractions() {
        assert_eq!(fix_mid_sentence_caps("once I've settled the bucket"), "once I've settled the bucket");
        assert_eq!(fix_mid_sentence_caps("then I'd like to check"), "then I'd like to check");
        assert_eq!(fix_mid_sentence_caps("because I'll be there"), "because I'll be there");
        assert_eq!(fix_mid_sentence_caps("now I'm ready"), "now I'm ready");
    }

    #[test]
    fn fix_caps_preserves_alphanumeric_tokens() {
        assert_eq!(fix_mid_sentence_caps("settled the R2 bucket"), "settled the R2 bucket");
        assert_eq!(fix_mid_sentence_caps("connect to the C4 instance"), "connect to the C4 instance");
        assert_eq!(fix_mid_sentence_caps("check INT8 performance"), "check INT8 performance");
    }

    #[test]
    fn fix_caps_preserves_first_word() {
        assert_eq!(fix_mid_sentence_caps("Hello world"), "Hello world");
    }

    #[test]
    fn fix_caps_multiple_false_caps() {
        assert_eq!(
            fix_mid_sentence_caps("this Is Really Bad"),
            "this is really bad"
        );
    }

    #[test]
    fn fix_caps_mixed_real_and_false() {
        assert_eq!(
            fix_mid_sentence_caps("That works. And Then We left."),
            "That works. And then we left."
        );
    }

    #[test]
    fn final_cleanup_fixes_mid_sentence_caps() {
        assert_eq!(
            final_cleanup("assert what We want out the other side"),
            "Assert what we want out the other side."
        );
    }

    // -- Stage isolation: call each stage function independently --

    #[test]
    fn stage_filler_isolated() {
        assert_eq!(filler::remove_fillers("um hello uh world"), "hello world");
    }

    #[test]
    fn stage_itn_isolated() {
        assert_eq!(itn::normalize("I have twenty three dollars"), "I have $23");
    }

    #[test]
    fn stage_vocab_isolated() {
        // Built-in informal contractions: "gonna" -> "going to"
        assert_eq!(vocab::apply("I'm gonna leave"), "I'm going to leave");
    }

    #[test]
    fn stage_cleanup_isolated() {
        assert_eq!(final_cleanup("hello world"), "Hello world.");
    }

    // -- Pipeline integration: verify stage metadata --

    #[test]
    fn pipeline_result_has_expected_stage_names() {
        let result = postprocess("hello world");
        let names: Vec<&str> = result.stages.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names[0], "Raw transcription");
        assert_eq!(names[1], "Filler removal");
        assert_eq!(names[2], "ITN");
        assert_eq!(names[3], "Vocab");
        // Stage 4 is grammar (neural, unavailable, or skipped)
        assert!(names[4].starts_with("Grammar"), "expected Grammar stage, got: {}", names[4]);
        assert_eq!(names[5], "Cleanup");
    }

    #[test]
    fn pipeline_result_has_six_stages() {
        let result = postprocess("um hello world");
        assert_eq!(result.stages.len(), 6, "expected 6 stages (raw + 5 processing)");
    }

    #[test]
    fn pipeline_changed_flag_correct_for_filler() {
        let result = postprocess("um I think this works fine");
        let filler_stage = result.stages.iter().find(|s| s.name == "Filler removal").unwrap();
        assert!(filler_stage.changed, "filler stage should report changed=true when fillers removed");
    }

    #[test]
    fn pipeline_changed_flag_false_when_no_change() {
        let result = postprocess("I think this works fine");
        let filler_stage = result.stages.iter().find(|s| s.name == "Filler removal").unwrap();
        assert!(!filler_stage.changed, "filler stage should report changed=false when no fillers");
    }

    #[test]
    fn pipeline_itn_stage_transforms_numbers() {
        let result = postprocess("I have twenty three items in the list today");
        let itn_stage = result.stages.iter().find(|s| s.name == "ITN").unwrap();
        assert!(itn_stage.text.contains("23"), "ITN stage should convert 'twenty three' to '23', got: {}", itn_stage.text);
        assert!(itn_stage.changed);
    }

    #[test]
    fn pipeline_cleanup_stage_capitalizes() {
        let result = postprocess("hello world");
        let cleanup_stage = result.stages.iter().find(|s| s.name == "Cleanup").unwrap();
        assert!(cleanup_stage.text.starts_with('H'), "cleanup should capitalize, got: {}", cleanup_stage.text);
    }

    #[test]
    fn pipeline_raw_stage_unchanged() {
        let result = postprocess("some text here");
        assert!(!result.stages[0].changed);
        assert_eq!(result.stages[0].duration_ms, 0);
    }

    #[test]
    fn pipeline_grammar_skipped_for_short_text() {
        let result = postprocess("hello world");
        let grammar_stage = result.stages.iter().find(|s| s.name.starts_with("Grammar")).unwrap();
        assert!(grammar_stage.name.contains("skipped"),
            "2-word text should skip grammar, got stage: {}", grammar_stage.name);
        assert!(!grammar_stage.changed);
    }

    #[test]
    fn pipeline_total_ms_populated() {
        let result = postprocess("um I think this works fine");
        // total_ms should be >= 0 (can be 0 on fast machines)
        // Just verify it's present and the field is set
        assert!(result.total_ms < 5000, "pipeline should complete in under 5s");
    }

    #[test]
    fn pipeline_stages_text_flows_sequentially() {
        // Verify that each stage's output feeds into the next stage.
        // The raw stage text should match input, and the final stage
        // text should match the result.
        let input = "um I have twenty three items in the shopping list";
        let result = postprocess(input);
        assert_eq!(result.stages[0].text, input);
        assert_eq!(result.stages.last().unwrap().text, result.text);
    }

    #[test]
    fn pipeline_result_serialization_roundtrip() {
        let result = postprocess("hello world");
        let json = serde_json::to_string(&result).unwrap();
        let parsed: PipelineResult = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.text, result.text);
        assert_eq!(parsed.stages.len(), result.stages.len());
        assert_eq!(parsed.total_ms, result.total_ms);
    }
}
