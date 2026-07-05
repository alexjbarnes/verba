#!/usr/bin/env python3
"""Create a custom Kokoro TTS voice from reference audio.

Optimizes a voice tensor to match a target speaker by iteratively mutating
and scoring against speaker similarity. Based on the KVoiceWalk approach.

Each step: mutate voice tensor with scaled noise -> synthesize speech ->
compare speaker embedding to reference -> keep if closer match.

Requirements:
    pip install torch kokoro resemblyzer soundfile numpy

Usage:
    python scripts/create_kokoro_voice.py reference.wav -o voices/my_voice.bin
    python scripts/create_kokoro_voice.py reference.wav -o voices/my_voice.bin --steps 5000 --start af_bella
    python scripts/create_kokoro_voice.py reference.wav -o voices/my_voice.bin --device cuda --steps 3000

The --voices-dir flag points to a directory of .pt voice files used to compute
mutation statistics. Defaults to ../kvoicewalk/voices/ if it exists.
"""

import argparse
import random
import sys
import time
import warnings
from pathlib import Path

import numpy as np
import soundfile as sf
import torch

warnings.filterwarnings("ignore", category=UserWarning)
warnings.filterwarnings("ignore", category=FutureWarning)

EVAL_TEXT = (
    "The quick brown fox jumped over the lazy dog. "
    "She sells sea shells by the sea shore on a warm summer afternoon."
)

SELF_SIM_TEXT = (
    "If you mix vinegar, baking soda, and a bit of dish soap in a tall cylinder, "
    "the resulting eruption is both a visual and tactile delight."
)


def load_voices(voices_dir):
    voices = {}
    for pt in sorted(Path(voices_dir).glob("*.pt")):
        voices[pt.stem] = torch.load(pt, map_location="cpu", weights_only=True)
    return voices


def synthesize(pipeline, text, voice_tensor):
    voice_arg = voice_tensor.detach().to(device="cpu", dtype=torch.float32)
    chunks = list(pipeline(text, voice=voice_arg, speed=1.0))
    if not chunks:
        return None
    return np.concatenate([c for _, _, c in chunks])


def embed(encoder, audio, sr=24000):
    from resemblyzer import preprocess_wav
    wav = preprocess_wav(audio, source_sr=sr)
    return encoder.embed_utterance(wav)


def score(pipeline, encoder, voice, target_embed, best_target_sim):
    audio = synthesize(pipeline, EVAL_TEXT, voice)
    if audio is None or len(audio) < 24000:
        return 0.0, 0.0, None

    cand_embed = embed(encoder, audio)
    target_sim = float(np.inner(cand_embed, target_embed))

    if target_sim < best_target_sim * 0.98:
        return target_sim, 0.0, None

    audio2 = synthesize(pipeline, SELF_SIM_TEXT, voice)
    if audio2 is None or len(audio2) < 24000:
        return target_sim, target_sim * 100, audio

    self_embed = embed(encoder, audio2)
    self_sim = float(np.inner(cand_embed, self_embed))

    w = [0.48, 0.50]
    v = [max(target_sim, 1e-6), max(self_sim, 1e-6)]
    combined = sum(w) / sum(wi / vi for wi, vi in zip(w, v)) * 100
    return target_sim, combined, audio


def optimize(args):
    from kokoro import KPipeline
    from resemblyzer import VoiceEncoder

    print(f"Loading voices from {args.voices_dir}")
    voices = load_voices(args.voices_dir)
    if len(voices) < 2:
        print(f"Need at least 2 voice files in {args.voices_dir}", file=sys.stderr)
        sys.exit(1)
    print(f"  {len(voices)} voices loaded")

    stacked = torch.stack(list(voices.values()))
    pop_std = stacked.std(dim=0)

    if args.start and args.start in voices:
        best_voice = voices[args.start].clone()
        print(f"Starting from: {args.start}")
    else:
        best_voice = stacked.mean(dim=0)
        if args.start:
            print(f"Voice '{args.start}' not found, starting from population mean")
        else:
            print("Starting from population mean")

    print("Loading Kokoro pipeline...")
    pipeline = KPipeline(lang_code="a", repo_id="hexgrad/Kokoro-82M", device=args.device)

    print("Loading speaker encoder...")
    encoder = VoiceEncoder(device=args.device)

    print(f"Loading reference: {args.reference}")
    ref_audio, ref_sr = sf.read(str(args.reference), dtype="float32")
    if ref_audio.ndim > 1:
        ref_audio = ref_audio.mean(axis=1)
    target_embed = embed(encoder, ref_audio, ref_sr)

    print("Scoring initial voice...")
    best_target_sim, best_score, best_audio = score(
        pipeline, encoder, best_voice, target_embed, 0.0
    )
    print(f"  target_sim={best_target_sim:.4f}  score={best_score:.2f}")

    args.output.parent.mkdir(parents=True, exist_ok=True)
    t0 = time.time()
    improved = 0

    for step in range(1, args.steps + 1):
        diversity = random.uniform(0.01, 0.15)
        noise = torch.randn_like(best_voice)
        candidate = best_voice + noise * pop_std * diversity

        target_sim, s, audio = score(
            pipeline, encoder, candidate, target_embed, best_target_sim
        )

        if s > best_score:
            best_voice = candidate
            best_score = s
            best_target_sim = max(best_target_sim, target_sim)
            best_audio = audio
            improved += 1
            elapsed = time.time() - t0
            rate = step / elapsed
            print(
                f"  [{step}/{args.steps}] NEW BEST "
                f"target_sim={target_sim:.4f} score={s:.2f} "
                f"diversity={diversity:.3f} ({rate:.1f} steps/s)"
            )

            if improved % 5 == 0:
                save_voice(best_voice, args.output)
                if best_audio is not None:
                    sf.write(str(args.output.with_suffix(".wav")), best_audio, 24000)

        elif step % 200 == 0:
            elapsed = time.time() - t0
            rate = step / elapsed
            print(
                f"  [{step}/{args.steps}] best={best_score:.2f} "
                f"target_sim={best_target_sim:.4f} "
                f"({rate:.1f} steps/s, {improved} improvements)"
            )

    elapsed = time.time() - t0
    print(f"\nDone: {args.steps} steps in {elapsed:.0f}s ({improved} improvements)")
    print(f"  Final score: {best_score:.2f}  target_sim: {best_target_sim:.4f}")

    save_voice(best_voice, args.output)
    pt_path = args.output.with_suffix(".pt")
    torch.save(best_voice, pt_path)
    print(f"  Saved: {args.output} (.bin for sherpa-onnx)")
    print(f"  Saved: {pt_path} (.pt for kokoro python)")

    if best_audio is not None:
        wav_path = args.output.with_suffix(".wav")
        sf.write(str(wav_path), best_audio, 24000)
        print(f"  Saved: {wav_path} (sample audio)")


def save_voice(voice_tensor, output_path):
    squeezed = voice_tensor.squeeze(1).detach().cpu().numpy().astype(np.float32)
    output_path.parent.mkdir(parents=True, exist_ok=True)
    squeezed.tofile(str(output_path))


def main():
    parser = argparse.ArgumentParser(
        description="Create a custom Kokoro voice from reference audio"
    )
    parser.add_argument("reference", type=Path, help="Reference audio (WAV, 10-30s of clean speech)")
    parser.add_argument("-o", "--output", type=Path, default=Path("custom_voice.bin"))
    parser.add_argument("--steps", type=int, default=2000, help="Optimization steps (default: 2000)")
    parser.add_argument("--start", type=str, default=None, help="Starting voice name (e.g. af_bella)")
    parser.add_argument("--device", type=str, default="cpu", help="cpu or cuda")
    parser.add_argument(
        "--voices-dir", type=Path, default=None,
        help="Directory of .pt voice files for population stats"
    )
    args = parser.parse_args()

    if not args.reference.exists():
        print(f"Error: {args.reference} not found", file=sys.stderr)
        sys.exit(1)

    if args.voices_dir is None:
        candidates = [
            Path(__file__).resolve().parent.parent.parent / "kvoicewalk" / "voices",
            Path.home() / ".cache" / "kokoro" / "voices",
        ]
        for c in candidates:
            if c.exists() and list(c.glob("*.pt")):
                args.voices_dir = c
                break
        if args.voices_dir is None:
            print(
                "No voices directory found. Provide --voices-dir pointing to "
                "a folder of Kokoro .pt voice files.\n"
                "You can get them from: https://github.com/RobViren/kvoicewalk/tree/main/voices",
                file=sys.stderr,
            )
            sys.exit(1)

    optimize(args)


if __name__ == "__main__":
    main()
