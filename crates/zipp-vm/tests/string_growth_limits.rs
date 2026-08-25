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
fn unicode_expansion_paths_obey_the_safe_string_cap() {
    // U+0587 uppercases to two Armenian capitals. U+FDFA has a long NFKD
    // expansion. Both inputs are comfortably below the cap while their output
    // crosses it.
    assert_catchable_range_error(r#""\u0587".repeat(300000).toUpperCase()"#);
    assert_catchable_range_error(r#""\uFDFA".repeat(40000).normalize("NFKD")"#);
    assert_catchable_range_error(r#"RegExp.escape(" ".repeat(262145))"#);
}

#[test]
fn codecs_preflight_amplified_results() {
    assert_catchable_range_error(r#"encodeURIComponent("\u00e9".repeat(174763))"#);
    assert_catchable_range_error(r#"escape("\u0100".repeat(174763))"#);
    assert_catchable_range_error("new Uint8Array(786433).toBase64()");
    assert_catchable_range_error("new Uint8Array(524289).toHex()");
}

#[test]
fn native_iteration_and_aggregate_builders_are_bounded() {
    // The length guard runs before raw[0], so an enormous array-like cannot
    // spend a whole bytecode instruction in String.raw property reads.
    assert_eq!(
        run_ok(
            r#"
            var reads = 0;
            var raw = new Proxy({}, { get: function (target, key) {
                if (key === "length") return 262145;
                reads++;
                return "";
            }});
            try { String.raw({raw: raw}); } catch (error) {
                console.log(error instanceof RangeError, reads);
            }
            "#,
        ),
        ["true 0"]
    );

    // TypedArray.from must reject an oversized array-like before reading index
    // zero, and must IteratorClose an iterable when the native work cap trips.
    assert_eq!(
        run_ok(
            r#"
            var reads = 0;
            var source = {length: 262145};
            Object.defineProperty(source, "0", {get: function () { reads++; return 1; }});
            try { Uint8Array.from(source); } catch (error) {
                console.log(error instanceof RangeError, reads);
            }

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
        ["true 0", "true true"]
    );

    assert_catchable_range_error(
        r#"var x = "x".repeat(600000); new Intl.ListFormat().format([x, x])"#,
    );
    assert_catchable_range_error(r#"Function("a".repeat(600000), "b".repeat(600000), "")"#);
    assert_catchable_range_error(
        r#"new Intl.Collator().compare("a".repeat(131073), "a".repeat(131072))"#,
    );
    assert_catchable_range_error(r#""a".repeat(262145)[Symbol.iterator]()"#);

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

#[test]
fn collection_iterators_bound_tombstone_scans() {
    // A single `next()` must not spend unbounded host work walking entries
    // deleted after the iterator was created.  Keep one slot beyond the work
    // budget so the test also pins the inclusive boundary.
    assert_eq!(
        run_ok(
            r#"
            var set = new Set();
            for (var i = 0; i <= 262144; i++) set.add(i);
            var iterator = set.values();
            for (var i = 0; i <= 262144; i++) set.delete(i);
            try {
                iterator.next();
                console.log("completed");
            } catch (error) {
                console.log(error instanceof RangeError ? "range" : "other");
            }
            "#,
        ),
        ["range"]
    );
}

#[test]
fn native_name_and_regexp_wrappers_reject_prefix_overflow() {
    assert_catchable_range_error(
        r#"RegExp.prototype.toString.call({source:"x".repeat(600000), flags:"g".repeat(600000)})"#,
    );
    assert_catchable_range_error(
        r#"
        var longName = "x".repeat(1048571);
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
        Object.defineProperty(target, "name", {value:"x".repeat(1048571)});
        target.bind(null)
        "#,
    );
    assert_catchable_range_error(
        r#"
        Object.defineProperty(Array, "name", {value:"x".repeat(1048550)});
        Function.prototype.toString.call(Array)
        "#,
    );
}

#[test]
fn wrappers_replacements_and_tags_reject_near_limit_alias_growth() {
    // The same quote-heavy string is aliased as receiver and attribute value;
    // link() must not materialise both an expanded attribute temporary and the
    // final wrapper.
    assert_catchable_range_error(r#"var x = "\"".repeat(180000); x.link(x)"#);
    assert_catchable_range_error(r#""a".repeat(1048576).replace("a", "aa")"#);
    assert_catchable_range_error(
        r#"var tag = "x".repeat(1048570); var o = {}; o[Symbol.toStringTag] = tag; Object.prototype.toString.call(o)"#,
    );
    assert_catchable_range_error(
        r#"var text = "x".repeat(600000); var error = new Error(); error.name = text; error.message = text; error.toString()"#,
    );
    assert_catchable_range_error(r#"Symbol("x".repeat(1048570)).toString()"#);
    assert_catchable_range_error(r#"Symbol.for("x".repeat(1048571))"#);

    // The exact cap remains usable; this pins the final-segment accounting so
    // the guard does not become an off-by-one compatibility regression.
    assert_eq!(
        run_ok(r#"console.log("a".repeat(1048575).replace("a", "aa").length)"#),
        ["1048576"]
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
