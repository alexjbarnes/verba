#!/usr/bin/env python3
"""Pack Kokoro .pt voice files into a sherpa-onnx voices.bin.

Downloads .pt files from the HuggingFace Kokoro-82M repo and packs them
into a single voices.bin ordered by speaker ID, matching the format that
sherpa-onnx expects.

Usage:
    python scripts/pack_kokoro_voices.py -o voices.bin
    python scripts/pack_kokoro_voices.py --voices-dir /path/to/local/pt/files -o voices.bin
    python scripts/pack_kokoro_voices.py --list
"""

import argparse
import sys
from pathlib import Path

import numpy as np

VOICES = [
    "af_alloy", "af_aoede", "af_bella", "af_heart", "af_jessica",
    "af_kore", "af_nicole", "af_nova", "af_river", "af_sarah", "af_sky",
    "am_adam", "am_echo", "am_eric", "am_fenrir", "am_liam",
    "am_michael", "am_onyx", "am_puck", "am_santa",
    "bf_alice", "bf_emma", "bf_isabella", "bf_lily",
    "bm_daniel", "bm_fable", "bm_george", "bm_lewis",
    "ef_dora", "em_alex",
    "ff_siwis",
    "hf_alpha", "hf_beta", "hm_omega", "hm_psi",
    "if_sara", "im_nicola",
    "jf_alpha", "jf_gongitsune", "jf_nezumi", "jf_tebukuro", "jm_kumo",
    "pf_dora", "pm_alex", "pm_santa",
    "zf_xiaobei", "zf_xiaoni", "zf_xiaoxiao", "zf_xiaoyi",
    "zm_yunjian", "zm_yunxi", "zm_yunxia", "zm_yunyang",
]

HF_REPO = "hexgrad/Kokoro-82M"


def download_voice(name, cache_dir):
    """Download a .pt voice file from HuggingFace if not cached."""
    cached = cache_dir / f"{name}.pt"
    if cached.exists():
        return cached

    try:
        from huggingface_hub import hf_hub_download
        path = hf_hub_download(
            repo_id=HF_REPO,
            filename=f"voices/{name}.pt",
            cache_dir=str(cache_dir / ".hf_cache"),
        )
        import shutil
        shutil.copy2(path, cached)
        return cached
    except ImportError:
        print("Install huggingface_hub: pip install huggingface_hub", file=sys.stderr)
        sys.exit(1)


def load_voice(pt_path):
    """Load a .pt voice file and return as flat float32 numpy array."""
    import torch
    tensor = torch.load(str(pt_path), map_location="cpu", weights_only=True)
    return tensor.squeeze(1).detach().cpu().numpy().astype(np.float32)


def main():
    parser = argparse.ArgumentParser(description="Pack Kokoro .pt voices into voices.bin")
    parser.add_argument("-o", "--output", type=Path, default=Path("voices.bin"))
    parser.add_argument("--voices-dir", type=Path, default=None,
                        help="Directory with .pt files (downloads from HF if not provided)")
    parser.add_argument("--list", action="store_true", help="List voice names and exit")
    parser.add_argument("--filter", type=str, default=None,
                        help="Only include voices matching prefix (e.g. 'af,am,bf,bm' for English)")
    args = parser.parse_args()

    if args.list:
        for i, name in enumerate(VOICES):
            print(f"  {i:3d}  {name}")
        print(f"\n{len(VOICES)} voices total")
        return

    voice_names = VOICES
    if args.filter:
        prefixes = [p.strip() for p in args.filter.split(",")]
        voice_names = [v for v in VOICES if any(v.startswith(p) for p in prefixes)]
        print(f"Filtering to {len(voice_names)} voices matching: {', '.join(prefixes)}")

    if args.voices_dir:
        voices_dir = args.voices_dir
    else:
        voices_dir = Path.home() / ".cache" / "kokoro_voices"
        voices_dir.mkdir(parents=True, exist_ok=True)

    import torch  # noqa: F811 - verify torch is available early

    arrays = []
    for i, name in enumerate(voice_names):
        if args.voices_dir:
            pt_path = voices_dir / f"{name}.pt"
            if not pt_path.exists():
                print(f"Error: {pt_path} not found", file=sys.stderr)
                sys.exit(1)
        else:
            print(f"  [{i+1}/{len(voice_names)}] Downloading {name}...")
            pt_path = download_voice(name, voices_dir)

        arr = load_voice(pt_path)
        arrays.append(arr)
        if i == 0:
            print(f"  Voice shape: {arr.shape} ({arr.nbytes} bytes per voice)")

    packed = np.concatenate(arrays)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    packed.tofile(str(args.output))

    print(f"\nPacked {len(voice_names)} voices into {args.output}")
    print(f"  Total: {packed.nbytes:,} bytes")
    print(f"  Per voice: {arrays[0].nbytes:,} bytes ({arrays[0].shape})")


if __name__ == "__main__":
    main()
