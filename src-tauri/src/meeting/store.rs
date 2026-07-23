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
    /// True while the post-stop pass (tail transcription, speaker refinement,
    /// final transcript write) still runs on the finalize thread; the meetings
    /// list shows the entry as busy and blocks open/delete until it clears.
    #[serde(default)]
    pub processing: bool,
    /// Free-form labels for grouping across dates. Recurring meetings get a
    /// series tag applied automatically (see `seed_series_tag`); everything
    /// else is the user's own.
    #[serde(default)]
    pub tags: Vec<String>,
}

pub struct MeetingStore {
    items: Mutex<Vec<MeetingMeta>>,
}

impl MeetingStore {
    pub fn global() -> &'static Self {
        STORE.get_or_init(|| {
            let mut items = Self::load_from_disk().unwrap_or_default();
            // A processing flag is only true while a finalize thread lives, so
            // any set at boot is an orphan from a killed process. Clear them so
            // the card unsticks (the autosaved transcript stays reachable).
            let mut dirty = clear_stale_processing(&mut items);
            // Series tags only get applied as meetings are recorded, so
            // meetings that predate the feature need one sweep. Marked with a
            // sentinel file rather than repeated every boot: a tag the user
            // removes must stay removed.
            if let Some(marker) = Self::index_path().map(|p| p.with_file_name(".series-backfilled")) {
                if !marker.exists() && !items.is_empty() {
                    dirty |= backfill_series_tags(&mut items);
                    let _ = std::fs::write(&marker, "1");
                }
            }
            if dirty {
                let _ = Self::save_to_disk(&items);
            }
            Self { items: Mutex::new(items) }
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

/// Clear orphaned processing flags (finalize thread died with its process).
/// Returns true when anything changed, so the caller knows to persist.
fn clear_stale_processing(items: &mut [MeetingMeta]) -> bool {
    let mut changed = false;
    for m in items.iter_mut().filter(|m| m.processing) {
        m.processing = false;
        changed = true;
    }
    changed
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

/// Wall-clock label for an utterance: meeting start plus the offset, rendered
/// in the timestamp's own timezone (`started` records the local offset).
/// Meeting-relative MM:SS fallback when `started` doesn't parse.
pub fn fmt_wall(started: &str, t_ms: u64) -> String {
    match chrono::DateTime::parse_from_rfc3339(started) {
        Ok(dt) => (dt + chrono::Duration::milliseconds(t_ms as i64)).format("%H:%M:%S").to_string(),
        Err(_) => fmt_clock(t_ms),
    }
}

/// Consecutive same-speaker utterances closer than this merge into one display
/// block; a longer gap reads as a new turn even from the same voice.
pub const MERGE_GAP_MS: u64 = 30_000;

/// One display block: consecutive same-speaker utterances joined, stamped with
/// the first utterance's offset.
pub struct DisplayBlock {
    pub t_ms: u64,
    pub speaker: String,
    pub text: String,
    /// Indices into the source utterance slice that compose this block, so
    /// line-level operations (reassigning a merged row) can address the
    /// underlying sidecar lines.
    pub lines: Vec<usize>,
}

/// Collapse consecutive utterances from the same speaker into display blocks.
/// Display-level only — the stored per-segment utterance list is untouched
/// (diarization relabelling and per-utterance voiceprints key on segment t_ms).
pub fn merge_for_display(utterances: &[Utterance]) -> Vec<DisplayBlock> {
    let mut out: Vec<DisplayBlock> = Vec::new();
    let mut last_start: u64 = 0;
    for (i, u) in utterances.iter().enumerate() {
        let text = u.text.trim();
        if text.is_empty() {
            continue;
        }
        if let Some(b) = out.last_mut() {
            if b.speaker == u.speaker && u.t_ms.saturating_sub(last_start) <= MERGE_GAP_MS {
                b.text.push(' ');
                b.text.push_str(text);
                b.lines.push(i);
                last_start = u.t_ms;
                continue;
            }
        }
        out.push(DisplayBlock {
            t_ms: u.t_ms,
            speaker: u.speaker.clone(),
            text: text.to_string(),
            lines: vec![i],
        });
        last_start = u.t_ms;
    }
    out
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

/// Tag every meeting whose title recurs, for the existing archive. Returns true
/// when anything changed. Runs once (see the sentinel in `global`).
fn backfill_series_tags(items: &mut [MeetingMeta]) -> bool {
    let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for m in items.iter() {
        let slug = series_slug(&m.title);
        if !slug.is_empty() {
            *counts.entry(slug).or_default() += 1;
        }
    }
    let mut changed = false;
    for m in items.iter_mut() {
        let slug = series_slug(&m.title);
        if slug.is_empty() || counts.get(&slug).copied().unwrap_or(0) < 2 {
            continue;
        }
        if !m.tags.iter().any(|t| t == &slug) {
            m.tags.push(slug);
            changed = true;
        }
    }
    changed
}

/// A title reduced to its comparable form, used to spot a recurring meeting:
/// case and punctuation are noise ("Standup", "standup!" and "Stand up" are
/// one series). Returns "" for titles with nothing left to compare.
pub fn series_slug(title: &str) -> String {
    let mut out = String::new();
    let mut pending_sep = false;
    for c in title.trim().chars() {
        if c.is_alphanumeric() {
            if pending_sep && !out.is_empty() {
                out.push('-');
            }
            pending_sep = false;
            out.extend(c.to_lowercase());
        } else {
            pending_sep = true;
        }
    }
    out
}

/// Auto-tag recurring meetings. A meeting whose title matches an existing one
/// joins that series; when the series is FIRST formed (exactly one other
/// meeting shares the title) the older meeting is tagged too, so the pair
/// groups together straight away.
///
/// Deliberately not re-applied to older meetings beyond that first pairing: a
/// tag the user removed should stay removed.
pub fn seed_series_tag(id: &str) -> Result<(), String> {
    let store = MeetingStore::global();
    let items = store.list();
    let Some(me) = items.iter().find(|m| m.id == id) else {
        return Ok(());
    };
    let slug = series_slug(&me.title);
    if slug.is_empty() {
        return Ok(());
    }
    let siblings: Vec<&MeetingMeta> =
        items.iter().filter(|m| m.id != id && series_slug(&m.title) == slug).collect();
    if siblings.is_empty() {
        return Ok(());
    }
    let mut updates: Vec<MeetingMeta> = Vec::new();
    if !me.tags.iter().any(|t| t == &slug) {
        let mut m = me.clone();
        m.tags.push(slug.clone());
        updates.push(m);
    }
    // Exactly one other: the series is new, so bring that one along.
    if siblings.len() == 1 && !siblings[0].tags.iter().any(|t| t == &slug) {
        let mut m = siblings[0].clone();
        m.tags.push(slug.clone());
        updates.push(m);
    }
    for m in updates {
        store.upsert(m)?;
    }
    Ok(())
}

/// Replace a meeting's tags (trimmed, deduplicated, empties dropped).
pub fn set_tags(id: &str, tags: Vec<String>) -> Result<(), String> {
    let store = MeetingStore::global();
    let mut meta = store.get(id).ok_or("meeting not found")?;
    let mut clean: Vec<String> = Vec::new();
    for t in tags {
        let t = t.trim().to_string();
        if !t.is_empty() && !clean.iter().any(|e| e.eq_ignore_ascii_case(&t)) {
            clean.push(t);
        }
    }
    meta.tags = clean;
    store.upsert(meta)
}

/// Where a query matched inside one meeting.
#[derive(Debug, Clone, Serialize)]
pub struct SearchHit {
    pub id: String,
    /// The matching line, trimmed to a readable window around the match.
    pub snippet: String,
    /// Who said it ("" when the match came from the title or the markdown
    /// fallback, which has no structured speaker).
    pub speaker: String,
    /// Number of matching lines, so a meeting full of hits reads as such.
    pub count: u32,
}

/// Characters of context kept either side of a match.
const SNIPPET_PAD: usize = 60;

/// A one-line excerpt centred on `at`, with ellipses where text was cut. Cuts
/// land on char boundaries (transcripts are full of non-ASCII punctuation).
fn snippet_around(text: &str, at: usize, needle_len: usize) -> String {
    let start = text[..at].char_indices().rev().nth(SNIPPET_PAD).map(|(i, _)| i);
    let end_from = at + needle_len;
    let end = text[end_from..].char_indices().nth(SNIPPET_PAD).map(|(i, _)| end_from + i);
    let mut out = String::new();
    if start.is_some() {
        out.push('…');
    }
    out.push_str(text[start.unwrap_or(0)..end.unwrap_or(text.len())].trim());
    if end.is_some() {
        out.push('…');
    }
    out
}

/// Case-insensitive search across every meeting's transcript: speaker names and
/// spoken text from the structured sidecar, falling back to the markdown for
/// meetings recorded before sidecars existed. Titles match too, so searching a
/// meeting's name still finds it when nobody said the word aloud.
///
/// Reads each transcript from disk per call — fine for a personal archive, and
/// the frontend debounces. Revisit if this ever grows to thousands of meetings.
pub fn search(query: &str) -> Vec<SearchHit> {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return Vec::new();
    }
    let mut hits = Vec::new();
    for meta in MeetingStore::global().list() {
        if let Some(hit) = search_one(&meta, &needle) {
            hits.push(hit);
        }
    }
    hits
}

fn search_one(meta: &MeetingMeta, needle: &str) -> Option<SearchHit> {
    let mut first: Option<(String, String)> = None; // (speaker, snippet)
    let mut count = 0u32;

    if let Some(utterances) = load_transcript(&meta.id) {
        for u in &utterances {
            let haystack = u.text.to_lowercase();
            let speaker_match = u.speaker.to_lowercase().contains(needle);
            let Some(at) = haystack.find(needle).or(speaker_match.then_some(0)) else {
                continue;
            };
            count += 1;
            if first.is_none() {
                let snippet = if speaker_match && !haystack.contains(needle) {
                    u.text.chars().take(SNIPPET_PAD * 2).collect()
                } else {
                    snippet_around(&u.text, at, needle.len())
                };
                first = Some((u.speaker.clone(), snippet));
            }
        }
    } else if !meta.transcript_path.is_empty() {
        // Pre-sidecar meeting: scan the exported markdown line by line. A file
        // that has gone missing must still fall through to the title check.
        if let Ok(text) = std::fs::read_to_string(&meta.transcript_path) {
            for line in text.lines() {
                let Some(at) = line.to_lowercase().find(needle) else { continue };
                count += 1;
                if first.is_none() {
                    first = Some((String::new(), snippet_around(line, at, needle.len())));
                }
            }
        }
    }

    // A title or tag match still surfaces the meeting, with no snippet to show.
    let labelled = meta.title.to_lowercase().contains(needle)
        || meta.tags.iter().any(|t| t.to_lowercase().contains(needle));
    if count == 0 && labelled {
        return Some(SearchHit { id: meta.id.clone(), snippet: String::new(), speaker: String::new(), count: 0 });
    }
    let (speaker, snippet) = first?;
    Some(SearchHit { id: meta.id.clone(), snippet, speaker, count })
}

/// Transcript markdown: title, the user's own notes verbatim, then the
/// merged display blocks as `**[HH:MM:SS] Speaker:** text` lines (wall clock).
pub fn transcript_markdown(meta: &MeetingMeta, notes: &str, utterances: &[Utterance]) -> String {
    let mut out = format!("# {}\n\nStarted: {}\n", meta.title, meta.started);
    if !notes.trim().is_empty() {
        out.push_str("\n## Notes\n\n");
        out.push_str(notes.trim_end());
        out.push('\n');
    }
    out.push_str("\n## Transcript\n\n");
    for b in merge_for_display(utterances) {
        out.push_str(&format!("**[{}] {}:** {}\n\n", fmt_wall(&meta.started, b.t_ms), b.speaker, b.text));
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
            processing: false,
            tags: Vec::new(),
        }
    }

    #[test]
    fn backfill_tags_only_recurring_titles() {
        let titled = |id: &str, title: &str| {
            let mut m = meta(id);
            m.title = title.into();
            m
        };
        let mut items = vec![
            titled("a", "Standup"),
            titled("b", "standup "),   // same series, different typing
            titled("c", "Planning"),   // one-off, stays untagged
        ];
        assert!(backfill_series_tags(&mut items));
        assert_eq!(items[0].tags, vec!["standup"]);
        assert_eq!(items[1].tags, vec!["standup"]);
        assert!(items[2].tags.is_empty());
        // Idempotent: a second sweep adds nothing.
        assert!(!backfill_series_tags(&mut items));
    }

    #[test]
    fn series_slug_folds_case_and_punctuation() {
        assert_eq!(series_slug("Standup"), "standup");
        assert_eq!(series_slug("  standup! "), "standup");
        assert_eq!(series_slug("Stand up"), "stand-up"); // spacing is a real difference
        assert_eq!(series_slug("1-1 Ryan"), "1-1-ryan");
        assert_eq!(series_slug("Weekly  ·  Sync"), "weekly-sync");
        assert_eq!(series_slug("   "), "");
    }

    #[test]
    fn snippet_keeps_context_and_marks_where_it_cut() {
        let line = "We should check the Grafana dashboard before the release goes out";
        let at = line.find("Grafana").unwrap();
        // Short line: nothing trimmed, so no ellipses.
        assert_eq!(snippet_around(line, at, 7), line);

        let long = format!("{} Grafana {}", "a ".repeat(80), "b ".repeat(80));
        let at = long.find("Grafana").unwrap();
        let snippet = snippet_around(&long, at, 7);
        assert!(snippet.starts_with('…') && snippet.ends_with('…'));
        assert!(snippet.contains("Grafana"));
        assert!(snippet.len() < long.len());
    }

    #[test]
    fn snippet_cuts_on_char_boundaries() {
        // Multi-byte either side: slicing mid-character would panic.
        let line = format!("{}—Grafana—{}", "é".repeat(80), "ü".repeat(80));
        let at = line.find("Grafana").unwrap();
        let snippet = snippet_around(&line, at, 7);
        assert!(snippet.contains("Grafana"));
    }

    #[test]
    fn clear_stale_processing_flips_only_flagged_entries() {
        let mut items = vec![meta("a"), { let mut m = meta("b"); m.processing = true; m }];
        assert!(clear_stale_processing(&mut items));
        assert!(items.iter().all(|m| !m.processing));
        assert!(!clear_stale_processing(&mut items)); // nothing left to change
    }

    #[test]
    fn transcript_markdown_golden() {
        let utterances = vec![
            Utterance { source: "mic".into(), speaker: "You".into(), text: "Morning all.".into(), t_ms: 1_000, embedding: None },
            Utterance { source: "system".into(), speaker: "Speaker 1".into(), text: "Morning. Let's start.".into(), t_ms: 4_500, embedding: None },
        ];
        let md = transcript_markdown(&meta("m1"), "ship friday\nrollback: priya", &utterances);
        let expected = "# Weekly sync\n\nStarted: 2026-07-07T14:30:00Z\n\n## Notes\n\nship friday\nrollback: priya\n\n## Transcript\n\n**[14:30:01] You:** Morning all.\n\n**[14:30:04] Speaker 1:** Morning. Let's start.\n\n";
        assert_eq!(md, expected);
    }

    #[test]
    fn transcript_markdown_merges_consecutive_same_speaker() {
        let utterances = vec![
            Utterance { source: "mic".into(), speaker: "You".into(), text: "So I scroll down,".into(), t_ms: 22_000, embedding: None },
            Utterance { source: "mic".into(), speaker: "You".into(), text: "then I add the URLs.".into(), t_ms: 29_000, embedding: None },
            Utterance { source: "system".into(), speaker: "Ed".into(), text: "Interesting.".into(), t_ms: 37_000, embedding: None },
        ];
        let md = transcript_markdown(&meta("m1"), "", &utterances);
        assert!(md.contains("**[14:30:22] You:** So I scroll down, then I add the URLs.\n"));
        assert!(md.contains("**[14:30:37] Ed:** Interesting.\n"));
        assert!(!md.contains("14:30:29"));
    }

    #[test]
    fn merge_keeps_blocks_apart_across_long_gaps() {
        let utterances = vec![
            Utterance { source: "mic".into(), speaker: "You".into(), text: "First remark.".into(), t_ms: 0, embedding: None },
            Utterance { source: "mic".into(), speaker: "You".into(), text: "Much later remark.".into(), t_ms: MERGE_GAP_MS + 1_000, embedding: None },
        ];
        let blocks = merge_for_display(&utterances);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].text, "First remark.");
        assert_eq!(blocks[1].text, "Much later remark.");
    }

    #[test]
    fn merge_gap_is_measured_between_neighbours_not_block_start() {
        // A long monologue: every utterance within the gap of its neighbour
        // stays one block even when the block's total span exceeds the gap.
        let utterances: Vec<Utterance> = (0..4)
            .map(|i| Utterance {
                source: "mic".into(),
                speaker: "You".into(),
                text: format!("part {i}."),
                t_ms: i * 20_000,
                embedding: None,
            })
            .collect();
        let blocks = merge_for_display(&utterances);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].text, "part 0. part 1. part 2. part 3.");
        assert_eq!(blocks[0].t_ms, 0);
    }

    #[test]
    fn fmt_wall_falls_back_to_relative_on_bad_start() {
        assert_eq!(fmt_wall("not-a-date", 61_000), "01:01");
        assert_eq!(fmt_wall("2026-07-07T14:30:00+01:00", 61_000), "14:31:01");
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
