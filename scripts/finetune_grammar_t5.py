#!/usr/bin/env python3
"""Fine-tune the T5 grammar corrector on corruption pairs.

Starts from the shipped corrector (visheratin/t5-efficient-tiny-grammar-
correction, "grammar: " task prefix — must match config.0.0.1.json
input_prefix) and trains on make_grammar_pairs.py output. Identity pairs
(src == tgt) teach it to copy text that needs nothing, targeting its
measured failure mode of inserting the/of/to on fine text.

Usage:
    finetune_grammar_t5.py PAIRS.train.jsonl PAIRS.val.jsonl OUT_DIR
        [--base visheratin/t5-efficient-tiny-grammar-correction]
        [--epochs 2] [--batch 16] [--lr 3e-4] [--max-len 96]
"""

import argparse
import json
import os
import random

import numpy as np
import torch
from torch.utils.data import DataLoader, Dataset
from transformers import AutoTokenizer, T5ForConditionalGeneration

PREFIX = "grammar: "


class Pairs(Dataset):
    def __init__(self, path):
        self.rows = [json.loads(l) for l in open(path)]

    def __len__(self):
        return len(self.rows)

    def __getitem__(self, i):
        r = self.rows[i]
        return PREFIX + r["src"], r["tgt"]


def collate(tok, max_len):
    def fn(batch):
        srcs = [b[0] for b in batch]
        tgts = [b[1] for b in batch]
        enc = tok(srcs, padding=True, truncation=True, max_length=max_len,
                  return_tensors="pt")
        lab = tok(text_target=tgts, padding=True, truncation=True,
                  max_length=max_len, return_tensors="pt").input_ids
        lab[lab == tok.pad_token_id] = -100
        enc["labels"] = lab
        return enc
    return fn


@torch.no_grad()
def val_loss(model, loader):
    model.eval()
    total = n = 0
    for enc in loader:
        total += model(**enc).loss.item()
        n += 1
    return total / max(1, n)


@torch.no_grad()
def show_samples(model, tok, rows, max_len):
    model.eval()
    for r in rows:
        enc = tok(PREFIX + r["src"], return_tensors="pt",
                  truncation=True, max_length=max_len)
        out = model.generate(**enc, max_new_tokens=max_len)
        got = tok.decode(out[0], skip_special_tokens=True)
        mark = "=" if got == r["tgt"] else ("~" if got == r["src"] else "!")
        print(f"  [{mark}] {r['src']!r} -> {got!r} (want {r['tgt']!r})")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("train")
    ap.add_argument("val")
    ap.add_argument("out_dir")
    ap.add_argument("--base", default="visheratin/t5-efficient-tiny-grammar-correction")
    ap.add_argument("--epochs", type=int, default=2)
    ap.add_argument("--batch", type=int, default=16)
    ap.add_argument("--lr", type=float, default=3e-4)
    ap.add_argument("--max-len", type=int, default=96)
    ap.add_argument("--seed", type=int, default=7)
    args = ap.parse_args()

    torch.manual_seed(args.seed)
    random.seed(args.seed)
    np.random.seed(args.seed)
    torch.set_num_threads(os.cpu_count() or 4)

    tok = AutoTokenizer.from_pretrained(args.base)
    model = T5ForConditionalGeneration.from_pretrained(args.base)

    train_ds, val_ds = Pairs(args.train), Pairs(args.val)
    train_dl = DataLoader(train_ds, batch_size=args.batch, shuffle=True,
                          collate_fn=collate(tok, args.max_len))
    val_dl = DataLoader(val_ds, batch_size=args.batch,
                        collate_fn=collate(tok, args.max_len))
    sample_rows = [json.loads(l) for l in open(args.val)][:8]

    print(f"before: val loss {val_loss(model, val_dl):.4f}")
    show_samples(model, tok, sample_rows, args.max_len)

    opt = torch.optim.AdamW(model.parameters(), lr=args.lr)
    steps = len(train_dl) * args.epochs
    sched = torch.optim.lr_scheduler.LambdaLR(
        opt, lambda s: min(1.0, s / max(1, steps // 10)) * max(0.0, 1 - s / steps))

    step = 0
    for epoch in range(args.epochs):
        model.train()
        for enc in train_dl:
            opt.zero_grad()
            loss = model(**enc).loss
            loss.backward()
            opt.step()
            sched.step()
            step += 1
            if step % 50 == 0:
                print(f"  step {step}/{steps} loss {loss.item():.4f}")
        print(f"epoch {epoch + 1}: val loss {val_loss(model, val_dl):.4f}")
        show_samples(model, tok, sample_rows, args.max_len)

    model.save_pretrained(args.out_dir)
    tok.save_pretrained(args.out_dir)
    print(f"saved {args.out_dir}")


if __name__ == "__main__":
    main()
