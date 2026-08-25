//! Safe-profile regressions for guest-controlled loops that run inside one
//! native VM operation, outside the ordinary bytecode step meter.

#![cfg(feature = "safe-sandbox")]

fn run_ok(source: &str) -> Vec<String> {
    let result = zipp_vm::run(source).expect("source compiles");
    assert!(
        result.error.is_none(),
        "unexpected uncaught runtime error: {:?}",
        result.error
    );
    result.output
}

fn assert_catchable_range_error(operation: &str) {
    let source = format!(
        r#"
        try {{
            {operation};
            console.log("completed");
        }} catch (error) {{
            console.log(error instanceof RangeError ? "range" : "other");
        }}
        "#
    );
    assert_eq!(run_ok(&source), ["range"], "operation: {operation}");
}

#[test]
fn oversized_typed_array_native_scans_fail_closed() {
    assert_catchable_range_error("new Uint8Array(262145).fill(1)");
    assert_catchable_range_error("new Uint8Array(262145).indexOf(1)");
    assert_catchable_range_error("new Uint8Array(262145).reverse()");
    assert_catchable_range_error("new Uint8Array(262145).sort()");

    // The callback sort uses insertion sort so its adversarial work is
    // quadratic; a small element count can already exceed the same budget.
    assert_catchable_range_error("new Uint8Array(725).sort(Number)");
}

#[test]
fn typed_array_copy_and_reflection_paths_preflight_before_index_reads() {
    assert_catchable_range_error("new Uint8Array(new Uint8Array(262145))");
    assert_catchable_range_error("Object.keys(new Uint8Array(262145))");
    assert_catchable_range_error("Reflect.ownKeys(new Uint8Array(262145))");

    assert_eq!(
        run_ok(
            r#"
            var reads = 0;
            var source = new Proxy({length: 262145}, {
                get: function (target, key) {
                    if (key !== "length" && key !== Symbol.iterator) reads++;
                    return target[key];
                }
            });
            try { new Uint8Array(source); }
            catch (error) { console.log(error instanceof RangeError, reads); }

            reads = 0;
            try { new Uint8Array(262145).set(source); }
            catch (error) { console.log(error instanceof RangeError, reads); }

            // Keep ToLength in u64 until after preflight: 2^32 used to narrow
            // to zero on wasm32 and silently skip the source.
            source.length = 4294967296;
            reads = 0;
            try { new Uint8Array(1).set(source); }
            catch (error) { console.log(error instanceof RangeError, reads); }
            "#,
        ),
        ["true 0", "true 0", "true 0"]
    );
}

#[test]
fn argument_lists_preflight_tolength_before_host_width_conversion() {
    assert_eq!(
        run_ok(
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
            var dense = [];
            for (var i = 0; i < 262145; i++) dense.push(0);
            try { Reflect.apply(function () {}, null, dense); }
            catch (error) { console.log(error instanceof RangeError); }
            "#,
        ),
        ["true 0", "true"]
    );
}

#[test]
fn iterator_helpers_bound_a_single_lazy_or_consuming_call() {
    assert_catchable_range_error("new Uint8Array(262145).values().drop(262145).next()");
    assert_catchable_range_error("new Uint8Array(262145).values().toArray()");
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
    assert_catchable_range_error(
        r#"new Temporal.Duration(300000).total({
            unit: "year",
            relativeTo: new Temporal.PlainDate(-100000, 1, 1)
        })"#,
    );
}

#[test]
fn collection_constructors_and_entry_consumers_bound_full_drains() {
    assert_catchable_range_error("new Set(new Uint8Array(262145))");
    assert_catchable_range_error("new Map(new Uint8Array(262145).values().map(Array))");
    assert_catchable_range_error("Object.fromEntries(new Uint8Array(262145).values().map(Array))");
}

#[test]
fn set_algebra_bounds_linear_and_quadratic_native_work() {
    // Real-set membership is linear over the cloned backing list in this
    // branch, so 600x600 comparisons exceed the aggregate work budget.
    assert_catchable_range_error(
        r#"
        var left = new Set(), right = new Set();
        for (var i = 0; i < 600; i++) { left.add(i); right.add(i + 1000); }
        left.intersection(right)
        "#,
    );

    // A set-like can under-report size while its keys iterator is much longer.
    // Dynamic accounting must still stop and close the native full drain.
    assert_catchable_range_error(
        r#"
        new Set([0]).isSupersetOf({
            size: 0,
            has: function () { return true; },
            keys: function () { return new Uint8Array(262145).values(); }
        })
        "#,
    );
}

#[test]
fn promise_combinators_reject_oversized_native_drains() {
    assert_eq!(
        run_ok(
            r#"
            var reason;
            function Capability(executor) {
                executor(function () {}, function (error) { reason = error; });
            }
            var shared = {then: function () {}};
            Capability.resolve = function () { return shared; };
            Promise.race.call(Capability, new Uint8Array(262145));
            console.log(reason instanceof RangeError);
            "#,
        ),
        ["true"]
    );
}

#[test]
fn observable_regexp_protocol_loops_and_capture_lists_are_bounded() {
    assert_catchable_range_error(
        r#"
        var rx = /x/;
        rx.exec = function () { return {0: "x", index: 0, length: 262146}; };
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
            return {0: "x", length: 262146};
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
        for (var i = 0; i < 20000; i++) {
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
    // Two thousand distinct four-byte variants fit comfortably below the
    // linear byte budget, but sorting them can exceed the native-call budget.
    // ApplyOptionsToTag and the language-tag parser must both fail before that
    // sort, without changing ordinary canonical ordering/duplicate semantics.
    let variants = (0..2_000usize)
        .map(|i| format!("{i:04}"))
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
    let attributes = (0..2_000usize)
        .rev()
        .map(|i| format!("a{i:03x}"))
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
        "new Intl.Segmenter('en').segment('a'.repeat(32769)).containing(0)",
    );
    assert_catchable_range_error(
        "new Intl.Segmenter('en').segment('a'.repeat(32769))[Symbol.iterator]().next()",
    );
}
