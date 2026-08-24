#!/usr/bin/env python3
"""Rewrite `foo.rs:N` doc anchors after tools/split_rs.py split `foo.rs`.

PERF_ROADMAP.md's main asset is its file:line precision. Splitting a monolith
into `foo/` invalidates every anchor into it, so this translates each one to the
submodule and line it now lives at.

Method: read the PRE-SPLIT file out of git, take the exact text of line N plus a
context window, and locate that window in the emitted submodules. A unique
window match is accepted; anything ambiguous or missing is reported and left
untouched, so the tool can never silently point an anchor at the wrong code.

Usage:
  python tools/remap_anchors.py --doc PERF_ROADMAP.md --rev HEAD \
      --split crates/zipp-vm/src/codegen.rs [--split ...] [--apply]
"""
import argparse
import os
import re
import subprocess
import sys
import tempfile


def git_show(rev, path, root):
    if rev.startswith("-"):
        raise ValueError("revision must not begin with '-'")
    out = subprocess.run(
        ["git", "show", f"{rev}:{path}"],
        capture_output=True,
        check=True,
        cwd=root,
        timeout=30,
    ).stdout
    return out.decode("utf-8", "replace").splitlines()


def load_modules(outdir):
    mods = {}
    for fn in sorted(os.listdir(outdir)):
        if fn.endswith(".rs"):
            p = os.path.join(outdir, fn)
            with open(p, encoding="utf-8") as f:
                mods[fn] = f.read().splitlines()
    return mods


_VIS_RE = re.compile(r"^(\s*)pub(\([a-z]+\))? ")


def _norm(line):
    """split_rs.py widens moved items to `pub(crate)`, so the emitted text no
    longer matches the pre-split text verbatim. Compare with any visibility
    prefix stripped."""
    return _VIS_RE.sub(r"\1", line)


def locate(mods, window, want_idx):
    """Find `window` (list of lines) in the modules. Returns
    (module, line_no_of_window[want_idx]) when the match is unique."""
    window = [_norm(l) for l in window]
    hits = []
    n = len(window)
    for name, lines in mods.items():
        norm = [_norm(l) for l in lines]
        for i in range(len(norm) - n + 1):
            if norm[i : i + n] == window:
                hits.append((name, i + want_idx + 1))
                if len(hits) > 1:
                    return None
    return hits[0] if len(hits) == 1 else None


def remap_one(doc_text, src_path, rev, root):
    stem = os.path.basename(src_path)[:-3]          # codegen
    outdir = os.path.join(root, src_path[:-3])      # crates/.../codegen
    if not os.path.isdir(outdir):
        print(f"  ! {outdir} does not exist — skipping {stem}")
        return doc_text, 0, 0
    orig = git_show(rev, src_path.replace(os.sep, "/"), root)
    mods = load_modules(outdir)
    pat = re.compile(re.escape(stem) + r"\.rs:(\d+)")

    resolved, failed = 0, []

    def sub(m):
        nonlocal resolved
        n = int(m.group(1))
        if not (1 <= n <= len(orig)):
            failed.append((n, "out of range"))
            return m.group(0)
        # widen the window until the match is unique (anchors often point at
        # short lines like `}` that repeat hundreds of times)
        for pre, post in ((0, 0), (2, 2), (5, 5), (12, 12), (30, 30)):
            lo, hi = max(0, n - 1 - pre), min(len(orig), n + post)
            got = locate(mods, orig[lo:hi], (n - 1) - lo)
            if got:
                resolved += 1
                return f"{stem}/{got[0]}:{got[1]}"
        failed.append((n, "ambiguous/not found"))
        return m.group(0)

    new_text = pat.sub(sub, doc_text)
    for n, why in failed:
        print(f"  ! {stem}.rs:{n} unresolved ({why}) — left as-is")
    return new_text, resolved, len(failed)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--doc", required=True)
    ap.add_argument("--rev", default="HEAD")
    ap.add_argument("--split", action="append", required=True,
                    help="pre-split source path, e.g. crates/zipp-vm/src/codegen.rs")
    ap.add_argument("--root", default=".")
    ap.add_argument("--apply", action="store_true")
    a = ap.parse_args()

    root = os.path.realpath(a.root)

    def within_root(value, label):
        path = os.path.realpath(os.path.join(root, value))
        try:
            inside = os.path.commonpath((root, path)) == root
        except ValueError:  # different drives on Windows
            inside = False
        if not inside:
            ap.error(f"{label} must stay within --root: {value}")
        return path

    doc_path = within_root(a.doc, "--doc")
    splits = []
    for src in a.split:
        path = within_root(src, "--split")
        splits.append(os.path.relpath(path, root))
    with open(doc_path, encoding="utf-8", newline="") as f:
        text = f.read()

    total_ok = total_bad = 0
    for src in splits:
        print(f"{src}:")
        text, ok, bad = remap_one(text, src, a.rev, root)
        print(f"  resolved {ok}, unresolved {bad}")
        total_ok += ok
        total_bad += bad

    if a.apply:
        # Stage beside the document and atomically replace it. A failure can no
        # longer truncate the only copy of a long roadmap/handoff document.
        fd, staged = tempfile.mkstemp(prefix=".remap-anchors-", dir=os.path.dirname(doc_path))
        try:
            with os.fdopen(fd, "w", encoding="utf-8", newline="") as f:
                f.write(text)
                f.flush()
                os.fsync(f.fileno())
            os.replace(staged, doc_path)
        finally:
            try:
                os.remove(staged)
            except FileNotFoundError:
                pass
        print(f"\napplied to {a.doc}: {total_ok} anchors rewritten, {total_bad} left")
    else:
        print(f"\n[dry-run] would rewrite {total_ok}, leave {total_bad}"
              f" — pass --apply to write")


if __name__ == "__main__":
    main()
