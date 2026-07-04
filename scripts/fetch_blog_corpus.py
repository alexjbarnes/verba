#!/usr/bin/env python3
"""Fetch a Jekyll blog into a plain-text corpus for grammar fine-tuning.

Downloads every post linked from the index page, extracts the post body
(post-content div), strips code blocks and markup, and writes one .txt per
post. The corpus feeds make_grammar_pairs.py.

Usage:
    fetch_blog_corpus.py https://geohot.github.io/blog/ OUT_DIR [--exclude SLUG]
"""

import argparse
import html
import os
import re
import sys
import time
import urllib.request
from html.parser import HTMLParser


class PostText(HTMLParser):
    """Collect text inside the post-content div, skipping pre/code."""

    def __init__(self):
        super().__init__()
        self.depth = 0
        self.skip = 0
        self.parts = []

    def handle_starttag(self, tag, attrs):
        cls = dict(attrs).get("class", "")
        if self.depth == 0 and tag in ("div", "article") and "post-content" in cls:
            self.depth = 1
            return
        if self.depth:
            if tag in ("pre", "code", "script", "style", "figure"):
                self.skip += 1
            elif tag in ("div", "article"):
                self.depth += 1
            elif tag == "p" or tag == "br":
                self.parts.append("\n\n")

    def handle_endtag(self, tag):
        if not self.depth:
            return
        if tag in ("pre", "code", "script", "style", "figure"):
            self.skip = max(0, self.skip - 1)
        elif tag in ("div", "article"):
            self.depth -= 1

    def handle_data(self, data):
        if self.depth and not self.skip:
            self.parts.append(data)


def fetch(url):
    req = urllib.request.Request(url, headers={"User-Agent": "verba-corpus/1.0"})
    with urllib.request.urlopen(req, timeout=30) as r:
        return r.read().decode("utf-8", "replace")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("index_url")
    ap.add_argument("out_dir")
    ap.add_argument("--exclude", action="append", default=[],
                    help="skip post URLs containing this substring (held-out eval posts)")
    args = ap.parse_args()

    os.makedirs(args.out_dir, exist_ok=True)
    base = re.match(r"https?://[^/]+", args.index_url).group(0)
    index = fetch(args.index_url)
    links = sorted(set(re.findall(r'href="(/blog/[^"]+\.html)"', index)))
    links = [l for l in links if not any(x in l for x in args.exclude)]
    print(f"{len(links)} posts")

    for i, link in enumerate(links):
        slug = re.sub(r"[^a-z0-9-]+", "-", link.lower()).strip("-")
        out = os.path.join(args.out_dir, f"{slug}.txt")
        if os.path.exists(out):
            continue
        try:
            page = fetch(base + link)
        except Exception as e:
            print(f"  FAIL {link}: {e}", file=sys.stderr)
            continue
        p = PostText()
        p.feed(page)
        text = html.unescape("".join(p.parts))
        text = re.sub(r"[ \t]+", " ", text)
        text = re.sub(r"\n{3,}", "\n\n", text).strip()
        if len(text) < 200:
            continue
        open(out, "w").write(text + "\n")
        if (i + 1) % 25 == 0:
            print(f"  {i + 1}/{len(links)}")
        time.sleep(0.2)
    print("done")


if __name__ == "__main__":
    main()
