#!/usr/bin/env python3
"""Build the British English pronunciation dictionary (data/gb_dict.json).

Sources:
  - wikipron eng_latn_uk_broad.tsv (Wiktionary-derived, CC BY-SA 3.0 — keep
    attribution if the dictionary ships): word -> space-separated IPA tokens,
    possibly several variants per word, NO stress marks.
  - data/cmudict_data.json (bundled US dict): ARPAbet with stress digits,
    used only to transfer stress positions onto the GB phones.

Output: {"word": "kəmpjˈuːtə", ...} — final IPA strings in espeak style
(stress mark immediately before the stressed vowel), every codepoint
guaranteed to exist in the target model's phoneme_id_map.

Usage:
    build_gb_dict.py WIKIPRON.tsv MODEL.onnx.json CMUDICT.json OUT.json [FREQ.txt]

With FREQ.txt (word-frequency list, "word count" per line) and espeak-ng on
PATH, the most frequent entries are validated against espeak en-gb-x-rp:
entries whose consonant skeleton disagrees are DROPPED (they fall through to
the app's US+transform path). espeak output is never copied into the output —
it is a dev-time oracle only, so nothing GPL-derived ships.
"""

import json
import re
import shutil
import subprocess
import sys
import unicodedata
from collections import defaultdict

# Normalize wikipron tokens toward the espeak en-gb inventory alba was
# trained on. Applied per token, before scoring/validation.
TOKEN_MAP = {
    "ɚ": "ə",       # americanized variant entries
    "ɝ": "ɜː",
    "ɐː": "ɑː",
    "əː": "ɜː",     # NURSE variant notation
    "ɛː": "eə",     # SQUARE monophthong notation -> trained diphthong
    "ɑ": "ɑː",      # bare PALM
    "ɔ": "ɒ",       # bare LOT variant
    "x": "k",       # loch etc.
    "ʍ": "w",
    "ɫ": "l",
    "ʔ": "t",
    "d͡ʒ": "dʒ",     # tie bars
    "t͡ʃ": "tʃ",
    "d͜ʒ": "dʒ",
    "t͜ʃ": "tʃ",
    "ɡ̊": "ɡ",
    "g": "ɡ",       # ASCII g -> IPA script g
}

# Combining marks to strip entirely (non-syllabic glide, tie bars leftovers).
STRIP_MARKS = {"̯", "͡", "͜", "̃", "̊"}

# Vowel first-codepoints, for stress placement and rhoticity checks.
VOWELS = set("ɑæʌəɔaɛɜeɪiɒoʊuʉɐ")

CMU_VOWEL = re.compile(r"^(AA|AE|AH|AO|AW|AY|EH|ER|EY|IH|IY|OW|OY|UH|UW)([0-2])$")

WORD_RE = re.compile(r"^[a-z']+$")

# The US phonemizer's function-word list (piper-plus-g2p english.rs), mirrored
# exactly. These words are EXCLUDED from the GB dictionary: Wiktionary lists
# their strong/citation forms ("a" as stressed ɑː, "to" with a full vowel),
# which over-stresses every sentence. Left out, they fall through to the US
# path, which uses the correct weak forms and destresses them — and function
# words don't differ materially between the dialects.
FUNCTION_WORDS = {
    "a", "about", "after", "am", "an", "and", "are", "as", "at", "be",
    "because", "been", "before", "being", "between", "but", "by", "can",
    "could", "did", "do", "does", "for", "from", "had", "has", "have",
    "having", "he", "her", "hers", "herself", "him", "himself", "his", "i",
    "if", "in", "into", "is", "it", "its", "itself", "may", "me", "might",
    "mine", "must", "my", "myself", "no", "nor", "not", "of", "on", "or",
    "our", "ours", "ourselves", "shall", "she", "should", "since", "so",
    "than", "that", "the", "their", "theirs", "them", "themselves", "they",
    "through", "to", "under", "us", "was", "we", "were", "when", "while",
    "will", "with", "would", "yet", "you", "your", "yours", "yourself",
}


def normalize_tokens(tokens):
    """Map/clean one wikipron pronunciation. Returns list of tokens or None.

    The post-pass aligns transcriber conventions with what espeak en-gb-x-rp
    (alba's training text) actually emits: TRAP is æ (bare 'a' only starts
    aɪ/aʊ), word-final schwa is ɐ, and happY is final ɪ.
    """
    out = []
    for t in tokens:
        t = TOKEN_MAP.get(t, t)
        t = "".join(ch for ch in unicodedata.normalize("NFD", t) if ch not in STRIP_MARKS)
        t = TOKEN_MAP.get(t, t)
        if not t:
            continue
        out.append(t)
    if not out:
        return None
    for i, t in enumerate(out):
        if t == "a":
            nxt = out[i + 1] if i + 1 < len(out) else ""
            if not nxt.startswith(("ɪ", "ʊ")):
                out[i] = "æ"
    # CHOICE is ɔɪ; some transcribers write ɒɪ.
    for i in range(len(out) - 1):
        if out[i] == "ɒ" and out[i + 1].startswith("ɪ"):
            out[i] = "ɔ"
    # Centring diphthongs: espeak writes NEAR as iə and SQUARE as eə. Only at
    # a vowel-unit start — the ɪ inside aɪ (fire f-a-ɪ-ə) is a diphthong tail
    # and must stay ɪ.
    for i in range(len(out) - 1):
        if out[i + 1] == "ə" and (i == 0 or out[i - 1][0] not in VOWELS):
            if out[i] == "ɪ":
                out[i] = "i"
            elif out[i] == "ɛ":
                out[i] = "e"
    # Word-final schwa is ɐ (lettER, commA) — but not the tail of a centring
    # diphthong (near niə, fire faɪə), where the previous token is a vowel.
    if out[-1] == "ə" and (len(out) < 2 or out[-2][0] not in VOWELS):
        out[-1] = "ɐ"
    elif out[-1] == "i":
        out[-1] = "ɪ"
    return out


# ARPAbet consonants -> IPA, for comparing consonant skeletons across dicts.
ARPA_CONS = {
    "B": "b", "CH": "tʃ", "D": "d", "DH": "ð", "F": "f", "G": "ɡ", "HH": "h",
    "JH": "dʒ", "K": "k", "L": "l", "M": "m", "N": "n", "NG": "ŋ", "P": "p",
    "R": "ɹ", "S": "s", "SH": "ʃ", "T": "t", "TH": "θ", "V": "v", "W": "w",
    "Y": "j", "Z": "z", "ZH": "ʒ",
}
IPA_CONS = set("".join(ARPA_CONS.values()))


def cons_skeleton_ipa(s):
    return "".join(ch for ch in s if ch in IPA_CONS)


def cons_skeleton_cmu(arpabet):
    return "".join(ARPA_CONS.get(re.sub(r"[0-2]$", "", t), "") for t in arpabet.split())


def variant_score(tokens, cmu_entry=None):
    """Lower is better: prefer plainly-RP transcriptions over dialect variants."""
    joined = "".join(tokens)
    score = 0
    # Variants whose consonants disagree with the US dictionary are usually
    # scrape noise or an obscure regionalism (wikipron's first "dig" entry is
    # d ɪ d͡ʒ). Consonants rarely differ between the dialects, so agreement
    # with CMU is a strong signal when the word has one.
    if cmu_entry and cons_skeleton_ipa(joined) != cons_skeleton_cmu(cmu_entry):
        score += 15
    for bad in ("ɚ", "ɝ", "ɹ̩", "ᵻ"):
        score += joined.count(bad) * 10
    # Narrow-transcription variants (aspiration, dental marks) are noise the
    # model never saw for English; a plain variant should always beat them.
    for narrow in ("ʰ", "̪", "ʲ", "ˤ", "̚", "̥"):
        score += joined.count(narrow) * 20
    # Rhotic transcriptions (ɹ before a consonant/word-end) are US-flavoured.
    for i, t in enumerate(tokens):
        if t == "ɹ":
            nxt = tokens[i + 1] if i + 1 < len(tokens) else ""
            if not nxt or nxt[0] not in VOWELS:
                score += 5
    # When Wiktionary lists dialect variants side by side (bath: æ vs ɑː,
    # hot: ɑ vs ɒ), prefer the RP realization. TRAP/LOT words with a single
    # transcription are unaffected — this only breaks ties between variants.
    score -= joined.count("ɑː") * 2
    score -= joined.count("ɒ")
    score -= joined.count("əʊ")
    return score


def cmu_stress_positions(arpabet):
    """Vowel-index -> stress level (1 primary, 2 secondary) from a CMU string."""
    out = {}
    vi = 0
    for tok in arpabet.split():
        m = CMU_VOWEL.match(tok)
        if m:
            lvl = int(m.group(2))
            if lvl in (1, 2):
                out[vi] = lvl
            vi += 1
    return out, vi


def apply_stress(tokens, word, cmudict):
    """Insert espeak-style stress marks immediately before stressed vowels.

    A vowel token directly after another vowel token is the tail of a
    diphthong pair (aɪ, əʊ, iə...), not a new syllable — count units, not
    tokens, or stress alignment against CMU drifts on every diphthong word.
    """
    vowel_idx = [
        i for i, t in enumerate(tokens)
        if t[0] in VOWELS and (i == 0 or tokens[i - 1][0] not in VOWELS)
    ]
    if not vowel_idx:
        return tokens
    marks = {}
    cmu = cmudict.get(word)
    if cmu:
        pos, cmu_vcount = cmu_stress_positions(cmu)
        if pos and cmu_vcount == len(vowel_idx):
            marks = pos
        elif pos:
            # Vowel counts differ across dialects; clamp indexes.
            marks = {min(k, len(vowel_idx) - 1): v for k, v in pos.items()}
    if not marks:
        # Monosyllables trivially; else default primary stress on the first
        # vowel (the dominant English pattern).
        marks = {0: 1}
    out = []
    for i, t in enumerate(tokens):
        if i in (vowel_idx[k] for k in marks):
            k = vowel_idx.index(i)
            out.append("ˈ" if marks.get(k) == 1 else "ˌ")
        out.append(t)
    return out


def main():
    wik_path, model_json, cmu_path, out_path = sys.argv[1:5]
    id_map = set(json.load(open(model_json))["phoneme_id_map"].keys())
    cmudict = {k.lower(): v for k, v in json.load(open(cmu_path)).items()}

    variants = defaultdict(list)
    for line in open(wik_path, encoding="utf-8"):
        parts = line.rstrip("\n").split("\t")
        if len(parts) != 2:
            continue
        word = parts[0].lower()
        if not WORD_RE.match(word) or word in FUNCTION_WORDS:
            continue
        tokens = normalize_tokens(parts[1].split(" "))
        if tokens:
            variants[word].append(tokens)

    out = {}
    dropped_symbols = defaultdict(int)
    dropped_words = 0
    stressed_from_cmu = 0
    for word, cands in variants.items():
        cands.sort(key=lambda t: variant_score(t, cmudict.get(word)))
        tokens = cands[0]
        tokens = apply_stress(tokens, word, cmudict)
        final = "".join(tokens)
        bad = [ch for ch in final if ch not in id_map]
        if bad:
            for ch in bad:
                dropped_symbols[ch] += 1
            dropped_words += 1
            continue
        if word in cmudict:
            stressed_from_cmu += 1
        out[word] = final

    # Optional espeak sweep: drop frequent entries whose consonant skeleton
    # disagrees with espeak en-gb-x-rp (single-entry landmines have no variant
    # to out-score). Drop-only — nothing from espeak is written to the output.
    freq_path = sys.argv[5] if len(sys.argv) > 5 else None
    if freq_path and shutil.which("espeak-ng"):
        frequent = []
        with open(freq_path, encoding="utf-8") as f:
            for line in f:
                w = line.split(" ")[0]
                if w in out:
                    frequent.append(w)
                if len(frequent) >= 5000:
                    break
        proc = subprocess.run(
            ["espeak-ng", "-v", "en-gb-x-rp", "--ipa", "-q"],
            input="\n".join(frequent), capture_output=True, text=True,
        )
        esp = [l.strip().replace(" ", "") for l in proc.stdout.strip().split("\n")]
        swept = 0
        for w, e in zip(frequent, esp):
            if cons_skeleton_ipa(out[w]) != cons_skeleton_ipa(e):
                del out[w]
                swept += 1
        print(f"espeak sweep: dropped {swept} of {len(frequent)} frequent entries")

    with open(out_path, "w", encoding="utf-8") as f:
        json.dump(out, f, ensure_ascii=False, separators=(",", ":"))

    print(f"entries: {len(out)}  dropped: {dropped_words}")
    print(f"stress transferred from CMU: {stressed_from_cmu} ({stressed_from_cmu * 100 // max(1, len(out))}%)")
    if dropped_symbols:
        top = sorted(dropped_symbols.items(), key=lambda kv: -kv[1])[:12]
        print("dropped symbols:", " ".join(f"{repr(s)}x{n}" for s, n in top))
    for probe in ("computer", "garden", "water", "schedule", "tomato", "bath", "grass", "privacy"):
        print(f"  {probe}: {out.get(probe)}")


if __name__ == "__main__":
    main()
