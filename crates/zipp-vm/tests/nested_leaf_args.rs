//! The nested-leaf splice accepts inner calls WITH ARGUMENTS (B76).
//!
//! `splice_nested_leaf` used to reject any wrapper whose inner call passed args
//! (`argc != 0`), which the B75/B76 surveys showed was every remaining nested
//! reject on the call-heavy rows. Params are now seeded with plain `Move`s after
//! the guard marker; params the call leaves unfilled keep the emitter's
//! undefined zero-fill. Measured −55.1% on a 3M-call wrapper micro (136ms →
//! 61ms), faster than node.
//!
//! Every expectation here was executed in node and diffs byte-identical.

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    out.output
}

const HOT: usize = 4000;

#[test]
fn args_flow_through_the_splice() {
    let out = run_ok(&format!(
        r#"
        "use strict";
        function inner(a, b) {{ return (a * 3 + b) | 0; }}
        function wrap(n) {{ return inner(n, 7) + 1; }}
        var s = 0;
        for (var i = 0; i < {HOT}; i++) s = (s + wrap(i & 15)) | 0;
        console.log(s + "," + wrap(100));
        "#
    ));
    assert_eq!(out[0], format!("{},308", (0..HOT).map(|i| (i & 15) * 3 + 7 + 1).sum::<usize>()));
}

#[test]
fn missing_params_read_undefined() {
    // inner has 2 params, the wrapper passes 1: `b` is undefined, so `b | 0` is 0.
    let out = run_ok(&format!(
        r#"
        "use strict";
        function inner(a, b) {{ return ((a | 0) + (b | 0)) | 0; }}
        function wrap(n) {{ return inner(n); }}
        var s = 0;
        for (var i = 0; i < {HOT}; i++) s = wrap(5);
        console.log(s + "," + (undefined | 0));
        "#
    ));
    assert_eq!(out[0], "5,0");
}

#[test]
fn extra_args_are_evaluated_but_unbound() {
    let out = run_ok(&format!(
        r#"
        "use strict";
        var evals = 0;
        function bump() {{ evals++; return 9; }}
        function inner(a) {{ return a + 1; }}
        function wrap(n) {{ return inner(n, bump()); }}
        var s = 0;
        for (var i = 0; i < {HOT}; i++) s = wrap(1);
        console.log(s + " evals=" + evals);
        "#
    ));
    assert_eq!(out[0], format!("2 evals={HOT}"));
}

#[test]
fn a_void_inner_return_reads_undefined() {
    // Exercises the emitter's LoadUndefined arm, which did not exist: a spliced
    // inner ending in ReturnUndefined hit `unreachable!` at region compile time
    // under panic=abort. Latent until args widened what reaches the emitter.
    let out = run_ok(&format!(
        r#"
        "use strict";
        var hits = 0;
        function inner(a) {{ hits = (hits + a) | 0; }}
        function wrap(n) {{ return inner(n); }}
        var last = "x";
        for (var i = 0; i < {HOT}; i++) last = wrap(1);
        console.log(typeof last + " hits=" + hits);
        "#
    ));
    assert_eq!(out[0], format!("undefined hits={HOT}"));
}

#[test]
fn rebinding_the_inner_mid_loop_is_observed() {
    // The splice guards the wrapper's callee identity per call; rebinding the
    // INNER global must fall back and call the new function.
    let out = run_ok(&format!(
        r#"
        "use strict";
        var inner = function (a) {{ return a + 1; }};
        function wrap(n) {{ return inner(n); }}
        var first = 0, last = 0;
        for (var i = 0; i < {HOT}; i++) {{
          if (i === {HOT} / 2) inner = function (a) {{ return a * 100; }};
          var r = wrap(2);
          if (i === 0) first = r; last = r;
        }}
        console.log(first + "," + last);
        "#
    ));
    assert_eq!(out[0], "3,200");
}
