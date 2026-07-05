# APK Slimming Analysis

Method: measured, not estimated. All numbers below come from `unzip`/`zipfile`
inspection of a real build, `readelf`/`size` on the extracted native library,
`stat` on every `include_bytes!`/`include_str!` source file, and direct reading
of `models.rs`, `piper.rs`, `grammar_neural.rs`, `build.rs`, and the Gradle
build files.

## A note on which APK was measured

`verba.apk` at the repo root (39,122,171 bytes) is corrupted. Both `unzip -l`
and Python's `zipfile` fail to open it ("cannot find zipfile directory" / "File
is not a zip file"). Its timestamp (22:30) is five minutes after the last good
build artifact (22:25), which points to an interrupted `just apk` run: the
`_strip` and `_repackage` steps completed, but the `_sign` step (zipalign +
apksigner) did not finish cleanly, leaving a truncated file at the destination
path.

This analysis instead uses
`src-tauri/gen/android/app/build/outputs/apk/universal/release/app-universal-release-unsigned.apk`,
which is the exact intermediate artifact the `just` pipeline produces right
before signing (single ABI, arm64-v8a only, already stripped, matching the
`_repackage` step's output path). `ls -lh` reports it as 111M
(115,699,803 bytes, 110.3 MiB), matching the figure this task was scoped
against. Recommend regenerating `verba.apk` before relying on it again.

## APK breakdown by category

| Category | Files | Compressed bytes | MiB | % of APK |
|---|---|---|---|---|
| lib/arm64-v8a | 3 | 113,144,328 | 107.9 | 97.8% |
| resources.arsc | 1 | 1,037,888 | 1.0 | 0.9% |
| dex | 1 | 989,491 | 0.9 | 0.9% |
| res/ (density PNGs, AndroidX drawables) | 868 | 371,947 | 0.35 | 0.3% |
| assets/ (webview bundle, this build) | 6 | 9,609 | 0.01 | 0.01% |
| META-INF + kotlin metadata + manifest | 62 | ~15,300 | 0.01 | 0.01% |

Native libraries dominate. Everything else combined is under 2.2% of the APK.

| Native library | Bytes | MiB | % of APK | Link type |
|---|---|---|---|---|
| lib/arm64-v8a/libverba_rs_lib.so | 78,137,088 | 74.5 | 67.5% | app cdylib, static-links sherpa-onnx |
| lib/arm64-v8a/libonnxruntime.so | 25,770,888 | 24.6 | 22.3% | prebuilt, dlopen'd at runtime |
| lib/arm64-v8a/libc++_shared.so | 9,236,352 | 8.8 | 8.0% | NDK C++ runtime |

Only one ABI ships (arm64-v8a). There is no unused-ABI fat to trim.

## Inside libverba_rs_lib.so (78.1 MB)

`readelf -S` on the extracted library:

| Section | Bytes | % of .so |
|---|---|---|
| .rodata | 61,088,060 | 78.2% |
| .text | 9,832,376 | 12.6% |
| .data.rel.ro | 2,029,816 | 2.6% |
| .rela.dyn | 2,722,824 | 3.5% |
| .gcc_except_table | 827,040 | 1.1% |
| .eh_frame + .eh_frame_hdr | 1,585,024 | 2.0% |

No `.symtab`/`.debug_*` sections are present, so this build already reflects
the pipeline's `strip --strip-unneeded` step. `.text` at 9.8 MB is what
remains, after `--gc-sections`, of roughly 226 MB of statically linked
sherpa-onnx archives (see below). `.rodata` at 61.1 MB is almost entirely
`include_bytes!`/`include_str!` payloads, confirmed below.

## include_bytes! / include_str! inventory

| Source | Embedded file | Bytes | MiB |
|---|---|---|---|
| src/postprocess/grammar_neural.rs | cola_model_quantized.0.0.1.onnx (router) | 14,051,413 | 13.4 |
| src/postprocess/grammar_neural.rs | cola_tokenizer.0.0.1.json | 711,648 | 0.68 |
| src/postprocess/grammar_neural.rs | encoder_model_quantized.0.0.1.onnx (T5) | 11,503,627 | 11.0 |
| src/postprocess/grammar_neural.rs | decoder_with_past_quantized.0.0.1.onnx (T5) | 20,340,669 | 19.4 |
| src/postprocess/grammar_neural.rs | cross_attn_kv_weights.0.0.1.bin | 2,097,152 | 2.0 |
| src/postprocess/grammar_neural.rs | t5_tokenizer.0.0.1.json | 2,422,331 | 2.3 |
| src/postprocess/grammar_neural.rs | config.0.0.1.json (include_str!) | 259 | 0.0002 |
| src/piper.rs | cmudict_data.json | 3,748,042 | 3.6 |
| src/piper.rs | gb_dict.json | 1,992,744 | 1.9 |
| src/models.rs | silero_vad.onnx | 643,854 | 0.6 |

Grammar subtotal: 51,126,840 bytes (48.8 MiB, 44.2% of the whole APK, 65.4%
of the .so). All three groups together: 57,511,480 bytes (54.9 MiB, 49.7%
of the APK). That accounts for 57.5 of the .so's 61.1 MB of `.rodata`. The
remaining ~3.6 MB is ordinary Rust/C++ string and table data. Roughly half
of this APK, by weight, is these three embeds.

## What already lives outside the APK (do not re-migrate)

- ASR models (Whisper, Parakeet, Conformer-CTC, Zipformer): downloaded from
  Hugging Face (`huggingface.co/csukuangfj/...`) via `ModelManager`, not R2,
  and not embedded. Correction to the task premise: R2 is not currently used
  for ASR.
- TTS Piper voice acoustic models: downloaded from R2
  (`R2_PIPER_TTS = pub-c88baaac61224fbba973b547f1d947ca.r2.dev/models/TTS`),
  63-114 MB each, loaded via `Session::builder()...commit_from_file(path)` in
  `piper.rs`. Confirmed not `include_bytes!`. This directly answers the
  memory-hint check: voice models are already the download-on-demand pattern
  this task wants applied elsewhere. Nothing to do here.

R2 today hosts only the custom duration-patched Piper exports (not available
from any public host). Grammar models are in the same position: they are
Verba fine-tunes with no public host, so R2 is the correct and only home if
they move, mirroring the TTS precedent rather than inventing a new mechanism.

## Candidate table

| Item | Size | Current mechanism | Move to R2? | User-visible cost | Effort | Risk |
|---|---|---|---|---|---|---|
| Grammar router (CoLA) | 14.8 MB (model+tokenizer) | include_bytes!, compile-time cfg-gated, `commit_from_memory` | Yes | First grammar-stage use downloads ~14.8 MB, or silently stays a no-op like today if skipped | M | Moderate |
| Grammar T5 corrector | 36.4 MB (encoder+decoder+kv+tokenizer) | include_bytes!, compile-time cfg-gated, `commit_from_memory` | Yes | Bundled with router download, ~36.4 MB more | M | Moderate |
| CMUdict + gb_dict (piper phonemizer data) | 5.5 MB | include_bytes!, always compiled in | Yes | Gate on first TTS use, ~5.5 MB download | S/M | Low-moderate |
| Silero VAD | 0.6 MB | include_bytes! then written to a cache file and loaded from disk | Yes | Fold into existing first-run ASR model download | S | Low |
| TTS Piper voice models | 63-114 MB each | Already R2 download, `commit_from_file` | Already done | N/A | None | None |
| pdf.js + fflate + readability + import.js | ~2.1 MB source (pdf.js alone is 1.8 MB) | webview asset, copied verbatim from `src/` (frontendDist, no build step) | Partial | Would need the WebView asset-serving path (`RustWebViewClient.kt`) to read from app-data dir instead of bundled assets | M | Low reward for the effort |
| Unused ABI libs | 0 | N/A | N/A | Already single-ABI (arm64-v8a only) | - | - |
| Resource shrinking (`shrinkResources`) | up to 0.35 MB (all of res/) | Not enabled. `isMinifyEnabled=true` (R8) is on, `shrinkResources` is absent | N/A (local Gradle flag, not R2) | None | S | Low, but res/ is already tiny so payoff is marginal |
| sherpa-onnx feature trimming | .text is 9.8 MB from ~226 MB of static archives | Already builds with TTS/JNI/Python/tests/portaudio/websocket OFF (`android-build.sh`). `--gc-sections` already strips ~96% | N/A | None | L | Low confidence of further meaningful savings |
| ORT execution-provider trimming | libonnxruntime.so, 25.8 MB | Prebuilt shared lib, dlopen'd, shared by ASR+grammar+TTS | No | Needs a from-source minimal ORT rebuild. Not deferrable to R2 (must be present at process start for any inference) | L | High effort, uncertain payoff (rough guess 4-8 MB). Downloading executable native code post-install also carries its own Play-policy and compatibility risk |

Frontend byte sizes above are from current `src/` (source of truth for a
fresh build): the measured APK's `assets/` is stale (the Android project's
copied asset snapshot dates to Jun 14, main.js there is 13 KB vs 162 KB in
current source, and pdfjs/fflate.js/readability.js/import.js are entirely
absent from that stale copy). A fresh `just apk` build will pull in the full
~2.1 MB via `frontendDist: "../src"`. Either way this is under 2% of the APK.

## Tiered size projection

| Stage | Removes | Running total | MiB | % of baseline |
|---|---|---|---|---|
| Baseline (measured) | - | 115,699,803 | 110.3 | 100% |
| Quick wins (Silero VAD + CMUdict/gb_dict to R2, enable shrinkResources) | ~6.4-6.7 MB | ~109,000,000 | ~104 | ~94% |
| Medium (+ grammar router and T5 to R2) | ~51.1 MB | ~58,000,000 | ~55.5 | ~50% |
| Aggressive (+ speculative minimal-ORT rebuild, + frontend assets to R2) | ~5.5-10 MB more, low confidence | ~48,000,000-53,000,000 | ~46-51 | ~42-46% |

The medium tier is the one number in this table backed by hard measurement
end to end (embedded-byte sizes cross-checked against the `.so`'s actual
`.rodata` size). The aggressive tier's extra savings are estimates, not
measurements, since they require code that does not exist yet (a custom ORT
build) to know for sure.

## APK vs AAB + Play Asset Delivery

Android App Bundles mainly pay off by letting Play strip unused ABIs,
densities, and languages per device. Verba already ships a single ABI with no
density-split waste, so that specific AAB benefit does not apply here. Play
Asset Delivery (on-demand or install-time asset packs) could replace
hand-rolled download code for the grammar models or TTS voices with Play's
own delivery and integrity-checking infrastructure, but it only works for
Play Store distribution. The build pipeline signs with a debug keystore via
direct `apksigner` (`justfile`'s `_sign` step) rather than a Play upload flow,
which suggests Verba is not currently Play-first. The R2 approach already in
use for TTS voices is distribution-channel-agnostic (works for sideloaded
APKs too) and should stay the primary mechanism. AAB/PAD is worth revisiting
only if Play Store becomes the primary distribution channel.

## Constraints worth stating plainly

- `build.rs` currently makes grammar-model inclusion an all-or-nothing
  compile-time decision. `grammar_neural_bundled` is set only if all 7
  files exist on the machine running `cargo build`. `include_bytes!` and
  `include_str!` are resolved at compile time, so there is no way today to
  ship a build where grammar correction exists as a "not yet downloaded"
  runtime state. Moving to R2 requires removing that compile-time either/or
  and replacing it with the same runtime download-status check `ModelManager`
  already does for ASR and TTS models.
- The code change in `grammar_neural.rs` is a known pattern, not a new one:
  `piper.rs` already loads its (R2-downloaded) voice model via
  `Session::builder()...commit_from_file(path)`. Grammar currently uses
  `commit_from_memory(ROUTER_MODEL_BYTES)` etc. Swapping to
  `commit_from_file` on a downloaded path is the same change piper.rs already
  made for its own model.
- `commit_from_file` lets ORT mmap the weights from app-private storage
  instead of reading them out of the process's already-mmap'd `.so` image.
  Net memory behavior is similar either way (both are ultimately demand-paged
  by the OS). The actual win is APK size, since embedded `.rodata` ships to
  every install regardless of whether the user ever uses grammar correction,
  while a downloaded file only exists on devices that fetch it.
- Preserve today's behavior for the "not downloaded" case: silently skip the
  stage, exactly like the current build-time no-op when files are absent.
  That avoids adding a new failure mode and matches the existing "grammar
  correction is best-effort" design (also skipped today for texts under 5
  words).
- Verify downloads the same way `piper.rs` already does for voices
  (`model_fingerprint`/`cache_fingerprint`-style checks), since a corrupted or
  partial 51 MB download that isn't caught would silently degrade grammar
  correction in a hard-to-diagnose way.

## Recommended sequenced plan

1. Regenerate `verba.apk` (rerun `just apk-release` end to end) so the repo
   has a valid signed artifact and future size measurements are not taken
   against a stale or corrupted file.
2. Quick wins: move Silero VAD to the existing `ModelManager` download path
   (bytes already get written to a cache file today, only the source
   changes), move CMUdict/gb_dict the same way gated on first TTS use, and
   flip on `shrinkResources` in the release build type. Low risk, ships in
   isolation, ~6.5 MB.
3. Medium, the real lever: restructure `grammar_neural.rs` to drop the
   compile-time `grammar_neural_bundled` gate, add router and T5 files as
   `ModelFile` entries in `models.rs` pointed at R2, switch
   `commit_from_memory` to `commit_from_file`, and keep the no-op fallback
   for "not downloaded yet." This alone is worth ~51 MB, roughly half the
   APK.
4. Re-measure with the same method used here (`unzip`/`zipfile` + `readelf`)
   before deciding whether the aggressive tier is worth it. Only pursue a
   custom minimal ONNX Runtime build if the ~55 MB post-medium APK is still a
   problem. It is high effort against an unverified, likely modest payoff and
   is not an R2 candidate regardless (it must ship in the APK).
5. Deprioritize frontend assets to R2: ~2 MB against a WebView asset-serving
   code change is the worst effort-to-reward ratio on this list.
