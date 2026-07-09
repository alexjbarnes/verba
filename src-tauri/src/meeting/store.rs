//! Meeting persistence: a small JSON index of meeting metadata plus Markdown
//! transcript/summary files written to the user-configured directories.
//!
//! Mirrors library.rs: tmp+rename writes, an in-memory Mutex over the index,
//! reload-from-disk on list so external edits are picked up. Audio is never
//! stored — the files hold text only.

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};

static STORE: OnceLock<MeetingStore> = OnceLock::new();

/// One utterance as assembled by the session: who said it (channel or
/// clustered speaker), when (wall-clock ms since the meeting started), what.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Utterance {
    /// "mic" | "system"
    pub source: String,
    /// "You", "Speaker 1", ...
    pub speaker: String,
    pub text: String,
    /// Milliseconds since meeting start (segment capture time).
    pub t_ms: u64,
    /// Per-utterance speaker voiceprint, attached at stop (system channel only)
    /// so identity stays traceable through split/merge/re-cluster without the
    /// audio. None on the live event, the mic channel, and pre-embedding
    /// meetings. Lives only in the structured sidecar — never the markdown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding: Option<Vec<f32>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeetingMeta {
    pub id: String,
    pub title: String,
    /// RFC3339 start time.
    pub started: String,
    #[serde(default)]
    pub duration_ms: u64,
    #[serde(default)]
    pub utterance_count: u32,
    /// Absolute path of the transcript markdown ("" until written).
    #[serde(default)]
    pub transcript_path: String,
    /// Absolute path of the summary markdown ("" until summarized).
    #[serde(default)]
    pub summary_path: String,
    /// Summarizer component that produced the summary ("" if none yet).
    #[serde(default)]
    pub summarizer_id: String,
    /// Count of still-unnamed "Speaker N" speakers, for the meetings-list badge.
    #[serde(default)]
    pub unnamed_speakers: u32,
}

pub struct MeetingStore {
    items: Mutex<Vec<MeetingMeta>>,
}

impl MeetingStore {
    pub fn global() -> &'static Self {
        STORE.get_or_init(|| Self {
            items: Mutex::new(Self::load_from_disk().unwrap_or_default()),
        })
    }

    fn index_path() -> Option<PathBuf> {
        dirs::data_dir().map(|d| d.join("verba").join("meetings.json"))
    }

    fn load_from_disk() -> Option<Vec<MeetingMeta>> {
        let raw = std::fs::read_to_string(Self::index_path()?).ok()?;
        serde_json::from_str(&raw).ok()
    }

    fn save_to_disk(items: &[MeetingMeta]) -> Result<(), String> {
        let path = Self::index_path().ok_or("no data dir")?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("meetings dir: {e}"))?;
        }
        let raw = serde_json::to_string_pretty(items).map_err(|e| e.to_string())?;
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, raw).map_err(|e| format!("meetings write: {e}"))?;
        std::fs::rename(&tmp, &path).map_err(|e| format!("meetings rename: {e}"))
    }

    pub fn list(&self) -> Vec<MeetingMeta> {
        // Reload so deletions/edits from another process (or a crash-recovered
        // index) are reflected, mirroring history.rs.
        let mut items = self.items.lock().unwrap();
        if let Some(disk) = Self::load_from_disk() {
            *items = disk;
        }
        let mut out = items.clone();
        out.reverse(); // newest first
        out
    }

    pub fn get(&self, id: &str) -> Option<MeetingMeta> {
        self.items.lock().unwrap().iter().find(|m| m.id == id).cloned()
    }

    /// Insert or replace by id (the session upserts on autosave and stop).
    pub fn upsert(&self, meta: MeetingMeta) -> Result<(), String> {
        let mut items = self.items.lock().unwrap();
        if let Some(existing) = items.iter_mut().find(|m| m.id == meta.id) {
            *existing = meta;
        } else {
            items.push(meta);
        }
        Self::save_to_disk(&items)
    }

    /// Remove a meeting from the index; optionally delete its files too.
    pub fn delete(&self, id: &str, delete_files: bool) -> Result<(), String> {
        let mut items = self.items.lock().unwrap();
        let Some(pos) = items.iter().position(|m| m.id == id) else {
            return Ok(()); // already gone
        };
        let meta = items.remove(pos);
        Self::save_to_disk(&items)?;
        if delete_files {
            for p in [&meta.transcript_path, &meta.summary_path] {
                if !p.is_empty() {
                    let _ = std::fs::remove_file(p);
                }
            }
        }
        Ok(())
    }
}

/// "2026-07-07 14-30 Meeting.md" — filesystem-safe, sorts chronologically.
pub fn meeting_filename(started_local: &str, suffix: &str) -> String {
    let safe: String = started_local
        .chars()
        .map(|c| if c == ':' { '-' } else { c })
        .collect();
    format!("{safe} {suffix}.md")
}

pub fn fmt_clock(t_ms: u64) -> String {
    let s = t_ms / 1000;
    format!("{:02}:{:02}", s / 60, s % 60)
}

/// Structured transcript sidecar: the full `Utterance` list (incl. per-utterance
/// voiceprints) as JSON at `data_dir/verba/transcripts/<id>.json`. This is the
/// source of truth for speaker operations; the markdown is a derived export.
/// Kept beside the voiceprint sidecars so the same delete path clears both.
fn transcript_sidecar_path(id: &str) -> Option<PathBuf> {
    let safe: String =
        id.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect();
    dirs::data_dir().map(|d| d.join("verba").join("transcripts").join(format!("{safe}.json")))
}

pub fn save_transcript(id: &str, utterances: &[Utterance]) -> Result<(), String> {
    let path = transcript_sidecar_path(id).ok_or("no data dir")?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("transcripts dir: {e}"))?;
    }
    let raw = serde_json::to_string(utterances).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, raw).map_err(|e| format!("transcript write: {e}"))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("transcript rename: {e}"))
}

/// The structured transcript, when a sidecar exists (meetings recorded after the
/// per-utterance-voiceprint change). `None` falls callers back to the markdown.
pub fn load_transcript(id: &str) -> Option<Vec<Utterance>> {
    transcript_sidecar_path(id)
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|raw| serde_json::from_str(&raw).ok())
}

pub fn delete_transcript(id: &str) {
    if let Some(p) = transcript_sidecar_path(id) {
        let _ = std::fs::remove_file(p);
    }
}

/// Transcript markdown: title, the user's own notes verbatim, then the
/// utterances as `**[MM:SS] Speaker:** text` lines.
pub fn transcript_markdown(meta: &MeetingMeta, notes: &str, utterances: &[Utterance]) -> String {
    let mut out = format!("# {}\n\nStarted: {}\n", meta.title, meta.started);
    if !notes.trim().is_empty() {
        out.push_str("\n## Notes\n\n");
        out.push_str(notes.trim_end());
        out.push('\n');
    }
    out.push_str("\n## Transcript\n\n");
    for u in utterances {
        out.push_str(&format!("**[{}] {}:** {}\n\n", fmt_clock(u.t_ms), u.speaker, u.text));
    }
    out
}

/// Rewrite a transcript's speaker labels, renaming `from` to `to` on the
/// `**[MM:SS] Speaker:** text` lines (only the speaker field, never the text).
pub fn rename_speaker_in_markdown(md: &str, from: &str, to: &str) -> String {
    let mut out = String::with_capacity(md.len());
    for line in md.lines() {
        if let Some(rest) = line.strip_prefix("**[") {
            if let Some((time, after)) = rest.split_once("] ") {
                if let Some((speaker, text)) = after.split_once(":** ") {
                    if speaker == from {
                        out.push_str(&format!("**[{time}] {to}:** {text}\n"));
                        continue;
                    }
                }
            }
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Write markdown into `dir` (created if needed) as `filename`, tmp+rename.
/// Returns the absolute path written.
pub fn write_markdown(dir: &str, filename: &str, content: &str) -> Result<PathBuf, String> {
    if dir.trim().is_empty() {
        return Err("no directory configured".into());
    }
    let dir = PathBuf::from(dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let path = dir.join(filename);
    let tmp = path.with_extension("md.tmp");
    std::fs::write(&tmp, content).map_err(|e| format!("write transcript: {e}"))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("rename transcript: {e}"))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(id: &str) -> MeetingMeta {
        MeetingMeta {
            id: id.into(),
            title: "Weekly sync".into(),
            started: "2026-07-07T14:30:00Z".into(),
            duration_ms: 65_000,
            utterance_count: 2,
            transcript_path: String::new(),
            summary_path: String::new(),
            summarizer_id: String::new(),
            unnamed_speakers: 0,
        }
    }

    #[test]
    fn transcript_markdown_golden() {
        let utterances = vec![
            Utterance { source: "mic".into(), speaker: "You".into(), text: "Morning all.".into(), t_ms: 1_000, embedding: None },
            Utterance { source: "system".into(), speaker: "Speaker 1".into(), text: "Morning. Let's start.".into(), t_ms: 4_500, embedding: None },
        ];
        let md = transcript_markdown(&meta("m1"), "ship friday\nrollback: priya", &utterances);
        let expected = "# Weekly sync\n\nStarted: 2026-07-07T14:30:00Z\n\n## Notes\n\nship friday\nrollback: priya\n\n## Transcript\n\n**[00:01] You:** Morning all.\n\n**[00:04] Speaker 1:** Morning. Let's start.\n\n";
        assert_eq!(md, expected);
    }

    #[test]
    fn transcript_markdown_skips_empty_notes() {
        let md = transcript_markdown(&meta("m1"), "   \n", &[]);
        assert!(!md.contains("## Notes"));
        assert!(md.contains("## Transcript"));
    }

    #[test]
    fn meeting_filename_is_safe() {
        assert_eq!(
            meeting_filename("2026-07-07 14:30", "Meeting"),
            "2026-07-07 14-30 Meeting.md"
        );
    }

    #[test]
    fn write_markdown_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_markdown(dir.path().to_str().unwrap(), "t.md", "# hi\n").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "# hi\n");
        // Overwrite goes through tmp+rename (no .md.tmp left behind).
        let p2 = write_markdown(dir.path().to_str().unwrap(), "t.md", "# hi2\n").unwrap();
        assert_eq!(p, p2);
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "# hi2\n");
        assert!(!p.with_extension("md.tmp").exists());
    }

    #[test]
    fn rename_speaker_only_touches_speaker_field() {
        let md = "## Transcript\n\n**[00:01] You:** Speaker 2 is loud.\n\n**[00:04] Speaker 2:** Hello.\n\n";
        let out = rename_speaker_in_markdown(md, "Speaker 2", "Bob");
        // The speaker label changes, the word "Speaker 2" inside text does not.
        assert!(out.contains("**[00:04] Bob:** Hello."));
        assert!(out.contains("**[00:01] You:** Speaker 2 is loud."));
        assert!(!out.contains("**[00:04] Speaker 2:**"));
    }
}
