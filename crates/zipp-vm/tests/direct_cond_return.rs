//! Direct branch returns for the narrow recursive-base-case conditional.
//!
//! `return n < 2 ? n : alt` (and `<=`) can return from each arm instead of
//! materialising a shared conditional destination. The true/base arm thereby
//! loses its `Move` and the unconditional `Jump` over `alt`. The recogniser is
//! deliberately narrow; these tests also pin contexts which must keep the
//! ordinary return protocol and the pre-existing Pad2/tail-call lowerings.

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}",
        out.error
    );
    out.output
}

fn text_of(src: &str) -> String {
    zipp_vm::compile_to_text(src, false).expect("source compiles")
}

#[test]
fn fib_lt_and_le_emit_branch_local_returns() {
    for (op, guard_op) in [("<", "JumpIfNotLt {"), ("<=", "JumpIfNotLe {")] {
        let src = format!("function fib(n) {{ return n {op} 2 ? n : fib(n - 1) + fib(n - 2); }}");
        let bc = text_of(&src);
        assert_eq!(
            bc.matches(guard_op).count(),
            1,
            "comparison guard shape changed:\n{bc}"
        );
        assert_eq!(
            bc.matches("Return {").count(),
            2,
            "both conditional arms should return directly:\n{bc}"
        );
        assert!(
            !bc.contains("Move {") && !bc.contains("Jump {"),
            "base path retained the conditional Move/Jump:\n{bc}"
        );
    }
}

#[test]
fn selected_arm_and_binding_reread_semantics_are_preserved() {
    let out = run_ok(
        r#"
        var log = [];
        function choose(n) {
          return n < 2 ? n : (log.push("alt:" + n), n + 40);
        }
        function reread(n) {
          var args = arguments;
          n = { valueOf: function () {
            log.push("coerce");
            args[0] = 7;
            return 1;
          }};
          return n < 2 ? n : (log.push("wrong-alt"), 99);
        }
        console.log(choose(1), choose(3), reread(0), log.join(","));
        "#,
    );
    assert_eq!(out, ["1 43 7 alt:3,coerce"]);
}

#[test]
fn return_still_runs_try_finally_on_both_arms() {
    let out = run_ok(
        r#"
        var log = [];
        function f(n) {
          try { return n <= 2 ? n : (log.push("alt"), 9); }
          finally { log.push("finally:" + n); }
        }
        console.log(f(2), f(3), log.join(","));
        "#,
    );
    assert_eq!(out, ["2 9 finally:2,alt,finally:3"]);
}

#[test]
fn iterator_close_context_declines_and_closes_before_returning() {
    let src = r#"
        var log = [];
        var iterable = {
          [Symbol.iterator]: function () {
            var done = false;
            return {
              next: function () {
                if (done) return { done: true };
                done = true;
                return { done: false, value: 1 };
              },
              return: function () { log.push("close"); return { done: true }; }
            };
          }
        };
        function f(n) {
          for (var x of iterable) return n < 2 ? n : 9;
        }
        console.log(f(1), f(3), log.join(","));
    "#;
    assert_eq!(run_ok(src), ["1 9 close,close"]);

    // Keep the shape probe free of helper function literals so its Return
    // count describes only `f`; the semantic source above deliberately has
    // `next`/`return` methods of its own.
    let bc = text_of("function f(n) { for (var x of [1]) return n < 2 ? n : 9; }");
    assert_eq!(
        bc.matches("Return {").count(),
        1,
        "iterator-close return incorrectly split into branch Returns:\n{bc}"
    );
}

#[test]
fn async_generator_keeps_await_return_protocol() {
    let bc = text_of("async function* g(n) { return n < 2 ? n : 9; }");
    assert!(
        bc.contains("Await {") && bc.matches("Return {").count() == 1,
        "async-generator return bypassed Await or split its Return:\n{bc}"
    );
}

#[test]
fn sync_generator_branch_returns_keep_completion_values() {
    let out = run_ok(
        r#"
        function* g(n) { return n < 2 ? n : 9; }
        var a = g(1).next(), b = g(3).next();
        console.log(a.done, a.value, b.done, b.value);
        "#,
    );
    assert_eq!(out, ["true 1 true 9"]);
}

#[test]
fn proper_tail_call_lowering_remains_authoritative() {
    let bc = text_of(
        r#""use strict";
        function target(n) { return n; }
        function f(n) { return n < 2 ? n : target(n); }
        "#,
    );
    assert!(
        bc.contains("TailCall {") || bc.contains("TailCallWithThis {"),
        "direct conditional return stole a proper-tail-call site:\n{bc}"
    );
}

#[test]
fn pad2_conditional_specialisation_is_unchanged() {
    let bc = text_of(
        r#""use strict";
        function pad2(n) { return n < 10 ? "0" + n : "" + n; }
        "#,
    );
    assert_eq!(
        bc.matches("Pad2Conditional {").count(),
        1,
        "direct return stole or duplicated Pad2Conditional:\n{bc}"
    );
    assert_eq!(
        bc.matches("Return {").count(),
        1,
        "Pad2Conditional should retain its one shared Return:\n{bc}"
    );
}
