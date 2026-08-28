#!/usr/bin/env python3
"""Resolve `ZIPP_PROF_PC_DUMP` samples to function names via the linker map.

The `ZIPP_PROF_PC=1` profiler (see `vm/prof.rs`) answers "which emitted body"
directly, because the JIT registers its own buffers. For a sample that lands in
`zipp.exe` it can only report an offset, because dbghelp will not load the
build's private symbols. The MSVC linker map resolves those offsets, and unlike
public-symbol lookup it cannot silently attribute an address to a wrong nearby
symbol -- an offset below the first symbol is reported as unresolved instead.

Build the engine so both exist:

    RUSTFLAGS="-C link-arg=/MAP:zipp_profiling.map" cargo build --profile profiling

Collect samples (the workload is deterministic, so runs may be accumulated --
one run yields only a few hundred samples because Windows timer granularity
holds the sampler near 0.5ms):

    for i in $(seq 30); do
      ZIPP_PROF_PC=1 ZIPP_PROF_PC_DUMP=dump/$i.txt ./target/profiling/zipp.exe js BENCH
    done

    python tools/pcmap.py zipp_profiling.map dump/*.txt
"""
import collections
import glob
import re
import sys

SYM = re.compile(r"^\s+[0-9a-fA-F]{4}:[0-9a-fA-F]{8}\s+(\S+)\s+([0-9a-fA-F]{16})\s")


def load_map(path):
    """`(sorted [(rva, name)], preferred_base)` for the image's code symbols."""
    base = None
    syms = []
    with open(path, "r", errors="replace") as fh:
        for line in fh:
            if base is None and "Preferred load address is" in line:
                base = int(line.strip().split()[-1], 16)
                continue
            m = SYM.match(line)
            if not m or base is None:
                continue
            addr = int(m.group(2), 16)
            if addr < base:            # absolutes and thread-locals
                continue
            syms.append((addr - base, m.group(1)))
    syms.sort()
    # Collapse duplicate RVAs (aliases) keeping the first name seen.
    out = []
    for rva, name in syms:
        if not out or out[-1][0] != rva:
            out.append((rva, name))
    return out, base


def resolve(syms, off):
    lo, hi = 0, len(syms)
    while lo < hi:
        mid = (lo + hi) // 2
        if syms[mid][0] <= off:
            lo = mid + 1
        else:
            hi = mid
    return syms[lo - 1][1] if lo else None


def main(argv):
    if len(argv) < 3:
        print(__doc__)
        return 2
    syms, base = load_map(argv[1])
    if not syms:
        print("no symbols parsed from", argv[1])
        return 1
    paths = []
    for pat in argv[2:]:
        paths.extend(glob.glob(pat) or [pat])

    tally = collections.Counter()
    total = 0
    for path in paths:
        with open(path, "r", errors="replace") as fh:
            for line in fh:
                line = line.strip()
                if not line:
                    continue
                total += 1
                if line.startswith("abs:"):
                    tally["<outside the image: emitted code>"] += 1
                    continue
                name = resolve(syms, int(line, 16))
                tally[name or "<below the first symbol>"] += 1

    print("%d samples from %d dump(s), %d map symbols\n" % (total, len(paths), len(syms)))
    for name, n in tally.most_common(40):
        print("%8d  %5.1f%%  %s" % (n, n * 100.0 / total, name))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
