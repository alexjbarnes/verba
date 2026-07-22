# scripts/data/

## corrector_test_cases.json

Real transcription samples collected from production use. Each entry captures the full post-processing pipeline: raw ASR output through every stage (filler removal, ITN, vocab, neural grammar, cleanup) with timing and scoring metadata.

We collect these to build a regression suite for the grammar corrector. Cases demonstrate failure modes like the neural corrector inverting negations ("isn't working" to "is working") when the CoLA router scores compound nouns like "create snippet button" as low-acceptability.

When adding new cases, include the complete pipeline_stages array and chunk_timings so we can replay the full transformation chain.

## router_test_cases.json

Labeled examples for fine-tuning and evaluating the CoLA router (grammar acceptability classifier). Each entry is a sentence with a label: 1 = acceptable (should pass through), 0 = needs correction (should route to corrector).

When adding cases, focus on sentences the router currently gets wrong: clean text it flags as unacceptable (false positives) and broken text it lets through (false negatives).

## Known corrector failure modes (2026-07-22 history review)

From a review of Apr-Jul production history (362 entries added to
corrector_test_cases.json, 59 with grammar-stage changes). Both models were
fine-tuned previously on synthetic spoken-register corruption pairs (router:
ELECTRA-small, corrector: T5-efficient-tiny — see the repo CLAUDE.md pipeline);
these cases exist to drive the next round. No pipeline changes made for these
yet — catalogue only.

1. Article insertion before proper/domain nouns. "pull main" -> "pull the
   main" (6+ occurrences), "leave region" -> "leave the region", "tagging
   against main" -> "the mains". The corrector treats bare domain nouns as
   missing-article errors. Note the same edit is CORRECT before common nouns
   ("not even client" -> "not even a client"), so this needs context, not a
   blanket rule.
2. Meaning rewrites. "do me a new list" -> "do I make a new list" (request
   became a question), "I have run it" -> "I have to run it", "we could have
   say" -> "we could have to say".
3. Agreement corruption. "are we sure" -> "is we sure", "only has seven
   speakers" -> "only have", "two that jump to mind are" -> "is". The
   corrector sometimes makes agreement WORSE on garbled input.
4. Confabulated word swaps on ASR garble. "a new tasition at 6" -> "a new
   tradition at 6" - a confident wrong word where leaving the garble (or no
   edit) would be safer.
5. Contraction expansion. "You're" -> "You are", "it's" -> "it is" - loses
   the spoken register the pipeline otherwise preserves.
6. Preposition swaps (mild). in <-> on ("in the transcript" -> "on the
   transcript").

Candidate mitigations when we act on these: protect user-vocab terms from
grammar edits, forbid contraction expansion, raise the corrector acceptance
bar on low-router-score garble instead of editing it.
