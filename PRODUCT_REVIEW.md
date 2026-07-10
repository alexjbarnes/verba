# Verba: Commercial Product Review

A skeptical review of Verba as a commercial product. Focus: what makes people
adopt, pay, and stay. Claims are grounded in the code and docs, cited inline.

Reviewer stance: the value here is in naming real weaknesses, not cheerleading.

**Context: this is a solo developer.** Verba is built and run by one person, and
the goal is to capture some value from a strong side project, not to build a
venture-scale business. That reframes several recommendations below. Prefer the
lowest-operations monetization (annual and lifetime licences over per-seat sales
and sync you have to run), keep the SKU count tiny, and treat your own time as
the scarcest resource. Section 4's pricing subsections are written for this.

---

## 1. Executive summary

Verba is three good local-first apps wearing one shell: real-time dictation
(`recorder.rs`, `transcribe.rs`, Parakeet), a TTS reader for articles/EPUB/PDF
with word highlighting and RSS (`library`, `feeds`, `tts_*` commands), and a
desktop meeting recorder with local diarization and local-LLM summaries
(`src-tauri/src/meeting/`). The engineering is strong and the privacy story is
genuinely differentiated: meeting mode stores no audio, only text and speaker
voiceprints (`MODEL_PACKAGES.md`), and nothing leaves the device.

The commercial reality is thinner than the roadmap suggests. The paid product
does not exist yet. There is no auth, no Stripe, no entitlement, no account UI
anywhere in `src/main.js` or the Rust backend. The subscription plan lives only
in a 95-day-old memory note, and one of its core decisions (full app lockout on
lapse) directly attacks the local-first trust that is the whole brand.

The "OpenAI-compatible HTTP API" and Android select-to-speak are real code, not
vapor: both are complete on the `local-api` branch (increments 1-4:
`/v1/models`, transcriptions with SSE, speech wav/pcm/flac, and
`ACTION_PROCESS_TEXT`), but not merged to `main`, which is why `API.md` still
reads "proposal". Treat them as near-term-shippable, not live in the current
build.

The single biggest adoption blocker is time-to-value. First run downloads
roughly half a gigabyte or more of models before a user can dictate one word,
with no onboarding, no permission priming, and no instant-first-use path
(`main.js` has no first-run flow, only a dismissable "models not downloaded"
banner in `index.html`). Dictation and voices are English-only today, which caps
the market against Wispr Flow and Superwhisper.

The moat is thin on models (all open weights: Parakeet CC BY 4.0, Whisper MIT,
Piper MIT, Qwen/Llama/Gemma) and real on execution, privacy positioning, and
accumulated personal data (voiceprint gallery, vocab, snippets, history). The
best commercial wedge is not dictation, it is confidential meetings: transcribe
and summarize a sensitive call with no bot joining and no audio leaving the room,
a promise Granola, Otter, and Fireflies structurally cannot make.

Top priorities in order: fix activation, monetize meetings without bricking the
app, add global search plus cross-capability wiring, ship E2E sync, add
multilingual dictation.

---

## 2. Positioning and differentiation

### Who it is for

Two buyers, one product:

- **Privacy-constrained professionals.** Lawyers, doctors, therapists,
  journalists, execs under NDA, GDPR-bound teams. They cannot legally or
  comfortably put client audio into Otter or Granola. For them, on-device
  meeting transcription is not a preference, it is a requirement.
- **Prosumer voice power-users.** People who dictate all day and read long-form
  on the go. They pick local for control, no subscriptions, and no data exhaust.

### The wedge

Lead with **confidential meetings on desktop**. It is the highest willingness to
pay, the clearest business use, and the sharpest contrast with cloud incumbents.
Dictation is the daily-habit driver that creates DAU, but meetings are what a
privacy buyer will open their wallet for.

The unifying story: *your voice, on your device, forever. No cloud, no account,
no audio leaves the machine.* Verba is one privacy roof over dictate, listen, and
meet.

### Local-first versus cloud incumbents

| Capability | Verba | Named competitors | Where Verba wins | Where Verba loses |
|---|---|---|---|---|
| Dictation | Local Parakeet, desktop hotkey + tray, Android accessibility IME overlay | Wispr Flow, Superwhisper, macOS/Windows built-in dictation | Privacy, offline, free models, Android IME, cross-desktop | English-only, no AI edit/command modes, no per-app formatting, less polish, no iOS |
| Meetings | Local dual-stream (mic + loopback), offline diarization, local-LLM summary, voiceprint gallery | Granola, Otter, Fireflies | No bot joins, no audio leaves device, captures system audio directly, cross-meeting speaker memory | Desktop-only, no calendar, no sharing/collaboration, weaker local LLM vs GPT-class, no mobile |
| Reading / TTS | Local Piper voices, word highlighting, EPUB/PDF/RSS, queue, speeds | Speechify, ElevenLabs Reader, Natural Reader | Local, offline, free, no account, privacy | Voice quality ceiling (admitted in `TTS_ENGINES.md`), English-only voices, no OCR/scan, no library sync |

The honest read: Verba loses head-to-head on polish, languages, and voice
quality in every category. It wins on one axis that none of the incumbents can
copy without abandoning their architecture: nothing leaves the device. Sell that
axis hard, to buyers who actually price it.

### The positioning risk nobody is naming

The audience that chooses a local app did so partly to escape cloud
subscriptions. A pure recurring sub with lockout (the current plan in
`project_auth_subscription.md`) fights the exact instinct that brought them in.
That tension runs through the whole monetization section below.

---

## 3. Feature gaps by area

### Dictation (highest-frequency surface, treat as the retention engine)

- **English-only.** The catalogue collapsed to one Parakeet model
  (`MODEL_PACKAGES.md` removed Whisper/Zipformer/Conformer). `config.language`
  exists but there is effectively one language. Wispr Flow ships 100-plus.
  This is the biggest single parity gap. *High priority.*
- **No AI command or edit mode.** Wispr and Superwhisper let you say "make this a
  bullet list" or auto-clean rambling into prose. Verba's pipeline is rule-based
  plus a tiny grammar model (`grammar_neural.rs`), not instruction-following.
  A small local instruct-LLM pass (the meeting summarizer already proves the
  decoder-loop machinery in `summarize.rs`) would close this. *High priority.*
- **No transcription editing.** History is read-only (no `contenteditable` or
  edit path in `main.js`). A bad transcription cannot be fixed in-app, only
  re-dictated. *Medium.*
- **No per-app or context formatting.** Same output everywhere. Competitors adapt
  tone to Slack versus email. *Medium.*
- **No dictation stats.** No words-dictated or time-saved counter. Wispr uses
  this as a habit hook. Cheap to add locally. *Low effort, medium impact.*

### Listen (the consumer-growth surface)

- **Voice quality is the ceiling.** `TTS_ENGINES.md` is candid: Piper has a hard
  naturalness limit, Kokoro is too slow on target CPUs, Supertonic is unproven
  and OpenRAIL-M licensed. Speechify and ElevenLabs win on ears. Until a better
  CPU-real-time voice lands, Listen cannot compete on quality, only on privacy
  and price. *High, but blocked on the model search.*
- **No OCR / scanned-document reading.** Speechify's growth feature. PDF import
  exists but image-only PDFs will not read. *Medium.*
- **No library sync.** Your saved articles and books live in local JSON only. Add
  on desktop, they are not on your phone. Kills the cross-device reading loop.
  *High (see sync below).*
- **English voices only.** 27 voices, all en_GB/en_US (`LICENCES.md`). *Medium.*

### Meeting (the monetization surface)

- **Desktop-only.** `cpal 0.18` and loopback resolution are desktop-bound
  (`loopback.rs`). Many meetings happen on laptops, so this is defensible, but it
  cuts off phone-recorded in-person meetings entirely. *Medium.*
- **No calendar integration.** Granola's core loop is calendar-aware auto-capture
  and per-event notes. Verba requires a manual press of the record button
  (`meeting_start`). *High for the meeting buyer.*
- **No sharing or export polish.** Summaries write to a local folder
  (`store.rs`). No shareable link, no send-to-Slack/Notion, no team space.
  Privacy buyers may not want cloud sharing, but they do want a clean export and
  paste. *Medium.*
- **Local summary quality risk.** A Qwen3-0.6B or Gemma-3-1b summary is not a
  GPT-class summary. For a paid meeting feature this is the quality bar buyers
  will judge. The RAM-tier recommendation helps, but set expectations. *Medium.*

### API and integrations (undifferentiated today, opportunity tomorrow)

- **Built on `local-api`, not merged.** The HTTP server (`/v1/models`,
  transcriptions with SSE, speech wav/pcm/flac) and Android `ACTION_PROCESS_TEXT`
  are complete on the `local-api` branch; `TextToSpeechService` remains unbuilt.
  Merging and surfacing the local OpenAI-compatible endpoint would give power
  users and developers a reason to pick Verba as their local STT/TTS backend, a
  wedge Ollama-style. *Medium impact, low-medium effort (mostly merge + polish).*
- **Android TextToSpeechService is the sleeper.** `API.md` identifies it
  correctly: Android has a system Settings picker for TTS engines, so
  implementing the standard interface makes every app that calls Android TTS able
  to use Verba's voices. Real leverage for the Listen brand. *Medium.*

### Cross-platform

- **No iOS.** Covered in section 7. This is the largest TAM gap for a
  dictation/reading product.
- **No cross-device anything.** No sync, no handoff, no shared account. Local-first
  makes this hard, but its absence is the top retention weakness.

---

## 4. Monetization

### The core tension

Verba has no per-use compute cost. Inference runs on the user's CPU. So the
classic SaaS justification for recurring billing (we pay for your compute) does
not apply. Charging monthly for software that runs entirely on the buyer's
machine, and that they chose specifically to avoid cloud subscriptions, needs a
better answer than "because SaaS."

### What the current plan gets wrong

From `project_auth_subscription.md`:

- **"Full app lockout when subscription lapses (not just download gating)."**
  This is the single worst product decision on the table. A user who downloaded
  models, went offline for two weeks (the exact scenario local-first sells), and
  had a card expire, opens a bricked app that runs 100% on their own hardware.
  That is a guaranteed one-star review and a betrayal of the brand promise. Do
  not ship this.
- **"3-day grace window on network failure."** Too short for a product whose
  pitch is "works offline forever." A local app should never hard-require the
  network to keep functioning with already-downloaded models.
- **Email + password only, no OAuth.** Adds signup friction to an app that today
  needs zero account. Every auth step is a conversion leak.

### The gating mechanism is also porous

`MODEL_PACKAGES.md` states ASR and summarizer weights stay on public Hugging Face
hosts, and voices sit on a public `r2.dev` bucket. The manifest points straight
at them. So "R2-gated downloads" gate nothing today: anyone reading the manifest
pulls the weights directly. Enforcing the gate means re-hosting open-weight
models (Parakeet, Whisper, Piper, Qwen, Llama, Gemma) behind private R2. Legal
(those licenses permit commercial redistribution with attribution), but it deters
only casual users, not anyone who can find the same models free upstream. Price
and package accordingly: the gate is a convenience wall, not a real one.

### What is genuinely paywall-worthy in Verba

Rank by willingness to pay and by how well each survives the "but it runs on my
machine" objection:

1. **Meeting mode.** Clear business value, replaces an $18/mo Granola or Otter
   seat, and the privacy angle is a real reason to switch. This is the anchor
   paid feature. Charge per seat for teams.
2. **Cloud E2E-encrypted sync.** The one feature with a genuine recurring server
   cost, so recurring billing is honest. E2E keeps the privacy promise intact.
   This is the cleanest subscription justification a local app can have.
3. **Premium voices for Listen.** If a Supertonic/Kokoro-class voice lands, this
   is Speechify's entire business model. Better ears, paid.
4. **Pro dictation.** Multilingual models, larger/faster ASR, the AI edit/command
   mode. Power-user upsell on the daily-habit surface.
5. **The local API and TTS engine.** A developer/power tier once shipped.

### The proposed pricing, assessed (solo developer)

The developer's plan: competitors (Granola, Wispr, ElevenReader) run about
£33/mo, so undercut hard with £2/mo per module, £5/mo for all three, or £50/yr,
and let local privacy plus a fraction of the price carry it.

Half right, and the off half is load-bearing:

- **Price is not the bottleneck, distribution and trust are.** £5 versus £33
  changes nothing if nobody finds a one-person app or trusts it with their
  meetings. Getting 200 people to the download page is far harder than choosing a
  number. Do not optimize price down before there is any funnel.
- **Do not underprice the buyer who needs you most.** The privacy-constrained
  professional (therapist, lawyer) is not comparing on features, for them Verba is
  the only usable option, worth £15-20/mo. £5 leaves that on the table, and a
  suspiciously cheap price can read as "hobby, not safe for client data" to
  exactly that buyer. Cheap can cost the sale.
- **"Cheaper because slightly worse" is a weak frame. "The only one that never
  sends your audio anywhere" is a strong one.** Sell what the incumbents
  structurally cannot be, not a discount clone. Same feature gap, different story,
  and the second supports a real price.
- **Drop per-module £2.** It fragments the one-product story, and £2 is poor unit
  economics: Stripe's 20p + 2.9% eats ~13% of it versus ~3% of a £50 annual.
  Bundle, do not itemize.
- **Right instincts:** annual, and affordable. £50/yr is a good headline. The only
  mistake is leading with monthly.

### Recommended structure (solo developer)

Your scarcest resource is your own time, not server cost. Keep the SKU count tiny
and the operations near zero.

- **Free forever:** base English dictation and basic reading. The funnel, never
  gate it.
- **One paid unlock, not three:** meetings, premium voices, multilingual, and sync
  (once it exists) in a single tier.
- **Lead with annual £50 and a lifetime around £99.** The lifetime matters, the
  core audience chose local to escape subscriptions and a perpetual licence
  converts the buyer a sub would lose. Recurring cost (R2 storage, no per-use
  compute) is tiny, so lifetime is not reckless.
- **Monthly around £6 as a low-commitment on-ramp only,** not the hero SKU.
- **A Confidential/Pro tier at £12-15/mo later,** for the compliance buyer, once
  there are testimonials. One SKU on day one, not two.
- **Never brick on lapse.** Stop sync and new downloads, keep installed features
  working.

The maths that reframes it: £50/yr times 200 customers is about £10k/yr, real
solo-dev side income and very reachable. Reaching 200 paying customers means
roughly 5,000 to 20,000 people finding the app. The bottleneck is the top of the
funnel, not the price tag. Spend the energy there.

---

## 5. Retention and habit formation

### What brings users back

- **Dictation is the retention engine.** Used many times a day. It, not meetings,
  drives DAU. Protect its friction budget above all else.
- **Accumulated personal data is the switching cost.** The voiceprint gallery
  (`gallery.rs`, recognizes people across meetings), user vocab, self-healing
  snippets (`snippets.rs`), and history all compound over time. A user six months
  in has a Verba that knows their colleagues, their jargon, and their shortcuts.
  Surface this value explicitly ("Verba recognizes 14 people you meet with").

### Cross-capability synergy is currently weak

The three modes barely talk to each other. Easy, high-value wiring:

- **Global search across history, library, and meetings.** None exists today.
  This alone turns three tools into one searchable voice workspace and raises
  switching cost sharply. *Highest-leverage retention feature.*
- **Send-to-Listen from any text.** Dictated a long note or got a meeting summary?
  Read it back with one tap. The `tts_speak` path already exists.
- **Read meeting summaries aloud.** Direct reuse of Listen inside Meeting.
- **Dictate into the library.** Voice memo becomes a readable, searchable item.

### Habit hooks

- A **words-dictated / time-saved counter** is a proven, cheap, fully-local
  retention nudge. Wispr leans on it. No server needed.

### Data and network effects, honestly

Local apps have weak network effects and no virality without a sharing surface.
Verba's "moat" is switching cost, not network effect. The voiceprint gallery is
the closest thing to a compounding data asset, and it is personal, not networked.
Do not pretend there is a flywheel. Build for switching cost and habit, and add a
share/export surface if you want any organic growth at all.

---

## 6. Onboarding and activation

This is the weakest part of the product and the highest-ROI fix.

### The problem

- **No first-run experience.** `main.js` has no onboarding, welcome, or
  permission-priming flow (grep finds only a buffering-spinner comment). New
  users land on an empty History screen with a dismissable banner.
- **Huge model download before any value.** The dictation package is grammar
  (51MB) plus VAD (0.6MB) plus the ASR weights, which dominate: Parakeet TDT
  0.6B is roughly 600MB at INT8 and multiple GB unquantized on desktop. Meeting
  adds speaker (26.5MB) plus segmentation (6MB) plus a summarizer LLM (0.4GB to
  ~2GB). Voices are separate downloads on top. The user waits before the app does
  anything.
- **No mic permission priming.** No in-app education before the OS prompt fires.
- **Time-to-value path is brutal:** install, find Settings, download half a gig,
  wait, grant mic, learn the hotkey, then finally dictate. Compare Wispr: install,
  sign in, talk.

### Fixes, in order

1. **Ship an instant-first-use tiny model.** Bundle or fast-download a small ASR
   model so the user dictates within seconds while the good model downloads in the
   background. The registry already supports a preference order and fallbacks
   (`first_downloaded_model()`).
2. **Add a real onboarding wizard.** Prime the mic permission, kick off the
   download with a visible total size and Wi-Fi-only option, explain the hotkey,
   and land the user on a working dictation in under a minute.
3. **Show download size and progress up front,** not buried in Settings > Updates.
4. **Progressive disclosure of modes.** Do not present three modes and a dozen nav
   items on day one. Start with dictation, reveal Listen and Meeting as the user
   arrives.

---

## 7. Platform and distribution gaps

- **No iOS.** `tauri.conf.json` targets Android (`minSdkVersion 26`) and desktop.
  `isDesktop` in `main.js` is literally `!userAgent.includes('Android')`, so iOS
  is an afterthought. iOS is the most lucrative consumer market for dictation and
  reading (Speechify and Wispr are iOS-strong). Meeting mode cannot come to iOS
  easily (background mic and system-audio capture are restricted, no accessibility
  IME equivalent), but **Listen plus in-app dictation would work and would open a
  large market.** The absence is a strategic hole, not just a checkbox.
- **Android IME and Play Store risk.** The dictation overlay leans on
  `VerbaAccessibilityService` (`android_ime.rs`, `VerbaAccessibilityService.kt`).
  Google Play scrutinizes AccessibilityService use hard and has removed apps that
  use it for non-accessibility purposes. This is a real distribution risk that
  needs a policy answer before store submission, not after.
- **No app stores today.** Desktop ships as a signed APK / local build (`just
  apk`). No Mac App Store, no Microsoft Store, no Homebrew cask, no Play Store
  listing. Discoverability is currently zero.
- **Cross-device sync is absent** and hard for local-first. E2E-encrypted sync is
  the right answer (keeps privacy, justifies subscription). See section 4.
- **No marketing surface.** `README.md` is still the default Tauri template
  ("This template should help get you started"). There is no landing page, no
  positioning, no demo. For a product about to charge money, this is a launch
  blocker.
- **Runtime CDN dependencies contradict the pitch.** `index.html` loads Tailwind
  from `cdn.tailwindcss.com` and fonts from `fonts.googleapis.com` at runtime. A
  privacy-first, offline-first app that phones Google Fonts and Cloudflare on
  every launch is a credibility gap (and Tailwind's CDN is explicitly not for
  production). Vendor these locally. Low effort, real trust payoff.

---

## 8. Risks and moats

### What is defensible

- **Execution and polish.** The UI is genuinely well built (Material-style theming,
  full-screen player, pull-to-refresh, word highlighting). Hard to match quickly.
- **The privacy architecture as a promise.** No-audio-stored meetings, on-device
  everything. Copyable in principle, but it is a positioning and trust asset that
  cloud incumbents will not adopt because it breaks their model.
- **Accumulated personal data.** Voiceprint gallery, vocab, snippets, history.
  Switching cost, not network effect, but real.
- **The integration IP.** The espeak-free phonemization (bundled CMUdict/gb_dict),
  the fine-tuned grammar router and corrector, the diarization tuning
  (`diarize.rs`, the merge/consolidate pass). This is the closest thing to
  proprietary, though it is derived from open data and reproducible with effort.

### What is copyable

- **The models.** All open weights (Parakeet CC BY 4.0, Whisper MIT, Piper MIT,
  Qwen/Llama/Gemma). Verba's differentiation is emphatically not the models.
  Anyone can build a competing local app on the same weights. The moat is UX,
  distribution, and switching cost, so invest there, not in guarding models.

### Licensing risks

- **gb_dict is CC BY-SA 3.0** (`LICENCES.md`), a share-alike data license compiled
  into the binary. Attribution is mandatory and the share-alike clause deserves a
  lawyer's read before commercial shipping, to confirm the bundled dictionary
  counts as an aggregate and not a derivative that propagates SA.
- **Supertonic is OpenRAIL-M** (`TTS_ENGINES.md`), behavioural use restrictions
  incompatible with a clean commercial license. If adopted, review carefully.
- **CC BY attributions** (Parakeet, Whisper voices, alba) must be surfaced in-app.
  `LICENCES.md` exists in the repo but there is no in-app licences/about screen.
  Add one. Cheap compliance and trust.

### Model-hosting cost structure

R2 has no egress fees, so hosting even multi-hundred-MB models is storage-cost
only, cheap. The gate's weakness is not cost, it is that the same models are free
upstream (see section 4). Price the paid tiers on delivered value (meetings,
sync, premium voices), not on the illusion that model access is scarce.

### The self-inflicted risk

The subscription lockout plan is the biggest risk in this document. It is
optional, it is unbuilt, and it can be fixed by decision before any code ships.
Do not brick a local app.

---

## 9. Prioritized roadmap

Ranked by impact against effort. Effort is rough engineering size, not calendar.

| # | Opportunity | Impact | Effort | Why it matters |
|---|---|---|---|---|
| 1 | First-run onboarding + instant-first-use tiny model | High | Med | Nothing converts if TTV is a 0.5GB+ wait with no guidance |
| 2 | Global search across history/library/meetings + cross-capability wiring | High | Low-Med | Turns three tools into one sticky workspace, raises switching cost |
| 3 | Monetize Meeting mode, never brick on lapse | High | Med-High | Clearest willingness to pay, without torching local-first trust |
| 4 | Multilingual dictation | High | Med | Biggest TAM unlock and sharpest parity gap vs Wispr/Superwhisper |
| 5 | E2E-encrypted cross-device sync | High | High | The one honest recurring-revenue justification, top retention driver |
| 6 | Remove runtime CDN deps (Tailwind, Google Fonts) | Med | Low | Privacy/offline integrity, removes a credibility gap |
| 7 | Dictation stats (words/time saved) | Med | Low | Proven, fully-local habit hook |
| 8 | In-app licences/about screen | Low-Med | Low | CC BY compliance and trust |
| 9 | AI edit/command mode for dictation | High | Med | Wispr/Superwhisper parity; summarizer loop already exists |
| 10 | Ship the local OpenAI-compatible API | Med | Med | Developer wedge, design already done in `API.md` |
| 11 | Android TextToSpeechService | Med | Med | Every Android TTS caller can use Verba voices |
| 12 | Meeting calendar integration + clean export | Med | Med | Granola parity for the meeting buyer |
| 13 | Real marketing site + README/positioning | Med | Low-Med | Discoverability, launch blocker today |
| 14 | Better Listen voices (Supertonic/Kokoro) | Med-High | Med | Unlocks premium-voice monetization; blocked on model/license |
| 15 | iOS app (Listen + in-app dictation) | High | High | Largest untapped consumer market |
| 16 | Android IME polish + Play Store policy answer | Med-High | Med | Distribution risk on AccessibilityService must be resolved |

### Top 5 bets

1. **Fix activation (onboarding + instant-first-use).** The best product loses if
   the first minute is a silent half-gig download. This gates every other metric.
2. **Monetize meetings, never brick the app.** Meetings are the clearest thing a
   privacy buyer pays for. Gating meetings and sync while leaving installed
   dictation and reading working keeps the trust that is the entire brand.
3. **Global search plus cross-capability wiring.** The cheapest way to make three
   separate tools feel like one indispensable workspace, and to raise switching
   cost.
4. **E2E-encrypted sync.** The single honest reason a local app can charge
   recurring, and the top driver of multi-device retention, without breaking the
   privacy promise.
5. **Multilingual dictation.** The biggest market unlock and the most glaring
   competitor-parity gap. English-only caps Verba's ceiling no matter how good the
   rest gets.
