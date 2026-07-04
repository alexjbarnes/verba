#!/usr/bin/env python3
"""Sentence-level grammar benchmark: run pair files through the real pipeline.

Feeds every src sentence from a make_grammar_pairs JSONL through the
grammar_probe bin and scores the grammar stage in isolation (its input is
the post-Vocab snapshot, so filler/ITN fixes are credited upstream, not to
the models):

  corrupted rows   fixed (output == tgt), missed (unchanged), mangled
                   (changed but still wrong), upstream (already clean
                   before the grammar stage ran)
  acceptable rows  harmed (grammar stage changed text that needed nothing)

plus a router threshold sweep (recall on corrupted vs collateral on clean).

Note: when the pairs come from the same generator as the training data this
is an in-distribution benchmark — use the round-trip article probe for
out-of-distribution numbers.

Usage:
    grammar_bench.py PAIRS.val.jsonl --probe-bin src-tauri/target/debug/grammar_probe
        --work-dir DIR [--limit N]
"""

import argparse
import json
import os

from stt_grammar_probe import run_probe, words_of


def stage_words(result, prefix):
    for st in result["stages"]:
        if st["name"].startswith(prefix):
            return words_of(st["text"]), st
    raise KeyError(prefix)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("pairs")
    ap.add_argument("--probe-bin", required=True)
    ap.add_argument("--work-dir", required=True)
    ap.add_argument("--limit", type=int, default=0)
    args = ap.parse_args()

    rows = [json.loads(l) for l in open(args.pairs)]
    if args.limit:
        rows = rows[:args.limit]
    os.makedirs(args.work_dir, exist_ok=True)
    results = run_probe(args.probe_bin, [r["src"] for r in rows],
                        os.path.join(args.work_dir, "pipeline_bench.json"))

    counts = {"fixed": 0, "missed": 0, "mangled": 0, "upstream": 0,
              "kept": 0, "harmed": 0}
    scores = []  # (score, label)
    harmed_samples, mangled_samples = [], []
    for row, res in zip(rows, results):
        pre, g = stage_words(res, "Vocab")
        post, gstage = stage_words(res, "Grammar")
        want = words_of(row["tgt"])
        for s in gstage.get("grammar_sentences") or []:
            if s.get("score") is not None:
                scores.append((s["score"], row["label"]))
        if row["label"] == 1:
            if post == pre:
                counts["kept"] += 1
            else:
                counts["harmed"] += 1
                if len(harmed_samples) < 8:
                    harmed_samples.append((row["src"], " ".join(post)))
        else:
            if pre == want:
                counts["upstream"] += 1
            elif post == want:
                counts["fixed"] += 1
            elif post == pre:
                counts["missed"] += 1
            else:
                counts["mangled"] += 1
                if len(mangled_samples) < 8:
                    mangled_samples.append((row["src"], " ".join(post), row["tgt"]))

    n_bad = counts["fixed"] + counts["missed"] + counts["mangled"]
    n_ok = counts["kept"] + counts["harmed"]
    print(f"corrupted rows (grammar-stage scope, n={n_bad}; "
          f"{counts['upstream']} fixed upstream):")
    for k in ("fixed", "missed", "mangled"):
        print(f"  {k:8}: {counts[k]:4}  ({counts[k] / max(1, n_bad):.0%})")
    print(f"acceptable rows (n={n_ok}):")
    print(f"  harmed  : {counts['harmed']:4}  ({counts['harmed'] / max(1, n_ok):.1%})")

    bad = [s for s, l in scores if l == 0]
    ok = [s for s, l in scores if l == 1]
    print(f"\nrouter sweep ({len(bad)} corrupted / {len(ok)} clean sentences):")
    for t in (0.3, 0.5, 0.7, 0.9):
        rb = sum(1 for s in bad if s < t) / max(1, len(bad))
        ro = sum(1 for s in ok if s < t) / max(1, len(ok))
        print(f"  threshold {t}: recall {rb:.0%}, clean routed {ro:.1%}")

    if harmed_samples:
        print("\nsample harmed:")
        for src, post in harmed_samples:
            print(f"  {src!r} -> {post!r}")
    if mangled_samples:
        print("\nsample mangled:")
        for src, post, tgt in mangled_samples:
            print(f"  {src!r} -> {post!r} (want {tgt!r})")


if __name__ == "__main__":
    main()
