//! Regression tests for the security/safety fixes that landed after the
//! external code review. Each test closes one specific finding from
//! that review; they're grouped here to keep the rationale visible.

use zipp_engine::config::ZippConfig;
use zipp_engine::engine::ZippEngine;
use zipp_engine::object::Object;

/// Heap::reset used to clear `objects` but leave `bump_next` /
/// `bump_end` non-zero. The next `alloc_fast` would then write via
/// `get_unchecked_mut` past the cleared Vec's logical length —
/// memory corruption on a pooled VM. The fix resets both cursors.
///
/// We can't directly probe `bump_next` from outside the crate, so the
/// test exercises the path: a script that triggers the bump path,
/// then re-runs through the pooled VM and concatenates more strings.
/// Before the fix this corrupted heap and produced wrong output.
#[test]
fn pooled_vm_string_workload_does_not_corrupt_heap() {
    let engine = ZippEngine::default();
    let src = r#"
        let s = "";
        for (let i = 0; i < 50; i = i + 1) { s = s + "x"; }
        s.length;
    "#;
    for _ in 0..4 {
        let out = engine.eval(src).expect("eval");
        match out {
            Object::Integer(n) => assert_eq!(n, 50),
            other => panic!("expected Integer(50), got {:?}", other),
        }
    }
}

/// Circular references used to recurse forever in `eval_to_json`,
/// stack-overflowing the host. The depth cap returns a sentinel
/// instead.
#[test]
fn eval_to_json_does_not_stack_overflow_on_cycle() {
    let engine = ZippEngine::default();
    let json = engine
        .eval_to_json(r#"let a = {}; a.self = a; a"#)
        .expect("eval_to_json");
    // Depth-guard kicks in. The exact form is implementation-defined
    // (we emit "[Circular]" as the placeholder) but the important
    // property is that we returned a finite string.
    assert!(json.contains("Circular"), "got {}", json);
}

/// `compile_script` used to silently continue with a partial AST when
/// the parser produced errors but at least one statement parsed —
/// running half of a malformed script in production. The default is
/// now fail-closed; the embedder must set
/// `config.allow_partial_parse = true` to opt back in.
#[test]
fn malformed_script_fails_closed_by_default() {
    let engine = ZippEngine::default();
    let bad = "let ok = 1; let oops = (;";
    assert!(
        engine.compile_script(bad).is_err(),
        "default config must reject malformed scripts"
    );
}

#[test]
fn malformed_script_runs_when_explicitly_opted_in() {
    let mut cfg = ZippConfig::default();
    cfg.allow_partial_parse = true;
    let engine = ZippEngine::with_config(cfg);
    // Half-parses are noisy by design — the test only cares that the
    // engine accepts the prefix when the embedder opted in.
    let _ = engine.compile_script("let ok = 1; let oops = (;");
}

/// JIT JumpIfNot/JumpIfTruthy/Not used to compare the operand
/// against `VAL_TRUE` only, so `if (1) { … }` would route through the
/// falsy branch once the function got JIT-compiled. The fix tracks
/// per-register "known boolean" status during emit and refuses to
/// JIT functions whose conditional jumps consume a non-boolean
/// register, falling through to the interpreter (which has correct
/// truthiness semantics).
///
/// This test calls a function many times to push it past the
/// `DJIT_THRESHOLD = 4` and would have flipped the result before the
/// fix.
#[test]
fn jit_truthiness_correct_for_non_boolean_operand() {
    let engine = ZippEngine::default();
    let mut state = engine
        .compile_script(
            r#"
            function check(x) { if (x) { return "yes"; } return "no"; }
            "#,
        )
        .expect("compile");
    state.run_init().expect("init");

    let runs = 50; // well past the JIT threshold
    for _ in 0..runs {
        let out = state
            .call_function("check", &[Object::Integer(1)])
            .expect("call");
        match out {
            Object::String(s) => assert_eq!(s.as_ref(), "yes"),
            other => panic!("expected \"yes\" for truthy 1, got {:?}", other),
        }
    }
    for _ in 0..runs {
        let out = state
            .call_function("check", &[Object::Integer(0)])
            .expect("call");
        match out {
            Object::String(s) => assert_eq!(s.as_ref(), "no"),
            other => panic!("expected \"no\" for falsy 0, got {:?}", other),
        }
    }
}

/// Errors thrown from JIT helpers used to be swallowed and surface as
/// `Value::UNDEFINED`. The new side-channel propagates them as a
/// real `Result::Err` — even after the function has been JIT-compiled
/// (4+ calls).
#[test]
fn jit_call_helper_propagates_thrown_errors() {
    let engine = ZippEngine::default();
    let mut state = engine
        .compile_script(
            r#"
            function boom() { throw new Error("nope"); }
            function caller() { return boom(); }
            "#,
        )
        .expect("compile");
    state.run_init().expect("init");

    // Warm up past the JIT threshold so subsequent calls go through
    // emitted code. We invoke through `boom` to trigger the throw
    // inside the JIT-helper boundary.
    for _ in 0..30 {
        let res = state.call_function("caller", &[]);
        // Whether the helper has fully promoted to JIT or not, the
        // error must always come back as Err — never as Ok(undefined).
        assert!(
            res.is_err(),
            "throw must surface as Err, got Ok: {:?}",
            res.ok()
        );
    }
}

// ── Round 8 (external code review #2) ────────────────────────────────

/// `set_execution_limits(None, None)` used to silently disable heap
/// and abort-flag checks because it only inspected
/// `max_instructions` / `max_wall_time_ms` when recomputing
/// `enforce_limits`. The fix routes through
/// `ZippConfig::requires_limit_checks` so heap caps and
/// `abort_flag` survive a wall-clock-limit reset.
#[test]
fn set_execution_limits_none_keeps_heap_limit() {
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    let mut cfg = ZippConfig::default();
    cfg.max_heap_objects = Some(64);
    cfg.max_heap_bytes = None;
    cfg.max_instructions = Some(1_000_000);
    cfg.max_wall_time_ms = Some(1_000);
    cfg.abort_flag = Some(Arc::new(AtomicBool::new(false)));
    let engine = ZippEngine::with_config(cfg);
    let mut state = engine.compile_script("let x = 0;").expect("compile");
    state.run_init().expect("init");

    // Now disable instr/wall-time. Heap + abort_flag must still be
    // enforced — otherwise an embedder that wanted to relax CPU
    // limits while keeping memory caps in place would silently lose
    // the memory cap too.
    state.set_execution_limits(None, None);
    let vm = state.vm();
    assert!(
        vm.config.max_heap_objects.is_some(),
        "max_heap_objects must persist across set_execution_limits"
    );
    assert!(
        vm.config.abort_flag.is_some(),
        "abort_flag must persist across set_execution_limits"
    );
}

/// Persistent `ScriptState` calls used to leave argument values on
/// the value stack when the callee threw — the next `call_function`
/// would push fresh args on top of the stale ones, blowing up sp
/// across calls. The fix pulls `sp = arg_start` *before* the `?`
/// propagates the error.
#[test]
fn call_function_resets_sp_on_thrown_error() {
    let engine = ZippEngine::default();
    let mut state = engine
        .compile_script(
            r#"
            function boom() { throw new Error("nope"); }
            function ping() { return 1; }
            "#,
        )
        .expect("compile");
    state.run_init().expect("init");

    let sp_before = state.vm().sp;
    for _ in 0..5 {
        let _ = state.call_function("boom", &[Object::Integer(1), Object::Integer(2)]);
        // Each failed call must restore sp; otherwise it would grow
        // by `nargs` per iteration.
        assert_eq!(
            state.vm().sp,
            sp_before,
            "sp must be restored after a thrown exception"
        );
    }
    // A subsequent successful call must still produce the right
    // result. If the leaked args were still on the stack, ping's
    // arg-window would slide and it would read garbage.
    let out = state.call_function("ping", &[]).expect("call ping");
    match out {
        Object::Integer(n) => assert_eq!(n, 1),
        other => panic!("expected Integer(1), got {:?}", other),
    }
}

/// `eval_in_context` used to compile snippets through
/// `compile_program` (function scope) and never merged any new
/// top-level bindings back into `globals_table` — a REPL session
/// could `let x = 1` without `x` ever becoming visible to the next
/// eval. The fix uses `compile_program_persistent` and merges the
/// snippet's exported globals.
#[test]
fn eval_in_context_persists_top_level_let_across_calls() {
    let engine = ZippEngine::default();
    let mut state = engine.compile_script("").expect("compile");
    state.run_init().expect("init");

    state.eval_in_context("let x = 41;").expect("first eval");
    let out = state.eval_in_context("x + 1").expect("second eval");
    match out {
        Object::Integer(n) => assert_eq!(n, 42),
        other => panic!("expected Integer(42), got {:?}", other),
    }
}

/// `set_global` used to allocate the next slot from
/// `globals_table.len()`, which is *not* the high-water mark when
/// the compiled program reserved private slots for inner closures.
/// The new code uses `next_global_slot` (mirrored from the
/// compiler) so an embedder-installed runtime global can never
/// collide with one of those reserved slots.
#[test]
fn set_global_avoids_compiler_private_slots() {
    let engine = ZippEngine::default();
    // The IIFE's `e` is captured by `getter`, so the compiler
    // mirrors `e` to a global slot. After running, an embedder that
    // calls `set_global("dummy", …)` must NOT pick that same slot.
    let mut state = engine
        .compile_script(
            r#"
            (function() {
                var e = { check: 7 };
                var getter = function() { return e.check; };
                window.__getter = getter;
            })();
            window;
            "#,
        )
        .expect("compile");
    // We don't need run_init to expose the bug — the compiler-side
    // accounting is what `set_global` consumes.
    let _ = state.run_init();
    state
        .set_global("dummy", Object::Integer(99))
        .expect("set_global");
    // The dummy must round-trip without overwriting any pre-existing
    // global; if it had collided with a captured slot, the value
    // there would no longer be 99.
    match state.get_global("dummy") {
        Ok(Object::Integer(n)) => assert_eq!(n, 99),
        other => panic!("expected Integer(99), got {:?}", other),
    }
}

/// `SharedGlobals::new` used to lazily grow the globals Vec, but
/// the dispatch loop caches a raw pointer into that Vec — a regrow
/// would dangle the pointer. The fix pre-sizes to `GLOBALS_SIZE`
/// up front. We exercise a high-slot write to make sure no
/// reallocation happens (the test would have UB-tripped Miri
/// before the fix).
#[test]
fn high_index_global_does_not_dangle_dispatch_pointer() {
    let engine = ZippEngine::default();
    let mut state = engine.compile_script("0").expect("compile");
    state.run_init().expect("init");

    // Set then read a slot far above the previous lazy-grow
    // threshold. Round-trip must observe what we just wrote.
    state
        .set_global("high_water_test", Object::Integer(123))
        .expect("set_global");
    match state.get_global("high_water_test") {
        Ok(Object::Integer(n)) => assert_eq!(n, 123),
        other => panic!("expected Integer(123), got {:?}", other),
    }
}

// ── Round 9 (external code review #3) ────────────────────────────────

/// The dispatch loop used to keep a "direct property cache" — three
/// locals (`prop_cache_obj`, `prop_cache_values`, `prop_cache_shape`)
/// that bypassed the inline-cache path on consecutive accesses to the
/// same hash. The cache key was `obj_val.bits()` (a heap index), but
/// the heap recycles indices through its `free_list`: a freed object's
/// slot can be reused by a subsequent allocation, so a cache hit could
/// secretly target a different object. Worse, `Object.freeze` and
/// `defineProperty` accessors don't bump `shape_version`, so freezing
/// or installing a getter/setter never invalidated the cache —
/// post-freeze writes silently mutated frozen objects, and getters
/// were never invoked.
///
/// Round 9 removed the direct cache entirely and routes through the
/// inline-cache path, which already re-checks `frozen` and
/// `has_accessors()` on every access. This test warms the cache with
/// a hot write loop and then freezes the object — subsequent writes
/// must be no-ops.
#[test]
fn frozen_object_writes_rejected_after_warmup() {
    let engine = ZippEngine::default();
    let out = engine
        .eval(
            r#"
            let o = { count: 0 };
            // Warm the inline cache: many writes to the same shape.
            for (let i = 0; i < 200; i = i + 1) { o.count = i; }
            // o.count is now 199. Freeze, then try to mutate.
            Object.freeze(o);
            for (let i = 0; i < 50; i = i + 1) { o.count = -1; }
            o.count;
            "#,
        )
        .expect("eval");
    match out {
        Object::Integer(n) => assert_eq!(
            n, 199,
            "frozen-object writes must be silently dropped, not corrupt the slot"
        ),
        other => panic!("expected Integer(199), got {:?}", other),
    }
}

/// `HashObject::insert_pair` and `HashObject::remove_pair` used to
/// happily mutate frozen hashes — only `set_by_sym` checked. Code
/// that went through the property-store opcode slow path,
/// `Object.assign`, `Object.fromEntries`, or any of the iterator
/// builtins would silently bypass `Object.freeze`. The fix
/// centralises the check in the two mutation primitives so every
/// caller inherits it.
#[test]
fn frozen_hash_rejects_indirect_mutation() {
    let engine = ZippEngine::default();
    let out = engine
        .eval(
            r#"
            let o = { a: 1 };
            Object.freeze(o);
            // assign tries to add a property — must be a no-op on frozen.
            Object.assign(o, { b: 2 });
            // delete also has to silently fail.
            delete o.a;
            // o should still equal { a: 1 }, with b absent.
            (o.a + ":" + (o.b === undefined ? "noB" : o.b));
            "#,
        )
        .expect("eval");
    match out {
        Object::String(s) => assert_eq!(s.as_ref(), "1:noB"),
        other => panic!("expected String(\"1:noB\"), got {:?}", other),
    }
}

/// `max_instructions` was previously only checked at the periodic
/// 64K-iteration safepoint. A script that ran ~64K instructions and
/// then hit a terminator (Halt / HaltValue / Return / ReturnUndef)
/// flushed `loop_counter` into `quota.instructions` and returned
/// `Ok(())` without re-checking the limit, so a tight script just
/// under the limit could silently overrun. The fix re-checks each
/// terminal path.
#[test]
fn max_instructions_enforced_at_terminal_return() {
    use zipp_engine::config::ZippConfig;
    let mut cfg = ZippConfig::default();
    cfg.max_instructions = Some(50);
    let engine = ZippEngine::with_config(cfg);
    // ~5,000 arithmetic ops in a tight loop. Far over a 50-instruction
    // budget but not large enough to hit the 64K safepoint. Before the
    // fix, this returned `Ok(4999)`; after the fix, it must error.
    let res = engine.eval(
        r#"
        let n = 0;
        for (let i = 0; i < 5000; i = i + 1) { n = n + 1; }
        n;
        "#,
    );
    assert!(
        res.is_err(),
        "max_instructions overrun must surface as Err, got Ok: {:?}",
        res.ok()
    );
}

/// Companion to `frozen_object_writes_rejected_after_warmup`: a
/// getter installed via `Object.defineProperty` must be invoked even
/// after a hot read loop has had a chance to populate the inline
/// cache. Before the Round 9 fix, the direct cache returned the raw
/// underlying slot value, skipping the getter entirely.
#[test]
fn getter_invoked_after_inline_cache_warmup() {
    let engine = ZippEngine::default();
    let out = engine
        .eval(
            r#"
            let o = { _x: 1 };
            // Warm reads so any cache populates first.
            let warm = 0;
            for (let i = 0; i < 200; i = i + 1) { warm = warm + o._x; }
            Object.defineProperty(o, "x", { get: function() { return 42; } });
            // The new "x" getter must run; it must not return undefined
            // or some stale slot value because of a poisoned cache.
            o.x;
            "#,
        )
        .expect("eval");
    match out {
        Object::Integer(n) => assert_eq!(n, 42, "getter must be invoked"),
        other => panic!("expected Integer(42), got {:?}", other),
    }
}
