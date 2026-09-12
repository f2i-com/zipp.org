//! Iterator helpers hold temporary values across guest callbacks. They must
//! remain traced while collection is enabled, including abrupt IteratorClose.

use zipp_vm::embed::{compile_script, JsValue, ScriptState};

const SOURCE: &str = r#"
    function churn() {
        for (let wave = 0; wave < 4; wave++) {
            const garbage = [];
            for (let i = 0; i < 200; i++) garbage.push({n: i, text: 'garbage-' + i});
        }
    }
    function source(count) {
        let i = 0;
        return {
            next() {
                churn();
                return i < count ? {done: false, value: {marker: 'value-' + i++}} : {done: true};
            },
            return() { churn(); return {done: true}; }
        };
    }
    function collect() {
        return Iterator.prototype.toArray.call(source(3)).map(v => v.marker).join(',');
    }
    function reduce() {
        return Iterator.prototype.reduce.call(source(3),
            (acc, v) => ({marker: acc.marker + ',' + v.marker})).marker;
    }
    function find() {
        return Iterator.prototype.find.call(source(3), function(v) {
            v = null;
            churn();
            return true;
        }).marker;
    }
    function filter() {
        return Iterator.prototype.filter.call(source(1), function(v) {
            v = null;
            churn();
            return true;
        }).next().value.marker;
    }
    function concat() {
        const first = {[Symbol.iterator]() { return ['first'][Symbol.iterator](); }};
        const second = {get [Symbol.iterator]() {
            churn();
            return function() { return ['second'][Symbol.iterator](); };
        }};
        return Iterator.concat(first, second).toArray().join(',');
    }
    function callbackError() {
        try {
            Iterator.prototype.forEach.call(source(1), function() {
                throw {marker: 'original-error', nested: {value: 42}};
            });
        } catch(e) { return e.marker + ':' + e.nested.value; }
        throw new Error('callback did not throw');
    }
    function cachedNext() {
        let count = 0;
        const iterator = {get next() {
            return function() {
                return count++ < 3 ? {done: false, value: count} : {done: true};
            };
        }};
        let sum = 0;
        Iterator.prototype.forEach.call(iterator, function(v) { sum += v; churn(); });
        return sum;
    }
    function failedCollect() {
        let count = 0;
        return Iterator.prototype.toArray.call({next() {
            if (count++) { churn(); throw new Error('collect failure'); }
            return {done: false, value: {marker: 'discarded'}};
        }});
    }
    function failedConcat() {
        return Iterator.concat(
            {[Symbol.iterator]() { return [1][Symbol.iterator](); }},
            {get [Symbol.iterator]() { churn(); throw new Error('concat failure'); }}
        );
    }
"#;

fn fixture() -> ScriptState {
    std::env::set_var("ZIPP_GC_STRESS", "1");
    let mut state = compile_script(SOURCE).expect("compile iterator root regression");
    state
        .run_init()
        .expect("initialize iterator root regression");
    state
}

fn expect_string(name: &str, expected: &str) {
    let mut state = fixture();
    for _ in 0..3 {
        assert_eq!(
            state.call_global(name, &[]),
            Ok(JsValue::String(expected.into()))
        );
        assert_eq!(state.host_result_roots_for_test(), 0);
    }
}

#[test]
fn collected_values_survive_later_iterator_steps() {
    expect_string("collect", "value-0,value-1,value-2");
}

#[test]
fn reduce_accumulators_survive_later_iterator_steps() {
    expect_string("reduce", "value-0,value-1,value-2");
}

#[test]
fn find_keeps_its_selected_value_through_predicate_and_close() {
    expect_string("find", "value-0");
}

#[test]
fn filter_keeps_its_selected_value_through_the_predicate() {
    expect_string("filter", "value-0");
}

#[test]
fn concat_keeps_earlier_pairs_during_later_iterator_getters() {
    expect_string("concat", "first,second");
}

#[test]
fn callback_exception_survives_allocating_iterator_close() {
    expect_string("callbackError", "original-error:42");
}

#[test]
fn consuming_helpers_keep_a_fresh_cached_next_function() {
    let mut state = fixture();
    assert_eq!(
        state.call_global("cachedNext", &[]),
        Ok(JsValue::Number(6.0))
    );
    assert_eq!(state.host_result_roots_for_test(), 0);
}

#[test]
fn temporary_root_scopes_unwind_on_collection_and_concat_errors() {
    let mut state = fixture();
    for _ in 0..3 {
        for (name, message) in [
            ("failedCollect", "collect failure"),
            ("failedConcat", "concat failure"),
        ] {
            let error = state
                .call_global(name, &[])
                .expect_err("guest callback throws");
            assert!(error.contains(message), "{error}");
            assert_eq!(state.host_result_roots_for_test(), 0);
        }
        assert_eq!(
            state.call_global("cachedNext", &[]),
            Ok(JsValue::Number(6.0))
        );
        assert_eq!(state.host_result_roots_for_test(), 0);
    }
}
