#!/usr/bin/env python3
"""
Download and prepare T5 grammar correction ONNX files for the grammar_neural module.

Downloads pre-exported ONNX files from HuggingFace, quantizes to INT8, normalizes
input/output names to match what the Rust code expects, and extracts cross-attention
KV projection weights.

Outputs (all written to --output-dir, with optional --version suffix):
  encoder_model_quantized.onnx       - T5 encoder (INT8)
  decoder_with_past_quantized.onnx   - T5 decoder with KV cache inputs (INT8)
  cross_attn_kv_weights.bin          - 8x [256,256] f32 cross-attention projections
  t5_tokenizer.json                  - SentencePiece tokenizer (HF format)
  special_tokens_map.json            - EOS/UNK/PAD token mappings
  tokenizer_config.json              - Max length, padding side, special token IDs

Usage:
    pip install -r scripts/requirements-grammar.txt
    python scripts/download_t5_grammar_onnx.py --output-dir src-tauri/data/grammar/
    python scripts/download_t5_grammar_onnx.py --output-dir src-tauri/data/grammar/ --version 0.0.1
    python scripts/download_t5_grammar_onnx.py --output-dir src-tauri/data/grammar/ --version 0.0.1 --verify

Reproducibility:
    Quantization output depends on exact library versions. Use the pinned versions
    in scripts/requirements-grammar.txt. Run with --verify to check output MD5s
    against known-good values.
"""
import argparse
import hashlib
import re
import shutil
import sys
import tempfile
from pathlib import Path

import numpy as np
import onnx


MODEL_ID = "visheratin/t5-efficient-tiny-grammar-correction"

# Canonical decoder input names matching the Rust grammar_neural code.
# Rust feeds inputs by name, reads outputs by positional index.
EXPECTED_DECODER_INPUTS = {
    "input_ids",
    "encoder_attention_mask",
    "encoder_hidden_states",
    *(f"pkv_{i}" for i in range(16)),
}

# Known-good MD5 checksums for v0.0.1 output files.
# Generated with onnxruntime==1.24.4, onnx==1.21.0, numpy==2.4.3.
KNOWN_CHECKSUMS = {
    "0.0.1": {
        "decoder_with_past_quantized.onnx": "88e7f9f00085d51c0bfc777e5dc60fd9",
        "encoder_model_quantized.onnx": "87193644b7f16105f2a28e8347cf522e",
        "cross_attn_kv_weights.bin": "d29c5831e60b94232b85703a8723c9c4",
        "t5_tokenizer.json": "4bab65b652e076c1a6fc8ed1bdfae2c2",
        "special_tokens_map.json": "8fd03e945174de0818746ecbde1aad8e",
        "tokenizer_config.json": "d171383bb03ce07e046c17dc737db124",
    },
}


def check_dependency_versions():
    """Warn if installed versions differ from the pinned requirements."""
    import importlib.metadata

    expected = {
        "onnxruntime": "1.24.4",
        "onnx": "1.21.0",
        "numpy": "2.4.3",
    }
    mismatches = []
    for pkg, want in expected.items():
        try:
            got = importlib.metadata.version(pkg)
        except importlib.metadata.PackageNotFoundError:
            mismatches.append(f"  {pkg}: NOT INSTALLED (need {want})")
            continue
        if got != want:
            mismatches.append(f"  {pkg}: {got} (need {want})")

    if mismatches:
        print("ERROR: dependency version mismatch. Quantization will not be reproducible.")
        for m in mismatches:
            print(m)
        print("Install pinned versions: pip install -r scripts/requirements-grammar.txt")
        sys.exit(1)


def md5_file(path: Path) -> str:
    h = hashlib.md5()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(8192), b""):
            h.update(chunk)
    return h.hexdigest()


def download_onnx_files(model_id: str, tmp_dir: Path):
    """Download ONNX model files from HuggingFace."""
    from huggingface_hub import hf_hub_download, list_repo_files

    print(f"Listing files in {model_id}...")
    all_files = list(list_repo_files(model_id))
    onnx_files = [f for f in all_files if f.endswith(".onnx")]
    print(f"ONNX files found: {onnx_files}")

    def is_quant(f):
        return "quant" in f.lower()

    # Encoder: prefer quantized if available in repo
    enc = next((f for f in onnx_files if "encoder" in f and is_quant(f)), None) \
       or next((f for f in onnx_files if "encoder" in f), None)
    if enc is None:
        raise RuntimeError(f"No encoder ONNX found in {onnx_files}")
    local = hf_hub_download(model_id, enc)
    enc_path = tmp_dir / "encoder.onnx"
    shutil.copy(local, enc_path)
    print(f"  {enc} -> {enc_path.name} (needs_quant={not is_quant(enc)})")

    # Decoder with past KV cache.
    # Naming conventions vary across Optimum versions:
    #   - Older exports: decoder_with_past_model.onnx
    #   - Newer exports: decoder_model.onnx (contains past KV inputs)
    # We detect the right file by checking for past KV inputs.
    dec_candidates = [
        f for f in onnx_files
        if "decoder" in f and "init" not in f
    ]
    dec_wp = None
    for candidate in dec_candidates:
        local = hf_hub_download(model_id, candidate)
        m = onnx.load(local)
        input_names = {inp.name for inp in m.graph.input}
        has_kv = any("pkv" in n or "past_key_values" in n for n in input_names)
        if has_kv:
            dec_wp = local
            print(f"  {candidate} -> decoder with past KV ({len(input_names)} inputs)")
            break
        print(f"  {candidate}: no past KV inputs, skipping")

    if dec_wp is None:
        raise RuntimeError(
            f"No decoder-with-past ONNX found in {onnx_files}. "
            f"All decoder candidates lacked past KV cache inputs."
        )
    dec_path = tmp_dir / "decoder_with_past.onnx"
    shutil.copy(dec_wp, dec_path)

    # Tokenizer files
    for fname in ["tokenizer.json", "tokenizer_config.json", "special_tokens_map.json"]:
        if fname in all_files:
            local = hf_hub_download(model_id, fname)
            shutil.copy(local, tmp_dir / fname)
            print(f"  {fname}")

    return enc_path, dec_path, is_quant(enc)


def normalize_decoder_inputs(model_path: Path) -> Path:
    """Rename decoder inputs to canonical names if needed.

    Different Optimum versions produce different naming conventions:
      - Older: pkv_0, pkv_1, ..., encoder_hidden_states
      - Newer: past_key_values.0.decoder.key, ..., possibly no encoder_hidden_states

    Normalizes to: pkv_0..pkv_15 + encoder_hidden_states.
    """
    model = onnx.load(str(model_path))
    input_names = {inp.name for inp in model.graph.input}

    if input_names == EXPECTED_DECODER_INPUTS:
        print(f"  Decoder inputs already canonical")
        return model_path

    rename_map = {}

    # Map past_key_values.L.{decoder,encoder}.{key,value} -> pkv_N
    # Per-layer order: decoder_key=0, decoder_value=1, encoder_key=2, encoder_value=3
    pkv_pattern = re.compile(
        r"past_key_values\.(\d+)\.(decoder|encoder)\.(key|value)"
    )
    offset_table = {
        ("decoder", "key"): 0, ("decoder", "value"): 1,
        ("encoder", "key"): 2, ("encoder", "value"): 3,
    }
    for inp in model.graph.input:
        m = pkv_pattern.match(inp.name)
        if m:
            layer = int(m.group(1))
            idx = layer * 4 + offset_table[(m.group(2), m.group(3))]
            rename_map[inp.name] = f"pkv_{idx}"

    # Find encoder_hidden_states under alternative names
    if "encoder_hidden_states" not in input_names:
        for inp in model.graph.input:
            if inp.name in rename_map or inp.name in ("input_ids", "encoder_attention_mask"):
                continue
            dims = inp.type.tensor_type.shape.dim
            if len(dims) == 3:
                last_dim = dims[2].dim_value
                if last_dim in (256, 512, 768):
                    rename_map[inp.name] = "encoder_hidden_states"
                    print(f"  Mapped {inp.name} -> encoder_hidden_states (3D, hidden={last_dim})")
                    break

    if not rename_map:
        print(f"  No renames needed")
        return model_path

    print(f"  Renaming {len(rename_map)} nodes:")
    for old, new in sorted(rename_map.items(), key=lambda x: x[1]):
        print(f"    {old} -> {new}")

    # Apply renames everywhere in the graph
    for inp in model.graph.input:
        if inp.name in rename_map:
            inp.name = rename_map[inp.name]
    for out in model.graph.output:
        if out.name in rename_map:
            out.name = rename_map[out.name]
    for node in model.graph.node:
        for i, name in enumerate(node.input):
            if name in rename_map:
                node.input[i] = rename_map[name]
        for i, name in enumerate(node.output):
            if name in rename_map:
                node.output[i] = rename_map[name]
    for init in model.graph.initializer:
        if init.name in rename_map:
            init.name = rename_map[init.name]

    out_path = model_path.with_name("decoder_with_past_normalized.onnx")
    onnx.save(model, str(out_path))
    return out_path


def normalize_encoder_outputs(model_path: Path) -> Path:
    """Ensure the encoder's primary output is named 'hidden_states'."""
    model = onnx.load(str(model_path))
    output_names = {out.name for out in model.graph.output}

    if "hidden_states" in output_names:
        print(f"  Encoder output already named 'hidden_states'")
        return model_path

    rename_map = {}
    if "last_hidden_state" in output_names:
        rename_map["last_hidden_state"] = "hidden_states"

    if not rename_map:
        print(f"  Encoder outputs: {output_names} (no rename needed)")
        return model_path

    print(f"  Renaming encoder outputs: {rename_map}")
    for out in model.graph.output:
        if out.name in rename_map:
            out.name = rename_map[out.name]
    for node in model.graph.node:
        for i, name in enumerate(node.output):
            if name in rename_map:
                node.output[i] = rename_map[name]

    out_path = model_path.with_name("encoder_normalized.onnx")
    onnx.save(model, str(out_path))
    return out_path


def quantize_model(model_path: Path, output_path: Path):
    """Quantize an ONNX model to INT8 dynamic quantization."""
    from onnxruntime.quantization import QuantType, quantize_dynamic

    print(f"  Quantizing {model_path.name}...")
    quantize_dynamic(
        str(model_path),
        str(output_path),
        weight_type=QuantType.QInt8,
    )
    orig_mb = model_path.stat().st_size / (1024 * 1024)
    quant_mb = output_path.stat().st_size / (1024 * 1024)
    print(f"  {orig_mb:.1f}MB -> {quant_mb:.1f}MB")


def extract_cross_attn_weights(model_id: str, out_path: Path):
    """Extract cross-attention K/V projection weights from the PyTorch model.

    ONNX exports use opaque initializer names (onnx::MatMul_NNN), so we pull
    the weights from PyTorch where they have proper names like
    decoder.block.0.layer.1.EncDecAttention.k.weight.

    Extracts 8 weight matrices (4 layers x K,V): each [256, 256] f32.
    Written as contiguous little-endian floats.
    """
    from transformers import T5ForConditionalGeneration

    print(f"\nExtracting cross-attention KV weights from PyTorch model...")
    model = T5ForConditionalGeneration.from_pretrained(model_id)

    num_layers = 4
    dim = 256
    matrices = []

    for layer in range(num_layers):
        for proj in ["k", "v"]:
            name = f"decoder.block.{layer}.layer.1.EncDecAttention.{proj}.weight"
            param = dict(model.named_parameters()).get(name)
            if param is None:
                raise RuntimeError(f"Could not find {name} in model parameters")

            arr = param.detach().cpu().numpy()
            if arr.shape != (dim, dim):
                raise RuntimeError(f"{name}: expected ({dim},{dim}), got {arr.shape}")
            matrices.append(arr)
            print(f"  layer {layer} {proj}: {name} [{arr.shape[0]}x{arr.shape[1]}]")

    # Transpose: PyTorch nn.Linear stores [out_features, in_features], but the
    # Rust code projects as hidden @ W (not hidden @ W.T), so save transposed.
    with open(out_path, "wb") as f:
        for mat in matrices:
            f.write(mat.T.astype("<f4").tobytes())

    expected_bytes = num_layers * 2 * dim * dim * 4
    actual = out_path.stat().st_size
    assert actual == expected_bytes, f"Expected {expected_bytes} bytes, got {actual}"
    print(f"  -> {out_path.name} ({actual // 1024}KB)")


def verify_decoder(model_path: Path):
    """Print and validate final decoder input/output names."""
    model = onnx.load(str(model_path))
    input_names = {inp.name for inp in model.graph.input}

    print(f"\nDecoder verification ({model_path.name}):")
    print(f"  Inputs ({len(model.graph.input)}):")
    for inp in model.graph.input:
        dims = [d.dim_value or d.dim_param for d in inp.type.tensor_type.shape.dim]
        print(f"    {inp.name}: {dims}")

    missing = EXPECTED_DECODER_INPUTS - input_names
    extra = input_names - EXPECTED_DECODER_INPUTS
    if missing:
        print(f"\n  FAIL: missing inputs: {missing}")
        return False
    if extra:
        print(f"\n  WARNING: unexpected extra inputs: {extra}")
    print(f"\n  OK: all {len(EXPECTED_DECODER_INPUTS)} expected inputs present")
    return True


def verify_checksums(output_dir: Path, version: str, versioned_fn) -> bool:
    """Verify output files match known-good checksums."""
    checksums = KNOWN_CHECKSUMS.get(version)
    if not checksums:
        print(f"\nNo known checksums for version {version}, skipping verification")
        return True

    print(f"\nVerifying checksums (version {version}):")
    all_ok = True
    for base_name, expected_md5 in sorted(checksums.items()):
        out_name = versioned_fn(base_name)
        path = output_dir / out_name
        if not path.exists():
            print(f"  MISSING: {out_name}")
            all_ok = False
            continue
        actual_md5 = md5_file(path)
        match = actual_md5 == expected_md5
        status = "OK" if match else "MISMATCH"
        print(f"  {status}: {out_name} ({actual_md5})")
        if not match:
            print(f"         expected {expected_md5}")
            all_ok = False

    return all_ok


def main():
    parser = argparse.ArgumentParser(
        description="Download and prepare T5 grammar correction ONNX model files"
    )
    parser.add_argument(
        "--output-dir",
        required=True,
        help="Directory to write model files to (e.g. src-tauri/data/grammar/)",
    )
    parser.add_argument(
        "--version",
        default=None,
        help="Version suffix for output files (e.g. 0.0.1 -> encoder_model_quantized.0.0.1.onnx)",
    )
    parser.add_argument(
        "--verify",
        action="store_true",
        help="After building, verify output MD5s match known-good checksums",
    )
    parser.add_argument(
        "--verify-only",
        action="store_true",
        help="Only verify existing files, don't download or build",
    )
    args = parser.parse_args()

    out = Path(args.output_dir)

    def versioned(name: str) -> str:
        if args.version is None:
            return name
        stem, ext = name.rsplit(".", 1)
        return f"{stem}.{args.version}.{ext}"

    if args.verify_only:
        if not args.version:
            print("--verify-only requires --version")
            sys.exit(1)
        ok = verify_checksums(out, args.version, versioned)
        sys.exit(0 if ok else 1)

    check_dependency_versions()

    out.mkdir(parents=True, exist_ok=True)

    with tempfile.TemporaryDirectory(prefix="grammar-export-") as tmp:
        tmp_dir = Path(tmp)

        # Step 1: Download from HuggingFace
        enc_path, dec_path, enc_already_quantized = download_onnx_files(
            MODEL_ID, tmp_dir
        )

        # Step 2: Normalize input/output names
        print(f"\nNormalizing decoder inputs...")
        dec_path = normalize_decoder_inputs(dec_path)

        print(f"\nNormalizing encoder outputs...")
        enc_path = normalize_encoder_outputs(enc_path)

        # Step 3: Quantize (skip encoder if repo already had quantized version)
        print(f"\nQuantizing models...")
        enc_quant = tmp_dir / "encoder_model_quantized.onnx"
        dec_quant = tmp_dir / "decoder_with_past_quantized.onnx"

        if enc_already_quantized:
            shutil.copy(enc_path, enc_quant)
            print(f"  Encoder already quantized, copying as-is")
        else:
            quantize_model(enc_path, enc_quant)
        quantize_model(dec_path, dec_quant)

        # Step 4: Extract cross-attention weights from PyTorch model.
        # ONNX exports use opaque initializer names, so we pull from PyTorch.
        cross_attn_path = tmp_dir / "cross_attn_kv_weights.bin"
        extract_cross_attn_weights(MODEL_ID, cross_attn_path)

        # Step 5: Verify decoder has correct inputs
        if not verify_decoder(dec_quant):
            raise RuntimeError("Decoder verification failed, aborting")

        # Step 6: Copy to output directory
        print(f"\nCopying to {out}:")
        file_map = [
            (enc_quant, versioned("encoder_model_quantized.onnx")),
            (dec_quant, versioned("decoder_with_past_quantized.onnx")),
            (cross_attn_path, versioned("cross_attn_kv_weights.bin")),
        ]
        for src, dst_name in file_map:
            shutil.copy(src, out / dst_name)
            mb = src.stat().st_size / (1024 * 1024)
            print(f"  -> {dst_name} ({mb:.1f}MB, md5:{md5_file(src)})")

        # Tokenizer
        tok_src = tmp_dir / "tokenizer.json"
        if tok_src.exists():
            shutil.copy(tok_src, out / versioned("t5_tokenizer.json"))
            print(f"  -> {versioned('t5_tokenizer.json')}")

        for fname in ["tokenizer_config.json", "special_tokens_map.json"]:
            src = tmp_dir / fname
            if src.exists():
                dst = versioned(fname) if args.version else fname
                shutil.copy(src, out / dst)
                print(f"  -> {dst}")

    # Summary
    print(f"\nDone. Files in {out}:")
    for f in sorted(out.iterdir()):
        if f.is_file():
            kb = f.stat().st_size / 1024
            label = f"{kb/1024:.1f}MB" if kb > 1024 else f"{kb:.0f}KB"
            print(f"  {f.name} ({label})")

    # Verify if requested or if we have checksums for this version
    if args.verify and args.version:
        ok = verify_checksums(out, args.version, versioned)
        if not ok:
            print("\nWARNING: checksum mismatch. Output differs from known-good build.")
            print("This usually means a dependency version changed.")
            print("Install pinned versions: pip install -r scripts/requirements-grammar.txt")
            sys.exit(1)


if __name__ == "__main__":
    main()
