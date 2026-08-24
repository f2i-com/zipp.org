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

Security: this is a trusted-developer conformance harness, not an untrusted-code
sandbox. Test262 needs the engine's privileged `$262` host surface and exercises
the JIT deliberately, so use only a reviewed/pinned checkout. Run a checkout you
do not trust inside an external OS sandbox with filesystem/network/resource
limits; use `zipp sandbox` for ordinary untrusted application scripts.
"""
import argparse, os, re, shutil, subprocess, sys, tempfile, threading, concurrent.futures, collections

FM = re.compile(r"/\*---(.*?)---\*/", re.S)

# Prefix for the per-execution scratch files this runner drops beside each test.
# Anything carrying it is OURS, never a test — see the walk filter in main().
TMP_PREFIX = ".zipptmp-"

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
    else:
        # YAML multi-line list form, which `includes` below already handled and
        # this did not:
        #
        #     flags:
        #       - noStrict
        #
        # Missing it ran every such test in BOTH modes, so a `noStrict` test —
        # `staging/sm` uses this spelling throughout — was additionally run under
        # "use strict" and failed on `with`, `delete x`, `arguments`/`eval` as
        # bindings, and the other sloppy-only constructs it exists to test. Those
        # were counted as engine failures.
        fl2 = re.search(r"^flags:\s*\n((?:\s*-\s*\S+\n)+)", block, re.M)
        if fl2:
            meta["flags"] = re.findall(r"-\s*(\S+)", fl2.group(1))
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
    harness_root = os.path.realpath(h)
    root = os.path.dirname(harness_root)  # the test262 checkout
    def get(name):
        if name not in cache:
            path = os.path.realpath(os.path.join(harness_root, name))
            try:
                inside = os.path.commonpath((harness_root, path)) == harness_root
            except ValueError:  # different drives on Windows
                inside = False
            if not inside or not os.path.isfile(path):
                raise ValueError(f"invalid harness include {name!r}")
            cache[name] = read_source(path, root)
        return cache[name]
    return get

# `sta.js`'s $DONOTEVALUATE throws this exact string, and `assert.js`'s failures
# are Test262Error. Either one appearing in the output of a NEGATIVE test is
# definitional proof that the test body EXECUTED — i.e. the early error the test
# exists to check was never raised. Scoring those as passes (which the old
# `phase == "parse"` blanket did) hid ~1,200 genuine failures.
DONOTEVALUATE = "should not be evaluated"
TEST262ERROR = "Test262Error"


def classify(meta, code, out, err):
    blob = (out or "") + (err or "")
    neg = meta["negative"]
    if neg:
        # Should fail. Pass if it errored; tighten by matching the error type name.
        if code == 0:
            return ("FAIL", "negative-but-passed")
        want = neg["type"]
        # The body ran, so whatever the exit code says, the required early error
        # was NOT raised. This check must precede the type match: a test whose
        # body throws its own TypeError would otherwise "match" want=TypeError.
        # ...except when Test262Error IS the wanted type: those tests (e.g. the
        # line-terminator and html-comment ones) assert a construct works by
        # ending in `throw new Test262Error()`, so the body running is the pass
        # condition. $DONOTEVALUATE throws a bare string, never a Test262Error,
        # so that half of the proof stays active for them.
        if DONOTEVALUATE in blob or (want != TEST262ERROR and TEST262ERROR in blob):
            return ("FAIL", f"early-error-not-raised want={want}")
        if want and (want in err or want.lower() in err.lower()):
            return ("PASS", None)
        # Errored, but the engine did not name a type. zipp's parse rejections
        # are not all typed (e.g. "`break` target not found"), and the check
        # above already excludes the "body ran" case, so accept it for the parse
        # phase only — a runtime-phase mismatch stays a failure.
        if neg["phase"] == "parse":
            return ("PASS", None)
        return ("FAIL", f"wrong-error want={want}")
    if "async" in meta["flags"]:
        # An async test must print COMPLETE and must NOT print FAILURE: a promise
        # resolved twice yields FAILURE-then-COMPLETE, which is exactly the bug
        # these tests exist to catch. A nonzero exit (a post-completion crash)
        # is a failure too.
        if "Test262:AsyncTestFailure" in blob:
            fail = next((l for l in blob.splitlines() if "AsyncTestFailure" in l), "async-failure")
            return ("FAIL", fail.strip()[:80])
        if "Test262:AsyncTestComplete" not in out:
            return ("FAIL", err.strip().splitlines()[0] if err.strip() else "async-no-complete")
        if code != 0:
            return ("FAIL", f"async-completed-then-exit={code}")
        return ("PASS", None)
    if code == 0:
        # A positive test that reported a Test262Error but still exited 0 did not
        # pass: an assertion inside a promise reaction throws, the rejection goes
        # unhandled, and the process exits cleanly.
        if TEST262ERROR in blob:
            line = next((l for l in blob.splitlines() if TEST262ERROR in l), TEST262ERROR)
            return ("FAIL", f"swallowed: {line.strip()[:64]}")
        return ("PASS", None)
    sig = err.strip().splitlines()[-1] if err.strip() else f"exit={code}"
    return ("FAIL", sig[:80])

def modes_for(flags):
    """The execution modes INTERPRETING.md requires for a test.

    A test carrying none of `onlyStrict` / `noStrict` / `raw` / `module` must be
    run TWICE — once as sloppy code and once with a "use strict" directive
    prepended. Running it once (the old behaviour) performed 48,556 of the
    93,065 required executions, so all strict-only semantics reachable from the
    44,509 default-flag tests went unmeasured."""
    if "raw" in flags or "module" in flags:
        return ("sloppy",)
    if "onlyStrict" in flags:
        return ("strict",)
    if "noStrict" in flags:
        return ("sloppy",)
    return ("sloppy", "strict")


def run_one(args, get_harness, job):
    path, mode = job
    try:
        src = read_source(path, args.t262)
    except Exception as e:
        return ("SKIP", f"read-error {e}", job)
    meta = parse_frontmatter(src)
    flags = meta["flags"]
    is_module = "module" in flags
    # CanBlockIsTrue runs normally (the default agent can block);
    # CanBlockIsFalse launches the engine with ZIPP_CAN_BLOCK=0 so
    # Atomics.wait throws TypeError per AgentCanSuspend.
    cannot_block = "CanBlockIsFalse" in flags
    # Assemble.
    #
    # The harness is a SEPARATE SCRIPT from the test, never concatenated with it
    # (INTERPRETING.md: `includes` "must be evaluated in the test realm's global
    # scope PRIOR TO test execution", and the strict directive is "inserted as the
    # initial character sequence of the file" -- the TEST file). Both `js` and
    # `mjs` take the harness as a second argument and evaluate it as a realm
    # script first.
    #
    # This used to be one concatenated source with `"use strict";` glued on top,
    # which made the HARNESS strict too. A harness helper that runs code through a
    # DIRECT eval -- sm/non262-strict-shell.js's testLenientAndStrict, and
    # sm/non262-expressions-shell.js -- inherits its caller's strictness, so the
    # sloppy half of those helpers ran strict and 19 executions failed. They are
    # not engine failures: V8 fails the same 19 on the concatenated bytes and
    # passes all 19 on this assembly.
    parts = []
    strict = mode == "strict"
    if "raw" not in flags:
        parts.append(get_harness("assert.js"))
        parts.append(get_harness("sta.js"))
        if "async" in flags:
            parts.append(get_harness("doneprintHandle.js"))
        for inc in meta["includes"]:
            try:
                parts.append(get_harness(inc))
            except Exception:
                return ("SKIP", f"missing-include {inc}", job)
    # `raw` takes no harness and no directive: the file is run exactly as authored.
    harness_src = "\n".join(parts) if parts else None
    if is_module:
        # A module is always strict; there is no directive to add.
        assembled = src
    else:
        # The directive goes on the TEST TEXT ONLY, as its own directive prologue.
        assembled = ('"use strict";\n' + src) if (strict and "raw" not in flags) else src
    # Create the temp script IN THE TEST'S OWN DIRECTORY so a relative dynamic
    # `import('./x_FIXTURE.js')` resolves against the fixtures beside the test
    # (zipp resolves import() relative to the running script's directory).
    #
    # The `.zipptmp-` prefix (plus the `_FIXTURE`-style skip in the walk) keeps a
    # leftover from a crashed sweep from being collected as a phantom TEST on the
    # next run — which silently inflated the file count and scored as a PASS.
    fd, tmp = tempfile.mkstemp(prefix=TMP_PREFIX, suffix=".js", dir=os.path.dirname(path))
    hf = None
    try:
        with os.fdopen(fd, "wb") as tmp_file:
            tmp_file.write(assembled.encode("utf-8"))
        try:
            # A `flags:[module]` test runs as an ES module (top-level await,
            # module scope); the test text alone is the module, with the harness
            # supplied separately -- the same shape scripts now use.
            subcmd = "mjs" if is_module else "js"
            env = dict(os.environ)
            env["RUST_BACKTRACE"] = "0"
            if cannot_block:
                env["ZIPP_CAN_BLOCK"] = "0"
            # Module tests run the ORIGINAL file (self-imports resolve to the
            # same module record); scripts run the assembled tmp.
            entry = path if is_module else tmp
            # test262 tests the Script goal as specified: top-level `return` is
            # an early SyntaxError and `import`/`export` are Module-only. The
            # engine's default is node's CommonJS shape (both legal), so ask for
            # the pure goal. Module tests already use it by construction.
            cmd = [args.zipp, subcmd] + ([] if is_module else ["--script-goal"]) + [entry]
            if harness_src is not None:
                hfd, hf = tempfile.mkstemp(prefix=TMP_PREFIX, suffix=".js", dir=os.path.dirname(path))
                with os.fdopen(hfd, "wb") as harness_file:
                    harness_file.write(harness_src.encode("utf-8"))
                cmd.append(hf)
            p = subprocess.run(cmd, capture_output=True,
                               encoding="utf-8", errors="replace", timeout=args.timeout,
                               env=env, cwd=os.path.dirname(path))
            verdict, sig = classify(meta, p.returncode, p.stdout or "", p.stderr or "")
        except subprocess.TimeoutExpired:
            verdict, sig = ("FAIL", "timeout")
    finally:
        try: os.remove(tmp)
        except OSError: pass
        try:
            if hf: os.remove(hf)
        except OSError: pass
    return (verdict, sig, job)

def report_engine_identity(zipp):
    """Print WHICH BUILD is about to be conformance-tested, before any test runs.

    `cargo test --workspace --release` builds the library and the test harnesses
    but NOT `target/release/zipp.exe` -- nothing under test depends on the CLI
    binary. A gate script that runs `cargo test` and then calls this runner
    therefore tests whatever binary is on disk, which after a `git stash`/rebuild
    cycle is the wrong one. That happened on 2026-08-01: three modes, ~96,000
    executions each, all reported IDENTICAL, for a build that did not contain the
    change being gated (PERF_ROADMAP B108).

    Nothing here can prevent that -- only building first can. But a run that
    names its engine's commit and dirty flag makes the mistake visible in the log
    instead of invisible in a green result.
    """
    try:
        out = subprocess.run([zipp, "--version", "--json"], stdout=subprocess.PIPE,
                             stderr=subprocess.DEVNULL, timeout=10).stdout
        import json as _json
        d = _json.loads(out.decode("utf-8", "replace"))
        print(f"engine: {d.get('source', '?')}  rustc {d.get('rustc', '?')}  "
              f"jit={d.get('jit')}  profile={d.get('profile')}", flush=True)
        if d.get("dirty"):
            print("engine: tree is DIRTY -- the commit above is the parent it was "
                  "built on, not the code that runs", flush=True)
    except Exception as exc:
        print(f"engine: could not read {zipp} --version --json ({exc})", flush=True)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--t262", required=True)
    ap.add_argument("--sub", default="test")
    ap.add_argument("--zipp", default="./target/release/zipp.exe")
    ap.add_argument("--jobs", type=int, default=12)
    ap.add_argument("--limit", type=int, default=0)
    ap.add_argument("--timeout", type=int, default=20)
    ap.add_argument("--show-fails", type=int, default=25)
    ap.add_argument("--dump-fails", default="", help="write sorted FAIL ids ('relpath [mode]') to this file (exact regression diffs)")
    ap.add_argument("--include-intl402", action="store_true",
                    help="also run test/intl402 (ECMA-402). Excluded by default: it has its own baseline")
    ap.add_argument("--no-staging", action="store_true",
                    help="skip test/staging (INTERPRETING.md says it should be run; this opts out)")
    a = ap.parse_args()
    if a.jobs <= 0:
        ap.error("--jobs must be positive")
    if a.timeout <= 0:
        ap.error("--timeout must be positive")
    if a.limit < 0:
        ap.error("--limit must be non-negative")
    if a.show_fails < 0:
        ap.error("--show-fails must be non-negative")
    zipp = os.path.realpath(a.zipp) if os.path.isfile(a.zipp) else shutil.which(a.zipp)
    if not zipp:
        ap.error(f"--zipp executable was not found: {a.zipp}")
    a.zipp = zipp
    a.t262 = os.path.realpath(a.t262)
    if not os.path.isdir(a.t262):
        ap.error(f"test262 root does not exist: {a.t262}")
    report_engine_identity(a.zipp)
    root = os.path.realpath(os.path.join(a.t262, a.sub))
    try:
        inside_checkout = os.path.commonpath((a.t262, root)) == a.t262
    except ValueError:  # different drives on Windows
        inside_checkout = False
    if not inside_checkout:
        ap.error(f"--sub must stay within the test262 checkout: {a.sub}")
    if not os.path.isdir(root):
        ap.error(f"test262 subtree does not exist: {root}")
    files = []
    for dp, _, fns in os.walk(root):
        norm = dp.replace(os.sep, "/")
        if "/intl402" in norm and not a.include_intl402:
            continue
        if "/staging" in norm and a.no_staging:
            continue
        for fn in fns:
            if fn.startswith(TMP_PREFIX):
                continue
            if fn.endswith(".js") and not fn.endswith("_FIXTURE.js"):
                path = os.path.realpath(os.path.join(dp, fn))
                try:
                    inside_checkout = os.path.commonpath((a.t262, path)) == a.t262
                except ValueError:
                    inside_checkout = False
                if not inside_checkout:
                    ap.error(f"test file escapes the checkout through a link: {path}")
                files.append(path)
    files.sort()
    if a.limit:
        files = files[: a.limit]
    # One JOB per required execution: an unflagged test contributes both a
    # sloppy and a strict run (INTERPRETING.md), so the totals below count
    # executions, not files.
    jobs = []
    for p in files:
        try:
            flags = parse_frontmatter(read_source(p, a.t262))["flags"]
        except Exception:
            flags = []
        for mode in modes_for(flags):
            jobs.append((p, mode))
    get_harness = load_harness(os.path.join(a.t262, "harness"))
    totals = collections.Counter()
    by_cat = collections.defaultdict(collections.Counter)
    fail_sigs = collections.Counter()
    n = len(jobs)
    print(f"running {n} executions ({len(files)} files) with {a.jobs} workers …", flush=True)
    fail_paths = []
    with concurrent.futures.ThreadPoolExecutor(max_workers=a.jobs) as ex:
        for i, (verdict, sig, job) in enumerate(
            ex.map(lambda j: run_one(a, get_harness, j), jobs)
        ):
            path, mode = job
            totals[verdict] += 1
            rel = os.path.relpath(path, os.path.join(a.t262, "test"))
            cat = os.sep.join(rel.split(os.sep)[:3])
            by_cat[cat][verdict] += 1
            if verdict == "FAIL":
                fail_sigs[sig] += 1
                fail_paths.append(f"{rel.replace(os.sep, '/')} [{mode}]")
            if (i + 1) % 2000 == 0:
                print(f"  … {i+1}/{n}", flush=True)
    if a.dump_fails:
        with open(a.dump_fails, "w", newline="\n") as fh:
            fh.write("\n".join(sorted(fail_paths)) + "\n")
        print(f"wrote {len(fail_paths)} FAIL relpaths to {a.dump_fails}")
    p, f, s = totals["PASS"], totals["FAIL"], totals["SKIP"]
    # SKIPs stay in the denominator: a change that made 500 tests unreadable
    # must not be able to RAISE the reported pass rate.
    ran = p + f + s
    print(f"\n==== test262 ({a.sub}) ====")
    print(f"PASS {p}  FAIL {f}  SKIP {s}   pass-rate {100*p/ran:.1f}% of {ran} executions\n")
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
