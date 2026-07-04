//! Round-trip harness, synthesis half: run text through the REAL app
//! pipeline (normalization, GB dictionary, overrides, OOV handling, RP
//! transform, the actual ONNX voice) and write a WAV plus the expected
//! spoken text for the ASR alignment step (scripts/tts_roundtrip.py).
//!
//!     ORT_DYLIB_PATH=<libonnxruntime.so> cargo run --bin tts_roundtrip -- \
//!         MODEL.onnx CONFIG.onnx.json TEXT.txt OUT.wav OUT.json [sid]
//!
//! Requires the host link stubs (host_stubs.cpp) — the same change that lets
//! `cargo test` run.

use std::io::Write;

fn write_wav16(path: &str, sample_rate: u32, pcm: &[f32]) -> std::io::Result<()> {
    let mut f = std::fs::File::create(path)?;
    let data_len = (pcm.len() * 2) as u32;
    f.write_all(b"RIFF")?;
    f.write_all(&(36 + data_len).to_le_bytes())?;
    f.write_all(b"WAVEfmt ")?;
    f.write_all(&16u32.to_le_bytes())?;
    f.write_all(&1u16.to_le_bytes())?; // PCM
    f.write_all(&1u16.to_le_bytes())?; // mono
    f.write_all(&sample_rate.to_le_bytes())?;
    f.write_all(&(sample_rate * 2).to_le_bytes())?;
    f.write_all(&2u16.to_le_bytes())?;
    f.write_all(&16u16.to_le_bytes())?;
    f.write_all(b"data")?;
    f.write_all(&data_len.to_le_bytes())?;
    let mut buf = Vec::with_capacity(pcm.len() * 2);
    for &s in pcm {
        buf.extend_from_slice(&((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes());
    }
    f.write_all(&buf)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 5 {
        eprintln!("usage: tts_roundtrip MODEL.onnx CONFIG.json TEXT.txt OUT.wav OUT.json [sid]");
        std::process::exit(2);
    }
    let (model, config, text_path, out_wav, out_json) =
        (&args[0], &args[1], &args[2], &args[3], &args[4]);
    let sid: i32 = args.get(5).and_then(|s| s.parse().ok()).unwrap_or(0);

    let text = std::fs::read_to_string(text_path).expect("read text");
    let mut engine =
        verba_rs_lib::piper::PiperEngine::load(model, config, 4).expect("load engine");
    let sr = engine.sample_rate() as u32;

    // Paragraph-sized chunks keep memory bounded; synth_chunk handles the
    // per-segment pause splitting internally, same as the app.
    let mut pcm: Vec<f32> = Vec::new();
    let mut spoken = String::new();
    let mut para_meta = Vec::new();
    let paras: Vec<&str> = text.split("\n\n").filter(|p| !p.trim().is_empty()).collect();
    for (i, para) in paras.iter().enumerate() {
        let start = pcm.len();
        let (audio, _spans) = engine
            .synth_chunk(para, sid, 1.0)
            .unwrap_or_else(|e| panic!("synth para {i}: {e}"));
        pcm.extend_from_slice(&audio);
        let end = pcm.len();
        // Half a second between paragraphs, matching the app's paragraph gap.
        pcm.extend(std::iter::repeat(0.0f32).take(sr as usize / 2));
        let spoken_para = verba_rs_lib::piper::spoken_text(para);
        spoken.push_str(&spoken_para);
        spoken.push('\n');
        para_meta.push(serde_json::json!({
            "text": para.trim(),
            "spoken": spoken_para,
            "start": start,
            "end": end,
        }));
        eprintln!("[{}/{}] {:.1}s total", i + 1, paras.len(), pcm.len() as f32 / sr as f32);
    }

    write_wav16(out_wav, sr, &pcm).expect("write wav");
    let meta = serde_json::json!({ "sample_rate": sr, "spoken": spoken, "paragraphs": para_meta });
    std::fs::write(out_json, serde_json::to_string(&meta).unwrap()).expect("write json");
    eprintln!("wrote {out_wav} ({:.1}s) and {out_json}", pcm.len() as f32 / sr as f32);
}
