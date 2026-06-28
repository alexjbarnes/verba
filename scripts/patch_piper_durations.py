#!/usr/bin/env python3
"""Expose the Piper VITS duration predictor output (`w_ceil`) as a second ONNX
graph output so the app can compute exact per-word timing.

Stock Piper exports emit only the waveform. The duration predictor runs anyway;
this just declares its `Ceil` output (`/Ceil_output_0` = per-input-id frame
counts) as a graph output. At runtime: samples_for_id = w_ceil[id] * hop_length
(hop_length = 256 for the medium models; verified: output_samples == sum(w_ceil)
* 256). The added output is free — that node already executes.

Usage:
    patch_piper_durations.py IN.onnx OUT.onnx [--tensor /Ceil_output_0] [--no-verify]

Run in an env with onnx (and onnxruntime for --verify): the repo's
.android-deps/grammar-venv has both.
"""
import argparse
import sys

import onnx
from onnx import TensorProto, helper


def find_duration_tensor(graph, preferred):
    """Return the name of the per-id duration tensor. Prefer `preferred`
    (`/Ceil_output_0` on the medium models); otherwise fall back to the output
    of the single `Ceil` node that feeds a `CumSum` (the VITS alignment), which
    is how the duration predictor result is built across Piper exports."""
    outputs = {o for n in graph.node for o in n.output}
    if preferred in outputs:
        return preferred
    ceils = [n for n in graph.node if n.op_type == "Ceil"]
    cumsum_inputs = {i for n in graph.node if n.op_type == "CumSum" for i in n.input}
    for n in ceils:
        if n.output and n.output[0] in cumsum_inputs:
            return n.output[0]
    if len(ceils) == 1 and ceils[0].output:
        return ceils[0].output[0]
    raise SystemExit(
        f"Could not locate the duration tensor (preferred {preferred!r} absent, "
        f"found {len(ceils)} Ceil nodes). Inspect the graph manually."
    )


def verify(path, hop=256):
    import json
    import os

    import numpy as np
    import onnxruntime as ort

    cfg_path = path.replace("_dur.onnx", ".onnx").rsplit(".onnx", 1)[0] + ".onnx.json"
    if not os.path.exists(cfg_path):
        print(f"  (skip verify: no sidecar at {cfg_path})")
        return
    cfg = json.load(open(cfg_path))
    pim = cfg["phoneme_id_map"]
    syms = [s for s in ["h", "ɛ", "l", "o", "ʊ", "w", "ɹ", "d"] if s in pim]
    ids = [1]
    for s in syms:
        ids += [pim[s][0], 0]
    ids += [2]
    sess = ort.InferenceSession(path, providers=["CPUExecutionProvider"])
    inf = cfg.get("inference", {})
    out = sess.run(None, {
        "input": np.array([ids], dtype=np.int64),
        "input_lengths": np.array([len(ids)], dtype=np.int64),
        "scales": np.array([inf.get("noise_scale", 0.667), 1.0, inf.get("noise_w", 0.8)], dtype=np.float32),
        "sid": np.array([0], dtype=np.int64),
    })
    res = dict(zip([o.name for o in sess.get_outputs()], out))
    dur_name = next(n for n in res if n != "output")
    T = res["output"].squeeze().shape[-1]
    s = float(res[dur_name].sum())
    ratio = T / s if s else 0
    ok = abs(ratio - hop) < 0.5
    print(f"  verify: T={T} sum(w_ceil)={s:.0f} T/sum={ratio:.2f} (hop {hop}) -> {'OK' if ok else 'MISMATCH'}")
    if not ok:
        sys.exit("verification failed: T/sum(w_ceil) != hop_length")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("input")
    ap.add_argument("output")
    ap.add_argument("--tensor", default="/Ceil_output_0")
    ap.add_argument("--no-verify", action="store_true")
    args = ap.parse_args()

    model = onnx.load(args.input)
    name = find_duration_tensor(model.graph, args.tensor)
    if any(o.name == name for o in model.graph.output):
        print(f"{name} already an output; nothing to do")
    else:
        model.graph.output.extend([helper.make_tensor_value_info(name, TensorProto.FLOAT, None)])
        print(f"exposed duration tensor {name!r} as a graph output")
    onnx.save(model, args.output)
    print(f"wrote {args.output}")
    if not args.no_verify:
        verify(args.output)


if __name__ == "__main__":
    main()
