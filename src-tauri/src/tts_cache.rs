//! Persistent on-disk cache of synthesized speech segments.
//!
//! A "segment" is one `split_for_pauses` unit (a clause/sentence). Each entry is
//! keyed by (model fingerprint, speaker id, quantized speed, raw segment text)
//! and stores the segment's SPEECH pcm as 16-bit samples plus its per-word ms
//! timing. The inter-segment pause silence is deterministic and re-spliced by
//! the caller, so it is not stored.
//!
//! Effect: the same sentence in the same voice+speed is synthesized at most
//! once, ever. A reopened article whose segments are all cached generates
//! nothing, and a seek that re-renders from a word pulls the already-made
//! segments instead of re-synthesizing them.
//!
//! Audio is bit-identical to fresh synthesis aside from i16 quantization
//! (inaudible). All reads/writes happen on the generation thread, never on the
//! realtime audio callback. Writes are atomic (temp + rename) so a kill mid-gen
//! can't leave a torn entry that a later read would trust.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

const MAGIC: &[u8; 4] = b"TSC1";
// Folded into every key. Bump when the synthesis path changes in a way that
// would make stored audio wrong (normalization, encoder, pause handling). Old
// entries then simply never match and age out via eviction.
// v2/v3 (2026-06-30/07-01): more PRONUNCIATION_OVERRIDES entries. v4: the OOV
// compound splitter (piper.rs segment_compound). v5: OOV possessive handling
// (Claude's -> claude + /z/). v6: read/reading heteronym flip. v7: json/scapes/
// hellscape(s). v8: iteration/unordered/ai + lives heteronym. v9: compound-split
// stress demotion (salesforce joins without a mid-word pause). v10: acronym
// plurals (MCPs -> "em-see-peez") + eval/verifier(s)/plugin(s). Each bump makes
// any stale mispronunciation miss and regenerate.
const CACHE_VERSION: u32 = 10;
// Soft cap on total cache size. On write, oldest entries are evicted down to
// ~90% of this. ~1 GiB is roughly 6-7 hours of 22 kHz i16 speech.
const CACHE_CAP_BYTES: u64 = 1024 * 1024 * 1024;
// Re-scan for eviction only after this many bytes have been written since the
// last scan, so a fresh generation doesn't stat the whole dir on every segment.
const EVICT_SCAN_DELTA: u64 = 32 * 1024 * 1024;

static CACHE_DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
// u64::MAX so the first write of a session always triggers one eviction scan.
static BYTES_SINCE_SCAN: AtomicU64 = AtomicU64::new(u64::MAX);

pub struct CachedSegment {
    pub pcm: Vec<f32>,
    pub word_ms: Vec<u64>,
}

fn base_dir() -> Option<PathBuf> {
    #[cfg(target_os = "android")]
    {
        std::env::var_os("VERBA_DATA_DIR").map(|d| PathBuf::from(d).join("tts-cache"))
    }
    #[cfg(not(target_os = "android"))]
    {
        dirs::data_dir().map(|d| d.join("verba").join("tts-cache"))
    }
}

fn cache_dir() -> Option<&'static Path> {
    CACHE_DIR.get_or_init(base_dir).as_deref()
}

/// Stable key for a segment render. `model_fp` separates voices/models,
/// `speed_milli` quantizes the length_scale so float jitter can't fragment the
/// cache, and the RAW segment text is what determines both the audio and the
/// per-word split, so it (not the normalized form) is the identity.
pub fn key(model_fp: &str, sid: i32, speed_milli: u32, raw_segment: &str) -> String {
    // FNV-1a 64-bit over the tuple — fast, dependency-free, ample for a
    // filename. Collisions are caught on read by comparing the stored raw text.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut feed = |bytes: &[u8]| {
        for &b in bytes {
            h ^= b as u64;
            h = h.wrapping_mul(0x100_0000_01b3);
        }
        // Separator so field boundaries are significant ("ab"+"c" != "a"+"bc").
        h ^= 0xff;
        h = h.wrapping_mul(0x100_0000_01b3);
    };
    feed(&CACHE_VERSION.to_le_bytes());
    feed(model_fp.as_bytes());
    feed(&sid.to_le_bytes());
    feed(&speed_milli.to_le_bytes());
    feed(raw_segment.as_bytes());
    format!("{h:016x}")
}

fn entry_path(dir: &Path, key: &str) -> PathBuf {
    // Shard by the first two hex chars so no single directory holds everything.
    dir.join(&key[0..2]).join(format!("{key}.seg"))
}

// ── read ──

fn rd_u32(b: &[u8], at: &mut usize) -> Option<u32> {
    let end = at.checked_add(4)?;
    let v = u32::from_le_bytes(b.get(*at..end)?.try_into().ok()?);
    *at = end;
    Some(v)
}

/// Load the entry for `key`, verifying it was written for this `sample_rate` and
/// this exact `raw_segment` (guards a hash collision). Returns `None` on miss,
/// any parse failure, or mismatch — the caller then synthesizes.
pub fn get(key: &str, sample_rate: u32, raw_segment: &str) -> Option<CachedSegment> {
    let dir = cache_dir()?;
    let path = entry_path(dir, key);
    let bytes = std::fs::read(&path).ok()?;

    if bytes.get(0..4)? != MAGIC.as_slice() {
        return None;
    }
    let mut at = 4usize;
    if rd_u32(&bytes, &mut at)? != CACHE_VERSION {
        return None;
    }
    if rd_u32(&bytes, &mut at)? != sample_rate {
        return None;
    }
    let text_len = rd_u32(&bytes, &mut at)? as usize;
    let text_end = at.checked_add(text_len)?;
    if bytes.get(at..text_end)? != raw_segment.as_bytes() {
        return None; // hash collision (or stale text) — treat as a miss
    }
    at = text_end;

    let word_count = rd_u32(&bytes, &mut at)? as usize;
    let mut word_ms = Vec::with_capacity(word_count);
    for _ in 0..word_count {
        word_ms.push(rd_u32(&bytes, &mut at)? as u64);
    }

    let pcm_len = rd_u32(&bytes, &mut at)? as usize;
    let pcm_end = at.checked_add(pcm_len * 2)?;
    let raw = bytes.get(at..pcm_end)?;
    let mut pcm = Vec::with_capacity(pcm_len);
    for s in raw.chunks_exact(2) {
        let i = i16::from_le_bytes([s[0], s[1]]);
        pcm.push(i as f32 / 32768.0);
    }
    Some(CachedSegment { pcm, word_ms })
}

/// Cheap existence + length check: reads only the entry header (not the PCM) and
/// returns the stored sample count, verifying it was written for this
/// `sample_rate` and exact `raw_segment`. Used by the cache-coverage path so
/// opening an article doesn't decode every cached segment's audio.
pub fn cached_meta(key: &str, sample_rate: u32, raw_segment: &str) -> Option<usize> {
    use std::io::Read;
    let dir = cache_dir()?;
    let mut f = std::fs::File::open(entry_path(dir, key)).ok()?;

    let mut head = [0u8; 12]; // magic(4) + version(4) + sample_rate(4)
    f.read_exact(&mut head).ok()?;
    if &head[..4] != MAGIC.as_slice() {
        return None;
    }
    if u32::from_le_bytes(head[4..8].try_into().ok()?) != CACHE_VERSION {
        return None;
    }
    if u32::from_le_bytes(head[8..12].try_into().ok()?) != sample_rate {
        return None;
    }

    let mut u = [0u8; 4];
    f.read_exact(&mut u).ok()?;
    let text_len = u32::from_le_bytes(u) as usize;
    let mut text = vec![0u8; text_len];
    f.read_exact(&mut text).ok()?;
    if text != raw_segment.as_bytes() {
        return None;
    }

    f.read_exact(&mut u).ok()?;
    let word_count = u32::from_le_bytes(u) as usize;
    let mut skip = vec![0u8; word_count * 4];
    f.read_exact(&mut skip).ok()?; // word_ms, not needed here

    f.read_exact(&mut u).ok()?;
    Some(u32::from_le_bytes(u) as usize)
}

// ── write ──

/// Store the segment's speech `pcm` + `word_ms`. No-op for empty audio. Best
/// effort: any IO error is logged and swallowed (the cache is an optimization,
/// never a correctness dependency).
pub fn put(key: &str, sample_rate: u32, raw_segment: &str, pcm: &[f32], word_ms: &[u64]) {
    if pcm.is_empty() {
        return;
    }
    let Some(dir) = cache_dir() else { return };
    let path = entry_path(dir, key);
    let Some(shard) = path.parent() else { return };
    if let Err(e) = std::fs::create_dir_all(shard) {
        log::warn!("tts cache: create dir {shard:?}: {e}");
        return;
    }

    let mut out: Vec<u8> = Vec::with_capacity(32 + raw_segment.len() + pcm.len() * 2);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&CACHE_VERSION.to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&(raw_segment.len() as u32).to_le_bytes());
    out.extend_from_slice(raw_segment.as_bytes());
    out.extend_from_slice(&(word_ms.len() as u32).to_le_bytes());
    for &w in word_ms {
        out.extend_from_slice(&(w.min(u32::MAX as u64) as u32).to_le_bytes());
    }
    out.extend_from_slice(&(pcm.len() as u32).to_le_bytes());
    for &x in pcm {
        let i = (x.clamp(-1.0, 1.0) * 32767.0).round() as i16;
        out.extend_from_slice(&i.to_le_bytes());
    }

    // Atomic publish: a torn write stays in `.tmp` and is never read as `.seg`.
    let tmp = path.with_extension("tmp");
    if let Err(e) = std::fs::write(&tmp, &out) {
        log::warn!("tts cache: write {tmp:?}: {e}");
        return;
    }
    if let Err(e) = std::fs::rename(&tmp, &path) {
        log::warn!("tts cache: rename {tmp:?}: {e}");
        let _ = std::fs::remove_file(&tmp);
        return;
    }
    maybe_evict(dir, out.len() as u64);
}

// ── eviction ──

fn maybe_evict(dir: &Path, just_wrote: u64) {
    let prev = BYTES_SINCE_SCAN.fetch_add(just_wrote, Ordering::Relaxed);
    if prev.saturating_add(just_wrote) < EVICT_SCAN_DELTA {
        return;
    }
    BYTES_SINCE_SCAN.store(0, Ordering::Relaxed);
    evict(dir);
}

/// Walk the shard dirs, and if total size exceeds the cap, delete oldest-first
/// (by mtime) until under 90% of it. Also sweeps stray `.tmp` files from
/// interrupted writes.
fn evict(dir: &Path) {
    let mut entries: Vec<(PathBuf, std::time::SystemTime, u64)> = Vec::new();
    let mut total: u64 = 0;
    let Ok(shards) = std::fs::read_dir(dir) else { return };
    for shard in shards.flatten() {
        let Ok(files) = std::fs::read_dir(shard.path()) else { continue };
        for f in files.flatten() {
            let p = f.path();
            let Ok(meta) = f.metadata() else { continue };
            if p.extension().and_then(|e| e.to_str()) == Some("tmp") {
                let _ = std::fs::remove_file(&p);
                continue;
            }
            let mtime = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
            total += meta.len();
            entries.push((p, mtime, meta.len()));
        }
    }
    if total <= CACHE_CAP_BYTES {
        return;
    }
    entries.sort_by_key(|e| e.1); // oldest first
    let target = CACHE_CAP_BYTES / 10 * 9;
    let mut freed = 0u64;
    for (p, _, len) in entries {
        if total - freed <= target {
            break;
        }
        if std::fs::remove_file(&p).is_ok() {
            freed += len;
        }
    }
    log::info!("tts cache: evicted {} bytes (was {total}, cap {CACHE_CAP_BYTES})", freed);
}

// ── maintenance (exposed via commands) ──

/// Total bytes currently held by the cache.
pub fn size_bytes() -> u64 {
    let Some(dir) = cache_dir() else { return 0 };
    let mut total = 0u64;
    let Ok(shards) = std::fs::read_dir(dir) else { return 0 };
    for shard in shards.flatten() {
        if let Ok(files) = std::fs::read_dir(shard.path()) {
            for f in files.flatten() {
                if let Ok(meta) = f.metadata() {
                    total += meta.len();
                }
            }
        }
    }
    total
}

/// Delete the whole cache.
pub fn clear() -> Result<(), String> {
    let Some(dir) = cache_dir() else { return Ok(()) };
    if dir.exists() {
        std::fs::remove_dir_all(dir).map_err(|e| format!("clear tts cache: {e}"))?;
    }
    BYTES_SINCE_SCAN.store(u64::MAX, Ordering::Relaxed);
    Ok(())
}
