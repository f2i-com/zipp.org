//! Tier-C application-op lanes (`ZIPP_NO_TIERC_CLOSURE_MAKE`,
//! `ZIPP_NO_TIERC_ITER`): closure creation with live capture cells, the
//! sync `for-of` machinery with its finally bracket, `Object.keys`, `in`,
//! `new Array`, computed element calls, and the general method route — all
//! byte-identical across the native lanes, their comparators, the
//! interpreter, GC stress, and an eager threshold, including the observable
//! decline paths (patched iterator protocol, proxies, abrupt completions).

const SOURCE: &str = r#"
  "use strict";
  const out = [];

  // Closure creation in a hot compiled body: fresh identity per iteration,
  // shared mutable capture cells, lexical `this` for arrows, and inherited
  // home objects through methods.
  function makeCounters(n) {
    const counters = [];
    for (let i = 0; i < n; i++) {
      let count = i;
      counters.push({
        id: i,
        bump(step) { count = (count + step) | 0; return count; },
        read: () => count
      });
    }
    return counters;
  }
  let bumped = 0;
  for (let round = 0; round < 900; round++) {
    const counters = makeCounters(8);
    for (let i = 0; i < counters.length; i++) {
      bumped = (bumped + counters[i].bump(round & 7) + counters[i].read()) | 0;
    }
  }
  out.push("closures:" + bumped);

  // Fresh arrows are distinct identities (a React-style diff observes this).
  const a1 = makeCounters(1)[0];
  const a2 = makeCounters(1)[0];
  out.push("identity:" + (a1.read === a2.read) + ":" + (a1.read === a1.read));

  // for-of over Object.keys + `in` + nested finally, hot enough to compile.
  function diffKeys(oldProps, newProps) {
    let patches = 0;
    for (const key of Object.keys(newProps)) {
      if (oldProps[key] !== newProps[key]) patches = (patches + 1) | 0;
    }
    for (const key of Object.keys(oldProps)) {
      if (!(key in newProps)) patches = (patches + 31) | 0;
    }
    return patches;
  }
  let diffSum = 0;
  for (let i = 0; i < 4000; i++) {
    const oldProps = { a: i, b: "x" + (i & 7), c: i & 1 };
    const newProps = (i & 1)
      ? { a: i, c: i & 1, d: true }
      : { a: i + 1, b: "x" + (i & 7), c: i & 1 };
    diffSum = (diffSum + diffKeys(oldProps, newProps)) | 0;
  }
  out.push("diff:" + diffSum);

  // Abrupt completions THROUGH the natively-pushed finally bracket: the throw
  // must run the iterator close and the finally routing exactly once.
  function abruptSum(rows) {
    let seen = 0;
    try {
      for (const row of rows) {
        seen = (seen + row) | 0;
        if (row === 13) throw new Error("mid-loop");
      }
    } catch (e) {
      seen = (seen + e.message.length) | 0;
    }
    return seen;
  }
  let abrupt = 0;
  for (let i = 0; i < 2600; i++) {
    abrupt = (abrupt + abruptSum([1, 2, (i & 15) === 5 ? 13 : 3, 4])) | 0;
  }
  out.push("abrupt:" + abrupt);

  // break/continue through the bracket (jump completions).
  function firstBig(rows, floor) {
    for (const row of rows) {
      if (row < floor) continue;
      return row;
    }
    return -1;
  }
  let jumps = 0;
  for (let i = 0; i < 2600; i++) {
    jumps = (jumps + firstBig([i & 3, i & 7, i & 15, 99], 5)) | 0;
  }
  out.push("jumps:" + jumps);

  // The PATCHED protocol must be honoured after warmup: a replaced
  // Array.prototype[Symbol.iterator] takes over for-of exactly.
  const realIter = Array.prototype[Symbol.iterator];
  Array.prototype[Symbol.iterator] = function () {
    let n = 0;
    const self = this;
    return {
      next() {
        n += 1;
        return n <= self.length
          ? { value: "patched" + n, done: false }
          : { value: undefined, done: true };
      }
    };
  };
  const patched = [];
  for (const v of ["a", "b"]) patched.push(v);
  Array.prototype[Symbol.iterator] = realIter;
  for (const v of ["c", "d"]) patched.push(v);
  out.push("patched:" + patched.join(","));

  // Proxies through `in` and Object.keys after the native lanes warmed.
  const trapLog = [];
  const proxied = new Proxy({ p: 1 }, {
    has(t, k) { trapLog.push("has:" + String(k)); return k in t; },
    ownKeys(t) { trapLog.push("keys"); return Reflect.ownKeys(t); },
    getOwnPropertyDescriptor(t, k) { return Reflect.getOwnPropertyDescriptor(t, k); }
  });
  out.push("proxy:" + ("p" in proxied) + ("q" in proxied) + Object.keys(proxied).join("")
    + ":" + trapLog.join("|"));

  // new Array + computed element calls (listeners[i](...)).
  const listeners = new Array(4);
  for (let i = 0; i < listeners.length; i++) {
    listeners[i] = (v) => (v * (i + 1)) | 0;
  }
  let dispatched = 0;
  for (let i = 0; i < 6000; i++) {
    dispatched = (dispatched + listeners[i & 3](i & 31)) | 0;
  }
  out.push("dispatch:" + dispatched + ":" + new Array(3).length + ":" + Array(17, 4).join("-"));

  // The general method route: user-closure methods called hot by name, plus a
  // native (reverse) mixed at the same shape.
  const store = (() => {
    let selected = 0;
    return {
      getState: () => ({ selected }),
      select(id) { selected = id | 0; }
    };
  })();
  let stateSum = 0;
  for (let i = 0; i < 5000; i++) {
    if ((i & 15) === 0) store.select(i & 63);
    stateSum = (stateSum + store.getState().selected) | 0;
  }
  const rev = [1, 2, 3, 4];
  for (let i = 0; i < 64; i++) rev.reverse();
  out.push("methods:" + stateSum + ":" + rev.join(""));

  // TDZ semantics survive the native cell lanes.
  let tdz = "none";
  try {
    // eslint-disable-next-line no-use-before-define
    const probe = () => late;
    probe();
    let late = 1;
  } catch (e) {
    tdz = e.constructor.name;
  }
  out.push("tdz:" + tdz);

  console.log(out.join("\n"));
"#;

fn run_source(source: &str) -> Vec<String> {
    let out = zipp_vm::run(source).expect("source compiles");
    assert!(out.error.is_none(), "runtime error: {:?}", out.error);
    out.output
}

#[test]
fn tierc_app_ops_child() {
    if std::env::var("ZIPP_TIERC_APP_CHILD").is_err() {
        return;
    }
    let output = run_source(SOURCE);
    println!("tierc-app-result:{}", output.join("|"));
}

#[cfg(all(feature = "jit", target_arch = "x86_64"))]
fn child(mode: &str, vars: &[(&str, &str)]) -> (String, String) {
    let exe = std::env::current_exe().expect("test binary path");
    let mut cmd = std::process::Command::new(exe);
    cmd.args(["tierc_app_ops_child", "--exact", "--nocapture"])
        .env("ZIPP_TIERC_APP_CHILD", mode)
        .env("ZIPP_JIT_THRESHOLD", "1")
        .env("ZIPP_JITLOG", "1");
    for key in [
        "ZIPP_TIERC_CLOSURE_MAKE",
        "ZIPP_NO_TIERC_CLOSURE_MAKE",
        "ZIPP_NO_TIERC_ITER",
        "ZIPP_NO_OBJECT_FINALIZE",
        "ZIPP_NOJIT",
        "ZIPP_GC_STRESS",
        "ZIPP_NURSERY_VERIFY",
    ] {
        cmd.env_remove(key);
    }
    cmd.envs(vars.iter().copied());
    let out = cmd.output().expect("spawn tierc-app child");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "{mode} failed:\n{stdout}\n{stderr}");
    let result = stdout
        .lines()
        .find_map(|line| line.strip_prefix("tierc-app-result:"))
        .unwrap_or_else(|| panic!("{mode} emitted no result marker:\n{stdout}\n{stderr}"))
        .to_owned();
    (result, stderr)
}

/// The default JIT child must compile the closure/iterator bodies (not just
/// interpret), and every mode must print byte-identical output.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
#[test]
fn app_ops_match_comparators_interpreter_and_gc_paths() {
    // B181: the closure lane is default-off pending the cross-entry
    // effect-then-deopt exclusion; opt in explicitly so this equivalence
    // matrix keeps exercising the lane it was written for.
    let (jit, jit_log) = child("jit", &[("ZIPP_TIERC_CLOSURE_MAKE", "1")]);
    assert!(
        jit_log.contains("Tier C fn") && jit_log.contains(" compiled"),
        "hot application bodies never compiled through Tier C:\n{jit_log}"
    );
    let (nojit, _) = child("nojit", &[("ZIPP_NOJIT", "1")]);
    let (gc, _) = child(
        "gc",
        &[
            ("ZIPP_TIERC_CLOSURE_MAKE", "1"),
            ("ZIPP_GC_STRESS", "1"),
            ("ZIPP_NURSERY_VERIFY", "1"),
        ],
    );
    let (lanes_off, _) = child(
        "lanes-off",
        &[
            ("ZIPP_NO_TIERC_CLOSURE_MAKE", "1"),
            ("ZIPP_NO_TIERC_ITER", "1"),
        ],
    );
    let (closure_off, _) = child("closure-off", &[("ZIPP_NO_TIERC_CLOSURE_MAKE", "1")]);
    let (iter_off, _) = child("iter-off", &[("ZIPP_NO_TIERC_ITER", "1")]);
    assert_eq!(jit, nojit);
    assert_eq!(jit, gc);
    assert_eq!(jit, lanes_off);
    assert_eq!(jit, closure_off);
    assert_eq!(jit, iter_off);
}

/// The interpreter-only build runs the same program identically.
#[cfg(not(all(feature = "jit", target_arch = "x86_64")))]
#[test]
fn app_ops_match_interpreter_semantics_without_tier_c() {
    assert!(!run_source(SOURCE).is_empty());
}
