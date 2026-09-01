//! Safe-profile bounds on string growth inside one native operation.
//!
//! Every hostile size below is derived from the live ceilings in
//! `zipp_vm::safe_native_limits`. v0.0.10 raised the string cap from 1 MiB to
//! 128 MiB and the work budget 256x; the copied numbers this file used to carry
//! then fit comfortably and every guard here went untested.

#![cfg(feature = "safe-sandbox")]

use zipp_vm::safe_native_limits::{
    MAX_NATIVE_ITERATION_WORK as WORK, MAX_STRING_BYTES as BYTES, MAX_STRING_UNITS as UNITS,
};

fn run_ok(source: &str) -> Vec<String> {
    let result = zipp_vm::run(source).expect("source compiles");
    assert!(
        result.error.is_none(),
        "unexpected uncaught runtime error: {:?}",
        result.error
    );
    result.output
}

/// Substitute the live limits into a JS template. One-byte strings are bound
/// by `BYTES`, two-byte strings by `UNITS`, native loops by `WORK`.
fn sized(template: &str) -> String {
    let bytes = BYTES as u64;
    let units = UNITS as u64;
    template
        .replace("@BYTES@", &bytes.to_string())
        .replace("@BYTES_MINUS_1@", &(bytes - 1).to_string())
        .replace("@BYTES_MINUS_5@", &(bytes - 5).to_string())
        .replace("@BYTES_MINUS_6@", &(bytes - 6).to_string())
        .replace("@BYTES_MINUS_26@", &(bytes - 26).to_string())
        .replace("@HALF_BYTES_PLUS_1@", &(bytes / 2 + 1).to_string())
        .replace("@BYTES_DIV_4_PLUS_1@", &(bytes / 4 + 1).to_string())
        .replace("@BYTES_DIV_6_PLUS_1@", &(bytes / 6 + 1).to_string())
        .replace("@BYTES_TIMES_3_DIV_4_PLUS_1@", &(bytes / 4 * 3 + 1).to_string())
        .replace("@HALF_UNITS_PLUS_1@", &(units / 2 + 1).to_string())
        .replace("@UNITS_DIV_9_PLUS_1@", &(units / 9 + 1).to_string())
        .replace("@OVER_WORK@", &(WORK + 1).to_string())
        .replace("@HALF_WORK@", &(WORK / 2).to_string())
        .replace("@HALF_WORK_PLUS_1@", &(WORK / 2 + 1).to_string())
        .replace("@WORK@", &WORK.to_string())
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
fn unicode_expansion_paths_obey_the_safe_string_cap() {
    // U+0587 uppercases to two Armenian capitals. U+FDFA has an 18-unit NFKD
    // expansion (33 bytes, so the byte cap is the one that fires; the input is
    // sized to cross both). Both inputs are comfortably below the cap while
    // their output crosses it.
    assert_catchable_range_error(r#""\u0587".repeat(@HALF_UNITS_PLUS_1@).toUpperCase()"#);
    assert_catchable_range_error(r#""\uFDFA".repeat(@UNITS_DIV_9_PLUS_1@).normalize("NFKD")"#);
    // Every escaped space becomes four bytes.
    assert_catchable_range_error(r#"RegExp.escape(" ".repeat(@BYTES_DIV_4_PLUS_1@))"#);
}

#[test]
fn codecs_preflight_amplified_results() {
    // Six output bytes per input character for both codecs.
    assert_catchable_range_error(r#"encodeURIComponent("\u00e9".repeat(@BYTES_DIV_6_PLUS_1@))"#);
    assert_catchable_range_error(r#"escape("\u0100".repeat(@BYTES_DIV_6_PLUS_1@))"#);
    assert_catchable_range_error("new Uint8Array(@BYTES_TIMES_3_DIV_4_PLUS_1@).toBase64()");
    assert_catchable_range_error("new Uint8Array(@HALF_BYTES_PLUS_1@).toHex()");
}

#[test]
fn native_iteration_and_aggregate_builders_are_bounded() {
    // The length guard runs before raw[0], so an enormous array-like cannot
    // spend a whole bytecode instruction in String.raw property reads.
    assert_eq!(
        run_ok(&sized(
            r#"
            var reads = 0;
            var raw = new Proxy({}, { get: function (target, key) {
                if (key === "length") return @OVER_WORK@;
                reads++;
                return "";
            }});
            try { String.raw({raw: raw}); } catch (error) {
                console.log(error instanceof RangeError, reads);
            }
            "#,
        )),
        ["true 0"]
    );

    // TypedArray.from must reject an oversized array-like before reading index
    // zero (the iterable case, which drains dynamically, is its own test).
    assert_eq!(
        run_ok(&sized(
            r#"
            var reads = 0;
            var source = {length: @OVER_WORK@};
            Object.defineProperty(source, "0", {get: function () { reads++; return 1; }});
            try { Uint8Array.from(source); } catch (error) {
                console.log(error instanceof RangeError, reads);
            }
            "#,
        )),
        ["true 0"]
    );

    assert_catchable_range_error(
        r#"var x = "x".repeat(@HALF_BYTES_PLUS_1@); new Intl.ListFormat().format([x, x])"#,
    );
    assert_catchable_range_error(
        r#"Function("a".repeat(@HALF_BYTES_PLUS_1@), "b".repeat(@HALF_BYTES_PLUS_1@), "")"#,
    );
    assert_catchable_range_error(
        r#"new Intl.Collator().compare("a".repeat(@HALF_WORK_PLUS_1@), "a".repeat(@HALF_WORK@))"#,
    );
    assert_catchable_range_error(r#""a".repeat(@OVER_WORK@)[Symbol.iterator]()"#);

    // Array.from's custom-constructor branch is not protected by the dense
    // Array result ceiling. In particular, 2^32 used to narrow to zero on
    // wasm32 before the property loop was entered.
    assert_eq!(
        run_ok(
            r#"
            var calls = 0;
            function Custom() { calls++; return {}; }
            try { Array.from.call(Custom, {length: 4294967296}); }
            catch (error) {
                console.log(error instanceof RangeError, calls);
            }
            "#,
        ),
        ["true 0"]
    );
}

/// TypedArray.from must IteratorClose an iterable when the native work cap
/// trips. The drain is charged one unit per element, so reaching the budget
/// costs MAX_NATIVE_ITERATION_WORK guest iterator calls: seconds optimised,
/// over a minute unoptimised.
#[test]
#[cfg_attr(debug_assertions, ignore = "walks MAX_NATIVE_ITERATION_WORK guest iterator steps (over a minute unoptimised); run with --release")]
fn typed_array_from_closes_an_iterable_when_the_work_cap_trips() {
    assert_eq!(
        run_ok(
            r#"
            var closed = false;
            var iterable = {};
            iterable[Symbol.iterator] = function () {
                return {
                    next: function () { return {done:false, value:1}; },
                    return: function () { closed = true; return {done:true}; }
                };
            };
            try { Uint8Array.from(iterable); } catch (error) {
                console.log(error instanceof RangeError, closed);
            }
            "#,
        ),
        ["true true"]
    );
}

/// A single `next()` must not spend unbounded host work walking entries
/// deleted after the iterator was created. Keep one slot beyond the work
/// budget so the test also pins the inclusive boundary.
///
/// Ignored by default since v0.0.10: exceeding the raised budget needs one
/// tombstone per unit of work, and 67 million live Set entries cost more than
/// a gigabyte and minutes of debug-build time. Run it deliberately with
/// `cargo test --release ... -- --ignored`.
#[test]
#[ignore = "needs MAX_NATIVE_ITERATION_WORK+1 live Set entries (>1 GiB); run with --release -- --ignored"]
fn collection_iterators_bound_tombstone_scans() {
    assert_eq!(
        run_ok(&sized(
            r#"
            var set = new Set();
            for (var i = 0; i <= @WORK@; i++) set.add(i);
            var iterator = set.values();
            for (var i = 0; i <= @WORK@; i++) set.delete(i);
            try {
                iterator.next();
                console.log("completed");
            } catch (error) {
                console.log(error instanceof RangeError ? "range" : "other");
            }
            "#,
        )),
        ["range"]
    );
}

#[test]
fn native_name_and_regexp_wrappers_reject_prefix_overflow() {
    assert_catchable_range_error(
        r#"RegExp.prototype.toString.call({source:"x".repeat(@HALF_BYTES_PLUS_1@), flags:"g".repeat(@HALF_BYTES_PLUS_1@)})"#,
    );
    // "bound " is six bytes: a name five short of the cap overflows it.
    assert_catchable_range_error(
        r#"
        var longName = "x".repeat(@BYTES_MINUS_5@);
        var target = new Proxy(function () {}, { get: function (fn, key) {
            if (key === "name") return longName;
            if (key === "length") return 0;
            return fn[key];
        }});
        target.bind(null)
        "#,
    );
    assert_catchable_range_error(
        r#"
        function target() {}
        Object.defineProperty(target, "name", {value:"x".repeat(@BYTES_MINUS_5@)});
        target.bind(null)
        "#,
    );
    // "function " plus "() { [native code] }" is more than 26 bytes.
    assert_catchable_range_error(
        r#"
        Object.defineProperty(Array, "name", {value:"x".repeat(@BYTES_MINUS_26@)});
        Function.prototype.toString.call(Array)
        "#,
    );
}

#[test]
fn wrappers_replacements_and_tags_reject_near_limit_alias_growth() {
    // The same quote-heavy string is aliased as receiver and attribute value;
    // link() must not materialise both an expanded attribute temporary and the
    // final wrapper. Each quote becomes the six-byte `&quot;`.
    assert_catchable_range_error(r#"var x = "\"".repeat(@BYTES_DIV_6_PLUS_1@); x.link(x)"#);
    assert_catchable_range_error(r#""a".repeat(@BYTES@).replace("a", "aa")"#);
    // "[object " and "]" add nine bytes; "Symbol(" and ")" add eight.
    assert_catchable_range_error(
        r#"var tag = "x".repeat(@BYTES_MINUS_6@); var o = {}; o[Symbol.toStringTag] = tag; Object.prototype.toString.call(o)"#,
    );
    assert_catchable_range_error(
        r#"var text = "x".repeat(@HALF_BYTES_PLUS_1@); var error = new Error(); error.name = text; error.message = text; error.toString()"#,
    );
    assert_catchable_range_error(r#"Symbol("x".repeat(@BYTES_MINUS_6@)).toString()"#);
    assert_catchable_range_error(r#"Symbol.for("x".repeat(@BYTES_MINUS_5@))"#);

    // The exact cap remains usable; this pins the final-segment accounting so
    // the guard does not become an off-by-one compatibility regression.
    assert_eq!(
        run_ok(&sized(
            r#"console.log("a".repeat(@BYTES_MINUS_1@).replace("a", "aa").length)"#
        )),
        [BYTES.to_string()]
    );
}

#[test]
fn bounded_builders_preserve_small_result_semantics() {
    assert_eq!(
        run_ok(
            r#"
            console.log("\u039f\u03a3".toLowerCase());
            console.log("\u00e9".normalize("NFD") === "e\u0301");
            console.log("a\"b".link("x\"y"));
            console.log(RegExp.escape("a-b c"));
            console.log(encodeURIComponent("\u00e9 /"));
            console.log(escape("\u0100 "));
            console.log(unescape("%uD800").charCodeAt(0));
            console.log(new Uint8Array([0, 255]).toBase64());
            console.log(new Uint8Array([0, 255]).toHex());
            var o = {}; o[Symbol.toStringTag] = "Cool";
            console.log(Object.prototype.toString.call(o));
            var error = new Error("detail"); error.name = "Oops";
            console.log(error.toString());
            console.log(Symbol("desc").toString());
            "#,
        ),
        [
            "\u{03bf}\u{03c2}",
            "true",
            "<a href=\"x&quot;y\">a\"b</a>",
            "\\x61\\x2db\\x20c",
            "%C3%A9%20%2F",
            "%u0100%20",
            "55296",
            "AP8=",
            "00ff",
            "[object Cool]",
            "Oops: detail",
            "Symbol(desc)",
        ]
    );
}

#[test]
fn typed_array_locale_string_requires_and_coerces_element_methods() {
    assert_eq!(
        run_ok(
            r#"
            var original = Number.prototype.toLocaleString;
            Number.prototype.toLocaleString = 1;
            try {
                new Uint8Array([7]).toLocaleString();
                console.log("completed");
            } catch (error) {
                console.log(error instanceof TypeError ? "type" : "other");
            }
            Number.prototype.toLocaleString = function () {
                return { toString: function () { return "coerced"; } };
            };
            console.log(new Uint8Array([7, 8]).toLocaleString());
            Number.prototype.toLocaleString = original;
            "#,
        ),
        ["type", "coerced,coerced"]
    );
}
