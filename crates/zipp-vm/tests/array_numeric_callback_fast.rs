use zipp_vm::embed;

fn run_ok(source: &str) -> Vec<String> {
    let mut state = embed::compile_script(source).expect("source compiles");
    state.run_init().expect("source runs");
    state.take_output()
}

#[test]
fn exact_numeric_pipeline_matches_javascript_results() {
    let output = run_ok(
        r#"
        let a = [];
        for (let i = 0; i < 30; i++) a.push(i);
        let r = a.map(x => x * 2)
                 .filter(x => x % 3 === 0)
                 .reduce((p, c) => p + c, 0);
        console.log(String(r));
        "#,
    );
    assert_eq!(output, ["270"]);
}

#[test]
fn non_numbers_fall_back_and_preserve_live_array_semantics() {
    let output = run_ok(
        r#"
        let a = [1, { valueOf() { a[2] = 9; return 2; } }, 3];
        let mapped = a.map(x => x * 2);

        let filtered = [-0, 0, 3, -3, 4, NaN, Infinity]
            .filter(x => x % 3 === 0);

        let b = [1, { valueOf() { b[2] = 9; return 2; } }, 3];
        let reduced = b.reduce((p, c) => p + c, 0);

        console.log(mapped.join(",") + "|" + filtered.length + "|"
            + Object.is(filtered[0], -0) + "|" + reduced);
        "#,
    );
    assert_eq!(output, ["2,4,18|4|true|12"]);

    let mut bigint =
        embed::compile_script("[1n].map(x => x * 2);").expect("BigInt fallback source compiles");
    let error = bigint
        .run_init()
        .expect_err("mixed BigInt/Number multiplication must throw");
    assert!(error.contains("BigInt"), "unexpected error: {error}");
}

#[cfg(all(feature = "meter-only", not(feature = "jit")))]
mod metering {
    use super::*;

    const FAST: &str = r#"
        let a = [];
        for (let i = 0; i < 256; i++) a.push(i);
        let r = a.map(x => x * 2)
                 .filter(x => x % 3 === 0)
                 .reduce((p, c) => p + c, 0);
        console.log(String(r));
    "#;

    // Each callback has the same opcode count and numeric result as FAST, but
    // its operand order is outside the exact recogniser.  This is the metering
    // A/B oracle: off-frame execution must bill exactly what real frames bill.
    const FRAME_CONTROL: &str = r#"
        let a = [];
        for (let i = 0; i < 256; i++) a.push(i);
        let r = a.map(x => 2 * x)
                 .filter(x => 0 === x % 3)
                 .reduce((p, c) => c + p, 0);
        console.log(String(r));
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
    fn exact_fast_callbacks_match_frame_metering_and_budget_edges() {
        const BUDGET: u64 = 5_000_000;
        let (fast_result, fast_output, fast_used, fast_remaining) = metered(FAST, BUDGET);
        fast_result.expect("fast numeric pipeline runs");
        let (frame_result, frame_output, frame_used, frame_remaining) =
            metered(FRAME_CONTROL, BUDGET);
        frame_result.expect("frame control pipeline runs");

        assert_eq!(fast_output, frame_output);
        assert_eq!(fast_used, frame_used, "callback opcode billing diverged");
        assert_eq!(fast_used + fast_remaining, BUDGET);
        assert_eq!(frame_used + frame_remaining, BUDGET);

        let (exact_result, _, exact_used, exact_remaining) = metered(FAST, fast_used);
        exact_result.expect("the exact final allowance succeeds");
        assert_eq!(exact_used, fast_used);
        assert_eq!(exact_remaining, 0);

        let (short_result, _, short_used, short_remaining) = metered(FAST, fast_used - 1);
        let error = short_result.expect_err("one fewer callback/body step is rejected");
        assert!(
            error.contains("instruction budget"),
            "unexpected error: {error}"
        );
        assert_eq!(short_used, fast_used - 1);
        assert_eq!(short_remaining, 0);
    }
}
