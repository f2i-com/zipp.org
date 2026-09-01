//! Safe-profile bounds for Intl work performed inside one VM instruction.
//!
//! Every hostile size below is derived from the live ceiling in
//! `zipp_vm::safe_native_limits`: v0.0.10 raised the work budget 256x and the
//! copied numbers this file used to carry kept passing while exercising nothing.

#![cfg(feature = "safe-sandbox")]

use zipp_vm::safe_native_limits::MAX_NATIVE_ITERATION_WORK as WORK;

/// The language-tag parser charges 16 units of work per byte
/// (`locale_tag::LOCALE_PARSE_WORK_PER_BYTE`); this is the shortest
/// `-abcd1234` run that pushes one tag over the whole budget on its own.
const HUGE_TAG_REPEATS: u64 = WORK / 16 / 9 + 1024;

/// Thirty-two tags whose parse work only exceeds the budget in aggregate.
const AGGREGATE_TAGS: u64 = 32;
const AGGREGATE_TAG_REPEATS: u64 = WORK / 16 / AGGREGATE_TAGS / 9 + 64;

fn run_ok(source: &str) -> Vec<String> {
    let result = zipp_vm::run(source).expect("source compiles");
    assert!(
        result.error.is_none(),
        "unexpected uncaught runtime error: {:?}",
        result.error
    );
    result.output
}

fn sized(template: &str) -> String {
    template
        .replace("@OVER_WORK@", &(WORK + 1).to_string())
        .replace("@HUGE_TAG_REPEATS@", &HUGE_TAG_REPEATS.to_string())
        .replace("@AGGREGATE_TAGS@", &AGGREGATE_TAGS.to_string())
        .replace("@AGGREGATE_TAG_REPEATS@", &AGGREGATE_TAG_REPEATS.to_string())
}

#[test]
fn locale_list_preflights_full_tolength_before_index_observation() {
    assert_eq!(
        run_ok(&sized(
            r#"
            var reads = 0;
            var locales = new Proxy({length: @OVER_WORK@}, {
                has: function (target, key) { reads++; return key in target; },
                get: function (target, key) {
                    if (key !== "length") reads++;
                    return target[key];
                }
            });
            try { Intl.getCanonicalLocales(locales); }
            catch (error) { console.log(error instanceof RangeError, reads); }
            "#,
        )),
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
        run_ok(&sized(
            r#"
            var huge = "en-x-" + "abcd1234-".repeat(@HUGE_TAG_REPEATS@) + "abcd1234";
            try { Intl.getCanonicalLocales(huge); }
            catch (error) { console.log(error instanceof RangeError); }

            var tag = "en-x-" + "abcd1234-".repeat(@AGGREGATE_TAG_REPEATS@) + "abcd1234";
            var locales = [];
            for (var i = 0; i < @AGGREGATE_TAGS@; i++) locales.push(tag);
            try { Intl.getCanonicalLocales(locales); }
            catch (error) { console.log(error instanceof RangeError); }
            "#,
        )),
        ["true", "true"]
    );
}
