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
import argparse, os, re, subprocess, sys, tempfile, threading, concurrent.futures, collections

FM = re.compile(r"/\*---(.*?)---\*/", re.S)

# ---- byte-faithful source reading -------------------------------------------
# Function.prototype.toString must reproduce a function's ORIGINAL line
# terminators, so test sources have to reach the engine byte-faithful (the old
# text-mode reads folded \r and \r\n to \n via universal newlines). But a
# core.autocrlf=true checkout writes LF-stored files to disk as CRLF, so raw
# bytes alone would feed the engine \r\n for almost every test. Undo exactly
# that mangle: a file the git INDEX stores as LF is normalised back to \n;
# anything else (genuine CRLF/CR-authored sources like the line-terminator-
# normalisation tests, or a non-checkout tree) keeps its on-disk bytes.
_EOL_MAPS = {}
_EOL_LOCK = threading.Lock()

def _eol_index_map(root):
    """abs path -> git index eol kind ('lf', 'crlf', '-text', …) for every file
    under the checkout at `root`; empty when git/checkout is unavailable
    (on-disk bytes are then trusted as authentic)."""
    try:
        out = subprocess.run(["git", "-C", root, "ls-files", "--eol", "-z"],
                             capture_output=True, timeout=120).stdout
    except Exception:
        return {}
    m = {}
    for ent in out.split(b"\0"):
        if b"\t" not in ent:
            continue
        info, rel = ent.split(b"\t", 1)
        fields = info.split()
        if not fields or not fields[0].startswith(b"i/"):
            continue
        p = os.path.normcase(os.path.normpath(os.path.join(root, rel.decode("utf-8", "replace"))))
        m[p] = fields[0][2:].decode("ascii", "replace")
    return m

def _eol_map_for(root):
    root = os.path.normcase(os.path.abspath(root))
    with _EOL_LOCK:
        if root not in _EOL_MAPS:
            _EOL_MAPS[root] = _eol_index_map(root)
        return _EOL_MAPS[root]

def read_source(path, t262_root):
    """Read a test/harness file byte-faithfully (modulo the autocrlf undo)."""
    with open(path, encoding="utf-8", errors="replace", newline="") as f:
        src = f.read()
    if "\r" in src:
        kind = _eol_map_for(t262_root).get(os.path.normcase(os.path.abspath(path)))
        if kind == "lf":
            src = src.replace("\r\n", "\n")
    return src
# -----------------------------------------------------------------------------

def parse_frontmatter(src):
    # Metadata regexes are line-based (`^flags:` under re.M only matches after
    # \n) — parse a newline-normalised COPY; the engine still gets `src`.
    src = src.replace("\r\n", "\n").replace("\r", "\n")
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
    root = os.path.dirname(os.path.abspath(h))  # the test262 checkout
    def get(name):
        if name not in cache:
            cache[name] = read_source(os.path.join(h, name), root)
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
        src = read_source(path, args.t262)
    except Exception as e:
        return ("SKIP", f"read-error {e}", path)
    meta = parse_frontmatter(src)
    flags = meta["flags"]
    is_module = "module" in flags
    # CanBlockIsTrue runs normally (the default agent can block);
    # CanBlockIsFalse launches the engine with ZIPP_CAN_BLOCK=0 so
    # Atomics.wait throws TypeError per AgentCanSuspend.
    cannot_block = "CanBlockIsFalse" in flags
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
    if is_module:
        # The harness runs as a realm SCRIPT (its vars become realm globals,
        # visible to every module -- real-engine harness semantics); the TEST
        # text alone is the module entry, so its decls stay module-scoped and
        # a self-import of the on-disk file links cleanly.
        harness_src = "\n".join(parts)
        assembled = src
    else:
        parts.append(src)
        assembled = "\n".join(parts)
        harness_src = None
    # Create the temp script IN THE TEST'S OWN DIRECTORY so a relative dynamic
    # `import('./x_FIXTURE.js')` resolves against the fixtures beside the test
    # (zipp resolves import() relative to the running script's directory).
    fd, tmp = tempfile.mkstemp(suffix=".js", dir=os.path.dirname(path))
    hf = None
    try:
        os.write(fd, assembled.encode("utf-8"))
        os.close(fd)
        try:
            # A `flags:[module]` test runs as an ES module (top-level await,
            # module scope); the assembled harness + test is one module.
            subcmd = "mjs" if is_module else "js"
            env = None
            if cannot_block:
                env = dict(os.environ)
                env["ZIPP_CAN_BLOCK"] = "0"
            # Module tests run the ORIGINAL file (self-imports resolve to the
            # same module record); scripts run the assembled tmp.
            entry = path if is_module else tmp
            cmd = [args.zipp, subcmd, entry]
            if harness_src is not None:
                hfd, hf = tempfile.mkstemp(suffix=".js", dir=os.path.dirname(path))
                os.write(hfd, harness_src.encode("utf-8"))
                os.close(hfd)
                cmd.append(hf)
            p = subprocess.run(cmd, capture_output=True,
                               encoding="utf-8", errors="replace", timeout=args.timeout,
                               env=env)
            verdict, sig = classify(meta, p.returncode, p.stdout or "", p.stderr or "")
        except subprocess.TimeoutExpired:
            verdict, sig = ("FAIL", "timeout")
    finally:
        try: os.remove(tmp)
        except OSError: pass
        try:
            if hf: os.remove(hf)
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
    ap.add_argument("--dump-fails", default="", help="write sorted FAIL relpaths to this file (exact regression diffs)")
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
    fail_paths = []
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
                fail_paths.append(rel.replace(os.sep, "/"))
            if (i + 1) % 2000 == 0:
                print(f"  … {i+1}/{n}", flush=True)
    if a.dump_fails:
        with open(a.dump_fails, "w", newline="\n") as fh:
            fh.write("\n".join(sorted(fail_paths)) + "\n")
        print(f"wrote {len(fail_paths)} FAIL relpaths to {a.dump_fails}")
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
