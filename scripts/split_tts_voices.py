#!/usr/bin/env python3
"""Split a TTS voices.bin into individual per-speaker .bin files.

Usage:
    python scripts/split_tts_voices.py voices.bin -n 103 -o output_dir/
    python scripts/split_tts_voices.py voices.bin -n 11 --names af,af_bella,af_nicole,...

If --names is given, files are named accordingly. Otherwise: speaker_0.bin, speaker_1.bin, etc.
"""

import argparse
import sys
from pathlib import Path


def main():
    parser = argparse.ArgumentParser(description="Split voices.bin into individual voice files")
    parser.add_argument("voices_bin", type=Path, help="Path to voices.bin")
    parser.add_argument("-n", "--num-speakers", type=int, required=True, help="Number of speakers in the file")
    parser.add_argument("-o", "--output", type=Path, default=Path("voices"), help="Output directory")
    parser.add_argument("--names", type=str, help="Comma-separated voice names (must match --num-speakers)")
    args = parser.parse_args()

    data = args.voices_bin.read_bytes()
    n = args.num_speakers
    per_voice = len(data) // n

    if per_voice * n != len(data):
        print(f"Error: {len(data)} bytes not evenly divisible by {n} speakers", file=sys.stderr)
        print(f"  Remainder: {len(data) - per_voice * n} bytes", file=sys.stderr)
        sys.exit(1)

    names = None
    if args.names:
        names = [s.strip() for s in args.names.split(",")]
        if len(names) != n:
            print(f"Error: {len(names)} names given but {n} speakers expected", file=sys.stderr)
            sys.exit(1)

    args.output.mkdir(parents=True, exist_ok=True)

    for i in range(n):
        name = names[i] if names else f"speaker_{i}"
        chunk = data[i * per_voice : (i + 1) * per_voice]
        out = args.output / f"{name}.bin"
        out.write_bytes(chunk)

    print(f"Split {len(data)} bytes into {n} files ({per_voice} bytes each) in {args.output}/")
    print(f"  {per_voice // 4} floats per voice ({per_voice} bytes)")


if __name__ == "__main__":
    main()
