//! Generic-binary LHS scratch reclamation.
//!
//! The compiler may reuse temporaries created while evaluating a completed LHS,
//! but the returned LHS value and every pre-existing local/outer register remain
//! live until the binary opcode consumes them after RHS evaluation.

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}",
        out.error
    );
    out.output
}

fn named_reg_count(text: &str, name: &str) -> u16 {
    let marker = format!("name: \"{name}\",");
    let tail = text
        .split_once(&marker)
        .unwrap_or_else(|| panic!("missing function {name:?}:\n{text}"))
        .1;
    let digits = tail
        .split_once("reg_count: ")
        .unwrap_or_else(|| panic!("missing reg_count for {name:?}:\n{text}"))
        .1
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>();
    digits.parse().expect("reg_count is a u16")
}

#[test]
fn fib_frame_is_nine_registers() {
    let text = zipp_vm::compile_to_text(
        "function fib(n){ return n < 2 ? n : fib(n-1) + fib(n-2); } fib(8);",
        false,
    )
    .expect("fib compiles");
    assert_eq!(named_reg_count(&text, "fib"), 9, "{text}");
    assert_eq!(
        text.matches("Call {").count(),
        3,
        "two fib calls plus script call"
    );
    assert_eq!(
        text.matches("Add {\n").count(),
        1,
        "binary opcode shape changed"
    );
}

#[test]
fn operand_evaluation_and_coercion_order_are_unchanged() {
    let out = run_ok(
        r#"
        var log = [];
        function operand(label, value) {
          log.push(label + ":eval");
          return { valueOf: function () { log.push(label + ":coerce"); return value; } };
        }
        var result = operand("L", 20) + operand("R", 22);
        console.log(result, log.join(","));
        "#,
    );
    assert_eq!(out, ["42 L:eval,R:eval,L:coerce,R:coerce"]);
}

#[test]
fn coercion_exception_runs_after_both_evaluations_and_skips_rhs_coercion() {
    let out = run_ok(
        r#"
        var log = [];
        function operand(label, fail) {
          log.push(label + ":eval");
          return { valueOf: function () {
            log.push(label + ":coerce");
            if (fail) throw new Error(label);
            return 2;
          }};
        }
        try { operand("L", true) * operand("R", false); }
        catch (e) { log.push("caught:" + e.message); }
        console.log(log.join(","));
        "#,
    );
    assert_eq!(out, ["L:eval,R:eval,L:coerce,caught:L"]);
}

#[test]
fn nested_calls_do_not_clobber_existing_locals() {
    let out = run_ok(
        r#"
        var log = [];
        function step(tag, value) { log.push(tag); return value; }
        function wrap(value) { log.push("wrap" + value); return value; }
        function combine(a, b) {
          let k0 = 10, k1 = 20, k2 = 30, k3 = 40;
          let sum = wrap(step("L", a)) + wrap(step("R", b));
          return (k0 + k1 + k2 + k3) + ":" + sum;
        }
        console.log(combine(7, 8), log.join(","));
        "#,
    );
    assert_eq!(out, ["100:15 L,wrap7,R,wrap8"]);
}

#[test]
fn nested_binary_local_results_preserve_the_outer_floor() {
    assert_eq!(
        run_ok(
            r#"
            function calc(a, b, c, d) {
              return ((a * b) + (c * d)) * ((a - d) + (b * c));
            }
            console.log(calc(2, 3, 4, 5));
            "#,
        ),
        ["234"]
    );
}

#[test]
fn local_and_this_results_that_bypass_the_requested_dst_stay_live() {
    let out = run_ok(
        r#"
        var log = [];
        function touch(label) { log.push(label); }
        var holder = {
          check: function (a, b) {
            let localA = a, localB = b;
            let sum = (touch("left"), localA) + (touch("right"), localB);
            let same = (touch("this-left"), this) === (touch("this-right"), this);
            return sum + ":" + same;
          }
        };
        console.log(holder.check(4, 5), log.join(","));
        "#,
    );
    assert_eq!(out, ["9:true left,right,this-left,this-right"]);
}

#[test]
fn module_high_result_register_is_not_reused_by_the_rhs() {
    let out = zipp_vm::run_module_with_base("console.log(import.meta === 1);", None)
        .expect("module compiles and runs");
    assert!(
        out.error.is_none(),
        "unexpected module error: {:?}",
        out.error
    );
    assert_eq!(out.output, ["false"]);
}

#[test]
fn closure_graph_in_the_lhs_survives_rhs_allocation_and_gc() {
    let out = run_ok(
        r#"
        function make(value) {
          let captured = { number: value };
          return { valueOf: function () { return captured.number; } };
        }
        function churn() {
          let items = [];
          for (let i = 0; i < 96; i++) {
            items.push({ index: i, payload: [i, i + 1, i + 2] });
          }
          return items.length === 96 ? 2 : 1000;
        }
        console.log(make(40) + churn());
        "#,
    );
    assert_eq!(out, ["42"]);
}

#[test]
fn left_and_right_exceptions_keep_source_order_and_skip_the_binary() {
    let out = run_ok(
        r#"
        var log = [];
        function leftThrow() { log.push("Lthrow"); throw new Error("left"); }
        function never() { log.push("never"); return 1; }
        function left() { log.push("L"); return 1; }
        function rightThrow() { log.push("Rthrow"); throw new Error("right"); }
        try { leftThrow() + never(); } catch (e) { log.push("catchL"); }
        try { left() + rightThrow(); } catch (e) { log.push("catchR"); }
        console.log(log.join(","));
        "#,
    );
    assert_eq!(out, ["Lthrow,catchL,L,Rthrow,catchR"]);
}

#[test]
fn recursive_binary_result_is_unchanged() {
    assert_eq!(
        run_ok("function fib(n){ return n < 2 ? n : fib(n-1) + fib(n-2); } console.log(fib(10));"),
        ["55"]
    );
}

#[cfg(feature = "instrument")]
#[test]
fn metered_and_traced_dispatch_observes_the_reclaimed_frame() {
    let mut state = zipp_vm::embed::compile_script(
        "function fib(n){ return n < 2 ? n : fib(n-1) + fib(n-2); } console.log(fib(8));",
    )
    .expect("instrumented source compiles");
    state.set_limits(1_000_000, None);
    state.start_trace(1 << 18);
    state.run_init().expect("instrumented source runs");
    assert_eq!(state.take_output(), ["21"]);
    let trace = state.finish_trace(0).expect("trace stays below its cap");
    assert!(trace.len() > 100, "recursive trace unexpectedly short");
}
