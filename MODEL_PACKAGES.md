# Model packages and versioning

Design for moving Verba from a browsable model zoo to two user-visible
packages — Dictation and Voices — with server-side versioning, partial
updates, and a single "Update" affordance. Companion to APK_SLIMMING.md,
which motivates getting the grammar models (48.8 MiB, 44% of the APK) out of
the binary; this document defines the mechanism that replaces embedding.

## User-visible model

- **Dictation** is ONE package. It contains the ASR model (Parakeet TDT
  0.6B v3 — INT8 on Android, unquantized on desktop), the Silero VAD, and
  the grammar-correction models (CoLA router + T5 corrector). The user sees
  one install state and one Update button. Which component changed is never
  surfaced.
- **Voices** stays a list. Each voice downloads individually, exactly as
  today. The LIST is server-driven, so adding a voice is a manifest edit,
  not an app release. Individual voice files are not expected to change; if
  one must, its manifest entry points at a revised object name (see
  "voice revisions" below) so existing installs re-download naturally.
- The Whisper / Zipformer / Conformer / Parakeet-v2 catalogue is gone. The
  Models tab is removed; the speak-mode bottom nav drops to History,
  Snippets, Settings, More. Files from previously downloaded legacy models
  are reclaimable via Settings > Storage.

## Manifest

Single JSON document on the existing public R2 bucket (the one already
serving TTS voices):

```
https://pub-c88baaac61224fbba973b547f1d947ca.r2.dev/manifest/v1.json
```

The path carries the schema version; breaking schema changes publish a new
path, so old app builds keep reading a manifest shape they understand.

```json
{
  "schema": 1,
  "dictation": {
    "version": 1,
    "components": {
      "asr-mobile":  { "version": 1, "files": [ {"url", "rel_path", "bytes", "role"} ] },
      "asr-desktop": { "version": 1, "files": [ ... ] },
      "vad":         { "version": 1, "files": [ ... ] },
      "grammar":     { "version": 1, "files": [ ...7 files... ] }
    }
  },
  "voices": {
    "version": 1,
    "list": [ { "id", "name", "desc", "size", "engine", "files": [ ... ] } ]
  }
}
```

- `dictation.version` is the aggregate the UI compares against; it bumps
  whenever any component bumps. Component versions drive PARTIAL updates:
  only components whose version differs from the installed record are
  re-downloaded. `rel_path`s are stable per component; an update deletes the
  component's old files then downloads replacements.
- Voice entries reuse the `ModelDef`/`ModelFile` shape the registry already
  uses. `voices.version` bumps when the list changes (the UI can mention new
  voices; the Voices tab simply re-renders from the new list).
- **Voice revisions**: to update a shipped voice, upload the new file under a
  revised object name (`en_GB-alba-medium-dur.r2.onnx`) and point the
  manifest's `url` + `rel_path` at it. `is_downloaded` sees the new
  `rel_path` missing and the voice offers a re-download; the TTS audio cache
  key already folds in the model file identity, so stale audio self-heals.

## Client behavior

- **Source of truth chain**: fresh remote manifest -> cached manifest
  (`models/manifest.json`, refreshed in the background at most every 24 h and
  on explicit "Check for updates") -> embedded snapshot
  (`src-tauri/data/model-manifest.json`, compiled in via `include_str!`).
  The registry (`builtin_registry()` today) is REPLACED by parsing whichever
  manifest wins that chain: one shape, no drift between built-in and remote.
  The app never bricks offline: the snapshot always parses.
- **Installed state**: `models/packages.json` records the installed
  dictation aggregate version and per-component versions. Voices need no
  installed record (file existence is the record, as today).
- **Update check**: compare installed vs manifest versions. States:
  `not_installed`, `installed`, `update_available`, `downloading`.
  Surfaced in Settings > Updates as one Dictation row + a Check-for-updates
  button (also refreshes the voice list).
- **Install/update**: download components whose version differs, using the
  existing per-file machinery (tmp + rename, progress events, resume by
  skip-if-exists). After install the transcriber reloads if the ASR files
  changed. Byte sizes in the manifest are verified after download
  (size mismatch -> delete + error) as a cheap integrity check, mirroring
  what the voice download path relies on today.
- **Platform selection**: Android resolves `asr-mobile`, desktop
  `asr-desktop`, at manifest-parse time via `cfg(target_os)`. The transcriber
  preference order becomes platform-appropriate (INT8 first on Android,
  unquantized first on desktop) with the other variant and previously
  downloaded legacy models as fallbacks, so existing installs keep working
  before their first package install.

## What moves out of the binary (APK effect)

Per APK_SLIMMING.md measurements:

| Item | Mechanism today | After | APK delta |
|---|---|---|---|
| Grammar router + T5 (7 files) | `include_bytes!`, compile-time `grammar_neural_bundled` cfg | `grammar` component, `commit_from_file` from `models/grammar/`, stage silently no-ops until downloaded | -48.8 MiB |
| Silero VAD | `include_bytes!` then written to disk on first use | `vad` component file; `ensure_vad_model()` just returns the path or "not downloaded" | -0.6 MiB |
| `shrinkResources` | off | on (release) | -0.3 MiB |
| CMUdict + gb_dict | `include_bytes!` | **stays embedded** — 5.5 MiB buys hermetic tests, offline-first phonemization, and no download step before any TTS works | 0 |

Expected APK: ~111 MiB -> ~60 MiB.

`build.rs` loses the all-7-files-or-no-op compile gate; grammar becomes a
runtime state exactly like a not-yet-downloaded voice. Behavior for the
missing case is unchanged by design: the stage silently skips (same as
today's cfg-off builds and same as the <5-word skip).

## Meeting package (desktop only)

A third top-level manifest package, `meeting`, alongside `dictation` and
`voices` (optional in the schema, so old manifests and old apps ignore it).
It holds a `speaker` component (the ERes2Net embedding model) and a
`segmentation` component (the pyannote model) — both needed for diarization —
plus one `sum-*` component per offered summarizer LLM. The user installs the
speaker + segmentation components and exactly ONE summarizer — a RAM-tier
recommendation preselects
it (`recommended_summarizer`: <8 GB Qwen3-0.6B, 8-16 GB Qwen3-1.7B, ≥16 GB
Llama-3.2-3B; Gemma-3-1b is always listed, never the recommendation). The
chosen id lives in both `packages.json` (meeting_summarizer) and AppConfig.

Summarizer LLMs are decoder-only ONNX (q4f16, onnx-community exports) run
through `meeting/summarize.rs`'s hand-rolled KV-cache loop — the grammar T5
decoder's technique, minus the encoder: one prefill over the whole prompt
with empty caches, then one token per step with the model's `present.*`
outputs fed back as `past_key_values.*` inputs (moved as opaque DynValues so
f16 caches never get converted). Per-model facts (layers, kv heads, head
dim, EOS ids, chat template, entry `model_file`) come from an
`llm_config.json` beside the weights, authored at staging — nothing
per-family is hardcoded. Gemma/Llama exports use ONNX external-data
companions, so their files keep upstream names and `model_file` names the
entry `.onnx`.

Summarization map-reduces the transcript (~1000-token chunks, 10% overlap on
utterance boundaries → per-chunk bullets → one combine pass) with the user's
own notes leading as authoritative anchors, emitting `## Summary` /
`## Decisions` / `## Action items`.

Speaker diarization is loopback-only and two-layered. LIVE, each utterance's
embedding is matched (via its accumulated cluster voiceprint) against a
persisted gallery — enrolled people get their name, others a provisional
`Speaker N`. At STOP, `meeting/diarize.rs` runs sherpa
`OfflineSpeakerDiarization` (pyannote segmentation + the ERes2Net embedding)
over the reconstructed loopback waveform, then a pooled re-embed merge plus a
relative-floor consolidate that recover the true speaker count automatically,
and relabels the transcript (POC on AMI: ~13% DER, count recovered, vs ~37% for
the old online clusterer). Naming a speaker enrolls their voiceprint into the
gallery (`data_dir/verba/speakers.json`), so a person named once is recognized
live in later meetings.

## Hosting layout on R2

```
models/TTS/...                        (existing, unchanged)
models/grammar/1/<the 7 files>        (path carries component version)
models/vad/1/silero_vad.onnx
models/summarizer/<id>/1/llm_config.json   (new; the ONLY meeting files on R2)
manifest/v1.json
```

ASR and summarizer WEIGHTS stay on their public hosts (Hugging Face for
Parakeet + the LLMs + the pyannote segmentation model, the sherpa GitHub
release for the ERes2Net speaker model) — the
manifest points straight at them, exactly like ASR does today. Only the
authored `llm_config.json` files (a few hundred bytes each, no public home)
ship from R2. This box has no R2 write credentials: artifacts are staged
locally under `r2-staging/` with exact upload commands; the app-side code
ships first and each component activates when its objects go live.

## Storage management (Settings > Storage)

`storage_summary()` reports bytes per category: dictation package, voices,
TTS audio cache, grammar, library + books, and "unclaimed" files under
`models/` that no current registry entry owns (legacy Whisper/v2 downloads).
Each row gets a Clear action (with confirm): TTS cache uses the existing
cache-clear path; unclaimed files are deleted by scan; voices/dictation
delete their files and reset installed state.
