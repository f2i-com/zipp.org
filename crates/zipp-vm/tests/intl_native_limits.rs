//! Safe-profile bounds for Intl work performed inside one VM instruction.

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

#[test]
fn locale_list_preflights_full_tolength_before_index_observation() {
    assert_eq!(
        run_ok(
            r#"
            var reads = 0;
            var locales = new Proxy({length: 262145}, {
                has: function (target, key) { reads++; return key in target; },
                get: function (target, key) {
                    if (key !== "length") reads++;
                    return target[key];
                }
            });
            try { Intl.getCanonicalLocales(locales); }
            catch (error) { console.log(error instanceof RangeError, reads); }
            "#,
        ),
        ["true 0"]
    );
}

#[test]
fn locale_list_hash_deduplication_preserves_first_seen_order() {
    assert_eq!(
        run_ok(
            r#"
            console.log(Intl.getCanonicalLocales([
                "EN-us", "fr", "en-US", "DE", "fr"
            ]).join(","));
            "#,
        ),
        ["en-US,fr,de"]
    );
}

#[test]
fn locale_parsing_charges_direct_and_aggregate_native_work() {
    assert_eq!(
        run_ok(
            r#"
            var huge = "en-x-" + "abcd1234-".repeat(2000) + "abcd1234";
            try { Intl.getCanonicalLocales(huge); }
            catch (error) { console.log(error instanceof RangeError); }

            var tag = "en-x-" + "abcd1234-".repeat(100) + "abcd1234";
            var locales = [];
            for (var i = 0; i < 32; i++) locales.push(tag);
            try { Intl.getCanonicalLocales(locales); }
            catch (error) { console.log(error instanceof RangeError); }
            "#,
        ),
        ["true", "true"]
    );
}
