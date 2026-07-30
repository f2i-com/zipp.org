//! `install_clock` on a native target: once a clock is installed, EVERY engine
//! time read honors it — including the call shapes the compiler intrinsifies
//! to clock opcodes, which never consult the JS `Date` binding, so a host
//! running untrusted JS can make time deterministic from outside the script.
//!
//! The sibling `clock_real.rs` covers the no-clock-installed path. They are
//! separate test binaries because an installed clock is process-global and
//! cannot be uninstalled mid-process.

use std::sync::Mutex;
use zipp_vm::embed::{self, JsValue};

/// 2009-02-13T23:31:30Z — far enough from any real "now" that a read
/// bypassing the installed clock cannot pass these assertions by accident.
const FIXED_EPOCH_MS: f64 = 1_234_567_890_000.0;
const FIXED_MONO_MS: f64 = 250_000.0;

fn fixed_epoch() -> f64 {
    FIXED_EPOCH_MS
}
fn fixed_mono() -> f64 {
    FIXED_MONO_MS
}

/// The installed clock is process-global; serialize so one test's VM cannot
/// be constructed on another test's clock.
static LOCK: Mutex<()> = Mutex::new(());

fn eval(src: &str) -> JsValue {
    let mut st = embed::compile_script("var x = 0;").expect("compiles");
    st.run_init().expect("runs");
    st.eval_in_context(src)
        .unwrap_or_else(|e| panic!("{src:?} failed: {e}"))
}

#[test]
fn date_reads_the_installed_epoch_clock() {
    let _g = LOCK.lock().unwrap();
    zipp_vm::install_clock(fixed_epoch, fixed_mono);

    // The literal shape — intrinsified by the compiler to a clock opcode.
    assert_eq!(eval("Date.now()"), JsValue::Number(FIXED_EPOCH_MS));
    // The same call aliased — no intrinsic, an ordinary native call that must
    // land on the same clock.
    assert_eq!(
        eval("var D = Date; D.now()"),
        JsValue::Number(FIXED_EPOCH_MS)
    );
    // No-arg construction — the intrinsified `new Date()` opcode.
    assert_eq!(
        eval("new Date().getTime()"),
        JsValue::Number(FIXED_EPOCH_MS)
    );
    // Temporal reads the same wall clock.
    assert_eq!(
        eval("Temporal.Now.instant().epochMilliseconds"),
        JsValue::Number(FIXED_EPOCH_MS)
    );
}

#[test]
fn performance_now_reads_the_installed_monotonic_clock() {
    let _g = LOCK.lock().unwrap();
    zipp_vm::install_clock(fixed_epoch, fixed_mono);

    // A fixed monotonic reading: the VM's zero point IS every later reading,
    // so elapsed time is exactly zero — deterministic, which is the point.
    assert_eq!(eval("performance.now()"), JsValue::Number(0.0));
}
