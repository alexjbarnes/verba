//! Temporary debug audio dumps, enabled by launching with VERBA_DEBUG_AUDIO=1.
//!
//! Writes mono PCM16 WAV files into `<temp>/verba-debug-audio/` so capture
//! problems can be diagnosed by listening to exactly what each stage saw:
//!   `<ts>-<label>-raw.wav`      audio as received from the device/tap (device rate)
//!   `<ts>-<label>-16k.wav`      the same audio after resampling to 16kHz
//!   `<id>-diarize-input.wav`    the reconstructed waveform fed to the diarizer
//! The WAV header is re-patched roughly once per second of audio, so the files
//! stay playable even if the process dies mid-recording.

use std::fs::File;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

pub fn enabled() -> bool {
    std::env::var_os("VERBA_DEBUG_AUDIO").is_some_and(|v| v != "0")
}

pub fn dir() -> PathBuf {
    std::env::temp_dir().join("verba-debug-audio")
}

/// Incremental mono PCM16 WAV writer. Finalizes the header on drop.
pub struct DebugWav {
    file: BufWriter<File>,
    data_bytes: u32,
    samples_since_patch: u32,
    rate: u32,
}

impl DebugWav {
    pub fn create(path: &Path, rate: u32) -> std::io::Result<Self> {
        let mut file = BufWriter::new(File::create(path)?);
        let mut header = [0u8; 44];
        header[0..4].copy_from_slice(b"RIFF");
        header[8..12].copy_from_slice(b"WAVE");
        header[12..16].copy_from_slice(b"fmt ");
        header[16..20].copy_from_slice(&16u32.to_le_bytes());
        header[20..22].copy_from_slice(&1u16.to_le_bytes()); // PCM
        header[22..24].copy_from_slice(&1u16.to_le_bytes()); // mono
        header[24..28].copy_from_slice(&rate.to_le_bytes());
        header[28..32].copy_from_slice(&(rate * 2).to_le_bytes());
        header[32..34].copy_from_slice(&2u16.to_le_bytes()); // block align
        header[34..36].copy_from_slice(&16u16.to_le_bytes()); // bits per sample
        header[36..40].copy_from_slice(b"data");
        file.write_all(&header)?;
        Ok(Self { file, data_bytes: 0, samples_since_patch: 0, rate })
    }

    pub fn write(&mut self, samples: &[f32]) {
        for &s in samples {
            let v = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
            let _ = self.file.write_all(&v.to_le_bytes());
        }
        self.data_bytes = self.data_bytes.saturating_add((samples.len() * 2) as u32);
        self.samples_since_patch = self.samples_since_patch.saturating_add(samples.len() as u32);
        if self.samples_since_patch >= self.rate {
            self.samples_since_patch = 0;
            let _ = self.patch_header();
        }
    }

    fn patch_header(&mut self) -> std::io::Result<()> {
        self.file.flush()?;
        self.file.seek(SeekFrom::Start(4))?;
        self.file.write_all(&(36 + self.data_bytes).to_le_bytes())?;
        self.file.seek(SeekFrom::Start(40))?;
        self.file.write_all(&self.data_bytes.to_le_bytes())?;
        self.file.seek(SeekFrom::End(0))?;
        Ok(())
    }
}

impl Drop for DebugWav {
    fn drop(&mut self) {
        let _ = self.patch_header();
        let _ = self.file.flush();
    }
}

/// Paired dumps for one capture stream: pre-resample and post-resample.
pub struct StreamDump {
    raw: DebugWav,
    k16: DebugWav,
}

impl StreamDump {
    pub fn write_raw(&mut self, samples: &[f32]) {
        self.raw.write(samples);
    }

    pub fn write_16k(&mut self, samples: &[f32]) {
        self.k16.write(samples);
    }
}

/// A fixed destination for a stream's dumps: files land in `dir` named
/// `<prefix>-<label>-{raw,16k}.wav`. Meeting mode passes one of these so its
/// audio lands beside the transcript, unconditionally.
#[derive(Clone)]
pub struct DumpTarget {
    pub dir: PathBuf,
    pub prefix: String,
}

/// Open the raw + 16k dump pair for a stream. With a `DumpTarget` the dump is
/// always on; without one it only happens when VERBA_DEBUG_AUDIO is set
/// (files go to the temp dir, timestamp-prefixed). `label` is "mic" or
/// "system". None when off or the files can't be created.
pub fn stream_dump_for(
    target: Option<&DumpTarget>,
    label: &str,
    device_rate: u32,
) -> Option<StreamDump> {
    let (dir, stem) = match target {
        Some(t) => (t.dir.clone(), format!("{}-{label}", t.prefix)),
        None => {
            if !enabled() {
                return None;
            }
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0);
            (dir(), format!("{ts}-{label}"))
        }
    };
    if let Err(e) = std::fs::create_dir_all(&dir) {
        log::warn!("debug-audio: cannot create {}: {e}", dir.display());
        return None;
    }
    let raw_path = unique_path(&dir, &format!("{stem}-raw"));
    let k16_path = unique_path(&dir, &format!("{stem}-16k"));
    match (DebugWav::create(&raw_path, device_rate), DebugWav::create(&k16_path, 16_000)) {
        (Ok(raw), Ok(k16)) => {
            log::info!(
                "debug-audio: dumping {label} stream to {} (device rate) and {} (16k)",
                raw_path.display(),
                k16_path.display()
            );
            Some(StreamDump { raw, k16 })
        }
        _ => {
            log::warn!("debug-audio: failed to create dump files in {}", dir.display());
            None
        }
    }
}

/// First non-existing `<stem>.wav`, `<stem>-2.wav`, ... so a stream retry
/// never truncates the audio already captured before the reconnect.
fn unique_path(dir: &Path, stem: &str) -> PathBuf {
    let first = dir.join(format!("{stem}.wav"));
    if !first.exists() {
        return first;
    }
    for n in 2..100 {
        let p = dir.join(format!("{stem}-{n}.wav"));
        if !p.exists() {
            return p;
        }
    }
    first
}

/// One-shot WAV write for a complete buffer (used for the diarizer input).
pub fn write_wav(path: &Path, rate: u32, samples: &[f32]) -> std::io::Result<()> {
    let mut w = DebugWav::create(path, rate)?;
    w.write(samples);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_roundtrip_header_and_samples() {
        let path = std::env::temp_dir().join(format!("verba-debug-wav-test-{}.wav", std::process::id()));
        {
            let mut w = DebugWav::create(&path, 16_000).unwrap();
            w.write(&[0.0, 0.5, -0.5, 1.5, -1.5]);
        }
        let bytes = std::fs::read(&path).unwrap();
        std::fs::remove_file(&path).ok();

        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WAVE");
        let rate = u32::from_le_bytes(bytes[24..28].try_into().unwrap());
        assert_eq!(rate, 16_000);
        let channels = u16::from_le_bytes(bytes[22..24].try_into().unwrap());
        assert_eq!(channels, 1);
        let data_bytes = u32::from_le_bytes(bytes[40..44].try_into().unwrap());
        assert_eq!(data_bytes, 10);
        assert_eq!(bytes.len(), 44 + 10);

        let sample = |i: usize| i16::from_le_bytes(bytes[44 + i * 2..46 + i * 2].try_into().unwrap());
        assert_eq!(sample(0), 0);
        assert_eq!(sample(1), 16383);
        assert_eq!(sample(2), -16383);
        assert_eq!(sample(3), 32767); // clamped
        assert_eq!(sample(4), -32767); // clamped
    }
}
