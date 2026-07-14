# TTS tweaks reference

Every knob in the verba TTS stack, what it does, its current value, and what
testing has shown. Companion to VOICES.md (per-voice review). Updated 2026-07-14.

Rule of thumb from a week of measuring: the big quality wins so far came from
the text/phoneme layer and from picking better speakers, not from synthesis
parameters. Test any tweak with the harness at the bottom before shipping it.

---

## 1. Per-voice synthesis scales

Piper takes three floats per synthesis call, read from each voice's
`.onnx.json` sidecar (`inference` block), defaults in `piper.rs`
(`default_noise_scale` etc.). They are per-voice, shippable via the R2 config
files without touching code.

| Scale | Current | What it controls | Tested? |
|---|---|---|---|
| `noise_scale` | 0.667 | Acoustic variation - timbre/pitch liveliness of each render | Yes. Halving it (0.333) trimmed cross-sentence register drift only 38->29 Hz on southern_english_female. The wobble lives in the model weights, not the noise. Dead end for that problem. |
| `noise_w` | 0.8 | Duration variation per phoneme - the rhythm/flow knob | Yes, twice. Single sentence: fewer word-gaps at 0.3-0.5. Full paragraph: measurement inverted (0.8 had fewer gaps), and the user A/B heard no clear winner. Effect is run-to-run inconsistent. Keeping 0.8. If ever revisited, 0.4-0.5 is the plausible band; below that risks flat delivery. |
| `length_scale` | 1.0 | Global tempo (inverse of speed) | Redundant with user-facing speed controls (see 3). Leave at 1.0. |

**Cache warning (applies to any scale change):** the audio cache key is
model file + PRON/GB versions + sid + speed + segment text. Scales are NOT in
the key. Shipping a new sidecar value without bumping `PRON_VERSION` gives
users a mix of old-scale cached segments and new-scale fresh ones inside the
same article - worse than either setting. Bump the version or add scales to
`tts_cache::key` first.

## 2. Inserted pause lengths (app-side, global)

`piper.rs` constants, spliced as real silence between separately-synthesized
segments. The model also sees the punctuation mark itself (segments keep it,
confirmed in `split_for_pauses`), so these gaps sit ON TOP of the model's own
punctuation intonation.

| Constant | Value | Fires at |
|---|---|---|
| `CLAUSE_PAUSE_MS` | 250 | `, ; :` and spoken dashes |
| `SENTENCE_PAUSE_MS` | 500 | `. ! ?` |
| `PARAGRAPH_PAUSE_MS` | 800 | newlines |

Cheapest whole-app "feel" lever. Natural speech clause gaps run nearer
100-150 ms, so 250 is bookish and deliberate. Changing these does NOT need a
cache bump (silence is spliced at assembly, never cached). Candidate future
setting: expose as a "pacing" preference (tight/normal/relaxed).

## 3. Speed

Two user-facing layers, both already shipped: the player speed control, and
per-voice multipliers (`tts_voice_speeds` in config.toml - e.g. voice "7" at
0.75). Speed IS part of the cache key, so each speed setting synthesizes its
own cached copies. Prefer these over `length_scale`.

## 4. Chunking and batching

`tts.rs::batch_sentences(min 15, max 45 words)` groups sentences into chunks
for streaming - it controls time-to-first-audio and cancel granularity, not
prosody (segments still split at punctuation inside `synth_chunk`). Not a
quality lever; leave unless changing playback latency behaviour.

## 5. Text and phoneme layer (the real wins so far)

Where nearly all audible improvement has come from:

- **Pronunciation dictionaries + overrides + normalize rules.** The
  `fix-tts-mispronunciation` skill documents the probe-first workflow. The
  2026-07-11 batch (68 reports) fixed OOV spelling, honorific mid-word
  expansion, unit suffixes, acronym splitting, letter-name clipping.
- **Locale routing.** Cori synthesized US phonemes through a GB model for its
  whole life until 2026-07-13 (`espeak_voice_is_gb`). Symptom was "robotic,
  odd variance". If a voice sounds systematically off, check its sidecar's
  `espeak.voice` against the loader predicate before touching anything else.
- **Punctuation reaches the model** and shapes its contour - already true, no
  action. Untested idea, medium risk: injecting commas into very long
  comma-less clauses to force phrasing breaks.

## 6. Model-level choices

- **Quality tier beats every knob.** The only 16 kHz `low` model shipped
  (southern_english_female) measured 3.5 st register drift and was removed;
  `medium` models range 0.6-2.7 st. Do not ship `low` models again.
- **Speaker choice within multi-speaker models** is free variety: VCTK's 70
  UK speakers measured 0.6-1.9 st - all steadier than every single-speaker
  live voice. Aru/semaine/VCTK speakers are one manifest label each.
- **Register wobble is baked into weights** - proven unfixable by scales.
  The fix is a different speaker or a fine-tune.
- **Fine-tuning** (new voice or repairing an existing one): runbook agreed
  2026-07 - piper1-gpl checkpoints, dataset prep on this box, training on the
  M4 (MPS, unproven) or a rented 4090. Parked.
- **The duration patch** (`patch_piper_durations.py`, `w_ceil` output) only
  exposes per-phoneme timing for word highlighting; zero audio effect.
- Piper has **no speaker blending, no emotion control, no SSML**. Semaine's
  "characters" are acted training data, not a controllable parameter.

## 7. Engine/runtime

CPU execution provider only (validated); thread count from config
(`with_intra_threads`); fp32 models. None of these change audio content,
only synthesis latency.

## 8. Not implemented - candidate DSP features, ranked

1. **Segment-edge silence trim.** Models emit their own leading/trailing
   silence per segment; trimming it before splicing our fixed pauses would
   attack the choppy-joins feel directly and make section 2's values honest.
   Low risk, measurable with the existing gap profiler.
2. **Short crossfade (5-10 ms) at splice points** instead of hard
   concatenation - removes any residual click/discontinuity.
3. **Per-voice loudness normalization** - voices ship at noticeably
   different levels; a one-time RMS alignment constant per voice in the
   catalogue would fix volume jumps when switching.
4. EQ / de-esser - overkill until a specific complaint demands it.

## 9. Proven dead ends (do not re-litigate without new evidence)

- `noise_scale` against register wobble (2026-07-14, measured).
- `noise_w` against word-by-word flow (2026-07-14, measured + user A/B).
- Improving `low`-quality models by any parameter (weights problem).
- espeak at runtime (licence decision; the g2p replacement is the pipeline).

## 10. How to test any tweak

`tts_roundtrip` runs text through the REAL pipeline into a wav:

```bash
cargo build --bin tts_roundtrip
XDG_DATA_HOME=/tmp/fresh-cache ORT_DYLIB_PATH=.desktop-deps/sherpa-onnx/lib/libonnxruntime.so \
  src-tauri/target/debug/tts_roundtrip MODEL-dur.onnx CONFIG.onnx.json text.txt out.wav meta.json SID
```

Always isolate `XDG_DATA_HOME` - the cache otherwise silently replays old
audio and invalidates the comparison (this burned one experiment already).
Analysis scripts from the July 2026 sessions: per-sentence median F0
(register stability) and internal gap profiling (flow). Listening artifacts:
voice audition https://claude.ai/code/artifact/0dfd071a-19e4-4f05-b727-a6c0bc4f6a79
and noise_w A/B https://claude.ai/code/artifact/f78c2b52-9c7e-4c33-af46-74a309787727.
