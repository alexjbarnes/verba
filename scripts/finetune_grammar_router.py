#!/usr/bin/env python3
"""Fine-tune the grammar router as a needs-repair classifier.

Starts from the shipped router's base (ELECTRA-small CoLA) and retrains it
on make_grammar_pairs.py output: label 1 = acceptable as-is, 0 = needs
repair. Label order matches the Rust side (logits index 1 = acceptable,
p_acceptable = softmax[1]), so the exported model drops straight in.

Usage:
    finetune_grammar_router.py PAIRS.train.jsonl PAIRS.val.jsonl OUT_DIR
        [--base pszemraj/electra-small-discriminator-CoLA]
        [--epochs 2] [--batch 32] [--lr 3e-5] [--max-len 64]
"""

import argparse
import json
import os
import random

import numpy as np
import torch
from torch.utils.data import DataLoader, Dataset
from transformers import AutoModelForSequenceClassification, AutoTokenizer


class Pairs(Dataset):
    def __init__(self, path):
        self.rows = [json.loads(l) for l in open(path)]

    def __len__(self):
        return len(self.rows)

    def __getitem__(self, i):
        r = self.rows[i]
        return r["src"], r["label"]


def collate(tok, max_len):
    def fn(batch):
        texts = [b[0] for b in batch]
        labels = torch.tensor([b[1] for b in batch])
        enc = tok(texts, padding=True, truncation=True, max_length=max_len,
                  return_tensors="pt")
        return enc, labels
    return fn


@torch.no_grad()
def evaluate(model, loader, threshold):
    model.eval()
    correct = total = 0
    routed_bad = n_bad = routed_ok = n_ok = 0
    for enc, labels in loader:
        logits = model(**enc).logits
        p_ok = torch.softmax(logits, dim=-1)[:, 1]
        pred = (p_ok >= 0.5).long()
        correct += (pred == labels).sum().item()
        total += len(labels)
        bad = labels == 0
        routed_bad += (p_ok[bad] < threshold).sum().item()
        n_bad += bad.sum().item()
        routed_ok += (p_ok[~bad] < threshold).sum().item()
        n_ok += (~bad).sum().item()
    return (correct / total,
            routed_bad / max(1, n_bad),
            routed_ok / max(1, n_ok))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("train")
    ap.add_argument("val")
    ap.add_argument("out_dir")
    ap.add_argument("--base", default="pszemraj/electra-small-discriminator-CoLA")
    ap.add_argument("--epochs", type=int, default=2)
    ap.add_argument("--batch", type=int, default=32)
    ap.add_argument("--lr", type=float, default=3e-5)
    ap.add_argument("--max-len", type=int, default=64)
    ap.add_argument("--threshold", type=float, default=0.5,
                    help="routing threshold used for the reported metrics")
    ap.add_argument("--seed", type=int, default=7)
    args = ap.parse_args()

    torch.manual_seed(args.seed)
    random.seed(args.seed)
    np.random.seed(args.seed)
    torch.set_num_threads(os.cpu_count() or 4)

    tok = AutoTokenizer.from_pretrained(args.base)
    model = AutoModelForSequenceClassification.from_pretrained(args.base)

    train_ds, val_ds = Pairs(args.train), Pairs(args.val)
    train_dl = DataLoader(train_ds, batch_size=args.batch, shuffle=True,
                          collate_fn=collate(tok, args.max_len))
    val_dl = DataLoader(val_ds, batch_size=args.batch,
                        collate_fn=collate(tok, args.max_len))

    acc, recall, collateral = evaluate(model, val_dl, args.threshold)
    print(f"before: val acc {acc:.3f}, corrupted routed {recall:.0%}, "
          f"clean routed {collateral:.0%} @ {args.threshold}")

    opt = torch.optim.AdamW(model.parameters(), lr=args.lr)
    steps = len(train_dl) * args.epochs
    sched = torch.optim.lr_scheduler.LambdaLR(
        opt, lambda s: min(1.0, s / max(1, steps // 10)) * max(0.0, 1 - s / steps))
    loss_fn = torch.nn.CrossEntropyLoss()

    step = 0
    for epoch in range(args.epochs):
        model.train()
        for enc, labels in train_dl:
            opt.zero_grad()
            loss = loss_fn(model(**enc).logits, labels)
            loss.backward()
            opt.step()
            sched.step()
            step += 1
            if step % 50 == 0:
                print(f"  step {step}/{steps} loss {loss.item():.4f}")
        acc, recall, collateral = evaluate(model, val_dl, args.threshold)
        print(f"epoch {epoch + 1}: val acc {acc:.3f}, corrupted routed {recall:.0%}, "
              f"clean routed {collateral:.0%} @ {args.threshold}")

    model.save_pretrained(args.out_dir)
    tok.save_pretrained(args.out_dir)
    print(f"saved {args.out_dir}")


if __name__ == "__main__":
    main()
