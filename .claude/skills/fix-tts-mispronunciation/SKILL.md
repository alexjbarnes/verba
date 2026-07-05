---
description: Fix reported TTS mispronunciations in verba's Piper voices (Listen mode). Use when the user pastes a batch of words that sound wrong read aloud, or a pronunciation-report JSON (word + voice). Covers the layered lookup (GB dict, US CMUdict + ARPAbet overrides, GB IPA overrides, the US->RP transform, normalize_text, heteronyms), a diagnostic probe to classify each word on the ACTUAL engine path before changing anything, the fix decision tree, cache-version bumps, and regression tests. Prevents rediscovering the mechanism every time.
---

# Fixing TTS mispronunciations

Words the user reports come from the in-app pronunciation report. Each entry has a `word` and a `voice` (e.g. `tts-piper-alba#0`). The current shipping voices are all **GB** (alba, jenny, alan, cori, ...), so **read the GB column of the probe**, not US.

## The one rule: probe first, never guess

Reasoning about ARPAbet, IPA, and the RP transform is error-prone. Two past bugs shipped because a fix looked right on paper but the audio was still wrong (heteronym digit-stripping; cache collisions). And roughly a third of reported words turn out **already correct** (the user misheard, or it is prosody, not phonemes). So always run the probe on the real engine path first, classify each word, then fix only what is genuinely broken.

```bash
cd src-tauri && PROBE_WORDS="requeuing,generated,we're,Dr. Smith,IDs" \
  cargo test --lib probe_reported_words -- --ignored --nocapture
```

No `ORT_DYLIB_PATH` needed: phonemization is pure Rust. The probe (`piper::tests::probe_reported_words`) runs each word through `normalize_text` then `phonemize` on both the GB and US dictionaries, exactly as `load()` builds them, and prints:

```
requeuing        gb=ɹiːkjˈuːɪŋ                     us=ɹiːkjˈuːɪŋ
generated        gb=dʒˈɛnəˌeɪtəd                   us=dʒˈɛnɚˌeɪtəd
```

Read the IPA aloud in your head. A string of letter-names (`ˈɛskjˈuːˈɛl...` = "S-Q-L") means the word is OOV and being spelled out.

## How pronunciation resolves (why the fix location matters)

Per word, in order (`phonemize` + `phonemize_word` in `src-tauri/src/piper.rs`):

1. **GB dict** (`data/gb_dict.json`, ~76k wikipron IPA entries) is checked first on GB voices. If present, it wins outright. A wrong entry here can only be fixed by a **GB override**, never a US one.
2. Else the **US phonemizer**: CMUdict (`data/cmudict_data.json`, ARPAbet) plus `PRONUNCIATION_OVERRIDES` (ARPAbet) merged in at load.
3. If the US result is real (not OOV), GB voices run it through **`us_to_rp`** (`src-tauri/src/gb_english.rs`) to rewrite US IPA into RP.
4. If still OOV, fallbacks fire in order: possessive, acronym-plural, British respelling, plural, prefix, compound split, then letter-by-letter spelling.

`normalize_text` runs before all of this and rewrites raw text (abbreviations, curly quotes, dashes). Heteronyms (`heteronyms.rs`) inject context-resolved readings via pseudo dict keys.

## Decision tree (per reported word)

- **Probe reads it out as letters** and the word is not a real English word -> **OOV**. Add to `PRONUNCIATION_OVERRIDES` (US ARPAbet). On GB it rides `us_to_rp` automatically, so you rarely need a GB entry too. Examples this covers: brand/tech terms, glued compounds, neologisms.
- **Probe gives a real but wrong pronunciation** and the word is in `gb_dict.json` -> **bad GB dict entry**. Add to `GB_PRONUNCIATION_OVERRIDES` (final RP IPA, matching gb_dict conventions). A US override will NOT help here (GB wins at step 1).
- **Several words wrong in the same patterned way** (a sound consistently dropped, added, or shifted by phonetic rule) -> **systematic `us_to_rp` bug**. Fix the transform in `gb_english.rs`. This is the highest-value fix: it corrects the whole class at once instead of N per-word overrides. (Precedent: the `ɚ` arm dropped the RP linking /r/ before a vowel, breaking generated / trickery / separate with one line.)
- **An abbreviation or symbol read wrong** (`Dr.` -> "drive", `e.g.` -> "ee gee") -> **text expansion** in `normalize_text`.
- **Reading depends on sentence context** (read/read, lives, live, separate as verb vs adjective) -> **heteronym** in `heteronyms.rs` (that file has its own structure: `PRONS`, `KEYS`, `is_heteronym`, `wants_alt`, `STRESS_NV`). Note the phonemizer already disambiguates a few (like separate) on its own, so probe phrases both ways before adding.
- **Probe sounds correct** -> **do not touch it**. Record it as checked-and-correct so nobody re-investigates. Likely a mishear or prosody.

## Deriving the phonemes

Do not invent ARPAbet or IPA. Copy from a word already in the dict that shares the sound.

```bash
# US ARPAbet: find a component/rhyming word
python3 -c "import json;d=json.load(open('src-tauri/data/cmudict_data.json'));print(d.get('queue'))"
# GB IPA: find a rhyming word to match conventions (NEAR = iə, etc.)
python3 -c "import json;d=json.load(open('src-tauri/data/gb_dict.json'));print(d.get('near'))"
```

**ARPAbet notes.** Key is lowercase. Vowels carry a stress digit: `1` primary, `2` secondary, `0` unstressed (e.g. `AE1`, `AH0`). Digits in the VALUE are stress and are fine. Do NOT put digits in a KEY: the tokenizer strips them (this is why heteronyms mangle keys via `dict_key()`; irrelevant for normal overrides). Build derived forms from real morphemes (`monetized` = `monetize` phones + `D`).

**GB IPA notes.** Match `gb_dict.json` conventions, not raw US IPA. NEAR is `iə` (here=`hˈiə`), stress mark `ˈ`/`ˌ` precedes the stressed syllable, no rhotic `r` before a consonant or word-finally.

## Cache versions (must bump, or old audio persists)

Cached audio is keyed by `cache_fingerprint`, which folds in these. In `src-tauri/src/piper.rs`:

- Changed `PRONUNCIATION_OVERRIDES` or `normalize_text` (affects all locales) -> bump **`PRON_VERSION`**.
- Changed `gb_dict.json`, `GB_PRONUNCIATION_OVERRIDES`, or `us_to_rp` (GB only) -> bump **`GB_DICT_VERSION`**.
- Changed both kinds -> bump both.

Skipping this means the fix is correct but the user still hears the old cached reading.

## Regression tests

Pin exact output so a later dict/transform change that reverts a fix fails loudly.

- Full engine path: add assertions to `piper::tests::reported_words_reach_phonemes_gb` using the `gb_engine()` helper. Pin the exact GB IPA the probe printed after your fix.
- Systematic transform fix: add a unit test to `gb_english.rs` tests (see `linking_r_in_rhotic_schwa`) on `us_to_rp` directly, including a word-final counter-case so you do not over-apply.
- Text expansion: assert `normalize_text("Dr. X") == "Doctor X"` (see `honorific_expansion`).

## Ship it

```bash
cd src-tauri && cargo test --lib      # all green
cd .. && just apk                     # embeds the frontend + models
```

Then commit and push (gitmoji `🔊`, per the user's flow: only when asked). In the commit body, list what was fixed, note which reported words were checked and left correct, and state the version bumps.

## Deliberately out of scope

- **Prosody / expressiveness** ("Oh, no!" wanting an exclamatory tone). The model cannot be made expressive through phoneme overrides. Skip and say so.
- **Reading symbols as words** ("/" as "slash" for paths). `split_tokens` drops most punctuation. This is a feature, not an override. Skip unless the user asks for path reading specifically.
- **Highly ambiguous heteronyms** (bow, bass, sow, address, polish). High false-positive rate. The existing heteronym set deliberately excludes them.
