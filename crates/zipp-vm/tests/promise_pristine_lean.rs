//! The lean pristine-%Promise.prototype% warm re-check.
//!
//! `promise_proto_pristine` is asked on every `.then`, every `await` and every
//! `Promise.all` element. Its slot cache (`promise_slot_cache.rs`) already
//! guards the LAYOUT with version compares, but re-read three key strings,
//! three `PropAttrs::at` results and three unpacked heap identities per call
//! because an in-place `vals[i] = v` bumps nothing — 7.5% of
//! `async-promise-chain`'s samples, 13.5% of its await part.
//!
//! `promise_pristine_bit_identical` answers the warm case from bit compares
//! alone: the two owner versions, the value BITS at the `then`/`constructor`/
//! `@@species` slots against the bits the fill-time proof saw, the accessor
//! bits, and the species getter's version. It only ever answers `true`; any
//! mismatch runs the original re-check unchanged, so every `false` and every
//! re-proof has exactly its old provenance.
//!
//! What must invalidate the cached `true`, and through which guard:
//!
//!   * `Promise.prototype.then = f` / `.constructor = x` — an in-place data
//!     write (interpreter `set_prop`, or the JIT's `SetProp` own-data hit
//!     writing `vals_ptr[slot]` with no call) — changes the slot's BITS, so
//!     the value compare fails on the very next call;
//!   * `defineProperty` redefinitions (accessor `then`/`constructor`, a
//!     replaced `@@species` getter) — bump the owner's version, and the
//!     accessor bit is re-read regardless;
//!   * `delete` + re-add (slots move) — bumps; the re-proof resolves the new
//!     slot indices;
//!   * a subclass instance — fails the instance checks before the shared
//!     proof is consulted, cache or no cache;
//!   * `Object.freeze(Promise.prototype)` — changes nothing the proof reads
//!     (writable/configurable are not part of it), so the cache stays warm
//!     and the intrinsic keeps being taken, exactly as the spec requires.
//!
//! Every `lean_parity_*` expectation below was executed in node v24.12.0
//! (`node <case>.js`) and diffs byte-identical. The whole set re-runs in child
//! processes with `ZIPP_NO_PRISTINE_LEAN=1` (the original re-check), with
//! both `ZIPP_NO_PRISTINE_LEAN=1` and `ZIPP_NO_PROMISE_SLOT_CACHE=1` (the
//! original full proof on every call), and under `ZIPP_NOJIT=1`,
//! `ZIPP_JIT_THRESHOLD=1` and `ZIPP_NO_NURSERY=1`, so every mode is held to
//! the same node-derived outputs.

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}",
        out.error
    );
    out.output
}

#[test]
fn lean_parity_warm_chain_threads_values() {
    let out = run_ok(
        r#"
        "use strict";
        var p = Promise.resolve(0);
        function inc(x) { return x + 1; }
        for (var i = 0; i < 5000; i++) p = p.then(inc);
        p.then(function (v) { console.log("chain:" + v); });
        "#,
    );
    assert_eq!(out[0], "chain:5000");
}

#[test]
fn lean_parity_warm_await_loop_accumulates() {
    let out = run_ok(
        r#"
        "use strict";
        async function g() {
          var one = Promise.resolve(1);
          var s = 0;
          for (var i = 0; i < 3000; i++) s += await one;
          return s;
        }
        g().then(function (v) { console.log("await:" + v); });
        "#,
    );
    assert_eq!(out[0], "await:3000");
}

#[test]
fn lean_parity_in_place_then_patch_and_restore_are_both_observed() {
    // The soundness-critical mutation: an in-place value write bumps nothing,
    // so only the bits compare can catch it — 2000 warm calls, patch, 5
    // wrapped calls, restore (another in-place write), 5 unwrapped calls.
    let out = run_ok(
        r#"
        "use strict";
        var orig = Promise.prototype.then;
        var p = Promise.resolve(0);
        function inc(x) { return x + 1; }
        for (var i = 0; i < 2000; i++) p = p.then(inc);
        var patched = 0;
        Promise.prototype.then = function (f, r) { patched++; return orig.call(this, f, r); };
        for (var j = 0; j < 5; j++) p = p.then(inc);
        Promise.prototype.then = orig;
        for (var k = 0; k < 5; k++) p = p.then(inc);
        p.then(function (v) { console.log(v + "/" + patched); });
        "#,
    );
    assert_eq!(out[0], "2010/5");
}

#[test]
fn lean_parity_a_compiled_store_site_toggling_then_is_counted_exactly() {
    // `put` is a hot SetProp site whose receiver IS %Promise.prototype% — the
    // shape a compiled own-data store hit would take, writing the slot with
    // no VM call. Alternating the intrinsic and a wrapper on every iteration,
    // the wrapper must be called exactly once per `.then` issued while it
    // was installed.
    let out = run_ok(
        r#"
        "use strict";
        var orig = Promise.prototype.then;
        var calls = 0;
        function wrap(f, r) { calls++; return orig.call(this, f, r); }
        function put(o, f) { o.then = f; }
        var expect = 0;
        var p = Promise.resolve(0);
        function inc(x) { return x + 1; }
        for (var i = 0; i < 4000; i++) {
          put(Promise.prototype, (i & 1) ? wrap : orig);
          if (i & 1) expect++;
          p = p.then(inc);
        }
        put(Promise.prototype, orig);
        p.then(function (v) { console.log(v + "/" + calls + "/" + expect); });
        "#,
    );
    assert_eq!(out[0], "4000/2000/2000");
}

#[test]
fn lean_parity_in_place_constructor_patch_routes_species() {
    // `constructor = fake` is the OTHER un-bumped in-place overwrite; every
    // later `.then` must run SpeciesConstructor through it (5 incs + the
    // final observer = 6 constructions via C2).
    let out = run_ok(
        r#"
        "use strict";
        function inc(x) { return x + 1; }
        var made = 0;
        function C2(exec) { made++; return new Promise(exec); }
        var p = Promise.resolve(0);
        for (var i = 0; i < 1000; i++) p = p.then(inc);
        var fake = {};
        Object.defineProperty(fake, Symbol.species, { value: C2, configurable: true });
        Promise.prototype.constructor = fake;
        for (var j = 0; j < 5; j++) p = p.then(inc);
        p.then(function (v) { console.log(v + "/" + made); });
        "#,
    );
    assert_eq!(out[0], "1005/6");
}

#[test]
fn lean_parity_species_getter_redefined_runs_per_then() {
    // `defineProperty(Promise, @@species, {get})` bumps the ctor version —
    // the getter must run once per later `.then`.
    let out = run_ok(
        r#"
        "use strict";
        function inc(x) { return x + 1; }
        var p = Promise.resolve(0);
        for (var i = 0; i < 1000; i++) p = p.then(inc);
        var reads = 0;
        Object.defineProperty(Promise, Symbol.species, {
          get: function () { reads++; return Promise; },
          configurable: true
        });
        for (var j = 0; j < 5; j++) p = p.then(inc);
        p.then(function (v) { console.log(v + "/" + reads); });
        "#,
    );
    assert_eq!(out[0], "1005/6");
}

#[test]
fn lean_parity_species_swapped_for_another_intrinsic_getter_refills() {
    // Re-install the intrinsic species getter through `defineProperty`: the
    // ctor version bumps, the re-proof accepts the getter (any
    // `Native(SPECIES_GET)` answers the same question) and the cache refills.
    // (node hands out a distinct getter function per constructor; zipp shares
    // one, so the two engines' getter identities are not compared here.)
    let out = run_ok(
        r#"
        "use strict";
        function inc(x) { return x + 1; }
        var p = Promise.resolve(0);
        for (var i = 0; i < 1000; i++) p = p.then(inc);
        var arrGet = Object.getOwnPropertyDescriptor(Array, Symbol.species).get;
        Object.defineProperty(Promise, Symbol.species, { get: arrGet, configurable: true });
        for (var j = 0; j < 1000; j++) p = p.then(inc);
        var now = Object.getOwnPropertyDescriptor(Promise, Symbol.species).get;
        p.then(function (v) { console.log(v + "/" + (now === arrGet) + "/" + (Promise[Symbol.species] === Promise)); });
        "#,
    );
    assert_eq!(out[0], "2000/true/true");
}

#[test]
fn lean_parity_then_redefined_as_accessor_runs_the_getter_per_then() {
    // The accessor-bit case a stale cache would get wrong: the redefinition
    // bumps the proto version AND the bit is re-read; the getter must run
    // once per later `.then`.
    let out = run_ok(
        r#"
        "use strict";
        function inc(x) { return x + 1; }
        var orig = Object.getOwnPropertyDescriptor(Promise.prototype, "then");
        var p = Promise.resolve(0);
        for (var i = 0; i < 1000; i++) p = p.then(inc);
        var gets = 0;
        Object.defineProperty(Promise.prototype, "then", {
          get: function () { gets++; return orig.value; },
          configurable: true
        });
        for (var j = 0; j < 5; j++) p = p.then(inc);
        Object.defineProperty(Promise.prototype, "then", orig);
        p.then(function (v) { console.log(v + "/" + gets); });
        "#,
    );
    assert_eq!(out[0], "1005/5");
}

#[test]
fn lean_parity_constructor_redefined_as_accessor_is_read_per_then() {
    let out = run_ok(
        r#"
        "use strict";
        function inc(x) { return x + 1; }
        var p = Promise.resolve(0);
        for (var i = 0; i < 1000; i++) p = p.then(inc);
        var reads = 0;
        Object.defineProperty(Promise.prototype, "constructor", {
          get: function () { reads++; return Promise; },
          configurable: true
        });
        for (var j = 0; j < 5; j++) p = p.then(inc);
        p.then(function (v) { console.log(v + "/" + reads); });
        "#,
    );
    assert_eq!(out[0], "1005/6");
}

#[test]
fn lean_parity_delete_and_readd_then_moves_the_slot_and_reproves() {
    // A key delete + re-add shifts every later slot: the version bumps, the
    // re-proof resolves the NEW index, and the chain keeps threading through
    // the (now enumerable, re-added) intrinsic.
    let out = run_ok(
        r#"
        "use strict";
        var orig = Promise.prototype.then;
        var p = Promise.resolve(0);
        function inc(x) { return x + 1; }
        for (var i = 0; i < 1000; i++) p = p.then(inc);
        var before = Object.getOwnPropertyNames(Promise.prototype).join(",");
        delete Promise.prototype.then;
        Promise.prototype.then = orig;
        var after = Object.getOwnPropertyNames(Promise.prototype).join(",");
        for (var j = 0; j < 1000; j++) p = p.then(inc);
        var d = Object.getOwnPropertyDescriptor(Promise.prototype, "then");
        p.then(function (v) { console.log(v + "/" + (before !== after) + "/" + d.enumerable + "/" + d.writable + "/" + d.configurable); });
        "#,
    );
    assert_eq!(out[0], "2000/true/true/true/true");
}

#[test]
fn lean_parity_freezing_the_prototype_keeps_the_intrinsic_warm() {
    // freeze flips writable/configurable — neither is part of the proof, so
    // nothing the cache reads changes; a strict-mode patch attempt throws
    // and the intrinsic keeps being taken.
    let out = run_ok(
        r#"
        "use strict";
        var p = Promise.resolve(0);
        function inc(x) { return x + 1; }
        for (var i = 0; i < 1000; i++) p = p.then(inc);
        Object.freeze(Promise.prototype);
        var patched = 0;
        try { Promise.prototype.then = function () { patched = -1; }; } catch (e) { patched = 1; }
        for (var j = 0; j < 5; j++) p = p.then(inc);
        p.then(function (v) { console.log(v + "/" + patched + "/" + Object.isFrozen(Promise.prototype)); });
        "#,
    );
    assert_eq!(out[0], "1005/1/true");
}

#[test]
fn lean_parity_rewriting_the_same_values_stays_pristine() {
    // Same bits written back in place: bit-identical to the proof, so the
    // fast `true` holds — and it is the right answer.
    let out = run_ok(
        r#"
        "use strict";
        function inc(x) { return x + 1; }
        var p = Promise.resolve(0);
        for (var i = 0; i < 1000; i++) p = p.then(inc);
        Promise.prototype.then = Promise.prototype.then;
        Promise.prototype.constructor = Promise;
        for (var j = 0; j < 1000; j++) p = p.then(inc);
        p.then(function (v) { console.log(v + "/" + (Promise.prototype.then === Object.getOwnPropertyDescriptor(Promise.prototype, "then").value)); });
        "#,
    );
    assert_eq!(out[0], "2000/true");
}

#[test]
fn lean_parity_awaiting_a_constructor_patched_promise_adds_the_adoption_ticks() {
    // The Await lane after 1000 warm awaits: an in-place `constructor`
    // overwrite through a hot compiled store site pushes the await onto the
    // NewPromiseCapability route — node delivers `after=x` two rounds later.
    let out = run_ok(
        r#"
        "use strict";
        var order = [];
        function put(o, k, v) { o[k] = v; }
        for (var w = 0; w < 3000; w++) put({}, "constructor", w);
        async function one(p, tag) { var r = await p; order.push(tag + "=" + r); }
        async function main() {
          var p = Promise.resolve("x");
          for (var i = 0; i < 1000; i++) await p;
          put(Promise.prototype, "constructor", function Nope() {});
          one(p, "after");
          var t = Promise.resolve();
          for (var n = 1; n <= 8; n++) (function (n) {
            t = t.then(function () { order.push("t" + n); });
          })(n);
          await t;
          await null; await null; await null; await null;
          Promise.prototype.constructor = Promise;
          console.log(order.join("|"));
        }
        main();
        "#,
    );
    assert_eq!(out[0], "t1|t2|after=x|t3|t4|t5|t6|t7|t8");
}

#[test]
fn lean_parity_await_loop_with_then_patched_midway_is_unaffected() {
    // Await never Gets `then` on a native promise: patching it mid-loop must
    // be invisible (no calls, no extra ticks) — in both engines.
    let out = run_ok(
        r#"
        "use strict";
        var orig = Promise.prototype.then;
        var calls = 0;
        async function main() {
          var one = Promise.resolve(1);
          var s = 0;
          for (var i = 0; i < 2000; i++) s += await one;
          Promise.prototype.then = function (f, r) { calls++; return orig.call(this, f, r); };
          for (var j = 0; j < 5; j++) s += await one;
          Promise.prototype.then = orig;
          return s + "/" + calls;
        }
        main().then(function (v) { console.log(v); });
        "#,
    );
    assert_eq!(out[0], "2005/0");
}

#[test]
fn lean_parity_promise_all_element_lane_sees_a_patched_then() {
    let out = run_ok(
        r#"
        "use strict";
        var warm = [];
        for (var i = 0; i < 1000; i++) warm.push(Promise.resolve(i));
        Promise.all(warm).then(function () {
          var orig = Promise.prototype.then;
          var calls = 0;
          var elems = [Promise.resolve(1), Promise.resolve(2), Promise.resolve(3)];
          Promise.prototype.then = function (f, r) { calls++; return orig.call(this, f, r); };
          Promise.all(elems).then(function (vs) {
            Promise.prototype.then = orig;
            console.log(vs.join(",") + "/" + calls);
          });
        });
        "#,
    );
    assert_eq!(out[0], "1,2,3/4");
}

#[test]
fn lean_parity_promise_all_element_lane_sees_a_patched_constructor() {
    let out = run_ok(
        r#"
        "use strict";
        var warm = [];
        for (var i = 0; i < 1000; i++) warm.push(Promise.resolve(i));
        Promise.all(warm).then(function () {
          var made = 0;
          function C2(exec) { made++; return new Promise(exec); }
          var fake = {};
          Object.defineProperty(fake, Symbol.species, { value: C2, configurable: true });
          Promise.prototype.constructor = fake;
          var elems = [Promise.resolve(1), Promise.resolve(2), Promise.resolve(3)];
          Promise.all(elems).then(function (vs) {
            Promise.prototype.constructor = Promise;
            console.log(vs.join(",") + "/" + made);
          });
        });
        "#,
    );
    assert_eq!(out[0], "1,2,3/7");
}

#[test]
fn lean_parity_a_subclass_never_consults_the_cache() {
    let out = run_ok(
        r#"
        "use strict";
        class MyP extends Promise {
          static get [Symbol.species]() { return Promise; }
        }
        class MyQ extends Promise {}
        var q = MyP.resolve(1);
        var r = q.then(function (x) { return x + 1; });
        var s = MyQ.resolve(3).then(function (x) { return x + 1; });
        console.log((r instanceof MyP) + "/" + (r instanceof Promise) + "/" + (q instanceof MyP) + "/" + (s instanceof MyQ));
        r.then(function (v) { console.log("v=" + v); });
        "#,
    );
    assert_eq!(out[0], "false/true/true/true");
    assert_eq!(out[1], "v=2");
}

/// Re-run every `lean_parity_*` expectation in a child process under `envs`
/// (the latches are read once per process), with every OTHER latch and mode
/// switch removed so the child is in exactly the requested mode.
fn child_passes(envs: &[(&str, &str)]) {
    let exe = std::env::current_exe().expect("test exe path");
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("lean_parity_");
    for k in [
        "ZIPP_NO_PRISTINE_LEAN",
        "ZIPP_NO_PROMISE_SLOT_CACHE",
        "ZIPP_NO_PROMISE_PRISTINE",
        "ZIPP_NOJIT",
        "ZIPP_JIT_THRESHOLD",
        "ZIPP_NO_NURSERY",
    ] {
        cmd.env_remove(k);
    }
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("spawn the test binary");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "child {envs:?} failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !stdout.contains("running 0 tests"),
        "the lean_parity_ filter matched nothing:\n{stdout}"
    );
}

#[test]
fn the_off_switch_selects_the_original_recheck_with_identical_behavior() {
    child_passes(&[("ZIPP_NO_PRISTINE_LEAN", "1")]);
}

#[test]
fn both_switches_off_select_the_full_proof_with_identical_behavior() {
    child_passes(&[
        ("ZIPP_NO_PRISTINE_LEAN", "1"),
        ("ZIPP_NO_PROMISE_SLOT_CACHE", "1"),
    ]);
}

#[test]
fn zz_gate_modes_agree() {
    // Tier parity: the interpreter, the forced-JIT threshold and the
    // nursery-off heap must all print the node-derived outputs.
    child_passes(&[("ZIPP_NOJIT", "1")]);
    child_passes(&[("ZIPP_JIT_THRESHOLD", "1")]);
    child_passes(&[("ZIPP_NO_NURSERY", "1")]);
}
