#!/usr/bin/env python3
"""Frame-based DER with optional collar and best-permutation mapping.
Usage: der.py <ref.rttm> <run.log> [collar]
Parses "span   A-  Bs -> speaker N" lines from the harness log.
"""
import itertools
import re
import sys

STEP = 0.01

def load_rttm(path):
    segs = []
    for line in open(path):
        p = line.split()
        if len(p) >= 8 and p[0] == "SPEAKER":
            segs.append((float(p[3]), float(p[3]) + float(p[4]), p[7]))
    return segs

def load_log(path):
    segs = []
    pat = re.compile(r"span\s+([\d.]+)-\s*([\d.]+)s -> speaker (\d+)")
    for line in open(path):
        m = pat.search(line)
        if m:
            segs.append((float(m.group(1)), float(m.group(2)), m.group(3)))
    return segs

def frames(segs, n):
    out = [set() for _ in range(n)]
    for a, b, s in segs:
        for i in range(int(a / STEP), min(int(b / STEP) + 1, n)):
            out[i].add(s)
    return out

def main():
    ref_segs = load_rttm(sys.argv[1])
    hyp_segs = load_log(sys.argv[2])
    collar = float(sys.argv[3]) if len(sys.argv) > 3 else 0.25
    end = max(max(b for _, b, _ in ref_segs), max(b for _, b, _ in hyp_segs))
    n = int(end / STEP) + 1
    ref = frames(ref_segs, n)
    hyp = frames(hyp_segs, n)

    skip = [False] * n
    if collar > 0:
        for a, b, _ in ref_segs:
            for t in (a, b):
                for i in range(max(0, int((t - collar) / STEP)), min(n, int((t + collar) / STEP) + 1)):
                    skip[i] = True

    ref_names = sorted({s for _, _, s in ref_segs})
    hyp_names = sorted({s for _, _, s in hyp_segs})
    best = None
    for perm in itertools.permutations(hyp_names, min(len(hyp_names), len(ref_names))):
        mapping = dict(zip(perm, ref_names))
        miss = fa = conf = total = 0
        for i in range(n):
            if skip[i]:
                continue
            r, h = ref[i], {mapping.get(x, "?" + x) for x in hyp[i]}
            total += len(r)
            miss += max(0, len(r) - len(h))
            fa += max(0, len(h) - len(r))
            conf += min(len(r), len(h)) - len(r & h)
        der = (miss + fa + conf) / max(total, 1)
        if best is None or der < best[0]:
            best = (der, miss, fa, conf, total, mapping)

    der, miss, fa, conf, total, mapping = best
    print(f"DER {der * 100:.1f}%  (miss {miss / total * 100:.1f}, fa {fa / total * 100:.1f}, conf {conf / total * 100:.1f})  collar {collar}")
    print("mapping:", {h: r for h, r in mapping.items()})

    # Per-ref-speaker attribution: where did each ref speaker's time go?
    for rs in ref_names:
        got = {}
        tot = 0
        for i in range(n):
            if rs in ref[i]:
                tot += 1
                for h in hyp[i]:
                    got[mapping.get(h, "?" + h)] = got.get(mapping.get(h, "?" + h), 0) + 1
                if not hyp[i]:
                    got["(missed)"] = got.get("(missed)", 0) + 1
        parts = ", ".join(f"{k} {v * STEP:.1f}s" for k, v in sorted(got.items(), key=lambda x: -x[1]))
        print(f"  ref {rs} ({tot * STEP:.1f}s): {parts}")

if __name__ == "__main__":
    main()
