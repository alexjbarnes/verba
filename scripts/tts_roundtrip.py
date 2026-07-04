#!/usr/bin/env python3
"""Pronunciation round-trip harness, ASR half.

Takes the WAV + expected-spoken JSON produced by the tts_roundtrip Rust bin
(which runs the REAL app pipeline: normalization, dictionaries, overrides,
OOV handling, RP transform, the actual voice model), transcribes it with
sherpa-onnx (the same ASR engine the app ships) and aligns the transcript
against the expected text. Words that come back wrong are pronunciation
suspects — the way a broken "side" would transcribe as "seeday".

Usage:
    tts_roundtrip.py OUT.wav OUT.json --asr-dir DIR_WITH_WHISPER_MODEL

ASR is imperfect: treat the output as a shortlist to scan, not a verdict.
Homophones and short function words produce noise; repeated offenders and
garbled multi-word runs are the real signals.
"""

import argparse
import difflib
import json
import re
import wave

import numpy as np
import sherpa_onnx


def read_wav(path):
    with wave.open(path, "rb") as w:
        sr = w.getframerate()
        pcm = np.frombuffer(w.readframes(w.getnframes()), dtype=np.int16)
    samples = pcm.astype(np.float32) / 32768.0
    if sr != 16000:
        n = int(len(samples) * 16000 / sr)
        samples = np.interp(
            np.linspace(0, len(samples) - 1, n), np.arange(len(samples)), samples
        ).astype(np.float32)
        sr = 16000
    return sr, samples


ONES = ["zero", "one", "two", "three", "four", "five", "six", "seven", "eight",
        "nine", "ten", "eleven", "twelve", "thirteen", "fourteen", "fifteen",
        "sixteen", "seventeen", "eighteen", "nineteen"]
TENS = ["", "", "twenty", "thirty", "forty", "fifty", "sixty", "seventy",
        "eighty", "ninety"]


def say_cardinal(n):
    if n < 20:
        return ONES[n]
    if n < 100:
        t, o = TENS[n // 10], n % 10
        return t if o == 0 else f"{t} {ONES[o]}"
    if n < 1000:
        h, r = f"{ONES[n // 100]} hundred", n % 100
        return h if r == 0 else f"{h} {say_cardinal(r)}"
    for div, name in [(10**12, "trillion"), (10**9, "billion"), (10**6, "million"), (1000, "thousand")]:
        if n >= div:
            head = f"{say_cardinal(n // div)} {name}"
            r = n % div
            return head if r == 0 else f"{head} {say_cardinal(r)}"
    return str(n)


def say_year(y):
    hi, lo = y // 100, y % 100
    if 2000 <= y <= 2009:
        return "two thousand" if lo == 0 else f"two thousand {ONES[lo]}"
    if lo == 0:
        return f"{say_cardinal(hi)} hundred"
    if lo < 10:
        return f"{say_cardinal(hi)} oh {ONES[lo]}"
    return f"{say_cardinal(hi)} {say_cardinal(lo)}"


def spoken_numbers(text):
    """Mirror the app's number reading on the ASR side, so whisper writing
    '1980' or '$10 billion' aligns against 'nineteen eighty' / 'ten billion
    dollars' instead of flagging as a mismatch."""
    def num(m):
        cur = m.group(1) == "$"
        val = int(m.group(2).replace(",", ""))
        mag = (m.group(3) or "").lower()
        is_year = not cur and not mag and len(m.group(2)) == 4 and 1100 <= val <= 2099
        if is_year:
            return say_year(val)
        words = say_cardinal(val)
        if mag:
            words += {"k": " thousand", "m": " million", "b": " billion",
                      "bn": " billion", "t": " trillion",
                      "thousand": " thousand", "million": " million",
                      "billion": " billion", "trillion": " trillion"}[mag]
        if cur:
            words += " dollars"
        return words

    text = re.sub(r"(\$?)(\d[\d,]*)\s*(thousand|million|billion|trillion|bn|[kKmMbBtT]\b)?",
                  num, text)
    return text.replace("%", " percent")


def words_of(text):
    return re.findall(r"[a-z']+", spoken_numbers(text).lower())


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("wav")
    ap.add_argument("meta")
    ap.add_argument("--asr-dir", required=True)
    ap.add_argument("--context", type=int, default=3)
    args = ap.parse_args()

    meta = json.load(open(args.meta))
    expected = words_of(meta["spoken"])

    # Pick the best whisper variant present in the model dir.
    import glob
    import os
    prefix = None
    for cand in ("base.en", "small.en", "tiny.en"):
        if glob.glob(os.path.join(args.asr_dir, f"{cand}-encoder*.onnx")):
            prefix = cand
            break
    if prefix is None:
        raise SystemExit(f"no whisper model found in {args.asr_dir}")
    rec = sherpa_onnx.OfflineRecognizer.from_whisper(
        encoder=f"{args.asr_dir}/{prefix}-encoder.int8.onnx",
        decoder=f"{args.asr_dir}/{prefix}-decoder.int8.onnx",
        tokens=f"{args.asr_dir}/{prefix}-tokens.txt",
        num_threads=4,
    )
    print(f"ASR: whisper {prefix}")
    sr, samples = read_wav(args.wav)
    # Whisper decodes up to 30s windows; feed in slices with a little overlap
    # is unnecessary — sherpa's offline whisper handles the full stream in
    # 30s chunks internally when fed as one stream per chunk. Simpler: chunk.
    heard = []
    chunk = sr * 28
    for start in range(0, len(samples), chunk):
        s = rec.create_stream()
        s.accept_waveform(sr, samples[start:start + chunk])
        rec.decode_stream(s)
        heard.extend(words_of(s.result.text))

    def lev(a, b):
        if abs(len(a) - len(b)) > 2:
            return 3
        row = list(range(len(b) + 1))
        for i, ca in enumerate(a, 1):
            prev, row[0] = row[0], i
            for j, cb in enumerate(b, 1):
                prev, row[j] = row[j], min(row[j] + 1, row[j - 1] + 1, prev + (ca != cb))
        return row[-1]

    CONTRACTIONS = {
        "they're": "they are", "we're": "we are", "you're": "you are",
        "i've": "i have", "we've": "we have", "you've": "you have",
        "they've": "they have", "i'll": "i will", "we'll": "we will",
        "it'll": "it will", "i'm": "i am", "who's": "who is",
    }

    def is_noise(exp, got):
        """ASR noise, not pronunciation: same letters with different spacing
        ('thirty minutes'/'thirtyminutes'), contraction expansion
        ("they're"/'they are'), near-homophones and spelling variants
        ('then'/'than', 'centre'/'center')."""
        es, gs = " ".join(exp), " ".join(got)
        if CONTRACTIONS.get(es) == gs or CONTRACTIONS.get(gs) == es:
            return True
        ej = es.replace(" ", "").replace("'", "")
        gj = gs.replace(" ", "").replace("'", "")
        if ej == gj:
            return True
        if len(exp) == 1 and len(got) == 1 and len(ej) >= 3:
            allowed = 2 if len(ej) >= 6 else 1
            if lev(ej, gj) <= allowed:
                return True
        return False

    sm = difflib.SequenceMatcher(a=expected, b=heard, autojunk=False)
    suspects = []
    for op, a0, a1, b0, b1 in sm.get_opcodes():
        if op == "equal":
            continue
        exp = expected[a0:a1]
        got = heard[b0:b1]
        # Ignore pure ASR insertions and mechanical noise classes.
        if op == "insert" and a1 - a0 == 0:
            continue
        if op == "replace" and is_noise(exp, got):
            continue
        ctx_pre = " ".join(expected[max(0, a0 - args.context):a0])
        ctx_post = " ".join(expected[a1:a1 + args.context])
        suspects.append((op, " ".join(exp), " ".join(got), ctx_pre, ctx_post))

    print(f"expected {len(expected)} words, heard {len(heard)}")
    print(f"aligned similarity: {sm.ratio():.3f}")
    print(f"{len(suspects)} suspect spans:\n")
    for op, exp, got, pre, post in suspects:
        print(f"  [{op}] …{pre} [{exp!r} -> heard {got!r}] {post}…")


if __name__ == "__main__":
    main()
