#!/usr/bin/env python3
"""Generate grammar-corrector training pairs from a clean text corpus.

Sentence-level pairs for BOTH grammar models:
  - T5 corrector: src (corrupted or clean) -> tgt (clean); identity pairs
    teach it to leave good text alone (its measured failure mode is
    inserting the/of/to on text that needed nothing).
  - Router: label 1 = acceptable as-is, 0 = needs repair. Same JSONL.

Corruptions are audible spoken-register errors only (the audio channel
erases inaudible ones like their/there): agreement, dropped/swapped
articles, tense, plural after numeral, pronoun case, prepositions,
duplicated words.

Optionally mixes in ASR transcripts of CLEAN text (--asr-clean, the raws
of a tts_roundtrip run) as identity/acceptable examples: garbled-but-
grammatical output must NOT be routed or "fixed" — T5 makes garble worse.

Split is by document, not sentence, so near-duplicate phrasing within a
post cannot leak across train/val.

Usage:
    make_grammar_pairs.py CORPUS_DIR OUT_PREFIX [--seed 7] [--identity-rate 0.45]
        [--val-frac 0.1] [--asr-clean raws.json ...]
Writes OUT_PREFIX.train.jsonl and OUT_PREFIX.val.jsonl.
"""

import argparse
import glob
import json
import random
import re

AGREEMENT = [
    (r"\bis\b", "are"), (r"\bare\b", "is"), (r"\bwas\b", "were"),
    (r"\bwere\b", "was"), (r"\bhas\b", "have"), (r"\bhave\b", "has"),
    (r"\bdoes\b", "do"), (r"\bdon't\b", "doesn't"), (r"\bdoesn't\b", "don't"),
]
ARTICLES = [
    # Drops only: a<->the swaps usually produce valid English (semantic
    # shift, not a grammar error) and would poison the labels.
    (r"\bthe ", ""), (r"\ban ", ""), (r"\ba ", ""),
]
TENSE = [
    (r"\bwent\b", "go"), (r"\bsaid\b", "say"), (r"\bgot\b", "get"),
    (r"\btook\b", "take"), (r"\bmade\b", "make"), (r"\bbought\b", "buy"),
    (r"\bcame\b", "come"), (r"\bfound\b", "find"), (r"\bknew\b", "know"),
    (r"\bthought\b", "think"), (r"\bsaw\b", "see"), (r"\bwrote\b", "write"),
    (r"\bgave\b", "give"), (r"\bran\b", "run"), (r"\bbuilt\b", "build"),
]
PLURAL = [
    (r"\b(two|three|four|five|six|seven|eight|nine|ten|\d+) ([a-z]{3,}?)s\b",
     r"\1 \2"),
]
PRONOUN = [
    (r"\bI\b", "me"), (r"\bhe\b", "him"), (r"\bthey\b", "them"),
]
PREPOSITION = [
    # Locative confusions only: to/for/with swaps too often stay valid.
    (r"\bin\b", "on"), (r"\bon\b", "in"), (r"\bat\b", "in"),
]
INFINITIVE = [
    (r"\b(want|need|going|have|used|trying) to\b", r"\1"),
]
COPULA = [
    # "he is going" -> "he going": dropped copula before a gerund.
    (r"\b(he|she|it|they|we|you|I|there) (?:is|are|am|was|were) ([a-z]+ing)\b",
     r"\1 \2"),
]
MODAL = [
    (r"\b(can|could|will|would|should|might|must) be\b", r"\1 is"),
]
A_AN = [
    (r"\ban ([aeiouAEIOU])", r"a \1"),
    (r"\ba ([bcdfghjklmnpqrstvwxz])", r"an \1"),
]
MENU = {
    "agreement": AGREEMENT, "article": ARTICLES, "tense": TENSE,
    "plural": PLURAL, "pronoun": PRONOUN, "preposition": PREPOSITION,
    "infinitive": INFINITIVE, "copula": COPULA, "modal": MODAL, "a_an": A_AN,
}


def apply_swap(sentence, swaps, rng):
    swaps = list(swaps)
    rng.shuffle(swaps)
    for pat, repl in swaps:
        hits = list(re.finditer(pat, sentence))
        if not hits:
            continue
        m = rng.choice(hits)
        replaced = re.sub(pat, repl, m.group(0))
        out = sentence[:m.start()] + replaced + sentence[m.end():]
        if out != sentence:
            return re.sub(r"  +", " ", out)
    return None


def dup_word(sentence, rng):
    words = sentence.split(" ")
    cands = [i for i, w in enumerate(words[1:-1], 1)
             if len(w) >= 4 and w.isalpha()]
    if not cands:
        return None
    i = rng.choice(cands)
    words.insert(i, words[i])
    return " ".join(words)


def corrupt_variants(sentence, rng, n):
    """Up to n distinct corruptions of the sentence, each a different kind.
    dup_word is caught by the filler stage at runtime, so keep it a small
    slice of the corrupt class rather than a dominant pattern."""
    kinds = list(MENU) + (["dup_word"] if rng.random() < 0.15 else [])
    rng.shuffle(kinds)
    out, seen = [], set()
    for kind in kinds:
        if len(out) >= n:
            break
        bad = dup_word(sentence, rng) if kind == "dup_word" \
            else apply_swap(sentence, MENU[kind], rng)
        if bad and bad not in seen:
            out.append((bad, kind))
            seen.add(bad)
    return out


SENT_SPLIT = re.compile(r"(?<=[.!?])\s+(?=[A-Z\"'])")


def sentences_of(path):
    text = open(path).read()
    for para in text.split("\n\n"):
        para = " ".join(para.split())
        for s in SENT_SPLIT.split(para):
            s = s.strip()
            words = s.split()
            if not (5 <= len(words) <= 30):
                continue
            if not s[0].isupper() and not s[0] in "\"'":
                continue
            if "http" in s or "@" in s:
                continue
            letters = sum(c.isalpha() or c.isspace() for c in s)
            if letters / len(s) < 0.8:
                continue
            yield s


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("corpus_dir")
    ap.add_argument("out_prefix")
    ap.add_argument("--seed", type=int, default=7)
    ap.add_argument("--corrupt-rate", type=float, default=0.55,
                    help="fraction of sentences that also get corrupted variants")
    ap.add_argument("--variants", type=int, default=3,
                    help="max corrupted variants per corrupted sentence")
    ap.add_argument("--val-frac", type=float, default=0.1)
    ap.add_argument("--asr-clean", action="append", default=[],
                    help="raws.json of a clean-text tts_roundtrip run; "
                         "sentences become acceptable/identity examples")
    args = ap.parse_args()

    rng = random.Random(args.seed)
    docs = sorted(glob.glob(f"{args.corpus_dir}/*.txt"))
    if not docs:
        raise SystemExit(f"no .txt files in {args.corpus_dir}")
    rng.shuffle(docs)
    n_val = max(1, int(len(docs) * args.val_frac))
    splits = {"val": docs[:n_val], "train": docs[n_val:]}

    stats = {}
    for split, files in splits.items():
        pairs = []
        for path in files:
            for sent in sentences_of(path):
                # Every sentence contributes an identity pair (T5 learns to
                # copy, router learns the acceptable side); corrupted
                # variants of the same sentence give contrastive signal.
                pairs.append({"src": sent, "tgt": sent, "label": 1})
                if rng.random() >= args.corrupt_rate:
                    continue
                for bad, kind in corrupt_variants(sent, rng, args.variants):
                    pairs.append({"src": bad, "tgt": sent, "label": 0})
                    stats[kind] = stats.get(kind, 0) + 1
        if split == "train":
            for raws_path in args.asr_clean:
                for para in json.load(open(raws_path)):
                    for s in SENT_SPLIT.split(para):
                        if 5 <= len(s.split()) <= 30:
                            pairs.append({"src": s.strip(), "tgt": s.strip(),
                                          "label": 1})
        rng.shuffle(pairs)
        out = f"{args.out_prefix}.{split}.jsonl"
        with open(out, "w") as f:
            for p in pairs:
                f.write(json.dumps(p) + "\n")
        n_id = sum(1 for p in pairs if p["label"] == 1)
        print(f"{out}: {len(pairs)} pairs ({n_id} acceptable, {len(pairs) - n_id} corrupted)")
    print(f"corruption mix: {dict(sorted(stats.items()))}")


if __name__ == "__main__":
    main()
