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

use std::sync::atomic::AtomicBool;
#[cfg(not(feature = "meter-only"))]
use std::sync::atomic::Ordering;
use std::sync::Arc;
use zipp_vm::embed::{self, ScriptState};
#[cfg(not(feature = "meter-only"))]
use zipp_vm::embed::{op, TraceStep};

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
#[cfg(not(feature = "meter-only"))]
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
#[cfg(not(feature = "meter-only"))]
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

#[cfg(not(feature = "meter-only"))]
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

#[cfg(not(feature = "meter-only"))]
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
#[cfg(not(feature = "meter-only"))]
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

/// The consumed count is the remaining budget's other half: a host billing
/// for execution (gas) needs what the script DID, not just what it may still
/// do. The two sum to the budget at every observational point — interpreter
/// and native charges included.
#[test]
fn steps_used_reports_what_the_script_consumed() {
    const BUDGET: u64 = 10_000_000;
    let mut st = instrumented(BUDGET, None);
    // The bootstrap already consumed some of the budget; the invariant holds.
    assert_eq!(st.steps_used() + st.steps_remaining(), BUDGET);

    let before = st.steps_used();
    st.eval_in_context("(0,eval)('let s=0; for(let i=0;i<1000;i++) s+=i; s')")
        .expect("a small loop runs");
    let used = st.steps_used() - before;
    assert!(
        used > 1_000,
        "a 1000-iteration loop consumed only {used} steps — is it counted at all?"
    );
    assert!(
        used < 1_000_000,
        "a 1000-iteration loop consumed {used} steps — the count is not sane"
    );
    assert_eq!(st.steps_used() + st.steps_remaining(), BUDGET);
}

#[test]
fn an_unlimited_meter_retains_a_real_usage_counter() {
    let mut st = instrumented(u64::MAX, None);
    let before = st.steps_used();
    st.eval_in_context("(0,eval)('let s=0; for(let i=0;i<1000;i++) s+=i; s')")
        .expect("an unlimited metered loop runs");
    assert!(
        st.steps_used() > before,
        "unlimited work must still be billed"
    );
    assert_eq!(st.steps_remaining(), u64::MAX);
}

/// Exhaustion reports exactly the budget — the number a gas meter charges for
/// a rejected transaction.
#[test]
fn an_exhausted_budget_reports_the_full_cap() {
    const BUDGET: u64 = 100_000;
    let mut st = instrumented(BUDGET, None);
    let err = st
        .eval_in_context("(0,eval)('while(true){}')")
        .expect_err("a runaway loop must not return");
    assert!(err.contains("instruction budget"), "got {err:?}");
    assert_eq!(st.steps_used(), BUDGET);
    assert_eq!(st.steps_remaining(), 0);
}

/// Static object finalization charges one dispatch plus its field count: the
/// latter is completed work outside the dispatch loop. A finite meter must
/// include both in billing without letting that off-loop charge shift the
/// interpreter's periodic-poll clock.
#[test]
fn finite_budget_exactly_replays_weighted_off_loop_work() {
    const BUDGET: u64 = 10_000_000;
    const SCRIPT: &str = "(0,eval)('let sink; for (let i=0;i<256;i++) { sink={a:i,b:i,c:i,d:i,e:i,f:i,g:i,h:i}; } sink.h')";

    let mut measured = instrumented(BUDGET, None);
    measured
        .eval_in_context(SCRIPT)
        .expect("weighted object finalization runs");
    let exact = measured.steps_used();
    assert!(exact > 256 * 8, "off-loop field charges were not reflected");
    assert_eq!(exact + measured.steps_remaining(), BUDGET);

    let mut replay = embed::compile_script(BOOT).expect("bootstrap compiles");
    replay.set_limits(exact, None);
    replay.run_init().expect("bootstrap replays");
    replay
        .eval_in_context(SCRIPT)
        .expect("the exact final allowance succeeds");
    assert_eq!(replay.steps_used(), exact);
    assert_eq!(replay.steps_remaining(), 0);

    let mut short = embed::compile_script(BOOT).expect("bootstrap compiles");
    short.set_limits(exact - 1, None);
    short.run_init().expect("bootstrap fits the short budget");
    let err = short
        .eval_in_context(SCRIPT)
        .expect_err("one fewer weighted step is rejected");
    assert!(err.contains("instruction budget"), "got {err:?}");
    assert_eq!(short.steps_used(), exact - 1);
    assert_eq!(short.steps_remaining(), 0);
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
#[cfg(not(feature = "meter-only"))]
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
#[cfg(not(feature = "meter-only"))]
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

/// The step count must be the SAME NUMBER whether or not the JIT ran, or
/// `max_steps` would mean two different things depending on how hot the code
/// got — and a script near its limit would succeed or fail unpredictably.
///
/// Compiled code charges one basic block at a time, by that block's exact
/// instruction count. A basic block is straight-line, so entering it means
/// executing all of it: the charge is what the interpreter would have counted.
/// The explicit JIT switch gives the test an interpreter-only oracle without
/// allocating a trace, so the same test also covers the WASM `meter-only`
/// profile.
#[test]
fn the_jit_charges_exactly_what_the_interpreter_would() {
    const BIG: u64 = 1_000_000_000;

    fn steps_and_result(script: &str, interpreter_only: bool) -> (u64, String) {
        let mut st = embed::compile_script(BOOT).expect("compiles");
        st.set_limits(BIG, None);
        if interpreter_only {
            st.disable_vm_jit();
        }
        st.run_init().expect("runs");
        let before = st.steps_remaining();
        let _ = st.eval_in_context(&format!(
            "globalThis.__r = (0,eval)({});",
            js_string(script)
        ));
        let used = before - st.steps_remaining();
        let out = st
            .eval_in_context("String(globalThis.__r)")
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_default();
        (used, out)
    }

    for script in [
        // A counted loop far past OSR_THRESHOLD — the integer region tier.
        "let s=0; for(let i=0;i<200000;i++) s+=i; s",
        // A branchy body, so the block partition has to be right and not just
        // "the whole loop span".
        "let s=0; for(let i=0;i<200000;i++){ if(i%3===0) s+=i; else s-=1; } s",
        // Self-recursion — the whole-function tier and its native self-calls.
        "function fib(n){return n<2?n:fib(n-1)+fib(n-2)} fib(22)",
        // Arrays: pinned dense-element access in a hot loop.
        "const a=[]; for(let i=0;i<50000;i++)a.push(i*3);          let t=0; for(let i=0;i<a.length;i++)t+=a[i]; t",
        // Property access, which routes through the inline-cache memory tier.
        "let o={n:0}; for(let i=0;i<100000;i++){ o.n = o.n + 1; } o.n",
        // Many receiver identities at one Get/Set site. The adaptive direct-
        // miss gate must not deopt/replay a block after it has been charged;
        // metered execution keeps the probe form and therefore the exact count.
        "let a=[]; for(let i=0;i<64;i++)a.push({n:i,p:i}); function touch(o,v){o.n=v;return o.n} let s=0; for(let i=0;i<50000;i++)s=(s+touch(a[i&63],i))|0; s",
        // String building — the helper-call path.
        "let s=''; for(let i=0;i<2000;i++) s+='x'; s.length",
        // Runtime-declining Tier-C primitives stay interpreter-owned under a
        // meter: modulo, nested-member RequireObjectCoercible, guarded
        // Map.set/clear and guarded string case conversion must not pre-charge
        // a native block and then charge replayed bytecode a second time.
        "function hot(m,h,i){let r=i%4;m.set('k',r);if((i&255)===255)m.clear();return h.text.toUpperCase().length+r} const m=new Map(),h={text:'ab'};let s=0;for(let i=0;i<5000;i++)s+=hot(m,h,i);s",
        // Rotating closures with a capture-free same-prototype leaf. Leaf
        // splices deliberately stay disabled for metered VMs until the splice
        // can charge the callee body (including its default prologue) exactly.
        "let fs=[]; function mk(){ let f=function rot(x,n=5){return (x<<n)|(x>>>(32-n))}; return function(x){return f(x,3)} } for(let i=0;i<16;i++)fs.push(mk()); let s=0; for(let i=0;i<100000;i++)s=(s+fs[i&15](i))|0; s",
        // Own-data random-method direct-slot probing is deliberately omitted
        // from metered Tier-C bodies until its native prefix has an exact
        // charge proof. The ordinary method/callee bodies still charge exactly.
        "function random(){return this.n} function hot(o){let s=0;for(let i=0;i<100000;i++)s+=o.random();return s} hot({n:3,random:random})",
        // Likewise, the transactional own-method/global splice intentionally
        // declines under a meter until nested callee-bytecode charging is
        // represented exactly. This seed/update shape would otherwise engage.
        "let st=123456789;function random(){st^=st<<13;st^=st>>>17;st^=st<<5;return(st>>>0)/4294967296}const o={random:random};function call(){return o.random()}let s=0;for(let i=0;i<5000;i++)s=(s+((call()*1000000)|0))|0;s",
    ] {
        let (jit, jit_out) = steps_and_result(script, false);
        let (interp, interp_out) = steps_and_result(script, true);
        assert_eq!(jit_out, interp_out, "different RESULT for {script:?}");
        assert_eq!(jit, interp, "different step count for {script:?}");
        assert!(jit > 1000, "{script:?} charged only {jit} steps — is it running at all?");
    }
}

/// The point of the whole exercise: a loop hot enough to be compiled must still
/// stop at its budget. Before native metering it would run to completion,
/// ignore the limit entirely, and return the right answer — the failure mode
/// that is hardest to notice.
#[test]
fn a_jit_hot_loop_still_hits_its_budget() {
    let mut st = instrumented(500_000, None);
    let started = std::time::Instant::now();
    let err = st
        .eval_in_context("(0,eval)('let s=0; for(let i=0;i<1000000000;i++) s+=i; s')")
        .expect_err("a billion-iteration loop must not complete");
    assert!(err.contains("instruction budget"), "got {err:?}");
    assert_eq!(st.steps_remaining(), 0);
    // A billion iterations would take seconds even compiled; stopping at half a
    // million steps must be immediate.
    assert!(
        started.elapsed().as_millis() < 500,
        "took {:?}",
        started.elapsed()
    );
}

/// The abort flag has to reach compiled code too. It is polled in Rust once per
/// native entry, and the lent chunk is what forces those entries to keep
/// happening inside an otherwise unbounded loop.
#[test]
#[cfg(not(feature = "meter-only"))]
fn the_abort_flag_stops_a_jit_hot_loop() {
    let flag = Arc::new(AtomicBool::new(false));
    let setter = flag.clone();
    let t = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(100));
        setter.store(true, Ordering::Relaxed);
    });
    let mut st = instrumented(u64::MAX, Some(flag));
    let started = std::time::Instant::now();
    let err = st
        .eval_in_context("(0,eval)('let s=0; for(let i=0;i<100000000000;i++) s+=i; s')")
        .expect_err("must be aborted");
    assert!(err.contains("aborted by the host"), "got {err:?}");
    assert!(
        started.elapsed().as_secs() < 5,
        "took {:?}",
        started.elapsed()
    );
    t.join().unwrap();
}

/// Metering a VM must not change what its programs compute. This is the same
/// contract the crate's JIT-on/JIT-off differential tests enforce, extended to
/// the third configuration that now exists: JIT on, and charging.
#[test]
fn metering_does_not_change_results() {
    for (script, want) in [
        ("let s=0; for(let i=0;i<100000;i++) s+=i; s", "4999950000"),
        (
            "function fib(n){return n<2?n:fib(n-1)+fib(n-2)} fib(24)",
            "46368",
        ),
        (
            "const a=[]; for(let i=0;i<10000;i++)a.push(i%7); a.filter(x=>x===3).length",
            "1429",
        ),
        (
            "let o={a:0,b:0}; for(let i=0;i<50000;i++){o.a+=1;o.b+=o.a;} o.b",
            "1250025000",
        ),
        (
            "let s=0; for(let i=0;i<50000;i++){ s = (s + i*3) | 0; } s",
            "-545042296",
        ),
    ] {
        let mut metered = instrumented(u64::MAX, None);
        let got = metered
            .eval_in_context(&format!("String((0,eval)({}))", js_string(script)))
            .unwrap_or_else(|e| panic!("metered run of {script:?} failed: {e}"));
        assert_eq!(got.as_str(), Some(want), "metered result for {script:?}");

        let mut plain = embed::compile_script(BOOT).expect("compiles");
        plain.run_init().expect("runs");
        let base = plain
            .eval_in_context(&format!("String((0,eval)({}))", js_string(script)))
            .expect("plain run");
        assert_eq!(got, base, "metered vs unmetered differ for {script:?}");
    }
}

/// An embedded VM must not be handed the test262 harness.
///
/// `$262.agent.start()` spawns a detached OS thread running its own VM: outside
/// the budget, outside the abort flag, absent from the trace, and still running
/// after the host's timeout fires. `createRealm`, `evalScript` and
/// `detachArrayBuffer` are the same kind of thing. None of it belongs in an API
/// whose entire purpose is running code somebody else wrote.
#[test]
fn the_test262_host_object_is_not_exposed_to_embedded_code() {
    let mut st = embed::compile_script(BOOT).expect("compiles");
    st.run_init().expect("runs");
    assert_eq!(
        st.eval_in_context("typeof $262").unwrap().as_str(),
        Some("undefined"),
        "$262 must not exist in an embedded VM"
    );
    // Reaching it through the global object must fail too.
    assert_eq!(
        st.eval_in_context("typeof globalThis.$262")
            .unwrap()
            .as_str(),
        Some("undefined")
    );
    // And the thread-spawning corner specifically.
    assert!(st.eval_in_context("$262.agent.start('')").is_err());
}

/// Uninstrumented VMs must behave exactly as before — no budget, no recorder,
/// and (the part worth pinning) the JIT still on.
#[test]
fn an_uninstrumented_vm_is_unbounded() {
    let mut st = embed::compile_script("var x = 0;").expect("compiles");
    st.run_init().expect("runs");
    assert_eq!(st.steps_remaining(), u64::MAX);
    #[cfg(not(feature = "meter-only"))]
    assert!(st.finish_trace(0).is_none());
    assert_eq!(
        st.eval_in_context("(function(){var s=0;for(var i=0;i<300000;i++)s+=i;return s})()")
            .unwrap(),
        embed::JsValue::Number(44_999_850_000.0)
    );
}

/// A runaway allocation is stopped, and the error says why.
///
/// The step budget alone does not bound memory: `a.push({})` in a loop is a
/// handful of instructions per object, so a script can be well inside its
/// instruction budget and still take the host's heap with it.
#[test]
fn a_script_that_allocates_without_bound_is_stopped() {
    let mut st = embed::compile_script("var x = 0;").expect("compiles");
    st.run_init().expect("runs");
    st.set_limits(50_000_000, None);
    let baseline = st.heap_bytes();
    st.set_heap_limit(baseline + 400_000);

    let err = st
        .eval_in_context(
            "(function(){var a=[];for(var i=0;i<1000000;i++)a.push({n:i});return a.length})()",
        )
        .expect_err("must not be allowed to finish");
    assert!(err.contains("memory budget"), "unexpected error: {err}");

    // Stopped for memory, not for running out of instructions.
    assert!(
        st.steps_remaining() > 0,
        "the step budget should be untouched — this was a memory stop"
    );
}

/// The ceiling is a ceiling, not a straitjacket: a script that stays under it
/// runs to completion untouched.
#[test]
fn a_script_within_its_heap_budget_is_left_alone() {
    let mut st = embed::compile_script("var x = 0;").expect("compiles");
    st.run_init().expect("runs");
    st.set_limits(50_000_000, None);
    st.set_heap_limit(st.heap_bytes() + 4_000_000);

    assert_eq!(
        st.eval_in_context(
            "(function(){var a=[];for(var i=0;i<2000;i++)a.push({n:i});return a.length})()"
        )
        .unwrap(),
        embed::JsValue::Number(2000.0)
    );
}

/// Overshoot is bounded. The check rides the abort poll rather than running per
/// instruction, so a script passes the line before it is stopped — but by a
/// margin proportional to the poll interval, not an unbounded one.
#[test]
fn the_overshoot_past_the_ceiling_is_bounded() {
    let mut st = embed::compile_script("var x = 0;").expect("compiles");
    st.run_init().expect("runs");
    st.set_limits(50_000_000, None);
    let ceiling = st.heap_bytes() + 200_000;
    st.set_heap_limit(ceiling);

    let _ = st.eval_in_context(
        "(function(){var a=[];for(var i=0;i<1000000;i++)a.push({n:i});return a.length})()",
    );

    let overshoot = st.heap_bytes().saturating_sub(ceiling);
    assert!(
        overshoot < 4_000_000,
        "overshot the ceiling by {overshoot} bytes, which is not a bounded margin"
    );
}

/// No limit set means no limit, and in particular no cost: the default recorder
/// leaves the ceiling at `usize::MAX`, which the hot path tests against once.
#[test]
fn no_heap_limit_means_no_heap_limit() {
    let mut st = embed::compile_script("var x = 0;").expect("compiles");
    st.run_init().expect("runs");
    st.set_limits(50_000_000, None);

    assert_eq!(
        st.eval_in_context(
            "(function(){var a=[];for(var i=0;i<50000;i++)a.push({n:i});return a.length})()"
        )
        .unwrap(),
        embed::JsValue::Number(50000.0)
    );
}
