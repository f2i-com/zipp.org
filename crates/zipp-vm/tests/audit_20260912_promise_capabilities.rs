//! Promise combinators must reject through the constructor's captured capability,
//! including abrupt setup and iteration, and preserve IteratorClose's original error.

use zipp_vm::embed::{compile_script, JsValue, ScriptState};

const PRELUDE: &str = r#"
    const methods = ['all', 'allSettled', 'any', 'race'];
    function check(condition, label) { if (!condition) throw new Error(label); }
    function fixture(phase, rejectThrows) {
        const error = { phase };
        const rejectError = { phase: 'reject' };
        const closeError = { phase: 'close' };
        const log = [];
        let instance;
        function C(executor) {
            instance = this;
            executor(function() { log.push('resolve'); }, function(reason) {
                'use strict';
                check(this === undefined, 'reject receiver');
                check(phase === 'resolve-noncallable' || phase === 'iterator-nonobject'
                    ? reason instanceof TypeError : reason === error, 'rejection identity: ' + phase);
                log.push('reject');
                if (rejectThrows) throw rejectError;
            });
        }
        Object.defineProperty(C, 'resolve', { get() {
            if (phase === 'resolve-getter') throw error;
            if (phase === 'resolve-noncallable') return 1;
            return function(value) {
                check(this === C, 'resolve receiver');
                if (phase === 'resolve-call') throw error;
                return { get then() {
                    if (phase === 'then-getter') throw error;
                    return function() { if (phase === 'then-call') throw error; };
                } };
            };
        } });
        const iterable = { get [Symbol.iterator]() {
            if (phase === 'iterator-getter') throw error;
            return function() {
                if (phase === 'iterator-call') throw error;
                if (phase === 'iterator-nonobject') return 1;
                return {
                    get next() {
                        if (phase === 'next-getter') throw error;
                        return function() {
                            if (phase === 'next-call') throw error;
                            return {
                                get done() { if (phase === 'done-getter') throw error; return false; },
                                get value() { if (phase === 'value-getter') throw error; return 1; }
                            };
                        };
                    },
                    get return() {
                        log.push('close');
                        // A throw while closing must not replace the original reason,
                        // poison the reject callback, or survive a handled rejection.
                        throw closeError;
                    }
                };
            };
        } };
        return { C, iterable, log, rejectError, instance: () => instance };
    }
    function exercise(phases, rejectThrows) {
        for (const name of methods) for (const phase of phases) {
            const f = fixture(phase, rejectThrows);
            let caught = false;
            try {
                const result = Promise[name].call(f.C, f.iterable);
                check(!rejectThrows, name + ' must propagate reject throw: ' + phase);
                check(result === f.instance(), name + ' capability result: ' + phase);
            } catch (e) {
                if (!rejectThrows || e !== f.rejectError) throw e;
                caught = true;
            }
            check(caught === !!rejectThrows, name + ' reject completion: ' + phase);
            const closes = phase === 'resolve-call' || phase === 'then-getter' || phase === 'then-call';
            check(f.log.join(',') === (closes ? 'close,reject' : 'reject'),
                name + '/' + phase + ': ' + f.log.join(','));
            let sum = 0;
            for (let i = 0; i < 1000; i++) sum += i;
            check(sum === 499500, 'handled error poisoned later execution');
        }
    }
"#;

fn run(source: &str) -> ScriptState {
    std::env::set_var("ZIPP_GC_STRESS", "1");
    let mut state = compile_script(&format!("{PRELUDE}\n{source}"))
        .expect("compile Promise capability regression");
    state.run_init().expect("run Promise capability regression");
    assert_eq!(
        state.eval_in_context("'healthy'"),
        Ok(JsValue::String("healthy".into()))
    );
    assert_eq!(state.host_result_roots_for_test(), 0);
    state
}

#[test]
fn resolve_lookup_errors_use_the_custom_reject_capability() {
    run("exercise(['resolve-getter', 'resolve-noncallable'], false);");
}

#[test]
fn iterator_acquisition_errors_use_the_custom_reject_capability() {
    run("exercise(['iterator-getter', 'iterator-call', 'iterator-nonobject', 'next-getter'], false);");
}

#[test]
fn iterator_step_errors_reject_without_closing() {
    run("exercise(['next-call', 'done-getter', 'value-getter'], false);");
}

#[test]
fn resolve_call_errors_keep_the_original_reason_when_close_throws() {
    run("exercise(['resolve-call'], false);");
}

#[test]
fn then_errors_close_and_use_the_custom_reject_capability() {
    run("exercise(['then-getter', 'then-call'], false);");
}

#[test]
fn reject_capability_throws_propagate_synchronously() {
    run("exercise(['resolve-getter', 'resolve-noncallable', 'iterator-getter', 'iterator-call', 'iterator-nonobject', 'next-getter', 'next-call', 'done-getter', 'value-getter', 'resolve-call', 'then-getter', 'then-call'], true);");
}

#[test]
fn native_promises_reject_and_successful_combinators_still_settle() {
    let mut state = run(r#"
        let checks = 0;
        for (const name of methods) {
            const error = { name };
            Promise[name]({ get [Symbol.iterator]() { throw error; } }).then(
                () => { throw new Error(name + ' unexpectedly fulfilled'); },
                reason => { check(reason === error, name + ' native reason'); checks++; }
            );
        }
        Promise.all([1, 2]).then(value => { check(value.join(',') === '1,2', 'all'); checks++; });
        Promise.allSettled([Promise.resolve(3), Promise.reject(4)]).then(value => {
            check(value[0].status === 'fulfilled' && value[0].value === 3, 'allSettled fulfill');
            check(value[1].status === 'rejected' && value[1].reason === 4, 'allSettled reject');
            checks++;
        });
        Promise.any([Promise.reject(5), 6]).then(value => { check(value === 6, 'any'); checks++; });
        Promise.race([7]).then(value => { check(value === 7, 'race'); checks++; });
        globalThis.checkNativeResults = function() { check(checks === 8, 'native checks: ' + checks); };
    "#);
    // run_init drains the event loop; separately verify counts so a silently
    // uncalled or rejected assertion callback cannot make the control pass.
    assert_eq!(
        state.call_global("checkNativeResults", &[]),
        Ok(JsValue::Undefined)
    );
}
