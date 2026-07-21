# Voice review

Working checklist for the full voice pass, 2026-07-14. Listen in the app:
Listen mode -> Voices tab. Every voice auditions from a streamed sample before
any download; VCTK is one 77 MB download that covers all its speakers.

Status: `keep` / `remove` / blank = not yet reviewed. Edit this file as you go.
Entries are NEVER deleted from this file - removed voices keep their row and
notes for future context; removal only takes them out of the app's manifest.
Drift = pitch spread across 8 test sentences, in semitones; under ~2 sounds
like one steady reader, 3+ is the different-people effect. Measured values
only exist where a sweep was run; blank means unmeasured, judge by ear.

## Live voices (all users see these today)

| Voice | Sex | Accent | Drift | Status | Notes |
|---|---|---|---|---|---|
| Alba | F | Scottish | 2.7 | keep | Favourite. Default voice. |
| Jenny | F | Irish (not southern English) | 2.1 | keep for now (2026-07-14) | Favourite. Dioco dataset, Dublin speaker. User: listenable but does have some voice variance (matches the 2.1 st drift - mild, about half the removed voices'). |
| Alan | M | English | 1.8 | | Steadiest live voice. |
| Cori | F | accent unverified | 2.3 | keep for now (2026-07-14) | User (post-fix build): very similar to Cori HQ, maybe slightly more noticeable variance. |
| Cori HQ | F | accent unverified | | keep for now (2026-07-14) | User: OK, not a favourite - quite a bit of variance, a little robotic in places. Judged on the post-fix build (13 Jul phoneme fix already applied), so this is her true ceiling. |
| Northern English Male | M | Northern English | 3.2 | **remove** (2026-07-14) | User: same issues as Southern English Female - jumps around, sounds like completely different voices. Consistent with the 3.2 st measured drift. VCTK northern males (Yorkshire, Newcastle, Cumbria, Manchester) are the replacements. |
| Southern English Female | F | Southern English | 3.5 | **remove** (2026-07-14) | User: too much variance, sounds like different voices. Measured 3.5 st text-driven register drift; unfixable (baked into the low-quality weights, no medium version upstream). Replacement: VCTK southern females below. Removal executes with the end-of-review batch. |
| Prudence (semaine) | F | English | 3.7 | keep for now (2026-07-14) | User: similar to Poppy - quite high, jumps to squeaky at points; sounds a little Chinese in places; not a favourite but listenable. (Highest measured drift of any voice, 3.7 st - the squeaky jumps are that register instability.) |
| Spike (semaine) | M | English | | **remove** (2026-07-14) | User: doesn't flow - each word delivered separately like its own sentence; struggles with p's and f's. (Spike is the corpus's angry character - the staccato is the acted affect, and it doesn't suit reading.) |
| Obadiah (semaine) | M | English | | keep for now (2026-07-14) | User: not a favourite - quite a slow, dull voice - but technically fine. (In the corpus Obadiah is the gloomy character, so the dullness is the acted affect.) |
| Poppy (semaine) | F | English | | keep for now (2026-07-14) | User: quite high pitched, not a favourite but seems OK; accent drifts a little Chinese/Australian in places. |
| Aru 1 | ? | UK (per-speaker accent unknown) | 1.0 | | |
| Aru 2 | ? | UK (per-speaker accent unknown) |  | | |
| Aru 3 | ? | UK (per-speaker accent unknown) |  | keep (2026-07-20) | User: fine. |
| Aru 4 | ? | UK (per-speaker accent unknown) |  | **remove** (2026-07-20) | User: sounds very robotic. Per-speaker removal (catalogue entry) - executes with the end-of-review batch. |
| Aru 5 | ? | UK (per-speaker accent unknown) |  | **remove** (2026-07-20) | User: doesn't flow well and doesn't sound right. Per-speaker removal (catalogue entry) - executes with the end-of-review batch. |
| Aru 6 | F | UK (per-speaker accent unknown) |  | keep (2026-07-20) | User: really good - probably favourite female English voice. |
| Aru 7 | F | UK (per-speaker accent unknown) |  | keep for now (2026-07-20) | User: fine, not a favourite, but the catalogue is short on female voices and this one is OK. |
| Aru 8 | ? | UK (per-speaker accent unknown) |  | keep (2026-07-20) | User: quite nasal, but quite listenable - doesn't mind it. |
| Aru 9 | M | UK (per-speaker accent unknown) |  | keep (2026-07-14) | User: similar to Aru 10 but better. Likes Aru overall; flagged flow-improvement as a wider question (see Flow note below table). |
| Aru 10 | M | UK (per-speaker accent unknown) |  | keep (2026-07-14) | User: flow not the best but probably the best male voice so far. First outright keep among the Aru speakers. |
| Aru 11 | ? | UK (per-speaker accent unknown) |  | keep for now (2026-07-14) | User: like the voice, but doesn't always flow perfectly (similar to Aru 12). |
| Aru 12 | ? | UK (per-speaker accent unknown) | | keep for now (2026-07-14) | User: quite like it overall; occasionally doesn't flow well (words more than sentences) but infrequent. |

## VCTK UK speakers (NEW - in the Voices tab under More voices)

One model, 70 UK speakers by documented accent. Six auditioned already:
https://claude.ai/code/artifact/0dfd071a-19e4-4f05-b727-a6c0bc4f6a79

### Female

| Speaker | Age | Region | Drift | Status | Notes |
|---|---|---|---|---|---|
| p225 | 23 | Southern England | 1.2 | | |
| p228 | 22 | Southern England | 1.7 | | |
| p229 | 23 | Southern England | 0.6 | | |
| p231 | 23 | Southern England | 1.2 | | |
| p240 | 21 | Southern England | 1.1 | | |
| p257 | 24 | Southern England | 1.3 | | |
| p268 | 23 | Southern England | 1.9 | | |
| p236 | 23 | Manchester |  | | |
| p244 | 22 | Manchester |  | | |
| p269 | 20 | Newcastle |  | | |
| p282 | 23 | Newcastle |  | | |
| p277 | 23 | Northeast England |  | | |
| p276 | 24 | Oxford | 1.7 | | |
| p250 | 22 | Southeast England | 1.5 | | |
| p239 | 22 | Southwest England |  | | |
| p233 | 23 | Staffordshire |  | | |
| p230 | 22 | Stockton-on-tees |  | | |
| p234 |  | UK (region unconfirmed) |  | | |
| p238 |  | UK (region unconfirmed) |  | | |
| p249 |  | UK (region unconfirmed) |  | | |
| p253 |  | UK (region unconfirmed) |  | | |
| p261 |  | UK (region unconfirmed) |  | | |
| p262 |  | UK (region unconfirmed) |  | | |
| p264 |  | UK (region unconfirmed) |  | | |
| p265 |  | UK (region unconfirmed) |  | | |
| p266 |  | UK (region unconfirmed) |  | | |
| p280 |  | UK (region unconfirmed) |  | | |
| p288 |  | UK (region unconfirmed) |  | | |
| p293 |  | UK (region unconfirmed) |  | | |
| p295 |  | UK (region unconfirmed) |  | | |
| p313 |  | UK (region unconfirmed) |  | | |
| p335 |  | UK (region unconfirmed) |  | | |
| p340 |  | UK (region unconfirmed) |  | | |
| p351 |  | UK (region unconfirmed) |  | | |
| p267 | 23 | Yorkshire |  | | |

### Male

| Speaker | Age | Region | Drift | Status | Notes |
|---|---|---|---|---|---|
| p232 | 23 | Southern England |  | | |
| p258 | 22 | Southern England |  | | |
| p263 | 22 | Aberdeen |  | | |
| p247 | 22 | Argyll |  | | |
| p292 | 23 | Belfast |  | | |
| p304 | 22 | Belfast |  | | |
| p256 | 24 | Birmingham |  | | |
| p278 | 22 | Cheshire |  | | |
| p227 | 38 | Cumbria |  | | |
| p364 | 23 | Donegal |  | | |
| p245 | 25 | Dublin |  | | |
| p252 | 22 | Edinburgh |  | | |
| p272 | 23 | Edinburgh |  | | |
| p281 | 29 | Edinburgh |  | | |
| p285 | 21 | Edinburgh |  | | |
| p274 | 22 | Essex |  | | |
| p237 | 22 | Fife |  | | |
| p271 | 19 | Fife |  | | |
| p284 | 20 | Fife |  | | |
| p255 | 19 | Galloway |  | | |
| p279 | 23 | Leicester |  | | |
| p243 | 22 | London |  | | |
| p275 | 23 | Midlothian |  | | |
| p286 | 23 | Newcastle |  | | |
| p259 | 23 | Nottingham |  | | |
| p260 | 21 | Orkney |  | | |
| p241 | 21 | Perth |  | | |
| p246 | 22 | Selkirk |  | | |
| p273 | 23 | Suffolk |  | | |
| p226 | 22 | Surrey |  | | |
| p254 | 21 | Surrey |  | | |
| p298 | 19 | Tipperary |  | | |
| p283 |  | UK (region unconfirmed) |  | | |
| p287 | 23 | York |  | | |
| p270 | 21 | Yorkshire |  | | |

## Staged US voices (manifest only - NOT visible in the app yet)

17 voices uploaded to R2 but held out of the live list pending review.
Caveat: kristin, ljspeech, bryce, john, norman declare espeak voice "en" and
now take the GB phoneme path (correct, matches their training), so their old
review samples are stale - regenerate before judging those five.

| Voice | Sex | Notes |
|---|---|---|
| amy | F | |
| hfc-female | F | |
| kathleen | F | |
| kristin | F | Stale sample (bare-en fix). |
| lessac | F | |
| ljspeech | F | Stale sample (bare-en fix). |
| bryce | M | Stale sample (bare-en fix). |
| danny | M | |
| hfc-male | M | |
| joe | M | |
| john | M | Stale sample (bare-en fix). |
| kusal | M | |
| norman | M | Stale sample (bare-en fix). |
| reza-ibrahim | M | |
| ryan | M | |
| sam | M | |
| libritts-r | mixed | |

## Attribution

VCTK is CC BY 4.0 (CSTR, University of Edinburgh) - attribution ships in the
licences screen alongside the existing voice credits.
