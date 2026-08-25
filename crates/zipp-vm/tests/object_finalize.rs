//! One-step `FinalizeObject` literal lowering: exact semantic equivalence with
//! the per-field append comparator (`ZIPP_NO_OBJECT_FINALIZE=1`) across the
//! interpreter, Tier C, GC stress, and eager-threshold modes, plus staged-block
//! safety around throws, generators, methods/`super`, and the SROA/concat-len
//! deopt materialization path.

/// Broad literal semantics in one deterministic program. Every observation is
/// printed, so cross-mode equality pins enumeration order, descriptors, name
/// inference, `super` resolution, structural mutation after a completed plan,
/// numeric keys, wide literals (past the property-index threshold), nested
/// blocks, and evaluation-order side effects.
const SOURCE: &str = r#"
  "use strict";
  const log = [];
  function effect(tag, v) { log.push(tag); return v; }

  // Hot four-field literal, method + super, survivors across collections.
  const protoBase = { label() { return "base"; } };
  function makeNode(i) {
    return {
      serial: i,
      tag: "n" + (i & 31),
      label() { return super.label() + ":" + this.serial; },
      pair: { left: i ^ 85, right: (i + 3) | 0 }
    };
  }
  let sum = 0;
  const keep = [];
  for (let i = 0; i < 2400; i++) {
    const n = makeNode(i);
    Object.setPrototypeOf(n, protoBase);
    sum = (sum + n.pair.left + n.pair.right + n.tag.length) | 0;
    if (i % 97 === 0) keep.push(n);
  }
  console.log("sum:" + sum + ":" + keep.length + ":" + keep[7].label());

  // Extracted method keeps its [[HomeObject]] through further allocation.
  const extracted = keep[3].label;
  for (let i = 0; i < 5000; i++) keep.push(makeNode(i));
  console.log("extracted:" + extracted.call({ serial: "X" }));

  // Evaluation order and abrupt completion inside the staged block.
  const ordered = { a: effect("A", 1), b: effect("B", 2), c: effect("C", 3) };
  let caught = "none";
  try {
    void { x: effect("X", 1), y: (() => { throw new Error("mid"); })(), z: effect("Z", 3) };
  } catch (e) {
    caught = e.message;
  }
  console.log("order:" + log.join("") + ":" + caught + ":" + JSON.stringify(ordered));

  // Numeric keys, name inference, shorthand __proto__, wide literals.
  const one = 1;
  const __proto__ = "ownName";
  const numeric = { 1: "one", a: "letter", 0: "zero", __proto__ };
  const named = { fn: function () { return 1; }, arrow: () => 2, m() { return 3; } };
  const wide = { k0:0,k1:1,k2:2,k3:3,k4:4,k5:5,k6:6,k7:7,k8:8,k9:9,k10:10,k11:11,k12:12,k13:13 };
  console.log("numeric:" + numeric[1] + numeric[0] + ":" + Object.keys(numeric).join(",")
    + ":" + numeric.__proto__ + ":" + (Object.getPrototypeOf(numeric) === Object.prototype));
  console.log("named:" + named.fn.name + "," + named.arrow.name + "," + named.m.name
    + ":" + (one === 1));
  console.log("wide:" + wide.k0 + wide.k13 + ":" + Object.keys(wide).length
    + ":" + JSON.stringify(Object.getOwnPropertyDescriptor(wide, "k7")));

  // Structural mutation after the completed one-step build.
  const mutate = { p: 1, q: 2, r: 3 };
  mutate.extra = 4;
  delete mutate.q;
  Object.defineProperty(mutate, "hidden", { value: 9, enumerable: false });
  const forIn = [];
  for (const k in mutate) forIn.push(k);
  console.log("mutate:" + forIn.join(",") + ":" + JSON.stringify(mutate) + ":" + mutate.hidden);

  // A literal whose value expression suspends the frame (staged block must
  // survive the generator park/resume).
  function* gen() {
    return { anchor: 1, kept: (yield "pause", { marker: "alive" }), tail: 3 };
  }
  const it = gen();
  const first = it.next();
  const done = it.next();
  console.log("gen:" + first.value + ":" + done.value.kept.marker + ":"
    + Object.keys(done.value).join(","));
"#;

/// The ephemeral SROA shape: a finalized literal whose fields are read back
/// (two forwarded slots plus the virtual concat-length projection), crossing
/// the interpreted-warmup -> OSR-entry boundary and the Int -> Double
/// accumulator transition (`checksum` deliberately has no `|0`).
const SROA_SOURCE: &str = r#"
  "use strict";
  (function main() {
    const rounds = 120000;
    let checksum = 0;
    for (let i = 0; i < rounds; i++) {
      const point = { x: i & 1023, y: (i * 3) & 2047, tag: "p" + (i & 31) };
      const pair = [point.x + point.y, point.tag.length];
      checksum = checksum + pair[0] + pair[1];
    }
    console.log("sroa-shape", checksum, rounds);
  })();
"#;

fn run_source(source: &str) -> Vec<String> {
    let out = zipp_vm::run(source).expect("source compiles");
    assert!(out.error.is_none(), "runtime error: {:?}", out.error);
    out.output
}

#[test]
fn object_finalize_child() {
    let Some(mode) = std::env::var("ZIPP_OBJECT_FINALIZE_CHILD").ok() else {
        return;
    };
    let source = if mode.starts_with("sroa") {
        SROA_SOURCE
    } else {
        SOURCE
    };
    let output = run_source(source);
    println!("finalize-result:{}", output.join("|"));
}

#[cfg(all(feature = "jit", target_arch = "x86_64"))]
fn child(mode: &str, vars: &[(&str, &str)]) -> (String, String) {
    let exe = std::env::current_exe().expect("test binary path");
    let mut cmd = std::process::Command::new(exe);
    cmd.args(["object_finalize_child", "--exact", "--nocapture"])
        .env("ZIPP_OBJECT_FINALIZE_CHILD", mode)
        .env("ZIPP_JIT_THRESHOLD", "1")
        .env("ZIPP_JITLOG", "1");
    for key in [
        "ZIPP_NO_OBJECT_FINALIZE",
        "ZIPP_NO_STATIC_KEY_PLANS",
        "ZIPP_NO_LOCAL_SROA",
        "ZIPP_NO_LOCAL_CONCAT_LEN",
        "ZIPP_NOJIT",
        "ZIPP_GC_STRESS",
        "ZIPP_NURSERY_VERIFY",
    ] {
        cmd.env_remove(key);
    }
    cmd.envs(vars.iter().copied());
    let out = cmd.output().expect("spawn object-finalize child");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "{mode} failed:\n{stdout}\n{stderr}");
    let result = stdout
        .lines()
        .find_map(|line| line.strip_prefix("finalize-result:"))
        .unwrap_or_else(|| panic!("{mode} emitted no result marker:\n{stdout}\n{stderr}"))
        .to_owned();
    (result, stderr)
}

/// Every mode must print byte-identical semantics; the default JIT child must
/// actually reach Tier C so the fused allocation helper is exercised, not just
/// the interpreter arm.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
#[test]
fn finalized_literals_match_the_append_comparator_everywhere() {
    let (jit, jit_log) = child("jit", &[]);
    assert!(
        jit_log.contains("Tier C fn") && jit_log.contains(" compiled"),
        "hot literal builder never compiled through Tier C:\n{jit_log}"
    );
    let (nojit, _) = child("nojit", &[("ZIPP_NOJIT", "1")]);
    let (gc, _) = child(
        "gc",
        &[("ZIPP_GC_STRESS", "1"), ("ZIPP_NURSERY_VERIFY", "1")],
    );
    let (off, _) = child("off", &[("ZIPP_NO_OBJECT_FINALIZE", "1")]);
    let (off_nojit, _) = child(
        "off-nojit",
        &[("ZIPP_NO_OBJECT_FINALIZE", "1"), ("ZIPP_NOJIT", "1")],
    );
    let (plans_off, _) = child("plans-off", &[("ZIPP_NO_STATIC_KEY_PLANS", "1")]);
    assert_eq!(jit, nojit);
    assert_eq!(jit, gc);
    assert_eq!(jit, off);
    assert_eq!(jit, off_nojit);
    assert_eq!(jit, plans_off);
}

/// The interpreter-only build still runs the same program identically.
#[cfg(not(all(feature = "jit", target_arch = "x86_64")))]
#[test]
fn finalized_literals_match_interpreter_semantics_without_tier_c() {
    let with = run_source(SOURCE);
    assert!(!with.is_empty());
}

/// The finalized-literal SROA lane must still engage (with its virtual
/// concat-length sub-lane), and its output must match the un-scalar-replaced,
/// the append-comparator, and the interpreter-only executions exactly.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
#[test]
fn finalized_sroa_engages_and_matches_every_comparator() {
    let (fast, log) = child("sroa", &[]);
    assert!(
        log.contains("LOCAL-SROA region") && log.contains("finalized=1"),
        "finalized literal was not scalar-replaced:\n{log}"
    );
    assert!(
        log.contains("concat_lens=1"),
        "virtual concat-length sub-lane did not engage:\n{log}"
    );
    let (no_sroa, _) = child("sroa-off", &[("ZIPP_NO_LOCAL_SROA", "1")]);
    let (legacy, _) = child("sroa-legacy", &[("ZIPP_NO_OBJECT_FINALIZE", "1")]);
    let (interp, _) = child("sroa-nojit", &[("ZIPP_NOJIT", "1")]);
    assert_eq!(fast, no_sroa);
    assert_eq!(fast, legacy);
    assert_eq!(fast, interp);
}
