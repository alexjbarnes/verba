//! Grammar-stage probe for the STT round-trip harness: run raw ASR
//! transcripts through the real postprocess pipeline and dump each
//! PipelineResult (stage snapshots + per-sentence router scores) as JSON
//! for scripts/stt_grammar_probe.py to classify.
//!
//!     ORT_DYLIB_PATH=<libonnxruntime.so> \
//!         cargo run --bin grammar_probe < raw.json > results.json
//!
//! Input: JSON array of strings. Requires the grammar models bundled
//! (grammar_neural_bundled set by build.rs); exits 2 otherwise.

use std::io::Read;

fn main() {
    verba_rs_lib::postprocess::grammar_neural::init_global();
    if verba_rs_lib::postprocess::grammar_neural::global().is_none() {
        eprintln!("neural grammar unavailable (models not bundled or failed to load)");
        std::process::exit(2);
    }
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input).expect("read stdin");
    let texts: Vec<String> = serde_json::from_str(&input).expect("parse input JSON");
    let results: Vec<verba_rs_lib::postprocess::PipelineResult> = texts
        .iter()
        .map(|t| verba_rs_lib::postprocess::postprocess(t))
        .collect();
    println!("{}", serde_json::to_string(&results).unwrap());
}
