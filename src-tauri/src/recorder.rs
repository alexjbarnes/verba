use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::vad::Vad;

const TARGET_SAMPLE_RATE: i32 = 16_000;
const MAX_STREAM_RETRIES: u32 = 3;
const RETRY_DELAY_MS: u64 = 500;
/// Force-split VAD segments that exceed this duration so the background
/// transcriber can work on them during recording instead of waiting until stop.
const MAX_SEGMENT_SECS: f32 = 10.0;

/// Linear interpolation resampler (replaces sherpa_onnx::LinearResampler to
/// avoid pulling in session.cc.o which contains an unresolvable NNAPI symbol
/// on Android arm64).
struct LinearResampler {
    input_rate: f64,
    output_rate: f64,
    /// Fractional position in the input stream (carries over between calls).
    pos: f64,
    /// Last sample from previous chunk for interpolation across boundaries.
    last_sample: f32,
}

impl LinearResampler {
    fn create(input_rate: i32, output_rate: i32) -> Option<Self> {
        if input_rate <= 0 || output_rate <= 0 {
            return None;
        }
        Some(Self {
            input_rate: input_rate as f64,
            output_rate: output_rate as f64,
            pos: 0.0,
            last_sample: 0.0,
        })
    }

    fn resample(&mut self, input: &[f32], flush: bool) -> Vec<f32> {
        if input.is_empty() && !flush {
            return Vec::new();
        }

        let ratio = self.input_rate / self.output_rate;
        let input_len = input.len() as f64;
        let mut output = Vec::new();

        while self.pos < input_len {
            let idx = self.pos.floor() as usize;
            let frac = (self.pos - idx as f64) as f32;

            let a = if idx == 0 && self.pos < 1.0 {
                self.last_sample
            } else if idx < input.len() {
                input[idx]
            } else {
                break;
            };

            let b = if idx + 1 < input.len() {
                input[idx + 1]
            } else if flush {
                a
            } else {
                break;
            };

            output.push(a + frac * (b - a));
            self.pos += ratio;
        }

        self.pos -= input_len;
        if !input.is_empty() {
            self.last_sample = input[input.len() - 1];
        }

        output
    }
}

enum Cmd {
    Start {
        segment_tx: Option<mpsc::Sender<Vec<f32>>>,
    },
    Stop,
}

enum Event {
    Started,
    Stopped(Vec<f32>),
    Error(String),
}

/// Why the record loop exited.
enum LoopExit {
    UserStopped,
    Disconnected,
}

/// The underlying audio backend behind a `StreamHandle`. Almost always a
/// cpal input stream; on macOS, Meeting mode's system-audio path instead
/// holds a global CoreAudio tap (see `meeting/system_tap.rs`), which cpal
/// cannot express (it's not bound to any single device). Both variants are
/// held purely for RAII (drop = stop capture) and never read back out,
/// hence `allow(dead_code)`.
#[allow(dead_code)]
enum StreamKind {
    Cpal(cpal::Stream),
    #[cfg(target_os = "macos")]
    SystemTap(crate::meeting::system_tap::SystemTapHandle),
}

struct StreamHandle {
    stream: StreamKind,
    audio_rx: mpsc::Receiver<Vec<f32>>,
    resampler: Option<LinearResampler>,
    disconnected: Arc<AtomicBool>,
    /// VERBA_DEBUG_AUDIO=1 only: raw + resampled WAV dumps of this stream.
    debug: Option<crate::debug_wav::StreamDump>,
}

/// Which audio device a recorder should open. Resolved inside the worker
/// thread (a `cpal::Device` is not `Send`, so only this Send descriptor
/// crosses the thread boundary). `ConfigInput` preserves the dictation
/// behavior — the device_index in AppConfig, else the default input. The
/// loopback variants back Meeting mode's system-audio capture.
#[derive(Clone, Debug)]
pub enum DeviceSpec {
    /// AppConfig.device_index, falling back to the default input device.
    ConfigInput,
    /// A specific input device selected by its human name.
    InputByName(String),
    /// The default OUTPUT device opened as an input (WASAPI loopback on
    /// Windows, Core Audio process tap on macOS 14.6+).
    LoopbackDefaultOutput,
    /// A specific OUTPUT device (by human name) opened as an input for loopback
    /// capture. Meeting mode uses this when the user picks a non-default
    /// speaker/output to record. macOS/Windows only.
    LoopbackByName(String),
    /// macOS global system-audio process tap (captures all output,
    /// device-independent). Meeting-mode loopback on macOS.
    SystemTapGlobal,
}

/// Audio recorder with a dedicated worker thread.
pub struct AudioRecorder {
    cmd_tx: mpsc::Sender<Cmd>,
    event_rx: mpsc::Receiver<Event>,
    vad_path: Option<PathBuf>,
    device_spec: DeviceSpec,
}

impl AudioRecorder {
    /// Spawn the recorder worker on the configured input device. The VAD model
    /// path must point to a Silero ONNX file. If `vad_model` is None, VAD is
    /// disabled and all audio is kept.
    pub fn new(vad_model: Option<&Path>) -> Result<Self, String> {
        Self::new_with_device(vad_model, DeviceSpec::ConfigInput)
    }

    /// Spawn the recorder worker on an explicit device (Meeting mode uses this
    /// for the loopback stream).
    pub fn new_with_device(vad_model: Option<&Path>, device_spec: DeviceSpec) -> Result<Self, String> {
        let vad_path: Option<PathBuf> = vad_model.map(|p| p.to_path_buf());
        let (cmd_tx, event_rx) = Self::spawn_worker(vad_path.as_deref(), device_spec.clone())?;
        Ok(Self { cmd_tx, event_rx, vad_path, device_spec })
    }

    fn spawn_worker(
        vad_path: Option<&Path>,
        device_spec: DeviceSpec,
    ) -> Result<(mpsc::Sender<Cmd>, mpsc::Receiver<Event>), String> {
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();

        let vad_owned: Option<PathBuf> = vad_path.map(|p| p.to_path_buf());

        std::thread::Builder::new()
            .name("audio-recorder".into())
            .spawn(move || {
                let vad = match vad_owned {
                    Some(ref path) => match Vad::new(path) {
                        Ok(v) => {
                            let _ = ready_tx.send(Ok(()));
                            Some(v)
                        }
                        Err(e) => {
                            let _ = ready_tx.send(Err(e));
                            return;
                        }
                    },
                    None => {
                        let _ = ready_tx.send(Ok(()));
                        None
                    }
                };
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    worker(cmd_rx, event_tx, vad, device_spec);
                }));
                if let Err(panic_info) = result {
                    let msg = if let Some(s) = panic_info.downcast_ref::<String>() {
                        s.clone()
                    } else if let Some(s) = panic_info.downcast_ref::<&str>() {
                        s.to_string()
                    } else {
                        "unknown panic".to_string()
                    };
                    log::error!("Recorder worker thread panicked: {msg}");
                }
            })
            .map_err(|e| format!("spawn recorder thread: {e}"))?;

        ready_rx
            .recv()
            .map_err(|e| format!("recorder thread died: {e}"))??;

        Ok((cmd_tx, event_rx))
    }

    /// Returns true if the worker thread is still accepting commands.
    pub fn is_alive(&self) -> bool {
        // A zero-capacity test isn't possible with mpsc, but we can check
        // if the receiver side has hung up (which means the thread exited).
        // Sending on a disconnected channel returns Err, so we just test that.
        // We can't actually send a dummy command, so check event_rx instead:
        // if the worker's event_tx was dropped, try_recv returns Disconnected.
        matches!(
            self.event_rx.try_recv(),
            Err(mpsc::TryRecvError::Empty) | Ok(_)
        )
    }

    /// Respawn the worker thread. Returns Ok if the new thread started,
    /// or Err if it failed to start (e.g. VAD model missing).
    pub fn respawn(&mut self) -> Result<(), String> {
        log::info!("Respawning recorder worker thread");
        let (cmd_tx, event_rx) = Self::spawn_worker(self.vad_path.as_deref(), self.device_spec.clone())?;
        self.cmd_tx = cmd_tx;
        self.event_rx = event_rx;
        Ok(())
    }

    pub fn start(&self) -> Result<(), String> {
        self.cmd_tx
            .send(Cmd::Start { segment_tx: None })
            .map_err(|_| "recorder thread dead".to_string())?;

        match self.event_rx.recv() {
            Ok(Event::Started) => Ok(()),
            Ok(Event::Error(e)) => Err(e),
            Ok(Event::Stopped(_)) => Err("unexpected stop event".into()),
            Err(_) => Err("recorder thread dead".into()),
        }
    }

    /// Start recording and return a channel that receives completed VAD speech
    /// segments during recording. When stop() is called, the channel closes
    /// and stop() returns only the remaining tail audio.
    pub fn start_streaming(&self) -> Result<mpsc::Receiver<Vec<f32>>, String> {
        let (tx, rx) = mpsc::channel();
        self.cmd_tx
            .send(Cmd::Start { segment_tx: Some(tx) })
            .map_err(|_| "recorder thread dead".to_string())?;

        match self.event_rx.recv() {
            Ok(Event::Started) => Ok(rx),
            Ok(Event::Error(e)) => Err(e),
            Ok(Event::Stopped(_)) => Err("unexpected stop event".into()),
            Err(_) => Err("recorder thread dead".into()),
        }
    }

    pub fn stop(&self) -> Result<Vec<f32>, String> {
        self.cmd_tx
            .send(Cmd::Stop)
            .map_err(|_| "recorder thread dead".to_string())?;

        match self.event_rx.recv() {
            Ok(Event::Stopped(samples)) => Ok(samples),
            Ok(Event::Error(e)) => Err(e),
            Ok(Event::Started) => Err("unexpected start event".into()),
            Err(_) => Err("recorder thread dead".into()),
        }
    }
}

fn worker(cmd_rx: mpsc::Receiver<Cmd>, event_tx: mpsc::Sender<Event>, mut vad: Option<Vad>, device_spec: DeviceSpec) {
    while let Ok(cmd) = cmd_rx.recv() {
        match cmd {
            Cmd::Start { segment_tx } => {
                if let Some(ref mut v) = vad {
                    v.reset();
                }

                let mut sent_started = false;
                let mut all_samples: Vec<f32> = Vec::new();
                let mut last_err = String::new();
                let mut user_stopped = false;

                for attempt in 0..MAX_STREAM_RETRIES {
                    if attempt > 0 {
                        log::info!(
                            "Retrying stream (attempt {}/{})...",
                            attempt + 1,
                            MAX_STREAM_RETRIES,
                        );
                        std::thread::sleep(std::time::Duration::from_millis(RETRY_DELAY_MS));

                        // Check if user sent Stop while we were sleeping
                        if let Ok(Cmd::Stop) = cmd_rx.try_recv() {
                            log::info!("User stopped during retry wait");
                            user_stopped = true;
                            break;
                        }
                    }

                    match open_stream(&device_spec) {
                        Ok(mut handle) => {
                            if !sent_started {
                                let _ = event_tx.send(Event::Started);
                                sent_started = true;
                            }

                            let (samples, exit_reason) = record_loop(
                                &cmd_rx,
                                &handle.audio_rx,
                                &mut handle.resampler,
                                &mut vad,
                                &handle.disconnected,
                                &segment_tx,
                                &mut handle.debug,
                            );
                            drop(handle.stream);

                            match exit_reason {
                                LoopExit::UserStopped => {
                                    all_samples = samples;
                                    user_stopped = true;
                                    last_err.clear();
                                    break;
                                }
                                LoopExit::Disconnected => {
                                    if !samples.is_empty() {
                                        log::info!(
                                            "Stream disconnected after capturing {:.1}s",
                                            samples.len() as f32 / TARGET_SAMPLE_RATE as f32
                                        );
                                        all_samples = samples;
                                        last_err.clear();
                                        break;
                                    }
                                    log::warn!(
                                        "Stream disconnected with no audio (attempt {})",
                                        attempt + 1
                                    );
                                    last_err = "audio stream keeps disconnecting".into();
                                }
                            }
                        }
                        Err(e) => {
                            log::error!("Failed to open audio stream: {e}");
                            last_err = e;
                        }
                    }
                }

                if !sent_started {
                    let _ = event_tx.send(Event::Error(last_err));
                } else if user_stopped {
                    let _ = event_tx.send(Event::Stopped(all_samples));
                } else {
                    // All retries failed
                    log::error!("All {MAX_STREAM_RETRIES} stream attempts failed");
                    let _ = event_tx.send(Event::Stopped(all_samples));
                }
            }
            Cmd::Stop => {
                let _ = event_tx.send(Event::Stopped(Vec::new()));
            }
        }
    }
}

/// Open the mic and return the stream handle.
/// Resolve the cpal device for a `DeviceSpec`. Runs on the recorder worker
/// thread (the returned `Device` is not `Send`). Loopback opens the default
/// OUTPUT device as an input, which cpal maps to WASAPI loopback / a Core
/// Audio tap / a monitor source depending on platform.
fn resolve_device(host: &cpal::Host, spec: &DeviceSpec) -> Result<cpal::Device, String> {
    match spec {
        DeviceSpec::ConfigInput => {
            let cfg = crate::config::AppConfig::load();
            if cfg.device_index >= 0 {
                let idx = cfg.device_index as usize;
                if let Ok(inputs) = host.input_devices() {
                    for (i, dev) in inputs.enumerate() {
                        if i == idx {
                            if let Ok(desc) = dev.description() {
                                log::info!("Using configured input device [{idx}]: {}", desc.name());
                            }
                            return Ok(dev);
                        }
                    }
                }
                log::warn!("Configured device_index {idx} not found, falling back to default");
            }
            let dev = host.default_input_device().ok_or("no input device available")?;
            if let Ok(desc) = dev.description() {
                log::info!("Using system default input device: {}", desc.name());
            }
            Ok(dev)
        }
        DeviceSpec::InputByName(name) => {
            if let Ok(inputs) = host.input_devices() {
                for dev in inputs {
                    if dev.description().map(|d| d.name() == name).unwrap_or(false) {
                        log::info!("Using named input device: {name}");
                        return Ok(dev);
                    }
                }
            }
            Err(format!("input device not found: {name}"))
        }
        DeviceSpec::LoopbackDefaultOutput => {
            let dev = host
                .default_output_device()
                .ok_or("no output device available for loopback")?;
            if let Ok(desc) = dev.description() {
                log::info!("Loopback: capturing default output device: {}", desc.name());
            }
            Ok(dev)
        }
        DeviceSpec::LoopbackByName(name) => {
            if let Ok(outputs) = host.output_devices() {
                for dev in outputs {
                    if dev.description().map(|d| d.name() == name).unwrap_or(false) {
                        log::info!("Loopback: capturing output device: {name}");
                        return Ok(dev);
                    }
                }
            }
            log::warn!("Loopback output '{name}' not found, using default output");
            host.default_output_device()
                .ok_or_else(|| format!("output device not found: {name}"))
        }
        // Handled before this function is called (see open_stream); this
        // arm only exists so the match stays exhaustive on all platforms.
        DeviceSpec::SystemTapGlobal => Err("system tap is not a cpal device".into()),
    }
}

fn open_stream(spec: &DeviceSpec) -> Result<StreamHandle, String> {
    // macOS system audio bypasses cpal entirely: a global CoreAudio process
    // tap isn't bound to any single device, so there's no cpal Device to
    // resolve/configure below. Everything else (mic input, Windows/Linux
    // loopback) stays on the normal cpal path.
    #[cfg(target_os = "macos")]
    if matches!(spec, DeviceSpec::SystemTapGlobal) {
        let (audio_tx, audio_rx) = mpsc::channel::<Vec<f32>>();
        let (handle, rate, _channels) = crate::meeting::system_tap::start_global_tap(audio_tx)?;
        let resampler = if rate as i32 != TARGET_SAMPLE_RATE {
            Some(
                LinearResampler::create(rate as i32, TARGET_SAMPLE_RATE)
                    .ok_or("failed to create resampler")?,
            )
        } else {
            None
        };
        return Ok(StreamHandle {
            stream: StreamKind::SystemTap(handle),
            audio_rx,
            resampler,
            disconnected: Arc::new(AtomicBool::new(false)),
            debug: crate::debug_wav::stream_dump("system", rate),
        });
    }

    let t = std::time::Instant::now();
    let host = cpal::default_host();
    let device = resolve_device(&host, spec)?;

    // Loopback captures an OUTPUT device, so its stream shape comes from the
    // output config; every other spec is a normal input.
    let supported = match spec {
        DeviceSpec::LoopbackDefaultOutput | DeviceSpec::LoopbackByName(_) => device
            .default_output_config()
            .map_err(|e| format!("no output config for loopback: {e}"))?,
        _ => device
            .default_input_config()
            .map_err(|e| format!("no input config: {e}"))?,
    };

    let device_rate = supported.sample_rate() as i32;
    let channels = supported.channels() as usize;

    log::info!(
        "Opening stream: {device_rate}Hz, {channels}ch, {:?}",
        supported.sample_format()
    );

    let mut stream_config: cpal::StreamConfig = supported.clone().into();

    // Request a small buffer for lower latency. 256 frames at 48kHz is ~5ms,
    // which helps reduce the time between stream.play() and first audio callback.
    // If the device doesn't support our target, clamp to its range.
    const TARGET_BUFFER_FRAMES: u32 = 256;
    match supported.buffer_size() {
        cpal::SupportedBufferSize::Range { min, max } => {
            let size = TARGET_BUFFER_FRAMES.clamp(*min, *max);
            stream_config.buffer_size = cpal::BufferSize::Fixed(size);
            log::info!("Buffer size: {size} frames (range {min}..{max})");
        }
        cpal::SupportedBufferSize::Unknown => {
            log::info!("Buffer size: using platform default (range unknown)");
        }
    }

    let resampler = if device_rate != TARGET_SAMPLE_RATE {
        Some(
            LinearResampler::create(device_rate, TARGET_SAMPLE_RATE)
                .ok_or("failed to create resampler")?,
        )
    } else {
        None
    };

    let (audio_tx, audio_rx) = mpsc::channel::<Vec<f32>>();
    let disconnected = Arc::new(AtomicBool::new(false));
    let disc_flag = disconnected.clone();

    // Diagnostic level metering: log peak amplitude ~once/second per stream so
    // we can tell whether a stream (especially the Meeting loopback tap) is
    // delivering real audio, pure silence (peak ~0), or nothing (no lines).
    let meter_label = match spec {
        DeviceSpec::LoopbackDefaultOutput => "loopback:default-output".to_string(),
        DeviceSpec::LoopbackByName(n) => format!("loopback:{n}"),
        DeviceSpec::InputByName(n) => format!("mic:{n}"),
        DeviceSpec::ConfigInput => "mic:config".to_string(),
        // Unreachable: open_stream returns before here on macOS (see the
        // early branch above); kept so this match stays exhaustive.
        DeviceSpec::SystemTapGlobal => "system-tap".to_string(),
    };
    let mut meter_frames: usize = 0;
    let mut meter_peak: f32 = 0.0;
    // Opt-in (VERBA_AUDIO_METER): keep this diagnostic logging quiet by default.
    let meter_on = std::env::var_os("VERBA_AUDIO_METER").is_some();

    let stream = device
        .build_input_stream(
            stream_config,
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                if meter_on {
                    for &s in data {
                        let a = s.abs();
                        if a > meter_peak {
                            meter_peak = a;
                        }
                    }
                    meter_frames += if channels > 0 { data.len() / channels } else { data.len() };
                    if meter_frames >= device_rate.max(1) as usize {
                        log::info!(
                            "audio level [{meter_label}]: peak {meter_peak:.4} over ~{}ms",
                            meter_frames * 1000 / device_rate.max(1) as usize
                        );
                        meter_frames = 0;
                        meter_peak = 0.0;
                    }
                }
                let mono: Vec<f32> = if channels > 1 {
                    data.chunks(channels)
                        .map(|frame| frame.iter().sum::<f32>() / channels as f32)
                        .collect()
                } else {
                    data.to_vec()
                };
                let _ = audio_tx.send(mono);
            },
            move |err| {
                log::error!("input stream error: {err}");
                disc_flag.store(true, Ordering::SeqCst);
            },
            None,
        )
        .map_err(|e| format!("build input stream: {e}"))?;

    stream
        .play()
        .map_err(|e| format!("start stream: {e}"))?;

    log::info!("Recording at {device_rate}Hz, {channels}ch -> {TARGET_SAMPLE_RATE}Hz (stream opened in {}ms)", t.elapsed().as_millis());

    let debug_label = match spec {
        DeviceSpec::ConfigInput | DeviceSpec::InputByName(_) => "mic",
        _ => "system",
    };

    Ok(StreamHandle {
        stream: StreamKind::Cpal(stream),
        audio_rx,
        resampler,
        disconnected,
        debug: crate::debug_wav::stream_dump(debug_label, device_rate as u32),
    })
}

/// Record audio until a Stop command arrives or the stream disconnects.
fn record_loop(
    cmd_rx: &mpsc::Receiver<Cmd>,
    audio_rx: &mpsc::Receiver<Vec<f32>>,
    resampler: &mut Option<LinearResampler>,
    vad: &mut Option<Vad>,
    disconnected: &AtomicBool,
    segment_tx: &Option<mpsc::Sender<Vec<f32>>>,
    debug: &mut Option<crate::debug_wav::StreamDump>,
) -> (Vec<f32>, LoopExit) {
    let mut all_samples: Vec<f32> = Vec::new();
    let mut speech_samples: Vec<f32> = Vec::new();
    let mut total_samples: usize = 0;
    let mut exit_reason = LoopExit::Disconnected;
    let max_segment_samples = (TARGET_SAMPLE_RATE as f32 * MAX_SEGMENT_SECS) as usize;
    let mut samples_since_segment: usize = 0;
    let mut segments_sent: usize = 0;

    loop {
        // Check for stop command
        match cmd_rx.try_recv() {
            Ok(Cmd::Stop) => {
                // Drain any audio buffers still in the channel so we don't
                // lose trailing consonants or word endings.
                while let Ok(mono) = audio_rx.try_recv() {
                    if let Some(d) = debug.as_mut() {
                        d.write_raw(&mono);
                    }
                    let resampled = match resampler {
                        Some(ref mut r) => r.resample(&mono, false),
                        None => mono,
                    };
                    if let Some(d) = debug.as_mut() {
                        d.write_16k(&resampled);
                    }
                    match vad {
                        Some(ref mut v) => {
                            if segments_sent == 0 {
                                all_samples.extend_from_slice(&resampled);
                            }
                            total_samples += resampled.len();
                            if let Some(segment) = v.accept(&resampled) {
                                if let Some(ref tx) = segment_tx {
                                    let _ = tx.send(segment);
                                    segments_sent += 1;
                                    // First segment sent: drop fallback buffer
                                    if segments_sent == 1 {
                                        all_samples = Vec::new();
                                    }
                                } else {
                                    speech_samples.extend_from_slice(&segment);
                                }
                            }
                        }
                        None => {
                            all_samples.extend_from_slice(&resampled);
                            total_samples += resampled.len();
                        }
                    }
                }
                exit_reason = LoopExit::UserStopped;
                break;
            }
            Ok(Cmd::Start { .. }) => {}
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => {
                exit_reason = LoopExit::UserStopped;
                break;
            }
        }

        // Check for stream disconnect
        if disconnected.load(Ordering::SeqCst) {
            // Drain any remaining buffered audio
            while let Ok(mono) = audio_rx.try_recv() {
                if let Some(d) = debug.as_mut() {
                    d.write_raw(&mono);
                }
                let resampled = match resampler {
                    Some(ref mut r) => r.resample(&mono, false),
                    None => mono,
                };
                if let Some(d) = debug.as_mut() {
                    d.write_16k(&resampled);
                }
                total_samples += resampled.len();
                if segments_sent == 0 {
                    all_samples.extend_from_slice(&resampled);
                }
            }
            log::warn!("Stream disconnected, exiting record loop");
            exit_reason = LoopExit::Disconnected;
            break;
        }

        match audio_rx.recv_timeout(std::time::Duration::from_millis(50)) {
            Ok(mono) => {
                if let Some(d) = debug.as_mut() {
                    d.write_raw(&mono);
                }
                let resampled = match resampler {
                    Some(ref mut r) => r.resample(&mono, false),
                    None => mono,
                };
                if let Some(d) = debug.as_mut() {
                    d.write_16k(&resampled);
                }

                match vad {
                    Some(ref mut v) => {
                        if segments_sent == 0 {
                            all_samples.extend_from_slice(&resampled);
                        }
                        total_samples += resampled.len();
                        samples_since_segment += resampled.len();

                        if let Some(segment) = v.accept(&resampled) {
                            samples_since_segment = 0;
                            if let Some(ref tx) = segment_tx {
                                let _ = tx.send(segment);
                                segments_sent += 1;
                                if segments_sent == 1 {
                                    all_samples = Vec::new();
                                }
                            } else {
                                speech_samples.extend_from_slice(&segment);
                            }
                        } else if samples_since_segment > max_segment_samples {
                            // Force-split: flush VAD mid-speech so the
                            // background transcriber can start working.
                            log::info!(
                                "Force-splitting at {:.1}s of continuous speech",
                                samples_since_segment as f32 / TARGET_SAMPLE_RATE as f32
                            );
                            if let Some(segment) = v.flush() {
                                if let Some(ref tx) = segment_tx {
                                    let _ = tx.send(segment);
                                    segments_sent += 1;
                                } else {
                                    speech_samples.extend_from_slice(&segment);
                                }
                            }
                            v.reset();
                            samples_since_segment = 0;
                        }
                    }
                    None => {
                        all_samples.extend_from_slice(&resampled);
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    // Flush resampler tail
    if let Some(ref mut r) = resampler {
        let tail = r.resample(&[], true);
        if !tail.is_empty() {
            if let Some(d) = debug.as_mut() {
                d.write_16k(&tail);
            }
            match vad {
                Some(ref mut v) => {
                    if let Some(segment) = v.accept(&tail) {
                        if let Some(ref tx) = segment_tx {
                            let _ = tx.send(segment);
                        } else {
                            speech_samples.extend_from_slice(&segment);
                        }
                    }
                }
                None => {
                    all_samples.extend_from_slice(&tail);
                }
            }
        }
    }

    // Flush VAD
    if let Some(ref mut v) = vad {
        if let Some(segment) = v.flush() {
            speech_samples.extend_from_slice(&segment);
        }
    }

    let total_secs = total_samples as f32 / TARGET_SAMPLE_RATE as f32;

    let samples = if segment_tx.is_some() && vad.is_some() {
        // Streaming mode: segments were sent via channel during recording.
        // Only the flushed tail remains in speech_samples.
        let tail = speech_samples.len() as f32 / TARGET_SAMPLE_RATE as f32;
        if speech_samples.is_empty() && segments_sent == 0 {
            // VAD detected no speech at all, but there might be audio the
            // VAD missed (short utterances, noisy environments). Fall back
            // to raw audio so the transcriber gets a chance.
            let min_fallback = (TARGET_SAMPLE_RATE as f32 * 0.5) as usize;
            if all_samples.len() > min_fallback {
                log::info!("Streaming: VAD found no speech in {total_secs:.1}s, falling back to raw audio");
                all_samples
            } else {
                log::info!("Streaming: no speech in {total_secs:.1}s (too short for fallback)");
                speech_samples
            }
        } else if speech_samples.is_empty() {
            log::info!("Streaming: all {total_secs:.1}s sent as {segments_sent} segments, no tail");
            speech_samples
        } else {
            log::info!("Streaming: {tail:.1}s tail remaining from {total_secs:.1}s total ({segments_sent} segments sent)");
            speech_samples
        }
    } else if vad.is_some() && !speech_samples.is_empty() {
        log::info!(
            "VAD kept {:.1}s of speech from {total_secs:.1}s total",
            speech_samples.len() as f32 / TARGET_SAMPLE_RATE as f32,
        );
        speech_samples
    } else {
        log::info!("Recorded {total_secs:.1}s (no VAD or no speech detected)");
        all_samples
    };

    (samples, exit_reason)
}
