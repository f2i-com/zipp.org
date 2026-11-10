//! Regression: an entry block may have loop back-edges. Tier O must create a
//! separate SSA preheader so initial parameter/undefined values are merged
//! with the loop-carried values by phis, instead of asserting no predecessors.
#![cfg(all(feature = "jit", target_arch = "x86_64"))]

const PROGRAM: &str = include_str!("fixtures/tier_o_entry_loops.js");
const EXPECTED: &str = "0 11108365 130357 1788395 61924 23400 4950\nx321\n-2";

#[test]
fn loop_entry_phi_inputs_match_node() {
    // The environment toggles are cached per test process; use one test.
    std::env::set_var("ZIPP_OPT", "1");
    std::env::set_var("ZIPP_NO_OPT_TIER_UP", "1");
    let out = zipp_vm::run(PROGRAM).expect("compiles");
    assert_eq!(out.error, None, "{:?}", out.errput);
    assert_eq!(out.output.join("\n"), EXPECTED);
}
