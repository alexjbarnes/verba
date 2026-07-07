//! Phase-1 spike: prove the microphone and system-audio loopback streams
//! capture concurrently through the cpal 0.18 recorder.
//!
//!     cargo run --bin loopback_probe
//!
//! Opens the configured mic and the resolved loopback device (if any) for 5s
//! with VAD disabled (raw capture), then prints per-stream sample count and
//! RMS. Play some audio while it runs; both RMS values should be non-zero.
//! Loopback being Unsupported here (headless CI, no monitor source) is a
//! valid outcome — it prints the reason and captures mic-only.

use std::time::Duration;
use verba_rs_lib::meeting::loopback::{self, Loopback};
use verba_rs_lib::recorder::{AudioRecorder, DeviceSpec};

fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum_sq: f64 = samples.iter().map(|&s| (s as f64) * (s as f64)).sum();
    ((sum_sq / samples.len() as f64).sqrt()) as f32
}

/// Record one device for `secs` with VAD off, returning all captured samples.
fn capture(label: &str, spec: DeviceSpec, secs: u64) -> Result<Vec<f32>, String> {
    let rec = AudioRecorder::new_with_device(None, spec)?;
    rec.start().map_err(|e| format!("{label} start: {e}"))?;
    std::thread::sleep(Duration::from_secs(secs));
    rec.stop().map_err(|e| format!("{label} stop: {e}"))
}

fn main() {
    const SECS: u64 = 5;
    println!("Recording mic + loopback for {SECS}s — play some audio now...\n");

    // Mic and loopback run on independent recorder worker threads.
    let mic_handle = std::thread::spawn(|| capture("mic", DeviceSpec::ConfigInput, SECS));

    let loop_result = match loopback::resolve() {
        Loopback::Available(spec) => {
            println!("loopback: available ({spec:?})");
            capture("loopback", spec, SECS)
        }
        Loopback::Unsupported(reason) => {
            println!("loopback: unsupported — {reason}");
            Ok(Vec::new())
        }
    };

    let mic_result = mic_handle.join().expect("mic thread panicked");

    println!();
    match mic_result {
        Ok(s) => println!("mic:      {} samples, RMS {:.5}", s.len(), rms(&s)),
        Err(e) => println!("mic:      ERROR {e}"),
    }
    match loop_result {
        Ok(s) if s.is_empty() => println!("loopback: (no capture)"),
        Ok(s) => println!("loopback: {} samples, RMS {:.5}", s.len(), rms(&s)),
        Err(e) => println!("loopback: ERROR {e}"),
    }
}
