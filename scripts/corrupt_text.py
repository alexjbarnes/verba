#!/usr/bin/env python3
"""Inject audible spoken-register grammar errors into clean text.

Produces the source side of grammar-corrector training/eval pairs for the
round-trip harness: the corrupted text is synthesized and transcribed, and
the clean original is the correction target. Only corruptions that survive
the audio channel are useful (agreement, dropped articles, tense, plurals,
duplicated words); inaudible ones (their/there) are pointless here.

Usage:
    corrupt_text.py CLEAN.txt CORRUPTED.txt CORRUPTIONS.json [--seed 7] [--rate 0.5]
"""

import argparse
import json
import random
import re

SWAPS = {
    "agreement": [
        (r"\bis\b", "are"), (r"\bare\b", "is"), (r"\bwas\b", "were"),
        (r"\bwere\b", "was"), (r"\bhas\b", "have"), (r"\bdoes\b", "do"),
    ],
    "article_drop": [
        (r"\bthe ", ""), (r"\ban ", ""), (r"\ba ", ""),
    ],
    "tense": [
        (r"\bwent\b", "go"), (r"\bsaid\b", "say"), (r"\bgot\b", "get"),
        (r"\btook\b", "take"), (r"\bmade\b", "make"), (r"\bbought\b", "buy"),
        (r"\bcame\b", "come"), (r"\bfound\b", "find"), (r"\bknew\b", "know"),
    ],
    "plural_drop": [
        (r"\b(two|three|four|five|six|seven|eight|nine|ten|\d+) ([a-z]{3,}?)s\b",
         r"\1 \2"),
    ],
    "pronoun": [
        (r"\bI\b", "me"),
    ],
}


def corrupt_sentence(sentence, rng, order):
    for kind in order:
        swaps = list(SWAPS[kind])
        rng.shuffle(swaps)
        for pat, repl in swaps:
            m = re.search(pat, sentence)
            if not m:
                continue
            corrupted = sentence[:m.start()] + re.sub(pat, repl, m.group(0)) \
                + sentence[m.end():]
            if corrupted != sentence:
                return corrupted, kind, m.group(0)
    return sentence, None, None


def dup_word(sentence, rng):
    words = sentence.split(" ")
    cands = [i for i, w in enumerate(words[1:-1], 1) if len(w) >= 4 and w.isalpha()]
    if not cands:
        return sentence, None
    i = rng.choice(cands)
    words.insert(i, words[i])
    return " ".join(words), words[i]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("clean")
    ap.add_argument("corrupted")
    ap.add_argument("log")
    ap.add_argument("--seed", type=int, default=7)
    ap.add_argument("--rate", type=float, default=0.5)
    args = ap.parse_args()

    rng = random.Random(args.seed)
    kinds = list(SWAPS) + ["dup_word"]
    text = open(args.clean).read()
    out_paras = []
    records = []
    for pi, para in enumerate(p for p in text.split("\n\n") if p.strip()):
        sentences = re.split(r"(?<=[.!?]) ", para.strip())
        out = []
        for si, sent in enumerate(sentences):
            if rng.random() >= args.rate:
                out.append(sent)
                continue
            order = kinds[:]
            rng.shuffle(order)
            kind, detail, corrupted = None, None, sent
            for k in order:
                if k == "dup_word":
                    corrupted, detail = dup_word(sent, rng)
                    kind = k if detail else None
                else:
                    corrupted, kind, detail = corrupt_sentence(sent, rng, [k])
                if kind:
                    break
            out.append(corrupted)
            if kind:
                records.append({"para": pi, "sent": si, "type": kind,
                                "detail": detail, "before": sent,
                                "after": corrupted})
        out_paras.append(" ".join(out))

    open(args.corrupted, "w").write("\n\n".join(out_paras) + "\n")
    json.dump(records, open(args.log, "w"), indent=1)
    by_kind = {}
    for r in records:
        by_kind[r["type"]] = by_kind.get(r["type"], 0) + 1
    print(f"{len(records)} corruptions: {by_kind}")


if __name__ == "__main__":
    main()
