#!/usr/bin/env python3
"""STT-side round-trip probe: evaluate the grammar stage on realistic ASR errors.

Takes the WAV + per-paragraph meta produced by the tts_roundtrip Rust bin,
transcribes each paragraph with the app's dictation model (Parakeet TDT via
sherpa-onnx, model_type nemo_transducer, mirroring transcribe.rs), then runs
the raw transcripts through the real postprocess pipeline via the
grammar_probe bin (CoLA router + T5 corrector).

Word-level classification per paragraph, against the original article text:

  pre_err      errors still present at the corrector's input (post-Vocab)
  fixed        pre_err sites gone after the grammar stage
  missed       pre_err sites still present
  introduced   new error sites created by the grammar stage

Missed sites map back to the per-sentence router records: score >= threshold
means the router never sent the sentence to T5 (router miss), otherwise the
corrector had its chance and failed.

Usage:
    stt_grammar_probe.py OUT.wav OUT.json --asr-dir PARAKEET_DIR \
        --probe-bin src-tauri/target/debug/grammar_probe --work-dir DIR
"""

import argparse
import difflib
import glob
import json
import os
import re
import subprocess
import wave

import numpy as np

CONTRACTIONS = {
    "they're": "they are", "we're": "we are", "you're": "you are",
    "i've": "i have", "we've": "we have", "you've": "you have",
    "they've": "they have", "i'll": "i will", "we'll": "we will",
    "it'll": "it will", "i'm": "i am", "who's": "who is",
    "that's": "that is", "it's": "it is", "there's": "there is",
    "can't": "can not", "cannot": "can not", "won't": "will not",
    "don't": "do not", "doesn't": "does not", "didn't": "did not",
    "isn't": "is not", "aren't": "are not", "wasn't": "was not",
    "weren't": "were not", "couldn't": "could not",
    "wouldn't": "would not", "shouldn't": "should not",
    "haven't": "have not", "hasn't": "has not", "hadn't": "had not",
    "let's": "let us",
}


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
    for div, name in [(10**9, "billion"), (10**6, "million"), (1000, "thousand")]:
        if n >= div:
            head = f"{say_cardinal(n // div)} {name}"
            r = n % div
            return head if r == 0 else f"{head} {say_cardinal(r)}"
    return str(n)


def spell_number(tok):
    """Digits -> words so '1' and 'one' (or '1980' and its year reading)
    compare equal regardless of which side ITN normalized."""
    n = int(tok)
    if len(tok) == 4 and 1100 <= n <= 2099:
        hi, lo = n // 100, n % 100
        if 2000 <= n <= 2009:
            return "two thousand" if lo == 0 else f"two thousand {ONES[lo]}"
        if lo == 0:
            return f"{say_cardinal(hi)} hundred"
        if lo < 10:
            return f"{say_cardinal(hi)} oh {ONES[lo]}"
        return f"{say_cardinal(hi)} {say_cardinal(lo)}"
    return say_cardinal(n)


def words_of(text):
    text = text.replace("’", "'").replace("‘", "'")
    raw = re.findall(r"[a-z0-9']+", text.lower())
    out = []
    for w in raw:
        w = w.strip("'")
        if not w:
            continue
        if w.isdigit() and len(w) <= 9:
            out.extend(spell_number(w).split())
            continue
        out.extend(CONTRACTIONS.get(w, w).split())
    return out


def read_wav_16k(path):
    with wave.open(path, "rb") as w:
        sr = w.getframerate()
        pcm = np.frombuffer(w.readframes(w.getnframes()), dtype=np.int16)
    samples = pcm.astype(np.float32) / 32768.0
    if sr != 16000:
        n = int(len(samples) * 16000 / sr)
        samples = np.interp(
            np.linspace(0, len(samples) - 1, n), np.arange(len(samples)), samples
        ).astype(np.float32)
    return sr, samples


def transcribe_paragraphs(wav_path, meta, asr_dir, cache_path):
    if os.path.exists(cache_path):
        return json.load(open(cache_path))
    import sherpa_onnx
    rec = sherpa_onnx.OfflineRecognizer.from_transducer(
        encoder=glob.glob(os.path.join(asr_dir, "encoder*.onnx"))[0],
        decoder=glob.glob(os.path.join(asr_dir, "decoder*.onnx"))[0],
        joiner=glob.glob(os.path.join(asr_dir, "joiner*.onnx"))[0],
        tokens=os.path.join(asr_dir, "tokens.txt"),
        num_threads=4,
        model_type="nemo_transducer",
    )
    native_sr, samples = read_wav_16k(wav_path)
    scale = 16000 / meta["sample_rate"]
    raws = []
    for i, para in enumerate(meta["paragraphs"]):
        a = int(para["start"] * scale)
        b = int(para["end"] * scale)
        s = rec.create_stream()
        s.accept_waveform(16000, samples[a:b])
        rec.decode_stream(s)
        raws.append(s.result.text)
        print(f"  transcribed {i + 1}/{len(meta['paragraphs'])}")
    json.dump(raws, open(cache_path, "w"), indent=1)
    return raws


def run_probe(probe_bin, raws, out_path):
    if os.path.exists(out_path):
        return json.load(open(out_path))
    env = dict(os.environ)
    if "ORT_DYLIB_PATH" not in env:
        import onnxruntime
        capi = os.path.dirname(onnxruntime.__file__)
        libs = glob.glob(os.path.join(capi, "capi", "libonnxruntime.so*"))
        env["ORT_DYLIB_PATH"] = libs[0]
    p = subprocess.run([probe_bin], input=json.dumps(raws), capture_output=True,
                       text=True, env=env)
    if p.returncode != 0:
        raise SystemExit(f"probe failed: {p.stderr[-2000:]}")
    results = json.loads(p.stdout)
    json.dump(results, open(out_path, "w"), indent=1)
    return results


def stage_text(result, prefix):
    for st in result["stages"]:
        if st["name"].startswith(prefix):
            return st["text"]
    raise KeyError(prefix)


def error_sites(expected, got):
    """Expected-index intervals that differ, via word alignment."""
    sm = difflib.SequenceMatcher(a=expected, b=got, autojunk=False)
    sites = []
    for op, a0, a1, b0, b1 in sm.get_opcodes():
        if op == "equal":
            continue
        sites.append((a0, max(a1, a0 + 1), expected[a0:a1], got[b0:b1], b0, b1))
    return sites


def overlaps(site, others):
    a0, a1 = site[0], site[1]
    return any(o[0] < a1 and a0 < o[1] for o in others)


def sentence_for_word(sentences, word_idx):
    """Map a word index of the pre-grammar text to its router sentence."""
    n = 0
    for s in sentences:
        n += len(words_of(s["text"]))
        if word_idx < n:
            return s
    return sentences[-1] if sentences else None


def classify(meta, results, threshold):
    totals = {"pre_err": 0, "fixed": 0, "missed": 0, "introduced": 0,
              "router_miss": 0, "corrector_fail": 0, "guarded": 0}
    details = []
    for para, result in zip(meta["paragraphs"], results):
        expected = words_of(para["text"])
        pre = stage_text(result, "Vocab")
        grammar_stage = next(st for st in result["stages"]
                             if st["name"].startswith("Grammar"))
        post = grammar_stage["text"]
        pre_w, post_w = words_of(pre), words_of(post)
        pre_sites = error_sites(expected, pre_w)
        post_sites = error_sites(expected, post_w)
        sentences = grammar_stage.get("grammar_sentences") or []

        fixed = [s for s in pre_sites if not overlaps(s, post_sites)]
        missed = [s for s in pre_sites if overlaps(s, post_sites)]
        introduced = [s for s in post_sites if not overlaps(s, pre_sites)]

        totals["pre_err"] += len(pre_sites)
        totals["fixed"] += len(fixed)
        totals["missed"] += len(missed)
        totals["introduced"] += len(introduced)

        for site in missed:
            sent = sentence_for_word(sentences, site[4])
            if sent is None or sent.get("score") is None:
                continue
            if sent.get("guarded"):
                totals["guarded"] += 1
            elif sent["score"] >= threshold:
                totals["router_miss"] += 1
            else:
                totals["corrector_fail"] += 1

        details.append({
            "pre_err": len(pre_sites), "fixed": len(fixed),
            "missed": len(missed), "introduced": len(introduced),
            "fixed_sites": [fmt_site(s) for s in fixed],
            "missed_sites": [fmt_site(s, sentence_for_word(sentences, s[4]))
                             for s in missed],
            "introduced_sites": [fmt_site(s) for s in introduced],
        })
    return totals, details


def fmt_site(site, sent=None):
    _, _, exp, got, _, _ = site
    d = {"expected": " ".join(exp), "got": " ".join(got)}
    if sent is not None:
        d["router_score"] = sent.get("score")
        d["corrected"] = sent.get("corrected")
        d["guarded"] = sent.get("guarded")
    return d


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("wav")
    ap.add_argument("meta")
    ap.add_argument("--asr-dir", required=True)
    ap.add_argument("--probe-bin", required=True)
    ap.add_argument("--work-dir", required=True)
    ap.add_argument("--threshold", type=float, default=None)
    ap.add_argument("--expected", help="text file whose paragraphs are the "
                    "comparison target (for corrupted-input runs); defaults "
                    "to the synthesized text itself")
    args = ap.parse_args()

    meta = json.load(open(args.meta))
    if "paragraphs" not in meta:
        raise SystemExit("meta has no paragraphs; re-run tts_roundtrip with the offsets change")
    if args.expected:
        clean = [p.strip() for p in open(args.expected).read().split("\n\n")
                 if p.strip()]
        if len(clean) != len(meta["paragraphs"]):
            raise SystemExit(f"--expected has {len(clean)} paragraphs, "
                             f"meta has {len(meta['paragraphs'])}")
        for para, text in zip(meta["paragraphs"], clean):
            para["text"] = text
    os.makedirs(args.work_dir, exist_ok=True)

    threshold = args.threshold
    if threshold is None:
        cfg = os.path.join(os.path.dirname(__file__),
                           "../src-tauri/data/grammar/config.0.0.1.json")
        threshold = json.load(open(cfg))["router"]["threshold"]

    print("transcribing with Parakeet...")
    raws = transcribe_paragraphs(args.wav, meta, args.asr_dir,
                                 os.path.join(args.work_dir, "raws.json"))

    # Raw ASR quality headline (vs original text).
    raw_err = sum(len(error_sites(words_of(p["text"]), words_of(r)))
                  for p, r in zip(meta["paragraphs"], raws))
    print(f"raw ASR error sites vs original: {raw_err}")

    results = run_probe(args.probe_bin, raws,
                        os.path.join(args.work_dir, "pipeline_neural.json"))
    totals, details = classify(meta, results, threshold)
    json.dump(details, open(os.path.join(args.work_dir, "classified_neural.json"), "w"),
              indent=1)
    print(f"\n=== grammar stage (router threshold {threshold}) ===")
    print(f"  corrector input errors : {totals['pre_err']}")
    print(f"  fixed by grammar stage : {totals['fixed']}")
    print(f"  missed                 : {totals['missed']}"
          f"  (router miss {totals['router_miss']}, corrector fail {totals['corrector_fail']}, guarded {totals['guarded']})")
    print(f"  introduced by grammar  : {totals['introduced']}")


if __name__ == "__main__":
    main()
