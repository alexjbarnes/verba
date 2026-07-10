# Verba: Go-to-Market and Monetization Plan

Forward-looking plan for taking Verba to a paid product as a solo developer.
This is the "what we are going to do". The reasoning behind each call lives in
`PRODUCT_REVIEW.md` (the analysis). Where this plan and the older
`project_auth_subscription` notes disagree, this plan wins (notably: never brick
on lapse).

Status: draft plan, nothing here is built yet. Revisit as the beta teaches us
what is actually true.

---

## Guiding principles

- **Solo developer.** Own time is the scarce resource, not server cost. Keep the
  SKU count tiny and operations near zero. The goal is to capture value from a
  strong side project, not to build a venture-scale business.
- **Distribution and trust are the bottleneck, not price.** Effort goes to the top
  of the funnel first.
- **Never brick installed functionality.** Local-first trust is the entire brand.
  A lapse gates new and premium features only.
- **Sell the unique axis, not a discount clone.** "The only one that never sends
  your audio anywhere", not "a cheaper Granola".
- **Price on value, not effort, and note that dictation and reading are NOT
  low-value.** Willingness to pay sets the price. The market proves the point:
  Wispr Flow (dictation) and ElevenReader (TTS) command Granola-level prices,
  roughly £10-18/mo, and Wispr can top Granola. Only BASE dictation is
  commoditised (free OS dictation exists); best-in-class premium dictation and
  reading are each real paid products. So the free/paid line is base-versus-premium
  within every mode, and the premium tier of each is genuinely monetisable, not
  funnel garnish. The pipeline work does double duty: it makes the free tier
  best-in-class (the acquisition engine) and it underpins premium features people
  pay for.

---

## Positioning

- **Wedge: confidential local meetings.** A promise Granola, Otter, and Fireflies
  cannot make without abandoning their architecture. Highest willingness to pay.
- **Dictation is the daily-habit / DAU driver.** It creates the retention; meetings
  are what a privacy buyer opens their wallet for.
- **One line:** your voice, on your device, forever. No cloud, no account, no audio
  leaves the machine.
- **Do not sell it as "pay for sync".** Obsidian Sync draws constant "overpriced
  for just sync" complaints, and free alternatives (iCloud, Git) invite the
  comparison. Verba Pro is more than sync: a private meeting assistant plus a
  dictation and reading pipeline an ordinary person cannot reproduce from the raw
  open models. Sync is the honest reason the price recurs, not the headline. Lead
  with the meeting assistant and the output quality, and bundle sync as a perk.
- **That value argues for pricing at or above Obsidian's sync, not below.** GBP 5
  is a deliberate launch undercut for an unknown solo app, which is defensible,
  but the fine-tuned grammar model and the tuned pipelines are exactly what would
  justify GBP 6-8. There is room. Do not underprice the part that is genuinely
  differentiated.

---

## Pricing (decisions)

- **Free forever: the base of every mode.** Base English dictation with the
  standard model and the full clean-up pipeline, and reading with the standard
  voices. This is the funnel and the daily habit, and the pipeline quality is what
  makes it best-in-class free.
- **Pro (the paid tier): the premium of every mode, plus sync, one subscription.**
  GBP 5/mo or GBP 50/yr (the annual is two months free). Today that is meetings +
  sync, because they are the only premium pieces built. As they ship, multilingual
  dictation, the AI edit/command mode, and premium voices join Pro too. So the
  paywall is not "meetings", it is "everything premium across the app, plus sync",
  and meetings is simply what is ready first. This is the Obsidian shape: sync is a
  genuine recurring server cost, so a recurring price is honest, and it lifts the
  premium local features along with it. Lead the pitch with the meeting assistant
  and the output quality, not with sync.
- **Subscription only, no lifetime for this tier.** You cannot sell a lifetime on
  an ongoing-cost service like sync. This supersedes the earlier annual-plus-
  lifetime idea; bundling sync changes the logic. If the subscription-averse buyer
  turns out to matter in beta, revisit a meetings-only lifetime (no sync) as a
  separate, later SKU.
- **GBP 5 is a launch undercut, not the ceiling, and probably not the steady
  state.** Pro bundles what three separate products each charge for: dictation
  (Wispr Flow ~GBP 12-15), reading (ElevenReader ~GBP 10), and meetings (Granola
  ~GBP 14-18), plus sync. Pricing that bundle at GBP 5 is not a clever undercut,
  it is underselling by a wide margin. Even GBP 10 stays radically cheaper than any
  ONE of them, so the "great value" story survives at double the revenue. The
  brake: Verba is behind each specialist today (English-only, voice-quality
  ceiling, small local summariser), so do not price at the sum. Launch low if you
  want the adoption undercut, grandfather early buyers, but treat GBP 8-12 as the
  target once quality and trust catch up, not GBP 5. The real flex is not "cheaper
  than one of them", it is "all three, privately, for less than any single one",
  which is true at GBP 10, not only at GBP 5.
- **Confidential / Pro tier at GBP 12-15/mo: later,** for the compliance buyer,
  once testimonials exist. One SKU on day one, not two.
- **Dropped: per-module GBP 2.** Fragments the one-product story and has poor unit
  economics (Stripe's fixed fee eats ~13% of GBP 2 vs ~3% of a GBP 50 annual).
- **On lapse: never brick.** Stop sync and new downloads, keep everything already
  installed working.

### Sync design and cost control

Sync is the only feature with real server cost and real abuse potential, so
design it defensively from the start:

- **Sync text, settings, progress, and voiceprints, not the heavy blobs.**
  Everything in Verba except the reading library is tiny: dictation history,
  vocab, snippets, config, the speaker gallery (embeddings), meeting transcripts
  and summaries are all text or KB-scale. The only heavy data is imported book and
  PDF bodies. Do not sync those binaries by default. Sync the library index and
  reading position and re-import the source locally, or make book-body sync an
  explicit, capped opt-in.
- **Cap account storage.** A free or beta account gets a tight cap, the paid tier a
  larger one (the Obsidian Sync vault-size model). Caps stop one whale from running
  up the bill.
- **Do not give sync away free and unlimited in beta.** It is the one
  genuine-cost, will-be-paid, abuse-prone feature. Free-syncing for months invites
  whales and anchors users to free sync, so they churn at the paywall. Keep
  meetings free in beta (local, no cost, safe to test), but hold sync out of the
  free beta: defer it to launch, or introduce it late as a tightly capped "sync
  beta".

---

## Beta and launch

- **Beta is generous free access, framed as a beta perk against a known launch
  price.** State it plainly in-app and on the site: "Free during beta. At launch,
  GBP 50/yr. Beta users get [reward]." Silence about future pricing makes the
  cohort anchor to free and churn at the paywall.
- **Length is product-readiness-driven, not a fixed calendar.** Beta ends when the
  app is good enough to charge (activation fixed, worst gaps closed). Roughly 6
  months to start, tapering toward 3 for later cohorts, which rewards the
  earliest and riskiest adopters.
- **No card during beta.** Low friction; the goal is testers and feedback, not
  pre-qualified buyers.
- **Meetings free in beta, sync not.** Meetings are local and safe to give away for
  testing. Sync is the costly, will-be-paid, abuse-prone one (see Sync design and
  cost control), so keep it out of the free beta.
- **Instrument it or it is wasted.** Watch where new users drop off (the activation
  blocker), talk to them, collect testimonials. The feedback is worth more than
  the headcount.
- **Convert carefully.** Beta-to-paid is where the cohort is kept or lost. Reward
  beta users (grandfather them or a beta-only lifetime), lead with that reward,
  and do not brick.

---

## The real work: distribution and trust (do first)

Price and trial mechanics do not matter until people find and trust the app.

- **Activation (top blocker):** onboarding wizard, instant-first-use tiny model so
  the user dictates within seconds, mic-permission priming, download size shown up
  front with a Wi-Fi-only option.
- **Trust:** a real README and landing page (the README is still the Tauri
  template), an in-app licences/about screen (CC BY attributions are mandatory),
  and vendoring the runtime CDN dependencies (Tailwind, Google Fonts) locally so a
  privacy app stops phoning Google on launch.
- **Multilingual dictation:** the biggest market unlock and sharpest parity gap.
- **Global search + cross-capability wiring:** the cheapest retention lever, turns
  three tools into one sticky workspace.

---

## Build prerequisites before charging

- **Auth + entitlement** (Supabase + Stripe) and device management. Design the gate
  so a lapse blocks new downloads, sync, and premium only, and never bricks
  installed features. This reverses the old `project_auth_subscription` "full app
  lockout on lapse" decision.
- **Free-vs-paid gating** in the app: base dictation and reading free forever, the
  rest behind the paid unlock.
- **Licence compliance:** in-app licences screen, surfaced CC BY attributions.

---

## Sequence

1. **Pre-beta:** fix activation, remove CDN deps, ship a real README and landing
   page and an in-app licences screen. Build the auth/entitlement scaffolding
   (gate premium, never brick).
2. **Beta:** free, instrumented, framed with the launch price. Gather feedback, fix
   the drop-offs, collect testimonials.
3. **Convert:** turn on launch pricing (annual GBP 50 + lifetime GBP 99, monthly
   GBP 6 on-ramp), lead beta users with their reward.
4. **Post-launch steady state:** permanent free tier + a short 14-to-30-day trial
   of the paid features. Add the Pro/Confidential tier once testimonials support
   it. Build sync only when its recurring value and ops burden are justified.

---

## Open questions (decide as the beta teaches us)

- **The exact free/paid line.** Meetings are clearly paid. Is Listen free, or a
  paid premium-voices upsell?
- **Lifetime price point.** GBP 99 steady, with a founder's GBP 49 for the first N
  buyers to create urgency?
- **Sync: build or defer?** It is the one honest recurring-revenue justification and
  a top retention driver, but also real ops burden for a solo dev. Likely defer
  past launch.
- **Distribution channels.** The genuine unknown: where do the first 5,000 to
  20,000 users actually come from? Nothing else in this plan matters until this
  has an answer.
- **Payment/auth stack build effort and timing.** Sized against everything else on
  the pre-beta list.
