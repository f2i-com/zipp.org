//! The pristine-%Promise.prototype% proof is answered from resolved slots.
//!
//! `promise_proto_pristine` re-found `then`/`constructor`/`@@species` with
//! 2-4 linear `pos()` scans plus 3-4 `heap.get`s on EVERY `.then` / `await` /
//! `Promise.all` element (~6M times on `async-promise-chain`). The slot cache
//! resolves those positions once and re-validates them per call with version
//! compares against the heap's index-parallel `versions` array — the same
//! invalidation the JIT inline cache rides.
//!
//! What the cache must observe, and through which guard:
//!
//!   * `Promise.prototype.then = f` / `.constructor = x` — an IN-PLACE data
//!     write, which deliberately does NOT bump a version (access.rs: "no
//!     shape change, so no version bump"), so slot VALUES are re-read on
//!     every call and never trusted from the cache;
//!   * `defineProperty` redefinitions (accessor `constructor`, a replaced
//!     `@@species`) — always bump the owning object's version (define.rs),
//!     so the cache misses and the full proof re-runs;
//!   * a subclass instance — its `proto_of` entry names the SUBCLASS
//!     prototype, so the instance checks decline before the shared proof is
//!     even consulted, cache or no cache.
//!
//! Every `cache_parity_*` expectation below was executed in node v24.12.0
//! (`node <case>.js`) and diffs byte-identical — including the microtask
//! ORDERING pin, which moves by exactly the two adoption rounds the spec
//! adds once `constructor` is patched. The whole set re-runs in a child
//! process with `ZIPP_NO_PROMISE_SLOT_CACHE=1` (the old per-call proof), so
//! both modes are held to the same node-derived outputs.

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
fn cache_parity_a_long_then_chain_threads_values() {
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
fn cache_parity_an_await_loop_over_a_plain_promise_accumulates() {
    // The Await fast lane: `promise_plain_unobservable` per iteration.
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
fn cache_parity_an_await_loop_over_numbers_accumulates() {
    let out = run_ok(
        r#"
        "use strict";
        async function f() {
          var s = 0;
          for (var i = 0; i < 3000; i++) s += await i;
          return s;
        }
        f().then(function (v) { console.log("sum:" + v); });
        "#,
    );
    assert_eq!(out[0], "sum:4498500");
}

#[test]
fn cache_parity_reassigning_prototype_then_mid_chain_is_observed_from_that_point() {
    // The soundness-critical mutation: `then = f` overwrites the value slot
    // IN PLACE — no version bump anywhere — so only the per-call value
    // re-read can catch it. 1000 warm calls first, then the patch.
    let out = run_ok(
        r#"
        "use strict";
        var orig = Promise.prototype.then;
        var p = Promise.resolve(0);
        function inc(x) { return x + 1; }
        for (var i = 0; i < 1000; i++) p = p.then(inc);
        var patched = 0;
        Promise.prototype.then = function (f, r) { patched++; return orig.call(this, f, r); };
        for (var j = 0; j < 5; j++) p = p.then(inc);
        p.then(function (v) { console.log(v + "/" + patched); });
        "#,
    );
    assert_eq!(out[0], "1005/6");
}

#[test]
fn cache_parity_promise_all_calls_a_patched_then_per_element() {
    // The Promise.all element lane: after a warm 1000-element combinator,
    // a patched `then` must be Invoked per element (3) plus the outer then.
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
fn cache_parity_replacing_prototype_constructor_mid_chain_routes_species() {
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
fn cache_parity_redefining_species_on_promise_mid_chain_runs_the_getter() {
    // `defineProperty(Promise, @@species, {get})` bumps the ctor version —
    // the cache must miss and the getter must run once per later `.then`.
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
fn cache_parity_a_constructor_accessor_mid_chain_is_read_per_then() {
    // Redefining `constructor` AS AN ACCESSOR bumps the proto version (the
    // accessor-bit case a stale cache would get wrong); the getter must run
    // once per later `.then`. (The `then`-as-accessor sibling stays pinned —
    // with its known gets=2 divergence — in promise_pristine_dispatch.rs.)
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
fn cache_parity_awaiting_a_constructor_patched_promise_adds_the_adoption_ticks() {
    // After 1000 warm awaits, an in-place `constructor` overwrite (no bump)
    // must push the await onto the spec's NewPromiseCapability route: node
    // delivers `after=x` two rounds LATER than the pristine control
    // (`after=x|t1|…` unpatched vs `t1|t2|after=x|t3|…` patched).
    let out = run_ok(
        r#"
        "use strict";
        var order = [];
        async function one(p, tag) { var r = await p; order.push(tag + "=" + r); }
        async function main() {
          var p = Promise.resolve("x");
          for (var i = 0; i < 1000; i++) await p;
          Promise.prototype.constructor = function Nope() {};
          one(p, "after");
          var t = Promise.resolve();
          for (var n = 1; n <= 8; n++) (function (n) {
            t = t.then(function () { order.push("t" + n); });
          })(n);
          await t;
          await null; await null; await null; await null;
          console.log(order.join("|"));
        }
        main();
        "#,
    );
    assert_eq!(out[0], "t1|t2|after=x|t3|t4|t5|t6|t7|t8");
}

#[test]
fn cache_parity_a_subclass_with_overridden_species_constructs_via_the_species() {
    // A subclass instance never consults the cache (its proto_of entry fails
    // the instance check); its overridden @@species must still route `.then`
    // to plain %Promise%, while a plain subclass keeps constructing itself.
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

#[test]
fn cache_parity_thenable_adoption_still_runs_the_then() {
    let out = run_ok(
        r#"
        "use strict";
        var log = [];
        var thenable = {
          then: function (res) { log.push("adopt"); res(41); }
        };
        Promise.resolve(0).then(function () { return thenable; }).then(function (v) {
          log.push("got:" + (v + 1));
          console.log(log.join("|"));
        });
        "#,
    );
    assert_eq!(out[0], "adopt|got:42");
}

#[test]
fn cache_parity_restoring_the_intrinsic_then_is_seen_immediately() {
    // Patch and restore are BOTH in-place value writes (no bumps): the
    // wrapper must count exactly the one call made while it was installed.
    let out = run_ok(
        r#"
        "use strict";
        var orig = Promise.prototype.then;
        var calls = 0;
        Promise.prototype.then = function (f, r) { calls++; return orig.call(this, f, r); };
        var p = Promise.resolve(0).then(function (x) { return x + 1; });
        Promise.prototype.then = orig;
        var q = p.then(function (x) { return x + 1; });
        q.then(function (v) { console.log(v + "/" + calls); });
        "#,
    );
    assert_eq!(out[0], "2/1");
}

#[test]
fn the_off_switch_selects_the_old_proof_with_identical_behavior() {
    // The env latch is read once per process, so the off mode needs its own
    // process: re-run every `cache_parity_*` expectation in a child with
    // `ZIPP_NO_PROMISE_SLOT_CACHE=1`. The same node-derived assertions
    // passing in both modes IS the both-modes parity check.
    let exe = std::env::current_exe().expect("test exe path");
    let out = std::process::Command::new(exe)
        .arg("cache_parity_")
        .env("ZIPP_NO_PROMISE_SLOT_CACHE", "1")
        .output()
        .expect("spawn the test binary");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "off-mode run failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // The filter must have matched the parity set, not zero tests.
    assert!(
        !stdout.contains("running 0 tests"),
        "the cache_parity_ filter matched nothing:\n{stdout}"
    );
}
