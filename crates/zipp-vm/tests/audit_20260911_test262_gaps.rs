//! Two engine defects surfaced by the fresh test262 run recorded in
//! HANDOFF.md (B309), reduced to the engine's own API so they stay pinned
//! whether or not the conformance corpus is at hand.
//!
//! 1. A native array-builtin callback entry that bailed PAST its first
//!    instruction was re-run from the top on the interpreter, repeating every
//!    side effect it had already performed — `[1].forEach(cb, thisArg)` counted
//!    two calls for one element (test262
//!    language/expressions/arrow-function/cannot-override-this-with-thisArg).
//!    The continuation now resumes at the bail ip over the same window.
//! 2. Named-property lookup stopped at an ARRAY spliced into a prototype chain
//!    (`Object.setPrototypeOf(a, [..])`): `a.push` read as undefined (test262
//!    built-ins/Array/prototype/copyWithin/coerced-values-start-change-start).

fn run_lines(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}; output: {:?}",
        out.error,
        out.output
    );
    out.output
}

/// The harness-shaped callback: a global counter, then a method call on a
/// FUNCTION object holding a helper (the shape test262's `assert` has), inside
/// a `forEach` with a `thisArg`. The counter must move exactly once per
/// element under the default (JIT) configuration, the interpreter, and a
/// forced tier, and the arrow's `this` must stay lexical.
#[test]
fn a_native_callback_bail_never_repeats_side_effects() {
    let src = r#"
        var calls = 0; var seen = [];
        var u = {};
        function helper() {}
        helper.same = function (a, b) { return a === b; };
        var detached = helper.same;
        [1, 2].forEach(v => { calls++; seen.push(helper.same(this, u)); }, u);
        [1].forEach(v => { calls++; detached(1, 2); }, u);
        [1].forEach(v => { calls++; helper.same.call(helper, 1, 1); }, u);
        [1].forEach(function (v) { calls++; helper.same(this, u); }, u);
        var mapped = [1, 2, 3].map(v => { calls++; helper.same(v, v); return v * 2; }, u);
        console.log(calls, seen.join(","), mapped.join(","));
    "#;
    assert_eq!(run_lines(src), ["8 false,false 2,4,6"]);
}

#[test]
fn a_callback_that_throws_after_a_bail_throws_once() {
    let src = r#"
        var calls = 0; var u = {};
        function helper() {}
        helper.boom = function () { throw new Error("boom"); };
        var caught = "";
        try { [1].forEach(v => { calls++; helper.boom(); }, u); } catch (e) { caught = e.message; }
        console.log(calls, caught);
    "#;
    assert_eq!(run_lines(src), ["1 boom"]);
}

#[test]
fn an_array_in_the_prototype_chain_keeps_named_lookup_going() {
    let src = r#"
        var a = [1, 2];
        Object.setPrototypeOf(a, [3, 4]);
        var o = {};
        Object.setPrototypeOf(o, [3, 4]);
        var ownOnProtoArray = [5];
        ownOnProtoArray.extra = function () { return "extra"; };
        var b = [];
        Object.setPrototypeOf(b, ownOnProtoArray);
        console.log(
          typeof a.push, a.push === Array.prototype.push, typeof a.map, a.length, a[1],
          typeof o.push, o[1], typeof o.length,
          b.extra(), typeof b.slice, b[0]
        );
        // The test262 shape: copyWithin resolved through the array prototype,
        // with the receiver shortened while `start` is coerced and the source
        // elements then read through the prototype.
        function longDenseArray() { var r = [0]; for (var i = 0; i < 1024; i++) r[i] = i; return r; }
        var curr = longDenseArray();
        Object.setPrototypeOf(curr, longDenseArray());
        function shorten() { curr.length = 20; return 1000; }
        var result = curr.copyWithin(0, { valueOf: shorten });
        var expected = longDenseArray(); expected.length = 20;
        for (var i = 0; i < 24; i++) expected[i] = Object.getPrototypeOf(curr)[i + 1000];
        // Set(O, k, v) past the shortened length extends it: both arrays end at 24.
        var same = result.length === expected.length;
        for (var i = 0; i < expected.length; i++) if (result[i] !== expected[i]) same = false;
        console.log(result === curr, same, result.length);
    "#;
    assert_eq!(
        run_lines(src),
        [
            "function true function 2 2 function 4 number extra function 5",
            "true true 24",
        ]
    );
}
