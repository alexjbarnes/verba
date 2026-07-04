# Licences and attributions

Third-party data, models, and code that Verba bundles, downloads, or links.
Verba's own code is under the repository's licence; everything below keeps its
original licence.

## Bundled data (compiled into the app)

- **CMU Pronouncing Dictionary** (`src-tauri/data/cmudict_data.json`) —
  Carnegie Mellon University, BSD-style 2-clause licence.
  https://github.com/cmusphinx/cmudict
- **British English pronunciation dictionary** (`src-tauri/data/gb_dict.json`) —
  derived from [WikiPron](https://github.com/CUNY-CL/wikipron) scrapes of
  Wiktionary (`eng_latn_uk_broad`). Pronunciation data originates from
  Wiktionary and is licensed **CC BY-SA 3.0** (attribution: Wiktionary
  contributors). The WikiPron tool itself is Apache-2.0. Stress positions are
  transferred from the CMU Pronouncing Dictionary (licence above). Built by
  `scripts/build_gb_dict.py`.
- **SymSpell frequency dictionaries**
  (`src-tauri/data/frequency_dictionary_en_82_765.txt`,
  `frequency_bigramdictionary_en_243_342.txt`) — from
  [SymSpell](https://github.com/wolfgarbe/SymSpell), MIT licence.
- **Grammar models** (`src-tauri/data/grammar/`) — acceptability router
  fine-tuned from Google **ELECTRA-small** (Apache-2.0) and corrector
  fine-tuned from Google **T5-efficient-tiny** (Apache-2.0).

## Vendored code

- **Readability** (`src/readability.js`) — Copyright (c) 2010 Arc90 Inc,
  maintained by Mozilla. Apache-2.0.
  https://github.com/mozilla/readability

## Downloadable voice models (TTS)

Both voices are [Piper](https://github.com/rhasspy/piper) (MIT) VITS models
from [rhasspy/piper-voices](https://huggingface.co/rhasspy/piper-voices),
re-hosted with a patch that exposes the model's duration output (no weight
changes). The upstream voices were trained using espeak-ng phonemisation;
**espeak-ng (GPL-3.0) is not included in, linked by, or executed by this
app** — phonemisation is done with the bundled dictionaries above.

- **en_US-libritts_r-medium** — trained on
  [LibriTTS-R](http://www.openslr.org/141/) (**CC BY 4.0**).
- **en_GB-alba-medium** — trained on the Alba corpus,
  [Edinburgh DataShare](https://datashare.ed.ac.uk/handle/10283/3270)
  (**CC BY 4.0**); fine-tuned from the lessac voice.

## Downloadable speech-recognition models (ASR)

Converted and published for sherpa-onnx by
[csukuangfj](https://huggingface.co/csukuangfj); see each model's card for
dataset details.

- **Whisper** (base.en, turbo, large-v3) — OpenAI, MIT licence.
  https://github.com/openai/whisper
- **Parakeet TDT 0.6B** (v2/v3) — NVIDIA NeMo, **CC BY 4.0**.
- **Conformer-CTC** (small/medium/large) — NVIDIA NeMo, **CC BY 4.0**.
- **Zipformer** — k2-fsa/icefall (Apache-2.0), trained on LibriSpeech
  (**CC BY 4.0**).

## Linked libraries (selection)

- **sherpa-onnx** — Apache-2.0 (statically linked; includes the
  **Silero VAD** model, MIT, https://github.com/snakers4/silero-vad).
- **ONNX Runtime** — MIT.
- **piper-plus-g2p** — MIT.
- **Tauri** — MIT/Apache-2.0.
- Rust crate dependencies are predominantly MIT and/or Apache-2.0; the full
  set is enumerable with `cargo license` in `src-tauri/`.

## Loaded at runtime (CDN, not redistributed)

- **Tailwind CSS** — MIT.
- **Inter** typeface — SIL Open Font License 1.1.
- **Material Symbols** — Apache-2.0 (Google Fonts).
