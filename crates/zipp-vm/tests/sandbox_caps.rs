//! The hardened profile's fixed caps — regex pattern size, arrays nested in
//! one join, JSON nesting and BigInt bits — are per-VM and a host may move
//! them (`ScriptState::set_sandbox_caps`, f2i-com/zipp.org#69). The defaults
//! are the values the profile always enforced; each cap is clamped to
//! `SandboxCaps::MAX`; reaching one throws a RangeError the script can catch,
//! with or without `set_limits`.

#![cfg(feature = "safe-sandbox")]

use zipp_vm::embed::{compile_script, JsValue, SandboxCaps, ScriptState};

fn state() -> ScriptState {
    let mut state = compile_script("").expect("empty script compiles");
    state.run_init().expect("empty script runs");
    state
}

/// Evaluate `body`, an expression run inside `try`, and report its string
/// value or the name of what it threw.
fn outcome(state: &mut ScriptState, body: &str) -> String {
    let src = format!("(function () {{ try {{ return String({body}); }} catch (e) {{ return 'threw ' + e.name; }} }})()");
    match state.eval_in_context(&src) {
        Ok(JsValue::String(s)) => s,
        other => panic!("{body}: unexpected {other:?}"),
    }
}

const NESTED_ARRAY: &str = "(function (n) { let a = [1]; for (let i = 1; i < n; i++) a = [a]; return a; })";
const NESTED_JSON: &str = "(function (n) { return '['.repeat(n) + ']'.repeat(n); })";

#[test]
fn defaults_are_the_profiles_values() {
    let state = state();
    assert_eq!(state.sandbox_caps(), SandboxCaps::DEFAULT);
    assert_eq!(SandboxCaps::default(), SandboxCaps::DEFAULT);
    assert_eq!(
        SandboxCaps::DEFAULT,
        SandboxCaps {
            regex_pattern_bytes: 16 << 10,
            array_stringify_depth: 4,
            json_nesting_depth: 64,
            bigint_bits: 1 << 20,
        }
    );
}

#[test]
fn regex_pattern_size_follows_the_cap() {
    let mut state = state();
    let big = "new RegExp('a'.repeat(20000)).source.length";
    assert_eq!(outcome(&mut state, big), "threw RangeError");
    assert_eq!(outcome(&mut state, "new RegExp('a'.repeat(16384)).source.length"), "16384");
    state.set_sandbox_caps(SandboxCaps { regex_pattern_bytes: 32 << 10, ..SandboxCaps::DEFAULT });
    assert_eq!(outcome(&mut state, big), "20000");
    // A literal is built by the same check when it is evaluated.
    let literal = format!("/{}/.source.length", "b".repeat(20000));
    assert_eq!(outcome(&mut state, &literal), "20000");
    state.set_sandbox_caps(SandboxCaps { regex_pattern_bytes: 100, ..SandboxCaps::DEFAULT });
    assert_eq!(outcome(&mut state, "new RegExp('c'.repeat(200)).source.length"), "threw RangeError");
    assert_eq!(outcome(&mut state, &format!("/{}/.source.length", "d".repeat(200))), "threw RangeError");
    assert_eq!(outcome(&mut state, "/x/.compile('e'.repeat(200)).source.length"), "threw RangeError");
    assert_eq!(outcome(&mut state, "new RegExp('f'.repeat(100)).source.length"), "100");
}

#[test]
fn array_join_depth_follows_the_cap() {
    let mut state = state();
    let join = |n: usize| format!("{NESTED_ARRAY}({n}).join()");
    assert_eq!(outcome(&mut state, &join(4)), "1");
    assert_eq!(outcome(&mut state, &join(5)), "threw RangeError");
    state.set_sandbox_caps(SandboxCaps { array_stringify_depth: 12, ..SandboxCaps::DEFAULT });
    assert_eq!(outcome(&mut state, &join(12)), "1");
    assert_eq!(outcome(&mut state, &format!("{NESTED_ARRAY}(12).toString()")), "1");
    assert_eq!(outcome(&mut state, &join(13)), "threw RangeError");
    state.set_sandbox_caps(SandboxCaps { array_stringify_depth: 2, ..SandboxCaps::DEFAULT });
    assert_eq!(outcome(&mut state, &join(3)), "threw RangeError");
    assert_eq!(outcome(&mut state, &join(2)), "1");
}

#[test]
fn json_nesting_follows_the_cap() {
    let mut state = state();
    let parse = |n: usize| format!("JSON.parse({NESTED_JSON}({n})).length");
    let stringify = |n: usize| format!("JSON.stringify(JSON.parse({NESTED_JSON}({n}))).length");
    let pretty = |n: usize| format!("JSON.stringify({NESTED_ARRAY}({n}), null, 2).length > 0");
    let revive = |n: usize| format!("JSON.parse({NESTED_JSON}({n}), (k, v) => v).length");
    assert_eq!(outcome(&mut state, &parse(64)), "1");
    assert_eq!(outcome(&mut state, &parse(65)), "threw RangeError");
    state.set_sandbox_caps(SandboxCaps { json_nesting_depth: 200, ..SandboxCaps::DEFAULT });
    assert_eq!(outcome(&mut state, &parse(150)), "1");
    assert_eq!(outcome(&mut state, &stringify(150)), "300");
    assert_eq!(outcome(&mut state, &pretty(150)), "true");
    assert_eq!(outcome(&mut state, &revive(150)), "1");
    assert_eq!(outcome(&mut state, &parse(201)), "threw RangeError");
    state.set_sandbox_caps(SandboxCaps { json_nesting_depth: 8, ..SandboxCaps::DEFAULT });
    assert_eq!(outcome(&mut state, &parse(10)), "threw RangeError");
    assert_eq!(outcome(&mut state, &format!("JSON.stringify({NESTED_ARRAY}(10))")), "threw RangeError");
    assert_eq!(outcome(&mut state, &parse(8)), "1");
}

#[test]
fn bigint_size_follows_the_cap() {
    let mut state = state();
    let shift = |bits: u64| format!("(1n << {bits}n).toString(16).length");
    assert_eq!(outcome(&mut state, &shift(1_000_000)), "250001");
    assert_eq!(outcome(&mut state, &shift(1_100_000)), "threw RangeError");
    assert_eq!(outcome(&mut state, "(3n ** 700000n) > 0n"), "threw RangeError");
    state.set_sandbox_caps(SandboxCaps { bigint_bits: 1 << 22, ..SandboxCaps::DEFAULT });
    assert_eq!(outcome(&mut state, &shift(1_100_000)), "275001");
    assert_eq!(outcome(&mut state, "(3n ** 700000n) > 0n"), "true");
    assert_eq!(outcome(&mut state, "BigInt.asUintN(2000000, -1n) > 0n"), "true");
    state.set_sandbox_caps(SandboxCaps { bigint_bits: 1_000, ..SandboxCaps::DEFAULT });
    assert_eq!(outcome(&mut state, &shift(2_000)), "threw RangeError");
    assert_eq!(outcome(&mut state, "(2n ** 64n) * (2n ** 1000n)"), "threw RangeError");
    assert_eq!(outcome(&mut state, "BigInt.asUintN(2000, -1n)"), "threw RangeError");
    assert_eq!(outcome(&mut state, &shift(900)), "226");
}

#[test]
fn caps_are_clamped_to_the_maximum() {
    let mut state = state();
    state.set_sandbox_caps(SandboxCaps {
        regex_pattern_bytes: usize::MAX,
        array_stringify_depth: usize::MAX,
        json_nesting_depth: usize::MAX,
        bigint_bits: u64::MAX,
    });
    assert_eq!(state.sandbox_caps(), SandboxCaps::MAX);
    assert_eq!(
        SandboxCaps::MAX,
        SandboxCaps {
            regex_pattern_bytes: 1 << 20,
            array_stringify_depth: 1_024,
            json_nesting_depth: 1_000,
            bigint_bits: 1 << 30,
        }
    );
    assert_eq!(outcome(&mut state, &format!("JSON.parse({NESTED_JSON}(1001)).length")), "threw RangeError");
    assert_eq!(outcome(&mut state, "(1n << 1073741825n) > 0n"), "threw RangeError");
}

#[test]
fn caps_are_per_vm_and_catchable_under_limits() {
    let mut raised = state();
    let mut plain = state();
    raised.set_sandbox_caps(SandboxCaps { json_nesting_depth: 100, ..SandboxCaps::DEFAULT });
    let deep = format!("JSON.parse({NESTED_JSON}(80)).length");
    assert_eq!(outcome(&mut raised, &deep), "1");
    assert_eq!(outcome(&mut plain, &deep), "threw RangeError");
    assert_eq!(plain.sandbox_caps(), SandboxCaps::DEFAULT);

    // With a recorder attached the refusals stay catchable and the VM goes on.
    plain.set_limits(u64::MAX, None);
    assert_eq!(outcome(&mut plain, &deep), "threw RangeError");
    assert_eq!(outcome(&mut plain, "new RegExp('a'.repeat(20000))"), "threw RangeError");
    assert_eq!(outcome(&mut plain, &format!("{NESTED_ARRAY}(6).join()")), "threw RangeError");
    assert_eq!(outcome(&mut plain, "(1n << 2000000n) > 0n"), "threw RangeError");
    assert_eq!(outcome(&mut plain, "6 * 7"), "42");
    assert_eq!(plain.resource_limit_error(), None);
}
