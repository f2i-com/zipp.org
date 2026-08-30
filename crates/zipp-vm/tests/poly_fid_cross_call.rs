//! Different-FuncProto Tier-C cross-call routing. A hot plain-call site may
//! observe several user-function fids; the planner may plant the existing
//! generic native prefix, but runtime identity/fid/entry/realm/GC/throw checks
//! remain authoritative and every decline falls through to the ordinary call.

#[cfg(all(feature = "jit", target_arch = "x86_64"))]
use std::process::{Command, Output};

const CHILD_ENV: &str = "ZIPP_POLY_FID_CROSS_CHILD";
#[cfg(feature = "instrument")]
const METER_CHILD_ENV: &str = "ZIPP_POLY_FID_CROSS_METER_CHILD";
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
const MARKER: &str = "POLY-FID generic live-resolution";
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
const EMITTED_MARKER: &str = "CROSS3-POLY-FID arms=2 native-emitted lane";
const EXPECTED: &str = "7010000:104950:1007:9:150000:42:1:poly";

// `dispatch` exercises an arrow closure plus strict and sloppy ordinary
// functions at one call site, then replaces one live callee after native code
// is installed. `invoke` exercises three more fids, a GetProp guard miss that
// re-enters through an accessor, and a throw that must unwind across the
// frame-free native activation exactly once.
const SOURCE: &str = r#"
    var getterHits = 0;
    const maker = {
      base: 40,
      make() { return (x) => this.base + x; }
    };
    const arrow = maker.make();
    function sloppy(x) { return (this === globalThis ? 100 : -1000) + x; }
    function strictFn(x) { "use strict"; return (this === undefined ? 200 : -2000) + x; }
    function dispatch(fn, x) { return fn(x) + 0; }
    const funcs = [arrow, sloppy, strictFn];
    let sum = 0;
    for (let i = 0; i < 60000; i++) {
      sum = (sum + dispatch(funcs[i % 3], i & 7)) | 0;
    }

    function replacement(x) { return 1000 + x; }
    let replacementWarm = 0;
    for (let i = 0; i < 100; i++) replacementWarm += replacement(i);
    funcs[1] = replacement;
    const replaced = dispatch(funcs[1], 7);
    funcs[1] = Math.abs;
    const nativeReplaced = dispatch(funcs[1], -9);

    function readX(o) { return o.x + 1; }
    function readY(o) { return o.y + 2; }
    function throwsNow() { throw "poly"; }
    function mayThrow(o) { if (o.fail) return throwsNow() + 0; return o.z + 3; }
    function invoke(fn, o) { return fn(o) + 0; }
    const readers = [readX, readY, mayThrow];
    const objects = [{ x: 2 }, { y: 3 }, { z: 4, fail: false }];
    let aux = 0;
    for (let i = 0; i < 30000; i++) {
      aux = (aux + invoke(readers[i % 3], objects[i % 3])) | 0;
    }

    const accessor = { get x() { getterHits++; return 41; } };
    const reentered = invoke(readX, accessor);
    let caught = "";
    try { invoke(mayThrow, { z: 0, fail: true }); } catch (e) { caught = String(e); }
    const result = [sum, replacementWarm, replaced, nativeReplaced, aux, reentered, getterHits, caught].join(":");
    console.log(result);
"#;

fn execute_source() {
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "runtime error: {:?}", out.error);
    assert_eq!(out.output, [EXPECTED]);
}

#[cfg(all(feature = "jit", target_arch = "x86_64"))]
fn run_child(env: &[(&str, &str)]) -> Output {
    let exe = std::env::current_exe().expect("test binary path");
    let mut cmd = Command::new(exe);
    cmd.args(["poly_fid_execution_child", "--exact", "--nocapture"])
        .env(CHILD_ENV, "1")
        .env("ZIPP_JITLOG", "1")
        .env_remove("ZIPP_NO_POLY_FID_CROSSCALL")
        .env_remove("ZIPP_NO_CROSS3_POLY_FID")
        .env_remove("ZIPP_NO_POLY_CROSSCALL")
        .env_remove("ZIPP_NO_CROSSCALL")
        .env_remove("ZIPP_NOJIT")
        .env_remove("ZIPP_JIT_THRESHOLD")
        .env_remove("ZIPP_GC_STRESS");
    cmd.envs(env.iter().copied());
    cmd.output().expect("spawn cross-call child")
}

#[cfg(all(feature = "jit", target_arch = "x86_64"))]
fn assert_child(label: &str, out: &Output) -> String {
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success() && !stdout.contains("running 0 tests"),
        "{label} child failed:\n{stdout}\n{stderr}"
    );
    stderr.into_owned()
}

#[test]
fn poly_fid_execution_child() {
    if std::env::var_os(CHILD_ENV).is_some() {
        execute_source();
    }
}

#[test]
fn different_fids_use_generic_live_cross_call_and_switch_restores_old_plan() {
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    {
        let enabled = assert_child("enabled", &run_child(&[]));
        assert!(
            enabled.contains(MARKER),
            "different-fid site did not select the generic cross-call prefix:\n{enabled}"
        );
        assert!(
            enabled.contains(EMITTED_MARKER),
            "eligible different-fid arms were not emitted:\n{enabled}"
        );

        let helper_only = assert_child(
            "helper-only",
            &run_child(&[("ZIPP_NO_CROSS3_POLY_FID", "1")]),
        );
        assert!(
            helper_only.contains(MARKER),
            "emitted-lane switch incorrectly removed generic poly-fid routing:\n{helper_only}"
        );
        assert!(
            !helper_only.contains(EMITTED_MARKER),
            "emitted-lane switch still planted poly-fid CROSS3 arms:\n{helper_only}"
        );

        let disabled = assert_child(
            "disabled",
            &run_child(&[("ZIPP_NO_POLY_FID_CROSSCALL", "1")]),
        );
        assert!(
            !disabled.contains(MARKER),
            "off switch still selected the different-fid prefix:\n{disabled}"
        );
        assert!(
            !disabled.contains(EMITTED_MARKER),
            "generic-route switch still planted different-fid CROSS3 arms:\n{disabled}"
        );

        // Collection at every allocation/safe point exercises the helper's
        // retained `maybe_gc()` transition and explicit native closure roots.
        let stress = assert_child("gc-stress", &run_child(&[("ZIPP_GC_STRESS", "1")]));
        assert!(
            stress.contains(MARKER),
            "GC-stress run did not reach the planned route:\n{stress}"
        );
        assert!(
            stress.contains(EMITTED_MARKER),
            "GC-stress run did not build the guarded different-fid arms:\n{stress}"
        );
    }

    #[cfg(not(all(feature = "jit", target_arch = "x86_64")))]
    execute_source();
}

/// A metered VM intentionally keeps the pre-change planner decision for a
/// different-fid site. The ordinary metered call machinery remains responsible
/// for exact callee-bytecode charging until this broader route gets a dedicated
/// charging proof.
#[cfg(feature = "instrument")]
#[test]
fn poly_fid_metered_child() {
    if std::env::var_os(METER_CHILD_ENV).is_none() {
        return;
    }
    let mut state = zipp_vm::embed::compile_script(SOURCE).expect("meter source compiles");
    // This child proves planner exclusion, not a particular budget boundary;
    // use the recorder's unbounded value so unrelated block-size changes cannot
    // turn the mechanism check into a fragile instruction-count assertion.
    state.set_limits(u64::MAX, None);
    state.run_init().expect("meter source runs");
    assert!(state.steps_remaining() > 0, "meter unexpectedly exhausted");
}

#[cfg(all(feature = "instrument", feature = "jit", target_arch = "x86_64"))]
#[test]
fn metered_vm_declines_the_new_poly_fid_plan() {
    let exe = std::env::current_exe().expect("test binary path");
    let out = Command::new(exe)
        .args(["poly_fid_metered_child", "--exact", "--nocapture"])
        .env(METER_CHILD_ENV, "1")
        .env("ZIPP_JITLOG", "1")
        .env_remove("ZIPP_NO_POLY_FID_CROSSCALL")
        .env_remove("ZIPP_NO_POLY_CROSSCALL")
        .env_remove("ZIPP_NO_CROSSCALL")
        .env_remove("ZIPP_NOJIT")
        .env_remove("ZIPP_JIT_THRESHOLD")
        .output()
        .expect("spawn metered cross-call child");
    let stderr = assert_child("metered", &out);
    assert!(
        !stderr.contains(MARKER),
        "metered VM selected the new different-fid plan:\n{stderr}"
    );
    // Pin the two exact callers that select POLY-FID in the unmetered child:
    // SOURCE's declaration order makes dispatch fn5 (@1) and invoke fn11 (@1).
    // A generic "Tier C" line could belong to an unrelated leaf and would not
    // prove that the metered planner actually reconsidered these call sites.
    for caller in ["Tier C fn5 compiled", "Tier C fn11 compiled"] {
        assert!(
            stderr.contains(caller),
            "metered child did not compile relevant caller {caller}:\n{stderr}"
        );
    }
}
