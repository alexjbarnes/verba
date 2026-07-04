#!/usr/bin/env python3
"""Export fine-tuned grammar checkpoints to the app's ONNX INT8 layout.

Takes local HF checkpoints from finetune_grammar_t5.py and/or
finetune_grammar_router.py and produces the exact file set grammar_neural.rs
embeds (same graph input/output names, same INT8 quantization, fresh
cross-attention KV weights — those train too, so they MUST be re-extracted
per fine-tune). Reuses the normalization/quantization helpers from
download_t5_grammar_onnx.py.

Usage:
    export_finetuned_grammar.py --output-dir DIR --version 0.0.2 \
        [--t5-checkpoint CKPT] [--router-checkpoint CKPT]

The output dir then holds files named for --version; to build them into the
app, copy over src-tauri/data/grammar/ with the 0.0.1 names build.rs and
include_bytes! expect (or bump the version in both).
"""

import argparse
import shutil
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
import download_t5_grammar_onnx as base  # noqa: E402


def patch_optimum_py314():
    """functools.partial-as-descriptor breaks Optimum config init on 3.14."""
    from optimum.exporters import base as export_base

    def fixed_init(self, config, task, int_dtype="int64", float_dtype="fp32"):
        self.task = task
        self._config = config
        self._normalized_config = type(self).NORMALIZED_CONFIG_CLASS(self._config)
        self.int_dtype = int_dtype
        self.float_dtype = float_dtype

    export_base.ExporterConfig.__init__ = fixed_init


def export_t5(ckpt: str, out: Path, ver):
    """Hand-rolled ONNX export matching grammar_neural.rs's decoder contract.

    Current Optimum emits decoder graphs WITHOUT an encoder_hidden_states
    input (cross-attn KV arrives only via pkv), but the Rust side feeds
    encoder_hidden_states every step and reads 1 + 16 positional outputs
    (logits, then present KV for all 16 slots including re-emitted cross).
    So the graphs are exported directly from torch with exact input/output
    names, then quantized with the same helpers as the stock pipeline.
    """
    import torch
    from transformers import T5ForConditionalGeneration
    from transformers.cache_utils import EncoderDecoderCache

    model = T5ForConditionalGeneration.from_pretrained(ckpt).eval()
    cfg = model.config
    layers, heads, d_kv, d_model = (cfg.num_decoder_layers, cfg.num_heads,
                                    cfg.d_kv, cfg.d_model)
    if (layers, heads, d_kv, d_model) != (4, 4, 64, 256):
        raise SystemExit(f"model geometry {(layers, heads, d_kv, d_model)} != "
                         "(4, 4, 64, 256) expected by grammar_neural.rs")

    tmp = out / "_t5_export"
    tmp.mkdir(parents=True, exist_ok=True)
    ids = torch.tensor([[21603, 10, 3, 9, 1782, 1]])  # any short sequence
    mask = torch.ones_like(ids)

    class Encoder(torch.nn.Module):
        def __init__(self):
            super().__init__()
            self.enc = model.encoder

        def forward(self, input_ids, attention_mask):
            return self.enc(input_ids=input_ids,
                            attention_mask=attention_mask).last_hidden_state

    enc_path = tmp / "encoder.onnx"
    torch.onnx.export(
        Encoder(), (ids, mask), str(enc_path),
        input_names=["input_ids", "attention_mask"],
        output_names=["hidden_states"],
        dynamic_axes={"input_ids": {0: "batch", 1: "seq"},
                      "attention_mask": {0: "batch", 1: "seq"},
                      "hidden_states": {0: "batch", 1: "seq"}},
        opset_version=14, dynamo=False,
    )

    class Decoder(torch.nn.Module):
        def __init__(self):
            super().__init__()
            self.dec = model.decoder
            self.lm_head = model.lm_head
            self.scale = d_model ** -0.5 if cfg.tie_word_embeddings else 1.0

        def forward(self, input_ids, encoder_attention_mask,
                    encoder_hidden_states, *pkv):
            legacy = tuple(tuple(pkv[4 * l + j] for j in range(4))
                           for l in range(layers))
            past = EncoderDecoderCache.from_legacy_cache(legacy)
            # Explicit cache_position derived from the past tensor's shape:
            # keeps position bias dynamic over past length instead of baking
            # the traced value in (the query is always a single token).
            past_len = torch._shape_as_tensor(pkv[0])[2].to(input_ids.device)
            cache_position = past_len.reshape(1)
            outd = self.dec(
                input_ids=input_ids,
                encoder_hidden_states=encoder_hidden_states,
                encoder_attention_mask=encoder_attention_mask,
                past_key_values=past,
                use_cache=True,
                cache_position=cache_position,
            )
            logits = self.lm_head(outd.last_hidden_state * self.scale)
            flat = [t for layer in outd.past_key_values.to_legacy_cache()
                    for t in layer]
            # With cross-KV precomputed, nothing consumes
            # encoder_hidden_states and the tracer would prune the input —
            # but the Rust side feeds it every step, so keep it alive as a
            # trailing output (outputs are read positionally, extras are
            # ignored).
            return (logits, *flat, encoder_hidden_states)

    enc_hidden = Encoder()(ids, mask)
    tok1 = torch.tensor([[cfg.decoder_start_token_id]])
    # Slot layout per layer: self.key, self.value, cross.key, cross.value —
    # self starts empty, cross is the precomputed projection (any content
    # with the right shape works for tracing).
    pkv = []
    for _ in range(layers):
        pkv += [torch.zeros(1, heads, 0, d_kv), torch.zeros(1, heads, 0, d_kv),
                torch.zeros(1, heads, ids.shape[1], d_kv),
                torch.zeros(1, heads, ids.shape[1], d_kv)]
    pkv_names = [f"pkv_{i}" for i in range(4 * layers)]
    dyn = {"input_ids": {0: "batch", 1: "dec_seq"},
           "encoder_attention_mask": {0: "batch", 1: "enc_seq"},
           "encoder_hidden_states": {0: "batch", 1: "enc_seq"},
           "logits": {0: "batch", 1: "dec_seq"}}
    for i, n in enumerate(pkv_names):
        axis = "enc_seq" if i % 4 >= 2 else "past_seq"
        dyn[n] = {0: "batch", 2: axis}
        dyn[f"present_{i}"] = {0: "batch", 2: "enc_seq" if i % 4 >= 2 else "present_seq"}

    dec_path = tmp / "decoder_with_past.onnx"
    torch.onnx.export(
        Decoder(), (tok1, mask, enc_hidden, *pkv), str(dec_path),
        input_names=["input_ids", "encoder_attention_mask",
                     "encoder_hidden_states"] + pkv_names,
        output_names=["logits"] + [f"present_{i}" for i in range(4 * layers)]
        + ["hidden_passthrough"],
        dynamic_axes=dyn, opset_version=14, dynamo=False,
    )

    # Cross-attention KV weights change with fine-tuning: always re-extract.
    kv_bin = out / ver("cross_attn_kv_weights.bin")
    base.extract_cross_attn_weights(ckpt, kv_bin)

    # Contract check on the fp32 graphs: compare tensors against the torch
    # modules directly. Text-level equality is the wrong oracle here — even
    # fp32 graphs flip near-tie tokens vs torch kernels.
    print("\nfp32 contract check:")
    verify_contract(ckpt, enc_path, dec_path, kv_bin, ids, mask)

    q_enc = out / ver("encoder_model_quantized.onnx")
    q_dec = out / ver("decoder_with_past_quantized.onnx")
    base.quantize_model(enc_path, q_enc)
    base.quantize_model(dec_path, q_dec)
    if not base.verify_decoder(q_dec):
        raise SystemExit("decoder input verification failed")

    print("\nINT8 spot check (small drift vs fp32 is expected):")
    ok, n = verify_roundtrip(ckpt, q_enc, q_dec, kv_bin)
    if ok < n - 2:
        raise SystemExit(f"INT8 graphs diverge badly ({ok}/{n}) — inspect before shipping")

    for src_name, dst in [("tokenizer.json", "t5_tokenizer.json"),
                          ("tokenizer_config.json", "tokenizer_config.json"),
                          ("special_tokens_map.json", "special_tokens_map.json")]:
        p = Path(ckpt) / src_name
        if p.exists():
            shutil.copy(p, out / ver(dst))
    shutil.rmtree(tmp)


def verify_contract(ckpt, enc_file: Path, dec_file: Path, kv_file: Path,
                    ids, mask):
    """Numeric check of the fp32 graphs against the torch modules: encoder
    hidden states, the KV-bin cross projections, and two decoder steps (one
    empty-past, one through the returned cache). Catches layout/contract
    bugs without tripping on engine-level float drift. Loads its own model
    instance — the one used for tracing is not trusted after export."""
    import numpy as np
    import onnxruntime as rt
    import torch
    from transformers import T5ForConditionalGeneration

    model = T5ForConditionalGeneration.from_pretrained(ckpt).eval()
    enc = rt.InferenceSession(str(enc_file))
    dec = rt.InferenceSession(str(dec_file))
    kv_w = np.fromfile(kv_file, dtype="<f4").reshape(8, 256, 256)

    def close(name, a, b, tol):
        d = float(np.abs(a - b).max())
        print(f"  {name}: max|diff| {d:.2e}")
        if d > tol:
            raise SystemExit(f"{name} diverges ({d:.2e} > {tol}) — contract bug")

    with torch.no_grad():
        t_hidden = model.encoder(input_ids=ids, attention_mask=mask).last_hidden_state
    o_hidden = enc.run(None, {"input_ids": ids.numpy(), "attention_mask": mask.numpy()})[0]
    close("encoder hidden", o_hidden, t_hidden.numpy(), 2e-4)

    seq = o_hidden.shape[1]
    kv, w = [], 0
    with torch.no_grad():
        for i in range(16):
            if i % 4 >= 2:
                proj = (o_hidden[0] @ kv_w[w]).reshape(seq, 4, 64)
                bin_kv = np.ascontiguousarray(proj.transpose(1, 0, 2)[None], dtype=np.float32)
                attn = model.decoder.block[i // 4].layer[1].EncDecAttention
                lin = attn.k if i % 4 == 2 else attn.v
                t_kv = lin(t_hidden).view(1, seq, 4, 64).transpose(1, 2).numpy()
                close(f"cross-KV slot {i}", bin_kv, t_kv, 2e-4)
                kv.append(bin_kv)
                w += 1
            else:
                kv.append(np.zeros((1, 4, 0, 64), np.float32))

    from transformers.cache_utils import EncoderDecoderCache
    tok0 = np.array([[model.config.decoder_start_token_id]], np.int64)
    for step in range(2):
        feed = {"input_ids": tok0, "encoder_attention_mask": mask.numpy(),
                "encoder_hidden_states": o_hidden}
        feed.update({f"pkv_{i}": kv[i] for i in range(16)})
        outs = dec.run(None, feed)
        with torch.no_grad():
            legacy = tuple(tuple(torch.from_numpy(kv[4 * l + j]) for j in range(4))
                           for l in range(4))
            outd = model.decoder(
                input_ids=torch.from_numpy(tok0),
                encoder_hidden_states=torch.from_numpy(o_hidden),
                encoder_attention_mask=mask,
                past_key_values=EncoderDecoderCache.from_legacy_cache(legacy),
                use_cache=True,
            )
            scale = model.config.d_model ** -0.5 \
                if model.config.tie_word_embeddings else 1.0
            t_logits = model.lm_head(outd.last_hidden_state * scale).numpy()
        close(f"decoder step {step + 1} logits", outs[0], t_logits, 5e-3)
        kv = outs[1:17]
        tok0 = np.array([[int(outs[0][0, -1].argmax())]], np.int64)
    print("  contract OK")


def verify_roundtrip(ckpt: str, enc_file: Path, dec_file: Path, kv_file: Path):
    """Greedy-decode through the exported graphs exactly the way the Rust
    side does (precomputed cross KV from the .bin, positional outputs) and
    compare against the torch checkpoint's own generations."""
    import numpy as np
    import onnxruntime as rt
    import torch
    from transformers import AutoTokenizer, T5ForConditionalGeneration

    tok = AutoTokenizer.from_pretrained(ckpt)
    model = T5ForConditionalGeneration.from_pretrained(ckpt).eval()
    enc = rt.InferenceSession(str(enc_file))
    dec = rt.InferenceSession(str(dec_file))
    kv_w = np.fromfile(kv_file, dtype="<f4").reshape(8, 256, 256)

    tests = [
        "grammar: me and him goes to the store yesterday",
        "grammar: But guy's got places to go and he better show up.",
        "grammar: The players don't even get a vote, let alone the fans.",
        "grammar: Why sprint if you aren't sure where you is going?",
        "grammar: I would opt out of wheels and heels entirely.",
        "grammar: There is few status games more superficial than this.",
    ]
    ok = 0
    for text in tests:
        e = tok(text, return_tensors="np")
        ids, mask = e["input_ids"].astype(np.int64), e["attention_mask"].astype(np.int64)
        hidden = enc.run(None, {"input_ids": ids, "attention_mask": mask})[0]
        seq = hidden.shape[1]
        kv = []
        w = 0
        for i in range(16):
            if i % 4 >= 2:
                proj = (hidden[0] @ kv_w[w]).reshape(seq, 4, 64)
                kv.append(np.ascontiguousarray(
                    proj.transpose(1, 0, 2)[None]).astype(np.float32))
                w += 1
            else:
                kv.append(np.zeros((1, 4, 0, 64), np.float32))
        toks = []
        nxt = 0  # decoder_start_token_id
        for _ in range(seq + 32):
            feed = {"input_ids": np.array([[nxt]], np.int64),
                    "encoder_attention_mask": mask,
                    "encoder_hidden_states": hidden}
            feed.update({f"pkv_{i}": kv[i] for i in range(16)})
            outs = dec.run(None, feed)
            nxt = int(outs[0][0, -1].argmax())
            kv = outs[1:17]
            if nxt == 1:  # eos
                break
            toks.append(nxt)
        got = tok.decode(toks, skip_special_tokens=True)
        with torch.no_grad():
            ref_ids = model.generate(torch.tensor(e["input_ids"]),
                                     max_new_tokens=seq + 32, num_beams=1)
        ref = tok.decode(ref_ids[0], skip_special_tokens=True)
        match = got == ref
        ok += match
        print(f"  [{'=' if match else '!'}] {got!r}"
              + ("" if match else f"  (torch: {ref!r})"))
    print(f"round-trip: {ok}/{len(tests)} match torch generate")
    return ok, len(tests)


def export_router(ckpt: str, out: Path, ver):
    patch_optimum_py314()
    from onnxruntime.quantization import QuantType, quantize_dynamic
    from optimum.onnxruntime import ORTModelForSequenceClassification

    tmp = out / "_router_export"
    print(f"Exporting router {ckpt} -> ONNX...")
    ORTModelForSequenceClassification.from_pretrained(ckpt, export=True) \
        .save_pretrained(str(tmp))
    model = next(tmp.glob("*.onnx"))
    q = out / ver("cola_model_quantized.onnx")
    quantize_dynamic(str(model), str(q), weight_type=QuantType.QInt8)
    print(f"  {q.name} ({q.stat().st_size // (1024 * 1024)}MB)")
    tok = next(p for p in (Path(ckpt) / "tokenizer.json", tmp / "tokenizer.json")
               if p.exists())
    shutil.copy(tok, out / ver("cola_tokenizer.json"))
    shutil.rmtree(tmp)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--output-dir", required=True)
    ap.add_argument("--version", default="0.0.2")
    ap.add_argument("--t5-checkpoint")
    ap.add_argument("--router-checkpoint")
    args = ap.parse_args()
    if not args.t5_checkpoint and not args.router_checkpoint:
        raise SystemExit("nothing to export: pass --t5-checkpoint and/or --router-checkpoint")

    out = Path(args.output_dir)
    out.mkdir(parents=True, exist_ok=True)

    def ver(name):
        stem, dot, ext = name.rpartition(".")
        return f"{stem}.{args.version}.{ext}" if dot else name

    if args.t5_checkpoint:
        export_t5(args.t5_checkpoint, out, ver)
    if args.router_checkpoint:
        export_router(args.router_checkpoint, out, ver)

    print("\nExported files:")
    for p in sorted(out.iterdir()):
        if p.is_file() and args.version in p.name:
            print(f"  {p.name} ({p.stat().st_size // 1024}KB)")


if __name__ == "__main__":
    main()
