# TTS engine evaluation (beyond Piper)

Verba's spoken output (Listen mode reading articles/books, the OpenAI-compatible
`/v1/audio/speech` endpoint) uses Piper (`piper.rs` + `piper-plus-g2p` + ORT).
Piper is fast but its naturalness has a hard ceiling (it is a small VITS model,
and the whole rhasspy/piper-voices catalogue shares that architecture, so
swapping voices does not move quality much).

This documents the search for a better engine and, crucially, the **two
constraints that must both hold**:

1. **Clearly more natural than Piper** (otherwise there is no point).
2. **Real-time factor comfortably under 1 on the target CPU**, because Listen
   mode plays on demand. Anything at or above real-time drains the lookahead
   buffer and stalls mid-article.

## Kokoro-82M — best quality, but ruled out on speed

[hexgrad/Kokoro-82M](https://huggingface.co/hexgrad/Kokoro-82M) /
[onnx-community/Kokoro-82M-v1.0-ONNX](https://huggingface.co/onnx-community/Kokoro-82M-v1.0-ONNX)

- **Quality:** a genuine step up from Piper. Scores well in blind listening
  tests (TTS Arena Elo ~1056, where Piper is not competitive). Natural prosody,
  real voice character.
- **License:** Apache-2.0 (weights + code). Clean to ship.
- **ONNX:** `onnx-community/Kokoro-82M-v1.0-ONNX`, eight quantizations
  (fp32 326MB, fp16 163MB, q8f16 86MB, q8 92MB, q4 …). 24kHz.
- **Voices:** 26 English — 20 en-US (`af_*` female, `am_*` male) + 8 en-GB
  (`bf_*` female, `bm_*` male). Map cleanly onto the OpenAI voice aliases.
- **Phonemizer:** needs IPA. An **espeak-free** path exists: `misaki-rs` compiled
  with `default-features = false` (drops the GPL espeak feature) + reuse Verba's
  existing CMUdict/ARPAbet OOV dictionaries. So it would not reintroduce espeak.

**Why it is ruled out:** tried already. On the target hardware its real-time
factor is **above 1** — over a 10-minute article the lookahead buffer (even with
a 30s head start) drains and playback stalls. Worse, the **INT8 quantized model
was slower than fp32**, which is a known ONNX Runtime CPU behaviour: many CPUs
have no fast INT8 kernels, so quantization adds dequantize/quantize overhead
instead of saving time. Kokoro is ~5-10x Piper's compute per sentence; streaming
and buffering do not fix a raw-compute wall. Kokoro is the quality target but not
usable here until the hardware or the model gets faster.

## Supertonic — the speed candidate (quality under evaluation)

[Supertone/supertonic](https://huggingface.co/Supertone/supertonic) ·
sherpa-onnx int8 port
[csukuangfj2/sherpa-onnx-supertonic-tts-int8-2026-03-06](https://huggingface.co/csukuangfj2/sherpa-onnx-supertonic-tts-int8-2026-03-06)

- **Speed:** the fastest by far — RTF ~0.012 on a fast desktop CPU, still ~0.3
  on a weak e-reader CPU. That is roughly 20-80x faster than Kokoro, so buffer
  exhaustion is a non-issue.
- **Integration:** takes **raw text** (built-in normalization), so there is no
  phonemizer and no espeak. Easiest of everything to integrate. 66M ConvNeXt.
  44.1kHz. Multi-stage ONNX (text_encoder + duration_predictor + vector_estimator
  + vocoder), so it needs sherpa-onnx's glue (already linked) or its own runner.
- **Measured speed (this repo, sherpa-onnx int8 port, modest 4-thread VM):**
  RTF ~0.06-0.07 (article/book/long passages), i.e. ~15x faster than real-time.
  Piper alba on the same box/threads was ~0.03. So Supertonic is ~2x Piper's
  compute but nowhere near the real-time wall that made Kokoro unusable. Speed
  is not a concern.
- **Open risks (why it is not yet the answer):**
  - **Quality is unvalidated in blind tests** — absent from TTS Arena. Whether it
    actually beats Piper is an ears question. A/B samples generated against Piper
    alba (`tts-samples/{supertonic,piper_alba}_{article,book,long}.wav`, same
    text) — decision pending a listen.
  - **License is OpenRAIL-M** (behavioural use-restrictions), not Apache/MIT — a
    review item alongside the billing/subscription plans.
  - **One voice per model file** (the sherpa int8 port ships a single
    `voice.bin` style). en-US/en-GB named-voice coverage from the upstream
    Supertone repo is poorly documented — verify accents/voices before committing.

If Supertonic clearly beats Piper by ear and the license clears, it is the pick
(fast enough, trivial integration). If it does not beat Piper, the honest
conclusion is that a "much better AND CPU-fast-enough" voice does not exist yet
and we stay on Piper.

## Ruled out for this use case

| Model | Verdict |
|---|---|
| Kitten TTS (15M/80M) | Below Kokoro quality ("noticeably synthetic"), espeak dependency, en-US only. |
| StyleTTS2 | Heavy/diffusion, not reliably real-time on CPU; checkpoint license friction. |
| Chatterbox (Resemble) | 0.5B Llama backbone, needs GPU/VRAM. Not CPU-real-time. |
| NeuTTS Air | 748M Qwen2 LLM, GGUF-first, espeak, voice-cloning (needs a reference clip). |
| MeloTTS | Same VITS family as Piper (marginal gain) + espeak dependency. |
| OuteTTS / Parler-TTS | LLM-scale, minutes per utterance on CPU. Not viable. |
| Piper successor (`OHF-Voice/piper1-gpl`) | GPL-3.0, same VITS, no quality leap. |

## Integration notes

Adding any of these is a **new ORT engine path** in `piper.rs` (different inputs
and outputs than Piper), similar in spirit to how meeting mode added the LLM
path. Hard requirement: **no espeak-ng** in the shipped binary (GPL), which is
why the phonemizer path matters (Supertonic sidesteps it entirely with raw-text
input; Kokoro needs the misaki-rs-without-espeak route).

## Training new Piper voices

**On CPU: no.** VITS training is GPU-bound. A weak GPU already takes ~5 days for
one fine-tune; CPU is weeks to months (roughly 30-100x slower) and is not a
supported path. Don't.

**The practical answer is cloud fine-tuning:**
- Record ~1 hour of clean single-speaker audio (22050Hz mono, LJSpeech-style
  `metadata.csv`) with `piper-recording-studio`. ~4h if training from scratch
  (alba itself is the ~4h SALB corpus, a single Scottish female, CSTR Edinburgh).
- Fine-tune from a medium English checkpoint (lessac / ljspeech / cori) using
  `OHF-Voice/piper1-gpl`. The accent and voice identity come from **your data**,
  not the base checkpoint, so no alba checkpoint is needed. ~1000 added epochs,
  stop when `loss_disc_all` plateaus. Fine-tuning cuts data and compute by ~10x
  versus from scratch (~1,300 phrases vs ~13,000).
- Train on a free Kaggle GPU (30 GPU-h/week, T4/P100 16GB) or rent a 4090
  (~$0.30/hr on runpod). Cost $0-$15, time a few hours to overnight. Colab free
  works but its quota is flaky, and community notebooks break on PyTorch/Python
  version drift — budget time for environment wrangling, not the training.
- Export to ONNX; ship and run it via sherpa-onnx exactly like today's voices.

**Difficulty:** ~2-3/5 via cloud (dominated by recording quality and voice
casting, not the training command). 5/5 on CPU (effectively impossible).

**Licensing (good news):** using the GPL-3.0 `piper1-gpl` tool to TRAIN does not
make the resulting `.onnx` GPL. piper1-gpl is GPL only because it embeds espeak-ng;
the FSF position is that a program's output isn't covered by its license unless
the output copies the program itself, and trained weights don't embed training
code. Two things to watch: (1) fine-tune from a permissively-licensed checkpoint
(lessac/ljspeech/alba-family MIT) so the base data's license doesn't encumber the
voice; (2) the GPL only bites at runtime if you ship the piper1-gpl engine —
verba runs voices through sherpa-onnx, so the training tool never enters the app.
You set the new voice's own license.
