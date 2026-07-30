//! With no clock installed, native targets read real time — the override in
//! `vm::clock` must be behaviorally invisible until `install_clock` is called.
//! This is a separate test binary from `clock_installed.rs` because an
//! installed clock is process-global and cannot be uninstalled mid-process.

use zipp_vm::embed::{self, JsValue};

fn eval(src: &str) -> JsValue {
    let mut st = embed::compile_script("var x = 0;").expect("compiles");
    st.run_init().expect("runs");
    st.eval_in_context(src)
        .unwrap_or_else(|e| panic!("{src:?} failed: {e}"))
}

fn eval_num(src: &str) -> f64 {
    match eval(src) {
        JsValue::Number(n) => n,
        other => panic!("{src:?} produced {other:?}, not a number"),
    }
}

fn real_epoch_ms() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock is before the Unix epoch")
        .as_millis() as f64
}

#[test]
fn date_now_is_real_time() {
    let before = real_epoch_ms();
    let t = eval_num("Date.now()");
    let after = real_epoch_ms();
    assert!(
        before <= t && t <= after,
        "Date.now() = {t}, real time bracketed [{before}, {after}]"
    );
}

#[test]
fn new_date_agrees_with_the_wall_clock() {
    let before = real_epoch_ms();
    let t = eval_num("new Date().getTime()");
    let after = real_epoch_ms();
    assert!(
        before <= t && t <= after,
        "new Date().getTime() = {t}, real time bracketed [{before}, {after}]"
    );
}

#[test]
fn the_wall_clock_does_not_run_backwards() {
    let a = eval_num("Date.now()");
    let b = eval_num("var D = Date; D.now()");
    assert!(b >= a, "Date.now() went backwards: {a} then {b}");
}

#[test]
fn performance_now_is_a_small_uptime() {
    // Each `eval` builds a fresh VM, so this is milliseconds since ITS start.
    let p = eval_num("performance.now()");
    assert!(
        p.is_finite() && (0.0..60_000.0).contains(&p),
        "performance.now() = {p}, not a plausible VM uptime"
    );
}
