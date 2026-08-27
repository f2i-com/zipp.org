//! B216: `hoistable_length`'s "is this global mutated in the region?" check ran
//! as ONE forward pass that compared each `StoreGlobal` against the global
//! discovered so far — so a store standing BEFORE the load in instruction order
//! was examined while that global was still unknown and went unseen. The hoist
//! was then admitted for a global the loop reassigns every iteration, and the
//! region served the region-entry value's length for the whole loop.
//!
//! Every expectation below is node-oracled (v24.12.0) and equals the
//! interpreter's answer; each case must hold with the JIT on.

fn run(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(out.error.is_none(), "unexpected error: {:?}", out.error);
    out.output
}

/// The original wrong answer: store-then-load of a global string, its `.length`
/// read every iteration. Lengths alternate 6 ("prop_0".."prop_9") and 7
/// ("prop_10".."prop_59"), so a correct pass sums 10*6 + 50*7 = 410 per sweep.
#[test]
fn store_before_load_forbids_the_length_hoist() {
    let out = run(r#"
        "use strict";
        var acc = 0;
        for (var d = 0; d < 3000; d++) {
          for (var p = 0; p < 60; p++) { var s = "prop_" + p; acc = (acc + s.length) | 0; }
        }
        console.log("acc=" + acc);
        "#);
    assert_eq!(out, ["acc=1230000"]); // node v24
}

/// The same hazard with an ARRAY global: the loop rebinds `a` to a fresh array
/// of a different length before reading `a.length`.
#[test]
fn store_before_load_forbids_the_hoist_for_arrays() {
    let out = run(r#"
        "use strict";
        var total = 0;
        for (var d = 0; d < 2000; d++) {
          for (var k = 0; k < 40; k++) {
            var a = new Array(k % 7);
            total = (total + a.length) | 0;
          }
        }
        console.log("total=" + total);
        "#);
    assert_eq!(out, ["total=230000"]); // node v24
}

/// The hoist REMAINS available where it is legal: a global the region only
/// reads. This pins the fix as a narrowing, not a blanket disable.
#[test]
fn a_read_only_global_still_hoists_and_answers_right() {
    let out = run(r#"
        "use strict";
        var text = "abcdefghij";
        var n = 0;
        for (var d = 0; d < 5000; d++) {
          for (var i = 0; i < text.length; i++) { n = (n + text.charCodeAt(i)) | 0; }
        }
        console.log("n=" + n);
        "#);
    assert_eq!(out, ["n=5075000"]); // node v24
}

/// B217: the reject scan was a BLACKLIST of four ops and went stale as region
/// admission widened. A COMPUTED method call (`fns[0](p)` — `CallMethodComputed`,
/// never on the list) runs user code that pushes/pops the very array whose
/// `.length` is hoisted.
#[test]
fn a_computed_call_that_mutates_the_container_forbids_the_hoist() {
    let out = run(r#"
        var a = [1, 2, 3];
        var fns = [function (x) { if (x % 2) { a.push(x); } else if (a.length > 3) { a.pop(); } return 0; }];
        var t = 0;
        for (var d = 0; d < 5000; d++) {
          for (var p = 0; p < 60; p++) {
            fns[0](p);
            t = (t + a.length) | 0;
          }
        }
        console.log("t=" + t + " len=" + a.length);
        "#);
    assert_eq!(out, ["t=1050000 len=4"]); // node v24
}

/// B217 again, through a completely different door: `delete px[k]` is
/// `DeleteIndexConcat`, and a Proxy `deleteProperty` trap is user code. The
/// accumulator is order-sensitive so a stale read cannot cancel out.
#[test]
fn a_proxy_delete_trap_forbids_the_hoist() {
    let out = run(r#"
        var s = "123456789";
        var px = new Proxy({}, { deleteProperty: function (t, k) { s = (s.length === 9) ? "x" : "123456789"; return true; } });
        var acc = 0;
        for (var d = 0; d < 20000; d++) {
          for (var p = 0; p < 8; p++) {
            delete px["key_" + p];
            acc = (acc * 3 + s.length) | 0;
          }
        }
        console.log("acc=" + acc);
        "#);
    assert_eq!(out, ["acc=-1459253760"]); // node v24
}

/// A store AFTER the load in instruction order — the direction the old
/// single-pass check did catch. Kept so a future rewrite cannot regress it.
#[test]
fn store_after_load_forbids_the_hoist_too() {
    let out = run(r#"
        "use strict";
        var s = "xxxxx";
        var acc = 0;
        for (var d = 0; d < 3000; d++) {
          for (var p = 0; p < 60; p++) {
            acc = (acc + s.length) | 0;
            s = "prop_" + p;
          }
        }
        console.log("acc=" + acc);
        "#);
    assert_eq!(out, ["acc=1229998"]); // node v24
}
