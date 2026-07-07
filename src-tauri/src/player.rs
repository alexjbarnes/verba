use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

const PRE_BUFFER_SECS: usize = 4;
// Once starved, wait until this many seconds of audio are queued ahead of the
// cursor before resuming. Hysteresis (enter on starve, exit on margin) stops
// the player thrashing between play and silence near the generation frontier.
// Larger margin = fewer but longer pauses, which is less disruptive to listen
// to than constant micro-stutters when generation runs below real-time.
const REBUFFER_MARGIN_SECS: usize = 3;

#[derive(Clone, Copy)]
pub struct PlaybackPosition {
    pub cursor: usize,
    pub buffered: usize,
    pub paused: bool,
    pub gen_done: bool,
    pub estimated: usize,
    pub rebuffering: bool,
}

struct SharedState {
    cursor: AtomicUsize,
    /// Samples written into the playback buffer by the feeder (single producer).
    /// Published with Release; the audio callback reads it with Acquire and only
    /// ever reads buffer indices strictly below it.
    buffered: AtomicUsize,
    paused: AtomicBool,
    active: AtomicBool,
    gen_done: AtomicBool,
    seek_to: AtomicI64,
    estimated_samples: usize,
    rebuffering: AtomicBool,
    /// Count of audio callbacks that exceeded their time budget (diagnostic).
    overruns: AtomicUsize,
}

/// Raw pointer to the playback buffer, shared between the feeder (writes ahead
/// of `buffered`) and the audio callback (reads behind it). Safe under the SPSC
/// discipline: the two never touch the same index, and `buffered`'s Release/
/// Acquire ordering publishes written samples. The backing Vec outlives the
/// stream (dropped after it), so the pointer is always valid in the callback.
#[derive(Clone, Copy)]
struct SendPtr(*mut f32);
unsafe impl Send for SendPtr {}

impl SendPtr {
    /// Pointer `offset` samples into the buffer. Taking `self` by value forces
    /// the audio closure to capture the whole `SendPtr` (which is `Send`) rather
    /// than the bare `*mut f32` field under Rust 2021 disjoint captures.
    #[inline]
    fn at(self, offset: usize) -> *mut f32 {
        unsafe { self.0.add(offset) }
    }
}

/// Drain the channel into the buffer starting at `write_pos`, returning how many
/// samples were written (clamped to remaining capacity). Runs on the feeder
/// thread only — never on the realtime audio callback.
fn drain_into(rx: &mpsc::Receiver<Vec<f32>>, ptr: *mut f32, cap: usize, write_pos: usize) -> usize {
    let mut wp = write_pos;
    while let Ok(chunk) = rx.try_recv() {
        let space = cap - wp;
        if space == 0 {
            log::warn!("TTS playback buffer full ({cap} samples) — dropping tail");
            break;
        }
        let n = chunk.len().min(space);
        unsafe { std::ptr::copy_nonoverlapping(chunk.as_ptr(), ptr.add(wp), n); }
        wp += n;
    }
    wp - write_pos
}

pub struct AudioPlayer {
    shared: Arc<SharedState>,
    tx: mpsc::Sender<Vec<f32>>,
}

impl AudioPlayer {
    pub fn new(
        sample_rate: i32,
        estimated_samples: usize,
        on_position: Option<Box<dyn Fn(PlaybackPosition) + Send + 'static>>,
    ) -> Result<Self, String> {
        let shared = Arc::new(SharedState {
            cursor: AtomicUsize::new(0),
            buffered: AtomicUsize::new(0),
            paused: AtomicBool::new(false),
            active: AtomicBool::new(true),
            gen_done: AtomicBool::new(false),
            seek_to: AtomicI64::new(-1),
            estimated_samples,
            rebuffering: AtomicBool::new(false),
            overruns: AtomicUsize::new(0),
        });

        let (tx, rx) = mpsc::channel::<Vec<f32>>();

        let s = shared.clone();
        std::thread::Builder::new()
            .name("audio-player".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run_stream(s.clone(), rx, sample_rate, estimated_samples, on_position)
                }));
                match result {
                    Ok(Err(e)) => log::error!("Audio player: {e}"),
                    Err(_) => log::error!("Audio player thread panicked"),
                    _ => {}
                }
                log::info!("TTS audio player thread exiting");
                s.active.store(false, Ordering::SeqCst);
            })
            .map_err(|e| format!("spawn audio player: {e}"))?;

        Ok(Self { shared, tx })
    }

    pub fn push(&self, samples: Vec<f32>) {
        if self.shared.active.load(Ordering::SeqCst) {
            let _ = self.tx.send(samples);
        }
    }

    pub fn is_active(&self) -> bool {
        self.shared.active.load(Ordering::SeqCst)
    }

    pub fn pause(&self) {
        self.shared.paused.store(true, Ordering::SeqCst);
    }

    pub fn resume(&self) {
        self.shared.paused.store(false, Ordering::SeqCst);
    }

    pub fn seek(&self, sample_pos: usize) {
        self.shared.seek_to.store(sample_pos as i64, Ordering::SeqCst);
    }

    pub fn position(&self) -> PlaybackPosition {
        PlaybackPosition {
            cursor: self.shared.cursor.load(Ordering::Relaxed),
            buffered: self.shared.buffered.load(Ordering::Relaxed),
            paused: self.shared.paused.load(Ordering::Relaxed),
            gen_done: self.shared.gen_done.load(Ordering::Relaxed),
            estimated: self.shared.estimated_samples,
            rebuffering: self.shared.rebuffering.load(Ordering::Relaxed),
        }
    }

    pub fn stop(&self) {
        self.shared.active.store(false, Ordering::SeqCst);
    }

    pub fn finish(&self) {
        self.shared.gen_done.store(true, Ordering::SeqCst);
    }
}

fn run_stream(
    shared: Arc<SharedState>,
    rx: mpsc::Receiver<Vec<f32>>,
    sample_rate: i32,
    estimated_samples: usize,
    on_position: Option<Box<dyn Fn(PlaybackPosition) + Send + 'static>>,
) -> Result<(), String> {
    let min_samples = sample_rate as usize * PRE_BUFFER_SECS;
    let capacity = (estimated_samples * 2).max(sample_rate as usize * 60);

    // Fixed playback buffer, allocated once and never reallocated. The feeder
    // (this thread) writes generated samples into it ahead of `buffered`; the
    // audio callback only reads behind `buffered`. Retaining all audio (rather
    // than a recycling ring) keeps seek/skip working.
    let mut backing: Vec<f32> = vec![0.0; capacity];
    let buf = SendPtr(backing.as_mut_ptr());

    // Pre-buffer: fill until we have a head start (or generation finished).
    let mut write_pos: usize = 0;
    loop {
        if !shared.active.load(Ordering::SeqCst) { return Ok(()); }
        write_pos += drain_into(&rx, buf.0, capacity, write_pos);
        shared.buffered.store(write_pos, Ordering::Release);
        if write_pos >= min_samples || (shared.gen_done.load(Ordering::SeqCst) && write_pos > 0) {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }

    let host = cpal::default_host();
    let device = host.default_output_device().ok_or("no output audio device")?;
    let config = cpal::StreamConfig {
        channels: 1,
        sample_rate: sample_rate as u32,
        buffer_size: cpal::BufferSize::Default,
    };

    let shared_cb = shared.clone();
    let mut cursor: usize = 0;
    let mut rebuffering = false;
    let rebuffer_margin = (sample_rate as usize) * REBUFFER_MARGIN_SECS;
    let stream = device
        .build_output_stream(
            config,
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                // Realtime audio thread. Do ONLY a bounded copy here — no
                // allocation, no channel draining (that is the feeder's job).
                // Variable/blocking work here overruns oboe's tiny callback
                // budget, and on underrun the HAL repeats its last buffer, which
                // is heard as duplicated/stuttering speech.
                let started = Instant::now();
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let wp = shared_cb.buffered.load(Ordering::Acquire);

                    let seek = shared_cb.seek_to.swap(-1, Ordering::SeqCst);
                    if seek >= 0 {
                        cursor = (seek as usize).min(wp);
                        rebuffering = false;
                    }

                    if shared_cb.paused.load(Ordering::Relaxed) {
                        for s in data.iter_mut() { *s = 0.0; }
                        shared_cb.cursor.store(cursor, Ordering::Relaxed);
                        return;
                    }

                    let avail = wp.saturating_sub(cursor);
                    let frame = data.len();
                    let gen_done = shared_cb.gen_done.load(Ordering::Relaxed);

                    // Rebuffer hysteresis: while generation is still running and
                    // we can't fill a whole frame, output clean silence and hold
                    // the cursor until a healthy margin is back.
                    if !gen_done {
                        if rebuffering {
                            if avail >= rebuffer_margin { rebuffering = false; }
                        } else if avail < frame {
                            rebuffering = true;
                        }
                    }
                    if rebuffering {
                        for s in data.iter_mut() { *s = 0.0; }
                        shared_cb.cursor.store(cursor, Ordering::Relaxed);
                        shared_cb.rebuffering.store(true, Ordering::Relaxed);
                        return;
                    }
                    shared_cb.rebuffering.store(false, Ordering::Relaxed);

                    let n = avail.min(frame);
                    // SAFETY: cursor + n <= wp, and the feeder only writes indices
                    // >= the published `wp`, so [cursor, cursor+n) is fully
                    // written and not concurrently mutated. `backing` outlives
                    // the stream (dropped after it), so `buf` is always valid.
                    unsafe {
                        std::ptr::copy_nonoverlapping(buf.at(cursor), data.as_mut_ptr(), n);
                    }
                    for s in data[n..].iter_mut() { *s = 0.0; }
                    cursor += n;
                    shared_cb.cursor.store(cursor, Ordering::Relaxed);
                }));
                if result.is_err() {
                    for s in data.iter_mut() { *s = 0.0; }
                    shared_cb.active.store(false, Ordering::SeqCst);
                    log::error!("audio player callback panicked — stopping stream");
                }
                if started.elapsed() >= Duration::from_millis(5) {
                    shared_cb.overruns.fetch_add(1, Ordering::Relaxed);
                }
            },
            |err| log::error!("audio player stream error: {err}"),
            None,
        )
        .map_err(|e| format!("build output stream: {e}"))?;

    stream.play().map_err(|e| format!("start stream: {e}"))?;
    log::info!("TTS audio player thread started (sr={sample_rate}, buf_cap={capacity})");

    let mut last_report = Instant::now();
    let mut was_rebuffering = false;
    let mut last_overruns = 0usize;
    while shared.active.load(Ordering::SeqCst) {
        // Feeder: move generated audio into the playback buffer off the RT
        // thread, where a large memcpy is harmless.
        let added = drain_into(&rx, buf.0, capacity, write_pos);
        if added > 0 {
            write_pos += added;
            shared.buffered.store(write_pos, Ordering::Release);
        }

        let rb = shared.rebuffering.load(Ordering::Relaxed);
        if rb != was_rebuffering {
            log::info!(
                "TTS playback {}",
                if rb { "starved — rebuffering (generation behind playback)" } else { "resumed after rebuffer" }
            );
            was_rebuffering = rb;
        }

        let overruns = shared.overruns.load(Ordering::Relaxed);
        if overruns != last_overruns {
            log::warn!("TTS audio callback overruns: {overruns} (>5ms on the RT thread)");
            last_overruns = overruns;
        }

        if let Some(ref cb) = on_position {
            if last_report.elapsed() >= Duration::from_millis(250) {
                cb(PlaybackPosition {
                    cursor: shared.cursor.load(Ordering::Relaxed),
                    buffered: shared.buffered.load(Ordering::Relaxed),
                    paused: shared.paused.load(Ordering::Relaxed),
                    gen_done: shared.gen_done.load(Ordering::Relaxed),
                    estimated: shared.estimated_samples,
                    rebuffering: rb,
                });
                last_report = Instant::now();
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    // Keep the backing buffer alive until after the stream is dropped (drop
    // order: `stream` before `backing`), so the callback can never read freed
    // memory. This statement documents and enforces that the Vec lives here.
    drop(stream);
    drop(backing);
    Ok(())
}
