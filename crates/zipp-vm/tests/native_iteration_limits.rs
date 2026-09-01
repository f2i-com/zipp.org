//! Safe-profile regressions for guest-controlled loops that run inside one
//! native VM operation, outside the ordinary bytecode step meter.
//!
//! Every hostile size below is derived from the live ceilings in
//! `zipp_vm::safe_native_limits`. v0.0.10 raised the work budget 256x, and the
//! copied numbers this file used to carry then fit comfortably, so every guard
//! here passed while exercising nothing.

#![cfg(feature = "safe-sandbox")]

use zipp_vm::safe_native_limits::{
    MAX_NATIVE_ITERATION_WORK as WORK, MAX_TEMPORAL_CALENDAR_ITERATIONS as CALENDAR_STEPS,
};

/// Largest `n` with `n * n <= WORK`, so `QUADRATIC_OVER * QUADRATIC_OVER`
/// exceeds the budget by a comfortable margin for the pairwise loops below.
const QUADRATIC_OVER: u64 = isqrt(WORK) + 64;

/// The comparator TypedArray sort is charged `n * (n - 1) / 2` up front
/// (`builtins::typed_array_sort_work_bound`); this element count exceeds it.
const SORT_OVER: u64 = isqrt(2 * WORK) + 64;


const fn isqrt(n: u64) -> u64 {
    let mut lo = 0u64;
    let mut hi = 1u64 << 32;
    while lo < hi {
        let mid = (lo + hi + 1) / 2;
        if mid.saturating_mul(mid) <= n {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    lo
}

/// The timer queue charges `n * (log2(n) + 2)` for its `n`th entry
/// (`natives::timer_queue_work_bound`); this is the first count past the
/// budget. A runtime search: at 2^26 the walk is millions of steps, too many
/// for constant evaluation.
fn first_timer_count_over_budget() -> u64 {
    let mut n = 1u64;
    loop {
        let levels = (u64::BITS - (n - 1).leading_zeros()) as u64;
        if n.saturating_mul(levels + 2) > WORK {
            return n;
        }
        n += 1;
    }
}

fn run_ok(source: &str) -> Vec<String> {
    let result = zipp_vm::run(source).expect("source compiles");
    assert!(
        result.error.is_none(),
        "unexpected uncaught runtime error: {:?}",
        result.error
    );
    result.output
}

/// Substitute the live limits into a JS template so the shapes track the
/// constants instead of copying them.
fn sized(template: &str) -> String {
    template
        .replace("@OVER_WORK@", &(WORK + 1).to_string())
        .replace("@OVER_WORK_PLUS_1@", &(WORK + 2).to_string())
        .replace("@QUADRATIC_OVER@", &QUADRATIC_OVER.to_string())
        .replace("@SORT_OVER@", &SORT_OVER.to_string())
        .replace("@TIMERS_OVER@", &first_timer_count_over_budget().to_string())
        .replace("@OVER_CALENDAR_STEPS@", &(CALENDAR_STEPS + 1).to_string())
}

fn assert_catchable_range_error(operation: &str) {
    let source = sized(&format!(
        r#"
        try {{
            {operation};
            console.log("completed");
        }} catch (error) {{
            console.log(error instanceof RangeError ? "range" : "other");
        }}
        "#
    ));
    assert_eq!(run_ok(&source), ["range"], "operation: {operation}");
}

#[test]
fn oversized_typed_array_native_scans_fail_closed() {
    assert_catchable_range_error("new Uint8Array(@OVER_WORK@).fill(1)");
    assert_catchable_range_error("new Uint8Array(@OVER_WORK@).indexOf(1)");
    assert_catchable_range_error("new Uint8Array(@OVER_WORK@).reverse()");
    assert_catchable_range_error("new Uint8Array(@OVER_WORK@).sort()");

    // The callback sort uses insertion sort so its adversarial work is
    // quadratic; a much smaller element count can already exceed the same budget.
    assert_catchable_range_error("new Uint8Array(@SORT_OVER@).sort(Number)");
}

#[test]
fn typed_array_copy_and_reflection_paths_preflight_before_index_reads() {
    assert_catchable_range_error("new Uint8Array(new Uint8Array(@OVER_WORK@))");
    assert_catchable_range_error("Object.keys(new Uint8Array(@OVER_WORK@))");
    assert_catchable_range_error("Reflect.ownKeys(new Uint8Array(@OVER_WORK@))");

    assert_eq!(
        run_ok(&sized(
            r#"
            var reads = 0;
            var source = new Proxy({length: @OVER_WORK@}, {
                get: function (target, key) {
                    if (key !== "length" && key !== Symbol.iterator) reads++;
                    return target[key];
                }
            });
            try { new Uint8Array(source); }
            catch (error) { console.log(error instanceof RangeError, reads); }

            reads = 0;
            try { new Uint8Array(@OVER_WORK@).set(source); }
            catch (error) { console.log(error instanceof RangeError, reads); }

            // Keep ToLength in u64 until after preflight: 2^32 used to narrow
            // to zero on wasm32 and silently skip the source.
            source.length = 4294967296;
            reads = 0;
            try { new Uint8Array(1).set(source); }
            catch (error) { console.log(error instanceof RangeError, reads); }
            "#,
        )),
        ["true 0", "true 0", "true 0"]
    );
}

#[test]
fn argument_lists_preflight_tolength_before_host_width_conversion() {
    // `new Array(n)` past the dense cap is a virtual-length array: the length
    // is real, the elements are holes, so the list is as long as the budget
    // forbids without the test paying to materialise it.
    assert_eq!(
        run_ok(&sized(
            r#"
            var reads = 0;
            var list = new Proxy({length: 4294967296}, {
                get: function (target, key) {
                    if (key !== "length") reads++;
                    return target[key];
                }
            });
            try { Reflect.apply(function () {}, null, list); }
            catch (error) { console.log(error instanceof RangeError, reads); }
            var long = new Array(@OVER_WORK@);
            try { Reflect.apply(function () {}, null, long); }
            catch (error) { console.log(error instanceof RangeError); }
            "#,
        )),
        ["true 0", "true"]
    );
}

/// Dynamic drains are charged one unit per element pulled, so exceeding the
/// budget costs the engine MAX_NATIVE_ITERATION_WORK iterations: seconds in a
/// release build, minutes in a debug build. Those tests run only optimised.
#[test]
#[cfg_attr(debug_assertions, ignore = "walks MAX_NATIVE_ITERATION_WORK elements dynamically (minutes unoptimised); run with --release")]
fn iterator_helpers_bound_a_single_lazy_or_consuming_call() {
    assert_catchable_range_error(
        "new Uint8Array(@OVER_WORK@).values().drop(@OVER_WORK@).next()",
    );
    assert_catchable_range_error("new Uint8Array(@OVER_WORK@).values().toArray()");
}

#[test]
fn proxy_own_keys_set_validation_preserves_semantics() {
    assert_eq!(
        run_ok(
            r#"
            var target = {};
            Object.defineProperty(target, "fixed", {value: 1, configurable: false});
            Object.preventExtensions(target);
            var valid = new Proxy(target, {ownKeys: function () { return ["fixed"]; }});
            console.log(Reflect.ownKeys(valid).join(","));
            var duplicate = new Proxy({}, {ownKeys: function () { return ["x", "x"]; }});
            try { Reflect.ownKeys(duplicate); }
            catch (error) { console.log(error instanceof TypeError); }
            "#,
        ),
        ["fixed", "true"]
    );
}

#[test]
fn temporal_calendar_totals_obey_the_native_work_budget() {
    // `total` brackets a calendar unit by stepping from the anchor; the walk
    // is bounded by MAX_TEMPORAL_CALENDAR_ITERATIONS. Months keep the far end
    // of a walk that long inside Temporal's representable range, so the loop
    // bound is what fires rather than a date-range error.
    assert_catchable_range_error(
        r#"new Temporal.Duration(0, @OVER_CALENDAR_STEPS@).total({
            unit: "month",
            relativeTo: new Temporal.PlainDate(-100000, 1, 1)
        })"#,
    );
}

#[test]
#[cfg_attr(debug_assertions, ignore = "walks MAX_NATIVE_ITERATION_WORK elements dynamically (minutes unoptimised); run with --release")]
fn collection_constructors_and_entry_consumers_bound_full_drains() {
    assert_catchable_range_error("new Set(new Uint8Array(@OVER_WORK@))");
    assert_catchable_range_error("new Map(new Uint8Array(@OVER_WORK@).values().map(Array))");
    assert_catchable_range_error(
        "Object.fromEntries(new Uint8Array(@OVER_WORK@).values().map(Array))",
    );
}

#[test]
fn set_algebra_bounds_linear_and_quadratic_native_work() {
    // Real-set membership is linear over the cloned backing list in this
    // branch, so n x n comparisons exceed the aggregate work budget once n
    // passes the square root of the budget.
    assert_catchable_range_error(
        r#"
        var left = new Set(), right = new Set();
        for (var i = 0; i < @QUADRATIC_OVER@; i++) { left.add(i); right.add(i + @QUADRATIC_OVER@ * 2); }
        left.intersection(right)
        "#,
    );

}

/// A set-like can under-report size while its keys iterator is much longer.
/// Dynamic accounting must still stop and close the native full drain.
#[test]
#[cfg_attr(debug_assertions, ignore = "walks MAX_NATIVE_ITERATION_WORK elements dynamically (minutes unoptimised); run with --release")]
fn set_like_keys_drain_is_bounded_dynamically() {
    assert_catchable_range_error(
        r#"
        new Set([0]).isSupersetOf({
            size: 0,
            has: function () { return true; },
            keys: function () { return new Uint8Array(@OVER_WORK@).values(); }
        })
        "#,
    );
}

#[test]
#[cfg_attr(debug_assertions, ignore = "walks MAX_NATIVE_ITERATION_WORK elements dynamically (minutes unoptimised); run with --release")]
fn promise_combinators_reject_oversized_native_drains() {
    assert_eq!(
        run_ok(&sized(
            r#"
            var reason;
            function Capability(executor) {
                executor(function () {}, function (error) { reason = error; });
            }
            var shared = {then: function () {}};
            Capability.resolve = function () { return shared; };
            Promise.race.call(Capability, new Uint8Array(@OVER_WORK@));
            console.log(reason instanceof RangeError);
            "#,
        )),
        ["true"]
    );
}

#[test]
fn observable_regexp_protocol_loops_and_capture_lists_are_bounded() {
    assert_catchable_range_error(
        r#"
        var rx = /x/;
        rx.exec = function () { return {0: "x", index: 0, length: @OVER_WORK_PLUS_1@}; };
        rx[Symbol.replace]("x", "")
        "#,
    );

    assert_catchable_range_error(
        r#"
        var rx = /x/g, shared = {0: "x"};
        rx.exec = function () { return shared; };
        rx[Symbol.match]("")
        "#,
    );

    assert_catchable_range_error(
        r#"
        var originalExec = RegExp.prototype.exec;
        RegExp.prototype.exec = function () {
            this.lastIndex = 1;
            return {0: "x", length: @OVER_WORK_PLUS_1@};
        };
        try { /x/[Symbol.split]("x"); }
        finally { RegExp.prototype.exec = originalExec; }
        "#,
    );
}

#[test]
fn timers_reject_non_finite_delays_and_bound_queue_drain_work() {
    assert_catchable_range_error("setTimeout(function () {}, Infinity)");
    assert_catchable_range_error("$262.agent.sleep(Infinity)");
    assert_catchable_range_error(
        r#"
        for (var i = 0; i < @TIMERS_OVER@; i++) {
            setTimeout(function () {}, 0);
        }
        "#,
    );
}

#[test]
fn json_parse_source_index_stays_linear_and_keeps_last_duplicate_source() {
    use std::fmt::Write as _;

    let count = 12_000usize;
    let mut json = String::from("{");
    for i in 0..count {
        if i != 0 {
            json.push(',');
        }
        write!(&mut json, "\"k{i}\":{i}").unwrap();
    }
    json.push_str(",\"dup\":1,\"dup\":2}");
    let source = format!(
        r#"
        var seenLast, seenDup;
        var value = JSON.parse({json:?}, function (key, item, context) {{
            if (key === "k11999") seenLast = context.source;
            if (key === "dup") seenDup = context.source;
            return item;
        }});
        console.log(value.k11999, seenLast, value.dup, seenDup);
        "#,
    );
    assert_eq!(run_ok(&source), ["11999 11999 2 2"]);
}

#[test]
fn class_private_name_index_avoids_quadratic_makeclass_work() {
    use std::fmt::Write as _;

    let mut fields = String::new();
    for i in 0..12_000usize {
        write!(&mut fields, "#p{i};").unwrap();
    }
    let source = format!(
        r#"
        class Many {{ {fields} }}
        class Semantics {{
            #field;
            get #pair() {{ return 1; }}
            set #pair(value) {{}}
            has(value) {{ return #field in value; }}
        }}
        var a = new Semantics(), b = new Semantics();
        console.log(typeof Many, a.has(b));
        "#,
    );
    assert_eq!(run_ok(&source), ["function true"]);
}

#[test]
fn locale_variant_and_extension_sort_work_is_bounded() {
    // Distinct six-character variants: the tag parser charges 16 units per
    // byte and the variant sort n*log2(n)*8, so this many exceed the budget
    // together while each subtag stays well-formed. ApplyOptionsToTag and the
    // language-tag parser must both fail before that sort, without changing
    // ordinary canonical ordering/duplicate semantics.
    let count = 400_000usize;
    let variants = (0..count)
        .map(|i| format!("{i:06}"))
        .collect::<Vec<_>>()
        .join("-");
    assert_catchable_range_error(&format!(
        "new Intl.Locale('en', {{ variants: {variants:?} }})"
    ));
    assert_catchable_range_error(&format!(
        "Intl.getCanonicalLocales({:?})",
        format!("en-{variants}")
    ));

    // Unicode extension attributes had the same Vec::contains + sort shape.
    let attributes = (0..count)
        .rev()
        .map(|i| format!("a{i:05x}"))
        .collect::<Vec<_>>()
        .join("-");
    assert_catchable_range_error(&format!(
        "Intl.getCanonicalLocales({:?})",
        format!("en-u-{attributes}")
    ));

    assert_eq!(
        run_ok(
            r#"
            console.log(new Intl.Locale("en", {variants: "fonipa-1996"}).toString());
            try { new Intl.Locale("en", {variants: "fonipa-fonipa"}); }
            catch (error) { console.log(error instanceof RangeError); }
            console.log(Intl.getCanonicalLocales("en-u-foo-foo")[0]);
            "#,
        ),
        ["en-1996-fonipa", "true", "en-u-foo"]
    );
}

#[test]
fn segmenter_boundary_queries_obey_the_native_work_budget() {
    assert_catchable_range_error(
        "new Intl.Segmenter('en').segment('a'.repeat(@OVER_WORK@)).containing(0)",
    );
    assert_catchable_range_error(
        "new Intl.Segmenter('en').segment('a'.repeat(@OVER_WORK@))[Symbol.iterator]().next()",
    );
}
