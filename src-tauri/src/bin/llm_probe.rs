//! Phase-2 spike: prove decoder-only LLM generation through the ort KV-cache
//! loop before building the summarization pipeline on top of it.
//!
//!     SHERPA_ONNX_LIB_DIR=<abs .desktop-deps/sherpa-onnx/lib> \
//!     ORT_DYLIB_PATH=<abs libonnxruntime.so.x.y.z> \
//!     LD_LIBRARY_PATH=<same dir> \
//!         cargo run --release --bin llm_probe -- <model_dir> ["prompt"]
//!
//! `<model_dir>` holds model.onnx + tokenizer.json + llm_config.json. Prints
//! the session's declared I/O first (the ground truth the runner probes),
//! then generates and reports prefill/decode timing.

use std::path::PathBuf;

use verba_rs_lib::meeting::summarize::LlmRunner;

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = PathBuf::from(args.next().unwrap_or_else(|| {
        eprintln!("usage: llm_probe <model_dir> [prompt]");
        std::process::exit(2);
    }));
    let user_prompt = args.next().unwrap_or_else(|| {
        "Summarize this meeting note in two sentences: We agreed to ship the beta on Friday. \
         Sam owns the release notes, Priya owns the rollback plan, and we'll skip the demo \
         until the following sprint."
            .to_string()
    });

    let t = std::time::Instant::now();
    let runner = match LlmRunner::load(&dir) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("load failed: {e}");
            std::process::exit(1);
        }
    };
    println!("loaded in {:.1}s\n", t.elapsed().as_secs_f32());

    match runner.generate(
        "You are a concise assistant that writes meeting notes.",
        &user_prompt,
        200,
    ) {
        Ok(gen) => {
            println!("--- output ---\n{}\n--------------", gen.text);
            let tok_per_s = if gen.decode_ms > 0 {
                gen.new_tokens as f64 / (gen.decode_ms as f64 / 1000.0)
            } else {
                f64::INFINITY
            };
            println!(
                "prompt {} tok, prefill {}ms | generated {} tok in {}ms ({:.1} tok/s)",
                gen.prompt_tokens, gen.prefill_ms, gen.new_tokens, gen.decode_ms, tok_per_s
            );
        }
        Err(e) => {
            eprintln!("generate failed: {e}");
            std::process::exit(1);
        }
    }
}
