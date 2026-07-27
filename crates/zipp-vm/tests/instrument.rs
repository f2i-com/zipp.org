//! Integration tests for the `instrument` feature: step budget, cooperative
//! abort, and the execution trace.
//!
//! Run with `cargo test -p zipp-vm --features instrument` — the feature is off
//! by default, so a plain `cargo test` skips every test in this file.
//!
//! The trace assertions here are the AIR's boundary and transition conditions
//! restated in Rust. They are worth having on this side of the wire because the
//! prover only checks them in a debug build (Winterfell's trace validation is
//! `#[cfg(debug_assertions)]`); a release prover hands back a proof object that
//! fails later, at the verifier, with no locator.

#![cfg(feature = "instrument")]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use zipp_vm::embed::{self, op, ScriptState, TraceStep};

/// A bootstrap that mentions the globals the eval'd script reaches through.
/// A name the compiled program never mentions has no global slot, so `eval`
/// cannot resolve it — see the module docs on `ScriptState::eval_in_context`.
const BOOT: &str = "void JSON; void globalThis; void eval;";

fn js_string(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn instrumented(max_steps: u64, abort: Option<Arc<AtomicBool>>) -> ScriptState {
    let mut st = embed::compile_script(BOOT).expect("bootstrap compiles");
    st.set_limits(max_steps, abort);
    st.run_init().expect("bootstrap runs");
    st
}

/// Trace `script` and return `(rows, result JSON)`.
fn trace(script: &str) -> (Option<Vec<TraceStep>>, String) {
    let mut st = instrumented(u64::MAX, None);
    st.start_trace(1 << 20);
    let _ = st.eval_in_context(&format!(
        "globalThis.__r = (0,eval)({});",
        js_string(script)
    ));
    let rows = st.finish_trace(0);
    let json = st
        .eval_in_context("JSON.stringify(globalThis.__r === undefined ? null : globalThis.__r)")
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default();
    (rows, json)
}

/// Every condition the prover's AIR imposes on a trace, checked here so a
/// producer bug shows up as a named assertion instead of an opaque
/// `InconsistentOodConstraintEvaluations` at verification time.
fn assert_provable(rows: &[TraceStep]) {
    assert!(rows.len() >= 2, "row 0 is asserted not to be the halt row");
    assert_eq!(rows[0].clk, 0, "clk[0] == 0");
    assert_eq!(rows[0].pc, 0, "pc[0] == 0");
    assert_ne!(rows[0].opcode, op::HALT, "halt[0] == 0");
    assert_eq!(rows.last().unwrap().opcode, op::HALT, "the last row halts");

    let mut depth: i64 = 0;
    for (i, r) in rows.iter().enumerate() {
        assert_eq!(r.clk, i as u64, "clk is the row index");
        assert!(
            r.opcode < op::COUNT,
            "opcode {} is outside the contract",
            r.opcode
        );
        match r.opcode {
            // `val_dst == const_val`, `val_dst == val_a`, `val_dst == 0`.
            op::CONST => assert_eq!(r.val_dst, r.const_val),
            op::MOVE | op::GET_GLOBAL => assert_eq!(r.val_dst, r.val_a),
            op::SET_GLOBAL => assert_eq!(r.val_dst, 0),
            op::ADD => assert_eq!(r.val_a + r.val_b, r.val_dst),
            op::SUB => assert_eq!(r.val_a - r.val_b, r.val_dst),
            op::MUL => assert_eq!(r.val_a * r.val_b, r.val_dst),
            op::DIV => assert_eq!(r.val_dst * r.val_b, r.val_a),
            op::MOD => assert_eq!(r.val_b * r.aux + r.val_dst, r.val_a),
            op::NEG => assert_eq!(r.val_dst + r.val_a, 0),
            op::NOT => assert!(r.val_dst <= 1),
            op::CMP => assert!(r.aux <= 1),
            op::BITWISE => assert!(r.val_dst < 256, "val_dst must fit the eight bit columns"),
            op::JUMP => assert_eq!(r.aux, rows[i + 1].pc, "jump aux is the next pc"),
            op::CALL => depth += 1,
            op::RETURN => {
                depth -= 1;
                assert!(depth >= 0, "call depth went negative at row {i}");
            }
            _ => {}
        }
        // Once halted, every later row must also halt.
        if r.opcode == op::HALT {
            assert!(rows[i..].iter().all(|s| s.opcode == op::HALT));
        }
    }
}

#[test]
fn a_trace_of_real_javascript_is_provable() {
    for script in [
        "42 * 2",
        "const d=[1,2,3,4,5]; d.map(x=>x*2).filter(x=>x>4)",
        "let s=0; for(let i=0;i<50;i++) s+=i; s",
        "function fib(n){return n<2?n:fib(n-1)+fib(n-2)} fib(10)",
        "var o={a:1,b:2}; o.a+o.b",
        "class A{constructor(){this.x=5}} new A().x",
        "try { null.x } catch(e) { e.constructor.name }",
        "'a1b2'.replace(/[0-9]/g,'#')",
        // Values with no exact field form: these must produce OTHER rows, not
        // false arithmetic claims.
        "(-5) + 3",
        "0.1 + 0.2",
        "'a' + 'b'",
        "2 ** 40",
    ] {
        let (rows, _) = trace(script);
        let rows = rows.unwrap_or_else(|| panic!("no trace for {script:?}"));
        assert_provable(&rows);
    }
}

#[test]
fn results_are_unchanged_by_tracing() {
    for (script, want) in [
        ("42 * 2", "84"),
        (
            "const d=[1,2,3,4,5]; d.map(x=>x*2).filter(x=>x>4)",
            "[6,8,10]",
        ),
        (
            "function fib(n){return n<2?n:fib(n-1)+fib(n-2)} fib(12)",
            "144",
        ),
        ("17 % 5", "2"),
        ("(-5) + 3", "-2"),
        ("'a'+'b'", "\"ab\""),
    ] {
        let (_, json) = trace(script);
        assert_eq!(json, want, "for {script:?}");
    }
}

/// A row may only claim arithmetic when the identity is exactly true over the
/// integers. Negative results, fractions and strings must fall back to OTHER —
/// this is the difference between a proof that means something and one that is
/// false.
#[test]
fn unprovable_arithmetic_is_demoted_rather_than_faked() {
    for script in ["(-5) + 3", "0.1 + 0.2", "'a' + 'b'", "1 / 3", "7 % 2.5"] {
        let (rows, _) = trace(script);
        let rows = rows.unwrap();
        assert_provable(&rows);
        // The demotion is what assert_provable would catch; assert the shape
        // directly too, so a change that starts claiming these is loud.
        let claimed = rows
            .iter()
            .filter(|r| matches!(r.opcode, op::ADD | op::SUB | op::MUL | op::DIV | op::MOD))
            .count();
        assert_eq!(claimed, 0, "{script:?} must claim no arithmetic row");
    }
    // …while arithmetic that IS exact still gets claimed, or the classifier
    // would be trivially "sound" by never claiming anything.
    for (script, opcode) in [
        ("6 * 7", op::MUL),
        ("100 / 4", op::DIV),
        ("17 % 5", op::MOD),
        ("6 & 3", op::BITWISE),
    ] {
        let (rows, _) = trace(script);
        let rows = rows.unwrap();
        assert!(
            rows.iter().any(|r| r.opcode == opcode),
            "{script:?} should have produced an opcode-{opcode} row"
        );
    }
}

#[test]
fn the_step_budget_stops_an_infinite_loop() {
    let mut st = instrumented(200_000, None);
    let err = st
        .eval_in_context("(0,eval)('while(true){}')")
        .expect_err("a runaway loop must not return");
    assert!(err.contains("instruction budget"), "got {err:?}");
    assert_eq!(st.steps_remaining(), 0);
}

/// The budget is a hard stop, not a catchable error: a script must not be able
/// to `try`/`catch` its way past its own limit and keep running.
#[test]
fn the_budget_cannot_be_caught_and_ignored() {
    let mut st = instrumented(100_000, None);
    let err = st
        .eval_in_context("(0,eval)('try { while(true){} } catch (e) { }')")
        .expect_err("the budget must propagate through catch");
    assert!(err.contains("instruction budget"), "got {err:?}");
}

#[test]
fn the_abort_flag_stops_a_running_script() {
    let flag = Arc::new(AtomicBool::new(false));
    let setter = flag.clone();
    let t = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(100));
        setter.store(true, Ordering::Relaxed);
    });
    let mut st = instrumented(u64::MAX, Some(flag));
    let err = st
        .eval_in_context("(0,eval)('while(true){}')")
        .expect_err("must be aborted");
    assert!(err.contains("aborted by the host"), "got {err:?}");
    t.join().unwrap();
}

/// A truncated recording is discarded, not returned. A trace missing its tail
/// would attest to an execution that did not happen, and the caller has no way
/// to tell the difference from the rows alone.
#[test]
fn hitting_the_row_cap_yields_no_trace_at_all() {
    let mut st = instrumented(u64::MAX, None);
    st.start_trace(500);
    let _ = st
        .eval_in_context("globalThis.__r = (0,eval)('let s=0; for(let i=0;i<100000;i++) s+=i; s')");
    assert!(
        st.finish_trace(0).is_none(),
        "a truncated trace must not be handed out"
    );
    assert!(st.trace_truncated());
    // The script itself still ran to completion — only the recording stopped.
    let v = st.eval_in_context("String(globalThis.__r)").unwrap();
    assert_eq!(v.as_str(), Some("4999950000"));
}

/// Uninstrumented VMs must behave exactly as before — no budget, no recorder,
/// and (the part worth pinning) the JIT still on.
#[test]
fn an_uninstrumented_vm_is_unbounded() {
    let mut st = embed::compile_script("var x = 0;").expect("compiles");
    st.run_init().expect("runs");
    assert_eq!(st.steps_remaining(), u64::MAX);
    assert!(st.finish_trace(0).is_none());
    assert_eq!(
        st.eval_in_context("(function(){var s=0;for(var i=0;i<300000;i++)s+=i;return s})()")
            .unwrap(),
        embed::JsValue::Number(44_999_850_000.0)
    );
}
