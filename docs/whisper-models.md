# Whisper Model Variants

All official OpenAI Whisper checkpoints. 12 distinct models across 3 versions.

## Architecture

| Size | Params | Encoder Layers | Decoder Layers | d_model | Heads | Mel Bins |
|------|--------|---------------|---------------|---------|-------|----------|
| tiny | 39M | 4 | 4 | 384 | 6 | 80 |
| base | 74M | 6 | 6 | 512 | 8 | 80 |
| small | 244M | 12 | 12 | 768 | 12 | 80 |
| medium | 769M | 24 | 24 | 1024 | 16 | 80 |
| large | 1,550M | 32 | 32 | 1280 | 20 | 80 (v1/v2), 128 (v3) |
| turbo | 809M | 32 | 4 | 1280 | 20 | 128 |

## v1 (September 2022)

Original release. All sizes shipped together. Trained on 680,000 hours of weakly
labeled web audio. .en variants are English-only fine-tuned weights. Large is
multilingual only (no .en variant, same for all versions).

| Model | Multilingual | English-only | PyTorch Size |
|-------|-------------|-------------|-------------|
| tiny | tiny | tiny.en | ~75 MB |
| base | base | base.en | ~142 MB |
| small | small | small.en | ~466 MB |
| medium | medium | medium.en | ~1.5 GB |
| large | large-v1 | n/a | ~2.9 GB |

## v2 (December 2022)

Large only. Retrained for 2.5x more epochs with added regularization. Same
architecture, same 80 mel bins. ~5% relative error reduction on English, ~10%
on other languages vs large-v1.

| Model | English-only | PyTorch Size |
|-------|-------------|-------------|
| large-v2 | n/a | ~2.9 GB |

No v2 versions of tiny, base, small, or medium were released.

## v3 (November 2023, OpenAI DevDay)

Large only. Switched to 128 mel frequency bins (up from 80). Trained on 5 million
hours (1M weakly labeled + 4M pseudo-labeled via large-v2). Added Cantonese
language token. 10-20% error reduction over large-v2 across many languages.
License changed to Apache 2.0.

| Model | English-only | PyTorch Size |
|-------|-------------|-------------|
| large-v3 | n/a | ~2.9 GB |

## v3-turbo (October 2024)

Fine-tuned from large-v3 with decoder pruned from 32 layers to 4. Not a
distillation (unlike distil-whisper). Encoder is identical to large-v3.
Roughly comparable accuracy to large-v2. 5-8x faster on the decoder side.
Some degradation on certain languages (Thai, Cantonese).

| Model | English-only | PyTorch Size |
|-------|-------------|-------------|
| large-v3-turbo | n/a | ~1.5 GB |

## All 12 Checkpoints

| # | Model ID | Version | Params | .en variant | Release |
|---|----------|---------|--------|-------------|---------|
| 1 | tiny | v1 | 39M | yes | Sep 2022 |
| 2 | tiny.en | v1 | 39M | is .en | Sep 2022 |
| 3 | base | v1 | 74M | yes | Sep 2022 |
| 4 | base.en | v1 | 74M | is .en | Sep 2022 |
| 5 | small | v1 | 244M | yes | Sep 2022 |
| 6 | small.en | v1 | 244M | is .en | Sep 2022 |
| 7 | medium | v1 | 769M | yes | Sep 2022 |
| 8 | medium.en | v1 | 769M | is .en | Sep 2022 |
| 9 | large-v1 | v1 | 1,550M | no | Sep 2022 |
| 10 | large-v2 | v2 | 1,550M | no | Dec 2022 |
| 11 | large-v3 | v3 | 1,550M | no | Nov 2023 |
| 12 | large-v3-turbo | v3 | 809M | no | Oct 2024 |

## sherpa-onnx Availability

All 12 models have ONNX conversions by csukuangfj on HuggingFace, each with
INT8 quantized variants. Repo pattern: `csukuangfj/sherpa-onnx-whisper-{model}`.

## What Verba Registers

Currently 5 Whisper models in the registry (all INT8, English-only where available):

| Model | ONNX INT8 Size |
|-------|---------------|
| base.en | ~153 MB |
| small.en | ~357 MB |
| medium.en | ~945 MB |
| large-v3 | ~1.8 GB |
| turbo | ~1.0 GB |

Not registered: tiny (both), multilingual base/small/medium, large-v1, large-v2.

## Platform Considerations

Parakeet (transducer) is preferred over all Whisper models for mobile/Android:

- **Speed**: Parakeet uses a single encoder pass. Whisper requires 50-100+
  sequential autoregressive decoder passes per segment. On mobile ARM CPUs
  this difference is dramatic.
- **Quality**: Whisper was trained on 30-second padded segments. VAD-clipped
  short audio (which is what our pipeline produces) works against Whisper's
  design. Parakeet was trained on diverse audio lengths and handles short
  segments naturally. INT8 quantization on mobile also introduces more
  numerical precision issues that cascade through Whisper's autoregressive
  decoder.
- **Multilingual**: Whisper large-v3 and turbo support 99 languages. Parakeet
  is English-only. This is the main reason to keep Whisper available.

Plan: offer only Parakeet models on mobile. Offer Whisper alongside Parakeet on
desktop for multilingual support and lightweight options (base.en at 153 MB vs
Parakeet INT8 at 630 MB).
