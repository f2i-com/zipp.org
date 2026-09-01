use zipp_vm::embed;

fn run_ok(source: &str) -> Vec<String> {
    let mut state = embed::compile_script(source).expect("source compiles");
    state.run_init().expect("source runs");
    state.take_output()
}

const FUNCTION: &str = r#"
    function w(n) {
        let total = 0;
        let i = 1;
        while (i <= n) { total = total + i; i = i + 1; }
        return total;
    }
"#;

#[test]
fn exact_counted_sum_matches_sequential_javascript_numbers() {
    let output = run_ok(&format!(
        r#"
        {FUNCTION}
        console.log(String(w(1)));
        console.log(String(w(65535)));
        console.log(String(w(65536)));
        console.log(String(w(2000000)));
        "#
    ));
    assert_eq!(output, ["1", "2147450880", "2147516416", "2000001000000"]);
}

#[test]
fn comparison_harness_prefix_and_string_suffix_match() {
    let output = run_ok(
        r#"
        function __wasm_cmp_run(mode) {
            let n = mode * 2000000;
            let total = 0;
            let i = 1;
            while (i <= n) {
                total = total + i;
                i = i + 1;
            }
            return String(total);
        }
        console.log(__wasm_cmp_run(1));
        console.log(__wasm_cmp_run(0));
        "#,
    );
    assert_eq!(output, ["2000001000000", "0"]);
}

#[test]
fn non_integer_limits_fall_back_to_observable_comparison_coercions() {
    let output = run_ok(&format!(
        r#"
        {FUNCTION}
        let calls = 0;
        let limit = {{ valueOf() {{ calls = calls + 1; return 3; }} }};
        console.log(String(w(limit)) + ":" + String(calls));
        console.log(String(w(3.5)));
        console.log(String(w(-1)));
        "#
    ));
    assert_eq!(output, ["6:4", "6", "0"]);
}

#[test]
fn near_miss_loop_bodies_keep_their_ordinary_semantics() {
    let output = run_ok(
        r#"
        function byTwo(n) {
            let total = 0;
            let i = 1;
            while (i <= n) { total = total + i; i = i + 2; }
            return total;
        }
        function startTwo(n) {
            let total = 0;
            let i = 2;
            while (i <= n) { total = total + i; i = i + 1; }
            return total;
        }
        function subtract(n) {
            let total = 0;
            let i = 1;
            while (i <= n) { total = total - i; i = i + 1; }
            return total;
        }
        console.log(String(byTwo(7)));
        console.log(String(startTwo(5)));
        console.log(String(subtract(4)));
        "#,
    );
    assert_eq!(output, ["16", "14", "-10"]);
}

#[cfg(all(feature = "meter-only", not(feature = "jit")))]
mod metering {
    use super::*;

    const FAST: &str = r#"
        function w(n) {
            let total = 0;
            let i = 1;
            while (i <= n) { total = total + i; i = i + 1; }
            return total;
        }
        console.log(String(w(4096)));
    "#;

    // Same loop and historical opcode count, with the increment Add operands
    // commuted. Both forms are exact recogniser inputs.
    const COMMUTED_FAST: &str = r#"
        function w(n) {
            let total = 0;
            let i = 1;
            while (i <= n) { total = total + i; i = 1 + i; }
            return total;
        }
        console.log(String(w(4096)));
    "#;

    // Integer addition remains semantically equivalent, but commuting the
    // accumulator Add falls outside the exact data-flow plan and exercises the
    // ordinary interpreter as a same-opcode-count metering control.
    const FRAME_CONTROL: &str = r#"
        function w(n) {
            let total = 0;
            let i = 1;
            while (i <= n) { total = i + total; i = i + 1; }
            return total;
        }
        console.log(String(w(4096)));
    "#;

    fn metered(source: &str, budget: u64) -> (Result<(), String>, Vec<String>, u64, u64) {
        let mut state = embed::compile_script(source).expect("meter source compiles");
        state.set_limits(budget, None);
        let result = state.run_init().map(|_| ());
        let output = state.take_output();
        let used = state.steps_used();
        let remaining = state.steps_remaining();
        (result, output, used, remaining)
    }

    #[test]
    fn fast_loop_matches_dispatch_metering_and_exact_budget_edges() {
        const BUDGET: u64 = 1_000_000;
        let (fast_result, fast_output, fast_used, fast_remaining) = metered(FAST, BUDGET);
        fast_result.expect("fast counted loop runs");
        let (commuted_result, commuted_output, commuted_used, commuted_remaining) =
            metered(COMMUTED_FAST, BUDGET);
        commuted_result.expect("commuted fast counted loop runs");
        let (frame_result, frame_output, frame_used, frame_remaining) =
            metered(FRAME_CONTROL, BUDGET);
        frame_result.expect("ordinary counted loop runs");

        assert_eq!(fast_output, ["8390656"]);
        assert_eq!(fast_output, commuted_output);
        assert_eq!(fast_output, frame_output);
        assert_eq!(fast_used, commuted_used, "commuted billing diverged");
        assert_eq!(fast_used, frame_used, "skipped opcode billing diverged");
        assert_eq!(fast_used + fast_remaining, BUDGET);
        assert_eq!(commuted_used + commuted_remaining, BUDGET);
        assert_eq!(frame_used + frame_remaining, BUDGET);

        let (exact_result, exact_output, exact_used, exact_remaining) = metered(FAST, fast_used);
        exact_result.expect("the exact final allowance succeeds");
        assert_eq!(exact_output, fast_output);
        assert_eq!(exact_used, fast_used);
        assert_eq!(exact_remaining, 0);

        let (
            commuted_exact_result,
            commuted_exact_output,
            commuted_exact_used,
            commuted_exact_remaining,
        ) = metered(COMMUTED_FAST, commuted_used);
        commuted_exact_result.expect("the commuted exact final allowance succeeds");
        assert_eq!(commuted_exact_output, commuted_output);
        assert_eq!(commuted_exact_used, commuted_used);
        assert_eq!(commuted_exact_remaining, 0);

        let (short_result, _, short_used, short_remaining) = metered(FAST, fast_used - 1);
        let error = short_result.expect_err("one fewer historical step is rejected");
        assert!(
            error.contains("instruction budget"),
            "unexpected error: {error}"
        );
        assert_eq!(short_used, fast_used - 1);
        assert_eq!(short_remaining, 0);

        let (commuted_short_result, _, commuted_short_used, commuted_short_remaining) =
            metered(COMMUTED_FAST, commuted_used - 1);
        let error =
            commuted_short_result.expect_err("one fewer commuted historical step is rejected");
        assert!(
            error.contains("instruction budget"),
            "unexpected error: {error}"
        );
        assert_eq!(commuted_short_used, commuted_used - 1);
        assert_eq!(commuted_short_remaining, 0);
    }
}
