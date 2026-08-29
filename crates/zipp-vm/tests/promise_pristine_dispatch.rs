//! `p.then()` proves the intrinsic WITHOUT walking the prototype chain (B79).
//!
//! `dispatch_builtin_method_inner`'s Promise arm has always had to prove that
//! `then`/`catch`/`finally` really resolve to the intrinsic native before taking
//! the inline path — an own shadow, a patched `Promise.prototype.then`, or a
//! subclass override must win, and test262 observes all three. It proved it with
//! a full `get_prop(recv, name)`, and a Promise receiver misses `get_member`'s
//! fast path on the heap discriminant, so that meant `get_member_slow`'s exotic
//! preamble plus a chain walk on EVERY call.
//!
//! `promise_method_is_intrinsic` decides the same question from three cheap
//! reads — the `proto_of` slot table, the `arr_props` own-property side table,
//! and one `pos` on %Promise.prototype% — which is the guard shape B69 already
//! uses for `re.test`/`re.exec`. Measured: **100ns -> 87ns per `.then()`**
//! (node 73ns).
//!
//! The probe sits behind a `||`, so declining runs the ORIGINAL expression
//! unchanged; `ZIPP_NO_PROMISE_PRISTINE=1` forces that and was used to confirm
//! every expectation here is identical on both paths.
//!
//! `async-promise-chain` makes 1,500,003 of these calls and nothing else — they
//! are 100% of that row's builtin dispatches (`ZIPP_BUILTINSTATS=1`).
//!
//! Every expectation was executed in node as a SCRIPT (`vm.runInThisContext`)
//! and diffs byte-identical.

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
fn an_own_shadow_on_the_instance_wins() {
    let out = run_ok(
        r#"
        "use strict";
        var q = Promise.resolve("base");
        q.then = function () { return "OWN"; };
        console.log(q.then());
        "#,
    );
    assert_eq!(out[0], "OWN");
}

#[test]
fn a_patched_prototype_then_is_observed() {
    let out = run_ok(
        r#"
        "use strict";
        var orig = Promise.prototype.then;
        var seen = 0;
        Promise.prototype.then = function (f) { seen++; return orig.call(this, f); };
        Promise.resolve(7).then(function () {});
        Promise.prototype.then = orig;
        console.log(seen);
        "#,
    );
    assert_eq!(out[0], "1");
}

#[test]
fn a_patched_prototype_catch_is_observed() {
    // `catch` and `finally` go through the same arm and the same probe.
    let out = run_ok(
        r#"
        "use strict";
        var orig = Promise.prototype.catch;
        var seen = 0;
        Promise.prototype.catch = function (f) { seen++; return orig.call(this, f); };
        Promise.resolve(1).catch(function () {});
        Promise.prototype.catch = orig;
        console.log(seen);
        "#,
    );
    assert_eq!(out[0], "1");
}

#[test]
fn a_subclass_override_wins() {
    // A subclass instance carries an EXPLICIT `proto_of` entry naming the
    // subclass prototype, so the probe declines on its first check.
    let out = run_ok(
        r#"
        "use strict";
        var hits = 0;
        class MyP extends Promise {
          then(f, g) { hits++; return super.then(f, g); }
        }
        var s = new MyP(function (res) { res(1); });
        s.then(function () {});
        console.log(hits);
        "#,
    );
    assert_eq!(out[0], "1");
}

#[test]
fn replacing_the_prototype_method_with_a_non_native_is_observed() {
    // The probe's third check: %Promise.prototype% must still hold the
    // intrinsic native at that key. A plain function there must win.
    let out = run_ok(
        r#"
        "use strict";
        var orig = Promise.prototype.then;
        Promise.prototype.then = function () { return "PATCHED"; };
        var r = Promise.resolve(1).then(function () {});
        Promise.prototype.then = orig;
        console.log(r);
        "#,
    );
    assert_eq!(out[0], "PATCHED");
}

#[test]
fn an_accessor_on_the_prototype_is_not_taken_as_intrinsic() {
    // `!m.attr_at(i).accessor` in the probe: a getter must RUN, and its RESULT is
    // what gets called.
    // Capturing the reference before evaluating arguments also ensures the
    // accessor runs exactly once, matching ECMAScript and Node.
    let out = run_ok(
        r#"
        "use strict";
        var orig = Object.getOwnPropertyDescriptor(Promise.prototype, "then");
        var gets = 0;
        Object.defineProperty(Promise.prototype, "then", {
          get: function () { gets++; return function () { return "VIA-GETTER"; }; },
          configurable: true
        });
        var r = Promise.resolve(1).then(function () {});
        Object.defineProperty(Promise.prototype, "then", orig);
        console.log(r + "/gets=" + gets);
        "#,
    );
    assert_eq!(out[0], "VIA-GETTER/gets=1");
}

#[test]
fn the_ordinary_chain_still_threads_values_and_runs_finally() {
    // Long enough to be the shape the row actually runs.
    let out = run_ok(
        r#"
        "use strict";
        var p = Promise.resolve(0);
        for (var i = 0; i < 2000; i++) p = p.then(function (x) { return x + 1; });
        var finRan = 0;
        p.finally(function () { finRan = 1; }).then(function (v) {
          console.log(v + "/" + finRan);
        });
        "#,
    );
    assert_eq!(out[0], "2000/1");
}

#[test]
fn a_rejection_still_reaches_catch() {
    let out = run_ok(
        r#"
        "use strict";
        Promise.reject(new Error("boom"))
          .catch(function (e) { return "caught:" + e.message; })
          .then(function (v) { console.log(v); });
        "#,
    );
    assert_eq!(out[0], "caught:boom");
}
