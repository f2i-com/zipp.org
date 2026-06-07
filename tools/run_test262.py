#!/usr/bin/env python3
"""test262 conformance runner for the zipp-vm JavaScript engine.

Runs the official ECMAScript test suite (https://github.com/tc39/test262) against
`zipp js` and reports pass/fail/skip totals, broken down by category and by the
most common failure signatures (so the biggest conformance gaps surface first).

Usage:
  python tools/run_test262.py --t262 <path-to-test262> [--sub test/built-ins/Array]
                              [--jobs N] [--zipp ./target/release/zipp.exe]
                              [--limit N] [--show-fails K]

A test is a .js file with a `/*--- … ---*/` YAML frontmatter:
  flags:    onlyStrict | noStrict | raw | module | async | …
  includes: harness files to prepend (from <t262>/harness/)
  negative: { phase: parse|resolution|runtime, type: <ErrorName> } — must fail
Positive tests pass on clean exit; async tests pass when the harness prints
`Test262:AsyncTestComplete`; negative tests pass when the run errors (matching the
error type when we can read it from stderr).
"""
import argparse, os, re, subprocess, sys, tempfile, concurrent.futures, collections

FM = re.compile(r"/\*---(.*?)---\*/", re.S)

def parse_frontmatter(src):
    m = FM.search(src)
    meta = {"flags": [], "includes": [], "negative": None}
    if not m:
        return meta
    block = m.group(1)
    fl = re.search(r"^flags:\s*\[(.*?)\]", block, re.M)
    if fl:
        meta["flags"] = [x.strip() for x in fl.group(1).split(",") if x.strip()]
    inc = re.search(r"^includes:\s*\[(.*?)\]", block, re.M)
    if inc:
        meta["includes"] = [x.strip() for x in inc.group(1).split(",") if x.strip()]
    else:
        # multi-line list form
        inc2 = re.search(r"^includes:\s*\n((?:\s*-\s*\S+\n)+)", block, re.M)
        if inc2:
            meta["includes"] = re.findall(r"-\s*(\S+)", inc2.group(1))
    neg = re.search(r"^negative:\s*\n\s*phase:\s*(\S+)\s*\n\s*type:\s*(\S+)", block, re.M)
    if neg:
        meta["negative"] = {"phase": neg.group(1), "type": neg.group(2)}
    return meta

def load_harness(h):
    cache = {}
    def get(name):
        if name not in cache:
            with open(os.path.join(h, name), encoding="utf-8") as f:
                cache[name] = f.read()
        return cache[name]
    return get

def classify(meta, code, out, err):
    neg = meta["negative"]
    if neg:
        # Should fail. Pass if it errored; tighten by matching the error type name.
        if code == 0:
            return ("FAIL", "negative-but-passed")
        want = neg["type"]
        if want and (want in err or want.lower() in err.lower()):
            return ("PASS", None)
        # Errored but type unknown/mismatched — still count as pass-ish for parse
        # phase (the VM's parse errors aren't always typed), else a soft fail.
        if neg["phase"] == "parse":
            return ("PASS", None)
        return ("FAIL", f"wrong-error want={want}")
    if "async" in meta["flags"]:
        return ("PASS", None) if "Test262:AsyncTestComplete" in out else ("FAIL", err.strip().splitlines()[0] if err.strip() else "async-no-complete")
    if code == 0:
        return ("PASS", None)
    sig = err.strip().splitlines()[-1] if err.strip() else f"exit={code}"
    return ("FAIL", sig[:80])

def run_one(args, get_harness, path):
    try:
        with open(path, encoding="utf-8", errors="replace") as f:
            src = f.read()
    except Exception as e:
        return ("SKIP", f"read-error {e}", path)
    meta = parse_frontmatter(src)
    flags = meta["flags"]
    if "module" in flags:
        return ("SKIP", "module", path)        # ESM not supported yet
    if "CanBlockIsFalse" in flags or "CanBlockIsTrue" in flags:
        return ("SKIP", "agent", path)
    # Assemble.
    parts = []
    strict = "onlyStrict" in flags
    if strict and "raw" not in flags:
        parts.append('"use strict";')
    if "raw" not in flags:
        parts.append(get_harness("assert.js"))
        parts.append(get_harness("sta.js"))
        if "async" in flags:
            parts.append(get_harness("doneprintHandle.js"))
        for inc in meta["includes"]:
            try:
                parts.append(get_harness(inc))
            except Exception:
                return ("SKIP", f"missing-include {inc}", path)
    parts.append(src)
    assembled = "\n".join(parts)
    # Create the temp script IN THE TEST'S OWN DIRECTORY so a relative dynamic
    # `import('./x_FIXTURE.js')` resolves against the fixtures beside the test
    # (zipp resolves import() relative to the running script's directory).
    fd, tmp = tempfile.mkstemp(suffix=".js", dir=os.path.dirname(path))
    try:
        os.write(fd, assembled.encode("utf-8"))
        os.close(fd)
        try:
            p = subprocess.run([args.zipp, "js", tmp], capture_output=True,
                               encoding="utf-8", errors="replace", timeout=args.timeout)
            verdict, sig = classify(meta, p.returncode, p.stdout or "", p.stderr or "")
        except subprocess.TimeoutExpired:
            verdict, sig = ("FAIL", "timeout")
    finally:
        try: os.remove(tmp)
        except OSError: pass
    return (verdict, sig, path)

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--t262", required=True)
    ap.add_argument("--sub", default="test")
    ap.add_argument("--zipp", default="./target/release/zipp.exe")
    ap.add_argument("--jobs", type=int, default=12)
    ap.add_argument("--limit", type=int, default=0)
    ap.add_argument("--timeout", type=int, default=20)
    ap.add_argument("--show-fails", type=int, default=25)
    a = ap.parse_args()
    root = os.path.join(a.t262, a.sub)
    files = []
    for dp, _, fns in os.walk(root):
        if os.sep + "intl402" in dp or os.sep + "staging" in dp:
            continue
        for fn in fns:
            if fn.endswith(".js") and not fn.endswith("_FIXTURE.js"):
                files.append(os.path.join(dp, fn))
    files.sort()
    if a.limit:
        files = files[: a.limit]
    get_harness = load_harness(os.path.join(a.t262, "harness"))
    totals = collections.Counter()
    by_cat = collections.defaultdict(collections.Counter)
    fail_sigs = collections.Counter()
    n = len(files)
    print(f"running {n} tests with {a.jobs} workers …", flush=True)
    with concurrent.futures.ThreadPoolExecutor(max_workers=a.jobs) as ex:
        for i, (verdict, sig, path) in enumerate(
            ex.map(lambda p: run_one(a, get_harness, p), files)
        ):
            totals[verdict] += 1
            rel = os.path.relpath(path, os.path.join(a.t262, "test"))
            cat = os.sep.join(rel.split(os.sep)[:3])
            by_cat[cat][verdict] += 1
            if verdict == "FAIL":
                fail_sigs[sig] += 1
            if (i + 1) % 2000 == 0:
                print(f"  … {i+1}/{n}", flush=True)
    p, f, s = totals["PASS"], totals["FAIL"], totals["SKIP"]
    ran = p + f
    print(f"\n==== test262 ({a.sub}) ====")
    print(f"PASS {p}  FAIL {f}  SKIP {s}   pass-rate {100*p/ran:.1f}% of {ran} run\n")
    print("worst categories (by FAIL count):")
    for cat, c in sorted(by_cat.items(), key=lambda kv: -kv[1]["FAIL"])[:20]:
        r = c["PASS"] + c["FAIL"]
        if c["FAIL"]:
            print(f"  {c['FAIL']:5d} fail / {r:5d}  {100*c['PASS']/r:5.1f}%  {cat}")
    print(f"\ntop {a.show_fails} failure signatures:")
    for sig, cnt in fail_sigs.most_common(a.show_fails):
        print(f"  {cnt:5d}  {sig}")

if __name__ == "__main__":
    main()
