//! Pipeline evaluation harness.
//!
//! Runs the full post-processing pipeline against collected test cases and
//! reports diffs between expected and actual output at each stage.
//!
//! Usage:
//!   cargo run --bin eval_pipeline                              # run all test cases
//!   cargo run --bin eval_pipeline -- --input cases.json        # custom input file
//!   cargo run --bin eval_pipeline -- --text "some raw text"    # single sentence
//!   cargo run --bin eval_pipeline -- --route "check this"      # router score only
//!
//! The grammar neural stage only activates once the dictation package's
//! grammar models have been downloaded to the runtime models directory.

use std::path::PathBuf;
use verba_rs_lib::postprocess;

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp(None)
        .init();

    postprocess::grammar_neural::init_global();
    postprocess::warm_up();

    let args: Vec<String> = std::env::args().collect();
    let mode = parse_args(&args);

    match mode {
        Mode::File(path) => run_file(&path),
        Mode::Text(text) => run_single(&text),
        Mode::Route(text) => run_route(&text),
    }
}

enum Mode {
    File(PathBuf),
    Text(String),
    Route(String),
}

fn parse_args(args: &[String]) -> Mode {
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--input" | "-i" => {
                i += 1;
                return Mode::File(PathBuf::from(&args[i]));
            }
            "--text" | "-t" => {
                i += 1;
                return Mode::Text(args[i..].join(" "));
            }
            "--route" | "-r" => {
                i += 1;
                return Mode::Route(args[i..].join(" "));
            }
            "--help" | "-h" => {
                print_usage();
                std::process::exit(0);
            }
            "--" => {
                i += 1;
                continue;
            }
            _ => {
                eprintln!("Unknown argument: {}", args[i]);
                print_usage();
                std::process::exit(1);
            }
        }
    }
    // Default: read the standard test cases file
    let default_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("scripts/data/corrector_test_cases.json");
    Mode::File(default_path)
}

fn print_usage() {
    eprintln!("Usage:");
    eprintln!("  eval_pipeline                           Run all test cases from default file");
    eprintln!("  eval_pipeline --input <file>            Run test cases from a JSON file");
    eprintln!("  eval_pipeline --text <raw text>         Run pipeline on a single input");
    eprintln!("  eval_pipeline --route <text>            Show router score only (no correction)");
}

fn run_single(text: &str) {
    println!("Input: {text}");
    println!("{}", "-".repeat(70));

    let result = postprocess::postprocess(text);

    for stage in &result.stages {
        let marker = if stage.changed { "CHANGED" } else { "      " };
        println!("[{marker}] {} ({}ms)", stage.name, stage.duration_ms);
        println!("         {}", stage.text);
        if let Some(score) = stage.grammar_score {
            println!("         score: {score:.4}");
        }
        for s in &stage.grammar_sentences {
            let flag = if s.corrected { "corrected" } else { "kept" };
            println!("         sentence ({flag}, score={:?}): {}", s.score, s.text);
        }
    }

    println!("{}", "-".repeat(70));
    println!("Output: {} ({}ms total)", result.text, result.total_ms);
}

fn run_route(text: &str) {
    let checker = postprocess::grammar_neural::global();
    match checker {
        Some(c) => {
            let (needs_correction, score) = c.route(text);
            println!("Input:  {text}");
            println!("Score:  {score:?}");
            println!("Route:  {}", if needs_correction { "CORRECT" } else { "KEEP" });
            if needs_correction {
                let (corrected, _) = c.apply(text);
                println!("Output: {corrected}");
            }
        }
        None => {
            eprintln!("Neural grammar not available (models not downloaded).");
            eprintln!("Install the dictation package from the app, then rerun.");
            std::process::exit(1);
        }
    }
}

#[derive(serde::Deserialize)]
struct TestCase {
    text: String,
    pipeline_stages: Vec<postprocess::PipelineStage>,
    #[serde(default)]
    model_id: String,
}

fn run_file(path: &PathBuf) {
    let raw = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Failed to read {}: {e}", path.display());
            std::process::exit(1);
        }
    };

    let cases: Vec<TestCase> = match serde_json::from_str(&raw) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Failed to parse {}: {e}", path.display());
            std::process::exit(1);
        }
    };

    println!("Running {} test cases from {}", cases.len(), path.display());
    println!("{}", "=".repeat(70));

    let mut pass = 0;
    let mut fail = 0;

    for (i, case) in cases.iter().enumerate() {
        let raw_text = case.pipeline_stages.first()
            .map(|s| s.text.as_str())
            .unwrap_or("");
        if raw_text.is_empty() {
            continue;
        }

        let result = postprocess::postprocess(raw_text);
        let expected = &case.text;
        let matched = result.text == *expected;

        if matched {
            pass += 1;
            println!("[PASS] Case {} ({})", i + 1, truncate(&result.text, 60));
        } else {
            fail += 1;
            println!("[DIFF] Case {} (model: {})", i + 1, case.model_id);
            println!("  Raw:      {}", truncate(raw_text, 80));
            println!("  Expected: {}", truncate(expected, 80));
            println!("  Actual:   {}", truncate(&result.text, 80));

            // Show per-stage diffs
            for (j, stage) in result.stages.iter().enumerate() {
                let expected_stage = case.pipeline_stages.get(j);
                let expected_text = expected_stage.map(|s| s.text.as_str()).unwrap_or("");
                if stage.text != expected_text {
                    println!("  Stage '{}': differs", stage.name);
                    println!("    expected: {}", truncate(expected_text, 70));
                    println!("    actual:   {}", truncate(&stage.text, 70));
                }
            }
        }
    }

    println!("{}", "=".repeat(70));
    println!("{pass} passed, {fail} diverged, {} total", pass + fail);

    if fail > 0 {
        std::process::exit(1);
    }
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        s
    } else {
        &s[..max]
    }
}
